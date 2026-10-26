//! The SQL Server dashboard: findings, KPI tiles, trend charts, sessions,
//! blocking and transactions, top waits, storage and top statements.

use std::collections::VecDeque;

use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use pythia_db::DbError;

use super::dashboard::{Refresh, insights_strip, top_list};
use super::style::{self, Kpi, Palette, bytes, compact, duration, section};
use super::{Analysis, Want, Wants, refresh_selector};
use crate::queries::{EventRate, SystemEvent, event_rates};
use crate::sqlserver::{Counters, MsSnapshot, SERVER_STATE, Storage, TopStatement, insights};

/// Sizes are read this often (seconds): they cost more than the rest on
/// servers with many databases.
const STORAGE_EVERY: f64 = 60.0;

/// Keeps about an hour at 5 s refresh.
const HISTORY: usize = 720;

/// One point of the rolling history.
#[derive(Debug, Clone, Copy)]
struct Sample {
    t: f64,
    rates: Counters,
    connections: f64,
    requests: f64,
}

#[derive(Default)]
pub struct SqlServerDashboardView {
    pub refresh: Refresh,
    snapshot: Option<MsSnapshot>,
    /// The last sizes read.
    storage: Option<Storage>,
    storage_requested: Option<f64>,
    prev: Option<(Counters, f64)>,
    rates: Option<Counters>,
    /// Waits since startup at the last snapshot, and when.
    prev_waits: Option<(Vec<SystemEvent>, f64)>,
    wait_rates: Option<Vec<EventRate>>,
    history: VecDeque<Sample>,
    in_flight: bool,
    last_request: Option<f64>,
    error: Option<String>,
    start: Option<f64>,
}

