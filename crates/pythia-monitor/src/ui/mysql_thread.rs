//! Details of one MySQL thread: how it connected, the statement it runs
//! with its plan, its transaction and locks, the statements before; kill
//! its query or its connection.

use egui::{Color32, RichText, Ui};
use pythia_db::DbError;

use super::style::{Palette, compact, duration, section};
use super::{MonitorAction, Want, Wants};
use crate::mysql::{ThreadDetail, ThreadStatement};

pub struct ThreadDetailView {
    pub id: i64,
    detail: Option<ThreadDetail>,
    error: Option<String>,
    loading: bool,
    requested: bool,
    /// Asking before a kill: of the query only, or of the connection.
    confirm: Option<bool>,
}

impl ThreadDetailView {
    pub fn new(id: i64) -> Self {
        Self {
            id,
            detail: None,
            error: None,
            loading: false,
            requested: false,
            confirm: None,
        }
    }

    pub fn title(&self) -> String {
        format!("Thread {}", self.id)
    }

    /// Reads it again with the next frame.
    pub(crate) fn refresh(&mut self) {
        if !self.loading {
            self.requested = false;
        }
    }

    pub(crate) fn on_detail(&mut self, result: Result<ThreadDetail, DbError>) {
        self.loading = false;
        match result {
            Ok(d) => {
                self.error = None;
                self.detail = Some(d);
            }
            Err(e) => self.error = Some(e.text()),
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, wants: &mut Wants) {
        if !self.requested && !self.loading {
            self.requested = true;
            self.loading = true;
            wants.push(Want::ConnectionDetail { id: self.id });
        }
        let thread = self.detail.as_ref().and_then(|d| d.thread.as_ref());
        let (mut confirm, mut refresh) = (None, false);
        ui.horizontal(|ui| {
            match thread {
                Some(t) => {
                    ui.label(
                        RichText::new(format!("{}  {}", t.id, t.user))
                            .strong()
                            .monospace(),
                    );
                    ui.weak(format!(
                        "{} · {} · {} for {} · {}",
                        t.host,
                        t.db.as_deref().unwrap_or("no database"),
                        t.command,
                        duration(t.time_secs as f64),
                        t.state
                    ));
                }
                None if self.detail.is_some() => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "Thread {} isn't running (or isn't visible without the PROCESS \
                             privilege)",
                            self.id
                        ),
                    );
                }
                None => {
                    ui.label(RichText::new(self.title()).strong());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if thread.is_some() {
                    ui.menu_button("Thread ⏷", |ui| {
                        if ui
                            .button("Kill query…")
                            .on_hover_text("Ends the statement it runs; the connection stays")
                            .clicked()
                        {
                            confirm = Some(true);
                            ui.close();
                        }
                        if ui
                            .button("Kill connection…")
                            .on_hover_text("Ends the connection and rolls back its transaction")
                            .clicked()
                        {
                            confirm = Some(false);
                            ui.close();
                        }
                    });
                }
                if ui.button("⟳").on_hover_text("Refresh").clicked() {
                    refresh = true;
                }
                if self.loading {
                    ui.spinner();
                }
            });
        });
        ui.separator();
        if confirm.is_some() {
            self.confirm = confirm;
        }
        if refresh {
            self.refresh();
        }
        self.kill_dialog(ui.ctx(), wants);
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        let Some(d) = &self.detail else {
            ui.spinner();
            return;
        };
        super::style::section_errors(ui, &d.errors);
        if d.thread.is_none() {
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| details(ui, d, wants));
    }

    fn kill_dialog(&mut self, ctx: &egui::Context, wants: &mut Wants) {
        let Some(query_only) = self.confirm else {
            return;
        };
        let transaction = self.detail.as_ref().and_then(|d| d.transaction.as_ref());
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new(("kill-thread", self.id))).show(ctx, |ui| {
            ui.set_max_width(400.0);
            ui.heading(if query_only {
                "Kill query"
            } else {
                "Kill connection"
            });
            ui.add_space(6.0);
            if query_only {
                ui.label(format!(
                    "End the statement thread {} runs? Its connection stays.",
                    self.id
                ));
            } else {
                ui.label(format!("End the connection of thread {}?", self.id));
                match transaction {
                    Some(t) => {
                        ui.label(
                            RichText::new(format!(
                                "Its transaction, open for {}, is rolled back: {} rows modified.",
                                duration(t.age_secs as f64),
                                t.rows_modified
                            ))
                            .color(Palette::ORANGE),
                        );
                    }
                    None => {
                        ui.label("It has no open transaction.");
                    }
                }
            }
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
                wants.push(Want::KillConnection {
                    id: self.id,
                    query_only,
                });
            }
            self.confirm = None;
        }
    }
}

