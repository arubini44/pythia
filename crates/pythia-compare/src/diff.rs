//! Line diffs for display: side-by-side rows with intra-line change spans,
//! and unified text.

use std::ops::Range;

use similar::{ChangeTag, DiffOp, TextDiff};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    Equal,
    /// Line exists on both sides with changes.
    Changed,
    /// Only in the source (left).
    Removed,
    /// Only in the target (right).
    Added,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Side {
    /// 1-based line number.
    pub line: usize,
    pub text: String,
    /// Byte ranges of `text` that differ from the other side.
    pub changes: Vec<Range<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    pub left: Option<Side>,
    pub right: Option<Side>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LineDiff {
    pub rows: Vec<Row>,
    pub added: usize,
    pub removed: usize,
    pub changed: usize,
}

impl LineDiff {
    pub fn is_equal(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.changed == 0
    }

    /// Indices of rows that start a block of differences (for "next change").
    pub fn hunk_starts(&self) -> Vec<usize> {
        let mut out = Vec::new();
        let mut prev_equal = true;
        for (i, row) in self.rows.iter().enumerate() {
            let equal = row.kind == RowKind::Equal;
            if !equal && prev_equal {
                out.push(i);
            }
            prev_equal = equal;
        }
        out
    }
}

/// Side-by-side diff of `left` (source) and `right` (target).
pub fn side_by_side(left: &str, right: &str) -> LineDiff {
    let diff = TextDiff::from_lines(left, right);
    let old: Vec<&str> = left.lines().collect();
    let new: Vec<&str> = right.lines().collect();
    let side = |lines: &[&str], i: usize| Side {
        line: i + 1,
        text: lines.get(i).copied().unwrap_or("").to_owned(),
        changes: Vec::new(),
    };
    let mut out = LineDiff::default();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                for k in 0..len {
                    out.rows.push(Row {
                        kind: RowKind::Equal,
                        left: Some(side(&old, old_index + k)),
                        right: Some(side(&new, new_index + k)),
                    });
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for k in 0..old_len {
                    out.removed += 1;
                    out.rows.push(Row {
                        kind: RowKind::Removed,
                        left: Some(side(&old, old_index + k)),
                        right: None,
                    });
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for k in 0..new_len {
                    out.added += 1;
                    out.rows.push(Row {
                        kind: RowKind::Added,
                        left: None,
                        right: Some(side(&new, new_index + k)),
                    });
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                // Pair lines up; leftovers become plain removals/additions.
                for k in 0..old_len.max(new_len) {
                    match (k < old_len, k < new_len) {
                        (true, true) => {
                            let mut l = side(&old, old_index + k);
                            let mut r = side(&new, new_index + k);
                            (l.changes, r.changes) = inline_changes(&l.text, &r.text);
                            out.changed += 1;
                            out.rows.push(Row {
                                kind: RowKind::Changed,
                                left: Some(l),
                                right: Some(r),
                            });
                        }
                        (true, false) => {
                            out.removed += 1;
                            out.rows.push(Row {
                                kind: RowKind::Removed,
                                left: Some(side(&old, old_index + k)),
                                right: None,
                            });
                        }
                        (false, true) => {
                            out.added += 1;
                            out.rows.push(Row {
                                kind: RowKind::Added,
                                left: None,
                                right: Some(side(&new, new_index + k)),
                            });
                        }
                        (false, false) => {}
                    }
                }
            }
        }
    }
    out
}

/// Changed byte ranges within a pair of lines, word-granular.
fn inline_changes(left: &str, right: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let diff = TextDiff::configure().diff_words(left, right);
    let (mut l, mut r) = (Vec::new(), Vec::new());
    let (mut lpos, mut rpos) = (0, 0);
    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                lpos += len;
                rpos += len;
            }
            ChangeTag::Delete => {
                push_merged(&mut l, lpos..lpos + len);
                lpos += len;
            }
            ChangeTag::Insert => {
                push_merged(&mut r, rpos..rpos + len);
                rpos += len;
            }
        }
    }
    (l, r)
}

fn push_merged(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    match ranges.last_mut() {
        Some(last) if last.end == range.start => last.end = range.end,
        _ => ranges.push(range),
    }
}

/// Unified diff text with three lines of context.
pub fn unified(left: &str, right: &str, left_name: &str, right_name: &str) -> String {
    TextDiff::from_lines(left, right)
        .unified_diff()
        .context_radius(3)
        .header(left_name, right_name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_counts() {
        let d = side_by_side("a\nb\nc\nd\n", "a\nB x\nc\ne\nf\n");
        let kinds: Vec<RowKind> = d.rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [
                RowKind::Equal,
                RowKind::Changed,
                RowKind::Equal,
                RowKind::Changed,
                RowKind::Added
            ]
        );
        assert_eq!((d.added, d.removed, d.changed), (1, 0, 2));
        assert_eq!(d.hunk_starts(), [1, 3]);
        assert!(!d.is_equal());
        assert!(side_by_side("x\n", "x\n").is_equal());
    }

    #[test]
    fn inline_spans() {
        let d = side_by_side("select a, b from t\n", "select a, c from t\n");
        let row = &d.rows[0];
        let l = row.left.as_ref().unwrap();
        let r = row.right.as_ref().unwrap();
        assert_eq!(&l.text[l.changes[0].clone()], "b");
        assert_eq!(&r.text[r.changes[0].clone()], "c");
    }

    #[test]
    fn unified_text() {
        let u = unified("a\nb\n", "a\nc\n", "source", "target");
        assert!(u.contains("--- source") && u.contains("-b") && u.contains("+c"));
    }
}
