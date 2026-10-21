//! Database dashboard: KPI tiles, trend charts, activity, memory, storage, top SQL.

use std::collections::VecDeque;

use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{HLine, Legend, Line, Plot, PlotPoints};
use orui_db::DbError;

use super::style::{self, Kpi, Palette, bytes, compact, duration, section};
use super::{Want, Wants, refresh_selector};
use crate::insights::{Insight, Severity, Target, insights};
use crate::queries::{Counters, DashboardSnapshot, Rates, explain_error};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Refresh {
    S2,
    #[default]
    S5,
    S10,
    S30,
    Paused,
}

impl Refresh {
    pub const ALL: [Refresh; 5] = [
        Refresh::S2,
        Refresh::S5,
        Refresh::S10,
        Refresh::S30,
        Refresh::Paused,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Refresh::S2 => "Every 2 s",
            Refresh::S5 => "Every 5 s",
            Refresh::S10 => "Every 10 s",
            Refresh::S30 => "Every 30 s",
            Refresh::Paused => "Paused",
        }
    }

    pub fn seconds(self) -> Option<f64> {
        match self {
            Refresh::S2 => Some(2.0),
            Refresh::S5 => Some(5.0),
            Refresh::S10 => Some(10.0),
            Refresh::S30 => Some(30.0),
            Refresh::Paused => None,
        }
    }

    /// Whether a refresh is due, and schedules a repaint for when it will be.
    pub(crate) fn due(self, ui: &Ui, now: f64, last: Option<f64>, in_flight: bool) -> bool {
        if in_flight {
            return false;
        }
        let Some(every) = self.seconds() else {
            return last.is_none();
        };
        match last {
            None => true,
            Some(last) if now - last >= every => true,
            Some(last) => {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(
                        every - (now - last),
                    ));
                false
            }
        }
    }
}

/// One point of the rolling history.
#[derive(Debug, Clone, Copy)]
struct Sample {
    t: f64,
    rates: Rates,
    active: f64,
    total: f64,
}

/// Keeps about an hour at 5 s refresh.
const HISTORY: usize = 720;

#[derive(Default)]
pub struct DashboardView {
    pub refresh: Refresh,
    snapshot: Option<DashboardSnapshot>,
    prev: Option<(Counters, f64)>,
    rates: Option<Rates>,
    history: VecDeque<Sample>,
    in_flight: bool,
    last_request: Option<f64>,
    error: Option<String>,
    start: Option<f64>,
}

