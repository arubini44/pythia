//! The PostgreSQL dashboard's data: server counters, sessions and what
//! they run, transactions and lock waits, replication, storage and top
//! statements, read through [`pythia_db::Client`] (the session's
//! `DbRequest::Run`).
//!
//! It comes from `pg_stat_activity`, `pg_stat_database`, `pg_locks`
//! (`pg_blocking_pids`), the replication views, `pg_database`, the table
//! statistics and `pg_stat_statements`. Sections the user can't read are
//! left out with what they need. Without `pg_read_all_stats` (granted with
//! `pg_monitor`), other users' sessions show without their statements.

use pythia_db::{Cell, Client, DbError};

use crate::cells::{all_counts, int, num, opt, plural, text, top_counts};
use crate::insights::{Insight, Severity};
use crate::queries::SectionError;
use crate::ui::style::{bytes, compact, duration};

macro_rules! counters {
    ($($(#[$doc:meta])* $field:ident,)*) => {
        /// Cumulative counters: `pg_stat_database` summed over the
        /// databases, the WAL position and the checkpoints. Between two
        /// snapshots they become rates ([`Counters::per_second`]).
        #[derive(Debug, Clone, Copy, Default, PartialEq)]
        pub struct Counters {
            $($(#[$doc])* pub $field: f64,)*
        }

        impl Counters {
            /// From a row with the counters in the order declared.
            fn from_row(row: &[Cell]) -> Self {
                let mut values = (0..).map(|i| num(row, i));
                Self {
                    $($field: values.next().unwrap_or(0.0),)*
                }
            }

            /// Each counter's change per second from `prev` to `cur`.
            pub fn per_second(prev: &Self, cur: &Self, seconds: f64) -> Self {
                if seconds <= 0.0 {
                    return Self::default();
                }
                // They start over when the server restarts or the
                // statistics are reset.
                Self {
                    $($field: (cur.$field - prev.$field).max(0.0) / seconds,)*
                }
            }
        }
    };
}

counters! {
    commits,
    rollbacks,
    /// Blocks read from disk (or the operating system's cache).
    blocks_read,
    /// Blocks found in shared buffers.
    blocks_hit,
    /// Rows returned by scans.
    rows_returned,
    /// Rows fetched by index scans.
    rows_fetched,
    rows_inserted,
    rows_updated,
    rows_deleted,
    temp_files,
    temp_bytes,
    deadlocks,
    /// Queries cancelled on a standby for conflicts with recovery.
    conflicts,
    /// WAL written; on a standby, WAL replayed.
    wal_bytes,
    /// Checkpoints started on schedule (`checkpoint_timeout`).
    checkpoints_timed,
    /// Checkpoints started because WAL reached `max_wal_size`, or asked for.
    checkpoints_requested,
}

impl Counters {
    /// Share of block reads served from shared buffers, in percent.
    pub fn hit_ratio(&self) -> Option<f64> {
        let total = self.blocks_read + self.blocks_hit;
        (total > 0.0).then(|| 100.0 * self.blocks_hit / total)
    }

    pub fn rows_changed(&self) -> f64 {
        self.rows_inserted + self.rows_updated + self.rows_deleted
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerInfo {
    /// E.g. "18.1 (Debian 18.1-1.pgdg13+2)".
    pub version: String,
    /// `host:port`; empty over a Unix socket.
    pub address: String,
    pub database: String,
    pub uptime_secs: f64,
    /// A standby in recovery: read-only, replaying its primary's WAL.
    pub standby: bool,
    pub max_connections: i64,
    /// Kept for superusers.
    pub reserved_connections: i64,
    pub shared_buffers: f64,
    /// Superusers and members of `pg_read_all_stats` see every session's
    /// statement.
    pub sees_all: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Connections {
    /// Client sessions, besides the dashboard's.
    pub total: i64,
    pub active: i64,
    /// `idle in transaction` (also aborted): holding locks and snapshots.
    pub idle_in_transaction: i64,
    /// Active, waiting for a lock.
    pub waiting: i64,
    pub max: i64,
    pub reserved: i64,
}

impl Connections {
    /// Connections in use of those ordinary users may open, in percent.
    pub fn used_pct(&self) -> f64 {
        100.0 * self.total as f64 / (self.max - self.reserved).max(1) as f64
    }
}

/// A client session (`pg_stat_activity`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Backend {
    pub pid: i64,
    pub user: Option<String>,
    pub database: Option<String>,
    pub application: String,
    /// Host name or address; `local` over a Unix socket.
    pub client: String,
    /// `active`, `idle`, `idle in transaction`, …; none for another user's
    /// session without `pg_read_all_stats`.
    pub state: Option<String>,
    /// `Lock: transactionid`, `IO: DataFileRead`, …
    pub wait: Option<String>,
    pub query_secs: Option<f64>,
    pub transaction_secs: Option<f64>,
    /// Since the state last changed: how long it has been idle.
    pub state_secs: Option<f64>,
    /// The statement it runs, or (not active) ran last; none when hidden.
    pub query: Option<String>,
}

impl Backend {
    pub fn idle_in_transaction(&self) -> bool {
        self.state
            .as_deref()
            .is_some_and(|s| s.starts_with("idle in transaction"))
    }
}

/// A session waiting for a lock another one holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LockWait {
    pub waiting_pid: i64,
    pub waiting_user: Option<String>,
    pub wait_secs: f64,
    pub waiting_query: Option<String>,
    pub blocking_pid: i64,
    pub blocking_user: Option<String>,
    pub blocking_state: Option<String>,
    pub blocking_query: Option<String>,
    /// The table involved, if any.
    pub relation: Option<String>,
}

/// A standby streaming from this server (`pg_stat_replication`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Replica {
    pub name: String,
    pub client: String,
    /// `streaming`, `catchup`, `startup`, …
    pub state: Option<String>,
    /// `async`, `sync`, `potential`, `quorum`.
    pub sync_state: Option<String>,
    pub lag_secs: Option<f64>,
    /// WAL written here and not yet replayed there.
    pub lag_bytes: Option<f64>,
}

/// This server as a standby: its WAL receiver.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Receiver {
    /// None: no receiver runs.
    pub status: Option<String>,
    pub sender: Option<String>,
    /// How far replay is behind the primary's last commit.
    pub lag_secs: Option<f64>,
}

/// A replication slot and the WAL it keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Slot {
    pub name: String,
    /// `physical` or `logical`.
    pub kind: String,
    pub active: bool,
    pub retained_bytes: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatabaseSize {
    pub name: String,
    /// None without CONNECT on it.
    pub bytes: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableSize {
    pub schema: String,
    pub name: String,
    /// With indexes and TOAST.
    pub total_bytes: f64,
    pub live_rows: i64,
    pub dead_rows: i64,
    /// Since the last (auto)vacuum; none if it never ran.
    pub vacuumed_secs_ago: Option<f64>,
}

impl TableSize {
    pub fn dead_pct(&self) -> f64 {
        100.0 * self.dead_rows as f64 / (self.live_rows + self.dead_rows).max(1) as f64
    }
}

/// Table statistics, refreshed less often than the rest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    pub databases: Vec<DatabaseSize>,
    /// The current database's largest tables.
    pub tables: Vec<TableSize>,
    /// Its tables with many dead rows, most first.
    pub dead_rows: Vec<TableSize>,
}

/// A normalized statement and what its executions cost since the
/// statistics were reset (`pg_stat_statements`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopStatement {
    pub query_id: String,
    pub database: Option<String>,
    pub user: Option<String>,
    /// Constants replaced by `$1`, `$2`, …
    pub text: String,
    pub calls: i64,
    pub total_secs: f64,
    pub rows: i64,
    pub blocks_hit: i64,
    pub blocks_read: i64,
    pub temp_blocks_written: i64,
}

#[derive(Debug, Clone, Default)]
pub struct PgSnapshot {
    pub server: Option<ServerInfo>,
    pub counters: Option<Counters>,
    pub connections: Option<Connections>,
    pub by_user: Vec<(String, i64)>,
    pub by_application: Vec<(String, i64)>,
    pub by_client: Vec<(String, i64)>,
    pub by_database: Vec<(String, i64)>,
    /// Active sessions by wait event type (`CPU` when not waiting).
    pub activity: Vec<(String, i64)>,
    /// Active sessions, longest running first.
    pub running: Vec<Backend>,
    /// Open transactions (all of them, not just those listed).
    pub open_transactions: Option<i64>,
    /// The oldest open transactions.
    pub transactions: Vec<Backend>,
    pub lock_waits: Vec<LockWait>,
    pub autovacuum_workers: i64,
    pub replicas: Vec<Replica>,
    pub receiver: Option<Receiver>,
    pub slots: Vec<Slot>,
    /// The database closest to transaction ID wraparound, and its age.
    pub oldest_xid: Option<(String, i64)>,
    /// Only on the snapshots that read table statistics.
    pub storage: Option<Storage>,
    pub top_statements: Vec<TopStatement>,
    /// Why there are no top statements (the extension isn't there).
    pub statements_missing: Option<String>,
    /// Without `pg_read_all_stats`, other users' statements are hidden.
    pub own_sessions_only: bool,
    pub errors: Vec<SectionError>,
}

/// What `pg_read_all_stats` gives, for the notes.
pub const STATS_ROLE: &str = "pg_read_all_stats or pg_monitor";

/// Collects everything the dashboard shows; never fails as a whole. Table
/// statistics only with `storage`: they cost more on databases with many
/// tables.
pub fn snapshot(c: &mut dyn Client, storage: bool) -> PgSnapshot {
    let mut s = PgSnapshot::default();
    let mut errors = Vec::new();
    let mut section = |name: &'static str, needs: &str, r: Result<(), DbError>| {
        if let Err(e) = r {
            errors.push((name, explain(&e, needs)));
        }
    };
    section("Server", "", server(c, &mut s));
    section("Statistics", "", counters(c, &mut s));
    section("Sessions", "", sessions(c, &mut s));
    if s.connections.is_none_or(|c| c.waiting > 0) {
        section("Lock waits", "", lock_waits(c, &mut s));
    }
    // Most servers don't replicate: no need to ask for the privilege.
    if let Err(e) = replication(c, &mut s)
        && !is_privilege_error(&e)
    {
        section("Replication", "", Err(e));
    }
    section("Transaction IDs", "", oldest_xid(c, &mut s));
    if storage {
        section("Storage", "", storage_stats(c, &mut s));
    }
    section("Top statements", STATS_ROLE, top_statements(c, &mut s));
    s.errors = errors;
    s
}

/// The SQLSTATE of a server error: its message reads `ERROR 42501: …`.
fn sqlstate(e: &DbError) -> Option<&str> {
    let code = e.message.split_whitespace().nth(1)?.strip_suffix(':')?;
    (code.len() == 5).then_some(code)
}

fn is_privilege_error(e: &DbError) -> bool {
    sqlstate(e) == Some("42501")
}

/// The error, and for a privilege error what the user would need.
pub fn explain(e: &DbError, needs: &str) -> String {
    if is_privilege_error(e) && !needs.is_empty() {
        format!(
            "Not accessible with the current privileges (needs {needs}). {}",
            e.message
        )
    } else {
        e.message.clone()
    }
}

fn flag(row: &[Cell], i: usize) -> bool {
    opt(row, i).as_deref() == Some("t")
}

fn float(row: &[Cell], i: usize) -> Option<f64> {
    opt(row, i).and_then(|v| v.parse().ok())
}

fn server(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        "select current_setting('server_version'), \
                coalesce(host(inet_server_addr()) || ':' || inet_server_port(), ''), \
                current_database(), \
                extract(epoch from now() - pg_postmaster_start_time()), \
                pg_is_in_recovery(), \
                current_setting('max_connections'), \
                current_setting('superuser_reserved_connections'), \
                pg_size_bytes(current_setting('shared_buffers')), \
                pg_has_role('pg_read_all_stats', 'member')",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(());
    };
    s.server = Some(ServerInfo {
        version: text(r, 0),
        address: text(r, 1),
        database: text(r, 2),
        uptime_secs: num(r, 3),
        standby: flag(r, 4),
        max_connections: int(r, 5),
        reserved_connections: int(r, 6),
        shared_buffers: num(r, 7),
        sees_all: flag(r, 8),
    });
    Ok(())
}

