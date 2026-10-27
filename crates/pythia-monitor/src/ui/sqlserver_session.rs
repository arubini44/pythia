//! Details of one SQL Server session: how it connected, the request it
//! runs with its plan, its transactions and locks, who it waits for and who
//! waits for it; kill it.

use egui::{Color32, RichText, Ui};
use pythia_db::DbError;

use super::style::{Palette, bytes, compact, duration, section};
use super::{MonitorAction, Want, Wants};
use crate::sqlserver::{SERVER_STATE, SessionDetail};

pub struct MsSessionDetailView {
    pub id: i64,
    detail: Option<SessionDetail>,
    error: Option<String>,
    loading: bool,
    requested: bool,
    /// Asking before a kill.
    confirm: bool,
}

impl MsSessionDetailView {
    pub fn new(id: i64) -> Self {
        Self {
            id,
            detail: None,
            error: None,
            loading: false,
            requested: false,
            confirm: false,
        }
    }

    pub fn title(&self) -> String {
        format!("Session {}", self.id)
    }

    /// Reads it again with the next frame.
    pub(crate) fn refresh(&mut self) {
        if !self.loading {
            self.requested = false;
        }
    }

    pub(crate) fn on_detail(&mut self, result: Result<SessionDetail, DbError>) {
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
        let session = self.detail.as_ref().and_then(|d| d.session.as_ref());
        let (mut confirm, mut refresh) = (false, false);
        ui.horizontal(|ui| {
            match session {
                Some(s) => {
                    ui.label(
                        RichText::new(format!("{}  {}", self.id, s.login))
                            .strong()
                            .monospace(),
                    );
                    let place: Vec<&str> = [&s.host, &s.program, &s.database]
                        .into_iter()
                        .filter_map(|v| v.as_deref())
                        .collect();
                    ui.weak(format!("{} · {}", place.join(" · "), s.status));
                }
                None if self.detail.is_some() => {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        format!(
                            "Session {} has ended (or isn't visible without {SERVER_STATE})",
                            self.id
                        ),
                    );
                }
                None => {
                    ui.label(RichText::new(self.title()).strong());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if session.is_some() {
                    ui.menu_button("Session ⏷", |ui| {
                        if ui
                            .button("Kill…")
                            .on_hover_text("Ends the session and rolls back its open transaction")
                            .clicked()
                        {
                            confirm = true;
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
        if confirm {
            self.confirm = true;
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
        if d.session.is_none() {
            return;
        }
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| details(ui, d, wants));
    }

    fn kill_dialog(&mut self, ctx: &egui::Context, wants: &mut Wants) {
        if !self.confirm {
            return;
        }
        let transactions = self
            .detail
            .as_ref()
            .map(|d| d.transactions.as_slice())
            .unwrap_or_default();
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::new(("kill-session", self.id))).show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.heading("Kill session");
            ui.add_space(6.0);
            ui.label(format!(
                "End session {}? Its connection is closed.",
                self.id
            ));
            match transactions.iter().max_by_key(|t| t.age_secs) {
                Some(t) => {
                    let log: f64 = transactions.iter().map(|t| t.log_bytes).sum();
                    ui.label(
                        RichText::new(format!(
                            "Its transaction, open for {}, is rolled back ({} of log): that \
                             takes about as long as the work did.",
                            duration(t.age_secs as f64),
                            bytes(log)
                        ))
                        .color(Palette::ORANGE),
                    );
                }
                None => {
                    ui.label("It has no open transaction.");
                }
            }
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let kill = egui::Button::new(RichText::new("Kill").color(Color32::WHITE))
                    .fill(Palette::RED);
                if ui.add(kill).clicked() {
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
                    id: self.id,
                    query_only: false,
                });
            }
            self.confirm = false;
        }
    }
}

/// Links to other sessions, after a label.
fn session_links(ui: &mut Ui, label: RichText, ids: &[i64], wants: &mut Wants) {
    if ids.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        for id in ids {
            if ui
                .link(id.to_string())
                .on_hover_text("Show the session")
                .clicked()
            {
                wants.push(Want::OpenConnection { id: *id });
            }
        }
    });
}

fn grid(ui: &mut Ui, id: &str, rows: &[(&str, String)]) {
    egui::Grid::new(id).num_columns(2).show(ui, |ui| {
        for (name, value) in rows {
            ui.weak(*name);
            ui.label(value);
            ui.end_row();
        }
    });
}

fn details(ui: &mut Ui, d: &SessionDetail, wants: &mut Wants) {
    let Some(s) = &d.session else {
        return;
    };
    section(ui, "Session", |ui| {
        let mut rows = vec![
            ("Logged in", s.login_time.clone()),
            (
                "Last request ended",
                s.last_request_end.clone().unwrap_or_else(|| "—".into()),
            ),
            ("Isolation level", s.isolation.clone()),
            ("CPU", duration(s.cpu_ms as f64 / 1000.0)),
            (
                "Reads / writes",
                format!(
                    "{} logical / {}",
                    compact(s.logical_reads as f64),
                    compact(s.writes as f64)
                ),
            ),
        ];
        rows.extend(s.connection.iter().map(|(n, v)| (n.as_str(), v.clone())));
        grid(ui, "mssql-session", &rows);
    });
    ui.add_space(6.0);
    match &d.request {
        Some(r) => {
            section(ui, "Statement it runs", |ui| {
                let statement = r.statement.clone().unwrap_or_default();
                ui.add(egui::Label::new(RichText::new(&statement).monospace()).wrap());
                let mut notes = vec![
                    format!("{} {}", r.command.to_lowercase(), r.status),
                    format!("for {}", duration(r.elapsed_secs)),
                ];
                if let Some(wait) = &r.wait_type {
                    notes.push(format!("waiting {} for {wait}", duration(r.wait_secs)));
                }
                if let Some(resource) = &r.wait_resource {
                    notes.push(resource.clone());
                }
                ui.weak(notes.join(" · "));
                if !statement.is_empty()
                    && ui
                        .small_button("Open in SQL window")
                        .on_hover_text("Run it, explain it")
                        .clicked()
                {
                    wants.push(Want::Act(MonitorAction::OpenSql(statement)));
                }
            });
            if !d.plan.is_empty() {
                ui.add_space(6.0);
                section(ui, "Plan (cached, estimated)", |ui| {
                    egui::ScrollArea::horizontal()
                        .id_salt("mssql-plan")
                        .show(ui, |ui| {
                            ui.label(RichText::new(d.plan.join("\n")).monospace());
                        });
                    if let Some(xml) = &d.plan_xml
                        && ui
                            .small_button("Copy as showplan XML")
                            .on_hover_text("Open it in SSMS or Azure Data Studio")
                            .clicked()
                    {
                        wants.push(Want::Act(MonitorAction::CopyText(xml.clone())));
                    }
                });
            }
        }
        None => {
            section(ui, "Batch it ran last", |ui| match &d.last_batch {
                Some(batch) => {
                    ui.add(egui::Label::new(RichText::new(batch).monospace()).wrap());
                }
                None => {
                    ui.weak("—");
                }
            });
        }
    }
    ui.add_space(6.0);
    section(ui, "Transactions", |ui| {
        if d.transactions.is_empty() {
            ui.weak("No open transaction");
            return;
        }
        egui::Grid::new("mssql-transactions")
            .striped(true)
            .show(ui, |ui| {
                for t in &d.transactions {
                    ui.label(&t.name);
                    ui.label(format!("open for {}", duration(t.age_secs as f64)));
                    ui.weak(format!("{} · {}", t.kind, t.state));
                    ui.label(format!("{} of log", bytes(t.log_bytes)));
                    ui.end_row();
                }
            });
    });
    ui.add_space(6.0);
    section(ui, "Locks", |ui| {
        let waits_for: Vec<i64> = d.request.iter().filter_map(|r| r.blocked_by).collect();
        session_links(
            ui,
            RichText::new("Waits for").color(Palette::RED),
            &waits_for,
            wants,
        );
        session_links(ui, RichText::new("Blocks"), &d.blocks, wants);
        if d.locks.is_empty() {
            ui.weak("None");
            return;
        }
        egui::Grid::new("mssql-locks").striped(true).show(ui, |ui| {
            for l in &d.locks {
                let granted = l.status == "GRANT";
                ui.label(RichText::new(l.status.to_lowercase()).color(if granted {
                    ui.visuals().text_color()
                } else {
                    Palette::RED
                }));
                let object = l
                    .object
                    .clone()
                    .or_else(|| l.database.clone())
                    .unwrap_or_default();
                ui.label(RichText::new(object).monospace());
                ui.weak(l.resource.to_lowercase());
                ui.label(&l.mode);
                ui.weak(l.description.as_deref().unwrap_or(""));
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
    use crate::sqlserver::{Request, SessionInfo, SessionTransaction};

    /// The session it waits for opens; Kill asks first, saying what is
    /// rolled back, then kills (#96).
    #[test]
    fn links_and_kill() {
        let mut view = MsSessionDetailView::new(57);
        view.requested = true;
        view.on_detail(Ok(SessionDetail {
            id: 57,
            session: Some(SessionInfo {
                login: "app".into(),
                status: "suspended".into(),
                ..SessionInfo::default()
            }),
            request: Some(Request {
                session: 57,
                statement: Some("update t set v = 2".into()),
                wait_type: Some("LCK_M_U".into()),
                blocked_by: Some(55),
                ..Request::default()
            }),
            transactions: vec![SessionTransaction {
                name: "user_transaction".into(),
                age_secs: 90,
                log_bytes: 2048.0,
                ..SessionTransaction::default()
            }],
            ..SessionDetail::default()
        }));
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 700.0))
            .build_ui_state(
                |ui, (view, wants): &mut (MsSessionDetailView, Wants)| view.ui(ui, wants),
                (view, Wants::default()),
            );
        harness.run_steps(2);
        harness.get_by_label("55").click();
        harness.run_steps(2);
        assert!(matches!(
            harness.state().1.list.as_slice(),
            [Want::OpenConnection { id: 55 }]
        ));
        harness.state_mut().1.list.clear();
        harness.get_by_label("Session ⏷").click();
        harness.run_steps(2);
        harness.get_by_label("Kill…").click();
        harness.run_steps(2);
        assert!(harness.state().1.list.is_empty(), "asks first");
        assert!(
            harness
                .query_by_label_contains("is rolled back (2.0 KB of log)")
                .is_some()
        );
        harness.get_by_label("Kill").click();
        harness.run_steps(2);
        assert!(matches!(
            harness.state().1.list.as_slice(),
            [Want::KillConnection {
                id: 57,
                query_only: false
            }]
        ));
    }
}
