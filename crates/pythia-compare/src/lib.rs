//! Schema and data compare with sync script generation (CONCEPT.md M7).
//!
//! - [`schema`]: object inventories and DDL of two schemas, structural table
//!   diffs, a dependency-ordered sync script that is safe by default.
//! - [`data`]: key-ordered streaming merge-join of a table on two
//!   connections, with INSERT/UPDATE/DELETE sync statements.
//! - [`ui`]: egui views (setup, summary, diff, script) driven by database
//!   tasks the host app runs on the chosen connections.

pub mod data;
pub mod ddl;
pub mod diff;
pub mod schema;
pub mod script;
pub mod table;
pub mod ui;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Oracle(#[from] pythia_db::oracle::Error),
    #[error("{0}")]
    Compare(String),
}

impl From<Error> for pythia_db::DbError {
    fn from(e: Error) -> Self {
        match e {
            Error::Oracle(e) => e.into(),
            Error::Compare(message) => pythia_db::DbError::new(message),
        }
    }
}
