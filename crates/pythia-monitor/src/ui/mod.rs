//! egui views and the [`Monitor`] driver.
//!
//! The driver never touches a connection. Views ask for data; the driver turns
//! that into [`MonitorRequest`]s (a token plus a closure) which the host sends
//! as [`pythia_db::DbRequest::Task`] (Oracle) or [`pythia_db::DbRequest::Run`]
//! (MySQL) on a dedicated monitoring session, and feeds the results back with
//! [`Monitor::on_task_done`].
//!
//! ```ignore
//! let out = monitor.ui(ui);
//! for req in out.requests {
//!     let id = app.request_id();
//!     app.monitor_tasks.insert(id, req.token);
//!     monitor_session.send(match req.task {
//!         MonitorTask::Oracle(task) => DbRequest::Task { id, task },
//!         MonitorTask::Client(task) => DbRequest::Run { id, task },
//!     });
//! }
//! // later, on DbEvent::TaskDone { id, result }:
//! if let Some(token) = app.monitor_tasks.remove(&id) { monitor.on_task_done(token, result); }
//! ```

mod awr;
mod dashboard;
mod detail;
mod mysql;
mod sessions;
mod sql_detail;
pub mod style;
mod trace;

use std::collections::HashMap;
use std::path::PathBuf;

use egui::{RichText, Ui};
use pythia_db::{DbError, DbKind, Dialect, RunFn, TaskFn, TaskResult};

use crate::queries::{self, Capabilities};

pub use crate::queries::DetailTab;
pub use awr::AwrView;
pub use dashboard::{DashboardView, Refresh};
pub use detail::SessionDetailView;
pub use mysql::MySqlDashboardView;
pub use sessions::{FieldFilter, SessionsView};
pub use sql_detail::SqlDetailView;
pub use trace::TraceView;

/// Identifies a request inside one [`Monitor`].
pub type Token = u64;

/// Work for the monitoring session.
pub struct MonitorRequest {
    pub token: Token,
    pub task: MonitorTask,
}

/// How a request runs on the session.
pub enum MonitorTask {
    /// On the Oracle connection (`DbRequest::Task`).
    Oracle(TaskFn),
    /// On any database's [`pythia_db::Client`] (`DbRequest::Run`).
    Client(RunFn),
}

/// Things the host should do on the monitor's behalf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MonitorAction {
    /// Open a file (an HTML report) with the system's default application.
    OpenPath(PathBuf),
    /// Put text on the clipboard (the monitor also copies it itself via egui).
    CopyText(String),
    /// Have the host's AI assistant analyze slow SQL or a report.
    Analyze(Analysis),
    /// Open a database object in the IDE: a program window (at
    /// `subprogram`) for PL/SQL, else its object view. `object_type` as in
    /// `ALL_OBJECTS`, e.g. `PACKAGE`.
    OpenObject {
        owner: String,
        name: String,
        object_type: String,
        subprogram: Option<String>,
    },
    /// Open a SQL window with this text, on the monitor's connection.
    OpenSql(String),
}

/// Slow SQL or a report to have analyzed, with what the monitor knows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Analysis {
    /// What it is, e.g. "top SQL 7h35uxf5uhmm1".
    pub title: String,
    /// The statement, if it is about one.
    pub sql: Option<String>,
    /// Its SQL_ID, for the plan in the cursor cache.
    pub sql_id: Option<String>,
    /// Statistics, waits, plans or report text: metadata.
    pub details: String,
    /// Captured bind values: data, sent only if the user allows it.
    pub binds: Vec<String>,
}

