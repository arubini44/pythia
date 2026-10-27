//! Editing query results: finding the base table and turning grid changes
//! into DML.
//!
//! On Oracle, rows are identified by ROWID, fetched by wrapping the user's
//! query: `select q.*, q.rowid as pythia$rowid from (<query>) q` — Oracle
//! itself rejects shapes that aren't key-preserved (ORA-01446). MySQL has no
//! ROWID: there the table's primary key (or a unique key on NOT NULL
//! columns) identifies a row, its columns added to the select list as
//! `pythia$<column>`. Nothing is locked while editing; instead every
//! UPDATE/DELETE re-checks the values that were fetched (optimistic
//! concurrency), so a row changed by another session is reported as a
//! conflict rather than silently overwritten.

use pythia_data::literal;
use pythia_db::{Cell, Client, Column, DbError, DbKind, Dialect};
use pythia_plsql::{TokenKind, tokenize};
use pythia_ui::Changes;

/// Name of the ROWID column added to editable queries.
pub const ROWID_COLUMN: &str = "PYTHIA$ROWID";

/// Functions that fold rows into one (unless used with `OVER`).
const AGGREGATES: &[&str] = &[
    "AVG",
    "BIT_AND",
    "BIT_OR",
    "BIT_XOR",
    "COUNT",
    "GROUP_CONCAT",
    "JSON_ARRAYAGG",
    "JSON_OBJECTAGG",
    "LISTAGG",
    "MAX",
    "MEDIAN",
    "MIN",
    "STD",
    "STDDEV",
    "STDDEV_POP",
    "STDDEV_SAMP",
    "SUM",
    "VAR_POP",
    "VAR_SAMP",
    "VARIANCE",
];

/// How the rows of an editable result are found again. The result ends
/// with the columns that identify them, read-only in the grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKey {
    /// Oracle: one column with the ROWID.
    Rowid,
    /// The table's key columns, in key order.
    Columns(Vec<String>),
}

impl RowKey {
    /// How many columns at the end of the result hold the key.
    pub fn width(&self) -> usize {
        match self {
            RowKey::Rowid => 1,
            RowKey::Columns(columns) => columns.len(),
        }
    }
}

/// A result being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Editable {
    /// The table changes go to, as written in the query.
    pub table: String,
    /// The query as the user wrote it.
    pub source: String,
    pub key: RowKey,
}

/// Wraps a query so the result carries the base table's ROWID.
pub fn editable_query(sql: &str) -> String {
    format!(
        "select q.*, q.rowid as {} from (\n{}\n) q",
        ROWID_COLUMN.to_lowercase(),
        sql.trim().trim_end_matches(';')
    )
}

/// Adds the key columns to a query's select list (MySQL), as
/// `pythia$<column>`. `None` unless [`can_edit`].
pub fn keyed_query(sql: &str, key: &[String]) -> Option<String> {
    let sql = sql.trim().trim_end_matches(';');
    let query = parse(sql).filter(|q| !q.grouped)?;
    let dialect = Dialect::MYSQL;
    let columns: Vec<String> = key
        .iter()
        .map(|k| {
            format!(
                "{} as {}",
                dialect.quote_ident(k),
                dialect.quote_ident(&format!("pythia${k}"))
            )
        })
        .collect();
    Some(format!(
        "{}, {}{}",
        &sql[..query.list_end],
        columns.join(", "),
        &sql[query.list_end..]
    ))
}

/// The query fetching the rows of `edit` with what identifies them.
pub fn edit_query(edit: &Editable) -> Option<String> {
    match &edit.key {
        RowKey::Rowid => Some(editable_query(&edit.source)),
        RowKey::Columns(key) => keyed_query(&edit.source, key),
    }
}

/// A simple single-table query, as [`parse`] finds it.
struct Query {
    /// The table, as written.
    table: String,
    /// Where the select list ends (after its last token).
    list_end: usize,
    /// DISTINCT, GROUP BY, HAVING or an aggregate: a row of the result
    /// isn't a row of the table.
    grouped: bool,
}

