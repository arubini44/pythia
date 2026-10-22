//! Database monitoring for Pythia: dashboard, sessions and blocking tree,
//! session details, AWR/ASH reports and a tkprof-style trace analyzer.
//!
//! - [`queries`]: plain data functions over `&oracle::Connection`.
//! - [`insights`]: plain-language findings derived from a snapshot.
//! - [`tkprof`]: pure-Rust 10046 trace parser and aggregator.
//! - [`ui`]: egui views and the [`Monitor`] driver that hosts them.

pub mod insights;
pub mod queries;
pub mod tkprof;
pub mod ui;

pub use ui::{Analysis, Monitor, MonitorAction, MonitorOutput, MonitorRequest, MonitorTab, Token};
