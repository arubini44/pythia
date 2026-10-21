//! Editor state owned by a document.
//!
//! Documents talk to this wrapper only, so swapping the widget underneath
//! (TextEdit spike → rope-backed code editor) doesn't touch them.

use std::ops::Range;

use egui::Ui;
use orui_ui::SqlEditor;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerKind {
    Error,
    Warning,
}

#[derive(Debug, Clone)]
#[expect(dead_code, reason = "drawn by the rope-backed editor (in progress)")]
pub struct Marker {
    /// 1-based line.
    pub line: usize,
    pub kind: MarkerKind,
    pub message: String,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Editor {
    text: String,
    #[serde(skip)]
    view: View,
}

#[derive(Default)]
struct View {
    cursor: usize,
    selection: Option<Range<usize>>,
    line_col: (usize, usize),
    pending_select: Option<Range<usize>>,
    markers: Vec<Marker>,
}

impl Editor {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            view: View::default(),
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.view.pending_select = Some(0..0);
    }

    pub fn cursor(&self) -> usize {
        self.view.cursor.min(self.text.len())
    }

    /// Non-empty selection as a byte range.
    pub fn selection(&self) -> Option<Range<usize>> {
        self.view
            .selection
            .clone()
            .filter(|r| r.end <= self.text.len())
    }

    pub fn line_col(&self) -> (usize, usize) {
        self.view.line_col
    }

    pub fn select(&mut self, range: Range<usize>) {
        let end = range.end.min(self.text.len());
        self.view.pending_select = Some(range.start.min(end)..end);
    }

    /// Moves the cursor to the start of a 1-based line (and column).
    pub fn goto(&mut self, line: usize, column: usize) {
        let start = line_start(&self.text, line);
        let line_end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |i| start + i);
        let col = self.text[start..line_end]
            .char_indices()
            .nth(column.saturating_sub(1))
            .map_or(line_end, |(i, _)| start + i);
        self.view.pending_select = Some(col..col);
    }

    /// Replaces the selection (or inserts at the cursor) and selects the result.
    pub fn insert(&mut self, text: &str) {
        let range = self
            .selection()
            .unwrap_or_else(|| self.cursor()..self.cursor());
        self.text.replace_range(range.clone(), text);
        self.view.pending_select = Some(range.start..range.start + text.len());
    }

    pub fn set_markers(&mut self, markers: Vec<Marker>) {
        self.view.markers = markers;
    }

    #[expect(dead_code, reason = "drawn by the rope-backed editor (in progress)")]
    pub fn markers(&self) -> &[Marker] {
        &self.view.markers
    }

    pub fn ui(&mut self, ui: &mut Ui, id: impl egui::AsId, settings: &orui_config::EditorSettings) {
        self.show(ui, id, settings, false);
    }

    pub fn ui_read_only(
        &mut self,
        ui: &mut Ui,
        id: impl egui::AsId,
        settings: &orui_config::EditorSettings,
    ) {
        self.show(ui, id, settings, true);
    }

    fn show(
        &mut self,
        ui: &mut Ui,
        id: impl egui::AsId,
        settings: &orui_config::EditorSettings,
        read_only: bool,
    ) {
        let out = SqlEditor::new(&mut self.text, id)
            .font_size(settings.font_size)
            .line_numbers(settings.show_line_numbers)
            .select(self.view.pending_select.take())
            .read_only(read_only)
            .show(ui);
        self.view.cursor = out.cursor;
        self.view.selection = out.selection;
        self.view.line_col = out.line_col;
    }
}

/// Byte offset of the start of a 1-based line (end of text if out of range).
pub fn line_start(text: &str, line: usize) -> usize {
    if line <= 1 {
        return 0;
    }
    text.match_indices('\n')
        .nth(line - 2)
        .map_or(text.len(), |(i, _)| i + 1)
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
    fn goto_column() {
        let mut e = Editor::new("ab\ncdef");
        e.goto(2, 3);
        assert_eq!(e.view.pending_select, Some(5..5));
    }
}