fn counters(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    // In the order of `Counters`.
    let rows = c.query(
        "select sum(xact_commit), sum(xact_rollback), sum(blks_read), sum(blks_hit), \
                sum(tup_returned), sum(tup_fetched), sum(tup_inserted), sum(tup_updated), \
                sum(tup_deleted), sum(temp_files), sum(temp_bytes), sum(deadlocks), \
                sum(conflicts), \
                pg_wal_lsn_diff(case when pg_is_in_recovery() then pg_last_wal_replay_lsn() \
                                     else pg_current_wal_lsn() end, '0/0') \
           from pg_stat_database",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(());
    };
    let mut counters = Counters::from_row(r);
    // PostgreSQL 17 moved them to pg_stat_checkpointer.
    let checkpoints = c
        .query(
            "select num_timed, num_requested from pg_stat_checkpointer",
            &[],
        )
        .or_else(|e| match sqlstate(&e) {
            Some("42P01") => c.query(
                "select checkpoints_timed, checkpoints_req from pg_stat_bgwriter",
                &[],
            ),
            _ => Err(e),
        })?;
    if let Some(r) = checkpoints.first() {
        counters.checkpoints_timed = num(r, 0);
        counters.checkpoints_requested = num(r, 1);
    }
    s.counters = Some(counters);
    Ok(())
}

