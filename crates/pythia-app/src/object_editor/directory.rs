//! Directory: a name for a folder on the database server, used by
//! UTL_FILE, external tables and Data Pump. Created and changed alike with
//! `CREATE OR REPLACE DIRECTORY`, which keeps its grants.

use egui::Ui;
use pythia_db::DbError;
use pythia_db::oracle::Connection;

use super::{Lists, form_grid, form_row, ident, name_field, names};
use crate::documents::quote_ident;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct DirectoryDef {
    pub name: String,
    /// The folder on the database server.
    pub path: String,
}

pub fn load(conn: &Connection, name: &str) -> Result<DirectoryDef, DbError> {
    let path = conn
        .query_as::<Option<String>>(
            "select directory_path from all_directories where directory_name = :1",
            &[&name],
        )?
        .next()
        .transpose()?
        .ok_or_else(|| DbError::new(format!("Directory {name} not found")))?;
    Ok(DirectoryDef {
        name: name.to_owned(),
        path: path.unwrap_or_default(),
    })
}

/// What the session may do.
pub fn lists(conn: &Connection, lists: &mut super::BaseLists) -> Result<(), DbError> {
    lists.session_privileges = names(conn, "select privilege from session_privs", &[])?;
    Ok(())
}

pub fn statements(
    original: Option<&DirectoryDef>,
    def: &DirectoryDef,
) -> Result<Vec<String>, String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    let path = def.path.trim();
    if path.is_empty() {
        return Err("Enter the folder's path on the database server".into());
    }
    if original.is_some_and(|o| o.path.trim() == path) {
        return Ok(Vec::new());
    }
    Ok(vec![format!(
        "create or replace directory {} as '{}'",
        quote_ident(&ident(&def.name)),
        path.replace('\'', "''")
    )])
}

/// What the session can't do that the form needs.
pub fn missing_privilege(lists: &Lists) -> Option<String> {
    (!lists.session_privileges.is_empty()
        && !lists
            .session_privileges
            .iter()
            .any(|p| p == "CREATE ANY DIRECTORY"))
    .then(|| "This session doesn't have CREATE ANY DIRECTORY.".to_owned())
}

pub fn ui(ui: &mut Ui, def: &mut DirectoryDef, existing: bool, lists: &Lists) {
    if let Some(missing) = missing_privilege(lists) {
        ui.colored_label(ui.visuals().warn_fg_color, missing);
        ui.add_space(4.0);
    }
    form_grid(ui, "directory", |ui| {
        name_field(ui, &mut def.name, existing);
        form_row(ui, "Path", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.path)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("/u01/app/exports")
                    .desired_width(420.0),
            );
        });
    });
    ui.add_space(6.0);
    ui.weak(
        "The path is on the database server, and Oracle doesn't check it: the folder must \
         exist and the server's operating system user must be able to use it.",
    );
    if existing {
        ui.weak("A new path replaces the old one; the directory's grants stay.");
    } else {
        ui.weak("Grants… in its context menu lets others read or write it.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_moves_directories() {
        let def = DirectoryDef {
            name: "exports".into(),
            path: " /u01/it's here ".into(),
        };
        assert_eq!(
            statements(None, &def).unwrap(),
            ["create or replace directory EXPORTS as '/u01/it''s here'"]
        );
        let original = DirectoryDef {
            name: "EXPORTS".into(),
            path: "/u01/exports".into(),
        };
        assert!(statements(Some(&original), &original).unwrap().is_empty());
        let moved = DirectoryDef {
            path: "/u02/exports".into(),
            ..original.clone()
        };
        assert_eq!(
            statements(Some(&original), &moved).unwrap(),
            ["create or replace directory EXPORTS as '/u02/exports'"]
        );
        assert_eq!(
            statements(None, &DirectoryDef::default()).unwrap_err(),
            "Enter a name"
        );
        let no_path = DirectoryDef {
            name: "x".into(),
            path: " ".into(),
        };
        assert_eq!(
            statements(None, &no_path).unwrap_err(),
            "Enter the folder's path on the database server"
        );
    }
}
