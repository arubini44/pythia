//! Database connectivity (Oracle, MySQL), directly or through SSH.
//!
//! Each [`Session`] owns a worker thread with one database connection. The UI
//! never touches the connection directly: it sends [`DbRequest`]s and polls
//! [`DbEvent`]s, so a slow query can never block a frame. The only call that
//! bypasses the queue is [`Session::cancel`], which has to interrupt the
//! worker while it is busy.
//!
//! Requests and events are the same for every database; a connection's
//! [`Dialect`] tells what its database supports.

pub mod compat;
mod dialect;
mod meta;
mod mysql_meta;
mod mysql_session;
pub mod recompile;
mod session;
pub mod ssh;
mod types;

pub use dialect::{DbKind, Dialect, Feature};
pub use meta::{
    CompileError, DbObject, Detail, Member, MetaQuery, MetaResult, ObjectType, TableColumn,
    like_pattern, object_ddl, object_source,
};
pub use session::{Session, open_connection, stream_query};
pub use types::{
    Cell, Client, Column, Compiled, ConnectParams, DbError, DbEvent, DbRequest, ExecSummary, RunFn,
    ServerInfo, TaskFn, TaskResult,
};

/// Re-exported so tools running [`DbRequest::Task`] can use the driver.
/// The database driver: the `oracle` crate (ODPI-C, needs the Instant
/// Client), or with the `thin` feature a stand-in with the same API on the
/// pure-Rust `oracledb` driver (#13).
#[cfg(not(feature = "thin"))]
pub use ::oracle;
#[cfg(feature = "thin")]
#[path = "thin.rs"]
pub mod oracle;
