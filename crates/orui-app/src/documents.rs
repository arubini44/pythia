//! Documents are the dockable windows bound to a connection: SQL windows,
//! program windows and object views.

use std::sync::atomic::{AtomicU64, Ordering};

use egui::{RichText, Ui};
use orui_db::{DbEvent, DbRequest, ObjectType};
use serde::{Deserialize, Serialize};

use crate::object_view::ObjectView;
use crate::program_window::ProgramWindow;
use crate::sql_window::SqlWindow;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DocId(pub u64);

static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

/// Unique id for a database request, across all sessions.
pub fn request_id() -> u64 {
    NEXT_REQUEST.fetch_add(1, Ordering::Relaxed)
}

/// Where a document wants a request to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// The document's own session (its transaction).
    Own,
    /// The connection's main session (metadata lookups).
    Main,
}

/// A stored object, as documents and the browser refer to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectRef {
    pub owner: String,
    pub name: String,
    pub object_type: ObjectType,
}

impl ObjectRef {
    pub fn qualified(&self) -> String {
        format!("{}.{}", quote_ident(&self.owner), quote_ident(&self.name))
    }
}

/// Quotes an identifier only if Oracle would need it.
pub fn quote_ident(name: &str) -> String {
    let plain = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "_$#".contains(c))
        && !orui_plsql::is_keyword(name);
    if plain {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Things a document asks the app to do after a frame or an event.
#[derive(Default)]
pub struct Effects {
    pub requests: Vec<(Target, DbRequest)>,
    pub open: Vec<OpenRequest>,
    /// Statements that ran successfully, for the query history.
    pub executed: Vec<String>,
    /// The document wants its running statement cancelled.
    pub cancel: bool,
    /// Lines for the application log.
    pub log: Vec<String>,
}

impl Effects {
    pub fn request(&mut self, target: Target, request: DbRequest) {
        self.requests.push((target, request));
    }

    pub fn extend(&mut self, other: Effects) {
        self.requests.extend(other.requests);
        self.open.extend(other.open);
        self.executed.extend(other.executed);
        self.cancel |= other.cancel;
        self.log.extend(other.log);
    }
}

/// Request to open a new document (from the browser or another document).
#[derive(Debug, Clone)]
pub enum OpenRequest {
    Sql {
        connection: String,
        text: String,
        run: bool,
    },
    Program {
        connection: String,
        object: ObjectRef,
    },
    Object {
        connection: String,
        object: ObjectRef,
    },
    /// Show this connection in the object browser.
    Browse { connection: String },
    Monitor {
        connection: String,
        start: crate::monitor_window::MonitorStart,
    },
}

/// A query result as shown in a grid.
pub type TableData = (Vec<orui_db::Column>, Vec<Vec<orui_db::Cell>>);

#[derive(Serialize, Deserialize)]
#[expect(
    clippy::large_enum_variant,
    reason = "a handful of documents, never copied"
)]
pub enum DocKind {
    Sql(SqlWindow),
    Program(ProgramWindow),
    Object(ObjectView),
    Monitor(crate::monitor_window::MonitorWindow),
}

#[derive(Serialize, Deserialize)]
pub struct Document {
    /// Display name of the connection this document runs on.
    pub connection: Option<String>,
    pub kind: DocKind,
}

/// Context handed to documents while drawing.
pub struct DocContext<'a> {
    pub id: DocId,
    pub settings: &'a orui_config::Settings,
    pub connected: bool,
    pub fetch_size: usize,
    /// Display name of the document's connection.
    pub connection: Option<String>,
}

impl Document {
    pub fn title(&self) -> String {
        match &self.kind {
            DocKind::Sql(w) => w.title.clone(),
            DocKind::Program(w) => w.title(),
            DocKind::Object(v) => v.title(),
            DocKind::Monitor(m) => m.title(),
        }
    }

    pub fn tab_title(&self, color: Option<egui::Color32>) -> RichText {
        let busy = if self.is_busy() { " …" } else { "" };
        let dirty = if self.is_dirty() { " •" } else { "" };
        let text = RichText::new(format!("{}{dirty}{busy}", self.title()));
        match color {
            Some(c) => text.color(c),
            None => text,
        }
    }

    pub fn is_busy(&self) -> bool {
        match &self.kind {
            DocKind::Sql(w) => w.is_running(),
            DocKind::Program(w) => w.is_busy(),
            DocKind::Object(v) => v.is_loading(),
            DocKind::Monitor(m) => m.is_busy(),
        }
    }

    pub fn is_dirty(&self) -> bool {
        match &self.kind {
            DocKind::Program(w) => w.is_dirty(),
            _ => false,
        }
    }

    /// Text that "execute" and friends act on, if this document has an editor.
    pub fn sql_window(&mut self) -> Option<&mut SqlWindow> {
        match &mut self.kind {
            DocKind::Sql(w) => Some(w),
            _ => None,
        }
    }

    pub fn on_event(&mut self, event: DbEvent, cx: &DocContext) -> Effects {
        match &mut self.kind {
            DocKind::Sql(w) => w.on_event(event, cx),
            DocKind::Program(w) => w.on_event(event),
            DocKind::Object(v) => v.on_event(event),
            DocKind::Monitor(m) => m.on_event(event),
        }
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        match &mut self.kind {
            DocKind::Sql(w) => w.ui(ui, cx),
            DocKind::Program(w) => w.ui(ui, cx),
            DocKind::Object(v) => v.ui(ui, cx, self.connection.as_deref()),
            DocKind::Monitor(m) => m.ui(ui, cx),
        }
    }

    /// Called once after the document was created or its connection changed.
    pub fn on_attach(&mut self) -> Effects {
        match &mut self.kind {
            DocKind::Sql(_) => Effects::default(),
            DocKind::Program(w) => w.load(),
            DocKind::Object(v) => v.load(),
            DocKind::Monitor(_) => Effects::default(),
        }
    }

    /// Line/column of the cursor and a status summary for the status bar.
    pub fn status(&self) -> (Option<(usize, usize)>, Option<String>) {
        match &self.kind {
            DocKind::Sql(w) => (Some(w.state.line_col), w.result_status()),
            DocKind::Program(w) => (Some(w.line_col()), None),
            DocKind::Object(_) | DocKind::Monitor(_) => (None, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers() {
        assert_eq!(quote_ident("EMP"), "EMP");
        assert_eq!(quote_ident("EMP_2$"), "EMP_2$");
        assert_eq!(quote_ident("Emp"), "\"Emp\"");
        assert_eq!(quote_ident("TABLE"), "\"TABLE\"");
        assert_eq!(quote_ident("A B"), "\"A B\"");
    }
}
