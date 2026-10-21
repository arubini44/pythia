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
    /// Compile errors from the database.
    compile_markers: Vec<Marker>,
    /// Syntax problems found while typing.
    syntax_markers: Vec<Marker>,
    /// Breakpoints and the current execution line while debugging.
    debug_markers: Vec<Marker>,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new("")
    }
}

/// Saved form: just the text, or the text with bookmarks and folds.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum Saved {
    Text(String),
    Full {
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        bookmarks: Vec<(Option<u8>, usize)>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        folds: Vec<usize>,
    },
}

impl Serialize for Editor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let bookmarks = self.inner.bookmarks();
        let folds = self.inner.folded_lines();
        if bookmarks.is_empty() && folds.is_empty() {
            return self.text.serialize(serializer);
        }
        Saved::Full {
            text: self.text.clone(),
            bookmarks,
            folds,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Editor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Saved::deserialize(deserializer)? {
            Saved::Text(text) => Editor::new(text),
            Saved::Full {
                text,
                bookmarks,
                folds,
            } => {
                let mut editor = Editor::new(text);
                editor.inner.set_bookmarks(bookmarks);
                editor.inner.set_folded_lines(&folds);
                editor
            }
        })
    }
}

impl Editor {
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let inner = CodeEditor::new(&text);
        let revision = inner.revision();
        Self {
            inner,
            text,
            revision,
            compile_markers: Vec::new(),
            syntax_markers: Vec::new(),
            debug_markers: Vec::new(),
        }
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
        self.inner
            .selection()
            .filter(|r| r.end <= self.text.len() && !r.is_empty())
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

    /// Compile error markers (replacing the previous ones).
    pub fn set_markers(&mut self, markers: Vec<Marker>) {
        self.compile_markers = markers;
        self.push_markers();
    }

    /// Syntax markers from live analysis (replacing the previous ones).
    pub fn set_syntax_markers(&mut self, markers: Vec<Marker>) {
        self.syntax_markers = markers;
        self.push_markers();
    }

    fn push_markers(&mut self) {
        // Compile errors are authoritative; skip syntax markers on their lines.
        let mut all = self.compile_markers.clone();
        all.extend(
            self.syntax_markers
                .iter()
                .filter(|m| !self.compile_markers.iter().any(|c| c.line == m.line))
                .cloned(),
        );
        all.extend(self.debug_markers.iter().cloned());
        self.inner.set_markers(all);
    }

    /// Breakpoint lines and the execution line (1-based) from the debugger.
    pub fn set_debug_lines(&mut self, breakpoints: &[u32], current: Option<u32>) {
        let mut markers: Vec<Marker> = breakpoints
            .iter()
            .map(|l| Marker {
                line: *l as usize,
                kind: MarkerKind::Breakpoint,
                message: "Breakpoint".into(),
            })
            .collect();
        if let Some(line) = current {
            markers.push(Marker {
                line: line as usize,
                kind: MarkerKind::ExecutionPoint,
                message: "Stopped here".into(),
            });
        }
        let same = markers.len() == self.debug_markers.len()
            && markers
                .iter()
                .zip(&self.debug_markers)
                .all(|(a, b)| a.line == b.line && a.kind == b.kind);
        if !same {
            self.debug_markers = markers;
            self.push_markers();
            if let Some(line) = current {
                self.inner.goto_line(line as usize);
            }
        }
    }

    /// Replaces `range` with `text` as one undo step and puts the cursor on
    /// `line` (1-based).
    pub fn replace(&mut self, range: Range<usize>, text: &str, line: usize) {
        self.inner.replace_range(range, text);
        self.refresh();
        self.inner.goto_line(line.max(1));
    }

    /// Changes whenever the text changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Replaces the selection (or inserts at the cursor), as one undo step.
    pub fn insert(&mut self, text: &str) {
        self.inner.replace_selection(text);
        self.refresh();
    }

    /// Labels in the open completion popup.
    #[cfg_attr(not(test), expect(dead_code, reason = "used by UI tests"))]
    pub fn completion_labels(&self) -> Vec<&str> {
        self.inner.completion_labels()
    }

    /// The underlying widget, for code assistance popups.
    pub fn inner(&mut self) -> &mut CodeEditor {
        &mut self.inner
    }

