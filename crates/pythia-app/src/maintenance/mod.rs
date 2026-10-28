//! Maintenance of a table, materialized view or index (#144): its state as
//! evidence, the database's maintenance tasks with their options and what
//! they cost, the statements, and running them one by one on the window's
//! own session (which autocommits, so it never commits another window's
//! work, and VACUUM isn't in a transaction block).

mod facts;
mod tasks;

use std::time::{Duration, Instant};

use egui::{Color32, RichText, Ui};
use pythia_db::oracle::Connection;
use pythia_db::{DbEvent, DbKind, DbRequest, Dialect, ObjectType};
use pythia_ui::icons::Icon;
use serde::{Deserialize, Serialize};

pub use facts::Facts;
pub use tasks::{Options, Task, has_tasks};

use crate::documents::{DocContext, Effects, ObjectRef, OpenRequest, Target, request_id};

/// Rows of a statement's answer kept (MySQL's ANALYZE TABLE and friends
/// answer with a few).
const MAX_ROWS: usize = 200;

#[derive(Serialize, Deserialize)]
pub struct MaintenanceWindow {
    pub object: ObjectRef,
    /// The task picked; the first one the database has when none.
    pub task: Option<Task>,
    pub options: Options,
    #[serde(skip)]
    state: State,
}

#[derive(Default)]
struct State {
    /// The state was asked for (once shown).
    requested: bool,
    loading: Option<u64>,
    facts: Option<Facts>,
    load_error: Option<String>,
    run: Option<Run>,
    /// The statements of the last run, with what came back.
    steps: Vec<Step>,
    /// The step that ended last, which trailing messages belong to.
    ended: Option<usize>,
    /// How the last run ended, and whether it went well.
    outcome: Option<(String, bool)>,
    /// Asking before a run on a production connection.
    confirm: bool,
}

/// A run in progress.
struct Run {
    /// The task's name, for the outcome.
    label: &'static str,
    /// The step running, and its request.
    step: usize,
    id: u64,
    columns: Vec<String>,
    started: Instant,
    began: Instant,
    cancelled: bool,
}

struct Step {
    sql: String,
    status: Status,
    /// Notices, output and result rows.
    messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum Status {
    Waiting,
    Running,
    Done(Duration),
    Failed(Duration, String),
    Skipped,
}

impl MaintenanceWindow {
    pub fn new(object: ObjectRef) -> Self {
        Self {
            object,
            task: None,
            options: Options::default(),
            state: State::default(),
        }
    }

    pub fn title(&self) -> String {
        format!("{} maintenance", self.object.name.to_lowercase())
    }

    pub fn is_busy(&self) -> bool {
        self.state.loading.is_some() || self.state.run.is_some()
    }

    /// The task picked, if the database has it, else its first.
    fn task(&self, dialect: Dialect) -> Option<Task> {
        let offered = tasks::tasks(dialect, self.object.object_type);
        self.task
            .filter(|t| offered.contains(t))
            .or_else(|| offered.first().copied())
    }

    /// Loads the object's state (again).
    pub fn load(&mut self, dialect: Dialect) -> Effects {
        let mut effects = Effects::default();
        let id = request_id();
        self.state.requested = true;
        self.state.loading = Some(id);
        let object = self.object.clone();
        let request = if dialect == Dialect::ORACLE {
            let task: pythia_db::TaskFn = Box::new(move |conn: &Connection| {
                Ok(Box::new(facts::oracle(conn, &object)?) as Box<dyn std::any::Any + Send>)
            });
            DbRequest::Task { id, task }
        } else {
            let task: pythia_db::RunFn = Box::new(move |client| {
                Ok(Box::new(facts::other(client, &object)?) as Box<dyn std::any::Any + Send>)
            });
            DbRequest::Run { id, task }
        };
        effects.request(Target::Meta, request);
        effects
    }

