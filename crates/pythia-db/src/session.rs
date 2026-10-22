use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use crate::oracle::sql_type::OracleType;
use crate::oracle::{Connection, Connector, ResultSet, Row, StatementType};
use pythia_config::Privilege;
use tracing::{debug, info, warn};

use crate::meta;
use crate::types::{
    Cell, Column, Compiled, ConnectParams, DbError, DbEvent, DbRequest, ExecSummary, ServerInfo,
};

/// Handle to a database session running on its own worker thread.
pub struct Session {
    requests: Sender<DbRequest>,
    events: Receiver<DbEvent>,
    /// Shared with the worker so a running call can be interrupted.
    connection: Arc<Mutex<Option<Arc<Connection>>>>,
}

impl Session {
    /// Starts connecting in the background. `notify` is called whenever a new
    /// event is available (typically `egui::Context::request_repaint`).
    pub fn connect(params: ConnectParams, notify: impl Fn() + Send + 'static) -> Self {
        let (req_tx, req_rx) = mpsc::channel();
        let (ev_tx, ev_rx) = mpsc::channel();
        let connection = Arc::new(Mutex::new(None));
        let shared = connection.clone();
        thread::Builder::new()
            .name(format!("db-{}", params.username))
            .spawn(move || {
                let events = Events {
                    tx: ev_tx,
                    notify: Box::new(notify),
                };
                Worker::run(params, req_rx, events, shared);
            })
            .expect("spawning db worker thread");
        Self {
            requests: req_tx,
            events: ev_rx,
            connection,
        }
    }

    pub fn send(&self, request: DbRequest) {
        // The worker only goes away after `Disconnect` or a failed connect;
        // requests sent after that are meaningless anyway.
        let _ = self.requests.send(request);
    }

    pub fn try_recv(&self) -> Option<DbEvent> {
        self.events.try_recv().ok()
    }

    /// Interrupts the statement currently running on this session (OCIBreak).
    pub fn cancel(&self) {
        let conn = self.connection.lock().expect("connection mutex").clone();
        if let Some(conn) = conn
            && let Err(e) = conn.break_execution()
        {
            warn!("cancel failed: {e}");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.send(DbRequest::Disconnect);
    }
}

struct Events {
    tx: Sender<DbEvent>,
    notify: Box<dyn Fn() + Send>,
}

impl Events {
    fn send(&self, event: DbEvent) {
        if self.tx.send(event).is_ok() {
            (self.notify)();
        }
    }
}

/// The open result set of the last query, kept for "fetch more".
struct OpenQuery {
    id: u64,
    rows: ResultSet<'static, Row>,
}

struct Worker {
    conn: Arc<Connection>,
    events: Events,
    autocommit: bool,
    open_query: Option<OpenQuery>,
}

impl Worker {
    fn run(
        params: ConnectParams,
        requests: Receiver<DbRequest>,
        events: Events,
        shared: Arc<Mutex<Option<Arc<Connection>>>>,
    ) {
        info!(?params, "connecting");
        let conn = match connect(&params) {
            Ok(conn) => Arc::new(conn),
            Err(e) => {
                let error = DbError::from(e);
                warn!("connect failed: {error}");
                events.send(DbEvent::ConnectFailed(error));
                return;
            }
        };
        *shared.lock().expect("connection mutex") = Some(conn.clone());

        let mut worker = Worker {
            conn,
            events,
            autocommit: false,
            open_query: None,
        };
        match worker.on_connect() {
            Ok(server) => {
                worker.events.send(DbEvent::Connected(server));
                for sql in &params.init_sql {
                    if let Err(e) = worker.conn.execute(sql, &[]) {
                        let e = DbError::from(e);
                        worker.events.send(DbEvent::Notice(format!(
                            "Login script: {e} ({})",
                            sql.lines().next().unwrap_or_default()
                        )));
                    }
                }
            }
            Err(e) => worker.events.send(DbEvent::ConnectFailed(e)),
        }

        while let Ok(request) = requests.recv() {
            debug!(?request, "db request");
            match request {
                DbRequest::Execute {
                    id,
                    sql,
                    max_rows,
                    binds,
                } => worker.execute(id, &sql, max_rows, &binds),
                DbRequest::Meta { id, query } => {
                    let started = Instant::now();
                    let result = meta::run(&worker.conn, &query);
                    debug!(
                        id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        ok = result.is_ok(),
                        ?query,
                        "metadata query done"
                    );
                    worker.events.send(DbEvent::Meta { id, result });
                }
                DbRequest::FetchMore { id, max_rows } => worker.fetch_more(id, max_rows),
                DbRequest::Commit => worker.end_transaction(true),
                DbRequest::Rollback => worker.end_transaction(false),
                DbRequest::SetAutocommit(on) => worker.autocommit = on,
                DbRequest::Task { id, task } => {
                    let started = Instant::now();
                    let result = task(&worker.conn);
                    debug!(
                        id,
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        ok = result.is_ok(),
                        "task done"
                    );
                    worker.events.send(DbEvent::TaskDone { id, result });
                    // Tasks (data edits, debug runs, …) can open a transaction.
                    worker.report_transaction();
                }
                DbRequest::Disconnect => break,
            }
        }

        worker.open_query = None;
        *shared.lock().expect("connection mutex") = None;
        if let Err(e) = worker.conn.close() {
            warn!("closing connection: {e}");
        }
        info!("disconnected");
        worker.events.send(DbEvent::Disconnected);
    }

