//! Oracle connectivity.
//!
//! Each [`Session`] owns a worker thread with one database connection. The UI
//! never touches the connection directly: it sends [`DbRequest`]s and polls
//! [`DbEvent`]s, so a slow query can never block a frame. The only call that
//! bypasses the queue is [`Session::cancel`], which has to interrupt the
//! worker while it is busy.

pub mod compat;
mod meta;
pub mod recompile;
mod session;
mod types;

pub use meta::{
    CompileError, DbObject, Detail, Member, MetaQuery, MetaResult, ObjectType, TableColumn,
    like_pattern, object_ddl, object_source,
};
pub use session::{Session, open_connection, stream_query};
pub use types::{
    Cell, Column, Compiled, ConnectParams, DbError, DbEvent, DbRequest, ExecSummary, ServerInfo,
    TaskFn, TaskResult,
};

/// The database driver, for tools running [`DbRequest::Task`]: the `oracle`
/// API on the pure-Rust `oracledb` driver (no client libraries needed), or
/// with the `thick` feature the `oracle` crate itself (ODPI-C, needs the
/// Oracle Instant Client).
#[cfg(feature = "thick")]
pub use ::oracle;
#[cfg(not(feature = "thick"))]
#[path = "thin.rs"]
pub mod oracle;
