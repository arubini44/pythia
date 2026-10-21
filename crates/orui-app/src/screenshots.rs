//! Renders the real app offscreen (wgpu) into PNGs for visual review.
//!
//! Skipped unless `ORUI_SCREENSHOT_DIR` is set. With `ORUI_TEST_DB`
//! (`user/password@connect_string`) the scenarios connect to a database:
//!
//! ```text
//! ORUI_SCREENSHOT_DIR=/tmp/shots ORUI_TEST_DB=orui/orui@localhost:1521/FREEPDB1 \
//!   cargo test -p orui-app screenshots -- --nocapture
//! ```

use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use orui_config::{ConnectionProfile, Privilege};

use crate::app::OruiApp;
use crate::documents::{ObjectRef, OpenRequest};

fn out_dir() -> Option<PathBuf> {
    std::env::var_os("ORUI_SCREENSHOT_DIR").map(PathBuf::from)
}

fn harness(dark: bool) -> Harness<'static, OruiApp> {
    let harness = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .with_pixels_per_point(1.5)
        .wgpu()
        .build_eframe(|cc| OruiApp::new(cc, None));
    harness.ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    harness
}

fn save(harness: &mut Harness<'_, OruiApp>, name: &str) {
    let Some(dir) = out_dir() else { return };
    std::fs::create_dir_all(&dir).unwrap();
    harness.run_steps(4);
    let image = harness.render().expect("render");
    let path = dir.join(format!("{name}.png"));
    image.save(&path).unwrap();
    eprintln!("wrote {}", path.display());
}