/// Shown for another user's statement without `pg_read_all_stats`.
const HIDDEN: &str = "<insufficient privilege>";

fn sessions(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        "select pid, usename, datname, application_name, \
                coalesce(client_hostname, host(client_addr), 'local'), \
                state, wait_event_type || ': ' || wait_event, \
                extract(epoch from now() - query_start), \
                extract(epoch from now() - xact_start), \
                extract(epoch from now() - state_change), \
                query, backend_type \
           from pg_stat_activity \
          where pid <> pg_backend_pid()",
        &[],
    )?;
    let mut clients = Vec::new();
    for r in &rows {
        match opt(r, 11).as_deref() {
            Some("client backend") => {}
            Some("autovacuum worker") => {
                s.autovacuum_workers += 1;
                continue;
            }
            Some(_) => continue,
            // Hidden with the statement without pg_read_all_stats: a
            // client's has its user, the server's own processes have none.
            None if opt(r, 1).is_some() => {}
            None => continue,
        }
        let query = opt(r, 10).filter(|q| q.as_str() != HIDDEN && !q.is_empty());
        if opt(r, 10).as_deref() == Some(HIDDEN) {
            s.own_sessions_only = true;
        }
        clients.push(Backend {
            pid: int(r, 0),
            user: opt(r, 1),
            database: opt(r, 2),
            application: text(r, 3),
            client: text(r, 4),
            state: opt(r, 5),
            wait: opt(r, 6),
            query_secs: float(r, 7),
            transaction_secs: float(r, 8),
            state_secs: float(r, 9),
            query,
        });
    }
    let (max, reserved) = s
        .server
        .as_ref()
        .map_or((0, 0), |i| (i.max_connections, i.reserved_connections));
    let active = |b: &&Backend| b.state.as_deref() == Some("active");
    s.connections = Some(Connections {
        total: clients.len() as i64,
        active: clients.iter().filter(active).count() as i64,
        idle_in_transaction: clients.iter().filter(|b| b.idle_in_transaction()).count() as i64,
        waiting: clients
            .iter()
            .filter(active)
            .filter(|b| b.wait.as_deref().is_some_and(|w| w.starts_with("Lock:")))
            .count() as i64,
        max,
        reserved,
    });
    let named = |v: &Option<String>| v.clone().unwrap_or_else(|| "(unknown)".into());
    s.by_user = top_counts(clients.iter().map(|b| named(&b.user)));
    s.by_application = top_counts(clients.iter().map(|b| {
        if b.application.is_empty() {
            "(none)".into()
        } else {
            b.application.clone()
        }
    }));
    s.by_client = top_counts(clients.iter().map(|b| b.client.clone()));
    s.by_database = top_counts(clients.iter().map(|b| named(&b.database)));
    s.activity = all_counts(clients.iter().filter(active).map(|b| {
        b.wait
            .as_deref()
            .and_then(|w| w.split_once(':'))
            .map_or("CPU".to_owned(), |(kind, _)| kind.to_owned())
    }));
    let longest = |a: &Option<f64>, b: &Option<f64>| {
        b.unwrap_or(0.0)
            .partial_cmp(&a.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    };
    let mut running: Vec<Backend> = clients.iter().filter(active).cloned().collect();
    running.sort_by(|a, b| longest(&a.query_secs, &b.query_secs));
    running.truncate(10);
    s.running = running;
    let mut transactions: Vec<Backend> = clients
        .into_iter()
        .filter(|b| b.transaction_secs.is_some())
        .collect();
    s.open_transactions = Some(transactions.len() as i64);
    transactions.sort_by(|a, b| longest(&a.transaction_secs, &b.transaction_secs));
    transactions.truncate(10);
    s.transactions = transactions;
    Ok(())
}

