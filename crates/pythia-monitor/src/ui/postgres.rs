//! The PostgreSQL dashboard: findings, KPI tiles, trend charts, sessions,
//! lock waits and transactions, replication, storage and top statements.

use std::collections::VecDeque;

use egui::{Color32, RichText, Ui};
use egui_extras::{Column, TableBuilder};
use egui_plot::{Legend, Line, Plot, PlotPoints};
use pythia_db::DbError;

use super::dashboard::{Refresh, insights_strip, top_list};
use super::style::{self, Kpi, Palette, bytes, compact, duration, section};
use super::{Analysis, Want, Wants, refresh_selector};
use crate::postgres::{Backend, Counters, PgSnapshot, STATS_ROLE, Storage, TopStatement, insights};

/// Table statistics are read this often (seconds): they cost more than
/// the rest on databases with many tables.
const STORAGE_EVERY: f64 = 60.0;

/// Keeps about an hour at 5 s refresh.
const HISTORY: usize = 720;

/// What a backend's pid in the tables opens.
const SESSION_LINK: &str = "Show the session";

/// One point of the rolling history.
#[derive(Debug, Clone, Copy)]
struct Sample {
    t: f64,
    rates: Counters,
    total: f64,
    active: f64,
    idle_in_transaction: f64,
}