impl SqlServerDashboardView {
    pub(crate) fn on_snapshot(&mut self, result: Result<MsSnapshot, DbError>, now: f64) {
        self.in_flight = false;
        let mut s = match result {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(e.message);
                return;
            }
        };
        self.error = None;
        // Samples closer than half a second give meaningless rates.
        let too_close = self.prev.is_some_and(|(_, t)| now - t < 0.5);
        if let Some(cur) = s.counters.filter(|_| !too_close) {
            if let Some((prev, t)) = self.prev {
                let rates = Counters::per_second(&prev, &cur, now - t);
                self.rates = Some(rates);
                let start = *self.start.get_or_insert(now);
                self.history.push_back(Sample {
                    t: now - start,
                    rates,
                    connections: s.gauges.map_or(0.0, |g| g.user_connections as f64),
                    requests: s.activity.iter().map(|(_, n)| *n as f64).sum(),
                });
                while self.history.len() > HISTORY {
                    self.history.pop_front();
                }
            }
            self.prev = Some((cur, now));
        }
        if !s.waits.is_empty() && !too_close {
            if let Some((prev, t)) = &self.prev_waits {
                self.wait_rates = Some(event_rates(prev, &s.waits, now - t));
            }
            self.prev_waits = Some((std::mem::take(&mut s.waits), now));
        }
        if let Some(storage) = s.storage.take() {
            self.storage = Some(storage);
        }
        self.snapshot = Some(s);
    }

    pub(crate) fn ui(&mut self, ui: &mut Ui, now: f64, wants: &mut Wants) {
        let mut refresh_now = false;
        ui.horizontal(|ui| {
            match self.snapshot.as_ref().and_then(|s| s.server.as_ref()) {
                Some(server) => {
                    ui.label(RichText::new(&server.name).strong());
                    let machine = match (server.cpus, server.memory_bytes) {
                        (Some(cpus), Some(memory)) => {
                            format!(" · {cpus} CPUs · {}", bytes(memory))
                        }
                        _ => String::new(),
                    };
                    ui.weak(format!(
                        "{} {} {} · {} · up {}{machine}",
                        server.product,
                        server.version,
                        server.level,
                        server.edition,
                        duration(server.uptime_secs),
                    ));
                }
                None => {
                    ui.label(RichText::new("Dashboard").strong());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                refresh_selector(ui, "sqlserver-dashboard-refresh", &mut self.refresh);
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
            let storage = refresh_now
                || self
                    .storage_requested
                    .is_none_or(|t| now - t >= STORAGE_EVERY);
            if storage {
                self.storage_requested = Some(now);
            }
            self.in_flight = true;
            self.last_request = Some(now);
            wants.push(Want::SqlServerDashboard { storage });
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
                if snap.own_sessions_only {
                    ui.add_space(4.0);
                    ui.weak(format!(
                        "Without {SERVER_STATE} (on SQL Server 2022, VIEW SERVER PERFORMANCE \
                         STATE is enough) only this login's own sessions are listed, and the \
                         server's counters, requests, transactions, waits and statement \
                         statistics can't be read."
                    ));
                }
                ui.add_space(8.0);
                if snap.counters.is_some() {
                    self.kpis(ui, snap);
                    ui.add_space(8.0);
                    self.charts(ui);
                    ui.add_space(8.0);
                }
                if !snap.own_sessions_only {
                    activity(ui, snap);
                    ui.add_space(8.0);
                }
                ui.columns(4, |cols| {
                    top_list(&mut cols[0], "Sessions by login", &snap.by_login, None);
                    top_list(&mut cols[1], "Sessions by host", &snap.by_host, None);
                    top_list(&mut cols[2], "Sessions by program", &snap.by_program, None);
                    top_list(
                        &mut cols[3],
                        "Sessions by database",
                        &snap.by_database,
                        None,
                    );
                });
                if !snap.blocked.is_empty() {
                    ui.add_space(8.0);
                    blocking(ui, snap);
                }
                if !snap.own_sessions_only {
                    ui.add_space(8.0);
                    running(ui, snap);
                }
                if !snap.transactions.is_empty() {
                    ui.add_space(8.0);
                    transactions(ui, snap);
                }
                if !snap.own_sessions_only {
                    ui.add_space(8.0);
                    top_waits(ui, self.wait_rates.as_deref());
                }
                if let Some(storage) = &self.storage {
                    ui.add_space(8.0);
                    ui.columns(2, |cols| {
                        databases(&mut cols[0], storage, &snap.log_used);
                        tables(&mut cols[1], storage);
                    });
                }
                if !snap.top_statements.is_empty() {
                    ui.add_space(8.0);
                    top_statements(ui, snap, wants);
                }
                ui.add_space(8.0);
                style::section_errors(ui, &snap.errors);
            });
    }

    fn kpis(&self, ui: &mut Ui, snap: &MsSnapshot) {
        let r = self.rates;
        let rate = |f: fn(&Counters) -> f64| r.map_or("…".to_owned(), |r| compact(f(&r)));
        let mut tiles = Vec::new();
        if let Some(g) = snap.gauges {
            let running: i64 = snap.activity.iter().map(|(_, n)| n).sum();
            tiles.push(
                Kpi::new("Connections", g.user_connections.to_string()).detail(format!(
                    "{running} running requests · {} sessions",
                    snap.sessions.unwrap_or(0)
                )),
            );
        }
        tiles.push(
            Kpi::new("Batch requests / s", rate(|r| r.batch_requests)).detail(format!(
                "{} compilations · {} recompilations / s",
                rate(|r| r.compilations),
                rate(|r| r.recompilations)
            )),
        );
        tiles.push(
            Kpi::new("Transactions / s", rate(|r| r.transactions))
                .detail(format!("{} writing / s", rate(|r| r.write_transactions))),
        );
        let hit = r
            .and_then(|r| r.hit_ratio())
            .or_else(|| snap.gauges.and_then(|g| g.buffer_hit_ratio));
        tiles.push(
            Kpi::new(
                "Buffer cache hit ratio",
                match (hit, r) {
                    (Some(hit), _) => format!("{hit:.1} %"),
                    (None, Some(_)) => "idle".into(),
                    (None, None) => "…".into(),
                },
            )
            .detail(format!("{} page reads / s", rate(|r| r.page_reads)))
            .color(hit.map(|h| style::level_color(ui, 100.0 - h, 5.0, 10.0))),
        );
        if let Some(g) = snap.gauges {
            tiles.push(
                Kpi::new("Page life expectancy", duration(g.page_life_expectancy))
                    .detail(format!(
                        "memory {} of {}",
                        bytes(g.total_memory_bytes),
                        bytes(g.target_memory_bytes)
                    ))
                    // Low is bad: under 5 minutes, under a minute.
                    .color(
                        (g.page_life_expectancy > 0.0).then(|| {
                            style::level_color(ui, -g.page_life_expectancy, -300.0, -60.0)
                        }),
                    ),
            );
        }
        tiles.push(
            Kpi::new("Lock waits / s", rate(|r| r.lock_waits)).detail(format!(
                "{} blocked now · {} deadlocks / s",
                snap.blocked.len(),
                rate(|r| r.deadlocks)
            )),
        );
        tiles.push(
            Kpi::new("Logical reads / s", rate(|r| r.page_lookups))
                .detail(format!("{} page writes / s", rate(|r| r.page_writes))),
        );
        tiles.push(
            Kpi::new(
                "Log flushed / s",
                r.map_or("…".into(), |r| bytes(r.log_bytes)),
            )
            .detail(format!("{} flushes / s", rate(|r| r.log_flushes))),
        );
        tiles.push(
            Kpi::new("Full scans / s", rate(|r| r.full_scans))
                .detail(format!("{} index searches / s", rate(|r| r.index_searches))),
        );
        let open = snap
            .open_transactions
            .map(|n| n.to_string())
            .unwrap_or_else(|| "—".into());
        tiles.push(Kpi::new("Open transactions", open).detail(
            snap.transactions.first().map_or("none".into(), |t| {
                format!("oldest {}", duration(t.age_secs as f64))
            }),
        ));
        if let Some(g) = snap.gauges {
            tiles.push(
                Kpi::new("Memory grants pending", g.memory_grants_pending.to_string())
                    .color((g.memory_grants_pending > 0).then_some(Palette::ORANGE)),
            );
        }
        tiles.push(
            Kpi::new("Errors / s", rate(|r| r.errors))
                .detail(format!("{} logins / s", rate(|r| r.logins))),
        );
        style::kpi_grid(ui, &tiles, 160.0);
    }

    fn charts(&self, ui: &mut Ui) {
        let series = |f: &dyn Fn(&Sample) -> f64| -> PlotPoints<'static> {
            self.history
                .iter()
                .map(|s| [s.t, f(s)])
                .collect::<Vec<_>>()
                .into()
        };
        let chart = |ui: &mut Ui,
                     title: &str,
                     id: &str,
                     unit: fn(f64) -> String,
                     add: &dyn Fn(&mut egui_plot::PlotUi)| {
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
                    .y_axis_min_width(40.0)
                    .show_x(false)
                    .legend(Legend::default())
                    .x_axis_formatter(|m, _| duration(m.value))
                    .y_axis_formatter(move |m, _| unit(m.value))
                    .show(ui, |p| add(p));
            });
        };
        ui.columns(2, |cols| {
            chart(
                &mut cols[0],
                "Batch requests / s",
                "sqlserver-batches",
                compact,
                &|p| {
                    p.line(
                        Line::new("Batches", series(&|s| s.rates.batch_requests))
                            .color(Palette::BLUE)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                    p.line(
                        Line::new("Transactions", series(&|s| s.rates.transactions))
                            .color(Palette::GREEN),
                    );
                    p.line(
                        Line::new("Compilations", series(&|s| s.rates.compilations))
                            .color(Palette::ORANGE),
                    );
                },
            );
            chart(
                &mut cols[1],
                "Connections",
                "sqlserver-connections",
                compact,
                &|p| {
                    p.line(
                        Line::new("Connected", series(&|s| s.connections)).color(Palette::PURPLE),
                    );
                    p.line(
                        Line::new("Running", series(&|s| s.requests))
                            .color(Palette::TEAL)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                },
            );
        });
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            chart(
                &mut cols[0],
                "Pages / s",
                "sqlserver-pages",
                compact,
                &|p| {
                    p.line(
                        Line::new("Logical reads", series(&|s| s.rates.page_lookups))
                            .color(Palette::BLUE)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                    p.line(
                        Line::new("Disk reads", series(&|s| s.rates.page_reads))
                            .color(Palette::ORANGE),
                    );
                    p.line(
                        Line::new("Writes", series(&|s| s.rates.page_writes))
                            .color(Palette::PURPLE),
                    );
                },
            );
            chart(
                &mut cols[1],
                "Log flushed / s",
                "sqlserver-log",
                bytes,
                &|p| {
                    p.line(
                        Line::new("Log", series(&|s| s.rates.log_bytes))
                            .color(Palette::GREEN)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                },
            );
        });
    }
}

