//! The target side and setup: running code under the debugger, compiling
//! units with debug info, privilege checks and the admin setup script.

use pythia_db::oracle::Connection;
use pythia_db::{DbError, TaskFn};

use crate::types::{Unit, UnitType};

/// Connects the session to the debugger, runs `block`, then disconnects.
///
/// Blocks for the whole debug session, so run it on a dedicated session
/// (e.g. as a `DbRequest::Task`). The block's own error is returned; a
/// terminated session fails with ORA-30687.
pub fn run_under_debugger(
    conn: &Connection,
    host: &str,
    port: u16,
    block: &str,
) -> Result<(), DbError> {
    conn.execute(
        "begin dbms_debug_jdwp.connect_tcp(:1, :2); end;",
        &[&host, &i32::from(port)],
    )?;
    let result = conn.execute(block, &[]).map(drop).map_err(DbError::from);
    // The session may be gone if the debugger terminated it.
    let _ = conn.execute("begin dbms_debug_jdwp.disconnect; end;", &[]);
    result
}

/// How a debugged block ended: what it printed with DBMS_OUTPUT and its own
/// error, if any.
#[derive(Debug)]
pub struct DebugOutcome {
    pub output: Vec<String>,
    pub error: Option<DbError>,
}

/// [`run_under_debugger`] as a task for `pythia_db::DbRequest::Task`. The task
/// result is a [`DebugOutcome`] (the block's error is part of it, so its
/// output isn't lost).
pub fn debug_task(host: String, port: u16, block: String) -> TaskFn {
    Box::new(move |conn: &Connection| {
        let error = run_under_debugger(conn, &host, port, &block).err();
        let output = read_output(conn).unwrap_or_default();
        Ok(Box::new(DebugOutcome { output, error }) as Box<dyn std::any::Any + Send>)
    })
}

/// Drains the session's DBMS_OUTPUT buffer.
pub fn read_output(conn: &Connection) -> Result<Vec<String>, DbError> {
    use pythia_db::oracle::sql_type::OracleType;
    let mut stmt = conn
        .statement("begin dbms_output.get_line(:1, :2); end;")
        .build()?;
    let mut lines = Vec::new();
    loop {
        stmt.execute(&[&OracleType::Varchar2(32767), &OracleType::Number(0, 0)])?;
        let status: i32 = stmt.bind_value(2)?;
        if status != 0 {
            return Ok(lines);
        }
        lines.push(stmt.bind_value::<_, Option<String>>(1)?.unwrap_or_default());
    }
}

/// SQL to recompile a unit with debug information.
pub fn compile_debug_sql(unit: &Unit) -> Option<String> {
    let name = format!("\"{}\".\"{}\"", unit.owner, unit.name);
    Some(match unit.unit_type {
        UnitType::Procedure => format!("alter procedure {name} compile debug"),
        UnitType::Function => format!("alter function {name} compile debug"),
        UnitType::Package => format!("alter package {name} compile debug package"),
        UnitType::PackageBody => format!("alter package {name} compile debug body"),
        UnitType::Trigger => format!("alter trigger {name} compile debug"),
        UnitType::Type => format!("alter type {name} compile debug specification"),
        UnitType::TypeBody => format!("alter type {name} compile debug body"),
        UnitType::Block => return None,
    })
}

/// Recompiles `unit` with debug information.
pub fn compile_debug(conn: &Connection, unit: &Unit) -> Result<(), DbError> {
    if let Some(sql) = compile_debug_sql(unit) {
        conn.execute(&sql, &[])?;
    }
    Ok(())
}

