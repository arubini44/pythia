//! SQL window: editor on top, results / plan / messages / DBMS_OUTPUT below.

use std::collections::{BTreeMap, VecDeque};
use std::ops::Range;
use std::time::Duration;

use egui::{RichText, Ui};
use orui_db::{Cell, Column, DbError, DbEvent, DbRequest, MetaQuery, MetaResult};
use orui_plsql::{StatementKind, TokenKind, split_statements, statement_at, tokenize};
use orui_ui::icons::{Icon, icon_text_button};
use orui_ui::{ResultGrid, to_tsv};
use serde::{Deserialize, Serialize};

use crate::documents::{DocContext, Effects, TableData, Target, request_id};
use crate::editor::{Editor, Marker, MarkerKind};

#[derive(Serialize, Deserialize)]
pub struct SqlWindow {
    pub title: String,
    pub editor: Editor,
    /// Last values entered for bind variables, by lowercase name.
    #[serde(default)]
    pub bind_values: BTreeMap<String, Option<String>>,
    /// `&&var` / `DEFINE` values, by lowercase name.
    #[serde(default)]
    pub defines: BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub substitution: bool,
    #[serde(skip)]
    pub state: RunState,
}

fn yes() -> bool {
    true
}

/// Everything that is not persisted with the layout.
#[derive(Default)]
pub struct RunState {
    /// Statements still to run in the current script execution.
    pub queue: VecDeque<PendingStatement>,
    /// The statement currently running.
    pub running: Option<Running>,
    /// Request id of a "fetch more" in flight.
    pub fetching: Option<u64>,
    pub results: Vec<QueryResult>,
    pub plan: Option<Plan>,
    pub messages: Vec<Message>,
    pub output: Vec<String>,
    pub bottom: BottomTab,
    pub line_col: (usize, usize),
    pub prompt: Option<VariablePrompt>,
}

#[derive(Debug, Clone)]
pub struct PendingStatement {
    pub kind: StatementKind,
    pub sql: String,
    /// Byte offset of the statement in the editor text.
    pub start: usize,
}

pub struct Running {
    pub start: usize,
    pub sql: String,
}

pub struct QueryResult {
    pub id: u64,
    pub title: String,
    pub sql: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
    /// More rows can be fetched.
    pub more: bool,
    pub elapsed: Duration,
}

pub struct Plan {
    pub id: u64,
    pub result: Option<Result<TableData, String>>,
}

