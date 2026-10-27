//! Oracle's PL/SQL error stack: the `ORA-06512: at ...` lines of an error,
//! and where the code of each one is.

use pythia_plsql::{TokenKind, tokenize};

use crate::DbError;
use crate::meta::ObjectType;
use crate::oracle::Connection;

/// One `ORA-06512` line of an error. Oracle lists them innermost first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackFrame {
    /// Index of the frame's line in the error text.
    pub text_line: usize,
    /// `(owner, name)` of the stored unit, `None` for an anonymous block.
    pub unit: Option<(String, String)>,
    /// The line as Oracle reports it (for a trigger, counted from its
    /// block).
    pub line: u32,
}

/// The `ORA-06512` frames in an error's text.
pub fn parse(text: &str) -> Vec<StackFrame> {
    text.lines()
        .enumerate()
        .filter_map(|(text_line, l)| {
            let rest = l.trim().strip_prefix("ORA-06512: at ")?;
            if let Some(line) = rest.strip_prefix("line ") {
                return Some(StackFrame {
                    text_line,
                    unit: None,
                    line: line.trim().parse().ok()?,
                });
            }
            // `"OWNER.NAME", line N`: names are shown as stored, unquoted.
            let (unit, line) = rest.strip_prefix('"')?.rsplit_once("\", line ")?;
            let (owner, name) = unit.split_once('.')?;
            Some(StackFrame {
                text_line,
                unit: Some((owner.to_owned(), name.to_owned())),
                line: line.trim().parse().ok()?,
            })
        })
        .collect()
}

/// Where a stored unit's frame is in the code Pythia shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameSource {
    /// The part of the unit and the line of its source (`ALL_SOURCE`), with
    /// that line's text.
    Source {
        object_type: ObjectType,
        line: u32,
        text: String,
    },
    /// The source can't be shown, and why.
    Hidden(String),
}

/// The parts a frame's line can be in, the most likely first: code runs
/// in bodies, a spec only initializes its variables.
const PARTS: [ObjectType; 7] = [
    ObjectType::PackageBody,
    ObjectType::TypeBody,
    ObjectType::Procedure,
    ObjectType::Function,
    ObjectType::Trigger,
    ObjectType::Package,
    ObjectType::Type,
];

/// Finds the code of a stored unit's frame: line `line` of `owner.name`.
pub fn locate(
    conn: &Connection,
    owner: &str,
    name: &str,
    line: u32,
) -> Result<FrameSource, DbError> {
    // Line 1 tells wrapped code; a trigger's whole source tells where its
    // block starts.
    let rows = conn.query_as::<(String, u32, Option<String>)>(
        "select type, line, text from all_source \
          where owner = :1 and name = :2 \
            and type in ('PACKAGE BODY', 'TYPE BODY', 'PROCEDURE', 'FUNCTION', 'TRIGGER', \
                         'PACKAGE', 'TYPE') \
            and (line in (1, :3) or type = 'TRIGGER') \
          order by line",
        &[&owner, &name, &line],
    )?;
    let mut lines: Vec<(ObjectType, u32, String)> = Vec::new();
    for row in rows {
        let (ty, n, text) = row?;
        if let Some(ty) = ObjectType::from_dictionary(&ty) {
            lines.push((ty, n, text.unwrap_or_default()));
        }
    }
    Ok(pick(&lines, line))
}

/// [`locate`]'s answer from the source lines it read: `(part, line, text)`.
fn pick(lines: &[(ObjectType, u32, String)], line: u32) -> FrameSource {
    let Some(&object_type) = PARTS.iter().find(|t| lines.iter().any(|(ty, ..)| ty == *t)) else {
        return FrameSource::Hidden("Its source isn't visible to you".into());
    };
    if let Some(body) = object_type.body() {
        // Its body exists (it is where the error came from), but isn't
        // visible: without the DEBUG privilege, EXECUTE shows only the spec.
        return FrameSource::Hidden(format!(
            "The source of its {} isn't visible to you",
            body.dictionary_name().to_lowercase()
        ));
    }
    let part: Vec<_> = lines.iter().filter(|(ty, ..)| *ty == object_type).collect();
    if part
        .iter()
        .find(|(_, n, _)| *n == 1)
        .is_some_and(|(_, _, text)| is_wrapped(text))
    {
        return FrameSource::Hidden("Its source is wrapped".into());
    }
    let line = if object_type == ObjectType::Trigger {
        let source: String = part.iter().map(|(_, _, text)| text.as_str()).collect();
        let body = pythia_plsql::trigger_body_line(&source).map_or(1, |l| l as u32);
        line + body - 1
    } else {
        line
    };
    let text = part
        .iter()
        .find(|(_, n, _)| *n == line)
        .map(|(_, _, text)| text.trim_end().to_owned())
        .unwrap_or_default();
    FrameSource::Source {
        object_type,
        line,
        text,
    }
}

