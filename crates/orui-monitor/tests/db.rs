//! Integration tests against a real database.
//!
//! Skipped unless `ORUI_TEST_DB_DBA` is set (a DBA account), e.g.
//! `ORUI_TEST_DB_DBA=system/oracle@localhost:1521/FREEPDB1`. The app-user
//! capability test also uses `ORUI_TEST_DB` (e.g. `orui/orui@...`) if set.

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use orui_db::oracle::Connection;
use orui_monitor::queries::{self, SessionFilter};
use orui_monitor::tkprof::{self, CallKind};

fn connect(var: &str) -> Option<Connection> {
    let spec = std::env::var(var).ok()?;
    let (cred, db) = spec.rsplit_once('@')?;
    let (user, pass) = cred.split_once('/')?;
    Some(Connection::connect(user, pass, db).expect("connect"))
}

fn dba() -> Option<Connection> {
    let c = connect("ORUI_TEST_DB_DBA");
    if c.is_none() {
        eprintln!("ORUI_TEST_DB_DBA not set, skipping");
    }
    c
}

fn sid(c: &Connection) -> i64 {
    c.query_row_as(
        "select to_number(sys_context('USERENV', 'SID')) from dual",
        &[],
    )
    .unwrap()
}

/// Polls until `f` returns true or the timeout passes.
fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "timed out waiting for {what}"
        );
        thread::sleep(Duration::from_millis(200));
    }
}

#[test]
fn capabilities_dba_and_app_user() {
    let Some(conn) = dba() else { return };
    let caps = queries::capabilities(&conn).unwrap();
    assert!(
        caps.missing_views().is_empty(),
        "missing: {:?}",
        caps.missing_views()
    );
    assert!(caps.alter_system && caps.dbms_monitor);
    assert!(caps.session_control().is_ok());
    assert!(caps.trace_files().is_ok());
    // Oracle Free enables the packs; the gate must allow AWR there.
    assert!(caps.pack_access.is_some());
    assert_eq!(caps.awr().is_ok(), caps.diagnostics_licensed());

    // The unprivileged checks only hold for a user without catalog access.
    let catalog_role = |c: &Connection| {
        c.query_row_as::<i64>(
            "select count(*) from session_roles where role = 'SELECT_CATALOG_ROLE'",
            &[],
        )
        .unwrap()
            > 0
    };
    if let Some(app) = connect("ORUI_TEST_DB").filter(|c| !catalog_role(c)) {
        let caps = queries::capabilities(&app).unwrap();
        assert!(!caps.missing_views().is_empty());
        assert!(caps.awr().is_err());
        assert!(caps.session_control().is_err());
        // The dashboard degrades per section instead of failing.
        let snap = queries::dashboard_snapshot(&app);
        assert!(!snap.errors.is_empty());
        assert!(
            snap.errors
                .iter()
                .any(|(_, e)| e.contains("SELECT_CATALOG_ROLE")),
            "{:?}",
            snap.errors
        );
    }
}

#[test]
fn dashboard_snapshot() {
    let Some(conn) = dba() else { return };
    let a = queries::dashboard_snapshot(&conn);
    assert!(a.errors.is_empty(), "{:?}", a.errors);
    let inst = a.instance.as_ref().unwrap();
    assert!(inst.cpus >= 1 && inst.uptime_secs > 0);
    assert!(a.sessions.unwrap().total > 0);
    assert!(a.open_cursors.unwrap() > 0);
    assert!(!a.tablespaces.is_empty());
    assert!(!a.top_sql.is_empty());
    assert!(a.memory.as_ref().is_some_and(|m| !m.sga.is_empty()));

    for _ in 0..50 {
        conn.query_row_as::<i64>("select count(*) from all_objects where rownum < 100", &[])
            .unwrap();
    }
    let b = queries::dashboard_snapshot(&conn);
    let rates = queries::Rates::between(&a.counters.unwrap(), &b.counters.unwrap(), 1.0);
    assert!(rates.executions >= 50.0, "{rates:?}");
    assert!(rates.logical_reads > 0.0);
}

