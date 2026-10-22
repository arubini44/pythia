//! Spike for #13: does the pure-Rust `oracledb` ("thin") driver do what
//! Pythia needs? Runs each pattern against a test database and prints a
//! Markdown table (check, result, time, details), next to the `oracle` crate
//! (Instant Client, "thick") where comparing helps.
//!
//! ```text
//! PYTHIA_TEST_DB=pythia/pythia@localhost:1521/FREEPDB1 \
//! PYTHIA_TEST_DB_DBA=system/oracle@localhost:1521/FREEPDB1 \
//! PYTHIA_DEBUG_HOST=192.168.144.1 \
//!   cargo run -p pythia-db --example thin_probe [-- check names…]
//! ```
//!
//! `PYTHIA_PROBE_SYS` (default `sys/oracle`) is used for the SYSDBA check,
//! `PYTHIA_PROBE_DBS` (`label=user/pw@db;…`) for the version check.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use oracledb::{Config, Connection, ErrorKind};
use pythia_db::oracle;

type Check = Result<String, String>;
/// A named check.
type Probe = (&'static str, fn() -> Check);

fn main() {
    let only: Vec<String> = std::env::args().skip(1).collect();
    let checks: &[Probe] = &[
        ("connect_easy", connect_easy),
        ("connect_tns_alias", connect_tns_alias),
        ("connect_sysdba", connect_sysdba),
        ("connect_proxy", connect_proxy),
        ("login_module_schema", login_module_schema),
        ("named_binds", named_binds),
        ("cursor_kept", cursor_kept),
        ("stream_100k", stream_100k),
        ("dbms_output", dbms_output),
        ("compile_errors", compile_errors),
        ("create_trigger", create_trigger),
        ("dml_transaction", dml_transaction),
        ("types", types),
        ("long_column", long_column),
        ("tstz_region", tstz_region),
        ("tstz_value", tstz_value),
        ("batch_error", batch_error),
        ("cancel_thick_baseline", cancel_thick_baseline),
        ("cancel_call_timeout_sleep", cancel_call_timeout_sleep),
        ("cancel_call_timeout_sql", cancel_call_timeout_sql),
        ("cancel_sql_from_dba", cancel_sql_from_dba),
        ("cancel_sleep_from_dba", cancel_sleep_from_dba),
        ("kill_session", kill_session),
        ("debugger_connect", debugger_connect),
        ("versions", versions),
        ("thick_connect", thick_connect),
    ];
    println!("| Check | Result | ms | Details |\n|---|---|---|---|");
    for (name, check) in checks {
        if !only.is_empty() && !only.iter().any(|o| o == name) {
            continue;
        }
        let start = Instant::now();
        let result = check();
        let ms = start.elapsed().as_millis();
        let (mark, details) = match result {
            Ok(d) => ("ok", d),
            Err(d) => ("FAIL", d),
        };
        let details = details.replace('\n', "<br>").replace('|', "\\|");
        println!("| {name} | {mark} | {ms} | {details} |");
    }
}

// ----- connections -----------------------------------------------------

/// `user/password@connect_string` from an environment variable.
fn spec(var: &str) -> (String, String, String) {
    let spec = std::env::var(var).unwrap_or_else(|_| panic!("{var} not set"));
    split(&spec)
}

fn split(spec: &str) -> (String, String, String) {
    let (cred, db) = spec.rsplit_once('@').expect("user/pw@db");
    let (user, pw) = cred.split_once('/').expect("user/pw@db");
    (user.into(), pw.into(), db.into())
}

fn config(user: &str, pw: &str, db: &str) -> Result<Config, String> {
    Config::default()
        .set_credentials(user, pw)
        .set_connect_string(db)
        .map_err(err)
}

fn thin(var: &str) -> Result<Connection, String> {
    let (user, pw, db) = spec(var);
    oracledb::connect(config(&user, &pw, &db)?).map_err(err)
}

fn thick(var: &str) -> Result<oracle::Connection, String> {
    let (user, pw, db) = spec(var);
    oracle::Connection::connect(user, pw, db).map_err(|e| e.to_string())
}

/// An error as Pythia would show it: the database error's code and message,
/// else the driver's kind.
fn err(e: oracledb::Error) -> String {
    match e.kind() {
        ErrorKind::DbError(db) => format!(
            "ORA-{:05} (offset {}): {}",
            db.code(),
            db.offset(),
            db.message()
        ),
        kind => format!("{kind:?}: {e}"),
    }
}

/// The first column of a one-row query, as text.
fn one(conn: &Connection, sql: &str) -> Result<String, String> {
    let stmt = conn.statement(sql).map_err(err)?.build().map_err(err)?;
    let mut cursor = stmt.query(&[]).map_err(err)?;
    let column = cursor.columns()[0].clone();
    let row = cursor.next().ok_or("no row")?.map_err(err)?;
    Ok(thin_cell(&row, 0, &column).unwrap_or_else(|| "NULL".into()))
}

fn connect_easy() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let version = conn.version().map_err(err)?;
    let banner = one(&conn, "select banner_full from v$version where rownum = 1")
        .or_else(|_| one(&conn, "select banner from v$version where rownum = 1"))?;
    Ok(format!("version {version}; {banner}"))
}