fn lock_waits(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    // pg_locks.waitstart came with PostgreSQL 14; before, the statement's
    // start stands in.
    let sql = |since: &str| {
        format!(
            "select w.pid, w.usename, extract(epoch from now() - {since}), w.query, \
                    b.pid, b.usename, b.state, b.query, \
                    (select r.relation::regclass::text from pg_locks r \
                      where r.pid = w.pid and r.relation is not null \
                      order by r.granted limit 1) \
               from pg_stat_activity w \
               join pg_locks l on l.pid = w.pid and not l.granted \
              cross join lateral unnest(pg_blocking_pids(w.pid)) as blocker(pid) \
               join pg_stat_activity b on b.pid = blocker.pid \
              order by 3 desc \
              limit 20"
        )
    };
    let rows = c
        .query(&sql("coalesce(l.waitstart, w.query_start)"), &[])
        .or_else(|e| match sqlstate(&e) {
            Some("42703") => c.query(&sql("w.query_start"), &[]),
            _ => Err(e),
        })?;
    let shown = |r: &[Cell], i: usize| opt(r, i).filter(|q| q.as_str() != HIDDEN);
    s.lock_waits = rows
        .iter()
        .map(|r| LockWait {
            waiting_pid: int(r, 0),
            waiting_user: opt(r, 1),
            wait_secs: num(r, 2),
            waiting_query: shown(r, 3),
            blocking_pid: int(r, 4),
            blocking_user: opt(r, 5),
            blocking_state: opt(r, 6),
            blocking_query: shown(r, 7),
            relation: opt(r, 8),
        })
        .collect();
    Ok(())
}

