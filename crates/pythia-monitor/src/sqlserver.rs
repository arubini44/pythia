//! The SQL Server dashboard's data: server counters, sessions and
//! requests, blocking and transactions, waits, storage and top statements,
//! read through [`pythia_db::Client`] (the session's `DbRequest::Run`).
//!
//! It comes from the dynamic management views (`sys.dm_os_performance_counters`,
//! `sys.dm_exec_*`, `sys.dm_tran_*`, `sys.dm_os_wait_stats`) and the catalog.
//! They need VIEW SERVER STATE (on SQL Server 2022, VIEW SERVER PERFORMANCE
//! STATE is enough); without it a login sees its own sessions only, and the
//! dashboard says so instead of failing section by section.

use pythia_db::{Client, DbError};

use crate::cells::{all_counts, int, num, opt, plural, text, top_counts};
use crate::insights::{Insight, Severity};
use crate::queries::{SectionError, SystemEvent};
use crate::ui::style::{bytes, duration};

/// What the server-wide views need.
pub const SERVER_STATE: &str = "VIEW SERVER STATE";

macro_rules! counters {
    ($($(#[$doc:meta])* $field:ident: $object:literal $counter:literal;)*) => {
        /// Cumulative counters from `sys.dm_os_performance_counters` (the
        /// "/sec" ones count up since startup). Between two snapshots they
        /// become rates ([`Counters::per_second`]).
        #[derive(Debug, Clone, Copy, Default, PartialEq)]
        pub struct Counters {
            $($(#[$doc])* pub $field: f64,)*
        }

        impl Counters {
            fn from_perf(perf: &Perf) -> Self {
                Self {
                    $($field: perf.get($object, $counter),)*
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
    /// Batches sent by clients.
    batch_requests: "SQL Statistics" "Batch Requests/sec";
    /// Plans compiled; many per batch means ad-hoc SQL without parameters.
    compilations: "SQL Statistics" "SQL Compilations/sec";
    recompilations: "SQL Statistics" "SQL Re-Compilations/sec";
    transactions: "Databases" "Transactions/sec";
    write_transactions: "Databases" "Write Transactions/sec";
    /// Lock requests that had to wait.
    lock_waits: "Locks" "Lock Waits/sec";
    lock_timeouts: "Locks" "Lock Timeouts/sec";
    deadlocks: "Locks" "Number of Deadlocks/sec";
    /// Pages looked up in the buffer pool (logical reads).
    page_lookups: "Buffer Manager" "Page lookups/sec";
    /// Of those, pages read from disk.
    page_reads: "Buffer Manager" "Page reads/sec";
    page_writes: "Buffer Manager" "Page writes/sec";
    log_flushes: "Databases" "Log Flushes/sec";
    log_bytes: "Databases" "Log Bytes Flushed/sec";
    full_scans: "Access Methods" "Full Scans/sec";
    index_searches: "Access Methods" "Index Searches/sec";
    page_splits: "Access Methods" "Page Splits/sec";
    logins: "General Statistics" "Logins/sec";
    errors: "SQL Errors" "Errors/sec";
}

impl Counters {
    /// Share of page lookups served from memory, in percent.
    pub fn hit_ratio(&self) -> Option<f64> {
        (self.page_lookups > 0.0)
            .then(|| 100.0 * (1.0 - self.page_reads / self.page_lookups).max(0.0))
    }

    /// Share of batches that compiled a plan, in percent.
    pub fn compile_pct(&self) -> Option<f64> {
        (self.batch_requests > 0.0).then(|| 100.0 * self.compilations / self.batch_requests)
    }
}

/// The counters that say how things are now, not since startup.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Gauges {
    pub user_connections: i64,
    pub processes_blocked: i64,
    /// Seconds a page stays in the buffer pool.
    pub page_life_expectancy: f64,
    /// Buffer cache hit ratio as the server computes it, in percent.
    pub buffer_hit_ratio: Option<f64>,
    /// Queries waiting for memory to run.
    pub memory_grants_pending: i64,
    pub total_memory_bytes: f64,
    pub target_memory_bytes: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerInfo {
    pub name: String,
    /// "SQL Server 2022", from the major version.
    pub product: String,
    pub version: String,
    /// "RTM CU27".
    pub level: String,
    pub edition: String,
    pub uptime_secs: f64,
    /// Need VIEW SERVER STATE.
    pub cpus: Option<i64>,
    pub memory_bytes: Option<f64>,
    /// Always On availability groups are enabled.
    pub hadr: bool,
    pub max_connections: i64,
}

/// A request a user session is running.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Request {
    pub session: i64,
    pub login: String,
    pub database: Option<String>,
    /// `running`, `runnable`, `suspended`, …
    pub status: String,
    pub command: String,
    pub elapsed_secs: f64,
    pub wait_type: Option<String>,
    pub wait_secs: f64,
    pub blocked_by: Option<i64>,
    pub wait_resource: Option<String>,
    /// The statement of the batch it runs.
    pub statement: Option<String>,
}

/// A request waiting for a lock another session holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Blocked {
    pub session: i64,
    pub wait_secs: f64,
    pub wait_type: Option<String>,
    pub resource: Option<String>,
    pub statement: Option<String>,
    pub blocker: i64,
    pub blocker_login: Option<String>,
    /// What the blocker runs; none while it is idle.
    pub blocker_statement: Option<String>,
    /// The blocker sleeps with a transaction open: likely uncommitted.
    pub blocker_idle: bool,
    /// The blocker's last batch, when it is idle (from a Pythia session:
    /// its transaction bookkeeping).
    pub blocker_last: Option<String>,
}

/// An open transaction of a user session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Transaction {
    pub session: i64,
    pub login: String,
    /// `user_transaction`, `implicit_transaction`, …
    pub name: String,
    pub age_secs: i64,
    pub log_bytes: f64,
    /// The session sleeps with the transaction open.
    pub idle: bool,
    /// The statement it runs; none while idle.
    pub statement: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatabaseSize {
    pub name: String,
    pub data_bytes: f64,
    pub log_bytes: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableSize {
    pub schema: String,
    pub name: String,
    pub rows: i64,
    pub reserved_bytes: f64,
    pub data_bytes: f64,
    pub index_bytes: f64,
}

/// Database and table sizes, refreshed less often than the rest.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Storage {
    pub databases: Vec<DatabaseSize>,
    /// Of the connection's current database.
    pub tables: Vec<TableSize>,
    /// The login can't see `sys.master_files`: only the current database.
    pub current_database_only: bool,
}

/// A statement (all plans of one query hash) and what its executions cost
/// since its plans were cached.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TopStatement {
    pub hash: String,
    pub database: Option<String>,
    pub text: String,
    pub executions: i64,
    pub total_secs: f64,
    pub cpu_secs: f64,
    pub logical_reads: i64,
    pub physical_reads: i64,
    pub writes: i64,
    pub rows: i64,
    pub max_secs: f64,
}

#[derive(Debug, Clone, Default)]
pub struct MsSnapshot {
    pub server: Option<ServerInfo>,
    pub counters: Option<Counters>,
    pub gauges: Option<Gauges>,
    /// User sessions besides the dashboard's.
    pub sessions: Option<i64>,
    pub by_login: Vec<(String, i64)>,
    pub by_host: Vec<(String, i64)>,
    pub by_program: Vec<(String, i64)>,
    pub by_database: Vec<(String, i64)>,
    /// Running requests by what they wait for (`CPU` when they don't).
    pub activity: Vec<(String, i64)>,
    /// Running requests, longest first.
    pub running: Vec<Request>,
    pub blocked: Vec<Blocked>,
    /// Open transactions of user sessions (all of them, not just those
    /// listed).
    pub open_transactions: Option<i64>,
    /// The oldest open transactions.
    pub transactions: Vec<Transaction>,
    /// Waits since startup, benign ones left out; between snapshots they
    /// become rates (`queries::event_rates`).
    pub waits: Vec<SystemEvent>,
    /// Percent of each database's log in use.
    pub log_used: Vec<(String, f64)>,
    /// Only on the snapshots that read sizes.
    pub storage: Option<Storage>,
    pub top_statements: Vec<TopStatement>,
    /// Without VIEW SERVER STATE: only the login's own sessions are listed
    /// and the server-wide sections are left out.
    pub own_sessions_only: bool,
    pub errors: Vec<SectionError>,
}

/// Collects everything the dashboard shows; never fails as a whole. Sizes
/// only with `storage`: they cost more on servers with many databases.
pub fn snapshot(c: &mut dyn Client, storage: bool) -> MsSnapshot {
    let mut s = MsSnapshot::default();
    let mut errors = Vec::new();
    let mut section = |name: &'static str, r: Result<(), DbError>| {
        if let Err(e) = r {
            errors.push((name, explain(&e)));
        }
    };
    section("Server", server(c, &mut s));
    let counters = performance_counters(c, &mut s);
    s.own_sessions_only = counters.as_ref().is_err_and(is_privilege_error);
    if !s.own_sessions_only {
        section("Performance counters", counters);
    }
    // Without the privilege only the login's own sessions are listed.
    section("Sessions", sessions(c, &mut s));
    // The rest is server-wide: one note instead of an error each.
    if !s.own_sessions_only {
        section("Requests", requests(c, &mut s));
        if !s.blocked.is_empty() {
            section("Blocking", blockers(c, &mut s));
        }
        section("Transactions", transactions(c, &mut s));
        section("Waits", waits(c, &mut s));
        section("Top statements", top_statements(c, &mut s));
    }
    if storage {
        section("Storage", storage_stats(c, &mut s));
    }
    s.errors = errors;
    s
}

fn is_privilege_error(e: &DbError) -> bool {
    // Permission denied on an object, a column, in a database; VIEW SERVER
    // (PERFORMANCE) STATE denied; no access to the database.
    matches!(e.code, Some(229 | 230 | 262 | 297 | 300 | 916))
}

/// The error, and for a privilege error what the user would need.
pub fn explain(e: &DbError) -> String {
    if is_privilege_error(e) {
        format!(
            "Not accessible with the current privileges (needs {SERVER_STATE}). {}",
            e.message
        )
    } else {
        e.message.clone()
    }
}

/// The product name of a major version.
fn product(version: &str) -> String {
    let year = match version
        .split('.')
        .next()
        .and_then(|m| m.parse::<u32>().ok())
    {
        Some(17) => "2025",
        Some(16) => "2022",
        Some(15) => "2019",
        Some(14) => "2017",
        Some(13) => "2016",
        Some(12) => "2014",
        Some(11) => "2012",
        _ => return "SQL Server".into(),
    };
    format!("SQL Server {year}")
}

fn server(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    // tempdb is created at startup: its age is the uptime, for any login.
    let rows = c.query(
        "select @@servername, cast(serverproperty('ProductVersion') as nvarchar(32)), \
                cast(serverproperty('ProductLevel') as nvarchar(32)), \
                cast(serverproperty('ProductUpdateLevel') as nvarchar(32)), \
                cast(serverproperty('Edition') as nvarchar(128)), \
                cast(serverproperty('IsHadrEnabled') as int), @@max_connections, \
                datediff(second, (select create_date from sys.databases where name = 'tempdb'), \
                         getdate())",
        &[],
    )?;
    let Some(r) = rows.first() else {
        return Ok(());
    };
    let version = text(r, 1);
    let mut info = ServerInfo {
        name: text(r, 0),
        product: product(&version),
        version,
        level: [opt(r, 2), opt(r, 3)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" "),
        edition: text(r, 4),
        uptime_secs: num(r, 7),
        hadr: int(r, 5) == 1,
        max_connections: int(r, 6),
        ..ServerInfo::default()
    };
    // The machine needs VIEW SERVER STATE.
    if let Ok(rows) = c.query(
        "select datediff(second, sqlserver_start_time, sysdatetime()), cpu_count, \
                physical_memory_kb from sys.dm_os_sys_info",
        &[],
    ) && let Some(r) = rows.first()
    {
        info.uptime_secs = num(r, 0);
        info.cpus = Some(int(r, 1));
        info.memory_bytes = Some(num(r, 2) * 1024.0);
    }
    s.server = Some(info);
    Ok(())
}

/// `sys.dm_os_performance_counters` as (object, counter) → value, objects
/// without their `SQLServer:` / `MSSQL$instance:` prefix, in lowercase.
struct Perf(Vec<(String, String, f64)>);

impl Perf {
    fn get(&self, object: &str, counter: &str) -> f64 {
        self.0
            .iter()
            .find(|(o, c, _)| o.eq_ignore_ascii_case(object) && c.eq_ignore_ascii_case(counter))
            .map_or(0.0, |(_, _, v)| *v)
    }
}

fn performance_counters(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    // Instance-wide values: no instance, or the total of all databases,
    // lock types and error kinds.
    let rows = c.query(
        "select rtrim(substring(object_name, charindex(':', object_name) + 1, 128)), \
                rtrim(counter_name), cntr_value \
           from sys.dm_os_performance_counters \
          where rtrim(instance_name) in ('', '_Total')",
        &[],
    )?;
    let perf = Perf(
        rows.iter()
            .map(|r| (text(r, 0), text(r, 1), num(r, 2)))
            .collect(),
    );
    s.counters = Some(Counters::from_perf(&perf));
    let base = perf.get("Buffer Manager", "Buffer cache hit ratio base");
    s.gauges = Some(Gauges {
        user_connections: perf.get("General Statistics", "User Connections") as i64,
        processes_blocked: perf.get("General Statistics", "Processes blocked") as i64,
        page_life_expectancy: perf.get("Buffer Manager", "Page life expectancy"),
        buffer_hit_ratio: (base > 0.0)
            .then(|| 100.0 * perf.get("Buffer Manager", "Buffer cache hit ratio") / base),
        memory_grants_pending: perf.get("Memory Manager", "Memory Grants Pending") as i64,
        total_memory_bytes: perf.get("Memory Manager", "Total Server Memory (KB)") * 1024.0,
        target_memory_bytes: perf.get("Memory Manager", "Target Server Memory (KB)") * 1024.0,
    });
    // Per database; the counters also list internal ones.
    let rows = c.query(
        "select rtrim(instance_name), cntr_value \
           from sys.dm_os_performance_counters \
          where rtrim(counter_name) = 'Percent Log Used' \
            and rtrim(instance_name) in (select name from sys.databases) \
          order by cntr_value desc",
        &[],
    )?;
    s.log_used = rows.iter().map(|r| (text(r, 0), num(r, 1))).collect();
    Ok(())
}

fn sessions(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        "select login_name, host_name, program_name, db_name(database_id) \
           from sys.dm_exec_sessions \
          where is_user_process = 1 and session_id <> @@spid",
        &[],
    )?;
    let unknown = |v: Option<String>| {
        v.filter(|v| !v.is_empty())
            .unwrap_or_else(|| "(none)".into())
    };
    s.sessions = Some(rows.len() as i64);
    s.by_login = top_counts(rows.iter().map(|r| text(r, 0)));
    s.by_host = top_counts(rows.iter().map(|r| unknown(opt(r, 1))));
    s.by_program = top_counts(rows.iter().map(|r| unknown(opt(r, 2))));
    s.by_database = top_counts(rows.iter().map(|r| unknown(opt(r, 3))));
    Ok(())
}

/// The statement a request runs within its batch.
const STATEMENT: &str = "substring(t.text, r.statement_start_offset / 2 + 1, \
     (case r.statement_end_offset when -1 then datalength(t.text) \
      else r.statement_end_offset end - r.statement_start_offset) / 2 + 1)";

/// What a wait is about, for the activity bar and the top waits.
pub fn wait_class(wait_type: &str) -> &'static str {
    let w = wait_type;
    if w.is_empty() || w == "SOS_SCHEDULER_YIELD" || w == "THREADPOOL" {
        "CPU"
    } else if w.starts_with("LCK_M_") {
        "Lock"
    } else if w.starts_with("PAGEIOLATCH_")
        || w.starts_with("IO_COMPLETION")
        || w.starts_with("ASYNC_IO_COMPLETION")
        || w.starts_with("BACKUPIO")
    {
        "Disk I/O"
    } else if w.starts_with("PAGELATCH_") || w.starts_with("LATCH_") {
        "Latch"
    } else if w == "WRITELOG" || w.starts_with("LOGBUFFER") || w.starts_with("HADR_SYNC_COMMIT") {
        "Log"
    } else if w.starts_with("CX") || w.starts_with("EXCHANGE") {
        "Parallelism"
    } else if w == "ASYNC_NETWORK_IO" {
        "Network"
    } else if w.starts_with("RESOURCE_SEMAPHORE") || w == "CMEMTHREAD" {
        "Memory"
    } else if w.starts_with("PREEMPTIVE_") {
        "External"
    } else {
        "Other"
    }
}

fn requests(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        &format!(
            "select r.session_id, s.login_name, db_name(r.database_id), r.status, r.command, \
                    r.total_elapsed_time / 1000.0, r.wait_type, r.wait_time / 1000.0, \
                    r.blocking_session_id, r.wait_resource, {STATEMENT} \
               from sys.dm_exec_requests r \
               join sys.dm_exec_sessions s on s.session_id = r.session_id \
               outer apply sys.dm_exec_sql_text(r.sql_handle) t \
              where s.is_user_process = 1 and r.session_id <> @@spid"
        ),
        &[],
    )?;
    let mut running: Vec<Request> = rows
        .iter()
        .map(|r| Request {
            session: int(r, 0),
            login: text(r, 1),
            database: opt(r, 2),
            status: text(r, 3),
            command: text(r, 4),
            elapsed_secs: num(r, 5),
            wait_type: opt(r, 6),
            wait_secs: num(r, 7),
            // Negative: an orphaned distributed transaction, a latch.
            blocked_by: Some(int(r, 8)).filter(|b| *b > 0),
            wait_resource: opt(r, 9).filter(|w| !w.is_empty()),
            statement: opt(r, 10),
        })
        .collect();
    s.activity = all_counts(
        running
            .iter()
            .map(|r| wait_class(r.wait_type.as_deref().unwrap_or_default()).to_owned()),
    );
    s.blocked = running
        .iter()
        .filter_map(|r| {
            Some(Blocked {
                session: r.session,
                wait_secs: r.wait_secs,
                wait_type: r.wait_type.clone(),
                resource: r.wait_resource.clone(),
                statement: r.statement.clone(),
                blocker: r.blocked_by?,
                ..Blocked::default()
            })
        })
        .collect();
    s.blocked
        .sort_by(|a, b| b.wait_secs.total_cmp(&a.wait_secs));
    running.sort_by(|a, b| b.elapsed_secs.total_cmp(&a.elapsed_secs));
    running.truncate(10);
    s.running = running;
    Ok(())
}

/// Who the blocked requests wait for: what the blockers run, or whether
/// they sleep with a transaction open.
fn blockers(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    let ids: Vec<String> = s.blocked.iter().map(|b| b.blocker.to_string()).collect();
    // Session ids are numbers: safe in the statement.
    let rows = c.query(
        &format!(
            "select s.session_id, s.login_name, s.status, s.open_transaction_count, \
                    case when r.session_id is null then null else {STATEMENT} end, lt.text \
               from sys.dm_exec_sessions s \
               left join sys.dm_exec_requests r on r.session_id = s.session_id \
               left join sys.dm_exec_connections c on c.session_id = s.session_id \
               outer apply sys.dm_exec_sql_text(r.sql_handle) t \
               outer apply sys.dm_exec_sql_text(c.most_recent_sql_handle) lt \
              where s.session_id in ({})",
            ids.join(", ")
        ),
        &[],
    )?;
    for b in &mut s.blocked {
        if let Some(r) = rows.iter().find(|r| int(r, 0) == b.blocker) {
            b.blocker_login = opt(r, 1);
            b.blocker_statement = opt(r, 4);
            b.blocker_idle = text(r, 2) == "sleeping" && int(r, 3) > 0;
            b.blocker_last = opt(r, 5).filter(|_| b.blocker_statement.is_none());
        }
    }
    Ok(())
}

fn transactions(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    const FROM: &str = "from sys.dm_tran_session_transactions st \
         join sys.dm_tran_active_transactions a on a.transaction_id = st.transaction_id \
         join sys.dm_exec_sessions s on s.session_id = st.session_id \
        where s.is_user_process = 1 and st.session_id <> @@spid";
    let count = c.query(&format!("select count(*) {FROM}"), &[])?;
    s.open_transactions = count.first().map(|r| int(r, 0));
    let rows = c.query(
        &format!(
            "select top 10 st.session_id, s.login_name, a.name, \
                    datediff(second, a.transaction_begin_time, getdate()), \
                    (select sum(d.database_transaction_log_bytes_used) \
                       from sys.dm_tran_database_transactions d \
                      where d.transaction_id = a.transaction_id), \
                    s.status, case when r.session_id is null then null else {STATEMENT} end \
               {} \
              order by a.transaction_begin_time",
            FROM.replace(
                "where s.is_user_process",
                "left join sys.dm_exec_requests r on r.session_id = st.session_id \
                 outer apply sys.dm_exec_sql_text(r.sql_handle) t \
                 where s.is_user_process"
            )
        ),
        &[],
    )?;
    s.transactions = rows
        .iter()
        .map(|r| Transaction {
            session: int(r, 0),
            login: text(r, 1),
            name: text(r, 2),
            age_secs: int(r, 3),
            log_bytes: num(r, 4),
            idle: text(r, 5) == "sleeping",
            statement: opt(r, 6),
        })
        .collect();
    Ok(())
}

/// Waits that are part of the server idling or doing its background work.
const BENIGN_WAITS: &str = "('BROKER_EVENTHANDLER', 'BROKER_RECEIVE_WAITFOR', 'BROKER_TASK_STOP', \
     'BROKER_TO_FLUSH', 'BROKER_TRANSMITTER', 'CHECKPOINT_QUEUE', 'CHKPT', 'CLR_AUTO_EVENT', \
     'CLR_MANUAL_EVENT', 'CLR_SEMAPHORE', 'DIRTY_PAGE_POLL', 'DISPATCHER_QUEUE_SEMAPHORE', \
     'EXECSYNC', 'FSAGENT', 'FT_IFTS_SCHEDULER_IDLE_WAIT', 'FT_IFTSHC_MUTEX', \
     'HADR_CLUSAPI_CALL', 'HADR_FILESTREAM_IOMGR_IOCOMPLETION', 'HADR_LOGCAPTURE_WAIT', \
     'HADR_NOTIFICATION_DEQUEUE', 'HADR_TIMER_TASK', 'HADR_WORK_QUEUE', 'KSOURCE_WAKEUP', \
     'LAZYWRITER_SLEEP', 'LOGMGR_QUEUE', 'MEMORY_ALLOCATION_EXT', 'ONDEMAND_TASK_QUEUE', \
     'PREEMPTIVE_OS_FLUSHFILEBUFFERS', 'PREEMPTIVE_XE_GETTARGETSTATE', 'PVS_PREALLOCATE', \
     'PWAIT_ALL_COMPONENTS_INITIALIZED', 'PWAIT_DIRECTLOGCONSUMER_GETNEXT', \
     'PWAIT_EXTENSIBILITY_CLEANUP_TASK', 'REDO_THREAD_PENDING_WORK', \
     'REQUEST_FOR_DEADLOCK_SEARCH', 'RESOURCE_QUEUE', 'SERVER_IDLE_CHECK', 'SNI_HTTP_ACCEPT', \
     'SOS_WORK_DISPATCHER', 'SP_SERVER_DIAGNOSTICS_SLEEP', 'VDI_CLIENT_OTHER', \
     'WAIT_FOR_RESULTS', 'WAITFOR', 'WAITFOR_TASKSHUTDOWN')";

fn waits(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    let rows = c.query(
        &format!(
            "select wait_type, waiting_tasks_count, wait_time_ms \
               from sys.dm_os_wait_stats \
              where waiting_tasks_count > 0 and wait_type not in {BENIGN_WAITS} \
                and wait_type not like 'SLEEP[_]%' and wait_type not like 'XE[_]%' \
                and wait_type not like 'QDS[_]%' and wait_type not like 'SQLTRACE[_]%' \
                and wait_type not like 'PARALLEL_REDO[_]%' and wait_type not like 'WAIT_XTP[_]%' \
                and wait_type not like 'DBMIRROR%'"
        ),
        &[],
    )?;
    s.waits = rows
        .iter()
        .map(|r| {
            let event = text(r, 0);
            SystemEvent {
                wait_class: wait_class(&event).to_owned(),
                event,
                waits: num(r, 1),
                micros: num(r, 2) * 1000.0,
            }
        })
        .collect();
    Ok(())
}

fn storage_stats(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    const SIZES: &str = "sum(case when type = 1 then 0 else cast(size as bigint) end) * 8192, \
         sum(case when type = 1 then cast(size as bigint) else 0 end) * 8192";
    let mut databases = c.query(
        &format!(
            "select top 10 db_name(database_id), {SIZES} from sys.master_files \
              group by database_id order by sum(cast(size as bigint)) desc"
        ),
        &[],
    )?;
    // Without VIEW ANY DEFINITION the server's files aren't listed.
    let current_database_only = databases.is_empty();
    if current_database_only {
        databases = c.query(
            &format!("select db_name(), {SIZES} from sys.database_files"),
            &[],
        )?;
    }
    let tables = c.query(
        "select top 10 sc.name, o.name, \
                sum(case when p.index_id < 2 then p.row_count else 0 end), \
                sum(p.reserved_page_count) * 8192, \
                sum(case when p.index_id < 2 then p.used_page_count else 0 end) * 8192, \
                sum(case when p.index_id > 1 then p.used_page_count else 0 end) * 8192 \
           from sys.dm_db_partition_stats p \
           join sys.objects o on o.object_id = p.object_id \
           join sys.schemas sc on sc.schema_id = o.schema_id \
          where o.is_ms_shipped = 0 \
          group by sc.name, o.name \
          order by sum(p.reserved_page_count) desc",
        &[],
    )?;
    s.storage = Some(Storage {
        databases: databases
            .iter()
            .map(|r| DatabaseSize {
                name: text(r, 0),
                data_bytes: num(r, 1),
                log_bytes: num(r, 2),
            })
            .collect(),
        tables: tables
            .iter()
            .map(|r| TableSize {
                schema: text(r, 0),
                name: text(r, 1),
                rows: int(r, 2),
                reserved_bytes: num(r, 3),
                data_bytes: num(r, 4),
                index_bytes: num(r, 5),
            })
            .collect(),
        current_database_only,
    });
    Ok(())
}

fn top_statements(c: &mut dyn Client, s: &mut MsSnapshot) -> Result<(), DbError> {
    // A statement's plans share its query hash: summed over them, with
    // the text of the costliest. Leaves out queries of the catalog and
    // the DMVs (`sys.…`, `[sys].…`, `information_schema`: the dashboard's
    // own, the object browser's): it is about the application's load.
    let rows = c.query(
        &format!(
            "with q as ( \
               select qs.sql_handle, qs.plan_handle, qs.statement_start_offset, \
                      qs.statement_end_offset, convert(varchar(18), qs.query_hash, 1) as hash, \
                      row_number() over (partition by qs.query_hash \
                                         order by qs.total_elapsed_time desc) as rn, \
                      sum(qs.execution_count) over (partition by qs.query_hash) as execs, \
                      sum(qs.total_elapsed_time) over (partition by qs.query_hash) as elapsed, \
                      sum(qs.total_worker_time) over (partition by qs.query_hash) as cpu, \
                      sum(qs.total_logical_reads) over (partition by qs.query_hash) as lreads, \
                      sum(qs.total_physical_reads) over (partition by qs.query_hash) as preads, \
                      sum(qs.total_logical_writes) over (partition by qs.query_hash) as writes, \
                      sum(qs.total_rows) over (partition by qs.query_hash) as total_rows, \
                      max(qs.max_elapsed_time) over (partition by qs.query_hash) as max_elapsed \
                 from sys.dm_exec_query_stats qs) \
             select top 10 r.hash, db_name(convert(int, pa.value)), {}, r.execs, \
                    r.elapsed / 1e6, r.cpu / 1e6, r.lreads, r.preads, r.writes, r.total_rows, \
                    r.max_elapsed / 1e6 \
               from q r \
              cross apply sys.dm_exec_sql_text(r.sql_handle) t \
              outer apply (select value from sys.dm_exec_plan_attributes(r.plan_handle) \
                            where attribute = 'dbid') pa \
              where r.rn = 1 and t.text not like '%sys.[a-z]%' \
                and t.text not like '%[[]sys].%' \
                and t.text not like '%information[_]schema%' \
              order by r.elapsed desc",
            STATEMENT
        ),
        &[],
    )?;
    s.top_statements = rows
        .iter()
        .map(|r| TopStatement {
            hash: text(r, 0),
            database: opt(r, 1),
            text: text(r, 2),
            executions: int(r, 3),
            total_secs: num(r, 4),
            cpu_secs: num(r, 5),
            logical_reads: int(r, 6),
            physical_reads: int(r, 7),
            writes: int(r, 8),
            rows: int(r, 9),
            max_secs: num(r, 10),
        })
        .collect();
    Ok(())
}

/// Findings, most severe first; empty means all is well. `rates` are the
/// counters per second since the previous snapshot.
pub fn insights(snap: &MsSnapshot, rates: Option<&Counters>) -> Vec<Insight> {
    let mut out = Vec::new();
    let mut add = |severity, text: String| {
        out.push(Insight {
            severity,
            text,
            target: None,
            session: None,
        })
    };

    // Blocked requests by blocking session.
    let mut blockers: Vec<(&Blocked, i64, f64)> = Vec::new();
    for b in &snap.blocked {
        match blockers
            .iter_mut()
            .find(|(first, ..)| first.blocker == b.blocker)
        {
            Some((_, n, wait)) => {
                *n += 1;
                *wait = wait.max(b.wait_secs);
            }
            None => blockers.push((b, 1, b.wait_secs)),
        }
    }
    for (b, blocked, wait) in blockers {
        let who = b
            .blocker_login
            .as_ref()
            .map(|l| format!(" ({l})"))
            .unwrap_or_default();
        add(
            if wait >= 10.0 {
                Severity::Critical
            } else {
                Severity::Warning
            },
            format!(
                "Session {}{who} blocks {}, longest wait {}{}",
                b.blocker,
                plural(blocked, "session"),
                duration(wait),
                if b.blocker_idle {
                    " — idle, likely an uncommitted transaction"
                } else {
                    ""
                }
            ),
        );
    }

    for (database, pct) in snap.log_used.iter().filter(|(_, p)| *p >= 90.0) {
        add(
            if *pct >= 98.0 {
                Severity::Critical
            } else {
                Severity::Warning
            },
            format!("The log of database {database} is {pct:.0} % full"),
        );
    }
    if let Some(g) = snap.gauges {
        if g.memory_grants_pending > 0 {
            add(
                Severity::Warning,
                match g.memory_grants_pending {
                    1 => "1 query waiting for a memory grant".to_owned(),
                    n => format!("{n} queries waiting for a memory grant"),
                },
            );
        }
        if g.page_life_expectancy > 0.0 && g.page_life_expectancy < 300.0 {
            add(
                Severity::Warning,
                format!(
                    "Page life expectancy is {}: pages leave the buffer pool quickly \
                     (memory pressure)",
                    duration(g.page_life_expectancy)
                ),
            );
        }
        if let Some(server) = &snap.server
            && server.max_connections > 0
        {
            let used = g.user_connections as f64 / server.max_connections as f64;
            if used >= 0.75 {
                add(
                    if used >= 0.9 {
                        Severity::Critical
                    } else {
                        Severity::Warning
                    },
                    format!(
                        "{} of {} connections in use ({:.0} %)",
                        g.user_connections,
                        server.max_connections,
                        used * 100.0
                    ),
                );
            }
        }
    }
    if let Some(r) = rates {
        if r.deadlocks > 0.0 {
            add(
                Severity::Warning,
                format!("Deadlocks occur: {:.2} per second", r.deadlocks),
            );
        }
        if let Some(hit) = r.hit_ratio().filter(|_| r.page_lookups >= 100.0)
            && hit < 95.0
        {
            add(
                Severity::Warning,
                format!(
                    "Buffer cache hit ratio is {hit:.1} %: {:.0} pages per second are read \
                     from disk",
                    r.page_reads
                ),
            );
        }
        if let Some(pct) = r.compile_pct().filter(|_| r.batch_requests >= 10.0)
            && pct >= 25.0
        {
            add(
                Severity::Info,
                format!("{pct:.0} % of batches compile a plan: ad-hoc SQL without parameters?"),
            );
        }
    }

    if let Some(t) = snap.transactions.first().filter(|t| t.age_secs >= 60) {
        add(
            Severity::Warning,
            format!(
                "Session {} ({}) has had a transaction open for {}{}, {} of log",
                t.session,
                t.login,
                duration(t.age_secs as f64),
                if t.idle { " while idle" } else { "" },
                bytes(t.log_bytes)
            ),
        );
    }
    if let Some(r) = snap
        .running
        .first()
        .filter(|r| r.elapsed_secs >= 60.0 && r.statement.is_some())
    {
        add(
            Severity::Info,
            format!(
                "Session {} ({}) has run a statement for {}",
                r.session,
                r.login,
                duration(r.elapsed_secs)
            ),
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
        let perf = Perf(
            [
                ("SQL Statistics", "Batch Requests/sec", 1000.0),
                ("SQL Statistics", "SQL Compilations/sec", 400.0),
                ("Buffer Manager", "Page lookups/sec", 10_000.0),
                ("Buffer Manager", "Page reads/sec", 100.0),
                ("Databases", "Transactions/sec", 50.0),
            ]
            .into_iter()
            .map(|(o, c, v)| (o.to_owned(), c.to_owned(), v))
            .collect(),
        );
        let before = Counters::from_perf(&perf);
        assert_eq!(before.batch_requests, 1000.0);
        assert_eq!(before.transactions, 50.0);
        assert_eq!(before.compile_pct(), Some(40.0));
        let after = Counters {
            batch_requests: 1500.0,
            page_lookups: 30_000.0,
            page_reads: 1100.0,
            ..before
        };
        let r = Counters::per_second(&before, &after, 10.0);
        assert_eq!(r.batch_requests, 50.0);
        assert_eq!(r.hit_ratio(), Some(95.0));
        assert_eq!(r.compile_pct(), Some(0.0));
        // A restarted server's counters start over.
        assert_eq!(
            Counters::per_second(&after, &before, 10.0).batch_requests,
            0.0
        );
    }

    #[test]
    fn names() {
        assert_eq!(product("16.0.4305.5"), "SQL Server 2022");
        assert_eq!(product("15.0.2000.5"), "SQL Server 2019");
        assert_eq!(product("99.0"), "SQL Server");
        assert_eq!(wait_class("LCK_M_U"), "Lock");
        assert_eq!(wait_class("PAGEIOLATCH_SH"), "Disk I/O");
        assert_eq!(wait_class("WRITELOG"), "Log");
        assert_eq!(wait_class("CXPACKET"), "Parallelism");
        assert_eq!(wait_class(""), "CPU");
        assert_eq!(wait_class("SOMETHING_NEW"), "Other");
    }

    #[test]
    fn findings() {
        let blocked = |session, wait_secs| Blocked {
            session,
            wait_secs,
            blocker: 57,
            blocker_login: Some("app".into()),
            blocker_idle: true,
            ..Default::default()
        };
        let snap = MsSnapshot {
            server: Some(ServerInfo {
                max_connections: 100,
                ..Default::default()
            }),
            gauges: Some(Gauges {
                user_connections: 92,
                page_life_expectancy: 120.0,
                memory_grants_pending: 2,
                ..Default::default()
            }),
            blocked: vec![blocked(60, 15.0), blocked(61, 4.0)],
            transactions: vec![Transaction {
                session: 57,
                login: "app".into(),
                age_secs: 125,
                log_bytes: 2048.0,
                idle: true,
                ..Default::default()
            }],
            log_used: vec![("shop".into(), 93.0), ("tempdb".into(), 12.0)],
            ..Default::default()
        };
        let rates = Counters {
            batch_requests: 100.0,
            compilations: 60.0,
            page_lookups: 1000.0,
            page_reads: 200.0,
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
                    "Session 57 (app) blocks 2 sessions, longest wait 15s — idle, likely an \
                     uncommitted transaction"
                        .into()
                ),
                (
                    Severity::Critical,
                    "92 of 100 connections in use (92 %)".into()
                ),
                (
                    Severity::Warning,
                    "The log of database shop is 93 % full".into()
                ),
                (
                    Severity::Warning,
                    "2 queries waiting for a memory grant".into()
                ),
                (
                    Severity::Warning,
                    "Page life expectancy is 2m 0s: pages leave the buffer pool quickly \
                     (memory pressure)"
                        .into()
                ),
                (
                    Severity::Warning,
                    "Buffer cache hit ratio is 80.0 %: 200 pages per second are read from disk"
                        .into()
                ),
                (
                    Severity::Warning,
                    "Session 57 (app) has had a transaction open for 2m 5s while idle, 2.0 KB \
                     of log"
                        .into()
                ),
                (
                    Severity::Info,
                    "60 % of batches compile a plan: ad-hoc SQL without parameters?".into()
                ),
            ]
        );
        assert!(insights(&MsSnapshot::default(), None).is_empty());
    }
}
