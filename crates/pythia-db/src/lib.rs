//! Oracle connectivity.
//!
//! Each [`Session`] owns a worker thread with one database connection. The UI
//! never touches the connection directly: it sends [`DbRequest`]s and polls
//! [`DbEvent`]s, so a slow query can never block a frame. The only call that
//! bypasses the queue is [`Session::cancel`], which has to interrupt the
//! worker while it is busy.

mod meta;
mod session;
mod types;

pub use meta::{
    CompileError, DbObject, Detail, Member, MetaQuery, MetaResult, ObjectType, TableColumn,
    like_pattern, object_ddl, object_source,
};
pub use session::{Session, stream_query};
pub use types::{
    Cell, Column, Compiled, ConnectParams, DbError, DbEvent, DbRequest, ExecSummary, ServerInfo,
    TaskFn, TaskResult,
};

/// Re-exported so tools running [`DbRequest::Task`] can use the driver.
pub use oracle;
