//! Incremental, line-based syntax highlighting.
//!
//! Each line is lexed on its own, starting from the lexer state the previous
//! line ended in (inside a block comment, a string, ...). Only the start
//! states are stored; tokens of visible lines are recomputed when painted,
//! which is cheap for a screenful of lines. After an edit, states are
//! recomputed from the edited line until they match the old ones again.

use std::ops::Range;

use pythia_plsql::{TokenKind, tokenize};
use ropey::Rope;

use super::text::line_text;

/// Lexer state at the start of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineState {
    #[default]
    Normal,
    /// Inside `/* ... */`.
    Comment,
    /// Inside `/*+ ... */`.
    Hint,
    /// Inside `'...'`.
    Str,
    /// Inside `q'<open>...<close>'`.
    QStr { open: char, close: char },
}

/// A token of one line; `range` is a byte range into the line text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineToken {
    pub range: Range<usize>,
    pub kind: TokenKind,
}

/// Lexes one line (without line break) that starts in `state`.
pub fn lex_line(line: &str, state: LineState) -> (Vec<LineToken>, LineState) {
    let prefix: String = match state {
        LineState::Normal => String::new(),
        LineState::Comment => "/*".into(),
        LineState::Hint => "/*+".into(),
        LineState::Str => "'".into(),
        LineState::QStr { open, .. } => format!("q'{open}"),
    };
    let src = format!("{prefix}{line}");
    let p = prefix.len();
    let raw = tokenize(&src);
    let end_state = raw.last().map_or(LineState::Normal, |t| {
        end_state(&src[t.span.clone()], t.kind)
    });
    let mut tokens: Vec<LineToken> = raw
        .into_iter()
        .filter(|t| t.span.end > p)
        .map(|t| LineToken {
            range: t.span.start.saturating_sub(p)..t.span.end - p,
            kind: t.kind,
        })
        .collect();
    if state == LineState::Normal {
        mark_sqlplus(line, &mut tokens);
    }
    (tokens, end_state)
}

/// SQL*Plus commands that start a line (in column 0).
const SQLPLUS: &[&str] = &[
    "ACCEPT",
    "APPEND",
    "ARCHIVE",
    "ATTRIBUTE",
    "BREAK",
    "BTITLE",
    "CLEAR",
    "COL",
    "COLUMN",
    "COMPUTE",
    "CONN",
    "CONNECT",
    "COPY",
    "DEF",
    "DEFINE",
    "DESC",
    "DESCRIBE",
    "DISC",
    "DISCONNECT",
    "EXEC",
    "EXECUTE",
    "EXIT",
    "HOST",
    "PAUSE",
    "PRINT",
    "PRO",
    "PROMPT",
    "QUIT",
    "REM",
    "REMARK",
    "SHO",
    "SHOW",
    "SPO",
    "SPOOL",
    "STA",
    "START",
    "TIMING",
    "TTITLE",
    "UNDEF",
    "UNDEFINE",
    "VAR",
    "VARIABLE",
    "WHENEVER",
];

/// `SET` options of SQL*Plus (`SET x = 1` in an UPDATE is SQL).
const SQLPLUS_SET: &[&str] = &[
    "APPI",
    "APPINFO",
    "ARRAY",
    "ARRAYSIZE",
    "AUTO",
    "AUTOCOMMIT",
    "AUTOP",
    "AUTOPRINT",
    "AUTOT",
    "AUTOTRACE",
    "CMDS",
    "COLSEP",
    "CON",
    "CONCAT",
    "DEF",
    "DEFINE",
    "ECHO",
    "ESC",
    "ESCAPE",
    "FEED",
    "FEEDBACK",
    "HEA",
    "HEADING",
    "HEADSEP",
    "LIN",
    "LINESIZE",
    "LONG",
    "LONGCHUNKSIZE",
    "MARK",
    "MARKUP",
    "NEWP",
    "NEWPAGE",
    "NULL",
    "NUMF",
    "NUMFORMAT",
    "NUM",
    "NUMWIDTH",
    "PAGES",
    "PAGESIZE",
    "PAU",
    "PAUSE",
    "SERVEROUT",
    "SERVEROUTPUT",
    "SQLBL",
    "SQLBLANKLINES",
    "SQLP",
    "SQLPROMPT",
    "SQLT",
    "SQLTERMINATOR",
    "TAB",
    "TERM",
    "TERMOUT",
    "TI",
    "TIME",
    "TIMI",
    "TIMING",
    "TRIM",
    "TRIMOUT",
    "TRIMS",
    "TRIMSPOOL",
    "VER",
    "VERIFY",
    "WRA",
    "WRAP",
];

