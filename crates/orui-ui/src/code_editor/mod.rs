//! Rope-backed PL/SQL code editor widget (CONCEPT.md §5.3).
//!
//! [`CodeEditor`] owns the text and all editor state; call
//! [`CodeEditor::ui`] once per frame. Only visible lines are lexed, laid out
//! and painted, so large package bodies stay responsive. The editing logic
//! lives in `state.rs` and is independent of egui.

mod find;
mod highlight;
mod history;
mod matching;
mod search;
mod selection;
mod state;
mod text;
mod view;

use std::ops::Range;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use self::find::FindBar;
use self::selection::Selection;
use self::state::EditorState;

pub use self::search::SearchQuery;

/// Display settings, typically from the user's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorConfig {
    pub font_size: f32,
    pub tab_width: usize,
    pub show_line_numbers: bool,
    /// Render spaces as `·` and tabs as `→`.
    pub show_whitespace: bool,
    /// Highlight the line the cursor is on.
    pub highlight_current_line: bool,
    pub read_only: bool,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            tab_width: 4,
            show_line_numbers: true,
            show_whitespace: false,
            highlight_current_line: true,
            read_only: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkerKind {
    Error,
    Warning,
    Breakpoint,
    Bookmark,
    /// Where the debugger is stopped.
    ExecutionPoint,
}

/// A gutter marker, e.g. a compile error or breakpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// 1-based line number.
    pub line: usize,
    pub kind: MarkerKind,
    /// Shown as tooltip; may be empty.
    pub message: String,
}

/// Result of one frame. Offsets are byte offsets into [`CodeEditor::text`].
pub struct EditorOutput {
    pub response: egui::Response,
    /// Primary caret.
    pub cursor: usize,
    /// Primary selection, if not empty.
    pub selection: Option<Range<usize>>,
    /// 1-based line and column of the primary caret.
    pub line_col: (usize, usize),
    /// The text changed this frame.
    pub changed: bool,
    /// The marker area of the gutter was clicked on this 1-based line
    /// (e.g. to toggle a breakpoint).
    pub gutter_clicked: Option<usize>,
}

/// How the mouse is currently selecting.
#[derive(Debug, Clone, Copy)]
enum Drag {
    /// Plain drag from a char position.
    Chars { anchor: usize },
    /// Alt+drag block selection from (line, visual column).
    Block { from: (usize, usize) },
}

pub struct CodeEditor {
    state: EditorState,
    markers: Vec<Marker>,
    find: FindBar,
    /// Input of the go-to-line popup, if open.
    goto_line: Option<String>,
    id_salt: Option<egui::Id>,
    focus_requested: bool,
    scroll_to_cursor: bool,
    drag: Option<Drag>,
    /// Time of the last caret-affecting input, for blinking.
    blink_epoch: f64,
    /// Rows visible in the last frame (for PageUp/PageDown).
    page_rows: usize,
    pairs_cache: Option<(u64, Vec<matching::Pair>)>,
    width_cache: Option<(u64, usize)>,
}

impl Default for CodeEditor {
    fn default() -> Self {
        Self::new("")
    }
}

impl CodeEditor {
    pub fn new(text: &str) -> Self {
        Self {
            state: EditorState::new(text),
            markers: Vec::new(),
            find: FindBar::default(),
            goto_line: None,
            id_salt: None,
            focus_requested: false,
            scroll_to_cursor: false,
            drag: None,
            blink_epoch: 0.0,
            page_rows: 20,
            pairs_cache: None,
            width_cache: None,
        }
    }

    /// Sets the id used for focus and scroll state. Needed when several
    /// editors share one parent `Ui`; otherwise derived from the parent id.
    pub fn set_id_salt(&mut self, salt: impl egui::AsId) {
        self.id_salt = Some(egui::Id::new(salt));
    }

    pub fn text(&self) -> String {
        self.state.text()
    }

    /// Replaces the whole text and clears the undo history.
    pub fn set_text(&mut self, text: &str) {
        self.state.set_text(text);
        self.markers
            .retain(|m| m.line <= self.state.rope.len_lines());
    }

    pub fn len_bytes(&self) -> usize {
        self.state.rope.len_bytes()
    }

    pub fn line_count(&self) -> usize {
        self.state.rope.len_lines()
    }

    /// Increments on every change of the text.
    pub fn revision(&self) -> u64 {
        self.state.revision
    }

    /// Primary caret as byte offset.
    pub fn cursor(&self) -> usize {
        self.state.rope.char_to_byte(self.state.primary().head)
    }

    /// Primary selection as byte range, if not empty.
    pub fn selection(&self) -> Option<Range<usize>> {
        let s = self.state.primary();
        (!s.is_empty())
            .then(|| self.state.rope.char_to_byte(s.start())..self.state.rope.char_to_byte(s.end()))
    }