#[derive(Default)]
pub struct PostgresDashboardView {
    pub refresh: Refresh,
    snapshot: Option<PgSnapshot>,
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

impl PostgresDashboardView {
    pub(crate) fn on_snapshot(&mut self, result: Result<PgSnapshot, DbError>, now: f64) {
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
                let c = s.connections.unwrap_or_default();
                self.history.push_back(Sample {
                    t: now - start,
                    rates,
                    total: c.total as f64,
                    active: c.active as f64,
                    idle_in_transaction: c.idle_in_transaction as f64,
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
                    let place = if server.address.is_empty() {
                        server.database.clone()
                    } else {
                        format!("{} · {}", server.address, server.database)
                    };
                    ui.label(RichText::new(place).strong());
                    ui.weak(format!(
                        "PostgreSQL {} · up {} · {} · shared buffers {}",
                        server.version,
                        duration(server.uptime_secs),
                        if server.standby { "standby" } else { "primary" },
                        bytes(server.shared_buffers)
                    ));
                }
                None => {
                    ui.label(RichText::new("Dashboard").strong());
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                refresh_selector(ui, "postgres-dashboard-refresh", &mut self.refresh);
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
            wants.push(Want::PostgresDashboard { storage });
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
                ui.columns(4, |cols| {
                    top_list(&mut cols[0], "Connections by user", &snap.by_user, None);
                    top_list(
                        &mut cols[1],
                        "Connections by application",
                        &snap.by_application,
                        None,
                    );
                    top_list(&mut cols[2], "Connections by client", &snap.by_client, None);
                    top_list(
                        &mut cols[3],
                        "Connections by database",
                        &snap.by_database,
                        None,
                    );
                });
                if snap.own_sessions_only {
                    ui.weak(format!(
                        "Other users' statements are hidden: they need {STATS_ROLE}."
                    ));
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
                if !snap.replicas.is_empty() || snap.receiver.is_some() || !snap.slots.is_empty() {
                    ui.add_space(8.0);
                    replication(ui, snap);
                }
                if let Some(storage) = &self.storage {
                    let database = snap
                        .server
                        .as_ref()
                        .map_or("this database", |s| s.database.as_str());
                    ui.add_space(8.0);
                    ui.columns(2, |cols| {
                        databases(&mut cols[0], storage);
                        tables(&mut cols[1], storage, database);
                    });
                }
                ui.add_space(8.0);
                top_statements(ui, snap, wants);
                ui.add_space(8.0);
                style::section_errors(ui, &snap.errors);
            });
    }

    fn kpis(&self, ui: &mut Ui, snap: &PgSnapshot) {
        let r = self.rates;
        let rate = |f: fn(&Counters) -> f64| r.map_or("…".to_owned(), |r| compact(f(&r)));
        let mut tiles = Vec::new();
        if let Some(c) = snap.connections {
            tiles.push(
                Kpi::new(
                    "Connections",
                    format!("{} / {}", c.total, c.max - c.reserved),
                )
                .detail(format!(
                    "{} active · {} idle in transaction",
                    c.active, c.idle_in_transaction
                ))
                .color(Some(style::level_color(ui, c.used_pct(), 75.0, 90.0))),
            );
        }
        tiles.push(
            Kpi::new("Commits / s", rate(|r| r.commits))
                .detail(format!("{} rollbacks / s", rate(|r| r.rollbacks))),
        );
        let hit = r.and_then(|r| r.hit_ratio());
        tiles.push(
            Kpi::new(
                "Cache hit ratio",
                match (hit, r) {
                    (Some(hit), _) => format!("{hit:.1} %"),
                    (None, Some(_)) => "idle".into(),
                    (None, None) => "…".into(),
                },
            )
            .detail(format!("{} blocks read / s", rate(|r| r.blocks_read)))
            .color(hit.map(|h| style::level_color(ui, 100.0 - h, 5.0, 10.0))),
        );
        tiles.push(
            Kpi::new("Rows returned / s", rate(|r| r.rows_returned))
                .detail(format!("{} fetched by index / s", rate(|r| r.rows_fetched))),
        );
        tiles.push(
            Kpi::new("Rows changed / s", rate(Counters::rows_changed)).detail(format!(
                "{} inserted · {} updated · {} deleted",
                rate(|r| r.rows_inserted),
                rate(|r| r.rows_updated),
                rate(|r| r.rows_deleted)
            )),
        );
        let oldest = snap.transactions.first().and_then(|t| t.transaction_secs);
        tiles.push(
            Kpi::new("Oldest transaction", oldest.map_or("—".into(), duration))
                .detail(format!(
                    "{} open · {} lock waits",
                    snap.open_transactions.unwrap_or(0),
                    snap.lock_waits.len()
                ))
                .color(oldest.map(|s| style::level_color(ui, s, 60.0, 600.0))),
        );
        tiles.push(
            Kpi::new(
                "Temp files / s",
                r.map_or("…".into(), |r| bytes(r.temp_bytes)),
            )
            .detail(format!("{} files / s", rate(|r| r.temp_files))),
        );
        tiles.push(
            Kpi::new(
                "Deadlocks",
                snap.counters.map_or("…".into(), |c| compact(c.deadlocks)),
            )
            .detail(format!("{} / s", rate(|r| r.deadlocks)))
            .color(
                r.filter(|r| r.deadlocks > 0.0)
                    .map(|_| ui.visuals().warn_fg_color),
            ),
        );
        tiles.push(
            Kpi::new("WAL / s", r.map_or("…".into(), |r| bytes(r.wal_bytes))).detail(
                snap.counters.map_or(String::new(), |c| {
                    format!(
                        "checkpoints: {} timed · {} requested",
                        compact(c.checkpoints_timed),
                        compact(c.checkpoints_requested)
                    )
                }),
            ),
        );
        if let Some((database, age)) = &snap.oldest_xid {
            let used = 100.0 * *age as f64 / 2_147_483_648.0;
            tiles.push(
                Kpi::new("Transaction ID age", compact(*age as f64))
                    .detail(format!("{used:.0} % to wraparound · {database}"))
                    .color(Some(style::level_color(ui, used, 45.0, 70.0))),
            );
        }
        tiles.push(
            Kpi::new("Autovacuum workers", snap.autovacuum_workers.to_string())
                .detail("running now"),
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
                "Transactions / s",
                "postgres-transactions",
                compact,
                &|p| {
                    p.line(
                        Line::new("Commits", series(&|s| s.rates.commits))
                            .color(Palette::GREEN)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                    p.line(
                        Line::new("Rollbacks", series(&|s| s.rates.rollbacks)).color(Palette::RED),
                    );
                },
            );
            chart(
                &mut cols[1],
                "Connections",
                "postgres-connections",
                compact,
                &|p| {
                    p.line(Line::new("Connected", series(&|s| s.total)).color(Palette::PURPLE));
                    p.line(
                        Line::new("Active", series(&|s| s.active))
                            .color(Palette::TEAL)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                    p.line(
                        Line::new("Idle in transaction", series(&|s| s.idle_in_transaction))
                            .color(Palette::ORANGE),
                    );
                },
            );
        });
        ui.add_space(8.0);
        ui.columns(2, |cols| {
            chart(&mut cols[0], "Rows / s", "postgres-rows", compact, &|p| {
                p.line(
                    Line::new("Returned", series(&|s| s.rates.rows_returned))
                        .color(Palette::BLUE)
                        .fill(0.0)
                        .fill_alpha(0.15),
                );
                p.line(
                    Line::new("Fetched", series(&|s| s.rates.rows_fetched)).color(Palette::TEAL),
                );
                p.line(
                    Line::new("Changed", series(&|s| s.rates.rows_changed()))
                        .color(Palette::ORANGE),
                );
            });
            chart(
                &mut cols[1],
                "Blocks / s",
                "postgres-blocks",
                compact,
                &|p| {
                    p.line(
                        Line::new("From the cache", series(&|s| s.rates.blocks_hit))
                            .color(Palette::GREEN)
                            .fill(0.0)
                            .fill_alpha(0.15),
                    );
                    p.line(Line::new("Read", series(&|s| s.rates.blocks_read)).color(Palette::RED));
                },
            );
        });
    }
}

/// Colors wait event types like Oracle's wait classes.
fn wait_color(kind: &str) -> Color32 {
    match kind {
        "CPU" => Palette::GREEN,
        "Lock" | "BufferPin" => Palette::RED,
        "LWLock" => Palette::ORANGE,
        "IO" => Palette::BLUE,
        "IPC" | "Client" => Palette::PURPLE,
        _ => Palette::TEAL,
    }
}

fn activity(ui: &mut Ui, snap: &PgSnapshot) {
    section(ui, "Active sessions by wait (now)", |ui| {
        if snap.activity.is_empty() {
            ui.weak("No statements running besides the dashboard's.");
            return;
        }
        let segments: Vec<(String, f64, Color32)> = snap
            .activity
            .iter()
            .map(|(kind, n)| (kind.clone(), *n as f64, wait_color(kind)))
            .collect();
        style::stacked_bar(ui, &segments, None);
    });
}

/// A session's statement as shown: hidden ones say why.
fn statement(b: &Backend) -> String {
    match (&b.query, &b.state) {
        (Some(q), _) => q.clone(),
        (None, None) => "(another user's: hidden)".into(),
        (None, Some(_)) => String::new(),
    }
}

fn secs(s: Option<f64>) -> String {
    s.map_or(String::new(), duration)
}

fn lock_waits(ui: &mut Ui, snap: &PgSnapshot, wants: &mut Wants) {
    section(ui, "Lock waits", |ui| {
        let rows: Vec<Vec<String>> = snap
            .lock_waits
            .iter()
            .map(|w| {
                let blocker = match (&w.blocking_query, &w.blocking_state) {
                    (Some(q), Some(s)) if s.starts_with("idle in transaction") => {
                        format!("({s}) {q}")
                    }
                    (Some(q), _) => q.clone(),
                    (None, _) => String::new(),
                };
                vec![
                    w.waiting_pid.to_string(),
                    duration(w.wait_secs),
                    w.blocking_pid.to_string(),
                    w.relation.clone().unwrap_or_default(),
                    blocker,
                    w.waiting_query.clone().unwrap_or_default(),
                ]
            })
            .collect();
        let clicked = style::text_table(
            ui,
            "postgres-lock-waits",
            &[
                ("Waiting", 70.0, true),
                ("For", 70.0, true),
                ("Blocked by", 80.0, true),
                ("Table", 160.0, false),
                ("Blocker runs (or ran last)", 240.0, false),
                ("Waiting statement", 0.0, false),
            ],
            &rows,
            &[0, 2],
            SESSION_LINK,
        );
        if let Some(id) = clicked {
            wants.push(Want::OpenConnection { id });
        }
    });
}

fn running(ui: &mut Ui, snap: &PgSnapshot, wants: &mut Wants) {
    section(ui, "Running statements, longest first", |ui| {
        if snap.running.is_empty() {
            ui.weak("—");
            return;
        }
        let rows: Vec<Vec<String>> = snap
            .running
            .iter()
            .map(|b| {
                vec![
                    b.pid.to_string(),
                    b.user.clone().unwrap_or_default(),
                    b.database.clone().unwrap_or_default(),
                    secs(b.query_secs),
                    b.wait.clone().unwrap_or_else(|| "CPU".into()),
                    statement(b),
                ]
            })
            .collect();
        let clicked = style::text_table(
            ui,
            "postgres-running",
            &[
                ("PID", 70.0, true),
                ("User", 110.0, false),
                ("Database", 110.0, false),
                ("Time", 70.0, true),
                ("Waiting for", 180.0, false),
                ("Statement", 0.0, false),
            ],
            &rows,
            &[0],
            SESSION_LINK,
        );
        if let Some(id) = clicked {
            wants.push(Want::OpenConnection { id });
        }
    });
}

fn transactions(ui: &mut Ui, snap: &PgSnapshot, wants: &mut Wants) {
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
            .map(|b| {
                let state = match (&b.state, b.idle_in_transaction()) {
                    (Some(s), true) => format!("{s} for {}", secs(b.state_secs)),
                    (Some(s), false) => s.clone(),
                    (None, _) => String::new(),
                };
                vec![
                    b.pid.to_string(),
                    b.user.clone().unwrap_or_default(),
                    b.database.clone().unwrap_or_default(),
                    secs(b.transaction_secs),
                    state,
                    statement(b),
                ]
            })
            .collect();
        let clicked = style::text_table(
            ui,
            "postgres-transactions",
            &[
                ("PID", 70.0, true),
                ("User", 110.0, false),
                ("Database", 110.0, false),
                ("Open for", 80.0, true),
                ("State", 210.0, false),
                ("Statement (or the last one)", 0.0, false),
            ],
            &rows,
            &[0],
            SESSION_LINK,
        );
        if let Some(id) = clicked {
            wants.push(Want::OpenConnection { id });
        }
    });
}

