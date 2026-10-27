//! Database dashboard: KPI tiles, trend charts, activity, memory, storage, top SQL.

use std::collections::VecDeque;

use egui::{RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{HLine, Legend, Line, Plot, PlotPoints};
use pythia_db::DbError;

use super::style::{self, Kpi, Palette, bytes, compact, duration, section};
use super::{Analysis, DetailTab, FieldFilter, Want, Wants, refresh_selector};
use crate::insights::{Insight, Severity, Target, insights};
use crate::queries::{
    Counters, DashboardSnapshot, EventRate, PgaUser, Rates, SystemEvent, TablespaceDetail, TopSql,
    event_rates, explain_error,
};

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

/// An event's latency histogram as read: (up to ms, waits), or why not.
type Histogram = Result<Vec<(f64, i64)>, String>;

/// What is unfolded on the dashboard, with its data once read (`None`
/// while it is being read).
#[derive(Default)]
struct Unfolded {
    /// An event and its latency histogram.
    histogram: Option<(String, Option<Histogram>)>,
    /// A tablespace and what it holds.
    tablespace: Option<(String, Option<Result<TablespaceDetail, String>>)>,
    /// The sessions using the most PGA.
    pga: Option<Option<Result<Vec<PgaUser>, String>>>,
}

#[derive(Default)]
pub struct DashboardView {
    pub refresh: Refresh,
    snapshot: Option<DashboardSnapshot>,
    prev: Option<(Counters, f64)>,
    rates: Option<Rates>,
    /// Wait events as of the snapshot before, and the rates since.
    prev_events: Option<(Vec<SystemEvent>, f64)>,
    event_rates: Option<Vec<EventRate>>,
    unfolded: Unfolded,
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
                if !too_close {
                    if let Some((prev, t)) = &self.prev_events {
                        self.event_rates = Some(event_rates(prev, &s.events, now - t));
                    }
                    self.prev_events = Some((s.events.clone(), now));
                }
                self.snapshot = Some(s);
            }
            Err(e) => self.error = Some(explain_error(&e)),
        }
    }

    /// The histogram of the event shown.
    pub(crate) fn on_histogram(&mut self, event: &str, result: Result<Vec<(f64, i64)>, DbError>) {
        if let Some((shown, data)) = &mut self.unfolded.histogram
            && shown == event
        {
            *data = Some(result.map_err(|e| explain_error(&e)));
        }
    }

    pub(crate) fn on_tablespace(&mut self, name: &str, result: Result<TablespaceDetail, DbError>) {
        if let Some((shown, data)) = &mut self.unfolded.tablespace
            && shown == name
        {
            *data = Some(result.map_err(|e| explain_error(&e)));
        }
    }

    pub(crate) fn on_pga_users(&mut self, result: Result<Vec<PgaUser>, DbError>) {
        if let Some(data) = &mut self.unfolded.pga {
            *data = Some(result.map_err(|e| explain_error(&e)));
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
                    let cpus = i.cpus.map(|n| format!(" · {n} CPUs")).unwrap_or_default();
                    ui.weak(format!(
                        "Oracle {} · up {} · {} · {}{container}{cpus}",
                        i.version,
                        duration(i.uptime_secs as f64),
                        i.role,
                        i.log_mode,
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

        // Taken out while the rest of the view is borrowed.
        let mut unfolded = std::mem::take(&mut self.unfolded);
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
                self.kpis(ui, snap, wants);
                ui.add_space(8.0);
                self.charts(ui, snap);
                ui.add_space(8.0);
                activity(ui, snap, wants);
                ui.add_space(8.0);
                top_events(
                    ui,
                    self.event_rates.as_deref(),
                    &mut unfolded.histogram,
                    wants,
                );
                ui.add_space(8.0);
                let mut field = None;
                ui.columns(3, |cols| {
                    let hint = Some("Show these sessions");
                    let by = [
                        (
                            &snap.by_user,
                            "Sessions by user",
                            FieldFilter::User as fn(String) -> FieldFilter,
                        ),
                        (
                            &snap.by_program,
                            "Sessions by program",
                            FieldFilter::Program,
                        ),
                        (
                            &snap.by_machine,
                            "Sessions by machine",
                            FieldFilter::Machine,
                        ),
                    ];
                    for (col, (rows, title, filter)) in cols.iter_mut().zip(by) {
                        if let Some(name) = top_list(col, title, rows, hint) {
                            field = Some(filter(name));
                        }
                    }
                });
                if let Some(field) = field {
                    wants.push(Want::ShowSessions(Some(field)));
                }
                ui.add_space(8.0);
                ui.columns(2, |cols| {
                    cursors(&mut cols[0], snap, wants);
                    memory(&mut cols[1], snap, &mut unfolded.pga, wants);
                });
                ui.add_space(8.0);
                tablespaces(ui, snap, &mut unfolded.tablespace, wants);
                ui.add_space(8.0);
                top_sql(ui, snap, wants);
                if !snap.long_ops.is_empty() {
                    ui.add_space(8.0);
                    long_ops(ui, snap, wants);
                }
                ui.add_space(8.0);
                style::section_errors(ui, &snap.errors);
            });
        self.unfolded = unfolded;
    }

    fn kpis(&self, ui: &mut Ui, snap: &DashboardSnapshot, wants: &mut Wants) {
        let r = self.rates;
        let cpus = snap
            .instance
            .as_ref()
            .and_then(|i| i.cpus)
            .map(|n| n.max(1) as f64);
        let rate = |f: fn(&Rates) -> f64| r.map_or("…".to_owned(), |r| compact(f(&r)));
        let mut tiles = Vec::new();
        if let Some(s) = snap.sessions {
            tiles.push(
                Kpi::new("Sessions", format!("{} / {}", s.active, s.user))
                    .detail(format!("active / user · {} background", s.background))
                    .link("Show the sessions"),
            );
            tiles.push(
                Kpi::new("Blocked sessions", s.blocked.to_string())
                    .detail("waiting on a lock")
                    .color((s.blocked > 0).then_some(Palette::RED))
                    .link("Show the blocking tree"),
            );
        }
        let aas = r.map_or(0.0, |r| r.aas);
        tiles.push(
            Kpi::new(
                "Average active sessions",
                r.map_or("…".into(), |r| format!("{:.2}", r.aas)),
            )
            .detail(cpus.map_or("CPUs unknown".into(), |n| format!("{n:.0} CPUs available")))
            .color(cpus.map(|n| style::level_color(ui, aas, n * 0.7, n))),
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
        match style::kpi_grid(ui, &tiles, 160.0).map(|i| tiles[i].caption.as_str()) {
            Some("Sessions") => wants.push(Want::ShowSessions(None)),
            Some("Blocked sessions") => wants.push(Want::ShowBlocking),
            _ => {}
        }
    }

    fn charts(&self, ui: &mut Ui, snap: &DashboardSnapshot) {
        let cpus = snap
            .instance
            .as_ref()
            .and_then(|i| i.cpus)
            .map(|n| n.max(1) as f64);
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
                if let Some(cpus) = cpus {
                    p.hline(HLine::new("CPUs", cpus).color(Palette::RED.gamma_multiply(0.6)));
                }
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

fn activity(ui: &mut Ui, snap: &DashboardSnapshot, wants: &mut Wants) {
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
        if let Some(i) = style::stacked_bar(ui, &segments, Some("Show these sessions")) {
            let class = segments[i].0.clone();
            wants.push(Want::ShowSessions(Some(FieldFilter::WaitClass(class))));
        }
    });
}

/// The events waited on most since the snapshot before; each leads to the
/// sessions waiting on it and shows its latency histogram.
fn top_events(
    ui: &mut Ui,
    rates: Option<&[EventRate]>,
    histogram: &mut Option<(String, Option<Histogram>)>,
    wants: &mut Wants,
) {
    section(ui, "Top wait events (since the refresh before)", |ui| {
        let Some(rates) = rates else {
            ui.weak("Measuring…");
            return;
        };
        if rates.is_empty() {
            ui.weak("No waits besides idle ones");
            return;
        }
        let max = rates.first().map_or(1.0, |r| r.load).max(0.000_001);
        for r in rates.iter().take(10) {
            ui.horizontal(|ui| {
                let shown = histogram.as_ref().is_some_and(|(e, _)| *e == r.event);
                if ui
                    .selectable_label(shown, "Histogram")
                    .on_hover_text("How long its waits took since startup")
                    .clicked()
                {
                    if shown {
                        *histogram = None;
                    } else {
                        *histogram = Some((r.event.clone(), None));
                        wants.push(Want::EventHistogram {
                            event: r.event.clone(),
                        });
                    }
                }
                let text = format!(
                    "{:.2}  {} · {} · {} waits/s · {:.2} ms avg",
                    r.load,
                    r.event,
                    r.wait_class,
                    compact(r.waits_per_s),
                    r.average_ms
                );
                let color = style::wait_class_color(&r.wait_class);
                if style::link_bar(
                    ui,
                    (r.load / max) as f32,
                    color,
                    text,
                    "Show the sessions waiting on it",
                ) {
                    wants.push(Want::ShowSessions(Some(FieldFilter::Event(
                        r.event.clone(),
                    ))));
                }
            });
            if let Some((event, data)) = histogram.as_ref()
                && *event == r.event
            {
                ui.indent(("event-histogram", event), |ui| match data {
                    None => {
                        ui.spinner();
                    }
                    Some(Err(e)) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    Some(Ok(buckets)) => {
                        let most = buckets.iter().map(|b| b.1).max().unwrap_or(1).max(1) as f32;
                        for (ms, n) in buckets {
                            style::usage_bar(
                                ui,
                                *n as f32 / most,
                                Palette::TEAL,
                                format!("≤ {ms} ms  {}", compact(*n as f64)),
                            );
                        }
                    }
                });
            }
            ui.add_space(2.0);
        }
    });
}

/// Counts per name as bars, largest first. With a `hint`, the bars are
/// links: returns the name clicked.
pub(super) fn top_list(
    ui: &mut Ui,
    title: &str,
    rows: &[(String, i64)],
    hint: Option<&str>,
) -> Option<String> {
    section(ui, title, |ui| {
        if rows.is_empty() {
            ui.weak("—");
        }
        let max = rows.iter().map(|r| r.1).max().unwrap_or(1).max(1) as f32;
        let mut clicked = None;
        for (name, n) in rows {
            let (fraction, text) = (*n as f32 / max, format!("{n}  {name}"));
            match hint {
                Some(hint) => {
                    if style::link_bar(ui, fraction, Palette::BLUE, text, hint) {
                        clicked = Some(name.clone());
                    }
                }
                None => style::usage_bar(ui, fraction, Palette::BLUE, text),
            }
            ui.add_space(2.0);
        }
        clicked
    })
}

fn cursors(ui: &mut Ui, snap: &DashboardSnapshot, wants: &mut Wants) {
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
            if style::link_bar(
                ui,
                frac,
                color,
                format!("{}  sid {} · {who} · {program}", c.open, c.sid),
                "Show the session's open cursors",
            ) {
                wants.push(Want::OpenDetail {
                    sid: c.sid,
                    serial: c.serial,
                    tab: Some(DetailTab::Cursors),
                });
            }
            ui.add_space(2.0);
        }
    });
}

