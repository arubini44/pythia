//! Compare → apply sync script → re-compare against a real database.
//!
//! Skipped unless `ORUI_TEST_DB_DBA=system/password@connect_string` is set
//! (a DBA user that may create and drop the ORUI_CMP_A / ORUI_CMP_B schemas).

use orui_compare::data::{self, DataOptions, DataSource};
use orui_compare::schema::{self, Options, Status};
use orui_db::oracle::Connection;
use orui_plsql::split_statements;

fn connect() -> Option<Connection> {
    let spec = std::env::var("ORUI_TEST_DB_DBA").ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (user, password) = credentials.split_once('/')?;
    Some(Connection::connect(user, password, connect_string).expect("connect"))
}

fn run(conn: &Connection, script: &str) {
    for s in split_statements(script) {
        let sql = s.text(script);
        if let Err(e) = conn.execute(sql, &[]) {
            panic!("{e}\n--- statement:\n{sql}\n--- script:\n{script}");
        }
    }
}

fn try_run(conn: &Connection, sql: &str) {
    let _ = conn.execute(sql, &[]);
}

const SOURCE: &str = "
create table dept (id number(4) primary key, name varchar2(30 char) not null, loc varchar2(40 char));
create table emp (
  id      number(6) constraint emp_pk primary key,
  name    varchar2(50 char) not null,
  salary  number(8,2) default 0,
  dept_id number(4) constraint emp_dept_fk references dept on delete cascade,
  email   varchar2(60 char) unique,
  constraint emp_sal_ck check (salary >= 0)
);
create index emp_name_ix on emp (name);
comment on table emp is 'Employees';
comment on column emp.salary is 'Monthly salary';
create table new_t (id number primary key, dept_id number(4) references dept);
create sequence s1 increment by 1 cache 20;
create sequence s2;
create or replace view v_emp as select id, name from emp;
create or replace type t_obj as object (a number, b varchar2(10));
/
create or replace package pkg as
  function total return number;
end;
/
create or replace package body pkg as
  function total return number is
    l number;
  begin
    select count(*) into l from emp;
    return l;
  end;
end;
/
create or replace function f return number is begin return 1; end;
/
create or replace procedure p_a is begin null; end;
/
create or replace trigger trg before insert on emp for each row
begin
  :new.name := upper(:new.name);
end;
/
create or replace synonym syn for emp;
insert into dept values (10, 'Admin', 'Zurich');
insert into dept values (20, 'Dev', 'Basel');
insert into dept values (30, 'Sales', 'Bern');
commit;
";

const TARGET: &str = "
create table dept (id number(4) primary key, name varchar2(30 char) not null, loc varchar2(40 char));
create table emp (
  id      number(6) constraint emp_pk primary key,
  name    varchar2(30 char),
  salary  number(6,2),
  dept_id number(4),
  old_col date
);
create index emp_old_ix on emp (salary);
create table old_t (x number);
create sequence s1 increment by 5 cache 20;
create or replace view v_emp as select id from emp;
create or replace package pkg as
  function total return number;
end;
/
create or replace package body pkg as
  function total return number is begin return 0; end;
end;
/
create or replace function f return number is begin return 1; end;
/
create or replace procedure p_b is begin null; end;
/
insert into dept values (10, 'Administration', 'Zurich');
insert into dept values (20, 'Dev', 'Basel');
insert into dept values (40, 'Ops', null);
commit;
";

fn setup(conn: &Connection) {
    for schema in ["ORUI_CMP_A", "ORUI_CMP_B"] {
        try_run(conn, &format!("drop user {schema} cascade"));
        run(
            conn,
            &format!("create user {schema} identified by \"Cmp_Test_1\" quota unlimited on users;"),
        );
    }
    run(conn, "alter session set current_schema = orui_cmp_a;");
    run(conn, SOURCE);
    run(conn, "alter session set current_schema = orui_cmp_b;");
    run(conn, TARGET);
    run(conn, "alter session set current_schema = system;");
}