#[derive(Default)]
pub struct MonitorOutput {
    pub requests: Vec<MonitorRequest>,
    pub actions: Vec<MonitorAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorTab {
    #[default]
    Dashboard,
    Sessions,
    Awr,
    Trace,
    /// An open session detail (index into the detail list).
    Detail(usize),
}

/// What a view wants fetched or done. Turned into tasks by the driver.
pub(crate) enum Want {
    Capabilities,
    Dashboard,
    /// With table statistics or without.
    MySqlDashboard {
        storage: bool,
    },
    Sessions {
        background: bool,
    },
    /// The header of session details and the parts asked for.
    Detail {
        sid: i64,
        serial: i64,
        parts: Vec<DetailTab>,
    },
    Kill {
        sid: i64,
        serial: i64,
        immediate: bool,
    },
    CancelSql {
        sid: i64,
        serial: i64,
    },
    /// Disconnect once its transaction has ended.
    Disconnect {
        sid: i64,
        serial: i64,
    },
    /// A session's open transaction, for the kill dialog.
    Transaction {
        sid: i64,
        serial: i64,
    },
    /// A statement in the shared pool, with one of its children.
    SqlDetail {
        sql_id: String,
        child: Option<i64>,
    },
    /// Open a statement's details tab (handled by the driver, no task).
    OpenSqlDetail {
        sql_id: String,
    },
    Trace {
        sid: i64,
        serial: i64,
        on: bool,
    },
    AwrSnapshots,
    AwrReport {
        dbid: i64,
        instance: i64,
        begin: i64,
        end: i64,
        html: bool,
    },
    AshReport {
        minutes: i64,
        html: bool,
    },
    CreateSnapshot,
    TraceFiles {
        filter: String,
    },
    ReadTrace {
        home: String,
        name: String,
    },
    SessionTraceFile {
        sid: i64,
    },
    /// Switch to the blocking tree (handled by the driver, no task).
    ShowBlocking,
    /// Switch to the session list, filtered to one user, program, machine
    /// or wait class.
    ShowSessions(Option<FieldFilter>),
    /// Open a session detail tab, on one of its tabs (handled by the
    /// driver, no task).
    OpenDetail {
        sid: i64,
        serial: i64,
        tab: Option<DetailTab>,
    },
    /// Hand to the host's assistant (no task).
    Analyze(Analysis),
    /// Something for the host to do (no task).
    Act(MonitorAction),
}

/// An open details tab: of a session or of a statement.
enum DetailView {
    Session(Box<SessionDetailView>),
    Sql(Box<SqlDetailView>),
}

impl DetailView {
    fn title(&self) -> String {
        match self {
            DetailView::Session(d) => d.title(),
            DetailView::Sql(d) => d.title(),
        }
    }
}

/// Result routing for a token.
enum Job {
    Capabilities,
    Dashboard,
    MySqlDashboard,
    Sessions,
    Detail(i64, i64),
    SqlDetail(String),
    Control(String),
    Transaction(i64, i64),
    AwrSnapshots,
    Report { html: bool, title: String },
    CreateSnapshot,
    TraceFiles,
    ReadTrace(String),
    SessionTraceFile,
}

/// Collected by views while drawing.
#[derive(Default)]
pub(crate) struct Wants {
    list: Vec<Want>,
    /// Views offer "Analyze with AI".
    pub analyze: bool,
}

impl Wants {
    pub fn push(&mut self, w: Want) {
        self.list.push(w);
    }
}

pub struct Monitor {
    /// The connection's database: MySQL has the dashboard only.
    dialect: Dialect,
    tab: MonitorTab,
    show_tabs: bool,
    caps: Option<Result<Capabilities, String>>,
    caps_pending: bool,
    pub dashboard: DashboardView,
    pub mysql: MySqlDashboardView,
    pub sessions: SessionsView,
    pub awr: AwrView,
    pub trace: TraceView,
    details: Vec<DetailView>,
    jobs: HashMap<Token, Job>,
    next_token: Token,
    toast: Option<(String, bool, f64)>,
    /// Follow-up requests produced while handling results.
    deferred: Vec<Want>,
    /// The host has an AI assistant: views offer to analyze with it.
    analyze: bool,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new(MonitorTab::Dashboard)
    }
}

