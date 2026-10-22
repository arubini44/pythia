//! Trace analyzer: pick a trace file (database or local), show a tkprof-style report.

use egui::{RichText, Ui};
use pythia_db::DbError;

use super::style::{self, Kpi, Palette, compact, duration, section};
use super::{Analysis, Want, Wants};
use crate::queries::{Capabilities, TraceFile, explain_error};
use crate::tkprof::{self, CallKind, CallStats, Report, SortKey, Statement, WaitStats};

#[derive(Default)]
pub struct TraceView {
    files: Option<Result<Vec<TraceFile>, String>>,
    requested: bool,
    filter: String,
    local_path: String,
    loading: Option<String>,
    report: Option<(String, Report)>,
    sort: SortKey,
    show_recursive: bool,
    text_mode: bool,
    error: Option<String>,
}

impl TraceView {
    pub(crate) fn on_files(&mut self, result: Result<Vec<TraceFile>, DbError>) {
        self.files = Some(result.map_err(|e| explain_error(&e)));
    }

    pub(crate) fn on_contents(&mut self, name: &str, result: Result<String, DbError>) {
        self.loading = None;
        match result {
            Ok(text) => self.load_text(name, &text),
            Err(e) => self.error = Some(explain_error(&e)),
        }
    }

    pub(crate) fn on_session_file(
        &mut self,
        result: Result<Option<(String, String)>, DbError>,
    ) -> Option<Want> {
        match result {
            Ok(Some((dir, name))) => {
                self.loading = Some(name.clone());
                Some(Want::ReadTrace { home: dir, name })
            }
            Ok(None) => {
                self.error = Some("The session has no trace file (is it still connected?)".into());
                None
            }
            Err(e) => {
                self.error = Some(explain_error(&e));
                None
            }
        }
    }

