//! Dictionary lookups for code completion and navigation.
//!
//! `pythia_lsp_core` reports what it is missing as [`CatalogRequest`]s; each is
//! answered by a task on the connection's main session and stored in the
//! connection's [`CatalogCache`]. Names follow Oracle's storage rules:
//! the engine already normalizes unquoted identifiers to uppercase.

use pythia_db::oracle::Connection;
use pythia_db::{DbError, DbKind, DbRequest, Dialect, TaskFn};
use pythia_lsp_core::{
    CatalogCache, CatalogColumn, CatalogMember, CatalogObject, CatalogParam, CatalogRequest,
    MemberKind, ParamMode,
};

/// Object types offered by completion and resolution.
const TYPES: &str = "'TABLE', 'VIEW', 'MATERIALIZED VIEW', 'SYNONYM', 'PACKAGE', 'PROCEDURE', 'FUNCTION', \
                     'SEQUENCE', 'TYPE'";

/// Result of one request, applied to the cache on the UI thread.
pub enum Answer {
    Schemas(Vec<String>),
    Objects(Option<String>, Vec<CatalogObject>),
    Resolved(Option<String>, String, Option<CatalogObject>),
    Columns(String, String, Vec<CatalogColumn>),
    Members(String, String, Vec<CatalogMember>),
}

impl Answer {
    pub fn apply(self, cache: &mut CatalogCache) {
        match self {
            Answer::Schemas(s) => cache.set_schemas(s),
            Answer::Objects(schema, objects) => cache.set_objects(schema.as_deref(), objects),
            Answer::Resolved(schema, name, object) => {
                cache.set_resolved(schema.as_deref(), &name, object)
            }
            Answer::Columns(owner, name, cols) => cache.set_columns(&owner, &name, cols),
            Answer::Members(owner, name, members) => cache.set_members(&owner, &name, members),
        }
    }
}

/// Builds the task answering `request` on Oracle.
pub fn task(request: CatalogRequest) -> TaskFn {
    Box::new(move |conn: &Connection| {
        let answer = fetch(conn, request)?;
        Ok(Box::new(answer) as Box<dyn std::any::Any + Send>)
    })
}

/// The request answering `request` (with an [`Answer`]) on a connection of
/// `dialect`.
pub fn request(id: u64, request: CatalogRequest, dialect: Dialect) -> DbRequest {
    match dialect.kind() {
        DbKind::Oracle => DbRequest::Task {
            id,
            task: task(request),
        },
        DbKind::MySql => DbRequest::Run {
            id,
            task: Box::new(move |client| {
                let answer = mysql::fetch(client, request)?;
                Ok(Box::new(answer) as Box<dyn std::any::Any + Send>)
            }),
        },
    }
}

/// MySQL's dictionary. Code assistance asks with uppercase names (Oracle's
/// rule for unquoted ones); MySQL keeps their case, so names are compared
/// without it and answered as stored.
mod mysql {
    use pythia_db::{Cell, Client, DbError};
    use pythia_lsp_core::{CatalogColumn, CatalogObject, CatalogRequest};

    use super::Answer;

    /// Tables, views and routines as `(owner, name, type)`.
    const OBJECTS: &str = "select table_schema as owner, table_name as name, \
                                  if(table_type = 'VIEW', 'VIEW', 'TABLE') as type \
                             from information_schema.tables \
                           union all \
                           select routine_schema, routine_name, routine_type \
                             from information_schema.routines";

