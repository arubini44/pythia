//! SQL and PL/SQL code formatter.
//!
//! Opinionated defaults (lowercase keywords, two-space indentation, clauses
//! on their own lines once a statement gets longer, aligned declarations and
//! parameters) with options for the common style choices.
//!
//! Guarantees:
//! - **Lossless**: only whitespace and the case of unquoted keywords and
//!   identifiers change. Every formatted statement is re-lexed and compared
//!   with the original; on any difference the statement is kept as it was.
//! - **Comments survive** at the same place relative to the code.
//! - **SQL*Plus commands and `/` terminators** are kept as written.
//! - **Idempotent**: formatting formatted code changes nothing.
//! - **Never panics**, whatever the input.

mod doc;
mod infer;
mod layout;
#[cfg(test)]
mod tests;

use std::ops::Range;

use serde::{Deserialize, Serialize};

use crate::lexer::{TokenKind, tokenize};
use crate::splitter::{StatementKind, split_statements};
pub use infer::infer;
use layout::{Layout, Tok};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Case {
    Upper,
    Lower,
    Preserve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommaStyle {
    /// `a,` at the end of the line.
    Trailing,
    /// `, b` at the start of the next line.
    Leading,
}

/// When a construct is laid out on several lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wrap {
    /// On one line if it fits, else one item per line.
    #[default]
    Fit,
    /// Always one item per line.
    Always,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatOptions {
    /// Spaces per indentation level.
    pub indent: usize,
    pub keyword_case: Case,
    /// Case of unquoted identifiers (quoted ones are never changed).
    pub identifier_case: Case,
    /// Target line width; lists and conditions wrap beyond it.
    pub max_width: usize,
    pub comma: CommaStyle,
    /// Align `AS alias` columns of a wrapped select list.
    pub align_aliases: bool,
    /// Align consecutive `x := value;` statements.
    pub align_assignments: bool,
    /// Align names/types of declarations, parameters and table columns.
    pub align_declarations: bool,
    /// Put the `ON` of a join on its own line even if it would fit.
    pub on_new_line: bool,
    /// Blank lines kept where the source had them (0 or 1).
    pub blank_lines: usize,
    /// `WHEN` / `ELSE` arms of `CASE` expressions.
    pub case_arms: Wrap,
    /// Subqueries, inline views and `WITH` queries in parentheses.
    pub subqueries: Wrap,
    /// Parameters of procedures, functions and cursors.
    pub parameters: Wrap,
    /// The column and `VALUES` lists of `INSERT`.
    pub insert_lists: Wrap,
    /// `THEN` and `LOOP` on their own line under `IF` / `ELSIF`, `WHILE` and
    /// `FOR`.
    pub then_on_new_line: bool,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent: 2,
            keyword_case: Case::Lower,
            identifier_case: Case::Preserve,
            max_width: 100,
            comma: CommaStyle::Trailing,
            align_aliases: true,
            align_assignments: true,
            align_declarations: true,
            on_new_line: false,
            blank_lines: 1,
            case_arms: Wrap::Fit,
            subqueries: Wrap::Fit,
            parameters: Wrap::Fit,
            insert_lists: Wrap::Fit,
            then_on_new_line: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Formatted {
    pub text: String,
    /// The text differs from the input.
    pub changed: bool,
}

/// Formats a whole script.
pub fn format(src: &str, options: &FormatOptions) -> Formatted {
    let text = format_script(src, options);
    Formatted {
        changed: text != src,
        text,
    }
}

/// Formats only the statements overlapping `range` (byte offsets); the rest
/// of `src` is left untouched.
pub fn format_range(src: &str, range: Range<usize>, options: &FormatOptions) -> Formatted {
    let mut text = String::with_capacity(src.len());
    let mut pos = 0;
    for stmt in split_statements(src) {
        let r = stmt.range.clone();
        let (a, b) = (range.start, range.end.max(range.start));
        let overlaps = if a == b {
            r.start <= a && a <= r.end
        } else {
            r.start < b && a < r.end
        };
        if !overlaps {
            continue;
        }
        text.push_str(&src[pos..r.start]);
        // Statements inside a line keep their column: indent continuation
        // lines by the statement's starting column.
        let line_start = src[..r.start].rfind('\n').map_or(0, |i| i + 1);
        let prefix = &src[line_start..r.start];
        let formatted = format_statement(&src[r.clone()], stmt.kind, options);
        if prefix.chars().all(|c| c == ' ' || c == '\t') && !prefix.is_empty() {
            let pad = prefix.to_owned();
            let mut first = true;
            for line in formatted.split('\n') {
                if !first {
                    text.push('\n');
                    if !line.is_empty() {
                        text.push_str(&pad);
                    }
                }
                text.push_str(line);
                first = false;
            }
        } else {
            text.push_str(&formatted);
        }
        pos = r.end;
    }
    text.push_str(&src[pos..]);
    Formatted {
        changed: text != src,
        text,
    }
}

fn tokens(src: &str) -> Vec<(Tok<'_>, Range<usize>)> {
    let mut out = Vec::new();
    let mut nl = 0u32;
    for t in tokenize(src) {
        match t.kind {
            TokenKind::Whitespace => {}
            TokenKind::Newline => nl += 1,
            kind => {
                out.push((
                    Tok {
                        kind,
                        text: &src[t.span.clone()],
                        nl_before: nl,
                    },
                    t.span,
                ));
                nl = 0;
            }
        }
    }
    out
}

/// Output assembly between statements: terminators, comments, blank lines.
struct Script {
    out: String,
    /// The current line ends in a `--` comment: nothing may follow on it.
    line_comment: bool,
}

impl Script {
    fn line(&mut self, text: &str, blank: bool) {
        if !self.out.is_empty() {
            self.out.push('\n');
            if blank {
                self.out.push('\n');
            }
        }
        self.out.push_str(text);
        self.line_comment = false;
    }

    fn inline(&mut self, text: &str, space: bool) {
        if self.line_comment || self.out.is_empty() {
            self.line(text, false);
            return;
        }
        if space {
            self.out.push(' ');
        }
        self.out.push_str(text);
    }
}

fn format_script(src: &str, o: &FormatOptions) -> String {
    let toks = tokens(src);
    let statements = split_statements(src);
    let mut script = Script {
        out: String::new(),
        line_comment: false,
    };
    let mut i = 0;
    let mut next_stmt = 0;
    let blank = |t: &Tok| o.blank_lines > 0 && t.nl_before >= 2;
    while i < toks.len() {
        let (tok, span) = (toks[i].0, toks[i].1.clone());
        if let Some(stmt) = statements
            .get(next_stmt)
            .filter(|s| span.start >= s.range.start)
        {
            let end = toks[i..]
                .iter()
                .position(|(_, s)| s.start >= stmt.range.end)
                .map_or(toks.len(), |n| i + n);
            let text = format_statement(&src[stmt.range.clone()], stmt.kind, o);
            script.line(&text, blank(&tok));
            script.line_comment = text.lines().last().is_some_and(ends_in_line_comment);
            i = end.max(i + 1);
            next_stmt += 1;
            continue;
        }
        match tok.kind {
            TokenKind::Semicolon => script.inline(";", false),
            TokenKind::LineComment | TokenKind::BlockComment => {
                let text = if tok.kind == TokenKind::LineComment {
                    tok.text.trim_end()
                } else {
                    tok.text
                };
                if tok.nl_before == 0 && !script.out.is_empty() {
                    script.inline(text, true);
                } else {
                    script.line(text, blank(&tok));
                }
                if tok.kind == TokenKind::LineComment {
                    script.line_comment = true;
                }
            }
            _ if tok.text == "/" => script.line("/", blank(&tok)),
            _ => script.inline(tok.text, true),
        }
        i += 1;
    }
    // An unterminated comment or string at the end already ends in the
    // newline (it is part of the token); don't add another.
    if src.ends_with('\n') && !script.out.is_empty() && !script.out.ends_with('\n') {
        script.out.push('\n');
    }
    script.out
}

fn ends_in_line_comment(line: &str) -> bool {
    tokenize(line)
        .last()
        .is_some_and(|t| t.kind == TokenKind::LineComment)
}

/// Formats one statement (no terminator), falling back to the original text
/// if formatting would change anything but whitespace and case.
/// The statements [`format`] keeps as written although they are SQL or
/// PL/SQL: constructs it doesn't support and statements that failed the
/// safety check, as byte ranges of `src`. SQL*Plus commands, wrapped code
/// and Java sources aren't listed.
pub fn kept_as_written(src: &str, options: &FormatOptions) -> Vec<Range<usize>> {
    split_statements(src)
        .into_iter()
        .filter(|s| {
            try_format_statement(&src[s.range.clone()], s.kind, options) == Err(Kept::Unsupported)
        })
        .map(|s| s.range)
        .collect()
}

fn format_statement(text: &str, kind: StatementKind, o: &FormatOptions) -> String {
    try_format_statement(text, kind, o).unwrap_or_else(|_| text.to_owned())
}

/// Why a statement is kept as written.
#[derive(Debug, PartialEq, Eq)]
enum Kept {
    /// SQL*Plus commands, wrapped code, Java sources.
    Opaque,
    /// A construct the formatter doesn't support, or a failed safety check.
    Unsupported,
}

fn try_format_statement(
    text: &str,
    kind: StatementKind,
    o: &FormatOptions,
) -> Result<String, Kept> {
    if kind == StatementKind::SqlPlus {
        return Err(Kept::Opaque);
    }
    let toks: Vec<Tok> = tokens(text).into_iter().map(|(t, _)| t).collect();
    let mut layout = Layout::new(&toks, o);
    let doc = layout.statement();
    if layout.opaque {
        return Err(Kept::Opaque);
    }
    if layout.bail {
        return Err(Kept::Unsupported);
    }
    let formatted = doc::render(&doc, o.max_width.max(20));
    if equivalent(text, &formatted) {
        Ok(formatted)
    } else {
        Err(Kept::Unsupported)
    }
}

/// Same tokens in the same order, ignoring whitespace, the case of words and
/// trailing blanks of line comments: `b` is `a` laid out differently. The
/// formatter's safety check, also for formatting done elsewhere.
pub fn equivalent(a: &str, b: &str) -> bool {
    let sig = |s: &str| -> Vec<(TokenKind, String)> {
        tokenize(s)
            .into_iter()
            .filter(|t| !matches!(t.kind, TokenKind::Whitespace | TokenKind::Newline))
            .map(|t| {
                let text = &s[t.span];
                let text = match t.kind {
                    TokenKind::Keyword | TokenKind::Ident => text.to_uppercase(),
                    TokenKind::LineComment => text.trim_end().to_owned(),
                    _ => text.to_owned(),
                };
                (t.kind, text)
            })
            .collect()
    };
    sig(a) == sig(b)
}
