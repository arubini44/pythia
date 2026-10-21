use std::fmt;
use std::time::Duration;

use orui_config::Privilege;

use crate::meta::{CompileError, MetaQuery, MetaResult};

#[derive(Clone)]
pub struct ConnectParams {
    pub username: String,
    pub password: String,
    pub connect_string: String,
    pub privilege: Privilege,
}

impl fmt::Debug for ConnectParams {
    // Never print the password, not even in debug logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnectParams")
            .field("username", &self.username)
            .field("connect_string", &self.connect_string)
            .field("privilege", &self.privilege)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone)]
pub enum DbRequest {
    /// Executes one statement. Queries return the first `max_rows` rows.
    Execute {
        id: u64,
        sql: String,
        max_rows: usize,
        /// Values for `:name` bind variables, bound as strings (`None` = NULL).
        binds: Vec<(String, Option<String>)>,
    },
    /// Data dictionary lookup; answered with [`DbEvent::Meta`].
    Meta {
        id: u64,
        query: MetaQuery,
    },
    /// Fetches more rows of the open result set of query `id`.
    FetchMore {
        id: u64,
        max_rows: usize,
    },
    Commit,
    Rollback,
    SetAutocommit(bool),
    Disconnect,
}

#[derive(Debug, Clone)]
pub enum DbEvent {
    Connected(ServerInfo),
    ConnectFailed(DbError),
    QueryStarted {
        id: u64,
        columns: Vec<Column>,
    },
    Rows {
        id: u64,
        rows: Vec<Vec<Cell>>,
        more: bool,
        elapsed: Duration,
    },
    Executed {
        id: u64,
        summary: ExecSummary,
        elapsed: Duration,
    },
    Failed {
        id: u64,
        error: DbError,
        elapsed: Duration,
    },
    Meta {
        id: u64,
        result: Result<MetaResult, DbError>,
    },
    /// Lines read from `DBMS_OUTPUT` after a statement.
    Output(Vec<String>),
    /// Whether the session has uncommitted changes.
    Transaction {
        pending: bool,
    },
    Notice(String),
    Error(DbError),
    Disconnected,
}

#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub version: String,
    pub banner: String,
    pub current_schema: String,
}

#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub type_name: String,
    /// Numbers are right aligned in the grid.
    pub numeric: bool,
}

/// A cell value rendered to text; `None` is SQL NULL.
pub type Cell = Option<String>;

#[derive(Debug, Clone)]
pub struct ExecSummary {
    /// Statement type, e.g. `UPDATE`, `CREATE`, `PL/SQL block`.
    pub kind: String,
    pub rows_affected: Option<u64>,
    /// For `CREATE` of a stored unit: the object and its compile errors.
    pub compiled: Option<Compiled>,
}

#[derive(Debug, Clone)]
pub struct Compiled {
    pub owner: String,
    pub name: String,
    pub object_type: String,
    pub errors: Vec<CompileError>,
}

impl fmt::Display for ExecSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(c) = &self.compiled {
            let ty = c.object_type.to_lowercase();
            let errors = c.errors.iter().filter(|e| !e.warning).count();
            let warnings = c.errors.len() - errors;
            return match (errors, warnings) {
                (0, 0) => write!(f, "{ty} {}.{} compiled", c.owner, c.name),
                (0, w) => write!(
                    f,
                    "{ty} {}.{} compiled with {w} warning(s)",
                    c.owner, c.name
                ),
                (e, _) => write!(f, "{ty} {}.{} compiled with {e} error(s)", c.owner, c.name),
            };
        }
        match self.rows_affected {
            Some(1) => write!(f, "{}: 1 row affected", self.kind),
            Some(n) => write!(f, "{}: {n} rows affected", self.kind),
            None => write!(f, "{} executed", self.kind),
        }
    }
}

#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct DbError {
    /// ORA error code, if the error came from the database.
    pub code: Option<i32>,
    pub message: String,
    /// Byte offset of the error in the statement text, if known.
    pub offset: Option<u32>,
}

impl DbError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: None,
            message: message.into(),
            offset: None,
        }
    }

    /// True if the Oracle client libraries could not be loaded (DPI-1047).
    pub fn is_missing_client(&self) -> bool {
        self.message.contains("DPI-1047")
    }
}

impl From<oracle::Error> for DbError {
    fn from(e: oracle::Error) -> Self {
        match e.db_error() {
            Some(db) => Self {
                code: Some(db.code()).filter(|c| *c != 0),
                message: db.message().trim_end().to_owned(),
                offset: Some(db.offset()).filter(|o| *o != 0),
            },
            None => Self::new(e.to_string()),
        }
    }
}