    /// The statements of the picked task, or why there are none yet.
    fn statements(&self, dialect: Dialect) -> Result<Vec<String>, String> {
        let task = self
            .task(dialect)
            .ok_or_else(|| format!("No maintenance tasks on {}", dialect.name()))?;
        let facts = self.state.facts.as_ref().ok_or("Reading the state…")?;
        tasks::statements(dialect, &self.object, task, &self.options, facts)
    }

    /// Starts running the statements, the first one now.
    fn start(&mut self, task: Task, dialect: Dialect, statements: Vec<String>) -> Effects {
        let mut effects = Effects::default();
        if statements.is_empty() || self.state.run.is_some() {
            return effects;
        }
        self.state.outcome = None;
        self.state.ended = None;
        self.state.steps = statements
            .into_iter()
            .map(|sql| Step {
                sql,
                status: Status::Waiting,
                messages: Vec::new(),
            })
            .collect();
        let now = Instant::now();
        self.state.run = Some(Run {
            label: task.label(dialect),
            step: 0,
            id: 0,
            columns: Vec::new(),
            started: now,
            began: now,
            cancelled: false,
        });
        self.send_step(0, &mut effects);
        effects
    }

    fn send_step(&mut self, step: usize, effects: &mut Effects) {
        let (Some(run), Some(s)) = (self.state.run.as_mut(), self.state.steps.get_mut(step)) else {
            return;
        };
        let id = request_id();
        run.step = step;
        run.id = id;
        run.columns.clear();
        run.started = Instant::now();
        s.status = Status::Running;
        effects.request(
            Target::Own,
            DbRequest::Execute {
                id,
                sql: s.sql.clone(),
                max_rows: MAX_ROWS,
                binds: Vec::new(),
            },
        );
    }

    /// The running step ended; the next one starts, or the run is over.
    fn step_ended(&mut self, failed: Option<String>, effects: &mut Effects) {
        let Some(run) = self.state.run.as_ref() else {
            return;
        };
        let (step, elapsed, cancelled) = (run.step, run.started.elapsed(), run.cancelled);
        self.state.ended = Some(step);
        if let Some(s) = self.state.steps.get_mut(step) {
            match &failed {
                Some(e) => s.status = Status::Failed(elapsed, e.clone()),
                None => {
                    s.status = Status::Done(elapsed);
                    effects.executed.push(s.sql.clone());
                }
            }
        }
        if failed.is_none() && !cancelled && step + 1 < self.state.steps.len() {
            self.send_step(step + 1, effects);
            return;
        }
        let Some(run) = self.state.run.take() else {
            return;
        };
        for s in &mut self.state.steps[step + 1..] {
            s.status = Status::Skipped;
        }
        let (label, secs) = (run.label, run.began.elapsed().as_secs_f64());
        self.state.outcome = Some(match (failed, cancelled) {
            (_, true) => (format!("{label}: cancelled after {secs:.1} s"), false),
            (Some(e), _) => (format!("{label}: failed after {secs:.1} s: {e}"), false),
            (None, _) => (format!("{label}: done in {secs:.1} s"), true),
        });
    }