fn connect_tns_alias() -> Check {
    let (user, pw, db) = spec("PYTHIA_TEST_DB");
    let (host_port, service) = db.split_once('/').ok_or("easy connect expected")?;
    let (host, port) = host_port.split_once(':').unwrap_or((host_port, "1521"));
    let dir = std::env::temp_dir().join(format!("pythia-probe-tns-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join("tnsnames.ora"),
        format!(
            "PYTHIA_PROBE =\n  (DESCRIPTION =\n    (ADDRESS = (PROTOCOL = TCP)(HOST = {host})(PORT = {port}))\n    (CONNECT_DATA = (SERVICE_NAME = {service})))\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    let config = Config::default()
        .set_credentials(&user, &pw)
        .set_config_dir(dir.to_str().unwrap())
        .set_connect_string("pythia_probe")
        .map_err(err)?;
    let conn = oracledb::connect(config).map_err(err)?;
    let _ = std::fs::remove_dir_all(&dir);
    Ok(format!(
        "alias (lower case) → {}",
        one(
            &conn,
            "select sys_context('userenv', 'service_name') from dual"
        )?
    ))
}

fn connect_sysdba() -> Check {
    let (_, _, db) = spec("PYTHIA_TEST_DB");
    let sys = std::env::var("PYTHIA_PROBE_SYS").unwrap_or_else(|_| "sys/oracle".into());
    let (user, pw) = sys.split_once('/').unwrap();
    let config = config(user, pw, &db)?.set_auth_mode(oracledb::AUTH_MODE_SYSDBA);
    let conn = oracledb::connect(config).map_err(err)?;
    Ok(format!(
        "session user {}, sysdba {}",
        one(
            &conn,
            "select sys_context('userenv', 'session_user') from dual"
        )?,
        one(&conn, "select sys_context('userenv', 'isdba') from dual")?
    ))
}

fn connect_proxy() -> Check {
    let dba = thin("PYTHIA_TEST_DB_DBA")?;
    let (target, _, db) = spec("PYTHIA_TEST_DB");
    let (dba_user, dba_pw, _) = spec("PYTHIA_TEST_DB_DBA");
    // Through the DBA (a common user in a CDB) and through a local user.
    let _ = dba.execute("drop user pythia_probe_proxy", &[]);
    dba.execute(
        "create user pythia_probe_proxy identified by \"Probe_1\"",
        &[],
    )
    .map_err(err)?;
    dba.execute("grant create session to pythia_probe_proxy", &[])
        .map_err(err)?;
    let mut lines = Vec::new();
    for (proxy, pw) in [
        (dba_user.as_str(), dba_pw.as_str()),
        ("pythia_probe_proxy", "Probe_1"),
    ] {
        dba.execute(
            &format!("alter user {target} grant connect through {proxy}"),
            &[],
        )
        .map_err(err)?;
        let user = format!("{proxy}[{target}]");
        let thick = oracle::Connection::connect(&user, pw, &db)
            .map(|_| "ok".to_owned())
            .unwrap_or_else(|e| e.to_string());
        let thin = oracledb::connect(config(&user, pw, &db)?)
            .map_err(err)
            .and_then(|conn| {
                Ok(format!(
                    "session user {}, proxy user {}",
                    one(
                        &conn,
                        "select sys_context('userenv', 'session_user') from dual"
                    )?,
                    one(
                        &conn,
                        "select sys_context('userenv', 'proxy_user') from dual"
                    )?
                ))
            });
        lines.push(format!("{user}: thin {}; thick {thick}", show(thin)));
        dba.execute(
            &format!("alter user {target} revoke connect through {proxy}"),
            &[],
        )
        .map_err(err)?;
    }
    dba.execute("drop user pythia_probe_proxy", &[])
        .map_err(err)?;
    let text = lines.join("\n");
    if text.contains("FAILED") {
        Err(text)
    } else {
        Ok(text)
    }
}

fn login_module_schema() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    conn.execute(
        "alter session set nls_date_format = 'YYYY-MM-DD HH24:MI:SS'",
        &[],
    )
    .map_err(err)?;
    conn.set_module("pythia").map_err(err)?;
    let date = one(&conn, "select to_char(date '2026-10-22') from dual")?;
    let module = one(&conn, "select sys_context('userenv', 'module') from dual")?;
    let schema = one(
        &conn,
        "select sys_context('userenv', 'current_schema') from dual",
    )?;
    Ok(format!(
        "login script applied: {date}; module {module}; schema {schema}"
    ))
}

fn named_binds() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    // Pythia binds every value as text (the variable prompt).
    let row = conn
        .query_row_named(
            "select :a || '-' || :b, to_number(:c) * 2 from dual",
            &[("a", &"x"), ("b", &"y"), ("c", &"21")],
        )
        .map_err(err)?;
    let text: String = row.get(0).map_err(err)?;
    let number: oracledb::OracleNumber = row.get(1).map_err(err)?;
    let null_bind = conn
        .query_row_named(
            "select nvl(:a, 'was null') from dual",
            &[("a", &None::<String>)],
        )
        .map_err(err)?;
    let null_text: String = null_bind.get(0).map_err(err)?;
    Ok(format!("{text}, {number}, {null_text}"))
}

/// Pythia keeps an open result set between "fetch more" requests, while the
/// session runs other statements (`ResultSet<'static>` today).
struct OpenQuery {
    cursor: oracledb::Cursor,
}

fn cursor_kept() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let mut open = OpenQuery {
        cursor: conn
            .query("select level from dual connect by level <= 1000", &[])
            .map_err(err)?,
    };
    let first = open.cursor.by_ref().take(100).count();
    let between = one(&conn, "select 'other statement' from dual")?;
    let mut rest = 0;
    for row in open.cursor.by_ref() {
        row.map_err(err)?;
        rest += 1;
    }
    Ok(format!(
        "{first} rows, then '{between}' on the same connection, then {rest} more"
    ))
}

