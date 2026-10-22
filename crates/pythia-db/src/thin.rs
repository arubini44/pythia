//! A stand-in for the `oracle` crate (ODPI-C, which needs the Instant
//! Client) on top of the pure-Rust `oracledb` driver, for the spike in #13.
//! It has the part of the `oracle` API Pythia uses, under the same names, so
//! the workspace builds unchanged with `--features pythia-db/thin`.
//!
//! Known gaps (see #13): cancel (`break_execution`) isn't possible, no
//! external authentication or proxy logins (#23), LOB columns are typed
//! LONG, values with a time zone region make the driver panic (caught here
//! and shown as an error), and batch errors are found row by row.

use std::fmt;
use std::marker::PhantomData;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, OnceLock};

use oracledb::{
    DbType, ErrorKind as ThinKind, JsonValue, Metadata, OracleIntervalDS, OracleIntervalYM,
    OracleNumber, OracleTimestamp, ToDbValue, Vector, VectorData,
};

/// The `oracle::sql_type` module.
pub mod sql_type {
    pub use super::{FromSql, OracleType, ToSql};
}

pub type Result<T> = std::result::Result<T, Error>;

// ----- errors ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    OciError,
    NoDataFound,
    NullValue,
    InvalidTypeConversion,
    BatchErrors,
    NotSupported,
    Other,
}

/// An error reported by the database.
#[derive(Debug, Clone)]
pub struct DbError {
    code: i32,
    offset: u32,
    message: String,
}

impl DbError {
    pub fn code(&self) -> i32 {
        self.code
    }

    pub fn offset(&self) -> u32 {
        self.offset
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    message: String,
    db: Option<DbError>,
    batch: Option<Vec<DbError>>,
}

impl Error {
    fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            db: None,
            batch: None,
        }
    }

    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    pub fn db_error(&self) -> Option<&DbError> {
        self.db.as_ref()
    }

    /// Per-row errors of a batch run with `with_batch_errors`; each error's
    /// offset is its row in the batch.
    pub fn batch_errors(&self) -> Option<&Vec<DbError>> {
        self.batch.as_ref()
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.db {
            Some(db) => f.write_str(&db.message),
            None => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for Error {}

impl From<oracledb::Error> for Error {
    fn from(e: oracledb::Error) -> Self {
        match e.kind() {
            ThinKind::DbError(db) => Self {
                kind: ErrorKind::OciError,
                message: db.message().to_owned(),
                db: Some(DbError {
                    code: db.code() as i32,
                    offset: db.offset() as u32,
                    message: db.message().to_owned(),
                }),
                batch: None,
            },
            ThinKind::NoDataFound => Self::new(ErrorKind::NoDataFound, "no data found"),
            // As ORA-03113, which Pythia takes for a lost connection.
            ThinKind::DeadConnection | ThinKind::NotConnected | ThinKind::UnableToRecover => Self {
                kind: ErrorKind::OciError,
                message: e.to_string(),
                db: Some(DbError {
                    code: 3113,
                    offset: 0,
                    message: format!("ORA-03113: {e}"),
                }),
                batch: None,
            },
            _ => Self::new(ErrorKind::Other, e.to_string()),
        }
    }
}

fn unsupported(what: &str) -> Error {
    Error::new(
        ErrorKind::NotSupported,
        format!("{what} isn't supported by the thin driver"),
    )
}

// ----- connections -------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    Sysdba,
    Sysoper,
    Sysbackup,
}

/// Connection options (`oracle::Connector`).
pub struct Connector {
    user: String,
    password: String,
    connect_string: String,
    privilege: Option<Privilege>,
    external_auth: bool,
}

impl Connector {
    pub fn new(
        user: impl AsRef<str>,
        password: impl AsRef<str>,
        connect_string: impl AsRef<str>,
    ) -> Self {
        Self {
            user: user.as_ref().to_owned(),
            password: password.as_ref().to_owned(),
            connect_string: connect_string.as_ref().to_owned(),
            privilege: None,
            external_auth: false,
        }
    }

    pub fn privilege(&mut self, privilege: Privilege) -> &mut Self {
        self.privilege = Some(privilege);
        self
    }

    pub fn external_auth(&mut self, on: bool) -> &mut Self {
        self.external_auth = on;
        self
    }

    pub fn connect(&self) -> Result<Connection> {
        if self.external_auth {
            return Err(unsupported(
                "External authentication (OS or wallet, `/@alias`)",
            ));
        }
        let mut config = oracledb::Config::default()
            .set_credentials(&self.user, &self.password)
            .set_connect_string(&self.connect_string)?;
        if let Some(privilege) = self.privilege {
            config = config.set_auth_mode(match privilege {
                Privilege::Sysdba => oracledb::AUTH_MODE_SYSDBA,
                Privilege::Sysoper => oracledb::AUTH_MODE_SYSOPER,
                Privilege::Sysbackup => oracledb::AUTH_MODE_SYSBKP,
            });
        }
        let inner = oracledb::connect(config)?;
        // TIMESTAMP WITH LOCAL TIME ZONE values arrive in UTC; they are
        // shown in the session's time zone (its offset now).
        let ltz_offset = inner
            .query_row(
                "select extract(timezone_hour from current_timestamp) * 60 \
                 + extract(timezone_minute from current_timestamp) from dual",
                &[],
            )
            .and_then(|row| row.get::<i32>(0))
            .unwrap_or(0);
        Ok(Connection {
            inner,
            ltz_offset,
            banner: OnceLock::new(),
        })
    }
}

pub struct Connection {
    inner: oracledb::Connection,
    /// The session time zone's offset in minutes, for TSLTZ values.
    ltz_offset: i32,
    banner: OnceLock<String>,
}

impl Connection {
    pub fn connect(
        user: impl AsRef<str>,
        password: impl AsRef<str>,
        connect_string: impl AsRef<str>,
    ) -> Result<Connection> {
        Connector::new(user, password, connect_string).connect()
    }

    pub fn statement(&self, sql: &str) -> StatementBuilder<'_> {
        StatementBuilder {
            conn: self,
            sql: sql.to_owned(),
            fetch_array_size: None,
        }
    }

    pub fn execute(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Statement<'_>> {
        let mut stmt = self.statement(sql).build()?;
        stmt.execute(params)?;
        Ok(stmt)
    }

    pub fn execute_named(&self, sql: &str, params: &[(&str, &dyn ToSql)]) -> Result<Statement<'_>> {
        let mut stmt = self.statement(sql).build()?;
        stmt.execute_named(params)?;
        Ok(stmt)
    }

    pub fn query(&self, sql: &str, params: &[&dyn ToSql]) -> Result<ResultSet<'static, Row>> {
        self.statement(sql).build()?.query(params)
    }

    pub fn query_named(
        &self,
        sql: &str,
        params: &[(&str, &dyn ToSql)],
    ) -> Result<ResultSet<'static, Row>> {
        self.statement(sql).build()?.query_named(params)
    }

    pub fn query_as<T: RowValue>(
        &self,
        sql: &str,
        params: &[&dyn ToSql],
    ) -> Result<ResultSet<'static, T>> {
        Ok(self.query(sql, params)?.into_typed())
    }

    pub fn query_as_named<T: RowValue>(
        &self,
        sql: &str,
        params: &[(&str, &dyn ToSql)],
    ) -> Result<ResultSet<'static, T>> {
        Ok(self.query_named(sql, params)?.into_typed())
    }

    pub fn query_row(&self, sql: &str, params: &[&dyn ToSql]) -> Result<Row> {
        first(self.query(sql, params)?)
    }

    pub fn query_row_named(&self, sql: &str, params: &[(&str, &dyn ToSql)]) -> Result<Row> {
        first(self.query_named(sql, params)?)
    }

    pub fn query_row_as<T: RowValue>(&self, sql: &str, params: &[&dyn ToSql]) -> Result<T> {
        T::get(&self.query_row(sql, params)?)
    }

    pub fn query_row_as_named<T: RowValue>(
        &self,
        sql: &str,
        params: &[(&str, &dyn ToSql)],
    ) -> Result<T> {
        T::get(&self.query_row_named(sql, params)?)
    }

    pub fn batch(&self, sql: &str, max_batch_size: usize) -> BatchBuilder<'_> {
        BatchBuilder {
            conn: self,
            sql: sql.to_owned(),
            max_batch_size: max_batch_size.max(1),
            batch_errors: false,
        }
    }

    pub fn commit(&self) -> Result<()> {
        Ok(self.inner.commit()?)
    }

    pub fn rollback(&self) -> Result<()> {
        Ok(self.inner.rollback()?)
    }

    /// Not possible with the thin driver (oracle/rust-oracledb#24): a call
    /// can't be interrupted from another thread.
    pub fn break_execution(&self) -> Result<()> {
        Err(unsupported("Cancelling a running statement"))
    }

    pub fn set_module(&self, module: &str) -> Result<()> {
        Ok(self.inner.set_module(module)?)
    }

    pub fn ping(&self) -> Result<()> {
        Ok(self.inner.ping()?)
    }

    /// The connection closes when it's dropped.
    pub fn close(&self) -> Result<()> {
        Ok(())
    }

    pub fn server_version(&self) -> Result<(Version, String)> {
        let v = self.inner.version()?;
        let version = Version([v.0, v.1, v.2, v.3, v.4].map(|n| n as i32));
        let banner = match self.banner.get() {
            Some(banner) => banner.clone(),
            None => {
                let banner = ["banner_full", "banner"]
                    .iter()
                    .find_map(|column| {
                        self.query_row_as::<String>(
                            &format!("select {column} from v$version where rownum = 1"),
                            &[],
                        )
                        .ok()
                    })
                    .unwrap_or_else(|| format!("Oracle Database {version}"));
                let _ = self.banner.set(banner.clone());
                banner
            }
        };
        Ok((version, banner))
    }

    /// Runs DDL whose text the driver would take for having bind variables
    /// (`:new` in a trigger, oracle/rust-oracledb#33) as dynamic SQL, where
    /// the text is a value. Compile errors (ORA-24344) are success, as when
    /// the statement runs directly.
    fn execute_ddl_text(&self, sql: &str) -> Result<()> {
        let text = sql.to_owned();
        match self
            .inner
            .execute("begin execute immediate :1; end;", &[&text])
        {
            Err(e) => {
                let e = Error::from(e);
                if e.db.as_ref().is_some_and(|d| d.code == 24344) {
                    Ok(())
                } else {
                    Err(e)
                }
            }
            Ok(_) => Ok(()),
        }
    }
}

