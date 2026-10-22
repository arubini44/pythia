//! View: column aliases, query and the `FORCE` / `WITH READ ONLY` /
//! `WITH CHECK OPTION` clauses; always `CREATE OR REPLACE`.

use egui::Ui;
use pythia_db::DbError;
use pythia_db::oracle::Connection;

use super::{form_grid, form_row, name_field, qualified};
use crate::documents::{ObjectRef, quote_ident};

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewDef {
    pub name: String,
    /// Comma separated column aliases; empty uses the query's names.
    pub columns: String,
    pub query: String,
    pub force: bool,
    pub read_only: bool,
    pub check_option: bool,
}

impl ViewDef {
    pub fn new() -> Self {
        Self {
            query: "select *\n  from dual".into(),
            ..Self::default()
        }
    }
}

pub fn load(conn: &Connection, owner: &str, name: &str) -> Result<ViewDef, DbError> {
    let (text, read_only) = conn
        .query_row_as::<(Option<String>, String)>(
            "select text, read_only from all_views where owner = :1 and view_name = :2",
            &[&owner, &name],
        )
        .map_err(|e| match e.db_error().map(|d| d.code()) {
            Some(1403) => DbError::new(format!("View {owner}.{name} not found")),
            _ => e.into(),
        })?;
    let text = text.unwrap_or_default();
    let (query, clause) = split_clause(&text);
    let read_only = read_only == "Y" || clause == Some(Clause::ReadOnly);
    // 23ai keeps the clause in the text; before, a check option is only
    // known as a constraint of the view (slow to look up on a big dictionary).
    let check_option = match clause {
        Some(clause) => clause == Clause::CheckOption,
        None if read_only || conn.server_version()?.0.major() >= 23 => false,
        None => conn
            .query_as::<String>(
                "select constraint_type from all_constraints
                  where owner = :1 and table_name = :2 and constraint_type = 'V'",
                &[&owner, &name],
            )?
            .next()
            .is_some(),
    };
    Ok(ViewDef {
        name: quote_ident(name),
        columns: String::new(),
        query: query.to_owned(),
        force: false,
        read_only,
        check_option,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Clause {
    ReadOnly,
    CheckOption,
}

/// Splits off a trailing `WITH READ ONLY` / `WITH CHECK OPTION [CONSTRAINT x]`
/// that newer versions keep in `ALL_VIEWS.TEXT`.
fn split_clause(text: &str) -> (&str, Option<Clause>) {
    let trimmed = text.trim_end();
    let lower = trimmed.to_lowercase();
    for (clause, kind) in [
        ("with read only", Clause::ReadOnly),
        ("with check option", Clause::CheckOption),
    ] {
        if let Some(at) = lower.rfind(clause) {
            let rest = lower[at + clause.len()..].trim();
            let tail_ok = rest.is_empty()
                || rest.starts_with("constraint") && rest.split_whitespace().count() == 2;
            if tail_ok {
                return (trimmed[..at].trim_end(), Some(kind));
            }
        }
    }
    (trimmed, None)
}

pub fn statements(owner: &str, def: &ViewDef) -> Result<Vec<String>, String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    let query = def.query.trim().trim_end_matches(';').trim_end();
    if query.is_empty() {
        return Err("Enter the query".into());
    }
    if def.read_only && def.check_option {
        return Err("A view is either read only or has a check option".into());
    }
    let force = if def.force { "force " } else { "" };
    let columns = match def.columns.trim() {
        "" => String::new(),
        c => format!(" ({c})"),
    };
    let clause = if def.read_only {
        "\nwith read only"
    } else if def.check_option {
        "\nwith check option"
    } else {
        ""
    };
    Ok(vec![format!(
        "create or replace {force}view {}{columns} as\n{query}{clause}",
        qualified(owner, &def.name)
    )])
}

/// A query of all columns of `from`, for a view on it in `owner`'s schema.
pub fn query_on(owner: &str, from: &ObjectRef, columns: &[String]) -> String {
    let name = |n: &str| {
        let q = quote_ident(n);
        if q.starts_with('"') {
            q
        } else {
            q.to_lowercase()
        }
    };
    let table = if from.owner == owner {
        name(&from.name)
    } else {
        format!("{}.{}", name(&from.owner), name(&from.name))
    };
    let list = if columns.is_empty() {
        "*".to_owned()
    } else {
        columns
            .iter()
            .map(|c| name(c))
            .collect::<Vec<_>>()
            .join(",\n       ")
    };
    format!("select {list}\n  from {table}")
}

/// The form above the query editor (which the document draws).
pub fn ui(ui: &mut Ui, def: &mut ViewDef, existing: bool) {
    form_grid(ui, "view", |ui| {
        name_field(ui, &mut def.name, existing);
        form_row(ui, "Column aliases", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.columns)
                    .hint_text("from the query")
                    .desired_width(360.0),
            );
        });
        form_row(ui, "Options", |ui| {
            ui.checkbox(&mut def.force, "force")
                .on_hover_text("Create the view even if the query has errors");
            if ui.checkbox(&mut def.read_only, "read only").changed() && def.read_only {
                def.check_option = false;
            }
            if ui
                .checkbox(&mut def.check_option, "check option")
                .on_hover_text("Inserts and updates must satisfy the WHERE clause")
                .changed()
                && def.check_option
            {
                def.read_only = false;
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_trailing_clauses() {
        assert_eq!(
            split_clause("select 1 x from dual with read only"),
            ("select 1 x from dual", Some(Clause::ReadOnly))
        );
        assert_eq!(
            split_clause("select * from t where a = 1 WITH CHECK OPTION constraint c1\n"),
            ("select * from t where a = 1", Some(Clause::CheckOption))
        );
        assert_eq!(
            split_clause("select 'with read only' x from dual"),
            ("select 'with read only' x from dual", None)
        );
    }

    #[test]
    fn queries_tables() {
        let table = ObjectRef {
            owner: "HR".into(),
            name: "EMP".into(),
            object_type: pythia_db::ObjectType::Table,
        };
        let columns = ["ID".to_owned(), "Name".to_owned()];
        assert_eq!(
            query_on("HR", &table, &columns),
            "select id,\n       \"Name\"\n  from emp"
        );
        assert_eq!(query_on("APP", &table, &[]), "select *\n  from hr.emp");
    }

    #[test]
    fn creates_views() {
        let mut def = ViewDef::new();
        def.name = "active_customers".into();
        def.columns = "id, name".into();
        def.query = "select id, name from customers where active = 'Y';".into();
        def.check_option = true;
        assert_eq!(
            statements("HR", &def).unwrap(),
            [
                "create or replace view HR.ACTIVE_CUSTOMERS (id, name) as\nselect id, name from customers where active = 'Y'\nwith check option"
            ]
        );
        def.read_only = true;
        assert!(statements("HR", &def).is_err());
    }
}