/// Steps the app until `done` holds (DB events arrive on other threads).
fn wait_until(harness: &mut Harness<'_, OruiApp>, what: &str, done: impl Fn(&OruiApp) -> bool) {
    let start = Instant::now();
    while !done(harness.state()) {
        assert!(
            start.elapsed() < Duration::from_secs(60),
            "timed out waiting for {what}"
        );
        harness.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    harness.run_steps(3);
}

fn profile() -> Option<(ConnectionProfile, String)> {
    let spec = std::env::var("ORUI_TEST_DB").ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (username, password) = credentials.split_once('/')?;
    Some((
        ConnectionProfile {
            name: "orui@FREEPDB1".into(),
            username: username.into(),
            connect_string: connect_string.into(),
            privilege: Privilege::Normal,
            color: Some([0x3b, 0x82, 0xf6]),
            ..Default::default()
        },
        password.into(),
    ))
}

const DEMO_SCHEMA: &str = "
begin
  for t in (select table_name from user_tables where table_name in ('DEMO_EMPLOYEES', 'DEMO_DEPARTMENTS')) loop
    execute immediate 'drop table ' || t.table_name || ' cascade constraints purge';
  end loop;
end;
/
create table demo_departments (
  department_id   number(4) primary key,
  department_name varchar2(30 char) not null,
  location        varchar2(40 char)
);
create table demo_employees (
  employee_id   number(6) primary key,
  first_name    varchar2(20 char),
  last_name     varchar2(25 char) not null,
  email         varchar2(40 char) not null unique,
  hire_date     date default sysdate not null,
  salary        number(8,2),
  department_id number(4) references demo_departments
);
comment on column demo_employees.salary is 'Monthly salary in CHF';
insert into demo_departments values (10, 'Administration', 'Zurich');
insert into demo_departments values (20, 'Engineering', 'Basel');
insert into demo_departments values (30, 'Sales', 'Geneva');
insert into demo_employees
  select level, initcap(dbms_random.string('l', 6)), initcap(dbms_random.string('l', 8)),
         'user' || level || '@example.com', date '2015-01-01' + level * 3,
         round(4000 + dbms_random.value * 9000, 2), 10 * (mod(level, 3) + 1)
    from dual connect by level <= 1200;
commit;
create or replace package demo_pkg as
  -- Raises the salary of an employee by a percentage.
  procedure raise_salary(p_employee_id in number, p_percent in number default 5);
  function department_total(p_department_id in number) return number;
end demo_pkg;
/
create or replace package body demo_pkg as
  procedure raise_salary(p_employee_id in number, p_percent in number default 5) is
  begin
    update demo_employees
       set salary = salary * (1 + p_percent / 100)
     where employee_id = p_employee_id;
  end raise_salary;

  function department_total(p_department_id in number) return number is
    l_total number;
  begin
    select sum(salary) into l_total from demo_employees where department_id = p_department_id;
    return l_total;
  end department_total;
end demo_pkg;
/
create or replace procedure demo_broken is
begin
  missing_procedure(42);
end;
/
";

const QUERY: &str = "-- Salary overview per department
select d.department_name,
       count(*)                 as employees,
       round(avg(e.salary), 2)  as avg_salary,
       max(e.hire_date)         as last_hire
  from demo_employees e
  join demo_departments d on d.department_id = e.department_id
 group by d.department_name
 order by avg_salary desc;

select * from demo_employees where salary > 9000 order by salary desc;
";

#[test]
fn screenshots() {
    if out_dir().is_none() {
        eprintln!("ORUI_SCREENSHOT_DIR not set, skipping");
        return;
    }
    for dark in [true, false] {
        let suffix = if dark { "dark" } else { "light" };
        let mut h = harness(dark);
        h.run_steps(3);
        save(&mut h, &format!("01-start-{suffix}"));
        h.state_mut().test_settings(true);
        h.run_steps(4);
        save(&mut h, &format!("00-settings-formatter-{suffix}"));
        h.state_mut().test_close_settings();
        let sample = |name: &str, folder: &str, color: Option<[u8; 3]>, production: bool| {
            ConnectionProfile {
                name: name.into(),
                folder: folder.into(),
                username: "app".into(),
                connect_string: format!(
                    "{}.db.example.com:1521/SVC",
                    name.to_lowercase().replace(' ', "-")
                ),
                color,
                production,
                ..Default::default()
            }
        };
        h.state_mut().test_show_connections(vec![
            sample("Local dev", "", Some([0x22, 0xc5, 0x5e]), false),
            sample("Acme test", "acme", Some([0xea, 0xb3, 0x08]), false),
            sample("Acme prod", "acme", None, true),
            sample("Globex QA", "globex", Some([0xa8, 0x55, 0xf7]), false),
        ]);
        h.run_steps(4);
        save(&mut h, &format!("00-connect-{suffix}"));
        h.state_mut().test_close_connect();

        let Some((profile, password)) = profile() else {
            continue;
        };
        h.state_mut().test_connect(profile, password);
        wait_until(&mut h, "connection", |app| app.test_connected());

        if dark {
            // Create the demo schema once, through a SQL window.
            h.state_mut().test_open(OpenRequest::Sql {
                connection: "orui@FREEPDB1".into(),
                text: DEMO_SCHEMA.into(),
                run: true,
            });
            wait_until(&mut h, "demo schema", |app| !app.test_focused_busy());
            h.state_mut().test_close_focused();
        }

        h.state_mut().test_open(OpenRequest::Sql {
            connection: "orui@FREEPDB1".into(),
            text: QUERY.into(),
            run: true,
        });
        wait_until(&mut h, "query", |app| !app.test_focused_busy());
        if dark {
            h.state_mut()
                .test_command(crate::commands::Command::ExplainPlan);
            wait_until(&mut h, "plan", |app| {
                app.test_sql_window_ref()
                    .is_some_and(|w| w.state.plan.as_ref().is_some_and(|p| p.result.is_some()))
            });
            save(&mut h, "13-plan-dark");
            h.state_mut()
                .test_command(crate::commands::Command::ExecuteWithStatistics);
            wait_until(&mut h, "statistics", |app| !app.test_focused_busy());
            save(&mut h, "14-statistics-dark");
        }
        h.state_mut()
            .test_expand_browser(&[orui_db::ObjectType::Table, orui_db::ObjectType::Package]);
        wait_until(&mut h, "browser", |app| !app.test_browser_loading());
        save(&mut h, &format!("02-sql-window-{suffix}"));

        let object = |name: &str, t| ObjectRef {
            owner: "ORUI".into(),
            name: name.into(),
            object_type: t,
        };
        h.state_mut().test_open(OpenRequest::Program {
            connection: "orui@FREEPDB1".into(),
            object: object("DEMO_PKG", orui_db::ObjectType::PackageBody),
        });
        wait_until(&mut h, "package source", |app| !app.test_focused_busy());
        save(&mut h, &format!("03-program-window-{suffix}"));

        h.state_mut().test_open(OpenRequest::Program {
            connection: "orui@FREEPDB1".into(),
            object: object("DEMO_BROKEN", orui_db::ObjectType::Procedure),
        });
        wait_until(&mut h, "procedure source", |app| !app.test_focused_busy());
        h.state_mut().test_execute_focused();
        wait_until(&mut h, "compile", |app| !app.test_focused_busy());
        save(&mut h, &format!("04-compile-errors-{suffix}"));

        h.state_mut().test_open(OpenRequest::Object {
            connection: "orui@FREEPDB1".into(),
            object: object("DEMO_EMPLOYEES", orui_db::ObjectType::Table),
        });
        wait_until(&mut h, "columns", |app| !app.test_focused_busy());
        save(&mut h, &format!("05-object-view-{suffix}"));

        h.state_mut().test_open(OpenRequest::Monitor {
            connection: "orui@FREEPDB1".into(),
            start: crate::monitor_window::MonitorStart::Dashboard,
        });
        for _ in 0..3 {
            wait_until(&mut h, "monitor", |app| !app.test_focused_busy());
        }
        save(&mut h, &format!("07-monitor-{suffix}"));

        h.state_mut()
            .test_command(crate::commands::Command::CompareSchemas);
        h.run_steps(5);
        save(&mut h, &format!("09-compare-{suffix}"));

        h.state_mut()
            .test_command(crate::commands::Command::FindInSource);
        h.run_steps(3);
        h.state_mut().test_search_source("salary");
        wait_until(&mut h, "source search", |app| !app.test_focused_busy());
        save(&mut h, &format!("10-source-search-{suffix}"));

        if dark {
            h.state_mut()
                .test_command(crate::commands::Command::SearchData);
            h.run_steps(3);
            h.state_mut().test_search_data("Basel");
            wait_until(&mut h, "data search", |app| !app.test_focused_busy());
            let hits = h.state().test_data_hits();
            assert!(
                hits.contains(&"DEMO_DEPARTMENTS.LOCATION=1".to_owned()),
                "{hits:?}"
            );
            save(&mut h, "15-data-search-dark");
            h.state_mut().test_open(OpenRequest::EditTable {
                connection: "orui@FREEPDB1".into(),
                object: object("DEMO_EMPLOYEES", orui_db::ObjectType::Table),
            });
            wait_until(&mut h, "table definition", |app| !app.test_focused_busy());
            save(&mut h, "16-table-editor-dark");
        }
    }
}

/// Edits a result in the grid, applies it, and checks that a row changed by
/// another session in the meantime is reported as a conflict.
#[test]
fn data_editing() {
    let Some((profile, password)) = profile() else {
        eprintln!("ORUI_TEST_DB not set, skipping");
        return;
    };
    let mut h = harness(true);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    let conn = "orui@FREEPDB1".to_string();
    let setup = "begin execute immediate 'drop table orui_edit_t purge'; exception when others then null; end;\n/\n\
                 create table orui_edit_t (id number primary key, name varchar2(20), hired date);\n\
                 insert into orui_edit_t values (1, 'one', date '2020-01-01');\n\
                 insert into orui_edit_t values (2, 'two', null);\n\
                 commit;\n";
    h.state_mut().test_open(OpenRequest::Sql {
        connection: conn.clone(),
        text: setup.into(),
        run: true,
    });
    wait_until(&mut h, "setup", |app| !app.test_focused_busy());
    h.state_mut().test_close_focused();

    h.state_mut().test_open(OpenRequest::Sql {
        connection: conn.clone(),
        text: "select * from orui_edit_t order by id".into(),
        run: true,
    });
    wait_until(&mut h, "query", |app| !app.test_focused_busy());
    let grid_window = h.state().test_focused();

    let cx = |app: &mut OruiApp| {
        let (id, settings, connection) = app.test_cx();
        (id, settings, connection)
    };
    let start_edit = |h: &mut Harness<'_, OruiApp>| {
        let (id, settings, connection) = cx(h.state_mut());
        let app = h.state_mut();
        let cx = crate::documents::DocContext {
            id,
            settings: &settings,
            connected: true,
            fetch_size: 100,
            connection,
            catalog: None,
            catalog_revision: 0,
            connections: &[],
            debug: None,
        };
        let effects = app.test_sql_window().start_editing(0, &cx);
        app.test_apply(effects);
    };
    start_edit(&mut h);
    wait_until(&mut h, "editable query", |app| !app.test_focused_busy());
    {
        let w = h.state_mut().test_sql_window();
        let r = &mut w.state.results[0];
        assert!(r.grid.is_editable(), "result should be editable");
        assert_eq!(r.columns.last().unwrap().type_name, "ROWID");
        let rows = r.rows.clone();
        r.grid.edit_cell(&rows, 0, 1, Some("uno".into()));
        r.grid
            .edit_cell(&rows, 1, 2, Some("2021-02-03 04:05:06".into()));
    }
    save(&mut h, "06-editing-dark");
    let apply = |h: &mut Harness<'_, OruiApp>| {
        let (id, settings, connection) = cx(h.state_mut());
        let app = h.state_mut();
        let cx = crate::documents::DocContext {
            id,
            settings: &settings,
            connected: true,
            fetch_size: 100,
            connection,
            catalog: None,
            catalog_revision: 0,
            connections: &[],
            debug: None,
        };
        let effects = app.test_sql_window().apply_changes(0, &cx);
        app.test_apply(effects);
    };
    apply(&mut h);
    wait_until(&mut h, "apply", |app| !app.test_focused_busy());
    {
        let w = h.state_mut().test_sql_window();
        let messages: Vec<String> = w.state.messages.iter().map(|m| m.text.clone()).collect();
        assert!(
            messages.iter().any(|m| m.contains("Changes applied")),
            "{messages:?}"
        );
        let r = &w.state.results[0];
        assert_eq!(r.rows[0][1].as_deref(), Some("uno"));
        assert_eq!(r.rows[1][2].as_deref(), Some("2021-02-03 04:05:06"));
        assert!(r.grid.changes().is_some_and(|c| c.is_empty()));
    }
    assert!(
        h.state().test_pending_tx(),
        "applied changes stay in the open transaction"
    );
    h.state_mut().test_commit();
    wait_until(&mut h, "commit", |app| !app.test_pending_tx());

    // Another session changes row 2 behind our back; editing it must conflict.
    h.state_mut().test_open(OpenRequest::Sql {
        connection: conn.clone(),
        text: "update orui_edit_t set name = 'changed' where id = 2;\ncommit;".into(),
        run: true,
    });
    wait_until(&mut h, "concurrent update", |app| !app.test_focused_busy());
    h.state_mut().test_close_focused();
    h.state_mut().test_focus(grid_window);
    {
        let w = h.state_mut().test_sql_window();
        let r = &mut w.state.results[0];
        let rows = r.rows.clone();
        r.grid.edit_cell(&rows, 1, 1, Some("mine".into()));
    }
    apply(&mut h);
    wait_until(&mut h, "conflicting apply", |app| !app.test_focused_busy());
    let w = h.state_mut().test_sql_window();
    let messages: Vec<String> = w.state.messages.iter().map(|m| m.text.clone()).collect();
    assert!(
        messages.iter().any(|m| m.starts_with("Conflict on row 2")),
        "{messages:?}"
    );
}

