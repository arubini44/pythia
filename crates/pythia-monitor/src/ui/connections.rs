//! Session browser of MySQL, PostgreSQL and SQL Server: every session in a
//! sortable table, and the blocking tree. A session opens in the details
//! tab of its database; the menu ends its statement or the session.

use std::collections::HashSet;

use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use pythia_db::DbKind;

use super::style::{self, Palette, duration};
use super::{MonitorAction, Refresh, Want, Wants, refresh_selector};
use crate::connections::{
    BlockingNode, Connection, ConnectionFilter, ConnectionList, blocking_tree,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Table,
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortCol {
    /// Blocked first, then blockers, then active by time, idle last.
    #[default]
    Activity,
    Id,
    User,
    Host,
    Database,
    Program,
    Status,
    Wait,
    Time,
    Transaction,
    Blocker,
}

/// What a database calls its sessions and how they end.
struct Words {
    /// The id column, e.g. "PID".
    id: &'static str,
    status: &'static str,
    wait: &'static str,
    /// Ending the statement: menu item, dialog heading; none if the
    /// database can only end the session.
    end_query: Option<&'static str>,
    end_session: &'static str,
    /// For "Thread 12", "Backend 4711".
    noun: &'static str,
}

fn words(kind: DbKind) -> Words {
    match kind {
        DbKind::Postgres => Words {
            id: "PID",
            status: "State",
            wait: "Wait event",
            end_query: Some("Cancel query"),
            end_session: "Terminate",
            noun: "backend",
        },
        DbKind::SqlServer => Words {
            id: "Session",
            status: "Status",
            wait: "Wait",
            end_query: None,
            end_session: "Kill session",
            noun: "session",
        },
        _ => Words {
            id: "Thread",
            status: "Command",
            wait: "State",
            end_query: Some("Kill query"),
            end_session: "Kill connection",
            noun: "thread",
        },
    }
}

/// Asks before ending a session or its statement.
struct KillConfirm {
    id: i64,
    who: String,
    query_only: bool,
    transaction_secs: Option<f64>,
}

#[derive(Default)]
pub struct ConnectionsView {
    kind: DbKind,
    pub refresh: Refresh,
    mode: Mode,
    filter: String,
    /// An exact filter from the dashboard, shown as a chip.
    field: Option<ConnectionFilter>,
    background: bool,
    active_only: bool,
    list: ConnectionList,
    tree: Vec<BlockingNode>,
    loaded: bool,
    error: Option<String>,
    in_flight: bool,
    last_request: Option<f64>,
    force: bool,
    sort: SortCol,
    descending: bool,
    collapsed: HashSet<i64>,
    confirm: Option<KillConfirm>,
}

impl ConnectionsView {
    pub fn set_kind(&mut self, kind: DbKind) {
        self.kind = kind;
    }

    /// Shows the blocking tree first.
    pub fn show_blocking(&mut self) {
        self.mode = Mode::Blocking;
    }

    /// Shows the session list, with only the sessions `field` matches.
    pub fn show_table(&mut self, field: Option<ConnectionFilter>) {
        self.mode = Mode::Table;
        self.field = field;
    }

    pub(crate) fn refresh_now(&mut self) {
        self.force = true;
    }

    pub(crate) fn on_connections(&mut self, result: Result<ConnectionList, String>) {
        self.in_flight = false;
        self.loaded = true;
        match result {
            Ok(list) => {
                self.tree = blocking_tree(&list.rows);
                self.list = list;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// The sessions listed, for tests.
    pub fn rows(&self) -> &[Connection] {
        &self.list.rows
    }

    fn matches(&self, c: &Connection) -> bool {
        if self.active_only && !c.active && !c.is_blocked() {
            return false;
        }
        if self.field.as_ref().is_some_and(|f| !f.matches(c)) {
            return false;
        }
        let f = self.filter.trim().to_lowercase();
        if f.is_empty() {
            return true;
        }
        let hay = [
            c.user.as_deref(),
            c.host.as_deref(),
            c.database.as_deref(),
            c.program.as_deref(),
            c.wait.as_deref(),
            c.query.as_deref(),
            Some(c.status.as_str()),
        ];
        c.id.to_string() == f || hay.iter().flatten().any(|h| h.to_lowercase().contains(&f))
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, now: f64, wants: &mut Wants) {
        let mut refresh_now = std::mem::take(&mut self.force);
        let rows = &self.list.rows;
        let blocked = rows.iter().filter(|c| c.is_blocked()).count();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Table, "Sessions");
            let label = if blocked > 0 {
                RichText::new(format!("Blocking tree ({blocked})")).color(Palette::RED)
            } else {
                RichText::new("Blocking tree")
            };
            ui.selectable_value(&mut self.mode, Mode::Blocking, label);
            ui.separator();
            ui.add(
                egui::TextEdit::singleline(&mut self.filter)
                    .hint_text("Filter: user, host, database, statement…")
                    .desired_width(240.0),
            );
            if let Some(field) = &self.field {
                let chip = egui::Button::new(
                    RichText::new(format!("{}  ×", field.label()))
                        .color(ui.visuals().hyperlink_color),
                )
                .small();
                if ui.add(chip).on_hover_text("Show all sessions").clicked() {
                    self.field = None;
                }
            }
            ui.checkbox(&mut self.active_only, "Active only");
            if ui
                .checkbox(&mut self.background, "Background")
                .on_hover_text("The server's own threads and processes too")
                .changed()
            {
                refresh_now = true;
            }
            if self.sort != SortCol::Activity
                && ui
                    .small_button("Activity order")
                    .on_hover_text("Blocked and long-running sessions first")
                    .clicked()
            {
                self.sort = SortCol::Activity;
                self.descending = false;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                refresh_selector(ui, "connections-refresh", &mut self.refresh);
                if ui.button("⟳").on_hover_text("Refresh now").clicked() {
                    refresh_now = true;
                }
                if self.in_flight {
                    ui.spinner();
                }
                let active = rows.iter().filter(|c| c.active).count();
                ui.weak(format!(
                    "{} sessions · {active} active · {blocked} blocked",
                    rows.len()
                ));
            });
        });
        for note in &self.list.notes {
            ui.weak(note);
        }
        ui.separator();

        if (refresh_now && !self.in_flight)
            || self.refresh.due(ui, now, self.last_request, self.in_flight)
        {
            self.in_flight = true;
            self.last_request = Some(now);
            wants.push(Want::Connections {
                background: self.background,
            });
        }
        if let Some(e) = &self.error {
            style::unavailable(ui, "Sessions are not available", e);
            return;
        }
        if !self.loaded {
            ui.spinner();
            return;
        }

        let words = words(self.kind);
        let mut actions = Vec::new();
        match self.mode {
            Mode::Table => self.table(ui, &words, &mut actions),
            Mode::Blocking => self.blocking(ui, &words, &mut actions),
        }
        for a in actions {
            match a {
                RowAction::Want(w) => wants.push(w),
                RowAction::Kill(k) => self.confirm = Some(k),
            }
        }
        self.kill_dialog(ui.ctx(), &words, wants);
    }

    fn sorted(&self) -> Vec<&Connection> {
        let rows = &self.list.rows;
        let mut shown: Vec<&Connection> = rows.iter().filter(|c| self.matches(c)).collect();
        let blockers: HashSet<i64> = rows.iter().filter_map(|c| c.blocked_by).collect();
        let secs = |s: f64| (s * 1000.0) as i64;
        let key = |c: &Connection| -> (i64, String) {
            let name = |v: &Option<String>| v.clone().unwrap_or_default().to_lowercase();
            match self.sort {
                SortCol::Activity => {
                    let rank = if c.is_blocked() {
                        0
                    } else if blockers.contains(&c.id) {
                        1
                    } else if c.active {
                        2
                    } else {
                        3
                    };
                    // Within a rank: longest running first.
                    (
                        rank * 1_000_000_000_000 - secs(c.seconds).min(999_999_999_999),
                        String::new(),
                    )
                }
                SortCol::Id => (c.id, String::new()),
                SortCol::User => (0, name(&c.user)),
                SortCol::Host => (0, name(&c.host)),
                SortCol::Database => (0, name(&c.database)),
                SortCol::Program => (0, name(&c.program)),
                SortCol::Status => (0, c.status.to_lowercase()),
                SortCol::Wait => (0, name(&c.wait)),
                SortCol::Time => (secs(c.seconds), String::new()),
                SortCol::Transaction => (c.transaction_secs.map_or(-1, secs), String::new()),
                SortCol::Blocker => (c.blocked_by.unwrap_or(-1), String::new()),
            }
        };
        shown.sort_by(|a, b| key(a).cmp(&key(b)).then(a.id.cmp(&b.id)));
        if self.descending {
            shown.reverse();
        }
        shown
    }

    fn table(&mut self, ui: &mut Ui, words: &Words, actions: &mut Vec<RowAction>) {
        let rows = self.sorted();
        let h = style::row_height(ui);
        let blocked_bg = Palette::RED.gamma_multiply(0.14);
        let blocker_bg = Palette::ORANGE.gamma_multiply(0.14);
        let blockers: HashSet<i64> = self.list.rows.iter().filter_map(|c| c.blocked_by).collect();
        // MySQL has no program name in its process list.
        let program = self.kind != DbKind::MySql;
        let mut columns: Vec<(&str, SortCol, Column)> = vec![
            (words.id, SortCol::Id, Column::initial(64.0)),
            ("User", SortCol::User, Column::initial(90.0).clip(true)),
            ("Host", SortCol::Host, Column::initial(110.0).clip(true)),
            (
                "Database",
                SortCol::Database,
                Column::initial(90.0).clip(true),
            ),
        ];
        if program {
            columns.push((
                "Program",
                SortCol::Program,
                Column::initial(120.0).clip(true),
            ));
        }
        columns.extend([
            (
                words.status,
                SortCol::Status,
                Column::initial(80.0).clip(true),
            ),
            (words.wait, SortCol::Wait, Column::initial(120.0).clip(true)),
            ("Time", SortCol::Time, Column::initial(64.0)),
            ("Transaction", SortCol::Transaction, Column::initial(72.0)),
            ("Blocker", SortCol::Blocker, Column::initial(56.0)),
        ]);
        let mut sort_click = None;
        let mut table = TableBuilder::new(ui)
            .id_salt("connections-table")
            .striped(true)
            .resizable(true)
            .auto_shrink(false)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
        for (_, _, column) in &columns {
            table = table.column(*column);
        }
        table
            .column(Column::remainder().clip(true))
            .header(h + 2.0, |mut r| {
                for (title, col, _) in &columns {
                    r.col(|ui| {
                        let arrow = match (*col == self.sort, self.descending) {
                            (true, true) => " ⏷",
                            (true, false) => " ⏶",
                            _ => "",
                        };
                        let resp = ui.add(
                            egui::Label::new(RichText::new(format!("{title}{arrow}")).strong())
                                .sense(egui::Sense::click()),
                        );
                        if resp.clicked() {
                            sort_click = Some(*col);
                        }
                    });
                }
                r.col(|ui| {
                    ui.strong("Statement");
                });
            })
            .body(|body| {
                body.rows(h, rows.len(), |mut row| {
                    let c = rows[row.index()];
                    let bg = if c.is_blocked() {
                        Some(blocked_bg)
                    } else if blockers.contains(&c.id) {
                        Some(blocker_bg)
                    } else {
                        None
                    };
                    let cell = |row: &mut egui_extras::TableRow, add: &dyn Fn(&mut Ui)| {
                        row.col(|ui| {
                            if let Some(bg) = bg {
                                ui.painter().rect_filled(
                                    ui.max_rect().expand2(egui::vec2(4.0, 2.0)),
                                    0.0,
                                    bg,
                                );
                            }
                            add(ui);
                        });
                    };
                    let right = |ui: &mut Ui, text: String| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(text);
                        });
                    };
                    let opt = |ui: &mut Ui, v: &Option<String>| {
                        ui.label(v.as_deref().unwrap_or("—"));
                    };
                    cell(&mut row, &|ui| {
                        ui.label(RichText::new(c.id.to_string()).monospace());
                    });
                    cell(&mut row, &|ui| opt(ui, &c.user));
                    cell(&mut row, &|ui| opt(ui, &c.host));
                    cell(&mut row, &|ui| opt(ui, &c.database));
                    if program {
                        cell(&mut row, &|ui| {
                            ui.label(c.program.as_deref().unwrap_or(""));
                        });
                    }
                    cell(&mut row, &|ui| status_label(ui, c));
                    cell(&mut row, &|ui| {
                        if let Some(w) = &c.wait {
                            ui.label(w);
                        }
                    });
                    cell(&mut row, &|ui| right(ui, duration(c.seconds)));
                    cell(&mut row, &|ui| {
                        if let Some(t) = c.transaction_secs {
                            right(ui, duration(t));
                        }
                    });
                    cell(&mut row, &|ui| {
                        if let Some(b) = c.blocked_by {
                            ui.label(RichText::new(b.to_string()).color(Palette::RED).strong())
                                .on_hover_text(format!("Waiting {}", duration(c.wait_secs)));
                        }
                    });
                    cell(&mut row, &|ui| statement(ui, &c.query));
                    let resp = row.response();
                    if resp.double_clicked() {
                        actions.push(RowAction::Want(Want::OpenConnection { id: c.id }));
                    }
                    resp.context_menu(|ui| menu(ui, c, false, words, actions));
                });
            });
        if let Some(c) = sort_click {
            if self.sort == c {
                self.descending = !self.descending;
            } else {
                self.sort = c;
                self.descending = matches!(c, SortCol::Time | SortCol::Transaction);
            }
        }
    }

    fn blocking(&mut self, ui: &mut Ui, words: &Words, actions: &mut Vec<RowAction>) {
        if self.tree.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No blocking sessions")
                        .heading()
                        .color(Palette::GREEN),
                );
                ui.weak("No session is currently waiting on a lock held by another session.");
            });
            return;
        }
        let mut toggles = Vec::new();
        egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
            for root in &self.tree {
                let summary = format!(
                    "blocks {} session(s) · longest wait {}",
                    root.blocked_count(),
                    duration(root.max_wait())
                );
                style::card(ui).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(summary)
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    );
                    node_ui(ui, root, 0, &self.collapsed, &mut toggles, words, actions);
                });
                ui.add_space(6.0);
            }
        });
        for id in toggles {
            if !self.collapsed.remove(&id) {
                self.collapsed.insert(id);
            }
        }
    }

    fn kill_dialog(&mut self, ctx: &egui::Context, words: &Words, wants: &mut Wants) {
        let Some(k) = &self.confirm else {
            return;
        };
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new("kill-connection")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            let heading = match words.end_query {
                Some(end) if k.query_only => end,
                _ => words.end_session,
            };
            ui.heading(heading);
            ui.add_space(6.0);
            if k.query_only {
                ui.label(format!(
                    "End the statement {} {} ({}) runs? The session stays.",
                    words.noun, k.id, k.who
                ));
            } else {
                ui.label(format!("End {} {} ({})?", words.noun, k.id, k.who));
                match k.transaction_secs {
                    Some(t) => {
                        ui.label(
                            RichText::new(format!(
                                "Its transaction, open for {}, is rolled back.",
                                duration(t)
                            ))
                            .color(Palette::ORANGE),
                        );
                    }
                    None => {
                        ui.label("Its open transaction, if any, is rolled back.");
                    }
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let go = egui::Button::new(RichText::new(heading).color(Color32::WHITE))
                    .fill(Palette::RED);
                if ui.add(go).clicked() {
                    decision = Some(true);
                }
                if ui.button("Keep").clicked() {
                    decision = Some(false);
                }
            });
        });
        if modal.should_close() && decision.is_none() {
            decision = Some(false);
        }
        if let Some(yes) = decision {
            if yes {
                wants.push(Want::KillConnection {
                    id: k.id,
                    query_only: k.query_only,
                });
            }
            self.confirm = None;
        }
    }
}

