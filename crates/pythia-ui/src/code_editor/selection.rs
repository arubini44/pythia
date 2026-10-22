//! Cursors and selections.

use std::ops::Range;

/// One caret with an optional selection. `anchor == head` means no selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started (stays put while extending).
    pub anchor: usize,
    /// Where the caret is.
    pub head: usize,
    /// Visual column to aim for when moving vertically.
    pub goal: Option<usize>,
}

impl Selection {
    pub fn caret(pos: usize) -> Self {
        Self {
            anchor: pos,
            head: pos,
            goal: None,
        }
    }

    pub fn new(anchor: usize, head: usize) -> Self {
        Self {
            anchor,
            head,
            goal: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.head
    }

    pub fn start(&self) -> usize {
        self.anchor.min(self.head)
    }

    pub fn end(&self) -> usize {
        self.anchor.max(self.head)
    }

    pub fn range(&self) -> Range<usize> {
        self.start()..self.end()
    }

    /// Moves the caret; extends the selection if `extend`, else collapses it.
    pub fn moved_to(self, pos: usize, extend: bool) -> Self {
        Self {
            anchor: if extend { self.anchor } else { pos },
            head: pos,
            goal: None,
        }
    }
}

/// Sorts selections and merges overlapping ones. Returns the new index of
/// the selection that was at `primary`.
pub fn normalize(sels: &mut Vec<Selection>, primary: usize) -> usize {
    if sels.len() <= 1 {
        return 0;
    }
    let primary_sel = sels[primary.min(sels.len() - 1)];
    sels.sort_by_key(|s| (s.start(), s.end()));
    let mut merged: Vec<Selection> = Vec::with_capacity(sels.len());
    let mut new_primary = 0;
    for s in sels.drain(..) {
        if let Some(last) = merged.last_mut() {
            let overlaps = s.start() < last.end()
                || (s.start() == last.end() && (s.is_empty() || last.is_empty()));
            if overlaps {
                let start = last.start().min(s.start());
                let end = last.end().max(s.end());
                // Keep the direction of the earlier selection.
                *last = if last.head >= last.anchor {
                    Selection::new(start, end)
                } else {
                    Selection::new(end, start)
                };
                if s == primary_sel {
                    new_primary = merged.len() - 1;
                }
                continue;
            }
        }
        if s == primary_sel {
            new_primary = merged.len();
        }
        merged.push(s);
    }
    *sels = merged;
    new_primary
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge() {
        let mut sels = vec![
            Selection::new(10, 15),
            Selection::caret(3),
            Selection::new(12, 20),
            Selection::caret(3),
        ];
        let primary = normalize(&mut sels, 2);
        assert_eq!(sels, vec![Selection::caret(3), Selection::new(10, 20)]);
        assert_eq!(primary, 1);
    }

    #[test]
    fn adjacent_selections_stay_separate() {
        let mut sels = vec![Selection::new(0, 2), Selection::new(2, 4)];
        normalize(&mut sels, 0);
        assert_eq!(sels.len(), 2);
    }
}
