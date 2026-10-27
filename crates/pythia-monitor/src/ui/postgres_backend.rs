//! Details of one PostgreSQL backend: how it connected, the statement it
//! runs or ran last, its transaction and locks, who it waits for and who
//! waits for it; cancel its statement or terminate it.

use egui::{Color32, RichText, Ui};
use pythia_db::DbError;

use super::style::{Palette, compact, duration, section};
use super::{MonitorAction, Want, Wants};
use crate::postgres::{BackendDetail, STATS_ROLE};

pub struct BackendDetailView {
    pub pid: i64,
    detail: Option<BackendDetail>,
    error: Option<String>,
    loading: bool,
    requested: bool,
    /// Asking before a signal: to cancel the query (`false`) or terminate.
    confirm: Option<bool>,
}

impl BackendDetailView {
    pub fn new(pid: i64) -> Self {
        Self {
            pid,
            detail: None,
            error: None,
            loading: false,
            requested: false,
            confirm: None,
        }
    }

    pub fn title(&self) -> String {
        format!("Backend {}", self.pid)
    }

    /// Reads it again with the next frame.
    pub(crate) fn refresh(&mut self) {
        if !self.loading {
            self.requested = false;
        }
    }

    pub(crate) fn on_detail(&mut self, result: Result<BackendDetail, DbError>) {
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
            wants.push(Want::ConnectionDetail { id: self.pid });
        }
        let backend = self.detail.as_ref().and_then(|d| d.backend.as_ref());
        let (mut confirm, mut refresh) = (None, false);
        ui.horizontal(|ui| {
            match backend {
                Some(b) => {
                    ui.label(
                        RichText::new(format!("{}  {}", b.pid, b.user.as_deref().unwrap_or("")))
                            .strong()
                            .monospace(),
                    );
                    let state = match (&b.state, b.state_secs) {
                        (Some(s), Some(secs)) => format!("{s} for {}", duration(secs)),
                        (Some(s), None) => s.clone(),
                        (None, _) => "state hidden".into(),
                    };
                    ui.weak(format!(
                        "{} · {} · {state}",
                        b.database.as_deref().unwrap_or("no database"),
                        b.client
                    ));
                }
                None if self.detail.is_some() => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!("Backend {} has ended", self.pid),
                    );
                }
                None => {
                    ui.label(RichText::new(self.title()).strong());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if backend.is_some() {
                    ui.menu_button("Backend ⏷", |ui| {
                        if ui
                            .button("Cancel query…")
                            .on_hover_text("Ends the statement it runs; the session stays")
                            .clicked()
                        {
                            confirm = Some(false);
                            ui.close();
                        }
                        if ui
                            .button("Terminate…")
                            .on_hover_text("Ends the session and rolls back its transaction")
                            .clicked()
                        {
                            confirm = Some(true);
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
        self.signal_dialog(ui.ctx(), wants);
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        let Some(d) = &self.detail else {
            ui.spinner();
            return;
        };
        super::style::section_errors(ui, &d.errors);
        if d.backend.is_none() {
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| details(ui, d, wants));
    }

    fn signal_dialog(&mut self, ctx: &egui::Context, wants: &mut Wants) {
        let Some(terminate) = self.confirm else {
            return;
        };
        let detail = self.detail.as_ref();
        let transaction = detail.and_then(|d| d.transaction.as_ref());
        let active = detail
            .and_then(|d| d.backend.as_ref())
            .is_some_and(|b| b.state.as_deref() == Some("active"));
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new(("signal-backend", self.pid))).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading(if terminate {
                "Terminate backend"
            } else {
                "Cancel the running statement"
            });
            ui.add_space(6.0);
            if terminate {
                ui.label(format!("End the session of backend {}?", self.pid));
                match transaction {
                    Some(t) => {
                        ui.label(
                            RichText::new(format!(
                                "Its transaction, open for {}, is rolled back{}.",
                                duration(t.age_secs),
                                if t.xid.is_some() {
                                    " with what it changed"
                                } else {
                                    " (it has only read so far)"
                                }
                            ))
                            .color(Palette::ORANGE),
                        );
                    }
                    None => {
                        ui.label("It has no open transaction.");
                    }
                }
            } else {
                ui.label(format!(
                    "End the statement backend {} runs? Its session stays.",
                    self.pid
                ));
                if !active {
                    ui.weak("It runs nothing right now.");
                }
                if transaction.is_some() {
                    ui.weak("Its open transaction is aborted and has to be rolled back.");
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let label = if terminate {
                    "Terminate"
                } else {
                    "Cancel query"
                };
                let go = egui::Button::new(RichText::new(label).color(Color32::WHITE))
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
                    id: self.pid,
                    query_only: !terminate,
                });
            }
            self.confirm = None;
        }
    }
}

/// Links to other backends, after a label.
fn backend_links(ui: &mut Ui, label: RichText, pids: &[i64], wants: &mut Wants) {
    if pids.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        for pid in pids {
            if ui
                .link(pid.to_string())
                .on_hover_text("Show the session")
                .clicked()
            {
                wants.push(Want::OpenConnection { id: *pid });
            }
        }
    });
}

