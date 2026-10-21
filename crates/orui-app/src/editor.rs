//! Editor state owned by a document.
//!
//! Wraps the rope-backed [`CodeEditor`] and keeps a `String` copy of its text
//! (refreshed when the editor's revision changes), so documents can borrow
//! the text cheaply for statement splitting and dirty checks.

use std::ops::Range;

use egui::Ui;
use orui_ui::code_editor::{CodeEditor, EditorConfig, EditorOutput};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub use orui_ui::code_editor::{Marker, MarkerKind};

pub struct Editor {
    inner: CodeEditor,
    text: String,
    revision: u64,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new("")
    }
}

impl Serialize for Editor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.text.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Editor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Editor::new)
    }
}

impl Editor {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let inner = CodeEditor::new(&text);
        let revision = inner.revision();
        Self { inner, text, revision }
    }

    fn refresh(&mut self) {
        if self.inner.revision() != self.revision {
            self.text = self.inner.text();
            self.revision = self.inner.revision();
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.inner.set_text(&text);
        self.text = text;
        self.revision = self.inner.revision();
        self.inner.select(0..0);
    }

    pub fn cursor(&self) -> usize {
        self.inner.cursor().min(self.text.len())
    }

    /// Non-empty selection as a byte range.
    pub fn selection(&self) -> Option<Range<usize>> {
        self.inner.selection().filter(|r| r.end <= self.text.len() && !r.is_empty())
    }

    pub fn line_col(&self) -> (usize, usize) {
        self.inner.line_col()
    }

    pub fn select(&mut self, range: Range<usize>) {
        let end = range.end.min(self.text.len());
        self.inner.select(range.start.min(end)..end);
        self.inner.request_focus();
    }

    /// Moves the cursor to a 1-based line and column.
    pub fn goto(&mut self, line: usize, column: usize) {
        self.inner.goto(line, column);
        self.inner.request_focus();
    }

    pub fn set_markers(&mut self, markers: Vec<Marker>) {
        self.inner.set_markers(markers);
    }

    /// Replaces the selection (or inserts at the cursor), as one undo step.
    pub fn insert(&mut self, text: &str) {
        self.inner.replace_selection(text);
        self.refresh();
    }

    pub fn ui(&mut self, ui: &mut Ui, id: impl egui::AsId, settings: &orui_config::EditorSettings) -> EditorOutput {
        self.show(ui, id, settings, false)
    }

    pub fn ui_read_only(
        &mut self,
        ui: &mut Ui,
        id: impl egui::AsId,
        settings: &orui_config::EditorSettings,
    ) -> EditorOutput {
        self.show(ui, id, settings, true)
    }

    fn show(
        &mut self,
        ui: &mut Ui,
        id: impl egui::AsId,
        settings: &orui_config::EditorSettings,
        read_only: bool,
    ) -> EditorOutput {
        let config = EditorConfig {
            font_size: settings.font_size,
            tab_width: settings.tab_width,
            show_line_numbers: settings.show_line_numbers,
            read_only,
            ..EditorConfig::default()
        };
        self.inner.set_id_salt(id);
        let out = self.inner.ui(ui, &config);
        self.refresh();
        out
    }
}

/// Byte offset of the start of a 1-based line (end of text if out of range).
pub fn line_start(text: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    text.match_indices('\n').nth(line - 2).map_or(text.len(), |(i, _)| i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let t = "ab\ncd\nef";
        assert_eq!(line_start(t, 1), 0);
        assert_eq!(line_start(t, 2), 3);
        assert_eq!(line_start(t, 3), 6);
        assert_eq!(line_start(t, 9), 8);
    }

    #[test]
    fn text_follows_edits() {
        let mut e = Editor::new("select 1");
        e.select(0..6);
        e.insert("SELECT");
        assert_eq!(e.text(), "SELECT 1");
        assert_eq!(serde_json::to_string(&e).unwrap(), "\"SELECT 1\"");
    }
}
