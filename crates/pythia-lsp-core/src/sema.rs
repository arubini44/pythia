//! Semantic model on top of the syntax tree: scopes, symbol lookup, types,
//! query columns and name resolution.

use pythia_plsql::ast::*;
use pythia_plsql::{Token, TokenKind, normalize_ident};

use crate::builtins::{self, Builtin};
use crate::catalog::{CatalogMember, CatalogObject, Tracker};

/// A symbol: the scope that declares it and its index in that scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SymRef {
    pub scope: ScopeId,
    pub index: usize,
}

/// A column, record field or attribute.
#[derive(Debug, Clone)]
pub(crate) struct FieldInfo {
    pub name: String,
    pub data_type: String,
    pub comment: Option<String>,
    pub nullable: Option<bool>,
    /// Declaration in the source (record fields, select items).
    pub decl: Option<Span>,
    /// Type reference for nested lookups (local fields only).
    pub ty: Option<TypeRef>,
    /// Table / view it belongs to, if it comes from the catalog.
    pub object: Option<CatalogObject>,
}

impl FieldInfo {
    fn named(name: String) -> Self {
        Self {
            name,
            data_type: String::new(),
            comment: None,
            nullable: None,
            decl: None,
            ty: None,
            object: None,
        }
    }
}

/// One part of a dotted reference like `a.b(1).c`.
#[derive(Debug, Clone)]
pub(crate) struct Part {
    pub ident: Ident,
    /// Followed by `(...)`: a call or collection index.
    pub indexed: bool,
    /// A bind variable such as `:new`.
    pub bind: bool,
}

/// What a dotted prefix (`x.` / `x.y.`) refers to, for member completion.
pub(crate) enum MemberTarget {
    Fields(Vec<FieldInfo>),
    Members {
        object: CatalogObject,
        members: Vec<CatalogMember>,
        attributes: Vec<FieldInfo>,
    },
    LocalPackage(ScopeId),
    Objects {
        schema: String,
        objects: Vec<CatalogObject>,
    },
    Sequence,
    Nothing,
}

/// What a name at a position refers to (hover, definition, references).
#[derive(Debug, Clone)]
pub(crate) enum Resolved {
    Local(SymRef),
    /// A table alias / source in a query.
    Source {
        query: QueryId,
        index: usize,
    },
    Cte {
        query: QueryId,
        index: usize,
    },
    Field(FieldInfo),
    Object(CatalogObject),
    Member {
        object: CatalogObject,
        name: String,
        members: Vec<CatalogMember>,
    },
    /// Member of a package declared in the same file.
    LocalMember {
        package: ScopeId,
        name: String,
    },
    /// A formal parameter in named notation (`p_id =>`) of a subprogram in
    /// this text.
    Parameter {
        subprogram: SymRef,
        index: usize,
    },
    /// A formal parameter in named notation of a subprogram in the
    /// database: `member` of `object`, or the object itself when it's a
    /// standalone procedure or function.
    CatalogParameter {
        object: CatalogObject,
        member: Option<String>,
        param: crate::catalog::CatalogParam,
    },
    Builtin(&'static Builtin),
    Schema(String),
}

pub(crate) struct Sema<'a> {
    pub src: &'a str,
    pub tokens: &'a [Token],
    pub ast: &'a Ast,
    pub cat: &'a Tracker<'a>,
}

const MAX_DEPTH: usize = 6;

