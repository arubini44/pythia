//! What the management packs keep about a session or a statement: its
//! activity from Active Session History, an ASH report of it and its plan
//! history from AWR (Diagnostics Pack), and the executions SQL Monitor
//! watched (Tuning Pack). Each section says why when its pack is off.

use egui::{RichText, Ui};
use pythia_db::DbError;

use super::style::{self, Palette, compact, duration, section};
use super::{MonitorAction, Want, Wants};
use crate::queries::{
    AshActivity, Capabilities, MonitoredExecution, PlanHistoryRow, Scope, explain_error,
};

/// Minutes of Active Session History shown.
pub(crate) const ASH_MINUTES: i64 = 60;

type Loaded<T> = Option<Result<T, String>>;

pub(crate) struct HistoryView {
    pub scope: Scope,
    requested: bool,
    ash: Loaded<AshActivity>,
    monitored: Loaded<Vec<MonitoredExecution>>,
    plans: Loaded<Vec<PlanHistoryRow>>,
    /// A plan AWR kept: its hash and text.
    awr_plan: Option<(i64, Loaded<Vec<String>>)>,
    /// A text report shown (ASH or SQL Monitor): title and text.
    report: Option<(String, Loaded<String>)>,
}

/// What a history request is for, to route its result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HistoryPart {
    Ash,
    Monitored,
    Plans,
    /// A plan hash and the AWR (dbid) it is in.
    AwrPlan(i64, i64),
    /// A report: text (shown here) or HTML (opened in the browser).
    Report {
        title: String,
        html: bool,
    },
}

/// A finished history request.
pub(crate) enum HistoryResult {
    Ash(Result<AshActivity, DbError>),
    Monitored(Result<Vec<MonitoredExecution>, DbError>),
    Plans(Result<Vec<PlanHistoryRow>, DbError>),
    AwrPlan(i64, Result<Vec<String>, DbError>),
    Report {
        title: String,
        html: bool,
        result: Result<String, DbError>,
    },
}

