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