impl DashboardView {
    pub(crate) fn on_snapshot(&mut self, result: Result<DashboardSnapshot, DbError>, now: f64) {
        self.in_flight = false;
        match result {
            Ok(s) => {
                self.error = None;
                // Samples closer than half a second give meaningless rates.
                let too_close = self.prev.is_some_and(|(_, t)| now - t < 0.5);
                if let Some(cur) = s.counters.filter(|_| !too_close) {
                    if let Some((prev, t)) = self.prev {
                        let rates = Rates::between(&prev, &cur, now - t);
                        self.rates = Some(rates);
                        let start = *self.start.get_or_insert(now);
                        let (active, total) = s
                            .sessions
                            .map_or((0.0, 0.0), |c| (c.active as f64, c.user as f64));
                        self.history.push_back(Sample {
                            t: now - start,
                            rates,
                            active,
                            total,
                        });
                        while self.history.len() > HISTORY {
                            self.history.pop_front();
                        }
                    }
                    self.prev = Some((cur, now));
                }
                self.snapshot = Some(s);
            }
            Err(e) => self.error = Some(explain_error(&e)),
        }
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, now: f64, wants: &mut Wants) {
        let mut refresh_now = false;
        ui.horizontal(|ui| {
            match self.snapshot.as_ref().and_then(|s| s.instance.as_ref()) {
                Some(i) => {
                    ui.label(RichText::new(format!("{} @ {}", i.instance, i.host)).strong());
                    let container = if i.cdb {
                        format!(" · container {}", i.container)
                    } else {
                        String::new()
                    };
                    ui.weak(format!(
                        "Oracle {} · up {} · {} · {}{container} · {} CPUs",
                        i.version,
                        duration(i.uptime_secs as f64),
                        i.role,
                        i.log_mode,
                        i.cpus
                    ));
                }
                None => {
                    ui.label(RichText::new("Dashboard").strong());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                refresh_selector(ui, "dashboard-refresh", &mut self.refresh);
                if ui.button("⟳").on_hover_text("Refresh now").clicked() {
                    refresh_now = true;
                }
                if self.in_flight {
                    ui.spinner();
                }
            });
        });
        if (refresh_now && !self.in_flight)
            || self.refresh.due(ui, now, self.last_request, self.in_flight)
        {
            self.in_flight = true;
            self.last_request = Some(now);
            wants.push(Want::Dashboard);
        }
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        let Some(snap) = &self.snapshot else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.weak("Collecting statistics…");
            });
            return;
        };

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                insights_strip(
                    ui,
                    &insights(snap, self.rates.as_ref()),
                    self.rates.is_some(),
                    wants,
                );
                ui.add_space(8.0);
                self.kpis(ui, snap);
                ui.add_space(8.0);
                self.charts(ui, snap);
                ui.add_space(8.0);
                activity(ui, snap);
                ui.add_space(8.0);
                ui.columns(3, |cols| {
                    top_list(&mut cols[0], "Sessions by user", &snap.by_user);
                    top_list(&mut cols[1], "Sessions by program", &snap.by_program);
                    top_list(&mut cols[2], "Sessions by machine", &snap.by_machine);
                });
                ui.add_space(8.0);
                ui.columns(2, |cols| {
                    cursors(&mut cols[0], snap);
                    memory(&mut cols[1], snap);
                });
                ui.add_space(8.0);
                tablespaces(ui, snap);
                ui.add_space(8.0);
                top_sql(ui, snap);
                if !snap.long_ops.is_empty() {
                    ui.add_space(8.0);
                    long_ops(ui, snap);
                }
                ui.add_space(8.0);
                style::section_errors(ui, &snap.errors);
            });
    }

    fn kpis(&self, ui: &mut Ui, snap: &DashboardSnapshot) {
        let r = self.rates;
        let cpus = snap.instance.as_ref().map_or(1, |i| i.cpus.max(1)) as f64;
        let rate = |f: fn(&Rates) -> f64| r.map_or("…".to_owned(), |r| compact(f(&r)));
        let mut tiles = Vec::new();
        if let Some(s) = snap.sessions {
            tiles.push(
                Kpi::new("Sessions", format!("{} / {}", s.active, s.user))
                    .detail(format!("active / user · {} background", s.background)),
            );
            tiles.push(
                Kpi::new("Blocked sessions", s.blocked.to_string())
                    .detail("waiting on a lock")
                    .color((s.blocked > 0).then_some(Palette::RED)),
            );
        }
        let aas = r.map_or(0.0, |r| r.aas);
        tiles.push(
            Kpi::new(
                "Average active sessions",
                r.map_or("…".into(), |r| format!("{:.2}", r.aas)),
            )
            .detail(format!("{cpus:.0} CPUs available"))
            .color(Some(style::level_color(ui, aas, cpus * 0.7, cpus))),
        );
        let host = r.and_then(|r| r.host_cpu_pct);
        tiles.push(
            Kpi::new(
                "Host CPU",
                host.map_or("n/a".into(), |p| format!("{p:.0} %")),
            )
            .detail(r.map_or(String::new(), |r| {
                format!("DB CPU {:.2} sessions", r.db_cpu)
            }))
            .color(host.map(|h| style::level_color(ui, h, 70.0, 90.0))),
        );
        if let Some(open) = snap.open_cursors {
            let mut t = Kpi::new("Open cursors", compact(open as f64));
            if let Some(l) = snap.open_cursors_limit {
                t = t.detail(format!("limit {l} per session"));
            }
            tiles.push(t);
        }
        tiles.push(
            Kpi::new("Executions / s", rate(|r| r.executions))
                .detail(format!("{} user calls / s", rate(|r| r.user_calls))),
        );
        tiles.push(Kpi::new("Logical reads / s", rate(|r| r.logical_reads)));
        tiles.push(Kpi::new("Physical reads / s", rate(|r| r.physical_reads)));
        tiles.push(Kpi::new(
            "Redo / s",
            r.map_or("…".into(), |r| bytes(r.redo_bytes)),
        ));
        tiles.push(
            Kpi::new("Commits / s", rate(|r| r.commits))
                .detail(format!("{} rollbacks / s", rate(|r| r.rollbacks))),
        );
        tiles.push(Kpi::new("Hard parses / s", rate(|r| r.hard_parses)));
        style::kpi_grid(ui, &tiles, 160.0);
    }

    fn charts(&self, ui: &mut Ui, snap: &DashboardSnapshot) {
        let cpus = snap.instance.as_ref().map_or(1, |i| i.cpus.max(1)) as f64;
        let series = |f: &dyn Fn(&Sample) -> Option<f64>| -> PlotPoints<'static> {
            self.history
                .iter()
                .filter_map(|s| f(s).map(|v| [s.t, v]))
                .collect::<Vec<_>>()
                .into()
        };
        let chart = |ui: &mut Ui, title: &str, id: &str, add: &dyn Fn(&mut egui_plot::PlotUi)| {
            section(ui, title, |ui| {
                if self.history.len() < 2 {
                    ui.add_sized(
                        [ui.available_width(), 140.0],
                        egui::Label::new(RichText::new("Collecting samples…").weak()),
                    );
                    return;
                }
                Plot::new(id)
                    .height(140.0)
                    .allow_drag(false)
                    .allow_zoom(false)
                    .allow_scroll(false)
                    .allow_boxed_zoom(false)
                    .include_y(0.0)
                    .y_axis_min_width(34.0)
                    .show_x(false)
                    .legend(Legend::default())
                    .x_axis_formatter(|m, _| duration(m.value))
                    .show(ui, |p| add(p));
            });
        };
        ui.columns(2, |cols| {
            chart(&mut cols[0], "Average active sessions", "dash-aas", &|p| {
                p.line(
                    Line::new("DB time", series(&|s| Some(s.rates.aas)))
                        .color(Palette::BLUE)
                        .fill(0.0)
                        .fill_alpha(0.15),
                );
                p.line(
                    Line::new("DB CPU", series(&|s| Some(s.rates.db_cpu))).color(Palette::GREEN),
                );
                p.hline(HLine::new("CPUs", cpus).color(Palette::RED.gamma_multiply(0.6)));
            });
            chart(&mut cols[1], "Host CPU %", "dash-cpu", &|p| {
                p.line(
                    Line::new("Host CPU", series(&|s| s.rates.host_cpu_pct))
                        .color(Palette::ORANGE)
                        .fill(0.0)
                        .fill_alpha(0.15),
                );
            });
        });
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            chart(&mut cols[0], "Sessions", "dash-sessions", &|p| {
                p.line(Line::new("User", series(&|s| Some(s.total))).color(Palette::PURPLE));
                p.line(
                    Line::new("Active", series(&|s| Some(s.active)))
                        .color(Palette::TEAL)
                        .fill(0.0)
                        .fill_alpha(0.15),
                );
            });
            chart(&mut cols[1], "Throughput / s", "dash-throughput", &|p| {
                p.line(
                    Line::new("Executions", series(&|s| Some(s.rates.executions)))
                        .color(Palette::BLUE),
                );
                p.line(
                    Line::new("User calls", series(&|s| Some(s.rates.user_calls)))
                        .color(Palette::PURPLE),
                );
                p.line(
                    Line::new("Commits", series(&|s| Some(s.rates.commits))).color(Palette::GREEN),
                );
            });
        });
    }
}

