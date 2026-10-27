//! The MySQL dashboard: findings, KPI tiles, trend charts, threads, lock
//! waits and transactions, replication, storage and top statements.

use std::collections::VecDeque;

use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use pythia_db::DbError;

use super::dashboard::{Refresh, insights_strip, session_bars};
use super::style::{self, Kpi, Palette, bytes, compact, duration, section};
use super::{Analysis, Want, Wants, refresh_selector};
use crate::connections::ConnectionFilter;
use crate::mysql::{Counters, MySqlSnapshot, Storage, TopStatement, insights};

/// Table statistics are read this often (seconds): they cost more than
/// the rest on servers with many tables.
const STORAGE_EVERY: f64 = 60.0;

/// Keeps about an hour at 5 s refresh.
const HISTORY: usize = 720;

/// What a thread id in the tables opens.
const THREAD_LINK: &str = "Show the thread";

/// One point of the rolling history.
#[derive(Debug, Clone, Copy)]
struct Sample {
    t: f64,
    rates: Counters,
    connected: f64,
    running: f64,
}

#[derive(Default)]
pub struct MySqlDashboardView {
    pub refresh: Refresh,
    snapshot: Option<MySqlSnapshot>,
    /// The last table statistics read.
    storage: Option<Storage>,
    storage_requested: Option<f64>,
    prev: Option<(Counters, f64)>,
    rates: Option<Counters>,
    history: VecDeque<Sample>,
    in_flight: bool,
    last_request: Option<f64>,
    error: Option<String>,
    start: Option<f64>,
}

