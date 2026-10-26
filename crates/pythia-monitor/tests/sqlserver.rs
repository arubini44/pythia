//! The SQL Server dashboard's data against a real server.
//!
//! Skipped unless `PYTHIA_TEST_MSSQL` is set (`user/password@host:port/database?trust`,
//! see pythia-db's SQL Server tests): a login without VIEW SERVER STATE, as
//! the docker-compose service's `pythia`. With `PYTHIA_TEST_MSSQL_DBA`
//! (e.g. `sa/Pythia-2026@localhost:1433/pythia?trust`) it also checks what
//! needs the privilege: counters, waits, blocking, transactions, top
//! statements.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use pythia_db::{ConnectParams, DbEvent, DbKind, DbRequest, Session};
use pythia_monitor::insights::Severity;
use pythia_monitor::sqlserver::{self, Counters, MsSnapshot};

fn params(var: &str) -> Option<ConnectParams> {
    let spec = std::env::var(var).ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (username, password) = credentials.split_once('/')?;
    Some(ConnectParams {
        kind: DbKind::SqlServer,
        username: username.into(),
        password: password.into(),
        connect_string: connect_string.into(),
        privilege: Default::default(),
        init_sql: Vec::new(),
        ssh: None,
    })
}

struct Harness {
    session: Session,
    wake: mpsc::Receiver<()>,
}