fn activity(ui: &mut Ui, snap: &DashboardSnapshot) {
    section(ui, "Active sessions by wait class (now)", |ui| {
        if snap.activity.is_empty() {
            ui.weak("No active user sessions besides this one.");
            return;
        }
        let segments: Vec<(String, f64, egui::Color32)> = snap
            .activity
            .iter()
            .map(|(c, n)| (c.clone(), *n as f64, style::wait_class_color(c)))
            .collect();
        style::stacked_bar(ui, &segments);
    });
}

fn top_list(ui: &mut Ui, title: &str, rows: &[(String, i64)]) {
    section(ui, title, |ui| {
        if rows.is_empty() {
            ui.weak("—");
        }
        let max = rows.iter().map(|r| r.1).max().unwrap_or(1).max(1) as f32;
        for (name, n) in rows {
            style::usage_bar(ui, *n as f32 / max, Palette::BLUE, format!("{n}  {name}"));
            ui.add_space(2.0);
        }
    });
}

fn cursors(ui: &mut Ui, snap: &DashboardSnapshot) {
    section(ui, "Most open cursors", |ui| {
        if snap.cursor_holders.is_empty() {
            ui.weak("—");
            return;
        }
        let limit = snap.open_cursors_limit.unwrap_or(0).max(1) as f32;
        let max = snap
            .cursor_holders
            .iter()
            .map(|c| c.open)
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        for c in &snap.cursor_holders {
            let who = c.username.as_deref().unwrap_or("(background)");
            let program = c.program.as_deref().unwrap_or("");
            let frac = c.open as f32
                / if snap.open_cursors_limit.is_some() {
                    limit
                } else {
                    max
                };
            let color = if frac > 0.85 {
                Palette::RED
            } else if frac > 0.6 {
                Palette::ORANGE
            } else {
                Palette::TEAL
            };
            style::usage_bar(
                ui,
                frac,
                color,
                format!("{}  sid {} · {who} · {program}", c.open, c.sid),
            );
            ui.add_space(2.0);
        }
    });
}