pub struct Message {
    pub level: Level,
    pub text: String,
    /// Clicking the message selects this range in the editor.
    pub jump: Option<Range<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BottomTab {
    Result(usize),
    Plan,
    #[default]
    Messages,
    Output,
}

/// Values the user must enter before a statement runs.
pub struct VariablePrompt {
    statement: PendingStatement,
    /// `&var` substitutions: (lowercase name, value).
    substitutions: Vec<(String, String)>,
    /// `:bind` variables: (name as written, value, is_null).
    binds: Vec<(String, String, bool)>,
    focus_first: bool,
}

impl SqlWindow {
    pub fn new(title: String, text: String) -> Self {
        Self {
            title,
            editor: Editor::new(text),
            bind_values: BTreeMap::new(),
            defines: BTreeMap::new(),
            substitution: true,
            state: RunState::default(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.state.running.is_some() || self.state.fetching.is_some() || self.state.prompt.is_some()
    }

    fn statements(&self, current_only: bool) -> Vec<PendingStatement> {
        let text = self.editor.text();
        let selection = self.editor.selection();
        let (base, src) = match &selection {
            Some(sel) => (sel.start, &text[sel.clone()]),
            None => (0, text),
        };
        let statements = if current_only && selection.is_none() {
            statement_at(src, self.editor.cursor())
                .into_iter()
                .collect()
        } else {
            split_statements(src)
        };
        statements
            .into_iter()
            .map(|s| PendingStatement {
                kind: s.kind,
                sql: s.text(src).to_owned(),
                start: base + s.range.start,
            })
            .collect()
    }

    /// Starts executing the selection / whole text, or the current statement.
    pub fn execute(&mut self, current_only: bool, cx: &DocContext) -> Effects {
        if self.is_running() {
            return Effects::default();
        }
        let statements = self.statements(current_only);
        let state = &mut self.state;
        state.results.clear();
        state.plan = None;
        state.messages.clear();
        state.output.clear();
        state.bottom = BottomTab::Messages;
        self.editor.set_markers(Vec::new());
        if statements.is_empty() {
            self.info("Nothing to execute");
            return Effects::default();
        }
        self.state.queue = statements.into();
        self.run_next(cx)
    }

    /// Runs `EXPLAIN PLAN` for the current statement (on the main session).
    pub fn explain(&mut self) -> Effects {
        let mut effects = Effects::default();
        let Some(stmt) = self.statements(true).into_iter().next() else {
            return effects;
        };
        if stmt.kind != StatementKind::Sql {
            self.info("Explain plan works for SQL statements only");
            return effects;
        }
        let id = request_id();
        self.state.plan = Some(Plan { id, result: None });
        self.state.bottom = BottomTab::Plan;
        let sql = substitute(&stmt.sql, &self.defines);
        effects.request(
            Target::Main,
            DbRequest::Meta {
                id,
                query: MetaQuery::ExplainPlan { sql },
            },
        );
        effects
    }

    /// Sends the next queued statement, handling SQL*Plus commands locally.
    fn run_next(&mut self, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        while let Some(mut stmt) = self.state.queue.pop_front() {
            if stmt.kind == StatementKind::SqlPlus {
                match self.sqlplus(&stmt) {
                    SqlPlus::Done => continue,
                    SqlPlus::Run(sql) => {
                        stmt.sql = sql;
                        stmt.kind = StatementKind::PlSql;
                    }
                    SqlPlus::Query(sql, binds) => {
                        self.send(stmt.start, sql, binds, cx, &mut effects);
                        return effects;
                    }
                }
            }
            let substitutions = if self.substitution {
                find_substitutions(&stmt.sql)
            } else {
                Vec::new()
            };
            let missing = dedup(
                substitutions
                    .iter()
                    .filter(|s| !self.defines.contains_key(&s.name))
                    .map(|s| s.name.clone()),
            );
            let binds = dedup(find_binds(&stmt.sql).into_iter());
            if !missing.is_empty() || !binds.is_empty() {
                let binds = binds
                    .into_iter()
                    .map(|name| {
                        let remembered = self.bind_values.get(&name.to_lowercase());
                        let is_null = matches!(remembered, Some(None));
                        let value = remembered.cloned().flatten().unwrap_or_default();
                        (name, value, is_null)
                    })
                    .collect();
                let substitutions = missing.into_iter().map(|n| (n, String::new())).collect();
                self.state.prompt = Some(VariablePrompt {
                    statement: stmt,
                    substitutions,
                    binds,
                    focus_first: true,
                });
                return effects;
            }
            let sql = if self.substitution {
                substitute(&stmt.sql, &self.defines)
            } else {
                stmt.sql.clone()
            };
            self.send(stmt.start, sql, Vec::new(), cx, &mut effects);
            return effects;
        }
        effects
    }

    fn send(
        &mut self,
        start: usize,
        sql: String,
        binds: Vec<(String, Option<String>)>,
        cx: &DocContext,
        effects: &mut Effects,
    ) {
        let id = request_id();
        self.state.running = Some(Running {
            start,
            sql: sql.clone(),
        });
        effects.request(
            Target::Own,
            DbRequest::Execute {
                id,
                sql,
                max_rows: cx.fetch_size,
                binds,
            },
        );
    }

    /// Handles a SQL*Plus command; returns SQL to run instead, if any.
    fn sqlplus(&mut self, stmt: &PendingStatement) -> SqlPlus {
        let line = stmt.sql.trim();
        let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        match command.to_ascii_uppercase().as_str() {
            "EXEC" | "EXECUTE" => {
                SqlPlus::Run(format!("begin\n  {};\nend;", rest.trim_end_matches(';')))
            }
            "PRO" | "PROMPT" => {
                self.state.output.push(rest.to_owned());
                SqlPlus::Done
            }
            "DEF" | "DEFINE" => {
                match rest.split_once('=') {
                    Some((name, value)) => {
                        let value = value.trim().trim_matches(|c| c == '\'' || c == '"');
                        self.defines
                            .insert(name.trim().to_lowercase(), value.to_owned());
                    }
                    None => {
                        let list: Vec<String> = self
                            .defines
                            .iter()
                            .map(|(k, v)| format!("DEFINE {k} = \"{v}\""))
                            .collect();
                        self.state.output.extend(list);
                    }
                }
                SqlPlus::Done
            }
            "UNDEF" | "UNDEFINE" => {
                for name in rest.split_whitespace() {
                    self.defines.remove(&name.to_lowercase());
                }
                SqlPlus::Done
            }
            "DESC" | "DESCRIBE" => {
                let name = rest.trim_end_matches(';').to_owned();
                let (owner, object) = match name.split_once('.') {
                    Some((o, n)) => (Some(o.to_owned()), n.to_owned()),
                    None => (None, name),
                };
                SqlPlus::Query(
                    DESCRIBE_SQL.into(),
                    vec![("name".into(), Some(object)), ("owner".into(), owner)],
                )
            }
            "SET" => {
                let lower = rest.to_ascii_lowercase();
                if let Some(v) = lower
                    .strip_prefix("define ")
                    .or_else(|| lower.strip_prefix("def "))
                {
                    self.substitution = v.trim() != "off";
                }
                SqlPlus::Done
            }
            "REM" | "REMARK" | "SPOOL" | "SPO" | "WHENEVER" | "COLUMN" | "COL" | "CLEAR"
            | "PAUSE" => SqlPlus::Done,
            _ => {
                let first = line.lines().next().unwrap_or_default();
                self.warn(format!(
                    "SQL*Plus command not supported in SQL windows: {first}"
                ));
                SqlPlus::Done
            }
        }
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.state.messages.push(Message {
            level: Level::Info,
            text: text.into(),
            jump: None,
        });
    }

    fn warn(&mut self, text: impl Into<String>) {
        self.state.messages.push(Message {
            level: Level::Warning,
            text: text.into(),
            jump: None,
        });
    }

    pub fn error(&mut self, error: &DbError) {
        let jump = self.state.running.as_ref().and_then(|r| {
            let offset = error.offset? as usize;
            let pos = r.start + offset.min(r.sql.len());
            Some(pos..pos + 1)
        });
        let text = match error.code {
            Some(code) if !error.message.starts_with("ORA-") => {
                format!("ORA-{code:05}: {}", error.message)
            }
            _ => error.message.clone(),
        };
        if let Some(jump) = &jump {
            self.editor.select(jump.clone());
        }
        self.state.messages.push(Message {
            level: Level::Error,
            text,
            jump,
        });
        self.state.bottom = BottomTab::Messages;
    }

    pub fn on_event(&mut self, event: DbEvent, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        match event {
            DbEvent::QueryStarted { id, columns } => {
                let n = self.state.results.len() + 1;
                let sql = self
                    .state
                    .running
                    .as_ref()
                    .map(|r| r.sql.clone())
                    .unwrap_or_default();
                self.state.results.push(QueryResult {
                    id,
                    title: format!("Result {n}"),
                    sql,
                    columns,
                    rows: Vec::new(),
                    more: true,
                    elapsed: Duration::ZERO,
                });
                self.state.bottom = BottomTab::Result(n - 1);
            }
            DbEvent::Rows {
                id,
                rows,
                more,
                elapsed,
            } => {
                let Some(result) = self.state.results.iter_mut().find(|r| r.id == id) else {
                    return effects;
                };
                result.rows.extend(rows);
                result.more = more;
                result.elapsed += elapsed;
                let fetched = result.rows.len();
                if self.state.fetching == Some(id) {
                    self.state.fetching = None;
                    return effects;
                }
                if let Some(running) = self.state.running.take() {
                    effects.executed.push(running.sql);
                }
                let suffix = if more { " (more available)" } else { "" };
                let rows = if fetched == 1 {
                    "1 row".to_owned()
                } else {
                    format!("{fetched} rows")
                };
                self.info(format!(
                    "{rows} selected in {}{suffix}",
                    fmt_duration(elapsed)
                ));
                effects.extend(self.run_next(cx));
            }
            DbEvent::Executed {
                summary, elapsed, ..
            } => {
                let running = self.state.running.take();
                let failed = summary
                    .compiled
                    .as_ref()
                    .is_some_and(|c| c.errors.iter().any(|e| !e.warning));
                let text = format!("{summary} in {}", fmt_duration(elapsed));
                if failed {
                    self.warn(text);
                } else {
                    self.info(text);
                }
                if let (Some(compiled), Some(running)) = (&summary.compiled, &running) {
                    self.compile_errors(compiled, running);
                }
                if let Some(running) = running {
                    effects.executed.push(running.sql);
                }
                effects.extend(self.run_next(cx));
            }
            DbEvent::Failed { id, error, elapsed } => {
                if self.state.fetching == Some(id) {
                    self.state.fetching = None;
                    self.error(&error);
                    return effects;
                }
                self.error(&error);
                self.state.running = None;
                let dropped = std::mem::take(&mut self.state.queue).len();
                self.info(format!("Failed after {}", fmt_duration(elapsed)));
                if dropped > 0 {
                    self.info(format!("{dropped} remaining statement(s) skipped"));
                }
            }
            DbEvent::Meta { id, result } => {
                if let Some(plan) = self.state.plan.as_mut().filter(|p| p.id == id) {
                    plan.result = Some(match result {
                        Ok(MetaResult::Table { columns, rows }) => Ok((columns, rows)),
                        Ok(_) => Err("unexpected result".into()),
                        Err(e) => Err(e.to_string()),
                    });
                }
            }
            DbEvent::Output(lines) => {
                self.state.output.extend(lines);
                if self.state.results.is_empty() {
                    self.state.bottom = BottomTab::Output;
                }
            }
            DbEvent::Notice(text) => self.info(text),
            DbEvent::Error(error) | DbEvent::ConnectFailed(error) => {
                self.state.running = None;
                self.state.fetching = None;
                self.state.queue.clear();
                self.error(&error);
            }
            _ => {}
        }
        effects
    }

    fn compile_errors(&mut self, compiled: &orui_db::Compiled, running: &Running) {
        let text = self.editor.text();
        // ALL_ERRORS lines are relative to the unit's source, whose line 1 is
        // the `CREATE ...` line of the statement.
        let base_line = text[..running.start.min(text.len())].matches('\n').count();
        let mut markers = Vec::new();
        let mut messages = Vec::new();
        for e in &compiled.errors {
            let line = base_line + e.line as usize;
            let start = crate::editor::line_start(text, line);
            let col = text[start..]
                .char_indices()
                .nth(e.position.saturating_sub(1) as usize)
                .map_or(start, |(i, _)| start + i);
            let level = if e.warning {
                Level::Warning
            } else {
                Level::Error
            };
            messages.push(Message {
                level,
                text: format!("  {}:{}  {}", e.line, e.position, e.text),
                jump: Some(col..col + 1),
            });
            markers.push(Marker {
                line,
                kind: if e.warning {
                    MarkerKind::Warning
                } else {
                    MarkerKind::Error
                },
                message: e.text.clone(),
            });
        }
        self.state.messages.extend(messages);
        self.editor.set_markers(markers);
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        egui::Panel::bottom(egui::Id::new(("sql-bottom", cx.id)))
            .resizable(true)
            .default_size(ui.available_height() * 0.45)
            .min_size(80.0)
            .show(ui, |ui| effects.extend(self.bottom_ui(ui, cx)));
        egui::CentralPanel::default().show(ui, |ui| {
            self.editor
                .ui(ui, ("sql-editor", cx.id), &cx.settings.editor);
            self.state.line_col = self.editor.line_col();
        });
        effects.extend(self.prompt_ui(ui.ctx(), cx));
        effects
    }

    fn bottom_ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        let state = &mut self.state;
        ui.horizontal(|ui| {
            for (i, result) in state.results.iter().enumerate() {
                ui.selectable_value(&mut state.bottom, BottomTab::Result(i), &result.title);
            }
            if state.plan.is_some() {
                ui.selectable_value(&mut state.bottom, BottomTab::Plan, "Plan");
            }
            let label = match state.messages.iter().map(|m| m.level).max() {
                Some(Level::Error) => RichText::new("Messages").color(ui.visuals().error_fg_color),
                Some(Level::Warning) => RichText::new("Messages").color(ui.visuals().warn_fg_color),
                _ => "Messages".into(),
            };
            ui.selectable_value(&mut state.bottom, BottomTab::Messages, label);
            let output = if state.output.is_empty() {
                "Output".to_owned()
            } else {
                format!("Output ({})", state.output.len())
            };
            ui.selectable_value(&mut state.bottom, BottomTab::Output, output);
            if state.running.is_some() || state.fetching.is_some() {
                ui.spinner();
                ui.label("Executing…");
                if icon_text_button(ui, Icon::Stop, "Break")
                    .on_hover_text("Shift+Esc")
                    .clicked()
                {
                    effects.cancel = true;
                }
            }
        });
        if let BottomTab::Result(i) = state.bottom
            && let Some(result) = state.results.get(i)
        {
            ui.horizontal(|ui| {
                let more = if result.more { "+" } else { "" };
                ui.weak(format!(
                    "{}{more} rows · {} columns · {}",
                    result.rows.len(),
                    result.columns.len(),
                    fmt_duration(result.elapsed)
                ))
                .on_hover_text(&result.sql);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icon_text_button(ui, Icon::Copy, "Copy all")
                        .on_hover_text("Copy fetched rows as tab separated text")
                        .clicked()
                    {
                        ui.ctx().copy_text(to_tsv(&result.columns, &result.rows));
                    }
                    let idle = state.fetching.is_none() && state.running.is_none();
                    if result.more
                        && idle
                        && icon_text_button(ui, Icon::Grid, "Fetch all").clicked()
                    {
                        state.fetching = Some(result.id);
                        effects.request(
                            Target::Own,
                            DbRequest::FetchMore {
                                id: result.id,
                                max_rows: usize::MAX,
                            },
                        );
                    }
                });
            });
        }
        ui.separator();

