//! SQL window: editor on top, results / messages / DBMS_OUTPUT below.

use std::collections::VecDeque;
use std::ops::Range;
use std::time::Duration;

use egui::{RichText, Ui};
use orui_db::{Cell, Column, DbError, DbEvent};
use orui_plsql::{Statement, StatementKind, split_statements, statement_at};
use orui_ui::{ResultGrid, SqlEditor};
use serde::{Deserialize, Serialize};

/// Stable identifier of a SQL window, used in dock tabs and request routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct WindowId(pub u64);

#[derive(Serialize, Deserialize)]
pub struct SqlWindow {
    pub title: String,
    pub text: String,
    #[serde(skip)]
    pub state: RunState,
}

/// Everything that is not persisted with the layout.
#[derive(Default)]
pub struct RunState {
    /// Statements still to run in the current script execution.
    pub queue: VecDeque<PendingStatement>,
    /// Request id of the statement currently running.
    pub running: Option<u64>,
    /// Request id of a "fetch more" in flight.
    pub fetching: Option<u64>,
    pub results: Vec<QueryResult>,
    pub messages: Vec<Message>,
    pub output: Vec<String>,
    pub bottom: BottomTab,
    pub cursor: usize,
    pub selection: Option<Range<usize>>,
    pub line_col: (usize, usize),
}

pub struct PendingStatement {
    pub kind: StatementKind,
    pub sql: String,
}

pub struct QueryResult {
    pub id: u64,
    pub title: String,
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
    /// More rows can be fetched.
    pub more: bool,
    pub elapsed: Duration,
}

pub struct Message {
    pub error: bool,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BottomTab {
    Result(usize),
    #[default]
    Messages,
    Output,
}

/// What the app should do after a window handled an event.
pub enum Next {
    /// Nothing to do (still running, or the event wasn't for a statement).
    Wait,
    /// Statement finished; run the next queued one if any.
    Continue,
    /// Statement failed; the rest of the script was dropped.
    Stop,
}

pub struct SqlWindowResponse {
    /// The grid wants more rows for query `id`.
    pub fetch_more: Option<u64>,
}

impl SqlWindow {
    pub fn new(title: String) -> Self {
        Self {
            title,
            text: String::new(),
            state: RunState::default(),
        }
    }

    pub fn is_running(&self) -> bool {
        self.state.running.is_some() || self.state.fetching.is_some()
    }

    /// Statements for "execute": the selection if there is one, else the whole text.
    pub fn statements_all(&self) -> Vec<PendingStatement> {
        match &self.state.selection {
            Some(sel) => statement_texts(
                &self.text[sel.clone()],
                split_statements(&self.text[sel.clone()]),
            ),
            None => statement_texts(&self.text, split_statements(&self.text)),
        }
    }

    /// The statement under the cursor (or the selection).
    pub fn statement_current(&self) -> Vec<PendingStatement> {
        if self.state.selection.is_some() {
            return self.statements_all();
        }
        statement_texts(
            &self.text,
            statement_at(&self.text, self.state.cursor)
                .into_iter()
                .collect(),
        )
    }

    /// Starts a new execution; the app then sends the queued statements.
    pub fn start(&mut self, statements: Vec<PendingStatement>) {
        let state = &mut self.state;
        state.queue = statements.into();
        state.results.clear();
        state.messages.clear();
        state.output.clear();
        state.bottom = BottomTab::Messages;
    }

    pub fn info(&mut self, text: impl Into<String>) {
        self.state.messages.push(Message {
            error: false,
            text: text.into(),
        });
    }

    pub fn error(&mut self, error: &DbError) {
        let text = match error.code {
            Some(code) => format!(
                "ORA-{code:05}: {}",
                error
                    .message
                    .trim_start_matches(&format!("ORA-{code:05}: "))
            ),
            None => error.message.clone(),
        };
        self.state.messages.push(Message { error: true, text });
        self.state.bottom = BottomTab::Messages;
    }

