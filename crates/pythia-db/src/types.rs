use std::fmt;
use std::time::Duration;

use pythia_config::Privilege;

use crate::meta::{CompileError, MetaQuery, MetaResult};

#[derive(Clone)]
pub struct ConnectParams {
    pub username: String,
    pub password: String,
    pub connect_string: String,
    pub privilege: Privilege,
    /// Statements run on every new session (login script, NLS, schema).
    pub init_sql: Vec<String>,
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

/// A closure run on the session's worker thread with its connection.
pub type TaskFn = Box<dyn FnOnce(&oracle::Connection) -> TaskResult + Send>;
pub type TaskResult = Result<Box<dyn std::any::Any + Send>, DbError>;

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
    /// Runs arbitrary code on the connection (used by tools such as the
    /// monitor). Answered with [`DbEvent::TaskDone`].
    Task {
        id: u64,
        task: TaskFn,
    },
    Disconnect,
}

impl fmt::Debug for DbRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbRequest::Execute {
                id,
                sql,
                max_rows,
                binds,
            } => f
                .debug_struct("Execute")
                .field("id", id)
                .field("sql", sql)
                .field("max_rows", max_rows)
                .field("binds", &binds.len())
                .finish(),
            DbRequest::Meta { id, query } => f
                .debug_struct("Meta")
                .field("id", id)
                .field("query", query)
                .finish(),
            DbRequest::FetchMore { id, max_rows } => f
                .debug_struct("FetchMore")
                .field("id", id)
                .field("max_rows", max_rows)
                .finish(),
            DbRequest::Commit => f.write_str("Commit"),
            DbRequest::Rollback => f.write_str("Rollback"),
            DbRequest::SetAutocommit(on) => f.debug_tuple("SetAutocommit").field(on).finish(),
            DbRequest::Task { id, .. } => f
                .debug_struct("Task")
                .field("id", id)
                .finish_non_exhaustive(),
            DbRequest::Disconnect => f.write_str("Disconnect"),
        }
    }
}

#[derive(Debug)]
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
    TaskDone {
        id: u64,
        result: TaskResult,
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
    /// Major and minor release, for features newer versions have.
    pub release: crate::compat::Release,
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

    /// The session is gone (network or instance failure, killed session).
    pub fn is_connection_lost(&self) -> bool {
        matches!(
            self.code,
            Some(28 | 1012 | 2396 | 3113 | 3114 | 3135 | 12537 | 12570)
        ) || [
            "DPI-1080",
            "DPI-1010",
            "ORA-03113",
            "ORA-03114",
            "ORA-03135",
        ]
        .iter()
        .any(|c| self.message.contains(c))
    }

    /// True if the Oracle client libraries could not be loaded (DPI-1047).
    pub fn is_missing_client(&self) -> bool {
        self.message.contains("DPI-1047")
    }
}

impl From<oracle::Error> for DbError {
    fn from(e: oracle::Error) -> Self {
        match e.db_error() {
            Some(db) => {
                let mut message = db.message().trim_end().to_owned();
                if db.code() == 1805 {
                    // Raised by the client while fetching, not by the database.
                    message.push_str(
                        "\nThe Oracle client's time zone file lacks a time zone region in this \
                         result (TIMESTAMP WITH TIME ZONE). Instant Client \"Basic Light\" only \
                         has the small time zone file: install \"Basic\" instead, or select the \
                         column with TO_CHAR().",
                    );
                }
                Self {
                    code: Some(db.code()).filter(|c| *c != 0),
                    message,
                    offset: Some(db.offset()).filter(|o| *o != 0),
                }
            }
            None => Self::new(e.to_string()),
        }
    }
}
