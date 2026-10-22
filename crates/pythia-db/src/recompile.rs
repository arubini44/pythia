//! Invalid objects: finding them and recompiling them in dependency order,
//! over several sessions.
//!
//! Objects are compiled in waves: first those whose invalid dependencies are
//! all in earlier waves, each wave spread over the sessions. Compiling in
//! dependency order avoids the repeated cascades of compiling alphabetically,
//! and never compiling an object while something it depends on is being
//! compiled avoids library cache lock waits. Whatever is still invalid at the
//! end (cycles, order effects) gets one more serial pass.

use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::oracle::Connection;

use crate::{ConnectParams, DbError};

/// Which schemas invalid objects are looked for in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidScope {
    Schema(String),
    /// Every schema that isn't maintained by Oracle.
    UserSchemas,
}

impl InvalidScope {
    /// A condition on `o.owner`, and its bind values.
    fn owner_filter(&self, conn: &Connection) -> Result<(String, Vec<String>), DbError> {
        Ok(match self {
            InvalidScope::Schema(s) => ("o.owner = :1".into(), vec![s.clone()]),
            InvalidScope::UserSchemas => (
                format!(
                    "o.owner in ({})",
                    crate::compat::schemas_query(crate::compat::release(conn)?, false)
                ),
                Vec::new(),
            ),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId {
    pub owner: String,
    pub name: String,
    /// `ALL_OBJECTS.OBJECT_TYPE`, e.g. `PACKAGE BODY`.
    pub object_type: String,
}

impl ObjectId {
    pub fn new(owner: &str, name: &str, object_type: &str) -> Self {
        Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
            object_type: object_type.to_owned(),
        }
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} {}.{}",
            self.object_type.to_lowercase(),
            self.owner,
            self.name
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidObject {
    pub id: ObjectId,
    pub last_ddl: String,
    /// Compile errors (not warnings) in `ALL_ERRORS`.
    pub errors: u32,
    /// Line and text of the first error.
    pub first_error: Option<(u32, String)>,
    /// The current user may compile it: in their own schema, or with the
    /// `ALTER ANY …` privilege for its type.
    pub compilable: bool,
}

/// Invalid objects per schema; cheap enough to poll.
pub fn invalid_counts(
    conn: &Connection,
    scope: &InvalidScope,
) -> Result<Vec<(String, u32)>, DbError> {
    let (filter, binds) = scope.owner_filter(conn)?;
    let sql = format!(
        "select o.owner, count(*) from all_objects o where o.status = 'INVALID' and {filter} \
         group by o.owner order by o.owner"
    );
    let binds: Vec<&dyn crate::oracle::sql_type::ToSql> = binds
        .iter()
        .map(|b| b as &dyn crate::oracle::sql_type::ToSql)
        .collect();
    let rows = conn.query_as::<(String, u32)>(&sql, &binds)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The invalid objects with their first error and whether they may be
/// compiled.
pub fn invalid_objects(
    conn: &Connection,
    scope: &InvalidScope,
) -> Result<Vec<InvalidObject>, DbError> {
    let (filter, binds) = scope.owner_filter(conn)?;
    let errors = "from all_errors e where e.owner = o.owner and e.name = o.object_name \
                  and e.type = o.object_type and e.attribute = 'ERROR'";
    let sql = format!(
        "select o.owner, o.object_name, o.object_type, \
                to_char(o.last_ddl_time, 'YYYY-MM-DD HH24:MI:SS'), \
                (select count(*) {errors}), \
                (select min(e.line) keep (dense_rank first order by e.sequence) {errors}), \
                (select min(e.text) keep (dense_rank first order by e.sequence) {errors}) \
         from all_objects o where o.status = 'INVALID' and {filter} \
         order by o.owner, o.object_type, o.object_name"
    );
    let binds: Vec<&dyn crate::oracle::sql_type::ToSql> = binds
        .iter()
        .map(|b| b as &dyn crate::oracle::sql_type::ToSql)
        .collect();
    let user: String = conn.query_row_as(
        "select sys_context('USERENV', 'CURRENT_USER') from dual",
        &[],
    )?;
    let privileges: HashSet<String> = conn
        .query_as::<String>("select privilege from session_privs", &[])?
        .collect::<Result<_, _>>()?;
    let rows = conn.query_as::<(
        String,
        String,
        String,
        Option<String>,
        u32,
        Option<u32>,
        Option<String>,
    )>(&sql, &binds)?;
    let mut out = Vec::new();
    for row in rows {
        let (owner, name, object_type, last_ddl, errors, line, text) = row?;
        let compilable = owner == user
            || compile_privilege(&object_type).is_some_and(|p| privileges.contains(p));
        out.push(InvalidObject {
            id: ObjectId {
                owner,
                name,
                object_type,
            },
            last_ddl: last_ddl.unwrap_or_default(),
            errors,
            first_error: line.zip(text.map(|t| t.trim().to_owned())),
            compilable,
        });
    }
    Ok(out)
}

/// The system privilege needed to compile an object of this type in another
/// schema.
fn compile_privilege(object_type: &str) -> Option<&'static str> {
    Some(match object_type {
        "PROCEDURE" | "FUNCTION" | "PACKAGE" | "PACKAGE BODY" | "LIBRARY" | "JAVA SOURCE"
        | "JAVA CLASS" => "ALTER ANY PROCEDURE",
        "TRIGGER" => "ALTER ANY TRIGGER",
        "TYPE" | "TYPE BODY" => "ALTER ANY TYPE",
        "VIEW" => "ALTER ANY TABLE",
        "MATERIALIZED VIEW" => "ALTER ANY MATERIALIZED VIEW",
        "SYNONYM" => "CREATE ANY SYNONYM",
        "DIMENSION" => "ALTER ANY DIMENSION",
        _ => return None,
    })
}

/// The statement that recompiles an object, if its type can be. PL/SQL
/// units keep their compiler settings (`REUSE SETTINGS`), debug information
/// included; a plain `COMPILE` would take the session's instead.
pub fn compile_sql(id: &ObjectId) -> Option<String> {
    let quote = |s: &str| format!("\"{}\"", s.replace('"', "\"\""));
    let name = format!("{}.{}", quote(&id.owner), quote(&id.name));
    Some(match id.object_type.as_str() {
        "PACKAGE BODY" => format!("alter package {name} compile body reuse settings"),
        "TYPE BODY" => format!("alter type {name} compile body reuse settings"),
        "SYNONYM" if id.owner == "PUBLIC" => {
            format!("alter public synonym {} compile", quote(&id.name))
        }
        "JAVA CLASS" => format!("alter java class {name} resolve"),
        t @ ("PACKAGE" | "PROCEDURE" | "FUNCTION" | "TRIGGER" | "TYPE") => {
            format!("alter {} {name} compile reuse settings", t.to_lowercase())
        }
        t
        @ ("VIEW" | "MATERIALIZED VIEW" | "SYNONYM" | "JAVA SOURCE" | "DIMENSION" | "LIBRARY") => {
            format!("alter {} {name} compile", t.to_lowercase())
        }
        _ => return None,
    })
}

/// Where a recompile is, for showing progress.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Progress {
    pub total: usize,
    pub done: usize,
    /// Being compiled right now.
    pub running: Vec<ObjectId>,
    /// The current wave (1-based) and how many there are.
    pub wave: usize,
    pub waves: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecompileSummary {
    /// Objects a compile statement ran for (with or without errors).
    pub compiled: usize,
    /// Objects that couldn't be compiled at all (no privilege, lock
    /// timeout, unsupported type, …) and why.
    pub failed: Vec<(ObjectId, String)>,
    pub cancelled: bool,
    pub elapsed: Duration,
}

/// Recompiles `objects` in dependency order on `conn` and `parallelism - 1`
/// more sessions opened with `params`. `cancel` stops starting new compiles.
pub fn recompile(
    conn: &Connection,
    params: &ConnectParams,
    objects: &[ObjectId],
    parallelism: usize,
    progress: &Mutex<Progress>,
    cancel: &AtomicBool,
) -> Result<RecompileSummary, DbError> {
    let started = Instant::now();
    let waves = waves(objects, &dependencies(conn, objects)?);
    *progress.lock().expect("progress") = Progress {
        total: objects.len(),
        waves: waves.len(),
        ..Progress::default()
    };
    let widest = waves.iter().map(Vec::len).max().unwrap_or(0);
    let mut extra = Vec::new();
    for _ in 1..parallelism.clamp(1, widest.max(1)) {
        extra.push(crate::session::open_connection(params)?);
    }
    let mut sessions: Vec<&Connection> = vec![conn];
    sessions.extend(extra.iter());

    let summary = Mutex::new(RecompileSummary::default());
    for (n, wave) in waves.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        progress.lock().expect("progress").wave = n + 1;
        let queue = Mutex::new(wave.iter().collect::<VecDeque<_>>());
        std::thread::scope(|scope| {
            for &session in sessions.iter().take(wave.len()) {
                scope.spawn(|| {
                    while !cancel.load(Ordering::Relaxed) {
                        let Some(id) = queue.lock().expect("queue").pop_front() else {
                            break;
                        };
                        progress.lock().expect("progress").running.push(id.clone());
                        let result = compile(session, id);
                        let mut p = progress.lock().expect("progress");
                        p.running.retain(|r| r != id);
                        p.done += 1;
                        drop(p);
                        let mut s = summary.lock().expect("summary");
                        match result {
                            Ok(()) => s.compiled += 1,
                            Err(e) => s.failed.push((id.clone(), e)),
                        }
                    }
                });
            }
        });
    }
    let mut summary = summary.into_inner().expect("summary");
    summary.cancelled = cancel.load(Ordering::Relaxed);
    if !summary.cancelled {
        // One more serial pass for what's still invalid (cycles, objects that
        // a later compile invalidated again).
        let failed: HashSet<&ObjectId> = summary.failed.iter().map(|(id, _)| id).collect();
        let retry: Vec<ObjectId> = still_invalid(conn, objects)?
            .into_iter()
            .filter(|id| !failed.contains(id))
            .collect();
        for id in retry {
            if cancel.load(Ordering::Relaxed) {
                summary.cancelled = true;
                break;
            }
            let _ = compile(conn, &id);
        }
    }
    summary.elapsed = started.elapsed();
    Ok(summary)
}

/// Recompiles every invalid object of `owner` with `UTL_RECOMP` (as
/// `utlrp.sql` does): needs SYSDBA and scheduler job processes.
pub fn recompile_with_utl_recomp(
    conn: &Connection,
    owner: &str,
    threads: usize,
) -> Result<(), DbError> {
    conn.execute(
        "begin utl_recomp.recomp_parallel(:1, :2); end;",
        &[&(threads.max(1) as i32), &owner],
    )?;
    Ok(())
}

/// Runs the compile statement. Compiling with errors (ORA-24344) still
/// compiled the object: its errors are in `ALL_ERRORS`.
fn compile(conn: &Connection, id: &ObjectId) -> Result<(), String> {
    let sql = compile_sql(id).ok_or_else(|| "can't be recompiled".to_owned())?;
    match conn.execute(&sql, &[]) {
        Ok(_) => Ok(()),
        Err(e) => {
            let e = DbError::from(e);
            if e.code == Some(24344) {
                Ok(())
            } else {
                Err(e.message)
            }
        }
    }
}

/// The objects of `objects` that are invalid now.
fn still_invalid(conn: &Connection, objects: &[ObjectId]) -> Result<Vec<ObjectId>, DbError> {
    let owners: BTreeSet<&str> = objects.iter().map(|o| o.owner.as_str()).collect();
    let wanted: HashSet<&ObjectId> = objects.iter().collect();
    let mut out = Vec::new();
    for owner in owners {
        let rows = conn.query_as::<(String, String, String)>(
            "select owner, object_name, object_type from all_objects \
             where owner = :1 and status = 'INVALID'",
            &[&owner],
        )?;
        for row in rows {
            let (owner, name, object_type) = row?;
            let id = ObjectId {
                owner,
                name,
                object_type,
            };
            if wanted.contains(&id) {
                out.push(id);
            }
        }
    }
    Ok(out)
}

/// Dependencies between the given objects: object → what it depends on.
fn dependencies(
    conn: &Connection,
    objects: &[ObjectId],
) -> Result<BTreeMap<ObjectId, Vec<ObjectId>>, DbError> {
    let set: HashSet<&ObjectId> = objects.iter().collect();
    let owners: BTreeSet<&str> = objects.iter().map(|o| o.owner.as_str()).collect();
    let mut out: BTreeMap<ObjectId, Vec<ObjectId>> = BTreeMap::new();
    for owner in owners {
        let rows = conn.query_as::<(String, String, String, String, String, String)>(
            "select owner, name, type, referenced_owner, referenced_name, referenced_type \
             from all_dependencies where owner = :1",
            &[&owner],
        )?;
        for row in rows {
            let (o, n, t, ro, rn, rt) = row?;
            let from = ObjectId::new(&o, &n, &t);
            let to = ObjectId::new(&ro, &rn, &rt);
            if from != to && set.contains(&from) && set.contains(&to) {
                out.entry(from).or_default().push(to);
            }
        }
    }
    Ok(out)
}

/// Splits `objects` into waves: each object comes after everything it
/// depends on. Objects in a cycle go into a last wave.
pub fn waves(
    objects: &[ObjectId],
    depends_on: &BTreeMap<ObjectId, Vec<ObjectId>>,
) -> Vec<Vec<ObjectId>> {
    let mut placed: HashSet<&ObjectId> = HashSet::new();
    let mut left: Vec<&ObjectId> = objects.iter().collect();
    let mut out = Vec::new();
    while !left.is_empty() {
        let (ready, waiting): (Vec<&ObjectId>, Vec<&ObjectId>) = left.into_iter().partition(|&o| {
            depends_on
                .get(o)
                .is_none_or(|deps| deps.iter().all(|d| placed.contains(d) || d == o))
        });
        if ready.is_empty() {
            // A cycle: compile the rest together, the final pass sorts it out.
            out.push(waiting.into_iter().cloned().collect());
            break;
        }
        placed.extend(ready.iter().copied());
        out.push(ready.into_iter().cloned().collect());
        left = waiting;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(name: &str, t: &str) -> ObjectId {
        ObjectId::new("HR", name, t)
    }

    #[test]
    fn waves_follow_dependencies() {
        let (v1, v2, p, b) = (
            id("V1", "VIEW"),
            id("V2", "VIEW"),
            id("P", "PACKAGE"),
            id("P", "PACKAGE BODY"),
        );
        let deps = BTreeMap::from([
            (v2.clone(), vec![v1.clone()]),
            (b.clone(), vec![p.clone(), v2.clone()]),
        ]);
        let objects = [b.clone(), v2.clone(), p.clone(), v1.clone()];
        assert_eq!(
            waves(&objects, &deps),
            [
                vec![p.clone(), v1.clone()],
                vec![v2.clone()],
                vec![b.clone()]
            ]
        );
        // A cycle ends up in a last wave.
        let deps = BTreeMap::from([
            (v1.clone(), vec![v2.clone()]),
            (v2.clone(), vec![v1.clone()]),
        ]);
        assert_eq!(
            waves(&[p.clone(), v1.clone(), v2.clone()], &deps),
            [vec![p], vec![v1, v2]]
        );
    }

    #[test]
    fn compile_statements() {
        assert_eq!(
            compile_sql(&id("P", "PACKAGE BODY")).unwrap(),
            "alter package \"HR\".\"P\" compile body reuse settings"
        );
        assert_eq!(
            compile_sql(&id("P", "PACKAGE")).unwrap(),
            "alter package \"HR\".\"P\" compile reuse settings"
        );
        assert_eq!(
            compile_sql(&id("V", "MATERIALIZED VIEW")).unwrap(),
            "alter materialized view \"HR\".\"V\" compile"
        );
        assert_eq!(
            compile_sql(&ObjectId::new("PUBLIC", "S", "SYNONYM")).unwrap(),
            "alter public synonym \"S\" compile"
        );
        assert!(compile_sql(&id("X", "TABLE")).is_none());
    }
}
