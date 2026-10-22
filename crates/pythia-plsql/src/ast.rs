//! Syntax tree produced by the error-tolerant parser.
//!
//! The tree is deliberately shallow: it records what navigation, outline and
//! completion need (scopes, declarations, query structure) rather than every
//! expression. Scopes and queries live in flat arenas addressed by id; every
//! node keeps byte spans into the source.

use std::ops::Range;

pub type Span = Range<usize>;

/// An identifier as Oracle stores it: unquoted names are uppercased, quoted
/// names keep their exact spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ident {
    pub name: String,
    pub span: Span,
    pub quoted: bool,
}

impl Ident {
    /// Compares against another normalized name.
    pub fn is(&self, name: &str) -> bool {
        self.name == name
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ScopeId(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QueryId(pub usize);

#[derive(Debug, Clone, Default)]
pub struct Ast {
    /// `scopes[0]` is the root scope covering the whole source.
    pub scopes: Vec<Scope>,
    pub queries: Vec<Query>,
    /// Top-level statements, as split by SQL*Plus rules.
    pub statements: Vec<StatementInfo>,
    pub diagnostics: Vec<Diagnostic>,
}

impl Ast {
    pub const ROOT: ScopeId = ScopeId(0);

    pub fn scope(&self, id: ScopeId) -> &Scope {
        &self.scopes[id.0]
    }

    pub fn query(&self, id: QueryId) -> &Query {
        &self.queries[id.0]
    }

    /// The innermost scope whose span contains `offset`.
    pub fn scope_at(&self, offset: usize) -> ScopeId {
        let mut current = Self::ROOT;
        'descend: loop {
            for &child in &self.scope(current).children {
                let span = &self.scope(child).span;
                if span.start <= offset && offset <= span.end {
                    current = child;
                    continue 'descend;
                }
            }
            return current;
        }
    }

    /// Scopes from `id` up to the root.
    pub fn ancestors(&self, id: ScopeId) -> impl Iterator<Item = ScopeId> + '_ {
        std::iter::successors(Some(id), |s| self.scope(*s).parent)
    }

    /// The innermost query whose span contains `offset`.
    pub fn query_at(&self, offset: usize) -> Option<QueryId> {
        self.queries
            .iter()
            .enumerate()
            .filter(|(_, q)| q.span.start <= offset && offset <= q.span.end)
            .min_by_key(|(_, q)| q.span.len())
            .map(|(i, _)| QueryId(i))
    }

    /// Queries from `id` up through its enclosing queries.
    pub fn query_ancestors(&self, id: QueryId) -> impl Iterator<Item = QueryId> + '_ {
        std::iter::successors(Some(id), |q| self.query(*q).parent)
    }

