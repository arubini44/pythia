//! SQL queries and DML: query blocks, sources, select items, clauses.

use super::Parser;
use crate::ast::*;
use crate::lexer::{Token, TokenKind};

/// Words that end a query block at paren depth 0 (outside `CASE`).
const QUERY_STOPS: &[&str] = &[
    "UNION",
    "INTERSECT",
    "MINUS",
    "EXCEPT",
    "BEGIN",
    "DECLARE",
    "END",
    "EXCEPTION",
    "ELSIF",
    "ELSE",
    "THEN",
    "WHEN",
    "LOOP",
];

/// Words that can't be a table alias.
const ALIAS_STOPS: &[&str] = &[
    "WHERE",
    "GROUP",
    "ORDER",
    "HAVING",
    "CONNECT",
    "START",
    "JOIN",
    "INNER",
    "LEFT",
    "RIGHT",
    "FULL",
    "OUTER",
    "CROSS",
    "NATURAL",
    "ON",
    "USING",
    "UNION",
    "INTERSECT",
    "MINUS",
    "EXCEPT",
    "PARTITION",
    "SAMPLE",
    "PIVOT",
    "UNPIVOT",
    "FETCH",
    "OFFSET",
    "FOR",
    "MODEL",
    "WINDOW",
    "SET",
    "LOG",
    "RETURNING",
    "RETURN",
    "WHEN",
    "VALUES",
    "SELECT",
    "INTO",
    "APPLY",
    "LATERAL",
    "AS",
    "VERSIONS",
    "LIMIT",
    "BULK",
    "SUBPARTITION",
    "WITH",
];