fn first(mut rows: ResultSet<'static, Row>) -> Result<Row> {
    rows.next()
        .unwrap_or_else(|| Err(Error::new(ErrorKind::NoDataFound, "no data found")))
}

/// A server version, e.g. 23.26.3.0.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version([i32; 5]);

impl Version {
    pub fn major(&self) -> i32 {
        self.0[0]
    }

    pub fn minor(&self) -> i32 {
        self.0[1]
    }

    pub fn update(&self) -> i32 {
        self.0[2]
    }

    pub fn patch(&self) -> i32 {
        self.0[3]
    }

    pub fn port_update(&self) -> i32 {
        self.0[4]
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d, e] = self.0;
        write!(f, "{a}.{b}.{c}.{d}.{e}")
    }
}

// ----- statements --------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementType {
    Select,
    Insert,
    Update,
    Delete,
    Merge,
    Create,
    Alter,
    Drop,
    Begin,
    Declare,
    Call,
    Commit,
    Rollback,
    ExplainPlan,
    Other,
}

impl StatementType {
    /// From the statement's first keyword (comments skipped).
    fn of(sql: &str) -> Self {
        let first = pythia_plsql::tokenize(sql)
            .into_iter()
            .find(|t| !t.kind.is_trivia())
            .map(|t| sql[t.span].to_ascii_uppercase())
            .unwrap_or_default();
        match first.as_str() {
            "SELECT" | "WITH" | "(" => Self::Select,
            "INSERT" => Self::Insert,
            "UPDATE" => Self::Update,
            "DELETE" => Self::Delete,
            "MERGE" => Self::Merge,
            "CREATE" => Self::Create,
            "ALTER" => Self::Alter,
            "DROP" => Self::Drop,
            "BEGIN" => Self::Begin,
            "DECLARE" => Self::Declare,
            "CALL" => Self::Call,
            "COMMIT" => Self::Commit,
            "ROLLBACK" => Self::Rollback,
            "EXPLAIN" => Self::ExplainPlan,
            _ => Self::Other,
        }
    }
}

pub struct StatementBuilder<'conn> {
    conn: &'conn Connection,
    sql: String,
    fetch_array_size: Option<u32>,
}

impl<'conn> StatementBuilder<'conn> {
    pub fn fetch_array_size(&mut self, size: u32) -> &mut Self {
        self.fetch_array_size = Some(size);
        self
    }

