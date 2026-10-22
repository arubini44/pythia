//! Details of one session: SQL, plan, waits, statistics, cursors, locks.

use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use pythia_db::DbError;

use super::style::{self, Palette, duration, section};
use super::{Analysis, Want, Wants};
use crate::queries::{Capabilities, SessionDetail, explain_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
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
    tab: Tab,
    detail: Option<SessionDetail>,
    error: Option<String>,
    loading: bool,
    requested: bool,
    stat_filter: String,
}

impl SessionDetailView {
    pub fn new(sid: i64, serial: i64) -> Self {
        Self {
            sid,
            serial,
            tab: Tab::default(),
            detail: None,
            error: None,
            loading: false,
            requested: false,
            stat_filter: String::new(),
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
                self.detail = Some(d);
            }
            Err(e) => self.error = Some(explain_error(&e)),
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, _caps: Option<&Capabilities>, wants: &mut Wants) {
        if !self.requested {
            self.requested = true;
            self.loading = true;
            wants.push(Want::Detail {
                sid: self.sid,
                serial: self.serial,
            });
        }
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
                    ui.label(
                        RichText::new(format!("blocked by {b}"))
                            .color(Palette::RED)
                            .strong(),
                    );
                }
                if let Some(o) = &s.wait_object {
                    ui.weak(format!("object {o}"));
                }
                if let Some(r) = &s.wait_rowid {
                    ui.weak(format!("row {r}"));
                }
            });
        }
        ui.horizontal(|ui| {
            for (t, label) in [
                (Tab::Sql, "SQL"),
                (Tab::Plan, "Plan"),
                (Tab::Waits, "Wait history"),
                (Tab::Stats, "Statistics"),
                (Tab::Cursors, "Open cursors"),
                (Tab::Locks, "Locks"),
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
            Tab::Sql => {
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
            Tab::Plan => {
                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    if d.plan.is_empty() {
                        ui.weak(
                            "No plan available (the session has no current or previous cursor).",
                        );
                    }
                    ui.label(RichText::new(d.plan.join("\n")).monospace());
                });
            }
            Tab::Waits => {
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
            Tab::Stats => {
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
            Tab::Cursors => {
                table(
                    ui,
                    "detail-cursors",
                    h,
                    &["SQL_ID", "Type", "SQL text"],
                    d.cursors.len(),
                    |row, i| {
                        let c = &d.cursors[i];
                        row.col(|ui| {
                            ui.label(RichText::new(c.sql_id.as_deref().unwrap_or("")).monospace());
                        });
                        row.col(|ui| {
                            ui.label(c.cursor_type.as_deref().unwrap_or(""));
                        });
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(RichText::new(&c.text).monospace().small())
                                    .truncate(),
                            );
                        });
                    },
                );
            }
            Tab::Locks => {
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