fn replication(ui: &mut Ui, snap: &PgSnapshot) {
    section(ui, "Replication", |ui| {
        if let Some(r) = &snap.receiver {
            ui.label(format!(
                "Standby of {} · receiver {} · {}",
                r.sender.as_deref().unwrap_or("?"),
                r.status.as_deref().unwrap_or("not running"),
                r.lag_secs
                    .map_or("lag unknown".into(), |l| format!("{} behind", duration(l)))
            ));
        }
        if !snap.replicas.is_empty() {
            let rows: Vec<Vec<String>> = snap
                .replicas
                .iter()
                .map(|r| {
                    vec![
                        r.name.clone(),
                        r.client.clone(),
                        r.state.clone().unwrap_or_default(),
                        r.sync_state.clone().unwrap_or_default(),
                        r.lag_secs.map_or("—".into(), duration),
                        r.lag_bytes.map_or("—".into(), bytes),
                    ]
                })
                .collect();
            style::text_table(
                ui,
                "postgres-replicas",
                &[
                    ("Standby", 140.0, false),
                    ("Client", 160.0, false),
                    ("State", 90.0, false),
                    ("Sync", 80.0, false),
                    ("Replay lag", 90.0, true),
                    ("Not replayed", 0.0, false),
                ],
                &rows,
                &[],
                "",
            );
        }
        if !snap.slots.is_empty() {
            ui.add_space(4.0);
            let rows: Vec<Vec<String>> = snap
                .slots
                .iter()
                .map(|s| {
                    vec![
                        s.name.clone(),
                        s.kind.clone(),
                        if s.active { "active" } else { "inactive" }.into(),
                        bytes(s.retained_bytes),
                    ]
                })
                .collect();
            style::text_table(
                ui,
                "postgres-slots",
                &[
                    ("Slot", 180.0, false),
                    ("Type", 80.0, false),
                    ("State", 80.0, false),
                    ("WAL kept", 0.0, false),
                ],
                &rows,
                &[],
                "",
            );
        }
    });
}