/// Types `.` after a table alias in a real editor and waits for the
/// completion popup, which needs a round trip to the dictionary.
#[test]
fn completion_popup() {
    let Some((profile, password)) = profile() else {
        eprintln!("ORUI_TEST_DB not set, skipping");
        return;
    };
    for dark in [true, false] {
        let mut h = harness(dark);
        h.state_mut()
            .test_connect(profile.clone(), password.clone());
        wait_until(&mut h, "connection", |app| app.test_connected());
        h.state_mut().test_open(OpenRequest::Sql {
            connection: "orui@FREEPDB1".into(),
            text: "select e\n  from demo_employees e\n where e.department_id = 20".into(),
            run: false,
        });
        h.run_steps(3);
        h.state_mut().test_sql_window().editor.select(8..8);
        h.run_steps(3);
        h.event(egui::Event::Text(".".into()));
        wait_until(&mut h, "completion popup", |app| {
            app.test_sql_window_ref()
                .is_some_and(|w| w.completion_visible_with("SALARY"))
        });
        save(
            &mut h,
            &format!("08-completion-{}", if dark { "dark" } else { "light" }),
        );
    }
}

/// Imports a CSV with a decimal comma, European dates and NULLs into a new
/// table and checks the converted values in the database.
#[test]
fn import_csv() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("orui-import-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("orders 2024.csv");
    std::fs::write(&path, "Order id;Customer;Amount;Order date\n1;Acme;1.234,50;31.12.2024\n2;;7;01.01.2025\n3;Globex;-0,5;15.06.2024\n").unwrap();

    let mut h = harness(true);
    h.state_mut()
        .test_connect(profile.clone(), password.clone());
    wait_until(&mut h, "connection", |app| app.test_connected());
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "begin execute immediate 'drop table orui_import_t purge'; exception when others then null; end;".into(),
        run: true,
    });
    wait_until(&mut h, "cleanup", |app| !app.test_focused_busy());

    h.state_mut().test_import(path, "orui_import_t");
    wait_until(&mut h, "import", |app| !app.test_focused_busy());
    let message = h.state().test_import_message();
    assert!(
        matches!(&message, Some((false, m)) if m.starts_with("Imported 3 rows")),
        "{message:?}"
    );
    save(&mut h, "11-import-dark");

    let (cred, db) = std::env::var("ORUI_TEST_DB")
        .unwrap()
        .rsplit_once('@')
        .map(|(c, d)| (c.to_owned(), d.to_owned()))
        .unwrap();
    let (user, pw) = cred.split_once('/').unwrap();
    let conn = orui_db::oracle::Connection::connect(user, pw, &db).unwrap();
    let rows: Vec<(i64, Option<String>, f64, String)> = conn
        .query_as("select order_id, customer, amount, to_char(order_date, 'YYYY-MM-DD') from orui_import_t order by order_id", &[])
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        rows,
        [
            (1, Some("Acme".into()), 1234.5, "2024-12-31".into()),
            (2, None, 7.0, "2025-01-01".into()),
            (3, Some("Globex".into()), -0.5, "2024-06-15".into()),
        ]
    );
    conn.execute("drop table orui_import_t purge", &[]).unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// Debugs a block calling `demo_pkg.raise_salary` with a breakpoint in the
