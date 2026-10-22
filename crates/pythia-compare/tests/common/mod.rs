//! Shared setup: two differing schemas PYTHIA_CMP_A (source) and PYTHIA_CMP_B (target).
#![allow(dead_code)]

use pythia_db::oracle::Connection;
use pythia_plsql::split_statements;

pub fn connect() -> Option<Connection> {
    let spec = std::env::var("PYTHIA_TEST_DB_DBA").ok()?;
    let (credentials, connect_string) = spec.rsplit_once('@')?;
    let (user, password) = credentials.split_once('/')?;
    Some(Connection::connect(user, password, connect_string).expect("connect"))
}

pub fn run(conn: &Connection, script: &str) {
    for s in split_statements(script) {
        let sql = s.text(script);
        if let Err(e) = conn.execute(sql, &[]) {
            panic!("{e}\n--- statement:\n{sql}\n--- script:\n{script}");
        }
    }
}

pub fn try_run(conn: &Connection, sql: &str) {
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

pub fn setup(conn: &Connection) {
    for schema in ["PYTHIA_CMP_A", "PYTHIA_CMP_B"] {
        try_run(conn, &format!("drop user {schema} cascade"));
        run(
            conn,
            &format!(
                "create user {schema} identified by \"Cmp_Test_1\" \
                 default tablespace users quota unlimited on users;"
            ),
        );
    }
    run(conn, "alter session set current_schema = pythia_cmp_a;");
    run(conn, SOURCE);
    run(conn, "alter session set current_schema = pythia_cmp_b;");
    run(conn, TARGET);
    run(conn, "alter session set current_schema = system;");
}

pub fn teardown(conn: &Connection) {
    try_run(conn, "alter session set current_schema = system");
    for schema in ["PYTHIA_CMP_A", "PYTHIA_CMP_B"] {
        try_run(conn, &format!("drop user {schema} cascade"));
    }
}
