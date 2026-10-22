use std::collections::{BTreeMap, HashMap};

use egui::{Color32, RichText, Ui};
use egui_dock::{DockArea, DockState, NodeIndex, Style as DockStyle, TabViewer};
use pythia_config::{
    ConnectionProfile, Connections as SavedConnections, Paths, Settings, ThemeChoice, load_toml,
    save_toml,
};
use pythia_db::{DbEvent, DbRequest, MetaQuery, MetaResult};
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::browser::Browser;
use crate::commands::{Command, Keymap};
use crate::connections::{Connections, Notify, SessionKey};
use crate::dialogs::{
    CommandPalette, Confirm, ConfirmAction, ConnectDialog, FinderAction, HistoryDialog,
    ObjectFinder, confirm_ui, opens_as_program,
};
use crate::documents::{
    DocContext, DocId, DocKind, Document, Effects, ObjectRef, OpenRequest, Target, request_id,
};
use crate::history::History;
use crate::object_view::ObjectView;
use crate::program_window::ProgramWindow;
use crate::sql_window::SqlWindow;
use crate::{secrets, theme};
use pythia_ui::icons::{Icon, icon_label};

// Named before the app became Pythia; kept so saved workspaces still load.
const WORKSPACE_KEY: &str = "orui-workspace-v2";
const MISSING_CLIENT_HINT: &str = "Oracle Instant Client could not be loaded. Download the \
     Basic or Basic Light package from \
     https://www.oracle.com/database/technologies/instant-client/downloads.html, unpack it and put \
     its directory on LD_LIBRARY_PATH (Linux), PATH (Windows) or DYLD_LIBRARY_PATH (macOS). On \
     Linux it also needs libaio.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileAction {
    Open,
    SaveAs(DocId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Browser,
    Project,
    Log,
    Debugger,
    Doc(DocId),
}

/// Layout and documents, restored on the next start.
#[derive(Serialize, Deserialize)]
struct Workspace {
    dock: DockState<Tab>,
    docs: BTreeMap<DocId, Document>,
    next_doc: u64,
    last_profile: Option<ConnectionProfile>,
    #[serde(default)]
    project: crate::project::Project,
    #[serde(default)]
    marks: crate::browser::Marks,
}

pub struct PythiaApp {
    paths: Option<Paths>,
    settings: Settings,
    saved: SavedConnections,
    keymap: Keymap,
    /// Code font file currently installed.
    loaded_font: Option<String>,
    notify: Notify,

    dock: DockState<Tab>,
    docs: BTreeMap<DocId, Document>,
    next_doc: u64,
    /// The document commands act on: the focused one, or the last focused.
    focused: Option<DocId>,

    conns: Connections,
    browser: Browser,
    project: crate::project::Project,
    /// Main-session requests issued by documents, routed back by id.
    routes: HashMap<u64, DocId>,
    last_profile: Option<ConnectionProfile>,
    /// Connection the connect dialog is currently opening.
    connecting: Option<String>,

    history: History,
    user_reports: crate::reports::UserReports,
    /// "Save as report" dialog: name, category, SQL.
    save_report: Option<(String, String, String)>,
    debug: pythia_debugger::ui::DebugSession,
    /// The running debug call: (document, task request id).
    debug_run: Option<(DocId, u64)>,
    /// Dictionary caches for code assistance, by connection.
    catalogs: HashMap<String, pythia_lsp_core::CatalogCache>,
    catalog_revision: u64,
    /// Catalog task request id → connection.
    catalog_inflight: HashMap<u64, String>,
    /// Requests already sent (or answered) per connection, to avoid repeats.
    catalog_requested: std::collections::HashSet<(String, pythia_lsp_core::CatalogRequest)>,
    log: Vec<String>,
    connect_dialog: ConnectDialog,
    palette: CommandPalette,
    /// The user confirmed running changes on production (one execution).
    production_confirmed: bool,
    /// Recently closed documents, for reopening (newest last).
    closed_docs: Vec<Document>,
    /// Native file dialog running on a helper thread.
    file_dialog: Option<(
        std::sync::mpsc::Receiver<Option<std::path::PathBuf>>,
        FileAction,
    )>,
    settings_dialog: crate::settings_dialog::SettingsDialog,
    finder: ObjectFinder,
    history_dialog: HistoryDialog,
    confirm: Option<Confirm>,
    about_open: bool,
    /// Exit was confirmed; don't ask again on the next close request.
    quit_confirmed: bool,
}

impl PythiaApp {
    pub fn new(cc: &eframe::CreationContext<'_>, paths: Option<Paths>) -> Self {
        let settings: Settings = paths
            .as_ref()
            .map(|p| load_or_default(&p.settings_file()))
            .unwrap_or_default();
        let saved: SavedConnections = paths
            .as_ref()
            .map(|p| load_or_default(&p.connections_file()))
            .unwrap_or_default();
        let history: History = paths
            .as_ref()
            .map(|p| load_or_default(&history_file(p)))
            .unwrap_or_default();
        let user_reports: crate::reports::UserReports = paths
            .as_ref()
            .map(|p| load_or_default(&reports_file(p)))
            .unwrap_or_default();
        let workspace: Option<Workspace> =
            cc.storage.and_then(|s| eframe::get_value(s, WORKSPACE_KEY));

        let mut app = Self {
            paths,
            keymap: Keymap::with_overrides(settings.keymap, &settings.keybindings),
            loaded_font: None,
            settings,
            saved,
            notify: Notify(cc.egui_ctx.clone()),
            dock: DockState::new(vec![]),
            docs: BTreeMap::new(),
            next_doc: 1,
            focused: None,
            conns: Connections::default(),
            browser: Browser::default(),
            project: Default::default(),
            routes: HashMap::new(),
            last_profile: None,
            connecting: None,
            history,
            user_reports,
            save_report: None,
            debug: Default::default(),
            debug_run: None,
            catalogs: HashMap::new(),
            catalog_revision: 0,
            catalog_inflight: HashMap::new(),
            catalog_requested: std::collections::HashSet::new(),
            log: Vec::new(),
            connect_dialog: ConnectDialog::default(),
            palette: CommandPalette::default(),
            production_confirmed: false,
            closed_docs: Vec::new(),
            file_dialog: None,
            settings_dialog: Default::default(),
            finder: ObjectFinder::default(),
            history_dialog: HistoryDialog::default(),
            confirm: None,
            about_open: false,
            quit_confirmed: false,
        };
        match workspace {
            Some(ws) if !ws.docs.is_empty() => {
                app.dock = ws.dock;
                app.docs = ws.docs;
                app.next_doc = ws.next_doc;
                app.last_profile = ws.last_profile;
                app.project = ws.project;
                app.browser.marks = ws.marks;
                let docs = &app.docs;
                app.dock
                    .retain_tabs(|tab| !matches!(tab, Tab::Doc(id) if !docs.contains_key(id)));
            }
            _ => app.reset_layout(),
        }
        app.focused = app.docs.keys().next().copied();

        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        theme::install(&cc.egui_ctx);
        app.apply_settings(&cc.egui_ctx);
        app.log("Pythia started");
        app
    }

    fn reset_layout(&mut self) {
        if self.docs.is_empty() {
            let id = self.new_doc_id();
            self.docs.insert(
                id,
                Document {
                    connection: None,
                    kind: DocKind::Sql(SqlWindow::new("SQL 1".into(), String::new())),
                },
            );
        }
        let tabs: Vec<Tab> = self.docs.keys().map(|id| Tab::Doc(*id)).collect();
        let mut dock = DockState::new(tabs);
        let surface = dock.main_surface_mut();
        surface.split_left(NodeIndex::root(), 0.24, vec![Tab::Browser, Tab::Project]);
        self.dock = dock;
    }

    fn new_doc_id(&mut self) -> DocId {
        let id = DocId(self.next_doc);
        self.next_doc += 1;
        id
    }

    fn apply_settings(&mut self, ctx: &egui::Context) {
        let font = self.settings.editor.font_file.trim().to_owned();
        if self.loaded_font.as_deref() != Some(font.as_str()) {
            let bytes = if font.is_empty() {
                None
            } else {
                match std::fs::read(&font) {
                    Ok(bytes) if is_font(&bytes) => Some(bytes),
                    Ok(_) => {
                        self.log(format!("{font} is not a TrueType or OpenType font"));
                        None
                    }
                    Err(e) => {
                        self.log(format!("Could not load font {font}: {e}"));
                        None
                    }
                }
            };
            theme::set_fonts(ctx, bytes);
            self.loaded_font = Some(font);
        }
        ctx.set_theme(match self.settings.theme {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
        });
        ctx.set_zoom_factor(self.settings.ui_scale);
    }

    fn save_user_reports(&self) {
        if let Some(paths) = &self.paths
            && let Err(e) = save_toml(&reports_file(paths), &self.user_reports)
        {
            error!("saving reports: {e:#}");
        }
    }