/// Whether a stored unit was compiled with debug information.
pub fn has_debug_info(conn: &Connection, unit: &Unit) -> Result<bool, DbError> {
    let flag: Option<String> = conn.query_row_as(
        "select max(plsql_debug) from all_plsql_object_settings where owner = :1 and name = :2 and type = :3",
        &[&unit.owner, &unit.name, &unit.unit_type.dictionary_name()],
    )?;
    Ok(flag.as_deref() == Some("TRUE"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingPrivilege {
    /// `DEBUG CONNECT SESSION` (required).
    DebugConnectSession,
    /// `DEBUG ANY PROCEDURE` (needed to debug other schemas' code).
    DebugAnyProcedure,
    /// No `jdwp` host ACE for the user (connect would fail with ORA-24247).
    JdwpAcl,
}

impl MissingPrivilege {
    pub fn describe(&self) -> &'static str {
        match self {
            MissingPrivilege::DebugConnectSession => "DEBUG CONNECT SESSION is required to debug",
            MissingPrivilege::DebugAnyProcedure => {
                "DEBUG ANY PROCEDURE is needed to debug other schemas' code"
            }
            MissingPrivilege::JdwpAcl => {
                "a network ACL with the JDWP privilege is needed for the database to connect to Pythia"
            }
        }
    }
}

/// Checks what the current user lacks for debugging.
pub fn check(conn: &Connection) -> Result<Vec<MissingPrivilege>, DbError> {
    let privs = conn.query_as::<String>(
        "select privilege from session_privs where privilege in ('DEBUG CONNECT SESSION', 'DEBUG ANY PROCEDURE')",
        &[],
    )?;
    let privs: Vec<String> = privs.collect::<Result<_, _>>()?;
    let mut missing = Vec::new();
    if !privs.iter().any(|p| p == "DEBUG CONNECT SESSION") {
        missing.push(MissingPrivilege::DebugConnectSession);
    }
    if !privs.iter().any(|p| p == "DEBUG ANY PROCEDURE") {
        missing.push(MissingPrivilege::DebugAnyProcedure);
    }
    let aces: i64 = conn
        .query_row_as(
            "select count(*) from user_host_aces where privilege = 'JDWP' and grant_type = 'GRANT'",
            &[],
        )
        .unwrap_or(0);
    if aces == 0 {
        missing.push(MissingPrivilege::JdwpAcl);
    }
    Ok(missing)
}

/// The SQL an administrator runs once so `user` can debug, with the
/// database connecting back to `host` (the machine running Pythia, as seen
/// from the database server). Each statement is executable as is.
pub fn setup_statements(user: &str, host: &str) -> Vec<String> {
    let user = user.to_uppercase();
    let host = host.replace('\'', "''");
    vec![
        format!("grant debug connect session to {user}"),
        format!("grant debug any procedure to {user}"),
        format!(
            "begin\n  \
               dbms_network_acl_admin.append_host_ace(\n    \
                 host       => '{host}',\n    \
                 lower_port => 1024,\n    \
                 upper_port => 65535,\n    \
                 ace        => xs$ace_type(privilege_list => xs$name_list('jdwp'),\n                               \
                                           principal_name => '{user}',\n                               \
                                           principal_type => xs_acl.ptype_db));\n\
             end;"
        ),
    ]
}

/// [`setup_statements`] as a SQL*Plus script for a DBA to run.
pub fn setup_script(user: &str, host: &str) -> String {
    let mut out = format!(
        "-- Allow {} to debug PL/SQL with Pythia (run as a DBA)\n",
        user.to_uppercase()
    );
    for stmt in setup_statements(user, host) {
        out.push_str(&stmt);
        out.push_str(if stmt.starts_with("begin") {
            "\n/\n"
        } else {
            ";\n"
        });
    }
    out
}

/// A host name the database can use to reach this machine, for
/// `DBMS_DEBUG_JDWP.CONNECT_TCP`. Prefers `PYTHIA_DEBUG_HOST`, then the
/// machine's host name. Databases in a local container usually need
/// `host.containers.internal` / `host.docker.internal` instead.
pub fn default_host() -> String {
    if let Ok(host) = std::env::var("PYTHIA_DEBUG_HOST")
        && !host.is_empty()
    {
        return host;
    }
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "localhost".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_sql() {
        let unit = Unit::new("HR", "PKG", UnitType::PackageBody);
        assert_eq!(
            compile_debug_sql(&unit).unwrap(),
            "alter package \"HR\".\"PKG\" compile debug body"
        );
        assert!(compile_debug_sql(&Unit::new("HR", "x", UnitType::Block)).is_none());
    }

    #[test]
    fn setup() {
        let sql = setup_script("pythia", "dev-box");
        assert!(sql.contains("grant debug connect session to PYTHIA"));
        assert!(sql.contains("host       => 'dev-box'"));
    }

    #[test]
    fn class_signatures() {
        let unit = Unit::new("PYTHIA", "DBG_DEMO", UnitType::Procedure);
        assert_eq!(
            unit.class_signature(),
            "L$Oracle/Procedure/PYTHIA/DBG_DEMO;"
        );
        assert_eq!(
            Unit::from_class_signature(&unit.class_signature()),
            Some(unit)
        );
        let block =
            Unit::from_class_signature("L$Oracle/Block/PYTHIA/BexqMksmIPthdGZpJ4tGJwg;").unwrap();
        assert_eq!(block.unit_type, UnitType::Block);
        assert_eq!(Unit::from_class_signature("Ljava/lang/String;"), None);
    }
}