impl Harness {
    fn connect(var: &str) -> Option<Self> {
        let params = params(var)?;
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

    /// Sends a statement without waiting for it.
    fn send(&self, sql: &str) {
        self.session.send(DbRequest::Execute {
            id: 1,
            sql: sql.into(),
            max_rows: 100,
            binds: vec![],
        });
    }

    /// Runs a statement and waits for the end of its events.
    fn ok(&mut self, sql: &str) {
        self.send(sql);
        self.settle(sql);
    }

    fn settle(&mut self, sql: &str) {
        loop {
            match self.next() {
                DbEvent::Transaction { .. } => return,
                DbEvent::Failed { error, .. } => panic!("{sql}: {error}"),
                _ => {}
            }
        }
    }

    fn scalar(&mut self, sql: &str) -> String {
        self.send(sql);
        let mut value = None;
        loop {
            match self.next() {
                DbEvent::Rows { rows, .. } if value.is_none() => {
                    value = rows[0][0].clone();
                }
                DbEvent::Transaction { .. } => return value.expect("a value"),
                DbEvent::Failed { error, .. } => panic!("{sql}: {error}"),
                _ => {}
            }
        }
    }

    fn rollback(&mut self) {
        self.session.send(DbRequest::Rollback);
        self.settle("rollback");
    }

    /// The dashboard's snapshot, read on this session's client.
    fn snapshot(&mut self, storage: bool) -> MsSnapshot {
        self.session.send(DbRequest::Run {
            id: 3,
            task: Box::new(move |c| {
                Ok(Box::new(sqlserver::snapshot(c, storage)) as Box<dyn std::any::Any + Send>)
            }),
        });
        loop {
            if let DbEvent::TaskDone { result, .. } = self.next() {
                return *result
                    .expect("the snapshot never fails")
                    .downcast::<MsSnapshot>()
                    .expect("a snapshot");
            }
        }
    }
}

/// With VIEW SERVER STATE every section is there, and two snapshots give
/// rates.
#[test]
fn snapshot_with_server_state() {
    let Some(mut dba) = Harness::connect("PYTHIA_TEST_MSSQL_DBA") else {
        eprintln!("PYTHIA_TEST_MSSQL_DBA not set, skipping");
        return;
    };
    let first = dba.snapshot(true);
    assert!(first.errors.is_empty(), "{:?}", first.errors);
    assert!(!first.own_sessions_only);
    let server = first.server.as_ref().expect("server info");
    assert!(server.product.starts_with("SQL Server"), "{server:?}");
    assert!(server.uptime_secs > 0.0 && server.cpus.is_some_and(|n| n > 0));
    let gauges = first.gauges.expect("gauges");
    assert!(gauges.user_connections >= 1 && gauges.page_life_expectancy > 0.0);
    assert!(!first.waits.is_empty(), "wait statistics");
    assert!(first.open_transactions.is_some());
    let storage = first.storage.as_ref().expect("storage");
    assert!(!storage.current_database_only);
    assert!(
        storage
            .databases
            .iter()
            .any(|d| d.name == "pythia" && d.log_bytes > 0.0),
        "{:?}",
        storage.databases
    );
    assert!(first.log_used.iter().any(|(name, _)| name == "pythia"));

    // Work between the two: batches, transactions.
    for _ in 0..5 {
        dba.scalar("select count(*) from sys.objects");
    }
    let second = dba.snapshot(false);
    assert!(second.storage.is_none());
    let rates = Counters::per_second(
        &first.counters.expect("counters"),
        &second.counters.expect("counters"),
        1.0,
    );
    assert!(rates.batch_requests >= 5.0, "{rates:?}");
}

/// A session holding a row lock in an open transaction blocks another:
/// the dashboard names the blocker, idle, and the waiting statement.
#[test]
fn blocking_chain() {
    let (Some(mut dba), Some(mut a), Some(mut b)) = (
        Harness::connect("PYTHIA_TEST_MSSQL_DBA"),
        Harness::connect("PYTHIA_TEST_MSSQL"),
        Harness::connect("PYTHIA_TEST_MSSQL"),
    ) else {
        eprintln!("PYTHIA_TEST_MSSQL(_DBA) not set, skipping");
        return;
    };
    a.ok("drop table if exists dash_block_t");
    a.ok("create table dash_block_t (id int primary key, v int)");
    a.ok("insert into dash_block_t values (1, 0)");
    a.session.send(DbRequest::Commit);
    a.settle("commit");
    let blocker: i64 = a.scalar("select @@spid").parse().unwrap();
    let waiting: i64 = b.scalar("select @@spid").parse().unwrap();

    // `a` updates without committing; `b`'s update waits for it.
    a.ok("update dash_block_t set v = 1 where id = 1");
    b.send("update dash_block_t set v = 2 where id = 1");
    let start = Instant::now();
    let snap = loop {
        let snap = dba.snapshot(false);
        if snap.blocked.iter().any(|w| w.session == waiting) {
            break snap;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "no blocking seen: {:?}",
            snap.blocked
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let w = snap.blocked.iter().find(|w| w.session == waiting).unwrap();
    assert_eq!(w.blocker, blocker);
    assert!(w.blocker_idle, "{w:?}");
    assert!(w.blocker_statement.is_none() && w.blocker_last.is_some());
    assert!(
        w.wait_type
            .as_deref()
            .is_some_and(|t| t.starts_with("LCK_M_")),
        "{w:?}"
    );
    // Parameterized by the server: `UPDATE [dash_block_t] set [v] = @1 …`.
    assert!(
        w.statement
            .as_deref()
            .is_some_and(|s| s.contains("dash_block_t")),
        "{w:?}"
    );
    assert!(
        snap.running
            .iter()
            .any(|r| r.session == waiting && r.blocked_by == Some(blocker))
    );
    let open = snap
        .transactions
        .iter()
        .find(|t| t.session == blocker)
        .expect("the blocker's transaction");
    assert!(open.idle && open.log_bytes > 0.0, "{open:?}");
    let found = sqlserver::insights(&snap, None);
    assert!(
        found.iter().any(|i| i.severity >= Severity::Warning
            && i.text
                .starts_with(&format!("Session {blocker} (pythia) blocks"))
            && i.text.contains("idle, likely an uncommitted transaction")),
        "{found:?}"
    );

    a.rollback();
    b.settle("the blocked update");
    b.rollback();
    a.ok("drop table dash_block_t");
}

/// Without VIEW SERVER STATE: the login's own sessions, the current
/// database's sizes, and one note instead of an error per section.
#[test]
fn snapshot_without_server_state() {
    let Some(mut h) = Harness::connect("PYTHIA_TEST_MSSQL") else {
        eprintln!("PYTHIA_TEST_MSSQL not set, skipping");
        return;
    };
    let granted = h.scalar("select has_perms_by_name(null, null, 'VIEW SERVER STATE')");
    if granted == "1" {
        eprintln!("the login has VIEW SERVER STATE, skipping");
        return;
    }
    let s = h.snapshot(true);
    assert!(s.own_sessions_only);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert!(s.counters.is_none() && s.waits.is_empty() && s.top_statements.is_empty());
    let server = s
        .server
        .as_ref()
        .expect("server info without the privilege");
    assert!(
        server.uptime_secs > 0.0 && server.cpus.is_none(),
        "{server:?}"
    );
    let storage = s.storage.as_ref().expect("the current database's sizes");
    assert!(storage.current_database_only);
    assert_eq!(storage.databases.len(), 1);
    assert_eq!(storage.databases[0].name, "pythia");
}
