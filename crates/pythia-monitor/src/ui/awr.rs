//! AWR and ASH reports (only when the Diagnostics Pack is licensed).

use std::path::PathBuf;

use egui::{RichText, Ui};
use pythia_db::DbError;

use super::style::{self, section};
use super::{Analysis, MonitorAction, Want, Wants};
use crate::queries::{AwrSnapshot, Capabilities, explain_error};

struct Report {
    title: String,
    text: String,
}

pub struct AwrView {
    snapshots: Option<Result<Vec<AwrSnapshot>, String>>,
    requested: bool,
    begin: Option<i64>,
    end: Option<i64>,
    ash_minutes: i64,
    report: Option<Report>,
    generating: bool,
    message: Option<(bool, String)>,
    pub(crate) actions: Vec<MonitorAction>,
}

impl Default for AwrView {
    fn default() -> Self {
        Self {
            snapshots: None,
            requested: false,
            begin: None,
            end: None,
            ash_minutes: 30,
            report: None,
            generating: false,
            message: None,
            actions: Vec::new(),
        }
    }
}

impl AwrView {
    pub(crate) fn reload(&mut self) {
        self.requested = false;
    }

    pub(crate) fn on_snapshots(&mut self, result: Result<Vec<AwrSnapshot>, DbError>) {
        match result {
            Ok(list) => {
                // Default range: the last two snapshots.
                if self.end.is_none() || !list.iter().any(|s| Some(s.snap_id) == self.end) {
                    self.end = list.first().map(|s| s.snap_id);
                    self.begin = list.get(1).map(|s| s.snap_id);
                }
                self.snapshots = Some(Ok(list));
            }
            Err(e) => self.snapshots = Some(Err(explain_error(&e))),
        }
    }

    pub(crate) fn on_report(&mut self, result: Result<String, DbError>, html: bool, title: &str) {
        self.generating = false;
        match result {
            Ok(text) if html => match write_report(title, &text) {
                Ok(path) => {
                    self.message = Some((false, format!("Report written to {}", path.display())));
                    self.actions.push(MonitorAction::OpenPath(path));
                }
                Err(e) => self.message = Some((true, format!("Writing report failed: {e}"))),
            },
            Ok(text) => {
                self.message = None;
                self.report = Some(Report {
                    title: title.to_owned(),
                    text,
                });
            }
            Err(e) => self.message = Some((true, explain_error(&e))),
        }
    }

