use std::collections::{BTreeMap, HashMap};

use egui::{Color32, RichText, Ui};
use egui_dock::{DockArea, DockState, NodeIndex, Style as DockStyle, TabViewer};
use orui_config::{
    ConnectionProfile, Connections, Paths, Settings, ThemeChoice, load_toml, save_toml,
};
use orui_db::{ConnectParams, DbEvent, DbRequest, ServerInfo, Session};
use orui_plsql::StatementKind;
use serde::{Deserialize, Serialize};
use tracing::{error, info, warn};

use crate::commands::{Command, Keymap};
use crate::dialogs::{CommandPalette, Confirm, ConfirmAction, ConnectDialog, confirm_ui};
use crate::sql_window::{Next, SqlWindow, WindowId};

const WORKSPACE_KEY: &str = "orui-workspace";
const MISSING_CLIENT_HINT: &str = "Oracle Instant Client could not be loaded. Install it from \
     oracle.com and put its directory on LD_LIBRARY_PATH (Linux), PATH (Windows) or \
     DYLD_LIBRARY_PATH (macOS).";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Tab {
    ObjectBrowser,
    Log,
    Sql(WindowId),
}

/// Layout and editor contents, restored on the next start.
#[derive(Serialize, Deserialize)]
struct Workspace {
    dock: DockState<Tab>,
    windows: BTreeMap<WindowId, SqlWindow>,
    next_window: u64,
    last_profile: Option<ConnectionProfile>,
}

enum ConnStatus {
    Connecting,
    Connected(ServerInfo),
}

struct ActiveConnection {
    profile: ConnectionProfile,
    session: Session,
    status: ConnStatus,
    pending_tx: bool,
    autocommit: bool,
}

pub struct OruiApp {
    paths: Option<Paths>,
    settings: Settings,
    saved: Connections,
    keymap: Keymap,

    dock: DockState<Tab>,
    windows: BTreeMap<WindowId, SqlWindow>,
    next_window: u64,
    /// The SQL window commands act on: the focused one, or the last focused.
    focused: Option<WindowId>,

    // Single connection for now; multiple connections and session modes
    // (multi/dual/single) follow in M1 (CONCEPT.md §5.2).
    conn: Option<ActiveConnection>,
    last_profile: Option<ConnectionProfile>,
    next_request: u64,
    /// Which window a request belongs to.
    requests: HashMap<u64, WindowId>,
    /// Window that ran the last statement; gets DBMS_OUTPUT and notices.
    last_window: Option<WindowId>,

    log: Vec<String>,
    connect_dialog: ConnectDialog,
    palette: CommandPalette,
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
        let saved: Connections = paths
            .as_ref()
            .map(|p| load_or_default(&p.connections_file()))
            .unwrap_or_default();
        let workspace: Option<Workspace> =
            cc.storage.and_then(|s| eframe::get_value(s, WORKSPACE_KEY));