fn details(ui: &mut Ui, d: &BackendDetail, wants: &mut Wants) {
    let Some(b) = &d.backend else {
        return;
    };
    if !d.connection.is_empty() {
        section(ui, "Connection", |ui| {
            egui::Grid::new("backend-connection")
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
    let active = b.state.as_deref() == Some("active");
    let title = if active {
        "Statement it runs"
    } else {
        "Statement it ran last"
    };
    section(ui, title, |ui| match &b.query {
        Some(q) => {
            ui.add(egui::Label::new(RichText::new(q).monospace()).wrap());
            let mut notes = Vec::new();
            if let Some(secs) = b.query_secs {
                notes.push(format!("started {} ago", duration(secs)));
            }
            if let Some(wait) = b.wait.as_ref().filter(|_| active) {
                notes.push(format!("waiting for {wait}"));
            }
            if !notes.is_empty() {
                ui.weak(notes.join(" · "));
            }
            if ui
                .small_button("Open in SQL window")
                .on_hover_text("Run it, explain it")
                .clicked()
            {
                wants.push(Want::Act(MonitorAction::OpenSql(q.clone())));
            }
        }
        None if b.state.is_none() => {
            ui.weak(format!("Another user's: it needs {STATS_ROLE}."));
        }
        None => {
            ui.weak("—");
        }
    });
    ui.add_space(6.0);
    section(ui, "Transaction", |ui| match &d.transaction {
        Some(t) => {
            egui::Grid::new("backend-transaction")
                .num_columns(2)
                .show(ui, |ui| {
                    let mut rows = vec![
                        ("Open for", duration(t.age_secs)),
                        (
                            "Transaction id",
                            t.xid
                                .clone()
                                .unwrap_or_else(|| "none yet: it has only read".into()),
                        ),
                    ];
                    if let Some(xmin) = &t.xmin {
                        let age = t.xmin_age.map_or(String::new(), |a| {
                            format!(
                                " ({} transactions back: VACUUM keeps what they changed)",
                                compact(a as f64)
                            )
                        });
                        rows.push(("Snapshot from", format!("{xmin}{age}")));
                    }
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
        backend_links(
            ui,
            RichText::new("Waits for").color(Palette::RED),
            &d.blocked_by,
            wants,
        );
        backend_links(ui, RichText::new("Blocks"), &d.blocks, wants);
        if d.locks.is_empty() {
            ui.weak("None");
            return;
        }
        egui::Grid::new("backend-locks")
            .striped(true)
            .show(ui, |ui| {
                for l in &d.locks {
                    let status = if l.granted { "granted" } else { "waiting" };
                    ui.label(RichText::new(status).color(if l.granted {
                        ui.visuals().text_color()
                    } else {
                        Palette::RED
                    }));
                    ui.label(RichText::new(l.relation.as_deref().unwrap_or("")).monospace());
                    ui.weak(&l.kind);
                    ui.label(&l.mode);
                    ui.weak(l.target.as_deref().unwrap_or(""));
                    ui.end_row();
                }
            });
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::postgres::{Backend, BackendTransaction};

    /// The backend it waits for opens; Cancel query asks first, then
    /// cancels (#96).
    #[test]
    fn links_and_cancel() {
        let mut view = BackendDetailView::new(4242);
        view.requested = true;
        view.on_detail(Ok(BackendDetail {
            pid: 4242,
            backend: Some(Backend {
                pid: 4242,
                user: Some("app".into()),
                state: Some("active".into()),
                wait: Some("Lock: transactionid".into()),
                query: Some("update t set v = 2".into()),
                ..Backend::default()
            }),
            transaction: Some(BackendTransaction {
                age_secs: 12.0,
                ..BackendTransaction::default()
            }),
            blocked_by: vec![4141],
            ..BackendDetail::default()
        }));
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                |ui, (view, wants): &mut (BackendDetailView, Wants)| view.ui(ui, wants),
                (view, Wants::default()),
            );
        harness.run_steps(2);
        harness.get_by_label("4141").click();
        harness.run_steps(2);
        assert!(matches!(
            harness.state().1.list.as_slice(),
            [Want::OpenConnection { id: 4141 }]
        ));
        harness.state_mut().1.list.clear();
        harness.get_by_label("Backend ⏷").click();
        harness.run_steps(2);
        harness.get_by_label("Cancel query…").click();
        harness.run_steps(2);
        assert!(harness.state().1.list.is_empty(), "asks first");
        harness.get_by_label("Cancel query").click();
        harness.run_steps(2);
        assert!(matches!(
            harness.state().1.list.as_slice(),
            [Want::KillConnection {
                id: 4242,
                query_only: true
            }]
        ));
    }
}
