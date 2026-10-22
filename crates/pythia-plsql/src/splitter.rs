//! Splits a script into executable statements, following SQL*Plus rules.
//!
//! - SQL statements end at `;` or at a line containing only `/`.
//! - PL/SQL blocks (`DECLARE`, `BEGIN`, `CREATE [OR REPLACE] PACKAGE ...`, ...)
//!   end only at a line containing only `/` (or end of input), since they
//!   contain `;` themselves.
//! - SQL*Plus commands (`SET`, `PROMPT`, `@file`, ...) end at the end of the line.

use std::ops::Range;

use crate::lexer::{Token, TokenKind, tokenize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    Sql,
    PlSql,
    SqlPlus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub kind: StatementKind,
    /// Byte range of the statement text in the script, without the terminator
    /// (`;` for SQL, `/` for PL/SQL) and without surrounding trivia.
    pub range: Range<usize>,
}

impl Statement {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.range.clone()]
    }
}

const SQLPLUS_COMMANDS: &[&str] = &[
    "ACCEPT",
    "CLEAR",
    "COL",
    "COLUMN",
    "CONN",
    "CONNECT",
    "DEF",
    "DEFINE",
    "DESC",
    "DESCRIBE",
    "DISC",
    "DISCONNECT",
    "EXEC",
    "EXECUTE",
    "HOST",
    "PAUSE",
    "PRI",
    "PRINT",
    "PRO",
    "PROMPT",
    "REM",
    "REMARK",
    "SET",
    "SHO",
    "SHOW",
    "SPO",
    "SPOOL",
    "STA",
    "START",
    "UNDEF",
    "UNDEFINE",
    "VAR",
    "VARIABLE",
    "WHENEVER",
];

/// `SET` is a SQL*Plus command unless followed by one of these.
const SQL_SET_TARGETS: &[&str] = &["TRANSACTION", "ROLE", "CONSTRAINT", "CONSTRAINTS"];

const PLSQL_UNITS: &[&str] = &[
    "FUNCTION",
    "PROCEDURE",
    "PACKAGE",
    "TRIGGER",
    "TYPE",
    "LIBRARY",
    "JAVA",
];

/// Words allowed between `CREATE` and the unit type of a PL/SQL unit.
const CREATE_MODIFIERS: &[&str] = &[
    "OR",
    "REPLACE",
    "EDITIONABLE",
    "NONEDITIONABLE",
    "EDITIONING",
    "AND",
    "RESOLVE",
    "COMPILE",
    "NOFORCE",
    "FORCE",
    "SOURCE",
    "NAMED",
];

pub fn split_statements(src: &str) -> Vec<Statement> {
    Splitter::new(src).run()
}

/// Like [`split_statements`], reusing tokens from [`tokenize`].
pub(crate) fn split_with_tokens(src: &str, tokens: Vec<Token>) -> Vec<Statement> {
    Splitter {
        src,
        tokens,
        out: Vec::new(),
    }
    .run()
}

/// Returns the statement the cursor at byte `offset` belongs to.
///
/// A cursor in the gap between two statements belongs to the preceding one,
/// so executing right after typing `;` runs the statement just written.
pub fn statement_at(src: &str, offset: usize) -> Option<Statement> {
    let statements = split_statements(src);
    let idx = statements
        .iter()
        .rposition(|s| s.range.start <= offset)
        .unwrap_or(0);
    statements.into_iter().nth(idx)
}

struct Splitter<'a> {
    src: &'a str,
    tokens: Vec<Token>,
    out: Vec<Statement>,
}

impl<'a> Splitter<'a> {
    fn new(src: &'a str) -> Self {
        Self {
            src,
            tokens: tokenize(src),
            out: Vec::new(),
        }
    }