        let mut app = Self {
            paths,
            keymap: Keymap::new(settings.keymap),
            settings,
            saved,
            dock: DockState::new(vec![]),
            windows: BTreeMap::new(),
            next_window: 1,
            focused: None,
            conn: None,
            last_profile: None,
            next_request: 1,
            requests: HashMap::new(),
            last_window: None,
            log: Vec::new(),
            connect_dialog: ConnectDialog::default(),
            palette: CommandPalette::default(),
            confirm: None,
            about_open: false,
            quit_confirmed: false,
        };
        match workspace {
            Some(ws) if !ws.windows.is_empty() => {
                app.dock = ws.dock;
                app.windows = ws.windows;
                app.next_window = ws.next_window;
                app.last_profile = ws.last_profile;
                // Drop tabs whose window data went missing.
                let windows = &app.windows;
                app.dock
                    .retain_tabs(|tab| !matches!(tab, Tab::Sql(id) if !windows.contains_key(id)));
            }
            _ => app.reset_layout(),
        }
        app.focused = app.windows.keys().next().copied();

        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);
        app.apply_settings(&cc.egui_ctx);
        app.log("orui started");
        app
    }

    fn reset_layout(&mut self) {
        let id = self.new_window_id();
        self.windows
            .entry(id)
            .or_insert_with(|| SqlWindow::new(format!("SQL Window {}", id.0)));
        let sql_tabs: Vec<Tab> = self.windows.keys().map(|id| Tab::Sql(*id)).collect();
        let mut dock = DockState::new(sql_tabs);
        let surface = dock.main_surface_mut();
        let [_, _browser] = surface.split_left(NodeIndex::root(), 0.22, vec![Tab::ObjectBrowser]);
        self.dock = dock;
    }

    fn new_window_id(&mut self) -> WindowId {
        let id = WindowId(self.next_window);
        self.next_window += 1;
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

    fn log(&mut self, line: impl Into<String>) {
        let line = line.into();
        info!("{line}");
        self.log.push(line);
    }

    // ----- commands -------------------------------------------------------

    fn run_command(&mut self, ctx: &egui::Context, command: Command) {
        match command {
            Command::NewSqlWindow => {
                let id = self.new_window_id();
                self.windows
                    .insert(id, SqlWindow::new(format!("SQL Window {}", id.0)));
                self.open_tab(Tab::Sql(id));
                self.focused = Some(id);
            }
            Command::CloseTab => {
                if let Some(id) = self.focused
                    && let Some(path) = self.dock.find_tab(&Tab::Sql(id))
                {
                    self.dock.remove_tab(path);
                    self.close_window(id);
                }
            }
            Command::Connect => self.connect_dialog.show(self.last_profile.as_ref()),
            Command::Disconnect => {
                if self.conn.as_ref().is_some_and(|c| c.pending_tx) {
                    self.confirm = Some(Confirm {
                        action: ConfirmAction::Disconnect,
                        title: "Uncommitted transaction".into(),
                        text: "The session has uncommitted changes. Disconnecting rolls them back."
                            .into(),
                        yes: "Disconnect and roll back",
                    });
                } else {
                    self.disconnect();
                }
            }
            Command::ExecuteAll => self.execute(false),
            Command::ExecuteCurrent => self.execute(true),
            Command::Cancel => {
                if let Some(conn) = &self.conn {
                    conn.session.cancel();
                }
            }
            Command::Commit => {
                if self.conn.as_ref().is_some_and(|c| c.profile.production) {
                    self.confirm = Some(Confirm {
                        action: ConfirmAction::Commit,
                        title: "Commit on production".into(),
                        text: format!(
                            "Commit the transaction on production connection {}?",
                            self.conn
                                .as_ref()
                                .map(|c| c.profile.display_name())
                                .unwrap_or_default()
                        ),
                        yes: "Commit",
                    });
                } else {
                    self.send(DbRequest::Commit);
                }
            }
            Command::Rollback => self.send(DbRequest::Rollback),
            Command::ToggleAutocommit => {
                if let Some(conn) = &mut self.conn {
                    conn.autocommit = !conn.autocommit;
                    conn.session.send(DbRequest::SetAutocommit(conn.autocommit));
                }
            }
            Command::ShowPalette => self.palette.toggle(),
            Command::ShowObjectBrowser => self.open_tab(Tab::ObjectBrowser),
            Command::ShowOutput => self.open_tab(Tab::Log),
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
            Command::ResetLayout => {
                self.windows
                    .retain(|_, w| !w.text.is_empty() || w.is_running());
                self.reset_layout();
            }
            Command::About => self.about_open = true,
            Command::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
        }
    }

    fn open_tab(&mut self, tab: Tab) {
        match self.dock.find_tab(&tab) {
            Some(path) => {
                let _ = self.dock.set_active_tab(path);
            }
            None => self.dock.push_to_focused_leaf(tab),
        }
    }

    fn close_window(&mut self, id: WindowId) {
        if let Some(window) = self.windows.get(&id)
            && window.is_running()
            && let Some(conn) = &self.conn
        {
            conn.session.cancel();
        }
        self.windows.remove(&id);
        self.requests.retain(|_, w| *w != id);
        if self.focused == Some(id) {
            self.focused = self.windows.keys().next().copied();
        }
    }

    fn send(&mut self, request: DbRequest) {
        match &self.conn {
            Some(conn) => {
                self.last_window = self.focused;
                conn.session.send(request);
            }
            None => self.log("Not connected"),
        }
    }

    fn execute(&mut self, current_only: bool) {
        let Some(id) = self.focused else {
            return;
        };
        let connected = matches!(
            self.conn,
            Some(ActiveConnection {
                status: ConnStatus::Connected(_),
                ..
            })
        );
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        if !connected {
            window.info("Not connected. Use Session → Connect… first.");
            self.connect_dialog.show(self.last_profile.as_ref());
            return;
        }
        if window.is_running() {
            return;
        }
        let statements = if current_only {
            window.statement_current()
        } else {
            window.statements_all()
        };
        if statements.is_empty() {
            window.info("Nothing to execute");
            return;
        }
        window.start(statements);
        self.run_next(id);
    }

    /// Sends the next queued statement of window `id`, skipping what can't run.
    fn run_next(&mut self, id: WindowId) {
        let Some(conn) = &self.conn else {
            return;
        };
        let Some(window) = self.windows.get_mut(&id) else {
            return;
        };
        while let Some(stmt) = window.state.queue.pop_front() {
            if stmt.kind == StatementKind::SqlPlus {
                let first_line = stmt.sql.lines().next().unwrap_or_default();
                window.info(format!(
                    "Skipped SQL*Plus command (not supported yet): {first_line}"
                ));
                continue;
            }
            if conn.profile.read_only && !is_query(&stmt.sql) {
                window.info("Connection is read-only: only queries are allowed");
                window.state.queue.clear();
                return;
            }
            let request = self.next_request;
            self.next_request += 1;
            self.requests.insert(request, id);
            self.last_window = Some(id);
            window.state.running = Some(request);
            conn.session.send(DbRequest::Execute {
                id: request,
                sql: stmt.sql,
                max_rows: self.settings.fetch_size,
                binds: Vec::new(),
            });
            return;
        }
    }

    fn connect(
        &mut self,
        ctx: &egui::Context,
        profile: ConnectionProfile,
        password: String,
        save_profile: bool,
    ) {
        if self.conn.is_some() {
            self.disconnect();
        }
        if save_profile && let Some(paths) = &self.paths {
            self.saved.upsert(profile.clone());
            if let Err(e) = save_toml(&paths.connections_file(), &self.saved) {
                error!("saving connections: {e:#}");
            }
        }
        let params = ConnectParams {
            username: profile.username.clone(),
            password,
            connect_string: profile.connect_string.clone(),
            privilege: profile.privilege,
        };
        let repaint = ctx.clone();
        let session = Session::connect(params, move || repaint.request_repaint());
        self.log(format!("Connecting to {}…", profile.display_name()));
        self.last_profile = Some(profile.clone());
        self.conn = Some(ActiveConnection {
            profile,
            session,
            status: ConnStatus::Connecting,
            pending_tx: false,
            autocommit: false,
        });
    }

    fn disconnect(&mut self) {
        if let Some(conn) = self.conn.take() {
            self.log(format!("Disconnected from {}", conn.profile.display_name()));
            // Dropping the session sends `Disconnect` to the worker.
        }
        for window in self.windows.values_mut() {
            window.state.running = None;
            window.state.fetching = None;
            window.state.queue.clear();
        }
        self.requests.clear();
    }

    // ----- database events ------------------------------------------------

    fn pump_events(&mut self) {
        let Some(conn) = &self.conn else {
            return;
        };
        let events: Vec<DbEvent> = std::iter::from_fn(|| conn.session.try_recv()).collect();
        for event in events {
            self.handle_event(event);
        }
    }

    fn handle_event(&mut self, event: DbEvent) {
        let Some(conn) = &mut self.conn else {
            return;
        };
        match event {
            DbEvent::Connected(server) => {
                let line = format!(
                    "Connected to {} (Oracle {}, schema {})",
                    conn.profile.display_name(),
                    server.version,
                    server.current_schema
                );
                conn.status = ConnStatus::Connected(server);
                self.connect_dialog.connected();
                self.log(line);
            }
            DbEvent::ConnectFailed(e) => {
                let message = if e.is_missing_client() {
                    format!("{MISSING_CLIENT_HINT}\n\n{e}")
                } else {
                    e.to_string()
                };
                warn!("connect failed: {e}");
                self.conn = None;
                self.connect_dialog.failed(message);
            }
            DbEvent::Transaction { pending } => conn.pending_tx = pending,
            DbEvent::Meta { .. } => {}
            DbEvent::Disconnected => self.conn = None,
            DbEvent::QueryStarted { id, .. }
            | DbEvent::Rows { id, .. }
            | DbEvent::Executed { id, .. }
            | DbEvent::Failed { id, .. } => {
                let Some(&window_id) = self.requests.get(&id) else {
                    return;
                };
                let Some(window) = self.windows.get_mut(&window_id) else {
                    return;
                };
                if let Next::Continue = window.handle_event(event) {
                    self.run_next(window_id);
                }
            }
            event @ (DbEvent::Output(_) | DbEvent::Notice(_) | DbEvent::Error(_)) => {
                match self.last_window.and_then(|id| self.windows.get_mut(&id)) {
                    Some(window) => {
                        window.handle_event(event);
                    }
                    None => self.log(format!("{event:?}")),
                }
            }
        }
    }

    // ----- ui -------------------------------------------------------------

    fn menu_bar(&mut self, ui: &mut Ui, commands: &mut Vec<Command>) {
        let keymap = &self.keymap;
        let mut item = |ui: &mut Ui, command: Command| {
            let shortcut = keymap
                .shortcut(command)
                .map(|s| ui.ctx().format_shortcut(s))
                .unwrap_or_default();
            if ui
                .add(egui::Button::new(command.short_label()).shortcut_text(shortcut))
                .clicked()
            {
                commands.push(command);
            }
        };
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                item(ui, Command::NewSqlWindow);
                item(ui, Command::CloseTab);
                ui.separator();
                item(ui, Command::Quit);
            });
            ui.menu_button("Session", |ui| {
                item(ui, Command::Connect);
                item(ui, Command::Disconnect);
                ui.separator();
                item(ui, Command::ExecuteAll);
                item(ui, Command::ExecuteCurrent);
                item(ui, Command::Cancel);
                ui.separator();
                item(ui, Command::Commit);
                item(ui, Command::Rollback);
                item(ui, Command::ToggleAutocommit);
            });
            ui.menu_button("View", |ui| {
                item(ui, Command::ShowPalette);
                item(ui, Command::ShowObjectBrowser);
                item(ui, Command::ShowOutput);
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
        let connected = self.conn.is_some();
        let running = self
            .focused
            .and_then(|id| self.windows.get(&id))
            .is_some_and(SqlWindow::is_running);
        let ctx = ui.ctx().clone();
        let tip = |c: Command| match self.keymap.shortcut(c) {
            Some(s) => format!("{} ({})", c.short_label(), ctx.format_shortcut(s)),
            None => c.short_label().to_owned(),
        };
        ui.horizontal(|ui| {
            let mut button = |ui: &mut Ui, text: &str, command: Command, enabled: bool| {
                if ui
                    .add_enabled(enabled, egui::Button::new(text))
                    .on_hover_text(tip(command))
                    .clicked()
                {
                    commands.push(command);
                }
            };
            button(ui, "🗋 New", Command::NewSqlWindow, true);
            ui.separator();
            if connected {
                button(ui, "⏏ Disconnect", Command::Disconnect, true);
            } else {
                button(ui, "🔌 Connect", Command::Connect, true);
            }
            ui.separator();
            button(ui, "▶ Execute", Command::ExecuteAll, connected && !running);
            button(ui, "⏹ Break", Command::Cancel, running);
            ui.separator();
            button(ui, "✔ Commit", Command::Commit, connected);
            button(ui, "↶ Rollback", Command::Rollback, connected);
            let mut autocommit = self.conn.as_ref().is_some_and(|c| c.autocommit);
            if ui
                .add_enabled(
                    connected,
                    egui::Checkbox::new(&mut autocommit, "Auto-commit"),
                )
                .changed()
            {
                commands.push(Command::ToggleAutocommit);
            }
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            match &self.conn {
                None => {
                    ui.weak("Not connected");
                }
                Some(conn) => {
                    let name = conn.profile.display_name();
                    let color = if conn.profile.production {
                        Color32::from_rgb(0xe0, 0x30, 0x30)
                    } else {
                        conn.profile
                            .color
                            .map_or(ui.visuals().text_color(), |[r, g, b]| {
                                Color32::from_rgb(r, g, b)
                            })
                    };
                    match &conn.status {
                        ConnStatus::Connecting => {
                            ui.spinner();
                            ui.label(format!("Connecting to {name}…"));
                        }
                        ConnStatus::Connected(server) => {
                            let marker = if conn.profile.production {
                                "● PRODUCTION "
                            } else {
                                "● "
                            };
                            ui.label(RichText::new(format!("{marker}{name}")).color(color))
                                .on_hover_text(&server.banner);
                        }
                    }
                    ui.separator();
                    if conn.pending_tx {
                        ui.label(
                            RichText::new("Transaction pending")
                                .color(Color32::from_rgb(0xe0, 0x90, 0x20)),
                        )
                        .on_hover_text(
                            "Uncommitted changes in this session (F10 commit, Shift+F10 rollback)",
                        );
                    } else {
                        ui.weak("No transaction");
                    }
                    if conn.autocommit {
                        ui.separator();
                        ui.label("Auto-commit");
                    }
                    if conn.profile.read_only {
                        ui.separator();
                        ui.label("Read-only");
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak("UTF-8");
                if let Some(window) = self.focused.and_then(|id| self.windows.get(&id)) {
                    ui.separator();
                    let (line, col) = window.state.line_col;
                    ui.label(format!("Ln {line}, Col {col}"));
                    if let Some(status) = window.result_status() {
                        ui.separator();
                        ui.label(status);
                    }
                }
            });
        });
    }

    fn handle_close_request(&mut self, ctx: &egui::Context) {
        let close_requested = ctx.input(|i| i.viewport().close_requested());
        if close_requested
            && !self.quit_confirmed
            && self.conn.as_ref().is_some_and(|c| c.pending_tx)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm = Some(Confirm {
                action: ConfirmAction::Quit,
                title: "Uncommitted transaction".into(),
                text: "The session has uncommitted changes. Exiting rolls them back.".into(),
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
}

impl eframe::App for OruiApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.pump_events();
        self.handle_close_request(&ctx);

        let modal_open = self.connect_dialog.open || self.confirm.is_some() || self.about_open;
        let mut commands = if modal_open {
            Vec::new()
        } else {
            self.keymap.triggered(&ctx)
        };

        egui::Panel::top("menu").show(ui, |ui| self.menu_bar(ui, &mut commands));
        egui::Panel::top("toolbar").show(ui, |ui| self.toolbar(ui, &mut commands));
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));

        if let Some((_, Tab::Sql(id))) = self.dock.find_active_focused() {
            self.focused = Some(*id);
        }
        let mut viewer = Viewer {
            windows: &mut self.windows,
            log: &self.log,
            settings: &self.settings,
            fetch_more: Vec::new(),
            closed: Vec::new(),
            connection: self.conn.as_ref().map(|c| c.profile.display_name()),
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
            fetch_more, closed, ..
        } = viewer;
        for id in closed {
            self.close_window(id);
        }
        for (window, id) in fetch_more {
            if let Some(conn) = &self.conn {
                self.last_window = Some(window);
                conn.session.send(DbRequest::FetchMore {
                    id,
                    max_rows: self.settings.fetch_size,
                });
            }
        }

        if let Some(request) = self.connect_dialog.ui(&ctx, &self.saved) {
            self.connect(
                &ctx,
                request.profile,
                request.password,
                request.save_profile,
            );
        }
        if let Some(command) = self.palette.ui(&ctx, &self.keymap) {
            commands.push(command);
        }
        if let Some(confirm) = &self.confirm
            && let Some(yes) = confirm_ui(&ctx, confirm)
        {
            let action = confirm.action;
            self.confirm = None;
            if yes {
                match action {
                    ConfirmAction::Commit => self.send(DbRequest::Commit),
                    ConfirmAction::Disconnect => self.disconnect(),
                    ConfirmAction::Quit => {
                        self.quit_confirmed = true;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            }
        }
        self.about_ui(&ctx);

        for command in commands {
            self.run_command(&ctx, command);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        let workspace = Workspace {
            dock: self.dock.clone(),
            windows: std::mem::take(&mut self.windows),
            next_window: self.next_window,
            last_profile: self.last_profile.clone(),
        };
        eframe::set_value(storage, WORKSPACE_KEY, &workspace);
        self.windows = workspace.windows;
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.disconnect();
        self.save_settings();
    }
}

struct Viewer<'a> {
    windows: &'a mut BTreeMap<WindowId, SqlWindow>,
    log: &'a [String],
    settings: &'a Settings,
    connection: Option<String>,
    /// (window, query id) pairs whose grid asked for more rows.
    fetch_more: Vec<(WindowId, u64)>,
    closed: Vec<WindowId>,
}

impl TabViewer for Viewer<'_> {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::ObjectBrowser => "Objects".into(),
            Tab::Log => "Log".into(),
            Tab::Sql(id) => self
                .windows
                .get(id)
                .map_or_else(|| "?".into(), |w| w.tab_title().into()),
        }
    }

    fn ui(&mut self, ui: &mut Ui, tab: &mut Tab) {
        match tab {
            Tab::ObjectBrowser => {
                ui.add_space(4.0);
                match &self.connection {
                    Some(name) => ui.label(RichText::new(name).strong()),
                    None => ui.weak("Not connected"),
                };
                ui.add_space(8.0);
                ui.weak("The object browser arrives with milestone M3.");
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
            Tab::Sql(id) => {
                if let Some(window) = self.windows.get_mut(id) {
                    let editor = &self.settings.editor;
                    let response = window.ui(ui, *id, editor.font_size, editor.show_line_numbers);
                    if let Some(query) = response.fetch_more {
                        self.fetch_more.push((*id, query));
                    }
                }
            }
        }
    }

    fn on_close(&mut self, tab: &mut Tab) -> egui_dock::widgets::tab_viewer::OnCloseResponse {
        if let Tab::Sql(id) = tab {
            self.closed.push(*id);
        }
        egui_dock::widgets::tab_viewer::OnCloseResponse::Close
    }

    fn scroll_bars(&self, _tab: &Tab) -> [bool; 2] {
        [false, false]
    }
}

fn load_or_default<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    load_toml(path).unwrap_or_else(|e| {
        error!("{e:#}");
        T::default()
    })
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
