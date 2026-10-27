//! The PostgreSQL dashboard's data against a real server.
//!
//! Skipped unless `PYTHIA_TEST_POSTGRES` is set (`user/password@host:port/database`,
//! a superuser, e.g. `pythia/pythia@localhost:5432/pythia` for the
//! docker-compose service, which loads pg_stat_statements).

use std::sync::mpsc;
use std::time::{Duration, Instant};

use pythia_db::{ConnectParams, DbEvent, DbKind, DbRequest, Session};
use pythia_monitor::insights::Severity;
use pythia_monitor::postgres::{self, PgSnapshot};

fn params(user: Option<(&str, &str)>) -> Option<ConnectParams> {
    let spec = std::env::var("PYTHIA_TEST_POSTGRES").ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (username, password) = match user {
        Some(user) => user,
        None => credentials.split_once('/')?,
    };
    Some(ConnectParams {
        kind: DbKind::Postgres,
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
            id: 4,
            task: Box::new(move |c| task(c).map(|v| Box::new(v) as Box<dyn std::any::Any + Send>)),
        });
        loop {
            if let DbEvent::TaskDone { result, .. } = self.next() {
                return result.map(|any| *any.downcast::<T>().expect("the task's result"));
            }
        }
    }

    fn snapshot(&mut self, storage: bool) -> PgSnapshot {
        self.session.send(DbRequest::Run {
            id: 3,
            task: Box::new(move |c| {
                Ok(Box::new(postgres::snapshot(c, storage)) as Box<dyn std::any::Any + Send>)
            }),
        });
        loop {
            if let DbEvent::TaskDone { result, .. } = self.next() {
                return *result
                    .expect("snapshot task")
                    .downcast::<PgSnapshot>()
                    .expect("a snapshot");
            }
        }
    }
}

/// Everything a superuser sees, with pg_stat_statements loaded.
#[test]
fn dashboard_snapshot() {
    let Some(p) = params(None) else {
        eprintln!("PYTHIA_TEST_POSTGRES not set, skipping");
        return;
    };
    let mut h = Harness::connect(p);
    h.ok("create extension if not exists pg_stat_statements");
    h.ok("select count(*) from pg_class");
    let first = h.snapshot(true);
    let s = h.snapshot(true);
    assert!(s.errors.is_empty(), "{:?}", s.errors);
    let server = s.server.as_ref().expect("server");
    assert!(server.sees_all && !server.standby);
    assert!(server.max_connections > 0 && server.uptime_secs > 0.0);
    assert!(server.shared_buffers > 0.0, "{server:?}");
    let counters = s.counters.expect("counters");
    assert!(counters.commits >= first.counters.unwrap().commits);
    assert!(counters.blocks_hit > 0.0 && counters.wal_bytes > 0.0);
    assert!(s.connections.unwrap().max > 0);
    let storage = s.storage.as_ref().expect("storage");
    assert!(
        storage
            .databases
            .iter()
            .any(|d| d.name == "pythia" && d.bytes.is_some())
    );
    assert!(s.oldest_xid.as_ref().is_some_and(|(_, age)| *age > 0));
    assert!(s.statements_missing.is_none());
    assert!(!s.top_statements.is_empty());
    // The dashboard's own lookups and Pythia's setup aren't the load it
    // shows.
    assert!(
        s.top_statements.iter().all(|t| {
            !t.text.contains("pg_stat_activity") && !t.text.contains("inet_server_addr")
        }),
        "{:?}",
        s.top_statements
    );
}