/// Recolors a line holding a SQL*Plus command: the command word (or `@`,
/// `/`) and, for `@file`, the path. Commands must start in column 0, which
/// keeps indented SQL (`  desc`, `  start with`) out.
fn mark_sqlplus(line: &str, tokens: &mut [LineToken]) {
    let significant: Vec<usize> = (0..tokens.len())
        .filter(|&i| !tokens[i].kind.is_trivia())
        .collect();
    let Some(&first) = significant.first() else {
        return;
    };
    let text = |i: usize| &line[tokens[i].range.clone()];
    let next = significant.get(1).map(|&i| text(i).to_ascii_uppercase());
    // A lone `/` runs the buffer; leading whitespace is fine for it.
    if significant.len() == 1 && text(first) == "/" {
        tokens[first].kind = TokenKind::SqlPlus;
        return;
    }
    if tokens[first].range.start != 0 {
        return;
    }
    let word = text(first).to_ascii_uppercase();
    if word == "@" || word == "@@" {
        for t in &mut tokens[first..] {
            if !matches!(t.kind, TokenKind::LineComment | TokenKind::SubstVar) {
                t.kind = TokenKind::SqlPlus;
            }
        }
        return;
    }
    if !tokens[first].kind.is_word() {
        return;
    }
    let is_command = match word.as_str() {
        "SET" => next.as_deref().is_some_and(|n| SQLPLUS_SET.contains(&n)),
        // `START WITH` and `CONNECT BY` belong to hierarchical queries.
        "START" | "STA" => next.as_deref() != Some("WITH"),
        "CONNECT" | "CONN" => next.as_deref() != Some("BY"),
        // `DESC` alone ends an ORDER BY; `desc t` describes.
        "DESC" | "DESCRIBE" => {
            significant.len() >= 2
                && tokens[significant[1]].kind.is_word()
                && next.as_deref() != Some("NULLS")
        }
        // PL/SQL's `exit when` is indented anyway; plain `exit` ends the script.
        "EXIT" | "QUIT" => next.as_deref() != Some("WHEN"),
        w => SQLPLUS.contains(&w),
    };
    if !is_command {
        return;
    }
    tokens[first].kind = TokenKind::SqlPlus;
    if word == "SET"
        && let Some(&option) = significant.get(1)
    {
        tokens[option].kind = TokenKind::SqlPlus;
    }
}

/// State after the last token of a line: unterminated constructs continue.
fn end_state(text: &str, kind: TokenKind) -> LineState {
    match kind {
        TokenKind::BlockComment if !(text.len() >= 4 && text.ends_with("*/")) => LineState::Comment,
        TokenKind::Hint if !(text.len() >= 5 && text.ends_with("*/")) => LineState::Hint,
        TokenKind::String if !string_terminated(text) => LineState::Str,
        TokenKind::QString => {
            let body = text.trim_start_matches(['n', 'N']);
            let mut chars = body.chars().skip(2); // q'
            let Some(open) = chars.next() else {
                return LineState::Normal;
            };
            let close = match open {
                '[' => ']',
                '{' => '}',
                '(' => ')',
                '<' => '>',
                c => c,
            };
            let head = body.find(open).map_or(body.len(), |i| i + open.len_utf8());
            let rest = &body[head..];
            if rest.ends_with('\'') && rest[..rest.len() - 1].ends_with(close) {
                LineState::Normal
            } else {
                LineState::QStr { open, close }
            }
        }
        _ => LineState::Normal,
    }
}

fn string_terminated(text: &str) -> bool {
    let body = text.trim_start_matches(['n', 'N']);
    let bytes = body.as_bytes();
    // bytes[0] is the opening quote.
    let mut i = 1;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            if bytes.get(i + 1) == Some(&b'\'') {
                i += 2;
                continue;
            }
            return i == bytes.len() - 1;
        }
        i += 1;
    }
    false
}

/// Start states of all lines.
#[derive(Debug, Default)]
pub struct Highlighter {
    states: Vec<LineState>,
}

impl Highlighter {
    pub fn new(rope: &Rope) -> Self {
        let mut h = Self {
            states: vec![LineState::Normal; rope.len_lines()],
        };
        h.relex(rope, 0, rope.len_lines());
        h
    }

    pub fn state(&self, line: usize) -> LineState {
        self.states.get(line).copied().unwrap_or_default()
    }

    /// Updates states after lines `line..line + removed + 1` were replaced
    /// by `inserted + 1` lines (`removed`/`inserted` count line breaks).
    pub fn edited(&mut self, rope: &Rope, line: usize, removed: usize, inserted: usize) {
        let line = line.min(self.states.len().saturating_sub(1));
        let from = (line + 1).min(self.states.len());
        let to = (line + 1 + removed).min(self.states.len());
        self.states
            .splice(from..to, std::iter::repeat_n(LineState::Normal, inserted));
        // Guard against drift (e.g. a lone `\r` joined with `\n`).
        self.states.resize(rope.len_lines(), LineState::Normal);
        self.relex(rope, line, line + inserted + 1);
    }

