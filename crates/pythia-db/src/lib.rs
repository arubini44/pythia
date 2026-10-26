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

/// The database driver, for tools running [`DbRequest::Task`]: the `oracle`
/// API on the pure-Rust `oracledb` driver (no client libraries needed), or
/// with the `thick` feature the `oracle` crate itself (ODPI-C, needs the
/// Oracle Instant Client).
#[cfg(feature = "thick")]
pub use ::oracle;
#[cfg(not(feature = "thick"))]
#[path = "thin.rs"]
pub mod oracle;

/// How this build reaches Oracle, for the About dialog. With the `thick`
/// feature the first call loads the Oracle Client to ask for its version.
pub fn oracle_driver() -> &'static str {
    static DRIVER: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    DRIVER.get_or_init(|| {
        #[cfg(feature = "thick")]
        let driver = match ::oracle::Version::client() {
            Ok(v) => format!("Oracle Client {v}"),
            Err(_) => "Oracle Client (not found)".to_owned(),
        };
        #[cfg(not(feature = "thick"))]
        let driver = "Pure-Rust driver (no Oracle client)".to_owned();
        driver
    })
}
