//! Code structure for an outline view.

use std::ops::Range;

use orui_plsql::ast::*;

use crate::sema::{Sema, SymRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OutlineKind {
    PackageSpec,
    PackageBody,
    Procedure,
    Function,
    Trigger,
    Type,
    TypeBody,
    Block,
    Variable,
    Constant,
    Cursor,
    TypeDecl,
    Exception,
    Statement,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineItem {
    pub name: String,
    pub kind: OutlineKind,
    /// Signature or type, for display.
    pub detail: String,
    /// The whole construct.
    pub span: Range<usize>,
    /// Where to put the cursor when the item is selected.
    pub name_span: Range<usize>,
    pub children: Vec<OutlineItem>,
}

pub(crate) fn outline(sema: &Sema) -> Vec<OutlineItem> {
    let ast = sema.ast;
    let mut out = Vec::new();
    for stmt in &ast.statements {
        match stmt.kind {
            StatementKind::Unit | StatementKind::Block => {
                let units = ast.scope(Ast::ROOT).children.iter().copied().filter(|c| {
                    let s = ast.scope(*c);
                    s.span.start >= stmt.span.start && s.span.start <= stmt.span.end
                });
                for id in units {
                    out.extend(unit_item(sema, id));
                }
                // Top-level CREATE PROCEDURE/FUNCTION are root symbols.
                for (index, s) in ast.scope(Ast::ROOT).symbols.iter().enumerate() {
                    if s.span.start >= stmt.span.start && s.span.start <= stmt.span.end {
                        out.extend(symbol_item(
                            sema,
                            SymRef {
                                scope: Ast::ROOT,
                                index,
                            },
                            true,
                        ));
                    }
                }
            }
            StatementKind::Query | StatementKind::Other => {
                let text: String = sema
                    .text(&stmt.span)
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let name = if text.chars().count() > 60 {
                    format!("{}…", text.chars().take(60).collect::<String>())
                } else {
                    text
                };
                let first =
                    stmt.span.start..stmt.span.start.max(stmt.span.end.min(stmt.span.start + 1));
                out.push(OutlineItem {
                    name,
                    kind: OutlineKind::Statement,
                    detail: String::new(),
                    span: stmt.span.clone(),
                    name_span: first,
                    children: Vec::new(),
                });
            }
            StatementKind::SqlPlus => {}
        }
    }
    out.sort_by_key(|i| i.span.start);
    out
}

fn unit_item(sema: &Sema, id: ScopeId) -> Option<OutlineItem> {
    let scope = sema.ast.scope(id);
    let kind = match scope.kind {
        ScopeKind::PackageSpec => OutlineKind::PackageSpec,
        ScopeKind::PackageBody => OutlineKind::PackageBody,
        ScopeKind::Trigger => OutlineKind::Trigger,
        ScopeKind::TypeSpec => OutlineKind::Type,
        ScopeKind::TypeBody => OutlineKind::TypeBody,
        ScopeKind::Block => OutlineKind::Block,
        // Standalone procedures/functions appear via their root symbol.
        _ => return None,
    };
    let (name, name_span) = match &scope.name {
        Some(n) => {
            let owner = scope
                .owner
                .as_ref()
                .map_or(String::new(), |o| format!("{}.", sema.text(&o.span)));
            (format!("{owner}{}", sema.text(&n.span)), n.span.clone())
        }
        None => (
            "anonymous block".to_owned(),
            scope.span.start..scope.span.start,
        ),
    };
    let detail = match scope.kind {
        ScopeKind::PackageBody => "body".into(),
        ScopeKind::TypeBody => "type body".into(),
        ScopeKind::Trigger => scope.trigger_table.as_ref().map_or(String::new(), |t| {
            format!(
                "on {}",
                t.iter()
                    .map(|i| sema.text(&i.span))
                    .collect::<Vec<_>>()
                    .join(".")
            )
        }),
        _ => String::new(),
    };
    let package_level = matches!(
        scope.kind,
        ScopeKind::PackageSpec | ScopeKind::PackageBody | ScopeKind::TypeSpec | ScopeKind::TypeBody
    );
    let children = (0..scope.symbols.len())
        .filter_map(|index| symbol_item(sema, SymRef { scope: id, index }, package_level))
        .collect();
    Some(OutlineItem {
        name,
        kind,
        detail,
        span: scope.span.clone(),
        name_span,
        children,
    })
}

/// Item for a symbol. `all`: include variables/constants/exceptions (package
/// level); inside subprograms only nested units, types and cursors show.
fn symbol_item(sema: &Sema, r: SymRef, all: bool) -> Option<OutlineItem> {
    let s = sema.symbol(r);
    let kind = match s.kind {
        SymbolKind::Procedure => OutlineKind::Procedure,
        SymbolKind::Function => OutlineKind::Function,
        SymbolKind::Cursor => OutlineKind::Cursor,
        SymbolKind::Type | SymbolKind::Subtype => OutlineKind::TypeDecl,
        SymbolKind::Variable | SymbolKind::Attribute if all => OutlineKind::Variable,
        SymbolKind::Constant if all => OutlineKind::Constant,
        SymbolKind::Exception if all => OutlineKind::Exception,
        _ => return None,
    };
    let detail = match s.kind {
        SymbolKind::Procedure | SymbolKind::Function => crate::complete::signature_detail(sema, r),
        _ => s.ty.as_ref().map(|t| t.text.clone()).unwrap_or_default(),
    };
    let children = s
        .body
        .map(|b| {
            let body = sema.ast.scope(b);
            (0..body.symbols.len())
                .filter_map(|index| symbol_item(sema, SymRef { scope: b, index }, false))
                .collect()
        })
        .unwrap_or_default();
    Some(OutlineItem {
        name: sema.text(&s.name.span).to_owned(),
        kind,
        detail,
        span: s.span.clone(),
        name_span: s.name.span.clone(),
        children,
    })
}