fn replication(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    if s.server.as_ref().is_some_and(|i| i.standby) {
        // Replay is caught up when it has applied all that was received,
        // however long ago the primary last committed.
        let rows = c.query(
            "select (select status from pg_stat_wal_receiver), \
                    (select sender_host || ':' || sender_port from pg_stat_wal_receiver), \
                    case when pg_last_wal_receive_lsn() = pg_last_wal_replay_lsn() then 0 \
                         else extract(epoch from now() - pg_last_xact_replay_timestamp()) end",
            &[],
        )?;
        s.receiver = rows.first().map(|r| Receiver {
            status: opt(r, 0),
            sender: opt(r, 1),
            lag_secs: float(r, 2),
        });
        return Ok(());
    }
    let replicas = c.query(
        "select application_name, coalesce(client_hostname, host(client_addr), 'local'), \
                state, sync_state, extract(epoch from replay_lag), \
                pg_wal_lsn_diff(pg_current_wal_lsn(), replay_lsn) \
           from pg_stat_replication \
          order by application_name",
        &[],
    )?;
    s.replicas = replicas
        .iter()
        .map(|r| Replica {
            name: text(r, 0),
            client: text(r, 1),
            state: opt(r, 2),
            sync_state: opt(r, 3),
            lag_secs: float(r, 4),
            lag_bytes: float(r, 5),
        })
        .collect();
    let slots = c.query(
        "select slot_name, slot_type, active, \
                coalesce(pg_wal_lsn_diff(pg_current_wal_lsn(), restart_lsn), 0) \
           from pg_replication_slots \
          order by 4 desc",
        &[],
    )?;
    s.slots = slots
        .iter()
        .map(|r| Slot {
            name: text(r, 0),
            kind: text(r, 1),
            active: flag(r, 2),
            retained_bytes: num(r, 3),
        })
        .collect();
    Ok(())
}

fn oldest_xid(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        "select datname, age(datfrozenxid) from pg_database order by 2 desc limit 1",
        &[],
    )?;
    s.oldest_xid = rows.first().map(|r| (text(r, 0), int(r, 1)));
    Ok(())
}

/// The schemas of the server itself, left out of the table lists.
const SYSTEM_SCHEMAS: &str = "n.nspname not in ('pg_catalog', 'information_schema') \
                              and n.nspname not like 'pg\\_toast%'";

fn storage_stats(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    let databases = c.query(
        "select datname, \
                case when has_database_privilege(datname, 'CONNECT') \
                     then pg_database_size(datname) end \
           from pg_database \
          where datallowconn and not datistemplate \
          order by 2 desc nulls last \
          limit 10",
        &[],
    )?;
    let table = |r: &[Cell]| TableSize {
        schema: text(r, 0),
        name: text(r, 1),
        total_bytes: num(r, 2),
        live_rows: int(r, 3),
        dead_rows: int(r, 4),
        vacuumed_secs_ago: float(r, 5),
    };
    let tables = c.query(
        &format!(
            "select n.nspname, c.relname, pg_total_relation_size(c.oid), \
                    s.n_live_tup, s.n_dead_tup, \
                    extract(epoch from now() - greatest(s.last_vacuum, s.last_autovacuum)) \
               from pg_class c \
               join pg_namespace n on n.oid = c.relnamespace \
               left join pg_stat_all_tables s on s.relid = c.oid \
              where c.relkind in ('r', 'm') and {SYSTEM_SCHEMAS} \
              order by 3 desc \
              limit 10"
        ),
        &[],
    )?;
    // Many dead rows, and a fair share of the table: (auto)vacuum doesn't
    // keep up, or something holds it back.
    let dead = c.query(
        "select schemaname, relname, pg_total_relation_size(relid), n_live_tup, n_dead_tup, \
                extract(epoch from now() - greatest(last_vacuum, last_autovacuum)) \
           from pg_stat_user_tables \
          where n_dead_tup >= 10000 and n_dead_tup > 0.2 * (n_live_tup + n_dead_tup) \
          order by n_dead_tup desc \
          limit 5",
        &[],
    )?;
    s.storage = Some(Storage {
        databases: databases
            .iter()
            .map(|r| DatabaseSize {
                name: text(r, 0),
                bytes: float(r, 1),
            })
            .collect(),
        tables: tables.iter().map(|r| table(r)).collect(),
        dead_rows: dead.iter().map(|r| table(r)).collect(),
    });
    Ok(())
}