fn parse(sql: &str) -> Option<Query> {
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
    let kind = |i: usize| tokens.get(i).map(|t| t.kind);
    // The parenthesis closing the one at `open`.
    let closing = |open: usize| {
        let mut depth = 0i32;
        (open..tokens.len()).find(|&j| {
            match tokens[j].kind {
                TokenKind::LParen => depth += 1,
                TokenKind::RParen => depth -= 1,
                _ => {}
            }
            depth == 0
        })
    };
    if !is(0, "SELECT") {
        return None;
    }
    let mut grouped = false;
    let mut depth = 0i32;
    let mut i = 1;
    let from = loop {
        match kind(i)? {
            // A subquery's aggregates are its own.
            TokenKind::LParen if is(i + 1, "SELECT") => i = closing(i)?,
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            _ if depth == 0 && is(i, "FROM") => break i,
            _ if is(i, "DISTINCT") || is(i, "DISTINCTROW") => grouped = true,
            _ if kind(i + 1) == Some(TokenKind::LParen) && AGGREGATES.iter().any(|a| is(i, a)) => {
                grouped |= !is(closing(i + 1)? + 1, "OVER");
            }
            _ => {}
        }
        i += 1;
    };
    let list_end = tokens[from - 1].span.end;
    let i = from + 1;
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
        "STRAIGHT_JOIN",
        "UNION",
        "INTERSECT",
        "MINUS",
        "EXCEPT",
    ];
    // From the table on: a join word that isn't a keyword looks like an alias.
    let multi = tokens.get(next).is_some_and(|t| t.kind == TokenKind::Comma)
        || (i + 1..tokens.len()).any(|j| joins.iter().any(|w| is(j, w)));
    if multi {
        return None;
    }
    let mut depth = 0i32;
    for (j, t) in tokens.iter().enumerate().skip(next) {
        match t.kind {
            TokenKind::LParen => depth += 1,
            TokenKind::RParen => depth -= 1,
            _ if depth == 0 && (is(j, "GROUP") || is(j, "HAVING")) => grouped = true,
            _ => {}
        }
    }
    Some(Query {
        table,
        list_end,
        grouped,
    })
}

/// The table a simple single-table query reads from, as written.
pub fn single_table(sql: &str) -> Option<String> {
    parse(sql).map(|q| q.table)
}

/// Whether the rows of `sql`'s result are rows of one table, so they can
/// be edited.
pub fn can_edit(sql: &str) -> bool {
    parse(sql).is_some_and(|q| !q.grouped)
}

/// The columns that identify a row of `table` (as written in a query, on
/// MySQL): its primary key, or else a unique key on NOT NULL columns.
/// Empty if it has neither (or isn't a table).
pub fn row_key(client: &mut dyn Client, table: &str) -> Result<Vec<String>, DbError> {
    let names = unquote(table);
    let (schema, name) = match names.as_slice() {
        [schema, name] => (Some(schema.as_str()), name.as_str()),
        [name] => (None, name.as_str()),
        _ => return Ok(Vec::new()),
    };
    // The query ran, so the name as written is right; the dictionary may
    // still have it in another case where names don't depend on it.
    let (schema_sql, mut params) = match schema {
        Some(schema) => ("lower(?)", vec![schema]),
        None => ("lower(database())", Vec::new()),
    };
    params.extend([name, name]);
    let sql = format!(
        "select s.table_name, s.index_name, s.column_name, s.nullable \
           from information_schema.statistics s \
          where lower(s.table_schema) = {schema_sql} \
            and lower(s.table_name) = lower(?) \
            and s.non_unique = 0 \
          order by s.table_name <> ?, s.table_name, s.index_name <> 'PRIMARY', \
                   s.index_name, s.seq_in_index"
    );
    let rows = client.query(&sql, &params)?;
    let value = |row: &Vec<Cell>, i: usize| row.get(i).cloned().flatten();
    let Some(table) = rows.first().and_then(|r| value(r, 0)) else {
        return Ok(Vec::new());
    };
    let mut indexes: Vec<(String, Vec<Option<String>>)> = Vec::new();
    for row in rows.iter().filter(|r| value(r, 0).as_ref() == Some(&table)) {
        let index = value(row, 1).unwrap_or_default();
        // Functional key parts have no column; nullable ones don't
        // identify a row.
        let column = value(row, 2).filter(|_| value(row, 3).as_deref() != Some("YES"));
        match indexes.last_mut() {
            Some((name, columns)) if *name == index => columns.push(column),
            _ => indexes.push((index, vec![column])),
        }
    }
    Ok(indexes
        .into_iter()
        .find_map(|(_, columns)| columns.into_iter().collect::<Option<Vec<_>>>())
        .unwrap_or_default())
}