    pub fn build(&self) -> Result<Statement<'conn>> {
        let inner = prepare(self.conn, &self.sql, self.fetch_array_size)?;
        Ok(Statement {
            conn: self.conn,
            kind: StatementType::of(&self.sql),
            sql: self.sql.clone(),
            fetch_array_size: self.fetch_array_size,
            is_query: inner.is_query(),
            is_dml: inner.is_dml(),
            is_plsql: inner.is_plsql(),
            is_ddl: inner.is_ddl(),
            bind_names: inner.bind_names().to_vec(),
            inner: Some(inner),
            row_count: 0,
            out: Vec::new(),
        })
    }
}

/// The driver doesn't take a statement that starts with a `--` comment for
/// what it is (a query isn't one; the server answers ORA-00900). Leading
/// comments become spaces, byte for byte, so error offsets still match.
fn blank_leading_comments(sql: &str) -> std::borrow::Cow<'_, str> {
    let mut end = 0;
    let mut line_comment = false;
    for token in pythia_plsql::tokenize(sql) {
        if !token.kind.is_trivia() {
            break;
        }
        line_comment |= sql[token.span.clone()].starts_with("--");
        end = token.span.end;
    }
    if !line_comment {
        return std::borrow::Cow::Borrowed(sql);
    }
    let mut blanked = String::with_capacity(sql.len());
    for c in sql[..end].chars() {
        if c == '\n' || c == '\r' {
            blanked.push(c);
        } else {
            blanked.extend(std::iter::repeat_n(' ', c.len_utf8()));
        }
    }
    blanked.push_str(&sql[end..]);
    std::borrow::Cow::Owned(blanked)
}

fn prepare(
    conn: &Connection,
    sql: &str,
    fetch_array_size: Option<u32>,
) -> Result<oracledb::Statement> {
    let sql = blank_leading_comments(sql);
    let mut builder = conn.inner.statement(&sql)?;
    if let Some(size) = fetch_array_size {
        builder = builder.fetch_array_size(size);
    }
    Ok(builder.build()?)
}

pub struct Statement<'conn> {
    conn: &'conn Connection,
    sql: String,
    kind: StatementType,
    fetch_array_size: Option<u32>,
    is_query: bool,
    is_dml: bool,
    is_plsql: bool,
    is_ddl: bool,
    /// Unique bind names, in order.
    bind_names: Vec<String>,
    /// Taken by a query (its cursor owns it); prepared again if needed.
    inner: Option<oracledb::Statement>,
    row_count: u64,
    /// OUT bind values of the last execution: (position from 1, name, value).
    out: Vec<(usize, String, SqlValue)>,
}

impl Statement<'_> {
    pub fn is_query(&self) -> bool {
        self.is_query
    }

    pub fn is_dml(&self) -> bool {
        self.is_dml
    }

    pub fn is_plsql(&self) -> bool {
        self.is_plsql
    }

    pub fn is_ddl(&self) -> bool {
        self.is_ddl
    }

    fn inner(&mut self) -> Result<&mut oracledb::Statement> {
        if self.inner.is_none() {
            self.inner = Some(prepare(self.conn, &self.sql, self.fetch_array_size)?);
        }
        Ok(self.inner.as_mut().expect("prepared"))
    }

    fn take_inner(&mut self) -> Result<oracledb::Statement> {
        match self.inner.take() {
            Some(inner) => Ok(inner),
            None => prepare(self.conn, &self.sql, self.fetch_array_size),
        }
    }

    pub fn statement_type(&self) -> StatementType {
        self.kind
    }

    pub fn row_count(&self) -> Result<u64> {
        Ok(self.row_count)
    }

    /// DDL the driver thinks has binds while none are given.
    fn ddl_with_colons(&self, given: usize) -> bool {
        given == 0 && self.is_ddl && !self.bind_names.is_empty()
    }

    /// Positional values as named ones, if there's one per unique name: the
    /// Instant Client binds positions to unique names, while the thin
    /// driver wants a value for each occurrence of a repeated placeholder in
    /// SQL (`:3 is null or type = :3`).
    fn by_name<'p>(&self, params: &[&'p dyn ToSql]) -> Option<Vec<(String, &'p dyn ToSql)>> {
        (!params.is_empty() && self.bind_names.len() == params.len()).then(|| {
            self.bind_names
                .iter()
                .cloned()
                .zip(params.iter().copied())
                .collect()
        })
    }

    pub fn execute(&mut self, params: &[&dyn ToSql]) -> Result<()> {
        if let Some(named) = self.by_name(params) {
            let named: Vec<(&str, &dyn ToSql)> =
                named.iter().map(|(n, v)| (n.as_str(), *v)).collect();
            // OUT binds keep their positions: the names are in that order.
            return self.execute_named(&named);
        }
        if self.ddl_with_colons(params.len()) {
            self.row_count = 0;
            return self.conn.execute_ddl_text(&self.sql);
        }
        let binds: Vec<Bind> = params.iter().map(|p| p.to_bind()).collect();
        let values: Vec<&dyn ToDbValue> = binds.iter().map(Bind::as_db).collect();
        let mut result = self.inner()?.execute(&values)?;
        self.row_count = result.rows_affected();
        let names = vec![String::new(); binds.len()];
        self.collect_out(&mut result, &binds, &names)
    }

    pub fn execute_named(&mut self, params: &[(&str, &dyn ToSql)]) -> Result<()> {
        if self.ddl_with_colons(params.len()) {
            self.row_count = 0;
            return self.conn.execute_ddl_text(&self.sql);
        }
        let binds: Vec<Bind> = params.iter().map(|(_, p)| p.to_bind()).collect();
        let values: Vec<(&str, &dyn ToDbValue)> = params
            .iter()
            .zip(&binds)
            .map(|((n, _), b)| (*n, b.as_db()))
            .collect();
        let mut result = self.inner()?.execute_named(&values)?;
        self.row_count = result.rows_affected();
        let names: Vec<String> = params.iter().map(|(n, _)| n.to_ascii_uppercase()).collect();
        self.collect_out(&mut result, &binds, &names)
    }

    /// Reads the OUT binds' values, in the order they were bound.
    fn collect_out(
        &mut self,
        result: &mut oracledb::ExecResult,
        binds: &[Bind],
        names: &[String],
    ) -> Result<()> {
        self.out.clear();
        if !binds.iter().any(|b| matches!(b, Bind::Out(_))) {
            return Ok(());
        }
        let row = result.out_bind_data();
        let mut k = 0;
        for (i, bind) in binds.iter().enumerate() {
            let Bind::Out(t) = bind else { continue };
            let value = if *t == oracledb::DB_TYPE_NUMBER {
                row.get::<Option<OracleNumber>>(k)?
                    .map_or(Value::Null, Value::Number)
            } else if *t == oracledb::DB_TYPE_DATE {
                row.get::<Option<OracleTimestamp>>(k)?
                    .map_or(Value::Null, |ts| Value::Timestamp(ts, TsKind::Date, 0))
            } else {
                row.get::<Option<String>>(k)?
                    .map_or(Value::Null, Value::Text)
            };
            self.out.push((i + 1, names[i].clone(), SqlValue(value)));
            k += 1;
        }
        Ok(())
    }

    /// An OUT bind's value: by position (from 1) or by name.
    pub fn bind_value<I: BindIndex, T: FromSql>(&self, index: I) -> Result<T> {
        let value = self
            .out
            .iter()
            .find(|(pos, name, _)| index.matches(*pos, name))
            .map(|(_, _, v)| v)
            .ok_or_else(|| Error::new(ErrorKind::Other, "no OUT bind at this index"))?;
        T::from_sql(value)
    }

    pub fn query(&mut self, params: &[&dyn ToSql]) -> Result<ResultSet<'static, Row>> {
        if let Some(named) = self.by_name(params) {
            let named: Vec<(&str, &dyn ToSql)> =
                named.iter().map(|(n, v)| (n.as_str(), *v)).collect();
            return self.query_named(&named);
        }
        let binds: Vec<Bind> = params.iter().map(|p| p.to_bind()).collect();
        let values: Vec<&dyn ToDbValue> = binds.iter().map(Bind::as_db).collect();
        let cursor = self.take_inner()?.query(&values)?;
        Ok(ResultSet::new(cursor, self.conn.ltz_offset))
    }

    pub fn query_named(
        &mut self,
        params: &[(&str, &dyn ToSql)],
    ) -> Result<ResultSet<'static, Row>> {
        let binds: Vec<Bind> = params.iter().map(|(_, p)| p.to_bind()).collect();
        let values: Vec<(&str, &dyn ToDbValue)> = params
            .iter()
            .zip(&binds)
            .map(|((n, _), b)| (*n, b.as_db()))
            .collect();
        let cursor = self.take_inner()?.query_named(&values)?;
        Ok(ResultSet::new(cursor, self.conn.ltz_offset))
    }
}

