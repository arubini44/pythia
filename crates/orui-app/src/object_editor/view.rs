//! View: column aliases, query and the `FORCE` / `WITH READ ONLY` /
//! `WITH CHECK OPTION` clauses; always `CREATE OR REPLACE`.

use egui::Ui;
use orui_db::DbError;
use orui_db::oracle::Connection;

use super::{form_grid, form_row, name_field, qualified};
use crate::documents::quote_ident;

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
    let text = conn
        .query_row_as::<Option<String>>(
            "select text from all_views where owner = :1 and view_name = :2",
            &[&owner, &name],
        )
        .map_err(|e| match e.db_error().map(|d| d.code()) {
            Some(1403) => DbError::new(format!("View {owner}.{name} not found")),
            _ => e.into(),
        })?
        .unwrap_or_default();
    // The clauses are constraints of the view: O = read only, V = check option.
    let mut read_only = false;
    let mut check_option = false;
    for kind in conn.query_as::<String>(
        "select constraint_type from all_constraints
          where owner = :1 and table_name = :2 and constraint_type in ('O', 'V')",
        &[&owner, &name],
    )? {
        match kind?.as_str() {
            "O" => read_only = true,
            _ => check_option = true,
        }
    }
    Ok(ViewDef {
        name: quote_ident(name),
        columns: String::new(),
        query: strip_clauses(&text).to_owned(),
        force: false,
        read_only,
        check_option,
    })
}

/// Removes a trailing `WITH READ ONLY` / `WITH CHECK OPTION [CONSTRAINT x]`
/// that newer versions keep in `ALL_VIEWS.TEXT`.
fn strip_clauses(text: &str) -> &str {
    let trimmed = text.trim_end();
    let lower = trimmed.to_lowercase();
    for clause in ["with read only", "with check option"] {
        if let Some(at) = lower.rfind(clause) {
            let rest = lower[at + clause.len()..].trim();
            let tail_ok = rest.is_empty()
                || rest.starts_with("constraint") && rest.split_whitespace().count() == 2;
            if tail_ok {
                return trimmed[..at].trim_end();
            }
        }
    }
    trimmed
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
    fn strips_trailing_clauses() {
        assert_eq!(
            strip_clauses("select 1 x from dual with read only"),
            "select 1 x from dual"
        );
        assert_eq!(
            strip_clauses("select * from t where a = 1 WITH CHECK OPTION constraint c1\n"),
            "select * from t where a = 1"
        );
        assert_eq!(
            strip_clauses("select 'with read only' x from dual"),
            "select 'with read only' x from dual"
        );
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
