//! Sync scripts: ordered statements with SQL*Plus terminators.
//!
//! Scripts are safe by default: destructive statements (dropping objects or
//! columns) are emitted commented out unless explicitly enabled, and every
//! risky step carries a warning comment.

/// Execution phases, in script order. Dependencies flow downwards: types
/// before the tables using them, tables before views and code, primary keys
/// before the foreign keys referencing them, triggers last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    DropForeignKeys,
    DropConstraints,
    DropIndexes,
    Sequences,
    Types,
    Tables,
    Constraints,
    Indexes,
    ForeignKeys,
    Comments,
    Synonyms,
    Views,
    PackageSpecs,
    Procedures,
    PackageBodies,
    TypeBodies,
    Triggers,
    DropObjects,
    Data,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::DropForeignKeys => "Drop foreign keys",
            Phase::DropConstraints => "Drop constraints",
            Phase::DropIndexes => "Drop indexes",
            Phase::Sequences => "Sequences",
            Phase::Types => "Types",
            Phase::Tables => "Tables",
            Phase::Constraints => "Constraints",
            Phase::Indexes => "Indexes",
            Phase::ForeignKeys => "Foreign keys",
            Phase::Comments => "Comments",
            Phase::Synonyms => "Synonyms",
            Phase::Views => "Views",
            Phase::PackageSpecs => "Package specifications",
            Phase::Procedures => "Procedures and functions",
            Phase::PackageBodies => "Package bodies",
            Phase::TypeBodies => "Type bodies",
            Phase::Triggers => "Triggers",
            Phase::DropObjects => "Drop objects",
            Phase::Data => "Data",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncStatement {
    pub phase: Phase,
    pub sql: String,
    /// PL/SQL units are terminated with `/` instead of `;`.
    pub plsql: bool,
    /// Shown as a `-- WARNING:` comment above the statement.
    pub warning: Option<String>,
    /// Disabled statements are written commented out.
    pub enabled: bool,
    /// The object this statement belongs to (for filtering by selection).
    pub object: String,
}

impl SyncStatement {
    pub fn new(phase: Phase, object: impl Into<String>, sql: impl Into<String>) -> Self {
        Self {
            phase,
            sql: sql.into(),
            plsql: false,
            warning: None,
            enabled: true,
            object: object.into(),
        }
    }
}

/// Header options of a rendered script.
#[derive(Debug, Clone, Default)]
pub struct ScriptHeader {
    pub title: String,
    /// `ALTER SESSION SET CURRENT_SCHEMA` target, so unqualified names land
    /// in the right schema.
    pub schema: Option<String>,
    /// Recompile invalid objects at the end.
    pub recompile: bool,
}

/// Orders statements by phase (stable within a phase) and renders them.
pub fn render(header: &ScriptHeader, statements: &[SyncStatement]) -> String {
    let mut sorted: Vec<&SyncStatement> = statements.iter().collect();
    sorted.sort_by_key(|s| s.phase);
    let mut out = String::new();
    if !header.title.is_empty() {
        for line in header.title.lines() {
            out.push_str("-- ");
            out.push_str(line);
            out.push('\n');
        }
        out.push('\n');
    }
    if sorted.is_empty() {
        out.push_str("-- Nothing to synchronize.\n");
        return out;
    }
    let disabled = sorted.iter().filter(|s| !s.enabled).count();
    if disabled > 0 {
        out.push_str(&format!(
            "-- {disabled} destructive statement(s) are commented out. Review and enable them deliberately.\n\n"
        ));
    }
    if let Some(schema) = &header.schema {
        out.push_str(&format!(
            "alter session set current_schema = {};\n\n",
            crate::table::ident(schema)
        ));
    }
    let mut phase = None;
    for s in sorted {
        if phase != Some(s.phase) {
            if phase.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("-- {}\n", s.phase.label()));
            phase = Some(s.phase);
        }
        if let Some(w) = &s.warning {
            out.push_str(&format!("-- WARNING: {w}\n"));
        }
        let terminated = if s.plsql {
            format!("{}\n/", s.sql.trim_end())
        } else {
            format!("{};", s.sql.trim_end())
        };
        if s.enabled {
            out.push_str(&terminated);
            out.push('\n');
        } else {
            for line in terminated.lines() {
                out.push_str("-- ");
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    if header.recompile {
        out.push_str(
            "\n-- Recompile invalid objects\n\
             begin\n  dbms_utility.compile_schema(schema => sys_context('USERENV', 'CURRENT_SCHEMA'), compile_all => false);\nend;\n/\n",
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_and_terminates() {
        let mut fk = SyncStatement::new(
            Phase::ForeignKeys,
            "EMP",
            "alter table emp add foreign key (d) references dept (id)",
        );
        fk.warning = Some("check data".into());
        let mut body = SyncStatement::new(
            Phase::PackageBodies,
            "PKG",
            "create or replace package body pkg as end;",
        );
        body.plsql = true;
        let mut drop = SyncStatement::new(Phase::DropObjects, "OLD", "drop table old purge");
        drop.enabled = false;
        let statements = vec![
            body,
            drop,
            fk,
            SyncStatement::new(Phase::Tables, "EMP", "alter table emp add (x number)"),
        ];
        let header = ScriptHeader {
            title: "Sync A → B".into(),
            schema: Some("B".into()),
            recompile: false,
        };
        assert_eq!(
            render(&header, &statements),
            "-- Sync A → B\n\n\
             -- 1 destructive statement(s) are commented out. Review and enable them deliberately.\n\n\
             alter session set current_schema = b;\n\n\
             -- Tables\nalter table emp add (x number);\n\n\
             -- Foreign keys\n-- WARNING: check data\nalter table emp add foreign key (d) references dept (id);\n\n\
             -- Package bodies\ncreate or replace package body pkg as end;\n/\n\n\
             -- Drop objects\n-- drop table old purge;\n"
        );
    }

    #[test]
    fn empty_script() {
        assert!(render(&ScriptHeader::default(), &[]).contains("Nothing to synchronize"));
    }
}