/// Colors wait classes like Oracle's: running, waiting for a lock, on
/// disk or the log, waiting for something else.
fn class_color(class: &str) -> Color32 {
    match class {
        "CPU" => style::wait_class_color("CPU"),
        "Lock" => Palette::RED,
        "Disk I/O" => style::wait_class_color("User I/O"),
        "Log" => style::wait_class_color("Commit"),
        "Latch" => style::wait_class_color("Concurrency"),
        "Network" => style::wait_class_color("Network"),
        "Parallelism" => Palette::TEAL,
        "Memory" => Palette::ORANGE,
        _ => Palette::PURPLE,
    }
}

fn activity(ui: &mut Ui, snap: &MsSnapshot) {
    section(ui, "Running requests by wait (now)", |ui| {
        if snap.activity.is_empty() {
            ui.weak("No requests running besides the dashboard's.");
            return;
        }
        let segments: Vec<(String, f64, Color32)> = snap
            .activity
            .iter()
            .map(|(class, n)| (class.clone(), *n as f64, class_color(class)))
            .collect();
        style::stacked_bar(ui, &segments, None);
    });
}

fn blocking(ui: &mut Ui, snap: &MsSnapshot) {
    section(ui, "Blocking", |ui| {
        let rows: Vec<Vec<String>> = snap
            .blocked
            .iter()
            .map(|b| {
                vec![
                    b.session.to_string(),
                    duration(b.wait_secs),
                    b.wait_type.clone().unwrap_or_default(),
                    b.blocker.to_string(),
                    b.resource.clone().unwrap_or_default(),
                    match (&b.blocker_statement, b.blocker_idle, &b.blocker_last) {
                        (Some(s), ..) => s.clone(),
                        (None, true, Some(last)) => format!("(idle in transaction) last: {last}"),
                        (None, true, None) => "(idle in transaction)".into(),
                        (None, false, last) => last.clone().unwrap_or_default(),
                    },
                    b.statement.clone().unwrap_or_default(),
                ]
            })
            .collect();
        style::text_table(
            ui,
            "sqlserver-blocking",
            &[
                ("Waiting", 70.0, true),
                ("For", 70.0, true),
                ("Wait", 90.0, false),
                ("Blocked by", 80.0, true),
                ("Resource", 160.0, false),
                ("Blocker runs", 220.0, false),
                ("Waiting statement", 0.0, false),
            ],
            &rows,
            &[],
            "",
        );
    });
}