    /// Analyzes trace text (e.g. a file the host read from disk).
    pub fn load_text(&mut self, name: &str, text: &str) {
        self.error = None;
        let report = tkprof::parse(text);
        if report.statements.is_empty() {
            self.error = Some(format!(
                "{name} contains no SQL trace records (was 10046 tracing enabled?)"
            ));
        }
        self.report = Some((name.to_owned(), report));
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, caps: Option<&Capabilities>, wants: &mut Wants) {
        let db_files = caps.map(|c| c.trace_files());
        egui::Panel::left("trace-files")
            .resizable(true)
            .default_size(300.0)
            .show(ui, |ui| {
                ui.label(RichText::new("Trace files").strong());
                ui.add_space(4.0);
                match &db_files {
                    Some(Ok(())) => {
                        if !self.requested {
                            self.requested = true;
                            wants.push(Want::TraceFiles {
                                filter: String::new(),
                            });
                        }
                        ui.horizontal(|ui| {
                            let r = ui.add(
                                egui::TextEdit::singleline(&mut self.filter)
                                    .hint_text("Filter, e.g. _ora_")
                                    .desired_width(180.0),
                            );
                            if ui.button("⟳").on_hover_text("Reload list").clicked()
                                || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                            {
                                wants.push(Want::TraceFiles {
                                    filter: self.filter.clone(),
                                });
                            }
                        });
                        match &self.files {
                            None => {
                                ui.spinner();
                            }
                            Some(Err(e)) => {
                                ui.colored_label(ui.visuals().error_fg_color, e);
                            }
                            Some(Ok(files)) => {
                                egui::ScrollArea::vertical()
                                    .max_height(ui.available_height() - 90.0)
                                    .show(ui, |ui| {
                                        for f in files {
                                            let selected = self
                                                .report
                                                .as_ref()
                                                .is_some_and(|(n, _)| *n == f.name);
                                            let r = ui
                                                .add(
                                                    egui::Button::selectable(
                                                        selected,
                                                        RichText::new(&f.name).monospace(),
                                                    )
                                                    .frame_when_inactive(false),
                                                )
                                                .on_hover_text(format!(
                                                    "{}\nchanged {}",
                                                    f.adr_home, f.changed
                                                ));
                                            if r.clicked() && self.loading.is_none() {
                                                self.loading = Some(f.name.clone());
                                                wants.push(Want::ReadTrace {
                                                    home: f.adr_home.clone(),
                                                    name: f.name.clone(),
                                                });
                                            }
                                        }
                                    });
                            }
                        }
                    }
                    Some(Err(reason)) => {
                        ui.label(
                            RichText::new(reason)
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                    None => {
                        ui.spinner();
                    }
                }
                ui.separator();
                ui.label(RichText::new("Local file").strong());
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.local_path)
                            .hint_text("/path/to/file.trc")
                            .desired_width(200.0),
                    );
                    if ui.button("Open").clicked() {
                        match std::fs::read_to_string(self.local_path.trim()) {
                            Ok(text) => {
                                let name = self
                                    .local_path
                                    .trim()
                                    .rsplit('/')
                                    .next()
                                    .unwrap_or("trace")
                                    .to_owned();
                                self.load_text(&name, &text);
                            }
                            Err(e) => self.error = Some(format!("{}: {e}", self.local_path.trim())),
                        }
                    }
                });
                ui.label(
                    RichText::new(
                        "Tip: enable SQL trace for a session from the Sessions view (right click).",
                    )
                    .small()
                    .color(ui.visuals().weak_text_color()),
                );
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(name) = &self.loading {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(format!("Reading {name}…"));
                });
            }
            if let Some(e) = &self.error {
                ui.colored_label(ui.visuals().warn_fg_color, e);
            }
            let Some((name, report)) = &self.report else {
                if self.loading.is_none() {
                    style::unavailable(
                        ui,
                        "Trace analyzer",
                        "Pick a trace file on the left. Pythia parses 10046 traces itself and shows a \
                         tkprof-style report: call statistics, row source operations and waits per statement.",
                    );
                }
                return;
            };
            ui.horizontal(|ui| {
                ui.label(RichText::new(name).strong().monospace());
                ui.weak(format!("{} lines", report.lines));
                ui.separator();
                ui.label("Sort by");
                egui::ComboBox::from_id_salt("tkprof-sort").selected_text(self.sort.label()).show_ui(ui, |ui| {
                    for k in SortKey::ALL {
                        ui.selectable_value(&mut self.sort, k, k.label());
                    }
                });
                ui.checkbox(&mut self.show_recursive, "Recursive SQL");
                ui.checkbox(&mut self.text_mode, "tkprof text");
                if ui.small_button("Copy report").clicked() {
                    ui.ctx().copy_text(report.to_text(self.sort));
                }
                if wants.analyze
                    && ui
                        .small_button("Analyze with AI")
                        .on_hover_text("Have the AI assistant explain where the traced time went")
                        .clicked()
                {
                    let binds = report
                        .statements
                        .iter()
                        .filter(|s| !s.binds.is_empty())
                        .map(bind_line)
                        .collect();
                    wants.push(Want::Analyze(Analysis {
                        title: format!("the trace file {name}"),
                        details: report.to_text(self.sort),
                        binds,
                        ..Default::default()
                    }));
                }
            });
            ui.add_space(4.0);
            if self.text_mode {
                let text = report.to_text(self.sort);
                egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                    ui.add(egui::Label::new(RichText::new(text).monospace().size(11.5)).extend());
                });
                return;
            }
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                summary(ui, report);
                ui.add_space(8.0);
                for (rank, i) in report.sorted(self.sort).into_iter().enumerate() {
                    let s = &report.statements[i];
                    if s.is_recursive() && !self.show_recursive {
                        continue;
                    }
                    statement_ui(ui, rank + 1, i, s, name, wants);
                    ui.add_space(4.0);
                }
            });
        });
    }
}

