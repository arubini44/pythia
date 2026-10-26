//! Details of one session: SQL, plan, waits, statistics, cursors, locks.

use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use pythia_db::DbError;

use super::sessions::{KillConfirm, RowAction, kill_dialog, session_actions};
use super::style::{self, Palette, duration, section};
use super::{Analysis, Want, Wants};
use crate::queries::{Capabilities, SessionDetail, Transaction, explain_error};

/// The tabs of session details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailTab {
    #[default]
    Sql,
    Plan,
    Waits,
    Stats,
    Cursors,
    Locks,
}

pub struct SessionDetailView {
    pub sid: i64,
    pub serial: i64,
    tab: DetailTab,
    detail: Option<SessionDetail>,
    error: Option<String>,
    loading: bool,
    requested: bool,
    stat_filter: String,
    /// The open cursor whose statement is shown below the list.
    cursor: Option<usize>,
    /// Asking before a kill.
    confirm: Option<KillConfirm>,
}

impl SessionDetailView {
    pub fn new(sid: i64, serial: i64) -> Self {
        Self {
            sid,
            serial,
            tab: DetailTab::default(),
            detail: None,
            error: None,
            loading: false,
            requested: false,
            stat_filter: String::new(),
            cursor: None,
            confirm: None,
        }
    }

    /// A copy with the same details, for a test harness of its own.
    #[cfg(test)]
    fn clone_for_test(&self) -> Self {
        let mut view = Self::new(self.sid, self.serial);
        view.requested = self.requested;
        view.detail = self.detail.clone();
        view
    }

    /// Shows one of the tabs (e.g. the open cursors of a session the
    /// dashboard lists for them).
    pub fn show_tab(&mut self, tab: DetailTab) {
        self.tab = tab;
    }

    /// Reads the details again with the next frame (after a kill, a cancel
    /// or a trace change).
    pub(crate) fn refresh(&mut self) {
        if !self.loading {
            self.requested = false;
        }
    }

    /// The transaction the kill dialog asked for.
    pub(crate) fn on_transaction(&mut self, result: &Result<Option<Transaction>, String>) {
        if let Some(k) = &mut self.confirm {
            k.on_transaction(self.sid, self.serial, result);
        }
    }

    pub fn title(&self) -> String {
        format!("Session {}", self.sid)
    }