fn teardown(conn: &Connection) {
    try_run(conn, "alter session set current_schema = system");
    for schema in ["ORUI_CMP_A", "ORUI_CMP_B"] {
        try_run(conn, &format!("drop user {schema} cascade"));
    }
}

fn describe(diff: &schema::SchemaDiff) -> String {
    diff.objects
        .iter()
        .filter(|o| o.status != Status::Equal)
        .map(|o| {
            format!(
                "{:?} {:?} {}\n  statements: {:?}\n  source:\n{}\n  target:\n{}\n",
                o.status,
                o.key.object_type,
                o.key.name,
                o.statements.iter().map(|s| &s.sql).collect::<Vec<_>>(),
                o.source_ddl.as_deref().unwrap_or("-"),
                o.target_ddl.as_deref().unwrap_or("-")
            )
        })
        .collect()
}

#[test]
fn schema_and_data_roundtrip() {
    let Some(conn) = connect() else {
        eprintln!("ORUI_TEST_DB_DBA not set, skipping");
        return;
    };
    setup(&conn);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let options = Options {
            include_drops: true,
            ..Options::default()
        };
        let source = schema::collect(&conn, "ORUI_CMP_A", &options).expect("collect source");
        let target = schema::collect(&conn, "ORUI_CMP_B", &options).expect("collect target");
        let diff = schema::compare(&source, &target, &options);
        assert!(diff.errors.is_empty(), "{:?}", diff.errors);

        let status = |name: &str| {
            diff.objects
                .iter()
                .find(|o| o.key.name == name)
                .map(|o| o.status)
        };
        assert_eq!(status("DEPT"), Some(Status::Equal), "{}", describe(&diff));
        assert_eq!(status("EMP"), Some(Status::Different));
        assert_eq!(status("NEW_T"), Some(Status::OnlyInSource));
        assert_eq!(status("OLD_T"), Some(Status::OnlyInTarget));
        assert_eq!(status("S1"), Some(Status::Different));
        assert_eq!(status("F"), Some(Status::Equal));
        assert_eq!(status("P_A"), Some(Status::OnlyInSource));
        assert_eq!(status("P_B"), Some(Status::OnlyInTarget));
        assert_eq!(status("TRG"), Some(Status::OnlyInSource));
        assert_eq!(status("SYN"), Some(Status::OnlyInSource));

        let script = diff.script("ORUI_CMP_A → ORUI_CMP_B");
        eprintln!("{script}");
        run(&conn, &script);
        run(&conn, "alter session set current_schema = system;");

        let target = schema::collect(&conn, "ORUI_CMP_B", &options).expect("collect target again");
        let again = schema::compare(&source, &target, &options);
        assert!(
            again.is_equal(),
            "not equal after sync:\n{}",
            describe(&again)
        );

        // Data: DEPT rows differ (changed, missing, extra).
        let src = DataSource::Table {
            schema: "ORUI_CMP_A".into(),
            table: "DEPT".into(),
        };
        let tgt = DataSource::Table {
            schema: "ORUI_CMP_B".into(),
            table: "DEPT".into(),
        };
        let options = DataOptions {
            chunk_size: 2,
            ..DataOptions::default()
        };
        let mut progress_calls = 0;
        let d = data::compare(&conn, &src, &conn, &tgt, options.clone(), |_, _| {
            progress_calls += 1
        })
        .expect("data compare");
        assert_eq!(
            (d.equal, d.changed, d.only_in_source, d.only_in_target),
            (1, 1, 1, 1),
            "{d:?}"
        );
        assert!(progress_calls > 0);
        let script = d.script("data");
        eprintln!("{script}");
        run(&conn, &script);
        run(&conn, "commit;");
        let d = data::compare(&conn, &src, &conn, &tgt, options, |_, _| {})
            .expect("data compare again");
        assert!(d.is_equal(), "{d:?}");
        assert_eq!(d.equal, 3);
    }));
    teardown(&conn);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