    /// The top-level statement whose extent contains `offset`.
    pub fn statement_at(&self, offset: usize) -> Option<&StatementInfo> {
        self.statements
            .iter()
            .find(|s| s.span.start <= offset && offset <= s.extent_end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatementKind {
    /// `CREATE [OR REPLACE] <unit>` of a stored PL/SQL unit.
    Unit,
    /// `DECLARE` / `BEGIN` block.
    Block,
    /// A query or DML statement.
    Query,
    /// Any other SQL (DDL etc.); embedded queries are still parsed.
    Other,
    /// SQL*Plus command.
    SqlPlus,
}

#[derive(Debug, Clone)]
pub struct StatementInfo {
    pub kind: StatementKind,
    /// Statement text without terminator.
    pub span: Span,
    /// Where the statement's influence ends: its terminator if it has one,
    /// else the start of the next statement (or end of text). A cursor in
    /// `span.end..extent_end` is still "in" the statement (typing at its end).
    pub extent_end: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScopeKind {
    Root,
    PackageSpec,
    PackageBody,
    Procedure,
    Function,
    Trigger,
    TypeSpec,
    TypeBody,
    /// Anonymous or nested `DECLARE`/`BEGIN` block.
    Block,
    /// `FOR` / `FORALL` loop: owns the loop index.
    Loop,
    /// Cursor declaration: owns the cursor parameters.
    Cursor,
}

#[derive(Debug, Clone)]
pub struct Scope {
    pub kind: ScopeKind,
    pub name: Option<Ident>,
    /// Schema of a top-level unit (`CREATE PACKAGE hr.pkg`).
    pub owner: Option<Ident>,
    pub span: Span,
    pub parent: Option<ScopeId>,
    pub children: Vec<ScopeId>,
    pub symbols: Vec<Symbol>,
    /// Byte range of the declaration section, if any.
    pub decls: Option<Span>,
    /// Table of a trigger (`ON [owner.]table`), for `:new` / `:old`.
    pub trigger_table: Option<Vec<Ident>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParamMode {
    In,
    Out,
    InOut,
}

impl ParamMode {
    pub fn label(self) -> &'static str {
        match self {
            ParamMode::In => "IN",
            ParamMode::Out => "OUT",
            ParamMode::InOut => "IN OUT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SymbolKind {
    Variable,
    Constant,
    Parameter(ParamMode),
    Cursor,
    /// `TYPE t IS RECORD / TABLE OF / VARRAY / REF CURSOR`.
    Type,
    Subtype,
    Exception,
    Procedure,
    Function,
    LoopIndex,
    /// Attribute of an object type.
    Attribute,
    Label,
}

impl SymbolKind {
    pub fn is_subprogram(self) -> bool {
        matches!(self, SymbolKind::Procedure | SymbolKind::Function)
    }
}

/// A reference to a type as written: `NUMBER(10)`, `emp%ROWTYPE`,
/// `emp.sal%TYPE`, `pkg.t_rec`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeRef {
    /// Dotted name before any `%` attribute.
    pub path: Vec<Ident>,
    pub attr: TypeAttr,
    /// The type as written, for display.
    pub text: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeAttr {
    None,
    /// `%ROWTYPE`
    RowType,
    /// `%TYPE`
    Type,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: Ident,
    pub mode: ParamMode,
    pub ty: Option<TypeRef>,
    pub has_default: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: Ident,
    pub ty: Option<TypeRef>,
}

/// What a `TYPE` declaration defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDef {
    Record(Vec<Field>),
    /// `TABLE OF` / `VARRAY OF`: the element type.
    Collection(Option<TypeRef>),
    RefCursor(Option<TypeRef>),
    /// Object type attributes.
    Object(Vec<Field>),
    Other,
}

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: Ident,
    pub kind: SymbolKind,
    /// The whole declaration.
    pub span: Span,
    /// Declared type; return type for functions; base type for subtypes;
    /// row type for loop indexes over a cursor.
    pub ty: Option<TypeRef>,
    /// Parameters of subprograms and cursors.
    pub params: Vec<Param>,
    pub type_def: Option<TypeDef>,
    /// Query of a cursor or of a cursor `FOR` loop index.
    pub query: Option<QueryId>,
    /// Implementation scope of a subprogram defined here.
    pub body: Option<ScopeId>,
    /// Declaration without implementation (package spec, forward declaration).
    pub forward: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QueryKind {
    Select,
    Insert,
    Update,
    Delete,
    Merge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Clause {
    With,
    Select,
    /// `SELECT ... INTO` targets, or the `INSERT INTO` / `MERGE INTO` table.
    Into,
    From,
    Where,
    /// `CONNECT BY` / `START WITH`
    Hierarchy,
    GroupBy,
    Having,
    OrderBy,
    /// `FETCH FIRST` / `OFFSET` / `FOR UPDATE`.
    Tail,
    /// Table after `UPDATE` / `DELETE FROM`.
    Target,
    /// `INSERT INTO t (col, ...)` / merge insert column list.
    InsertColumns,
    Values,
    Set,
    Using,
    On,
    /// `WHEN [NOT] MATCHED THEN ...`
    When,
    Returning,
}

#[derive(Debug, Clone)]
pub enum SourceKind {
    /// `[schema.]table[@dblink]`
    Table(Vec<Ident>),
    Subquery(QueryId),
    /// `TABLE(...)`, `JSON_TABLE(...)`, `XMLTABLE(...)`.
    Function,
}

#[derive(Debug, Clone)]
pub struct Source {
    pub kind: SourceKind,
    pub alias: Option<Ident>,
    pub span: Span,
}

impl Source {
    /// The name a column qualifier refers to: the alias, else the table name.
    pub fn exposed_name(&self) -> Option<&Ident> {
        self.alias.as_ref().or(match &self.kind {
            SourceKind::Table(path) => path.last(),
            _ => None,
        })
    }
}

#[derive(Debug, Clone)]
pub struct SelectItem {
    pub span: Span,
    /// Alias or column name the item is known by.
    pub name: Option<Ident>,
    /// `*` (empty qualifier) or `q.*`.
    pub star: Option<Vec<Ident>>,
}

#[derive(Debug, Clone)]
pub struct Cte {
    pub name: Ident,
    pub columns: Vec<Ident>,
    pub query: Option<QueryId>,
}

#[derive(Debug, Clone)]
pub struct Query {
    pub kind: QueryKind,
    pub span: Span,
    /// Enclosing query block (subqueries, inline views, CTEs).
    pub parent: Option<QueryId>,
    /// PL/SQL scope the query appears in.
    pub scope: ScopeId,
    pub ctes: Vec<Cte>,
    pub sources: Vec<Source>,
    pub items: Vec<SelectItem>,
    /// Clauses in source order; each span runs to the next clause.
    pub clauses: Vec<(Clause, Span)>,
    /// Next block of a set operation (`UNION`, `MINUS`, ...).
    pub next: Option<QueryId>,
}

impl Query {
    pub fn clause_at(&self, offset: usize) -> Option<Clause> {
        self.clauses
            .iter()
            .rev()
            .find(|(_, s)| s.start <= offset && offset <= s.end)
            .map(|(c, _)| *c)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Span,
    pub message: String,
    pub severity: Severity,
}