    pub fn ui(
        &mut self,
        ui: &mut Ui,
        id: impl egui::AsId,
        settings: &orui_config::EditorSettings,
    ) -> EditorOutput {
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
            show_whitespace: settings.show_whitespace,
            highlight_current_line: settings.highlight_current_line,
            // Automatic completion off = only Ctrl+Space.
            auto_complete_min_chars: if settings.auto_complete {
                settings.auto_complete_min_chars.max(1)
            } else {
                usize::MAX
            },
            auto_complete_delay_ms: settings.auto_complete_delay_ms,
            read_only,
            ..EditorConfig::default()
        };
        self.inner.set_id_salt(id);
        let out = self.inner.ui(ui, &config);
        // Names dragged from the object browser.
        if !read_only && let Some(dropped) = out.response.dnd_release_payload::<DroppedText>() {
            self.inner.insert_at_cursor(&dropped.0);
            out.response.request_focus();
        }
        self.refresh();
        out
    }
}

/// Text dragged onto an editor (e.g. an object name from the browser).
#[derive(Debug, Clone)]
pub struct DroppedText(pub String);

/// The dotted path ending with the word at `offset`, e.g. `l_rec.name` when
/// hovering `name` (or `l_rec` when hovering `l_rec`), with its byte range.
pub fn path_at(text: &str, offset: usize) -> Option<(Range<usize>, &str)> {
    let word = |c: char| c.is_alphanumeric() || matches!(c, '_' | '$' | '#');
    let offset = offset.min(text.len());
    if !text.is_char_boundary(offset) {
        return None;
    }
    let end = text[offset..]
        .find(|c: char| !word(c))
        .map_or(text.len(), |i| offset + i);
    // Back over the word and any `name.` / `name(1).` before it.
    let mut start = text[..offset]
        .rfind(|c: char| !word(c))
        .map_or(0, |i| i + 1);
    loop {
        let before = &text[..start];
        let Some(head) = before.strip_suffix('.') else {
            break;
        };
        let head = head.trim_end_matches(|c: char| c.is_ascii_digit() || c == '(' || c == ')');
        let begin = head.rfind(|c: char| !word(c)).map_or(0, |i| i + 1);
        if begin == head.len() {
            break;
        }
        start = begin;
    }
    (start < end).then(|| (start..end, &text[start..end]))
}

/// Answers a hover request with the debugger's value of the path under the
/// mouse, if stopped in `unit`. Returns whether it did.
pub fn debug_hover(
    editor: &mut Editor,
    offset: usize,
    debug: &orui_debugger::ui::DebugSession,
    unit: &orui_debugger::Unit,
) -> bool {
    let Some((range, path)) = path_at(editor.text(), offset) else {
        return false;
    };
    let Some((title, body)) = debug.hover(unit, path) else {
        return false;
    };
    editor.inner().show_hover(range, title, body);
    true
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
    fn paths_under_the_mouse() {
        let text = "x := l_rec.items(2).name + l_total;";
        let at = |word: &str| path_at(text, text.find(word).unwrap() + 1).map(|(_, p)| p);
        assert_eq!(at("name"), Some("l_rec.items(2).name"));
        assert_eq!(at("items"), Some("l_rec.items"));
        assert_eq!(at("l_rec"), Some("l_rec"));
        assert_eq!(at("l_total"), Some("l_total"));
        assert_eq!(path_at(text, text.find(":=").unwrap()), None);
    }

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

    #[test]
    fn bookmarks_and_folds_survive_saving() {
        let mut e = Editor::new("begin\n  null;\n  null;\nend;\nselect 1 from dual;\n");
        e.inner().toggle_bookmark(5, Some(3));
        e.inner().toggle_fold_at(1);
        let json = serde_json::to_string(&e).unwrap();
        let mut back: Editor = serde_json::from_str(&json).unwrap();
        assert_eq!(back.text(), e.text());
        assert_eq!(back.inner().bookmarks(), vec![(Some(3), 5)]);
        assert_eq!(back.inner().folded_lines(), vec![1]);
        // Old sessions saved just the text.
        let old: Editor = serde_json::from_str("\"select 2\"").unwrap();
        assert_eq!(old.text(), "select 2");
    }
}