impl fmt::Debug for Statement<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Statement")
            .field("sql", &self.sql)
            .field("kind", &self.kind)
            .field("row_count", &self.row_count)
            .finish()
    }
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection").finish_non_exhaustive()
    }
}

/// An OUT bind's index for [`Statement::bind_value`].
pub trait BindIndex {
    fn matches(&self, position: usize, name: &str) -> bool;
}

impl BindIndex for usize {
    fn matches(&self, position: usize, _: &str) -> bool {
        *self == position
    }
}

impl BindIndex for &str {
    fn matches(&self, _: usize, name: &str) -> bool {
        self.trim_start_matches(':').eq_ignore_ascii_case(name)
    }
}

// ----- batches -----------------------------------------------------------

pub struct BatchBuilder<'conn> {
    conn: &'conn Connection,
    sql: String,
    max_batch_size: usize,
    batch_errors: bool,
}

impl<'conn> BatchBuilder<'conn> {
    pub fn with_batch_errors(&mut self) -> &mut Self {
        self.batch_errors = true;
        self
    }

    pub fn build(&self) -> Result<Batch<'conn>> {
        Ok(Batch {
            conn: self.conn,
            sql: blank_leading_comments(&self.sql).into_owned(),
            max_batch_size: self.max_batch_size,
            batch_errors: self.batch_errors,
            rows: Vec::new(),
        })
    }
}

pub struct Batch<'conn> {
    conn: &'conn Connection,
    sql: String,
    max_batch_size: usize,
    batch_errors: bool,
    rows: Vec<Vec<Bind>>,
}

impl Batch<'_> {
    pub fn append_row(&mut self, params: &[&dyn ToSql]) -> Result<()> {
        self.rows.push(params.iter().map(|p| p.to_bind()).collect());
        if self.rows.len() >= self.max_batch_size {
            self.execute()?;
        }
        Ok(())
    }

    /// Runs the rows appended so far. With batch errors, rows run one by one
    /// (the driver doesn't report batch errors yet, oracle/rust-oracledb#41)
    /// and the failed ones are returned as [`Error::batch_errors`].
    pub fn execute(&mut self) -> Result<()> {
        let rows = std::mem::take(&mut self.rows);
        if rows.is_empty() {
            return Ok(());
        }
        if !self.batch_errors {
            let values: Vec<Vec<&dyn ToDbValue>> = rows
                .iter()
                .map(|r| r.iter().map(Bind::as_db).collect())
                .collect();
            let slices: Vec<&[&dyn ToDbValue]> = values.iter().map(Vec::as_slice).collect();
            self.conn
                .inner
                .execute_batch(&self.sql, slices.as_slice())?;
            return Ok(());
        }
        let mut errors = Vec::new();
        for (i, row) in rows.iter().enumerate() {
            let values: Vec<&dyn ToDbValue> = row.iter().map(Bind::as_db).collect();
            if let Err(e) = self.conn.inner.execute(&self.sql, &values) {
                let e = Error::from(e);
                match e.db {
                    Some(mut db) => {
                        db.offset = i as u32;
                        errors.push(db);
                    }
                    None => return Err(e),
                }
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error {
                kind: ErrorKind::BatchErrors,
                message: format!("{} row(s) failed", errors.len()),
                db: None,
                batch: Some(errors),
            })
        }
    }
}

// ----- binds -------------------------------------------------------------

/// A bind value, converted to a type the driver takes.
#[derive(Debug, Clone)]
pub enum Bind {
    Text(Option<String>),
    Int(Option<i64>),
    Double(Option<f64>),
    Bool(Option<bool>),
    Bytes(Option<Vec<u8>>),
    /// An OUT bind of this type.
    Out(&'static DbType),
}

impl Bind {
    fn as_db(&self) -> &dyn ToDbValue {
        match self {
            Bind::Text(v) => v,
            Bind::Int(v) => v,
            Bind::Double(v) => v,
            Bind::Bool(v) => v,
            Bind::Bytes(v) => v,
            Bind::Out(t) => t,
        }
    }
}

pub trait ToSql {
    fn to_bind(&self) -> Bind;
    /// NULL of this type (for `None`).
    fn null() -> Bind
    where
        Self: Sized,
    {
        Bind::Text(None)
    }
}

impl<T: ToSql + ?Sized> ToSql for &T {
    fn to_bind(&self) -> Bind {
        (**self).to_bind()
    }
}

impl ToSql for str {
    fn to_bind(&self) -> Bind {
        Bind::Text(Some(self.to_owned()))
    }
}

impl ToSql for String {
    fn to_bind(&self) -> Bind {
        Bind::Text(Some(self.clone()))
    }
}

// As text: 26.0.0-beta.4 encodes integers ending in 00 wrongly (ORA-03089,
// oracle/rust-oracledb#37, fixed after beta.4); the database converts.
macro_rules! int_to_sql {
    ($($t:ty),*) => {$(
        impl ToSql for $t {
            fn to_bind(&self) -> Bind {
                Bind::Text(Some(self.to_string()))
            }
            fn null() -> Bind {
                Bind::Int(None)
            }
        }
    )*};
}
int_to_sql!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize);

