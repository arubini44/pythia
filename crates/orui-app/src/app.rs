use std::collections::{BTreeMap, HashMap};

use egui::{Color32, RichText, Ui};
use egui_dock::{DockArea, DockState, NodeIndex, Style as DockStyle, TabViewer};
use orui_config::{
    ConnectionProfile, Connections as SavedConnections, Paths, Settings, ThemeChoice, load_toml,
    save_toml,
};
use orui_db::{DbEvent, DbRequest, MetaQuery, MetaResult};
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
use orui_ui::icons::{Icon, icon_label};

const WORKSPACE_KEY: &str = "orui-workspace-v2";
const MISSING_CLIENT_HINT: &str = "Oracle Instant Client could not be loaded. Install it from \
     oracle.com and put its directory on LD_LIBRARY_PATH (Linux), PATH (Windows) or \
     DYLD_LIBRARY_PATH (macOS).";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileAction {
    Open,
    SaveAs(DocId),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    Browser,
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
}

pub struct OruiApp {
    paths: Option<Paths>,
    settings: Settings,
    saved: SavedConnections,
    keymap: Keymap,
    notify: Notify,

    dock: DockState<Tab>,
    docs: BTreeMap<DocId, Document>,
    next_doc: u64,
    /// The document commands act on: the focused one, or the last focused.
    focused: Option<DocId>,

    conns: Connections,
    browser: Browser,
    /// Main-session requests issued by documents, routed back by id.
    routes: HashMap<u64, DocId>,
    last_profile: Option<ConnectionProfile>,
    /// Connection the connect dialog is currently opening.
    connecting: Option<String>,

    history: History,
    debug: orui_debugger::ui::DebugSession,
    /// The running debug call: (document, task request id).
    debug_run: Option<(DocId, u64)>,
    /// Dictionary caches for code assistance, by connection.
    catalogs: HashMap<String, orui_lsp_core::CatalogCache>,
    catalog_revision: u64,
    /// Catalog task request id → connection.
    catalog_inflight: HashMap<u64, String>,
    /// Requests already sent (or answered) per connection, to avoid repeats.
    catalog_requested: std::collections::HashSet<(String, orui_lsp_core::CatalogRequest)>,
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

impl OruiApp {
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
        let workspace: Option<Workspace> =
            cc.storage.and_then(|s| eframe::get_value(s, WORKSPACE_KEY));

        let mut app = Self {
            paths,
            keymap: Keymap::new(settings.keymap),
            settings,
            saved,
            notify: Notify(cc.egui_ctx.clone()),
            dock: DockState::new(vec![]),
            docs: BTreeMap::new(),
            next_doc: 1,
            focused: None,
            conns: Connections::default(),
            browser: Browser::default(),
            routes: HashMap::new(),
            last_profile: None,
            connecting: None,
            history,
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
        app.log("orui started");
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
        surface.split_left(NodeIndex::root(), 0.24, vec![Tab::Browser]);
        self.dock = dock;
    }

    fn new_doc_id(&mut self) -> DocId {
        let id = DocId(self.next_doc);
        self.next_doc += 1;
        id
    }

