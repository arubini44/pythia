//! Monitoring queries against the dynamic performance views.
//!
//! Every function takes a plain `&oracle::Connection` and returns plain data,
//! so it can run inside a [`pythia_db::DbRequest::Task`] on a dedicated
//! monitoring session (or directly in tests). Sections that fail because of
//! missing privileges are reported per section instead of failing the whole
//! snapshot, so the UI can degrade gracefully.

use pythia_db::oracle::sql_type::{FromSql, OracleType, ToSql};
use pythia_db::oracle::{self, Connection};
use pythia_db::{DbError, compat};

pub type Result<T> = std::result::Result<T, DbError>;

/// Hint shown when a dictionary view isn't accessible.
pub const DICTIONARY_GRANT: &str = "SELECT_CATALOG_ROLE or SELECT ANY DICTIONARY";

/// True for "table or view does not exist" / "insufficient privileges".
pub fn is_privilege_error(e: &DbError) -> bool {
    matches!(
        e.code,
        Some(942) | Some(1031) | Some(1039) | Some(6550) | Some(1924)
    )
}

/// A user-facing explanation for a failed section.
pub fn explain_error(e: &DbError) -> String {
    if is_privilege_error(e) {
        format!(
            "Not accessible with the current privileges (needs {DICTIONARY_GRANT}). {}",
            e.message
        )
    } else {
        e.message.clone()
    }
}

fn query<T>(
    conn: &Connection,
    sql: &str,
    params: &[&dyn ToSql],
    mut f: impl FnMut(&oracle::Row) -> Result<T>,
) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for row in conn.query(sql, params)? {
        out.push(f(&row?)?);
    }
    Ok(out)
}

fn get<T: FromSql>(row: &oracle::Row, idx: usize) -> Result<T> {
    Ok(row.get(idx)?)
}

/// Runs a statement that names objects through `{objects}` in `sql`:
/// `dba_objects`, which lists every schema's objects and is readable with
/// [`DICTIONARY_GRANT`], else `all_objects` (only the objects the user has
/// privileges on).
fn with_objects<T>(sql: &str, mut f: impl FnMut(&str) -> Result<T>) -> Result<T> {
    match f(&sql.replace("{objects}", "dba_objects")) {
        Err(e) if is_privilege_error(&e) => f(&sql.replace("{objects}", "all_objects")),
        r => r,
    }
}

// ----- instance ----------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct InstanceInfo {
    pub instance: String,
    pub host: String,
    pub version: String,
    pub startup: String,
    pub uptime_secs: i64,
    pub database: String,
    pub role: String,
    pub log_mode: String,
    pub cdb: bool,
    pub container: String,
    pub cpus: i64,
}

pub fn instance_info(conn: &Connection) -> Result<InstanceInfo> {
    // Containers came with 12.1; from 18c VERSION is only the base release.
    let container = if compat::at_least(conn, 12, 1)? {
        "d.cdb, sys_context('USERENV', 'CON_NAME')"
    } else {
        "'NO', d.name"
    };
    let version = if compat::at_least(conn, 18, 0)? {
        "i.version_full"
    } else {
        "i.version"
    };
    let row = conn.query_row(
        &format!(
            "select i.instance_name, i.host_name, {version}, \
                    to_char(i.startup_time, 'YYYY-MM-DD HH24:MI:SS'), \
                    round((sysdate - i.startup_time) * 86400), \
                    d.name, d.database_role, d.log_mode, {container}, \
                    (select value from v$osstat where stat_name = 'NUM_CPUS') \
               from v$instance i cross join v$database d"
        ),
        &[],
    )?;
    Ok(InstanceInfo {
        instance: get(&row, 0)?,
        host: get(&row, 1)?,
        version: get(&row, 2)?,
        startup: get(&row, 3)?,
        uptime_secs: get(&row, 4)?,
        database: get(&row, 5)?,
        role: get(&row, 6)?,
        log_mode: get(&row, 7)?,
        cdb: get::<String>(&row, 8)? == "YES",
        container: get(&row, 9)?,
        cpus: get::<Option<i64>>(&row, 10)?.unwrap_or(1),
    })
}

// ----- counters ----------------------------------------------------------

/// Cumulative system counters; rates are computed from two snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Counters {
    pub executions: f64,
    pub logical_reads: f64,
    pub physical_reads: f64,
    pub redo_bytes: f64,
    pub user_commits: f64,
    pub user_rollbacks: f64,
    pub user_calls: f64,
    pub hard_parses: f64,
    /// Centiseconds.
    pub db_time: f64,
    /// Centiseconds.
    pub db_cpu: f64,
    /// OS busy/idle time in centiseconds (all CPUs).
    pub os_busy: f64,
    pub os_idle: f64,
}

pub fn counters(conn: &Connection) -> Result<Counters> {
    let mut c = Counters::default();
    let rows = query(
        conn,
        "select name, value from v$sysstat where name in ('execute count', 'session logical reads', \
         'physical reads', 'redo size', 'user commits', 'user rollbacks', 'user calls', \
         'parse count (hard)', 'DB time', 'CPU used by this session')",
        &[],
        |r| Ok((get::<String>(r, 0)?, get::<f64>(r, 1)?)),
    )?;
    for (name, value) in rows {
        let slot = match name.as_str() {
            "execute count" => &mut c.executions,
            "session logical reads" => &mut c.logical_reads,
            "physical reads" => &mut c.physical_reads,
            "redo size" => &mut c.redo_bytes,
            "user commits" => &mut c.user_commits,
            "user rollbacks" => &mut c.user_rollbacks,
            "user calls" => &mut c.user_calls,
            "parse count (hard)" => &mut c.hard_parses,
            "DB time" => &mut c.db_time,
            "CPU used by this session" => &mut c.db_cpu,
            _ => continue,
        };
        *slot = value;
    }
    // The time model is updated continuously and keeps DB CPU <= DB time;
    // the V$SYSSTAT pair is only a fallback.
    if let Ok(rows) = query(
        conn,
        "select stat_name, value from v$sys_time_model where stat_name in ('DB time', 'DB CPU')",
        &[],
        |r| Ok((get::<String>(r, 0)?, get::<f64>(r, 1)?)),
    ) {
        for (name, micros) in rows {
            match name.as_str() {
                "DB time" => c.db_time = micros / 1e4,
                "DB CPU" => c.db_cpu = micros / 1e4,
                _ => {}
            }
        }
    }
    // V$OSSTAT may be restricted; host CPU is then simply unknown.
    if let Ok(rows) = query(
        conn,
        "select stat_name, value from v$osstat where stat_name in ('BUSY_TIME', 'IDLE_TIME')",
        &[],
        |r| Ok((get::<String>(r, 0)?, get::<f64>(r, 1)?)),
    ) {
        for (name, value) in rows {
            match name.as_str() {
                "BUSY_TIME" => c.os_busy = value,
                "IDLE_TIME" => c.os_idle = value,
                _ => {}
            }
        }
    }
    Ok(c)
}

