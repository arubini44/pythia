//! Integration tests against a real database.
//!
//! Skipped unless `PYTHIA_TEST_DB` is set to `user/password@connect_string`,
//! e.g. the Oracle Free container from `docker-compose.yml`:
//! `PYTHIA_TEST_DB=pythia/pythia@localhost:1521/FREEPDB1 cargo test -p pythia-db`

use std::sync::mpsc;
use std::time::Duration;

use pythia_config::Privilege;
use pythia_db::{ConnectParams, DbEvent, DbRequest, MetaQuery, MetaResult, ObjectType, Session};

fn params() -> Option<ConnectParams> {
    let spec = std::env::var("PYTHIA_TEST_DB").ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (username, password) = credentials.split_once('/')?;
    Some(ConnectParams {
        username: username.into(),
        password: password.into(),
        connect_string: connect_string.into(),
        privilege: Privilege::Normal,
        init_sql: Vec::new(),
    })
}

/// The DBA connection (`PYTHIA_TEST_DB_DBA`), after granting the test user
/// SELECT_CATALOG_ROLE for V$SESSION and DBA_DB_LINKS. It applies to sessions
/// that log in afterwards.
fn dba_with_catalog_role() -> Option<pythia_db::oracle::Connection> {
    let spec = std::env::var("PYTHIA_TEST_DB_DBA").ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (user, password) = credentials.split_once('/')?;
    let dba = pythia_db::oracle::Connection::connect(user, password, connect_string).ok()?;
    let test_user = params()?.username;
    dba.execute(&format!("grant select_catalog_role to {test_user}"), &[])
        .unwrap();
    Some(dba)
}

struct Harness {
    session: Session,
    wake: mpsc::Receiver<()>,
}

impl Harness {
    fn connect() -> Option<Self> {
        let params = params()?;
        let (tx, wake) = mpsc::channel();
        let session = Session::connect(params, move || {
            let _ = tx.send(());
        });
        let mut h = Self { session, wake };
        match h.next() {
            DbEvent::Connected(_) => Some(h),
            other => panic!("connect failed: {other:?}"),
        }
    }

    fn next(&mut self) -> DbEvent {
        loop {
            if let Some(event) = self.session.try_recv() {
                return event;
            }
            self.wake
                .recv_timeout(Duration::from_secs(60))
                .expect("timed out waiting for db event");
        }
    }

    /// Next event that isn't transaction/output bookkeeping.
    fn next_result(&mut self) -> DbEvent {
        loop {
            match self.next() {
                DbEvent::Transaction { .. } | DbEvent::Output(_) => continue,
                event => return event,
            }
        }
    }
}

