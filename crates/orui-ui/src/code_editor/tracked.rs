//! Line-anchored state that must follow edits: folded regions and bookmarks.

use super::fold::shift_line;

/// A bookmark on a 0-based line; numbered ones (1–9) can be jumped to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bookmark {
    pub number: Option<u8>,
    pub line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Tracked {
    /// Folded regions as (first line, last line), 0-based.
    pub folds: Vec<(usize, usize)>,
    pub bookmarks: Vec<Bookmark>,
}

impl Tracked {
    /// Follows an edit at `line` that removed/inserted line breaks.
    pub fn shift(&mut self, line: usize, removed: usize, inserted: usize, at_line_start: bool) {
        if removed == 0 && inserted == 0 {
            return;
        }
        let shift = |x: usize| shift_line(x, line, removed, inserted, at_line_start);
        self.folds.retain_mut(|(start, end)| {
            let Ok(s) = shift(*start) else {
                return false;
            };
            let e = shift(*end).unwrap_or_else(|l| l);
            *start = s;
            *end = e;
            e > s
        });
        for b in &mut self.bookmarks {
            b.line = shift(b.line).unwrap_or_else(|l| l);
        }
        // Two bookmarks merged onto one line: keep the numbered one.
        self.bookmarks.sort_by_key(|b| (b.line, b.number.is_none()));
        self.bookmarks.dedup_by_key(|b| b.line);
    }

    pub fn clear(&mut self) {
        self.folds.clear();
        self.bookmarks.clear();
    }

    /// Toggles a bookmark. A numbered bookmark moves to `line` if it was
    /// elsewhere and is removed if it was already there.
    pub fn toggle_bookmark(&mut self, line: usize, number: Option<u8>) {
        if let Some(i) = self
            .bookmarks
            .iter()
            .position(|b| b.line == line && b.number == number)
        {
            self.bookmarks.remove(i);
            return;
        }
        if number.is_some() {
            self.bookmarks.retain(|b| b.number != number);
        }
        self.bookmarks.retain(|b| b.line != line);
        self.bookmarks.push(Bookmark { number, line });
        self.bookmarks.sort_by_key(|b| b.line);
    }

    /// Next (or previous) bookmarked line after `line`, wrapping around.
    pub fn next_bookmark(&self, line: usize, forward: bool) -> Option<usize> {
        let mut lines: Vec<usize> = self.bookmarks.iter().map(|b| b.line).collect();
        lines.sort_unstable();
        if forward {
            lines
                .iter()
                .copied()
                .find(|l| *l > line)
                .or_else(|| lines.first().copied())
        } else {
            lines
                .iter()
                .rev()
                .copied()
                .find(|l| *l < line)
                .or_else(|| lines.last().copied())
        }
    }

    /// Unfolds every fold hiding `line`. Returns whether anything changed.
    pub fn reveal(&mut self, line: usize) -> bool {
        let before = self.folds.len();
        self.folds.retain(|(s, e)| !(line > *s && line <= *e));
        self.folds.len() != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_edits() {
        let mut t = Tracked {
            folds: vec![(2, 5), (8, 9)],
            bookmarks: vec![
                Bookmark {
                    number: Some(1),
                    line: 4,
                },
                Bookmark {
                    number: None,
                    line: 10,
                },
            ],
        };
        // Two lines inserted after line 0.
        t.shift(0, 0, 2, false);
        assert_eq!(t.folds, [(4, 7), (10, 11)]);
        assert_eq!(
            t.bookmarks.iter().map(|b| b.line).collect::<Vec<_>>(),
            [6, 12]
        );
        // Lines 10..=11 joined into line 9: the second fold's start is gone.
        t.shift(9, 2, 0, false);
        assert_eq!(t.folds, [(4, 7)]);
        assert_eq!(
            t.bookmarks.iter().map(|b| b.line).collect::<Vec<_>>(),
            [6, 10]
        );
        // A fold whose end merges into its start disappears.
        t.shift(4, 3, 0, false);
        assert!(t.folds.is_empty());
    }

    #[test]
    fn bookmarks() {
        let mut t = Tracked::default();
        t.toggle_bookmark(3, Some(1));
        t.toggle_bookmark(7, None);
        t.toggle_bookmark(5, Some(1));
        assert_eq!(
            t.bookmarks,
            [
                Bookmark {
                    number: Some(1),
                    line: 5
                },
                Bookmark {
                    number: None,
                    line: 7
                }
            ]
        );
        assert_eq!(t.next_bookmark(5, true), Some(7));
        assert_eq!(t.next_bookmark(7, true), Some(5));
        assert_eq!(t.next_bookmark(5, false), Some(7));
        t.toggle_bookmark(5, Some(1));
        assert_eq!(t.bookmarks.len(), 1);
        // Merging lines keeps one bookmark per line.
        t.toggle_bookmark(8, Some(2));
        t.shift(7, 1, 0, false);
        assert_eq!(
            t.bookmarks,
            [Bookmark {
                number: Some(2),
                line: 7
            }]
        );
    }

    #[test]
    fn reveal() {
        let mut t = Tracked {
            folds: vec![(1, 9), (3, 5)],
            bookmarks: vec![],
        };
        assert!(!t.reveal(1));
        assert!(t.reveal(4));
        assert!(t.folds.is_empty());
    }
}