fn stream_100k() -> Check {
    let sql = "select level n, 'row ' || level s, date '2026-01-01' + level / 86400 d, level / 7 f from dual connect by level <= 100000";
    let conn = thin("PYTHIA_TEST_DB")?;
    let start = Instant::now();
    let stmt = conn
        .statement(sql)
        .map_err(err)?
        .fetch_array_size(1000)
        .build()
        .map_err(err)?;
    let cursor = stmt.query(&[]).map_err(err)?;
    let columns: Vec<_> = cursor.columns().to_vec();
    let mut count = 0usize;
    let mut bytes = 0usize;
    for row in cursor {
        let row = row.map_err(err)?;
        for (i, c) in columns.iter().enumerate() {
            bytes += thin_cell(&row, i, c).map(|s| s.len()).unwrap_or(0);
        }
        count += 1;
    }
    let thin_ms = start.elapsed().as_millis();

    let conn = thick("PYTHIA_TEST_DB")?;
    let start = Instant::now();
    let mut stmt = conn
        .statement(sql)
        .fetch_array_size(1000)
        .build()
        .map_err(|e| e.to_string())?;
    let rows = stmt.query(&[]).map_err(|e| e.to_string())?;
    let mut thick_count = 0usize;
    let mut thick_bytes = 0usize;
    for row in rows {
        let row = row.map_err(|e| e.to_string())?;
        for v in row.sql_values() {
            thick_bytes += thick_cell(v).map(|s| s.len()).unwrap_or(0);
        }
        thick_count += 1;
    }
    let thick_ms = start.elapsed().as_millis();
    Ok(format!(
        "thin {count} rows / {bytes} chars in {thin_ms} ms; thick {thick_count} rows / {thick_bytes} chars in {thick_ms} ms"
    ))
}

fn dbms_output() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    conn.execute("begin dbms_output.enable(null); end;", &[])
        .map_err(err)?;
    conn.execute(
        "begin dbms_output.put_line('one'); dbms_output.put_line(null); dbms_output.put_line('three ünï'); end;",
        &[],
    )
    .map_err(err)?;
    let mut lines = Vec::new();
    loop {
        let mut result = conn
            .execute(
                "begin dbms_output.get_line(:1, :2); end;",
                &[&oracledb::DB_TYPE_VARCHAR, &oracledb::DB_TYPE_NUMBER],
            )
            .map_err(err)?;
        // out_bind_data() hands the values over once.
        let out = result.out_bind_data();
        let line: Option<String> = out.get(0).map_err(err)?;
        let status: i32 = out.get(1).map_err(err)?;
        if status != 0 {
            break;
        }
        lines.push(line.unwrap_or_default());
    }
    Ok(format!("{lines:?}"))
}