impl MySqlDashboardView {
    pub(crate) fn on_snapshot(&mut self, result: Result<MySqlSnapshot, DbError>, now: f64) {
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
                let (connected, running) = s
                    .connections
                    .map_or((0.0, 0.0), |c| (c.connected as f64, c.running as f64));
                self.history.push_back(Sample {
                    t: now - start,
                    rates,
                    connected,
                    running,
                });
                while self.history.len() > HISTORY {
                    self.history.pop_front();
                }
            }
            self.prev = Some((cur, now));
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
                    ui.label(
                        RichText::new(format!("{}:{}", server.hostname, server.port)).strong(),
                    );
                    let product = if server.version.contains("MariaDB") {
                        "MariaDB"
                    } else {
                        "MySQL"
                    };
                    ui.weak(format!(
                        "{product} {} · {} · up {}{}",
                        server.version,
                        server.comment,
                        duration(server.uptime_secs),
                        if server.read_only {
                            " · read-only"
                        } else {
                            ""
                        }
                    ));
                }
                None => {
                    ui.label(RichText::new("Dashboard").strong());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                refresh_selector(ui, "mysql-dashboard-refresh", &mut self.refresh);
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
            wants.push(Want::MySqlDashboard { storage });
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
                self.charts(ui);
                ui.add_space(8.0);
                activity(ui, snap);
                ui.add_space(8.0);
                session_bars(
                    ui,
                    &[
                        ("Connections by user", &snap.by_user, ConnectionFilter::User),
                        ("Connections by host", &snap.by_host, ConnectionFilter::Host),
                        (
                            "Connections by database",
                            &snap.by_db,
                            ConnectionFilter::Database,
                        ),
                    ],
                    wants,
                );
                if snap.own_threads_only {
                    ui.weak(
                        "Only this user's connections are listed: other users' need the \
                         PROCESS privilege.",
                    );
                }
                if !snap.lock_waits.is_empty() {
                    ui.add_space(8.0);
                    lock_waits(ui, snap, wants);
                }
                ui.add_space(8.0);
                running(ui, snap, wants);
                if !snap.transactions.is_empty() {
                    ui.add_space(8.0);
                    transactions(ui, snap, wants);
                }
                if !snap.replicas.is_empty() {
                    ui.add_space(8.0);
                    replication(ui, snap);
                }
                if let Some(storage) = &self.storage {
                    ui.add_space(8.0);
                    ui.columns(2, |cols| {
                        schemas(&mut cols[0], storage);
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

    fn kpis(&self, ui: &mut Ui, snap: &MySqlSnapshot) {
        let r = self.rates;
        let rate = |f: fn(&Counters) -> f64| r.map_or("…".to_owned(), |r| compact(f(&r)));
        let mut tiles = Vec::new();
        if let Some(c) = snap.connections {
            let used = 100.0 * c.connected as f64 / c.max_connections.max(1) as f64;
            tiles.push(
                Kpi::new(
                    "Connections",
                    format!("{} / {}", c.connected, c.max_connections),
                )
                .detail(format!("{} running · peak {}", c.running, c.max_used))
                .color(Some(style::level_color(ui, used, 75.0, 90.0))),
            );
        }
        tiles.push(Kpi::new("Queries / s", rate(|r| r.queries)).detail(format!(
            "{} selects · {} changes / s",
            rate(|r| r.selects),
            rate(|r| r.changes)
        )));
        tiles.push(
            Kpi::new("Commits / s", rate(|r| r.commits))
                .detail(format!("{} rollbacks / s", rate(|r| r.rollbacks))),
        );
        let hit = r.and_then(|r| r.hit_ratio());
        tiles.push(
            Kpi::new(
                "Buffer pool hit ratio",
                match (hit, r) {
                    (Some(hit), _) => format!("{hit:.1} %"),
                    (None, Some(_)) => "idle".into(),
                    (None, None) => "…".into(),
                },
            )
            .detail(format!("{} disk reads / s", rate(|r| r.physical_reads)))
            .color(hit.map(|h| style::level_color(ui, 100.0 - h, 5.0, 10.0))),
        );
        if let Some(b) = snap.buffer_pool {
            tiles.push(
                Kpi::new("Buffer pool used", format!("{:.0} %", b.used_pct())).detail(format!(
                    "of {} · {:.0} % dirty",
                    bytes(b.size_bytes),
                    b.dirty_pct()
                )),
            );
        }
        tiles.push(
            Kpi::new("Rows read / s", rate(|r| r.rows_read))
                .detail(format!("{} changed / s", rate(|r| r.rows_changed))),
        );
        tiles.push(
            Kpi::new("Received / s", r.map_or("…".into(), |r| bytes(r.bytes_in))).detail(format!(
                "sent {} / s",
                r.map_or("…".into(), |r| bytes(r.bytes_out))
            )),
        );
        if let (Some(c), Some(server)) = (snap.counters, &snap.server) {
            let detail = if server.slow_query_log {
                format!(
                    "{} / s · over {} s",
                    rate(|r| r.slow_queries),
                    server.long_query_time
                )
            } else {
                format!("{} / s · slow query log off", rate(|r| r.slow_queries))
            };
            tiles.push(Kpi::new("Slow queries", compact(c.slow_queries)).detail(detail));
        }
        let disk = r
            .and_then(|r| r.tmp_disk_pct())
            .or_else(|| snap.counters.and_then(|c| c.tmp_disk_pct()));
        tiles.push(
            Kpi::new(
                "Temp tables on disk",
                disk.map_or("—".into(), |p| format!("{p:.0} %")),
            )
            .detail(format!("{} created / s", rate(|r| r.tmp_tables)))
            .color(disk.map(|d| style::level_color(ui, d, 25.0, 50.0))),
        );
        let open = snap
            .open_transactions
            .map(|n| format!(" · {n} open transactions"))
            .unwrap_or_default();
        tiles.push(
            Kpi::new("Row lock waits / s", rate(|r| r.row_lock_waits))
                .detail(format!("{} blocked now{open}", snap.lock_waits.len())),
        );
        tiles.push(
            Kpi::new("Full scans / s", rate(|r| r.full_scans))
                .detail("table scans and joins without an index"),
        );
        tiles.push(Kpi::new(
            "Redo log / s",
            r.map_or("…".into(), |r| bytes(r.log_bytes)),
        ));
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
                "Queries / s",
                "mysql-queries",
                compact,
                &|p| {
                    p.line(
                        Line::new("Queries", series(&|s| s.rates.queries))
                            .color(Palette::BLUE)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                    p.line(Line::new("Selects", series(&|s| s.rates.selects)).color(Palette::TEAL));
                    p.line(
                        Line::new("Changes", series(&|s| s.rates.changes)).color(Palette::ORANGE),
                    );
                    p.line(
                        Line::new("Commits", series(&|s| s.rates.commits)).color(Palette::GREEN),
                    );
                },
            );
            chart(
                &mut cols[1],
                "Connections",
                "mysql-threads",
                compact,
                &|p| {
                    p.line(Line::new("Connected", series(&|s| s.connected)).color(Palette::PURPLE));
                    p.line(
                        Line::new("Running", series(&|s| s.running))
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
                "InnoDB rows / s",
                "mysql-rows",
                compact,
                &|p| {
                    p.line(
                        Line::new("Read", series(&|s| s.rates.rows_read))
                            .color(Palette::BLUE)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                    p.line(
                        Line::new("Changed", series(&|s| s.rates.rows_changed))
                            .color(Palette::ORANGE),
                    );
                },
            );
            chart(&mut cols[1], "Network / s", "mysql-network", bytes, &|p| {
                p.line(
                    Line::new("Received", series(&|s| s.rates.bytes_in))
                        .color(Palette::GREEN)
                        .fill(0.0)
                        .fill_alpha(0.15),
                );
                p.line(Line::new("Sent", series(&|s| s.rates.bytes_out)).color(Palette::PURPLE));
            });
        });
    }
}

/// Colors thread states like Oracle's wait classes: running, waiting for
/// a lock, working on disk, waiting for something else.
fn state_color(state: &str) -> Color32 {
    let state = state.to_lowercase();
    if state.contains("lock") {
        Palette::RED
    } else if ["tmp", "sort", "creating", "copy", "alter"]
        .iter()
        .any(|w| state.contains(w))
    {
        Palette::ORANGE
    } else if state.contains("wait") {
        Palette::PURPLE
    } else if ["executing", "sending", "query", "statistics", "updating"]
        .iter()
        .any(|w| state.contains(w))
    {
        Palette::GREEN
    } else {
        Palette::TEAL
    }
}

fn activity(ui: &mut Ui, snap: &MySqlSnapshot) {
    section(ui, "Running statements by state (now)", |ui| {
        if snap.activity.is_empty() {
            ui.weak("No statements running besides the dashboard's.");
            return;
        }
        let segments: Vec<(String, f64, Color32)> = snap
            .activity
            .iter()
            .map(|(state, n)| (state.clone(), *n as f64, state_color(state)))
            .collect();
        style::stacked_bar(ui, &segments, None);
    });
}

fn lock_waits(ui: &mut Ui, snap: &MySqlSnapshot, wants: &mut Wants) {
    section(ui, "Lock waits", |ui| {
        let rows: Vec<Vec<String>> = snap
            .lock_waits
            .iter()
            .map(|w| {
                vec![
                    w.waiting_thread.to_string(),
                    duration(w.wait_secs as f64),
                    w.blocking_thread.to_string(),
                    w.table.clone().unwrap_or_default(),
                    w.blocking_query
                        .clone()
                        .unwrap_or_else(|| "(idle in transaction)".into()),
                    w.waiting_query.clone().unwrap_or_default(),
                ]
            })
            .collect();
        let thread = style::text_table(
            ui,
            "mysql-lock-waits",
            &[
                ("Waiting", 70.0, true),
                ("For", 70.0, true),
                ("Blocked by", 80.0, true),
                ("Table", 160.0, false),
                ("Blocker runs", 220.0, false),
                ("Waiting statement", 0.0, false),
            ],
            &rows,
            &[0, 2],
            THREAD_LINK,
        );
        if let Some(id) = thread {
            wants.push(Want::OpenConnection { id });
        }
    });
}

fn running(ui: &mut Ui, snap: &MySqlSnapshot, wants: &mut Wants) {
    section(ui, "Running statements, longest first", |ui| {
        if snap.running.is_empty() {
            ui.weak("—");
            return;
        }
        let rows: Vec<Vec<String>> = snap
            .running
            .iter()
            .map(|t| {
                vec![
                    t.id.to_string(),
                    t.user.clone(),
                    t.db.clone().unwrap_or_default(),
                    duration(t.time_secs as f64),
                    if t.state.is_empty() {
                        t.command.clone()
                    } else {
                        t.state.clone()
                    },
                    t.query.clone().unwrap_or_default(),
                ]
            })
            .collect();
        let thread = style::text_table(
            ui,
            "mysql-running",
            &[
                ("Thread", 70.0, true),
                ("User", 110.0, false),
                ("Database", 110.0, false),
                ("Time", 70.0, true),
                ("State", 180.0, false),
                ("Statement", 0.0, false),
            ],
            &rows,
            &[0],
            THREAD_LINK,
        );
        if let Some(id) = thread {
            wants.push(Want::OpenConnection { id });
        }
    });
}

fn transactions(ui: &mut Ui, snap: &MySqlSnapshot, wants: &mut Wants) {
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
                    t.thread.to_string(),
                    t.user.clone().unwrap_or_default(),
                    duration(t.age_secs as f64),
                    t.state.clone(),
                    compact(t.rows_locked as f64),
                    compact(t.rows_modified as f64),
                    match (&t.query, t.idle) {
                        (Some(q), _) => q.clone(),
                        (None, true) => "(idle)".into(),
                        (None, false) => String::new(),
                    },
                ]
            })
            .collect();
        let thread = style::text_table(
            ui,
            "mysql-transactions",
            &[
                ("Thread", 70.0, true),
                ("User", 110.0, false),
                ("Open for", 80.0, true),
                ("State", 90.0, false),
                ("Rows locked", 90.0, true),
                ("Modified", 80.0, true),
                ("Statement", 0.0, false),
            ],
            &rows,
            &[0],
            THREAD_LINK,
        );
        if let Some(id) = thread {
            wants.push(Want::OpenConnection { id });
        }
    });
}