enum RowAction {
    Want(Want),
    Kill(KillConfirm),
}

fn status_label(ui: &mut Ui, c: &Connection) {
    let color = if c.active {
        Palette::GREEN
    } else if c.idle_in_transaction() {
        Palette::ORANGE
    } else {
        ui.visuals().weak_text_color()
    };
    let label = ui.label(RichText::new(&c.status).color(color).small());
    if c.idle_in_transaction() {
        label.on_hover_text("Idle with a transaction open");
    }
}

fn statement(ui: &mut Ui, query: &Option<String>) {
    if let Some(q) = query {
        ui.add(
            egui::Label::new(RichText::new(q.replace('\n', " ")).monospace().size(11.5)).truncate(),
        )
        .on_hover_text(q);
    }
}

fn node_ui(
    ui: &mut Ui,
    node: &BlockingNode,
    depth: usize,
    collapsed: &HashSet<i64>,
    toggles: &mut Vec<i64>,
    words: &Words,
    actions: &mut Vec<RowAction>,
) {
    let c = &node.connection;
    let is_root = depth == 0;
    let open = !collapsed.contains(&c.id);
    let resp = ui
        .horizontal(|ui| {
            ui.add_space(depth as f32 * 22.0);
            if node.children.is_empty() {
                ui.add_space(18.0);
            } else if ui
                .add(
                    egui::Button::new(if open { "⏷" } else { "⏵" })
                        .frame(false)
                        .small(),
                )
                .clicked()
            {
                toggles.push(c.id);
            }
            let (marker, _) = ui.allocate_exact_size(egui::vec2(4.0, 16.0), egui::Sense::hover());
            ui.painter().rect_filled(
                marker,
                2.0,
                if is_root {
                    Palette::RED
                } else {
                    Palette::ORANGE
                },
            );
            let title = format!("{}  {}", c.id, c.user.as_deref().unwrap_or("—"));
            let title = if is_root {
                RichText::new(title).strong()
            } else {
                RichText::new(title)
            };
            ui.label(title.monospace());
            if is_root {
                let status = if node.unknown {
                    "not in the session list"
                } else if c.active {
                    "active"
                } else {
                    "idle — likely an uncommitted transaction"
                };
                ui.label(
                    RichText::new(format!("final blocker · {status}"))
                        .small()
                        .color(Palette::RED),
                );
            } else {
                if let Some(w) = &c.wait {
                    ui.label(w);
                }
                ui.label(
                    RichText::new(format!("waiting {}", duration(c.wait_secs)))
                        .color(Palette::RED)
                        .small(),
                );
            }
            if let Some(h) = &c.host {
                ui.weak(h);
            }
            statement(ui, &c.query);
        })
        .response
        .interact(egui::Sense::click());
    if resp.double_clicked() && !node.unknown {
        actions.push(RowAction::Want(Want::OpenConnection { id: c.id }));
    }
    resp.context_menu(|ui| menu(ui, c, node.unknown, words, actions));
    if open {
        for child in &node.children {
            node_ui(ui, child, depth + 1, collapsed, toggles, words, actions);
        }
    }
}