impl ToSql for f64 {
    fn to_bind(&self) -> Bind {
        Bind::Double(Some(*self))
    }
    fn null() -> Bind {
        Bind::Double(None)
    }
}

impl ToSql for f32 {
    fn to_bind(&self) -> Bind {
        Bind::Double(Some(f64::from(*self)))
    }
    fn null() -> Bind {
        Bind::Double(None)
    }
}

impl ToSql for bool {
    fn to_bind(&self) -> Bind {
        Bind::Bool(Some(*self))
    }
    fn null() -> Bind {
        Bind::Bool(None)
    }
}

impl ToSql for Vec<u8> {
    fn to_bind(&self) -> Bind {
        Bind::Bytes(Some(self.clone()))
    }
    fn null() -> Bind {
        Bind::Bytes(None)
    }
}

impl ToSql for [u8] {
    fn to_bind(&self) -> Bind {
        Bind::Bytes(Some(self.to_vec()))
    }
}

impl<T: ToSql> ToSql for Option<T> {
    fn to_bind(&self) -> Bind {
        match self {
            Some(v) => v.to_bind(),
            None => T::null(),
        }
    }
}

/// An OUT bind of this type.
impl ToSql for OracleType {
    fn to_bind(&self) -> Bind {
        Bind::Out(match self {
            OracleType::Number(..)
            | OracleType::Float(_)
            | OracleType::Int64
            | OracleType::UInt64 => oracledb::DB_TYPE_NUMBER,
            OracleType::BinaryDouble => oracledb::DB_TYPE_BINARY_DOUBLE,
            OracleType::Date => oracledb::DB_TYPE_DATE,
            _ => oracledb::DB_TYPE_VARCHAR,
        })
    }
}

// ----- column types ------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OracleType {
    Varchar2(u32),
    NVarchar2(u32),
    Char(u32),
    NChar(u32),
    Rowid,
    Raw(u32),
    BinaryFloat,
    BinaryDouble,
    Number(u8, i8),
    Float(u8),
    Date,
    Timestamp(u8),
    TimestampTZ(u8),
    TimestampLTZ(u8),
    IntervalDS(i32, i32),
    IntervalYM(i32),
    CLOB,
    NCLOB,
    BLOB,
    BFILE,
    Long,
    LongRaw,
    Boolean,
    Json,
    Vector,
    Int64,
    UInt64,
    Other(String),
}

impl OracleType {
    fn of(m: &Metadata) -> Self {
        use oracledb::*;
        let t = m.db_type();
        let size = m.max_size();
        if t == DB_TYPE_VARCHAR {
            Self::Varchar2(size)
        } else if t == DB_TYPE_NVARCHAR {
            Self::NVarchar2(size)
        } else if t == DB_TYPE_CHAR {
            Self::Char(size)
        } else if t == DB_TYPE_NCHAR {
            Self::NChar(size)
        } else if t == DB_TYPE_NUMBER {
            // Scale -127 is FLOAT (oracle/rust-oracledb names it NUMBER(p,-127)).
            if m.scale() == -127 && m.precision() != 0 {
                Self::Float(m.precision())
            } else {
                Self::Number(m.precision(), m.scale())
            }
        } else if t == DB_TYPE_BINARY_INTEGER {
            Self::Int64
        } else if t == DB_TYPE_BINARY_FLOAT {
            Self::BinaryFloat
        } else if t == DB_TYPE_BINARY_DOUBLE {
            Self::BinaryDouble
        } else if t == DB_TYPE_DATE {
            Self::Date
        } else if t == DB_TYPE_TIMESTAMP {
            Self::Timestamp(m.scale().max(0) as u8)
        } else if t == DB_TYPE_TIMESTAMP_TZ {
            Self::TimestampTZ(m.scale().max(0) as u8)
        } else if t == DB_TYPE_TIMESTAMP_LTZ {
            Self::TimestampLTZ(m.scale().max(0) as u8)
        } else if t == DB_TYPE_INTERVAL_DS {
            Self::IntervalDS(i32::from(m.precision()), i32::from(m.scale()))
        } else if t == DB_TYPE_INTERVAL_YM {
            Self::IntervalYM(i32::from(m.precision()))
        } else if t == DB_TYPE_RAW {
            Self::Raw(size)
        } else if t == DB_TYPE_ROWID || t == DB_TYPE_UROWID {
            Self::Rowid
        } else if t == DB_TYPE_CLOB {
            Self::CLOB
        } else if t == DB_TYPE_NCLOB {
            Self::NCLOB
        } else if t == DB_TYPE_BLOB {
            Self::BLOB
        } else if t == DB_TYPE_BFILE {
            Self::BFILE
        } else if t == DB_TYPE_LONG || t == DB_TYPE_LONG_NVARCHAR {
            // Also CLOB/NCLOB columns: the driver fetches them as LONG.
            Self::Long
        } else if t == DB_TYPE_LONG_RAW {
            Self::LongRaw
        } else if t == DB_TYPE_BOOLEAN {
            Self::Boolean
        } else if t == DB_TYPE_JSON {
            Self::Json
        } else if t == DB_TYPE_VECTOR {
            Self::Vector
        } else {
            Self::Other(m.data_type())
        }
    }
}

