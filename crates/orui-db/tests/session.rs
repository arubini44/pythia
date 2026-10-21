//! Integration tests against a real database.
//!
//! Skipped unless `ORUI_TEST_DB` is set to `user/password@connect_string`,
//! e.g. the Oracle Free container from `docker-compose.yml`:
//! `ORUI_TEST_DB=orui/orui@localhost:1521/FREEPDB1 cargo test -p orui-db`

use std::sync::mpsc;
use std::time::Duration;

use orui_config::Privilege;
use orui_db::{ConnectParams, DbEvent, DbRequest, Session};

fn params() -> Option<ConnectParams> {
    let spec = std::env::var("ORUI_TEST_DB").ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (username, password) = credentials.split_once('/')?;
    Some(ConnectParams {
        username: username.into(),
        password: password.into(),
        connect_string: connect_string.into(),
        privilege: Privilege::Normal,
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
        eprintln!("ORUI_TEST_DB not set, skipping");
        return;
    };

    let sql = "select level as n, 'row ' || level as label from dual connect by level <= 25";
    h.session.send(DbRequest::Execute {
        id: 1,
        sql: sql.into(),
        max_rows: 10,
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
        sql: "create global temporary table orui_tx_test (x number)".into(),
        max_rows: 1,
    });
    let _ = h.next_result(); // may already exist
    h.session.send(DbRequest::Execute {
        id: 2,
        sql: "insert into orui_tx_test values (1)".into(),
        max_rows: 1,
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