    /// Recomputes states from `from`; always up to `force_until`, then until
    /// the computed state equals the stored one.
    fn relex(&mut self, rope: &Rope, from: usize, force_until: usize) {
        let n = self.states.len();
        let mut i = from;
        while i + 1 < n {
            let (_, end) = lex_line(&line_text(rope, i), self.states[i]);
            if i + 1 >= force_until && self.states[i + 1] == end {
                break;
            }
            self.states[i + 1] = end;
            i += 1;
        }
    }

    /// Number of line states (equals the rope's line count).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.states.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, state: LineState) -> (Vec<(TokenKind, &str)>, LineState) {
        let (tokens, end) = lex_line(line, state);
        (
            tokens
                .into_iter()
                .filter(|t| !matches!(t.kind, TokenKind::Whitespace))
                .map(|t| (t.kind, &line[t.range]))
                .collect(),
            end,
        )
    }

    #[test]
    fn states_across_lines() {
        assert_eq!(
            kinds("select /* start", LineState::Normal).1,
            LineState::Comment
        );
        assert_eq!(
            kinds("still */ x", LineState::Comment),
            (
                vec![
                    (TokenKind::BlockComment, "still */"),
                    (TokenKind::Ident, "x")
                ],
                LineState::Normal
            )
        );
        assert_eq!(kinds("x := 'it''s", LineState::Normal).1, LineState::Str);
        assert_eq!(
            kinds("end' || y", LineState::Str).0[0],
            (TokenKind::String, "end'")
        );
        assert_eq!(
            kinds("q'[abc", LineState::Normal).1,
            LineState::QStr {
                open: '[',
                close: ']'
            }
        );
        assert_eq!(
            kinds(
                "x]' y",
                LineState::QStr {
                    open: '[',
                    close: ']'
                }
            )
            .1,
            LineState::Normal
        );
        assert_eq!(kinds("'''", LineState::Normal).1, LineState::Str);
        assert_eq!(kinds("''''", LineState::Normal).1, LineState::Normal);
        assert_eq!(kinds("/*+ hint", LineState::Normal).1, LineState::Hint);
        assert_eq!(kinds("", LineState::Comment).1, LineState::Comment);
        assert_eq!(kinds("-- '/*", LineState::Normal).1, LineState::Normal);
    }

    #[test]
    fn incremental_matches_full_relex() {
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!("select {i} from dual; -- line\n"));
        }
        let mut rope = Rope::from_str(&text);
        let mut h = Highlighter::new(&rope);

        // Open a comment on line 10: everything after becomes comment.
        let at = rope.line_to_char(10);
        rope.insert(at, "/* ");
        h.edited(&rope, 10, 0, 0);
        assert_eq!(h.state(11), LineState::Comment);
        assert_eq!(h.state(199), LineState::Comment);

        // Close it on line 20 with an inserted line.
        let at = rope.line_to_char(20);
        rope.insert(at, "*/\n");
        h.edited(&rope, 20, 0, 1);
        assert_eq!(h.state(20), LineState::Comment);
        assert_eq!(h.state(21), LineState::Normal);

        // Delete two whole lines.
        let (a, b) = (rope.line_to_char(30), rope.line_to_char(32));
        rope.remove(a..b);
        h.edited(&rope, 30, 2, 0);

        let full = Highlighter::new(&rope);
        assert_eq!(h.len(), rope.len_lines());
        assert_eq!(h.states, full.states);
    }

    fn sqlplus_words(line: &str) -> Vec<String> {
        let (tokens, _) = lex_line(line, LineState::Normal);
        tokens
            .iter()
            .filter(|t| t.kind == TokenKind::SqlPlus)
            .map(|t| line[t.range.clone()].to_owned())
            .collect()
    }

    #[test]
    fn marks_sqlplus_commands() {
        assert_eq!(sqlplus_words("prompt Installing"), ["prompt"]);
        assert_eq!(
            sqlplus_words("set serveroutput on"),
            ["set", "serveroutput"]
        );
        assert_eq!(
            sqlplus_words("@@sub/tables.sql").concat(),
            "@@sub/tables.sql"
        );
        assert_eq!(sqlplus_words("   /"), ["/"]);
        assert_eq!(sqlplus_words("desc employees"), ["desc"]);
        assert_eq!(
            sqlplus_words("whenever sqlerror exit failure"),
            ["whenever"]
        );
        assert_eq!(sqlplus_words("exit"), ["exit"]);
        // SQL that looks similar stays SQL.
        assert!(sqlplus_words("set x = 1").is_empty());
        assert!(sqlplus_words("   set salary = 1").is_empty());
        assert!(sqlplus_words("start with manager_id is null").is_empty());
        assert!(sqlplus_words("connect by prior id = parent_id").is_empty());
        assert!(sqlplus_words("desc").is_empty());
        assert!(sqlplus_words("desc nulls last").is_empty());
        assert!(sqlplus_words("  prompt indented").is_empty());
        assert!(sqlplus_words("select 1 / 2 from dual").is_empty());
    }
}