fn top_statements(c: &mut dyn Client, s: &mut PgSnapshot) -> Result<(), DbError> {
    // total_exec_time since PostgreSQL 13. The dashboard's own lookups,
    // other statistics queries and Pythia's session setup are left out: it
    // is about the application's load.
    let sql = |total: &str| {
        format!(
            "select s.queryid, d.datname, r.rolname, s.query, s.calls, s.{total} / 1000, \
                    s.rows, s.shared_blks_hit, s.shared_blks_read, s.temp_blks_written \
               from pg_stat_statements s \
               left join pg_database d on d.oid = s.dbid \
               left join pg_roles r on r.oid = s.userid \
              where s.query !~ \
                    'pg_(stat|locks|database|replication|class|roles|namespace|postmaster)\
                     |inet_server_addr|current_schema\\(\\)' \
              order by s.{total} desc \
              limit 10"
        )
    };
    let rows = c
        .query(&sql("total_exec_time"), &[])
        .or_else(|e| match sqlstate(&e) {
            Some("42703") => c.query(&sql("total_time"), &[]),
            _ => Err(e),
        });
    let rows = match rows {
        Ok(rows) => rows,
        // Not created in this database, or not loaded at server start.
        Err(e) if sqlstate(&e) == Some("42P01") => {
            s.statements_missing = Some(
                "Top statements need the pg_stat_statements extension: load it with \
                 shared_preload_libraries and run CREATE EXTENSION pg_stat_statements."
                    .into(),
            );
            return Ok(());
        }
        Err(e) if sqlstate(&e) == Some("55000") => {
            s.statements_missing = Some(
                "pg_stat_statements is installed but not loaded: add it to \
                 shared_preload_libraries and restart the server."
                    .into(),
            );
            return Ok(());
        }
        Err(e) => return Err(e),
    };
    s.top_statements = rows
        .iter()
        .map(|r| TopStatement {
            query_id: text(r, 0),
            database: opt(r, 1),
            user: opt(r, 2),
            text: text(r, 3),
            calls: int(r, 4),
            total_secs: num(r, 5),
            rows: int(r, 6),
            blocks_hit: int(r, 7),
            blocks_read: int(r, 8),
            temp_blocks_written: int(r, 9),
        })
        .collect();
    Ok(())
}

/// Transaction IDs run out at 2^31: VACUUM must freeze old rows before.
const WRAPAROUND: f64 = 2_147_483_648.0;

