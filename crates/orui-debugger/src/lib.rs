//! PL/SQL debugger for orui, built on `DBMS_DEBUG_JDWP`.
//!
//! The debugger listens on a TCP port; the session under test runs
//! `DBMS_DEBUG_JDWP.CONNECT_TCP(host, port)` and the database connects back
//! and is driven with JDWP. Oracle presents PL/SQL units as classes named
//! `$Oracle.<Kind>.<OWNER>.<NAME>` (anonymous blocks: `$Oracle.Block.<OWNER>.<hash>`),
//! code indexes equal source lines, and variables are objects of
//! `$Oracle.Builtin.<TYPE>` whose `_value` field holds the value as text.
//!
//! - [`Debugger`]: the debugger worker (UI side, channel based).
//! - [`target`]: running code under the debugger, debug compilation,
//!   privilege checks and the admin setup script.
//! - [`ui`]: an egui debugger panel and the breakpoint model.
//! - [`jdwp`]: the wire protocol.

mod condition;
mod debugger;
pub mod jdwp;
pub mod target;
mod types;
pub mod ui;

pub use debugger::Debugger;
pub use types::{
    Breakpoint, BreakpointOptions, DebugCommand, DebugEvent, Frame, Location, StopReason, Unit,
    UnitType, Variable, Watch,
};