impl HistoryView {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            requested: false,
            ash: None,
            monitored: None,
            plans: None,
            awr_plan: None,
            report: None,
        }
    }

    /// Reads it all again when shown next.
    pub fn refresh(&mut self) {
        self.requested = false;
    }

    pub(crate) fn on_result(&mut self, result: HistoryResult, wants: &mut Vec<MonitorAction>) {
        let text = |e: DbError| explain_error(&e);
        match result {
            HistoryResult::Ash(r) => self.ash = Some(r.map_err(text)),
            HistoryResult::Monitored(r) => self.monitored = Some(r.map_err(text)),
            HistoryResult::Plans(r) => self.plans = Some(r.map_err(text)),
            HistoryResult::AwrPlan(hash, r) => {
                if self.awr_plan.as_ref().is_some_and(|(h, _)| *h == hash) {
                    self.awr_plan = Some((hash, Some(r.map_err(text))));
                }
            }
            HistoryResult::Report {
                title,
                html,
                result,
            } => match result {
                Ok(report) if html => match super::awr::write_report(&title, &report) {
                    Ok(path) => {
                        wants.push(MonitorAction::OpenPath(path));
                        self.report = None;
                    }
                    Err(e) => {
                        self.report = Some((title, Some(Err(format!("Writing it failed: {e}")))))
                    }
                },
                result => self.report = Some((title, Some(result.map_err(text)))),
            },
        }
    }

    fn want(&self, part: HistoryPart, wants: &mut Wants) {
        wants.push(Want::History {
            scope: self.scope.clone(),
            part,
        });
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, caps: Option<&Capabilities>, wants: &mut Wants) {
        let Some(caps) = caps else {
            ui.spinner();
            return;
        };
        let ash = caps.ash();
        let monitor = caps.sql_monitor();
        let plans = match self.scope {
            Scope::Sql(_) => Some(caps.plan_history()),
            Scope::Session { .. } => None,
        };
        if !self.requested {
            self.requested = true;
            if ash.is_ok() {
                self.ash = None;
                self.want(HistoryPart::Ash, wants);
            }
            if monitor.is_ok() {
                self.monitored = None;
                self.want(HistoryPart::Monitored, wants);
            }
            if let Some(Ok(())) = plans {
                self.plans = None;
                self.want(HistoryPart::Plans, wants);
            }
        }
        section(
            ui,
            &format!("Activity in the last {ASH_MINUTES} minutes (Active Session History)"),
            |ui| match &ash {
                Err(reason) => {
                    ui.weak(reason);
                }
                Ok(()) => {
                    if ui
                        .small_button("ASH report")
                        .on_hover_text("Oracle's ASH report of it for the same time, as text")
                        .clicked()
                    {
                        let title = format!("ASH report, last {ASH_MINUTES} minutes");
                        self.report = Some((title.clone(), None));
                        self.want(HistoryPart::Report { title, html: false }, wants);
                    }
                    activity(ui, self.ash.as_ref(), &self.scope, wants);
                }
            },
        );
        if let Some(plans) = plans {
            ui.add_space(6.0);
            section(ui, "Plan history (AWR)", |ui| match plans {
                Err(reason) => {
                    ui.weak(reason);
                }
                Ok(()) => {
                    if let Some((hash, dbid)) = plan_history(ui, self.plans.as_ref()) {
                        self.awr_plan = Some((hash, None));
                        self.want(HistoryPart::AwrPlan(hash, dbid), wants);
                    }
                    if let Some((hash, plan)) = &self.awr_plan {
                        ui.add_space(4.0);
                        ui.label(RichText::new(format!("Plan {hash}")).strong());
                        match plan {
                            None => {
                                ui.spinner();
                            }
                            Some(Err(e)) => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            Some(Ok(lines)) if lines.is_empty() => {
                                // The snapshot kept its statistics only (#93).
                                ui.weak("AWR kept no plan for it in this snapshot.");
                            }
                            Some(Ok(lines)) => {
                                egui::ScrollArea::horizontal()
                                    .id_salt("awr-plan")
                                    .show(ui, |ui| {
                                        ui.label(RichText::new(lines.join("\n")).monospace());
                                    });
                            }
                        }
                    }
                }
            });
        }
        ui.add_space(6.0);
        section(ui, "SQL Monitor", |ui| match &monitor {
            Err(reason) => {
                ui.weak(reason);
            }
            Ok(()) => {
                ui.weak(
                    "Executions that used at least 5 s of CPU or I/O, ran in parallel or were \
                     hinted MONITOR.",
                );
                if let Some((e, html)) = monitored(ui, self.monitored.as_ref()) {
                    let title = format!("SQL Monitor {} {}", e.sql_id, e.exec_id);
                    if !html {
                        self.report = Some((title.clone(), None));
                    }
                    wants.push(Want::MonitorReport {
                        scope: self.scope.clone(),
                        execution: Box::new(e),
                        title,
                        html,
                    });
                }
            }
        });
        if let Some((title, report)) = &self.report {
            ui.add_space(6.0);
            section(ui, title, |ui| match report {
                None => {
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                Some(Ok(text)) => {
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                    egui::ScrollArea::horizontal()
                        .id_salt("history-report")
                        .show(ui, |ui| {
                            ui.label(RichText::new(text).monospace().small());
                        });
                }
            });
        }
    }
}