fn compile_errors() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let created = conn.execute(
        "create or replace procedure pythia_probe_p as begin x := 1; end;",
        &[],
    );
    let outcome = match created {
        Ok(_) => "execute returned Ok".to_owned(),
        Err(e) => format!("execute returned {}", err(e)),
    };
    let warning = conn
        .last_warning()
        .map_err(err)?
        .unwrap_or_else(|| "none".into());
    let errors = one(
        &conn,
        "select count(*) || ' error(s), first: ' || min(text) keep (dense_rank first order by sequence) from user_errors where name = 'PYTHIA_PROBE_P'",
    )?;
    let _ = conn.execute("drop procedure pythia_probe_p", &[]);
    Ok(format!(
        "{outcome}; last_warning {warning}; user_errors: {errors}"
    ))
}

/// A trigger using `:new` (oracle/rust-oracledb#33), as a program window
/// compiles it, and a package with comments and a q-string.
fn create_trigger() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let _ = conn.execute("drop table pythia_probe_trg purge", &[]);
    conn.execute(
        "create table pythia_probe_trg (n number, changed date)",
        &[],
    )
    .map_err(err)?;
    let trigger = conn
        .execute(
            "create or replace trigger pythia_probe_trg_bu before update on pythia_probe_trg for each row\nbegin\n  :new.changed := sysdate; -- :old.n\nend;",
            &[],
        )
        .map(|_| "trigger created".to_owned())
        .unwrap_or_else(err);
    let package = conn
        .execute(
            "create or replace package pythia_probe_q as\n  /* a comment with :x */\n  c constant varchar2(20) := q'[it's :y]';\nend;",
            &[],
        )
        .map(|_| "package with comment and q-string created".to_owned())
        .unwrap_or_else(err);
    let _ = conn.execute("drop table pythia_probe_trg purge", &[]);
    let _ = conn.execute("drop package pythia_probe_q", &[]);
    let text = format!("{trigger}; {package}");
    if text.contains("ORA-") || text.contains(':') && text.contains("required") {
        Err(text)
    } else {
        Ok(text)
    }
}

fn dml_transaction() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let _ = conn.execute("drop table pythia_probe_t purge", &[]);
    conn.execute("create table pythia_probe_t (n number)", &[])
        .map_err(err)?;
    let result = (|| {
        let inserted = conn
            .execute(
                "insert into pythia_probe_t select level from dual connect by level <= 3",
                &[],
            )
            .map_err(err)?
            .rows_affected();
        let tx = one(
            &conn,
            "select dbms_transaction.local_transaction_id from dual",
        )?;
        conn.rollback().map_err(err)?;
        let after = one(
            &conn,
            "select dbms_transaction.local_transaction_id from dual",
        )?;
        let rows = one(&conn, "select count(*) from pythia_probe_t")?;
        Ok(format!(
            "{inserted} rows affected; transaction {tx}; after rollback: {after}, {rows} rows"
        ))
    })();
    let _ = conn.execute("drop table pythia_probe_t purge", &[]);
    result
}

// ----- values as text --------------------------------------------------

/// Today's cell text (`pythia_db::session::cell`).
fn thick_cell(value: &oracle::SqlValue) -> Option<String> {
    match value.is_null() {
        Ok(true) => None,
        Ok(false) => Some(value.get::<String>().unwrap_or_else(|_| value.to_string())),
        Err(e) => Some(format!("<{e}>")),
    }
}

