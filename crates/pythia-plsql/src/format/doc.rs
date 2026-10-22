//! A small Wadler/Prettier-style document model and renderer.
//!
//! Layout code builds a [`Doc`] tree; the renderer decides for every
//! [`Doc::Group`] whether it fits on the current line (flat) or must break.
//! On top of the classic algebra there are [`Doc::Rows`] for aligned
//! columns (declarations, parameters, select lists) and a deduplicating
//! [`Doc::Space`].

use std::rc::Rc;

#[derive(Debug, Clone)]
pub(crate) enum Doc {
    Nil,
    Text(Rc<str>),
    /// A single space, dropped at the start of a line and never doubled.
    Space,
    /// Space when flat, newline when broken.
    Line,
    /// Nothing when flat, newline when broken.
    SoftLine,
    /// Always a newline (forces enclosing groups to break).
    HardLine,
    /// A newline plus one empty line.
    Blank,
    /// Increases the indentation of lines inside.
    Nest(usize, Rc<Doc>),
    Group(Rc<Doc>),
    /// (broken, flat), chosen by the enclosing group's mode.
    IfBreak(Rc<Doc>, Rc<Doc>),
    Concat(Rc<Vec<Doc>>),
    /// Lines whose cells are padded into aligned columns.
    Rows(Rc<Vec<Row>>, bool),
}

#[derive(Debug, Clone)]
pub(crate) struct Row {
    /// Rendered before the row (leading comments).
    pub prefix: Doc,
    pub cells: Vec<Doc>,
}

pub(crate) fn text(s: impl Into<Rc<str>>) -> Doc {
    Doc::Text(s.into())
}

pub(crate) fn cat(parts: Vec<Doc>) -> Doc {
    let parts: Vec<Doc> = parts
        .into_iter()
        .filter(|d| !matches!(d, Doc::Nil))
        .collect();
    match parts.len() {
        0 => Doc::Nil,
        1 => parts.into_iter().next().unwrap_or(Doc::Nil),
        _ => Doc::Concat(Rc::new(parts)),
    }
}

pub(crate) fn nest(indent: usize, doc: Doc) -> Doc {
    if matches!(doc, Doc::Nil) {
        Doc::Nil
    } else {
        Doc::Nest(indent, Rc::new(doc))
    }
}

pub(crate) fn group(doc: Doc) -> Doc {
    if matches!(doc, Doc::Nil) {
        Doc::Nil
    } else {
        Doc::Group(Rc::new(doc))
    }
}

pub(crate) fn if_break(broken: Doc, flat: Doc) -> Doc {
    Doc::IfBreak(Rc::new(broken), Rc::new(flat))
}

pub(crate) fn rows(rows: Vec<Row>, align: bool) -> Doc {
    if rows.is_empty() {
        Doc::Nil
    } else {
        Doc::Rows(Rc::new(rows), align)
    }
}

fn width(s: &str) -> usize {
    s.chars().count()
}

/// Width of `doc` laid out flat, or `None` if it can't be flat (hard line
/// breaks, multi-line text) or exceeds `budget`. `content` tells whether the
/// line already has content (a leading space then counts).
pub(crate) fn measure(doc: &Doc, budget: usize, content: bool) -> Option<usize> {
    let mut total = 0usize;
    let mut has_content = content;
    let mut pending = false;
    let mut stack: Vec<&Doc> = vec![doc];
    while let Some(d) = stack.pop() {
        match d {
            Doc::Nil | Doc::SoftLine => {}
            Doc::Text(s) => {
                if s.contains('\n') {
                    return None;
                }
                if s.is_empty() {
                    continue;
                }
                if pending && has_content && !s.starts_with(' ') {
                    total += 1;
                }
                pending = false;
                total += width(s);
                has_content = true;
            }
            Doc::Space | Doc::Line => pending = true,
            Doc::HardLine | Doc::Blank => return None,
            Doc::Nest(_, d) | Doc::Group(d) => stack.push(d),
            Doc::IfBreak(_, flat) => stack.push(flat),
            Doc::Concat(parts) => stack.extend(parts.iter().rev()),
            Doc::Rows(rows, _) => {
                if rows.len() > 1 {
                    return None;
                }
                for row in rows.iter().rev() {
                    for cell in row.cells.iter().rev() {
                        stack.push(cell);
                        stack.push(&Doc::Space);
                    }
                    stack.push(&row.prefix);
                }
            }
        }
        if total > budget {
            return None;
        }
    }
    Some(total)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

struct Renderer {
    out: String,
    width: usize,
    col: usize,
    /// The current line has text on it.
    has_content: bool,
    pending_space: bool,
    /// Line breaks are written lazily, before the next text, so the output
    /// never ends in whitespace and breaks never pile up into empty lines.
    pending_break: Break,
    pending_indent: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Break {
    None,
    Line,
    Blank,
}

impl Renderer {
    fn newline(&mut self, indent: usize) {
        self.pending_space = false;
        self.pending_indent = indent;
        if self.has_content {
            self.pending_break = self.pending_break.max(Break::Line);
            self.has_content = false;
        }
    }

    fn blank(&mut self, indent: usize) {
        self.pending_space = false;
        self.pending_indent = indent;
        if !self.out.is_empty() {
            self.pending_break = Break::Blank;
            self.has_content = false;
        }
    }

    fn text(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if !self.has_content {
            if self.pending_break != Break::None {
                while self.out.ends_with(' ') {
                    self.out.pop();
                }
                self.out.push('\n');
                if self.pending_break == Break::Blank {
                    self.out.push('\n');
                }
                self.pending_break = Break::None;
            }
            for _ in 0..self.pending_indent {
                self.out.push(' ');
            }
            self.col = self.pending_indent;
            self.pending_space = false;
        } else if self.pending_space && !self.out.ends_with(' ') && !s.starts_with(' ') {
            self.out.push(' ');
            self.col += 1;
        }
        self.pending_space = false;
        self.out.push_str(s);
        match s.rfind('\n') {
            Some(i) => self.col = width(&s[i + 1..]),
            None => self.col += width(s),
        }
        self.has_content = true;
    }
}

/// Renders `doc` with a target line width.
pub(crate) fn render(doc: &Doc, width: usize) -> String {
    let mut r = Renderer {
        out: String::new(),
        width,
        col: 0,
        has_content: false,
        pending_space: false,
        pending_break: Break::None,
        pending_indent: 0,
    };
    let mut stack: Vec<(usize, Mode, Doc)> = vec![(0, Mode::Break, doc.clone())];
    while let Some((indent, mode, d)) = stack.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => r.text(&s),
            Doc::Space => {
                if r.has_content {
                    r.pending_space = true;
                }
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    if r.has_content {
                        r.pending_space = true;
                    }
                }
                Mode::Break => r.newline(indent),
            },
            Doc::SoftLine => {
                if mode == Mode::Break {
                    r.newline(indent);
                }
            }
            Doc::HardLine => r.newline(indent),
            Doc::Blank => r.blank(indent),
            Doc::Nest(n, d) => stack.push((indent + n, mode, (*d).clone())),
            Doc::Group(d) => {
                let m = if mode == Mode::Flat {
                    Mode::Flat
                } else {
                    let start = if r.has_content {
                        r.col
                    } else {
                        r.pending_indent
                    };
                    let budget = r.width.saturating_sub(start);
                    if measure(&d, budget, r.has_content).is_some() {
                        Mode::Flat
                    } else {
                        Mode::Break
                    }
                };
                stack.push((indent, m, (*d).clone()));
            }
            Doc::IfBreak(broken, flat) => {
                let d = if mode == Mode::Break { broken } else { flat };
                stack.push((indent, mode, (*d).clone()));
            }
            Doc::Concat(parts) => {
                for p in parts.iter().rev() {
                    stack.push((indent, mode, p.clone()));
                }
            }
            Doc::Rows(rs, align) => {
                let d = layout_rows(&rs, align);
                stack.push((indent, Mode::Break, d));
            }
        }
    }
    r.out
}