impl<'a> Sema<'a> {
    pub fn symbol(&self, r: SymRef) -> &'a Symbol {
        &self.ast.scopes[r.scope.0].symbols[r.index]
    }

    pub fn text(&self, span: &Span) -> &'a str {
        &self.src[span.clone()]
    }

    // ----- scopes & symbols -----------------------------------------------

    /// Scopes whose symbols are visible at `offset`, innermost first. Package
    /// and type bodies also see their spec.
    pub fn visible_scopes(&self, offset: usize) -> Vec<ScopeId> {
        let mut out = Vec::new();
        for id in self.ast.ancestors(self.ast.scope_at(offset)) {
            out.push(id);
            let scope = self.ast.scope(id);
            let spec_kind = match scope.kind {
                ScopeKind::PackageBody => Some(ScopeKind::PackageSpec),
                ScopeKind::TypeBody => Some(ScopeKind::TypeSpec),
                _ => None,
            };
            if let (Some(kind), Some(name)) = (spec_kind, &scope.name)
                && let Some(spec) = self.unit(kind, &name.name)
            {
                out.push(spec);
            }
        }
        out
    }

    /// Visible symbols, innermost first.
    pub fn visible_symbols(&self, offset: usize) -> Vec<SymRef> {
        let mut out = Vec::new();
        for scope in self.visible_scopes(offset) {
            for index in 0..self.ast.scope(scope).symbols.len() {
                out.push(SymRef { scope, index });
            }
        }
        out
    }

    /// First visible symbol named `name`.
    pub fn find_local(&self, offset: usize, name: &str) -> Option<SymRef> {
        self.visible_symbols(offset).into_iter().find(|r| {
            self.symbol(*r).name.name == name && self.symbol(*r).kind != SymbolKind::Label
        })
    }

    /// All visible subprograms named `name` (overloads), innermost scope first.
    pub fn find_subprograms(&self, offset: usize, name: &str) -> Vec<SymRef> {
        let mut out: Vec<SymRef> = Vec::new();
        for r in self.visible_symbols(offset) {
            let s = self.symbol(r);
            if s.kind.is_subprogram() && s.name.name == name {
                // Skip a spec declaration if the body version is already listed.
                if out.iter().any(|o| same_signature(self.symbol(*o), s)) {
                    continue;
                }
                out.push(r);
            }
        }
        out
    }

    /// Top-level unit of a kind with a name (`PackageSpec`, ...).
    pub fn unit(&self, kind: ScopeKind, name: &str) -> Option<ScopeId> {
        self.ast
            .scope(Ast::ROOT)
            .children
            .iter()
            .copied()
            .find(|c| {
                self.ast.scope(*c).kind == kind
                    && self
                        .ast
                        .scope(*c)
                        .name
                        .as_ref()
                        .is_some_and(|n| n.name == name)
            })
    }

    /// A package declared in this file: its spec, else its body.
    pub fn local_package(&self, name: &str) -> Option<ScopeId> {
        self.unit(ScopeKind::PackageSpec, name)
            .or_else(|| self.unit(ScopeKind::PackageBody, name))
    }

    /// The counterpart of a subprogram declaration: spec ↔ body, forward
    /// declaration ↔ implementation.
    pub fn counterpart(&self, r: SymRef) -> Option<SymRef> {
        let sym = self.symbol(r);
        let scope = self.ast.scope(r.scope);
        let mut candidates = vec![r.scope];
        if let Some(name) = &scope.name {
            let other = match scope.kind {
                ScopeKind::PackageSpec => self.unit(ScopeKind::PackageBody, &name.name),
                ScopeKind::PackageBody => self.unit(ScopeKind::PackageSpec, &name.name),
                ScopeKind::TypeSpec => self.unit(ScopeKind::TypeBody, &name.name),
                ScopeKind::TypeBody => self.unit(ScopeKind::TypeSpec, &name.name),
                _ => None,
            };
            candidates.extend(other);
        }
        let mut best = None;
        for scope_id in candidates {
            for (index, other) in self.ast.scope(scope_id).symbols.iter().enumerate() {
                let o = SymRef {
                    scope: scope_id,
                    index,
                };
                if o == r
                    || other.kind != sym.kind
                    || other.name.name != sym.name.name
                    || other.forward == sym.forward
                {
                    continue;
                }
                if same_signature(other, sym) {
                    return Some(o);
                }
                best.get_or_insert(o);
            }
        }
        best
    }

    /// The trigger table visible at `offset` (for `:new` / `:old`).
    pub fn trigger_table(&self, offset: usize) -> Option<&'a [Ident]> {
        self.ast
            .ancestors(self.ast.scope_at(offset))
            .find_map(|s| self.ast.scope(s).trigger_table.as_deref())
    }

    // ----- types ----------------------------------------------------------

    /// Fields of a record-like symbol (row type, record type, cursor loop index).
    pub fn symbol_fields(&self, r: SymRef, depth: usize) -> Option<Vec<FieldInfo>> {
        if depth > MAX_DEPTH {
            return None;
        }
        let sym = self.symbol(r);
        if let Some(q) = sym.query
            && matches!(sym.kind, SymbolKind::LoopIndex)
        {
            return Some(self.query_columns(q, depth + 1));
        }
        match sym.kind {
            SymbolKind::Type => match &sym.type_def {
                Some(TypeDef::Record(fields)) | Some(TypeDef::Object(fields)) => {
                    Some(self.local_fields(fields))
                }
                _ => None,
            },
            SymbolKind::Subtype => self.type_fields(sym.ty.as_ref()?, sym.span.start, depth + 1),
            _ => self.type_fields(sym.ty.as_ref()?, sym.span.start, depth + 1),
        }
    }

    fn local_fields(&self, fields: &[Field]) -> Vec<FieldInfo> {
        fields
            .iter()
            .map(|f| FieldInfo {
                name: f.name.name.clone(),
                data_type: f.ty.as_ref().map(|t| t.text.clone()).unwrap_or_default(),
                comment: None,
                nullable: None,
                decl: Some(f.name.span.clone()),
                ty: f.ty.clone(),
                object: None,
            })
            .collect()
    }

    /// Fields of a type as written (`t%ROWTYPE`, `c%ROWTYPE`, `t_rec`, `v%TYPE`).
    pub fn type_fields(&self, ty: &TypeRef, at: usize, depth: usize) -> Option<Vec<FieldInfo>> {
        if depth > MAX_DEPTH || ty.path.is_empty() {
            return None;
        }
        let first = &ty.path[0].name;
        match ty.attr {
            TypeAttr::RowType => {
                if ty.path.len() == 1
                    && let Some(r) = self.find_local(at, first)
                {
                    let sym = self.symbol(r);
                    if sym.kind == SymbolKind::Cursor {
                        if let Some(q) = sym.query {
                            return Some(self.query_columns(q, depth + 1));
                        }
                        return sym
                            .ty
                            .as_ref()
                            .and_then(|t| self.type_fields(t, at, depth + 1));
                    }
                }
                self.table_fields(&ty.path).map(|(_, f)| f)
            }
            TypeAttr::Type => {
                if ty.path.len() == 1 {
                    let r = self.find_local(at, first)?;
                    return self.symbol_fields(r, depth + 1);
                }
                None
            }
            TypeAttr::None => {
                if ty.path.len() == 1 {
                    if let Some(r) = self.find_local(at, first) {
                        return self.symbol_fields(r, depth + 1);
                    }
                    if builtins::DATA_TYPES.iter().any(|t| t == first) || is_scalar(first) {
                        return None;
                    }
                    // Object type from the catalog: attributes.
                    let obj = self.cat.resolve(None, first)??;
                    if obj.object_type == "TYPE" {
                        return Some(self.catalog_columns(&obj));
                    }
                    None
                } else if ty.path.len() == 2 {
                    // pkg.t_rec of a package in this file.
                    let pkg = self.local_package(first)?;
                    let idx = self
                        .ast
                        .scope(pkg)
                        .symbols
                        .iter()
                        .position(|s| s.name.name == ty.path[1].name)?;
                    self.symbol_fields(
                        SymRef {
                            scope: pkg,
                            index: idx,
                        },
                        depth + 1,
                    )
                } else {
                    None
                }
            }
        }
    }

    /// Fields of the element of a collection-typed symbol (`v(i).`).
    pub fn element_fields(&self, r: SymRef, depth: usize) -> Option<Vec<FieldInfo>> {
        let sym = self.symbol(r);
        let ty = sym.ty.as_ref()?;
        if ty.path.len() != 1 || ty.attr != TypeAttr::None {
            return None;
        }
        let type_sym = self.find_local(sym.span.start, &ty.path[0].name)?;
        match &self.symbol(type_sym).type_def {
            Some(TypeDef::Collection(Some(elem))) => {
                self.type_fields(elem, sym.span.start, depth + 1)
            }
            _ => None,
        }
    }

    /// Columns of `[schema.]table` from the catalog.
    pub fn table_fields(&self, path: &[Ident]) -> Option<(CatalogObject, Vec<FieldInfo>)> {
        let obj = match path {
            [table] => self.cat.resolve(None, &table.name)??,
            [schema, table] => self.cat.resolve(Some(&schema.name), &table.name)??,
            _ => return None,
        };
        let fields = self.catalog_columns(&obj);
        Some((obj, fields))
    }

    pub fn catalog_columns(&self, obj: &CatalogObject) -> Vec<FieldInfo> {
        self.cat
            .columns(&obj.owner, &obj.name)
            .unwrap_or_default()
            .into_iter()
            .map(|c| FieldInfo {
                name: c.name,
                data_type: c.data_type,
                comment: c.comment,
                nullable: Some(c.nullable),
                decl: None,
                ty: None,
                object: Some(obj.clone()),
            })
            .collect()
    }

    // ----- queries --------------------------------------------------------

    /// The first block of the set operation `q` belongs to.
    fn chain_head(&self, q: QueryId) -> QueryId {
        let mut head = q;
        while let Some(prev) = self.ast.queries.iter().position(|x| x.next == Some(head)) {
            head = QueryId(prev);
        }
        head
    }

    /// `q` and its enclosing queries (via set-operation heads).
    pub fn query_chain(&self, q: QueryId) -> Vec<QueryId> {
        let mut out = Vec::new();
        let mut current = Some(q);
        while let Some(id) = current {
            if out.contains(&id) || out.len() > 64 {
                break;
            }
            out.push(id);
            let head = self.chain_head(id);
            if head != id && !out.contains(&head) {
                out.push(head);
            }
            current = self.ast.query(head).parent.or(self.ast.query(id).parent);
        }
        out
    }

    /// Sources visible in `q`: its own first, then enclosing queries' (for
    /// correlated subqueries). Other blocks of a `UNION` are not visible.
    pub fn visible_sources(&self, q: QueryId) -> Vec<(QueryId, usize)> {
        let mut out = Vec::new();
        let mut current = Some(q);
        let mut guard = 0;
        while let Some(id) = current {
            for index in 0..self.ast.query(id).sources.len() {
                out.push((id, index));
            }
            current = self.ast.query(id).parent;
            guard += 1;
            if guard > 64 {
                break;
            }
        }
        out
    }

    pub fn source(&self, q: QueryId, index: usize) -> &'a Source {
        &self.ast.queries[q.0].sources[index]
    }

    /// Source whose alias (or table name) is `name`.
    pub fn source_by_name(&self, q: QueryId, name: &str) -> Option<(QueryId, usize)> {
        self.visible_sources(q).into_iter().find(|(qid, i)| {
            self.source(*qid, *i)
                .exposed_name()
                .is_some_and(|n| n.name == name)
        })
    }

    /// A CTE visible from `q`.
    pub fn cte(&self, q: QueryId, name: &str) -> Option<(QueryId, usize)> {
        for id in self.query_chain(q) {
            if let Some(i) = self
                .ast
                .query(id)
                .ctes
                .iter()
                .position(|c| c.name.name == name)
            {
                return Some((id, i));
            }
        }
        None
    }

    pub fn source_columns(&self, q: QueryId, index: usize, depth: usize) -> Vec<FieldInfo> {
        if depth > MAX_DEPTH {
            return Vec::new();
        }
        let source = self.source(q, index);
        match &source.kind {
            SourceKind::Table(path) => {
                if path.len() == 1
                    && let Some((cq, ci)) = self.cte(q, &path[0].name)
                {
                    return self.cte_columns(cq, ci, depth + 1);
                }
                self.table_fields(path).map(|(_, f)| f).unwrap_or_default()
            }
            SourceKind::Subquery(sub) => self.query_columns(*sub, depth + 1),
            SourceKind::Function => Vec::new(),
        }
    }

    pub fn cte_columns(&self, q: QueryId, index: usize, depth: usize) -> Vec<FieldInfo> {
        let cte = &self.ast.query(q).ctes[index];
        if !cte.columns.is_empty() {
            return cte
                .columns
                .iter()
                .map(|c| FieldInfo {
                    decl: Some(c.span.clone()),
                    ..FieldInfo::named(c.name.clone())
                })
                .collect();
        }
        cte.query
            .map(|sub| self.query_columns(sub, depth + 1))
            .unwrap_or_default()
    }

    /// Columns a query block produces (select list, `*` expanded).
    pub fn query_columns(&self, q: QueryId, depth: usize) -> Vec<FieldInfo> {
        if depth > MAX_DEPTH {
            return Vec::new();
        }
        let query = self.ast.query(q);
        let mut out = Vec::new();
        for item in &query.items {
            match (&item.star, &item.name) {
                (Some(qualifier), _) if qualifier.is_empty() => {
                    for i in 0..query.sources.len() {
                        out.extend(self.source_columns(q, i, depth + 1));
                    }
                }
                (Some(qualifier), _) => {
                    let name = &qualifier[qualifier.len() - 1].name;
                    if let Some(i) = query
                        .sources
                        .iter()
                        .position(|s| s.exposed_name().is_some_and(|n| &n.name == name))
                    {
                        out.extend(self.source_columns(q, i, depth + 1));
                    }
                }
                (None, Some(name)) => {
                    let mut field = FieldInfo {
                        decl: Some(name.span.clone()),
                        ..FieldInfo::named(name.name.clone())
                    };
                    // A plain column reference keeps the column's type.
                    if let Some(col) = self.item_column(q, item, depth) {
                        field.data_type = col.data_type;
                        field.comment = col.comment;
                        field.nullable = col.nullable;
                    }
                    out.push(field);
                }
                (None, None) => {}
            }
        }
        out
    }

    /// The column a select item like `e.salary` or `salary` refers to.
    fn item_column(&self, q: QueryId, item: &SelectItem, depth: usize) -> Option<FieldInfo> {
        let text = self.text(&item.span);
        let parts: Vec<&str> = text.split('.').map(str::trim).collect();
        if parts.iter().any(|p| {
            p.is_empty() || p.contains(|c: char| c.is_whitespace() || "()+-*/|,'".contains(c))
        }) {
            return None;
        }
        let column = normalize_ident(parts[parts.len() - 1]).0;
        let query = self.ast.query(q);
        let candidates: Vec<usize> = match parts.len() {
            1 => (0..query.sources.len()).collect(),
            2 => {
                let qual = normalize_ident(parts[0]).0;
                query
                    .sources
                    .iter()
                    .position(|s| s.exposed_name().is_some_and(|n| n.name == qual))
                    .into_iter()
                    .collect()
            }
            _ => return None,
        };
        for i in candidates {
            if let Some(f) = self
                .source_columns(q, i, depth + 1)
                .into_iter()
                .find(|f| f.name == column)
            {
                return Some(f);
            }
        }
        None
    }

    // ----- dotted references ----------------------------------------------

    /// What `parts.` refers to, for member completion.
    pub fn member_target(
        &self,
        parts: &[Part],
        offset: usize,
        query: Option<QueryId>,
    ) -> MemberTarget {
        let Some(first) = parts.first() else {
            return MemberTarget::Nothing;
        };
        let name = &first.ident.name;

        if first.bind {
            let upper = name.trim_start_matches(':').to_uppercase();
            if matches!(upper.as_str(), "NEW" | "OLD" | "PARENT")
                && parts.len() == 1
                && let Some(table) = self.trigger_table(offset)
            {
                return MemberTarget::Fields(
                    self.table_fields(table).map(|(_, f)| f).unwrap_or_default(),
                );
            }
            return MemberTarget::Nothing;
        }

        if let Some(q) = query
            && !first.indexed
            && let Some((qid, idx)) = self.source_by_name(q, name)
            // `from scott.|` parses `scott` as a table being typed: not an alias.
            && !matches!(&self.source(qid, idx).kind, SourceKind::Table(p) if p.iter().any(|x| x.span == first.ident.span))
        {
            return if parts.len() == 1 {
                MemberTarget::Fields(self.source_columns(qid, idx, 0))
            } else {
                MemberTarget::Nothing
            };
        }

        if let Some(r) = self.find_local(offset, name) {
            let sym = self.symbol(r);
            if !sym.kind.is_subprogram() && sym.kind != SymbolKind::Label {
                let mut fields = if first.indexed {
                    self.element_fields(r, 0)
                } else {
                    self.symbol_fields(r, 0)
                };
                for part in &parts[1..] {
                    let Some(current) = fields else {
                        return MemberTarget::Nothing;
                    };
                    let Some(field) = current.into_iter().find(|f| f.name == part.ident.name)
                    else {
                        return MemberTarget::Nothing;
                    };
                    fields = field
                        .ty
                        .as_ref()
                        .and_then(|t| self.type_fields(t, sym.span.start, 1));
                }
                return fields.map_or(MemberTarget::Nothing, MemberTarget::Fields);
            }
        }

        if parts.len() == 1
            && let Some(pkg) = self.local_package(name)
        {
            return MemberTarget::LocalPackage(pkg);
        }

        // Catalog: package / type / table / sequence / schema.
        let (schema, object_name) = match parts {
            [p] => (None, &p.ident.name),
            [s, p] => (Some(s.ident.name.as_str()), &p.ident.name),
            _ => return MemberTarget::Nothing,
        };
        match self.cat.resolve(schema, object_name) {
            Some(Some(obj)) => self.object_target(obj),
            Some(None) if schema.is_none() => MemberTarget::Objects {
                schema: name.clone(),
                objects: self.cat.objects(Some(name)).unwrap_or_default(),
            },
            Some(None) => MemberTarget::Nothing,
            None => {
                // Unknown yet; offer schema objects if it's a known schema.
                if schema.is_none() && self.cat.schemas().is_some_and(|s| s.contains(name)) {
                    MemberTarget::Objects {
                        schema: name.clone(),
                        objects: self.cat.objects(Some(name)).unwrap_or_default(),
                    }
                } else {
                    MemberTarget::Nothing
                }
            }
        }
    }

    fn object_target(&self, obj: CatalogObject) -> MemberTarget {
        match obj.object_type.as_str() {
            "PACKAGE" | "PACKAGE BODY" => {
                let members = self.cat.members(&obj.owner, &obj.name).unwrap_or_default();
                MemberTarget::Members {
                    object: obj,
                    members,
                    attributes: Vec::new(),
                }
            }
            "TYPE" => {
                let members = self.cat.members(&obj.owner, &obj.name).unwrap_or_default();
                let attributes = self.catalog_columns(&obj);
                MemberTarget::Members {
                    object: obj,
                    members,
                    attributes,
                }
            }
            "TABLE" | "VIEW" | "MATERIALIZED VIEW" => {
                MemberTarget::Fields(self.catalog_columns(&obj))
            }
            "SEQUENCE" => MemberTarget::Sequence,
            _ => MemberTarget::Nothing,
        }
    }

    // ----- name at a position ---------------------------------------------

    /// Index of the token at `offset` (inside it, or ending right at it).
    pub fn token_at(&self, offset: usize) -> Option<usize> {
        // First token ending at or after the offset: the one the cursor is
        // in, or the one it sits right after.
        let i = self.tokens.partition_point(|t| t.span.end < offset);
        let t = self.tokens.get(i)?;
        if t.span.start <= offset && is_ref_token(t.kind) {
            return Some(i);
        }
        // Cursor right before a word.
        let next = self.tokens.get(i + 1)?;
        (t.span.end == offset && next.span.start == offset && is_ref_token(next.kind))
            .then_some(i + 1)
    }

    fn prev_significant(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&j| !self.tokens[j].kind.is_trivia())
    }

    fn next_significant(&self, i: usize) -> Option<usize> {
        (i + 1..self.tokens.len()).find(|&j| !self.tokens[j].kind.is_trivia())
    }

    /// The dotted chain ending with token `i`: `a.b(1).c` → [a, b(), c].
    pub fn chain_ending_at(&self, i: usize) -> Vec<Part> {
        let mut parts = vec![self.part(i, false)];
        let mut j = i;
        while let Some(dot) = self.prev_significant(j) {
            if self.tokens[dot].kind != TokenKind::Dot {
                break;
            }
            let Some(mut k) = self.prev_significant(dot) else {
                break;
            };
            let mut indexed = false;
            if self.text(&self.tokens[k].span) == ")" {
                // Skip back over a balanced group.
                let mut depth = 0usize;
                let mut m = k;
                loop {
                    match self.text(&self.tokens[m].span) {
                        ")" => depth += 1,
                        "(" => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    if m == 0 {
                        return parts;
                    }
                    m -= 1;
                }
                let Some(before) = self.prev_significant(m) else {
                    break;
                };
                k = before;
                indexed = true;
            }
            if !is_ref_token(self.tokens[k].kind) {
                break;
            }
            parts.insert(0, self.part(k, indexed));
            j = k;
        }
        parts
    }

    fn part(&self, i: usize, indexed: bool) -> Part {
        let t = &self.tokens[i];
        let text = self.text(&t.span);
        let bind = t.kind == TokenKind::BindVar;
        let (name, quoted) = if bind {
            (text.to_uppercase(), false)
        } else {
            normalize_ident(text)
        };
        Part {
            ident: Ident {
                name,
                span: t.span.clone(),
                quoted,
            },
            indexed,
            bind,
        }
    }

    /// Resolves the reference at `offset`. Returns the resolution and the
    /// span of the name token.
    pub fn resolve_at(&self, offset: usize) -> Option<(Resolved, Span)> {
        let i = self.token_at(offset)?;
        let token = &self.tokens[i];
        let span = token.span.clone();
        let parts = self.chain_ending_at(i);
        let last = parts.last()?.clone();
        let called = self
            .next_significant(i)
            .is_some_and(|n| self.text(&self.tokens[n].span) == "(");
        let followed_by_dot = self
            .next_significant(i)
            .is_some_and(|n| self.tokens[n].kind == TokenKind::Dot);
        let query = self.ast.query_at(offset);

        // A declaration name resolves to its symbol.
        for (scope_idx, scope) in self.ast.scopes.iter().enumerate() {
            if scope.span.start > span.start || scope.span.end < span.end {
                continue;
            }
            if let Some(index) = scope.symbols.iter().position(|s| s.name.span == span) {
                return Some((
                    Resolved::Local(SymRef {
                        scope: ScopeId(scope_idx),
                        index,
                    }),
                    span,
                ));
            }
        }

        if parts.len() == 1 {
            let name = &last.ident.name;
            if last.bind {
                return None;
            }
            let named_argument = self
                .next_significant(i)
                .is_some_and(|n| self.text(&self.tokens[n].span) == "=>");
            if named_argument && let Some(formal) = self.formal_parameter(i, offset, name) {
                return Some((formal, span));
            }
            if self.in_label_position(i)
                && let Some(r) = self.visible_symbols(offset).into_iter().find(|r| {
                    let s = self.symbol(*r);
                    s.kind == SymbolKind::Label && s.name.name == *name
                })
            {
                return Some((Resolved::Local(r), span));
            }
            if let Some(q) = query {
                // Alias / table name in FROM, or used as `alias.`.
                if let Some((qid, idx)) = self.source_by_name(q, name) {
                    let source = self.source(qid, idx);
                    let is_decl = source.alias.as_ref().is_some_and(|a| a.span == span)
                        || matches!(&source.kind, SourceKind::Table(p) if p.iter().any(|x| x.span == span));
                    if followed_by_dot || is_decl {
                        if let SourceKind::Table(path) = &source.kind
                            && path.iter().any(|x| x.span == span)
                        {
                            return self.resolve_table_path(q, path, &span);
                        }
                        return Some((
                            Resolved::Source {
                                query: qid,
                                index: idx,
                            },
                            span,
                        ));
                    }
                }
                // A table name in FROM without alias, or a CTE reference.
                for (qid, idx) in self.visible_sources(q) {
                    if let SourceKind::Table(path) = &self.source(qid, idx).kind
                        && path.iter().any(|x| x.span == span)
                    {
                        return self.resolve_table_path(q, path, &span);
                    }
                }
                if let Some((cq, ci)) = self.cte(q, name) {
                    return Some((
                        Resolved::Cte {
                            query: cq,
                            index: ci,
                        },
                        span,
                    ));
                }
                // Column of any source in scope (columns take precedence over
                // PL/SQL variables inside SQL).
                if !called {
                    for (qid, idx) in self.visible_sources(q) {
                        if let Some(f) = self
                            .source_columns(qid, idx, 0)
                            .into_iter()
                            .find(|f| &f.name == name)
                        {
                            return Some((Resolved::Field(f), span));
                        }
                    }
                }
            }
            if let Some(r) = self.find_local(offset, name) {
                // A call names the overload its arguments fit.
                let r = if self.symbol(r).kind.is_subprogram() {
                    self.pick_overload(&self.find_subprograms(offset, name), i)
                        .unwrap_or(r)
                } else {
                    r
                };
                return Some((Resolved::Local(r), span));
            }
            if let Some(b) = builtins::function(name) {
                return Some((Resolved::Builtin(b), span));
            }
            return match self.cat.resolve(None, name) {
                Some(Some(obj)) => Some((Resolved::Object(obj), span)),
                Some(None)
                    if followed_by_dot && self.cat.schemas().is_some_and(|s| s.contains(name)) =>
                {
                    Some((Resolved::Schema(name.clone()), span))
                }
                _ => None,
            };
        }

        // Qualified: resolve what the qualifier refers to, then the member.
        let qualifier = &parts[..parts.len() - 1];
        let name = last.ident.name.clone();
        let first = &qualifier[0];
        if qualifier.len() == 1
            && !first.bind
            && !first.indexed
            && let Some(pkg) = self.local_package(&first.ident.name)
            && self.find_local(offset, &first.ident.name).is_none()
        {
            return Some((Resolved::LocalMember { package: pkg, name }, span));
        }
        match self.member_target(qualifier, offset, query) {
            MemberTarget::Fields(fields) => fields
                .into_iter()
                .find(|f| f.name == name)
                .map(|f| (Resolved::Field(f), span)),
            MemberTarget::Members {
                object,
                members,
                attributes,
            } => {
                let matching: Vec<CatalogMember> =
                    members.into_iter().filter(|m| m.name == name).collect();
                if !matching.is_empty() {
                    return Some((
                        Resolved::Member {
                            object,
                            name,
                            members: matching,
                        },
                        span,
                    ));
                }
                attributes
                    .into_iter()
                    .find(|f| f.name == name)
                    .map(|f| (Resolved::Field(f), span))
            }
            MemberTarget::LocalPackage(pkg) => {
                Some((Resolved::LocalMember { package: pkg, name }, span))
            }
            MemberTarget::Objects { schema, .. } => match self.cat.resolve(Some(&schema), &name) {
                Some(Some(obj)) => Some((Resolved::Object(obj), span)),
                _ => None,
            },
            MemberTarget::Sequence | MemberTarget::Nothing => None,
        }
    }

    /// Whether token `i` is where a label is named: after `EXIT`,
    /// `CONTINUE`, `GOTO` or `END LOOP`.
    fn in_label_position(&self, i: usize) -> bool {
        let Some(prev) = self.prev_significant(i) else {
            return false;
        };
        match self
            .text(&self.tokens[prev].span)
            .to_ascii_uppercase()
            .as_str()
        {
            "EXIT" | "CONTINUE" | "GOTO" => true,
            "LOOP" => self
                .prev_significant(prev)
                .is_some_and(|p| self.text(&self.tokens[p].span).eq_ignore_ascii_case("end")),
            _ => false,
        }
    }

    /// Arguments of the call whose name is token `i`: the number of
    /// positional ones and the names of named ones. Without parentheses
    /// there are none.
    fn call_args(&self, i: usize) -> (usize, Vec<String>) {
        let Some(open) = self
            .next_significant(i)
            .filter(|&n| self.text(&self.tokens[n].span) == "(")
        else {
            return (0, Vec::new());
        };
        let (mut depth, mut positional, mut named) = (0usize, 0usize, Vec::new());
        let (mut has_content, mut is_named) = (false, false);
        for j in open + 1..self.tokens.len() {
            let t = &self.tokens[j];
            if t.kind.is_trivia() {
                continue;
            }
            match self.text(&t.span) {
                ")" if depth == 0 => break,
                ";" => break,
                "(" => {
                    depth += 1;
                    has_content = true;
                }
                ")" => depth -= 1,
                "," if depth == 0 => {
                    positional += usize::from(has_content && !is_named);
                    (has_content, is_named) = (false, false);
                }
                "=>" if depth == 0 => {
                    if let Some(p) = self.prev_significant(j) {
                        named.push(normalize_ident(self.text(&self.tokens[p].span)).0);
                    }
                    is_named = true;
                }
                _ => has_content = true,
            }
        }
        positional += usize::from(has_content && !is_named);
        (positional, named)
    }

    /// The overload the call at token `i` means: named arguments must be
    /// parameters, and the argument count must fit (counting defaults).
    /// Prefers the closest parameter count.
    pub fn pick_overload(&self, candidates: &[SymRef], i: usize) -> Option<SymRef> {
        if candidates.len() < 2 {
            return candidates.first().copied();
        }
        let (positional, named) = self.call_args(i);
        let given = positional + named.len();
        candidates
            .iter()
            .copied()
            .filter(|r| {
                let params = &self.symbol(*r).params;
                named
                    .iter()
                    .all(|n| params.iter().any(|p| &p.name.name == n))
                    && given <= params.len()
                    && params.iter().filter(|p| !p.has_default).count() <= given
            })
            .min_by_key(|r| self.symbol(*r).params.len() - given)
            .or(candidates.first().copied())
    }

    /// The token naming the call whose argument list contains token `i`.
    fn enclosing_callee(&self, i: usize) -> Option<usize> {
        let mut depth = 0usize;
        let mut j = i;
        while let Some(k) = self.prev_significant(j) {
            match self.text(&self.tokens[k].span) {
                ")" => depth += 1,
                "(" if depth == 0 => {
                    let callee = self.prev_significant(k)?;
                    return is_ref_token(self.tokens[callee].kind).then_some(callee);
                }
                "(" => depth -= 1,
                ";" if depth == 0 => return None,
                _ => {}
            }
            j = k;
        }
        None
    }

    /// The formal parameter `name` (token `i`, followed by `=>`) of the
    /// subprogram being called.
    fn formal_parameter(&self, i: usize, offset: usize, name: &str) -> Option<Resolved> {
        let callee = self.enclosing_callee(i)?;
        let chain = self.chain_ending_at(callee);
        let names: Vec<&str> = chain.iter().map(|p| p.ident.name.as_str()).collect();
        let with_param = |candidates: Vec<SymRef>| -> Option<Resolved> {
            let candidates: Vec<SymRef> = candidates
                .into_iter()
                .filter(|r| self.symbol(*r).params.iter().any(|p| p.name.name == name))
                .collect();
            let subprogram = self.pick_overload(&candidates, callee)?;
            let index = self
                .symbol(subprogram)
                .params
                .iter()
                .position(|p| p.name.name == name)?;
            Some(Resolved::Parameter { subprogram, index })
        };
        match names.as_slice() {
            [proc] if self.find_local(offset, proc).is_some() => {
                return with_param(self.find_subprograms(offset, proc));
            }
            [pkg, proc] if self.find_local(offset, pkg).is_none() => {
                if let Some(scope) = self.local_package(pkg) {
                    let candidates = (0..self.ast.scope(scope).symbols.len())
                        .map(|index| SymRef { scope, index })
                        .filter(|r| {
                            let s = self.symbol(*r);
                            s.kind.is_subprogram() && s.name.name == *proc
                        })
                        .collect();
                    return with_param(candidates);
                }
            }
            _ => {}
        }
        // In the database: a standalone subprogram or a package member.
        let (object, member) = match names.as_slice() {
            [proc] => (self.cat.resolve(None, proc)??, None),
            [first, proc] => match self.cat.resolve(None, first)? {
                Some(o) => (o, Some(proc.to_string())),
                None => (self.cat.resolve(Some(first), proc)??, None),
            },
            [schema, object, proc] => (
                self.cat.resolve(Some(schema), object)??,
                Some(proc.to_string()),
            ),
            _ => return None,
        };
        let callable = match member {
            None => matches!(object.object_type.as_str(), "PROCEDURE" | "FUNCTION"),
            Some(_) => matches!(object.object_type.as_str(), "PACKAGE" | "TYPE"),
        };
        if !callable {
            return None;
        }
        let param = self
            .cat
            .members(&object.owner, &object.name)?
            .into_iter()
            .filter(|m| member.as_ref().is_none_or(|n| &m.name == n))
            .find_map(|m| m.params.into_iter().find(|p| p.name == name))?;
        Some(Resolved::CatalogParameter {
            object,
            member,
            param,
        })
    }

    fn resolve_table_path(
        &self,
        q: QueryId,
        path: &[Ident],
        span: &Span,
    ) -> Option<(Resolved, Span)> {
        if path.len() == 1
            && let Some((cq, ci)) = self.cte(q, &path[0].name)
        {
            return Some((
                Resolved::Cte {
                    query: cq,
                    index: ci,
                },
                span.clone(),
            ));
        }
        let pos = path.iter().position(|p| &p.span == span)?;
        if pos == 0 && path.len() == 2 {
            return Some((Resolved::Schema(path[0].name.clone()), span.clone()));
        }
        let obj = match path {
            [t] => self.cat.resolve(None, &t.name)??,
            [s, t] => self.cat.resolve(Some(&s.name), &t.name)??,
            _ => return None,
        };
        Some((Resolved::Object(obj), span.clone()))
    }
}

