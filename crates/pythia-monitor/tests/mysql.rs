//! The MySQL dashboard's data against a real server.
//!
//! Skipped unless `PYTHIA_TEST_MYSQL` is set (`user/password@host:port/database`,
//! see pythia-db's MySQL tests). With `PYTHIA_TEST_MYSQL_DBA` (e.g.
//! `root/mysql@localhost:3306/pythia` for the docker-compose service) it
//! also checks what needs privileges: transactions, lock waits, top
//! statements.

use std::sync::{Mutex, mpsc};
use std::time::Duration;

use pythia_db::{ConnectParams, DbEvent, DbKind, DbRequest, Session};
use pythia_monitor::insights::Severity;
use pythia_monitor::mysql::{self, MySqlSnapshot};

/// The tests that make lock waits, one at a time: each looks at the
/// server's lock waits.
static LOCK_WAITS: Mutex<()> = Mutex::new(());

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

    /// Runs a task on the session's client and waits for its result.
    fn run<T: Send + 'static>(
        &mut self,
        task: impl FnOnce(&mut dyn pythia_db::Client) -> Result<T, pythia_db::DbError> + Send + 'static,
    ) -> Result<T, pythia_db::DbError> {
        self.session.send(DbRequest::Run {
            id: 3,
            task: Box::new(move |c| task(c).map(|v| Box::new(v) as Box<dyn std::any::Any + Send>)),
        });
        loop {
            if let DbEvent::TaskDone { result, .. } = self.next() {
                return result.map(|any| *any.downcast::<T>().expect("the task's result"));
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
    let _serial = LOCK_WAITS.lock().unwrap_or_else(|e| e.into_inner());
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

/// A thread's details (#87): the statement it runs and its plan, the lock
/// it waits for and whose; the transaction of the one it waits for; then
/// killing the query, and the connection. On MariaDB (#146): no plan while
/// it waits for a row lock.
#[test]
fn thread_details_and_kill() {
    let (Some(app), Some(dba)) = (params("PYTHIA_TEST_MYSQL"), params("PYTHIA_TEST_MYSQL_DBA"))
    else {
        return;
    };
    let _serial = LOCK_WAITS.lock().unwrap_or_else(|e| e.into_inner());
    let mut setup = Harness::connect(app.clone());
    let mariadb = setup.scalar("select version()").contains("MariaDB");
    setup.ok("create table if not exists pythia_mon_threads (id int primary key, v int)");
    setup.ok("delete from pythia_mon_threads");
    setup.ok("insert into pythia_mon_threads values (1, 0)");
    setup.ok("commit");

    let mut a = Harness::connect(app.clone());
    let mut b = Harness::connect(app);
    let a_id: i64 = a.scalar("select connection_id()").parse().unwrap();
    let b_id: i64 = b.scalar("select connection_id()").parse().unwrap();
    a.ok("update pythia_mon_threads set v = 1 where id = 1");
    b.send("update pythia_mon_threads set v = 2 where id = 1");

    let mut monitor = Harness::connect(dba);
    let mut d = monitor
        .run(move |c| Ok(mysql::thread_detail(c, b_id)))
        .unwrap();
    for _ in 0..50 {
        if d.waits_for.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        d = monitor
            .run(move |c| Ok(mysql::thread_detail(c, b_id)))
            .unwrap();
    }
    eprintln!("{d:#?}");
    if mariadb {
        assert!(
            matches!(d.errors.as_slice(), [("Plan", e)] if e.contains("row lock")),
            "{:?}",
            d.errors
        );
        assert_eq!(d.plan, None);
    } else {
        assert!(d.errors.is_empty(), "{:?}", d.errors);
        let plan = d.plan.as_deref().expect("EXPLAIN FOR CONNECTION");
        assert!(plan.contains("pythia_mon_threads"), "{plan}");
    }
    assert_eq!(d.thread.as_ref().unwrap().command, "Query");
    let statement = d.statement.as_ref().expect("its statement");
    assert!(
        statement.running && statement.text.contains("set v = 2"),
        "{statement:?}"
    );
    assert_eq!(d.waits_for, Some(a_id));
    assert!(
        d.locks.iter().any(|l| l.status == "WAITING"),
        "{:?}",
        d.locks
    );
    assert!(!d.connection.is_empty());

    let held = monitor
        .run(move |c| Ok(mysql::thread_detail(c, a_id)))
        .unwrap();
    let t = held.transaction.as_ref().expect("A's transaction");
    assert!(t.rows_locked >= 1 && t.rows_modified == 1, "{t:?}");
    assert!(held.locks.iter().any(|l| l.status == "GRANTED"));
    assert!(
        held.history.iter().any(|s| s.text.contains("set v = 1")),
        "{:?}",
        held.history
    );

    // Kill B's query: its update ends with "Query execution was
    // interrupted"; then A's connection.
    monitor.run(move |c| mysql::kill(c, b_id, true)).unwrap();
    loop {
        match b.next() {
            DbEvent::Failed { error, .. } => {
                assert_eq!(error.code, Some(1317), "{error}");
                break;
            }
            DbEvent::Transaction { .. } => panic!("B's update went through"),
            _ => {}
        }
    }
    monitor.run(move |c| mysql::kill(c, a_id, false)).unwrap();
    let gone = monitor
        .run(move |c| Ok(mysql::thread_detail(c, a_id)))
        .unwrap();
    assert!(gone.thread.is_none() || gone.thread.unwrap().command == "Killed");
    b.session.send(DbRequest::Rollback);
    setup.ok("drop table pythia_mon_threads");
}

/// The plan of a running statement: on MariaDB from `SHOW EXPLAIN`, which
/// before 10.9 is all it has (#146).
#[test]
fn running_statement_plan() {
    let (Some(app), Some(dba)) = (params("PYTHIA_TEST_MYSQL"), params("PYTHIA_TEST_MYSQL_DBA"))
    else {
        return;
    };
    let mut setup = Harness::connect(app.clone());
    setup.ok("drop table if exists pythia_mon_plan");
    setup.ok("create table pythia_mon_plan (n int)");
    setup.ok("insert into pythia_mon_plan values (1)");
    // 1024 rows: their cross join runs until it is killed.
    for _ in 0..10 {
        setup.ok("insert into pythia_mon_plan select n from pythia_mon_plan");
    }
    setup.ok("commit");

    let mut b = Harness::connect(app);
    let b_id: i64 = b.scalar("select connection_id()").parse().unwrap();
    b.send(
        "select sum(plan_a.n + plan_d.n) from pythia_mon_plan plan_a, pythia_mon_plan plan_b, \
                pythia_mon_plan plan_c, pythia_mon_plan plan_d",
    );
    let mut monitor = Harness::connect(dba);
    let mut d = monitor
        .run(move |c| Ok(mysql::thread_detail(c, b_id)))
        .unwrap();
    for _ in 0..50 {
        if d.plan.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        d = monitor
            .run(move |c| Ok(mysql::thread_detail(c, b_id)))
            .unwrap();
    }
    eprintln!("{d:#?}");
    let plan = d.plan.as_deref().expect("the plan");
    assert!(plan.contains("plan_d"), "{plan}");
    assert!(d.errors.is_empty(), "{:?}", d.errors);

    monitor.run(move |c| mysql::kill(c, b_id, true)).unwrap();
    loop {
        match b.next() {
            DbEvent::Failed { .. } => break,
            DbEvent::Transaction { .. } => panic!("the cross join ended by itself"),
            _ => {}
        }
    }
    setup.ok("drop table pythia_mon_plan");
}

/// The session list (#131): idle sessions too, an open transaction, who
/// waits for whom, and the blocking tree; without PROCESS, the user's own
/// sessions and a note why.
#[test]
fn session_list() {
    let (Some(app), Some(dba)) = (params("PYTHIA_TEST_MYSQL"), params("PYTHIA_TEST_MYSQL_DBA"))
    else {
        return;
    };
    let _serial = LOCK_WAITS.lock().unwrap_or_else(|e| e.into_inner());
    let mut setup = Harness::connect(app.clone());
    setup.ok("create table if not exists pythia_mon_list (id int primary key, v int)");
    setup.ok("delete from pythia_mon_list");
    setup.ok("insert into pythia_mon_list values (1, 0)");
    setup.ok("commit");

    // A holds the row and goes idle; B waits for it.
    let mut a = Harness::connect(app.clone());
    let mut b = Harness::connect(app.clone());
    let a_id: i64 = a.scalar("select connection_id()").parse().unwrap();
    let b_id: i64 = b.scalar("select connection_id()").parse().unwrap();
    let setup_id: i64 = setup.scalar("select connection_id()").parse().unwrap();
    a.ok("update pythia_mon_list set v = 1 where id = 1");
    b.send("update pythia_mon_list set v = 2 where id = 1");

    let mut monitor = Harness::connect(dba);
    let mut list = monitor.run(|c| mysql::connections(c, false)).unwrap();
    for _ in 0..50 {
        if list.rows.iter().any(|t| t.blocked_by.is_some()) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
        list = monitor.run(|c| mysql::connections(c, false)).unwrap();
    }
    assert!(list.notes.is_empty(), "{:?}", list.notes);
    let find = |id: i64| list.rows.iter().find(|t| t.id == id).expect("listed");
    let (ta, tb) = (find(a_id), find(b_id));
    assert!(!ta.active && ta.status == "Sleep", "{ta:?}");
    assert!(ta.transaction_secs.is_some(), "A's open transaction");
    assert!(ta.idle_in_transaction());
    assert!(tb.active && tb.blocked_by == Some(a_id), "{tb:?}");
    assert!(
        tb.query.as_deref().is_some_and(|q| q.contains("set v = 2")),
        "{tb:?}"
    );
    assert!(!find(setup_id).active, "idle sessions are listed");
    assert!(list.rows.iter().all(|t| !t.background));
    let tree = pythia_monitor::connections::blocking_tree(&list.rows);
    assert_eq!(tree[0].connection.id, a_id);
    assert_eq!(tree[0].children[0].connection.id, b_id);

    // Without PROCESS: the user's own sessions, and why.
    let mut own = Harness::connect(app);
    let mine = own.run(|c| mysql::connections(c, false)).unwrap();
    let privileged = own.scalar(
        "select count(*) from information_schema.user_privileges \
          where privilege_type = 'PROCESS'",
    ) != "0";
    if !privileged {
        assert!(
            mine.notes.iter().any(|n| n.contains("PROCESS privilege")),
            "{:?}",
            mine.notes
        );
        assert!(mine.rows.iter().any(|t| t.id == a_id));
    }

    // The server's own threads only when asked for.
    let all = monitor.run(|c| mysql::connections(c, true)).unwrap();
    assert!(all.rows.len() >= list.rows.len());

    a.session.send(DbRequest::Rollback);
    b.settle("the waiting update");
    b.session.send(DbRequest::Rollback);
    setup.ok("drop table pythia_mon_list");
}