fn memory(ui: &mut Ui, snap: &DashboardSnapshot) {
    section(ui, "Memory", |ui| {
        let Some(m) = &snap.memory else {
            ui.weak("—");
            return;
        };
        let sga: f64 = m.sga.iter().map(|s| s.1).sum();
        ui.label(format!("SGA {}", bytes(sga)));
        for (name, v) in &m.sga {
            style::usage_bar(
                ui,
                (*v / sga.max(1.0)) as f32,
                Palette::PURPLE,
                format!("{}  {name}", bytes(*v)),
            );
            ui.add_space(2.0);
        }
        ui.add_space(4.0);
        let target = if m.pga_target > 0.0 {
            format!(" of target {}", bytes(m.pga_target))
        } else {
            String::new()
        };
        ui.label(format!("PGA allocated {}{target}", bytes(m.pga_allocated)));
        let frac = if m.pga_target > 0.0 {
            m.pga_allocated / m.pga_target
        } else {
            m.pga_inuse / m.pga_allocated.max(1.0)
        };
        style::usage_bar(
            ui,
            frac as f32,
            Palette::ORANGE,
            format!("{} in use", bytes(m.pga_inuse)),
        );
    });
}

fn tablespaces(ui: &mut Ui, snap: &DashboardSnapshot) {
    section(ui, "Tablespaces", |ui| {
        if snap.tablespaces.is_empty() {
            ui.weak("—");
            return;
        }
        for t in &snap.tablespaces {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [150.0, 16.0],
                    egui::Label::new(RichText::new(&t.name).monospace()).truncate(),
                );
                let color = if t.used_pct >= 90.0 {
                    Palette::RED
                } else if t.used_pct >= 75.0 {
                    Palette::ORANGE
                } else {
                    Palette::GREEN
                };
                style::usage_bar(
                    ui,
                    (t.used_pct / 100.0) as f32,
                    color,
                    format!(
                        "{:.1} %  ·  {} of {}",
                        t.used_pct,
                        bytes(t.used_bytes),
                        bytes(t.max_bytes)
                    ),
                );
            });
            ui.add_space(2.0);
        }
    });
}