    pub(crate) fn on_detail(&mut self, result: Result<SessionDetail, DbError>) {
        self.loading = false;
        match result {
            Ok(d) => {
                self.error = None;
                // Keep the selected cursor if it's still there.
                let selected = self
                    .cursor
                    .and_then(|i| self.detail.as_ref()?.cursors.get(i));
                self.cursor = selected.and_then(|c| d.cursors.iter().position(|n| n == c));
                self.detail = Some(d);
            }
            Err(e) => self.error = Some(explain_error(&e)),
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, caps: Option<&Capabilities>, wants: &mut Wants) {
        if !self.requested {
            self.requested = true;
            self.loading = true;
            wants.push(Want::Detail {
                sid: self.sid,
                serial: self.serial,
            });
        }
        let mut actions = Vec::new();
        ui.horizontal(|ui| {
            let s = self.detail.as_ref().and_then(|d| d.session.as_ref());
            match s {
                Some(s) => {
                    ui.label(
                        RichText::new(format!(
                            "{}  {}",
                            s.handle(),
                            s.username.as_deref().unwrap_or("—")
                        ))
                        .strong()
                        .monospace(),
                    );
                    let color = if s.is_active() {
                        Palette::GREEN
                    } else {
                        ui.visuals().weak_text_color()
                    };
                    ui.label(RichText::new(&s.status).color(color));
                    ui.weak(format!(
                        "{} · {} · logon {} · last call {}",
                        s.program.as_deref().unwrap_or(""),
                        s.machine.as_deref().unwrap_or(""),
                        s.logon_time,
                        duration(s.last_call_et as f64)
                    ));
                }
                None if self.detail.is_some() => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Session {},{} no longer exists", self.sid, self.serial),
                    );
                }
                None => {
                    ui.label(
                        RichText::new(format!("Session {},{}", self.sid, self.serial)).strong(),
                    );
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(s) = self.detail.as_ref().and_then(|d| d.session.as_ref()) {
                    ui.menu_button("Session ⏷", |ui| session_actions(ui, s, caps, &mut actions))
                        .response
                        .on_hover_text("Cancel its SQL, kill or disconnect it, trace it");
                }
                if ui.button("⟳").on_hover_text("Refresh").clicked() && !self.loading {
                    self.loading = true;
                    wants.push(Want::Detail {
                        sid: self.sid,
                        serial: self.serial,
                    });
                }
                if self.loading {
                    ui.spinner();
                }
                if wants.analyze
                    && let Some(d) = self.detail.as_ref().filter(|d| d.session.is_some())
                    && ui
                        .button("Analyze with AI")
                        .on_hover_text("Have the AI assistant explain what the session waits for and why its SQL is slow")
                        .clicked()
                {
                    wants.push(Want::Analyze(analysis(self.sid, self.serial, d)));
                }
            });
        });
        if let Some(s) = self.detail.as_ref().and_then(|d| d.session.as_ref())
            && (s.is_blocked() || s.event.is_some())
        {
            ui.horizontal(|ui| {
                ui.weak("Current wait:");
                ui.label(s.event.as_deref().unwrap_or("ON CPU"));
                if s.wait_secs > 0.0 {
                    ui.weak(format!("for {}", duration(s.wait_secs)));
                }
                if let Some(b) = s.blocking_session {
                    let text = RichText::new(format!("blocked by {b}"))
                        .color(Palette::RED)
                        .strong();
                    match s.blocking_serial {
                        Some(serial) => {
                            if ui.link(text).on_hover_text("Show its details").clicked() {
                                actions.push(RowAction::Want(Want::OpenDetail {
                                    sid: b,
                                    serial,
                                    tab: None,
                                }));
                            }
                        }
                        None => {
                            ui.label(text);
                        }
                    }
                }
                if let Some(o) = &s.wait_object {
                    ui.weak(format!("object {o}"));
                }
                if let Some(r) = &s.wait_rowid {
                    ui.weak(format!("row {r}"));
                }
            });
        }
        if let Some(d) = self.detail.as_ref().filter(|d| !d.blocks.is_empty()) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Blocks").color(Palette::RED).strong());
                for (sid, serial, user) in &d.blocks {
                    let who = user.as_deref().unwrap_or("background");
                    if ui
                        .link(format!("{sid} ({who})"))
                        .on_hover_text("Show its details")
                        .clicked()
                    {
                        actions.push(RowAction::Want(Want::OpenDetail {
                            sid: *sid,
                            serial: *serial,
                            tab: None,
                        }));
                    }
                }
            });
        }
        for a in actions {
            a.apply(&mut self.confirm, wants);
        }
        kill_dialog(ui.ctx(), &mut self.confirm, wants);
        ui.horizontal(|ui| {
            for (t, label) in [
                (DetailTab::Sql, "SQL"),
                (DetailTab::Plan, "Plan"),
                (DetailTab::Waits, "Wait history"),
                (DetailTab::Stats, "Statistics"),
                (DetailTab::Cursors, "Open cursors"),
                (DetailTab::Locks, "Locks"),
            ] {
                ui.selectable_value(&mut self.tab, t, label);
            }
        });
        ui.separator();
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        let Some(d) = &self.detail else {
            ui.spinner();
            return;
        };
        style::section_errors(ui, &d.errors);
        let h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        match self.tab {
            DetailTab::Sql => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        let sql_id = d
                            .session
                            .as_ref()
                            .and_then(|s| s.sql_id.clone())
                            .unwrap_or_default();
                        section(ui, &format!("Current SQL {sql_id}"), |ui| {
                            code(ui, d.sql_text.as_deref())
                        });
                        ui.add_space(6.0);
                        let prev = d
                            .session
                            .as_ref()
                            .and_then(|s| s.prev_sql_id.clone())
                            .unwrap_or_default();
                        section(ui, &format!("Previous SQL {prev}"), |ui| {
                            code(ui, d.prev_sql_text.as_deref())
                        });
                    });
            }
            DetailTab::Plan => {
                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    if d.plan.is_empty() {
                        ui.weak(
                            "No plan available (the session has no current or previous cursor).",
                        );
                    }
                    ui.label(RichText::new(d.plan.join("\n")).monospace());
                });
            }
            DetailTab::Waits => {
                table(
                    ui,
                    "detail-waits",
                    h,
                    &["#", "Event", "Wait (ms)", "Parameters"],
                    d.waits.len(),
                    |row, i| {
                        let w = &d.waits[i];
                        row.col(|ui| {
                            ui.label(w.seq.to_string());
                        });
                        row.col(|ui| {
                            ui.label(&w.event);
                        });
                        row.col(|ui| {
                            ui.label(format!("{:.2}", w.wait_ms));
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(&w.params).monospace().small());
                        });
                    },
                );
            }
            DetailTab::Stats => {
                ui.add(
                    egui::TextEdit::singleline(&mut self.stat_filter)
                        .hint_text("Filter statistics…"),
                );
                let f = self.stat_filter.to_lowercase();
                let stats: Vec<&(String, f64)> = d
                    .stats
                    .iter()
                    .filter(|(n, _)| n.to_lowercase().contains(&f))
                    .collect();
                table(
                    ui,
                    "detail-stats",
                    h,
                    &["Statistic", "Value"],
                    stats.len(),
                    |row, i| {
                        let (n, v) = stats[i];
                        row.col(|ui| {
                            ui.label(n);
                        });
                        row.col(|ui| {
                            ui.label(RichText::new(format!("{v}")).monospace());
                        });
                    },
                );
            }
            DetailTab::Cursors => {
                // The selected cursor's whole statement below the list.
                egui::Panel::bottom(egui::Id::new(("detail-cursor-text", self.sid)))
                    .resizable(true)
                    .default_size(140.0)
                    .show(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink(false)
                            .show(ui, |ui| match self.cursor.and_then(|i| d.cursors.get(i)) {
                                Some(c) => code(ui, Some(&c.text)),
                                None => {
                                    ui.weak("Select a cursor to see its statement");
                                }
                            });
                    });
                let mut clicked = None;
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ui, |ui| {
                        TableBuilder::new(ui)
                            .id_salt("detail-cursors")
                            .striped(true)
                            .resizable(true)
                            .auto_shrink(false)
                            .sense(egui::Sense::click())
                            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                            .column(Column::initial(130.0).clip(true))
                            .column(Column::initial(130.0).clip(true))
                            .column(Column::remainder().clip(true))
                            .header(h, |mut r| {
                                for title in ["SQL_ID", "Type", "SQL text"] {
                                    r.col(|ui| {
                                        ui.strong(title);
                                    });
                                }
                            })
                            .body(|body| {
                                body.rows(h, d.cursors.len(), |mut row| {
                                    let i = row.index();
                                    let c = &d.cursors[i];
                                    row.set_selected(self.cursor == Some(i));
                                    row.col(|ui| {
                                        ui.label(
                                            RichText::new(c.sql_id.as_deref().unwrap_or(""))
                                                .monospace(),
                                        );
                                    });
                                    row.col(|ui| {
                                        ui.label(c.cursor_type.as_deref().unwrap_or(""));
                                    });
                                    row.col(|ui| {
                                        let line =
                                            c.text.split_whitespace().collect::<Vec<_>>().join(" ");
                                        ui.add(
                                            egui::Label::new(
                                                RichText::new(line).monospace().small(),
                                            )
                                            .truncate()
                                            .show_tooltip_when_elided(false)
                                            .selectable(false),
                                        );
                                    });
                                    if row.response().clicked() {
                                        clicked = Some(i);
                                    }
                                });
                            });
                    });
                if clicked.is_some() {
                    self.cursor = clicked;
                }
            }
            DetailTab::Locks => {
                table(
                    ui,
                    "detail-locks",
                    h,
                    &[
                        "Type",
                        "Object / ID1",
                        "ID2",
                        "Held",
                        "Requested",
                        "Seconds",
                        "Blocking",
                    ],
                    d.locks.len(),
                    |row, i| {
                        let l = &d.locks[i];
                        row.col(|ui| {
                            ui.label(RichText::new(&l.lock_type).monospace());
                        });
                        row.col(|ui| {
                            ui.label(l.object.clone().unwrap_or_else(|| l.id1.to_string()));
                        });
                        row.col(|ui| {
                            ui.label(l.id2.to_string());
                        });
                        row.col(|ui| {
                            ui.label(&l.mode_held);
                        });
                        row.col(|ui| {
                            let color = if l.mode_requested != "None" {
                                Palette::RED
                            } else {
                                ui.visuals().weak_text_color()
                            };
                            ui.label(RichText::new(&l.mode_requested).color(color));
                        });
                        row.col(|ui| {
                            ui.label(duration(l.seconds as f64));
                        });
                        row.col(|ui| {
                            if l.blocking {
                                ui.label(RichText::new("blocking").color(Palette::RED).strong());
                            }
                        });
                    },
                );
            }
        }
    }
}