/// A candidate formatter for thin: by the column's type, from the driver's
/// value types (there's no generic value to ask for text).
fn thin_cell(row: &oracledb::Row, i: usize, column: &oracledb::Metadata) -> Option<String> {
    use oracledb::*;
    let t = column.db_type();
    let text =
        |r: Result<Option<String>, Error>| r.unwrap_or_else(|e| Some(format!("<{}>", err(e))));
    if t == DB_TYPE_NUMBER || t == DB_TYPE_BINARY_INTEGER {
        text(
            row.get::<Option<OracleNumber>>(i)
                .map(|v| v.map(|n| n.to_string())),
        )
    } else if t == DB_TYPE_BINARY_FLOAT {
        text(row.get::<Option<f32>>(i).map(|v| v.map(|n| n.to_string())))
    } else if t == DB_TYPE_BINARY_DOUBLE {
        text(row.get::<Option<f64>>(i).map(|v| v.map(|n| n.to_string())))
    } else if t == DB_TYPE_DATE
        || t == DB_TYPE_TIMESTAMP
        || t == DB_TYPE_TIMESTAMP_TZ
        || t == DB_TYPE_TIMESTAMP_LTZ
    {
        let fraction = t != DB_TYPE_DATE;
        let zone = t == DB_TYPE_TIMESTAMP_TZ;
        text(
            row.get::<Option<OracleTimestamp>>(i)
                .map(|v| v.map(|ts| timestamp(&ts, fraction, zone))),
        )
    } else if t == DB_TYPE_INTERVAL_DS {
        text(
            row.get::<Option<OracleIntervalDS>>(i)
                .map(|v| v.map(|n| n.to_string())),
        )
    } else if t == DB_TYPE_INTERVAL_YM {
        text(
            row.get::<Option<OracleIntervalYM>>(i)
                .map(|v| v.map(|n| n.to_string())),
        )
    } else if t == DB_TYPE_RAW || t == DB_TYPE_LONG_RAW || t == DB_TYPE_BLOB {
        text(
            row.get::<Option<Vec<u8>>>(i)
                .map(|v| v.map(|b| b.iter().map(|x| format!("{x:02X}")).collect())),
        )
    } else if t == DB_TYPE_BOOLEAN {
        text(
            row.get::<Option<bool>>(i)
                .map(|v| v.map(|b| if b { "TRUE" } else { "FALSE" }.to_owned())),
        )
    } else if t == DB_TYPE_JSON {
        text(
            row.get::<Option<JsonValue>>(i)
                .map(|v| v.map(|j| format!("{j:?}"))),
        )
    } else if t == DB_TYPE_VECTOR {
        text(
            row.get::<Option<Vector>>(i)
                .map(|v| v.map(|j| format!("{j:?}"))),
        )
    } else {
        // Strings, CLOB/NCLOB, LONG, ROWID/UROWID.
        text(row.get::<Option<String>>(i))
    }
}

/// `YYYY-MM-DD HH24:MI:SS[.FF][ ±TZH:TZM]`, as the thick driver shows it.
fn timestamp(ts: &oracledb::OracleTimestamp, fraction: bool, zone: bool) -> String {
    let s = ts.to_string(); // 2026-10-22T13:05:06.123456789Z or …+02:00
    let (date_time, tz) = match s.rfind(['Z', '+', '-']).filter(|&p| p > 10) {
        Some(p) => (&s[..p], &s[p..]),
        None => (s.as_str(), ""),
    };
    let mut out = date_time.replacen('T', " ", 1);
    if !fraction && let Some(dot) = out.find('.') {
        out.truncate(dot);
    }
    if zone {
        out.push(' ');
        out.push_str(if tz == "Z" { "+00:00" } else { tz });
    }
    out
}

/// One expression per type, each in its own query, so a type one driver
/// can't fetch doesn't hide the others.
const TYPES: &[(&str, &str)] = &[
    ("n_big", "123456789012345678901234567890"),
    ("n_neg_frac", "-0.000123"),
    ("n_third", "1/3"),
    ("n_scaled", "cast(12.5 as number(8,2))"),
    ("n_float", "cast(1.25 as float(10))"),
    ("bin_float", "to_binary_float(1.5)"),
    ("bin_double", "to_binary_double(1/3)"),
    ("dt", "date '2026-10-22' + 13/24 + 5/1440"),
    ("ts9", "timestamp '2026-10-22 13:05:06.123456789'"),
    ("tstz_offset", "timestamp '2026-10-22 13:05:06 +02:00'"),
    (
        "tsltz",
        "cast(timestamp '2026-10-22 13:05:06' as timestamp with local time zone)",
    ),
    ("ids", "interval '3 04:05:06.789' day to second"),
    ("iym", "interval '2-3' year to month"),
    ("raw_v", "hextoraw('DEADBEEF')"),
    ("rid", "chartorowid('AAAAB0AABAAAAOhAAA')"),
    ("chr", "cast('x' as char(3))"),
    ("nvc", "n'ünï'"),
    ("clob_v", "to_clob('clob text')"),
    ("nclob_v", "to_nclob('nclob ünï')"),
    ("blob_v", "to_blob(hextoraw('CAFE'))"),
    ("json_v", "json('{\"a\":1,\"b\":[true,null]}')"),
    ("vector_v", "to_vector('[1,2,3]')"),
    ("bool_v", "true"),
    ("null_n", "cast(null as number)"),
];

