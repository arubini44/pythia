//! Session browser: sortable table and hierarchical blocking tree.

use std::collections::HashSet;

use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use pythia_db::DbError;

use super::style::{self, Palette, duration};
use super::{Refresh, Want, Wants, refresh_selector};
use crate::queries::{BlockingNode, Capabilities, SessionRow, build_blocking_tree, explain_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Mode {
    #[default]
    Table,
    Blocking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum SortCol {
    /// Blocked first, then blockers, then active by call duration, idle last.
    #[default]
    Activity,
    Sid,
    User,
    Status,
    Program,
    Event,
    Wait,
    LastCall,
    Blocker,
}

struct KillConfirm {
    sid: i64,
    serial: i64,
    who: String,
    immediate: bool,
}

#[derive(Default)]
pub struct SessionsView {
    pub refresh: Refresh,
    mode: Mode,
    filter: String,
    background: bool,
    active_only: bool,
    rows: Vec<SessionRow>,
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

impl SessionsView {
    /// Shows the blocking tree first (e.g. when opened from a "blocked" KPI).
    pub fn show_blocking(&mut self) {
        self.mode = Mode::Blocking;
    }

    pub(crate) fn refresh_now(&mut self) {
        self.force = true;
    }

    pub(crate) fn on_sessions(&mut self, result: Result<Vec<SessionRow>, DbError>) {
        self.in_flight = false;
        self.loaded = true;
        match result {
            Ok(rows) => {
                self.tree = build_blocking_tree(&rows);
                self.rows = rows;
                self.error = None;
            }
            Err(e) => self.error = Some(explain_error(&e)),
        }
    }

    fn matches(&self, s: &SessionRow) -> bool {
        if self.active_only && !s.is_active() && !s.is_blocked() {
            return false;
        }
        let f = self.filter.trim().to_lowercase();
        if f.is_empty() {
            return true;
        }
        let hay = [
            s.username.as_deref(),
            s.program.as_deref(),
            s.machine.as_deref(),
            s.module.as_deref(),
            s.osuser.as_deref(),
            s.sql_id.as_deref(),
            s.event.as_deref(),
            s.sql_text.as_deref(),
        ];
        s.sid.to_string() == f || hay.iter().flatten().any(|h| h.to_lowercase().contains(&f))
    }

    pub(crate) fn ui(
        &mut self,
        ui: &mut Ui,
        now: f64,
        caps: Option<&Capabilities>,
        wants: &mut Wants,
    ) {
        let mut refresh_now = std::mem::take(&mut self.force);
        let blocked = self.rows.iter().filter(|s| s.is_blocked()).count();
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
                    .hint_text("Filter: user, program, SQL_ID, event…")
                    .desired_width(240.0),
            );
            ui.checkbox(&mut self.active_only, "Active only");
            if ui.checkbox(&mut self.background, "Background").changed() {
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
                refresh_selector(ui, "sessions-refresh", &mut self.refresh);
                if ui.button("⟳").on_hover_text("Refresh now").clicked() {
                    refresh_now = true;
                }
                if self.in_flight {
                    ui.spinner();
                }
                let active = self
                    .rows
                    .iter()
                    .filter(|s| s.is_active() && !s.background)
                    .count();
                ui.weak(format!(
                    "{} sessions · {active} active · {blocked} blocked",
                    self.rows.len()
                ));
            });
        });
        ui.separator();

        if (refresh_now && !self.in_flight)
            || self.refresh.due(ui, now, self.last_request, self.in_flight)
        {
            self.in_flight = true;
            self.last_request = Some(now);
            wants.push(Want::Sessions {
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

        let mut actions = Vec::new();
        match self.mode {
            Mode::Table => self.table(ui, caps, &mut actions),
            Mode::Blocking => self.blocking(ui, caps, &mut actions),
        }
        for a in actions {
            match a {
                RowAction::Want(w) => wants.push(w),
                RowAction::Kill(k) => self.confirm = Some(k),
            }
        }
        self.confirm_ui(ui.ctx(), wants);
    }

    fn sorted(&self) -> Vec<&SessionRow> {
        let mut rows: Vec<&SessionRow> = self.rows.iter().filter(|s| self.matches(s)).collect();
        let blockers: HashSet<i64> = self
            .rows
            .iter()
            .filter_map(|s| s.blocking_session)
            .collect();
        let key = |s: &SessionRow| -> (i64, String) {
            match self.sort {
                SortCol::Activity => {
                    let rank = if s.is_blocked() {
                        0
                    } else if blockers.contains(&s.sid) {
                        1
                    } else if s.is_active() && !s.is_self {
                        2
                    } else {
                        3
                    };
                    // Within a rank: longest running/waiting first.
                    (
                        rank * 1_000_000_000 - s.last_call_et.min(999_999_999),
                        String::new(),
                    )
                }
                SortCol::Sid => (s.sid, String::new()),
                SortCol::User => (0, s.username.clone().unwrap_or_default()),
                SortCol::Status => (0, s.status.clone()),
                SortCol::Program => (0, s.program.clone().unwrap_or_default()),
                SortCol::Event => (0, s.event.clone().unwrap_or_default()),
                SortCol::Wait => ((s.wait_secs * 1000.0) as i64, String::new()),
                SortCol::LastCall => (s.last_call_et, String::new()),
                SortCol::Blocker => (s.blocking_session.unwrap_or(-1), String::new()),
            }
        };
        rows.sort_by(|a, b| key(a).cmp(&key(b)).then(a.sid.cmp(&b.sid)));
        if self.descending {
            rows.reverse();
        }
        rows
    }

    fn table(&mut self, ui: &mut Ui, caps: Option<&Capabilities>, actions: &mut Vec<RowAction>) {
        let rows = self.sorted();
        let h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let blocked_bg = Palette::RED.gamma_multiply(0.14);
        let blocker_bg = Palette::ORANGE.gamma_multiply(0.14);
        let blockers: HashSet<i64> = self
            .rows
            .iter()
            .filter_map(|s| s.blocking_session)
            .collect();
        let mut sort_click = None;
        TableBuilder::new(ui)
            .id_salt("sessions-table")
            .striped(true)
            .resizable(true)
            .auto_shrink(false)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::initial(70.0))
            .column(Column::initial(110.0).clip(true))
            .column(Column::initial(70.0))
            .column(Column::initial(170.0).clip(true))
            .column(Column::initial(110.0).clip(true))
            .column(Column::initial(200.0).clip(true))
            .column(Column::initial(64.0))
            .column(Column::initial(64.0))
            .column(Column::initial(64.0))
            .column(Column::remainder().clip(true))
            .header(h + 2.0, |mut r| {
                for (title, col) in [
                    ("SID", Some(SortCol::Sid)),
                    ("User", Some(SortCol::User)),
                    ("Status", Some(SortCol::Status)),
                    ("Program", Some(SortCol::Program)),
                    ("SQL_ID", None),
                    ("Event", Some(SortCol::Event)),
                    ("Wait", Some(SortCol::Wait)),
                    ("Last call", Some(SortCol::LastCall)),
                    ("Blocker", Some(SortCol::Blocker)),
                    ("SQL text", None),
                ] {
                    r.col(|ui| {
                        let arrow = match col {
                            Some(c) if c == self.sort => {
                                if self.descending {
                                    " ⏷"
                                } else {
                                    " ⏶"
                                }
                            }
                            _ => "",
                        };
                        let resp = ui.add(
                            egui::Label::new(RichText::new(format!("{title}{arrow}")).strong())
                                .sense(egui::Sense::click()),
                        );
                        if let Some(c) = col
                            && resp.clicked()
                        {
                            sort_click = Some(c);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(h, rows.len(), |mut row| {
                    let s = rows[row.index()];
                    let bg = if s.is_blocked() {
                        Some(blocked_bg)
                    } else if blockers.contains(&s.sid) {
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
                    cell(&mut row, &|ui| {
                        let text = RichText::new(format!("{}", s.sid)).monospace();
                        ui.label(if s.is_self { text.italics() } else { text })
                            .on_hover_text(format!(
                                "SID,SERIAL# {}{}",
                                s.handle(),
                                if s.is_self { " (this monitor)" } else { "" }
                            ));
                    });
                    cell(&mut row, &|ui| {
                        ui.label(s.username.as_deref().unwrap_or("—"));
                    });
                    cell(&mut row, &|ui| {
                        let color = if s.is_active() {
                            Palette::GREEN
                        } else {
                            ui.visuals().weak_text_color()
                        };
                        ui.label(RichText::new(&s.status).color(color).small());
                    });
                    cell(&mut row, &|ui| {
                        ui.label(s.program.as_deref().unwrap_or(""))
                            .on_hover_text(format!(
                                "{}\nmachine {}\nOS user {}\nmodule {}\naction {}\nlogon {}",
                                s.program.as_deref().unwrap_or(""),
                                s.machine.as_deref().unwrap_or(""),
                                s.osuser.as_deref().unwrap_or(""),
                                s.module.as_deref().unwrap_or(""),
                                s.action.as_deref().unwrap_or(""),
                                s.logon_time
                            ));
                    });
                    cell(&mut row, &|ui| {
                        ui.label(RichText::new(s.sql_id.as_deref().unwrap_or("")).monospace());
                    });
                    cell(&mut row, &|ui| event_label(ui, s));
                    cell(&mut row, &|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if s.wait_secs > 0.0 {
                                ui.label(duration(s.wait_secs));
                            }
                        });
                    });
                    cell(&mut row, &|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(duration(s.last_call_et as f64));
                        });
                    });
                    cell(&mut row, &|ui| {
                        if let Some(b) = s.blocking_session {
                            ui.label(RichText::new(b.to_string()).color(Palette::RED).strong());
                        }
                    });
                    cell(&mut row, &|ui| {
                        if let Some(t) = &s.sql_text {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(t.replace('\n', " ")).monospace().size(11.5),
                                )
                                .truncate(),
                            )
                            .on_hover_text(t);
                        }
                    });
                    let resp = row.response();
                    if resp.double_clicked() {
                        actions.push(RowAction::Want(Want::OpenDetail {
                            sid: s.sid,
                            serial: s.serial,
                        }));
                    }
                    resp.context_menu(|ui| session_menu(ui, s, caps, actions));
                });
            });
        if let Some(c) = sort_click {
            if self.sort == c {
                self.descending = !self.descending;
            } else {
                self.sort = c;
                self.descending = matches!(c, SortCol::Wait | SortCol::LastCall);
            }
        }
    }

    fn blocking(&mut self, ui: &mut Ui, caps: Option<&Capabilities>, actions: &mut Vec<RowAction>) {
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
                    node_ui(ui, root, 0, &self.collapsed, &mut toggles, caps, actions);
                });
                ui.add_space(6.0);
            }
        });
        for sid in toggles {
            if !self.collapsed.remove(&sid) {
                self.collapsed.insert(sid);
            }
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, wants: &mut Wants) {
        let Some(k) = &self.confirm else {
            return;
        };
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new("kill-session")).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading(if k.immediate {
                "Kill session immediately"
            } else {
                "Kill session"
            });
            ui.add_space(6.0);
            ui.label(format!(
                "Kill session {},{} ({})? Its open transaction is rolled back.",
                k.sid, k.serial, k.who
            ));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let kill = egui::Button::new(RichText::new("Kill").color(Color32::WHITE))
                    .fill(Palette::RED);
                if ui.add(kill).clicked() {
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
            if yes {
                wants.push(Want::Kill {
                    sid: k.sid,
                    serial: k.serial,
                    immediate: k.immediate,
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

fn event_label(ui: &mut Ui, s: &SessionRow) {
    let on_cpu = s.state.as_deref() != Some("WAITING");
    let (class, text) = if on_cpu && s.is_active() {
        ("CPU".to_owned(), "ON CPU".to_owned())
    } else {
        (
            s.wait_class.clone().unwrap_or_default(),
            s.event.clone().unwrap_or_default(),
        )
    };
    let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
    ui.painter()
        .circle_filled(dot.center(), 4.0, style::wait_class_color(&class));
    let mut label = RichText::new(text);
    if class == "Idle" {
        label = label.color(ui.visuals().weak_text_color());
    }
    let mut hover = format!("Wait class: {class}");
    if let Some(o) = &s.wait_object {
        hover.push_str(&format!("\nObject: {o}"));
    }
    if let Some(r) = &s.wait_rowid {
        hover.push_str(&format!("\nRow: {r}"));
    }
    ui.label(label).on_hover_text(hover);
}

fn node_ui(
    ui: &mut Ui,
    node: &BlockingNode,
    depth: usize,
    collapsed: &HashSet<i64>,
    toggles: &mut Vec<i64>,
    caps: Option<&Capabilities>,
    actions: &mut Vec<RowAction>,
) {
    let s = &node.session;
    let is_root = depth == 0;
    let open = !collapsed.contains(&s.sid);
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
                toggles.push(s.sid);
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
            let title = format!("{}  {}", s.handle(), s.username.as_deref().unwrap_or("—"));
            let title = if is_root {
                RichText::new(title).strong()
            } else {
                RichText::new(title)
            };
            ui.label(title.monospace());
            if is_root {
                let status = if s.is_unknown() {
                    "not in the session list"
                } else if s.is_active() {
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
                event_label(ui, s);
                ui.label(
                    RichText::new(format!("waiting {}", duration(s.wait_secs)))
                        .color(Palette::RED)
                        .small(),
                );
            }
            if let Some(o) = &s.wait_object {
                ui.weak(format!("on {o}"));
            }
            if let Some(p) = &s.program {
                ui.weak(p);
            }
            if let Some(t) = &s.sql_text {
                ui.add(
                    egui::Label::new(RichText::new(t.replace('\n', " ")).monospace().size(11.5))
                        .truncate(),
                )
                .on_hover_text(t);
            }
        })
        .response
        .interact(egui::Sense::click());
    if resp.double_clicked() && !s.is_unknown() {
        actions.push(RowAction::Want(Want::OpenDetail {
            sid: s.sid,
            serial: s.serial,
        }));
    }
    resp.context_menu(|ui| session_menu(ui, s, caps, actions));
    if open {
        for c in &node.children {
            node_ui(ui, c, depth + 1, collapsed, toggles, caps, actions);
        }
    }
}

fn session_menu(
    ui: &mut Ui,
    s: &SessionRow,
    caps: Option<&Capabilities>,
    actions: &mut Vec<RowAction>,
) {
    let who = s
        .username
        .clone()
        .unwrap_or_else(|| s.program.clone().unwrap_or_default());
    // Only the SID of a blocker missing from the list is known: its serial#
    // would be a guess.
    let unknown = "Not in the session list: only its SID is known";
    if ui
        .add_enabled(!s.is_unknown(), egui::Button::new("Details"))
        .on_disabled_hover_text(unknown)
        .clicked()
    {
        actions.push(RowAction::Want(Want::OpenDetail {
            sid: s.sid,
            serial: s.serial,
        }));
        ui.close();
    }
    if ui.button("Copy SID,SERIAL#").clicked() {
        ui.ctx().copy_text(s.handle());
        ui.close();
    }
    if let Some(id) = &s.sql_id
        && ui.button("Copy SQL_ID").clicked()
    {
        ui.ctx().copy_text(id.clone());
        ui.close();
    }
    ui.separator();
    let possible = !s.is_self && !s.background && !s.is_unknown();
    let item = |ui: &mut Ui, label: &str, allowed: std::result::Result<(), String>| {
        let hint = allowed.as_ref().err().cloned().unwrap_or_else(|| {
            if s.is_unknown() {
                unknown.into()
            } else {
                "Not possible for this session".into()
            }
        });
        ui.add_enabled(allowed.is_ok() && possible, egui::Button::new(label))
            .on_disabled_hover_text(hint)
    };
    let checking = || Err("Checking privileges…".to_owned());
    let control = caps.map_or_else(checking, |c| c.session_control());
    let cancel = caps.map_or_else(checking, |c| c.cancel_sql());
    if item(ui, "Cancel current SQL", cancel).clicked() {
        actions.push(RowAction::Want(Want::CancelSql {
            sid: s.sid,
            serial: s.serial,
        }));
        ui.close();
    }
    if item(ui, "Kill session…", control.clone()).clicked() {
        actions.push(RowAction::Kill(KillConfirm {
            sid: s.sid,
            serial: s.serial,
            who: who.clone(),
            immediate: false,
        }));
        ui.close();
    }
    if item(ui, "Kill session immediately…", control).clicked() {
        actions.push(RowAction::Kill(KillConfirm {
            sid: s.sid,
            serial: s.serial,
            who,
            immediate: true,
        }));
        ui.close();
    }
    ui.separator();
    let tracing = if s.is_unknown() {
        Err(unknown.to_owned())
    } else {
        caps.map_or(Err("Checking privileges…".to_owned()), |c| c.tracing())
    };
    let hint = tracing.as_ref().err().cloned().unwrap_or_default();
    if ui
        .add_enabled(
            tracing.is_ok(),
            egui::Button::new("Enable SQL trace (waits, binds)"),
        )
        .on_disabled_hover_text(&hint)
        .clicked()
    {
        actions.push(RowAction::Want(Want::Trace {
            sid: s.sid,
            serial: s.serial,
            on: true,
        }));
        ui.close();
    }
    if ui
        .add_enabled(tracing.is_ok(), egui::Button::new("Disable SQL trace"))
        .on_disabled_hover_text(&hint)
        .clicked()
    {
        actions.push(RowAction::Want(Want::Trace {
            sid: s.sid,
            serial: s.serial,
            on: false,
        }));
        ui.close();
    }
    let files = if s.is_unknown() {
        Err(unknown.to_owned())
    } else {
        caps.map_or(Err("Checking privileges…".to_owned()), |c| {
            c.trace_files()
        })
    };
    let hint = files.as_ref().err().cloned().unwrap_or_default();
    if ui
        .add_enabled(files.is_ok(), egui::Button::new("Analyze trace file"))
        .on_disabled_hover_text(&hint)
        .clicked()
    {
        actions.push(RowAction::Want(Want::SessionTraceFile { sid: s.sid }));
        ui.close();
    }
}