    pub fn selected_text(&self) -> Option<String> {
        let s = self.state.primary();
        (!s.is_empty()).then(|| self.state.rope.slice(s.range()).to_string())
    }

    /// 1-based line and column of the primary caret.
    pub fn line_col(&self) -> (usize, usize) {
        let (l, c) = text::line_col(&self.state.rope, self.state.primary().head);
        (l + 1, c + 1)
    }

    /// Selects a byte range (clamped, snapped to char boundaries) and scrolls to it.
    pub fn select(&mut self, range: Range<usize>) {
        let rope = &self.state.rope;
        let len = rope.len_bytes();
        let a = rope.byte_to_char(range.start.min(len));
        let b = rope.byte_to_char(range.end.min(len));
        self.state.set_selections(vec![Selection::new(a, b)], 0);
        self.scroll_to_cursor = true;
    }

    /// Moves the caret to the start of a 1-based line and scrolls to it.
    pub fn goto_line(&mut self, line: usize) {
        let pos = text::pos_of(&self.state.rope, line.saturating_sub(1), 0);
        self.state.set_caret(pos);
        self.scroll_to_cursor = true;
    }

    /// Moves the caret to a 1-based line and column.
    pub fn goto(&mut self, line: usize, col: usize) {
        let pos = text::pos_of(
            &self.state.rope,
            line.saturating_sub(1),
            col.saturating_sub(1),
        );
        self.state.set_caret(pos);
        self.scroll_to_cursor = true;
    }

    pub fn set_markers(&mut self, markers: Vec<Marker>) {
        self.markers = markers;
    }

    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// Inserts text at every caret (replacing selections), as one undo step.
    pub fn insert_at_cursor(&mut self, text: &str) {
        self.state.history.seal();
        self.state.insert(text, f64::MAX);
        self.state.history.seal();
        self.scroll_to_cursor = true;
    }

    /// Replaces the primary selection (or inserts at the caret).
    pub fn replace_selection(&mut self, text: &str) {
        let primary = self.state.primary();
        self.state.set_selections(vec![primary], 0);
        self.insert_at_cursor(text);
    }

    /// Replaces a byte range as one undo step (e.g. formatter output).
    pub fn replace_range(&mut self, range: Range<usize>, text: &str) {
        self.state.history.seal();
        self.state
            .replace_byte_ranges(vec![(range, text.to_owned())], f64::MAX);
        self.state.history.seal();
    }

    pub fn request_focus(&mut self) {
        self.focus_requested = true;
    }

    pub fn undo(&mut self) -> bool {
        self.state.undo()
    }

    pub fn redo(&mut self) -> bool {
        self.state.redo()
    }

    pub fn can_undo(&self) -> bool {
        self.state.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.state.history.can_redo()
    }

    /// Opens the find bar (with replace if `replace`).
    pub fn open_find(&mut self, replace: bool) {
        let seed = self.selected_text().filter(|t| !t.contains('\n'));
        self.find.open(replace, seed);
    }

    fn pairs(&mut self) -> &[matching::Pair] {
        let rev = self.state.revision;
        if self.pairs_cache.as_ref().is_none_or(|(r, _)| *r != rev) {
            self.pairs_cache = Some((rev, matching::find_pairs(&self.state.text())));
        }
        &self.pairs_cache.as_ref().expect("just set").1
    }

    /// Widest line in chars (tabs counted as one), cached per revision.
    fn max_line_chars(&mut self) -> usize {
        let rev = self.state.revision;
        if let Some((r, w)) = self.width_cache
            && r == rev
        {
            return w;
        }
        let w = self
            .state
            .rope
            .lines()
            .map(|l| l.len_chars())
            .max()
            .unwrap_or(0);
        self.width_cache = Some((rev, w));
        w
    }
}

impl Serialize for CodeEditor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.text())
    }
}

impl<'de> Deserialize<'de> for CodeEditor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Ok(Self::new(&text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_is_plain_text() {
        let editor = CodeEditor::new("select 1\nfrom dual");
        let json = serde_json::to_string(&editor).unwrap();
        assert_eq!(json, "\"select 1\\nfrom dual\"");
        let back: CodeEditor = serde_json::from_str(&json).unwrap();
        assert_eq!(back.text(), editor.text());
    }

    #[test]
    fn byte_offsets_with_multibyte_text() {
        let mut editor = CodeEditor::new("-- grüße\nselect 1");
        editor.goto(2, 3);
        assert_eq!(editor.cursor(), "-- grüße\nse".len());
        assert_eq!(editor.line_col(), (2, 3));
        editor.select(3..9);
        assert_eq!(editor.selected_text().as_deref(), Some("grüß"));
        assert_eq!(editor.selection(), Some(3..9));
        editor.replace_selection("x");
        assert_eq!(editor.text(), "-- xe\nselect 1");
        assert!(editor.undo());
        assert_eq!(editor.text(), "-- grüße\nselect 1");
    }
}