fn replication(ui: &mut Ui, snap: &MySqlSnapshot) {
    section(ui, "Replication", |ui| {
        let state = |on: bool| if on { "running" } else { "stopped" };
        let rows: Vec<Vec<String>> = snap
            .replicas
            .iter()
            .map(|r| {
                vec![
                    if r.channel.is_empty() {
                        "(default)".into()
                    } else {
                        r.channel.clone()
                    },
                    r.source.clone(),
                    state(r.io_running).into(),
                    state(r.sql_running).into(),
                    r.lag_secs.map_or("—".into(), |l| duration(l as f64)),
                    r.last_error.clone().unwrap_or_default(),
                ]
            })
            .collect();
        style::text_table(
            ui,
            "mysql-replication",
            &[
                ("Channel", 100.0, false),
                ("Source", 180.0, false),
                ("Receiver", 80.0, false),
                ("Applier", 80.0, false),
                ("Lag", 70.0, true),
                ("Last error", 0.0, false),
            ],
            &rows,
            &[],
            "",
        );
    });
}

fn schemas(ui: &mut Ui, storage: &Storage) {
    section(ui, "Largest databases", |ui| {
        if storage.schemas.is_empty() {
            ui.weak("—");
            return;
        }
        let size = |s: &crate::mysql::SchemaSize| s.data_bytes + s.index_bytes;
        let max = storage.schemas.iter().map(size).fold(1.0, f64::max);
        for s in &storage.schemas {
            let free = if s.free_bytes > 0.0 {
                format!(" · {} free", bytes(s.free_bytes))
            } else {
                String::new()
            };
            style::usage_bar(
                ui,
                (size(s) / max) as f32,
                Palette::PURPLE,
                format!(
                    "{}  {} · {} tables · {} indexes{free}",
                    bytes(size(s)),
                    s.schema,
                    s.tables,
                    bytes(s.index_bytes)
                ),
            );
            ui.add_space(2.0);
        }
        ui.weak("From the table statistics, read every minute.");
    });
}