#[test]
fn query_fetch_more_and_output() {
    let Some(mut h) = Harness::connect() else {
        eprintln!("PYTHIA_TEST_DB not set, skipping");
        return;
    };

    let sql = "select level as n, 'row ' || level as label from dual connect by level <= 25";
    h.session.send(DbRequest::Execute {
        id: 1,
        sql: sql.into(),
        max_rows: 10,
        binds: vec![],
    });
    let DbEvent::QueryStarted { columns, .. } = h.next_result() else {
        panic!("expected QueryStarted")
    };
    assert_eq!(
        columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["N", "LABEL"]
    );
    assert!(columns[0].numeric && !columns[1].numeric);
    let DbEvent::Rows { rows, more, .. } = h.next_result() else {
        panic!("expected Rows")
    };
    assert_eq!((rows.len(), more), (10, true));
    assert_eq!(rows[0], vec![Some("1".into()), Some("row 1".into())]);

    h.session.send(DbRequest::FetchMore {
        id: 1,
        max_rows: 100,
    });
    let DbEvent::Rows { rows, more, .. } = h.next_result() else {
        panic!("expected Rows")
    };
    assert_eq!((rows.len(), more), (15, false));

    h.session.send(DbRequest::Execute {
        id: 2,
        sql: "begin dbms_output.put_line('hello'); end;".into(),
        max_rows: 10,
        binds: vec![],
    });
    // The output comes before the statement's result.
    let mut output = Vec::new();
    loop {
        match h.next() {
            DbEvent::Output(lines) => output.extend(lines),
            DbEvent::Transaction { .. } => {}
            DbEvent::Executed { id: 2, .. } => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(output, ["hello"]);
}

#[test]
fn errors_carry_ora_code() {
    let Some(mut h) = Harness::connect() else {
        return;
    };
    h.session.send(DbRequest::Execute {
        id: 1,
        sql: "select * from no_such_table".into(),
        max_rows: 10,
        binds: vec![],
    });
    let DbEvent::Failed { error, .. } = h.next_result() else {
        panic!("expected Failed")
    };
    assert_eq!(error.code, Some(942));
}

#[test]
fn transaction_state() {
    let Some(mut h) = Harness::connect() else {
        return;
    };
    h.session.send(DbRequest::Execute {
        id: 1,
        sql: "create global temporary table pythia_tx_test (x number)".into(),
        max_rows: 1,
        binds: vec![],
    });
    let _ = h.next_result(); // may already exist
    h.session.send(DbRequest::Execute {
        id: 2,
        sql: "insert into pythia_tx_test values (1)".into(),
        max_rows: 1,
        binds: vec![],
    });
    // The transaction state comes before the statement's result.
    assert!(matches!(h.next(), DbEvent::Transaction { pending: true }));
    let DbEvent::Executed { summary, .. } = h.next() else {
        panic!("expected Executed")
    };
    assert_eq!(summary.rows_affected, Some(1));
    h.session.send(DbRequest::Rollback);
    assert!(matches!(h.next(), DbEvent::Notice(_)));
    assert!(matches!(h.next(), DbEvent::Transaction { pending: false }));
}

#[test]
fn binds() {
    let Some(mut h) = Harness::connect() else {
        return;
    };
    h.session.send(DbRequest::Execute {
        id: 1,
        sql: "select :a || '-' || :b as v from dual".into(),
        max_rows: 10,
        binds: vec![("a".into(), Some("x".into())), ("b".into(), None)],
    });
    assert!(matches!(h.next_result(), DbEvent::QueryStarted { .. }));
    let DbEvent::Rows { rows, .. } = h.next_result() else {
        panic!("expected Rows")
    };
    assert_eq!(rows[0][0].as_deref(), Some("x-"));
}

#[test]
fn compile_errors_and_metadata() {
    let Some(mut h) = Harness::connect() else {
        return;
    };
    let exec = |h: &mut Harness, id, sql: &str| {
        h.session.send(DbRequest::Execute {
            id,
            sql: sql.into(),
            max_rows: 10,
            binds: vec![],
        });
        h.next_result()
    };
    let DbEvent::Executed { summary, .. } = exec(
        &mut h,
        1,
        "create or replace procedure pythia_broken is begin nonexistent_call; end;",
    ) else {
        panic!("expected Executed")
    };
    let compiled = summary.compiled.expect("compile info");
    assert_eq!(compiled.name, "PYTHIA_BROKEN");
    assert!(!compiled.errors.is_empty(), "{compiled:?}");
    assert_eq!(compiled.errors[0].line, 1);

    exec(
        &mut h,
        2,
        "create or replace package pythia_pkg as procedure p(a number, b out varchar2); function f return date; end;",
    );
    exec(
        &mut h,
        3,
        "create table pythia_meta_t (id number(10) primary key, name varchar2(30 char) not null, note clob)",
    );

    let meta = |h: &mut Harness, id, query| {
        h.session.send(DbRequest::Meta { id, query });
        match h.next_result() {
            DbEvent::Meta { result, .. } => result.expect("meta query"),
            other => panic!("expected Meta, got {other:?}"),
        }
    };
    let owner = "PYTHIA".to_string();
    let MetaResult::Schemas(schemas) = meta(&mut h, 10, MetaQuery::Schemas) else {
        panic!()
    };
    assert!(schemas.contains(&owner));

    let MetaResult::Objects(objs) = meta(
        &mut h,
        11,
        MetaQuery::Objects {
            owner: owner.clone(),
            object_type: ObjectType::Procedure,
            filter: Some("pythia_br".into()),
        },
    ) else {
        panic!()
    };
    assert_eq!(objs.len(), 1);
    assert!(!objs[0].valid);

    let MetaResult::Columns(cols) = meta(
        &mut h,
        12,
        MetaQuery::Columns {
            owner: owner.clone(),
            table: "PYTHIA_META_T".into(),
            keys: true,
        },
    ) else {
        panic!()
    };
    let summary: Vec<_> = cols
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.data_type.as_str(),
                c.nullable,
                c.pk_position,
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("ID", "NUMBER(10)", false, Some(1)),
            ("NAME", "VARCHAR2(30 CHAR)", false, None),
            ("NOTE", "CLOB", true, None)
        ]
    );

    let MetaResult::Members(members) = meta(
        &mut h,
        13,
        MetaQuery::Members {
            owner: owner.clone(),
            package: "PYTHIA_PKG".into(),
        },
    ) else {
        panic!()
    };
    assert_eq!(members.len(), 2);
    assert_eq!(members[0].signature, "(a IN NUMBER, b OUT VARCHAR2)");
    assert!(members[1].is_function);

    let MetaResult::Text(src) = meta(
        &mut h,
        14,
        MetaQuery::Source {
            owner: owner.clone(),
            name: "PYTHIA_PKG".into(),
            object_type: ObjectType::Package,
        },
    ) else {
        panic!()
    };
    assert!(
        src.starts_with("CREATE OR REPLACE package pythia_pkg"),
        "{src}"
    );

    let MetaResult::Text(ddl) = meta(
        &mut h,
        15,
        MetaQuery::Ddl {
            owner: owner.clone(),
            name: "PYTHIA_META_T".into(),
            object_type: ObjectType::Table,
        },
    ) else {
        panic!()
    };
    assert!(ddl.contains("CREATE TABLE"), "{ddl}");

    let MetaResult::Table { rows, .. } = meta(
        &mut h,
        16,
        MetaQuery::Details {
            owner: owner.clone(),
            name: "PYTHIA_META_T".into(),
            detail: pythia_db::Detail::Constraints,
        },
    ) else {
        panic!()
    };
    assert_eq!(rows.len(), 1, "{rows:?}");

    let MetaResult::Table { rows, .. } = meta(
        &mut h,
        17,
        MetaQuery::ExplainPlan {
            sql: "select * from pythia_meta_t where id = 1".into(),
        },
    ) else {
        panic!()
    };
    assert!(rows.len() >= 2, "{rows:?}");

    let MetaResult::Objects(found) = meta(
        &mut h,
        18,
        MetaQuery::Find {
            pattern: "pythia_m".into(),
            limit: 10,
        },
    ) else {
        panic!()
    };
    assert!(found.iter().any(|o| o.name == "PYTHIA_META_T"));

    exec(&mut h, 20, "drop table pythia_meta_t purge");
}

