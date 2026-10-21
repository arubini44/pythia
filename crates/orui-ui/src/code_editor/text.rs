//! Rope helpers: lines, columns, character classes.
//!
//! All positions inside the editor are char indices into the rope. Columns
//! are char offsets within a line; "visual" columns expand tabs.

use ropey::Rope;

/// Chars that make up an identifier (`$` and `#` are valid in Oracle names).
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharClass {
    Word,
    Space,
    Newline,
    Punct,
}

pub fn class(c: char) -> CharClass {
    if c == '\n' || c == '\r' {
        CharClass::Newline
    } else if c.is_whitespace() {
        CharClass::Space
    } else if is_word_char(c) {
        CharClass::Word
    } else {
        CharClass::Punct
    }
}

/// Number of chars of `line` without its line break.
pub fn line_len(rope: &Rope, line: usize) -> usize {
    let slice = rope.line(line);
    let mut len = slice.len_chars();
    if len > 0 && slice.char(len - 1) == '\n' {
        len -= 1;
        if len > 0 && slice.char(len - 1) == '\r' {
            len -= 1;
        }
    } else if len > 0 && slice.char(len - 1) == '\r' {
        len -= 1;
    }
    len
}

/// Text of `line` without its line break.
pub fn line_text(rope: &Rope, line: usize) -> String {
    let len = line_len(rope, line);
    rope.line(line).slice(..len).to_string()
}

pub fn line_start(rope: &Rope, line: usize) -> usize {
    rope.line_to_char(line)
}

pub fn line_end(rope: &Rope, line: usize) -> usize {
    rope.line_to_char(line) + line_len(rope, line)
}

/// (line, column) of a char index; both 0-based.
pub fn line_col(rope: &Rope, pos: usize) -> (usize, usize) {
    let line = rope.char_to_line(pos);
    (line, pos - rope.line_to_char(line))
}

/// Char index of (line, col), clamping both.
pub fn pos_of(rope: &Rope, line: usize, col: usize) -> usize {
    let line = line.min(rope.len_lines().saturating_sub(1));
    line_start(rope, line) + col.min(line_len(rope, line))
}

/// Visual column of char column `col` with tabs expanded.
pub fn visual_col(line: &str, col: usize, tab_width: usize) -> usize {
    let mut v = 0;
    for c in line.chars().take(col) {
        v = advance(v, c, tab_width);
    }
    v
}

/// Char column closest to visual column `vcol`.
pub fn col_from_visual(line: &str, vcol: usize, tab_width: usize) -> usize {
    let mut v = 0;
    for (i, c) in line.chars().enumerate() {
        let next = advance(v, c, tab_width);
        if vcol < next {
            // Round to the nearer edge of the char.
            return if vcol - v <= (next - v) / 2 { i } else { i + 1 };
        }
        v = next;
    }
    line.chars().count()
}

fn advance(v: usize, c: char, tab_width: usize) -> usize {
    if c == '\t' {
        (v / tab_width.max(1) + 1) * tab_width.max(1)
    } else {
        v + 1
    }
}

/// Leading whitespace of a line.
pub fn indent_of(line: &str) -> &str {
    let trimmed = line.trim_start_matches([' ', '\t']);
    &line[..line.len() - trimmed.len()]
}

/// Start of the next word to the right (Ctrl+Right).
pub fn next_word(rope: &Rope, pos: usize) -> usize {
    let len = rope.len_chars();
    if pos >= len {
        return len;
    }
    let mut i = pos;
    let first = class(rope.char(i));
    if first == CharClass::Newline {
        // Step over the line break as one unit.
        while i < len && class(rope.char(i)) == CharClass::Newline {
            i += 1;
            if i > pos && rope.char(i - 1) == '\n' {
                break;
            }
        }
        return i;
    }
    while i < len && class(rope.char(i)) == first {
        i += 1;
    }
    while i < len && class(rope.char(i)) == CharClass::Space {
        i += 1;
    }
    i
}

/// Start of the previous word to the left (Ctrl+Left).
pub fn prev_word(rope: &Rope, pos: usize) -> usize {
    if pos == 0 {
        return 0;
    }
    let mut i = pos;
    while i > 0 && class(rope.char(i - 1)) == CharClass::Space {
        i -= 1;
    }
    if i == 0 {
        return 0;
    }
    let c = class(rope.char(i - 1));
    if c == CharClass::Newline {
        i -= 1;
        if i > 0 && rope.char(i) == '\n' && rope.char(i - 1) == '\r' {
            i -= 1;
        }
        return i;
    }
    while i > 0 && class(rope.char(i - 1)) == c {
        i -= 1;
    }
    i
}

/// Range of the word (or run of same-class chars) around `pos`, for double-click.
pub fn word_range(rope: &Rope, pos: usize) -> (usize, usize) {
    let (line, _) = line_col(rope, pos);
    let (ls, le) = (line_start(rope, line), line_end(rope, line));
    if ls == le {
        return (pos, pos);
    }
    // Prefer the char under the cursor, else the one before it.
    let probe = if pos < le { pos } else { pos - 1 };
    let c = class(rope.char(probe));
    let mut start = probe;
    while start > ls && class(rope.char(start - 1)) == c {
        start -= 1;
    }
    let mut end = probe + 1;
    while end < le && class(rope.char(end)) == c {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines() {
        let rope = Rope::from_str("ab\r\ncd\n\nlast");
        assert_eq!(rope.len_lines(), 4);
        assert_eq!(line_len(&rope, 0), 2);
        assert_eq!(line_text(&rope, 1), "cd");
        assert_eq!(line_len(&rope, 2), 0);
        assert_eq!(line_end(&rope, 3), rope.len_chars());
        assert_eq!(line_col(&rope, 5), (1, 1));
        assert_eq!(pos_of(&rope, 0, 99), 2);
        assert_eq!(pos_of(&rope, 99, 0), line_start(&rope, 3));
    }

    #[test]
    fn tabs() {
        assert_eq!(visual_col("\tab", 1, 4), 4);
        assert_eq!(visual_col("a\tb", 2, 4), 4);
        assert_eq!(col_from_visual("a\tb", 4, 4), 2);
        assert_eq!(col_from_visual("a\tb", 1, 4), 1);
        assert_eq!(col_from_visual("a\tb", 3, 4), 2);
        assert_eq!(col_from_visual("ab", 10, 4), 2);
    }

    #[test]
    fn words() {
        let rope = Rope::from_str("select emp.name$1, x  from\ndual");
        assert_eq!(next_word(&rope, 0), 7);
        assert_eq!(next_word(&rope, 7), 10); // "emp" -> "."
        assert_eq!(next_word(&rope, 10), 11);
        assert_eq!(prev_word(&rope, 17), 11);
        assert_eq!(prev_word(&rope, 7), 0);
        let nl = rope.line_to_char(1) - 1;
        assert_eq!(next_word(&rope, nl), nl + 1);
        assert_eq!(prev_word(&rope, nl + 1), nl);
        assert_eq!(word_range(&rope, 12), (11, 17));
        assert_eq!(word_range(&rope, 6), (6, 7));
    }
}
