//! Signature help, hover, go to definition and find references.

use std::ops::Range;

use orui_plsql::TokenKind;
use orui_plsql::ast::*;

use crate::builtins;
use crate::catalog::{CatalogMember, CatalogRequest, EmptyCatalog, Tracker};
use crate::complete::member_signature;
use crate::sema::{Resolved, Sema, SymRef, is_ref_token};

// ----- signature help -----------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// e.g. `raise_salary(p_emp_id IN NUMBER, p_pct IN NUMBER := …)`
    pub label: String,
    /// Byte ranges of each parameter inside `label`.
    pub params: Vec<Range<usize>>,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignatureHelp {
    /// All overloads; empty while catalog data is still missing.
    pub signatures: Vec<Signature>,
    pub active_signature: usize,
    pub active_param: usize,
    pub missing: Vec<CatalogRequest>,
}

struct Call {
    parts_end: usize,
    commas: usize,
    named: Option<String>,
}

/// Finds the call whose argument list contains `offset`.
fn enclosing_call(sema: &Sema, offset: usize) -> Option<Call> {
    let tokens = sema.tokens;
    let stmt_start = sema.ast.statement_at(offset).map_or(0, |s| s.span.start);
    let mut j = tokens.partition_point(|t| t.span.end <= offset);
    let mut depth = 0usize;
    let mut commas = 0usize;
    let mut named = None;
    let mut segment_done = false;
    while j > 0 {
        j -= 1;
        let t = &tokens[j];
        if t.span.start < stmt_start {
            return None;
        }
        if t.kind.is_trivia() {
            continue;
        }
        match sema.text(&t.span) {
            ")" => depth += 1,
            "(" if depth == 0 => {
                let name = (0..j).rev().find(|&k| !tokens[k].kind.is_trivia())?;
                if !is_ref_token(tokens[name].kind) || tokens[name].kind == TokenKind::BindVar {
                    return None;
                }
                return Some(Call {
                    parts_end: name,
                    commas,
                    named,
                });
            }
            "(" => depth -= 1,
            "," if depth == 0 => {
                commas += 1;
                segment_done = true;
            }
            ";" if depth == 0 => return None,
            "=>" if depth == 0 && !segment_done => {
                let name = (0..j).rev().find(|&k| !tokens[k].kind.is_trivia())?;
                named = Some(orui_plsql::normalize_ident(sema.text(&tokens[name].span)).0);
            }
            _ => {}
        }
    }
    None
}

pub(crate) fn signature_help(sema: &Sema, offset: usize) -> Option<SignatureHelp> {
    let call = enclosing_call(sema, offset)?;
    let parts = sema.chain_ending_at(call.parts_end);
    let names: Vec<&str> = parts.iter().map(|p| p.ident.name.as_str()).collect();
    let mut signatures: Vec<(Signature, Vec<String>)> = Vec::new();

    // Local subprograms and cursors.
    let local: Vec<SymRef> = match names.as_slice() {
        [name] => {
            let mut v = sema.find_subprograms(offset, name);
            if v.is_empty()
                && let Some(r) = sema.find_local(offset, name)
                && sema.symbol(r).kind == SymbolKind::Cursor
            {
                v.push(r);
            }
            v
        }
        [pkg, name] if sema.find_local(offset, pkg).is_none() => match sema.local_package(pkg) {
            Some(scope) => (0..sema.ast.scope(scope).symbols.len())
                .map(|index| SymRef { scope, index })
                .filter(|r| {
                    sema.symbol(*r).kind.is_subprogram() && sema.symbol(*r).name.name == *name
                })
                .collect(),
            None => Vec::new(),
        },
        _ => Vec::new(),
    };
    for r in local {
        signatures.push(local_signature(sema, r));
    }

    if signatures.is_empty()
        && let [name] = names.as_slice()
        && let Some(b) = builtins::function(name)
    {
        let mut label = format!("{}(", b.name);
        let mut ranges = Vec::new();
        let mut param_names = Vec::new();
        for (i, p) in b.params.iter().enumerate() {
            if i > 0 {
                label.push_str(", ");
            }
            ranges.push(label.len()..label.len() + p.len());
            label.push_str(p);
            param_names.push(p.trim_matches(['[', ']']).to_uppercase());
        }
        label.push(')');
        if !b.returns.is_empty() {
            label.push_str(&format!(" → {}", b.returns));
        }
        signatures.push((
            Signature {
                label,
                params: ranges,
                doc: Some(b.doc.to_owned()),
            },
            param_names,
        ));
    }

    if signatures.is_empty() {
        for m in catalog_members(sema, &names) {
            signatures.push(catalog_signature(&m));
        }
    }

    let missing = Vec::new();
    if signatures.is_empty() {
        return Some(SignatureHelp {
            signatures: Vec::new(),
            active_signature: 0,
            active_param: 0,
            missing,
        });
    }
    let active_signature = match &call.named {
        Some(n) => signatures
            .iter()
            .position(|(_, names)| names.contains(n))
            .unwrap_or(0),
        None => signatures
            .iter()
            .position(|(s, _)| {
                s.params.len() > call.commas || (s.params.is_empty() && call.commas == 0)
            })
            .unwrap_or(0),
    };
    let active_param = match &call.named {
        Some(n) => signatures[active_signature]
            .1
            .iter()
            .position(|p| p == n)
            .unwrap_or(call.commas),
        None => call.commas,
    };
    Some(SignatureHelp {
        signatures: signatures.into_iter().map(|(s, _)| s).collect(),
        active_signature,
        active_param,
        missing,
    })
}