/// package body: stops at the line, shows variables, steps and finishes.
/// Needs DEBUG CONNECT SESSION / DEBUG ANY PROCEDURE and a JDWP ACL for the
/// test user (see orui-debugger's setup script) and ORUI_DEBUG_HOST.
#[test]
fn debugger_session() {
    let (Some((profile, password)), Ok(host)) = (profile(), std::env::var("ORUI_DEBUG_HOST"))
    else {
        eprintln!("ORUI_TEST_DB / ORUI_DEBUG_HOST not set, skipping");
        return;
    };
    use orui_debugger::{Unit, UnitType};
    let mut h = harness(true);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    let unit = Unit::new("ORUI", "DEMO_PKG", UnitType::PackageBody);
    // Line 4 of the body: the UPDATE inside raise_salary. It only stops for
    // employee 1 (the condition skips the first call).
    h.state_mut().test_debug_setup(&host, unit.clone(), 4);
    let bp = orui_debugger::Breakpoint {
        unit: unit.clone(),
        line: 4,
    };
    {
        let debug = h.state_mut().test_debug_mut();
        debug.set_options(
            &bp,
            orui_debugger::BreakpointOptions {
                condition: Some("p_employee_id = 1".into()),
                hit_count: None,
            },
        );
        debug.add_watch("p_percent");
        debug.add_watch("p_employee_id * 2");
    }
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "begin\n  demo_pkg.raise_salary(p_employee_id => 2, p_percent => 0);\n  \
               demo_pkg.raise_salary(p_employee_id => 1, p_percent => 0);\n  rollback;\nend;"
            .into(),
        run: false,
    });
    h.run_steps(2);
    h.state_mut()
        .test_command(crate::commands::Command::DebugStart);
    wait_until(&mut h, "breakpoint", |app| app.test_debug().is_suspended());
    let stack = &h.state().test_debug().stack;
    assert_eq!(stack[0].location.unit, unit, "{stack:?}");
    assert_eq!(stack[0].location.line, Some(4));
    let vars: Vec<String> = h
        .state()
        .test_debug()
        .variables
        .iter()
        .map(|v| format!("{}={}", v.name, v.value.as_deref().unwrap_or("NULL")))
        .collect();
    assert!(
        vars.iter()
            .any(|v| v.eq_ignore_ascii_case("p_employee_id=1")),
        "condition skipped employee 2: {vars:?}"
    );
    let debug = h.state().test_debug();
    assert_eq!(debug.hits.get(&bp), Some(&1));
    let watches: Vec<(String, Result<Option<String>, String>)> = debug
        .watch_values
        .iter()
        .map(|w| (w.expression.clone(), w.value.clone().map(|v| v.value)))
        .collect();
    assert_eq!(watches[0], ("p_percent".into(), Ok(Some("0".into()))));
    assert!(
        watches[1].1.is_err(),
        "expressions aren't evaluated: {watches:?}"
    );
    // Hovering a parameter in the stopped package body shows its value
    // (once the body's source has loaded).
    let needle = "where employee_id = p_employee_id";
    let start = Instant::now();
    let offset = loop {
        let program = h.state_mut().test_program();
        let editor = program.active_editor().expect("package body editor");
        if let Some(at) = editor.text().find(needle) {
            break at + "where employee_id = ".len() + 3;
        }
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "raise_salary body not shown: {:?}",
            editor.text()
        );
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    };
    let debug = h.state().test_debug();
    assert_eq!(
        debug.hover(&unit, "P_EMPLOYEE_ID").map(|(_, v)| v),
        Some("1".into())
    );
    // The app borrows both; take the session out for the call.
    let mut session = orui_debugger::ui::DebugSession::default();
    std::mem::swap(&mut session, h.state_mut().test_debug_mut());
    let program = h.state_mut().test_program();
    let editor = program.active_editor().unwrap();
    assert!(crate::editor::debug_hover(editor, offset, &session, &unit));
    std::mem::swap(&mut session, h.state_mut().test_debug_mut());
    h.run_steps(6);
    save(&mut h, "12-debugger-dark");

    h.state_mut()
        .test_debug_mut()
        .command(orui_debugger::DebugCommand::StepOver);
    wait_until(&mut h, "step", |app| {
        app.test_debug().is_suspended() || !app.test_debug_running()
    });
    h.state_mut()
        .test_debug_mut()
        .command(orui_debugger::DebugCommand::Resume);
    wait_until(&mut h, "finish", |app| !app.test_debug_running());
}