/// Findings, most severe first; empty means all is well. `rates` are the
/// counters per second since the previous snapshot.
pub fn insights(snap: &PgSnapshot, rates: Option<&Counters>) -> Vec<Insight> {
    let mut out = Vec::new();
    let mut add = |severity, text: String| {
        out.push(Insight {
            severity,
            text,
            target: None,
            session: None,
        })
    };
    let who = |pid: i64, user: &Option<String>| match user {
        Some(u) => format!("Session {pid} ({u})"),
        None => format!("Session {pid}"),
    };

    // Lock waits by blocking session.
    let mut blockers: Vec<(&LockWait, i64, f64)> = Vec::new();
    for w in &snap.lock_waits {
        match blockers
            .iter_mut()
            .find(|b| b.0.blocking_pid == w.blocking_pid)
        {
            Some(b) => {
                b.1 += 1;
                b.2 = b.2.max(w.wait_secs);
            }
            None => blockers.push((w, 1, w.wait_secs)),
        }
    }
    for (w, blocked, wait) in &blockers {
        let idle = w
            .blocking_state
            .as_deref()
            .is_some_and(|s| s.starts_with("idle in transaction"));
        add(
            if *wait >= 10.0 {
                Severity::Critical
            } else {
                Severity::Warning
            },
            format!(
                "{} blocks {}, longest wait {}{}",
                who(w.blocking_pid, &w.blocking_user),
                plural(*blocked, "session"),
                duration(*wait),
                if idle {
                    " — idle in transaction: it doesn't commit or roll back"
                } else {
                    ""
                }
            ),
        );
    }
    // Idle in transaction without blocking anyone (yet).
    if let Some(b) = snap.transactions.iter().find(|b| {
        b.idle_in_transaction()
            && b.state_secs.is_some_and(|s| s >= 60.0)
            && !blockers.iter().any(|(w, ..)| w.blocking_pid == b.pid)
    }) {
        add(
            Severity::Warning,
            format!(
                "{} has been idle in transaction for {}: it holds its locks and keeps \
                 VACUUM from removing dead rows",
                who(b.pid, &b.user),
                duration(b.state_secs.unwrap_or(0.0))
            ),
        );
    }

    if let Some(r) = &snap.receiver {
        match &r.status {
            None => add(
                Severity::Critical,
                "This standby receives no WAL from its primary".into(),
            ),
            Some(status) if status != "streaming" => add(
                Severity::Warning,
                format!("This standby's WAL receiver is {status}"),
            ),
            _ => {}
        }
        if let Some(lag) = r.lag_secs.filter(|l| *l >= 30.0) {
            add(
                if lag >= 300.0 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                format!("This standby is {} behind its primary", duration(lag)),
            );
        }
    }
    for r in &snap.replicas {
        let name = if r.name.is_empty() {
            &r.client
        } else {
            &r.name
        };
        if r.state.as_deref().is_some_and(|s| s != "streaming") {
            add(
                Severity::Warning,
                format!(
                    "Standby {name} is {}, not streaming",
                    r.state.as_deref().unwrap_or("?")
                ),
            );
        } else if let Some(lag) = r.lag_secs.filter(|l| *l >= 30.0) {
            add(
                if lag >= 300.0 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                format!("Standby {name} is {} behind", duration(lag)),
            );
        }
    }
    for slot in snap
        .slots
        .iter()
        .filter(|s| !s.active && s.retained_bytes >= 1024.0 * 1024.0 * 1024.0)
    {
        add(
            Severity::Warning,
            format!(
                "Replication slot {} is inactive and keeps {} of WAL from being removed",
                slot.name,
                bytes(slot.retained_bytes)
            ),
        );
    }
    if let Some((database, age)) = &snap.oldest_xid {
        let used = *age as f64 / WRAPAROUND;
        if used >= 0.45 {
            add(
                if used >= 0.7 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                format!(
                    "Database {database} is {:.0} % of the way to transaction ID \
                     wraparound: VACUUM (FREEZE) it, and check what holds autovacuum back",
                    used * 100.0
                ),
            );
        }
    }

    if let Some(c) = snap.connections.filter(|c| c.max > 0) {
        let used = c.used_pct();
        if used >= 75.0 {
            add(
                if used >= 90.0 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                format!(
                    "{} of {} connections in use ({used:.0} %)",
                    c.total,
                    c.max - c.reserved
                ),
            );
        }
    }
    if let Some(r) = rates {
        if r.deadlocks > 0.0 {
            add(
                Severity::Warning,
                format!("Deadlocks: {:.1} per minute", r.deadlocks * 60.0),
            );
        }
        if let Some(hit) = r
            .hit_ratio()
            .filter(|_| r.blocks_read + r.blocks_hit >= 100.0)
            && hit < 95.0
        {
            add(
                Severity::Warning,
                format!(
                    "Cache hit ratio is {hit:.1} %: {} blocks per second are read from disk",
                    compact(r.blocks_read)
                ),
            );
        }
        if r.temp_bytes >= 10.0 * 1024.0 * 1024.0 {
            add(
                Severity::Info,
                format!(
                    "Queries write {} per second to temporary files: work_mem may be too small",
                    bytes(r.temp_bytes)
                ),
            );
        }
        if r.conflicts > 0.0 {
            add(
                Severity::Info,
                "Queries on this standby are cancelled for conflicts with recovery".into(),
            );
        }
    }
    if let Some(c) = snap.counters.filter(|c| {
        c.checkpoints_requested >= 10.0 && c.checkpoints_requested > c.checkpoints_timed
    }) {
        add(
            Severity::Info,
            format!(
                "Most checkpoints ({:.0} of {:.0}) start because WAL reached max_wal_size, \
                 not on schedule: max_wal_size may be too small",
                c.checkpoints_requested,
                c.checkpoints_requested + c.checkpoints_timed
            ),
        );
    }
    if let Some(b) = snap
        .running
        .first()
        .filter(|b| b.query_secs.is_some_and(|s| s >= 60.0) && b.query.is_some())
    {
        add(
            Severity::Info,
            format!(
                "{} has run a statement for {}",
                who(b.pid, &b.user),
                duration(b.query_secs.unwrap_or(0.0))
            ),
        );
    }
    if let Some(t) = snap.storage.as_ref().and_then(|s| s.dead_rows.first()) {
        let vacuumed = t.vacuumed_secs_ago.map_or("never vacuumed".into(), |s| {
            format!("last vacuumed {} ago", duration(s))
        });
        add(
            Severity::Info,
            format!(
                "Table {}.{} has {} dead rows ({:.0} %), {vacuumed}",
                t.schema,
                t.name,
                compact(t.dead_rows as f64),
                t.dead_pct()
            ),
        );
    }
    if snap.server.as_ref().is_some_and(|s| s.standby) {
        add(
            Severity::Info,
            "This server is a standby (read-only)".into(),
        );
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.severity));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_ratios() {
        let row: Vec<Cell> = ["100", "4", "50", "950", "2000", "300", "10", "5", "1"]
            .iter()
            .map(|v| Some((*v).to_owned()))
            .collect();
        let before = Counters::from_row(&row);
        assert_eq!(before.rows_changed(), 16.0);
        assert_eq!(before.hit_ratio(), Some(95.0));
        let after = Counters {
            commits: 600.0,
            blocks_read: 250.0,
            blocks_hit: 1750.0,
            ..before
        };
        let r = Counters::per_second(&before, &after, 10.0);
        assert_eq!(r.commits, 50.0);
        assert_eq!(r.hit_ratio(), Some(80.0));
        // A restarted server's counters start over.
        assert_eq!(Counters::per_second(&after, &before, 10.0).commits, 0.0);
        assert_eq!(
            sqlstate(&DbError::new("ERROR 42501: permission denied for view x")),
            Some("42501")
        );
        assert_eq!(sqlstate(&DbError::new("PostgreSQL: closed")), None);
    }

    #[test]
    fn findings() {
        let snap = PgSnapshot {
            connections: Some(Connections {
                total: 85,
                active: 3,
                max: 100,
                reserved: 3,
                ..Default::default()
            }),
            lock_waits: vec![
                LockWait {
                    waiting_pid: 12,
                    wait_secs: 15.0,
                    blocking_pid: 7,
                    blocking_user: Some("app".into()),
                    blocking_state: Some("idle in transaction".into()),
                    ..Default::default()
                },
                LockWait {
                    waiting_pid: 13,
                    wait_secs: 4.0,
                    blocking_pid: 7,
                    blocking_user: Some("app".into()),
                    blocking_state: Some("idle in transaction".into()),
                    ..Default::default()
                },
            ],
            transactions: vec![
                Backend {
                    pid: 7,
                    state: Some("idle in transaction".into()),
                    state_secs: Some(300.0),
                    transaction_secs: Some(310.0),
                    ..Default::default()
                },
                Backend {
                    pid: 9,
                    user: Some("report".into()),
                    state: Some("idle in transaction".into()),
                    state_secs: Some(125.0),
                    transaction_secs: Some(130.0),
                    ..Default::default()
                },
            ],
            oldest_xid: Some(("shop".into(), 1_100_000_000)),
            slots: vec![Slot {
                name: "old_replica".into(),
                kind: "physical".into(),
                active: false,
                retained_bytes: 5.0 * 1024.0 * 1024.0 * 1024.0,
            }],
            ..Default::default()
        };
        let rates = Counters {
            blocks_read: 200.0,
            blocks_hit: 800.0,
            ..Default::default()
        };
        let texts: Vec<(Severity, String)> = insights(&snap, Some(&rates))
            .into_iter()
            .map(|i| (i.severity, i.text))
            .collect();
        assert_eq!(
            texts,
            [
                (
                    Severity::Critical,
                    "Session 7 (app) blocks 2 sessions, longest wait 15s — idle in \
                     transaction: it doesn't commit or roll back"
                        .into()
                ),
                (
                    Severity::Warning,
                    "Session 9 (report) has been idle in transaction for 2m 5s: it holds its \
                     locks and keeps VACUUM from removing dead rows"
                        .into()
                ),
                (
                    Severity::Warning,
                    "Replication slot old_replica is inactive and keeps 5.0 GB of WAL from \
                     being removed"
                        .into()
                ),
                (
                    Severity::Warning,
                    "Database shop is 51 % of the way to transaction ID wraparound: VACUUM \
                     (FREEZE) it, and check what holds autovacuum back"
                        .into()
                ),
                (
                    Severity::Warning,
                    "85 of 97 connections in use (88 %)".into()
                ),
                (
                    Severity::Warning,
                    "Cache hit ratio is 80.0 %: 200 blocks per second are read from disk".into()
                ),
            ]
        );
        assert!(insights(&PgSnapshot::default(), None).is_empty());
        let standby = PgSnapshot {
            server: Some(ServerInfo {
                standby: true,
                ..Default::default()
            }),
            receiver: Some(Receiver {
                status: Some("streaming".into()),
                lag_secs: Some(400.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let texts: Vec<String> = insights(&standby, None)
            .into_iter()
            .map(|i| i.text)
            .collect();
        assert_eq!(
            texts,
            [
                "This standby is 6m 40s behind its primary",
                "This server is a standby (read-only)"
            ]
        );
    }
}