fn running(ui: &mut Ui, snap: &MsSnapshot) {
    section(ui, "Running requests, longest first", |ui| {
        if snap.running.is_empty() {
            ui.weak("—");
            return;
        }
        let rows: Vec<Vec<String>> = snap
            .running
            .iter()
            .map(|r| {
                vec![
                    r.session.to_string(),
                    r.login.clone(),
                    r.database.clone().unwrap_or_default(),
                    duration(r.elapsed_secs),
                    match &r.wait_type {
                        Some(w) => format!("{w} ({})", duration(r.wait_secs)),
                        None => r.status.clone(),
                    },
                    r.command.clone(),
                    r.statement.clone().unwrap_or_default(),
                ]
            })
            .collect();
        style::text_table(
            ui,
            "sqlserver-running",
            &[
                ("Session", 70.0, true),
                ("Login", 110.0, false),
                ("Database", 110.0, false),
                ("Time", 70.0, true),
                ("Waiting for", 180.0, false),
                ("Command", 90.0, false),
                ("Statement", 0.0, false),
            ],
            &rows,
            &[],
            "",
        );
    });
}

fn transactions(ui: &mut Ui, snap: &MsSnapshot) {
    let title = match snap.open_transactions {
        Some(n) if n as usize > snap.transactions.len() => {
            format!("Open transactions, oldest first ({n} in all)")
        }
        _ => "Open transactions, oldest first".to_owned(),
    };
    section(ui, &title, |ui| {
        let rows: Vec<Vec<String>> = snap
            .transactions
            .iter()
            .map(|t| {
                vec![
                    t.session.to_string(),
                    t.login.clone(),
                    duration(t.age_secs as f64),
                    t.name.clone(),
                    bytes(t.log_bytes),
                    match (&t.statement, t.idle) {
                        (Some(s), _) => s.clone(),
                        (None, true) => "(idle)".into(),
                        (None, false) => String::new(),
                    },
                ]
            })
            .collect();
        style::text_table(
            ui,
            "sqlserver-transactions",
            &[
                ("Session", 70.0, true),
                ("Login", 110.0, false),
                ("Open for", 80.0, true),
                ("Name", 150.0, false),
                ("Log used", 80.0, true),
                ("Statement", 0.0, false),
            ],
            &rows,
            &[],
            "",
        );
    });
}