/// Runs a deployment script that includes another (`@@`), continues after
/// an error (`WHENEVER SQLERROR CONTINUE`) and creates and fills a table.
#[test]
fn deployment_script() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("orui-deploy-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(
        dir.join("install.sql"),
        "prompt Installing\nwhenever sqlerror continue\ndrop table orui_deploy_t purge;\n@@sub/tables\ninsert into orui_deploy_t values (2);\ncommit;\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sub/tables.sql"),
        "create table orui_deploy_t (id number);\ninsert into orui_deploy_t values (1);\n",
    )
    .unwrap();

    let mut h = harness(true);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    // The script expects the table not to exist (left over by a failed run).
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "begin
  for t in (select table_name from user_tables where table_name = 'ORUI_DEPLOY_T') loop
    execute immediate 'drop table orui_deploy_t purge';
  end loop;
end;
/"
        .into(),
        run: true,
    });
    wait_until(&mut h, "cleanup", |app| !app.test_focused_busy());
    h.state_mut().test_open_file(dir.join("install.sql"));
    h.run_steps(2);
    save(&mut h, "deployment-script");
    h.state_mut()
        .test_command(crate::commands::Command::ExecuteAll);
    wait_until(&mut h, "script", |app| !app.test_focused_busy());
    let w = h.state_mut().test_sql_window();
    let messages: Vec<String> = w.state.messages.iter().map(|m| m.text.clone()).collect();
    assert!(
        messages.iter().any(|m| m.contains("continuing"))
            || messages.iter().any(|m| m.contains("ORA-00942")),
        "{messages:?}"
    );
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("Running ") && m.contains("tables.sql")),
        "{messages:?}"
    );
    assert_eq!(w.state.output, ["Installing"]);

    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "select count(*) from orui_deploy_t;\ndrop table orui_deploy_t purge;".into(),
        run: true,
    });
    wait_until(&mut h, "check", |app| !app.test_focused_busy());
    let w = h.state_mut().test_sql_window();
    assert_eq!(w.state.results[0].rows[0][0].as_deref(), Some("2"));
    std::fs::remove_dir_all(dir).unwrap();
}

