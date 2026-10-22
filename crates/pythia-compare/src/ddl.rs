//! DDL text normalization.
//!
//! DBMS_METADATA output differs between schemas and versions in ways that
//! don't matter (schema prefixes, `EDITIONABLE`, trailing blanks, terminators,
//! system generated constraint names, a sequence's current `START WITH`).
//! [`display`] removes the noise but keeps the text readable for diffing;
//! [`canonical`] goes further and is only used to decide equality.

use pythia_plsql::{StatementKind, TokenKind, split_statements, tokenize};

/// Normalization switches (see [`crate::schema::Options`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Normalize {
    pub ignore_whitespace: bool,
    pub ignore_case: bool,
    pub ignore_comments: bool,
}

impl Default for Normalize {
    fn default() -> Self {
        Self {
            ignore_whitespace: true,
            ignore_case: false,
            ignore_comments: false,
        }
    }
}

/// One executable statement of a DDL text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdlStatement {
    pub sql: String,
    /// PL/SQL units must be terminated with `/` in scripts.
    pub plsql: bool,
}

/// Splits DDL (as produced with the `SQLTERMINATOR` transform) into
/// statements without their terminators.
pub fn statements(ddl: &str) -> Vec<DdlStatement> {
    split_statements(ddl)
        .into_iter()
        .map(|s| DdlStatement {
            sql: s.text(ddl).trim().to_owned(),
            plsql: s.kind == StatementKind::PlSql,
        })
        .filter(|s| !s.sql.is_empty())
        .collect()
}

/// Renders statements with SQL*Plus terminators.
pub fn render(statements: &[DdlStatement]) -> String {
    let mut out = String::new();
    for s in statements {
        out.push_str(s.sql.trim_end());
        if s.plsql {
            out.push_str("\n/\n");
        } else {
            out.push_str(";\n");
        }
    }
    out
}

/// Readable normalization: drops the schema qualifier, `EDITIONABLE`
/// keywords, trailing blanks, blank-line runs and terminators.
pub fn display(ddl: &str, schema: &str) -> String {
    let stmts: Vec<DdlStatement> = statements(ddl)
        .into_iter()
        .map(|s| DdlStatement {
            sql: strip_noise(&s.sql, schema),
            plsql: s.plsql,
        })
        .collect();
    let mut text = String::new();
    for (i, s) in stmts.iter().enumerate() {
        if i > 0 {
            text.push('\n');
        }
        let mut blank = false;
        for line in s.sql.lines() {
            let line = line.trim_end();
            if line.is_empty() {
                if blank {
                    continue;
                }
                blank = true;
            } else {
                blank = false;
            }
            text.push_str(line);
            text.push('\n');
        }
    }
    text.trim_end().to_owned() + "\n"
}

/// Removes schema qualifiers and edition keywords, token-aware so string
/// literals and comments are left alone.
fn strip_noise(sql: &str, schema: &str) -> String {
    let tokens = tokenize(sql);
    let mut out = String::with_capacity(sql.len());
    let quoted_schema = format!("\"{schema}\"");
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let text = &sql[t.span.clone()];
        // `"SCHEMA".` or `SCHEMA.` followed by a name.
        let is_schema = match t.kind {
            TokenKind::QuotedIdent => text == quoted_schema,
            TokenKind::Ident | TokenKind::Keyword => text.eq_ignore_ascii_case(schema),
            _ => false,
        };
        if is_schema && tokens.get(i + 1).is_some_and(|n| n.kind == TokenKind::Dot) {
            i += 2;
            continue;
        }
        if t.kind.is_word()
            && (text.eq_ignore_ascii_case("EDITIONABLE")
                || text.eq_ignore_ascii_case("NONEDITIONABLE"))
        {
            // Also drop the following blank.
            if tokens
                .get(i + 1)
                .is_some_and(|n| n.kind == TokenKind::Whitespace)
            {
                i += 1;
            }
            i += 1;
            continue;
        }
        out.push_str(text);
        i += 1;
    }
    out
}

/// The form used for equality: tokens joined with single spaces (or kept
/// verbatim), optionally uppercased and without comments. Also removes
/// system generated constraint names and a sequence's `START WITH` value.
pub fn canonical(display_text: &str, opts: Normalize) -> String {
    let tokens = tokenize(display_text);
    let mut words: Vec<String> = Vec::with_capacity(tokens.len());
    let mut verbatim = String::new();
    let mut i = 0;
    while i < tokens.len() {
        let t = &tokens[i];
        let text = &display_text[t.span.clone()];
        let comment = matches!(t.kind, TokenKind::LineComment | TokenKind::BlockComment);
        if comment && opts.ignore_comments {
            i += 1;
            continue;
        }
        let word = if opts.ignore_case && matches!(t.kind, TokenKind::Ident | TokenKind::Keyword) {
            text.to_ascii_uppercase()
        } else {
            text.to_owned()
        };
        if t.kind.is_trivia() && !comment {
            if !opts.ignore_whitespace {
                verbatim.push_str(text);
            }
        } else {
            if !opts.ignore_whitespace {
                verbatim.push_str(&word);
            }
            words.push(word);
        }
        i += 1;
    }
    let joined = if opts.ignore_whitespace {
        words.join(" ")
    } else {
        verbatim
    };
    strip_volatile(&joined)
}

