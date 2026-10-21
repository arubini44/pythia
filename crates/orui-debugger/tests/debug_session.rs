//! End-to-end debugging against a real database.
//!
//! Skipped unless both are set:
//! `ORUI_TEST_DB_DBA=system/oracle@localhost:1521/FREEPDB1`
//! `ORUI_TEST_DB=orui/orui@localhost:1521/FREEPDB1`
//! The database must reach this machine as `ORUI_DEBUG_HOST`
//! (default `host.containers.internal`, right for a local container).

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use orui_db::oracle::Connection;
use orui_debugger::target::{self, MissingPrivilege};
use orui_debugger::{
    Breakpoint, DebugCommand, DebugEvent, Debugger, StopReason, Unit, UnitType, Variable,
};

fn connect(var: &str) -> Option<Connection> {
    let spec = std::env::var(var).ok()?;
    let (cred, db) = spec.rsplit_once('@')?;
    let (user, pw) = cred.split_once('/')?;
    Some(Connection::connect(user, pw, db).expect("connect"))
}

fn host() -> String {
    std::env::var("ORUI_DEBUG_HOST").unwrap_or_else(|_| "host.containers.internal".into())
}

const DEMO: &str = "create or replace procedure dbg_demo(p_n in number) is
  type t_arr is table of number; type t_rec is record (a number := 7, b varchar2(10) := 'bee');
  l_arr   t_arr := t_arr(10, 20, 30); l_rec t_rec;
  l_total number := 0;
  l_name  varchar2(30) := 'start';
  l_flag  boolean := true;
  l_date  date := date '2024-02-29';
begin
  for i in 1 .. p_n loop
    l_total := l_total + i;
    l_name := 'step ' || i;
  end loop;
  dbg_helper(l_total);
  l_name := 'done';
end;";

const HELPER: &str = "create or replace procedure dbg_helper(p_v in out number) is
  l_x number := p_v * 2;
begin
  p_v := l_x + 1;
end;";

const PKG: &str = "create or replace package dbg_pkg as
  procedure p;
end;";

const PKG_BODY: &str = "create or replace package body dbg_pkg as
  g_count number := 0;
  procedure p is
  begin
    g_count := g_count + 1;
  end;
end;";

struct Harness {
    debugger: Debugger,
    wake: mpsc::Receiver<()>,
}

impl Harness {
    fn new() -> Self {
        let (tx, wake) = mpsc::channel();
        let debugger = Debugger::listen("0.0.0.0:0", move || {
            let _ = tx.send(());
        })
        .expect("listen");
        Self { debugger, wake }
    }

    fn next(&self) -> DebugEvent {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(e) = self.debugger.try_recv() {
                eprintln!("event: {}", short(&e));
                return e;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timed out waiting for a debug event");
            let _ = self.wake.recv_timeout(left.min(Duration::from_millis(200)));
        }
    }

    /// Next suspension: (reason, unit, line, variables, stack depth).
    fn stop(&self) -> (StopReason, Unit, Option<u32>, Vec<Variable>, usize) {
        loop {
            match self.next() {
                DebugEvent::Suspended {
                    reason,
                    location,
                    variables,
                    stack,
                } => {
                    return (reason, location.unit, location.line, variables, stack.len());
                }
                DebugEvent::Terminated => panic!("terminated while waiting for a stop"),
                DebugEvent::Error(e) => panic!("debugger error: {e}"),
                _ => {}
            }
        }
    }

    fn until_terminated(&self) {
        loop {
            match self.next() {
                DebugEvent::Terminated => return,
                DebugEvent::Suspended { .. } => self.debugger.send(DebugCommand::Resume),
                _ => {}
            }
        }
    }
}

fn short(e: &DebugEvent) -> String {
    match e {
        DebugEvent::Suspended {
            reason,
            location,
            stack,
            ..
        } => {
            format!(
                "suspended {reason:?} at {} line {:?} (depth {})",
                location.unit,
                location.line,
                stack.len()
            )
        }
        other => format!("{other:?}"),
    }
}

fn value<'a>(vars: &'a [Variable], name: &str) -> Option<&'a str> {
    vars.iter().find(|v| v.name == name)?.value.as_deref()
}

fn run_target(port: u16, block: &'static str) -> thread::JoinHandle<Result<(), orui_db::DbError>> {
    let host = host();
    thread::spawn(move || {
        let conn = connect("ORUI_TEST_DB").unwrap();
        target::run_under_debugger(&conn, &host, port, block)
    })
}