    /// The "Save as report" dialog.
    fn save_report_ui(&mut self, ctx: &egui::Context) {
        let Some((name, category, sql)) = &mut self.save_report else {
            return;
        };
        let mut open = true;
        let mut save = false;
        egui::Window::new("Save as report")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                egui::Grid::new("save_report")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Name");
                        let r = ui.add(egui::TextEdit::singleline(name).desired_width(280.0));
                        if name.is_empty() && ui.memory(|m| m.focused().is_none()) {
                            r.request_focus();
                        }
                        if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            save = true;
                        }
                        ui.end_row();
                        ui.label("Category");
                        ui.add(
                            egui::TextEdit::singleline(category)
                                .hint_text("optional, e.g. Sales")
                                .desired_width(280.0),
                        );
                        ui.end_row();
                    });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Bind variables (:name) and &substitutions become parameters, \
                         asked for when the report runs.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);
                let first = sql
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or_default();
                ui.label(RichText::new(first).monospace().weak());
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!name.trim().is_empty(), egui::Button::new("Save"))
                        .clicked()
                    {
                        save = true;
                    }
                });
            });
        if save && !name.trim().is_empty() {
            let report = crate::reports::UserReport {
                name: name.trim().to_owned(),
                category: category.trim().to_owned(),
                sql: sql.clone(),
            };
            self.log(format!("Saved report {}", report.name));
            self.user_reports.save(report);
            self.save_user_reports();
            self.save_report = None;
        } else if !open {
            self.save_report = None;
        }
    }

    fn save_settings(&self) {
        if let Some(paths) = &self.paths
            && let Err(e) = save_toml(&paths.settings_file(), &self.settings)
        {
            error!("saving settings: {e:#}");
        }
    }

    fn save_connections(&self) {
        if let Some(paths) = &self.paths
            && let Err(e) = save_toml(&paths.connections_file(), &self.saved)
        {
            error!("saving connections: {e:#}");
        }
    }

    fn save_history(&mut self) {
        if !self.history.dirty {
            return;
        }
        if let Some(paths) = &self.paths {
            match save_toml(&history_file(paths), &self.history) {
                Ok(()) => self.history.dirty = false,
                Err(e) => error!("saving history: {e:#}"),
            }
        }
    }

    fn log(&mut self, line: impl Into<String>) {
        let line = line.into();
        info!("{line}");
        self.log.push(line);
    }

    /// The connection new documents and commands use when nothing is focused.
    fn current_connection(&self) -> Option<String> {
        self.focused
            .and_then(|id| self.docs.get(&id))
            .and_then(|d| d.connection.clone())
            .filter(|c| self.conns.get(c).is_some())
            .or_else(|| self.browser.connection.clone())
            .or_else(|| self.conns.open.keys().next().cloned())
    }

    fn doc_context(&self, id: DocId) -> DocContext<'_> {
        let connected = self
            .docs
            .get(&id)
            .and_then(|d| d.connection.as_deref())
            .and_then(|c| self.conns.get(c))
            .is_some_and(|c| c.is_connected());
        DocContext {
            id,
            settings: &self.settings,
            connected,
            fetch_size: self.settings.fetch_size,
            connection: self.docs.get(&id).and_then(|d| d.connection.clone()),
            catalog: None,
            catalog_revision: self.catalog_revision,
            connections: &[],
            debug: Some(&self.debug),
        }
    }

    // ----- documents ------------------------------------------------------

    fn add_doc(&mut self, connection: Option<String>, kind: DocKind) -> DocId {
        let id = self.new_doc_id();
        let mut doc = Document { connection, kind };
        let effects = doc.on_attach();
        self.docs.insert(id, doc);
        self.open_tab(Tab::Doc(id));
        self.focused = Some(id);
        self.apply(Some(id), effects);
        id
    }

    fn new_sql_title(&self) -> String {
        let n = self
            .docs
            .values()
            .filter(|d| matches!(d.kind, DocKind::Sql(_)))
            .count()
            + 1;
        format!("SQL {n}")
    }

    fn open(&mut self, request: OpenRequest) {
        if let OpenRequest::Program { connection, object }
        | OpenRequest::ProgramAt {
            connection, object, ..
        }
        | OpenRequest::Object { connection, object }
        | OpenRequest::EditTable { connection, object } = &request
        {
            self.browser.marks.touch(connection, object.clone());
        }
        match request {
            OpenRequest::File { path, connection } => self.open_file(path, connection),
            OpenRequest::Sql {
                connection,
                text,
                run,
            } => {
                let title = self.new_sql_title();
                let id = self.add_doc(Some(connection), DocKind::Sql(SqlWindow::new(title, text)));
                if run {
                    self.execute(id, false);
                }
            }
            OpenRequest::Program { connection, object } => {
                if !self.focus_existing(&connection, &object, true) {
                    self.add_doc(
                        Some(connection),
                        DocKind::Program(ProgramWindow::new(object)),
                    );
                }
            }
            OpenRequest::Object { connection, object } => {
                if !self.focus_existing(&connection, &object, false) {
                    self.add_doc(Some(connection), DocKind::Object(ObjectView::new(object)));
                }
            }
            OpenRequest::Monitor { connection, start } => {
                let existing = self.docs.iter().find_map(|(id, d)| {
                    matches!(&d.kind, DocKind::Monitor(m) if m.start == start)
                        .then_some(*id)
                        .filter(|_| d.connection.as_deref() == Some(connection.as_str()))
                });
                match existing {
                    Some(id) => self.open_tab(Tab::Doc(id)),
                    None => {
                        self.add_doc(
                            Some(connection),
                            DocKind::Monitor(crate::monitor_window::MonitorWindow::new(start)),
                        );
                    }
                }
            }
            OpenRequest::ConnectDialog => self.connect_dialog.show(self.last_profile.as_ref()),
            OpenRequest::EditObject { connection, target } => {
                let existing = self.docs.iter().find_map(|(id, d)| match &d.kind {
                    DocKind::ObjectEditor(e)
                        if e.target == target
                            && d.connection.as_deref() == Some(connection.as_str())
                            && !matches!(target, crate::object_editor::EditTarget::New { .. }) =>
                    {
                        Some(*id)
                    }
                    _ => None,
                });
                match existing {
                    Some(id) => self.open_tab(Tab::Doc(id)),
                    None => {
                        self.add_doc(
                            Some(connection),
                            DocKind::ObjectEditor(crate::object_editor::ObjectEditor::new(target)),
                        );
                    }
                }
            }
            OpenRequest::EditTable { connection, object } => {
                self.add_doc(
                    Some(connection),
                    DocKind::TableEditor(crate::table_editor::TableEditor::new(object)),
                );
            }
            OpenRequest::InsertText { text } => {
                match self
                    .focused
                    .and_then(|id| self.docs.get_mut(&id))
                    .and_then(Document::sql_window)
                {
                    Some(w) => w.editor.insert(&text),
                    None => {
                        if let Some(connection) = self.current_connection() {
                            self.open(OpenRequest::Sql {
                                connection,
                                text,
                                run: false,
                            });
                        }
                    }
                }
            }
            OpenRequest::ProgramAt {
                connection,
                object,
                line,
            } => {
                let object_type = object.object_type;
                self.open(OpenRequest::Program { connection, object });
                if let Some(DocKind::Program(w)) = self
                    .focused
                    .and_then(|id| self.docs.get_mut(&id))
                    .map(|d| &mut d.kind)
                {
                    w.goto_line(object_type, line);
                }
            }
            OpenRequest::Browse { connection } => {
                let schema = self
                    .conns
                    .get(&connection)
                    .map(|c| c.current_schema())
                    .unwrap_or_default();
                let effects = self.browser.attach(connection.clone(), schema);
                self.apply_browser(effects);
            }
        }
    }

    /// Focuses an open program window / object view for the object, if any.
    fn focus_existing(&mut self, connection: &str, object: &ObjectRef, program: bool) -> bool {
        let found = self.docs.iter().find_map(|(id, d)| {
            let same = match &d.kind {
                DocKind::Program(w) if program => {
                    w.object.owner == object.owner
                        && w.object.name == object.name
                        && w.parts.iter().any(|p| p.object_type == object.object_type)
                }
                DocKind::Object(v) if !program => v.object == *object,
                _ => false,
            };
            (same && d.connection.as_deref() == Some(connection)).then_some(*id)
        });
        if let Some(id) = found {
            self.open_tab(Tab::Doc(id));
            self.focused = Some(id);
            if let Some(DocKind::Program(w)) = self.docs.get_mut(&id).map(|d| &mut d.kind) {
                w.active = w
                    .parts
                    .iter()
                    .position(|p| p.object_type == object.object_type)
                    .unwrap_or(w.active);
            }
        }
        found.is_some()
    }

    /// Shows the tab (adding it if needed) and gives it the dock's focus, so
    /// the next frame's "focused document" is this one.
    fn open_tab(&mut self, tab: Tab) {
        if self.dock.find_tab(&tab).is_none() {
            self.place_tab(tab);
        }
        if let Some(path) = self.dock.find_tab(&tab) {
            self.dock.set_focused_node_and_surface(path.node_path());
            let _ = self.dock.set_active_tab(path);
        }
        if let Tab::Doc(id) = tab {
            self.focused = Some(id);
        }
    }

    fn place_tab(&mut self, tab: Tab) {
        // The debugger docks below the documents so code and variables are
        // visible together.
        if tab == Tab::Debugger
            && let Some(path) = self.dock.find_tab_from(|t| matches!(t, Tab::Doc(_)))
        {
            self.dock[path.surface].split_below(path.node, 0.62, vec![tab]);
            return;
        }
        // The project sits next to the object browser.
        if tab == Tab::Project
            && let Some(path) = self.dock.find_tab(&Tab::Browser)
        {
            self.dock.set_focused_node_and_surface(path.node_path());
            self.dock.push_to_focused_leaf(tab);
            return;
        }
        if !matches!(tab, Tab::Doc(_)) {
            self.dock.push_to_focused_leaf(tab);
            return;
        }
        // Documents go next to the focused document, or any document, never
        // into the browser/log pane.
        let focused = self.focused.map(Tab::Doc);
        let near = focused
            .and_then(|f| self.dock.find_tab(&f))
            .or_else(|| self.dock.find_tab_from(|t| matches!(t, Tab::Doc(_))));
        match near {
            Some(path) => {
                self.dock.set_focused_node_and_surface(path.node_path());
                self.dock.push_to_focused_leaf(tab);
            }
            None => match self.dock.find_tab(&Tab::Browser) {
                Some(browser) => {
                    self.dock
                        .main_surface_mut()
                        .split_right(browser.node, 0.24, vec![tab]);
                }
                None => self.dock.push_to_focused_leaf(tab),
            },
        }
    }

    fn close_doc(&mut self, id: DocId) {
        self.conns.close_doc(id);
        if let Some(doc) = self.docs.remove(&id) {
            self.closed_docs.push(doc);
            if self.closed_docs.len() > 20 {
                self.closed_docs.remove(0);
            }
        }
        self.routes.retain(|_, d| *d != id);
        if self.focused == Some(id) {
            // Follow the tab the dock shows now, like the next frame would.
            self.focused = match self.dock.find_active_focused() {
                Some((_, Tab::Doc(doc))) if self.docs.contains_key(doc) => Some(*doc),
                _ => self.docs.keys().next().copied(),
            };
        }
    }

    /// Carries out what a document (or the browser, `doc == None`) asked for.
    fn apply(&mut self, doc: Option<DocId>, effects: Effects) {
        let connection = doc
            .and_then(|id| self.docs.get(&id))
            .and_then(|d| d.connection.clone());
        for (target, request) in effects.requests {
            let target_connection = match &target {
                Target::Connection(c) => Some(c.clone()),
                _ => connection.clone(),
            };
            let Some(name) = target_connection.filter(|c| self.conns.get(c).is_some()) else {
                if let Some(id) = doc {
                    let event = DbEvent::Error(pythia_db::DbError::new(
                        "Not connected. Connect first (Session → Connect…) or pick a connection for this window.",
                    ));
                    self.deliver(id, event);
                }
                continue;
            };
            match (target, doc) {
                (Target::Own | Target::Connection(_), Some(id)) => {
                    let conn = self.conns.get_mut(&name).expect("checked above");
                    conn.doc_session(id, &self.notify).session.send(request);
                }
                (_, doc) => {
                    if let (Some(id), Some(req)) = (doc, request_key(&request)) {
                        self.routes.insert(req, id);
                    }
                    self.conns.send_main(&name, request);
                }
            }
        }
        if effects.cancel
            && let (Some(id), Some(name)) = (doc, &connection)
            && let Some(s) = self.conns.get(name).and_then(|c| c.docs.get(&id))
        {
            s.session.cancel();
        }
        if let Some(name) = &connection {
            let mut ddl = false;
            for sql in effects.executed {
                let first = sql
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                ddl |= matches!(
                    first.as_str(),
                    "CREATE" | "ALTER" | "DROP" | "RENAME" | "COMMENT" | "TRUNCATE"
                );
                self.history.add(name, &sql);
            }
            if ddl {
                self.invalidate_catalog(name);
            }
        }
        for line in effects.log {
            self.log(line);
        }
        if let Some(name) = &connection {
            self.fetch_catalog(name, effects.catalog_requests);
        }
        for (unit, line) in effects.breakpoints {
            self.debug.toggle_breakpoint(unit, line);
        }
        if let (Some(block), Some(id)) = (effects.debug, doc) {
            self.start_debug(id, block);
        }
        for request in effects.open {
            self.open(request);
        }
    }

    /// Starts a debug run of `block` on the document's session.
    fn start_debug(&mut self, doc: DocId, block: String) {
        if self.debug.is_active() {
            self.log("A debug session is already running");
            return;
        }
        let host = Some(self.settings.debug_host.trim().to_owned())
            .filter(|h| !h.is_empty())
            .unwrap_or_else(pythia_debugger::target::default_host);
        let actions = self.debug.start(block, host, self.notify.callback());
        self.debug_run = Some((doc, 0));
        // Only this window shows where its block is stopped.
        for (id, d) in &mut self.docs {
            if let DocKind::Sql(w) = &mut d.kind
                && *id != doc
            {
                w.state.debug_start = None;
            }
        }
        self.open_tab(Tab::Debugger);
        self.open_tab(Tab::Doc(doc));
        self.debug_actions(actions);
    }

    fn debug_actions(&mut self, actions: Vec<pythia_debugger::ui::DebugAction>) {
        use pythia_debugger::ui::DebugAction;
        for action in actions {
            match action {
                DebugAction::RunTarget { host, port, block } => {
                    let Some((doc, _)) = self.debug_run else {
                        continue;
                    };
                    let Some(connection) = self.docs.get(&doc).and_then(|d| d.connection.clone())
                    else {
                        continue;
                    };
                    let Some(conn) = self.conns.get_mut(&connection) else {
                        continue;
                    };
                    let session = conn.doc_session(doc, &self.notify);
                    // Units with breakpoints need debug info; compile them on
                    // the same session so it happens before the block runs.
                    let units: std::collections::BTreeSet<pythia_debugger::Unit> = self
                        .debug
                        .breakpoints
                        .iter()
                        .map(|b| b.unit.clone())
                        .collect();
                    for unit in units {
                        let task: pythia_db::TaskFn =
                            Box::new(move |c: &pythia_db::oracle::Connection| {
                                pythia_debugger::target::compile_debug(c, &unit)?;
                                Ok(Box::new(()) as Box<dyn std::any::Any + Send>)
                            });
                        session.session.send(DbRequest::Task {
                            id: request_id(),
                            task,
                        });
                    }
                    let id = request_id();
                    session.session.send(DbRequest::Task {
                        id,
                        task: pythia_debugger::target::debug_task(host, port, block),
                    });
                    self.debug_run = Some((doc, id));
                }
                DebugAction::ShowLocation(location) => {
                    use pythia_debugger::UnitType;
                    let object_type = match location.unit.unit_type {
                        UnitType::Procedure => Some(pythia_db::ObjectType::Procedure),
                        UnitType::Function => Some(pythia_db::ObjectType::Function),
                        UnitType::Package => Some(pythia_db::ObjectType::Package),
                        UnitType::PackageBody => Some(pythia_db::ObjectType::PackageBody),
                        UnitType::Trigger => Some(pythia_db::ObjectType::Trigger),
                        UnitType::Type => Some(pythia_db::ObjectType::Type),
                        UnitType::TypeBody => Some(pythia_db::ObjectType::TypeBody),
                        UnitType::Block => {
                            // The SQL window marks the line itself.
                            if let Some((doc, _)) = self.debug_run {
                                self.open_tab(Tab::Doc(doc));
                            }
                            None
                        }
                    };
                    let connection = self
                        .debug_run
                        .and_then(|(doc, _)| self.docs.get(&doc))
                        .and_then(|d| d.connection.clone());
                    if let (Some(object_type), Some(connection), Some(line)) =
                        (object_type, connection, location.line)
                    {
                        let object = ObjectRef {
                            owner: location.unit.owner,
                            name: location.unit.name,
                            object_type,
                        };
                        self.open(OpenRequest::ProgramAt {
                            connection,
                            object,
                            line: line as usize,
                        });
                    }
                }
                DebugAction::Finished => self.log("Debug session ended"),
            }
        }
    }

    /// Fetches dictionary data for code assistance in the background.
    fn fetch_catalog(&mut self, connection: &str, requests: Vec<pythia_lsp_core::CatalogRequest>) {
        for request in requests {
            if !self
                .catalog_requested
                .insert((connection.to_owned(), request.clone()))
            {
                continue;
            }
            let id = request_id();
            self.catalog_inflight.insert(id, connection.to_owned());
            self.conns.send_main(
                connection,
                DbRequest::Task {
                    id,
                    task: crate::catalog::task(request),
                },
            );
        }
    }

    /// Forgets cached dictionary data (after DDL, or on demand).
    fn invalidate_catalog(&mut self, connection: &str) {
        if let Some(cache) = self.catalogs.get_mut(connection) {
            cache.clear();
        }
        self.catalog_requested.retain(|(c, _)| c != connection);
        self.catalog_revision += 1;
    }

    fn project_actions(&mut self, actions: Vec<crate::project::ProjectAction>) {
        use crate::project::ProjectAction;
        for action in actions {
            match action {
                ProjectAction::Open(path) => {
                    let connection = self.current_connection();
                    self.open_file(path, connection);
                }
                ProjectAction::Diff {
                    title,
                    left,
                    right,
                    file,
                } => {
                    let connection = self.current_connection();
                    let diff = crate::text_diff::TextDiff::new(title, left, right, Some(file));
                    self.add_doc(connection, DocKind::Diff(diff));
                }
                ProjectAction::Deploy(script) => match self.current_connection() {
                    Some(connection) => self.open(OpenRequest::Sql {
                        connection,
                        text: script,
                        run: false,
                    }),
                    None => {
                        self.add_doc(None, DocKind::Sql(SqlWindow::new("Deploy".into(), script)));
                    }
                },
                ProjectAction::Request(connection, request) => {
                    self.conns.send_main(&connection, request);
                }
            }
        }
    }

    /// Opens a file in a SQL window, or shows the window that has it open.
    fn open_file(&mut self, path: std::path::PathBuf, connection: Option<String>) {
        let existing = self.docs.iter().find_map(|(id, d)| match &d.kind {
            DocKind::Sql(w) if w.path.as_deref() == Some(path.as_path()) => Some(*id),
            _ => None,
        });
        if let Some(id) = existing {
            self.open_tab(Tab::Doc(id));
            return;
        }
        match crate::text_file::read(&path) {
            Ok(file) => {
                let connection = connection.or_else(|| self.current_connection());
                self.add_doc(connection, DocKind::Sql(SqlWindow::from_file(path, file)));
            }
            Err(e) => self.log(format!("Could not open {}: {e}", path.display())),
        }
    }

    fn apply_browser(&mut self, effects: Effects) {
        let connection = self.browser.connection.clone();
        let Effects {
            requests,
            open,
            log,
            ..
        } = effects;
        if let Some(name) = connection {
            for (_, request) in requests {
                self.conns.send_main(&name, request);
            }
        }
        for line in log {
            self.log(line);
        }
        for request in open {
            self.open(request);
        }
    }

    fn deliver(&mut self, id: DocId, event: DbEvent) {
        let cx = self.doc_context(id);
        let (settings, connected, fetch_size, connection) = (
            cx.settings.clone(),
            cx.connected,
            cx.fetch_size,
            cx.connection.clone(),
        );
        let Some(doc) = self.docs.get_mut(&id) else {
            return;
        };
        let catalog = connection.as_ref().and_then(|c| self.catalogs.get(c));
        let cx = DocContext {
            id,
            settings: &settings,
            connected,
            fetch_size,
            connection,
            catalog,
            catalog_revision: self.catalog_revision,
            connections: &[],
            debug: Some(&self.debug),
        };
        let effects = doc.on_event(event, &cx);
        self.apply(Some(id), effects);
    }

    // ----- commands -------------------------------------------------------

    fn run_command(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::NewSqlWindow => {
                let title = self.new_sql_title();
                let connection = self.current_connection();
                self.add_doc(
                    connection,
                    DocKind::Sql(SqlWindow::new(title, String::new())),
                );
            }
            Command::CloseTab => {
                if let Some(id) = self.focused
                    && let Some(path) = self.dock.find_tab(&Tab::Doc(id))
                {
                    self.dock.remove_tab(path);
                    self.close_doc(id);
                }
            }
            Command::CloseOtherTabs => {
                let keep = self.focused;
                let others: Vec<DocId> = self
                    .docs
                    .keys()
                    .copied()
                    .filter(|id| Some(*id) != keep)
                    .collect();
                for id in others {
                    if let Some(path) = self.dock.find_tab(&Tab::Doc(id)) {
                        self.dock.remove_tab(path);
                    }
                    self.close_doc(id);
                }
            }
            Command::Connect => self.connect_dialog.show(self.last_profile.as_ref()),
            Command::Settings => self.settings_dialog.open = true,
            Command::ReopenClosed => {
                if let Some(doc) = self.closed_docs.pop() {
                    let Document { connection, kind } = doc;
                    self.add_doc(connection, kind);
                }
            }
            Command::DebugStart => {
                if let Some(id) = self.focused {
                    let effects = match self.docs.get_mut(&id).map(|d| &mut d.kind) {
                        Some(DocKind::Sql(w)) => w.debug_current(),
                        _ => {
                            self.log(
                                "Open a SQL window with a PL/SQL block (e.g. from Test…) to debug",
                            );
                            Effects::default()
                        }
                    };
                    self.apply(Some(id), effects);
                }
            }
            Command::ToggleBreakpoint => {
                if let Some(id) = self.focused
                    && let Some(DocKind::Program(w)) = self.docs.get(&id).map(|d| &d.kind)
                {
                    let effects = w.toggle_breakpoint();
                    self.apply(Some(id), effects);
                }
            }
            Command::DebugContinue => self.debug.command(pythia_debugger::DebugCommand::Resume),
            Command::DebugStepOver => self.debug.command(pythia_debugger::DebugCommand::StepOver),
            Command::DebugStepInto => self.debug.command(pythia_debugger::DebugCommand::StepInto),
            Command::DebugStepOut => self.debug.command(pythia_debugger::DebugCommand::StepOut),
            Command::DebugStop => self.debug.stop(),
            Command::ShowDebugger => self.open_tab(Tab::Debugger),
            Command::OpenFile => self.file_dialog(FileAction::Open, None),
            Command::SaveFile | Command::SaveFileAs => {
                let Some(id) = self.focused else { return };
                match self.docs.get_mut(&id).map(|d| &mut d.kind) {
                    Some(DocKind::Sql(w)) => match (&w.path, command) {
                        (Some(path), Command::SaveFile) => {
                            let path = path.clone();
                            match w.save_to(path.clone()) {
                                Ok(()) => self.log(format!("Saved {}", path.display())),
                                Err(e) => {
                                    self.log(format!("Could not save {}: {e}", path.display()))
                                }
                            }
                        }
                        _ => {
                            let name = w.path.as_ref().and_then(|p| p.file_name()).map_or_else(
                                || format!("{}.sql", w.title.to_lowercase().replace(' ', "_")),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            self.file_dialog(FileAction::SaveAs(id), Some(name));
                        }
                    },
                    Some(DocKind::Program(w)) => {
                        let name = w.file_name();
                        self.file_dialog(FileAction::SaveAs(id), Some(name));
                    }
                    _ => {}
                }
            }
            Command::Disconnect => {
                let Some(name) = self.current_connection() else {
                    return;
                };
                if self.conns.get(&name).is_some_and(|c| c.any_pending_tx()) {
                    self.confirm = Some(Confirm {
                        action: ConfirmAction::Disconnect(name.clone()),
                        title: "Uncommitted transaction".into(),
                        text: format!(
                            "{name} has uncommitted changes. Disconnecting rolls them back."
                        ),
                        yes: "Disconnect and roll back",
                    });
                } else {
                    self.disconnect(&name);
                }
            }
            Command::ExecuteAll | Command::ExecuteCurrent => {
                if let Some(id) = self.focused {
                    self.execute(id, command == Command::ExecuteCurrent);
                }
            }
            Command::ExecuteWithStatistics => {
                if let Some(id) = self.focused
                    && let Some(w) = self.docs.get_mut(&id).and_then(Document::sql_window)
                {
                    let effects = w.execute_with_statistics();
                    self.apply(Some(id), effects);
                }
            }
            Command::Profile => {
                if let Some(id) = self.focused
                    && let Some(w) = self.docs.get_mut(&id).and_then(Document::sql_window)
                {
                    let effects = w.profile();
                    self.apply(Some(id), effects);
                }
            }
            Command::ExecuteToFile => {
                if let Some(id) = self.focused
                    && let Some(w) = self.docs.get_mut(&id).and_then(Document::sql_window)
                {
                    w.execute_to_file();
                }
            }
            Command::ExplainPlan => {
                if let Some(id) = self.focused
                    && let Some(w) = self.docs.get_mut(&id).and_then(Document::sql_window)
                {
                    let effects = w.explain();
                    self.apply(Some(id), effects);
                }
            }
            Command::Cancel => {
                if let Some((id, name)) = self.focused_session()
                    && let Some(s) = self.conns.get(&name).and_then(|c| c.docs.get(&id))
                {
                    s.session.cancel();
                }
            }
            Command::Commit => {
                let Some((_, name)) = self.focused_session() else {
                    return;
                };
                if self.conns.get(&name).is_some_and(|c| c.profile.production) {
                    self.confirm = Some(Confirm {
                        action: ConfirmAction::Commit,
                        title: "Commit on production".into(),
                        text: format!("Commit the transaction on production connection {name}?"),
                        yes: "Commit",
                    });
                } else {
                    self.end_transaction(true);
                }
            }
            Command::Rollback => self.end_transaction(false),
            Command::ToggleAutocommit => {
                if let Some((id, name)) = self.focused_session()
                    && let Some(conn) = self.conns.get_mut(&name)
                {
                    let s = conn.doc_session(id, &self.notify);
                    s.autocommit = !s.autocommit;
                    s.session.send(DbRequest::SetAutocommit(s.autocommit));
                }
            }
            Command::History => self.history_dialog.toggle(),
            Command::SaveAsReport => {
                let text = self
                    .focused
                    .and_then(|id| self.docs.get_mut(&id))
                    .and_then(Document::sql_window)
                    .map(|w| {
                        w.statements(true)
                            .into_iter()
                            .next()
                            .map(|s| s.sql)
                            .unwrap_or_default()
                    })
                    .unwrap_or_default();
                if text.trim().is_empty() {
                    self.log("Put the cursor on a query to save it as a report");
                } else {
                    self.save_report = Some((String::new(), String::new(), text));
                }
            }
            Command::RenameSymbol => {
                let editor = match self
                    .focused
                    .and_then(|id| self.docs.get_mut(&id))
                    .map(|d| &mut d.kind)
                {
                    Some(DocKind::Sql(w)) => Some(&mut w.editor),
                    Some(DocKind::Program(w)) => w.active_editor(),
                    _ => None,
                };
                if let Some(editor) = editor {
                    let message = rename_symbol(editor);
                    if let Some(message) = message {
                        self.log(message);
                    }
                }
            }
            Command::FoldAll | Command::UnfoldAll => {
                let editor = match self
                    .focused
                    .and_then(|id| self.docs.get_mut(&id))
                    .map(|d| &mut d.kind)
                {
                    Some(DocKind::Sql(w)) => Some(&mut w.editor),
                    Some(DocKind::Program(w)) => w.active_editor(),
                    _ => None,
                };
                if let Some(editor) = editor {
                    if command == Command::FoldAll {
                        editor.inner().fold_all();
                    } else {
                        editor.inner().unfold_all();
                    }
                }
            }
            Command::Format => {
                let options = self.settings.format.clone();
                let editor = match self
                    .focused
                    .and_then(|id| self.docs.get_mut(&id))
                    .map(|d| &mut d.kind)
                {
                    Some(DocKind::Sql(w)) => Some(&mut w.editor),
                    Some(DocKind::Program(w)) => w.active_editor(),
                    _ => None,
                };
                let message = match editor {
                    Some(editor) => format_editor(editor, &options),
                    None => "Nothing to format".into(),
                };
                self.log(message);
            }
            Command::ExportTables => {
                let connection = self.current_connection();
                self.add_doc(connection, DocKind::ExportTables(Default::default()));
            }
            Command::ImportData => {
                let connection = self.current_connection();
                self.add_doc(connection, DocKind::Import(Default::default()));
            }
            Command::SchedulerJobs => {
                let connection = self.current_connection();
                self.add_doc(connection, DocKind::Jobs(Default::default()));
            }
            Command::SearchData => {
                let connection = self.current_connection();
                self.add_doc(connection, DocKind::DataSearch(Default::default()));
            }
            Command::FindInSource => {
                let connection = self.current_connection();
                self.add_doc(
                    connection,
                    DocKind::Search(crate::source_search::SourceSearch::new()),
                );
            }
            Command::CompareSchemas | Command::CompareData => {
                let kind = if command == Command::CompareSchemas {
                    crate::compare_window::CompareKind::Schema
                } else {
                    crate::compare_window::CompareKind::Data
                };
                let connection = self.current_connection();
                self.add_doc(
                    connection.clone(),
                    DocKind::Compare(crate::compare_window::CompareWindow::new(kind, connection)),
                );
            }
            Command::MonitorDashboard
            | Command::MonitorSessions
            | Command::MonitorBlocking
            | Command::MonitorAwr
            | Command::MonitorTrace => {
                use crate::monitor_window::MonitorStart;
                let start = match command {
                    Command::MonitorSessions => MonitorStart::Sessions,
                    Command::MonitorBlocking => MonitorStart::Blocking,
                    Command::MonitorAwr => MonitorStart::Awr,
                    Command::MonitorTrace => MonitorStart::Trace,
                    _ => MonitorStart::Dashboard,
                };
                match self.current_connection() {
                    Some(connection) => self.open(OpenRequest::Monitor { connection, start }),
                    None => self.connect_dialog.show(self.last_profile.as_ref()),
                }
            }
            Command::FindObject => {
                if self.current_connection().is_some() {
                    self.finder.toggle();
                } else {
                    self.connect_dialog.show(self.last_profile.as_ref());
                }
            }
            Command::ShowPalette => self.palette.toggle(),
            Command::ShowBrowser => self.open_tab(Tab::Browser),
            Command::ShowProject => self.open_tab(Tab::Project),
            Command::ShowLog => self.open_tab(Tab::Log),
            Command::ThemeDark | Command::ThemeLight | Command::ThemeSystem => {
                self.settings.theme = match command {
                    Command::ThemeDark => ThemeChoice::Dark,
                    Command::ThemeLight => ThemeChoice::Light,
                    _ => ThemeChoice::System,
                };
                self.apply_settings(ctx);
                self.save_settings();
            }
            Command::ZoomIn | Command::ZoomOut | Command::ZoomReset => {
                self.settings.ui_scale = match command {
                    Command::ZoomIn => (self.settings.ui_scale + 0.1).min(3.0),
                    Command::ZoomOut => (self.settings.ui_scale - 0.1).max(0.5),
                    _ => 1.0,
                };
                self.apply_settings(ctx);
                self.save_settings();
            }
            Command::ResetLayout => self.reset_layout(),
            Command::About => self.about_open = true,
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    /// The focused document and its connection, if it has one that is open.
    fn focused_session(&self) -> Option<(DocId, String)> {
        let id = self.focused?;
        let name = self.docs.get(&id)?.connection.clone()?;
        self.conns.get(&name).map(|_| (id, name))
    }

    fn execute(&mut self, id: DocId, current_only: bool) {
        let cx = self.doc_context(id);
        let (settings, connected, fetch_size, connection) = (
            cx.settings.clone(),
            cx.connected,
            cx.fetch_size,
            cx.connection.clone(),
        );
        if !connected {
            if self.docs.get(&id).is_some_and(|d| d.connection.is_none())
                && let Some(name) = self.current_connection()
            {
                // An unbound window runs on the current connection.
                self.docs.get_mut(&id).expect("doc").connection = Some(name);
                return self.execute(id, current_only);
            }
            self.connect_dialog.show(self.last_profile.as_ref());
            return;
        }
        let profile = self
            .docs
            .get(&id)
            .and_then(|d| d.connection.as_deref())
            .and_then(|c| self.conns.get(c))
            .map(|c| c.profile.clone());
        let read_only = profile.as_ref().is_some_and(|p| p.read_only);
        // Production: confirm statements that change something.
        if profile.as_ref().is_some_and(|p| p.production)
            && !std::mem::take(&mut self.production_confirmed)
            && let Some(DocKind::Sql(w)) = self.docs.get(&id).map(|d| &d.kind)
        {
            let changing = w
                .statements(current_only)
                .iter()
                .filter(|s| s.kind != pythia_plsql::StatementKind::SqlPlus && !is_query(&s.sql))
                .count();
            if changing > 0 {
                let name = profile.map(|p| p.display_name()).unwrap_or_default();
                self.confirm = Some(Confirm {
                    action: ConfirmAction::Execute {
                        doc: id,
                        current_only,
                    },
                    title: "Change production data?".into(),
                    text: format!(
                        "{changing} statement(s) can modify data or objects on the production connection {name}."
                    ),
                    yes: "Run on production",
                });
                return;
            }
        }
        let Some(doc) = self.docs.get_mut(&id) else {
            return;
        };
        let catalog = connection.as_ref().and_then(|c| self.catalogs.get(c));
        let cx = DocContext {
            id,
            settings: &settings,
            connected,
            fetch_size,
            connection,
            catalog,
            catalog_revision: self.catalog_revision,
            connections: &[],
            debug: Some(&self.debug),
        };
        let effects = match &mut doc.kind {
            DocKind::Sql(w) => {
                let mut effects = w.execute(current_only, &cx);
                if read_only {
                    effects.requests.retain(|(_, r)| match r {
                        DbRequest::Execute { sql, .. } => is_query(sql),
                        _ => true,
                    });
                    if w.state.running.as_ref().is_some_and(|r| !is_query(&r.sql)) {
                        w.state.running = None;
                        w.state.queue.clear();
                        w.info("Connection is read-only: only queries are allowed");
                    }
                }
                effects
            }
            DocKind::Program(w) if !read_only => w.compile(false),
            _ => Effects::default(),
        };
        self.apply(Some(id), effects);
    }

    fn end_transaction(&mut self, commit: bool) {
        if let Some((id, name)) = self.focused_session()
            && let Some(conn) = self.conns.get_mut(&name)
        {
            conn.doc_session(id, &self.notify).session.send(if commit {
                DbRequest::Commit
            } else {
                DbRequest::Rollback
            });
        }
    }

    fn connect(&mut self, profile: ConnectionProfile, password: String, save_profile: bool) {
        let name = profile.display_name();
        if self.conns.get(&name).is_some() {
            self.disconnect(&name);
        }
        if save_profile && let Some(paths) = &self.paths {
            self.saved.upsert(profile.clone());
            if let Err(e) = save_toml(&paths.connections_file(), &self.saved) {
                error!("saving connections: {e:#}");
            }
        }
        if profile.save_password {
            secrets::store(&name, &password);
        } else {
            secrets::forget(&name);
        }
        self.log(format!("Connecting to {name}…"));
        self.last_profile = Some(profile.clone());
        self.connecting = Some(self.conns.connect(profile, password, &self.notify));
    }

    fn disconnect(&mut self, name: &str) {
        if self.conns.close(name).is_some() {
            self.log(format!("Disconnected from {name}"));
        }
        self.browser.detach(name);
        if self.browser.connection.is_none()
            && let Some(other) = self.conns.open.keys().next().cloned()
        {
            self.open(OpenRequest::Browse { connection: other });
        }
        for doc in self
            .docs
            .values_mut()
            .filter(|d| d.connection.as_deref() == Some(name))
        {
            if let DocKind::Sql(w) = &mut doc.kind {
                w.state.running = None;
                w.state.fetching = None;
                w.state.queue.clear();
            }
        }
    }

    // ----- database events ------------------------------------------------

    fn pump_events(&mut self) {
        for (name, key, event) in self.conns.poll() {
            let lost = match &event {
                DbEvent::Failed { error, .. } | DbEvent::Error(error) => error.is_connection_lost(),
                DbEvent::Meta {
                    result: Err(error), ..
                }
                | DbEvent::TaskDone {
                    result: Err(error), ..
                } => error.is_connection_lost(),
                _ => false,
            };
            if lost && let Some(conn) = self.conns.get_mut(&name) {
                match key {
                    SessionKey::Doc(id) => {
                        // The next statement in this window opens a new session.
                        conn.docs.remove(&id);
                        self.deliver(
                            id,
                            DbEvent::Notice(
                                "Connection lost. The next statement opens a new session; uncommitted changes were rolled back."
                                    .into(),
                            ),
                        );
                    }
                    SessionKey::Main => conn.reconnect_main(&self.notify),
                }
                self.log(format!("{name}: connection lost, reconnecting"));
            }
            match key {
                SessionKey::Main => self.main_event(&name, event),
                SessionKey::Doc(id) => match event {
                    DbEvent::TaskDone { id: task, result }
                        if self.debug_run.is_some_and(|(_, t)| t == task) =>
                    {
                        self.debug_run = None;
                        let outcome = result.and_then(|any| {
                            any.downcast::<pythia_debugger::target::DebugOutcome>()
                                .map_err(|_| pythia_db::DbError::new("unexpected debug result"))
                        });
                        let error = match outcome {
                            Ok(outcome) => {
                                if !outcome.output.is_empty() {
                                    self.deliver(id, DbEvent::Output(outcome.output));
                                }
                                outcome.error
                            }
                            Err(e) => Some(e),
                        };
                        match error {
                            None => self.log("Debugged block finished"),
                            Some(e) => {
                                self.log(format!("Debugged block failed: {e}"));
                                self.deliver(id, DbEvent::Error(e));
                            }
                        }
                    }
                    event => self.deliver(id, event),
                },
            }
        }
    }

    fn main_event(&mut self, name: &str, event: DbEvent) {
        match event {
            DbEvent::Connected(server) => {
                let line = format!(
                    "Connected to {name} (Oracle {}, schema {})",
                    server.version, server.current_schema
                );
                let schema = server.current_schema.clone();
                self.catalogs.insert(
                    name.to_owned(),
                    pythia_lsp_core::CatalogCache::new(schema.clone()),
                );
                self.catalog_requested.retain(|(c, _)| c != name);
                if let Some(conn) = self.conns.get_mut(name) {
                    conn.status = crate::connections::Status::Connected(server);
                }
                self.log(line);
                if self.connecting.as_deref() == Some(name) {
                    self.connecting = None;
                    self.connect_dialog.connected();
                    let effects = self.browser.attach(name.to_owned(), schema);
                    self.apply_browser(effects);
                    // Bind unbound windows to the first connection.
                    for doc in self.docs.values_mut().filter(|d| d.connection.is_none()) {
                        doc.connection = Some(name.to_owned());
                    }
                    let reload: Vec<DocId> = self
                        .docs
                        .iter()
                        .filter(|(_, d)| {
                            d.connection.as_deref() == Some(name)
                                && !matches!(d.kind, DocKind::Sql(_))
                        })
                        .map(|(id, _)| *id)
                        .collect();
                    for id in reload {
                        let effects = self.docs.get_mut(&id).expect("doc").on_attach();
                        self.apply(Some(id), effects);
                    }
                }
            }
            DbEvent::ConnectFailed(e) => {
                let message = if e.is_missing_client() {
                    format!("{MISSING_CLIENT_HINT}\n\n{e}")
                } else {
                    e.to_string()
                };
                warn!("connect to {name} failed: {e}");
                self.conns.close(name);
                if self.connecting.as_deref() == Some(name) {
                    self.connecting = None;
                    self.connect_dialog.failed(message);
                } else {
                    self.log(format!("{name}: {message}"));
                }
            }
            DbEvent::TaskDone { id, result } if self.catalog_inflight.contains_key(&id) => {
                let connection = self.catalog_inflight.remove(&id).unwrap_or_default();
                match result.map(|any| any.downcast::<crate::catalog::Answer>()) {
                    Ok(Ok(answer)) => {
                        if let Some(cache) = self.catalogs.get_mut(&connection) {
                            answer.apply(cache);
                            self.catalog_revision += 1;
                        }
                    }
                    Ok(Err(_)) => warn!("unexpected catalog task result"),
                    Err(e) => warn!("catalog lookup failed: {e}"),
                }
            }
            DbEvent::Meta { id, result } if self.finder.pending == Some(id) => match result {
                Ok(MetaResult::Objects(objects)) => self.finder.results(id, objects),
                Ok(_) => {}
                Err(e) => {
                    self.finder.results(id, Vec::new());
                    self.log(format!("Find object: {e}"));
                }
            },
            event => {
                let id = event_id(&event);
                if let Some(doc) = id.and_then(|id| self.routes.remove(&id)) {
                    self.deliver(doc, event);
                } else if id.is_some_and(|id| self.browser.owns(id)) {
                    let mut log = Vec::new();
                    let effects = self.browser.on_event(event, &mut log);
                    for line in log {
                        self.log(line);
                    }
                    self.apply_browser(effects);
                }
            }
        }
    }

    // ----- ui -------------------------------------------------------------

    fn menu_bar(
        &mut self,
        ui: &mut Ui,
        commands: &mut Vec<Command>,
        open_report: &mut Option<(String, String)>,
    ) {
        let keymap = &self.keymap;
        let user_reports = &self.user_reports;
        let mut delete = None;
        let ctx = ui.ctx().clone();
        let mut item = |ui: &mut Ui, command: Command| {
            let button = egui::Button::new(command.short_label())
                .shortcut_text(keymap.shortcut_text(&ctx, command));
            if ui.add(button).clicked() {
                commands.push(command);
            }
        };
        egui::MenuBar::new().ui(ui, |ui| {
            bar_menu(ui, "File", |ui| {
                item(ui, Command::NewSqlWindow);
                item(ui, Command::OpenFile);
                item(ui, Command::SaveFile);
                item(ui, Command::SaveFileAs);
                ui.separator();
                item(ui, Command::CloseTab);
                item(ui, Command::CloseOtherTabs);
                item(ui, Command::ReopenClosed);
                ui.separator();
                item(ui, Command::Settings);
                ui.separator();
                item(ui, Command::Quit);
            });
            bar_menu(ui, "Edit", |ui| {
                item(ui, Command::Format);
                item(ui, Command::RenameSymbol);
                item(ui, Command::History);
                ui.separator();
                item(ui, Command::FoldAll);
                item(ui, Command::UnfoldAll);
            });
            bar_menu(ui, "Session", |ui| {
                item(ui, Command::Connect);
                item(ui, Command::Disconnect);
                ui.separator();
                item(ui, Command::ExecuteAll);
                item(ui, Command::ExecuteCurrent);
                item(ui, Command::ExplainPlan);
                item(ui, Command::ExecuteWithStatistics);
                item(ui, Command::ExecuteToFile);
                item(ui, Command::Profile);
                item(ui, Command::Cancel);
                ui.separator();
                item(ui, Command::Commit);
                item(ui, Command::Rollback);
                item(ui, Command::ToggleAutocommit);
            });
            bar_menu(ui, "Tools", |ui| {
                item(ui, Command::FindObject);
                item(ui, Command::FindInSource);
                item(ui, Command::SearchData);
                ui.separator();
                item(ui, Command::SchedulerJobs);
                ui.separator();
                item(ui, Command::ImportData);
                item(ui, Command::ExportTables);
                ui.separator();
                item(ui, Command::CompareSchemas);
                item(ui, Command::CompareData);
            });
            bar_menu(ui, "Debug", |ui| {
                item(ui, Command::DebugStart);
                item(ui, Command::ToggleBreakpoint);
                ui.separator();
                item(ui, Command::DebugContinue);
                item(ui, Command::DebugStepOver);
                item(ui, Command::DebugStepInto);
                item(ui, Command::DebugStepOut);
                item(ui, Command::DebugStop);
                ui.separator();
                item(ui, Command::ShowDebugger);
            });
            bar_menu(ui, "Reports", |ui| {
                for category in crate::reports::categories() {
                    ui.menu_button(category, |ui| {
                        for report in crate::reports::REPORTS
                            .iter()
                            .filter(|r| r.category == category)
                        {
                            if ui.button(report.name).clicked() {
                                *open_report = Some((report.name.into(), report.sql.into()));
                                ui.close();
                            }
                        }
                    });
                }
                if !user_reports.reports.is_empty() {
                    ui.separator();
                    for category in user_reports.categories() {
                        let label = if category.is_empty() {
                            "My reports"
                        } else {
                            category
                        };
                        ui.menu_button(label, |ui| {
                            for (i, report) in user_reports.reports.iter().enumerate() {
                                if report.category != category {
                                    continue;
                                }
                                let r = ui.button(&report.name);
                                if r.clicked() {
                                    *open_report = Some((report.name.clone(), report.sql.clone()));
                                    ui.close();
                                }
                                r.context_menu(|ui| {
                                    if ui.button("Delete report").clicked() {
                                        delete = Some(i);
                                        ui.close();
                                    }
                                });
                            }
                        });
                    }
                }
                ui.separator();
                item(ui, Command::SaveAsReport);
            });
            bar_menu(ui, "Monitor", |ui| {
                item(ui, Command::MonitorDashboard);
                item(ui, Command::MonitorSessions);
                item(ui, Command::MonitorBlocking);
                ui.separator();
                item(ui, Command::MonitorAwr);
                item(ui, Command::MonitorTrace);
            });
            bar_menu(ui, "View", |ui| {
                item(ui, Command::ShowPalette);
                item(ui, Command::ShowBrowser);
                item(ui, Command::ShowLog);
                ui.separator();
                item(ui, Command::ThemeSystem);
                item(ui, Command::ThemeDark);
                item(ui, Command::ThemeLight);
                ui.separator();
                item(ui, Command::ZoomIn);
                item(ui, Command::ZoomOut);
                item(ui, Command::ZoomReset);
                ui.separator();
                item(ui, Command::ResetLayout);
            });
            bar_menu(ui, "Help", |ui| item(ui, Command::About));
        });
        if let Some(i) = delete {
            let report = self.user_reports.reports.remove(i);
            self.log(format!("Deleted report {}", report.name));
            self.save_user_reports();
        }
    }

    fn toolbar(&self, ui: &mut Ui, commands: &mut Vec<Command>) {
        let connected = self.current_connection().is_some();
        let focused = self.focused.and_then(|id| self.docs.get(&id));
        let running = focused.is_some_and(Document::is_busy);
        let is_sql = focused.is_some_and(|d| matches!(d.kind, DocKind::Sql(_)));
        let ctx = ui.ctx().clone();
        let tip = |c: Command| match self.keymap.shortcut(c) {
            Some(s) => format!("{} ({})", c.short_label(), ctx.format_shortcut(s)),
            None => c.short_label().to_owned(),
        };
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let mut button =
                |ui: &mut Ui, icon: Icon, text: &str, command: Command, enabled: bool| {
                    let image = if icon.is_colored() {
                        icon.image(ui)
                    } else {
                        icon.mono(Color32::WHITE)
                    };
                    let b = if text.is_empty() {
                        egui::Button::image(image)
                    } else {
                        egui::Button::image_and_text(image, text)
                    }
                    .frame_when_inactive(false)
                    .image_tint_follows_text_color(!icon.is_colored())
                    .min_size(egui::vec2(28.0, 28.0));
                    if ui
                        .add_enabled(enabled, b)
                        .on_hover_text(tip(command))
                        .clicked()
                    {
                        commands.push(command);
                    }
                };
            button(ui, Icon::Connect, "Connect", Command::Connect, true);
            button(ui, Icon::NewSql, "", Command::NewSqlWindow, true);
            ui.separator();
            button(
                ui,
                Icon::Execute,
                "Execute",
                Command::ExecuteAll,
                connected && !running,
            );
            button(
                ui,
                Icon::ExecuteCurrent,
                "",
                Command::ExecuteCurrent,
                connected && !running && is_sql,
            );
            button(ui, Icon::Stop, "", Command::Cancel, running);
            button(
                ui,
                Icon::Explain,
                "",
                Command::ExplainPlan,
                connected && is_sql,
            );
            ui.separator();
            button(ui, Icon::Commit, "Commit", Command::Commit, connected);
            button(ui, Icon::Rollback, "Rollback", Command::Rollback, connected);
            ui.separator();
            button(ui, Icon::History, "", Command::History, true);
            button(ui, Icon::Search, "", Command::FindObject, connected);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                button(ui, Icon::Palette, "", Command::ShowPalette, true);
            });
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let focused = self.focused.and_then(|id| self.docs.get(&id).map(|d| (id, d)));
            let connection = self.current_connection();
            match connection.as_deref().and_then(|c| self.conns.get(c)) {
                None => {
                    icon_label(ui, Icon::Disconnect, RichText::new("Not connected").weak());
                }
                Some(conn) => {
                    let name = conn.name();
                    let color = connection_color(&conn.profile).unwrap_or(ui.visuals().text_color());
                    match conn.server() {
                        None => {
                            ui.spinner();
                            ui.label(format!("Connecting to {name}…"));
                        }
                        Some(server) => {
                            let (icon, text) = if conn.profile.production {
                                (Icon::Production, format!("{name}  PRODUCTION"))
                            } else {
                                (Icon::Database, name)
                            };
                            icon_label(ui, icon, RichText::new(text).color(color))
                                .on_hover_text(format!("{}\nSchema {}", server.banner, server.current_schema));
                        }
                    }
                    let session = focused.and_then(|(id, _)| conn.docs.get(&id));
                    ui.separator();
                    if session.is_some_and(|s| s.pending_tx) {
                        icon_label(ui, Icon::Transaction, RichText::new("Transaction pending").color(theme::WARN))
                            .on_hover_text("Uncommitted changes in this window's session (F10 commit, Shift+F10 rollback)");
                    } else {
                        ui.weak("No transaction");
                    }
                    if session.is_some_and(|s| s.autocommit) {
                        ui.separator();
                        ui.label("Auto-commit");
                    }
                    if conn.profile.read_only {
                        ui.separator();
                        icon_label(ui, Icon::Lock, "Read-only");
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some((_, doc)) = focused
                    && let DocKind::Sql(w) = &doc.kind
                    && w.path.is_some()
                {
                    ui.weak(format!("{} · {}", w.encoding.label(), w.line_ending.label()))
                        .on_hover_text("Saved the way the file was read");
                }
                if let Some((_, doc)) = focused {
                    let (line_col, status) = doc.status();
                    if let Some((line, col)) = line_col {
                        ui.separator();
                        ui.label(format!("Ln {line}, Col {col}"));
                    }
                    if let Some(status) = status {
                        ui.separator();
                        ui.label(status);
                    }
                }
            });
        });
    }

    fn file_dialog(&mut self, action: FileAction, file_name: Option<String>) {
        if self.file_dialog.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let open = matches!(action, FileAction::Open);
        std::thread::spawn(move || {
            let dialog = rfd::FileDialog::new()
                .add_filter(
                    "SQL and PL/SQL",
                    &[
                        "sql", "pks", "pkb", "prc", "fnc", "tps", "tpb", "trg", "vw", "pls", "plb",
                    ],
                )
                .add_filter("All files", &["*"]);
            let path = if open {
                dialog.pick_file()
            } else {
                dialog
                    .set_file_name(file_name.unwrap_or_default())
                    .save_file()
            };
            let _ = tx.send(path);
        });
        self.file_dialog = Some((rx, action));
    }

    fn poll_file_dialog(&mut self) {
        let Some((rx, action)) = &self.file_dialog else {
            return;
        };
        let Ok(path) = rx.try_recv() else { return };
        let action = *action;
        self.file_dialog = None;
        let Some(path) = path else { return };
        match action {
            FileAction::Open => self.open_file(path, None),
            FileAction::SaveAs(id) => {
                let result = match self.docs.get_mut(&id).map(|d| &mut d.kind) {
                    Some(DocKind::Sql(w)) => w.save_to(path.clone()),
                    Some(DocKind::Program(w)) => std::fs::write(&path, w.active_source()),
                    _ => Ok(()),
                };
                match result {
                    Ok(()) => self.log(format!("Saved {}", path.display())),
                    Err(e) => self.log(format!("Could not save {}: {e}", path.display())),
                }
            }
        }
    }

    /// Data files go to an import window, SQL and PL/SQL files open in a
    /// SQL window.
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let paths: Vec<std::path::PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for path in paths {
            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .map(str::to_ascii_lowercase)
                .unwrap_or_default();
            match ext.as_str() {
                "csv" | "tsv" | "txt" | "xlsx" | "xls" | "xlsb" | "xlsm" | "ods" => {
                    let focused_import = self.focused.filter(|id| {
                        matches!(self.docs.get(id).map(|d| &d.kind), Some(DocKind::Import(_)))
                    });
                    let id = match focused_import {
                        Some(id) => id,
                        None => {
                            let connection = self.current_connection();
                            self.add_doc(connection, DocKind::Import(Default::default()))
                        }
                    };
                    if let Some(DocKind::Import(w)) = self.docs.get_mut(&id).map(|d| &mut d.kind) {
                        w.load(path);
                    }
                }
                _ => self.open_file(path, None),
            }
        }
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested && !self.quit_confirmed && self.conns.any_pending_tx() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(Confirm {
                action: ConfirmAction::Quit,
                title: "Uncommitted transaction".into(),
                text: "There are sessions with uncommitted changes. Exiting rolls them back."
                    .into(),
                yes: "Exit and roll back",
            });
        }
    }

    fn about_ui(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("about")).show(ctx, |ui| {
            ui.heading("Pythia");
            ui.label(format!("Version {}", env!("CARGO_PKG_VERSION")));
            ui.label("A fast, native Oracle PL/SQL IDE.");
            if let Some(paths) = &self.paths {
                ui.add_space(6.0);
                ui.weak(format!("Config: {}", paths.config_dir.display()));
                ui.weak(format!("Logs: {}", paths.log_dir().display()));
            }
            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                self.about_open = false;
            }
        });
        if modal.should_close() {
            self.about_open = false;
        }
    }

    fn popups(&mut self, ctx: &egui::Context, commands: &mut Vec<Command>) {
        match self.connect_dialog.ui(ctx, &self.saved) {
            Some(crate::dialogs::ConnectAction::Connect(request)) => {
                self.connect(request.profile, request.password, request.save_profile);
            }
            Some(crate::dialogs::ConnectAction::Save {
                profile,
                previous,
                password,
            }) => {
                if let Some(previous) = previous.filter(|p| *p != profile.display_name()) {
                    self.saved.profiles.retain(|p| p.display_name() != previous);
                    secrets::forget(&previous);
                }
                match &password {
                    Some(pw) => secrets::store(&profile.display_name(), pw),
                    None => secrets::forget(&profile.display_name()),
                }
                self.saved.upsert(profile);
                self.save_connections();
            }
            Some(crate::dialogs::ConnectAction::Delete(name)) => {
                self.saved.profiles.retain(|p| p.display_name() != name);
                secrets::forget(&name);
                self.save_connections();
            }
            None => {}
        }
        self.save_report_ui(ctx);
        match self.settings_dialog.ui(ctx, &mut self.settings) {
            crate::settings_dialog::SettingsResult::Changed => {
                self.keymap =
                    Keymap::with_overrides(self.settings.keymap, &self.settings.keybindings);
                self.apply_settings(ctx);
            }
            crate::settings_dialog::SettingsResult::Closed => {
                self.keymap =
                    Keymap::with_overrides(self.settings.keymap, &self.settings.keybindings);
                self.apply_settings(ctx);
                self.save_settings();
            }
            crate::settings_dialog::SettingsResult::None => {}
        }
        if let Some(command) = self.palette.ui(ctx, &self.keymap) {
            commands.push(command);
        }
        match self.finder.ui(ctx) {
            Some(FinderAction::Search(pattern)) => {
                if let Some(name) = self.current_connection() {
                    let id = request_id();
                    self.finder.pending = Some(id);
                    self.conns.send_main(
                        &name,
                        DbRequest::Meta {
                            id,
                            query: MetaQuery::Find {
                                pattern,
                                limit: 200,
                            },
                        },
                    );
                }
            }
            Some(FinderAction::Open(object)) => {
                if let Some(connection) = self.current_connection() {
                    self.open(if opens_as_program(object.object_type) {
                        OpenRequest::Program { connection, object }
                    } else {
                        OpenRequest::Object { connection, object }
                    });
                }
            }
            None => {}
        }
        let connection = self.current_connection();
        if let Some(sql) = self
            .history_dialog
            .ui(ctx, &self.history, connection.as_deref())
        {
            match self
                .focused
                .and_then(|id| self.docs.get_mut(&id))
                .and_then(Document::sql_window)
            {
                Some(w) => w.editor.insert(&sql),
                None => {
                    if let Some(connection) = connection {
                        self.open(OpenRequest::Sql {
                            connection,
                            text: sql,
                            run: false,
                        });
                    }
                }
            }
        }
        if let Some(confirm) = &self.confirm
            && let Some(yes) = confirm_ui(ctx, confirm)
        {
            let action = confirm.action.clone();
            self.confirm = None;
            if yes {
                match action {
                    ConfirmAction::Commit => self.end_transaction(true),
                    ConfirmAction::Execute { doc, current_only } => {
                        self.production_confirmed = true;
                        self.execute(doc, current_only);
                    }
                    ConfirmAction::Disconnect(name) => self.disconnect(&name),
                    ConfirmAction::Quit => {
                        self.quit_confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
        self.about_ui(ctx);
    }
}

impl eframe::App for PythiaApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_events();
        self.handle_dropped_files(&ctx);
        self.poll_file_dialog();
        self.handle_close_request(&ctx);

        let modal_open = self.connect_dialog.open
            || self.confirm.is_some()
            || self.about_open
            || self.palette.open
            || self.finder.open
            || self.history_dialog.open
            || self.settings_dialog.is_recording()
            || self.save_report.is_some();
        if self.debug.is_active() && !modal_open {
            self.debug.handle_keys(&ctx);
        }
        let actions = self.debug.poll();
        self.debug_actions(actions);
        let mut commands = if modal_open {
            Vec::new()
        } else {
            self.keymap.triggered(&ctx)
        };

        let mut report = None;
        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &mut commands, &mut report));
        if let Some(report) = report {
            match self.current_connection() {
                Some(connection) => {
                    let id = self.add_doc(
                        Some(connection),
                        DocKind::Sql(SqlWindow::new(report.0, report.1)),
                    );
                    self.execute(id, false);
                }
                None => self.connect_dialog.show(self.last_profile.as_ref()),
            }
        }
        egui::Panel::top("toolbar")
            .frame(theme::toolbar_frame(ui.style()))
            .show(ui, |ui| self.toolbar(ui, &mut commands));
        egui::Panel::bottom("status")
            .frame(theme::status_frame(ui.style()))
            .show(ui, |ui| self.status_bar(ui));

        if let Some((_, Tab::Doc(id))) = self.dock.find_active_focused() {
            self.focused = Some(*id);
        }
        let connections: Vec<String> = self.conns.open.keys().cloned().collect();
        let colors: HashMap<String, Color32> = self
            .conns
            .open
            .iter()
            .filter_map(|(n, c)| connection_color(&c.profile).map(|col| (n.clone(), col)))
            .collect();
        let connected: HashMap<String, bool> = self
            .conns
            .open
            .iter()
            .map(|(n, c)| (n.clone(), c.is_connected()))
            .collect();
        let mut viewer = Viewer {
            docs: &mut self.docs,
            browser: &mut self.browser,
            project: &mut self.project,
            project_actions: Vec::new(),
            log: &self.log,
            settings: &self.settings,
            connections: &connections,
            colors: &colors,
            connected: &connected,
            catalogs: &self.catalogs,
            catalog_revision: self.catalog_revision,
            debug: &mut self.debug,
            debug_actions: Vec::new(),
            effects: Vec::new(),
            browser_effects: Effects::default(),
            closed: Vec::new(),
            rebind: Vec::new(),
        };
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                DockArea::new(&mut self.dock)
                    .style(DockStyle::from_egui(ui.style().as_ref()))
                    .show_leaf_collapse_buttons(false)
                    .show_inside(ui, &mut viewer);
            });
        let Viewer {
            effects,
            browser_effects,
            project_actions,
            closed,
            rebind,
            ..
        } = viewer;
        for (id, connection) in rebind {
            self.conns.close_doc(id);
            if let Some(doc) = self.docs.get_mut(&id) {
                doc.connection = Some(connection);
                let effects = doc.on_attach();
                self.apply(Some(id), effects);
            }
        }
        for id in closed {
            self.close_doc(id);
        }
        for (id, effects) in effects {
            self.apply(Some(id), effects);
        }
        self.apply_browser(browser_effects);
        self.project_actions(project_actions);

        self.popups(&ctx, &mut commands);
        for command in commands {
            self.run_command(&ctx, command);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let workspace = Workspace {
            dock: self.dock.clone(),
            docs: std::mem::take(&mut self.docs),
            next_doc: self.next_doc,
            last_profile: self.last_profile.clone(),
            project: std::mem::take(&mut self.project),
            marks: std::mem::take(&mut self.browser.marks),
        };
        eframe::set_value(storage, WORKSPACE_KEY, &workspace);
        self.docs = workspace.docs;
        self.project = workspace.project;
        self.browser.marks = workspace.marks;
        self.save_history();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.conns.open.clear();
        self.save_settings();
        self.save_history();
    }
}