    pub fn on_event(&mut self, event: DbEvent, dialect: Dialect) -> Effects {
        let mut effects = Effects::default();
        let running = self.state.run.as_ref().map(|r| r.id);
        let running_step = self.state.run.as_ref().map(|r| r.step);
        match event {
            DbEvent::TaskDone { id, result } if self.state.loading == Some(id) => {
                self.state.loading = None;
                match result.map(|any| any.downcast::<Facts>()) {
                    Ok(Ok(facts)) => {
                        self.state.facts = Some(*facts);
                        self.state.load_error = None;
                    }
                    Ok(Err(_)) => self.state.load_error = Some("Unexpected answer".into()),
                    Err(e) => self.state.load_error = Some(e.to_string()),
                }
            }
            DbEvent::QueryStarted { id, columns } if running == Some(id) => {
                if let Some(run) = self.state.run.as_mut() {
                    run.columns = columns.into_iter().map(|c| c.name).collect();
                }
            }
            DbEvent::Rows { id, rows, .. } if running == Some(id) => {
                // A query's answer ends with its rows (no more are fetched).
                let columns = self
                    .state
                    .run
                    .as_ref()
                    .map(|r| r.columns.clone())
                    .unwrap_or_default();
                let (lines, error) = answer(&columns, &rows);
                if let Some(step) = running_step {
                    self.add_messages(step, lines);
                }
                self.step_ended(error, &mut effects);
                if self.state.run.is_none() {
                    effects.extend(self.load(dialect));
                }
            }
            DbEvent::Executed { id, .. } if running == Some(id) => {
                self.step_ended(None, &mut effects);
                if self.state.run.is_none() {
                    effects.extend(self.load(dialect));
                }
            }
            DbEvent::Failed { id, error, .. } if running == Some(id) => {
                self.step_ended(Some(error.to_string()), &mut effects);
                effects.extend(self.load(dialect));
            }
            DbEvent::Error(error) | DbEvent::ConnectFailed(error) if running.is_some() => {
                self.step_ended(Some(error.to_string()), &mut effects);
            }
            DbEvent::Notice(text) => self.message(dialect, running_step, vec![text]),
            DbEvent::Output(lines) => self.message(dialect, running_step, lines),
            _ => {}
        }
        effects
    }

    /// Notices and output go with their statement. PostgreSQL's notices and
    /// MySQL's warnings come after the event that ends it (when the next
    /// statement may run already); Oracle's output and SQL Server's
    /// messages before.
    fn message(&mut self, dialect: Dialect, running: Option<usize>, lines: Vec<String>) {
        let trailing = matches!(dialect.kind(), DbKind::Postgres | DbKind::MySql);
        if let Some(step) = if trailing { self.state.ended } else { running } {
            self.add_messages(step, lines);
        }
    }

    fn add_messages(&mut self, step: usize, lines: Vec<String>) {
        if let Some(s) = self.state.steps.get_mut(step) {
            s.messages.extend(lines);
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        let dialect = cx.dialect;
        if !cx.connected {
            ui.centered_and_justified(|ui| ui.weak("Connect to see the object's state"));
            return effects;
        }
        if !self.state.requested {
            effects.extend(self.load(dialect));
        }
        let task = self.task(dialect);
        let statements = self.statements(dialect);
        let running = self.state.run.is_some();
        let ready = matches!(&statements, Ok(s) if !s.is_empty());

        // Header: the object, and running.
        ui.horizontal(|ui| {
            let t = self.object.object_type;
            if let Some(icon) = Icon::for_object_type(t.dictionary_name()) {
                ui.add(icon.sized(ui, pythia_ui::icons::IconSize::Small));
            }
            ui.label(
                RichText::new(dialect.qualified(&self.object.owner, &self.object.name)).strong(),
            );
            ui.weak(t.dictionary_name().to_lowercase());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    let cancelling = self.state.run.as_ref().is_some_and(|r| r.cancelled);
                    let stop = egui::Button::image_and_text(Icon::Stop.image(ui), "Break");
                    if ui
                        .add_enabled(!cancelling, stop)
                        .on_hover_text("Cancel the statement running; the rest don't run")
                        .clicked()
                    {
                        if let Some(run) = self.state.run.as_mut() {
                            run.cancelled = true;
                        }
                        effects.cancel = true;
                    }
                } else {
                    let run = egui::Button::image_and_text(Icon::Execute.image(ui), "Run");
                    let response = ui.add_enabled(ready && !cx.read_only, run);
                    let response = if cx.read_only {
                        response.on_disabled_hover_text("The connection is read-only")
                    } else {
                        response.on_hover_text(
                            "Run the statements below on this window's own session, \
                             which commits each",
                        )
                    };
                    if response.clicked()
                        && let (Ok(s), Some(task)) = (&statements, task)
                    {
                        if cx.production {
                            self.state.confirm = true;
                        } else {
                            effects.extend(self.start(task, dialect, s.clone()));
                        }
                    }
                }
                if ui
                    .add_enabled(ready, egui::Button::new("Open in SQL window"))
                    .clicked()
                    && let (Some(connection), Ok(s)) = (cx.connection.clone(), &statements)
                {
                    effects.open.push(OpenRequest::Sql {
                        connection,
                        text: script(dialect, s),
                        run: false,
                    });
                }
                let busy = self.is_busy();
                let reload = ui
                    .add_enabled_ui(!busy, |ui| {
                        pythia_ui::icons::icon_button(ui, Icon::Refresh, "Read the state again")
                    })
                    .inner;
                if reload.clicked() {
                    effects.extend(self.load(dialect));
                }
                if self.state.loading.is_some() {
                    ui.spinner();
                }
            });
        });
        ui.separator();