#[test]
fn debug_procedures_and_packages() {
    let (Some(dba), Some(conn)) = (connect("ORUI_TEST_DB_DBA"), connect("ORUI_TEST_DB")) else {
        eprintln!("ORUI_TEST_DB_DBA / ORUI_TEST_DB not set, skipping");
        return;
    };
    // Setup: privileges and the JDWP ACL for the test user.
    for stmt in target::setup_statements("orui", &host()) {
        dba.execute(&stmt, &[])
            .unwrap_or_else(|e| panic!("{stmt}: {e}"));
    }
    let missing = target::check(&conn).unwrap();
    assert!(
        !missing.contains(&MissingPrivilege::DebugConnectSession),
        "{missing:?}"
    );

    for ddl in [HELPER, DEMO, PKG, PKG_BODY] {
        conn.execute(ddl, &[]).unwrap();
    }
    let demo = Unit::new("ORUI", "DBG_DEMO", UnitType::Procedure);
    let helper = Unit::new("ORUI", "DBG_HELPER", UnitType::Procedure);
    let body = Unit::new("ORUI", "DBG_PKG", UnitType::PackageBody);
    for unit in [
        &demo,
        &helper,
        &Unit::new("ORUI", "DBG_PKG", UnitType::Package),
        &body,
    ] {
        target::compile_debug(&conn, unit).unwrap();
    }
    assert!(target::has_debug_info(&conn, &demo).unwrap());

    // --- procedure: deferred breakpoint, variables, stepping ---------------
    let h = Harness::new();
    // Set before the target connects: bound when the unit is loaded.
    h.debugger.send(DebugCommand::SetBreakpoint(Breakpoint {
        unit: demo.clone(),
        line: 10,
    }));
    let target = run_target(h.debugger.port(), "begin dbg_demo(3); end;");

    let (reason, unit, line, vars, depth) = h.stop();
    assert_eq!(
        (reason, &unit, line),
        (StopReason::Breakpoint, &demo, Some(10))
    );
    assert_eq!(depth, 2, "dbg_demo called from the anonymous block");
    assert_eq!(value(&vars, "P_N"), Some("3"));
    assert_eq!(value(&vars, "L_TOTAL"), Some("0"));
    assert_eq!(value(&vars, "L_NAME"), Some("start"));
    assert_eq!(value(&vars, "L_FLAG"), Some("true"));
    assert!(
        value(&vars, "L_DATE").is_some_and(|d| d.contains("24")),
        "{vars:?}"
    );
    assert_eq!(value(&vars, "I"), Some("1"));
    let arr = vars
        .iter()
        .find(|v| v.name == "L_ARR")
        .expect("collection variable");
    eprintln!("collection: {arr:?}");
    assert_eq!(arr.type_name, "T_ARR");
    let elements: Vec<Option<&str>> = arr.children.iter().map(|c| c.value.as_deref()).collect();
    assert_eq!(elements, [Some("10"), Some("20"), Some("30")], "{arr:?}");
    let rec = vars
        .iter()
        .find(|v| v.name == "L_REC")
        .expect("record variable");
    eprintln!("record: {rec:?}");
    assert_eq!(value(&rec.children, "A"), Some("7"), "{rec:?}");
    assert_eq!(value(&rec.children, "B"), Some("bee"), "{rec:?}");

    // Step over: the line advances within the loop.
    h.debugger.send(DebugCommand::StepOver);
    let (reason, _, line, vars, _) = h.stop();
    assert_eq!((reason, line), (StopReason::Step, Some(11)));
    assert_eq!(value(&vars, "L_TOTAL"), Some("1"));
    h.debugger.send(DebugCommand::StepOver);
    let (_, _, line, vars, _) = h.stop();
    assert_ne!(line, Some(11), "stepping moved on");
    assert_eq!(value(&vars, "L_NAME"), Some("step 1"));

    // Move the breakpoint to the helper call and step into it.
    h.debugger.send(DebugCommand::ClearBreakpoint(Breakpoint {
        unit: demo.clone(),
        line: 10,
    }));
    h.debugger.send(DebugCommand::SetBreakpoint(Breakpoint {
        unit: demo.clone(),
        line: 13,
    }));
    h.debugger.send(DebugCommand::Resume);
    let (reason, unit, line, vars, _) = h.stop();
    assert_eq!(
        (reason, &unit, line),
        (StopReason::Breakpoint, &demo, Some(13))
    );
    assert_eq!(value(&vars, "L_TOTAL"), Some("6"));
    h.debugger.send(DebugCommand::StepInto);
    let (_, unit, _, vars, depth) = h.stop();
    assert_eq!(unit, helper, "stepped into dbg_helper");
    assert_eq!(depth, 3);
    assert_eq!(value(&vars, "P_V"), Some("6"));
    h.debugger.send(DebugCommand::StepOut);
    let (_, unit, line, _, _) = h.stop();
    assert_eq!(unit, demo, "stepped out to dbg_demo");
    assert!(line.is_some_and(|l| l >= 13), "{line:?}");
    h.debugger.send(DebugCommand::Resume);
    h.until_terminated();
    target.join().unwrap().expect("target ran to completion");

    // --- package body: class naming ------------------------------------------
    let h = Harness::new();
    h.debugger.send(DebugCommand::SetBreakpoint(Breakpoint {
        unit: body.clone(),
        line: 5,
    }));
    let target = run_target(h.debugger.port(), "begin dbg_pkg.p; dbg_pkg.p; end;");
    let (_, unit, line, vars, _) = h.stop();
    assert_eq!((&unit, line), (&body, Some(5)));
    eprintln!("package vars: {vars:?}");
    h.debugger.send(DebugCommand::Resume);
    let (_, unit, line, _, _) = h.stop();
    assert_eq!((&unit, line), (&body, Some(5)), "second call stops again");
    // Terminating aborts the target call.
    h.debugger.send(DebugCommand::Terminate);
    h.until_terminated();
    let result = target.join().unwrap();
    assert!(result.is_err(), "terminated call fails in the target");

    for ddl in [
        "drop procedure dbg_demo",
        "drop procedure dbg_helper",
        "drop package dbg_pkg",
    ] {
        conn.execute(ddl, &[]).unwrap();
    }
}