/// `sql`'s first row through both drivers: which columns differ.
fn compare_types(sql: &str) -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let stmt = conn.statement(sql).map_err(err)?.build().map_err(err)?;
    let mut cursor = stmt.query(&[]).map_err(err)?;
    let columns: Vec<_> = cursor.columns().to_vec();
    let row = cursor.next().ok_or("no row")?.map_err(err)?;
    let thin_values: Vec<(String, String, Option<String>)> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| (c.name().to_owned(), c.data_type(), thin_cell(&row, i, c)))
        .collect();

    let thick_values: Vec<(String, Option<String>)> =
        match thick("PYTHIA_TEST_DB").and_then(|tconn| {
            let rows = tconn.query(sql, &[]).map_err(|e| e.to_string())?;
            let types: Vec<String> = rows
                .column_info()
                .iter()
                .map(|c| c.oracle_type().to_string())
                .collect();
            let trow = rows
                .into_iter()
                .next()
                .ok_or("no row")?
                .map_err(|e| e.to_string())?;
            Ok(types
                .into_iter()
                .zip(trow.sql_values().iter().map(thick_cell))
                .collect())
        }) {
            Ok(v) => v,
            Err(e) => vec![("—".into(), Some(format!("<thick: {e}>"))); thin_values.len()],
        };

    let show = |v: &Option<String>| v.clone().unwrap_or_else(|| "NULL".into());
    let mut lines = Vec::new();
    for ((name, thin_type, thin), (thick_type, thick)) in thin_values.iter().zip(&thick_values) {
        let mark = if thin == thick && thin_type == thick_type {
            "="
        } else {
            "≠"
        };
        lines.push(format!(
            "{mark} {name}: thick `{}` {thick_type} / thin `{}` {thin_type}",
            show(thick),
            show(thin)
        ));
    }
    Ok(lines.join("\n"))
}

fn types() -> Check {
    let mut lines = Vec::new();
    for (name, expr) in TYPES {
        lines.push(
            compare_types(&format!("select {expr} {name} from dual"))
                .unwrap_or_else(|e| format!("≠ {name}: {e}")),
        );
    }
    let same = lines.iter().filter(|l| l.starts_with('=')).count();
    Ok(format!(
        "{same} of {} identical (value and type name)\n{}",
        lines.len(),
        lines.join("\n")
    ))
}

/// A TIMESTAMP WITH TIME ZONE with a region name (Scheduler jobs have them).
fn tstz_region() -> Check {
    let result = std::panic::catch_unwind(|| {
        compare_types(
            "select timestamp '2026-10-22 13:05:06.5 Europe/Zurich' tstz_region from dual",
        )
    });
    match result {
        Ok(r) => r,
        Err(panic) => {
            let message = panic
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(format!("the driver panicked: {message}"))
        }
    }
}

/// A TIMESTAMP WITH TIME ZONE with an offset as the driver hands it over:
/// its `Display`, next to the UTC time and the local time Oracle reports.
fn tstz_value() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let row = conn
        .query_row(
            "select ts, sys_extract_utc(ts), to_char(ts, 'YYYY-MM-DD HH24:MI:SS TZH:TZM') from (select timestamp '2026-10-22 13:05:06 +02:00' ts from dual)",
            &[],
        )
        .map_err(err)?;
    let ts: oracledb::OracleTimestamp = row.get(0).map_err(err)?;
    let utc: oracledb::OracleTimestamp = row.get(1).map_err(err)?;
    let text: String = row.get(2).map_err(err)?;
    Ok(format!(
        "OracleTimestamp {ts} ({ts:?}); sys_extract_utc {utc}; to_char {text}"
    ))
}

fn long_column() -> Check {
    compare_types(
        "select text, text_length from all_views where owner = 'SYS' and view_name = 'ALL_SYNONYMS'",
    )
}

fn batch_error() -> Check {
    let conn = thin("PYTHIA_TEST_DB")?;
    let _ = conn.execute("drop table pythia_probe_b purge", &[]);
    conn.execute(
        "create table pythia_probe_b (n number primary key, s varchar2(5))",
        &[],
    )
    .map_err(err)?;
    let result = (|| {
        let rows: [&[&dyn oracledb::ToDbValue]; 4] =
            [&[&1, &"a"], &[&2, &"b"], &[&2, &"dup"], &[&3, &"toolong"]];
        let outcome =
            match conn.execute_batch("insert into pythia_probe_b values (:1, :2)", &rows[..]) {
                Ok(r) => format!("Ok, {} rows affected", r.rows_affected()),
                Err(e) => err(e),
            };
        let inserted = one(&conn, "select count(*) from pythia_probe_b")?;
        conn.rollback().map_err(err)?;
        Ok(format!(
            "{outcome}; rows in the table before rollback: {inserted}"
        ))
    })();
    let _ = conn.execute("drop table pythia_probe_b purge", &[]);
    result
}

// ----- cancel ----------------------------------------------------------