/// Compiling a package body that someone else changed in the meantime asks
/// before overwriting.
#[test]
fn compile_conflict() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let mut h = harness(true);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    let body = ObjectRef {
        owner: "ORUI".into(),
        name: "DEMO_PKG".into(),
        object_type: orui_db::ObjectType::PackageBody,
    };
    h.state_mut().test_open(OpenRequest::Program {
        connection: "orui@FREEPDB1".into(),
        object: body.clone(),
    });
    wait_until(&mut h, "source", |app| !app.test_focused_busy());
    let program = h.state().test_focused();
    let source = h.state_mut().test_program().active_source();

    // Someone else recompiles the body a moment later.
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let sql = source
        .trim_end()
        .trim_end_matches('/')
        .trim_end()
        .to_owned();
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: format!(
            "{}\n/\n",
            sql.replacen(
                "end demo_pkg;",
                "  -- changed by someone else\nend demo_pkg;",
                1
            )
        ),
        run: true,
    });
    wait_until(&mut h, "other compile", |app| !app.test_focused_busy());
    h.state_mut().test_close_focused();
    h.state_mut().test_focus(program);

    let effects = h.state_mut().test_program().compile(false);
    h.state_mut().test_apply_focused(effects);
    wait_until(&mut h, "check", |app| !app.test_focused_busy());
    assert!(h.state_mut().test_program().has_conflict());
    save(&mut h, "17-compile-conflict-dark");

    let effects = h.state_mut().test_program().overwrite();
    h.state_mut().test_apply_focused(effects);
    wait_until(&mut h, "compile", |app| !app.test_focused_busy());
    let message = h.state_mut().test_program().message();
    assert!(
        matches!(&message, Some((false, m)) if m.contains("compiled")),
        "{message:?}"
    );
}

/// A result shown as a bar chart and as a line chart.
#[test]
fn result_chart() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let mut h = harness(false);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "-- Orders and returns per month
select to_char(add_months(date '2025-01-01', level - 1), 'Mon YYYY') as month,
       round(120 + 40 * sin(level / 2) + level * 6) as orders,
       round(50 + 20 * cos(level / 3) + level * 2) as returns
  from dual connect by level <= 12"
            .into(),
        run: true,
    });
    wait_until(&mut h, "query", |app| !app.test_focused_busy());
    let w = h.state_mut().test_sql_window();
    let result = &mut w.state.results[0];
    let chart = crate::chart_view::ChartState::guess(&result.columns, result.rows.len());
    assert_eq!(chart.x, Some(0));
    assert_eq!(chart.series, vec![1, 2]);
    result.chart = Some(chart);
    save(&mut h, "result-chart-bars");
    let w = h.state_mut().test_sql_window();
    w.state.results[0].chart.as_mut().unwrap().kind = crate::chart_view::ChartKind::Line;
    save(&mut h, "result-chart-lines");
}

/// A result of a table with foreign keys offers its parent and child rows.
#[test]
fn foreign_key_links() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let mut h = harness(false);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "begin
  for t in (select table_name from user_tables where table_name in ('ORUI_FK_C', 'ORUI_FK_P')) loop
    execute immediate 'drop table ' || t.table_name || ' cascade constraints purge';
  end loop;
end;
/
create table orui_fk_p (id number primary key, name varchar2(20));
create table orui_fk_c (id number primary key, p_id number references orui_fk_p);
insert into orui_fk_p values (1, 'one');
insert into orui_fk_c values (10, 1);
select * from orui_fk_c;"
            .into(),
        run: true,
    });
    wait_until(&mut h, "script", |app| !app.test_focused_busy());
    let start = Instant::now();
    loop {
        let w = h.state_mut().test_sql_window();
        if !w.state.results[0].grid.links.is_empty() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "no links");
        h.step();
        std::thread::sleep(Duration::from_millis(15));
    }
    let w = h.state_mut().test_sql_window();
    let result = &w.state.results[0];
    assert_eq!(result.grid.links.len(), 1);
    assert!(result.grid.links[0].label.starts_with("ORUI_FK_P via P_ID"));
    let relation = &result.relations[result.link_relations[0]];
    let sql = crate::relations::query(relation, &result.columns, &result.rows[0]);
    assert!(sql.contains("where ID = 1"), "{sql}");
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "drop table orui_fk_c purge;\ndrop table orui_fk_p purge;".into(),
        run: true,
    });
    wait_until(&mut h, "cleanup", |app| !app.test_focused_busy());
}

/// The scheduler jobs window lists a job, runs it and shows the run.
#[test]
fn scheduler_jobs() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let mut h = harness(false);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "begin
  for j in (select job_name from user_scheduler_jobs where job_name like 'ORUI\\_%' escape '\\') loop
    dbms_scheduler.drop_job(j.job_name, force => true);
  end loop;
  dbms_scheduler.create_job('ORUI_NIGHTLY_STATS', 'PLSQL_BLOCK',
    'begin dbms_session.sleep(0.2); end;',
    repeat_interval => 'FREQ=DAILY;BYHOUR=2', enabled => true,
    comments => 'Refreshes optimizer statistics of the schema every night');
  dbms_scheduler.create_job('ORUI_PURGE_LOGS', 'PLSQL_BLOCK', 'begin null; end;',
    repeat_interval => 'FREQ=HOURLY', enabled => false);