        // The state beside the task, its options and its cost.
        ui.columns(2, |columns| {
            self.state_ui(&mut columns[0], cx);
            if let Some(task) = task {
                self.task_ui(&mut columns[1], dialect, task);
            } else {
                columns[1].weak(format!("No maintenance tasks on {}", dialect.name()));
            }
        });
        ui.add_space(6.0);
        ui.separator();

        ui.label(RichText::new("Statements").strong());
        match &statements {
            Err(why) => {
                ui.label(RichText::new(why).color(ui.visuals().warn_fg_color));
            }
            Ok(s) => code(ui, cx, &script(dialect, s)),
        }
        self.run_ui(ui);
        if running {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        self.confirm_ui(ui.ctx(), cx, &statements, &mut effects);
        effects
    }

    fn state_ui(&self, ui: &mut Ui, cx: &DocContext) {
        ui.label(RichText::new("State").strong());
        ui.add_space(2.0);
        if let Some(e) = &self.state.load_error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        let Some(facts) = &self.state.facts else {
            ui.spinner();
            return;
        };
        egui::Grid::new(("maintenance-state", cx.id))
            .num_columns(2)
            .spacing([16.0, 4.0])
            .show(ui, |ui| {
                for fact in &facts.state {
                    ui.weak(&fact.label);
                    if fact.warn {
                        ui.label(RichText::new(&fact.value).color(ui.visuals().warn_fg_color));
                    } else {
                        ui.label(&fact.value);
                    }
                    ui.end_row();
                }
            });
    }

    fn task_ui(&mut self, ui: &mut Ui, dialect: Dialect, task: Task) {
        ui.label(RichText::new("Task").strong());
        ui.add_space(2.0);
        let running = self.state.run.is_some();
        ui.add_enabled_ui(!running, |ui| {
            for &t in tasks::tasks(dialect, self.object.object_type) {
                if ui
                    .radio(t == task, t.label(dialect))
                    .on_hover_text(t.purpose(dialect))
                    .clicked()
                {
                    self.task = Some(t);
                }
            }
        });
        ui.add_space(4.0);
        ui.weak(task.purpose(dialect));
        ui.add_space(6.0);
        let empty = Facts::default();
        let facts = self.state.facts.as_ref().unwrap_or(&empty);
        ui.add_enabled_ui(!running, |ui| {
            options_ui(ui, dialect, &self.object, task, &mut self.options, facts);
        });
        ui.add_space(6.0);
        let cost = tasks::impact(dialect, task, &self.options, facts);
        let color = if tasks::blocks(task, &self.options, facts) {
            ui.visuals().warn_fg_color
        } else {
            ui.visuals().text_color()
        };
        ui.label(RichText::new(cost).color(color));
    }