        match state.bottom {
            BottomTab::Result(i) if i < state.results.len() => {
                let result = &state.results[i];
                let out =
                    ResultGrid::new(&result.columns, &result.rows, ("grid", cx.id, result.id))
                        .show(ui);
                let is_open = state.results.len() == i + 1 && result.more;
                if out.reached_end && is_open && state.running.is_none() && state.fetching.is_none()
                {
                    state.fetching = Some(result.id);
                    effects.request(
                        Target::Own,
                        DbRequest::FetchMore {
                            id: result.id,
                            max_rows: cx.fetch_size,
                        },
                    );
                }
            }
            BottomTab::Plan => match state.plan.as_ref().and_then(|p| p.result.as_ref()) {
                None => {
                    ui.spinner();
                }
                Some(Ok((columns, rows))) => {
                    ResultGrid::new(columns, rows, ("plan", cx.id)).show(ui);
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            },
            BottomTab::Result(_) | BottomTab::Messages => {
                let mut jump = None;
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for m in &state.messages {
                            let color = match m.level {
                                Level::Error => ui.visuals().error_fg_color,
                                Level::Warning => ui.visuals().warn_fg_color,
                                Level::Info => ui.visuals().text_color(),
                            };
                            let text = RichText::new(&m.text).monospace().color(color);
                            match &m.jump {
                                Some(range) => {
                                    if ui.link(text).on_hover_text("Go to error").clicked() {
                                        jump = Some(range.clone());
                                    }
                                }
                                None => {
                                    ui.label(text);
                                }
                            }
                        }
                    });
                if let Some(range) = jump {
                    self.editor.select(range);
                }
            }
            BottomTab::Output => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in &state.output {
                            ui.label(RichText::new(line).monospace());
                        }
                    });
            }
        }
        effects
    }

    fn prompt_ui(&mut self, ctx: &egui::Context, cx: &DocContext) -> Effects {
        let Some(prompt) = self.state.prompt.as_mut() else {
            return Effects::default();
        };
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new(("variables", cx.id))).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Variables");
            ui.add_space(4.0);
            ui.label(
                RichText::new(first_line(&prompt.statement.sql))
                    .monospace()
                    .weak(),
            );
            ui.add_space(8.0);
            let mut first = prompt.focus_first;
            let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
            egui::Grid::new("vars")
                .num_columns(3)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    for (name, value) in &mut prompt.substitutions {
                        ui.label(format!("&{name}"));
                        let r = ui.add(egui::TextEdit::singleline(value).desired_width(260.0));
                        if std::mem::take(&mut first) {
                            r.request_focus();
                        }
                        ui.label("");
                        ui.end_row();
                    }
                    for (name, value, is_null) in &mut prompt.binds {
                        ui.label(format!(":{name}"));
                        let r = ui.add_enabled(
                            !*is_null,
                            egui::TextEdit::singleline(value).desired_width(260.0),
                        );
                        if std::mem::take(&mut first) {
                            r.request_focus();
                        }
                        ui.checkbox(is_null, "NULL");
                        ui.end_row();
                    }
                });
            prompt.focus_first = false;
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Execute").clicked() || enter {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        if modal.should_close() && decision.is_none() {
            decision = Some(false);
        }
        let mut effects = Effects::default();
        match decision {
            Some(true) => {
                let prompt = self.state.prompt.take().expect("prompt");
                let doubles: Vec<String> = find_substitutions(&prompt.statement.sql)
                    .into_iter()
                    .filter(|s| s.double)
                    .map(|s| s.name)
                    .collect();
                let mut values = self.defines.clone();
                for (name, value) in &prompt.substitutions {
                    values.insert(name.clone(), value.clone());
                    // `&&var` keeps its value for the window, `&var` only for this run.
                    if doubles.contains(name) {
                        self.defines.insert(name.clone(), value.clone());
                    }
                }
                let sql = if self.substitution {
                    substitute(&prompt.statement.sql, &values)
                } else {
                    prompt.statement.sql.clone()
                };
                let binds: Vec<(String, Option<String>)> = prompt
                    .binds
                    .into_iter()
                    .map(|(name, value, is_null)| {
                        let value = (!is_null).then_some(value);
                        self.bind_values.insert(name.to_lowercase(), value.clone());
                        (name, value)
                    })
                    .collect();
                self.send(prompt.statement.start, sql, binds, cx, &mut effects);
            }
            Some(false) => {
                self.state.prompt = None;
                self.state.queue.clear();
                self.info("Execution cancelled");
            }
            None => {}
        }
        effects
    }

    /// Summary for the status bar of the active result, if one is shown.
    pub fn result_status(&self) -> Option<String> {
        let BottomTab::Result(i) = self.state.bottom else {
            return None;
        };
        let r = self.state.results.get(i)?;
        let more = if r.more { "+" } else { "" };
        Some(format!(
            "{}{more} rows · {}",
            r.rows.len(),
            fmt_duration(r.elapsed)
        ))
    }
}

