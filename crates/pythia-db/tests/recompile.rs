//! Recompiling invalid objects against a real database (skipped unless
//! `PYTHIA_TEST_DB` is set).

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use pythia_db::recompile::{
    InvalidScope, ObjectId, Progress, invalid_counts, invalid_objects, recompile,
};
use pythia_db::{ConnectParams, open_connection};

fn params() -> Option<ConnectParams> {
    let spec = std::env::var("PYTHIA_TEST_DB").ok()?;
    let (cred, db) = spec.rsplit_once('@')?;
    let (user, pw) = cred.split_once('/')?;
    Some(ConnectParams {
        username: user.into(),
        password: pw.into(),
        connect_string: db.into(),
        privilege: Default::default(),
        init_sql: Vec::new(),
    })
}

const SETUP: &[&str] = &[
    "create table rc_t (a number, b number)",
    "create or replace view rc_v1 as select a, b from rc_t",
    "create or replace view rc_v2 as select a from rc_v1",
    "create or replace package rc_p as function total return number; end;",
    "create or replace package body rc_p as \
       function total return number is l number; \
       begin select count(*) into l from rc_v2; return l; end; end;",
    "create or replace package rc_bad as procedure p; end;",
    "create or replace package body rc_bad as \
       procedure p is l number; begin select b into l from rc_v2; end; end;",
];

#[test]
fn recompiles_in_dependency_order_over_several_sessions() {
    let Some(params) = params() else {
        eprintln!("PYTHIA_TEST_DB not set, skipping");
        return;
    };
    let conn = open_connection(&params).unwrap();
    for drop in [
        "drop package rc_bad",
        "drop package rc_p",
        "drop view rc_v2",
        "drop view rc_v1",
        "drop table rc_t purge",
    ] {
        let _ = conn.execute(drop, &[]);
    }
    for sql in SETUP {
        let _ = conn.execute(sql, &[]);
    }
    // Recreating the table invalidates everything built on it.
    conn.execute("drop table rc_t purge", &[]).unwrap();
    conn.execute("create table rc_t (a number, b number)", &[])
        .unwrap();
    let _ = conn.execute("alter view rc_v1 compile", &[]);

    let user = params.username.to_uppercase();
    let scope = InvalidScope::Schema(user.clone());
    let ours = |objects: &[pythia_db::recompile::InvalidObject]| -> Vec<ObjectId> {
        objects
            .iter()
            .filter(|o| o.id.name.starts_with("RC_"))
            .map(|o| o.id.clone())
            .collect()
    };
    let invalid = invalid_objects(&conn, &scope).unwrap();
    let before = ours(&invalid);
    assert!(before.len() >= 3, "{before:?}");
    assert!(invalid.iter().all(|o| o.compilable), "own schema");
    let count = invalid_counts(&conn, &scope).unwrap();
    assert!(
        count
            .iter()
            .any(|(owner, n)| *owner == user && *n as usize >= before.len())
    );

    let progress = Mutex::new(Progress::default());
    let summary = recompile(
        &conn,
        &params,
        &before,
        3,
        &progress,
        &AtomicBool::new(false),
    )
    .unwrap();
    eprintln!("{summary:?}");
    assert!(summary.failed.is_empty(), "{:?}", summary.failed);
    assert!(!summary.cancelled);
    let p = progress.lock().unwrap().clone();
    assert_eq!((p.done, p.total), (before.len(), before.len()));
    assert!(p.waves >= 2, "views and packages come in waves: {p:?}");
    assert!(p.running.is_empty());

    // Only the package body that doesn't compile is left, with its error.
    let after = invalid_objects(&conn, &scope).unwrap();
    let left: Vec<_> = after
        .iter()
        .filter(|o| o.id.name.starts_with("RC_"))
        .collect();
    assert_eq!(left.len(), 1, "{left:?}");
    assert_eq!(left[0].id, ObjectId::new(&user, "RC_BAD", "PACKAGE BODY"));
    let (line, text) = left[0].first_error.clone().expect("error");
    assert!(line >= 1 && text.contains("B"), "{text}");
    assert!(left[0].errors >= 1);

    for drop in [
        "drop package rc_bad",
        "drop package rc_p",
        "drop view rc_v2",
        "drop view rc_v1",
        "drop table rc_t purge",
    ] {
        conn.execute(drop, &[]).unwrap();
    }
}