/// Turns rows into lines with padded cells.
fn layout_rows(rs: &[Row], align: bool) -> Doc {
    const LIMIT: usize = 60;
    const OUTLIER: usize = 16;
    let columns = rs.iter().map(|r| r.cells.len()).max().unwrap_or(0);
    // A column is padded to the widest cell that isn't an outlier: one very
    // long name shouldn't push every other row's type far to the right.
    let measured: Vec<Vec<Option<usize>>> = rs
        .iter()
        .map(|row| row.cells.iter().map(|c| measure(c, 200, false)).collect())
        .collect();
    let mut widths = vec![0usize; columns];
    for (c, width) in widths.iter_mut().enumerate() {
        let ws: Vec<usize> = rs
            .iter()
            .zip(&measured)
            .filter_map(|(row, m)| {
                (Some(c) != last_non_empty(&row.cells) && c + 1 < columns)
                    .then(|| m.get(c).copied().flatten())
                    .flatten()
            })
            .filter(|w| *w <= LIMIT)
            .collect();
        if let Some(min) = ws.iter().copied().min() {
            *width = ws
                .iter()
                .copied()
                .filter(|w| *w <= min + OUTLIER)
                .max()
                .unwrap_or(min);
        }
    }
    let mut out = Vec::with_capacity(rs.len() * 3);
    for (i, (row, ws)) in rs.iter().zip(&measured).enumerate() {
        if i > 0 {
            out.push(Doc::HardLine);
        }
        out.push(row.prefix.clone());
        let last = last_non_empty(&row.cells);
        for (c, cell) in row.cells.iter().enumerate() {
            if last.is_some_and(|l| c > l) {
                break;
            }
            if c > 0 {
                out.push(Doc::Space);
            }
            out.push(cell.clone());
            let pad = match ws.get(c).copied().flatten() {
                Some(w) if align && Some(c) != last && w <= widths[c] => widths[c] - w,
                _ => 0,
            };
            if pad > 0 {
                // Padding includes the separator; the following Space is
                // then deduplicated by the renderer.
                out.push(Doc::Text(" ".repeat(pad + 1).into()));
            }
        }
    }
    cat(out)
}

fn last_non_empty(cells: &[Doc]) -> Option<usize> {
    cells.iter().rposition(|c| !matches!(c, Doc::Nil))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_break_when_too_long() {
        let items = || cat(vec![text("aaaa"), text(","), Doc::Line, text("bbbb")]);
        let doc = group(cat(vec![
            text("f("),
            nest(2, cat(vec![Doc::SoftLine, items()])),
            Doc::SoftLine,
            text(")"),
        ]));
        assert_eq!(render(&doc, 80), "f(aaaa, bbbb)");
        assert_eq!(render(&doc, 8), "f(\n  aaaa,\n  bbbb\n)");
    }

    #[test]
    fn spaces_dedupe_and_vanish_at_line_start() {
        let doc = cat(vec![
            text("a"),
            Doc::Space,
            Doc::Space,
            text("b"),
            Doc::HardLine,
            Doc::Space,
            text("c"),
        ]);
        assert_eq!(render(&doc, 80), "a b\nc");
    }

    #[test]
    fn rows_align() {
        let row = |a: &str, b: &str| Row {
            prefix: Doc::Nil,
            cells: vec![text(a), text(b)],
        };
        let doc = rows(vec![row("x", "number;"), row("long_name", "date;")], true);
        assert_eq!(render(&doc, 80), "x         number;\nlong_name date;");
    }
}