impl Monitor {
    pub fn new(tab: MonitorTab) -> Self {
        Self {
            dialect: Dialect::ORACLE,
            tab,
            show_tabs: true,
            caps: None,
            caps_pending: false,
            dashboard: DashboardView::default(),
            mysql: MySqlDashboardView::default(),
            sessions: SessionsView::default(),
            awr: AwrView::default(),
            trace: TraceView::default(),
            details: Vec::new(),
            jobs: HashMap::new(),
            next_token: 1,
            toast: None,
            deferred: Vec::new(),
            analyze: false,
        }
    }

    /// Offers "Analyze with AI" on slow SQL, sessions, reports and traces;
    /// chosen ones come back as [`MonitorAction::Analyze`].
    pub fn set_analyze(&mut self, on: bool) {
        self.analyze = on;
    }

    /// The connection's database. A MySQL connection gets its dashboard,
    /// whatever the tab.
    pub fn set_dialect(&mut self, dialect: Dialect) {
        self.dialect = dialect;
    }

    fn is_mysql(&self) -> bool {
        self.dialect.kind() == DbKind::MySql
    }

    /// Hide the tab bar to host a single view as its own document.
    pub fn with_tab_bar(mut self, show: bool) -> Self {
        self.show_tabs = show;
        self
    }

    pub fn tab(&self) -> MonitorTab {
        self.tab
    }

    pub fn set_tab(&mut self, tab: MonitorTab) {
        self.tab = tab;
    }

    /// Opens (or focuses) the detail tab of a session.
    pub fn open_session(&mut self, sid: i64, serial: i64) {
        self.deferred.push(Want::OpenDetail {
            sid,
            serial,
            tab: None,
        });
    }

    pub fn capabilities(&self) -> Option<&Capabilities> {
        self.caps.as_ref().and_then(|c| c.as_ref().ok())
    }

    /// True if `token` belongs to this monitor and is still pending.
    pub fn owns(&self, token: Token) -> bool {
        self.jobs.contains_key(&token)
    }