fn databases(ui: &mut Ui, storage: &Storage) {
    section(ui, "Largest databases", |ui| {
        if storage.databases.is_empty() {
            ui.weak("—");
            return;
        }
        let max = storage
            .databases
            .iter()
            .filter_map(|d| d.bytes)
            .fold(1.0, f64::max);
        for d in &storage.databases {
            match d.bytes {
                Some(b) => style::usage_bar(
                    ui,
                    (b / max) as f32,
                    Palette::PURPLE,
                    format!("{}  {}", bytes(b), d.name),
                ),
                None => {
                    ui.weak(format!("{}: size needs CONNECT on it", d.name));
                }
            }
            ui.add_space(2.0);
        }
        ui.weak("Read every minute.");
    });
}

fn tables(ui: &mut Ui, storage: &Storage, database: &str) {
    section(ui, &format!("Largest tables in {database}"), |ui| {
        if storage.tables.is_empty() {
            ui.weak("—");
            return;
        }
        let max = storage
            .tables
            .iter()
            .map(|t| t.total_bytes)
            .fold(1.0, f64::max);
        for t in &storage.tables {
            let dead = if t.dead_rows > 0 {
                format!(" · {:.0} % dead", t.dead_pct())
            } else {
                String::new()
            };
            style::usage_bar(
                ui,
                (t.total_bytes / max) as f32,
                Palette::BLUE,
                format!(
                    "{}  {}.{} · ~{} rows{dead}",
                    bytes(t.total_bytes),
                    t.schema,
                    t.name,
                    compact(t.live_rows as f64)
                ),
            );
            ui.add_space(2.0);
        }
        if !storage.dead_rows.is_empty() {
            ui.add_space(4.0);
            ui.weak("Many dead rows:");
            for t in &storage.dead_rows {
                ui.label(format!(
                    "{}.{}: {} dead ({:.0} %), {}",
                    t.schema,
                    t.name,
                    compact(t.dead_rows as f64),
                    t.dead_pct(),
                    t.vacuumed_secs_ago
                        .map_or("never vacuumed".into(), |s| format!(
                            "vacuumed {} ago",
                            duration(s)
                        ))
                ));
            }
        }
    });
}