/// The names in `schema.table` (or `table`) without their quotes.
fn unquote(name: &str) -> Vec<String> {
    tokenize(name)
        .into_iter()
        .filter_map(|t| {
            let text = &name[t.span];
            match t.kind {
                TokenKind::QuotedIdent if text.starts_with('`') => {
                    Some(text.trim_matches('`').replace("``", "`"))
                }
                TokenKind::QuotedIdent => Some(text.trim_matches('"').replace("\"\"", "\"")),
                TokenKind::Ident | TokenKind::Keyword => Some(text.to_owned()),
                _ => None,
            }
        })
        .collect()
}

/// One DML statement plus the number of rows it must affect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dml {
    pub sql: String,
    pub expect_rows: u64,
    /// Human description for conflict messages, e.g. `row 12`.
    pub what: String,
}

/// SQL that holds while column `name` (of type `col`) still has `value`.
fn same(dialect: Dialect, name: &str, col: &Column, value: Option<&str>) -> String {
    let name = dialect.quote_ident(name);
    let lit = literal(dialect, col, value);
    if dialect.kind() == DbKind::Oracle {
        // DECODE treats NULL = NULL.
        return format!("decode({name}, {lit}, 1, 0) = 1");
    }
    match value {
        None => format!("{name} is null"),
        // FLOAT is shown with six significant digits, rounded.
        Some(v) if col.type_name == "FLOAT" && v.parse::<f64>().is_ok_and(f64::is_finite) => {
            format!("abs({name} - {v}) <= abs({v}) * 0.00001")
        }
        // A string compares with JSON as a JSON string.
        Some(_) if col.type_name == "JSON" => format!("{name} = cast({lit} as json)"),
        Some(_) => format!("{name} = {lit}"),
    }
}