end;
/"
        .into(),
        run: true,
    });
    wait_until(&mut h, "jobs created", |app| !app.test_focused_busy());
    h.state_mut()
        .test_command(crate::commands::Command::SchedulerJobs);
    h.run_steps(2);
    wait_until(&mut h, "jobs", |app| !app.test_focused_busy());
    let jobs = h.state_mut().test_jobs_window();
    assert!(
        jobs.jobs()
            .iter()
            .any(|j| j.name == "ORUI_NIGHTLY_STATS" && j.enabled)
    );
    assert!(
        jobs.jobs()
            .iter()
            .any(|j| j.name == "ORUI_PURGE_LOGS" && !j.enabled)
    );
    let effects = jobs.select("ORUI", "ORUI_NIGHTLY_STATS");
    h.state_mut().test_apply(effects);
    let effects = h.state_mut().test_jobs_window().run_action("run");
    h.state_mut().test_apply(effects);
    let start = Instant::now();
    while h.state_mut().test_jobs_window().runs().is_empty() {
        assert!(start.elapsed() < Duration::from_secs(60), "no run logged");
        std::thread::sleep(Duration::from_millis(500));
        let effects = h.state_mut().test_jobs_window().refresh();
        h.state_mut().test_apply(effects);
        wait_until(&mut h, "refresh", |app| !app.test_focused_busy());
    }
    assert_eq!(
        h.state_mut().test_jobs_window().runs()[0].status,
        "SUCCEEDED"
    );
    save(&mut h, "scheduler-jobs");
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "begin
  dbms_scheduler.drop_job('ORUI_NIGHTLY_STATS', force => true);
  dbms_scheduler.drop_job('ORUI_PURGE_LOGS', force => true);
end;
/"
        .into(),
        run: true,
    });
    wait_until(&mut h, "cleanup", |app| !app.test_focused_busy());
}

/// "Execute to file" streams a query into a file and can be cancelled.
#[test]
fn execute_to_file() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let mut h = harness(false);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: String::new(),
        run: false,
    });
    h.run_steps(2);
    let dir = std::env::temp_dir().join(format!("orui-to-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let start_export = |h: &mut Harness<'_, OruiApp>, sql: &str, path: &std::path::Path| {
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Some(path.to_owned())).unwrap();
        h.state_mut().test_sql_window().state.to_file_pick = Some((rx, sql.to_owned()));
        h.run_steps(2);
    };

    let path = dir.join("rows.json");
    start_export(
        &mut h,
        "select level as id, 'item ' || level as name from dual connect by level <= 5000",
        &path,
    );
    wait_until(&mut h, "export", |app| !app.test_focused_busy());
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 5000);
    assert_eq!(json[4999]["ID"], 5000);
    let w = h.state_mut().test_sql_window();
    assert!(
        w.state
            .messages
            .last()
            .unwrap()
            .text
            .starts_with("5000 rows written to"),
        "{:?}",
        w.state.messages.last().map(|m| &m.text)
    );

    // Cancel a long export: the incomplete file is removed.
    let path = dir.join("huge.csv");
    start_export(
        &mut h,
        "select a.n, b.n as m from (select level n from dual connect by level <= 100000) a,
                (select level n from dual connect by level <= 100000) b",
        &path,
    );
    wait_until(&mut h, "rows", |app| {
        app.test_sql_window_ref()
            .and_then(|w| w.state.to_file.as_ref())
            .is_some_and(|t| t.rows.load(std::sync::atomic::Ordering::Relaxed) > 0)
    });
    save(&mut h, "execute-to-file");
    h.state_mut().test_command(crate::commands::Command::Cancel);
    wait_until(&mut h, "cancel", |app| !app.test_focused_busy());
    let w = h.state_mut().test_sql_window();
    let last = &w.state.messages.last().unwrap().text;
    assert!(last.contains("ORA-01013"), "{last}");
    assert!(!path.exists());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Tables in the browser show their indexes, constraints and triggers.
#[test]
fn browser_table_parts() {
    let Some((profile, password)) = profile() else {
        return;
    };
    let mut h = harness(false);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "begin
  for t in (select table_name from user_tables where table_name in ('ORDER_LINES', 'ORDERS')) loop
    execute immediate 'drop table ' || t.table_name || ' cascade constraints purge';
  end loop;
end;
/
create table orders (order_id number primary key, customer varchar2(40) not null,
  ordered_at date default sysdate not null);
create table order_lines (
  order_id number not null constraint order_lines_order_fk references orders,
  line_no number(4) not null,
  product varchar2(40) not null,
  quantity number(6) not null constraint order_lines_qty_ck check (quantity > 0),
  price number(10,2),
  constraint order_lines_pk primary key (order_id, line_no));
create index order_lines_product_ix on order_lines (product);
create or replace trigger order_lines_audit before insert or update on order_lines
for each row begin :new.price := round(:new.price, 2); end;
/"
        .into(),
        run: true,
    });
    wait_until(&mut h, "tables", |app| !app.test_focused_busy());
    h.state_mut()
        .test_expand_browser(&[orui_db::ObjectType::Table]);
    wait_until(&mut h, "folder", |app| !app.test_browser_loading());
    h.state_mut().test_expand_table(ObjectRef {
        owner: "ORUI".into(),
        name: "ORDER_LINES".into(),
        object_type: orui_db::ObjectType::Table,
    });
    wait_until(&mut h, "parts", |app| !app.test_browser_loading());
    save(&mut h, "browser-table-parts");
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "drop table order_lines purge;\ndrop table orders purge;".into(),
        run: true,
    });
    wait_until(&mut h, "cleanup", |app| !app.test_focused_busy());
}