#[cfg(test)]
impl PythiaApp {
    pub fn test_connect(&mut self, profile: ConnectionProfile, password: String) {
        self.connect(profile, password, false);
    }

    pub fn test_connected(&self) -> bool {
        self.connecting.is_none() && self.conns.open.values().any(|c| c.is_connected())
    }

    pub fn test_open(&mut self, request: OpenRequest) {
        self.open(request);
    }

    pub fn test_focused_busy(&self) -> bool {
        self.focused
            .and_then(|id| self.docs.get(&id))
            .is_some_and(Document::is_busy)
    }

    pub fn test_close_focused(&mut self) {
        self.run_command(&self.notify.0.clone(), Command::CloseTab);
    }

    pub fn test_execute_focused(&mut self) {
        if let Some(id) = self.focused {
            self.execute(id, false);
        }
    }

    pub fn test_expand_browser(&mut self, types: &[pythia_db::ObjectType]) {
        let effects = self.browser.expand(types);
        self.apply_browser(effects);
    }

    pub fn test_sql_window(&mut self) -> &mut SqlWindow {
        let id = self.focused.expect("focused document");
        self.docs
            .get_mut(&id)
            .and_then(Document::sql_window)
            .expect("SQL window")
    }

    pub fn test_export_tables(&mut self) -> &mut crate::export_tables::ExportTables {
        let id = self.focused.expect("focused document");
        match self.docs.get_mut(&id).map(|d| &mut d.kind) {
            Some(DocKind::ExportTables(e)) => e,
            _ => panic!("not an export window"),
        }
    }

