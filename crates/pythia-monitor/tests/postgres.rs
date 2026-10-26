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