impl fmt::Display for OracleType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Varchar2(n) => write!(f, "VARCHAR2({n})"),
            Self::NVarchar2(n) => write!(f, "NVARCHAR2({n})"),
            Self::Char(n) => write!(f, "CHAR({n})"),
            Self::NChar(n) => write!(f, "NCHAR({n})"),
            Self::Rowid => f.write_str("ROWID"),
            Self::Raw(n) => write!(f, "RAW({n})"),
            Self::BinaryFloat => f.write_str("BINARY_FLOAT"),
            Self::BinaryDouble => f.write_str("BINARY_DOUBLE"),
            Self::Number(0, _) => f.write_str("NUMBER"),
            Self::Number(p, 0) => write!(f, "NUMBER({p})"),
            Self::Number(p, s) => write!(f, "NUMBER({p},{s})"),
            Self::Float(p) => write!(f, "FLOAT({p})"),
            Self::Date => f.write_str("DATE"),
            Self::Timestamp(s) => write!(f, "TIMESTAMP({s})"),
            Self::TimestampTZ(s) => write!(f, "TIMESTAMP({s}) WITH TIME ZONE"),
            Self::TimestampLTZ(s) => write!(f, "TIMESTAMP({s}) WITH LOCAL TIME ZONE"),
            Self::IntervalDS(p, s) => write!(f, "INTERVAL DAY({p}) TO SECOND({s})"),
            Self::IntervalYM(p) => write!(f, "INTERVAL YEAR({p}) TO MONTH"),
            Self::CLOB => f.write_str("CLOB"),
            Self::NCLOB => f.write_str("NCLOB"),
            Self::BLOB => f.write_str("BLOB"),
            Self::BFILE => f.write_str("BFILE"),
            Self::Long => f.write_str("LONG"),
            Self::LongRaw => f.write_str("LONG RAW"),
            Self::Boolean => f.write_str("BOOLEAN"),
            Self::Json => f.write_str("JSON"),
            Self::Vector => f.write_str("VECTOR"),
            Self::Int64 => f.write_str("BINARY_INTEGER"),
            Self::UInt64 => f.write_str("NUMBER"),
            Self::Other(name) => f.write_str(name),
        }
    }
}

/// A result column.
#[derive(Debug, Clone)]
pub struct ColumnInfo {
    name: String,
    oracle_type: OracleType,
    nullable: bool,
}

impl ColumnInfo {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn oracle_type(&self) -> &OracleType {
        &self.oracle_type
    }

    pub fn nullable(&self) -> bool {
        self.nullable
    }
}

// ----- values ------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TsKind {
    Date,
    Plain,
    /// The value's own offset.
    Tz,
    /// The session's offset, in minutes.
    Ltz(i32),
}

#[derive(Debug, Clone)]
enum Value {
    Null,
    Text(String),
    Number(OracleNumber),
    Float(f32),
    Double(f64),
    /// With its kind and the column's fraction digits.
    Timestamp(OracleTimestamp, TsKind, u8),
    /// With the column's day precision and fraction digits.
    IntervalDS(OracleIntervalDS, i32, i32),
    IntervalYM(OracleIntervalYM, i32),
    Bytes(Vec<u8>),
    Bool(bool),
    Json(JsonValue),
    Vector(Vector),
}

/// A column value.
#[derive(Debug, Clone)]
pub struct SqlValue(Value);

impl SqlValue {
    pub fn is_null(&self) -> Result<bool> {
        Ok(matches!(self.0, Value::Null))
    }

    pub fn get<T: FromSql>(&self) -> Result<T> {
        T::from_sql(self)
    }

    /// The value as Pythia shows it (the Instant Client's formats).
    fn text(&self) -> Option<String> {
        Some(match &self.0 {
            Value::Null => return None,
            Value::Text(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Float(v) => v.to_string(),
            Value::Double(v) => v.to_string(),
            Value::Timestamp(ts, kind, scale) => timestamp(ts, *kind, *scale),
            Value::IntervalDS(v, precision, scale) => interval_ds(v, *precision, *scale),
            Value::IntervalYM(v, precision) => interval_ym(v, *precision),
            Value::Bytes(b) => hex(b),
            Value::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
            Value::Json(j) => {
                let mut out = String::new();
                json(j, &mut out);
                out
            }
            Value::Vector(v) => vector(v),
        })
    }

    fn number(&self) -> Option<String> {
        match &self.0 {
            Value::Number(n) => Some(n.to_string()),
            Value::Float(v) => Some(v.to_string()),
            Value::Double(v) => Some(v.to_string()),
            Value::Text(s) => Some(s.trim().to_owned()),
            Value::Bool(b) => Some(if *b { "1" } else { "0" }.to_owned()),
            _ => None,
        }
    }
}

impl fmt::Display for SqlValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.text().as_deref().unwrap_or("NULL"))
    }
}

/// Reads column `i` by its type: the driver has no untyped value.
fn decode(row: &oracledb::Row, i: usize, m: &Metadata, ltz: i32) -> Result<Value> {
    use oracledb::*;
    let t = m.db_type();
    let scale = m.scale().max(0) as u8;
    let value = if t == DB_TYPE_NUMBER || t == DB_TYPE_BINARY_INTEGER {
        row.get::<Option<OracleNumber>>(i)?.map(Value::Number)
    } else if t == DB_TYPE_BINARY_FLOAT {
        row.get::<Option<f32>>(i)?.map(Value::Float)
    } else if t == DB_TYPE_BINARY_DOUBLE {
        row.get::<Option<f64>>(i)?.map(Value::Double)
    } else if t == DB_TYPE_DATE
        || t == DB_TYPE_TIMESTAMP
        || t == DB_TYPE_TIMESTAMP_TZ
        || t == DB_TYPE_TIMESTAMP_LTZ
    {
        let kind = if t == DB_TYPE_DATE {
            TsKind::Date
        } else if t == DB_TYPE_TIMESTAMP_TZ {
            TsKind::Tz
        } else if t == DB_TYPE_TIMESTAMP_LTZ {
            TsKind::Ltz(ltz)
        } else {
            TsKind::Plain
        };
        // A time zone region makes the driver panic (oracle/rust-oracledb#42).
        let read = catch_unwind(AssertUnwindSafe(|| row.get::<Option<OracleTimestamp>>(i)));
        match read {
            Ok(v) => v?.map(|ts| Value::Timestamp(ts, kind, scale)),
            Err(_) => Some(Value::Text(
                "<time zone region: needs the Instant Client>".into(),
            )),
        }
    } else if t == DB_TYPE_INTERVAL_DS {
        row.get::<Option<OracleIntervalDS>>(i)?
            .map(|v| Value::IntervalDS(v, i32::from(m.precision()), i32::from(m.scale())))
    } else if t == DB_TYPE_INTERVAL_YM {
        row.get::<Option<OracleIntervalYM>>(i)?
            .map(|v| Value::IntervalYM(v, i32::from(m.precision())))
    } else if t == DB_TYPE_RAW || t == DB_TYPE_LONG_RAW || t == DB_TYPE_BLOB {
        row.get::<Option<Vec<u8>>>(i)?.map(Value::Bytes)
    } else if t == DB_TYPE_BOOLEAN {
        row.get::<Option<bool>>(i)?.map(Value::Bool)
    } else if t == DB_TYPE_JSON {
        row.get::<Option<JsonValue>>(i)?.map(Value::Json)
    } else if t == DB_TYPE_VECTOR {
        row.get::<Option<Vector>>(i)?.map(Value::Vector)
    } else {
        // Strings, CLOB/NCLOB, LONG, ROWID.
        row.get::<Option<String>>(i)?.map(Value::Text)
    };
    Ok(value.unwrap_or(Value::Null))
}