#[test]
fn blocking_chain_cancel_and_kill() {
    let Some(conn) = dba() else { return };
    let _ = conn.execute("drop table orui_mon_lock purge", &[]);
    conn.execute(
        "create table orui_mon_lock (id number primary key, v number)",
        &[],
    )
    .unwrap();
    conn.execute("insert into orui_mon_lock values (1, 0)", &[])
        .unwrap();
    conn.commit().unwrap();

    let spec = std::env::var("ORUI_TEST_DB_DBA").unwrap();
    let open = || {
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (u, p) = cred.split_once('/').unwrap();
        Connection::connect(u, p, db).unwrap()
    };

    // A holds the row lock.
    let a = open();
    let a_sid = sid(&a);
    a.execute("update orui_mon_lock set v = 1 where id = 1", &[])
        .unwrap();

    // B and C wait for it in their own threads.
    let spawn_waiter = |tag: &'static str| {
        let c = open();
        let my_sid = sid(&c);
        let (done_tx, done_rx) = mpsc::channel::<()>();
        let handle = thread::spawn(move || {
            let r = c.execute(
                &format!("update orui_mon_lock set v = 2 where id = 1 /* {tag} */"),
                &[],
            );
            let outcome = r
                .map(|_| ())
                .map_err(|e| e.db_error().map(|d| d.code()).unwrap_or(-1));
            // Hold any lock we got until told to finish.
            let _ = done_rx.recv();
            let _ = c.rollback();
            outcome
        });
        (my_sid, done_tx, handle)
    };
    let (b_sid, b_done, b_thread) = spawn_waiter("B");
    let (c_sid, c_done, c_thread) = spawn_waiter("C");

    wait_for("two blocked sessions", || {
        queries::sessions(&conn, SessionFilter::default())
            .unwrap()
            .iter()
            .filter(|s| s.blocking_session == Some(a_sid))
            .count()
            == 2
    });

    // The dashboard names the final blocker and the insight points at it.
    let snap = queries::dashboard_snapshot(&conn);
    let blocker = snap
        .blockers
        .iter()
        .find(|b| b.sid == a_sid)
        .unwrap_or_else(|| panic!("blockers {:?} errors {:?}", snap.blockers, snap.errors));
    assert_eq!(blocker.blocked, 2);
    assert_eq!(blocker.status, "INACTIVE");
    let found = orui_monitor::insights::insights(&snap, None);
    assert!(
        found
            .iter()
            .any(|i| i.text.contains(&format!("Session {a_sid} "))),
        "{found:?}"
    );

    let tree = queries::blocking_tree(&conn).unwrap();
    let root = tree
        .iter()
        .find(|n| n.session.sid == a_sid)
        .expect("A is a root blocker");
    let mut kids: Vec<i64> = root.children.iter().map(|c| c.session.sid).collect();
    kids.sort_unstable();
    let mut expected = vec![b_sid, c_sid];
    expected.sort_unstable();
    assert_eq!(kids, expected);
    let waiter = &root.children[0].session;
    assert!(
        waiter.event.as_deref().unwrap_or("").starts_with("enq: TX"),
        "{waiter:?}"
    );
    assert!(
        waiter
            .wait_object
            .as_deref()
            .unwrap_or("")
            .ends_with(".ORUI_MON_LOCK"),
        "{waiter:?}"
    );
    assert!(waiter.wait_rowid.is_some());
    assert!(
        waiter
            .sql_text
            .as_deref()
            .unwrap_or("")
            .contains("orui_mon_lock")
    );

    // Details of a waiter: the lock request is visible.
    let detail = queries::session_detail(&conn, waiter.sid, waiter.serial);
    assert!(detail.errors.is_empty(), "{:?}", detail.errors);
    assert!(
        detail
            .locks
            .iter()
            .any(|l| l.lock_type == "TX" && l.mode_requested != "None"),
        "{:?}",
        detail.locks
    );
    assert!(
        detail
            .sql_text
            .as_deref()
            .unwrap_or("")
            .contains("orui_mon_lock")
    );
    assert!(!detail.plan.is_empty());
    assert!(!detail.stats.is_empty());
    // And the blocker holds the TM lock on the table.
    let a_locks = queries::locks(&conn, a_sid).unwrap();
    assert!(a_locks.iter().any(|l| l.blocking), "{a_locks:?}");

    // Cancel C's statement: its update fails with ORA-01013.
    let c_serial = queries::sessions(&conn, SessionFilter::default())
        .unwrap()
        .into_iter()
        .find(|s| s.sid == c_sid)
        .unwrap()
        .serial;
    queries::cancel_sql(&conn, c_sid, c_serial).unwrap();
    c_done.send(()).unwrap();
    assert_eq!(c_thread.join().unwrap(), Err(1013));

    // Releasing A lets B through; then B is no longer blocked.
    a.rollback().unwrap();
    wait_for("B to get the lock", || {
        queries::sessions(&conn, SessionFilter::default())
            .unwrap()
            .iter()
            .all(|s| s.blocking_session.is_none() || s.sid != b_sid)
    });
    b_done.send(()).unwrap();
    assert_eq!(b_thread.join().unwrap(), Ok(()));

    // Kill an idle session.
    let d = open();
    let d_sid = sid(&d);
    let d_serial = queries::sessions(&conn, SessionFilter::default())
        .unwrap()
        .into_iter()
        .find(|s| s.sid == d_sid)
        .unwrap()
        .serial;
    queries::kill_session(&conn, d_sid, d_serial, true).unwrap();
    assert!(d.query_row_as::<i64>("select 1 from dual", &[]).is_err());

    drop(a);
    conn.execute("drop table orui_mon_lock purge", &[]).unwrap();
}