/// A session idle in a transaction blocks another one's update: both show,
/// and the finding names the blocker.
#[test]
fn lock_wait_and_idle_transaction() {
    let Some(p) = params(None) else {
        return;
    };
    let mut setup = Harness::connect(p.clone());
    setup.ok("create table if not exists pythia_mon_locks (id int primary key, v int)");
    setup.ok("insert into pythia_mon_locks values (1, 0) on conflict do nothing");
    setup.ok("commit");

    let mut holder = Harness::connect(p.clone());
    let holder_pid: i64 = holder.scalar("select pg_backend_pid()").parse().unwrap();
    // Without autocommit, the update opens a transaction that stays open.
    holder.ok("update pythia_mon_locks set v = v + 1 where id = 1");
    let mut waiter = Harness::connect(p.clone());
    let waiter_pid: i64 = waiter.scalar("select pg_backend_pid()").parse().unwrap();
    waiter.send("update pythia_mon_locks set v = v + 1 where id = 1");

    let mut dashboard = Harness::connect(p);
    let start = Instant::now();
    let snap = loop {
        let snap = dashboard.snapshot(false);
        if snap.lock_waits.iter().any(|w| w.waiting_pid == waiter_pid) {
            break snap;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "no lock wait");
        std::thread::sleep(Duration::from_millis(200));
    };
    let wait = snap
        .lock_waits
        .iter()
        .find(|w| w.waiting_pid == waiter_pid)
        .unwrap();
    assert_eq!(wait.blocking_pid, holder_pid);
    assert_eq!(wait.blocking_state.as_deref(), Some("idle in transaction"));
    assert!(
        wait.relation
            .as_deref()
            .is_some_and(|r| r.contains("pythia_mon_locks")),
        "{wait:?}"
    );
    assert!(
        wait.waiting_query
            .as_deref()
            .is_some_and(|q| q.contains("update pythia_mon_locks"))
    );
    assert!(snap.connections.unwrap().waiting >= 1);
    assert!(snap.activity.iter().any(|(kind, _)| kind == "Lock"));
    let holder_row = snap
        .transactions
        .iter()
        .find(|t| t.pid == holder_pid)
        .expect("the holder's transaction");
    assert!(holder_row.idle_in_transaction());
    let finding = postgres::insights(&snap, None)
        .into_iter()
        .find(|i| i.text.starts_with(&format!("Session {holder_pid} ")))
        .expect("a finding for the blocker");
    assert!(
        finding.text.contains("idle in transaction"),
        "{}",
        finding.text
    );
    assert!(finding.severity >= Severity::Warning);

    holder.ok("rollback");
    loop {
        if let DbEvent::Transaction { .. } = waiter.next() {
            break;
        }
    }
    waiter.ok("rollback");
    setup.ok("drop table pythia_mon_locks");
    setup.ok("commit");
}

/// A role without pg_read_all_stats: the dashboard works, other users'
/// statements are hidden and it says so.
#[test]
fn without_read_all_stats() {
    let Some(p) = params(None) else {
        return;
    };
    let mut admin = Harness::connect(p);
    admin.ok("do $$ begin \
           if not exists (select from pg_roles where rolname = 'pythia_mon_plain') then \
             create role pythia_mon_plain login password 'plain'; \
           end if; \
         end $$");
    admin.ok("commit");
    // The admin's session runs something the plain role must not see.
    admin.ok("select 'secret' from pg_class limit 1");
    let mut plain = Harness::connect(params(Some(("pythia_mon_plain", "plain"))).unwrap());
    let s = plain.snapshot(true);
    let server = s.server.as_ref().expect("server");
    assert!(!server.sees_all);
    assert!(s.own_sessions_only, "another user's statement was shown");
    assert!(s.connections.is_some() && s.counters.is_some());
    assert!(s.storage.is_some(), "{:?}", s.errors);
    drop(plain);
    // Its session may take a moment to end on the server.
    let start = Instant::now();
    while admin.scalar("select count(*) from pg_stat_activity where usename = 'pythia_mon_plain'")
        != "0"
    {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(100));
    }
    admin.ok("drop role pythia_mon_plain");
    admin.ok("commit");
}