    pub fn handle_event(&mut self, event: DbEvent) -> Next {
        let state = &mut self.state;
        match event {
            DbEvent::QueryStarted { id, columns } => {
                let n = state.results.len() + 1;
                state.results.push(QueryResult {
                    id,
                    title: format!("Result {n}"),
                    columns,
                    rows: Vec::new(),
                    more: true,
                    elapsed: Duration::ZERO,
                });
                state.bottom = BottomTab::Result(n - 1);
                Next::Wait
            }
            DbEvent::Rows {
                id,
                rows,
                more,
                elapsed,
            } => {
                let Some(result) = state.results.iter_mut().find(|r| r.id == id) else {
                    return Next::Wait;
                };
                result.rows.extend(rows);
                result.more = more;
                result.elapsed += elapsed;
                let fetched = result.rows.len();
                if state.fetching == Some(id) {
                    state.fetching = None;
                    return Next::Wait;
                }
                state.running = None;
                let suffix = if more { " (more available)" } else { "" };
                self.info(format!(
                    "{fetched} rows selected in {}{suffix}",
                    fmt_duration(elapsed)
                ));
                Next::Continue
            }
            DbEvent::Executed {
                summary, elapsed, ..
            } => {
                state.running = None;
                self.info(format!("{summary} in {}", fmt_duration(elapsed)));
                Next::Continue
            }
            DbEvent::Failed { id, error, elapsed } => {
                if state.fetching == Some(id) {
                    state.fetching = None;
                } else {
                    state.running = None;
                }
                let dropped = std::mem::take(&mut state.queue).len();
                self.error(&error);
                self.info(format!("Failed after {}", fmt_duration(elapsed)));
                if dropped > 0 {
                    self.info(format!("{dropped} remaining statement(s) skipped"));
                }
                Next::Stop
            }
            DbEvent::Output(lines) => {
                state.output.extend(lines);
                if state.results.is_empty() {
                    state.bottom = BottomTab::Output;
                }
                Next::Wait
            }
            DbEvent::Notice(text) => {
                self.info(text);
                Next::Wait
            }
            DbEvent::Error(error) => {
                self.error(&error);
                Next::Wait
            }
            _ => Next::Wait,
        }
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        id: WindowId,
        font_size: f32,
        line_numbers: bool,
    ) -> SqlWindowResponse {
        let mut response = SqlWindowResponse { fetch_more: None };
        egui::Panel::bottom(egui::Id::new(("sql-bottom", id)))
            .resizable(true)
            .default_size(ui.available_height() * 0.45)
            .min_size(80.0)
            .show(ui, |ui| response.fetch_more = self.bottom_ui(ui, id));
        egui::CentralPanel::default().show(ui, |ui| {
            let out = SqlEditor::new(&mut self.text, ("sql-editor", id))
                .font_size(font_size)
                .line_numbers(line_numbers)
                .show(ui);
            self.state.cursor = out.cursor;
            self.state.selection = out.selection;
            self.state.line_col = out.line_col;
        });
        response
    }

    fn bottom_ui(&mut self, ui: &mut Ui, id: WindowId) -> Option<u64> {
        let state = &mut self.state;
        ui.horizontal(|ui| {
            for (i, result) in state.results.iter().enumerate() {
                ui.selectable_value(&mut state.bottom, BottomTab::Result(i), &result.title);
            }
            let has_errors = state.messages.iter().any(|m| m.error);
            let label = if has_errors {
                RichText::new("Messages").color(ui.visuals().error_fg_color)
            } else {
                "Messages".into()
            };
            ui.selectable_value(&mut state.bottom, BottomTab::Messages, label);
            let output = format!("Output ({})", state.output.len());
            ui.selectable_value(&mut state.bottom, BottomTab::Output, output);
            if state.running.is_some() || state.fetching.is_some() {
                ui.spinner();
                ui.label("Executing… (Shift+Esc to break)");
            }
        });
        ui.separator();

        let mut fetch_more = None;
        match state.bottom {
            BottomTab::Result(i) if i < state.results.len() => {
                let result = &state.results[i];
                let out = ResultGrid::new(&result.columns, &result.rows, ("grid", id, result.id))
                    .show(ui);
                let is_open = state.results.len() == i + 1 && result.more;
                if out.reached_end && is_open && state.running.is_none() && state.fetching.is_none()
                {
                    state.fetching = Some(result.id);
                    fetch_more = Some(result.id);
                }
            }
            BottomTab::Result(_) | BottomTab::Messages => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for m in &state.messages {
                            let color = if m.error {
                                ui.visuals().error_fg_color
                            } else {
                                ui.visuals().text_color()
                            };
                            ui.label(RichText::new(&m.text).monospace().color(color));
                        }
                    });
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
        fetch_more
    }

    /// Title with a marker while running, for the dock tab.
    pub fn tab_title(&self) -> RichText {
        if self.is_running() {
            RichText::new(format!("⏳ {}", self.title))
        } else {
            RichText::new(&self.title)
        }
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

fn statement_texts(src: &str, statements: Vec<Statement>) -> Vec<PendingStatement> {
    statements
        .into_iter()
        .map(|s| PendingStatement {
            kind: s.kind,
            sql: s.text(src).to_owned(),
        })
        .collect()
}

pub fn fmt_duration(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{} ms", d.as_millis())
    } else {
        format!("{:.2} s", d.as_secs_f64())
    }
}