    pub(super) fn fetch(
        client: &mut dyn Client,
        request: CatalogRequest,
    ) -> Result<Answer, DbError> {
        Ok(match request {
            CatalogRequest::Schemas => {
                let rows = client.query(
                    "select schema_name from information_schema.schemata order by 1",
                    &[],
                )?;
                Answer::Schemas(rows.into_iter().filter_map(first).collect())
            }
            CatalogRequest::Objects { schema } => {
                let owner = owner(client, schema.as_deref())?;
                let rows = client.query(
                    &format!("select * from ({OBJECTS}) o where lower(owner) = lower(?)"),
                    &[&owner],
                )?;
                Answer::Objects(schema, rows.into_iter().filter_map(object).collect())
            }
            CatalogRequest::Resolve { schema, name } => {
                let owner = owner(client, schema.as_deref())?;
                let rows = client.query(
                    &format!(
                        "select * from ({OBJECTS}) o \
                          where lower(owner) = lower(?) and lower(name) = lower(?) \
                          order by name = ? desc, type = 'TABLE' desc limit 1"
                    ),
                    &[&owner, &name, &name],
                )?;
                let object = rows.into_iter().find_map(object);
                Answer::Resolved(schema, name, object)
            }
            CatalogRequest::Columns { owner, name } => {
                let rows = client.query(
                    "select column_name, column_type, is_nullable, nullif(column_comment, '') \
                       from information_schema.columns \
                      where lower(table_schema) = lower(?) and lower(table_name) = lower(?) \
                      order by ordinal_position",
                    &[&owner, &name],
                )?;
                let columns = rows
                    .into_iter()
                    .filter_map(|mut r| {
                        let comment = r.get_mut(3).and_then(Option::take);
                        Some(CatalogColumn {
                            name: r.first()?.clone()?,
                            data_type: r.get(1)?.clone()?,
                            nullable: r.get(2)?.as_deref() == Some("YES"),
                            comment,
                        })
                    })
                    .collect();
                Answer::Columns(owner, name, columns)
            }
            // Routines stand alone: no packages with members.
            CatalogRequest::Members { owner, name } => Answer::Members(owner, name, Vec::new()),
        })
    }

    /// The schema asked for, or the current database.
    fn owner(client: &mut dyn Client, schema: Option<&str>) -> Result<String, DbError> {
        if let Some(schema) = schema {
            return Ok(schema.to_owned());
        }
        let rows = client.query("select database()", &[])?;
        Ok(rows.into_iter().find_map(first).unwrap_or_default())
    }

    fn first(row: Vec<Cell>) -> Option<String> {
        row.into_iter().next().flatten()
    }

    fn object(row: Vec<Cell>) -> Option<CatalogObject> {
        let mut row = row.into_iter();
        Some(CatalogObject {
            owner: row.next()??,
            name: row.next()??,
            object_type: row.next()??,
        })
    }
}

fn fetch(conn: &Connection, request: CatalogRequest) -> Result<Answer, DbError> {
    Ok(match request {
        CatalogRequest::Schemas => {
            let rows =
                conn.query_as::<String>("select username from all_users order by username", &[])?;
            Answer::Schemas(rows.collect::<Result<_, _>>()?)
        }
        CatalogRequest::Objects { schema } => {
            let objects = objects(conn, schema.as_deref())?;
            Answer::Objects(schema, objects)
        }
        CatalogRequest::Resolve { schema, name } => {
            let object = resolve(conn, schema.as_deref(), &name)?;
            Answer::Resolved(schema, name, object)
        }
        CatalogRequest::Columns { owner, name } => {
            let columns = columns(conn, &owner, &name)?;
            Answer::Columns(owner, name, columns)
        }
        CatalogRequest::Members { owner, name } => {
            let members = members(conn, &owner, &name)?;
            Answer::Members(owner, name, members)
        }
    })
}

fn current_schema(conn: &Connection) -> Result<String, DbError> {
    Ok(conn.query_row_as(
        "select sys_context('USERENV', 'CURRENT_SCHEMA') from dual",
        &[],
    )?)
}