    fn on_connect(&self) -> Result<ServerInfo, DbError> {
        let (version, banner) = self.conn.server_version()?;
        self.conn.set_module("pythia")?;
        self.conn
            .execute("begin dbms_output.enable(null); end;", &[])?;
        Ok(ServerInfo {
            version: version.to_string(),
            release: (version.major(), version.minor()),
            banner,
            current_schema: current_schema(&self.conn)?,
        })
    }

    fn execute(&mut self, id: u64, sql: &str, max_rows: usize, binds: &[(String, Option<String>)]) {
        self.open_query = None;
        let started = Instant::now();
        let result = self.execute_inner(id, sql, max_rows, binds, started);
        if let Err(error) = result {
            self.events.send(DbEvent::Failed {
                id,
                error,
                elapsed: started.elapsed(),
            });
        }
        self.after_statement();
    }

    fn execute_inner(
        &mut self,
        id: u64,
        sql: &str,
        max_rows: usize,
        binds: &[(String, Option<String>)],
        started: Instant,
    ) -> Result<(), DbError> {
        let named: Vec<(&str, &dyn crate::oracle::sql_type::ToSql)> = binds
            .iter()
            .map(|(name, value)| (name.as_str(), value as &dyn crate::oracle::sql_type::ToSql))
            .collect();
        let mut stmt = self.conn.statement(sql).build()?;
        if stmt.is_query() {
            drop(stmt);
            let rows = self.conn.query_named(sql, &named)?;
            let columns = rows
                .column_info()
                .iter()
                .map(|c| column(c.name(), c.oracle_type()))
                .collect();
            self.events.send(DbEvent::QueryStarted { id, columns });
            self.open_query = Some(OpenQuery { id, rows });
            self.fetch(max_rows, started);
            return Ok(());
        }

        stmt.execute_named(&named)?;
        let kind = stmt.statement_type();
        let rows_affected = stmt.is_dml().then(|| stmt.row_count()).transpose()?;
        if self.autocommit && stmt.is_dml() {
            self.conn.commit()?;
        }
        let compiled = match kind {
            StatementType::Create => self.compiled(sql)?,
            _ => None,
        };
        let summary = ExecSummary {
            kind: statement_kind_label(&kind, sql),
            rows_affected,
            compiled,
        };
        self.events.send(DbEvent::Executed {
            id,
            summary,
            elapsed: started.elapsed(),
        });
        Ok(())
    }

    /// After `CREATE` of a stored unit, reads its compile errors.
    fn compiled(&self, sql: &str) -> Result<Option<Compiled>, DbError> {
        let Some(header) = pythia_plsql::unit_header(sql) else {
            return Ok(None);
        };
        let owner = match header.owner {
            Some(owner) => owner,
            None => current_schema(&self.conn)?,
        };
        let errors = meta::errors(&self.conn, &owner, &header.name, &header.object_type)?;
        Ok(Some(Compiled {
            owner,
            name: header.name,
            object_type: header.object_type,
            errors,
        }))
    }

    fn fetch_more(&mut self, id: u64, max_rows: usize) {
        if self.open_query.as_ref().is_some_and(|q| q.id == id) {
            self.fetch(max_rows, Instant::now());
        }
    }

    /// Fetches up to `max_rows` from the open query and reports them.
    fn fetch(&mut self, max_rows: usize, started: Instant) {
        let Some(query) = self.open_query.as_mut() else {
            return;
        };
        let id = query.id;
        let mut rows = Vec::with_capacity(max_rows.min(10_000));
        let mut more = true;
        while rows.len() < max_rows {
            match query.rows.next() {
                Some(Ok(row)) => rows.push(row.sql_values().iter().map(cell).collect()),
                Some(Err(e)) => {
                    self.open_query = None;
                    self.events.send(DbEvent::Failed {
                        id,
                        error: e.into(),
                        elapsed: started.elapsed(),
                    });
                    return;
                }
                None => {
                    more = false;
                    break;
                }
            }
        }
        if !more {
            self.open_query = None;
        }
        self.events.send(DbEvent::Rows {
            id,
            rows,
            more,
            elapsed: started.elapsed(),
        });
    }

    fn end_transaction(&mut self, commit: bool) {
        let result = if commit {
            self.conn.commit()
        } else {
            self.conn.rollback()
        };
        match result {
            Ok(()) => self.events.send(DbEvent::Notice(
                if commit { "Committed" } else { "Rolled back" }.into(),
            )),
            Err(e) => self.events.send(DbEvent::Error(e.into())),
        }
        self.after_statement();
    }

