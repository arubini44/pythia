//! The MySQL dashboard's data against a real server.
//!
//! Skipped unless `PYTHIA_TEST_MYSQL` is set (`user/password@host:port/database`,
//! see pythia-db's MySQL tests). With `PYTHIA_TEST_MYSQL_DBA` (e.g.
//! `root/mysql@localhost:3306/pythia` for the docker-compose service) it
//! also checks what needs privileges: transactions, lock waits, top
//! statements.

use std::sync::mpsc;
use std::time::Duration;

use pythia_db::{ConnectParams, DbEvent, DbKind, DbRequest, Session};
use pythia_monitor::insights::Severity;
use pythia_monitor::mysql::{self, MySqlSnapshot};

fn params(var: &str) -> Option<ConnectParams> {
    let spec = std::env::var(var).ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (username, password) = credentials.split_once('/')?;
    Some(ConnectParams {
        kind: DbKind::MySql,
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
    fn connect(params: ConnectParams) -> Self {
        let (tx, wake) = mpsc::channel();
        let session = Session::connect(params, move || {
            let _ = tx.send(());
        });
        let mut h = Self { session, wake };
        match h.next() {
            DbEvent::Connected(_) => h,
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

    fn snapshot(&mut self, storage: bool) -> MySqlSnapshot {
        self.session.send(DbRequest::Run {
            id: 2,
            task: Box::new(move |c| Ok(Box::new(mysql::snapshot(c, storage)))),
        });
        loop {
            if let DbEvent::TaskDone { result, .. } = self.next() {
                return *result.expect("snapshot").downcast().expect("a snapshot");
            }
        }
    }
}

#[test]
fn as_an_application_user() {
    let Some(params) = params("PYTHIA_TEST_MYSQL") else {
        return;
    };
    let mut h = Harness::connect(params);
    h.ok("create table if not exists pythia_mon_sizes (id int primary key, v varchar(100))");
    let s = h.snapshot(true);

    let server = s.server.as_ref().expect("server info");
    assert!(
        !server.version.is_empty() && server.uptime_secs > 0.0,
        "{server:?}"
    );
    let counters = s.counters.expect("counters");
    assert!(counters.queries > 0.0, "{counters:?}");
    let connections = s.connections.expect("connections");
    assert!(connections.connected >= 1 && connections.max_connections > 0);
    let pool = s.buffer_pool.expect("buffer pool");
    assert!(pool.size_bytes > 0.0 && pool.pages_total > 0.0, "{pool:?}");
    let storage = s.storage.as_ref().expect("storage");
    assert!(
        storage.tables.iter().any(|t| t.name == "pythia_mon_sizes"),
        "{storage:?}"
    );
    assert!(storage.schemas.iter().any(|t| t.schema == "pythia"));
    assert!(
        h.snapshot(false).storage.is_none(),
        "storage only when asked"
    );

    // What needs privileges says which.
    let error = |name: &str| {
        s.errors
            .iter()
            .find(|(section, _)| *section == name)
            .map(|(_, message)| message.clone())
    };
    if error("Transactions").is_some() {
        assert!(s.own_threads_only);
        assert!(
            error("Transactions")
                .unwrap()
                .contains("needs the PROCESS privilege"),
            "{:?}",
            s.errors
        );
    }
    if let Some(top) = error("Top statements") {
        assert!(top.contains("SELECT on performance_schema"), "{top}");
    }
    assert!(error("Server status").is_none() && error("Threads").is_none());
    assert!(error("Replication").is_none(), "{:?}", s.errors);
    h.ok("drop table pythia_mon_sizes");
}

#[test]
fn lock_waits_and_top_statements() {
    let (Some(app), Some(dba)) = (params("PYTHIA_TEST_MYSQL"), params("PYTHIA_TEST_MYSQL_DBA"))
    else {
        return;
    };
    let mut setup = Harness::connect(app.clone());
    setup.ok("create table if not exists pythia_mon_locks (id int primary key, v int)");
    setup.ok("delete from pythia_mon_locks");
    setup.ok("insert into pythia_mon_locks values (1, 0)");
    setup.ok("commit");

    // A holds the row and goes idle; B waits for it.
    let mut a = Harness::connect(app.clone());
    let mut b = Harness::connect(app);
    let (a_id, b_id) = (
        a.scalar("select connection_id()"),
        b.scalar("select connection_id()"),
    );
    a.ok("update pythia_mon_locks set v = 1 where id = 1");
    b.send("update pythia_mon_locks set v = 2 where id = 1");

    let mut monitor = Harness::connect(dba);
    let mut s = monitor.snapshot(false);
    for _ in 0..50 {
        if !s.lock_waits.is_empty() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        s = monitor.snapshot(false);
    }
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    assert!(!s.own_threads_only);
    let wait = s.lock_waits.first().expect("a lock wait");
    assert_eq!(wait.blocking_thread.to_string(), a_id);
    assert_eq!(wait.waiting_thread.to_string(), b_id);
    assert_eq!(wait.blocking_query, None, "A is idle");
    assert!(
        wait.waiting_query
            .as_deref()
            .is_some_and(|q| q.contains("set v = 2")),
        "{wait:?}"
    );
    assert_eq!(wait.table.as_deref(), Some("pythia.pythia_mon_locks"));
    assert!(s.open_transactions.unwrap() >= 2);
    let holder = s
        .transactions
        .iter()
        .find(|t| t.thread.to_string() == a_id)
        .expect("A's transaction");
    assert!(holder.idle && holder.rows_locked >= 1, "{holder:?}");
    let blocked = mysql::insights(&s, None);
    assert!(
        blocked[0]
            .text
            .starts_with(&format!("Thread {a_id} blocks 1 transaction")),
        "{blocked:?}"
    );
    assert!(blocked[0].severity >= Severity::Warning);
    assert!(
        s.running.iter().any(|t| t.id.to_string() == b_id),
        "B runs its update: {:?}",
        s.running
    );

    a.session.send(DbRequest::Rollback);
    b.settle("the waiting update");
    b.session.send(DbRequest::Rollback);

    // The application's statements, most time first, without the
    // dashboard's own.
    let s = monitor.snapshot(false);
    let top = &s.top_statements;
    assert!(!top.is_empty());
    assert!(
        top.windows(2).all(|w| w[0].total_secs >= w[1].total_secs),
        "{top:?}"
    );
    assert!(top.iter().all(|t| t.executions > 0 && !t.digest.is_empty()));
    assert!(
        s.top_statements
            .iter()
            .all(|t| !t.text.to_lowercase().contains("information_schema")),
        "{:?}",
        s.top_statements
    );
    setup.ok("drop table pythia_mon_locks");
}