/// Same name and same parameter names (overload matching).
pub(crate) fn same_signature(a: &Symbol, b: &Symbol) -> bool {
    a.name.name == b.name.name
        && a.params.len() == b.params.len()
        && a.params
            .iter()
            .zip(&b.params)
            .all(|(x, y)| x.name.name == y.name.name)
}

fn is_scalar(name: &str) -> bool {
    matches!(
        name,
        "NUMBER"
            | "VARCHAR2"
            | "VARCHAR"
            | "CHAR"
            | "DATE"
            | "TIMESTAMP"
            | "CLOB"
            | "BLOB"
            | "INTEGER"
            | "INT"
            | "BOOLEAN"
            | "PLS_INTEGER"
            | "BINARY_INTEGER"
            | "FLOAT"
            | "LONG"
            | "RAW"
            | "NVARCHAR2"
            | "NCHAR"
            | "NCLOB"
            | "ROWID"
            | "SYS_REFCURSOR"
            | "INTERVAL"
            | "DECIMAL"
            | "NUMERIC"
            | "REAL"
            | "SMALLINT"
            | "SIMPLE_INTEGER"
            | "NATURAL"
            | "POSITIVE"
            | "BINARY_FLOAT"
            | "BINARY_DOUBLE"
            | "XMLTYPE"
            | "JSON"
    )
}

/// Tokens that can name something.
pub(crate) fn is_ref_token(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::Ident | TokenKind::QuotedIdent | TokenKind::Keyword | TokenKind::BindVar
    )
}