/// Details, copying, and ending the statement or the session.
fn menu(ui: &mut Ui, c: &Connection, unknown: bool, words: &Words, actions: &mut Vec<RowAction>) {
    const UNKNOWN: &str = "Not in the session list";
    if ui
        .add_enabled(!unknown, egui::Button::new("Details"))
        .on_disabled_hover_text(UNKNOWN)
        .clicked()
    {
        actions.push(RowAction::Want(Want::OpenConnection { id: c.id }));
        ui.close();
    }
    if ui.button(format!("Copy {}", words.id)).clicked() {
        ui.ctx().copy_text(c.id.to_string());
        ui.close();
    }
    if let Some(q) = &c.query
        && ui
            .button("Open statement in SQL window")
            .on_hover_text("Run it, explain it")
            .clicked()
    {
        actions.push(RowAction::Want(Want::Act(MonitorAction::OpenSql(
            q.clone(),
        ))));
        ui.close();
    }
    ui.separator();
    let possible = !unknown && !c.background;
    let who = c
        .user
        .clone()
        .or_else(|| c.program.clone())
        .unwrap_or_default();
    let mut item = |ui: &mut Ui, label: String, query_only: bool| {
        if ui
            .add_enabled(possible, egui::Button::new(label))
            .on_disabled_hover_text("Not possible for this session")
            .clicked()
        {
            actions.push(RowAction::Kill(KillConfirm {
                id: c.id,
                who: who.clone(),
                query_only,
                transaction_secs: c.transaction_secs,
            }));
            ui.close();
        }
    };
    if let Some(end) = words.end_query {
        item(ui, format!("{end}…"), true);
    }
    item(ui, format!("{}…", words.end_session), false);
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn list() -> ConnectionList {
        let conn = |id, user: &str, active, blocked_by| Connection {
            id,
            user: Some(user.into()),
            host: Some("app-1".into()),
            status: if active { "Query" } else { "Sleep" }.into(),
            active,
            blocked_by,
            wait_secs: 4.0,
            ..Connection::default()
        };
        ConnectionList {
            rows: vec![
                Connection {
                    transaction_secs: Some(90.0),
                    ..conn(11, "app", false, None)
                },
                conn(12, "app", true, Some(11)),
                conn(13, "report", true, None),
            ],
            notes: vec!["Only this user's sessions are listed.".into()],
        }
    }

    fn harness(view: ConnectionsView) -> Harness<'static, (ConnectionsView, Wants)> {
        Harness::builder()
            .with_size(egui::vec2(1400.0, 600.0))
            .build_ui_state(
                |ui, (view, wants): &mut (ConnectionsView, Wants)| view.ui(ui, 0.0, wants),
                (view, Wants::default()),
            )
    }

    /// Every session is listed, idle ones too, with what keeps others out
    /// of view; a field filter from the dashboard narrows the list.
    #[test]
    fn lists_and_filters() {
        let mut view = ConnectionsView::default();
        view.set_kind(DbKind::MySql);
        view.on_connections(Ok(list()));
        let mut h = harness(view);
        h.run_steps(2);
        assert!(h.query_by_label("Thread").is_some());
        assert!(h.query_by_label("Program").is_none(), "not on MySQL");
        // 11 as a session and as 12's blocker.
        assert_eq!(h.query_all_by_label("11").count(), 2);
        assert!(h.query_by_label("13").is_some());
        assert!(
            h.query_by_label("Only this user's sessions are listed.")
                .is_some()
        );
        assert!(
            h.query_by_label("3 sessions · 2 active · 1 blocked")
                .is_some()
        );
        h.state_mut()
            .0
            .show_table(Some(ConnectionFilter::User("report".into())));
        h.run_steps(2);
        assert!(h.query_by_label("11").is_none());
        assert!(h.query_by_label("13").is_some());
        assert!(h.query_by_label("user report  ×").is_some());
    }

    /// The blocking tree puts the blocker on top; ending it asks first and
    /// says what is rolled back.
    #[test]
    fn blocking_and_kill() {
        let mut view = ConnectionsView::default();
        view.set_kind(DbKind::Postgres);
        view.on_connections(Ok(list()));
        view.show_blocking();
        let mut h = harness(view);
        h.run_steps(2);
        assert!(h.query_by_label("Blocking tree (1)").is_some());
        assert!(
            h.query_by_label("final blocker · idle — likely an uncommitted transaction")
                .is_some()
        );
        h.state_mut().0.confirm = Some(KillConfirm {
            id: 11,
            who: "app".into(),
            query_only: false,
            transaction_secs: Some(90.0),
        });
        h.run_steps(2);
        assert!(h.query_by_label("End backend 11 (app)?").is_some());
        assert!(
            h.query_by_label_contains("open for 1m 30s, is rolled back")
                .is_some()
        );
        // The heading, then the button.
        let button = h.query_all_by_label("Terminate").last().expect("a button");
        button.click();
        h.run_steps(2);
        let (view, wants) = h.state();
        assert!(view.confirm.is_none());
        assert!(matches!(
            wants.list.as_slice(),
            [
                ..,
                Want::KillConnection {
                    id: 11,
                    query_only: false
                }
            ]
        ));
    }
}