/// A project folder under git: schema export, status, diff with HEAD and
/// with the database, deployment script.
#[test]
fn project_folder() {
    let Some((profile, password)) = profile() else {
        return;
    };
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        return;
    }
    let mut h = harness(false);
    h.state_mut().test_connect(profile, password);
    wait_until(&mut h, "connection", |app| app.test_connected());
    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "create or replace package orui_proj_pkg as
  function answer return number;
end orui_proj_pkg;
/
create or replace package body orui_proj_pkg as
  function answer return number is
  begin
    return 42;
  end answer;
end orui_proj_pkg;
/"
        .into(),
        run: true,
    });
    wait_until(&mut h, "package", |app| !app.test_focused_busy());

    let dir = std::env::temp_dir().join(format!("orui-project-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // The panel picks up results while it's visible.
    h.state_mut()
        .test_command(crate::commands::Command::ShowProject);
    let mut actions = Vec::new();
    h.state_mut()
        .test_project()
        .export_schema(dir.clone(), "orui@FREEPDB1", &mut actions);
    h.state_mut().test_project_actions(actions);
    let start = Instant::now();
    while h.state_mut().test_project().is_busy() {
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "export {:?}",
            h.state_mut().test_project().export_progress()
        );
        h.step();
        std::thread::sleep(Duration::from_millis(50));
    }
    let message = h
        .state_mut()
        .test_project()
        .message()
        .unwrap_or_default()
        .to_owned();
    assert!(message.starts_with("Exported "), "{message}");
    let body = dir.join("packages/orui_proj_pkg.pkb");
    assert!(body.exists());

    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.email", "t@example.com"]);
    git(&["config", "user.name", "T"]);
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "export"]);
    let text = std::fs::read_to_string(&body).unwrap();
    std::fs::write(&body, text.replace("return 42;", "return 43;")).unwrap();

    let project = h.state_mut().test_project();
    project.open_root(dir.clone());
    project.refresh_now();
    project.expand(dir.join("packages"));
    let repo = project.repo().unwrap();
    let modified = repo.root.join("packages/orui_proj_pkg.pkb");
    assert_eq!(repo.status[&modified], crate::git::FileStatus::Modified);
    h.state_mut()
        .test_command(crate::commands::Command::ShowProject);
    h.run_steps(3);
    save(&mut h, "project-panel");

    // Working copy against the database: the changed line differs.
    let mut actions = Vec::new();
    h.state_mut()
        .test_project()
        .compare_db(&modified, "orui@FREEPDB1", &mut actions);
    h.state_mut().test_project_actions(actions);
    let start = Instant::now();
    while h.state_mut().test_project().is_busy() {
        assert!(start.elapsed() < Duration::from_secs(30), "compare");
        h.step();
        std::thread::sleep(Duration::from_millis(30));
    }
    h.run_steps(3);
    let title = h.state().test_focused_title().unwrap_or_default();
    assert!(title.starts_with("orui_proj_pkg.pkb ↔"), "{title}");
    save(&mut h, "project-compare-db");

    // And against the last commit.
    let mut actions = Vec::new();
    h.state_mut()
        .test_project()
        .diff_head(&modified, &mut actions);
    assert_eq!(actions.len(), 1);
    h.state_mut().test_project_actions(actions);
    h.run_steps(3);
    assert_eq!(
        h.state().test_focused_title().as_deref(),
        Some("orui_proj_pkg.pkb (changes)")
    );

    h.state_mut().test_open(OpenRequest::Sql {
        connection: "orui@FREEPDB1".into(),
        text: "drop package orui_proj_pkg;".into(),
        run: true,
    });
    wait_until(&mut h, "cleanup", |app| !app.test_focused_busy());
    std::fs::remove_dir_all(&dir).unwrap();
}

/// The keyboard settings list commands with their shortcuts.
#[test]
fn settings_keyboard() {
    if out_dir().is_none() {
        return;
    }
    let mut h = harness(true);
    h.state_mut().test_settings_keyboard();
    h.run_steps(3);
    save(&mut h, "settings-keyboard");
}

/// A font file for code replaces Hack; a bad file is reported, not loaded.
#[test]
fn code_font() {
    let font = "/usr/share/fonts/truetype/noto/NotoMono-Regular.ttf";
    if !std::path::Path::new(font).exists() {
        return;
    }
    let mut h = harness(false);
    let ctx = h.ctx.clone();
    h.state_mut().test_code_font(&ctx, font);
    h.run_steps(3);
    save(&mut h, "code-font");
    h.state_mut().test_code_font(&ctx, "/etc/hostname");
    h.run_steps(2);
    assert!(
        h.state()
            .test_log()
            .iter()
            .any(|l| l.contains("is not a TrueType or OpenType font"))
    );
}