/// The activity timeline and what it was about.
fn activity(
    ui: &mut Ui,
    ash: Option<&Result<AshActivity, String>>,
    scope: &Scope,
    wants: &mut Wants,
) {
    let a = match ash {
        None => {
            ui.spinner();
            return;
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        Some(Ok(a)) => a,
    };
    if a.samples == 0 {
        ui.weak("No samples: it wasn't active in this time.");
        return;
    }
    ui.weak(format!(
        "{} samples, about {} active",
        a.samples,
        duration(a.samples as f64)
    ));
    // Minutes with their wait classes, as stacked bars.
    let mut minutes: Vec<&str> = a.timeline.iter().map(|t| t.0.as_str()).collect();
    minutes.dedup();
    for minute in minutes {
        let segments: Vec<(String, f64, egui::Color32)> = a
            .timeline
            .iter()
            .filter(|t| t.0 == minute)
            .map(|t| (t.1.clone(), t.2 as f64, style::wait_class_color(&t.1)))
            .collect();
        ui.horizontal(|ui| {
            ui.label(RichText::new(minute).monospace().small());
            style::stacked_bar(ui, &segments, None);
        });
    }
    let list = |ui: &mut Ui, title: &str, rows: &[(String, i64)]| {
        if rows.is_empty() {
            return;
        }
        ui.add_space(4.0);
        ui.label(RichText::new(title).strong());
        let most = rows.iter().map(|r| r.1).max().unwrap_or(1).max(1) as f32;
        for (name, n) in rows {
            style::usage_bar(ui, *n as f32 / most, Palette::BLUE, format!("{n}  {name}"));
        }
    };
    list(ui, "Events", &a.events);
    list(ui, "Objects", &a.objects);
    if !a.sql.is_empty() {
        ui.add_space(4.0);
        ui.label(RichText::new("Statements").strong());
        for (sql_id, n) in &a.sql {
            ui.horizontal(|ui| {
                if ui
                    .link(RichText::new(sql_id).monospace())
                    .on_hover_text("Show the statement")
                    .clicked()
                {
                    wants.push(Want::OpenSqlDetail {
                        sql_id: sql_id.clone(),
                    });
                }
                ui.weak(format!("{n} samples"));
            });
        }
    }
    let sessions =
        |ui: &mut Ui, title: &str, rows: Vec<(i64, Option<i64>, i64)>, wants: &mut Wants| {
            if rows.is_empty() {
                return;
            }
            ui.add_space(4.0);
            ui.label(RichText::new(title).strong());
            ui.horizontal_wrapped(|ui| {
                for (sid, serial, n) in rows {
                    let text = format!("{sid} ({n} samples)");
                    match serial {
                        Some(serial) => {
                            if ui.link(text).on_hover_text("Show the session").clicked() {
                                wants.push(Want::OpenDetail {
                                    sid,
                                    serial,
                                    tab: None,
                                });
                            }
                        }
                        None => {
                            ui.label(text);
                        }
                    }
                }
            });
        };
    if let Scope::Sql(_) = scope {
        let ran = a.sessions.iter().map(|s| (s.0, Some(s.1), s.2)).collect();
        sessions(ui, "Sessions that ran it", ran, wants);
    }
    sessions(ui, "Blocked by", a.blockers.clone(), wants);
}

/// Executions per AWR snapshot; returns the plan hash clicked, with the
/// AWR it is in.
fn plan_history(
    ui: &mut Ui,
    plans: Option<&Result<Vec<PlanHistoryRow>, String>>,
) -> Option<(i64, i64)> {
    let rows = match plans {
        None => {
            ui.spinner();
            return None;
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return None;
        }
        Some(Ok(rows)) => rows,
    };
    if rows.is_empty() {
        ui.weak("Not in AWR (it ran too little, or not since the oldest snapshot).");
        return None;
    }
    let mut hashes: Vec<i64> = rows.iter().map(|r| r.plan_hash).collect();
    hashes.sort_unstable();
    hashes.dedup();
    if hashes.len() > 1 {
        ui.label(RichText::new(format!("{} plans over time", hashes.len())).color(Palette::ORANGE));
    }
    let mut clicked = None;
    egui::Grid::new("plan-history")
        .striped(true)
        .show(ui, |ui| {
            for t in [
                "Snapshot ended",
                "Plan",
                "Execs",
                "Elapsed / exec",
                "Gets / exec",
                "Rows / exec",
            ] {
                ui.strong(t);
            }
            ui.end_row();
            for r in rows {
                let per = |v: f64| {
                    if r.executions > 0.0 {
                        v / r.executions
                    } else {
                        0.0
                    }
                };
                ui.label(&r.end);
                if ui
                    .link(RichText::new(r.plan_hash.to_string()).monospace())
                    .on_hover_text("Show the plan")
                    .clicked()
                {
                    clicked = Some((r.plan_hash, r.dbid));
                }
                ui.label(compact(r.executions));
                ui.label(duration(per(r.elapsed)));
                ui.label(compact(per(r.buffer_gets)));
                ui.label(compact(per(r.rows)));
                ui.end_row();
            }
        });
    clicked
}

/// The monitored executions; returns the one whose report was asked for,
/// and whether as HTML.
fn monitored(
    ui: &mut Ui,
    list: Option<&Result<Vec<MonitoredExecution>, String>>,
) -> Option<(MonitoredExecution, bool)> {
    let rows = match list {
        None => {
            ui.spinner();
            return None;
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return None;
        }
        Some(Ok(rows)) => rows,
    };
    if rows.is_empty() {
        ui.weak("None monitored.");
        return None;
    }
    let mut asked = None;
    egui::Grid::new("sql-monitor").striped(true).show(ui, |ui| {
        for t in [
            "Started", "Status", "SQL_ID", "Session", "Elapsed", "CPU", "Gets", "",
        ] {
            ui.strong(t);
        }
        ui.end_row();
        for e in rows {
            ui.label(&e.start);
            ui.label(e.status.to_lowercase());
            ui.label(RichText::new(&e.sql_id).monospace());
            ui.label(format!(
                "{},{} {}",
                e.sid,
                e.serial,
                e.user.as_deref().unwrap_or("")
            ));
            ui.label(duration(e.elapsed));
            ui.label(duration(e.cpu));
            ui.label(compact(e.buffer_gets));
            ui.horizontal(|ui| {
                if ui.small_button("Report").clicked() {
                    asked = Some((e.clone(), false));
                }
                if ui
                    .small_button("HTML")
                    .on_hover_text("Open the report in the browser")
                    .clicked()
                {
                    asked = Some((e.clone(), true));
                }
            });
            ui.end_row();
        }
    });
    asked
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    fn caps(packs: &str) -> Capabilities {
        Capabilities {
            views: vec![
                ("V$ACTIVE_SESSION_HISTORY", true),
                ("DBA_HIST_SQLSTAT", true),
                ("DBA_HIST_SNAPSHOT", true),
                ("V$SQL_MONITOR", true),
            ],
            sql_monitor_package: Some("DBMS_SQL_MONITOR"),
            pack_access: Some(packs.into()),
            ..Capabilities::default()
        }
    }

    /// What the view asks for with `packs` enabled, and the harness.
    fn shown(packs: &str) -> (Vec<String>, Harness<'static, (HistoryView, Wants)>) {
        let caps = caps(packs);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(900.0, 900.0))
            .build_ui_state(
                move |ui, (view, wants): &mut (HistoryView, Wants)| view.ui(ui, Some(&caps), wants),
                (
                    HistoryView::new(Scope::Sql("7h35uxf5uhmm1".into())),
                    Wants::default(),
                ),
            );
        harness.run_steps(2);
        let asked = harness
            .state()
            .1
            .list
            .iter()
            .map(|w| match w {
                Want::History { part, .. } => format!("{part:?}"),
                _ => "other".into(),
            })
            .collect();
        (asked, harness)
    }

    /// Without the packs, each section says why and nothing is read; with
    /// them, activity, plan history and SQL Monitor are (#74).
    #[test]
    fn gated_by_the_packs() {
        let (asked, harness) = shown("NONE");
        assert!(asked.is_empty(), "{asked:?}");
        // Activity and plan history.
        let diagnostics =
            harness.query_all_by_label_contains("requires the Oracle Diagnostics Pack");
        assert_eq!(diagnostics.count(), 2);
        assert!(
            harness
                .query_by_label_contains("requires the Oracle Tuning Pack")
                .is_some()
        );
        let (asked, mut harness) = shown("DIAGNOSTIC+TUNING");
        assert_eq!(asked, ["Ash", "Monitored", "Plans"]);
        harness.state_mut().1.list.clear();
        harness.get_by_label("ASH report").click();
        harness.run_steps(2);
        assert!(matches!(
            harness.state().1.list.as_slice(),
            [Want::History {
                part: HistoryPart::Report { html: false, .. },
                ..
            }]
        ));
    }
}