/// Per-second rates between two counter snapshots.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Rates {
    /// Average active sessions (DB time per second).
    pub aas: f64,
    /// Database CPU in average sessions on CPU.
    pub db_cpu: f64,
    /// Host CPU utilization in percent, if V$OSSTAT is readable.
    pub host_cpu_pct: Option<f64>,
    pub executions: f64,
    pub logical_reads: f64,
    pub physical_reads: f64,
    pub redo_bytes: f64,
    pub commits: f64,
    pub rollbacks: f64,
    pub user_calls: f64,
    pub hard_parses: f64,
}

impl Rates {
    pub fn between(prev: &Counters, cur: &Counters, seconds: f64) -> Self {
        if seconds <= 0.0 {
            return Self::default();
        }
        // Counters can reset (instance restart); clamp negative deltas.
        let d = |a: f64, b: f64| ((b - a).max(0.0)) / seconds;
        let busy = cur.os_busy - prev.os_busy;
        let idle = cur.os_idle - prev.os_idle;
        let host_cpu_pct = (busy + idle > 0.0 && busy >= 0.0).then(|| 100.0 * busy / (busy + idle));
        Self {
            aas: d(prev.db_time, cur.db_time) / 100.0,
            db_cpu: d(prev.db_cpu, cur.db_cpu) / 100.0,
            host_cpu_pct,
            executions: d(prev.executions, cur.executions),
            logical_reads: d(prev.logical_reads, cur.logical_reads),
            physical_reads: d(prev.physical_reads, cur.physical_reads),
            redo_bytes: d(prev.redo_bytes, cur.redo_bytes),
            commits: d(prev.user_commits, cur.user_commits),
            rollbacks: d(prev.user_rollbacks, cur.user_rollbacks),
            user_calls: d(prev.user_calls, cur.user_calls),
            hard_parses: d(prev.hard_parses, cur.hard_parses),
        }
    }
}