    pub(crate) fn ui(
        &mut self,
        ui: &mut Ui,
        caps: Option<&Capabilities>,
        caps_pending: bool,
        wants: &mut Wants,
    ) {
        let Some(caps) = caps else {
            if caps_pending {
                ui.spinner();
            } else {
                style::unavailable(ui, "AWR / ASH", "Privileges could not be determined.");
            }
            return;
        };
        if let Err(reason) = caps.awr() {
            style::unavailable(ui, "AWR / ASH not available", &reason);
            return;
        }
        if !self.requested {
            self.requested = true;
            wants.push(Want::AwrSnapshots);
        }

        let snapshots = match &self.snapshots {
            None => {
                ui.spinner();
                return;
            }
            Some(Err(e)) => {
                style::unavailable(ui, "AWR snapshots not available", e);
                return;
            }
            Some(Ok(list)) => list.clone(),
        };

        ui.columns(2, |cols| {
            section(&mut cols[0], "AWR report", |ui| {
                if snapshots.len() < 2 {
                    ui.weak(
                        "At least two snapshots are needed. Create one now and another one later.",
                    );
                }
                let label = |id: Option<i64>| {
                    id.and_then(|id| snapshots.iter().find(|s| s.snap_id == id))
                        .map_or("—".to_owned(), |s| format!("{} · {}", s.snap_id, s.end))
                };
                egui::Grid::new("awr-range")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        for (caption, slot) in [("Begin", &mut self.begin), ("End", &mut self.end)]
                        {
                            ui.label(caption);
                            egui::ComboBox::from_id_salt(("awr", caption))
                                .selected_text(label(*slot))
                                .width(220.0)
                                .height(360.0)
                                .show_ui(ui, |ui| {
                                    for s in &snapshots {
                                        ui.selectable_value(
                                            slot,
                                            Some(s.snap_id),
                                            format!("{} · {} – {}", s.snap_id, s.begin, s.end),
                                        );
                                    }
                                });
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                let range = match (self.begin, self.end) {
                    (Some(b), Some(e)) if b < e => snapshots
                        .iter()
                        .find(|s| s.snap_id == e)
                        .map(|s| (s.dbid, s.instance, b, e)),
                    _ => None,
                };
                ui.horizontal(|ui| {
                    let ok = range.is_some() && !self.generating;
                    if ui
                        .add_enabled(ok, egui::Button::new("Text report"))
                        .clicked()
                        && let Some((dbid, instance, begin, end)) = range
                    {
                        self.generating = true;
                        wants.push(Want::AwrReport {
                            dbid,
                            instance,
                            begin,
                            end,
                            html: false,
                        });
                    }
                    if ui
                        .add_enabled(ok, egui::Button::new("HTML report in browser"))
                        .clicked()
                        && let Some((dbid, instance, begin, end)) = range
                    {
                        self.generating = true;
                        wants.push(Want::AwrReport {
                            dbid,
                            instance,
                            begin,
                            end,
                            html: true,
                        });
                    }
                    if range.is_none() && self.begin.is_some() && self.end.is_some() {
                        ui.weak("Begin must be before end");
                    }
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.generating, egui::Button::new("Create snapshot now"))
                        .clicked()
                    {
                        wants.push(Want::CreateSnapshot);
                    }
                    ui.weak(format!("{} snapshots available", snapshots.len()));
                });
            });
            section(&mut cols[1], "ASH report", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Last");
                    ui.add(
                        egui::DragValue::new(&mut self.ash_minutes)
                            .range(1..=1440)
                            .suffix(" min"),
                    );
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(!self.generating, egui::Button::new("Text report"))
                        .clicked()
                    {
                        self.generating = true;
                        wants.push(Want::AshReport {
                            minutes: self.ash_minutes,
                            html: false,
                        });
                    }
                    if ui
                        .add_enabled(
                            !self.generating,
                            egui::Button::new("HTML report in browser"),
                        )
                        .clicked()
                    {
                        self.generating = true;
                        wants.push(Want::AshReport {
                            minutes: self.ash_minutes,
                            html: true,
                        });
                    }
                });
            });
        });

        ui.horizontal(|ui| {
            if self.generating {
                ui.spinner();
                ui.label("Generating report…");
            }
            if let Some((error, text)) = &self.message {
                let color = if *error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.label(RichText::new(text).color(color));
            }
        });

        if let Some(report) = &self.report {
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(RichText::new(&report.title).strong());
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(report.text.clone());
                }
                if ui.small_button("Open as file").clicked() {
                    let path = std::env::temp_dir().join(format!("pythia-{}.txt", report.title));
                    match std::fs::write(&path, &report.text) {
                        Ok(()) => self.actions.push(MonitorAction::OpenPath(path)),
                        Err(e) => self.message = Some((true, e.to_string())),
                    }
                }
                if wants.analyze
                    && ui
                        .small_button("Analyze with AI")
                        .on_hover_text("Have the AI assistant find the bottlenecks in the report")
                        .clicked()
                {
                    wants.push(Want::Analyze(Analysis {
                        title: report_title(&report.title),
                        details: report.text.clone(),
                        ..Default::default()
                    }));
                }
            });
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.add(
                    egui::Label::new(RichText::new(&report.text).monospace().size(11.5)).extend(),
                );
            });
        }
    }
}

/// What a report is, from its file title (`awr_10_12`, `ash_last_30m`).
fn report_title(title: &str) -> String {
    if let Some((begin, end)) = title.strip_prefix("awr_").and_then(|r| r.split_once('_')) {
        format!("the AWR report for snapshots {begin} to {end}")
    } else if let Some(minutes) = title
        .strip_prefix("ash_last_")
        .and_then(|r| r.strip_suffix('m'))
    {
        format!("the ASH report of the last {minutes} minutes")
    } else {
        title.to_owned()
    }
}

/// Writes an HTML report to the temp directory.
fn write_report(title: &str, html: &str) -> std::io::Result<PathBuf> {
    let safe: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = std::env::temp_dir().join(format!("pythia-{safe}.html"));
    std::fs::write(&path, html)?;
    Ok(path)
}