    /// Collects DBMS_OUTPUT and refreshes the transaction state.
    fn after_statement(&self) {
        match self.read_output() {
            Ok(lines) if !lines.is_empty() => self.events.send(DbEvent::Output(lines)),
            Ok(_) => {}
            Err(e) => warn!("reading dbms_output: {e}"),
        }
        self.report_transaction();
    }

    /// Sends whether the session has an open transaction.
    fn report_transaction(&self) {
        match self.conn.query_row_as::<Option<String>>(
            "select dbms_transaction.local_transaction_id from dual",
            &[],
        ) {
            Ok(tx) => self.events.send(DbEvent::Transaction {
                pending: tx.is_some(),
            }),
            Err(e) => warn!("reading transaction state: {e}"),
        }
    }

    fn read_output(&self) -> Result<Vec<String>, DbError> {
        let mut stmt = self
            .conn
            .statement("begin dbms_output.get_line(:1, :2); end;")
            .build()?;
        let mut lines = Vec::new();
        loop {
            stmt.execute(&[&OracleType::Varchar2(32767), &OracleType::Number(0, 0)])?;
            let status: i32 = stmt.bind_value(2)?;
            if status != 0 {
                return Ok(lines);
            }
            lines.push(stmt.bind_value::<_, Option<String>>(1)?.unwrap_or_default());
        }
    }
}

/// A connection of its own (not behind a [`Session`]), e.g. for a task that
/// works in parallel. Runs the login statements; their errors don't fail it.
pub fn open_connection(params: &ConnectParams) -> Result<Connection, DbError> {
    let conn = connect(params)?;
    conn.set_module("pythia")?;
    for sql in &params.init_sql {
        if let Err(e) = conn.execute(sql, &[]) {
            warn!("login script on a worker session: {e}");
        }
    }
    Ok(conn)
}

fn connect(params: &ConnectParams) -> crate::oracle::Result<Connection> {
    let mut connector = Connector::new(&params.username, &params.password, &params.connect_string);
    let privilege = match params.privilege {
        Privilege::Normal => None,
        Privilege::Sysdba => Some(crate::oracle::Privilege::Sysdba),
        Privilege::Sysoper => Some(crate::oracle::Privilege::Sysoper),
        Privilege::Sysbackup => Some(crate::oracle::Privilege::Sysbackup),
    };
    if let Some(privilege) = privilege {
        connector.privilege(privilege);
    }
    // An empty username with a connect string means external auth (`/@alias`).
    if params.username.is_empty() {
        connector.external_auth(true);
    }
    connector.connect()
}

/// Runs a query and hands its rows to `sink` in batches, without keeping
/// them (e.g. to stream a large result into a file). The first call has no
/// rows, so the columns are known even for an empty result. Returns the row
/// count.
pub fn stream_query(
    conn: &Connection,
    sql: &str,
    batch: usize,
    mut sink: impl FnMut(&[Column], &[Vec<Cell>]) -> Result<(), DbError>,
) -> Result<u64, DbError> {
    let mut stmt = conn.statement(sql).fetch_array_size(batch as u32).build()?;
    let rows = stmt.query(&[])?;
    let columns: Vec<Column> = rows
        .column_info()
        .iter()
        .map(|c| column(c.name(), c.oracle_type()))
        .collect();
    sink(&columns, &[])?;
    let mut buffer = Vec::with_capacity(batch);
    let mut count = 0u64;
    for row in rows {
        let row = row?;
        buffer.push(row.sql_values().iter().map(cell).collect());
        if buffer.len() == batch {
            count += buffer.len() as u64;
            sink(&columns, &buffer)?;
            buffer.clear();
        }
    }
    count += buffer.len() as u64;
    if !buffer.is_empty() {
        sink(&columns, &buffer)?;
    }
    Ok(count)
}

fn column(name: &str, ty: &OracleType) -> Column {
    let numeric = matches!(
        ty,
        OracleType::Number(..)
            | OracleType::Float(_)
            | OracleType::BinaryFloat
            | OracleType::BinaryDouble
            | OracleType::Int64
            | OracleType::UInt64
    );
    Column {
        name: name.to_owned(),
        type_name: ty.to_string(),
        numeric,
    }
}

fn cell(value: &crate::oracle::SqlValue) -> Cell {
    match value.is_null() {
        Ok(true) => None,
        Ok(false) => Some(value.get::<String>().unwrap_or_else(|_| value.to_string())),
        Err(e) => Some(format!("<{e}>")),
    }
}

fn statement_kind_label(kind: &StatementType, sql: &str) -> String {
    match kind {
        StatementType::Begin | StatementType::Declare => "PL/SQL block".into(),
        // Fall back to the leading keyword(s), e.g. `CREATE`, `GRANT`, `TRUNCATE`.
        _ => sql
            .split_whitespace()
            .next()
            .unwrap_or("Statement")
            .to_uppercase(),
    }
}

/// The session's current schema. `Connection::current_schema` only reports a
/// value after it was set explicitly, so ask the database.
fn current_schema(conn: &Connection) -> crate::oracle::Result<String> {
    conn.query_row_as(
        "select sys_context('USERENV', 'CURRENT_SCHEMA') from dual",
        &[],
    )
}