fn summary(ui: &mut Ui, report: &Report) {
    let nr = report.non_recursive.total();
    let r = report.recursive.total();
    let wait: u64 = report
        .non_recursive
        .waits
        .iter()
        .map(|(_, w)| w.total)
        .sum();
    let mut tiles = vec![
        Kpi::new(
            "User statements",
            report.non_recursive.statements.to_string(),
        )
        .detail(format!("{} recursive", report.recursive.statements)),
        Kpi::new("Elapsed (user)", duration(nr.elapsed as f64 / 1e6))
            .detail(format!("CPU {}", duration(nr.cpu as f64 / 1e6))),
        Kpi::new("Elapsed (recursive)", duration(r.elapsed as f64 / 1e6))
            .detail(format!("CPU {}", duration(r.cpu as f64 / 1e6))),
        Kpi::new("Waited", duration(wait as f64 / 1e6)),
        Kpi::new("Logical reads", compact((nr.query + nr.current) as f64))
            .detail(format!("{} physical", compact(nr.disk as f64))),
        Kpi::new(
            "Transactions",
            format!("{}", report.commits + report.rollbacks),
        )
        .detail(format!(
            "{} commits · {} rollbacks",
            report.commits, report.rollbacks
        )),
    ];
    if let Some((name, _)) = report.non_recursive.waits.first() {
        tiles[3] = Kpi::new("Waited", duration(wait as f64 / 1e6)).detail(format!("mostly {name}"));
    }
    style::kpi_grid(ui, &tiles, 150.0);
    if !report.non_recursive.waits.is_empty() {
        ui.add_space(6.0);
        section(ui, "Waits (user statements)", |ui| {
            waits_grid(ui, "waits-total", &report.non_recursive.waits)
        });
    }
}

/// A statement's captured binds, as `SQL_ID: values`.
fn bind_line(s: &Statement) -> String {
    format!(
        "{}: {}",
        s.sql_id.as_deref().unwrap_or("statement"),
        s.binds.join(", ")
    )
}

fn statement_ui(
    ui: &mut Ui,
    rank: usize,
    idx: usize,
    s: &Statement,
    trace: &str,
    wants: &mut Wants,
) {
    let t = s.total();
    let first = s
        .text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    let headline = format!(
        "#{rank}  {}  ·  cpu {}  ·  {} exec  ·  {} rows  ·  {} waited",
        duration(t.elapsed as f64 / 1e6),
        duration(t.cpu as f64 / 1e6),
        s.call(CallKind::Execute).count,
        compact(t.rows as f64),
        duration(s.wait_total() as f64 / 1e6),
    );
    style::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::CollapsingHeader::new(RichText::new(headline).strong())
            .id_salt(("tkprof-stmt", idx))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if let Some(id) = &s.sql_id {
                        ui.label(RichText::new(format!("SQL_ID {id}")).monospace());
                    }
                    if let Some(og) = &s.optimizer_goal {
                        ui.weak(format!("optimizer {og}"));
                    }
                    if let Some(uid) = s.user_id {
                        ui.weak(format!("parsing user id {uid}"));
                    }
                    if s.is_recursive() {
                        ui.weak(format!("recursive depth {}", s.depth));
                    }
                    ui.weak(format!(
                        "misses in library cache: parse {} · execute {}",
                        s.call(CallKind::Parse).misses,
                        s.call(CallKind::Execute).misses
                    ));
                    if wants.analyze
                        && ui
                            .small_button("Analyze with AI")
                            .on_hover_text(
                                "Have the AI assistant explain why this statement is slow",
                            )
                            .clicked()
                    {
                        let what = match &s.sql_id {
                            Some(id) => format!("statement {id} from the trace file {trace}"),
                            None => format!("statement #{rank} from the trace file {trace}"),
                        };
                        wants.push(Want::Analyze(Analysis {
                            title: what,
                            sql: Some(s.text.clone()),
                            details: s.to_text(),
                            binds: if s.binds.is_empty() {
                                Vec::new()
                            } else {
                                vec![bind_line(s)]
                            },
                            ..Default::default()
                        }));
                    }
                });
                for e in &s.parse_errors {
                    ui.colored_label(ui.visuals().error_fg_color, format!("Parse error {e}"));
                }
                ui.add(egui::Label::new(RichText::new(&s.text).monospace()).wrap());
                if !s.binds.is_empty() {
                    ui.weak(format!("Binds (first execution): {}", s.binds.join(", ")));
                }
                ui.add_space(6.0);
                call_grid(ui, idx, &s.calls);
                if !s.row_source.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Row source operations").strong());
                    egui::Grid::new(("rowsource", idx))
                        .num_columns(2)
                        .spacing([12.0, 2.0])
                        .show(ui, |ui| {
                            ui.weak("Rows");
                            ui.weak("Operation");
                            ui.end_row();
                            for r in &s.row_source {
                                ui.label(RichText::new(compact(r.rows as f64)).monospace());
                                ui.label(
                                    RichText::new(format!(
                                        "{}{}",
                                        "  ".repeat(r.depth),
                                        r.operation
                                    ))
                                    .monospace(),
                                );
                                ui.end_row();
                            }
                        });
                }
                if !s.waits.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new("Waits").strong());
                    let mut waits = s.waits.clone();
                    waits.sort_by_key(|w| std::cmp::Reverse(w.1.total));
                    waits_grid(ui, &format!("waits-{idx}"), &waits);
                }
            });
        if !first.is_empty() {
            ui.add(
                egui::Label::new(
                    RichText::new(first)
                        .monospace()
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .truncate(),
            );
        }
    });
}

