//! Synonym: private or public, pointing at an object (optionally over a
//! database link).

use egui::Ui;
use orui_db::DbError;
use orui_db::oracle::Connection;

use super::{Lists, form_grid, form_row, ident, name_field, picker, qualified};
use crate::documents::quote_ident;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SynonymDef {
    pub public: bool,
    pub name: String,
    pub target_owner: String,
    pub target_name: String,
    pub db_link: String,
}

pub fn load(conn: &Connection, owner: &str, name: &str) -> Result<SynonymDef, DbError> {
    let (target_owner, target_name, db_link) = conn
        .query_row_as::<(Option<String>, String, Option<String>)>(
            "select table_owner, table_name, db_link from all_synonyms
              where owner = :1 and synonym_name = :2",
            &[&owner, &name],
        )
        .map_err(|e| match e.db_error().map(|d| d.code()) {
            Some(1403) => DbError::new(format!("Synonym {owner}.{name} not found")),
            _ => e.into(),
        })?;
    Ok(SynonymDef {
        public: owner == "PUBLIC",
        name: quote_ident(name),
        target_owner: target_owner.map(|o| quote_ident(&o)).unwrap_or_default(),
        target_name: quote_ident(&target_name),
        db_link: db_link.unwrap_or_default().to_lowercase(),
    })
}

pub fn statements(owner: &str, def: &SynonymDef) -> Result<Vec<String>, String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    if def.target_name.trim().is_empty() {
        return Err("Choose the object the synonym points to".into());
    }
    let name = if def.public {
        format!("public synonym {}", quote_ident(&ident(&def.name)))
    } else {
        format!("synonym {}", qualified(owner, &def.name))
    };
    let mut target = match def.target_owner.trim() {
        "" => quote_ident(&ident(&def.target_name)),
        o => qualified(&ident(o), &def.target_name),
    };
    if !def.db_link.trim().is_empty() {
        target = format!("{target}@{}", def.db_link.trim());
    }
    Ok(vec![format!("create or replace {name} for {target}")])
}

pub fn ui(ui: &mut Ui, def: &mut SynonymDef, existing: bool, lists: &mut Lists) {
    form_grid(ui, "synonym", |ui| {
        form_row(ui, "Visibility", |ui| {
            ui.add_enabled_ui(!existing, |ui| {
                ui.radio_value(&mut def.public, false, "private");
                ui.radio_value(&mut def.public, true, "public")
                    .on_hover_text("Visible to every user; needs CREATE PUBLIC SYNONYM");
            });
        });
        name_field(ui, &mut def.name, existing);
        form_row(ui, "Object owner", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.target_owner)
                    .hint_text("your schema")
                    .desired_width(200.0),
            );
            if let Some(owner) = picker(ui, "owner", &lists.owners) {
                def.target_owner = quote_ident(&owner);
            }
        });
        form_row(ui, "Object", |ui| {
            ui.add(egui::TextEdit::singleline(&mut def.target_name).desired_width(200.0));
            let owner = match ident(&def.target_owner) {
                o if o.is_empty() => lists.schema.clone(),
                o => o,
            };
            match lists.objects.get(&owner) {
                Some(objects) => {
                    if let Some(name) = picker(ui, "object", objects) {
                        def.target_name = quote_ident(&name);
                    }
                }
                None => {
                    lists.want_objects = Some(owner);
                    ui.spinner();
                }
            }
        });
        form_row(ui, "Database link", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.db_link)
                    .hint_text("none")
                    .desired_width(200.0),
            );
            if !lists.db_links.is_empty()
                && let Some(link) = picker(ui, "link", &lists.db_links)
            {
                def.db_link = link.to_lowercase();
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_synonyms() {
        let def = SynonymDef {
            public: false,
            name: "emp".into(),
            target_owner: "hr".into(),
            target_name: "employees".into(),
            db_link: String::new(),
        };
        assert_eq!(
            statements("APP", &def).unwrap(),
            ["create or replace synonym APP.EMP for HR.EMPLOYEES"]
        );
        let def = SynonymDef {
            public: true,
            target_owner: String::new(),
            db_link: "prod.example.com".into(),
            ..def
        };
        assert_eq!(
            statements("APP", &def).unwrap(),
            ["create or replace public synonym EMP for EMPLOYEES@prod.example.com"]
        );
    }
}