const DESCRIBE_SQL: &str = "select column_name as \"Name\", \
        decode(nullable, 'N', 'NOT NULL') as \"Null?\", \
        data_type || case \
          when data_type in ('VARCHAR2', 'NVARCHAR2', 'CHAR', 'NCHAR', 'RAW') \
            then '(' || decode(char_used, 'C', char_length || ' CHAR', data_length) || ')' \
          when data_precision is not null \
            then '(' || data_precision || nvl2(nullif(data_scale, 0), ',' || data_scale, '') || ')' \
        end as \"Type\" \
   from all_tab_columns \
  where table_name = upper(:name) \
    and owner = nvl(upper(:owner), sys_context('USERENV', 'CURRENT_SCHEMA')) \
  order by column_id";

enum SqlPlus {
    Done,
    Run(String),
    Query(String, Vec<(String, Option<String>)>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Substitution {
    pub range: Range<usize>,
    /// Lowercase name.
    pub name: String,
    pub double: bool,
}

/// Finds `&var` / `&&var` references, including inside string literals (as
/// SQL*Plus does), but not inside comments.
pub fn find_substitutions(sql: &str) -> Vec<Substitution> {
    let mut out = Vec::new();
    for token in tokenize(sql) {
        if !matches!(
            token.kind,
            TokenKind::SubstVar | TokenKind::String | TokenKind::QString
        ) {
            continue;
        }
        let text = &sql[token.span.clone()];
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != b'&' {
                i += 1;
                continue;
            }
            let start = i;
            let double = bytes.get(i + 1) == Some(&b'&');
            let name_start = i + if double { 2 } else { 1 };
            let name_len = text[name_start..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '#'))
                .unwrap_or(text.len() - name_start);
            if name_len == 0 {
                i = name_start;
                continue;
            }
            let mut end = name_start + name_len;
            // A trailing period terminates the name and is removed.
            if bytes.get(end) == Some(&b'.') {
                end += 1;
            }
            out.push(Substitution {
                range: token.span.start + start..token.span.start + end,
                name: text[name_start..name_start + name_len].to_lowercase(),
                double,
            });
            i = end;
        }
    }
    out
}