/// Removes parts that legitimately differ between equal objects.
fn strip_volatile(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        // `CONSTRAINT "SYS_C0012345" ` → ``
        let upper = rest.to_ascii_uppercase();
        let next_sys = upper.find("CONSTRAINT \"SYS_C");
        let next_start = upper.find("START WITH ");
        match (next_sys, next_start) {
            (None, None) => {
                out.push_str(rest);
                return out;
            }
            (Some(s), other) if other.is_none_or(|o| s < o) => {
                out.push_str(&rest[..s]);
                let after = &rest[s + "CONSTRAINT \"".len()..];
                let end = after.find('"').map_or(after.len(), |e| e + 1);
                rest = after[end..].trim_start();
            }
            (_, Some(s)) => {
                out.push_str(&rest[..s]);
                let after = &rest[s + "START WITH ".len()..];
                let end = after
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(after.len());
                rest = after[end..].trim_start();
            }
            _ => unreachable!(),
        }
    }
}

/// Splits `display` text into lines for diffing.
pub fn lines(text: &str) -> Vec<&str> {
    text.lines().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_and_renders() {
        let ddl = "\n  CREATE OR REPLACE EDITIONABLE PROCEDURE \"HR\".\"P\" is begin null; end;\n/\n  ALTER TRIGGER \"HR\".\"T\" ENABLE;";
        let s = statements(ddl);
        assert_eq!(s.len(), 2);
        assert!(s[0].plsql && !s[1].plsql);
        assert_eq!(render(&s), format!("{}\n/\n{};\n", s[0].sql, s[1].sql));
    }

    #[test]
    fn display_strips_schema_and_edition() {
        let ddl = "  CREATE OR REPLACE EDITIONABLE PACKAGE \"PYTHIA_A\".\"PKG\" AS\n  x varchar2(10) := 'PYTHIA_A.y';   \n\n\n  END;\n/\n";
        assert_eq!(
            display(ddl, "PYTHIA_A"),
            "CREATE OR REPLACE PACKAGE \"PKG\" AS\n  x varchar2(10) := 'PYTHIA_A.y';\n\n  END;\n"
        );
        // Unquoted schema prefixes in code are removed as well.
        assert_eq!(
            display("select a from pythia_a.t;", "PYTHIA_A"),
            "select a from t\n"
        );
    }

    #[test]
    fn canonical_ignores_noise() {
        let o = Normalize::default();
        assert_eq!(
            canonical("create  table t (\n a number )", o),
            canonical("create table t ( a number )", o)
        );
        assert_ne!(
            canonical("create table t (a number)", o),
            canonical("create table t (a varchar2(1))", o)
        );
        let ci = Normalize {
            ignore_case: true,
            ..o
        };
        assert_eq!(
            canonical("Create Table T", ci),
            canonical("CREATE TABLE t", ci)
        );
        assert_ne!(
            canonical("x := 'a'", ci),
            canonical("x := 'A'", ci),
            "literals keep case"
        );
        let nc = Normalize {
            ignore_comments: true,
            ..o
        };
        assert_eq!(canonical("x -- note\n := 1", nc), canonical("x := 1", nc));
        assert_ne!(canonical("x -- note\n := 1", o), canonical("x := 1", o));
    }

    #[test]
    fn canonical_drops_volatile_parts() {
        let o = Normalize::default();
        assert_eq!(
            canonical(
                "CREATE SEQUENCE \"S\" MINVALUE 1 INCREMENT BY 1 START WITH 2041 CACHE 20",
                o
            ),
            canonical(
                "CREATE SEQUENCE \"S\" MINVALUE 1 INCREMENT BY 1 START WITH 1 CACHE 20",
                o
            )
        );
        assert_eq!(
            canonical("A NUMBER CONSTRAINT \"SYS_C008812\" NOT NULL ENABLE", o),
            canonical("A NUMBER CONSTRAINT \"SYS_C001\" NOT NULL ENABLE", o)
        );
        assert_ne!(
            canonical("CONSTRAINT \"PK_T\" PRIMARY KEY", o),
            canonical("CONSTRAINT \"PK_U\" PRIMARY KEY", o)
        );
    }

    #[test]
    fn verbatim_mode_keeps_whitespace() {
        let o = Normalize {
            ignore_whitespace: false,
            ..Normalize::default()
        };
        assert_ne!(canonical("a  b", o), canonical("a b", o));
    }
}