    pub fn test_object_editor(&mut self) -> &mut crate::object_editor::ObjectEditor {
        let id = self.focused.expect("focused document");
        match self.docs.get_mut(&id).map(|d| &mut d.kind) {
            Some(DocKind::ObjectEditor(e)) => e,
            _ => panic!("not an object editor"),
        }
    }

    pub fn test_jobs_window(&mut self) -> &mut crate::jobs_window::JobsWindow {
        let id = self.focused.expect("focused document");
        match self.docs.get_mut(&id).map(|d| &mut d.kind) {
            Some(DocKind::Jobs(j)) => j,
            _ => panic!("not a jobs window"),
        }
    }

    pub fn test_sql_window_ref(&self) -> Option<&SqlWindow> {
        match &self.docs.get(&self.focused?)?.kind {
            DocKind::Sql(w) => Some(w),
            _ => None,
        }
    }

    pub fn test_cx(&self) -> (DocId, Settings, Option<String>) {
        let id = self.focused.expect("focused document");
        (id, self.settings.clone(), self.docs[&id].connection.clone())
    }

    pub fn test_apply(&mut self, effects: Effects) {
        self.apply(self.focused, effects);
    }

    pub fn test_show_connections(&mut self, profiles: Vec<ConnectionProfile>) {
        self.saved.profiles = profiles;
        let first = self.saved.profiles.first().cloned();
        self.connect_dialog.show(first.as_ref());
    }

