//! The MySQL dashboard's data: server counters, connections, InnoDB,
//! transactions and lock waits, storage and top statements, read through
//! [`pythia_db::Client`] (the session's `DbRequest::Run`).
//!
//! It comes from `SHOW GLOBAL STATUS` and `VARIABLES`, `information_schema`
//! and `performance_schema`. Sections the user can't read are left out with
//! the privilege they need, as on the Oracle dashboard. Without `PROCESS`,
//! MySQL only lists the user's own threads.

use std::collections::HashMap;

use pythia_db::{Cell, Client, DbError};

use crate::insights::{Insight, Severity};
use crate::queries::SectionError;
use crate::ui::style::{bytes, duration};

macro_rules! counters {
    ($($(#[$doc:meta])* $field:ident: $($var:literal)+;)*) => {
        /// Cumulative counters from `SHOW GLOBAL STATUS`. Between two
        /// snapshots they become rates ([`Counters::per_second`]).
        #[derive(Debug, Clone, Copy, Default, PartialEq)]
        pub struct Counters {
            $($(#[$doc])* pub $field: f64,)*
        }

        impl Counters {
            fn from_status(status: &Status) -> Self {
                Self {
                    $($field: 0.0 $(+ status.num($var))+,)*
                }
            }

            /// Each counter's change per second from `prev` to `cur`.
            pub fn per_second(prev: &Self, cur: &Self, seconds: f64) -> Self {
                if seconds <= 0.0 {
                    return Self::default();
                }
                // Counters start over when the server restarts.
                Self {
                    $($field: (cur.$field - prev.$field).max(0.0) / seconds,)*
                }
            }
        }
    };
}

counters! {
    /// Statements sent by clients.
    queries: "questions";
    selects: "com_select";
    /// INSERT, UPDATE, DELETE and REPLACE statements.
    changes: "com_insert" "com_insert_select" "com_update" "com_update_multi"
        "com_delete" "com_delete_multi" "com_replace" "com_replace_select";
    /// Explicit COMMITs (autocommitted statements aren't counted).
    commits: "com_commit";
    rollbacks: "com_rollback";
    bytes_in: "bytes_received";
    bytes_out: "bytes_sent";
    /// InnoDB page reads from the buffer pool.
    logical_reads: "innodb_buffer_pool_read_requests";
    /// Of those, reads that had to go to disk.
    physical_reads: "innodb_buffer_pool_reads";
    rows_read: "innodb_rows_read";
    rows_changed: "innodb_rows_inserted" "innodb_rows_updated" "innodb_rows_deleted";
    slow_queries: "slow_queries";
    tmp_tables: "created_tmp_tables";
    tmp_disk_tables: "created_tmp_disk_tables";
    row_lock_waits: "innodb_row_lock_waits";
    /// Failed connection attempts (wrong password, no privilege, timeouts).
    aborted_connects: "aborted_connects";
    /// Connections refused because `max_connections` was reached.
    refused_connections: "connection_errors_max_connections";
    /// Queries that scanned a whole table or joined without an index.
    full_scans: "select_scan" "select_full_join";
    /// Bytes written to InnoDB's redo log.
    log_bytes: "innodb_os_log_written";
}

impl Counters {
    /// Share of InnoDB page reads served from memory, in percent.
    pub fn hit_ratio(&self) -> Option<f64> {
        (self.logical_reads > 0.0)
            .then(|| 100.0 * (1.0 - self.physical_reads / self.logical_reads).max(0.0))
    }

    /// Share of internal temporary tables that went to disk, in percent.
    pub fn tmp_disk_pct(&self) -> Option<f64> {
        (self.tmp_tables > 0.0).then(|| 100.0 * self.tmp_disk_tables / self.tmp_tables)
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerInfo {
    pub hostname: String,
    pub port: String,
    pub version: String,
    /// E.g. "MySQL Community Server - GPL".
    pub comment: String,
    pub uptime_secs: f64,
    pub read_only: bool,
    pub slow_query_log: bool,
    pub long_query_time: f64,
    pub performance_schema: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Connections {
    pub connected: i64,
    /// Threads executing a statement (not sleeping).
    pub running: i64,
    pub max_used: i64,
    pub max_connections: i64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BufferPool {
    pub size_bytes: f64,
    pub pages_total: f64,
    pub pages_free: f64,
    pub pages_dirty: f64,
}

impl BufferPool {
    /// Pages in use, in percent.
    pub fn used_pct(&self) -> f64 {
        100.0 * (1.0 - self.pages_free / self.pages_total.max(1.0))
    }

    pub fn dirty_pct(&self) -> f64 {
        100.0 * self.pages_dirty / self.pages_total.max(1.0)
    }
}

/// A client thread running a statement.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Thread {
    pub id: i64,
    pub user: String,
    pub host: String,
    pub db: Option<String>,
    pub command: String,
    pub time_secs: i64,
    pub state: String,
    pub query: Option<String>,
}

/// An open InnoDB transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transaction {
    pub thread: i64,
    pub user: Option<String>,
    /// `RUNNING`, `LOCK WAIT`, …
    pub state: String,
    pub age_secs: i64,
    pub rows_locked: i64,
    pub rows_modified: i64,
    /// The statement it runs; none while the client is idle.
    pub query: Option<String>,
    /// The thread is idle (`Sleep`) with the transaction open.
    pub idle: bool,
}

/// A transaction waiting for a row lock another one holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LockWait {
    pub waiting_thread: i64,
    pub wait_secs: i64,
    pub waiting_query: Option<String>,
    pub blocking_thread: i64,
    /// None while the blocking client is idle: likely an uncommitted
    /// transaction.
    pub blocking_query: Option<String>,
    pub table: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Replica {
    pub channel: String,
    pub source: String,
    /// Receiver and applier threads.
    pub io_running: bool,
    pub sql_running: bool,
    /// How long ago the source committed what is being applied.
    pub lag_secs: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SchemaSize {
    pub schema: String,
    pub tables: i64,
    pub data_bytes: f64,
    pub index_bytes: f64,
    /// Allocated but unused (reclaimable with OPTIMIZE TABLE).
    pub free_bytes: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableSize {
    pub schema: String,
    pub name: String,
    pub engine: Option<String>,
    /// InnoDB's estimate.
    pub rows: i64,
    pub data_bytes: f64,
    pub index_bytes: f64,
}

/// Table statistics, refreshed less often than the rest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    pub schemas: Vec<SchemaSize>,
    pub tables: Vec<TableSize>,
}

/// A normalized statement and what its executions cost since the server
/// started (or the statistics were reset).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopStatement {
    pub schema: Option<String>,
    pub digest: String,
    /// Literals replaced by `?`.
    pub text: String,
    pub executions: i64,
    pub total_secs: f64,
    pub lock_secs: f64,
    pub rows_examined: i64,
    pub rows_sent: i64,
    pub rows_affected: i64,
    /// Executions that used no (good) index.
    pub no_index: i64,
    pub tmp_disk_tables: i64,
}

#[derive(Debug, Clone, Default)]
pub struct MySqlSnapshot {
    pub server: Option<ServerInfo>,
    pub counters: Option<Counters>,
    pub connections: Option<Connections>,
    pub buffer_pool: Option<BufferPool>,
    pub by_user: Vec<(String, i64)>,
    pub by_host: Vec<(String, i64)>,
    pub by_db: Vec<(String, i64)>,
    /// Running threads by state (`executing`, `Waiting for table metadata
    /// lock`, …).
    pub activity: Vec<(String, i64)>,
    /// Running statements, longest first.
    pub running: Vec<Thread>,
    /// Open InnoDB transactions (all of them, not just those listed).
    pub open_transactions: Option<i64>,
    /// The oldest open transactions.
    pub transactions: Vec<Transaction>,
    pub lock_waits: Vec<LockWait>,
    pub replicas: Vec<Replica>,
    /// Only on the snapshots that read table statistics.
    pub storage: Option<Storage>,
    pub top_statements: Vec<TopStatement>,
    /// Without `PROCESS`, only the user's own threads are listed.
    pub own_threads_only: bool,
    pub errors: Vec<SectionError>,
}

/// Collects everything the dashboard shows; never fails as a whole. Table
/// statistics only with `storage`: they cost more on servers with many
/// tables.
pub fn snapshot(c: &mut dyn Client, storage: bool) -> MySqlSnapshot {
    let mut s = MySqlSnapshot::default();
    let mut errors = Vec::new();
    let mut section = |name: &'static str, needs: &str, r: Result<(), DbError>| {
        if let Err(e) = r {
            errors.push((name, explain(&e, needs)));
        }
    };
    section("Server status", "", status(c, &mut s));
    let threads = threads(c, &mut s);
    let threads_listed = threads.is_ok();
    section("Threads", "", threads);
    let transactions = transactions(c, &mut s);
    if transactions.as_ref().is_err_and(is_privilege_error) {
        s.own_threads_only = true;
    }
    section("Transactions", "the PROCESS privilege", transactions);
    if threads_listed
        && !s.own_threads_only
        && let Some(connections) = &mut s.connections
    {
        // Client threads only: no replication or scheduler threads.
        connections.running = s.activity.iter().map(|(_, n)| n).sum();
    }
    if s.transactions.iter().any(|t| t.state == "LOCK WAIT") {
        section(
            "Lock waits",
            "SELECT on performance_schema",
            lock_waits(c, &mut s),
        );
    }
    // Most servers aren't replicas: no need to ask for the privilege.
    if let Err(e) = replication(c, &mut s)
        && !is_privilege_error(&e)
    {
        section("Replication", "", Err(e));
    }
    if storage {
        section("Storage", "", storage_stats(c, &mut s));
    }
    if s.server.as_ref().is_none_or(|i| i.performance_schema) {
        section(
            "Top statements",
            "SELECT on performance_schema",
            top_statements(c, &mut s),
        );
    }
    s.errors = errors;
    s
}

fn is_privilege_error(e: &DbError) -> bool {
    // Access denied to a database, a table, a column; a missing privilege.
    matches!(e.code, Some(1044 | 1142 | 1143 | 1227))
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

/// `SHOW` output as name → value, names in lowercase.
struct Status(HashMap<String, String>);

impl Status {
    fn read(c: &mut dyn Client, sql: &str) -> Result<Self, DbError> {
        Ok(Self(
            c.query(sql, &[])?
                .into_iter()
                .filter_map(|row| Some((text(&row, 0).to_lowercase(), row.get(1)?.clone()?)))
                .collect(),
        ))
    }

    fn get(&self, name: &str) -> &str {
        self.0.get(name).map_or("", String::as_str)
    }

    fn num(&self, name: &str) -> f64 {
        self.get(name).parse().unwrap_or(0.0)
    }

    fn on(&self, name: &str) -> bool {
        matches!(self.get(name), "ON" | "1")
    }
}

fn status(c: &mut dyn Client, s: &mut MySqlSnapshot) -> Result<(), DbError> {
    let status = Status::read(c, "show global status")?;
    let vars = Status::read(
        c,
        "show global variables where variable_name in ('hostname', 'port', 'version', \
         'version_comment', 'read_only', 'super_read_only', 'max_connections', \
         'innodb_buffer_pool_size', 'slow_query_log', 'long_query_time', 'performance_schema', \
         'event_scheduler')",
    )?;
    s.server = Some(ServerInfo {
        hostname: vars.get("hostname").to_owned(),
        port: vars.get("port").to_owned(),
        version: vars.get("version").to_owned(),
        comment: vars.get("version_comment").to_owned(),
        uptime_secs: status.num("uptime"),
        read_only: vars.on("read_only") || vars.on("super_read_only"),
        slow_query_log: vars.on("slow_query_log"),
        long_query_time: vars.num("long_query_time"),
        performance_schema: vars.on("performance_schema"),
    });
    s.counters = Some(Counters::from_status(&status));
    s.connections = Some(Connections {
        connected: status.num("threads_connected") as i64,
        // Without the dashboard's own SHOW and the event scheduler's
        // thread; with the whole process list, `snapshot` counts there.
        running: (status.num("threads_running") as i64 - 1 - i64::from(vars.on("event_scheduler")))
            .max(0),
        max_used: status.num("max_used_connections") as i64,
        max_connections: vars.num("max_connections") as i64,
    });
    if status.0.contains_key("innodb_buffer_pool_pages_total") {
        s.buffer_pool = Some(BufferPool {
            size_bytes: vars.num("innodb_buffer_pool_size"),
            pages_total: status.num("innodb_buffer_pool_pages_total"),
            pages_free: status.num("innodb_buffer_pool_pages_free"),
            pages_dirty: status.num("innodb_buffer_pool_pages_dirty"),
        });
    }
    Ok(())
}

/// The replication, scheduler and other threads of the server itself.
fn is_system_thread(user: &str, command: &str) -> bool {
    matches!(user, "system user" | "event_scheduler")
        || matches!(command, "Daemon" | "Binlog Dump" | "Binlog Dump GTID")
}

fn threads(c: &mut dyn Client, s: &mut MySqlSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        "select id, user, host, db, command, time, state, info \
           from information_schema.processlist \
          where id <> connection_id()",
        &[],
    )?;
    let threads: Vec<Thread> = rows
        .iter()
        .map(|r| Thread {
            id: int(r, 0),
            user: text(r, 1),
            host: text(r, 2),
            db: opt(r, 3),
            command: text(r, 4),
            time_secs: int(r, 5),
            state: text(r, 6),
            query: opt(r, 7),
        })
        .filter(|t| !is_system_thread(&t.user, &t.command))
        .collect();
    s.by_user = top_counts(threads.iter().map(|t| t.user.clone()));
    // Without the client's port.
    s.by_host = top_counts(threads.iter().map(|t| match t.host.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host.to_owned(),
        _ => t.host.clone(),
    }));
    s.by_db = top_counts(
        threads
            .iter()
            .map(|t| t.db.clone().unwrap_or_else(|| "(none)".into())),
    );
    let mut running: Vec<Thread> = threads
        .into_iter()
        .filter(|t| t.command != "Sleep")
        .collect();
    s.activity = all_counts(running.iter().map(|t| {
        if t.state.is_empty() {
            t.command.clone()
        } else {
            t.state.clone()
        }
    }));
    running.sort_by_key(|t| std::cmp::Reverse(t.time_secs));
    running.truncate(10);
    s.running = running;
    Ok(())
}

/// Counts of each value, most frequent first, at most 8.
fn top_counts(values: impl Iterator<Item = String>) -> Vec<(String, i64)> {
    let mut counts = all_counts(values);
    counts.truncate(8);
    counts
}

fn all_counts(values: impl Iterator<Item = String>) -> Vec<(String, i64)> {
    let mut counts: HashMap<String, i64> = HashMap::new();
    for v in values {
        *counts.entry(v).or_default() += 1;
    }
    let mut counts: Vec<(String, i64)> = counts.into_iter().collect();
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

fn transactions(c: &mut dyn Client, s: &mut MySqlSnapshot) -> Result<(), DbError> {
    let count = c.query("select count(*) from information_schema.innodb_trx", &[])?;
    s.open_transactions = count.first().map(|r| int(r, 0));
    let rows = c.query(
        "select t.trx_mysql_thread_id, p.user, t.trx_state, \
                timestampdiff(second, t.trx_started, now()), t.trx_rows_locked, \
                t.trx_rows_modified, t.trx_query, p.command \
           from information_schema.innodb_trx t \
           left join information_schema.processlist p on p.id = t.trx_mysql_thread_id \
          order by t.trx_started \
          limit 10",
        &[],
    )?;
    s.transactions = rows
        .iter()
        .map(|r| Transaction {
            thread: int(r, 0),
            user: opt(r, 1),
            state: text(r, 2),
            age_secs: int(r, 3),
            rows_locked: int(r, 4),
            rows_modified: int(r, 5),
            query: opt(r, 6),
            idle: opt(r, 7).as_deref() == Some("Sleep"),
        })
        .collect();
    Ok(())
}

fn lock_waits(c: &mut dyn Client, s: &mut MySqlSnapshot) -> Result<(), DbError> {
    const COLUMNS: &str = "r.trx_mysql_thread_id, \
         timestampdiff(second, r.trx_wait_started, now()), r.trx_query, \
         b.trx_mysql_thread_id, b.trx_query";
    let rows = c.query(
        &format!(
            "select {COLUMNS}, concat(l.object_schema, '.', l.object_name) \
               from performance_schema.data_lock_waits w \
               join information_schema.innodb_trx r \
                 on r.trx_id = w.requesting_engine_transaction_id \
               join information_schema.innodb_trx b \
                 on b.trx_id = w.blocking_engine_transaction_id \
               left join performance_schema.data_locks l \
                 on l.engine_lock_id = w.requesting_engine_lock_id \
              order by 2 desc \
              limit 20"
        ),
        &[],
    );
    let rows = match rows {
        // MySQL 5.7 and MariaDB have them in information_schema.
        Err(e) if e.code == Some(1146) => c.query(
            &format!(
                "select {COLUMNS}, l.lock_table \
                   from information_schema.innodb_lock_waits w \
                   join information_schema.innodb_trx r on r.trx_id = w.requesting_trx_id \
                   join information_schema.innodb_trx b on b.trx_id = w.blocking_trx_id \
                   left join information_schema.innodb_locks l \
                     on l.lock_id = w.requested_lock_id \
                  order by 2 desc \
                  limit 20"
            ),
            &[],
        )?,
        rows => rows?,
    };
    s.lock_waits = rows
        .iter()
        .map(|r| LockWait {
            waiting_thread: int(r, 0),
            wait_secs: int(r, 1),
            waiting_query: opt(r, 2),
            blocking_thread: int(r, 3),
            blocking_query: opt(r, 4),
            table: opt(r, 5),
        })
        .collect();
    Ok(())
}

fn replication(c: &mut dyn Client, s: &mut MySqlSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        "select c.channel_name, concat(f.host, ':', f.port), c.service_state, \
                a.service_state, \
                (select timestampdiff(second, \
                            min(nullif(w.applying_transaction_original_commit_timestamp, 0)), \
                            now(6)) \
                   from performance_schema.replication_applier_status_by_worker w \
                  where w.channel_name = c.channel_name and w.applying_transaction <> ''), \
                nullif(c.last_error_message, ''), \
                (select max(nullif(w.last_error_message, '')) \
                   from performance_schema.replication_applier_status_by_worker w \
                  where w.channel_name = c.channel_name) \
           from performance_schema.replication_connection_status c \
           join performance_schema.replication_applier_status a \
             on a.channel_name = c.channel_name \
           left join performance_schema.replication_connection_configuration f \
             on f.channel_name = c.channel_name",
        &[],
    )?;
    s.replicas = rows
        .iter()
        .map(|r| Replica {
            channel: text(r, 0),
            source: text(r, 1),
            io_running: text(r, 2) == "ON",
            sql_running: text(r, 3) == "ON",
            // Nothing being applied: caught up.
            lag_secs: Some(opt(r, 4).map_or(0, |_| int(r, 4).max(0))),
            last_error: opt(r, 5).or_else(|| opt(r, 6)),
        })
        .collect();
    Ok(())
}

/// The schemas of the server itself, left out of the storage lists.
const SYSTEM_SCHEMAS: &str = "('mysql', 'information_schema', 'performance_schema', 'sys')";

fn storage_stats(c: &mut dyn Client, s: &mut MySqlSnapshot) -> Result<(), DbError> {
    let schemas = c.query(
        &format!(
            "select table_schema, count(*), sum(data_length), sum(index_length), \
                    sum(data_free) \
               from information_schema.tables \
              where table_type = 'BASE TABLE' and table_schema not in {SYSTEM_SCHEMAS} \
              group by table_schema \
              order by sum(data_length) + sum(index_length) desc \
              limit 10"
        ),
        &[],
    )?;
    let tables = c.query(
        &format!(
            "select table_schema, table_name, engine, table_rows, data_length, index_length \
               from information_schema.tables \
              where table_type = 'BASE TABLE' and table_schema not in {SYSTEM_SCHEMAS} \
              order by data_length + index_length desc \
              limit 10"
        ),
        &[],
    )?;
    s.storage = Some(Storage {
        schemas: schemas
            .iter()
            .map(|r| SchemaSize {
                schema: text(r, 0),
                tables: int(r, 1),
                data_bytes: num(r, 2),
                index_bytes: num(r, 3),
                free_bytes: num(r, 4),
            })
            .collect(),
        tables: tables
            .iter()
            .map(|r| TableSize {
                schema: text(r, 0),
                name: text(r, 1),
                engine: opt(r, 2),
                rows: int(r, 3),
                data_bytes: num(r, 4),
                index_bytes: num(r, 5),
            })
            .collect(),
    });
    Ok(())
}

fn top_statements(c: &mut dyn Client, s: &mut MySqlSnapshot) -> Result<(), DbError> {
    // Leaves out the dashboard's own lookups (and other dictionary
    // queries): it is about the application's load.
    let rows = c.query(
        "select schema_name, digest, digest_text, count_star, sum_timer_wait / 1e12, \
                sum_lock_time / 1e12, sum_rows_examined, sum_rows_sent, sum_rows_affected, \
                sum_no_index_used + sum_no_good_index_used, sum_created_tmp_disk_tables \
           from performance_schema.events_statements_summary_by_digest \
          where digest_text is not null \
            and digest_text not like 'SHOW %' \
            and digest_text not like '%information_schema%' \
            and digest_text not like '%performance_schema%' \
          order by sum_timer_wait desc \
          limit 10",
        &[],
    )?;
    s.top_statements = rows
        .iter()
        .map(|r| TopStatement {
            schema: opt(r, 0),
            digest: text(r, 1),
            text: text(r, 2),
            executions: int(r, 3),
            total_secs: num(r, 4),
            lock_secs: num(r, 5),
            rows_examined: int(r, 6),
            rows_sent: int(r, 7),
            rows_affected: int(r, 8),
            no_index: int(r, 9),
            tmp_disk_tables: int(r, 10),
        })
        .collect();
    Ok(())
}

fn opt(row: &[Cell], i: usize) -> Option<String> {
    row.get(i).cloned().flatten()
}

fn text(row: &[Cell], i: usize) -> String {
    opt(row, i).unwrap_or_default()
}

fn num(row: &[Cell], i: usize) -> f64 {
    row.get(i)
        .and_then(|c| c.as_deref()?.parse().ok())
        .unwrap_or(0.0)
}

fn int(row: &[Cell], i: usize) -> i64 {
    num(row, i) as i64
}

fn plural(n: i64, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

/// Findings, most severe first; empty means all is well. `rates` are the
/// counters per second since the previous snapshot.
pub fn insights(snap: &MySqlSnapshot, rates: Option<&Counters>) -> Vec<Insight> {
    let mut out = Vec::new();
    let mut add = |severity, text: String| {
        out.push(Insight {
            severity,
            text,
            target: None,
        })
    };

    // Lock waits by blocking thread.
    let mut blockers: Vec<(i64, i64, i64, bool)> = Vec::new();
    for w in &snap.lock_waits {
        match blockers.iter_mut().find(|b| b.0 == w.blocking_thread) {
            Some(b) => {
                b.1 += 1;
                b.2 = b.2.max(w.wait_secs);
            }
            None => blockers.push((
                w.blocking_thread,
                1,
                w.wait_secs,
                w.blocking_query.is_none(),
            )),
        }
    }
    for (thread, blocked, wait, idle) in blockers {
        add(
            if wait >= 10 {
                Severity::Critical
            } else {
                Severity::Warning
            },
            format!(
                "Thread {thread} blocks {}, longest wait {}{}",
                plural(blocked, "transaction"),
                duration(wait as f64),
                if idle {
                    " — idle, likely an uncommitted transaction"
                } else {
                    ""
                }
            ),
        );
    }
    let waiting = snap.transactions.iter().filter(|t| t.state == "LOCK WAIT");
    if snap.lock_waits.is_empty() && waiting.clone().count() > 0 {
        add(
            Severity::Warning,
            format!(
                "{} waiting for a row lock",
                plural(waiting.count() as i64, "transaction")
            ),
        );
    }
    let metadata: i64 = snap
        .activity
        .iter()
        .filter(|(state, _)| state.contains("metadata lock"))
        .map(|(_, n)| n)
        .sum();
    if metadata > 0 {
        add(
            Severity::Warning,
            format!(
                "{} waiting for a metadata lock: DDL waits for an open transaction, \
                 and everything after it waits for the DDL",
                plural(metadata, "thread")
            ),
        );
    }

    for r in &snap.replicas {
        let name = if r.channel.is_empty() {
            "Replication".to_owned()
        } else {
            format!("Replication channel {}", r.channel)
        };
        if !(r.io_running && r.sql_running) {
            let stopped = match (r.io_running, r.sql_running) {
                (false, false) => "is stopped",
                (false, true) => "doesn't receive from the source",
                _ => "doesn't apply changes",
            };
            let error = r
                .last_error
                .as_ref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default();
            add(Severity::Critical, format!("{name} {stopped}{error}"));
        } else if let Some(lag) = r.lag_secs.filter(|l| *l >= 30) {
            add(
                if lag >= 300 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                format!("{name} is {} behind the source", duration(lag as f64)),
            );
        }
    }

    if let Some(c) = snap.connections.filter(|c| c.max_connections > 0) {
        let used = c.connected as f64 / c.max_connections as f64;
        if used >= 0.75 {
            add(
                if used >= 0.9 {
                    Severity::Critical
                } else {
                    Severity::Warning
                },
                format!(
                    "{} of {} connections in use ({:.0} %)",
                    c.connected,
                    c.max_connections,
                    used * 100.0
                ),
            );
        }
    }
    if let Some(r) = rates {
        if r.refused_connections > 0.0 {
            add(
                Severity::Critical,
                "Connections are refused: max_connections is reached".into(),
            );
        }
        if r.aborted_connects >= 0.2 {
            add(
                Severity::Warning,
                format!(
                    "{:.1} failed connection attempts per second (wrong passwords, \
                     missing privileges or timeouts)",
                    r.aborted_connects
                ),
            );
        }
        if let Some(hit) = r.hit_ratio().filter(|_| r.logical_reads >= 100.0)
            && hit < 95.0
        {
            add(
                Severity::Warning,
                format!(
                    "Buffer pool hit ratio is {hit:.1} %: {:.0} reads per second go to disk",
                    r.physical_reads
                ),
            );
        }
        if let Some(pct) = r.tmp_disk_pct().filter(|_| r.tmp_tables >= 1.0)
            && pct >= 25.0
        {
            add(
                Severity::Info,
                format!("{pct:.0} % of temporary tables go to disk"),
            );
        }
        if r.slow_queries > 0.0 {
            add(
                Severity::Info,
                format!("{:.1} slow queries per second", r.slow_queries),
            );
        }
    }

    if let Some(t) = snap.transactions.first().filter(|t| t.age_secs >= 60) {
        let who = t.user.as_deref().unwrap_or("?");
        add(
            Severity::Warning,
            format!(
                "Thread {} ({who}) has had a transaction open for {}{}, {} locked",
                t.thread,
                duration(t.age_secs as f64),
                if t.idle { " while idle" } else { "" },
                plural(t.rows_locked, "row")
            ),
        );
    }
    if let Some(t) = snap
        .running
        .first()
        .filter(|t| t.time_secs >= 60 && t.query.is_some())
    {
        add(
            Severity::Info,
            format!(
                "Thread {} ({}) has run a statement for {}",
                t.id,
                t.user,
                duration(t.time_secs as f64)
            ),
        );
    }
    if let Some(b) = snap.buffer_pool.filter(|b| b.dirty_pct() >= 75.0) {
        add(
            Severity::Info,
            format!(
                "{:.0} % of the buffer pool ({}) is waiting to be written",
                b.dirty_pct(),
                bytes(b.size_bytes)
            ),
        );
    }
    if snap.server.as_ref().is_some_and(|s| s.read_only) {
        add(Severity::Info, "The server is read-only".into());
    }
    out.sort_by_key(|i| std::cmp::Reverse(i.severity));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_and_ratios() {
        let status = Status(
            [
                ("questions", "1000"),
                ("com_insert", "10"),
                ("com_update", "5"),
                ("com_delete_multi", "1"),
                ("innodb_buffer_pool_read_requests", "10000"),
                ("innodb_buffer_pool_reads", "100"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect(),
        );
        let before = Counters::from_status(&status);
        assert_eq!(before.changes, 16.0);
        let after = Counters {
            queries: 1500.0,
            logical_reads: 30_000.0,
            physical_reads: 1100.0,
            ..before
        };
        let r = Counters::per_second(&before, &after, 10.0);
        assert_eq!(r.queries, 50.0);
        assert_eq!(r.hit_ratio(), Some(95.0));
        assert_eq!(r.tmp_disk_pct(), None);
        // A restarted server's counters start over.
        assert_eq!(Counters::per_second(&after, &before, 10.0).queries, 0.0);
    }

    #[test]
    fn findings() {
        let snap = MySqlSnapshot {
            connections: Some(Connections {
                connected: 140,
                running: 3,
                max_used: 150,
                max_connections: 151,
            }),
            lock_waits: vec![
                LockWait {
                    waiting_thread: 12,
                    wait_secs: 15,
                    blocking_thread: 7,
                    ..Default::default()
                },
                LockWait {
                    waiting_thread: 13,
                    wait_secs: 4,
                    blocking_thread: 7,
                    ..Default::default()
                },
            ],
            activity: vec![("Waiting for table metadata lock".into(), 2)],
            transactions: vec![Transaction {
                thread: 7,
                user: Some("app".into()),
                state: "RUNNING".into(),
                age_secs: 125,
                rows_locked: 1,
                idle: true,
                ..Default::default()
            }],
            ..Default::default()
        };
        let rates = Counters {
            logical_reads: 1000.0,
            physical_reads: 200.0,
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
                    "Thread 7 blocks 2 transactions, longest wait 15s — idle, likely an \
                     uncommitted transaction"
                        .into()
                ),
                (
                    Severity::Critical,
                    "140 of 151 connections in use (93 %)".into()
                ),
                (
                    Severity::Warning,
                    "2 threads waiting for a metadata lock: DDL waits for an open \
                     transaction, and everything after it waits for the DDL"
                        .into()
                ),
                (
                    Severity::Warning,
                    "Buffer pool hit ratio is 80.0 %: 200 reads per second go to disk".into()
                ),
                (
                    Severity::Warning,
                    "Thread 7 (app) has had a transaction open for 2m 5s while idle, 1 row \
                     locked"
                        .into()
                ),
            ]
        );
        assert!(insights(&MySqlSnapshot::default(), None).is_empty());
    }
}