/// Objects of a schema; for the current schema (`None`) also public
/// synonyms, so `dbms_` completes to the supplied packages.
fn objects(conn: &Connection, schema: Option<&str>) -> Result<Vec<CatalogObject>, DbError> {
    let owner = match schema {
        Some(s) => s.to_owned(),
        None => current_schema(conn)?,
    };
    let mut sql = format!(
        "select owner, object_name, object_type from all_objects \
          where owner = :1 and object_type in ({TYPES}) and generated = 'N' and object_name not like 'BIN$%'"
    );
    if schema.is_none() {
        sql.push_str(
            " union all select 'PUBLIC', synonym_name, 'SYNONYM' from all_synonyms \
               where owner = 'PUBLIC' and table_owner not in ('PUBLIC') and synonym_name not like '%/%'",
        );
    }
    let rows = conn.query_as::<(String, String, String)>(
        &pythia_db::compat::first_rows(&sql, 50000),
        &[&owner],
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (owner, name, object_type) = row?;
        out.push(CatalogObject {
            owner,
            name,
            object_type,
        });
    }
    Ok(out)
}

/// Resolves a name the way Oracle does: own object, private synonym, public
/// synonym. Synonyms resolve to their target.
fn resolve(
    conn: &Connection,
    schema: Option<&str>,
    name: &str,
) -> Result<Option<CatalogObject>, DbError> {
    let owner = match schema {
        Some(s) => s.to_owned(),
        None => current_schema(conn)?,
    };
    if let Some(object) = object_in(conn, &owner, name)? {
        return Ok(Some(object));
    }
    let synonym_owners: &[&str] = if schema.is_some() {
        &[]
    } else {
        &["", "PUBLIC"]
    };
    for syn_owner in synonym_owners {
        let syn_owner = if syn_owner.is_empty() {
            owner.as_str()
        } else {
            syn_owner
        };
        let target = conn.query_row_as::<(String, String)>(
            "select table_owner, table_name from all_synonyms where owner = :1 and synonym_name = :2 and db_link is null",
            &[&syn_owner, &name],
        );
        match target {
            Ok((t_owner, t_name)) => return object_in(conn, &t_owner, &t_name),
            Err(e) if e.kind() == pythia_db::oracle::ErrorKind::NoDataFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(None)
}

fn object_in(conn: &Connection, owner: &str, name: &str) -> Result<Option<CatalogObject>, DbError> {
    // Prefer the spec over bodies and tables over their indexes etc.
    let sql = format!(
        "select owner, object_name, object_type from all_objects \
          where owner = :1 and object_name = :2 and object_type in ({TYPES}) \
          order by decode(object_type, 'TABLE', 1, 'VIEW', 2, 'PACKAGE', 3, 'TYPE', 4, 5)"
    );
    let sql = pythia_db::compat::first_rows(&sql, 1);
    match conn.query_row_as::<(String, String, String)>(&sql, &[&owner, &name]) {
        Ok((owner, name, object_type)) => Ok(Some(CatalogObject {
            owner,
            name,
            object_type,
        })),
        Err(e) if e.kind() == pythia_db::oracle::ErrorKind::NoDataFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Table/view columns, or the attributes of an object type.
fn columns(conn: &Connection, owner: &str, name: &str) -> Result<Vec<CatalogColumn>, DbError> {
    let rows = conn.query_as::<(String, String, String, Option<String>)>(
        "select c.column_name, \
                case when c.data_type in ('VARCHAR2', 'NVARCHAR2', 'CHAR', 'NCHAR', 'RAW') \
                       then c.data_type || '(' || decode(c.char_used, 'C', c.char_length || ' CHAR', c.data_length) || ')' \
                     when c.data_type = 'NUMBER' and c.data_precision is not null \
                       then 'NUMBER(' || c.data_precision || nvl2(nullif(c.data_scale, 0), ',' || c.data_scale, '') || ')' \
                     else c.data_type end, \
                c.nullable, cc.comments \
           from all_tab_columns c \
           left join all_col_comments cc \
             on cc.owner = c.owner and cc.table_name = c.table_name and cc.column_name = c.column_name \
          where c.owner = :1 and c.table_name = :2 order by c.column_id",
        &[&owner, &name],
    )?;
    let mut out = Vec::new();
    for row in rows {
        let (name, data_type, nullable, comment) = row?;
        out.push(CatalogColumn {
            name,
            data_type,
            nullable: nullable == "Y",
            comment,
        });
    }
    if out.is_empty() {
        let rows = conn.query_as::<(String, String)>(
            "select attr_name, attr_type_name from all_type_attrs where owner = :1 and type_name = :2 order by attr_no",
            &[&owner, &name],
        )?;
        for row in rows {
            let (name, data_type) = row?;
            out.push(CatalogColumn {
                name,
                data_type,
                nullable: true,
                comment: None,
            });
        }
    }
    Ok(out)
}

/// Subprograms of a package or type, or a standalone procedure/function
/// as its single member, with parameters from ALL_ARGUMENTS.
fn members(conn: &Connection, owner: &str, name: &str) -> Result<Vec<CatalogMember>, DbError> {
    // The type is NULL in the row 11g has for a parameterless subprogram.
    #[allow(clippy::type_complexity)]
    let rows = conn.query_as::<(
        String,
        Option<String>,
        u32,
        Option<String>,
        String,
        Option<String>,
        String,
        u32,
    )>(
        &format!(
            "select a.object_name, a.overload, a.position, a.argument_name, a.in_out, \
                {}, nvl(a.defaulted, 'N'), a.subprogram_id \
           from all_arguments a \
          where a.owner = :1 and a.data_level = 0 \
            and ((a.package_name = :2) or (a.package_name is null and a.object_name = :2)) \
          order by a.subprogram_id, a.overload, a.position",
            pythia_db::compat::ARGUMENT_TYPE
        ),
        &[&owner, &name],
    )?;
    let mut out: Vec<CatalogMember> = Vec::new();
    let mut current: Option<(u32, Option<String>)> = None;
    for row in rows {
        let (proc, overload, position, arg, in_out, data_type, defaulted, subprogram) = row?;
        let data_type = data_type.unwrap_or_default();
        let key = (subprogram, overload.clone());
        if current.as_ref() != Some(&key) {
            current = Some(key);
            out.push(CatalogMember {
                name: proc,
                kind: MemberKind::Procedure,
                params: Vec::new(),
                return_type: None,
                overload: overload.and_then(|o| o.parse().ok()),
            });
        }
        let member = out.last_mut().expect("pushed above");
        match (position, arg) {
            (0, _) => {
                member.kind = MemberKind::Function;
                member.return_type = Some(data_type);
            }
            (_, Some(arg)) => member.params.push(CatalogParam {
                name: arg,
                mode: match in_out.as_str() {
                    "OUT" => ParamMode::Out,
                    "IN/OUT" => ParamMode::InOut,
                    _ => ParamMode::In,
                },
                data_type,
                has_default: defaulted == "Y",
            }),
            // A parameterless subprogram has a single row without a name.
            (_, None) => {}
        }
    }
    // Package subprograms without any argument rows don't appear in
    // ALL_ARGUMENTS; pick them up from ALL_PROCEDURES.
    let rows = conn.query_as::<(String, Option<String>)>(
        "select procedure_name, overload from all_procedures \
          where owner = :1 and object_name = :2 and procedure_name is not null",
        &[&owner, &name],
    )?;
    for row in rows {
        let (proc, overload) = row?;
        let overload: Option<u32> = overload.and_then(|o| o.parse().ok());
        if !out.iter().any(|m| m.name == proc && m.overload == overload) {
            out.push(CatalogMember {
                name: proc,
                kind: MemberKind::Procedure,
                params: Vec::new(),
                return_type: None,
                overload,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use pythia_lsp_core::{Analysis, Catalog};

    use super::*;

    /// Runs `f` until the engine stops asking for dictionary data, answering
    /// every request from the live database.
    fn settle<T>(
        conn: &Connection,
        cache: &mut CatalogCache,
        mut f: impl FnMut(&dyn Catalog) -> (T, Vec<CatalogRequest>),
    ) -> T {
        for _ in 0..8 {
            let (value, missing) = f(cache);
            if missing.is_empty() {
                return value;
            }
            for request in missing {
                fetch(conn, request).expect("catalog query").apply(cache);
            }
        }
        panic!("catalog requests did not settle");
    }

    #[test]
    fn completion_against_live_database() {
        let Ok(spec) = std::env::var("PYTHIA_TEST_DB") else {
            eprintln!("PYTHIA_TEST_DB not set, skipping");
            return;
        };
        crate::test_db::demo_schema();
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (user, pw) = cred.split_once('/').unwrap();
        let conn = Connection::connect(user, pw, db).unwrap();
        let mut cache = CatalogCache::new(current_schema(&conn).unwrap());

        // Columns after an alias (the table is resolved and described).
        let src = "select e. from demo_employees e";
        let analysis = Analysis::new(src);
        let labels = settle(&conn, &mut cache, |cat| {
            let c = analysis.complete(src.find("e.").unwrap() + 2, cat);
            (
                c.items
                    .iter()
                    .map(|i| i.label.to_uppercase())
                    .collect::<Vec<_>>(),
                c.missing,
            )
        });
        assert!(
            labels.contains(&"SALARY".to_owned()) && labels.contains(&"EMPLOYEE_ID".to_owned()),
            "{labels:?}"
        );

        // Package members, with parameters from ALL_ARGUMENTS.
        let src = "begin demo_pkg.ra";
        let analysis = Analysis::new(src);
        let items = settle(&conn, &mut cache, |cat| {
            let c = analysis.complete(src.len(), cat);
            (c.items, c.missing)
        });
        let raise = items
            .iter()
            .find(|i| i.label.eq_ignore_ascii_case("raise_salary"))
            .expect("member");
        assert!(
            raise.detail.to_lowercase().contains("p_employee_id"),
            "{raise:?}"
        );

        // Signature help inside the call.
        let src = "begin demo_pkg.raise_salary(1, ";
        let analysis = Analysis::new(src);
        let help = settle(&conn, &mut cache, |cat| {
            let h = analysis.signature_help(src.len(), cat).expect("signature");
            let missing = h.missing.clone();
            (h, missing)
        });
        assert_eq!(help.active_param, 1);
        assert!(
            help.signatures[0]
                .label
                .to_lowercase()
                .contains("p_percent"),
            "{:?}",
            help.signatures
        );

        // Public synonyms for supplied packages.
        let src = "begin dbms_outp";
        let analysis = Analysis::new(src);
        let labels = settle(&conn, &mut cache, |cat| {
            let c = analysis.complete(src.len(), cat);
            (
                c.items
                    .iter()
                    .map(|i| i.label.to_uppercase())
                    .collect::<Vec<_>>(),
                c.missing,
            )
        });
        assert!(labels.contains(&"DBMS_OUTPUT".to_owned()), "{labels:?}");

        // Go to definition resolves a table through the dictionary.
        let src = "select * from demo_departments";
        let analysis = Analysis::new(src);
        let target = settle(&conn, &mut cache, |cat| {
            let d = analysis.definition(src.len() - 3, cat);
            (d.target, d.missing)
        });
        assert!(
            matches!(&target, Some(pythia_lsp_core::Definition::Object { name, object_type, .. })
                if name == "DEMO_DEPARTMENTS" && object_type == "TABLE"),
            "{target:?}"
        );
    }
}

/// Completion with MySQL's dictionary (skipped unless `PYTHIA_TEST_MYSQL`
/// is set to `user/password@host:port/database`).
#[cfg(test)]
mod mysql_live {
    use std::sync::mpsc;
    use std::time::Duration;

    use pythia_db::{ConnectParams, DbEvent, Session};
    use pythia_lsp_core::{Analysis, CatalogCache, CatalogRequest};

    use super::*;

    struct Live {
        session: Session,
        wake: mpsc::Receiver<()>,
    }

    impl Live {
        fn next(&mut self) -> DbEvent {
            loop {
                if let Some(event) = self.session.try_recv() {
                    return event;
                }
                self.wake
                    .recv_timeout(Duration::from_secs(30))
                    .expect("timed out");
            }
        }

        fn answer(&mut self, request: CatalogRequest) -> Answer {
            self.session
                .send(super::request(7, request, Dialect::MYSQL));
            loop {
                if let DbEvent::TaskDone { result, .. } = self.next() {
                    return *result
                        .expect("catalog query")
                        .downcast::<Answer>()
                        .expect("an answer");
                }
            }
        }

        fn run(&mut self, sql: &str) {
            self.session.send(DbRequest::Execute {
                id: 1,
                sql: sql.into(),
                max_rows: 10,
                binds: Vec::new(),
            });
            loop {
                match self.next() {
                    DbEvent::Executed { .. } => return,
                    DbEvent::Failed { error, .. } => panic!("{sql}: {error}"),
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn columns_after_an_alias() {
        let Ok(spec) = std::env::var("PYTHIA_TEST_MYSQL") else {
            return;
        };
        let (credentials, connect_string) = spec.rsplit_once('@').unwrap();
        let (username, password) = credentials.split_once('/').unwrap();
        let (tx, wake) = mpsc::channel();
        let session = Session::connect(
            ConnectParams {
                kind: DbKind::MySql,
                username: username.into(),
                password: password.into(),
                connect_string: connect_string.into(),
                privilege: Default::default(),
                init_sql: Vec::new(),
                ssh: None,
            },
            move || {
                let _ = tx.send(());
            },
        );
        let mut live = Live { session, wake };
        let DbEvent::Connected(server) = live.next() else {
            panic!("not connected")
        };
        live.run("drop table if exists cat_orders");
        live.run("create table cat_orders (id int primary key, Customer varchar(30), `order` int)");

        let mut cache = CatalogCache::new(server.current_schema)
            .with_names(pythia_lsp_core::NameStyle::AsStored);
        let src = "select o.\n  from cat_orders o";
        let analysis = Analysis::new(src);
        let mut items = Vec::new();
        for _ in 0..8 {
            let c = analysis.complete(9, &cache);
            if c.missing.is_empty() {
                items = c.items;
                break;
            }
            for request in c.missing {
                live.answer(request).apply(&mut cache);
            }
        }
        // Names as stored; backticks only where MySQL needs them.
        let inserted: Vec<(&str, &str)> = items
            .iter()
            .map(|i| (i.label.as_str(), i.insert_text.as_str()))
            .collect();
        assert!(inserted.contains(&("Customer", "Customer")), "{inserted:?}");
        assert!(inserted.contains(&("order", "`order`")), "{inserted:?}");
        live.run("drop table cat_orders");
    }
}

#[cfg(test)]
mod test_script_live {
    use super::*;

    #[test]
    fn generated_test_block_runs() {
        let Ok(spec) = std::env::var("PYTHIA_TEST_DB") else {
            return;
        };
        crate::test_db::demo_schema();
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (user, pw) = cred.split_once('/').unwrap();
        let conn = Connection::connect(user, pw, db).unwrap();
        let members = members(&conn, &current_schema(&conn).unwrap(), "DEMO_PKG").unwrap();
        let total = members
            .iter()
            .find(|m| m.name == "DEPARTMENT_TOTAL")
            .expect("member");
        let block = crate::test_script::test_block("demo_pkg.department_total", total);
        let block = block
            .lines()
            .filter(|l| !l.starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        conn.execute("begin dbms_output.enable(null); end;", &[])
            .unwrap();
        conn.execute_named(&block, &[("p_department_id", &20)])
            .unwrap();
        let mut stmt = conn
            .statement("begin dbms_output.get_line(:1, :2); end;")
            .build()
            .unwrap();
        stmt.execute(&[
            &pythia_db::oracle::sql_type::OracleType::Varchar2(32767),
            &pythia_db::oracle::sql_type::OracleType::Number(0, 0),
        ])
        .unwrap();
        let line: String = stmt.bind_value(1).unwrap();
        assert!(line.starts_with("result = "), "{line}");
    }
}