    fn run_ui(&self, ui: &mut Ui) {
        if self.state.steps.is_empty() {
            return;
        }
        ui.add_space(6.0);
        let green = Color32::from_rgb(0x2f, 0xa0, 0x4c);
        if let Some((text, ok)) = &self.state.outcome {
            let color = if *ok {
                green
            } else {
                ui.visuals().error_fg_color
            };
            ui.label(RichText::new(text).color(color));
        }
        egui::ScrollArea::vertical()
            .id_salt("maintenance-run")
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for step in &self.state.steps {
                    ui.horizontal(|ui| {
                        let (mark, color, time) = match &step.status {
                            Status::Waiting => ("·", ui.visuals().weak_text_color(), None),
                            Status::Running => ("…", ui.visuals().text_color(), None),
                            Status::Done(d) => ("✔", green, Some(*d)),
                            Status::Failed(d, _) => ("✖", ui.visuals().error_fg_color, Some(*d)),
                            Status::Skipped => ("–", ui.visuals().weak_text_color(), None),
                        };
                        ui.label(RichText::new(mark).color(color));
                        let first = step.sql.lines().find(|l| {
                            let l = l.trim();
                            !l.is_empty() && l != "begin"
                        });
                        ui.label(RichText::new(first.unwrap_or_default().trim()).monospace());
                        match (&step.status, time) {
                            (Status::Running, _) => {
                                if let Some(run) = &self.state.run {
                                    ui.weak(format!(
                                        "{:.1} s",
                                        run.started.elapsed().as_secs_f64()
                                    ));
                                }
                                ui.spinner();
                            }
                            (_, Some(d)) => {
                                ui.weak(format!("{:.1} s", d.as_secs_f64()));
                            }
                            (Status::Skipped, _) => {
                                ui.weak("not run");
                            }
                            _ => {}
                        }
                    });
                    if let Status::Failed(_, e) = &step.status {
                        ui.indent(("failed", &step.sql), |ui| {
                            ui.colored_label(ui.visuals().error_fg_color, e);
                        });
                    }
                    if !step.messages.is_empty() {
                        ui.indent(("messages", &step.sql), |ui| {
                            for m in &step.messages {
                                ui.label(RichText::new(m).small());
                            }
                        });
                    }
                }
            });
    }

    fn confirm_ui(
        &mut self,
        ctx: &egui::Context,
        cx: &DocContext,
        statements: &Result<Vec<String>, String>,
        effects: &mut Effects,
    ) {
        if !self.state.confirm {
            return;
        }
        let mut decision = None;
        let modal =
            egui::Modal::new(egui::Id::new(("maintenance-confirm", cx.id))).show(ctx, |ui| {
                ui.set_max_width(400.0);
                ui.heading("Production connection");
                ui.add_space(6.0);
                let n = statements.as_ref().map_or(0, Vec::len);
                ui.label(format!(
                    "Run {n} statement{} on {}?",
                    if n == 1 { "" } else { "s" },
                    cx.connection.as_deref().unwrap_or("the connection")
                ));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Run").clicked() {
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
        if let Some(yes) = decision {
            self.state.confirm = false;
            if yes && let (Ok(s), Some(task)) = (statements, self.task(cx.dialect)) {
                effects.extend(self.start(task, cx.dialect, s.clone()));
            }
        }
    }

    #[cfg(test)]
    pub fn facts_for_test(&self) -> Option<&Facts> {
        self.state.facts.as_ref()
    }

    #[cfg(test)]
    pub fn statements_for_test(&self, dialect: Dialect) -> Result<Vec<String>, String> {
        self.statements(dialect)
    }

    #[cfg(test)]
    pub fn run_for_test(&mut self, dialect: Dialect) -> Effects {
        let statements = self.statements(dialect).expect("statements");
        let task = self.task(dialect).expect("a task");
        self.start(task, dialect, statements)
    }

    /// How the last run ended, and each statement's messages.
    #[cfg(test)]
    pub fn outcome_for_test(&self) -> (Option<(String, bool)>, Vec<Vec<String>>) {
        (
            self.state.outcome.clone(),
            self.state
                .steps
                .iter()
                .map(|s| s.messages.clone())
                .collect(),
        )
    }
}

/// The options the task has.
fn options_ui(
    ui: &mut Ui,
    dialect: Dialect,
    object: &ObjectRef,
    task: Task,
    o: &mut Options,
    facts: &Facts,
) {
    let index = object.object_type == ObjectType::Index;
    let online = |ui: &mut Ui, o: &mut Options| {
        let possible = tasks::online_possible(task, facts);
        let why = match (dialect.kind(), task) {
            (DbKind::Oracle, Task::Move) => "Needs Oracle 12.2 and Enterprise Edition",
            _ => "Needs Enterprise Edition",
        };
        let mut on = o.online && possible;
        let response = ui.add_enabled(possible, egui::Checkbox::new(&mut on, "Online"));
        let response = response.on_disabled_hover_text(why);
        if possible && response.changed() {
            o.online = on;
        }
    };
    match task {
        Task::GatherStats => {
            if !index {
                ui.checkbox(&mut o.cascade, "Indexes too");
            }
            ui.horizontal(|ui| {
                ui.label("Sample");
                let mut auto = o.sample.is_none();
                if ui.radio_value(&mut auto, true, "automatic").clicked() {
                    o.sample = None;
                }
                if ui.radio_value(&mut auto, false, "percent").clicked() && o.sample.is_none() {
                    o.sample = Some(10);
                }
                if let Some(p) = o.sample.as_mut() {
                    ui.add(egui::DragValue::new(p).range(1..=100).suffix(" %"));
                }
            })
            .response
            .on_hover_text(
                "Automatic (AUTO_SAMPLE_SIZE) is as accurate as reading everything and \
                 much faster; a percent is for comparing with older statistics",
            );
            ui.checkbox(&mut o.invalidate_now, "Invalidate cursors at once")
                .on_hover_text(
                    "Statements on the table are parsed again right away with the new \
                     statistics, instead of over a while",
                );
            force_ui(ui, o, facts);
        }
        Task::DeleteStats => force_ui(ui, o, facts),
        Task::LockStats | Task::UnlockStats => {}
        Task::RebuildUnusable => online(ui, o),
        Task::RebuildIndex => {
            online(ui, o);
            if facts.partitioned {
                ui.checkbox(&mut o.unusable_only, "Only unusable partitions");
            }
        }
        Task::Shrink => {
            ui.checkbox(&mut o.compact, "Compact only")
                .on_hover_text("Moves the rows together but keeps the space (no lock at the end)");
            if !index {
                ui.checkbox(&mut o.cascade, "Indexes too");
            }
        }
        Task::Move => {
            online(ui, o);
            ui.horizontal(|ui| {
                ui.label("Tablespace");
                ui.add(
                    egui::TextEdit::singleline(&mut o.tablespace)
                        .hint_text("the same")
                        .desired_width(160.0),
                );
            });
        }
        Task::Vacuum => {
            ui.checkbox(&mut o.analyze, "Analyze")
                .on_hover_text("Update the planner statistics too");
            ui.checkbox(&mut o.freeze, "Freeze")
                .on_hover_text("Freeze all rows, as against transaction ID wraparound");
            ui.checkbox(&mut o.full, "Full")
                .on_hover_text("Rewrite the table compactly, giving space back to the system");
            ui.checkbox(&mut o.verbose, "Verbose")
                .on_hover_text("Report what it did, table by table and index by index");
        }
        Task::Analyze if dialect.kind() == DbKind::Postgres => {
            ui.checkbox(&mut o.verbose, "Verbose");
        }
        Task::Analyze | Task::Optimize | Task::Check | Task::ReorganizeIndexes => {}
        Task::Reindex => {
            let possible = tasks::concurrently_possible(facts);
            let mut on = o.concurrently && possible;
            let response = ui
                .add_enabled(possible, egui::Checkbox::new(&mut on, "Concurrently"))
                .on_disabled_hover_text("Needs PostgreSQL 12");
            if possible && response.changed() {
                o.concurrently = on;
            }
        }
        Task::UpdateStatistics => {
            ui.checkbox(&mut o.fullscan, "Full scan")
                .on_hover_text("Read every row instead of a sample");
        }
        Task::RebuildIndexes => online(ui, o),
    }
}

/// Gathering or deleting locked statistics.
fn force_ui(ui: &mut Ui, o: &mut Options, facts: &Facts) {
    if facts.stats_locked {
        ui.checkbox(&mut o.force, "Force")
            .on_hover_text("The statistics are locked: without force, Oracle refuses (ORA-20005)");
    }
}

/// What came back as rows: MySQL's `Msg_type: Msg_text` lines (an error
/// among them fails the statement), other answers cell by cell.
fn answer(columns: &[String], rows: &[Vec<pythia_db::Cell>]) -> (Vec<String>, Option<String>) {
    let at = |name: &str| columns.iter().position(|c| c.eq_ignore_ascii_case(name));
    let mut lines = Vec::new();
    let mut error = None;
    match (at("Msg_type"), at("Msg_text")) {
        (Some(kind), Some(text)) => {
            for row in rows {
                let cell = |i: usize| row.get(i).cloned().flatten().unwrap_or_default();
                let (kind, text) = (cell(kind), cell(text));
                if kind.eq_ignore_ascii_case("error") && error.is_none() {
                    error = Some(text.clone());
                }
                lines.push(format!("{kind}: {text}"));
            }
        }
        _ => {
            for row in rows {
                lines.push(
                    row.iter()
                        .map(|c| c.as_deref().unwrap_or("NULL"))
                        .collect::<Vec<_>>()
                        .join(" · "),
                );
            }
        }
    }
    (lines, error)
}

/// The statements as a script the database's SQL window runs.
fn script(dialect: Dialect, statements: &[String]) -> String {
    statements
        .iter()
        .map(|s| {
            let block = s.trim_start().starts_with("begin");
            match dialect.kind() {
                DbKind::Oracle if block => format!("{s}\n/"),
                DbKind::SqlServer => format!("{s};\ngo"),
                _ => format!("{s};"),
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The statements, highlighted, in a frame like the object forms' preview.
fn code(ui: &mut Ui, cx: &DocContext, text: &str) {
    let theme = pythia_ui::SyntaxTheme::for_visuals(ui.visuals());
    let font = egui::FontId::monospace(cx.settings.editor.font_size.min(14.0));
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(4.0)
        .inner_margin(6.0)
        .show(ui, |ui| {
            egui::ScrollArea::both()
                .id_salt("maintenance-statements")
                .auto_shrink([false, true])
                .max_height(160.0)
                .show(ui, |ui| {
                    let job = pythia_ui::highlight(text, font, &theme);
                    ui.add(egui::Label::new(job).extend());
                });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mysql_answers_are_messages_and_errors() {
        let columns: Vec<String> = ["Table", "Op", "Msg_type", "Msg_text"]
            .map(String::from)
            .to_vec();
        let row = |kind: &str, text: &str| {
            vec![
                Some("shop.orders".to_owned()),
                Some("optimize".to_owned()),
                Some(kind.to_owned()),
                Some(text.to_owned()),
            ]
        };
        let (lines, error) = answer(
            &columns,
            &[
                row(
                    "note",
                    "Table does not support optimize, doing recreate + analyze instead",
                ),
                row("status", "OK"),
            ],
        );
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1], "status: OK");
        assert_eq!(error, None);
        let (_, error) = answer(
            &columns,
            &[
                row("Error", "Table 'shop.nope' doesn't exist"),
                row("status", "Operation failed"),
            ],
        );
        assert_eq!(error.as_deref(), Some("Table 'shop.nope' doesn't exist"));
    }

    #[test]
    fn scripts_end_statements_the_database_way() {
        let s = vec![
            "begin\n  dbms_stats.gather_table_stats(ownname => 'A', tabname => 'T');\nend;"
                .to_owned(),
            "alter index A.I rebuild".to_owned(),
        ];
        assert_eq!(
            script(Dialect::ORACLE, &s),
            "begin\n  dbms_stats.gather_table_stats(ownname => 'A', tabname => 'T');\nend;\n/\
             \n\nalter index A.I rebuild;"
        );
        assert_eq!(
            script(Dialect::SQLSERVER, &["update statistics dbo.t".to_owned()]),
            "update statistics dbo.t;\ngo"
        );
    }
}