fn code(ui: &mut Ui, text: Option<&str>) {
    match text {
        Some(t) => {
            ui.add(egui::Label::new(RichText::new(t).monospace()).wrap());
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(t.to_owned());
            }
        }
        None => {
            ui.weak("—");
        }
    }
}

fn table(
    ui: &mut Ui,
    id: &str,
    h: f32,
    headers: &[&str],
    rows: usize,
    mut add: impl FnMut(&mut egui_extras::TableRow<'_, '_>, usize),
) {
    let mut t = TableBuilder::new(ui)
        .id_salt(id)
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
    for (i, _) in headers.iter().enumerate() {
        t = if i + 1 == headers.len() {
            t.column(Column::remainder().clip(true))
        } else {
            t.column(Column::initial(130.0).clip(true))
        };
    }
    t.header(h, |mut r| {
        for title in headers {
            r.col(|ui| {
                ui.strong(*title);
            });
        }
    })
    .body(|body| {
        body.rows(h, rows, |mut row| {
            let i = row.index();
            add(&mut row, i);
        })
    });
}

/// A session's state, SQL, plan, waits, busiest statistics and locks, for
/// the assistant.
fn analysis(sid: i64, serial: i64, d: &SessionDetail) -> Analysis {
    let mut details = String::new();
    if let Some(s) = &d.session {
        details.push_str(&format!(
            "Session {sid},{serial}: user {}, program {}, module {}, status {}, \
             last call {} s ago\n",
            s.username.as_deref().unwrap_or("-"),
            s.program.as_deref().unwrap_or("-"),
            s.module.as_deref().unwrap_or("-"),
            s.status,
            s.last_call_et
        ));
        match &s.event {
            Some(event) => details.push_str(&format!(
                "Current wait: {event} ({}) for {:.1} s{}{}\n",
                s.wait_class.as_deref().unwrap_or("-"),
                s.wait_secs,
                s.blocking_session
                    .map(|b| format!(", blocked by session {b}"))
                    .unwrap_or_default(),
                s.wait_object
                    .as_ref()
                    .map(|o| format!(", on object {o}"))
                    .unwrap_or_default(),
            )),
            None => details.push_str("On CPU\n"),
        }
        if let Some(prev) = s.prev_sql_id.as_ref().filter(|_| d.sql_text.is_none()) {
            details.push_str(&format!("No current SQL; previous SQL_ID {prev}\n"));
        }
    }
    if !d.plan.is_empty() {
        details.push_str(&format!(
            "\nPlan of the current SQL:\n{}\n",
            d.plan.join("\n")
        ));
    }
    if !d.waits.is_empty() {
        details.push_str("\nRecent waits (newest first):\n");
        for w in &d.waits {
            details.push_str(&format!("{} {:.2} ms {}\n", w.event, w.wait_ms, w.params));
        }
    }
    let mut stats: Vec<&(String, f64)> = d.stats.iter().filter(|(_, v)| *v != 0.0).collect();
    stats.sort_by(|a, b| b.1.total_cmp(&a.1));
    if !stats.is_empty() {
        details.push_str("\nSession statistics (largest 40):\n");
        for (name, value) in stats.into_iter().take(40) {
            details.push_str(&format!("{name}: {value}\n"));
        }
    }
    if !d.locks.is_empty() {
        details.push_str("\nLocks:\n");
        for l in &d.locks {
            details.push_str(&format!(
                "{} on {} held {} requested {} for {} s{}\n",
                l.lock_type,
                l.object.clone().unwrap_or_else(|| l.id1.to_string()),
                l.mode_held,
                l.mode_requested,
                l.seconds,
                if l.blocking { ", blocking others" } else { "" }
            ));
        }
    }
    let sql_id = d.session.as_ref().and_then(|s| s.sql_id.clone());
    Analysis {
        title: format!("session {sid},{serial}"),
        sql: d.sql_text.clone().or_else(|| d.prev_sql_text.clone()),
        // The detail has the plan already.
        sql_id: sql_id.filter(|_| d.plan.is_empty()),
        details,
        binds: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::queries::{OpenCursor, SessionRow};
    use crate::ui::Want;

    /// The header links to the session it waits for and to those waiting
    /// for it (#74).
    #[test]
    fn blocking_links() {
        let mut view = SessionDetailView::new(42, 7);
        view.requested = true;
        view.on_detail(Ok(SessionDetail {
            session: Some(SessionRow {
                sid: 42,
                serial: 7,
                status: "ACTIVE".into(),
                event: Some("enq: TX - row lock contention".into()),
                blocking_session: Some(21),
                blocking_serial: Some(4),
                ..SessionRow::default()
            }),
            blocks: vec![(55, 8, Some("HR".into()))],
            ..SessionDetail::default()
        }));
        let opened = |label: &str| {
            let mut harness = Harness::builder()
                .with_size(egui::vec2(900.0, 500.0))
                .build_ui_state(
                    |ui, (view, wants): &mut (SessionDetailView, Wants)| view.ui(ui, None, wants),
                    (view.clone_for_test(), Wants::default()),
                );
            harness.run_steps(2);
            harness.get_by_label(label).click();
            harness.run_steps(2);
            harness
                .state()
                .1
                .list
                .iter()
                .filter_map(|w| match w {
                    Want::OpenDetail { sid, serial, .. } => Some((*sid, *serial)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(opened("blocked by 21"), [(21, 4)]);
        assert_eq!(opened("55 (HR)"), [(55, 8)]);
    }

    /// Clicking an open cursor shows its whole statement below the list
    /// (#73).
    #[test]
    fn open_cursor_statement_below_the_list() {
        let text = "select id,\n       name\n  from a_table_with_a_long_name\n where id = :1";
        let cursor = |sql_id: &str, text: &str| OpenCursor {
            sql_id: Some(sql_id.into()),
            text: text.into(),
            cursor_type: Some("OPEN".into()),
        };
        let mut view = SessionDetailView::new(42, 7);
        view.tab = DetailTab::Cursors;
        // Loaded already: no request, no spinner.
        view.requested = true;
        view.on_detail(Ok(SessionDetail {
            session: Some(SessionRow {
                sid: 42,
                serial: 7,
                status: "INACTIVE".into(),
                ..SessionRow::default()
            }),
            cursors: vec![cursor("a1", "select 1 from dual"), cursor("b2", text)],
            ..SessionDetail::default()
        }));
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 500.0))
            .build_ui_state(
                |ui, view: &mut SessionDetailView| view.ui(ui, None, &mut Wants::default()),
                view,
            );
        harness.run();
        // The list shows the statement on one line; the whole text only
        // once selected.
        assert!(harness.query_by_label(text).is_none());
        harness
            .get_by_label("select id, name from a_table_with_a_long_name where id = :1")
            .click();
        harness.run();
        assert!(harness.query_by_label(text).is_some());
        assert_eq!(harness.state().cursor, Some(1));
        // A refresh keeps the selection where the cursor is now.
        let mut detail = harness.state().detail.clone().unwrap();
        detail.cursors.insert(0, cursor("00", "begin null; end;"));
        harness.state_mut().on_detail(Ok(detail));
        harness.run();
        assert_eq!(harness.state().cursor, Some(2));
        assert!(harness.query_by_label(text).is_some());
    }
}
