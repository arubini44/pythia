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

#[cfg(test)]
mod tests {
    use super::*;

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
