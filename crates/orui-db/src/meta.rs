//! Data dictionary queries: schemas, objects, columns, source, DDL, errors.
//!
//! Everything here reads `ALL_*` views so it works for any user, and only
//! returns what the user is allowed to see.

use oracle::Connection;

use crate::types::{Cell, Column, DbError};

/// Object types shown in the object browser.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum ObjectType {
    Table,
    View,
    MaterializedView,
    Sequence,
    Synonym,
    Package,
    PackageBody,
    Procedure,
    Function,
    Type,
    TypeBody,
    Trigger,
    Index,
    DatabaseLink,
    JavaSource,
    Job,
    Library,
    Directory,
}

impl ObjectType {
    /// Types listed as folders in the object browser, in display order.
    pub const BROWSER: &[ObjectType] = &[
        ObjectType::Table,
        ObjectType::View,
        ObjectType::MaterializedView,
        ObjectType::Package,
        ObjectType::Procedure,
        ObjectType::Function,
        ObjectType::Type,
        ObjectType::Trigger,
        ObjectType::Sequence,
        ObjectType::Synonym,
        ObjectType::Index,
        ObjectType::DatabaseLink,
        ObjectType::JavaSource,
        ObjectType::Job,
        ObjectType::Library,
    ];

    /// Name as used in `ALL_OBJECTS.OBJECT_TYPE` and `DBMS_METADATA`.
    pub fn dictionary_name(self) -> &'static str {
        match self {
            ObjectType::Table => "TABLE",
            ObjectType::View => "VIEW",
            ObjectType::MaterializedView => "MATERIALIZED VIEW",
            ObjectType::Sequence => "SEQUENCE",
            ObjectType::Synonym => "SYNONYM",
            ObjectType::Package => "PACKAGE",
            ObjectType::PackageBody => "PACKAGE BODY",
            ObjectType::Procedure => "PROCEDURE",
            ObjectType::Function => "FUNCTION",
            ObjectType::Type => "TYPE",
            ObjectType::TypeBody => "TYPE BODY",
            ObjectType::Trigger => "TRIGGER",
            ObjectType::Index => "INDEX",
            ObjectType::DatabaseLink => "DATABASE LINK",
            ObjectType::JavaSource => "JAVA SOURCE",
            ObjectType::Job => "JOB",
            ObjectType::Library => "LIBRARY",
            ObjectType::Directory => "DIRECTORY",
        }
    }

    pub fn from_dictionary(name: &str) -> Option<Self> {
        ObjectType::ALL
            .iter()
            .copied()
            .find(|t| t.dictionary_name() == name)
    }

    const ALL: &[ObjectType] = &[
        ObjectType::Table,
        ObjectType::View,
        ObjectType::MaterializedView,
        ObjectType::Sequence,
        ObjectType::Synonym,
        ObjectType::Package,
        ObjectType::PackageBody,
        ObjectType::Procedure,
        ObjectType::Function,
        ObjectType::Type,
        ObjectType::TypeBody,
        ObjectType::Trigger,
        ObjectType::Index,
        ObjectType::DatabaseLink,
        ObjectType::JavaSource,
        ObjectType::Job,
        ObjectType::Library,
        ObjectType::Directory,
    ];

    /// Folder label in the object browser.
    pub fn plural(self) -> &'static str {
        match self {
            ObjectType::Table => "Tables",
            ObjectType::View => "Views",
            ObjectType::MaterializedView => "Materialized views",
            ObjectType::Sequence => "Sequences",
            ObjectType::Synonym => "Synonyms",
            ObjectType::Package => "Packages",
            ObjectType::PackageBody => "Package bodies",
            ObjectType::Procedure => "Procedures",
            ObjectType::Function => "Functions",
            ObjectType::Type => "Types",
            ObjectType::TypeBody => "Type bodies",
            ObjectType::Trigger => "Triggers",
            ObjectType::Index => "Indexes",
            ObjectType::DatabaseLink => "Database links",
            ObjectType::JavaSource => "Java sources",
            ObjectType::Job => "Scheduler jobs",
            ObjectType::Library => "Libraries",
            ObjectType::Directory => "Directories",
        }
    }

    /// Stored PL/SQL (or Java) source that can be opened in a program window.
    pub fn has_source(self) -> bool {
        matches!(
            self,
            ObjectType::Package
                | ObjectType::PackageBody
                | ObjectType::Procedure
                | ObjectType::Function
                | ObjectType::Type
                | ObjectType::TypeBody
                | ObjectType::Trigger
                | ObjectType::JavaSource
        )
    }

    /// Objects whose data can be queried.
    pub fn has_data(self) -> bool {
        matches!(
            self,
            ObjectType::Table | ObjectType::View | ObjectType::MaterializedView
        )
    }

    /// The type of the body belonging to a spec, if any.
    pub fn body(self) -> Option<ObjectType> {
        match self {
            ObjectType::Package => Some(ObjectType::PackageBody),
            ObjectType::Type => Some(ObjectType::TypeBody),
            _ => None,
        }
    }

    /// Name used by `DBMS_METADATA.GET_DDL` (differs for a few types).
    fn metadata_name(self) -> &'static str {
        match self {
            ObjectType::Package => "PACKAGE_SPEC",
            ObjectType::PackageBody => "PACKAGE_BODY",
            ObjectType::Type => "TYPE_SPEC",
            ObjectType::TypeBody => "TYPE_BODY",
            ObjectType::MaterializedView => "MATERIALIZED_VIEW",
            ObjectType::DatabaseLink => "DB_LINK",
            ObjectType::JavaSource => "JAVA_SOURCE",
            ObjectType::Job => "PROCOBJ",
            other => other.dictionary_name(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbObject {
    pub owner: String,
    pub name: String,
    pub object_type: ObjectType,
    pub valid: bool,
    /// `LAST_DDL_TIME` as `YYYY-MM-DD HH24:MI:SS`.
    pub last_ddl: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub name: String,
    /// Full type, e.g. `VARCHAR2(30 CHAR)`, `NUMBER(10,2)`.
    pub data_type: String,
    pub nullable: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
    /// Position in the primary key, if part of it.
    pub pk_position: Option<u32>,
}

/// A procedure or function inside a package or type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub overload: Option<String>,
    pub is_function: bool,
    /// Parameter list for display, e.g. `(p_id IN NUMBER, p_name IN VARCHAR2)`.
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompileError {
    pub object_type: String,
    pub name: String,
    pub line: u32,
    pub position: u32,
    pub text: String,
    pub warning: bool,
}

#[derive(Debug, Clone)]
pub enum MetaQuery {
    Schemas,
    Objects {
        owner: String,
        object_type: ObjectType,
        filter: Option<String>,
    },
    Columns {
        owner: String,
        table: String,
    },
    Members {
        owner: String,
        package: String,
    },
    /// Indexes, constraints, triggers, grants... of a table as a generic table.
    Details {
        owner: String,
        name: String,
        detail: Detail,
    },
    Source {
        owner: String,
        name: String,
        object_type: ObjectType,
    },
    Ddl {
        owner: String,
        name: String,
        object_type: ObjectType,
    },
    Errors {
        owner: String,
        name: String,
        object_type: ObjectType,
    },
    /// Objects matching a name pattern across all visible schemas.
    Find {
        pattern: String,
        limit: usize,
    },
    /// `EXPLAIN PLAN` for a statement, returned as a table.
    ExplainPlan {
        sql: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Detail {
    Indexes,
    Constraints,
    Triggers,
    Grants,
    Uses,
    UsedBy,
}

impl Detail {
    pub const TABLE: &[Detail] = &[
        Detail::Indexes,
        Detail::Constraints,
        Detail::Triggers,
        Detail::Grants,
        Detail::UsedBy,
    ];
    pub const CODE: &[Detail] = &[Detail::Uses, Detail::UsedBy, Detail::Grants];

    pub fn label(self) -> &'static str {
        match self {
            Detail::Indexes => "Indexes",
            Detail::Constraints => "Constraints",
            Detail::Triggers => "Triggers",
            Detail::Grants => "Grants",
            Detail::Uses => "Uses",
            Detail::UsedBy => "Used by",
        }
    }
}

#[derive(Debug, Clone)]
pub enum MetaResult {
    Schemas(Vec<String>),
    Objects(Vec<DbObject>),
    Columns(Vec<TableColumn>),
    Members(Vec<Member>),
    Table {
        columns: Vec<Column>,
        rows: Vec<Vec<Cell>>,
    },
    Text(String),
    Errors(Vec<CompileError>),
}

pub(crate) fn run(conn: &Connection, query: &MetaQuery) -> Result<MetaResult, DbError> {
    Ok(match query {
        MetaQuery::Schemas => {
            let rows =
                conn.query_as::<String>("select username from all_users order by username", &[])?;
            MetaResult::Schemas(rows.collect::<Result<_, _>>()?)
        }
        MetaQuery::Objects {
            owner,
            object_type,
            filter,
        } => MetaResult::Objects(objects(conn, owner, *object_type, filter.as_deref())?),
        MetaQuery::Columns { owner, table } => MetaResult::Columns(columns(conn, owner, table)?),
        MetaQuery::Members { owner, package } => {
            MetaResult::Members(members(conn, owner, package)?)
        }
        MetaQuery::Details {
            owner,
            name,
            detail,
        } => details(conn, owner, name, *detail)?,
        MetaQuery::Source {
            owner,
            name,
            object_type,
        } => MetaResult::Text(source(conn, owner, name, *object_type)?),
        MetaQuery::Ddl {
            owner,
            name,
            object_type,
        } => MetaResult::Text(ddl(conn, owner, name, *object_type)?),
        MetaQuery::Errors {
            owner,
            name,
            object_type,
        } => MetaResult::Errors(errors(conn, owner, name, object_type.dictionary_name())?),
        MetaQuery::Find { pattern, limit } => MetaResult::Objects(find(conn, pattern, *limit)?),
        MetaQuery::ExplainPlan { sql } => explain_plan(conn, sql)?,
    })
}

const OBJECT_COLUMNS: &str =
    "owner, object_name, object_type, status, to_char(last_ddl_time, 'YYYY-MM-DD HH24:MI:SS')";

fn object_rows(rows: oracle::ResultSet<'static, oracle::Row>) -> Result<Vec<DbObject>, DbError> {
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        let (owner, name, ty, status, last_ddl): (String, String, String, String, Option<String>) =
            row.get_as()?;
        let Some(object_type) = ObjectType::from_dictionary(&ty) else {
            continue;
        };
        out.push(DbObject {
            owner,
            name,
            object_type,
            valid: status == "VALID",
            last_ddl: last_ddl.unwrap_or_default(),
        });
    }
    Ok(out)
}

fn objects(
    conn: &Connection,
    owner: &str,
    object_type: ObjectType,
    filter: Option<&str>,
) -> Result<Vec<DbObject>, DbError> {
    // Scheduler jobs aren't in ALL_OBJECTS under a type of their own name
    // in every version; the dictionary name still matches on 12c+.
    let pattern = like_pattern(filter.unwrap_or(""));
    let sql = format!(
        "select {OBJECT_COLUMNS} from all_objects \
         where owner = :1 and object_type = :2 and object_name like :3 escape '\\' \
           and object_name not like 'BIN$%' and generated = 'N' \
         order by object_name"
    );
    object_rows(conn.query(&sql, &[&owner, &object_type.dictionary_name(), &pattern])?)
}

fn find(conn: &Connection, pattern: &str, limit: usize) -> Result<Vec<DbObject>, DbError> {
    let types: Vec<String> = ObjectType::BROWSER
        .iter()
        .map(|t| format!("'{}'", t.dictionary_name()))
        .collect();
    let sql = format!(
        "select {OBJECT_COLUMNS} from all_objects \
         where object_name like :1 escape '\\' and object_type in ({}) \
         order by case when owner = sys_context('USERENV', 'CURRENT_SCHEMA') then 0 else 1 end, \
                  length(object_name), object_name, owner \
         fetch first {limit} rows only",
        types.join(", ")
    );
    object_rows(conn.query(&sql, &[&like_pattern(pattern)])?)
}

/// Turns a user filter into a LIKE pattern: case-insensitive "contains",
/// `*` and `?` as wildcards, `%`/`_` matched literally.
pub fn like_pattern(filter: &str) -> String {
    let mut out = String::from("%");
    for c in filter.trim().to_uppercase().chars() {
        match c {
            '%' | '_' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '*' => out.push('%'),
            '?' => out.push('_'),
            c => out.push(c),
        }
    }
    out.push('%');
    out
}

fn columns(conn: &Connection, owner: &str, table: &str) -> Result<Vec<TableColumn>, DbError> {
    let sql = "select c.column_name, \
                case \
                  when c.data_type in ('VARCHAR2', 'NVARCHAR2', 'CHAR', 'NCHAR', 'RAW') then \
                    c.data_type || '(' || decode(c.char_used, 'C', c.char_length || ' CHAR', c.data_length) || ')' \
                  when c.data_type = 'NUMBER' and c.data_precision is not null then \
                    'NUMBER(' || c.data_precision || nvl2(nullif(c.data_scale, 0), ',' || c.data_scale, '') || ')' \
                  when c.data_type = 'NUMBER' and c.data_scale = 0 then 'INTEGER' \
                  else c.data_type end, \
                c.nullable, c.data_default, cc.comments, \
                (select p.position from all_cons_columns p join all_constraints k \
                   on k.owner = p.owner and k.constraint_name = p.constraint_name \
                  where k.constraint_type = 'P' and k.owner = c.owner and k.table_name = c.table_name \
                    and p.column_name = c.column_name) \
               from all_tab_columns c \
               left join all_col_comments cc \
                 on cc.owner = c.owner and cc.table_name = c.table_name and cc.column_name = c.column_name \
              where c.owner = :1 and c.table_name = :2 \
              order by c.column_id";
    let mut out = Vec::new();
    for row in conn.query(sql, &[&owner, &table])? {
        let row = row?;
        let (name, data_type, nullable, default, comment, pk): (
            String,
            String,
            String,
            Option<String>,
            Option<String>,
            Option<u32>,
        ) = row.get_as()?;
        out.push(TableColumn {
            name,
            data_type,
            nullable: nullable == "Y",
            default: default
                .map(|d| d.trim().to_owned())
                .filter(|d| !d.is_empty()),
            comment,
            pk_position: pk,
        });
    }
    Ok(out)
}

fn members(conn: &Connection, owner: &str, package: &str) -> Result<Vec<Member>, DbError> {
    let sql = "select p.procedure_name, p.overload, \
                (select count(*) from all_arguments a \
                  where a.owner = p.owner and a.package_name = p.object_name \
                    and a.object_name = p.procedure_name \
                    and nvl(a.overload, '-') = nvl(p.overload, '-') \
                    and a.position = 0 and a.data_level = 0), \
                (select listagg(lower(a.argument_name) || ' ' || a.in_out || ' ' || \
                        nvl(a.pls_type, nvl(a.type_name, a.data_type)), ', ') \
                        within group (order by a.position) \
                   from all_arguments a \
                  where a.owner = p.owner and a.package_name = p.object_name \
                    and a.object_name = p.procedure_name \
                    and nvl(a.overload, '-') = nvl(p.overload, '-') \
                    and a.position > 0 and a.data_level = 0) \
               from all_procedures p \
              where p.owner = :1 and p.object_name = :2 and p.procedure_name is not null \
              order by p.subprogram_id";
    let mut out = Vec::new();
    for row in conn.query(sql, &[&owner, &package])? {
        let row = row?;
        let (name, overload, returns, args): (String, Option<String>, u32, Option<String>) =
            row.get_as()?;
        out.push(Member {
            name,
            overload,
            is_function: returns > 0,
            signature: args.map(|a| format!("({a})")).unwrap_or_default(),
        });
    }
    Ok(out)
}

fn details(
    conn: &Connection,
    owner: &str,
    name: &str,
    detail: Detail,
) -> Result<MetaResult, DbError> {
    let sql = match detail {
        Detail::Indexes => {
            "select i.index_name, i.index_type, i.uniqueness, \
                    listagg(c.column_name, ', ') within group (order by c.column_position) as columns, \
                    i.status, i.tablespace_name \
               from all_indexes i \
               join all_ind_columns c on c.index_owner = i.owner and c.index_name = i.index_name \
              where i.table_owner = :1 and i.table_name = :2 \
              group by i.index_name, i.index_type, i.uniqueness, i.status, i.tablespace_name \
              order by i.index_name"
        }
        Detail::Constraints => {
            "select k.constraint_name, \
                    decode(k.constraint_type, 'P', 'Primary key', 'U', 'Unique', 'R', 'Foreign key', \
                           'C', 'Check', k.constraint_type) as type, \
                    (select listagg(c.column_name, ', ') within group (order by c.position) \
                       from all_cons_columns c \
                      where c.owner = k.owner and c.constraint_name = k.constraint_name) as columns, \
                    (select r.table_name from all_constraints r \
                      where r.owner = k.r_owner and r.constraint_name = k.r_constraint_name) as references, \
                    k.search_condition_vc as condition, k.status, k.delete_rule \
               from all_constraints k \
              where k.owner = :1 and k.table_name = :2 \
                and not (k.constraint_type = 'C' and k.search_condition_vc like '% IS NOT NULL') \
              order by decode(k.constraint_type, 'P', 1, 'U', 2, 'R', 3, 4), k.constraint_name"
        }
        Detail::Triggers => {
            "select trigger_name, trigger_type, triggering_event, status \
               from all_triggers where table_owner = :1 and table_name = :2 order by trigger_name"
        }
        Detail::Grants => {
            "select grantee, privilege, grantable, grantor \
               from all_tab_privs where table_schema = :1 and table_name = :2 \
              order by grantee, privilege"
        }
        Detail::Uses => {
            "select distinct referenced_owner as owner, referenced_name as name, referenced_type as type \
               from all_dependencies \
              where owner = :1 and name = :2 and referenced_owner not in ('SYS', 'PUBLIC') \
              order by 1, 3, 2"
        }
        Detail::UsedBy => {
            "select distinct owner, name, type from all_dependencies \
              where referenced_owner = :1 and referenced_name = :2 \
              order by 1, 3, 2"
        }
    };
    table_result(conn.query(sql, &[&owner, &name])?, &[])
}

fn explain_plan(conn: &Connection, sql: &str) -> Result<MetaResult, DbError> {
    let statement_id = format!("ORUI{}", std::process::id());
    conn.execute(
        "delete from plan_table where statement_id = :1",
        &[&statement_id],
    )?;
    conn.execute(
        &format!("explain plan set statement_id = '{statement_id}' for {sql}"),
        &[],
    )?;
    let rows = conn.query(
        "select id, parent_id, depth, operation || nvl2(options, ' ' || options, '') as operation, \
                nvl2(object_name, nvl2(object_owner, object_owner || '.', '') || object_name, '') as object, \
                cost, cardinality as \"ROWS\", bytes, time, \
                access_predicates as access_pred, filter_predicates as filter_pred \
           from plan_table where statement_id = :1 order by id",
        &[&statement_id],
    )?;
    let result = table_result(
        rows,
        &["ID", "PARENT_ID", "DEPTH", "COST", "ROWS", "BYTES", "TIME"],
    );
    conn.execute(
        "delete from plan_table where statement_id = :1",
        &[&statement_id],
    )?;
    conn.rollback()?;
    result
}

/// Collects a result set into a generic table; `numeric` names right-aligned columns.
fn table_result(
    rows: oracle::ResultSet<'static, oracle::Row>,
    numeric: &[&str],
) -> Result<MetaResult, DbError> {
    let columns = rows
        .column_info()
        .iter()
        .map(|c| Column {
            name: c.name().to_owned(),
            type_name: c.oracle_type().to_string(),
            numeric: numeric.contains(&c.name()),
        })
        .collect();
    let mut out = Vec::new();
    for row in rows {
        let row = row?;
        out.push(
            row.sql_values()
                .iter()
                .map(|v| {
                    if v.is_null().unwrap_or(true) {
                        None
                    } else {
                        v.get::<String>().ok()
                    }
                })
                .collect(),
        );
    }
    Ok(MetaResult::Table { columns, rows: out })
}

fn source(
    conn: &Connection,
    owner: &str,
    name: &str,
    object_type: ObjectType,
) -> Result<String, DbError> {
    let rows = conn.query_as::<Option<String>>(
        "select text from all_source where owner = :1 and name = :2 and type = :3 order by line",
        &[&owner, &name, &object_type.dictionary_name()],
    )?;
    let mut text = String::new();
    for line in rows {
        text.push_str(&line?.unwrap_or_default());
    }
    if text.is_empty() {
        return Err(DbError::new(format!(
            "No source found for {} {owner}.{name}",
            object_type.dictionary_name().to_lowercase()
        )));
    }
    // ALL_SOURCE starts with e.g. `PACKAGE BODY foo AS`; make it runnable.
    Ok(format!("CREATE OR REPLACE {}", text.trim_end()))
}

fn ddl(
    conn: &Connection,
    owner: &str,
    name: &str,
    object_type: ObjectType,
) -> Result<String, DbError> {
    conn.execute(
        "begin \
           dbms_metadata.set_transform_param(dbms_metadata.session_transform, 'SQLTERMINATOR', true); \
           dbms_metadata.set_transform_param(dbms_metadata.session_transform, 'PRETTY', true); \
           dbms_metadata.set_transform_param(dbms_metadata.session_transform, 'SEGMENT_ATTRIBUTES', false); \
           dbms_metadata.set_transform_param(dbms_metadata.session_transform, 'STORAGE', false); \
         end;",
        &[],
    )?;
    let ddl: String = conn.query_row_as(
        "select dbms_metadata.get_ddl(:1, :2, :3) from dual",
        &[&object_type.metadata_name(), &name, &owner],
    )?;
    let mut ddl = ddl.trim().to_owned();
    if object_type == ObjectType::Table {
        // Append comments and indexes the way PL/SQL Developer's "View" does.
        for (kind, sql) in [
            (
                "COMMENT",
                "select dbms_metadata.get_dependent_ddl('COMMENT', :1, :2) from dual",
            ),
            (
                "INDEX",
                "select dbms_metadata.get_dependent_ddl('INDEX', :1, :2) from dual",
            ),
        ] {
            // ORA-31608: no dependent objects of this kind.
            match conn.query_row_as::<String>(sql, &[&name, &owner]) {
                Ok(dep) => {
                    ddl.push_str("\n\n");
                    ddl.push_str(dep.trim());
                }
                Err(e) if e.db_error().is_some_and(|d| d.code() == 31608) => {}
                Err(e) => tracing::warn!("dependent {kind} ddl: {e}"),
            }
        }
    }
    Ok(ddl)
}

pub(crate) fn errors(
    conn: &Connection,
    owner: &str,
    name: &str,
    object_type: &str,
) -> Result<Vec<CompileError>, DbError> {
    let rows = conn.query(
        "select type, name, line, position, text, attribute from all_errors \
          where owner = :1 and name = :2 and (:3 is null or type = :3) \
          order by type, sequence",
        &[&owner, &name, &object_type],
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (object_type, name, line, position, text, attribute): (
            String,
            String,
            u32,
            u32,
            String,
            String,
        ) = row?.get_as()?;
        out.push(CompileError {
            object_type,
            name,
            line,
            position,
            text: text.trim_end().to_owned(),
            warning: attribute == "WARNING",
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn like_patterns() {
        assert_eq!(like_pattern(""), "%%");
        assert_eq!(like_pattern("emp"), "%EMP%");
        assert_eq!(like_pattern("emp*_hist"), "%EMP%\\_HIST%");
        assert_eq!(like_pattern("a?c"), "%A_C%");
    }

    #[test]
    fn dictionary_roundtrip() {
        for t in ObjectType::ALL {
            assert_eq!(ObjectType::from_dictionary(t.dictionary_name()), Some(*t));
        }
    }
}
