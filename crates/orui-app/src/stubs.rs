//! Package body stubs generated from the specification.

use orui_lsp_core::{Analysis, OutlineItem, OutlineKind};
use orui_plsql::{TokenKind, tokenize};

/// Normalized signature for matching spec and body declarations.
fn key(text: &str) -> String {
    tokenize(text)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .map(|t| text[t.span].to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A body subprogram's header: its text up to the `IS`/`AS` that starts it.
fn body_header(text: &str) -> &str {
    let mut depth = 0;
    for t in tokenize(text) {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            TokenKind::Keyword
                if depth == 0
                    && matches!(
                        text[t.span.clone()].to_ascii_uppercase().as_str(),
                        "IS" | "AS"
                    ) =>
            {
                return &text[..t.span.start];
            }
            _ => {}
        }
    }
    text
}

fn subprograms(items: &[OutlineItem], kind: OutlineKind) -> Vec<&OutlineItem> {
    items
        .iter()
        .filter(|i| i.kind == kind)
        .flat_map(|i| i.children.iter())
        .filter(|c| matches!(c.kind, OutlineKind::Procedure | OutlineKind::Function))
        .collect()
}

/// Result of generating stubs.
pub struct Stubs {
    pub body: String,
    pub added: usize,
}

/// Builds (or completes) a package body for `spec`. Subprograms already in
/// `body` (matched by full signature, so overloads work) are left alone.
pub fn package_body(spec: &str, body: Option<&str>) -> Option<Stubs> {
    let spec_outline = Analysis::new(spec).outline();
    let package = spec_outline
        .iter()
        .find(|i| i.kind == OutlineKind::PackageSpec)?;
    let name = package.name.to_lowercase();
    let existing: Vec<String> = match body {
        Some(body) if !body.trim().is_empty() => {
            subprograms(&Analysis::new(body).outline(), OutlineKind::PackageBody)
                .into_iter()
                .map(|i| key(body_header(&body[i.span.clone()])))
                .collect()
        }
        _ => Vec::new(),
    };
    let mut stubs = Vec::new();
    for item in subprograms(&spec_outline, OutlineKind::PackageSpec) {
        let decl = spec[item.span.clone()]
            .trim()
            .trim_end_matches(';')
            .trim_end();
        if existing.contains(&key(decl)) {
            continue;
        }
        let result = if item.kind == OutlineKind::Function {
            "return null;"
        } else {
            "null;"
        };
        stubs.push(format!(
            "  {decl} is\n  begin\n    {result} -- TODO: implement\n  end {};\n",
            item.name.to_lowercase()
        ));
    }
    let added = stubs.len();
    let body = match body.filter(|b| !b.trim().is_empty()) {
        None => format!(
            "create or replace package body {name} as\n\n{}\nend {name};\n",
            stubs.join("\n")
        ),
        Some(body) => {
            if added == 0 {
                return Some(Stubs {
                    body: body.to_owned(),
                    added: 0,
                });
            }
            // Before the package's final END.
            let end = tokenize(body).into_iter().rev().find(|t| {
                t.kind == TokenKind::Keyword && body[t.span.clone()].eq_ignore_ascii_case("END")
            })?;
            let line_start = body[..end.span.start].rfind('\n').map_or(0, |i| i + 1);
            format!(
                "{}\n{}\n{}",
                body[..line_start].trim_end_matches('\n'),
                stubs.join("\n"),
                &body[line_start..]
            )
        }
    };
    Some(Stubs { body, added })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "create or replace package p as\n  procedure a(x in number);\n  function b(y varchar2) return number;\n  procedure a(x in varchar2);\nend p;";

    #[test]
    fn new_body() {
        let stubs = package_body(SPEC, None).unwrap();
        assert_eq!(stubs.added, 3);
        assert!(stubs.body.starts_with("create or replace package body p as\n\n  procedure a(x in number) is\n  begin\n    null;"), "{}", stubs.body);
        assert!(
            stubs
                .body
                .contains("  function b(y varchar2) return number is\n  begin\n    return null;")
        );
        assert!(stubs.body.trim_end().ends_with("end p;"));
    }

    #[test]
    fn completes_existing_body() {
        let body = "create or replace package body p as\n  procedure a(x in number) is begin null; end a;\nend p;";
        let stubs = package_body(SPEC, Some(body)).unwrap();
        assert_eq!(stubs.added, 2);
        assert!(stubs.body.contains("procedure a(x in varchar2) is"));
        assert!(!stubs.body.contains("procedure a(x in number) is\n"));
        assert!(stubs.body.trim_end().ends_with("end p;"));
    }
}
