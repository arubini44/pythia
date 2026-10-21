//! Undo/redo history with grouping of typing bursts.

use super::selection::Selection;

/// One primitive change. `at` is a char index in the document as it is
/// right before this edit is applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub at: usize,
    pub removed: String,
    pub inserted: String,
}

/// What kind of change a transaction is; decides whether it merges with the
/// previous one into a single undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    /// Typing characters.
    Insert,
    /// Backspace / Delete.
    Delete,
    /// Everything else; never merged.
    Other,
}

#[derive(Debug, Clone)]
pub struct Transaction {
    /// Applied in order.
    pub edits: Vec<Edit>,
    pub before: Vec<Selection>,
    pub after: Vec<Selection>,
    kind: EditKind,
    time: f64,
}

/// Typing pauses longer than this start a new undo step.
const GROUP_TIMEOUT: f64 = 1.0;

#[derive(Debug, Default)]
pub struct History {
    undo: Vec<Transaction>,
    redo: Vec<Transaction>,
    /// Set by anything that should break the current typing group
    /// (cursor movement, focus loss, ...).
    sealed: bool,
}

impl History {
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.sealed = false;
    }

    /// Prevents the next edit from merging into the previous step.
    pub fn seal(&mut self) {
        self.sealed = true;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn record(
        &mut self,
        edits: Vec<Edit>,
        before: Vec<Selection>,
        after: Vec<Selection>,
        kind: EditKind,
        time: f64,
    ) {
        if edits.is_empty() {
            return;
        }
        self.redo.clear();
        if !self.sealed
            && let Some(prev) = self.undo.last_mut()
            && should_merge(prev, &edits, kind, time)
        {
            let edit = edits.into_iter().next().expect("one edit");
            let last = prev.edits.last_mut().expect("non-empty");
            match kind {
                EditKind::Insert => last.inserted.push_str(&edit.inserted),
                EditKind::Delete if edit.at < last.at => {
                    // Backspace: the removed text precedes what was removed before.
                    last.at = edit.at;
                    last.removed.insert_str(0, &edit.removed);
                }
                _ => last.removed.push_str(&edit.removed),
            }
            prev.after = after;
            prev.time = time;
            return;
        }
        self.sealed = false;
        self.undo.push(Transaction {
            edits,
            before,
            after,
            kind,
            time,
        });
    }

    /// Replaces the "after" selections of the most recent step.
    pub fn set_last_after(&mut self, after: Vec<Selection>) {
        if let Some(last) = self.undo.last_mut() {
            last.after = after;
        }
    }

    pub fn undo(&mut self) -> Option<&Transaction> {
        let tx = self.undo.pop()?;
        self.sealed = true;
        self.redo.push(tx);
        self.redo.last()
    }

    pub fn redo(&mut self) -> Option<&Transaction> {
        let tx = self.redo.pop()?;
        self.sealed = true;
        self.undo.push(tx);
        self.undo.last()
    }
}

fn should_merge(prev: &Transaction, edits: &[Edit], kind: EditKind, time: f64) -> bool {
    if kind == EditKind::Other || prev.kind != kind || time - prev.time > GROUP_TIMEOUT {
        return false;
    }
    let ([last], [edit]) = (prev.edits.as_slice(), edits) else {
        return false;
    };
    match kind {
        EditKind::Insert => {
            let contiguous = last.removed.is_empty()
                && edit.removed.is_empty()
                && edit.at == last.at + last.inserted.chars().count();
            // Start a new step at word boundaries: typing a space after a word,
            // or any line break.
            let prev_char = last.inserted.chars().last();
            let boundary = edit.inserted.contains('\n')
                || (edit.inserted.starts_with(char::is_whitespace)
                    && prev_char.is_some_and(|c| !c.is_whitespace()));
            contiguous && !boundary
        }
        EditKind::Delete => {
            let pure = last.inserted.is_empty() && edit.inserted.is_empty();
            let backspace = edit.at + edit.removed.chars().count() == last.at;
            let delete = edit.at == last.at;
            pure && (backspace || delete)
        }
        EditKind::Other => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(at: usize, s: &str) -> Vec<Edit> {
        vec![Edit {
            at,
            removed: String::new(),
            inserted: s.into(),
        }]
    }

    fn del(at: usize, s: &str) -> Vec<Edit> {
        vec![Edit {
            at,
            removed: s.into(),
            inserted: String::new(),
        }]
    }

    fn record(h: &mut History, edits: Vec<Edit>, kind: EditKind, time: f64) {
        h.record(edits, vec![], vec![], kind, time);
    }

    #[test]
    fn typing_groups_until_word_boundary() {
        let mut h = History::default();
        record(&mut h, ins(0, "s"), EditKind::Insert, 0.0);
        record(&mut h, ins(1, "e"), EditKind::Insert, 0.1);
        record(&mut h, ins(2, "l"), EditKind::Insert, 0.2);
        record(&mut h, ins(3, " "), EditKind::Insert, 0.3);
        record(&mut h, ins(4, "x"), EditKind::Insert, 0.4);
        assert_eq!(h.undo.len(), 2);
        assert_eq!(h.undo[0].edits[0].inserted, "sel");
        assert_eq!(h.undo[1].edits[0].inserted, " x");
    }

    #[test]
    fn pauses_and_seals_break_groups() {
        let mut h = History::default();
        record(&mut h, ins(0, "a"), EditKind::Insert, 0.0);
        record(&mut h, ins(1, "b"), EditKind::Insert, 5.0);
        h.seal();
        record(&mut h, ins(2, "c"), EditKind::Insert, 5.1);
        assert_eq!(h.undo.len(), 3);
    }

    #[test]
    fn backspace_groups() {
        let mut h = History::default();
        record(&mut h, del(4, "d"), EditKind::Delete, 0.0);
        record(&mut h, del(3, "c"), EditKind::Delete, 0.1);
        record(&mut h, del(2, "b"), EditKind::Delete, 0.2);
        assert_eq!(h.undo.len(), 1);
        assert_eq!(
            h.undo[0].edits[0],
            Edit {
                at: 2,
                removed: "bcd".into(),
                inserted: String::new()
            }
        );
    }

    #[test]
    fn redo_cleared_by_new_edit() {
        let mut h = History::default();
        record(&mut h, ins(0, "a"), EditKind::Other, 0.0);
        assert!(h.undo().is_some());
        assert!(h.can_redo());
        record(&mut h, ins(0, "b"), EditKind::Other, 1.0);
        assert!(!h.can_redo());
    }
}