fn top_sql(ui: &mut Ui, snap: &DashboardSnapshot) {
    section(ui, "Top SQL by elapsed time (since load)", |ui| {
        if snap.top_sql.is_empty() {
            ui.weak("—");
            return;
        }
        let h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        TableBuilder::new(ui)
            .id_salt("top-sql")
            .striped(true)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(110.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(90.0))
            .column(Column::exact(80.0))
            .column(Column::remainder().clip(true))
            .header(h, |mut r| {
                for t in [
                    "SQL_ID",
                    "Execs",
                    "Elapsed",
                    "CPU",
                    "Buffer gets",
                    "Disk reads",
                    "SQL text",
                ] {
                    r.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(h, snap.top_sql.len(), |mut row| {
                    let s = &snap.top_sql[row.index()];
                    row.col(|ui| {
                        if ui
                            .add(
                                egui::Label::new(RichText::new(&s.sql_id).monospace())
                                    .sense(egui::Sense::click()),
                            )
                            .on_hover_text("Click to copy")
                            .clicked()
                        {
                            ui.ctx().copy_text(s.sql_id.clone());
                        }
                    });
                    let right = |ui: &mut Ui, t: String| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(t)
                        });
                    };
                    row.col(|ui| right(ui, compact(s.executions as f64)));
                    row.col(|ui| right(ui, duration(s.elapsed_s)));
                    row.col(|ui| right(ui, duration(s.cpu_s)));
                    row.col(|ui| right(ui, compact(s.buffer_gets as f64)));
                    row.col(|ui| right(ui, compact(s.disk_reads as f64)));
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(RichText::new(s.text.replace('\n', " ")).monospace())
                                .truncate(),
                        )
                        .on_hover_text(&s.text);
                    });
                });
            });
    });
}

fn long_ops(ui: &mut Ui, snap: &DashboardSnapshot) {
    section(ui, "Long operations", |ui| {
        for op in &snap.long_ops {
            let frac = if op.totalwork > 0.0 {
                op.sofar / op.totalwork
            } else {
                0.0
            };
            let remaining = op
                .remaining_s
                .map(|s| format!(" · {} left", duration(s as f64)))
                .unwrap_or_default();
            ui.label(format!(
                "sid {} · {} {}{remaining}",
                op.sid,
                op.opname,
                op.target.as_deref().unwrap_or("")
            ));
            style::usage_bar(
                ui,
                frac as f32,
                Palette::BLUE,
                format!(
                    "{:.0} %  {}",
                    frac * 100.0,
                    op.message.as_deref().unwrap_or("")
                ),
            );
            ui.add_space(4.0);
        }
    });
}

/// "What needs attention" — findings first, so the dashboard answers the
/// question before showing the numbers.
fn insights_strip(ui: &mut Ui, found: &[Insight], have_rates: bool, wants: &mut Wants) {
    if found.is_empty() {
        let text = if have_rates {
            "All clear — no blocking, load and storage look healthy"
        } else {
            "Checking…"
        };
        style::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("✔  {text}")).color(Palette::GREEN));
        });
        return;
    }
    style::card(ui).show(ui, |ui| {
        ui.set_width(ui.available_width());
        for i in found {
            let color = match i.severity {
                Severity::Critical => Palette::RED,
                Severity::Warning => Palette::ORANGE,
                Severity::Info => Palette::BLUE,
            };
            ui.horizontal(|ui| {
                let (bar, _) = ui.allocate_exact_size(egui::vec2(4.0, 18.0), egui::Sense::hover());
                ui.painter().rect_filled(bar, 2.0, color);
                let text = RichText::new(&i.text).color(if i.severity == Severity::Info {
                    ui.visuals().text_color()
                } else {
                    color
                });
                match i.target {
                    Some(target) => {
                        if ui.link(text).on_hover_text("Show details").clicked() {
                            wants.push(match target {
                                Target::BlockingTree => Want::ShowBlocking,
                                Target::Session { sid, serial } => Want::OpenDetail { sid, serial },
                                Target::Sessions => Want::ShowSessions,
                            });
                        }
                    }
                    None => {
                        ui.label(text);
                    }
                }
            });
        }
    });
}