fn tables(ui: &mut Ui, storage: &Storage) {
    section(ui, "Largest tables", |ui| {
        if storage.tables.is_empty() {
            ui.weak("—");
            return;
        }
        let size = |t: &crate::mysql::TableSize| t.data_bytes + t.index_bytes;
        let max = storage.tables.iter().map(size).fold(1.0, f64::max);
        for t in &storage.tables {
            style::usage_bar(
                ui,
                (size(t) / max) as f32,
                Palette::BLUE,
                format!(
                    "{}  {}.{} · ~{} rows",
                    bytes(size(t)),
                    t.schema,
                    t.name,
                    compact(t.rows as f64)
                ),
            );
            ui.add_space(2.0);
        }
    });
}

fn top_statements(ui: &mut Ui, snap: &MySqlSnapshot, wants: &mut Wants) {
    section(ui, "Top statements by total time (since startup)", |ui| {
        let analyze = wants.analyze;
        let h = super::style::row_height(ui);
        let mut table = TableBuilder::new(ui)
            .id_salt("mysql-top-statements")
            .striped(true)
            .vscroll(false)
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(70.0))
            .column(Column::exact(80.0))
            .column(Column::exact(80.0))
            .column(Column::exact(100.0))
            .column(Column::exact(80.0))
            .column(Column::exact(70.0));
        if analyze {
            table = table.column(Column::exact(70.0));
        }
        table
            .column(Column::remainder().clip(true))
            .header(h, |mut r| {
                let mut titles = vec![
                    "Execs",
                    "Total",
                    "Average",
                    "Rows examined",
                    "Rows sent",
                    "No index",
                ];
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
                    row.col(|ui| right(ui, duration(s.total_secs)));
                    row.col(|ui| right(ui, duration(s.total_secs / execs)));
                    row.col(|ui| right(ui, compact(s.rows_examined as f64)));
                    row.col(|ui| right(ui, compact(s.rows_sent as f64)));
                    row.col(|ui| right(ui, compact(s.no_index as f64)));
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
                        ui.add(egui::Label::new(RichText::new(&s.text).monospace()).truncate())
                            .on_hover_text(&s.text);
                    });
                });
            });
    });
}

