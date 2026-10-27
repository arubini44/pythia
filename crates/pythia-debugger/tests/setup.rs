//! The setup checks against a real database: what the user has, trying
//! hosts end to end, and the fix run as a DBA.
//!
//! Skipped unless `PYTHIA_TEST_DB` and `PYTHIA_TEST_DB_DBA` are set; the
//! database must reach this machine as `PYTHIA_DEBUG_HOST`.

mod common;

use std::time::Duration;

use common::{connect, host};
use pythia_db::{ConnectParams, DbKind};
use pythia_debugger::setup::{self, Grant, Probe, Problem};
use pythia_debugger::target;

fn params(var: &str) -> Option<ConnectParams> {
    let spec = std::env::var(var).ok()?;
    let (cred, db) = spec.rsplit_once('@')?;
    let (user, pw) = cred.split_once('/')?;
    Some(ConnectParams {
        kind: DbKind::Oracle,
        username: user.into(),
        password: pw.into(),
        connect_string: db.into(),
        privilege: Default::default(),
        init_sql: Vec::new(),
        ssh: None,
    })
}

/// An ACL entry for `host` that only this test uses.
fn ace_for(host: &str) -> Grant {
    Grant {
        user: "PYTHIA".into(),
        debug_connect: false,
        debug_any: false,
        ace: Some((host.into(), setup::ANY_PORT)),
    }
}

#[test]
fn setup_checks() {
    let (Some(user), Some(dba), Some(conn)) = (
        params("PYTHIA_TEST_DB"),
        params("PYTHIA_TEST_DB_DBA"),
        connect("PYTHIA_TEST_DB"),
    ) else {
        eprintln!("PYTHIA_TEST_DB / PYTHIA_TEST_DB_DBA not set, skipping");
        return;
    };
    let dba_conn = connect("PYTHIA_TEST_DB_DBA").unwrap();
    for stmt in target::setup_statements("pythia", &host()) {
        dba_conn
            .execute(&stmt, &[])
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }

    // What the user has; the test user can read DBA_HOST_ACES.
    let facts = setup::facts(&conn).unwrap();
    assert_eq!(facts.user, "PYTHIA");
    assert!(facts.debug_connect && facts.debug_any, "{facts:?}");
    assert!(facts.acl, "the test database is 12.1+");
    assert!(facts.container.is_some());
    assert!(!facts.acl_admin && !facts.grant_any, "{facts:?}");
    let aces = facts.aces.clone().expect("DBA_HOST_ACES readable");
    assert!(
        setup::acl_allows(&aces, &host(), &setup::ANY_PORT),
        "{aces:?}"
    );
    assert!(!setup::acl_allows(
        &aces,
        "no-acl.invalid",
        &setup::ANY_PORT
    ));
    // A local connection to a container: the server sees its gateway.
    let client = facts.client_address.clone().expect("the client's address");
    let dba_facts = setup::facts(&dba_conn).unwrap();
    assert!(dba_facts.acl_admin && dba_facts.grant_any, "{dba_facts:?}");

    let timeout = Duration::from_secs(10);
    // The host the tests use works, on a fixed port too.
    match setup::probe(&user, &host(), Some(&(47_200..=47_209)), timeout) {
        Probe::Reached { port, agent, .. } => {
            assert!((47_200..=47_209).contains(&port));
            assert!(!agent.is_empty());
        }
        other => panic!("{other:?}"),
    }
    // So does the address the server sees for this machine, the wizard's
    // first guess, once an ACL entry allows it (with Docker: the network's
    // gateway, which PYTHIA_DEBUG_HOST names too).
    if setup::acl_allows(&aces, &client, &setup::ANY_PORT) {
        let probed = setup::probe(&user, &client, None, timeout);
        assert!(
            matches!(probed, Probe::Reached { .. }),
            "{client}: {probed:?}"
        );
    }
    match setup::probe(&user, "no-acl.invalid", None, timeout) {
        Probe::Failed { problem, error, .. } => {
            assert_eq!(problem, Problem::Acl);
            assert!(error.contains("ORA-24247"), "{error}");
        }
        other => panic!("{other:?}"),
    }

    // The fix, run as the DBA: an entry for a name the server can't
    // resolve, which then fails on the name instead of the ACL.
    let unknown = "pythia-setup-test.invalid";
    setup::run_as(
        &dba,
        facts.container.as_deref(),
        &ace_for(unknown).statements(),
    )
    .unwrap();
    let aces = setup::facts(&conn).unwrap().aces.unwrap();
    assert!(setup::acl_allows(&aces, unknown, &setup::ANY_PORT));
    let probed = setup::probe(&user, unknown, None, timeout);
    setup::run_as(&dba, facts.container.as_deref(), &ace_for(unknown).undo()).unwrap();
    match probed {
        Probe::Failed { problem, .. } => assert_eq!(problem, Problem::UnknownHost),
        other => panic!("{other:?}"),
    }
    let aces = setup::facts(&conn).unwrap().aces.unwrap();
    assert!(!setup::acl_allows(&aces, unknown, &setup::ANY_PORT));

    // An address that drops the connection: the probe gives up in time.
    let dropped = "192.0.2.1";
    setup::run_as(&dba, None, &ace_for(dropped).statements()).unwrap();
    let started = std::time::Instant::now();
    let probed = setup::probe(&user, dropped, None, Duration::from_secs(2));
    let took = started.elapsed();
    setup::run_as(&dba, None, &ace_for(dropped).undo()).unwrap();
    match probed {
        Probe::Failed { problem, error, .. } => {
            assert_eq!(problem, Problem::TimedOut);
            assert!(error.contains("within 2 s"), "{error}");
        }
        other => panic!("{other:?}"),
    }
    assert!(took < Duration::from_secs(5), "{took:?}");

    // A statement that fails says which.
    let err = setup::run_as(&user, None, &ace_for("x.invalid").statements()).unwrap_err();
    assert!(err.starts_with("begin…:"), "{err}");
}