fn local_signature(sema: &Sema, r: SymRef) -> (Signature, Vec<String>) {
    let s = sema.symbol(r);
    let mut label = format!("{}(", sema.text(&s.name.span));
    let mut ranges = Vec::new();
    let mut names = Vec::new();
    for (i, p) in s.params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let ty =
            p.ty.as_ref()
                .map_or(String::new(), |t| format!(" {}", t.text));
        let default = if p.has_default { " := …" } else { "" };
        let text = format!(
            "{} {}{ty}{default}",
            sema.text(&p.name.span),
            p.mode.label()
        );
        ranges.push(label.len()..label.len() + text.len());
        label.push_str(&text);
        names.push(p.name.name.clone());
    }
    label.push(')');
    if s.kind == SymbolKind::Function
        && let Some(t) = &s.ty
    {
        label.push_str(&format!(" return {}", t.text));
    }
    (
        Signature {
            label,
            params: ranges,
            doc: None,
        },
        names,
    )
}

fn catalog_signature(m: &CatalogMember) -> (Signature, Vec<String>) {
    let mut label = format!("{}(", m.name.to_lowercase());
    let mut ranges = Vec::new();
    for (i, p) in m.params.iter().enumerate() {
        if i > 0 {
            label.push_str(", ");
        }
        let default = if p.has_default { " := …" } else { "" };
        let text = format!(
            "{} {} {}{default}",
            p.name.to_lowercase(),
            p.mode.label(),
            p.data_type
        );
        ranges.push(label.len()..label.len() + text.len());
        label.push_str(&text);
    }
    label.push(')');
    if let Some(r) = &m.return_type {
        label.push_str(&format!(" return {r}"));
    }
    let names = m.params.iter().map(|p| p.name.clone()).collect();
    (
        Signature {
            label,
            params: ranges,
            doc: None,
        },
        names,
    )
}