    fn apply_settings(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.settings.theme {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
            ThemeChoice::Light => egui::ThemePreference::Light,
        });
        ctx.set_zoom_factor(self.settings.ui_scale);
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
        match request {
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
                    let event = DbEvent::Error(orui_db::DbError::new(
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
            .unwrap_or_else(orui_debugger::target::default_host);
        let actions = self.debug.start(block, host, self.notify.callback());
        self.debug_run = Some((doc, 0));
        self.open_tab(Tab::Debugger);
        self.open_tab(Tab::Doc(doc));
        self.debug_actions(actions);
    }

    fn debug_actions(&mut self, actions: Vec<orui_debugger::ui::DebugAction>) {
        use orui_debugger::ui::DebugAction;
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
                    let units: std::collections::BTreeSet<orui_debugger::Unit> = self
                        .debug
                        .breakpoints
                        .iter()
                        .map(|b| b.unit.clone())
                        .collect();
                    for unit in units {
                        let task: orui_db::TaskFn =
                            Box::new(move |c: &orui_db::oracle::Connection| {
                                orui_debugger::target::compile_debug(c, &unit)?;
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
                        task: orui_debugger::target::debug_task(host, port, block),
                    });
                    self.debug_run = Some((doc, id));
                }
                DebugAction::ShowLocation(location) => {
                    use orui_debugger::UnitType;
                    let object_type = match location.unit.unit_type {
                        UnitType::Procedure => Some(orui_db::ObjectType::Procedure),
                        UnitType::Function => Some(orui_db::ObjectType::Function),
                        UnitType::Package => Some(orui_db::ObjectType::Package),
                        UnitType::PackageBody => Some(orui_db::ObjectType::PackageBody),
                        UnitType::Trigger => Some(orui_db::ObjectType::Trigger),
                        UnitType::Type => Some(orui_db::ObjectType::Type),
                        UnitType::TypeBody => Some(orui_db::ObjectType::TypeBody),
                        UnitType::Block => None,
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
    fn fetch_catalog(&mut self, connection: &str, requests: Vec<orui_lsp_core::CatalogRequest>) {
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
            Command::DebugContinue => self.debug.command(orui_debugger::DebugCommand::Resume),
            Command::DebugStepOver => self.debug.command(orui_debugger::DebugCommand::StepOver),
            Command::DebugStepInto => self.debug.command(orui_debugger::DebugCommand::StepInto),
            Command::DebugStepOut => self.debug.command(orui_debugger::DebugCommand::StepOut),
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
            Command::ImportData => {
                let connection = self.current_connection();
                self.add_doc(connection, DocKind::Import(Default::default()));
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
                .filter(|s| s.kind != orui_plsql::StatementKind::SqlPlus && !is_query(&s.sql))
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
                        match result {
                            Ok(_) => self.log("Debugged block finished"),
                            Err(e) => {
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
                    orui_lsp_core::CatalogCache::new(schema.clone()),
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

    fn menu_bar(&mut self, ui: &mut Ui, commands: &mut Vec<Command>) {
        let keymap = &self.keymap;
        let ctx = ui.ctx().clone();
        let mut item = |ui: &mut Ui, command: Command| {
            let button = egui::Button::new(command.short_label())
                .shortcut_text(keymap.shortcut_text(&ctx, command));
            if ui.add(button).clicked() {
                commands.push(command);
            }
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
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
            ui.menu_button("Edit", |ui| {
                item(ui, Command::Format);
                item(ui, Command::History);
            });
            ui.menu_button("Session", |ui| {
                item(ui, Command::Connect);
                item(ui, Command::Disconnect);
                ui.separator();
                item(ui, Command::ExecuteAll);
                item(ui, Command::ExecuteCurrent);
                item(ui, Command::ExplainPlan);
                item(ui, Command::ExecuteWithStatistics);
                item(ui, Command::Cancel);
                ui.separator();
                item(ui, Command::Commit);
                item(ui, Command::Rollback);
                item(ui, Command::ToggleAutocommit);
            });
            ui.menu_button("Tools", |ui| {
                item(ui, Command::FindObject);
                item(ui, Command::FindInSource);
                item(ui, Command::SearchData);
                ui.separator();
                item(ui, Command::ImportData);
                ui.separator();
                item(ui, Command::CompareSchemas);
                item(ui, Command::CompareData);
            });
            ui.menu_button("Debug", |ui| {
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
            ui.menu_button("Monitor", |ui| {
                item(ui, Command::MonitorDashboard);
                item(ui, Command::MonitorSessions);
                item(ui, Command::MonitorBlocking);
                ui.separator();
                item(ui, Command::MonitorAwr);
                item(ui, Command::MonitorTrace);
            });
            ui.menu_button("View", |ui| {
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
            ui.menu_button("Help", |ui| item(ui, Command::About));
        });
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
                ui.weak("UTF-8");
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
            FileAction::Open => match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let connection = self.current_connection();
                    self.add_doc(connection, DocKind::Sql(SqlWindow::from_file(path, text)));
                }
                Err(e) => self.log(format!("Could not open {}: {e}", path.display())),
            },
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
                _ => match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        let connection = self.current_connection();
                        self.add_doc(connection, DocKind::Sql(SqlWindow::from_file(path, text)));
                    }
                    Err(e) => self.log(format!("Could not open {}: {e}", path.display())),
                },
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
            ui.heading("orui");
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
        match self.settings_dialog.ui(ctx, &mut self.settings) {
            crate::settings_dialog::SettingsResult::Changed => self.apply_settings(ctx),
            crate::settings_dialog::SettingsResult::Closed => {
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

impl eframe::App for OruiApp {
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
            || self.history_dialog.open;
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

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &mut commands));
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
        };
        eframe::set_value(storage, WORKSPACE_KEY, &workspace);
        self.docs = workspace.docs;
        self.save_history();
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.conns.open.clear();
        self.save_settings();
        self.save_history();
    }
}

#[cfg(test)]
impl OruiApp {
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

    pub fn test_expand_browser(&mut self, types: &[orui_db::ObjectType]) {
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

    pub fn test_search_data(&mut self, value: &str) {
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

    pub fn test_debug_setup(&mut self, host: &str, unit: orui_debugger::Unit, line: u32) {
        self.settings.debug_host = host.to_owned();
        self.debug.toggle_breakpoint(unit, line);
    }

    pub fn test_debug(&self) -> &orui_debugger::ui::DebugSession {
        &self.debug
    }

    pub fn test_debug_mut(&mut self) -> &mut orui_debugger::ui::DebugSession {
        &mut self.debug
    }

    pub fn test_debug_running(&self) -> bool {
        self.debug_run.is_some()
    }

    pub fn test_settings(&mut self, section_formatter: bool) {
        self.settings_dialog.open = true;
        if section_formatter {
            self.settings_dialog.show_formatter();
        }
    }

    pub fn test_close_settings(&mut self) {
        self.settings_dialog.open = false;
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

    pub fn test_browser_loading(&self) -> bool {
        self.browser.is_loading()
    }
}

struct Viewer<'a> {
    docs: &'a mut BTreeMap<DocId, Document>,
    browser: &'a mut Browser,
    log: &'a [String],
    settings: &'a Settings,
    connections: &'a [String],
    colors: &'a HashMap<String, Color32>,
    connected: &'a HashMap<String, bool>,
    catalogs: &'a HashMap<String, orui_lsp_core::CatalogCache>,
    catalog_revision: u64,
    debug: &'a mut orui_debugger::ui::DebugSession,
    debug_actions: Vec<orui_debugger::ui::DebugAction>,
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

/// Formats the selection (or the whole text) as one undo step.
fn format_editor(
    editor: &mut crate::editor::Editor,
    options: &orui_plsql::FormatOptions,
) -> String {
    let text = editor.text().to_owned();
    let line = editor.line_col().0;
    let formatted = match editor.selection() {
        Some(sel) => orui_plsql::format_range(&text, sel, options),
        None => orui_plsql::format(&text, options),
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
    let first = orui_plsql::tokenize(sql)
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
        let message = format_editor(&mut editor, &orui_plsql::FormatOptions::default());
        assert_eq!(message, "Formatted");
        assert_eq!(editor.text(), "select a, b\nfrom t\nwhere x = 1");
        assert_eq!(
            format_editor(&mut editor, &orui_plsql::FormatOptions::default()),
            "Already formatted"
        );
    }
}
