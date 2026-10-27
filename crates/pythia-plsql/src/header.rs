//! Recognizes the header of `CREATE [OR REPLACE] <unit> [owner.]name`.

use crate::lexer::{TokenKind, tokenize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitHeader {
    /// Dictionary type name, e.g. `PACKAGE BODY`, `TYPE`, `TRIGGER`.
    pub object_type: String,
    pub owner: Option<String>,
    pub name: String,
}

const MODIFIERS: &[&str] = &[
    "OR",
    "REPLACE",
    "EDITIONABLE",
    "NONEDITIONABLE",
    "EDITIONING",
    "FORCE",
    "NOFORCE",
    "AND",
    "RESOLVE",
    "COMPILE",
    "NOSHARE",
    "SHARING",
    "METADATA",
    "NONE",
];

/// Parses the header of a `CREATE` statement for a stored PL/SQL unit.
///
/// Identifiers are normalized the way Oracle stores them: unquoted names are
/// uppercased, quoted names are kept verbatim.
pub fn unit_header(sql: &str) -> Option<UnitHeader> {
    let tokens: Vec<_> = tokenize(sql)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let text = |i: usize| &sql[tokens[i].span.clone()];
    let word = |i: usize| {
        tokens
            .get(i)
            .filter(|t| t.kind.is_word())
            .map(|_| text(i).to_ascii_uppercase())
    };

    if word(0).as_deref() != Some("CREATE") {
        return None;
    }
    let mut i = 1;
    while word(i).is_some_and(|w| MODIFIERS.contains(&w.as_str()))
        || (i < tokens.len() && text(i) == "=")
    {
        i += 1;
    }
    let mut object_type = word(i)?;
    match object_type.as_str() {
        "PACKAGE" | "TYPE" if word(i + 1).as_deref() == Some("BODY") => {
            object_type.push_str(" BODY");
            i += 2;
        }
        "JAVA" => {
            // CREATE JAVA SOURCE NAMED "x" ...
            object_type = "JAVA SOURCE".into();
            i += 2;
            if word(i).as_deref() == Some("NAMED") {
                i += 1;
            }
        }
        "PACKAGE" | "TYPE" | "PROCEDURE" | "FUNCTION" | "TRIGGER" | "LIBRARY" => i += 1,
        _ => return None,
    }

    let ident = |i: usize| -> Option<String> {
        let t = tokens.get(i)?;
        match t.kind {
            TokenKind::Ident | TokenKind::Keyword => Some(text(i).to_ascii_uppercase()),
            TokenKind::QuotedIdent => Some(text(i).trim_matches(['"', '`']).to_owned()),
            _ => None,
        }
    };
    let first = ident(i)?;
    if tokens.get(i + 1).is_some_and(|t| t.kind == TokenKind::Dot) {
        Some(UnitHeader {
            object_type,
            owner: Some(first),
            name: ident(i + 2)?,
        })
    } else {
        Some(UnitHeader {
            object_type,
            owner: None,
            name: first,
        })
    }
}

/// The 1-based line of a trigger's source on which its PL/SQL block starts:
/// `DECLARE`, `BEGIN`, `COMPOUND TRIGGER` or `CALL`. Oracle counts the lines
/// of a trigger's compile errors and error stack from there, not from the
/// `TRIGGER` line. `None` if there is no block.
pub fn trigger_body_line(source: &str) -> Option<usize> {
    let tokens: Vec<_> = tokenize(source)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let word = |i: usize| {
        tokens
            .get(i)
            .filter(|t| t.kind.is_word())
            .map(|t| source[t.span.clone()].to_ascii_uppercase())
    };
    let mut depth = 0usize;
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth = depth.saturating_sub(1),
            _ if depth == 0 => {
                // Names in the header: `trigger x`, `of a, b`, `on owner.t`,
                // `as n`, `follows x`.
                let named = i > 0
                    && (matches!(tokens[i - 1].kind, TokenKind::Comma | TokenKind::Dot)
                        || word(i - 1).is_some_and(|w| {
                            matches!(
                                w.as_str(),
                                "TRIGGER" | "OF" | "ON" | "AS" | "FOLLOWS" | "PRECEDES"
                            )
                        }));
                let block = match word(i).as_deref() {
                    Some("DECLARE" | "BEGIN" | "CALL") => true,
                    Some("COMPOUND") => word(i + 1).as_deref() == Some("TRIGGER"),
                    _ => false,
                };
                if block && !named {
                    return Some(source[..t.span.start].matches('\n').count() + 1);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Where Oracle 23 counts a trigger's lines from (compile errors and
    /// ORA-06512 lines checked against the database).
    #[test]
    fn trigger_body_lines() {
        let simple = "trigger zz_stk_trg\nbefore insert on zz_stk_t\nfor each row\ndeclare\n  \
                      x number;\nbegin\n  x := 1;\nend;";
        assert_eq!(trigger_body_line(simple), Some(4));
        let when = "create or replace trigger t_trg\nbefore insert on t\nfor each row\n\
                    when (new.n > 0 and new.s <> 'begin')\n-- begin\n/* declare */\nbegin\n  \
                    null;\nend;";
        assert_eq!(trigger_body_line(when), Some(7));
        let compound = "trigger zz_stk_trg3\nfor insert on zz_stk_t\ncompound trigger\n  \
                        g number;\n  before each row is\n  begin\n    null;\n  end before each \
                        row;\nend;";
        assert_eq!(trigger_body_line(compound), Some(3));
        let call = "trigger t\nbefore insert on t\nfor each row\ncall p(:new.n)";
        assert_eq!(trigger_body_line(call), Some(4));
        // Names that read like the block's keywords.
        let names = "trigger \"BEGIN\"\nbefore update of a, call on hr.call\nreferencing new \
                     as declare\nfor each row begin null; end;";
        assert_eq!(trigger_body_line(names), Some(4));
        assert_eq!(trigger_body_line("trigger t before insert on t"), None);
    }

    fn h(t: &str, owner: Option<&str>, name: &str) -> Option<UnitHeader> {
        Some(UnitHeader {
            object_type: t.into(),
            owner: owner.map(Into::into),
            name: name.into(),
        })
    }

    #[test]
    fn headers() {
        assert_eq!(
            unit_header("create or replace package body hr.emp_pkg as"),
            h("PACKAGE BODY", Some("HR"), "EMP_PKG")
        );
        assert_eq!(
            unit_header("CREATE OR REPLACE EDITIONABLE FUNCTION \"MixedCase\" return number"),
            h("FUNCTION", None, "MixedCase")
        );
        assert_eq!(
            unit_header("-- c\ncreate procedure p is begin null; end;"),
            h("PROCEDURE", None, "P")
        );
        assert_eq!(
            unit_header("create or replace type t as object (x number)"),
            h("TYPE", None, "T")
        );
        assert_eq!(
            unit_header("create or replace trigger trg before insert on t"),
            h("TRIGGER", None, "TRG")
        );
        assert_eq!(unit_header("create table t (x number)"), None);
        assert_eq!(unit_header("select 1 from dual"), None);
    }
}