/// Members named by `pkg.name`, `schema.pkg.name`, `proc`, `schema.proc`.
fn catalog_members(sema: &Sema, names: &[&str]) -> Vec<CatalogMember> {
    let standalone = |schema: Option<&str>, name: &str| -> Vec<CatalogMember> {
        match sema.cat.resolve(schema, name) {
            Some(Some(o)) if matches!(o.object_type.as_str(), "PROCEDURE" | "FUNCTION") => {
                sema.cat.members(&o.owner, &o.name).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    };
    let members_named = |o: &crate::catalog::CatalogObject, name: &str| -> Vec<CatalogMember> {
        sema.cat
            .members(&o.owner, &o.name)
            .unwrap_or_default()
            .into_iter()
            .filter(|m| m.name == name)
            .collect()
    };
    match names {
        [name] => standalone(None, name),
        [first, name] => match sema.cat.resolve(None, first) {
            Some(Some(o)) if matches!(o.object_type.as_str(), "PACKAGE" | "TYPE") => {
                members_named(&o, name)
            }
            // Not an object: `schema.procedure`.
            Some(None) => standalone(Some(first), name),
            _ => Vec::new(),
        },
        [schema, object, name] => match sema.cat.resolve(Some(schema), object) {
            Some(Some(o)) if matches!(o.object_type.as_str(), "PACKAGE" | "TYPE") => {
                members_named(&o, name)
            }
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

// ----- hover ----------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hover {
    /// The name the hover is about.
    pub range: Range<usize>,
    pub title: String,
    /// Plain text, possibly multiple lines (declaration, type, comment).
    pub detail: String,
    pub missing: Vec<CatalogRequest>,
}

pub(crate) fn hover(sema: &Sema, offset: usize) -> Option<Hover> {
    let (resolved, range) = sema.resolve_at(offset)?;
    let (title, detail) = describe(sema, &resolved);
    Some(Hover {
        range,
        title,
        detail,
        missing: Vec::new(),
    })
}

fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Variable => "variable",
        SymbolKind::Constant => "constant",
        SymbolKind::Parameter(_) => "parameter",
        SymbolKind::Cursor => "cursor",
        SymbolKind::Type => "type",
        SymbolKind::Subtype => "subtype",
        SymbolKind::Exception => "exception",
        SymbolKind::Procedure => "procedure",
        SymbolKind::Function => "function",
        SymbolKind::LoopIndex => "loop index",
        SymbolKind::Attribute => "attribute",
        SymbolKind::Label => "label",
    }
}

/// The declaration text, shortened to its first lines.
fn declaration_text(sema: &Sema, s: &Symbol) -> String {
    let end = s.body.map_or(s.span.end, |b| {
        // Only the header of a subprogram with a body.
        sema.ast
            .scope(b)
            .decls
            .as_ref()
            .map_or(s.span.end, |d| d.start)
    });
    let text = sema.text(&(s.span.start..end.max(s.span.start).min(sema.src.len())));
    let lines: Vec<&str> = text.lines().take(8).collect();
    let mut out = lines.join("\n");
    if text.lines().count() > 8 {
        out.push_str("\n…");
    }
    out.trim_end().trim_end_matches(';').trim_end().to_owned()
}

fn describe(sema: &Sema, resolved: &Resolved) -> (String, String) {
    match resolved {
        Resolved::Local(r) => {
            let s = sema.symbol(*r);
            let title = format!("{} {}", kind_label(s.kind), sema.text(&s.name.span));
            let mut detail = declaration_text(sema, s);
            if s.kind == SymbolKind::LoopIndex {
                if let Some(q) = s.query {
                    let cols: Vec<String> = sema
                        .query_columns(q, 0)
                        .into_iter()
                        .map(|f| f.name.to_lowercase())
                        .collect();
                    detail = format!("record ({})", cols.join(", "));
                } else if let Some(t) = &s.ty {
                    detail = t.text.clone();
                }
            }
            (title, detail)
        }
        Resolved::Source { query, index } => {
            let source = sema.source(*query, *index);
            let alias = source
                .exposed_name()
                .map_or(String::new(), |n| sema.text(&n.span).to_owned());
            let target = match &source.kind {
                SourceKind::Table(path) => path
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join("."),
                SourceKind::Subquery(_) => "inline view".into(),
                SourceKind::Function => "table function".into(),
            };
            let cols: Vec<String> = sema
                .source_columns(*query, *index, 0)
                .into_iter()
                .map(|f| f.name.to_lowercase())
                .collect();
            let detail = if cols.is_empty() {
                target
            } else {
                format!("{target}\n({})", cols.join(", "))
            };
            (format!("alias {alias}"), detail)
        }
        Resolved::Cte { query, index } => {
            let cte = &sema.ast.query(*query).ctes[*index];
            let cols: Vec<String> = sema
                .cte_columns(*query, *index, 0)
                .into_iter()
                .map(|f| f.name.to_lowercase())
                .collect();
            (
                format!("WITH query {}", sema.text(&cte.name.span)),
                format!("({})", cols.join(", ")),
            )
        }
        Resolved::Field(f) => {
            let owner = f
                .object
                .as_ref()
                .map_or(String::new(), |o| format!("{}.{}.", o.owner, o.name));
            let title = format!(
                "{} {owner}{}",
                if f.object.is_some() {
                    "column"
                } else {
                    "field"
                },
                f.name
            );
            let mut detail = f.data_type.clone();
            if f.nullable == Some(false) {
                detail.push_str(" NOT NULL");
            }
            if let Some(c) = &f.comment {
                detail.push_str(&format!("\n{c}"));
            }
            (title, detail)
        }
        Resolved::Object(o) => (
            format!("{} {}.{}", o.object_type.to_lowercase(), o.owner, o.name),
            String::new(),
        ),
        Resolved::Member {
            object,
            name,
            members,
        } => {
            let kind = match members.first().map(|m| m.kind) {
                Some(crate::catalog::MemberKind::Function) => "function",
                _ => "procedure",
            };
            let lines: Vec<String> = members
                .iter()
                .map(|m| format!("{}{}", name.to_lowercase(), member_signature(m)))
                .collect();
            (
                format!("{kind} {}.{}.{}", object.owner, object.name, name),
                lines.join("\n"),
            )
        }
        Resolved::LocalMember { package, name } => {
            let scope = sema.ast.scope(*package);
            let pkg = scope.name.as_ref().map_or("", |n| n.name.as_str());
            let mut title = format!("{pkg}.{name}");
            let mut lines = Vec::new();
            for s in scope.symbols.iter().filter(|s| &s.name.name == name) {
                title = format!("{} {pkg}.{}", kind_label(s.kind), sema.text(&s.name.span));
                lines.push(declaration_text(sema, s));
            }
            (title, lines.join("\n"))
        }
        Resolved::Builtin(b) => (
            format!("function {}", b.name),
            format!("{}\n{}", b.signature(), b.doc),
        ),
        Resolved::Schema(s) => (format!("schema {s}"), String::new()),
    }
}

// ----- definition -----------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Definition {
    /// Jump within the same source: the declaration's name.
    Local(Range<usize>),
    /// A database object (optionally a member: column, subprogram) for the app
    /// to open.
    Object {
        owner: String,
        name: String,
        object_type: String,
        member: Option<String>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DefinitionResult {
    pub target: Option<Definition>,
    pub missing: Vec<CatalogRequest>,
}

pub(crate) fn definition(sema: &Sema, offset: usize) -> Option<Definition> {
    let (resolved, span) = sema.resolve_at(offset)?;
    match resolved {
        Resolved::Local(r) => {
            let s = sema.symbol(r);
            let on_declaration = s.name.span == span;
            if s.kind.is_subprogram() || s.kind == SymbolKind::Cursor {
                if on_declaration {
                    return sema
                        .counterpart(r)
                        .map(|c| Definition::Local(sema.symbol(c).name.span.clone()));
                }
                if s.forward
                    && let Some(c) = sema.counterpart(r)
                {
                    return Some(Definition::Local(sema.symbol(c).name.span.clone()));
                }
            }
            if on_declaration {
                return None;
            }
            Some(Definition::Local(s.name.span.clone()))
        }
        Resolved::Source { query, index } => {
            let source = sema.source(query, index);
            let target = source
                .alias
                .as_ref()
                .map(|a| a.span.clone())
                .unwrap_or_else(|| source.span.clone());
            (target != span).then_some(Definition::Local(target))
        }
        Resolved::Cte { query, index } => {
            let target = sema.ast.query(query).ctes[index].name.span.clone();
            (target != span).then_some(Definition::Local(target))
        }
        Resolved::Field(f) => {
            if let Some(decl) = f.decl.filter(|d| *d != span) {
                return Some(Definition::Local(decl));
            }
            f.object.map(|o| Definition::Object {
                owner: o.owner,
                name: o.name,
                object_type: o.object_type,
                member: Some(f.name),
            })
        }
        Resolved::Object(o) => Some(Definition::Object {
            owner: o.owner,
            name: o.name,
            object_type: o.object_type,
            member: None,
        }),
        Resolved::Member { object, name, .. } => Some(Definition::Object {
            owner: object.owner,
            name: object.name,
            object_type: object.object_type,
            member: Some(name),
        }),
        Resolved::LocalMember { package, name } => {
            let scope = sema.ast.scope(package);
            let r = scope
                .symbols
                .iter()
                .position(|s| s.name.name == name)
                .map(|index| SymRef {
                    scope: package,
                    index,
                })?;
            // Prefer the implementation in the body.
            let target = if sema.symbol(r).forward {
                sema.counterpart(r).unwrap_or(r)
            } else {
                r
            };
            Some(Definition::Local(sema.symbol(target).name.span.clone()))
        }
        Resolved::Builtin(_) | Resolved::Schema(_) => None,
    }
}

// ----- references -------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    Symbol(SymRef),
    Source(QueryId, usize),
    Cte(QueryId, usize),
}

fn identity(sema: &Sema, resolved: &Resolved) -> Option<Identity> {
    match resolved {
        Resolved::Local(r) => {
            let s = sema.symbol(*r);
            // Spec declaration and body implementation are the same symbol;
            // use the forward (spec) side as the canonical one.
            if (s.kind.is_subprogram() || s.kind == SymbolKind::Cursor)
                && !s.forward
                && let Some(c) = sema.counterpart(*r)
            {
                return Some(Identity::Symbol(c));
            }
            Some(Identity::Symbol(*r))
        }
        Resolved::LocalMember { package, name } => {
            let scope = sema.ast.scope(*package);
            let index = scope.symbols.iter().position(|s| &s.name.name == name)?;
            identity(
                sema,
                &Resolved::Local(SymRef {
                    scope: *package,
                    index,
                }),
            )
        }
        Resolved::Source { query, index } => Some(Identity::Source(*query, *index)),
        Resolved::Cte { query, index } => Some(Identity::Cte(*query, *index)),
        _ => None,
    }
}

pub(crate) fn references(analysis: &crate::Analysis, offset: usize) -> Vec<Range<usize>> {
    let empty = EmptyCatalog;
    let tracker = Tracker::new(&empty);
    let sema = analysis.sema(&tracker);
    let Some((resolved, _)) = sema.resolve_at(offset) else {
        return Vec::new();
    };
    let Some(target) = identity(&sema, &resolved) else {
        return Vec::new();
    };
    let (name, search) = match target {
        Identity::Symbol(r) => {
            let s = sema.symbol(r);
            let scope = sema.ast.scope(r.scope);
            let search = match scope.kind {
                ScopeKind::Root
                | ScopeKind::PackageSpec
                | ScopeKind::PackageBody
                | ScopeKind::TypeSpec
                | ScopeKind::TypeBody => 0..sema.src.len(),
                _ => scope.span.clone(),
            };
            (s.name.name.clone(), search)
        }
        Identity::Source(q, i) => {
            let name = sema
                .source(q, i)
                .exposed_name()
                .map(|n| n.name.clone())
                .unwrap_or_default();
            (name, sema.ast.query(q).span.clone())
        }
        Identity::Cte(q, i) => {
            let name = sema.ast.query(q).ctes[i].name.name.clone();
            let span = sema
                .ast
                .statement_at(sema.ast.query(q).span.start)
                .map_or(sema.ast.query(q).span.clone(), |s| s.span.clone());
            (name, span)
        }
    };
    let mut out = Vec::new();
    for t in sema.tokens {
        if t.span.start < search.start
            || t.span.end > search.end
            || !is_ref_token(t.kind)
            || t.kind == TokenKind::BindVar
        {
            continue;
        }
        if orui_plsql::normalize_ident(sema.text(&t.span)).0 != name {
            continue;
        }
        if let Some((r, span)) = sema.resolve_at(t.span.start)
            && span == t.span
            && identity(&sema, &r) == Some(target)
        {
            out.push(t.span.clone());
        }
    }
    out
}