    fn text(&self, t: &Token) -> &'a str {
        &self.src[t.span.clone()]
    }

    fn word_eq(&self, t: &Token, any_of: &[&str]) -> bool {
        t.kind.is_word() && any_of.iter().any(|w| self.text(t).eq_ignore_ascii_case(w))
    }

    /// True if the token is a `/` alone on its line (ignoring whitespace).
    fn is_slash_line(&self, idx: usize) -> bool {
        let t = &self.tokens[idx];
        if t.kind != TokenKind::Operator || self.text(t) != "/" {
            return false;
        }
        let line_start = self.src[..t.span.start].rfind('\n').map_or(0, |i| i + 1);
        let line_end = self.src[t.span.end..]
            .find('\n')
            .map_or(self.src.len(), |i| t.span.end + i);
        self.src[line_start..line_end].trim() == "/"
    }

    fn significant_from(&self, idx: usize) -> impl Iterator<Item = &Token> {
        self.tokens[idx..].iter().filter(|t| !t.kind.is_trivia())
    }

    fn classify(&self, start: usize) -> StatementKind {
        let mut words = self.significant_from(start);
        let Some(first) = words.next() else {
            return StatementKind::Sql;
        };
        if self.text(first) == "@" || self.text(first) == "@@" {
            return StatementKind::SqlPlus;
        }
        if self.word_eq(first, &["DECLARE", "BEGIN"]) || self.text(first) == "<<" {
            return StatementKind::PlSql;
        }
        if self.word_eq(first, &["SET"]) {
            let next = words.next();
            return if next.is_some_and(|n| self.word_eq(n, SQL_SET_TARGETS)) {
                StatementKind::Sql
            } else {
                StatementKind::SqlPlus
            };
        }
        if self.word_eq(first, SQLPLUS_COMMANDS) {
            return StatementKind::SqlPlus;
        }
        if self.word_eq(first, &["CREATE"]) {
            for t in words {
                if self.word_eq(t, PLSQL_UNITS) {
                    return StatementKind::PlSql;
                }
                if !self.word_eq(t, CREATE_MODIFIERS) {
                    break;
                }
            }
        }
        StatementKind::Sql
    }

    fn push(&mut self, kind: StatementKind, first: usize, last: usize) {
        let range = self.tokens[first].span.start..self.tokens[last].span.end;
        self.out.push(Statement { kind, range });
    }

    fn run(mut self) -> Vec<Statement> {
        let mut i = 0;
        while i < self.tokens.len() {
            if self.tokens[i].kind.is_trivia() || self.is_slash_line(i) {
                i += 1;
                continue;
            }
            let start = i;
            let kind = self.classify(start);
            // Index of the last significant token that belongs to the statement.
            let mut last = start;
            i += 1;
            while i < self.tokens.len() {
                let t = &self.tokens[i];
                match kind {
                    StatementKind::SqlPlus if t.kind == TokenKind::Newline => break,
                    StatementKind::Sql if t.kind == TokenKind::Semicolon => {
                        i += 1;
                        break;
                    }
                    StatementKind::Sql | StatementKind::PlSql if self.is_slash_line(i) => {
                        i += 1;
                        break;
                    }
                    _ => {}
                }
                if !t.kind.is_trivia() {
                    last = i;
                }
                i += 1;
            }
            // SQL*Plus commands may carry a trailing `;` (e.g. `exec foo;`).
            if kind == StatementKind::SqlPlus
                && self.tokens[last].kind == TokenKind::Semicolon
                && last > start
            {
                last -= 1;
                while self.tokens[last].kind.is_trivia() {
                    last -= 1;
                }
            }
            self.push(kind, start, last);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(src: &str) -> Vec<(StatementKind, &str)> {
        split_statements(src)
            .iter()
            .map(|s| (s.kind, s.text(src)))
            .collect()
    }

    #[test]
    fn sql_statements() {
        assert_eq!(
            split(
                "select 1 from dual;\n\n  update t set x = ';' where y = 1 ;\nselect 2 from dual"
            ),
            vec![
                (StatementKind::Sql, "select 1 from dual"),
                (StatementKind::Sql, "update t set x = ';' where y = 1"),
                (StatementKind::Sql, "select 2 from dual"),
            ]
        );
    }

    #[test]
    fn slash_terminates_sql() {
        assert_eq!(
            split("select 1\nfrom dual\n/\nselect 4/2 from dual\n /\n"),
            vec![
                (StatementKind::Sql, "select 1\nfrom dual"),
                (StatementKind::Sql, "select 4/2 from dual")
            ]
        );
    }

    #[test]
    fn plsql_blocks() {
        let src = "create or replace package body p as\n  procedure x is begin null; end;\nend p;\n/\n\
                   begin\n  p.x;\nend;\n/\ndeclare v number; begin null; end;";
        assert_eq!(
            split(src),
            vec![
                (
                    StatementKind::PlSql,
                    "create or replace package body p as\n  procedure x is begin null; end;\nend p;"
                ),
                (StatementKind::PlSql, "begin\n  p.x;\nend;"),
                (StatementKind::PlSql, "declare v number; begin null; end;"),
            ]
        );
    }

    #[test]
    fn create_table_is_sql() {
        assert_eq!(
            split(
                "create table t (a number);\ncreate or replace editionable function f return number is begin return 1; end;\n/"
            ),
            vec![
                (StatementKind::Sql, "create table t (a number)"),
                (
                    StatementKind::PlSql,
                    "create or replace editionable function f return number is begin return 1; end;"
                ),
            ]
        );
    }

    #[test]
    fn sqlplus_commands() {
        assert_eq!(
            split(
                "set serveroutput on\nset transaction read only;\nprompt hello; world\nexec dbms_lock.sleep(1);\n@install.sql\nselect 1 from dual;"
            ),
            vec![
                (StatementKind::SqlPlus, "set serveroutput on"),
                (StatementKind::Sql, "set transaction read only"),
                (StatementKind::SqlPlus, "prompt hello; world"),
                (StatementKind::SqlPlus, "exec dbms_lock.sleep(1)"),
                (StatementKind::SqlPlus, "@install.sql"),
                (StatementKind::Sql, "select 1 from dual"),
            ]
        );
    }

    #[test]
    fn comments_are_not_terminators() {
        assert_eq!(
            split("-- leading; comment\nselect 1 /* ; */ from dual -- trailing;\n;"),
            vec![(StatementKind::Sql, "select 1 /* ; */ from dual")]
        );
    }

    #[test]
    fn cursor_position() {
        let src = "select 1 from dual;\n\nselect 2 from dual;";
        assert_eq!(
            statement_at(src, 3).unwrap().text(src),
            "select 1 from dual"
        );
        assert_eq!(
            statement_at(src, 20).unwrap().text(src),
            "select 1 from dual"
        );
        assert_eq!(
            statement_at(src, 25).unwrap().text(src),
            "select 2 from dual"
        );
        assert_eq!(statement_at("", 0), None);
    }
}