/// Runs long work for minutes unless stopped.
const LONG_SQL: &str = "select count(*) from all_objects a, all_objects b";
const LONG_SLEEP: &str = "begin dbms_session.sleep(20); end;";

/// What a call on another thread reports, and how long it took.
struct Outcome {
    result: String,
    after: Duration,
    usable: String,
}

/// Starts `sql` on a thin connection of its own (with `timeout` as its call
/// timeout), returns (sid, serial#) and a receiver for the outcome.
fn run_thin(
    sql: &'static str,
    timeout: Option<Duration>,
) -> Result<((usize, usize), mpsc::Receiver<Outcome>), String> {
    let (ids_tx, ids_rx) = mpsc::channel();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let conn = match thin("PYTHIA_TEST_DB") {
            Ok(c) => c,
            Err(e) => {
                let _ = ids_tx.send(Err(e));
                return;
            }
        };
        let _ = conn.set_call_timeout(timeout);
        let ids = (
            conn.session_id().unwrap_or(0),
            conn.serial_num().unwrap_or(0),
        );
        let _ = ids_tx.send(Ok(ids));
        let start = Instant::now();
        let result = match conn.execute(sql, &[]) {
            Ok(_) => "finished normally".to_owned(),
            Err(e) => err(e),
        };
        let after = start.elapsed();
        let _ = conn.set_call_timeout(Some(Duration::from_secs(5)));
        let usable = match conn
            .ping()
            .and_then(|_| conn.query_row("select 1 from dual", &[]).map(drop))
        {
            Ok(()) => "connection still usable".to_owned(),
            Err(e) => format!("connection unusable: {}", err(e)),
        };
        let _ = tx.send(Outcome {
            result,
            after,
            usable,
        });
    });
    let ids = ids_rx.recv().map_err(|e| e.to_string())??;
    Ok((ids, rx))
}

fn wait(rx: &mpsc::Receiver<Outcome>, limit: Duration) -> String {
    match rx.recv_timeout(limit) {
        Ok(o) => format!(
            "after {:.1} s: {}; {}",
            o.after.as_secs_f32(),
            o.result,
            o.usable
        ),
        Err(_) => format!("still running after {} s", limit.as_secs()),
    }
}

/// Today: `break_execution` from another thread (OCIBreak).
fn cancel_thick_baseline() -> Check {
    let (tx, rx) = mpsc::channel();
    let conn = std::sync::Arc::new(thick("PYTHIA_TEST_DB")?);
    let worker = conn.clone();
    thread::spawn(move || {
        let start = Instant::now();
        let result = match worker.query_row(LONG_SQL, &[]) {
            Ok(_) => "finished normally".to_owned(),
            Err(e) => e.to_string(),
        };
        let usable = match worker.query_row("select 1 from dual", &[]) {
            Ok(_) => "connection still usable".to_owned(),
            Err(e) => format!("connection unusable: {e}"),
        };
        let _ = tx.send(Outcome {
            result,
            after: start.elapsed(),
            usable,
        });
    });
    thread::sleep(Duration::from_millis(1500));
    conn.break_execution().map_err(|e| e.to_string())?;
    Ok(format!(
        "break_execution after 1.5 s → {}",
        wait(&rx, Duration::from_secs(30))
    ))
}

fn cancel_call_timeout_sleep() -> Check {
    let (_, rx) = run_thin(LONG_SLEEP, Some(Duration::from_secs(2)))?;
    Ok(format!(
        "call timeout 2 s on dbms_session.sleep(20) → {}",
        wait(&rx, Duration::from_secs(40))
    ))
}

fn cancel_call_timeout_sql() -> Check {
    let (_, rx) = run_thin(LONG_SQL, Some(Duration::from_secs(2)))?;
    Ok(format!(
        "call timeout 2 s on a long query → {}",
        wait(&rx, Duration::from_secs(40))
    ))
}

fn alter_system(what: &str, ids: (usize, usize)) -> Result<(), String> {
    let dba = thin("PYTHIA_TEST_DB_DBA")?;
    dba.execute(&format!("alter system {what} '{}, {}'", ids.0, ids.1), &[])
        .map_err(err)?;
    Ok(())
}

fn kill(ids: (usize, usize)) {
    let _ = thin("PYTHIA_TEST_DB_DBA").and_then(|dba| {
        dba.execute(
            &format!("alter system kill session '{}, {}' immediate", ids.0, ids.1),
            &[],
        )
        .map_err(err)
    });
}