pub trait FromSql: Sized {
    fn from_sql(value: &SqlValue) -> Result<Self>;
}

impl FromSql for String {
    fn from_sql(value: &SqlValue) -> Result<Self> {
        value
            .text()
            .ok_or_else(|| Error::new(ErrorKind::NullValue, "NULL value"))
    }
}

macro_rules! number_from_sql {
    ($($t:ty),*) => {$(
        impl FromSql for $t {
            fn from_sql(value: &SqlValue) -> Result<Self> {
                if matches!(value.0, Value::Null) {
                    return Err(Error::new(ErrorKind::NullValue, "NULL value"));
                }
                let text = value.number().ok_or_else(|| {
                    Error::new(ErrorKind::InvalidTypeConversion, format!("{value} isn't a number"))
                })?;
                text.parse::<$t>()
                    .or_else(|_| text.parse::<f64>().map(|f| f as $t))
                    .map_err(|_| Error::new(ErrorKind::InvalidTypeConversion, format!("{text} isn't a {}", stringify!($t))))
            }
        }
    )*};
}
number_from_sql!(i8, i16, i32, i64, u8, u16, u32, u64, usize, isize, f32, f64);

impl FromSql for bool {
    fn from_sql(value: &SqlValue) -> Result<Self> {
        match &value.0 {
            Value::Bool(b) => Ok(*b),
            Value::Null => Err(Error::new(ErrorKind::NullValue, "NULL value")),
            _ => Ok(i64::from_sql(value)? != 0),
        }
    }
}

impl FromSql for Vec<u8> {
    fn from_sql(value: &SqlValue) -> Result<Self> {
        match &value.0 {
            Value::Bytes(b) => Ok(b.clone()),
            Value::Null => Err(Error::new(ErrorKind::NullValue, "NULL value")),
            _ => Ok(String::from_sql(value)?.into_bytes()),
        }
    }
}

impl<T: FromSql> FromSql for Option<T> {
    fn from_sql(value: &SqlValue) -> Result<Self> {
        match value.0 {
            Value::Null => Ok(None),
            _ => T::from_sql(value).map(Some),
        }
    }
}

// ----- rows --------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Row {
    values: Vec<SqlValue>,
    columns: Arc<Vec<ColumnInfo>>,
}

impl Row {
    pub fn get<I: ColumnIndex, T: FromSql>(&self, index: I) -> Result<T> {
        let i = index.position(&self.columns)?;
        T::from_sql(&self.values[i])
    }

    pub fn get_as<T: RowValue>(&self) -> Result<T> {
        T::get(self)
    }

    pub fn sql_values(&self) -> &[SqlValue] {
        &self.values
    }

    pub fn column_info(&self) -> &[ColumnInfo] {
        &self.columns
    }
}

pub trait ColumnIndex {
    fn position(&self, columns: &[ColumnInfo]) -> Result<usize>;
}

impl ColumnIndex for usize {
    fn position(&self, columns: &[ColumnInfo]) -> Result<usize> {
        if *self < columns.len() {
            Ok(*self)
        } else {
            Err(Error::new(ErrorKind::Other, format!("no column {self}")))
        }
    }
}

impl ColumnIndex for &str {
    fn position(&self, columns: &[ColumnInfo]) -> Result<usize> {
        columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(self))
            .ok_or_else(|| Error::new(ErrorKind::Other, format!("no column {self}")))
    }
}

/// What a row converts to: a value of its first column, a tuple, or the row.
pub trait RowValue: Sized {
    fn get(row: &Row) -> Result<Self>;
}

impl RowValue for Row {
    fn get(row: &Row) -> Result<Self> {
        Ok(row.clone())
    }
}

impl<T: FromSql> RowValue for T {
    fn get(row: &Row) -> Result<Self> {
        row.get(0)
    }
}

macro_rules! tuple_row_value {
    ($(($($t:ident $i:tt),+))*) => {$(
        impl<$($t: FromSql),+> RowValue for ($($t,)+) {
            fn get(row: &Row) -> Result<Self> {
                Ok(($(row.get::<usize, $t>($i)?,)+))
            }
        }
    )*};
}
tuple_row_value! {
    (A 0, B 1)
    (A 0, B 1, C 2)
    (A 0, B 1, C 2, D 3)
    (A 0, B 1, C 2, D 3, E 4)
    (A 0, B 1, C 2, D 3, E 4, F 5)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8)
    (A 0, B 1, C 2, D 3, E 4, F 5, G 6, H 7, I 8, J 9)
}

pub struct ResultSet<'a, T> {
    cursor: oracledb::Cursor,
    meta: Arc<Vec<Metadata>>,
    columns: Arc<Vec<ColumnInfo>>,
    ltz: i32,
    _row: PhantomData<(&'a (), T)>,
}

impl ResultSet<'static, Row> {
    fn new(cursor: oracledb::Cursor, ltz: i32) -> Self {
        let meta: Vec<Metadata> = cursor.columns().to_vec();
        let columns = meta
            .iter()
            .map(|m| ColumnInfo {
                name: m.name().to_owned(),
                oracle_type: OracleType::of(m),
                nullable: m.nullable(),
            })
            .collect();
        Self {
            cursor,
            meta: Arc::new(meta),
            columns: Arc::new(columns),
            ltz,
            _row: PhantomData,
        }
    }

    fn into_typed<T: RowValue>(self) -> ResultSet<'static, T> {
        ResultSet {
            cursor: self.cursor,
            meta: self.meta,
            columns: self.columns,
            ltz: self.ltz,
            _row: PhantomData,
        }
    }
}

impl<T> ResultSet<'_, T> {
    pub fn column_info(&self) -> &[ColumnInfo] {
        &self.columns
    }
}