    /// Title for a host tab.
    pub fn title(&self) -> String {
        if self.is_mysql() {
            return "Dashboard".into();
        }
        match self.tab {
            MonitorTab::Dashboard => "Dashboard".into(),
            MonitorTab::Sessions => "Sessions".into(),
            MonitorTab::Awr => "AWR / ASH".into(),
            MonitorTab::Trace => "Trace analyzer".into(),
            MonitorTab::Detail(i) => self.details.get(i).map_or("Details".into(), |d| d.title()),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) -> MonitorOutput {
        let now = ui.input(|i| i.time);
        let mut wants = Wants {
            list: std::mem::take(&mut self.deferred),
            analyze: self.analyze,
        };
        let mut out = MonitorOutput::default();

        if self.is_mysql() {
            self.mysql.ui(ui, now, &mut wants);
            self.handle(wants, &mut out);
            return out;
        }
        if self.caps.is_none() && !self.caps_pending {
            wants.push(Want::Capabilities);
        }

        if self.show_tabs {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, MonitorTab::Dashboard, "Dashboard");
                ui.selectable_value(&mut self.tab, MonitorTab::Sessions, "Sessions");
                ui.selectable_value(&mut self.tab, MonitorTab::Awr, "AWR / ASH");
                ui.selectable_value(&mut self.tab, MonitorTab::Trace, "Trace analyzer");
                let mut close = None;
                for (i, d) in self.details.iter().enumerate() {
                    ui.selectable_value(&mut self.tab, MonitorTab::Detail(i), d.title());
                    if ui.small_button("×").on_hover_text("Close").clicked() {
                        close = Some(i);
                    }
                }
                if let Some(i) = close {
                    self.details.remove(i);
                    self.tab = match self.tab {
                        MonitorTab::Detail(j) if j == i => MonitorTab::Sessions,
                        MonitorTab::Detail(j) if j > i => MonitorTab::Detail(j - 1),
                        t => t,
                    };
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some((text, error, until)) = &self.toast {
                        if now < *until {
                            let color = if *error {
                                ui.visuals().error_fg_color
                            } else {
                                style::Palette::GREEN
                            };
                            ui.label(RichText::new(text).color(color));
                            ui.ctx()
                                .request_repaint_after(std::time::Duration::from_secs_f64(
                                    until - now,
                                ));
                        } else {
                            self.toast = None;
                        }
                    }
                    if let Some(Err(e)) = &self.caps {
                        ui.colored_label(ui.visuals().error_fg_color, "⚠")
                            .on_hover_text(e);
                    }
                });
            });
            ui.separator();
        }

        let caps = self.caps.as_ref().and_then(|c| c.as_ref().ok());
        match self.tab {
            MonitorTab::Dashboard => self.dashboard.ui(ui, now, &mut wants),
            MonitorTab::Sessions => self.sessions.ui(ui, now, caps, &mut wants),
            MonitorTab::Awr => self.awr.ui(ui, caps, self.caps_pending, &mut wants),
            MonitorTab::Trace => self.trace.ui(ui, caps, &mut wants),
            MonitorTab::Detail(i) => match self.details.get_mut(i) {
                Some(DetailView::Session(d)) => d.ui(ui, caps, &mut wants),
                Some(DetailView::Sql(d)) => d.ui(ui, &mut wants),
                None => self.tab = MonitorTab::Sessions,
            },
        }
        self.handle(wants, &mut out);
        out
    }

    /// Turns what the views want into requests and actions.
    fn handle(&mut self, wants: Wants, out: &mut MonitorOutput) {
        for want in wants.list {
            if let Want::Analyze(analysis) = want {
                out.actions.push(MonitorAction::Analyze(analysis));
                continue;
            }
            if let Want::Act(action) = want {
                out.actions.push(action);
                continue;
            }
            if let Want::ShowSessions(filter) = want {
                self.tab = MonitorTab::Sessions;
                self.sessions.show_table(filter);
                continue;
            }
            if let Want::ShowBlocking = want {
                self.tab = MonitorTab::Sessions;
                self.sessions.show_blocking();
                continue;
            }
            if let Want::OpenDetail { sid, serial, tab } = want {
                let found = self.details.iter().position(
                    |d| matches!(d, DetailView::Session(s) if s.sid == sid && s.serial == serial),
                );
                let idx = found.unwrap_or_else(|| {
                    let view = SessionDetailView::new(sid, serial);
                    self.details.push(DetailView::Session(Box::new(view)));
                    self.details.len() - 1
                });
                if let (Some(tab), DetailView::Session(d)) = (tab, &mut self.details[idx]) {
                    d.show_tab(tab);
                }
                self.tab = MonitorTab::Detail(idx);
                continue;
            }
            if let Want::OpenSqlDetail { sql_id } = want {
                let found = self
                    .details
                    .iter()
                    .position(|d| matches!(d, DetailView::Sql(s) if s.sql_id == sql_id));
                let idx = found.unwrap_or_else(|| {
                    self.details
                        .push(DetailView::Sql(Box::new(SqlDetailView::new(sql_id))));
                    self.details.len() - 1
                });
                self.tab = MonitorTab::Detail(idx);
                continue;
            }
            if let Some(req) = self.task(want) {
                out.requests.push(req);
            }
        }
        out.actions.append(&mut self.awr.actions);
    }

    /// The open session details.
    fn session_details(&mut self) -> impl Iterator<Item = &mut SessionDetailView> {
        self.details.iter_mut().filter_map(|d| match d {
            DetailView::Session(s) => Some(&mut **s),
            DetailView::Sql(_) => None,
        })
    }

    fn token(&mut self, job: Job) -> Token {
        let t = self.next_token;
        self.next_token += 1;
        self.jobs.insert(t, job);
        t
    }

    fn task(&mut self, want: Want) -> Option<MonitorRequest> {
        fn boxed<T: Send + 'static>(r: Result<T, DbError>) -> TaskResult {
            r.map(|v| Box::new(v) as Box<dyn std::any::Any + Send>)
        }
        if let Want::MySqlDashboard { storage } = want {
            return Some(MonitorRequest {
                token: self.token(Job::MySqlDashboard),
                task: MonitorTask::Client(Box::new(move |c| {
                    boxed(Ok(crate::mysql::snapshot(c, storage)))
                })),
            });
        }
        let (job, task): (Job, TaskFn) = match want {
            Want::Capabilities => {
                self.caps_pending = true;
                (
                    Job::Capabilities,
                    Box::new(|c| boxed(queries::capabilities(c))),
                )
            }
            Want::Dashboard => (
                Job::Dashboard,
                Box::new(|c| boxed(Ok(queries::dashboard_snapshot(c)))),
            ),
            Want::Sessions { background } => (
                Job::Sessions,
                Box::new(move |c| {
                    boxed(queries::sessions(
                        c,
                        queries::SessionFilter {
                            include_background: background,
                        },
                    ))
                }),
            ),
            Want::Detail { sid, serial, parts } => (
                Job::Detail(sid, serial),
                Box::new(move |c| boxed(Ok(queries::session_details(c, sid, serial, &parts)))),
            ),
            Want::Kill {
                sid,
                serial,
                immediate,
            } => (
                Job::Control(format!("Session {sid},{serial} killed")),
                Box::new(move |c| boxed(queries::kill_session(c, sid, serial, immediate))),
            ),
            Want::CancelSql { sid, serial } => (
                Job::Control(format!("SQL of session {sid},{serial} cancelled")),
                Box::new(move |c| boxed(queries::cancel_sql(c, sid, serial))),
            ),
            Want::Disconnect { sid, serial } => (
                Job::Control(format!(
                    "Session {sid},{serial} disconnects once its transaction ends"
                )),
                Box::new(move |c| boxed(queries::disconnect_session(c, sid, serial))),
            ),
            Want::Transaction { sid, serial } => (
                Job::Transaction(sid, serial),
                Box::new(move |c| boxed(queries::transaction(c, sid, serial))),
            ),
            Want::SqlDetail { sql_id, child } => (
                Job::SqlDetail(sql_id.clone()),
                Box::new(move |c| boxed(Ok(queries::sql_detail(c, &sql_id, child)))),
            ),
            Want::Trace { sid, serial, on } => (
                Job::Control(format!(
                    "SQL trace {} for session {sid},{serial}",
                    if on { "enabled" } else { "disabled" }
                )),
                Box::new(move |c| {
                    boxed(if on {
                        queries::trace_enable(c, sid, serial, true, true)
                    } else {
                        queries::trace_disable(c, sid, serial)
                    })
                }),
            ),
            Want::AwrSnapshots => (
                Job::AwrSnapshots,
                Box::new(|c| boxed(queries::awr_snapshots(c, 500))),
            ),
            Want::AwrReport {
                dbid,
                instance,
                begin,
                end,
                html,
            } => (
                Job::Report {
                    html,
                    title: format!("awr_{begin}_{end}"),
                },
                Box::new(move |c| boxed(queries::awr_report(c, dbid, instance, begin, end, html))),
            ),
            Want::AshReport { minutes, html } => (
                Job::Report {
                    html,
                    title: format!("ash_last_{minutes}m"),
                },
                Box::new(move |c| boxed(queries::ash_report(c, minutes, html))),
            ),
            Want::CreateSnapshot => (
                Job::CreateSnapshot,
                Box::new(|c| boxed(queries::awr_create_snapshot(c))),
            ),
            Want::TraceFiles { filter } => (
                Job::TraceFiles,
                Box::new(move |c| boxed(queries::trace_files(c, &filter, 300))),
            ),
            Want::ReadTrace { home, name } => (
                Job::ReadTrace(name.clone()),
                Box::new(move |c| boxed(queries::read_trace_file(c, &home, &name))),
            ),
            Want::SessionTraceFile { sid } => (
                Job::SessionTraceFile,
                Box::new(move |c| boxed(queries::session_trace_file(c, sid))),
            ),
            Want::OpenDetail { .. }
            | Want::OpenSqlDetail { .. }
            | Want::ShowBlocking
            | Want::ShowSessions(_)
            | Want::Analyze(_)
            | Want::Act(_)
            | Want::MySqlDashboard { .. } => return None,
        };
        Some(MonitorRequest {
            token: self.token(job),
            task: MonitorTask::Oracle(task),
        })
    }

    fn toast(&mut self, text: impl Into<String>, error: bool, now: f64) {
        self.toast = Some((text.into(), error, now + 6.0));
    }

    /// Feeds a finished task back. `now` is egui time (`ctx.input(|i| i.time)`).
    pub fn on_task_done(&mut self, token: Token, result: TaskResult, now: f64) {
        let Some(job) = self.jobs.remove(&token) else {
            return;
        };
        fn take<T: 'static>(r: TaskResult) -> Result<T, DbError> {
            r.and_then(|b| {
                b.downcast::<T>()
                    .map(|b| *b)
                    .map_err(|_| DbError::new("unexpected task result"))
            })
        }
        match job {
            Job::Capabilities => {
                self.caps_pending = false;
                self.caps = Some(take::<Capabilities>(result).map_err(|e| e.to_string()));
            }
            Job::Dashboard => self.dashboard.on_snapshot(take(result), now),
            Job::MySqlDashboard => self.mysql.on_snapshot(take(result), now),
            Job::Sessions => self.sessions.on_sessions(take(result)),
            Job::Detail(sid, serial) => {
                let result = take(result);
                if let Some(d) = self
                    .session_details()
                    .find(|d| d.sid == sid && d.serial == serial)
                {
                    d.on_detail(result);
                }
            }
            Job::SqlDetail(sql_id) => {
                let result = take(result);
                let found = self.details.iter_mut().find_map(|d| match d {
                    DetailView::Sql(s) if s.sql_id == sql_id => Some(s),
                    _ => None,
                });
                if let Some(d) = found {
                    d.on_detail(result);
                }
            }
            Job::Transaction(sid, serial) => {
                let result = take::<Option<queries::Transaction>>(result)
                    .map_err(|e| queries::explain_error(&e));
                self.sessions.on_transaction(sid, serial, &result);
                for d in self
                    .session_details()
                    .filter(|d| d.sid == sid && d.serial == serial)
                {
                    d.on_transaction(&result);
                }
            }
            Job::Control(message) => match take::<()>(result) {
                Ok(()) => {
                    self.toast(message, false, now);
                    self.sessions.refresh_now();
                    for d in self.session_details() {
                        d.refresh();
                    }
                }
                Err(e) => self.toast(queries::explain_error(&e), true, now),
            },
            Job::AwrSnapshots => self.awr.on_snapshots(take(result)),
            Job::Report { html, title } => self.awr.on_report(take(result), html, &title),
            Job::CreateSnapshot => match take::<i64>(result) {
                Ok(id) => {
                    self.toast(format!("AWR snapshot {id} created"), false, now);
                    self.awr.reload();
                }
                Err(e) => self.toast(queries::explain_error(&e), true, now),
            },
            Job::TraceFiles => self.trace.on_files(take(result)),
            Job::ReadTrace(name) => self.trace.on_contents(&name, take(result)),
            Job::SessionTraceFile => {
                self.tab = MonitorTab::Trace;
                if let Some(want) = self.trace.on_session_file(take(result)) {
                    self.deferred.push(want);
                }
            }
        }
    }
}

/// Shared helper: a refresh interval selector.
pub(crate) fn refresh_selector(ui: &mut Ui, id: &str, refresh: &mut Refresh) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(refresh.label())
        .width(90.0)
        .show_ui(ui, |ui| {
            for r in Refresh::ALL {
                ui.selectable_value(refresh, r, r.label());
            }
        });
}