/// Generates the DML for `changes` to the rows of `edit`, in `dialect`'s
/// SQL. The last columns of `columns` / `rows` hold the key.
pub fn dml(
    dialect: Dialect,
    edit: &Editable,
    columns: &[Column],
    rows: &[Vec<Cell>],
    changes: &Changes,
) -> Vec<Dml> {
    let mut out = Vec::new();
    let table = &edit.table;
    let key_start = columns.len().saturating_sub(edit.key.width());
    let value = |row: &[Cell], c: usize| row.get(c).cloned().flatten();
    let editable = |c: usize| c < key_start;
    // Where the row is the one fetched; `None` without its key.
    let row_is = |row: &[Cell]| match &edit.key {
        RowKey::Rowid => value(row, key_start).map(|rowid| format!("rowid = '{rowid}'")),
        RowKey::Columns(key) => {
            let checks: Option<Vec<String>> = key
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let v = value(row, key_start + i)?;
                    Some(same(dialect, name, &columns[key_start + i], Some(&v)))
                })
                .collect();
            checks.map(|c| c.join(" and "))
        }
    };

    for &r in &changes.deletes {
        let Some(row) = rows.get(r) else { continue };
        let Some(found) = row_is(row) else {
            continue;
        };
        out.push(Dml {
            sql: format!("delete from {table} where {found}"),
            expect_rows: 1,
            what: format!("row {}", r + 1),
        });
    }
    for (&r, cells) in &changes.updates {
        if changes.deletes.contains(&r) {
            continue;
        }
        let Some(row) = rows.get(r) else { continue };
        let Some(found) = row_is(row) else {
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
                    dialect.quote_ident(&columns[**c].name),
                    literal(dialect, &columns[**c], v.as_deref())
                )
            })
            .collect();
        let checks: Vec<String> = cells
            .iter()
            .map(|(c, _)| {
                let col = &columns[**c];
                same(
                    dialect,
                    &col.name,
                    col,
                    row.get(**c).and_then(|v| v.as_deref()),
                )
            })
            .collect();
        out.push(Dml {
            sql: format!(
                "update {table} set {} where {found} and {}",
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
            .map(|(c, _)| dialect.quote_ident(&columns[*c].name))
            .collect();
        let values: Vec<String> = cells
            .iter()
            .map(|(c, v)| literal(dialect, &columns[*c], Some(v)))
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
            single_table("select * from `shop`.`order` o"),
            Some("`shop`.`order`".into())
        );
        assert_eq!(
            single_table("select * from emp e join dept d on d.id = e.dept"),
            None
        );
        assert_eq!(single_table("select * from emp, dept"), None);
        assert_eq!(single_table("select * from emp straight_join dept"), None);
        assert_eq!(single_table("update emp set x = 1"), None);
    }

    #[test]
    fn grouped_results_cant_be_edited() {
        for sql in [
            "select distinct dept from emp",
            "select count(*) from emp",
            "select coalesce(max(sal), 0) from emp where dept = 1",
            "select dept, sum(sal) from emp group by dept",
            "select dept from emp group by dept having count(*) > 1",
            "select * from emp group by id with rollup",
        ] {
            assert!(!can_edit(sql), "{sql}");
            assert!(single_table(sql).is_some(), "{sql}");
        }
        for sql in [
            "select * from emp",
            "select e.*, (select max(sal) from emp) top from emp e",
            "select e.*, sum(sal) over (partition by dept) from emp e",
            "select extract(year from hired) from emp where id in (select id from x group by id)",
            "select * from emp where name = 'distinct' -- count(*)",
        ] {
            assert!(can_edit(sql), "{sql}");
        }
    }

    #[test]
    fn keyed_queries() {
        let key = |k: &[&str]| k.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            keyed_query("select * from emp where x = 1;", &key(&["id"])).as_deref(),
            Some("select *, id as pythia$id from emp where x = 1")
        );
        assert_eq!(
            keyed_query(
                "select e.name -- the name\n  from `order` e",
                &key(&["order id", "line"])
            )
            .as_deref(),
            Some(
                "select e.name, `order id` as `pythia$order id`, line as pythia$line \
                 -- the name\n  from `order` e"
            )
        );
        assert_eq!(keyed_query("select count(*) from emp", &key(&["id"])), None);
    }

    #[test]
    fn unquoting() {
        assert_eq!(unquote("emp"), ["emp"]);
        assert_eq!(unquote("`my db`.`a``b`"), ["my db", "a`b"]);
        assert_eq!(unquote("\"Mixed\".x"), ["Mixed", "x"]);
    }

    fn col(n: &str, t: &str, numeric: bool) -> Column {
        Column {
            name: n.into(),
            type_name: t.into(),
            numeric,
        }
    }

    #[test]
    fn generates_dml() {
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
        let edit = Editable {
            table: "emp".into(),
            source: "select * from emp".into(),
            key: RowKey::Rowid,
        };
        let sql: Vec<String> = dml(Dialect::ORACLE, &edit, &columns, &rows, &changes)
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

    #[test]
    fn generates_mysql_dml() {
        let columns = vec![
            col("id", "INT", true),
            col("line", "INT", true),
            col("note", "VARCHAR", false),
            col("price", "FLOAT", true),
            col("doc", "JSON", false),
            col("pythia$id", "INT", true),
            col("pythia$line", "INT", true),
        ];
        let row = |id: &str, note: Option<&str>| {
            vec![
                Some(id.into()),
                Some("1".into()),
                note.map(Into::into),
                Some("123457".into()),
                Some(r#"{"a": 1}"#.into()),
                Some(id.into()),
                Some("1".into()),
            ]
        };
        let rows = vec![row("1", Some("a")), row("2", None)];
        let changes = Changes {
            updates: BTreeMap::from([
                (
                    1,
                    BTreeMap::from([(2, Some(r"C:\temp".into())), (5, Some("9".into()))]),
                ),
                (
                    0,
                    BTreeMap::from([(3, Some("1.5".into())), (4, Some("[]".into()))]),
                ),
            ]),
            inserts: vec![vec![None, Some("2".into()), Some("it's".into())]],
            deletes: [].into(),
        };
        let edit = Editable {
            table: "`order`".into(),
            source: "select * from `order`".into(),
            key: RowKey::Columns(vec!["id".into(), "line".into()]),
        };
        let sql: Vec<String> = dml(Dialect::MYSQL, &edit, &columns, &rows, &changes)
            .into_iter()
            .map(|d| d.sql)
            .collect();
        assert_eq!(
            sql,
            [
                "update `order` set price = 1.5, doc = '[]' where id = 1 and line = 1 \
                 and abs(price - 123457) <= abs(123457) * 0.00001 \
                 and doc = cast('{\"a\": 1}' as json)",
                "update `order` set note = _utf8mb4 X'433A5C74656D70' where id = 2 and line = 1 \
                 and note is null",
                "insert into `order` (line, note) values (2, 'it''s')",
            ]
        );
        let deletes = Changes {
            deletes: [1].into(),
            ..Changes::default()
        };
        assert_eq!(
            dml(Dialect::MYSQL, &edit, &columns, &rows, &deletes)[0].sql,
            "delete from `order` where id = 2 and line = 1"
        );
    }
}