impl<T: RowValue> Iterator for ResultSet<'_, T> {
    type Item = Result<T>;

    fn next(&mut self) -> Option<Result<T>> {
        let next = catch_unwind(AssertUnwindSafe(|| self.cursor.next()));
        let row = match next {
            Ok(row) => row?,
            Err(_) => {
                return Some(Err(Error::new(
                    ErrorKind::Other,
                    "the thin driver failed reading this row",
                )));
            }
        };
        Some(row.map_err(Error::from).and_then(|row| {
            let values = self
                .meta
                .iter()
                .enumerate()
                .map(|(i, m)| decode(&row, i, m, self.ltz).map(SqlValue))
                .collect::<Result<Vec<_>>>()?;
            T::get(&Row {
                values,
                columns: self.columns.clone(),
            })
        }))
    }
}

// ----- formats -----------------------------------------------------------

/// `YYYY-MM-DD HH24:MI:SS[.FF][ +TZH:TZM]`, local time. The driver hands over
/// TIMESTAMP WITH TIME ZONE values as UTC fields with the value's offset
/// (oracle/rust-oracledb, see #25), so the offset is added here.
fn timestamp(ts: &OracleTimestamp, kind: TsKind, scale: u8) -> String {
    let offset = match kind {
        TsKind::Tz => i32::from(ts.tz_hour_offset()) * 60 + i32::from(ts.tz_minute_offset()),
        TsKind::Ltz(minutes) => minutes,
        _ => 0,
    };
    let days = days_from_civil(
        i64::from(ts.year()),
        u32::from(ts.month()),
        u32::from(ts.day()),
    );
    let minutes =
        (days * 24 + i64::from(ts.hour())) * 60 + i64::from(ts.minute()) + i64::from(offset);
    let (days, minute_of_day) = (minutes.div_euclid(1440), minutes.rem_euclid(1440));
    let (y, m, d) = civil_from_days(days);
    let mut out = format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        minute_of_day / 60,
        minute_of_day % 60,
        ts.second()
    );
    if kind != TsKind::Date && scale > 0 {
        let fraction = format!("{:09}", ts.nanoseconds());
        out.push('.');
        out.push_str(&fraction[..usize::from(scale.min(9))]);
    }
    if matches!(kind, TsKind::Tz | TsKind::Ltz(_)) {
        let sign = if offset < 0 { '-' } else { '+' };
        out.push_str(&format!(
            " {sign}{:02}:{:02}",
            offset.abs() / 60,
            offset.abs() % 60
        ));
    }
    out
}

/// Days since 1970-01-01 (proleptic Gregorian calendar).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `+DD HH:MI:SS.FFFFFF`, as Oracle shows it.
fn interval_ds(v: &OracleIntervalDS, precision: i32, scale: i32) -> String {
    let negative =
        v.days() < 0 || v.hours() < 0 || v.minutes() < 0 || v.seconds() < 0 || v.nanoseconds() < 0;
    let width = precision.max(2) as usize;
    let mut out = format!(
        "{}{:0width$} {:02}:{:02}:{:02}",
        if negative { '-' } else { '+' },
        v.days().abs(),
        v.hours().abs(),
        v.minutes().abs(),
        v.seconds().abs()
    );
    if scale > 0 {
        let fraction = format!("{:09}", v.nanoseconds().abs());
        out.push('.');
        out.push_str(&fraction[..(scale.min(9) as usize)]);
    }
    out
}

/// `+YY-MM`.
fn interval_ym(v: &OracleIntervalYM, precision: i32) -> String {
    let negative = v.years() < 0 || v.months() < 0;
    let width = precision.max(2) as usize;
    format!(
        "{}{:0width$}-{:02}",
        if negative { '-' } else { '+' },
        v.years().abs(),
        v.months().abs()
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// JSON text; object keys sorted (the driver keeps them in a HashMap).
fn json(v: &JsonValue, out: &mut String) {
    match v {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        JsonValue::Number(n) => out.push_str(&n.to_string()),
        JsonValue::BinaryDouble(f) => out.push_str(&f.to_string()),
        JsonValue::BinaryFloat(f) => out.push_str(&f.to_string()),
        JsonValue::String(s) => json_string(s, out),
        JsonValue::Raw(b) | JsonValue::JsonId(b) => json_string(&hex(b), out),
        JsonValue::Timestamp(ts) => json_string(&timestamp(ts, TsKind::Plain, 9), out),
        JsonValue::IntervalDS(i) => json_string(&interval_ds(i, 2, 6), out),
        JsonValue::IntervalYM(i) => json_string(&interval_ym(i, 2), out),
        JsonValue::Vector(v) => out.push_str(&vector(v)),
        JsonValue::JsonArray(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json(item, out);
            }
            out.push(']');
        }
        JsonValue::JsonObject(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                json_string(key, out);
                out.push(':');
                json(&map[key], out);
            }
            out.push('}');
        }
    }
}

fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn vector(v: &Vector) -> String {
    let list = |items: Vec<String>| format!("[{}]", items.join(","));
    match v {
        Vector::Dense(VectorData::Float32(v)) => list(v.iter().map(f32::to_string).collect()),
        Vector::Dense(VectorData::Float64(v)) => list(v.iter().map(f64::to_string).collect()),
        Vector::Dense(VectorData::Int8(v)) => list(v.iter().map(i8::to_string).collect()),
        Vector::Dense(VectorData::Binary(v)) => list(v.iter().map(u8::to_string).collect()),
        Vector::Sparse(s) => format!("{s:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates_round_trip() {
        for (y, m, d) in [
            (1970, 1, 1),
            (2026, 10, 22),
            (2000, 2, 29),
            (1899, 12, 31),
            (2400, 3, 1),
        ] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 2), 1);
    }

    #[test]
    fn leading_line_comments_are_blanked() {
        assert_eq!(
            blank_leading_comments("select 1 from dual -- c"),
            "select 1 from dual -- c"
        );
        assert_eq!(
            blank_leading_comments("/* c */ select 1"),
            "/* c */ select 1"
        );
        let sql = "-- Käse\n  select 1 from dual -- x";
        let blanked = blank_leading_comments(sql);
        assert_eq!(blanked.len(), sql.len(), "offsets stay");
        assert_eq!(
            blanked,
            format!("{}\n  select 1 from dual -- x", " ".repeat("-- Käse".len()))
        );
    }

    #[test]
    fn statement_types() {
        assert_eq!(
            StatementType::of("  -- x\n select 1 from dual"),
            StatementType::Select
        );
        assert_eq!(
            StatementType::of("/* c */ create or replace trigger t"),
            StatementType::Create
        );
        assert_eq!(
            StatementType::of("declare x number; begin null; end;"),
            StatementType::Declare
        );
        assert_eq!(
            StatementType::of("with a as (select 1 from dual) select * from a"),
            StatementType::Select
        );
    }
}
