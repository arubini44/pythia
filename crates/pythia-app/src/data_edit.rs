//! Editing query results: finding the base table and turning grid changes
//! into DML.
//!
//! Rows are identified by ROWID, fetched by wrapping the user's query:
//! `select q.*, q.rowid as pythia$rowid from (<query>) q` — Oracle itself
//! rejects shapes that aren't key-preserved (ORA-01446). Nothing is locked
//! while editing; instead every UPDATE/DELETE re-checks the values that were
//! fetched (optimistic concurrency), so a row changed by another session is
//! reported as a conflict rather than silently overwritten.

use pythia_data::sql_literal;
use pythia_db::{Cell, Column};
use pythia_plsql::{TokenKind, tokenize};
use pythia_ui::Changes;

use crate::documents::quote_ident;

/// Name of the ROWID column added to editable queries.
pub const ROWID_COLUMN: &str = "PYTHIA$ROWID";

/// Wraps a query so the result carries the base table's ROWID.
pub fn editable_query(sql: &str) -> String {
    format!(
        "select q.*, q.rowid as {} from (\n{}\n) q",
        ROWID_COLUMN.to_lowercase(),
        sql.trim().trim_end_matches(';')
    )
}

/// The table a simple single-table query reads from, as written.
pub fn single_table(sql: &str) -> Option<String> {
    let tokens: Vec<_> = tokenize(sql)
        .into_iter()
        .filter(|t| !t.kind.is_trivia())
        .collect();
    let text = |i: usize| &sql[tokens[i].span.clone()];
    let is = |i: usize, word: &str| {
        tokens
            .get(i)
            .is_some_and(|t| t.kind.is_word() && text(i).eq_ignore_ascii_case(word))
    };
    if !is(0, "SELECT") {
        return None;
    }
    let mut depth = 0i32;
    let mut from = None;
    for (i, t) in tokens.iter().enumerate() {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            _ if depth == 0 && is(i, "FROM") => {
                from = Some(i);
                break;
            }
            _ => {}
        }
    }
    let i = from? + 1;
    let ident = |i: usize| {
        tokens
            .get(i)
            .is_some_and(|t| matches!(t.kind, TokenKind::Ident | TokenKind::QuotedIdent))
    };
    if !ident(i) {
        return None;
    }
    let (table, mut next) =
        if tokens.get(i + 1).is_some_and(|t| t.kind == TokenKind::Dot) && ident(i + 2) {
            (format!("{}.{}", text(i), text(i + 2)), i + 3)
        } else {
            (text(i).to_owned(), i + 1)
        };
    // Optional alias.
    if ident(next) {
        next += 1;
    }
    // Anything that brings in a second table makes the result read-only.
    let joins = [
        "JOIN",
        "INNER",
        "LEFT",
        "RIGHT",
        "FULL",
        "CROSS",
        "NATURAL",
        "UNION",
        "INTERSECT",
        "MINUS",
    ];
    let multi = tokens.get(next).is_some_and(|t| t.kind == TokenKind::Comma)
        || (next..tokens.len()).any(|j| joins.iter().any(|w| is(j, w)));
    (!multi).then_some(table)
}

/// One DML statement plus the number of rows it must affect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dml {
    pub sql: String,
    pub expect_rows: u64,
    /// Human description for conflict messages, e.g. `row 12`.
    pub what: String,
}