#[test]
fn rowid_of_wrapped_query() {
    let Some(mut h) = Harness::connect() else {
        return;
    };
    let exec = |h: &mut Harness, id, sql: &str| {
        h.session.send(DbRequest::Execute {
            id,
            sql: sql.into(),
            max_rows: 1,
            binds: vec![],
        });
        h.next_result()
    };
    exec(
        &mut h,
        1,
        "create table pythia_rowid_t (id number primary key, v varchar2(10))",
    );
    // Editable results wrap the user's query to fetch the base table's ROWID.
    let ok = "select q.*, q.rowid as pythia$rowid from (select id, v from pythia_rowid_t where id > 0 order by id) q";
    let DbEvent::QueryStarted { columns, .. } = exec(&mut h, 2, ok) else {
        panic!("expected rows")
    };
    assert_eq!(columns.last().unwrap().type_name, "ROWID");
    h.next_result();
    let grouped = "select q.*, q.rowid as pythia$rowid from (select v, count(*) n from pythia_rowid_t group by v) q";
    let DbEvent::Failed { error, .. } = exec(&mut h, 3, grouped) else {
        panic!("expected failure")
    };
    assert_eq!(error.code, Some(1446));
    exec(&mut h, 4, "drop table pythia_rowid_t purge");
}

#[test]
fn login_script_runs_on_connect() {
    let Some(mut params) = params() else {
        return;
    };
    params.init_sql = vec!["alter session set nls_date_format = 'YYYY'".into()];
    let (tx, wake) = mpsc::channel();
    let session = Session::connect(params, move || {
        let _ = tx.send(());
    });
    let mut h = Harness { session, wake };
    assert!(matches!(h.next(), DbEvent::Connected(_)));
    h.session.send(DbRequest::Execute {
        id: 1,
        sql: "select to_char(date '2024-06-01') from dual".into(),
        max_rows: 1,
        binds: vec![],
    });
    assert!(matches!(h.next_result(), DbEvent::QueryStarted { .. }));
    let DbEvent::Rows { rows, .. } = h.next_result() else {
        panic!("expected Rows")
    };
    assert_eq!(rows[0][0].as_deref(), Some("2024"));
}

#[test]
fn killed_session_is_reported_as_lost() {
    let Some(admin) = dba_with_catalog_role() else {
        return;
    };
    let Some(mut h) = Harness::connect() else {
        return;
    };
    h.session.send(DbRequest::Execute {
        id: 1,
        sql:
            "select sid || ',' || serial# from v$session where sid = sys_context('USERENV', 'SID')"
                .into(),
        max_rows: 1,
        binds: vec![],
    });
    let DbEvent::QueryStarted { .. } = h.next_result() else {
        panic!("expected rows")
    };
    let DbEvent::Rows { rows, .. } = h.next_result() else {
        panic!("expected Rows")
    };
    let sid_serial = rows[0][0].clone().unwrap();
    // ORA-00031 (only marked for kill) would make the driver drop `admin`.
    admin
        .execute(
            &format!(
                "begin execute immediate 'alter system kill session ''{sid_serial}'' immediate'; \
                 exception when others then if sqlcode != -31 then raise; end if; end;"
            ),
            &[],
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(500));
    h.session.send(DbRequest::Execute {
        id: 2,
        sql: "select 1 from dual".into(),
        max_rows: 1,
        binds: vec![],
    });
    match h.next_result() {
        DbEvent::Failed { error, .. } => assert!(error.is_connection_lost(), "{error:?}"),
        other => panic!("expected failure, got {other:?}"),
    }
}