/// Replaces substitution variables that have a value.
pub fn substitute(sql: &str, values: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut pos = 0;
    for s in find_substitutions(sql) {
        if let Some(value) = values.get(&s.name) {
            out.push_str(&sql[pos..s.range.start]);
            out.push_str(value);
            pos = s.range.end;
        }
    }
    out.push_str(&sql[pos..]);
    out
}

/// Names of `:bind` variables, excluding trigger `:new`/`:old` references.
pub fn find_binds(sql: &str) -> Vec<String> {
    if orui_plsql::unit_header(sql).is_some() {
        // Stored units (incl. triggers) don't take binds when created.
        return Vec::new();
    }
    tokenize(sql)
        .into_iter()
        .filter(|t| t.kind == TokenKind::BindVar)
        .map(|t| sql[t.span.start + 1..t.span.end].to_owned())
        .filter(|name| !name.chars().all(|c| c.is_ascii_digit()))
        .collect()
}

/// Removes case-insensitive duplicates, keeping the first spelling.
fn dedup(names: impl Iterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for name in names {
        if !out.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            out.push(name);
        }
    }
    out
}

fn first_line(sql: &str) -> String {
    let line = sql.lines().next().unwrap_or_default();
    if line.chars().count() > 70 {
        format!("{}…", line.chars().take(70).collect::<String>())
    } else {
        line.to_owned()
    }
}

pub fn fmt_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else {
        format!("{:.2} s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitutions() {
        let sql = "select '&a', &&b. || x from t where c = &c -- &d\n/* &e */";
        let names: Vec<_> = find_substitutions(sql)
            .into_iter()
            .map(|s| (s.name, s.double))
            .collect();
        assert_eq!(
            names,
            [("a".into(), false), ("b".into(), true), ("c".into(), false)]
        );
        let values = BTreeMap::from([
            ("a".into(), "1".into()),
            ("b".into(), "2".into()),
            ("c".into(), "3".into()),
        ]);
        assert_eq!(
            substitute(sql, &values),
            "select '1', 2 || x from t where c = 3 -- &d\n/* &e */"
        );
    }

    #[test]
    fn binds() {
        assert_eq!(
            find_binds("select :a, :B, :a from t where x = :1 and y = ':z'"),
            ["a", "B", "a"]
        );
        assert!(
            find_binds("create or replace trigger t before insert on x for each row begin :new.id := 1; end;")
                .is_empty()
        );
        assert_eq!(
            dedup(["a".to_owned(), "A".to_owned(), "b".to_owned()].into_iter()),
            ["a", "b"]
        );
    }
}