fn call_grid(ui: &mut Ui, idx: usize, calls: &[CallStats; 3]) {
    egui::Grid::new(("calls", idx))
        .num_columns(8)
        .striped(true)
        .spacing([18.0, 3.0])
        .show(ui, |ui| {
            for h in [
                "call", "count", "cpu", "elapsed", "disk", "query", "current", "rows",
            ] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            let mut total = CallStats::default();
            let row = |ui: &mut Ui, label: &str, c: &CallStats, strong: bool| {
                let cell = |t: String| {
                    if strong {
                        RichText::new(t).monospace().strong()
                    } else {
                        RichText::new(t).monospace()
                    }
                };
                ui.label(cell(label.to_owned()));
                ui.label(cell(c.count.to_string()));
                ui.label(cell(format!("{:.3}", c.cpu as f64 / 1e6)));
                ui.label(cell(format!("{:.3}", c.elapsed as f64 / 1e6)));
                ui.label(cell(c.disk.to_string()));
                ui.label(cell(c.query.to_string()));
                ui.label(cell(c.current.to_string()));
                ui.label(cell(c.rows.to_string()));
                ui.end_row();
            };
            for k in CallKind::ALL {
                let c = &calls[k as usize];
                total.count += c.count;
                total.cpu += c.cpu;
                total.elapsed += c.elapsed;
                total.disk += c.disk;
                total.query += c.query;
                total.current += c.current;
                total.rows += c.rows;
                row(ui, k.label(), c, false);
            }
            row(ui, "total", &total, true);
        });
}

fn waits_grid(ui: &mut Ui, id: &str, waits: &[(String, WaitStats)]) {
    let max_total = waits.iter().map(|(_, w)| w.total).max().unwrap_or(1).max(1) as f32;
    egui::Grid::new(id)
        .num_columns(4)
        .spacing([16.0, 3.0])
        .show(ui, |ui| {
            for h in ["Event", "Times waited", "Max wait", "Total waited"] {
                ui.label(RichText::new(h).strong());
            }
            ui.end_row();
            for (name, w) in waits.iter().take(25) {
                ui.label(name);
                ui.label(RichText::new(w.count.to_string()).monospace());
                ui.label(RichText::new(format!("{:.3}s", w.max as f64 / 1e6)).monospace());
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(
                        egui::vec2(80.0 * w.total as f32 / max_total + 1.0, 10.0),
                        egui::Sense::hover(),
                    );
                    ui.painter()
                        .rect_filled(r, 2.0, Palette::BLUE.gamma_multiply(0.8));
                    ui.label(RichText::new(format!("{:.3}s", w.total as f64 / 1e6)).monospace());
                });
                ui.end_row();
            }
        });
}