fn memory(
    ui: &mut Ui,
    snap: &DashboardSnapshot,
    pga: &mut Option<Option<Result<Vec<PgaUser>, String>>>,
    wants: &mut Wants,
) {
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
        if style::link_bar(
            ui,
            frac as f32,
            Palette::ORANGE,
            format!("{} in use", bytes(m.pga_inuse)),
            "Show the sessions using the most",
        ) {
            if pga.is_some() {
                *pga = None;
            } else {
                *pga = Some(None);
                wants.push(Want::PgaUsers);
            }
        }
        if let Some(users) = pga {
            ui.indent("pga-users", |ui| match users {
                None => {
                    ui.spinner();
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                Some(Ok(users)) => {
                    for u in users {
                        let text = format!(
                            "{} of {}  sid {} · {} · {}",
                            bytes(u.used),
                            bytes(u.allocated),
                            u.sid,
                            u.user.as_deref().unwrap_or("(background)"),
                            u.program.as_deref().unwrap_or("")
                        );
                        if ui.link(text).on_hover_text("Show the session").clicked() {
                            wants.push(Want::OpenDetail {
                                sid: u.sid,
                                serial: u.serial,
                                tab: Some(DetailTab::Overview),
                            });
                        }
                    }
                }
            });
        }
    });
}