/// Public links and another schema's private link: listed, described and
/// tested. Needs `PYTHIA_TEST_DB_DBA` to create them.
#[test]
fn database_links() {
    let Some(dba) = dba_with_catalog_role() else {
        return;
    };
    let Some(mut h) = Harness::connect() else {
        return;
    };
    let target = params().unwrap().connect_string;
    for sql in [
        "drop public database link pythia_t_pub",
        "drop database link pythia_t_sys",
    ] {
        let _ = dba.execute(sql, &[]);
    }
    for sql in [
        format!(
            "create public database link pythia_t_pub connect to pythia identified by pythia using '{target}'"
        ),
        format!(
            "create database link pythia_t_sys connect to pythia identified by pythia using '{target}'"
        ),
    ] {
        dba.execute(&sql, &[]).unwrap();
    }
    let dba_user = dba
        .query_row_as::<String>("select user from dual", &[])
        .unwrap();
    let mut next_id = 100;
    let mut meta = |h: &mut Harness, query| {
        next_id += 1;
        h.session.send(DbRequest::Meta { id: next_id, query });
        match h.next_result() {
            DbEvent::Meta { result, .. } => result,
            other => panic!("expected Meta, got {other:?}"),
        }
    };
    let links = |owner: &str| MetaQuery::Objects {
        owner: owner.into(),
        object_type: ObjectType::DatabaseLink,
        filter: Some("PYTHIA_T_".into()),
    };
    let details = |owner: &str, name: &str| MetaQuery::Details {
        owner: owner.into(),
        name: name.into(),
        detail: pythia_db::Detail::Link,
    };
    let test = |owner: &str, name: &str| MetaQuery::TestLink {
        owner: owner.into(),
        name: name.into(),
    };

    let Ok(MetaResult::Objects(public)) = meta(&mut h, links("PUBLIC")) else {
        panic!()
    };
    assert_eq!(public.len(), 1, "{public:?}");
    assert_eq!(public[0].name, "PYTHIA_T_PUB");
    assert!(
        !public[0].last_ddl.is_empty(),
        "the creation time stands in"
    );
    let Ok(MetaResult::Objects(private)) = meta(&mut h, links(&dba_user)) else {
        panic!()
    };
    assert_eq!(private.len(), 1, "{private:?}");

    // Public from ALL_DB_LINKS, the other schema's from DBA_DB_LINKS.
    for (owner, name) in [("PUBLIC", "PYTHIA_T_PUB"), (&dba_user, "PYTHIA_T_SYS")] {
        let Ok(MetaResult::Table { rows, .. }) = meta(&mut h, details(owner, name)) else {
            panic!()
        };
        let host = rows.iter().find(|r| r[0].as_deref() == Some("Host"));
        assert_eq!(
            host.and_then(|r| r[1].clone()),
            Some(target.clone()),
            "{rows:?}"
        );
    }

    let Ok(MetaResult::Text(remote)) = meta(&mut h, test("PUBLIC", "PYTHIA_T_PUB")) else {
        panic!()
    };
    assert!(!remote.is_empty());
    let Err(e) = meta(&mut h, test(&dba_user, "PYTHIA_T_SYS")) else {
        panic!("another schema's private link can't be used")
    };
    assert!(e.to_string().contains("private"), "{e}");

    let Ok(MetaResult::Text(ddl)) = meta(
        &mut h,
        MetaQuery::Ddl {
            owner: "PUBLIC".into(),
            name: "PYTHIA_T_PUB".into(),
            object_type: ObjectType::DatabaseLink,
        },
    ) else {
        panic!()
    };
    assert!(ddl.contains("CREATE PUBLIC DATABASE LINK"), "{ddl}");

    let Ok(MetaResult::Objects(synonyms)) = meta(
        &mut h,
        MetaQuery::Objects {
            owner: "PUBLIC".into(),
            object_type: ObjectType::Synonym,
            filter: Some("DBMS_OUTPUT".into()),
        },
    ) else {
        panic!()
    };
    assert!(synonyms.iter().any(|s| s.name == "DBMS_OUTPUT"));

    dba.execute("drop public database link pythia_t_pub", &[])
        .unwrap();
    dba.execute("drop database link pythia_t_sys", &[]).unwrap();
}