impl Parser<'_> {
    fn new_query(
        &mut self,
        kind: QueryKind,
        scope: ScopeId,
        parent: Option<QueryId>,
        start: usize,
    ) -> QueryId {
        let id = QueryId(self.ast.queries.len());
        self.ast.queries.push(Query {
            kind,
            span: start..start,
            parent,
            scope,
            ctes: Vec::new(),
            sources: Vec::new(),
            items: Vec::new(),
            clauses: Vec::new(),
            next: None,
        });
        id
    }

    fn q(&mut self, id: QueryId) -> &mut Query {
        &mut self.ast.queries[id.0]
    }

    /// Where a construct that stops at the current token ends.
    fn stop_end(&self) -> usize {
        self.start()
    }

    fn is_name_tok(&self, t: &Token) -> bool {
        match t.kind {
            TokenKind::Ident | TokenKind::QuotedIdent => true,
            TokenKind::Keyword => !super::is_reserved(self.text_of(t)),
            _ => false,
        }
    }

    fn ident_of(&self, t: &Token) -> Ident {
        let (name, quoted) = super::normalize_ident(self.text_of(t));
        Ident {
            name,
            span: t.span.clone(),
            quoted,
        }
    }

    pub(super) fn dml_or_query(
        &mut self,
        scope: ScopeId,
        parent: Option<QueryId>,
    ) -> Option<QueryId> {
        if self.is_w(0, "INSERT") {
            Some(self.dml(scope, parent, QueryKind::Insert))
        } else if self.is_w(0, "UPDATE") {
            Some(self.dml(scope, parent, QueryKind::Update))
        } else if self.is_w(0, "DELETE") {
            Some(self.dml(scope, parent, QueryKind::Delete))
        } else if self.is_w(0, "MERGE") {
            Some(self.dml(scope, parent, QueryKind::Merge))
        } else {
            self.query_expr(scope, parent, false)
        }
    }

    /// `[WITH ctes] block {set-op block}` at `WITH`, `SELECT` or `(`.
    /// Stops before `)`, `;` and block keywords.
    pub(super) fn query_expr(
        &mut self,
        scope: ScopeId,
        parent: Option<QueryId>,
        _in_parens: bool,
    ) -> Option<QueryId> {
        if self.at_end() {
            return None;
        }
        let start = self.start();
        let first = self.new_query(QueryKind::Select, scope, parent, start);
        if self.is_w(0, "WITH") {
            self.pos += 1;
            let ctes = self.ctes(scope, first);
            let end = self.stop_end();
            let q = self.q(first);
            q.ctes = ctes;
            q.clauses.push((Clause::With, start..end));
        }
        self.block_into(first, scope);
        let mut prev = first;
        while self.is_any(0, &["UNION", "INTERSECT", "MINUS", "EXCEPT"]) {
            self.pos += 1;
            self.eat_w("ALL");
            self.eat_w("DISTINCT");
            let next_start = self.start();
            let next = self.new_query(QueryKind::Select, scope, parent, next_start);
            self.block_into(next, scope);
            self.q(prev).next = Some(next);
            prev = next;
        }
        let end = self.stop_end();
        self.q(first).span.end = end;
        Some(first)
    }

    fn ctes(&mut self, scope: ScopeId, owner: QueryId) -> Vec<Cte> {
        let mut ctes = Vec::new();
        while let Some(name) = self.name() {
            let mut columns = Vec::new();
            if self.is_punct(0, "(") {
                self.pos += 1;
                while !self.at_end() && !self.eat_punct(")") {
                    if let Some(c) = self.name() {
                        columns.push(c);
                    } else {
                        self.pos += 1;
                    }
                }
            }
            self.eat_w("AS");
            while self.kind(0) == Some(TokenKind::Hint) {
                self.pos += 1;
            }
            let mut query = None;
            if self.is_punct(0, "(") {
                self.pos += 1;
                query = self.query_expr(scope, Some(owner), true);
                self.eat_punct(")");
            }
            ctes.push(Cte {
                name,
                columns,
                query,
            });
            // SEARCH / CYCLE clauses.
            while !self.at_end()
                && !self.is_punct(0, ",")
                && !self.is_any(0, &["SELECT", "WITH"])
                && !self.is_punct(0, ";")
                && !self.is_punct(0, ")")
            {
                if self.is_punct(0, "(") {
                    self.skip_group(scope, Some(owner));
                } else {
                    self.pos += 1;
                }
            }
            if !self.eat_punct(",") {
                break;
            }
        }
        ctes
    }

    /// One query block into `q`: `SELECT ...` or a parenthesized query.
    fn block_into(&mut self, q: QueryId, scope: ScopeId) {
        let start = self.start();
        if self.is_punct(0, "(") {
            // `(select ...) union ...`: the block is a wrapper around the inner query.
            self.pos += 1;
            let inner = self.query_expr(scope, Some(q), true);
            self.eat_punct(")");
            if let Some(inner) = inner {
                let span = start..self.prev_end();
                let query = self.q(q);
                query.sources.push(Source {
                    kind: SourceKind::Subquery(inner),
                    alias: None,
                    span: span.clone(),
                });
                query.items.push(SelectItem {
                    span,
                    name: None,
                    star: Some(Vec::new()),
                });
            }
            self.tail_clauses(q, scope);
            let end = self.stop_end();
            let query = self.q(q);
            query.span = query.span.start.min(start)..end;
            return;
        }
        if !self.is_w(0, "SELECT") {
            return;
        }
        self.pos += 1;
        while self.kind(0) == Some(TokenKind::Hint)
            || self.is_any(0, &["DISTINCT", "UNIQUE", "ALL"])
        {
            self.pos += 1;
        }
        let mut clause = Clause::Select;
        let mut clause_start = start;
        let mut item_begin = self.pos;
        let mut case_depth = 0usize;
        let mut expect_table = false;

        loop {
            if self.at_end() || self.is_punct(0, ";") || self.is_punct(0, ")") {
                break;
            }
            if case_depth == 0 && self.is_any(0, QUERY_STOPS) {
                break;
            }
            if self.is_w(0, "CASE") {
                case_depth += 1;
                self.pos += 1;
                continue;
            }
            if self.is_w(0, "END") && case_depth > 0 {
                case_depth -= 1;
                self.pos += 1;
                continue;
            }
            if case_depth == 0
                && let Some((next, words)) = self.select_clause_keyword(clause)
            {
                if clause == Clause::Select {
                    self.push_item(q, item_begin, self.pos);
                }
                let here = self.start();
                self.q(q).clauses.push((clause, clause_start..here));
                clause = next;
                clause_start = here;
                self.pos += words;
                expect_table = clause == Clause::From;
                continue;
            }
            if clause == Clause::Select && case_depth == 0 && self.is_punct(0, ",") {
                self.push_item(q, item_begin, self.pos);
                self.pos += 1;
                item_begin = self.pos;
                continue;
            }
            if clause == Clause::From
                && case_depth == 0
                && self.table_ref(q, scope, &mut expect_table)
            {
                continue;
            }
            if self.is_punct(0, "(") {
                if self.is_any(1, &["SELECT", "WITH"]) {
                    self.pos += 1;
                    self.query_expr(scope, Some(q), true);
                    self.eat_punct(")");
                } else {
                    self.skip_group(scope, Some(q));
                }
                continue;
            }
            self.pos += 1;
        }
        if clause == Clause::Select {
            self.push_item(q, item_begin, self.pos);
        }
        let end = self.stop_end();
        let query = self.q(q);
        query.clauses.push((clause, clause_start..end));
        query.span = query.span.start.min(start)..end;
    }

    /// `ORDER BY` / `FETCH` after a parenthesized block.
    fn tail_clauses(&mut self, q: QueryId, scope: ScopeId) {
        while self.is_any(0, &["ORDER", "FETCH", "OFFSET", "FOR"]) {
            let start = self.start();
            let clause = if self.is_w(0, "ORDER") {
                Clause::OrderBy
            } else {
                Clause::Tail
            };
            self.pos += 1;
            while !self.at_end()
                && !self.is_punct(0, ";")
                && !self.is_punct(0, ")")
                && !self.is_any(0, QUERY_STOPS)
            {
                if self.is_any(0, &["ORDER", "FETCH", "OFFSET"]) {
                    break;
                }
                if self.is_punct(0, "(") {
                    self.skip_group(scope, Some(q));
                } else {
                    self.pos += 1;
                }
            }
            let end = self.stop_end();
            self.q(q).clauses.push((clause, start..end));
        }
    }

    /// If the current token starts a new clause of a SELECT block, returns the
    /// clause and how many tokens the keyword spans.
    fn select_clause_keyword(&self, current: Clause) -> Option<(Clause, usize)> {
        if !self.kind(0).is_some_and(TokenKind::is_word) {
            return None;
        }
        let word = self.text(0).to_ascii_uppercase();
        Some(match word.as_str() {
            "INTO" if current == Clause::Select => (Clause::Into, 1),
            "BULK"
                if current == Clause::Select && self.is_w(1, "COLLECT") && self.is_w(2, "INTO") =>
            {
                (Clause::Into, 3)
            }
            "FROM" if matches!(current, Clause::Select | Clause::Into) => (Clause::From, 1),
            "WHERE" => (Clause::Where, 1),
            "GROUP" if self.is_w(1, "BY") => (Clause::GroupBy, 2),
            "HAVING" => (Clause::Having, 1),
            "ORDER" if self.is_w(1, "BY") => (Clause::OrderBy, 2),
            "ORDER" if self.is_w(1, "SIBLINGS") && self.is_w(2, "BY") => (Clause::OrderBy, 3),
            "CONNECT" if self.is_w(1, "BY") => (Clause::Hierarchy, 2),
            "START" if self.is_w(1, "WITH") => (Clause::Hierarchy, 2),
            "FETCH" if self.is_any(1, &["FIRST", "NEXT"]) => (Clause::Tail, 1),
            "OFFSET" => (Clause::Tail, 1),
            "FOR" if self.is_w(1, "UPDATE") => (Clause::Tail, 2),
            "MODEL" | "WINDOW" => (Clause::Tail, 1),
            _ => return None,
        })
    }

    /// Handles table references in a FROM clause. Returns true if it consumed
    /// tokens.
    fn table_ref(&mut self, q: QueryId, scope: ScopeId, expect_table: &mut bool) -> bool {
        if *expect_table {
            let start = self.start();
            if self.is_punct(0, "(") && self.is_any(1, &["SELECT", "WITH"]) {
                self.pos += 1;
                let sub = self.query_expr(scope, Some(q), true);
                self.eat_punct(")");
                let alias = self.alias();
                if let Some(sub) = sub {
                    let span = start..self.prev_end();
                    self.q(q).sources.push(Source {
                        kind: SourceKind::Subquery(sub),
                        alias,
                        span,
                    });
                }
                *expect_table = false;
                return true;
            }
            if self.is_any(0, &["TABLE", "LATERAL", "THE"]) && self.is_punct(1, "(") {
                let lateral = self.is_w(0, "LATERAL");
                self.pos += 1;
                if lateral && self.is_any(1, &["SELECT", "WITH"]) {
                    self.pos += 1;
                    let sub = self.query_expr(scope, Some(q), true);
                    self.eat_punct(")");
                    let alias = self.alias();
                    if let Some(sub) = sub {
                        let span = start..self.prev_end();
                        self.q(q).sources.push(Source {
                            kind: SourceKind::Subquery(sub),
                            alias,
                            span,
                        });
                    }
                } else {
                    self.skip_group(scope, Some(q));
                    let alias = self.alias();
                    let span = start..self.prev_end();
                    self.q(q).sources.push(Source {
                        kind: SourceKind::Function,
                        alias,
                        span,
                    });
                }
                *expect_table = false;
                return true;
            }
            if self.is_name(0) && !self.is_any(0, ALIAS_STOPS) {
                let mut path = self.dotted_name();
                if self.eat_punct("@") {
                    // Database link: keep the table path, drop the link name.
                    self.dotted_name();
                }
                let kind = if self.is_punct(0, "(") {
                    // JSON_TABLE(...), XMLTABLE(...), pipelined function call.
                    self.skip_group(scope, Some(q));
                    path.clear();
                    SourceKind::Function
                } else {
                    SourceKind::Table(path)
                };
                // PARTITION (p), SAMPLE (n), AS OF ...
                while self.is_any(0, &["PARTITION", "SUBPARTITION", "SAMPLE"]) {
                    self.pos += 1;
                    self.eat_w("FOR");
                    if self.is_punct(0, "(") {
                        self.skip_group(scope, Some(q));
                    }
                }
                let alias = self.alias();
                let span = start..self.prev_end();
                self.q(q).sources.push(Source { kind, alias, span });
                *expect_table = false;
                return true;
            }
            if self.is_punct(0, "(") {
                // Parenthesized join; its tables aren't tracked.
                self.skip_group(scope, Some(q));
                self.alias();
                *expect_table = false;
                return true;
            }
            return false;
        }
        if self.is_punct(0, ",") || self.is_any(0, &["JOIN", "APPLY"]) {
            self.pos += 1;
            *expect_table = true;
            return true;
        }
        if self.is_any(0, &["PIVOT", "UNPIVOT"]) {
            self.pos += 1;
            self.eat_w("XML");
            if self.is_any(0, &["INCLUDE", "EXCLUDE"]) {
                self.pos += 2;
            }
            if self.is_punct(0, "(") {
                self.skip_group(scope, Some(q));
            }
            self.alias();
            return true;
        }
        false
    }

    fn alias(&mut self) -> Option<Ident> {
        if self.is_w(0, "AS") && self.is_name(1) {
            self.pos += 1;
            return self.name();
        }
        if self.is_name(0) && !self.is_any(0, ALIAS_STOPS) {
            return self.name();
        }
        None
    }

    /// Records the select item made of tokens `begin..end`.
    fn push_item(&mut self, q: QueryId, begin: usize, end: usize) {
        if begin >= end || end > self.toks.len() {
            return;
        }
        let toks = &self.toks[begin..end];
        let span = toks[0].span.start..toks[toks.len() - 1].span.end;
        let last = &toks[toks.len() - 1];
        let last_text = self.text_of(last);
        let item = if last_text == "*" && !last.kind.is_word() {
            let qualifier = toks[..toks.len() - 1]
                .iter()
                .filter(|t| self.is_name_tok(t))
                .map(|t| self.ident_of(t))
                .collect();
            SelectItem {
                span,
                name: None,
                star: Some(qualifier),
            }
        } else if self.is_name_tok(last) {
            let prev = toks.len().checked_sub(2).map(|i| &toks[i]);
            let named = match prev {
                // `a`, `a.b`, `expr AS x`
                None => true,
                Some(p) if self.text_of(p) == "." || self.text_of(p).eq_ignore_ascii_case("AS") => {
                    true
                }
                // `expr x` (alias) vs `a + b` (unnamed expression)
                Some(p) => !matches!(
                    p.kind,
                    TokenKind::Operator | TokenKind::Comma | TokenKind::LParen
                ),
            };
            SelectItem {
                span,
                name: named.then(|| self.ident_of(last)),
                star: None,
            }
        } else {
            SelectItem {
                span,
                name: None,
                star: None,
            }
        };
        self.q(q).items.push(item);
    }

    /// INSERT / UPDATE / DELETE / MERGE.
    fn dml(&mut self, scope: ScopeId, parent: Option<QueryId>, kind: QueryKind) -> QueryId {
        let start = self.start();
        let q = self.new_query(kind, scope, parent, start);
        self.pos += 1;
        while self.kind(0) == Some(TokenKind::Hint) {
            self.pos += 1;
        }
        let mut clause = match kind {
            QueryKind::Insert | QueryKind::Merge => Clause::Into,
            _ => Clause::Target,
        };
        let mut clause_start = start;
        let mut expect_table = matches!(kind, QueryKind::Update | QueryKind::Delete);
        let mut case_depth = 0usize;
        // `INSERT (` after `THEN` in MERGE opens the column list.
        let mut columns_next = false;

        loop {
            if self.at_end() || self.is_punct(0, ";") || self.is_punct(0, ")") {
                break;
            }
            if case_depth == 0
                && self.is_any(
                    0,
                    &["BEGIN", "DECLARE", "END", "EXCEPTION", "ELSIF", "LOOP"],
                )
            {
                break;
            }
            if self.is_w(0, "CASE") {
                case_depth += 1;
                self.pos += 1;
                continue;
            }
            if self.is_w(0, "END") && case_depth > 0 {
                case_depth -= 1;
                self.pos += 1;
                continue;
            }
            if case_depth == 0
                && let Some((next, words)) = self.dml_clause_keyword(kind, clause)
            {
                let here = self.start();
                self.q(q).clauses.push((clause, clause_start..here));
                clause = next;
                clause_start = here;
                self.pos += words;
                expect_table = matches!(
                    (kind, next),
                    (QueryKind::Insert | QueryKind::Merge, Clause::Into)
                        | (QueryKind::Merge, Clause::Using)
                        | (QueryKind::Delete, Clause::Target)
                );
                if kind == QueryKind::Merge && next == Clause::When {
                    columns_next = false;
                }
                continue;
            }
            if kind == QueryKind::Merge && clause == Clause::When && self.is_w(0, "INSERT") {
                self.pos += 1;
                columns_next = true;
                continue;
            }
            if expect_table {
                let tstart = self.start();
                if self.is_punct(0, "(") && self.is_any(1, &["SELECT", "WITH"]) {
                    self.pos += 1;
                    let sub = self.query_expr(scope, Some(q), true);
                    self.eat_punct(")");
                    let alias = self.alias();
                    if let Some(sub) = sub {
                        let span = tstart..self.prev_end();
                        self.q(q).sources.push(Source {
                            kind: SourceKind::Subquery(sub),
                            alias,
                            span,
                        });
                    }
                    expect_table = false;
                    continue;
                }
                if self.is_name(0) && !self.is_any(0, ALIAS_STOPS) {
                    let path = self.dotted_name();
                    if self.eat_punct("@") {
                        self.dotted_name();
                    }
                    let alias = self.alias();
                    let span = tstart..self.prev_end();
                    self.q(q).sources.push(Source {
                        kind: SourceKind::Table(path),
                        alias,
                        span,
                    });
                    expect_table = false;
                    columns_next = kind == QueryKind::Insert;
                    continue;
                }
                expect_table = false;
            }
            if self.is_any(0, &["SELECT", "WITH"]) && case_depth == 0 {
                self.query_expr(scope, Some(q), false);
                continue;
            }
            if self.is_punct(0, "(") {
                if self.is_any(1, &["SELECT", "WITH"]) {
                    self.pos += 1;
                    self.query_expr(scope, Some(q), true);
                    self.eat_punct(")");
                } else if columns_next {
                    let here = self.start();
                    self.q(q).clauses.push((clause, clause_start..here));
                    clause = Clause::InsertColumns;
                    clause_start = here;
                    self.skip_group(scope, Some(q));
                    columns_next = false;
                } else {
                    self.skip_group(scope, Some(q));
                }
                continue;
            }
            columns_next = false;
            self.pos += 1;
        }
        let end = self.stop_end();
        let query = self.q(q);
        query.clauses.push((clause, clause_start..end));
        query.span.end = end;
        q
    }

    fn dml_clause_keyword(&self, kind: QueryKind, current: Clause) -> Option<(Clause, usize)> {
        if !self.kind(0).is_some_and(TokenKind::is_word) {
            return None;
        }
        let word = self.text(0).to_ascii_uppercase();
        Some(match (kind, word.as_str()) {
            (QueryKind::Delete, "FROM") if current == Clause::Target => (Clause::Target, 1),
            (QueryKind::Insert, "INTO") => (Clause::Into, 1),
            (QueryKind::Merge, "INTO") => (Clause::Into, 1),
            (QueryKind::Merge, "USING") if current == Clause::Into => (Clause::Using, 1),
            (QueryKind::Merge, "ON") if current == Clause::Using => (Clause::On, 1),
            (QueryKind::Merge, "WHEN") => (Clause::When, 1),
            (_, "SET") => (Clause::Set, 1),
            (_, "VALUES") => (Clause::Values, 1),
            (_, "WHERE") => (Clause::Where, 1),
            (_, "RETURNING" | "RETURN") => (Clause::Returning, 1),
            (_, "LOG") if self.is_w(1, "ERRORS") => (Clause::Tail, 2),
            _ => return None,
        })
    }
}
