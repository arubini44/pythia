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
    assert!(matches!(h.next_result(), DbEvent::Executed { id: 2, .. }));
    loop {
        if let DbEvent::Output(lines) = h.next() {
            assert_eq!(lines, ["hello"]);
            break;
        }
    }
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
    let DbEvent::Executed { summary, .. } = h.next_result() else {
        panic!("expected Executed")
    };
    assert_eq!(summary.rows_affected, Some(1));
    assert!(matches!(h.next(), DbEvent::Transaction { pending: true }));
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
    let (Some(mut h), Ok(dba)) = (Harness::connect(), std::env::var("PYTHIA_TEST_DB_DBA")) else {
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
    let (cred, db) = dba.rsplit_once('@').unwrap();
    let (user, pw) = cred.split_once('/').unwrap();
    let admin = pythia_db::oracle::Connection::connect(user, pw, db).unwrap();
    admin
        .execute(
            &format!("alter system kill session '{sid_serial}' immediate"),
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