    pub fn test_close_connect(&mut self) {
        self.connect_dialog.open = false;
        self.saved.profiles.clear();
    }

    pub fn test_search_data(&mut self, value: &str, column_names: bool) {
        let id = self.focused.expect("focused");
        let cx = self.doc_context(id);
        let (settings, connection) = (cx.settings.clone(), cx.connection.clone());
        let catalog = connection.as_ref().and_then(|c| self.catalogs.get(c));
        let cx = DocContext {
            id,
            settings: &settings,
            connected: true,
            fetch_size: 100,
            connection,
            catalog,
            catalog_revision: 0,
            connections: &[],
            debug: None,
        };
        let Some(DocKind::DataSearch(s)) = self.docs.get_mut(&id).map(|d| &mut d.kind) else {
            panic!("not a data search");
        };
        s.value = value.to_owned();
        s.column_names = column_names;
        let effects = s.start(&cx);
        self.apply(Some(id), effects);
    }

    pub fn test_data_hits(&self) -> Vec<String> {
        match self
            .focused
            .and_then(|id| self.docs.get(&id))
            .map(|d| &d.kind)
        {
            Some(DocKind::DataSearch(s)) => s
                .hits()
                .iter()
                .map(|h| format!("{}.{}={}", h.table, h.column, h.count))
                .collect(),
            _ => Vec::new(),
        }
    }