fn cancel_from_dba(sql: &'static str, label: &str) -> Check {
    let (ids, rx) = run_thin(sql, None)?;
    thread::sleep(Duration::from_millis(1500));
    alter_system("cancel sql", ids)?;
    let seen = wait(&rx, Duration::from_secs(30));
    if seen.starts_with("still running") {
        kill(ids);
    }
    Ok(format!(
        "alter system cancel sql after 1.5 s on {label} → {seen}"
    ))
}

fn cancel_sql_from_dba() -> Check {
    cancel_from_dba(LONG_SQL, "a long query")
}

fn cancel_sleep_from_dba() -> Check {
    cancel_from_dba(LONG_SLEEP, "dbms_session.sleep(20)")
}

fn kill_session() -> Check {
    let (ids, rx) = run_thin(LONG_SQL, None)?;
    thread::sleep(Duration::from_millis(1500));
    let dba = thin("PYTHIA_TEST_DB_DBA")?;
    // ORA-00031 ("marked for kill") is the usual answer, not a failure.
    let killed = match dba.execute(
        &format!("alter system kill session '{}, {}' immediate", ids.0, ids.1),
        &[],
    ) {
        Ok(_) => "killed".to_owned(),
        Err(e) => err(e),
    };
    Ok(format!(
        "kill session after 1.5 s on a long query ({killed}) → {}",
        wait(&rx, Duration::from_secs(30))
    ))
}

// ----- debugger, versions ------------------------------------------------

/// `DBMS_DEBUG_JDWP.CONNECT_TCP` from a thin session: the database connects
/// to a listener here and does the JDWP handshake.
fn debugger_connect() -> Check {
    let host = std::env::var("PYTHIA_DEBUG_HOST").map_err(|_| "PYTHIA_DEBUG_HOST not set")?;
    let listener = TcpListener::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let outcome = (|| -> std::io::Result<String> {
            let (mut socket, from) = listener.accept()?;
            let mut handshake = [0u8; 14];
            socket.read_exact(&mut handshake)?;
            socket.write_all(&handshake)?;
            Ok(format!(
                "{} from {from}",
                String::from_utf8_lossy(&handshake)
            ))
        })();
        let _ = tx.send(outcome.map_err(|e| e.to_string()));
    });
    let conn = thin("PYTHIA_TEST_DB")?;
    conn.set_call_timeout(Some(Duration::from_secs(15)))
        .map_err(err)?;
    let connected = conn.execute(
        "begin dbms_debug_jdwp.connect_tcp(:1, :2); end;",
        &[&host, &(port as i32)],
    );
    let handshake = rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| Err("no connection".into()));
    let connected = connected.map(|_| "connect_tcp returned").map_err(err);
    let _ = conn.execute("begin dbms_debug_jdwp.disconnect; end;", &[]);
    Ok(format!("{connected:?}; listener saw {handshake:?}"))
}

/// Connects to each of `PYTHIA_PROBE_DBS` (`label=user/pw@db;…`), thin and
/// thick.
fn versions() -> Check {
    let Ok(list) = std::env::var("PYTHIA_PROBE_DBS") else {
        return Ok("PYTHIA_PROBE_DBS not set".into());
    };
    let mut lines = Vec::new();
    for entry in list.split(';').filter(|e| !e.is_empty()) {
        let (label, spec) = entry.split_once('=').ok_or("label=user/pw@db")?;
        let (user, pw, db) = split(spec);
        let thin = config(&user, &pw, &db)
            .and_then(|c| oracledb::connect(c).map_err(err))
            .and_then(|conn| {
                let version = conn.version().map_err(err)?;
                let probe = one(&conn, "select 'query ok' from dual")?;
                Ok(format!("{version}, {probe}"))
            });
        let thick = oracle::Connection::connect(&user, &pw, &db)
            .map(|c| {
                c.server_version()
                    .map(|(v, _)| v.to_string())
                    .unwrap_or_default()
            })
            .map_err(|e| e.to_string());
        lines.push(format!(
            "{label}: thin {} / thick {}",
            show(thin),
            show(thick)
        ));
    }
    Ok(lines.join("\n"))
}

fn show(r: Result<String, String>) -> String {
    match r {
        Ok(s) => s,
        Err(e) => format!("FAILED {e}"),
    }
}

/// The Instant Client connecting, for comparison (e.g. with Native Network
/// Encryption required by the server).
fn thick_connect() -> Check {
    let conn = thick("PYTHIA_TEST_DB")?;
    let banner: Option<String> = conn
        .query_row_as(
            "select listagg(network_service_banner, '; ') within group (order by 1) from v$session_connect_info where sid = sys_context('userenv', 'sid')",
            &[],
        )
        .unwrap_or(None);
    Ok(banner.unwrap_or_else(|| "connected (no v$session_connect_info access)".into()))
}