/// The waits that took the most time since the snapshot before.
fn top_waits(ui: &mut Ui, rates: Option<&[EventRate]>) {
    section(ui, "Top waits (since the refresh before)", |ui| {
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
            style::usage_bar(
                ui,
                (r.load / max) as f32,
                class_color(&r.wait_class),
                format!(
                    "{:.2}  {} · {} · {} waits/s · {:.2} ms avg",
                    r.load,
                    r.event,
                    r.wait_class,
                    compact(r.waits_per_s),
                    r.average_ms
                ),
            );
            ui.add_space(2.0);
        }
    });
}

fn databases(ui: &mut Ui, storage: &Storage, log_used: &[(String, f64)]) {
    section(ui, "Largest databases", |ui| {
        if storage.databases.is_empty() {
            ui.weak("—");
            return;
        }
        let size = |d: &crate::sqlserver::DatabaseSize| d.data_bytes + d.log_bytes;
        let max = storage.databases.iter().map(size).fold(1.0, f64::max);
        for d in &storage.databases {
            let used = log_used
                .iter()
                .find(|(name, _)| *name == d.name)
                .map(|(_, pct)| format!(" ({pct:.0} % used)"))
                .unwrap_or_default();
            style::usage_bar(
                ui,
                (size(d) / max) as f32,
                Palette::PURPLE,
                format!(
                    "{}  {} · data {} · log {}{used}",
                    bytes(size(d)),
                    d.name,
                    bytes(d.data_bytes),
                    bytes(d.log_bytes)
                ),
            );
            ui.add_space(2.0);
        }
        if storage.current_database_only {
            ui.weak("Only the current database: listing all needs VIEW ANY DEFINITION.");
        } else {
            ui.weak("Allocated file sizes, read every minute.");
        }
    });
}

fn tables(ui: &mut Ui, storage: &Storage) {
    section(ui, "Largest tables (current database)", |ui| {
        if storage.tables.is_empty() {
            ui.weak("—");
            return;
        }
        let max = storage
            .tables
            .iter()
            .map(|t| t.reserved_bytes)
            .fold(1.0, f64::max);
        for t in &storage.tables {
            style::usage_bar(
                ui,
                (t.reserved_bytes / max) as f32,
                Palette::BLUE,
                format!(
                    "{}  {}.{} · {} rows · indexes {}",
                    bytes(t.reserved_bytes),
                    t.schema,
                    t.name,
                    compact(t.rows as f64),
                    bytes(t.index_bytes)
                ),
            );
            ui.add_space(2.0);
        }
    });
}