/// A blocking pair in its details: each side shows the lock and the other.
/// Cancelling the waiter's statement ends it; terminating the holder ends
/// its session and rolls back its update (#96).
#[test]
fn backend_details_cancel_and_terminate() {
    let Some(p) = params(None) else {
        return;
    };
    let mut setup = Harness::connect(p.clone());
    setup.ok("drop table if exists pythia_det_locks");
    setup.ok("create table pythia_det_locks (id int primary key, v int)");
    setup.ok("insert into pythia_det_locks values (1, 0)");
    setup.ok("commit");
    let mut holder = Harness::connect(p.clone());
    let holder_pid: i64 = holder.scalar("select pg_backend_pid()").parse().unwrap();
    holder.ok("update pythia_det_locks set v = v + 1 where id = 1");
    let mut waiter = Harness::connect(p.clone());
    let waiter_pid: i64 = waiter.scalar("select pg_backend_pid()").parse().unwrap();
    waiter.send("update pythia_det_locks set v = v + 1 where id = 1");

    let mut monitor = Harness::connect(p);
    let detail = |m: &mut Harness, pid: i64| {
        m.run(move |c| Ok(postgres::backend_detail(c, pid)))
            .unwrap()
    };
    let start = Instant::now();
    let waiting = loop {
        let d = detail(&mut monitor, waiter_pid);
        if !d.blocked_by.is_empty() {
            break d;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "no lock wait");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(waiting.errors.is_empty(), "{:?}", waiting.errors);
    assert_eq!(waiting.blocked_by, [holder_pid]);
    assert!(
        waiting.locks.iter().any(|l| !l.granted),
        "{:?}",
        waiting.locks
    );
    let backend = waiting.backend.as_ref().expect("the waiter");
    assert_eq!(backend.state.as_deref(), Some("active"));
    assert!(
        backend
            .wait
            .as_deref()
            .is_some_and(|w| w.starts_with("Lock:"))
    );
    assert!(
        waiting
            .connection
            .iter()
            .any(|(name, _)| name == "Encryption")
    );

    let holding = detail(&mut monitor, holder_pid);
    assert_eq!(holding.blocks, [waiter_pid]);
    let transaction = holding
        .transaction
        .as_ref()
        .expect("the holder's transaction");
    assert!(transaction.xid.is_some(), "it wrote: {transaction:?}");
    assert!(
        holding.locks.iter().any(|l| l.granted
            && l.relation
                .as_deref()
                .is_some_and(|r| r.contains("pythia_det_locks"))),
        "{:?}",
        holding.locks
    );

    // Cancelled, the waiter's update fails (57014); its session stays.
    monitor
        .run(move |c| postgres::signal(c, waiter_pid, false))
        .unwrap();
    let mut failed = None;
    loop {
        match waiter.next() {
            DbEvent::Failed { error, .. } => failed = Some(error.to_string()),
            DbEvent::Transaction { .. } => break,
            _ => {}
        }
    }
    let failed = failed.expect("the update was cancelled");
    assert!(failed.contains("57014"), "{failed}");
    waiter.ok("rollback");
    assert_eq!(waiter.scalar("select 1"), "1");

    // Terminated, the holder's session ends and its update is undone.
    monitor
        .run(move |c| postgres::signal(c, holder_pid, true))
        .unwrap();
    let start = Instant::now();
    while detail(&mut monitor, holder_pid).backend.is_some() {
        assert!(start.elapsed() < Duration::from_secs(10), "still there");
        std::thread::sleep(Duration::from_millis(100));
    }
    let gone = monitor
        .run(move |c| postgres::signal(c, holder_pid, false))
        .unwrap_err();
    assert!(
        gone.message.contains("wasn't signalled"),
        "{}",
        gone.message
    );
    assert_eq!(
        setup.scalar("select v from pythia_det_locks where id = 1"),
        "0"
    );
    setup.ok("drop table pythia_det_locks");
    setup.ok("commit");
}

/// The session list (#131): idle sessions too, an idle transaction, who
/// waits for whom and since when, the blocking tree, the server's own
/// processes only when asked; without pg_read_all_stats, a note.
#[test]
fn session_list() {
    let Some(p) = params(None) else {
        return;
    };
    let mut setup = Harness::connect(p.clone());
    setup.ok("drop table if exists pythia_mon_list");
    setup.ok("create table pythia_mon_list (id int primary key, v int)");
    setup.ok("insert into pythia_mon_list values (1, 0)");
    setup.ok("commit");
    let setup_pid: i64 = setup.scalar("select pg_backend_pid()").parse().unwrap();
    setup.ok("commit");

    let mut holder = Harness::connect(p.clone());
    let holder_pid: i64 = holder.scalar("select pg_backend_pid()").parse().unwrap();
    holder.ok("update pythia_mon_list set v = v + 1 where id = 1");
    let mut waiter = Harness::connect(p.clone());
    let waiter_pid: i64 = waiter.scalar("select pg_backend_pid()").parse().unwrap();
    waiter.send("update pythia_mon_list set v = v + 1 where id = 1");

    let mut monitor = Harness::connect(p.clone());
    let start = Instant::now();
    let list = loop {
        let list = monitor.run(|c| postgres::connections(c, false)).unwrap();
        if list
            .rows
            .iter()
            .any(|b| b.id == waiter_pid && b.blocked_by.is_some())
        {
            break list;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "no lock wait");
        std::thread::sleep(Duration::from_millis(200));
    };
    assert!(list.notes.is_empty(), "{:?}", list.notes);
    let find = |pid: i64| list.rows.iter().find(|b| b.id == pid).expect("listed");
    let (h, w) = (find(holder_pid), find(waiter_pid));
    assert_eq!(h.status, "idle in transaction");
    assert!(h.idle_in_transaction() && !h.active, "{h:?}");
    assert!(w.active && w.blocked_by == Some(holder_pid), "{w:?}");
    assert!(
        w.wait.as_deref().is_some_and(|e| e.starts_with("Lock:")),
        "{w:?}"
    );
    assert!(w.wait_secs >= 0.0 && w.wait_secs < 60.0, "{w:?}");
    assert!(
        w.query
            .as_deref()
            .is_some_and(|q| q.contains("pythia_mon_list"))
    );
    assert_eq!(find(setup_pid).status, "idle", "idle sessions are listed");
    assert!(list.rows.iter().all(|b| !b.background));
    let tree = pythia_monitor::connections::blocking_tree(&list.rows);
    let root = tree
        .iter()
        .find(|t| t.connection.id == holder_pid)
        .expect("the holder as a root");
    assert_eq!(root.children[0].connection.id, waiter_pid);

    let all = monitor.run(|c| postgres::connections(c, true)).unwrap();
    assert!(
        all.rows
            .iter()
            .any(|b| b.background && b.status == "checkpointer"),
        "{:?}",
        all.rows
    );

    // A plain role sees the sessions, not their statements, and why.
    monitor.ok("do $$ begin \
           if not exists (select from pg_roles where rolname = 'pythia_mon_list_plain') then \
             create role pythia_mon_list_plain login password 'plain'; \
           end if; \
         end $$");
    monitor.ok("commit");
    let mut plain = Harness::connect(params(Some(("pythia_mon_list_plain", "plain"))).unwrap());
    let seen = plain.run(|c| postgres::connections(c, false)).unwrap();
    assert!(
        seen.notes.iter().any(|n| n.contains("pg_read_all_stats")),
        "{:?}",
        seen.notes
    );
    let other = seen
        .rows
        .iter()
        .find(|b| b.id == waiter_pid)
        .expect("listed");
    assert_eq!(other.query, None);
    drop(plain);

    holder.ok("rollback");
    loop {
        if let DbEvent::Transaction { .. } = waiter.next() {
            break;
        }
    }
    waiter.ok("rollback");
    setup.ok("drop table pythia_mon_list");
    setup.ok("commit");
    let start = Instant::now();
    while monitor
        .scalar("select count(*) from pg_stat_activity where usename = 'pythia_mon_list_plain'")
        != "0"
    {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(100));
    }
    monitor.ok("drop role pythia_mon_list_plain");
    monitor.ok("commit");
}
