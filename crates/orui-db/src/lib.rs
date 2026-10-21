//! Oracle connectivity.
//!
//! Each [`Session`] owns a worker thread with one database connection. The UI
//! never touches the connection directly: it sends [`DbRequest`]s and polls
//! [`DbEvent`]s, so a slow query can never block a frame. The only call that
//! bypasses the queue is [`Session::cancel`], which has to interrupt the
//! worker while it is busy.

mod session;
mod types;

pub use session::Session;
pub use types::{
    Cell, Column, ConnectParams, DbError, DbEvent, DbRequest, ExecSummary, ServerInfo,
};