    pub fn test_search_source(&mut self, query: &str) {
        let id = self.focused.expect("focused");
        let Some(DocKind::Search(s)) = self.docs.get_mut(&id).map(|d| &mut d.kind) else {
            panic!("not a search document");
        };
        s.query = query.to_owned();
        let effects = s.search();
        self.apply(Some(id), effects);
    }

    pub fn test_import(&mut self, path: std::path::PathBuf, table: &str) {
        let connection = self.current_connection();
        let id = self.add_doc(connection, DocKind::Import(Default::default()));
        let Some(DocKind::Import(w)) = self.docs.get_mut(&id).map(|d| &mut d.kind) else {
            unreachable!()
        };
        w.load(path);
        let effects = w.test_new_table(table, true);
        self.apply(Some(id), effects);
    }

    pub fn test_import_message(&self) -> Option<(bool, String)> {
        match &self.docs.get(&self.focused?)?.kind {
            DocKind::Import(w) => w.test_message(),
            _ => None,
        }
    }

    pub fn test_debug_setup(&mut self, host: &str, unit: pythia_debugger::Unit, line: u32) {
        self.settings.debug_host = host.to_owned();
        self.debug.toggle_breakpoint(unit, line);
    }

    pub fn test_debug(&self) -> &pythia_debugger::ui::DebugSession {
        &self.debug
    }