#[test]
fn trace_round_trip() {
    let Some(conn) = dba() else { return };
    let traced = {
        let spec = std::env::var("ORUI_TEST_DB_DBA").unwrap();
        let (cred, db) = spec.rsplit_once('@').unwrap();
        let (u, p) = cred.split_once('/').unwrap();
        Connection::connect(u, p, db).unwrap()
    };
    let traced_sid = sid(&traced);
    queries::trace_self(&traced, true).unwrap();
    for i in 0..3 {
        let n: i64 = traced
            .query_row_as(
                "select /* orui_trace_marker */ count(*) from all_objects where object_id < :1",
                &[&(1000 + i)],
            )
            .unwrap();
        assert!(n > 0);
    }
    traced
        .execute("begin null; /* orui_trace_plsql */ end;", &[])
        .unwrap();
    queries::trace_self(&traced, false).unwrap();

    let (dir, file) = queries::session_trace_file(&conn, traced_sid)
        .unwrap()
        .expect("trace file");
    // The file shows up in the list and can be read through the database.
    let files = queries::trace_files(&conn, &file, 5).unwrap();
    assert!(files.iter().any(|f| f.name == file), "{files:?}");
    let text = queries::read_trace_file(&conn, &dir, &file).unwrap();
    assert!(text.contains("PARSING IN CURSOR"));

    if let Ok(path) = std::env::var("ORUI_WRITE_TRACE_FIXTURE") {
        std::fs::write(path, &text).unwrap();
    }

    let report = tkprof::parse(&text);
    let marker = report
        .statements
        .iter()
        .find(|s| s.text.contains("orui_trace_marker"))
        .expect("marker statement");
    assert_eq!(marker.call(CallKind::Execute).count, 3);
    assert_eq!(marker.call(CallKind::Fetch).rows, 3);
    assert!(marker.total().query > 0);
    assert_eq!(marker.binds.first().map(String::as_str), Some("1000"));
    assert!(!marker.row_source.is_empty());
    assert!(
        report
            .statements
            .iter()
            .any(|s| s.text.contains("orui_trace_plsql"))
    );
    assert!(
        report
            .to_text(tkprof::SortKey::TotalElapsed)
            .contains("orui_trace_marker")
    );
}

#[test]
fn awr_and_ash() {
    let Some(conn) = dba() else { return };
    let caps = queries::capabilities(&conn).unwrap();
    if caps.awr().is_err() {
        eprintln!("AWR not available: {:?}", caps.awr());
        return;
    }
    let before = queries::awr_snapshots(&conn, 10).unwrap();
    let id = queries::awr_create_snapshot(&conn).unwrap();
    let snaps = queries::awr_snapshots(&conn, 10).unwrap();
    assert!(
        snaps.len() > before.len() || snaps.first().map(|s| s.snap_id) == Some(id),
        "{snaps:?}"
    );
    if snaps.len() >= 2 {
        let (end, begin) = (&snaps[0], &snaps[1]);
        let text = queries::awr_report(
            &conn,
            end.dbid,
            end.instance,
            begin.snap_id,
            end.snap_id,
            false,
        )
        .unwrap();
        assert!(
            text.to_uppercase().contains("WORKLOAD REPOSITORY"),
            "{}",
            &text[..text.len().min(500)]
        );
        let html = queries::awr_report(
            &conn,
            end.dbid,
            end.instance,
            begin.snap_id,
            end.snap_id,
            true,
        )
        .unwrap();
        assert!(html.to_lowercase().contains("<html"));
    }
    let ash = queries::ash_report(&conn, 30, false).unwrap();
    assert!(
        ash.to_uppercase().contains("ASH REPORT"),
        "{}",
        &ash[..ash.len().min(500)]
    );
}
