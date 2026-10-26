//! The SQL Server dashboard.

use egui::Ui;
use pythia_db::DbError;

use super::dashboard::Refresh;
use super::{Want, Wants};
use crate::sqlserver::MsSnapshot;

#[derive(Default)]
pub struct SqlServerDashboardView {
    pub refresh: Refresh,
    snapshot: Option<MsSnapshot>,
    in_flight: bool,
    last_request: Option<f64>,
}

impl SqlServerDashboardView {
    pub(crate) fn on_snapshot(&mut self, result: Result<MsSnapshot, DbError>, _now: f64) {
        self.in_flight = false;
        self.snapshot = result.ok();
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, now: f64, wants: &mut Wants) {
        if self.refresh.due(ui, now, self.last_request, self.in_flight) {
            self.in_flight = true;
            self.last_request = Some(now);
            wants.push(Want::SqlServerDashboard { storage: true });
        }
        ui.weak(format!(
            "{} errors",
            self.snapshot.as_ref().map_or(0, |s| s.errors.len())
        ));
    }
}