    pub fn test_debug_mut(&mut self) -> &mut pythia_debugger::ui::DebugSession {
        &mut self.debug
    }

    pub fn test_debug_running(&self) -> bool {
        self.debug_run.is_some()
    }

    /// The focused editor's execution marker: the document's title and the
    /// marked line (1-based, with its text), and the cursor line.
    pub fn test_execution_marker(&self) -> (String, Option<(usize, String)>, usize) {
        let Some(doc) = self.focused.and_then(|id| self.docs.get(&id)) else {
            return (String::new(), None, 0);
        };
        let editor = match &doc.kind {
            DocKind::Program(w) => w.parts.get(w.active).map(|p| &p.editor),
            DocKind::Sql(w) => Some(&w.editor),
            _ => None,
        };
        let Some(editor) = editor else {
            return (doc.title(), None, 0);
        };
        let marker = editor.execution_line().map(|line| {
            let text = editor.text().lines().nth(line - 1).unwrap_or_default();
            (line, text.trim().to_owned())
        });
        (doc.title(), marker, editor.line_col().0)
    }

    pub fn test_settings(&mut self, section_formatter: bool) {
        self.settings_dialog.open = true;
        if section_formatter {
            self.settings_dialog.show_formatter();
        }
    }

    pub fn test_code_font(&mut self, ctx: &egui::Context, file: &str) {
        self.settings.editor.font_file = file.to_owned();
        self.apply_settings(ctx);
    }

    pub fn test_log(&self) -> &[String] {
        &self.log
    }

    pub fn test_settings_keyboard(&mut self) {
        self.settings_dialog.open = true;
        self.settings_dialog.show_keyboard();
        self.settings
            .keybindings
            .insert("Format".into(), "Alt+Shift+F".into());
    }

    pub fn test_close_settings(&mut self) {
        self.settings_dialog.open = false;
    }

    pub fn test_open_file(&mut self, path: std::path::PathBuf) {
        self.open_file(path, None);
    }

    pub fn test_program(&mut self) -> &mut ProgramWindow {
        let id = self.focused.expect("focused");
        match self.docs.get_mut(&id).map(|d| &mut d.kind) {
            Some(DocKind::Program(w)) => w,
            _ => panic!("not a program window"),
        }
    }