/// Generates the DML for `changes` against `table`. `rowid_col` is the index
/// of the ROWID column in `columns` / `rows`.
pub fn dml(
    table: &str,
    columns: &[Column],
    rows: &[Vec<Cell>],
    rowid_col: usize,
    changes: &Changes,
) -> Vec<Dml> {
    let mut out = Vec::new();
    let value = |row: &[Cell], c: usize| row.get(c).cloned().flatten();
    // Null-safe "unchanged since fetched" check; DECODE treats NULL = NULL.
    let unchanged = |col: &Column, original: Option<&str>| {
        format!(
            "decode({}, {}, 1, 0) = 1",
            quote_ident(&col.name),
            sql_literal(col, original)
        )
    };
    let editable = |c: usize| c != rowid_col && c < columns.len();

    for &r in &changes.deletes {
        let Some(row) = rows.get(r) else { continue };
        let Some(rowid) = value(row, rowid_col) else {
            continue;
        };
        out.push(Dml {
            sql: format!("delete from {table} where rowid = '{rowid}'"),
            expect_rows: 1,
            what: format!("row {}", r + 1),
        });
    }
    for (&r, cells) in &changes.updates {
        if changes.deletes.contains(&r) {
            continue;
        }
        let Some(row) = rows.get(r) else { continue };
        let Some(rowid) = value(row, rowid_col) else {
            continue;
        };
        let cells: Vec<_> = cells.iter().filter(|(c, _)| editable(**c)).collect();
        if cells.is_empty() {
            continue;
        }
        let sets: Vec<String> = cells
            .iter()
            .map(|(c, v)| {
                format!(
                    "{} = {}",
                    quote_ident(&columns[**c].name),
                    sql_literal(&columns[**c], v.as_deref())
                )
            })
            .collect();
        let checks: Vec<String> = cells
            .iter()
            .map(|(c, _)| unchanged(&columns[**c], row.get(**c).and_then(|v| v.as_deref())))
            .collect();
        out.push(Dml {
            sql: format!(
                "update {table} set {} where rowid = '{rowid}' and {}",
                sets.join(", "),
                checks.join(" and ")
            ),
            expect_rows: 1,
            what: format!("row {}", r + 1),
        });
    }
    for (i, row) in changes.inserts.iter().enumerate() {
        let cells: Vec<(usize, &str)> = row
            .iter()
            .enumerate()
            .filter(|(c, _)| editable(*c))
            .filter_map(|(c, v)| Some((c, v.as_deref()?)))
            .collect();
        if cells.is_empty() {
            continue;
        }
        let names: Vec<String> = cells
            .iter()
            .map(|(c, _)| quote_ident(&columns[*c].name))
            .collect();
        let values: Vec<String> = cells
            .iter()
            .map(|(c, v)| sql_literal(&columns[*c], Some(v)))
            .collect();
        out.push(Dml {
            sql: format!(
                "insert into {table} ({}) values ({})",
                names.join(", "),
                values.join(", ")
            ),
            expect_rows: 1,
            what: format!("new row {}", i + 1),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[test]
    fn single_tables() {
        assert_eq!(
            single_table("select * from emp where x = 1"),
            Some("emp".into())
        );
        assert_eq!(
            single_table("select e.* from hr.emp e order by 1"),
            Some("hr.emp".into())
        );
        assert_eq!(
            single_table("select (select 1 from dual) x from \"Mixed\" t"),
            Some("\"Mixed\"".into())
        );
        assert_eq!(
            single_table("select * from emp e join dept d on d.id = e.dept"),
            None
        );
        assert_eq!(single_table("select * from emp, dept"), None);
        assert_eq!(single_table("update emp set x = 1"), None);
    }

    #[test]
    fn generates_dml() {
        let col = |n: &str, t: &str, numeric| Column {
            name: n.into(),
            type_name: t.into(),
            numeric,
        };
        let columns = vec![
            col("ID", "NUMBER", true),
            col("NAME", "VARCHAR2(10)", false),
            col(ROWID_COLUMN, "ROWID", false),
        ];
        let rows = vec![
            vec![Some("1".into()), Some("a".into()), Some("AAA1".into())],
            vec![Some("2".into()), None, Some("AAA2".into())],
        ];
        let changes = Changes {
            updates: BTreeMap::from([(1, BTreeMap::from([(1, Some("b'c".into()))]))]),
            inserts: vec![vec![Some("3".into()), None, None]],
            deletes: [0].into(),
        };
        let sql: Vec<String> = dml("emp", &columns, &rows, 2, &changes)
            .into_iter()
            .map(|d| d.sql)
            .collect();
        assert_eq!(
            sql,
            [
                "delete from emp where rowid = 'AAA1'",
                "update emp set NAME = 'b''c' where rowid = 'AAA2' and decode(NAME, NULL, 1, 0) = 1",
                "insert into emp (ID) values (3)",
            ]
        );
    }
}