fn tablespaces(
    ui: &mut Ui,
    snap: &DashboardSnapshot,
    shown: &mut Option<(String, Option<Result<TablespaceDetail, String>>)>,
    wants: &mut Wants,
) {
    section(ui, "Tablespaces", |ui| {
        if snap.tablespaces.is_empty() {
            ui.weak("—");
            return;
        }
        for t in &snap.tablespaces {
            ui.horizontal(|ui| {
                let open = shown.as_ref().is_some_and(|(n, _)| *n == t.name);
                let name = ui
                    .add_sized(
                        [150.0, 16.0],
                        egui::Link::new(RichText::new(&t.name).monospace()),
                    )
                    .on_hover_text("Its files and largest segments, or who uses it");
                if name.clicked() {
                    if open {
                        *shown = None;
                    } else {
                        *shown = Some((t.name.clone(), None));
                        wants.push(Want::Tablespace {
                            name: t.name.clone(),
                        });
                    }
                }
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
            if let Some((name, data)) = shown.as_ref()
                && *name == t.name
            {
                ui.indent(("tablespace", name), |ui| {
                    tablespace(ui, data.as_ref(), wants)
                });
            }
            ui.add_space(2.0);
        }
    });
}

/// A tablespace's files, and its largest segments or who uses it.
fn tablespace(ui: &mut Ui, data: Option<&Result<TablespaceDetail, String>>, wants: &mut Wants) {
    let d = match data {
        None => {
            ui.spinner();
            return;
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return;
        }
        Some(Ok(d)) => d,
    };
    for f in &d.files {
        let grows = if f.autoextensible {
            format!(", grows to {}", bytes(f.max_bytes))
        } else {
            String::new()
        };
        ui.label(
            RichText::new(format!("{}  {}{grows}", f.name, bytes(f.bytes)))
                .monospace()
                .small(),
        );
    }
    if d.temporary {
        if d.temp_users.is_empty() {
            ui.weak("No session uses it now");
        }
        for (sid, serial, user, segment, b) in &d.temp_users {
            let text = format!(
                "{}  sid {sid} · {} · {}",
                bytes(*b),
                user.as_deref().unwrap_or("(background)"),
                segment.to_lowercase()
            );
            if ui.link(text).on_hover_text("Show the session").clicked() {
                wants.push(Want::OpenDetail {
                    sid: *sid,
                    serial: *serial,
                    tab: Some(DetailTab::Overview),
                });
            }
        }
    } else {
        let largest = d.segments.first().map_or(1.0, |s| s.2).max(1.0);
        for (name, kind, b) in &d.segments {
            style::usage_bar(
                ui,
                (*b / largest) as f32,
                Palette::BLUE,
                format!("{}  {name} · {}", bytes(*b), kind.to_lowercase()),
            );
        }
    }
}

fn top_sql(ui: &mut Ui, snap: &DashboardSnapshot, wants: &mut Wants) {
    section(ui, "Top SQL by elapsed time (since load)", |ui| {
        if snap.top_sql.is_empty() {
            ui.weak("—");
            return;
        }
        let analyze = wants.analyze;
        let h = super::style::row_height(ui);
        let mut table = TableBuilder::new(ui)
            .id_salt("top-sql")
            .striped(true)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(110.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(90.0))
            .column(Column::exact(80.0));
        if analyze {
            table = table.column(Column::exact(70.0));
        }
        table
            .column(Column::remainder().clip(true))
            .header(h, |mut r| {
                let mut titles = vec![
                    "SQL_ID",
                    "Execs",
                    "Elapsed",
                    "CPU",
                    "Buffer gets",
                    "Disk reads",
                ];
                if analyze {
                    titles.push("");
                }
                titles.push("SQL text");
                for t in titles {
                    r.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(h, snap.top_sql.len(), |mut row| {
                    let s = &snap.top_sql[row.index()];
                    row.col(|ui| {
                        let link = ui
                            .link(RichText::new(&s.sql_id).monospace())
                            .on_hover_text("Show the statement (right-click to copy)");
                        if link.clicked() {
                            wants.push(Want::OpenSqlDetail {
                                sql_id: s.sql_id.clone(),
                            });
                        }
                        link.context_menu(|ui| {
                            if ui.button("Copy SQL_ID").clicked() {
                                ui.ctx().copy_text(s.sql_id.clone());
                                ui.close();
                            }
                        });
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
                    if analyze {
                        row.col(|ui| {
                            if ui
                                .small_button("Analyze")
                                .on_hover_text("Have the AI assistant explain why it is expensive")
                                .clicked()
                            {
                                wants.push(Want::Analyze(top_sql_analysis(s)));
                            }
                        });
                    }
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

/// A top SQL statement and its cursor statistics, for the assistant.
fn top_sql_analysis(s: &TopSql) -> Analysis {
    let execs = s.executions.max(1) as f64;
    Analysis {
        title: format!("top SQL {}", s.sql_id),
        sql: Some(s.text.clone()),
        sql_id: Some(s.sql_id.clone()),
        details: format!(
            "Cursor statistics since it was loaded (V$SQL):\n\
             executions: {}\n\
             elapsed: {:.3} s ({:.4} s per execution)\n\
             CPU: {:.3} s\n\
             buffer gets: {} ({:.0} per execution)\n\
             disk reads: {} ({:.1} per execution)\n\
             rows processed: {} ({:.1} per execution)",
            s.executions,
            s.elapsed_s,
            s.elapsed_s / execs,
            s.cpu_s,
            s.buffer_gets,
            s.buffer_gets as f64 / execs,
            s.disk_reads,
            s.disk_reads as f64 / execs,
            s.rows,
            s.rows as f64 / execs,
        ),
        binds: Vec::new(),
    }
}

fn long_ops(ui: &mut Ui, snap: &DashboardSnapshot, wants: &mut Wants) {
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
            let text = format!(
                "sid {} · {} {}{remaining}",
                op.sid,
                op.opname,
                op.target.as_deref().unwrap_or("")
            );
            if ui.link(text).on_hover_text("Show the session").clicked() {
                wants.push(Want::OpenDetail {
                    sid: op.sid,
                    serial: op.serial,
                    tab: None,
                });
            }
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
pub(super) fn insights_strip(ui: &mut Ui, found: &[Insight], have_rates: bool, wants: &mut Wants) {
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
                                Target::Session { sid, serial } => Want::OpenDetail {
                                    sid,
                                    serial,
                                    tab: None,
                                },
                                Target::Sessions => Want::ShowSessions(None),
                            });
                        }
                    }
                    None => {
                        ui.label(text);
                    }
                }
                if let Some((sid, serial)) = i.session
                    && ui
                        .small_button("Details")
                        .on_hover_text("Show the session")
                        .clicked()
                {
                    wants.push(Want::OpenDetail {
                        sid,
                        serial,
                        tab: None,
                    });
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;
    use crate::queries::{Blocker, CursorHolder, LongOp, SessionCounts};

    fn snapshot() -> DashboardSnapshot {
        DashboardSnapshot {
            sessions: Some(SessionCounts {
                total: 40,
                user: 12,
                active: 3,
                inactive: 9,
                background: 28,
                blocked: 1,
            }),
            blockers: vec![Blocker {
                sid: 21,
                serial: 4,
                username: Some("SCOTT".into()),
                status: "INACTIVE".into(),
                blocked: 1,
                max_wait: 30.0,
            }],
            by_user: vec![("SCOTT".into(), 5), ("HR".into(), 2)],
            by_program: vec![("sqlplus".into(), 3)],
            by_machine: vec![("host1".into(), 7)],
            activity: vec![("User I/O".into(), 2), ("CPU".into(), 1)],
            open_cursors_limit: Some(300),
            cursor_holders: vec![CursorHolder {
                sid: 33,
                serial: 9,
                username: Some("APP".into()),
                program: Some("java".into()),
                open: 120,
            }],
            long_ops: vec![LongOp {
                sid: 44,
                serial: 2,
                opname: "Table Scan".into(),
                target: Some("APP.BIG".into()),
                sofar: 10.0,
                totalwork: 100.0,
                units: Some("Blocks".into()),
                elapsed_s: 20,
                remaining_s: Some(180),
                message: None,
            }],
            top_sql: vec![crate::queries::TopSql {
                sql_id: "7h35uxf5uhmm1".into(),
                executions: 10,
                elapsed_s: 2.0,
                cpu_s: 1.0,
                buffer_gets: 100,
                disk_reads: 1,
                rows: 10,
                text: "select 1 from dual".into(),
            }],
            tablespaces: vec![crate::queries::Tablespace {
                name: "USERS".into(),
                used_pct: 50.0,
                used_bytes: 5e6,
                max_bytes: 1e7,
            }],
            memory: Some(crate::queries::Memory {
                sga: vec![("buffer cache".into(), 1e8)],
                pga_allocated: 2e7,
                pga_inuse: 1e7,
                pga_target: 4e7,
            }),
            ..DashboardSnapshot::default()
        }
    }

    fn describe(w: &Want) -> String {
        match w {
            Want::ShowSessions(f) => format!("sessions {f:?}"),
            Want::ShowBlocking => "blocking tree".into(),
            Want::OpenDetail { sid, serial, tab } => format!("details {sid},{serial} {tab:?}"),
            Want::OpenSqlDetail { sql_id } => format!("sql {sql_id}"),
            Want::EventHistogram { event } => format!("histogram {event}"),
            Want::Tablespace { name } => format!("tablespace {name}"),
            Want::PgaUsers => "pga users".into(),
            _ => "other".into(),
        }
    }

    /// What clicking the element labeled `label` asks for.
    fn click(label: &str) -> Vec<String> {
        let mut view = DashboardView::default();
        // Two snapshots: rates between them.
        let event = |micros| crate::queries::SystemEvent {
            event: "log file sync".into(),
            wait_class: "Commit".into(),
            waits: micros / 1000.0,
            micros,
        };
        let first = DashboardSnapshot {
            events: vec![event(1e6)],
            ..snapshot()
        };
        view.on_snapshot(Ok(first), 0.0);
        let second = DashboardSnapshot {
            events: vec![event(3e6)],
            ..snapshot()
        };
        view.on_snapshot(Ok(second), 2.0);
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1400.0, 2400.0))
            .build_ui_state(
                |ui, (view, wants): &mut (DashboardView, Wants)| view.ui(ui, 0.0, wants),
                (view, Wants::default()),
            );
        harness.run_steps(2);
        harness.state_mut().1.list.clear();
        harness.get_by_label_contains(label).click();
        harness.run_steps(2);
        harness
            .state()
            .1
            .list
            .iter()
            .map(describe)
            .filter(|w| w != "other")
            .collect()
    }

    /// Tiles, bars and rows lead to the sessions behind them (#74).
    #[test]
    fn drill_down_links() {
        assert_eq!(click("active / user"), ["sessions None"]);
        assert_eq!(click("waiting on a lock"), ["blocking tree"]);
        assert_eq!(click("5  SCOTT"), [r#"sessions Some(User("SCOTT"))"#]);
        assert_eq!(
            click("3  sqlplus"),
            [r#"sessions Some(Program("sqlplus"))"#]
        );
        assert_eq!(click("7  host1"), [r#"sessions Some(Machine("host1"))"#]);
        assert_eq!(
            click("User I/O 2"),
            [r#"sessions Some(WaitClass("User I/O"))"#]
        );
        assert_eq!(click("120  sid 33"), ["details 33,9 Some(Cursors)"]);
        assert_eq!(click("sid 44 · Table Scan"), ["details 44,2 None"]);
        assert_eq!(click("Details"), ["details 21,4 None"]);
        assert_eq!(click("7h35uxf5uhmm1"), ["sql 7h35uxf5uhmm1"]);
        assert_eq!(
            click("log file sync · Commit"),
            [r#"sessions Some(Event("log file sync"))"#]
        );
        assert_eq!(click("Histogram"), ["histogram log file sync"]);
        assert_eq!(click("USERS"), ["tablespace USERS"]);
        assert_eq!(click("in use"), ["pga users"]);
    }
}