fn details(ui: &mut Ui, d: &ThreadDetail, wants: &mut Wants) {
    if !d.connection.is_empty() {
        section(ui, "Connection", |ui| {
            egui::Grid::new("thread-connection")
                .num_columns(2)
                .show(ui, |ui| {
                    for (name, value) in &d.connection {
                        ui.weak(name);
                        ui.label(value);
                        ui.end_row();
                    }
                });
        });
        ui.add_space(6.0);
    }
    let title = match &d.statement {
        Some(s) if s.running => "Statement it runs",
        _ => "Statement it ran last",
    };
    section(ui, title, |ui| match &d.statement {
        Some(s) => {
            statement(ui, s);
            if ui
                .small_button("Open in SQL window")
                .on_hover_text("Run it, explain it")
                .clicked()
            {
                wants.push(Want::Act(MonitorAction::OpenSql(s.text.clone())));
            }
        }
        None => {
            ui.weak(
                d.thread
                    .as_ref()
                    .and_then(|t| t.query.clone())
                    .unwrap_or_else(|| "—".into()),
            );
        }
    });
    if let Some(plan) = &d.plan {
        ui.add_space(6.0);
        section(ui, "Plan (EXPLAIN FOR CONNECTION)", |ui| {
            egui::ScrollArea::horizontal()
                .id_salt("thread-plan")
                .show(ui, |ui| {
                    ui.label(RichText::new(plan).monospace().small());
                });
        });
    }
    ui.add_space(6.0);
    section(ui, "Transaction", |ui| match &d.transaction {
        Some(t) => {
            egui::Grid::new("thread-transaction")
                .num_columns(2)
                .show(ui, |ui| {
                    let rows = [
                        ("State", t.state.clone()),
                        (
                            "Open since",
                            format!("{} ({})", t.started, duration(t.age_secs as f64)),
                        ),
                        ("Rows locked", compact(t.rows_locked as f64)),
                        ("Rows modified", compact(t.rows_modified as f64)),
                        ("Tables locked", t.tables_locked.to_string()),
                        ("Isolation", t.isolation.clone()),
                    ];
                    for (name, value) in rows {
                        ui.weak(name);
                        ui.label(value);
                        ui.end_row();
                    }
                });
        }
        None => {
            ui.weak("No open transaction");
        }
    });
    ui.add_space(6.0);
    section(ui, "Locks", |ui| {
        if let Some(blocker) = d.waits_for {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Waits for thread").color(Palette::RED));
                if ui
                    .link(blocker.to_string())
                    .on_hover_text("Show the thread")
                    .clicked()
                {
                    wants.push(Want::OpenConnection { id: blocker });
                }
            });
        }
        if d.locks.is_empty() && d.metadata_locks.is_empty() {
            ui.weak("None");
        }
        egui::Grid::new("thread-locks")
            .striped(true)
            .show(ui, |ui| {
                for l in &d.locks {
                    let status =
                        RichText::new(l.status.to_lowercase()).color(if l.status == "WAITING" {
                            Palette::RED
                        } else {
                            ui.visuals().text_color()
                        });
                    ui.label(status);
                    ui.label(RichText::new(&l.object).monospace());
                    ui.weak(l.index.as_deref().unwrap_or(""));
                    ui.label(format!("{} {}", l.lock_type.to_lowercase(), l.mode));
                    ui.weak(l.data.as_deref().unwrap_or(""));
                    ui.end_row();
                }
                for (object, kind, lock, status) in &d.metadata_locks {
                    ui.label(status.to_lowercase());
                    ui.label(RichText::new(object).monospace());
                    ui.weak(kind.to_lowercase());
                    ui.label(format!("metadata {}", lock.to_lowercase()));
                    ui.weak("");
                    ui.end_row();
                }
            });
    });
    if !d.history.is_empty() {
        ui.add_space(6.0);
        section(ui, "Statements before, newest first", |ui| {
            for s in &d.history {
                statement(ui, s);
                ui.separator();
            }
        });
    }
}

fn statement(ui: &mut Ui, s: &ThreadStatement) {
    ui.add(egui::Label::new(RichText::new(&s.text).monospace()).wrap());
    let mut notes = vec![
        format!("{} rows examined", compact(s.rows_examined as f64)),
        format!("{} sent", compact(s.rows_sent as f64)),
    ];
    if let Some(seconds) = s.seconds {
        notes.insert(0, duration(seconds));
    }
    if s.tmp_disk_tables > 0 {
        notes.push(format!("{} temp tables on disk", s.tmp_disk_tables));
    }
    if s.sort_rows > 0 {
        notes.push(format!("{} rows sorted", compact(s.sort_rows as f64)));
    }
    if s.no_index_used {
        notes.push("no index used".into());
    }
    if s.errors > 0 {
        notes.push("failed".into());
    }
    ui.weak(notes.join(" · "));
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::mysql::{Thread, ThreadTransaction};

    /// The thread it waits for opens; Kill query asks first, then kills
    /// (#87).
    #[test]
    fn links_and_kill() {
        let mut view = ThreadDetailView::new(42);
        view.requested = true;
        view.on_detail(Ok(ThreadDetail {
            id: 42,
            thread: Some(Thread {
                id: 42,
                user: "app".into(),
                command: "Query".into(),
                query: Some("update t set v = 2".into()),
                ..Thread::default()
            }),
            statement: Some(ThreadStatement {
                text: "update t set v = 2".into(),
                running: true,
                ..ThreadStatement::default()
            }),
            transaction: Some(ThreadTransaction::default()),
            waits_for: Some(41),
            ..ThreadDetail::default()
        }));
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                |ui, (view, wants): &mut (ThreadDetailView, Wants)| view.ui(ui, wants),
                (view, Wants::default()),
            );
        harness.run_steps(2);
        harness.get_by_label("41").click();
        harness.run_steps(2);
        assert!(matches!(
            harness.state().1.list.as_slice(),
            [Want::OpenConnection { id: 41 }]
        ));
        harness.state_mut().1.list.clear();
        harness.get_by_label("Thread ⏷").click();
        harness.run_steps(2);
        harness.get_by_label("Kill query…").click();
        harness.run_steps(2);
        assert!(harness.state().1.list.is_empty(), "asks first");
        harness.get_by_label("Kill").click();
        harness.run_steps(2);
        assert!(matches!(
            harness.state().1.list.as_slice(),
            [Want::KillConnection {
                id: 42,
                query_only: true
            }]
        ));
    }
}