// ----- dashboard ---------------------------------------------------------

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionCounts {
    pub total: i64,
    pub user: i64,
    pub active: i64,
    pub inactive: i64,
    pub background: i64,
    pub blocked: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CursorHolder {
    pub sid: i64,
    pub serial: i64,
    pub username: Option<String>,
    pub program: Option<String>,
    pub open: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tablespace {
    pub name: String,
    pub used_pct: f64,
    pub used_bytes: f64,
    pub max_bytes: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopSql {
    pub sql_id: String,
    pub executions: i64,
    pub elapsed_s: f64,
    pub cpu_s: f64,
    pub buffer_gets: i64,
    pub disk_reads: i64,
    pub rows: i64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LongOp {
    pub sid: i64,
    pub serial: i64,
    pub opname: String,
    pub target: Option<String>,
    pub sofar: f64,
    pub totalwork: f64,
    pub units: Option<String>,
    pub elapsed_s: i64,
    pub remaining_s: Option<i64>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Memory {
    pub sga: Vec<(String, f64)>,
    pub pga_allocated: f64,
    pub pga_inuse: f64,
    pub pga_target: f64,
}

/// A failed dashboard section: (section name, explanation).
pub type SectionError = (&'static str, String);

/// A session at the head of a blocking chain.
#[derive(Debug, Clone, PartialEq)]
pub struct Blocker {
    pub sid: i64,
    pub serial: i64,
    pub username: Option<String>,
    pub status: String,
    /// Sessions waiting on it, directly or indirectly.
    pub blocked: i64,
    /// Longest current wait among them, in seconds.
    pub max_wait: f64,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DashboardSnapshot {
    pub instance: Option<InstanceInfo>,
    pub counters: Option<Counters>,
    pub sessions: Option<SessionCounts>,
    /// Final blockers, most blocking first.
    pub blockers: Vec<Blocker>,
    pub by_user: Vec<(String, i64)>,
    pub by_machine: Vec<(String, i64)>,
    pub by_program: Vec<(String, i64)>,
    /// Active user sessions by wait class (`CPU` for sessions on CPU).
    pub activity: Vec<(String, i64)>,
    pub open_cursors: Option<i64>,
    /// The `open_cursors` parameter (per-session limit).
    pub open_cursors_limit: Option<i64>,
    pub cursor_holders: Vec<CursorHolder>,
    pub memory: Option<Memory>,
    pub tablespaces: Vec<Tablespace>,
    pub top_sql: Vec<TopSql>,
    pub long_ops: Vec<LongOp>,
    pub errors: Vec<SectionError>,
}

/// Collects everything the dashboard shows. Never fails as a whole.
pub fn dashboard_snapshot(conn: &Connection) -> DashboardSnapshot {
    let mut s = DashboardSnapshot::default();
    let section = |name: &'static str, errors: &mut Vec<SectionError>, r: Result<()>| {
        if let Err(e) = r {
            errors.push((name, explain_error(&e)));
        }
    };
    let mut errors = Vec::new();

    section(
        "Instance",
        &mut errors,
        instance_info(conn).map(|i| s.instance = Some(i)),
    );
    section(
        "Counters",
        &mut errors,
        counters(conn).map(|c| s.counters = Some(c)),
    );
    section(
        "Sessions",
        &mut errors,
        (|| {
            let r = conn.query_row(
                "select count(*), count(case when type = 'USER' then 1 end), \
                        count(case when type = 'USER' and status = 'ACTIVE' then 1 end), \
                        count(case when type = 'USER' and status = 'INACTIVE' then 1 end), \
                        count(case when type = 'BACKGROUND' then 1 end), \
                        count(case when blocking_session is not null then 1 end) \
                   from v$session",
                &[],
            )?;
            s.sessions = Some(SessionCounts {
                total: get(&r, 0)?,
                user: get(&r, 1)?,
                active: get(&r, 2)?,
                inactive: get(&r, 3)?,
                background: get(&r, 4)?,
                blocked: get(&r, 5)?,
            });
            let group = |col: &str| {
                query(
                    conn,
                    &compat::first_rows(
                        &format!(
                            "select nvl({col}, '(unknown)'), count(*) from v$session \
                              where type = 'USER' group by {col} order by 2 desc, 1"
                        ),
                        8,
                    ),
                    &[],
                    |r| Ok((get(r, 0)?, get(r, 1)?)),
                )
            };
            s.blockers = blockers(conn)?;
            s.by_user = group("username")?;
            s.by_machine = group("machine")?;
            s.by_program = group("program")?;
            s.activity = query(
                conn,
                "select case when state = 'WAITING' then wait_class else 'CPU' end, count(*) \
                   from v$session \
                  where type = 'USER' and status = 'ACTIVE' \
                    and (state <> 'WAITING' or wait_class <> 'Idle') \
                    and sid <> sys_context('USERENV', 'SID') \
                  group by case when state = 'WAITING' then wait_class else 'CPU' end \
                  order by 2 desc",
                &[],
                |r| Ok((get(r, 0)?, get(r, 1)?)),
            )?;
            Ok(())
        })(),
    );
    section(
        "Open cursors",
        &mut errors,
        (|| {
            s.open_cursors = Some(conn.query_row_as::<i64>(
                "select value from v$sysstat where name = 'opened cursors current'",
                &[],
            )?);
            s.open_cursors_limit = conn
                .query_row_as::<Option<i64>>(
                    "select to_number(value) from v$parameter where name = 'open_cursors'",
                    &[],
                )
                .ok()
                .flatten();
            s.cursor_holders = query(
                conn,
                &compat::first_rows(
                    "select s.sid, s.serial#, s.username, s.program, st.value \
                       from v$sesstat st \
                       join v$statname n on n.statistic# = st.statistic# \
                       join v$session s on s.sid = st.sid \
                      where n.name = 'opened cursors current' and st.value > 0 \
                      order by st.value desc",
                    10,
                ),
                &[],
                |r| {
                    Ok(CursorHolder {
                        sid: get(r, 0)?,
                        serial: get(r, 1)?,
                        username: get(r, 2)?,
                        program: get(r, 3)?,
                        open: get(r, 4)?,
                    })
                },
            )?;
            Ok(())
        })(),
    );
    section(
        "Memory",
        &mut errors,
        memory(conn).map(|m| s.memory = Some(m)),
    );
    section(
        "Tablespaces",
        &mut errors,
        tablespaces(conn).map(|t| s.tablespaces = t),
    );
    section(
        "Top SQL",
        &mut errors,
        top_sql(conn, 15).map(|t| s.top_sql = t),
    );
    section(
        "Long operations",
        &mut errors,
        long_ops(conn).map(|l| s.long_ops = l),
    );
    s.errors = errors;
    s
}

/// Heads of blocking chains, most blocking first.
///
/// Uses `BLOCKING_SESSION` (populated everywhere) rather than
/// `FINAL_BLOCKING_SESSION`, which some versions leave empty in a PDB.
pub fn blockers(conn: &Connection) -> Result<Vec<Blocker>> {
    let rows = query(
        conn,
        "select sid, serial#, username, status, blocking_session, \
                case when state = 'WAITING' then wait_time_micro / 1e6 else 0 end \
           from v$session \
          where blocking_session is not null \
             or sid in (select blocking_session from v$session where blocking_session is not null)",
        &[],
        |r| {
            Ok(SessionRow {
                sid: get(r, 0)?,
                serial: get(r, 1)?,
                username: get(r, 2)?,
                status: get::<Option<String>>(r, 3)?.unwrap_or_default(),
                blocking_session: get(r, 4)?,
                wait_secs: get::<Option<f64>>(r, 5)?.unwrap_or(0.0),
                ..SessionRow::default()
            })
        },
    )?;
    Ok(build_blocking_tree(&rows)
        .into_iter()
        .map(|root| Blocker {
            blocked: root.blocked_count() as i64,
            max_wait: root.max_wait(),
            sid: root.session.sid,
            serial: root.session.serial,
            username: root.session.username,
            status: root.session.status,
        })
        .collect())
}

pub fn memory(conn: &Connection) -> Result<Memory> {
    let sga = query(
        conn,
        "select name, value from v$sga order by value desc",
        &[],
        |r| Ok((get(r, 0)?, get(r, 1)?)),
    )?;
    let mut m = Memory {
        sga,
        ..Memory::default()
    };
    for (name, value) in query(
        conn,
        "select name, value from v$pgastat where name in \
         ('total PGA allocated', 'total PGA inuse', 'aggregate PGA target parameter')",
        &[],
        |r| Ok((get::<String>(r, 0)?, get::<f64>(r, 1)?)),
    )? {
        match name.as_str() {
            "total PGA allocated" => m.pga_allocated = value,
            "total PGA inuse" => m.pga_inuse = value,
            _ => m.pga_target = value,
        }
    }
    Ok(m)
}

pub fn tablespaces(conn: &Connection) -> Result<Vec<Tablespace>> {
    query(
        conn,
        "select m.tablespace_name, m.used_percent, m.used_space * t.block_size, m.tablespace_size * t.block_size \
           from dba_tablespace_usage_metrics m \
           join dba_tablespaces t on t.tablespace_name = m.tablespace_name \
          order by m.used_percent desc",
        &[],
        |r| {
            Ok(Tablespace {
                name: get(r, 0)?,
                used_pct: get(r, 1)?,
                used_bytes: get(r, 2)?,
                max_bytes: get(r, 3)?,
            })
        },
    )
}

pub fn top_sql(conn: &Connection, limit: usize) -> Result<Vec<TopSql>> {
    query(
        conn,
        &compat::first_rows(
            "select sql_id, executions, elapsed_time / 1e6, cpu_time / 1e6, buffer_gets, disk_reads, \
                    rows_processed, substr(sql_text, 1, 300) \
               from v$sqlstats order by elapsed_time desc",
            limit,
        ),
        &[],
        |r| {
            Ok(TopSql {
                sql_id: get(r, 0)?,
                executions: get(r, 1)?,
                elapsed_s: get(r, 2)?,
                cpu_s: get(r, 3)?,
                buffer_gets: get(r, 4)?,
                disk_reads: get(r, 5)?,
                rows: get(r, 6)?,
                text: get::<Option<String>>(r, 7)?.unwrap_or_default(),
            })
        },
    )
}

pub fn long_ops(conn: &Connection) -> Result<Vec<LongOp>> {
    query(
        conn,
        &compat::first_rows(
            "select sid, serial#, opname, target, sofar, totalwork, units, elapsed_seconds, time_remaining, message \
               from v$session_longops \
              where sofar < totalwork \
              order by start_time desc",
            20,
        ),
        &[],
        |r| {
            Ok(LongOp {
                sid: get(r, 0)?,
                serial: get(r, 1)?,
                opname: get::<Option<String>>(r, 2)?.unwrap_or_default(),
                target: get(r, 3)?,
                sofar: get::<Option<f64>>(r, 4)?.unwrap_or(0.0),
                totalwork: get::<Option<f64>>(r, 5)?.unwrap_or(0.0),
                units: get(r, 6)?,
                elapsed_s: get::<Option<i64>>(r, 7)?.unwrap_or(0),
                remaining_s: get(r, 8)?,
                message: get(r, 9)?,
            })
        },
    )
}

// ----- sessions ----------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionRow {
    pub sid: i64,
    pub serial: i64,
    pub username: Option<String>,
    pub status: String,
    pub osuser: Option<String>,
    pub machine: Option<String>,
    pub program: Option<String>,
    pub module: Option<String>,
    pub action: Option<String>,
    pub sql_id: Option<String>,
    pub prev_sql_id: Option<String>,
    pub event: Option<String>,
    pub wait_class: Option<String>,
    /// `WAITING`, `WAITED KNOWN TIME`, ...
    pub state: Option<String>,
    /// Seconds in the current wait (0 when on CPU).
    pub wait_secs: f64,
    pub blocking_session: Option<i64>,
    pub final_blocking_session: Option<i64>,
    pub logon_time: String,
    /// Seconds since the last call started (active) or ended (inactive).
    pub last_call_et: i64,
    pub background: bool,
    /// Object waited on (for row lock waits), `OWNER.NAME`.
    pub wait_object: Option<String>,
    /// Row waited on, if the session waits for a row lock.
    pub wait_rowid: Option<String>,
    pub sql_text: Option<String>,
    /// The monitoring session itself.
    pub is_self: bool,
}

impl SessionRow {
    pub fn is_blocked(&self) -> bool {
        self.blocking_session.is_some()
    }

    pub fn is_active(&self) -> bool {
        self.status == "ACTIVE"
    }

    /// A blocker the session list doesn't have, in the blocking tree: only
    /// its SID is known (see [`SessionRow::unknown`]).
    pub fn is_unknown(&self) -> bool {
        self.status == UNKNOWN
    }

    fn unknown(sid: i64) -> Self {
        SessionRow {
            sid,
            status: UNKNOWN.into(),
            ..SessionRow::default()
        }
    }

    /// `sid,serial#`, as used in `ALTER SYSTEM KILL SESSION`.
    pub fn handle(&self) -> String {
        format!("{},{}", self.sid, self.serial)
    }
}

/// Status of [`SessionRow::unknown`] (not one of `V$SESSION.STATUS`).
const UNKNOWN: &str = "UNKNOWN";

const SESSION_SELECT: &str = "\
select s.sid, s.serial#, s.username, s.status, s.osuser, s.machine, s.program, s.module, s.action, \
       s.sql_id, s.prev_sql_id, s.event, s.wait_class, s.state, \
       case when s.state = 'WAITING' then s.wait_time_micro / 1e6 else 0 end, \
       s.blocking_session, s.final_blocking_session, \
       to_char(s.logon_time, 'YYYY-MM-DD HH24:MI:SS'), s.last_call_et, s.type, \
       case when s.row_wait_obj# > 0 then o.owner || '.' || o.object_name end, \
       case when s.row_wait_obj# > 0 and s.event like 'enq: TX%' and o.data_object_id is not null then \
         rowidtochar(dbms_rowid.rowid_create(1, o.data_object_id, s.row_wait_file#, s.row_wait_block#, s.row_wait_row#)) end, \
       (select substr(q.sql_text, 1, 300) from v$sql q where q.sql_id = s.sql_id and rownum = 1), \
       case when s.sid = sys_context('USERENV', 'SID') then 1 else 0 end \
  from v$session s \
  left join {objects} o on o.object_id = s.row_wait_obj#";

fn session_row(r: &oracle::Row) -> Result<SessionRow> {
    Ok(SessionRow {
        sid: get(r, 0)?,
        serial: get(r, 1)?,
        username: get(r, 2)?,
        status: get::<Option<String>>(r, 3)?.unwrap_or_default(),
        osuser: get(r, 4)?,
        machine: get(r, 5)?,
        program: get(r, 6)?,
        module: get(r, 7)?,
        action: get(r, 8)?,
        sql_id: get(r, 9)?,
        prev_sql_id: get(r, 10)?,
        event: get(r, 11)?,
        wait_class: get(r, 12)?,
        state: get(r, 13)?,
        wait_secs: get::<Option<f64>>(r, 14)?.unwrap_or(0.0),
        blocking_session: get(r, 15)?,
        final_blocking_session: get(r, 16)?,
        logon_time: get::<Option<String>>(r, 17)?.unwrap_or_default(),
        last_call_et: get::<Option<i64>>(r, 18)?.unwrap_or(0),
        background: get::<Option<String>>(r, 19)?.as_deref() == Some("BACKGROUND"),
        wait_object: get(r, 20)?,
        wait_rowid: get(r, 21)?,
        sql_text: get(r, 22)?,
        is_self: get::<i64>(r, 23)? == 1,
    })
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionFilter {
    pub include_background: bool,
}

pub fn sessions(conn: &Connection, filter: SessionFilter) -> Result<Vec<SessionRow>> {
    let sql = if filter.include_background {
        format!("{SESSION_SELECT} order by s.sid")
    } else {
        format!("{SESSION_SELECT} where s.type = 'USER' order by s.sid")
    };
    with_objects(&sql, |sql| query(conn, sql, &[], session_row))
}

/// A session and the sessions it blocks.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockingNode {
    pub session: SessionRow,
    pub children: Vec<BlockingNode>,
}

impl BlockingNode {
    /// Number of sessions waiting on this one, directly or indirectly.
    pub fn blocked_count(&self) -> usize {
        self.children.iter().map(|c| 1 + c.blocked_count()).sum()
    }

    /// Longest wait in this subtree, in seconds.
    pub fn max_wait(&self) -> f64 {
        self.children
            .iter()
            .map(|c| c.session.wait_secs.max(c.max_wait()))
            .fold(0.0, f64::max)
    }
}

/// Builds the blocker → waiter forest from a session list.
///
/// Roots are sessions that block others but aren't blocked themselves (the
/// final blockers). A waiter whose blocker isn't in the list (e.g. another
/// instance, or a background session filtered out) gets a placeholder root.
/// Cycles (deadlocks in progress) are broken at the session with the lowest SID.
pub fn build_blocking_tree(sessions: &[SessionRow]) -> Vec<BlockingNode> {
    use std::collections::{HashMap, HashSet};
    let by_sid: HashMap<i64, &SessionRow> = sessions.iter().map(|s| (s.sid, s)).collect();
    let mut children: HashMap<i64, Vec<i64>> = HashMap::new();
    for s in sessions {
        if let Some(b) = s.blocking_session {
            children.entry(b).or_default().push(s.sid);
        }
    }
    for list in children.values_mut() {
        list.sort_unstable();
    }

    // Blockers that aren't blocked (or whose blocker is unknown) are roots.
    let mut roots: Vec<i64> = children
        .keys()
        .copied()
        .filter(
            |sid| match by_sid.get(sid).and_then(|s| s.blocking_session) {
                None => true,
                Some(b) => !by_sid.contains_key(&b) && !children.contains_key(&b),
            },
        )
        .collect();
    // Blockers referenced but missing from the list.
    roots.sort_unstable();

    fn build(
        sid: i64,
        by_sid: &HashMap<i64, &SessionRow>,
        children: &HashMap<i64, Vec<i64>>,
        seen: &mut HashSet<i64>,
    ) -> BlockingNode {
        seen.insert(sid);
        let session = by_sid
            .get(&sid)
            .map(|s| (*s).clone())
            .unwrap_or_else(|| SessionRow::unknown(sid));
        let kids = children
            .get(&sid)
            .into_iter()
            .flatten()
            .filter(|c| !seen.contains(c))
            .copied()
            .collect::<Vec<_>>();
        let children = kids
            .into_iter()
            .map(|c| build(c, by_sid, children, seen))
            .collect();
        BlockingNode { session, children }
    }

    let mut seen = HashSet::new();
    let mut forest: Vec<BlockingNode> = roots
        .into_iter()
        .map(|sid| build(sid, &by_sid, &children, &mut seen))
        .collect();
    // Anything left is part of a cycle.
    let mut rest: Vec<i64> = children
        .keys()
        .copied()
        .filter(|s| !seen.contains(s))
        .collect();
    rest.sort_unstable();
    for sid in rest {
        if !seen.contains(&sid) {
            forest.push(build(sid, &by_sid, &children, &mut seen));
        }
    }
    forest.sort_by(|a, b| {
        b.blocked_count()
            .cmp(&a.blocked_count())
            .then(a.session.sid.cmp(&b.session.sid))
    });
    forest
}

pub fn blocking_tree(conn: &Connection) -> Result<Vec<BlockingNode>> {
    Ok(build_blocking_tree(&sessions(
        conn,
        SessionFilter {
            include_background: true,
        },
    )?))
}

// ----- session detail ----------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct WaitHistory {
    pub seq: i64,
    pub event: String,
    pub wait_ms: f64,
    pub params: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LockRow {
    pub lock_type: String,
    pub id1: i64,
    pub id2: i64,
    pub mode_held: String,
    pub mode_requested: String,
    pub seconds: i64,
    pub blocking: bool,
    pub object: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenCursor {
    pub sql_id: Option<String>,
    pub text: String,
    pub cursor_type: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionDetail {
    pub session: Option<SessionRow>,
    pub sql_text: Option<String>,
    pub prev_sql_text: Option<String>,
    pub plan: Vec<String>,
    pub waits: Vec<WaitHistory>,
    pub stats: Vec<(String, f64)>,
    pub cursors: Vec<OpenCursor>,
    pub locks: Vec<LockRow>,
    pub errors: Vec<SectionError>,
}

pub fn session_detail(conn: &Connection, sid: i64, serial: i64) -> SessionDetail {
    let mut d = SessionDetail::default();
    let mut errors = Vec::new();
    let mut section = |name: &'static str, r: Result<()>| {
        if let Err(e) = r {
            errors.push((name, explain_error(&e)));
        }
    };
    section(
        "Session",
        (|| {
            let sql = format!("{SESSION_SELECT} where s.sid = :1 and s.serial# = :2");
            d.session = with_objects(&sql, |sql| query(conn, sql, &[&sid, &serial], session_row))?
                .into_iter()
                .next();
            Ok(())
        })(),
    );
    // The other views know sessions by SID alone: once the session has
    // ended, its SID may belong to another one.
    if d.session.is_none() {
        d.errors = errors;
        return d;
    }
    let sql_id = d.session.as_ref().and_then(|s| s.sql_id.clone());
    let prev_sql_id = d.session.as_ref().and_then(|s| s.prev_sql_id.clone());
    let full_text = |id: &str| -> Result<Option<String>> {
        Ok(query(
            conn,
            "select sql_fulltext from v$sql where sql_id = :1 and rownum = 1",
            &[&id],
            |r| get::<Option<String>>(r, 0),
        )?
        .into_iter()
        .next()
        .flatten())
    };
    section(
        "SQL",
        (|| {
            if let Some(id) = &sql_id {
                d.sql_text = full_text(id)?;
            }
            if let Some(id) = &prev_sql_id {
                d.prev_sql_text = full_text(id)?;
            }
            Ok(())
        })(),
    );
    if let Some(id) = sql_id.as_ref().or(prev_sql_id.as_ref()) {
        section(
            "Plan",
            query(
                conn,
                "select plan_table_output from table(dbms_xplan.display_cursor(:1, null, 'TYPICAL'))",
                &[id],
                |r| Ok(get::<Option<String>>(r, 0)?.unwrap_or_default()),
            )
            .map(|p| d.plan = p),
        );
    }
    section(
        "Wait history",
        query(
            conn,
            "select seq#, event, wait_time_micro / 1000, \
                    p1text || '=' || p1 || ' ' || p2text || '=' || p2 || ' ' || p3text || '=' || p3 \
               from v$session_wait_history where sid = :1 order by seq#",
            &[&sid],
            |r| {
                Ok(WaitHistory {
                    seq: get(r, 0)?,
                    event: get::<Option<String>>(r, 1)?.unwrap_or_default(),
                    wait_ms: get::<Option<f64>>(r, 2)?.unwrap_or(0.0),
                    params: get::<Option<String>>(r, 3)?.unwrap_or_default(),
                })
            },
        )
        .map(|w| d.waits = w),
    );
    section(
        "Statistics",
        query(
            conn,
            "select n.name, s.value from v$sesstat s join v$statname n on n.statistic# = s.statistic# \
              where s.sid = :1 and s.value <> 0 order by n.name",
            &[&sid],
            |r| Ok((get(r, 0)?, get(r, 1)?)),
        )
        .map(|s| d.stats = s),
    );
    section(
        "Open cursors",
        query(
            conn,
            "select sql_id, sql_text, cursor_type from v$open_cursor where sid = :1 order by sql_text",
            &[&sid],
            |r| {
                Ok(OpenCursor {
                    sql_id: get(r, 0)?,
                    text: get::<Option<String>>(r, 1)?.unwrap_or_default(),
                    cursor_type: get(r, 2)?,
                })
            },
        )
        .map(|c| d.cursors = c),
    );
    section("Locks", locks(conn, sid).map(|l| d.locks = l));
    d.errors = errors;
    d
}

fn lock_mode(m: i64) -> String {
    match m {
        0 => "None",
        1 => "Null",
        2 => "Row-S (SS)",
        3 => "Row-X (SX)",
        4 => "Share",
        5 => "S/Row-X (SSX)",
        6 => "Exclusive",
        _ => return m.to_string(),
    }
    .to_owned()
}

pub fn locks(conn: &Connection, sid: i64) -> Result<Vec<LockRow>> {
    let sql = "select l.type, l.id1, l.id2, l.lmode, l.request, l.ctime, l.block, \
                      (select o.owner || '.' || o.object_name from {objects} o \
                        where o.object_id = l.id1 and l.type = 'TM') \
                 from v$lock l where l.sid = :1 order by l.type, l.id1";
    with_objects(sql, |sql| locks_with(conn, sql, sid))
}

fn locks_with(conn: &Connection, sql: &str, sid: i64) -> Result<Vec<LockRow>> {
    query(conn, sql, &[&sid], |r| {
        Ok(LockRow {
            lock_type: get(r, 0)?,
            id1: get(r, 1)?,
            id2: get(r, 2)?,
            mode_held: lock_mode(get(r, 3)?),
            mode_requested: lock_mode(get(r, 4)?),
            seconds: get(r, 5)?,
            blocking: get::<i64>(r, 6)? > 0,
            object: get(r, 7)?,
        })
    })
}

// ----- session control ---------------------------------------------------

fn session_handle(sid: i64, serial: i64) -> String {
    // Both are numbers, so formatting them into the statement is safe.
    format!("'{sid},{serial}'")
}

pub fn kill_session(conn: &Connection, sid: i64, serial: i64, immediate: bool) -> Result<()> {
    // A session busy in a call is only marked for kill (ORA-00031) and goes
    // when the call returns. The driver takes ORA-00031 on any call for
    // "this connection was killed" and would drop the monitor's own
    // connection, so it is caught here.
    let sql = format!(
        "begin\n  execute immediate 'alter system kill session {}{}';\n\
         exception\n  when others then\n    if sqlcode != -31 then raise; end if;\nend;",
        session_handle(sid, serial).replace('\'', "''"),
        if immediate { " immediate" } else { "" }
    );
    conn.execute(&sql, &[])?;
    Ok(())
}

/// Cancels the session's current call (Oracle 18c+).
pub fn cancel_sql(conn: &Connection, sid: i64, serial: i64) -> Result<()> {
    conn.execute(
        &format!("alter system cancel sql {}", session_handle(sid, serial)),
        &[],
    )?;
    Ok(())
}

// ----- tracing -----------------------------------------------------------

pub fn trace_enable(
    conn: &Connection,
    sid: i64,
    serial: i64,
    waits: bool,
    binds: bool,
) -> Result<()> {
    conn.execute(
        "begin dbms_monitor.session_trace_enable(session_id => :1, serial_num => :2, \
           waits => :3 = 1, binds => :4 = 1, plan_stat => 'ALL_EXECUTIONS'); end;",
        &[&sid, &serial, &i32::from(waits), &i32::from(binds)],
    )?;
    Ok(())
}

pub fn trace_disable(conn: &Connection, sid: i64, serial: i64) -> Result<()> {
    conn.execute(
        "begin dbms_monitor.session_trace_disable(session_id => :1, serial_num => :2); end;",
        &[&sid, &serial],
    )?;
    Ok(())
}

/// Enables 10046 tracing (waits and binds) for the calling session itself.
pub fn trace_self(conn: &Connection, on: bool) -> Result<()> {
    let sql = if on {
        "begin dbms_monitor.session_trace_enable(waits => true, binds => true, plan_stat => 'ALL_EXECUTIONS'); end;"
    } else {
        "begin dbms_monitor.session_trace_disable; end;"
    };
    conn.execute(sql, &[])?;
    Ok(())
}

/// Trace file of a session (from V$PROCESS), as (directory, file name).
pub fn session_trace_file(conn: &Connection, sid: i64) -> Result<Option<(String, String)>> {
    let path: Option<String> = query(
        conn,
        "select p.tracefile from v$session s join v$process p on p.addr = s.paddr where s.sid = :1",
        &[&sid],
        |r| get(r, 0),
    )?
    .into_iter()
    .next()
    .flatten();
    Ok(path.and_then(|p| {
        let (dir, file) = p.rsplit_once('/')?;
        Some((dir.to_owned(), file.to_owned()))
    }))
}

#[derive(Debug, Clone, PartialEq)]
pub struct TraceFile {
    pub adr_home: String,
    pub name: String,
    pub changed: String,
}

pub fn trace_files(conn: &Connection, filter: &str, limit: usize) -> Result<Vec<TraceFile>> {
    let pattern = format!("%{}%", filter.trim());
    query(
        conn,
        &compat::first_rows(
            "select adr_home, trace_filename, to_char(change_time, 'YYYY-MM-DD HH24:MI:SS') \
               from v$diag_trace_file \
              where trace_filename like :1 and trace_filename like '%.trc' \
              order by change_time desc",
            limit,
        ),
        &[&pattern],
        |r| {
            Ok(TraceFile {
                adr_home: get(r, 0)?,
                name: get(r, 1)?,
                changed: get::<Option<String>>(r, 2)?.unwrap_or_default(),
            })
        },
    )
}

/// Reads a trace file through V$DIAG_TRACE_FILE_CONTENTS (Oracle 12.2+).
///
/// `adr_home` may also be the directory of the trace file (as returned by
/// [`session_trace_file`]); its `/trace` suffix is stripped.
pub fn read_trace_file(conn: &Connection, adr_home: &str, name: &str) -> Result<String> {
    let home = adr_home.strip_suffix("/trace").unwrap_or(adr_home);
    let rows = conn.query_as::<Option<String>>(
        "select payload from v$diag_trace_file_contents \
          where adr_home = :1 and trace_filename = :2 order by line_number",
        &[&home, &name],
    )?;
    let mut text = String::new();
    for line in rows {
        let line = line?.unwrap_or_default();
        text.push_str(&line);
        if !line.ends_with('\n') {
            text.push('\n');
        }
    }
    Ok(text)
}

// ----- AWR / ASH ---------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct AwrSnapshot {
    pub snap_id: i64,
    pub dbid: i64,
    pub instance: i64,
    pub begin: String,
    pub end: String,
}

/// The `V$DATABASE` column with the AWR id of the current container
/// (`CON_DBID`, 12.1+), or of the database before containers.
fn dbid_column(conn: &Connection) -> Result<&'static str> {
    Ok(if compat::at_least(conn, 12, 1)? {
        "con_dbid"
    } else {
        "dbid"
    })
}

pub fn awr_snapshots(conn: &Connection, limit: usize) -> Result<Vec<AwrSnapshot>> {
    let dbid = dbid_column(conn)?;
    query(
        conn,
        &compat::first_rows(
            &format!(
                "select snap_id, dbid, instance_number, \
                        to_char(begin_interval_time, 'YYYY-MM-DD HH24:MI'), \
                        to_char(end_interval_time, 'YYYY-MM-DD HH24:MI') \
                   from dba_hist_snapshot \
                  where dbid = (select {dbid} from v$database) \
                  order by snap_id desc"
            ),
            limit,
        ),
        &[],
        |r| {
            Ok(AwrSnapshot {
                snap_id: get(r, 0)?,
                dbid: get(r, 1)?,
                instance: get(r, 2)?,
                begin: get(r, 3)?,
                end: get(r, 4)?,
            })
        },
    )
}

/// Creates an AWR snapshot and returns its id.
pub fn awr_create_snapshot(conn: &Connection) -> Result<i64> {
    let mut stmt = conn
        .statement("begin :1 := dbms_workload_repository.create_snapshot(); end;")
        .build()?;
    stmt.execute(&[&OracleType::Number(0, 0)])?;
    Ok(stmt.bind_value(1)?)
}

fn collect_lines(conn: &Connection, sql: &str, params: &[&dyn ToSql]) -> Result<String> {
    let mut out = String::new();
    for line in conn.query_as::<Option<String>>(sql, params)? {
        out.push_str(&line?.unwrap_or_default());
        out.push('\n');
    }
    Ok(out)
}

/// AWR report between two snapshots, as text or HTML.
pub fn awr_report(
    conn: &Connection,
    dbid: i64,
    instance: i64,
    begin: i64,
    end: i64,
    html: bool,
) -> Result<String> {
    let func = if html {
        "awr_report_html"
    } else {
        "awr_report_text"
    };
    collect_lines(
        conn,
        &format!("select output from table(dbms_workload_repository.{func}(:1, :2, :3, :4))"),
        &[&dbid, &instance, &begin, &end],
    )
}

/// ASH report for the last `minutes` minutes, as text or HTML.
pub fn ash_report(conn: &Connection, minutes: i64, html: bool) -> Result<String> {
    let func = if html {
        "ash_report_html"
    } else {
        "ash_report_text"
    };
    let dbid = dbid_column(conn)?;
    collect_lines(
        conn,
        &format!(
            "select output from table(dbms_workload_repository.{func}( \
               (select {dbid} from v$database), \
               (select instance_number from v$instance), \
               sysdate - :1 / 1440, sysdate))"
        ),
        &[&minutes],
    )
}

// ----- capabilities ------------------------------------------------------

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Capabilities {
    pub user: String,
    /// Probed views: (name, accessible).
    pub views: Vec<(&'static str, bool)>,
    /// `control_management_pack_access`, if readable.
    pub pack_access: Option<String>,
    pub alter_system: bool,
    pub dbms_monitor: bool,
    pub dbms_workload_repository: bool,
    /// The server's release, for statements newer versions have.
    pub release: compat::Release,
}

const PROBED_VIEWS: &[&str] = &[
    "V$SESSION",
    "V$SYSSTAT",
    "V$OSSTAT",
    "V$SESSTAT",
    "V$SQL",
    "V$SQLSTATS",
    "V$LOCK",
    "V$OPEN_CURSOR",
    "V$SESSION_LONGOPS",
    "V$SESSION_WAIT_HISTORY",
    "V$SGA",
    "V$PGASTAT",
    "V$PARAMETER",
    "V$PROCESS",
    "DBA_TABLESPACE_USAGE_METRICS",
    "V$DIAG_TRACE_FILE",
    "V$DIAG_TRACE_FILE_CONTENTS",
    "DBA_HIST_SNAPSHOT",
    "V$ACTIVE_SESSION_HISTORY",
];

impl Capabilities {
    pub fn can(&self, view: &str) -> bool {
        self.views
            .iter()
            .any(|(v, ok)| *ok && v.eq_ignore_ascii_case(view))
    }

    /// True if the Diagnostics Pack is enabled for this database.
    pub fn diagnostics_licensed(&self) -> bool {
        self.pack_access
            .as_deref()
            .is_some_and(|v| v.to_ascii_uppercase().contains("DIAGNOSTIC"))
    }

    /// Whether AWR/ASH reports may be used, or why not.
    pub fn awr(&self) -> std::result::Result<(), String> {
        match &self.pack_access {
            None => {
                return Err("The control_management_pack_access parameter can't be read, so it is unknown whether \
                     the Oracle Diagnostics Pack is licensed. AWR and ASH are disabled."
                    .into());
            }
            Some(_) if !self.diagnostics_licensed() => {
                return Err(format!(
                    "AWR and ASH require the Oracle Diagnostics Pack, which is not enabled on this database \
                     (control_management_pack_access = {}).",
                    self.pack_access.as_deref().unwrap_or("NONE")
                ));
            }
            _ => {}
        }
        if !self.can("DBA_HIST_SNAPSHOT") || !self.dbms_workload_repository {
            return Err(format!(
                "AWR needs SELECT on DBA_HIST_SNAPSHOT ({DICTIONARY_GRANT}) and EXECUTE on DBMS_WORKLOAD_REPOSITORY."
            ));
        }
        Ok(())
    }

    /// Whether traces can be listed and read through the database.
    pub fn trace_files(&self) -> std::result::Result<(), String> {
        if self.can("V$DIAG_TRACE_FILE") && self.can("V$DIAG_TRACE_FILE_CONTENTS") {
            Ok(())
        } else {
            Err(format!(
                "Reading trace files needs SELECT on V$DIAG_TRACE_FILE and V$DIAG_TRACE_FILE_CONTENTS \
                 (Oracle 12.2+, {DICTIONARY_GRANT}). You can still open a trace file from disk."
            ))
        }
    }

    pub fn tracing(&self) -> std::result::Result<(), String> {
        if self.dbms_monitor {
            Ok(())
        } else {
            Err("Enabling SQL trace needs EXECUTE on DBMS_MONITOR.".into())
        }
    }

    pub fn session_control(&self) -> std::result::Result<(), String> {
        if self.alter_system {
            Ok(())
        } else {
            Err("Killing sessions and cancelling SQL needs the ALTER SYSTEM privilege.".into())
        }
    }

    /// Whether a session's current call can be cancelled, or why not.
    pub fn cancel_sql(&self) -> std::result::Result<(), String> {
        self.session_control()?;
        if self.release >= (18, 0) {
            Ok(())
        } else {
            Err("Cancelling the current SQL of a session needs Oracle 18c or later.".into())
        }
    }

    pub fn missing_views(&self) -> Vec<&'static str> {
        self.views
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(v, _)| *v)
            .collect()
    }
}

pub fn capabilities(conn: &Connection) -> Result<Capabilities> {
    let user: String = conn.query_row_as("select user from dual", &[])?;
    let views = PROBED_VIEWS
        .iter()
        .map(|v| {
            (
                *v,
                conn.query_row_as::<i64>(&format!("select count(*) from {v} where 1 = 0"), &[])
                    .is_ok(),
            )
        })
        .collect();
    let pack_access = conn
        .query_row_as::<Option<String>>(
            "select value from v$parameter where name = 'control_management_pack_access'",
            &[],
        )
        .ok()
        .map(|v| v.unwrap_or_else(|| "NONE".into()));
    let alter_system = conn
        .query_row_as::<i64>(
            "select count(*) from session_privs where privilege = 'ALTER SYSTEM'",
            &[],
        )
        .is_ok_and(|n| n > 0);
    let has_package = |name: &str| {
        conn.query_row_as::<i64>(
            "select count(*) from all_objects where owner = 'SYS' and object_name = :1 and object_type = 'PACKAGE'",
            &[&name],
        )
        .is_ok_and(|n| n > 0)
    };
    Ok(Capabilities {
        user,
        views,
        pack_access,
        alter_system,
        dbms_monitor: has_package("DBMS_MONITOR"),
        dbms_workload_repository: has_package("DBMS_WORKLOAD_REPOSITORY"),
        release: compat::release(conn)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(sid: i64, blocker: Option<i64>) -> SessionRow {
        SessionRow {
            sid,
            serial: 1,
            status: "ACTIVE".into(),
            blocking_session: blocker,
            ..SessionRow::default()
        }
    }

    fn shape(nodes: &[BlockingNode]) -> Vec<String> {
        fn walk(n: &BlockingNode, depth: usize, out: &mut Vec<String>) {
            out.push(format!("{}{}", "  ".repeat(depth), n.session.sid));
            for c in &n.children {
                walk(c, depth + 1, out);
            }
        }
        let mut out = Vec::new();
        for n in nodes {
            walk(n, 0, &mut out);
        }
        out
    }

    #[test]
    fn chain_and_fanout() {
        // 10 blocks 11 and 12; 12 blocks 13; 20 blocks 21; 30 is unrelated.
        let rows = vec![
            s(10, None),
            s(11, Some(10)),
            s(12, Some(10)),
            s(13, Some(12)),
            s(20, None),
            s(21, Some(20)),
            s(30, None),
        ];
        let tree = build_blocking_tree(&rows);
        assert_eq!(shape(&tree), ["10", "  11", "  12", "    13", "20", "  21"]);
        assert_eq!(tree[0].blocked_count(), 3);
    }

    #[test]
    fn unknown_blocker_gets_placeholder() {
        let tree = build_blocking_tree(&[s(5, Some(99)), s(6, Some(99))]);
        assert_eq!(shape(&tree), ["99", "  5", "  6"]);
        assert_eq!(tree[0].session.status, "UNKNOWN");
    }

    #[test]
    fn cycles_terminate() {
        let tree = build_blocking_tree(&[s(1, Some(2)), s(2, Some(1))]);
        assert_eq!(shape(&tree), ["1", "  2"]);
    }

    #[test]
    fn max_wait() {
        let mut a = s(2, Some(1));
        a.wait_secs = 4.0;
        let mut b = s(3, Some(2));
        b.wait_secs = 9.0;
        let tree = build_blocking_tree(&[s(1, None), a, b]);
        assert_eq!(tree[0].max_wait(), 9.0);
    }

    #[test]
    fn rates() {
        let a = Counters {
            db_time: 1000.0,
            executions: 100.0,
            os_busy: 100.0,
            os_idle: 300.0,
            ..Counters::default()
        };
        let b = Counters {
            db_time: 1400.0,
            executions: 300.0,
            os_busy: 200.0,
            os_idle: 400.0,
            ..Counters::default()
        };
        let r = Rates::between(&a, &b, 2.0);
        assert_eq!(r.aas, 2.0);
        assert_eq!(r.executions, 100.0);
        assert_eq!(r.host_cpu_pct, Some(50.0));
        // Counter reset never yields negative rates.
        assert_eq!(Rates::between(&b, &a, 2.0).executions, 0.0);
    }

    #[test]
    fn licensing_gate() {
        let mut caps = Capabilities {
            views: vec![("DBA_HIST_SNAPSHOT", true)],
            dbms_workload_repository: true,
            ..Capabilities::default()
        };
        assert!(caps.awr().is_err());
        caps.pack_access = Some("NONE".into());
        assert!(caps.awr().unwrap_err().contains("Diagnostics Pack"));
        caps.pack_access = Some("DIAGNOSTIC+TUNING".into());
        assert!(caps.awr().is_ok());
        caps.dbms_workload_repository = false;
        assert!(caps.awr().is_err());
    }
}