/// True if a unit's first source line is the header of wrapped code, e.g.
/// `package body owner.name wrapped`.
fn is_wrapped(first_line: &str) -> bool {
    let words: Vec<_> = tokenize(first_line)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let upper = |i: usize| first_line[words[i].span.clone()].to_ascii_uppercase();
    let mut i = 0;
    while i < words.len()
        && words[i].kind.is_word()
        && matches!(
            upper(i).as_str(),
            "PACKAGE"
                | "TYPE"
                | "BODY"
                | "PROCEDURE"
                | "FUNCTION"
                | "TRIGGER"
                | "EDITIONABLE"
                | "NONEDITIONABLE"
        )
    {
        i += 1;
    }
    let name = |i: usize| {
        words
            .get(i)
            .is_some_and(|t| t.kind.is_word() || t.kind == TokenKind::QuotedIdent)
    };
    if i == 0 || !name(i) {
        return false;
    }
    i += 1;
    if words.get(i).is_some_and(|t| t.kind == TokenKind::Dot) && name(i + 1) {
        i += 2;
    }
    words.get(i).is_some_and(|t| t.kind.is_word()) && upper(i) == "WRAPPED"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(text_line: usize, unit: Option<(&str, &str)>, line: u32) -> StackFrame {
        StackFrame {
            text_line,
            unit: unit.map(|(o, n)| (o.into(), n.into())),
            line,
        }
    }

    /// An error raised in a package called from a trigger, as Oracle 23
    /// reports it.
    #[test]
    fn parses_the_frames() {
        let text = "ORA-01403: no data found\n\
                    ORA-06512: at \"PYTHIA.ZZ_STK_P\", line 5\n\
                    ORA-06512: at \"PYTHIA.ZZ_STK_P\", line 9\n\
                    ORA-06512: at \"PYTHIA.Mixed Case.Trg\", line 5\n\
                    ORA-04088: error during execution of trigger 'PYTHIA.ZZ_STK_TRG'\n\
                    ORA-06512: at line 2\n\
                    Help: https://docs.oracle.com/error-help/db/ora-01403/";
        assert_eq!(
            parse(text),
            vec![
                frame(1, Some(("PYTHIA", "ZZ_STK_P")), 5),
                frame(2, Some(("PYTHIA", "ZZ_STK_P")), 9),
                frame(3, Some(("PYTHIA", "Mixed Case.Trg")), 5),
                frame(5, None, 2),
            ]
        );
        assert!(parse("ORA-00942: table or view does not exist").is_empty());
    }

    #[test]
    fn wrapped_headers() {
        assert!(is_wrapped("package body dbms_x wrapped \n"));
        assert!(is_wrapped("PACKAGE BODY \"SYS\".\"My Pkg\" wrapped\n"));
        assert!(is_wrapped("function f wrapped\n"));
        assert!(!is_wrapped("package body wrapped_api as\n"));
        assert!(!is_wrapped("procedure wrapped is\n"));
        assert!(!is_wrapped("procedure p(wrapped number) is\n"));
    }

    fn source(ty: ObjectType, lines: &[&str]) -> Vec<(ObjectType, u32, String)> {
        lines
            .iter()
            .enumerate()
            .map(|(i, l)| (ty, i as u32 + 1, format!("{l}\n")))
            .collect()
    }

    #[test]
    fn picks_the_part_and_line() {
        let mut lines = source(ObjectType::Package, &["package p as"]);
        lines.extend(source(
            ObjectType::PackageBody,
            &[
                "package body p as",
                "  procedure x is",
                "  begin",
                "    null;",
            ],
        ));
        assert_eq!(
            pick(&lines, 4),
            FrameSource::Source {
                object_type: ObjectType::PackageBody,
                line: 4,
                text: "    null;".into()
            }
        );
        let trigger = source(
            ObjectType::Trigger,
            &[
                "trigger t",
                "before insert on x",
                "for each row",
                "declare",
                "  n number;",
                "begin",
                "  n := 1;",
            ],
        );
        assert_eq!(
            pick(&trigger, 4),
            FrameSource::Source {
                object_type: ObjectType::Trigger,
                line: 7,
                text: "  n := 1;".into()
            }
        );
        assert_eq!(
            pick(&source(ObjectType::Package, &["package p as"]), 3),
            FrameSource::Hidden("The source of its package body isn't visible to you".into())
        );
        assert_eq!(
            pick(
                &source(
                    ObjectType::PackageBody,
                    &["package body p wrapped", "a000000"]
                ),
                2
            ),
            FrameSource::Hidden("Its source is wrapped".into())
        );
        assert_eq!(
            pick(&[], 2),
            FrameSource::Hidden("Its source isn't visible to you".into())
        );
    }
}