fn top_statements(ui: &mut Ui, snap: &MsSnapshot, wants: &mut Wants) {
    section(ui, "Top statements by total time (plans in cache)", |ui| {
        let analyze = wants.analyze;
        let h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
        let mut table = TableBuilder::new(ui)
            .id_salt("sqlserver-top-statements")
            .striped(true)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(70.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(100.0))
            .column(Column::exact(80.0));
        if analyze {
            table = table.column(Column::exact(70.0));
        }
        table
            .column(Column::remainder().clip(true))
            .header(h, |mut r| {
                let mut titles = vec!["Execs", "Total", "Average", "CPU", "Logical reads", "Rows"];
                if analyze {
                    titles.push("");
                }
                titles.push("Statement");
                for t in titles {
                    r.col(|ui| {
                        ui.strong(t);
                    });
                }
            })
            .body(|body| {
                body.rows(h, snap.top_statements.len(), |mut row| {
                    let s = &snap.top_statements[row.index()];
                    let execs = s.executions.max(1) as f64;
                    let right = |ui: &mut Ui, t: String| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(t)
                        });
                    };
                    row.col(|ui| right(ui, compact(s.executions as f64)));
                    row.col(|ui| right(ui, time(s.total_secs)));
                    row.col(|ui| right(ui, time(s.total_secs / execs)));
                    row.col(|ui| right(ui, time(s.cpu_secs)));
                    row.col(|ui| right(ui, compact(s.logical_reads as f64)));
                    row.col(|ui| right(ui, compact(s.rows as f64)));
                    if analyze {
                        row.col(|ui| {
                            if ui
                                .small_button("Analyze")
                                .on_hover_text("Have the AI assistant explain why it is expensive")
                                .clicked()
                            {
                                wants.push(Want::Analyze(statement_analysis(s)));
                            }
                        });
                    }
                    row.col(|ui| {
                        let text = s.text.replace('\n', " ");
                        ui.add(egui::Label::new(RichText::new(&text).monospace()).truncate())
                            .on_hover_text(&s.text);
                    });
                });
            });
    });
}

/// Seconds as `12.5 ms` under a second (most statements), else as
/// [`duration`].
fn time(secs: f64) -> String {
    if secs < 1.0 {
        format!("{:.1} ms", secs * 1000.0)
    } else {
        duration(secs)
    }
}

/// A top statement and what its executions cost, for the assistant. The
/// statement is the cached text, literals included, like a statement's
/// text on Oracle.
fn statement_analysis(s: &TopStatement) -> Analysis {
    let execs = s.executions.max(1) as f64;
    Analysis {
        title: format!("SQL Server statement {}", s.hash),
        sql: Some(s.text.clone()),
        sql_id: None,
        details: format!(
            "SQL Server statement statistics for the plans in cache \
             (sys.dm_exec_query_stats, summed over query hash {}){}:\n\
             executions: {}\n\
             total elapsed: {:.3} s ({:.4} s per execution, longest {:.3} s)\n\
             CPU: {:.3} s\n\
             logical reads: {} ({:.0} per execution)\n\
             physical reads: {}\n\
             logical writes: {}\n\
             rows: {} ({:.1} per execution)",
            s.hash,
            s.database
                .as_ref()
                .map(|d| format!(", database {d}"))
                .unwrap_or_default(),
            s.executions,
            s.total_secs,
            s.total_secs / execs,
            s.max_secs,
            s.cpu_secs,
            s.logical_reads,
            s.logical_reads as f64 / execs,
            s.physical_reads,
            s.writes,
            s.rows,
            s.rows as f64 / execs,
        ),
        binds: Vec::new(),
    }
}