/// A top statement and what its executions cost, for the assistant. The
/// digest text has `?` for the literals: no data.
fn statement_analysis(s: &TopStatement) -> Analysis {
    let execs = s.executions.max(1) as f64;
    Analysis {
        title: format!("MySQL statement {}", &s.digest[..s.digest.len().min(12)]),
        sql: Some(s.text.clone()),
        sql_id: None,
        details: format!(
            "MySQL statement statistics since startup \
             (performance_schema.events_statements_summary_by_digest){}:\n\
             executions: {}\n\
             total time: {:.3} s ({:.4} s per execution)\n\
             lock time: {:.3} s\n\
             rows examined: {} ({:.0} per execution)\n\
             rows sent: {} ({:.1} per execution)\n\
             rows affected: {}\n\
             executions without a (good) index: {}\n\
             temporary tables on disk: {}",
            s.schema
                .as_ref()
                .map(|d| format!(", database {d}"))
                .unwrap_or_default(),
            s.executions,
            s.total_secs,
            s.total_secs / execs,
            s.lock_secs,
            s.rows_examined,
            s.rows_examined as f64 / execs,
            s.rows_sent,
            s.rows_sent as f64 / execs,
            s.rows_affected,
            s.no_index,
            s.tmp_disk_tables,
        ),
        binds: Vec::new(),
    }
}