fn top_statements(ui: &mut Ui, snap: &PgSnapshot, wants: &mut Wants) {
    section(
        ui,
        "Top statements by total time (since the statistics were reset)",
        |ui| {
            if let Some(missing) = &snap.statements_missing {
                ui.weak(missing);
                return;
            }
            if snap.top_statements.is_empty() {
                ui.weak("—");
                return;
            }
            let analyze = wants.analyze;
            let h = super::style::row_height(ui);
            let mut table = TableBuilder::new(ui)
                .id_salt("postgres-top-statements")
                .striped(true)
                .vscroll(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(70.0))
                .column(Column::exact(80.0))
                .column(Column::exact(80.0))
                .column(Column::exact(90.0))
                .column(Column::exact(80.0))
                .column(Column::exact(80.0));
            if analyze {
                table = table.column(Column::exact(70.0));
            }
            table
                .column(Column::remainder().clip(true))
                .header(h, |mut r| {
                    let mut titles = vec!["Calls", "Total", "Average", "Rows", "Hit %", "Temp"];
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
                        let calls = s.calls.max(1) as f64;
                        let blocks = (s.blocks_hit + s.blocks_read) as f64;
                        let right = |ui: &mut Ui, t: String| {
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| ui.label(t),
                            );
                        };
                        row.col(|ui| right(ui, compact(s.calls as f64)));
                        row.col(|ui| right(ui, duration(s.total_secs)));
                        row.col(|ui| right(ui, duration(s.total_secs / calls)));
                        row.col(|ui| right(ui, compact(s.rows as f64)));
                        row.col(|ui| {
                            right(
                                ui,
                                if blocks > 0.0 {
                                    format!("{:.1}", 100.0 * s.blocks_hit as f64 / blocks)
                                } else {
                                    "—".into()
                                },
                            )
                        });
                        // PostgreSQL's blocks are 8 KB.
                        row.col(|ui| right(ui, bytes(s.temp_blocks_written as f64 * 8192.0)));
                        if analyze {
                            row.col(|ui| {
                                if ui
                                    .small_button("Analyze")
                                    .on_hover_text(
                                        "Have the AI assistant explain why it is expensive",
                                    )
                                    .clicked()
                                {
                                    wants.push(Want::Analyze(statement_analysis(s)));
                                }
                            });
                        }
                        row.col(|ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(s.text.replace('\n', " ")).monospace(),
                                )
                                .truncate(),
                            )
                            .on_hover_text(&s.text);
                        });
                    });
                });
        },
    );
}

/// A top statement and what its executions cost, for the assistant. The
/// normalized text has `$1`, `$2` for the constants: no data.
fn statement_analysis(s: &TopStatement) -> Analysis {
    let calls = s.calls.max(1) as f64;
    Analysis {
        title: format!("PostgreSQL statement {}", s.query_id),
        sql: Some(s.text.clone()),
        sql_id: None,
        details: format!(
            "PostgreSQL statement statistics since the last reset (pg_stat_statements){}:\n\
             calls: {}\n\
             total execution time: {:.3} s ({:.4} s per call)\n\
             rows: {} ({:.1} per call)\n\
             shared blocks hit: {}, read: {}\n\
             temporary blocks written: {}",
            s.database
                .as_ref()
                .map(|d| format!(", database {d}"))
                .unwrap_or_default(),
            s.calls,
            s.total_secs,
            s.total_secs / calls,
            s.rows,
            s.rows as f64 / calls,
            s.blocks_hit,
            s.blocks_read,
            s.temp_blocks_written,
        ),
        binds: Vec::new(),
    }
}