    pub fn test_apply_focused(&mut self, effects: Effects) {
        self.apply(self.focused, effects);
    }

    pub fn test_command(&mut self, command: Command) {
        let ctx = self.notify.0.clone();
        self.run_command(&ctx, command);
    }

    pub fn test_focused(&self) -> DocId {
        self.focused.expect("focused document")
    }

    pub fn test_focus(&mut self, id: DocId) {
        self.open_tab(Tab::Doc(id));
    }

    pub fn test_commit(&mut self) {
        self.end_transaction(true);
    }

    pub fn test_pending_tx(&self) -> bool {
        self.conns.any_pending_tx()
    }

    pub fn test_expand_table(&mut self, table: ObjectRef) {
        let effects = self.browser.expand_table(table);
        self.apply_browser(effects);
    }

    pub fn test_project(&mut self) -> &mut crate::project::Project {
        &mut self.project
    }

    pub fn test_project_actions(&mut self, actions: Vec<crate::project::ProjectAction>) {
        self.project_actions(actions);
    }

    pub fn test_focused_title(&self) -> Option<String> {
        self.focused
            .and_then(|id| self.docs.get(&id))
            .map(|d| d.title())
    }

    pub fn test_browser(&mut self) -> &mut Browser {
        &mut self.browser
    }

    pub fn test_browser_loading(&self) -> bool {
        self.browser.is_loading()
    }
}

struct Viewer<'a> {
    docs: &'a mut BTreeMap<DocId, Document>,
    browser: &'a mut Browser,
    project: &'a mut crate::project::Project,
    project_actions: Vec<crate::project::ProjectAction>,
    log: &'a [String],
    settings: &'a Settings,
    connections: &'a [String],
    colors: &'a HashMap<String, Color32>,
    connected: &'a HashMap<String, bool>,
    catalogs: &'a HashMap<String, pythia_lsp_core::CatalogCache>,
    catalog_revision: u64,
    debug: &'a mut pythia_debugger::ui::DebugSession,
    debug_actions: Vec<pythia_debugger::ui::DebugAction>,
    effects: Vec<(DocId, Effects)>,
    browser_effects: Effects,
    closed: Vec<DocId>,
    /// Documents moved to another connection.
    rebind: Vec<(DocId, String)>,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::Browser => "Objects".into(),
            Tab::Project => "Project".into(),
            Tab::Log => "Log".into(),
            Tab::Debugger => "Debugger".into(),
            Tab::Doc(id) => match self.docs.get(id) {
                Some(doc) => {
                    let color = doc
                        .connection
                        .as_ref()
                        .and_then(|c| self.colors.get(c))
                        .copied();
                    doc.tab_title(color).into()
                }
                None => "?".into(),
            },
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::Browser => {
                let effects = self.browser.ui(ui, self.connections);
                self.browser_effects.extend(effects);
            }
            Tab::Project => {
                let connection = self
                    .browser
                    .connection
                    .clone()
                    .filter(|c| self.connected.get(c).copied().unwrap_or(false));
                let actions = self.project.ui(ui, connection.as_deref());
                self.project_actions.extend(actions);
            }
            Tab::Debugger => {
                let actions = self.debug.ui(ui);
                self.debug_actions.extend(actions);
            }
            Tab::Log => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in self.log {
                            ui.label(RichText::new(line).monospace());
                        }
                    });
            }
            Tab::Doc(id) => {
                let Some(doc) = self.docs.get_mut(id) else {
                    return;
                };
                // Connection picker for windows that can switch connections.
                if matches!(doc.kind, DocKind::Sql(_)) && !self.connections.is_empty() {
                    let current = doc.connection.clone().unwrap_or_else(|| "(none)".into());
                    ui.horizontal(|ui| {
                        ui.weak("Connection");
                        egui::ComboBox::from_id_salt(("doc-conn", *id))
                            .selected_text(&current)
                            .show_ui(ui, |ui| {
                                for c in self.connections {
                                    if ui.selectable_label(*c == current, c).clicked()
                                        && *c != current
                                    {
                                        self.rebind.push((*id, c.clone()));
                                    }
                                }
                            });
                    });
                }
                let connected = doc
                    .connection
                    .as_ref()
                    .and_then(|c| self.connected.get(c))
                    .copied()
                    .unwrap_or(false);
                let cx = DocContext {
                    id: *id,
                    settings: self.settings,
                    connected,
                    fetch_size: self.settings.fetch_size,
                    connection: doc.connection.clone(),
                    catalog: doc.connection.as_ref().and_then(|c| self.catalogs.get(c)),
                    catalog_revision: self.catalog_revision,
                    connections: self.connections,
                    debug: Some(&*self.debug),
                };
                let effects = doc.ui(ui, &cx);
                self.effects.push((*id, effects));
            }
        }
    }

    fn on_close(&mut self, tab: &mut Tab) -> egui_dock::widgets::tab_viewer::OnCloseResponse {
        if let Tab::Doc(id) = tab {
            self.closed.push(*id);
        }
        egui_dock::widgets::tab_viewer::OnCloseResponse::Close
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        [false, false]
    }
}

/// TrueType, OpenType or font collection signature; anything else would
/// make egui panic when laying out text.
fn is_font(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(..4),
        Some([0, 1, 0, 0] | b"OTTO" | b"true" | b"ttcf")
    )
}

/// Selects every occurrence of the local symbol at the caret, so typing
/// renames them all at once. Returns a message for the log.
fn rename_symbol(editor: &mut crate::editor::Editor) -> Option<String> {
    let cursor = editor.cursor();
    let refs = pythia_lsp_core::references(editor.text(), cursor);
    if refs.is_empty() {
        return Some(
            "Rename works for local variables, parameters, cursors, aliases and CTEs".into(),
        );
    }
    let name = editor.text()[refs[0].clone()].to_owned();
    editor.inner().select_ranges(&refs, cursor);
    Some(format!(
        "Renaming {} occurrences of {name}: type the new name, Esc when done",
        refs.len()
    ))
}

/// Formats the selection (or the whole text) as one undo step.
fn format_editor(
    editor: &mut crate::editor::Editor,
    options: &pythia_plsql::FormatOptions,
) -> String {
    let text = editor.text().to_owned();
    let line = editor.line_col().0;
    let formatted = match editor.selection() {
        Some(sel) => pythia_plsql::format_range(&text, sel, options),
        None => pythia_plsql::format(&text, options),
    };
    if !formatted.changed {
        return "Already formatted".into();
    }
    // Replace only the changed middle, so undo and markers stay precise.
    let prefix = text
        .bytes()
        .zip(formatted.text.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let prefix = (0..=prefix)
        .rev()
        .find(|i| text.is_char_boundary(*i) && formatted.text.is_char_boundary(*i))
        .unwrap_or(0);
    let suffix = text[prefix..]
        .bytes()
        .rev()
        .zip(formatted.text[prefix..].bytes().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = (0..=suffix)
        .rev()
        .find(|s| {
            text.is_char_boundary(text.len() - s)
                && formatted.text.is_char_boundary(formatted.text.len() - s)
        })
        .unwrap_or(0);
    let new_line_count = formatted.text.lines().count().max(1);
    editor.replace(
        prefix..text.len() - suffix,
        &formatted.text[prefix..formatted.text.len() - suffix],
        line.min(new_line_count),
    );
    "Formatted".into()
}

fn reports_file(paths: &Paths) -> std::path::PathBuf {
    paths.config_dir.join("reports.toml")
}

fn history_file(paths: &Paths) -> std::path::PathBuf {
    paths.data_dir.join("history.toml")
}

fn load_or_default<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    load_toml(path).unwrap_or_else(|e| {
        error!("{e:#}");
        T::default()
    })
}

fn connection_color(profile: &ConnectionProfile) -> Option<Color32> {
    if profile.production {
        Some(theme::DANGER)
    } else {
        profile.color.map(|[r, g, b]| Color32::from_rgb(r, g, b))
    }
}

/// A top-level menu of the menu bar. As in native menu bars, moving onto
/// another title while one of the bar's menus is open switches to it.
fn bar_menu(ui: &mut Ui, title: &str, add_contents: impl FnOnce(&mut Ui)) {
    let response = ui.menu_button(title, add_contents).response;
    let ctx = ui.ctx();
    let popup_id = egui::Popup::default_response_id(&response);
    let open_key = ui.id().with("open_bar_menu");
    if response.hovered() && !egui::Popup::is_id_open(ctx, popup_id) {
        let open = ctx.data(|d| d.get_temp::<egui::Id>(open_key));
        if open.is_some_and(|id| egui::Popup::is_id_open(ctx, id)) {
            egui::Popup::open_id(ctx, popup_id);
            ctx.request_repaint();
        }
    }
    if egui::Popup::is_id_open(ctx, popup_id) {
        ctx.data_mut(|d| d.insert_temp(open_key, popup_id));
    }
}

/// Request id of requests that are answered with an id-carrying event.
fn request_key(request: &DbRequest) -> Option<u64> {
    match request {
        DbRequest::Execute { id, .. }
        | DbRequest::FetchMore { id, .. }
        | DbRequest::Meta { id, .. }
        | DbRequest::Task { id, .. } => Some(*id),
        _ => None,
    }
}

fn event_id(event: &DbEvent) -> Option<u64> {
    match event {
        DbEvent::QueryStarted { id, .. }
        | DbEvent::Rows { id, .. }
        | DbEvent::Executed { id, .. }
        | DbEvent::Failed { id, .. }
        | DbEvent::Meta { id, .. }
        | DbEvent::TaskDone { id, .. } => Some(*id),
        _ => None,
    }
}

/// Read-only connections only accept queries.
fn is_query(sql: &str) -> bool {
    let first = pythia_plsql::tokenize(sql)
        .into_iter()
        .find(|t| !t.kind.is_trivia());
    first.is_some_and(|t| {
        let word = &sql[t.span];
        ["SELECT", "WITH"]
            .iter()
            .any(|k| word.eq_ignore_ascii_case(k))
            || word == "("
    })
}

#[cfg(test)]
mod format_tests {
    use super::*;

    #[test]
    fn formats_editor_text() {
        let mut editor = crate::editor::Editor::new("SELECT a,b FROM t WHERE x=1");
        let message = format_editor(&mut editor, &pythia_plsql::FormatOptions::default());
        assert_eq!(message, "Formatted");
        assert_eq!(editor.text(), "select a, b\nfrom t\nwhere x = 1");
        assert_eq!(
            format_editor(&mut editor, &pythia_plsql::FormatOptions::default()),
            "Already formatted"
        );
    }
}

#[cfg(test)]
mod rename_tests {
    use super::rename_symbol;
    use crate::editor::Editor;

    #[test]
    fn renames_every_occurrence() {
        let src = "declare\n  l_total number := 0;\nbegin\n  l_total := l_total + 1;\n  dbms_output.put_line(l_total);\nend;";
        let mut e = Editor::new(src);
        e.select(src.find("l_total :=").unwrap()..src.find("l_total :=").unwrap());
        let message = rename_symbol(&mut e).unwrap();
        assert!(
            message.starts_with("Renaming 4 occurrences of l_total"),
            "{message}"
        );
        e.inner().insert_at_cursor("l_sum");
        let text = e.inner().text();
        assert_eq!(text.matches("l_sum").count(), 4, "{text}");
        assert!(!text.contains("l_total"));

        let mut e = Editor::new("select 1 from dual");
        assert!(
            rename_symbol(&mut e)
                .unwrap()
                .starts_with("Rename works for")
        );
    }
}
