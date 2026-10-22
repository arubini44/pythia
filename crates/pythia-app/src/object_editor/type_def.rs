//! Type: an object type with attributes, or a collection (`VARRAY`,
//! nested table) of an element type. Becomes the type's first source,
//! written further (and compiled) in a program window.

use egui::{RichText, Ui};

use super::program::source_name;
use super::{form_grid, form_row, ident, name_field, picker};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TypeKind {
    #[default]
    Object,
    Varray,
    Table,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Attribute {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeKind,
    /// An object type's attributes.
    pub attributes: Vec<Attribute>,
    /// Subtypes can be derived from it (`NOT FINAL`).
    pub not_final: bool,
    /// A collection's element type.
    pub element: String,
    /// A `VARRAY`'s maximum size.
    pub limit: u32,
}

impl TypeDef {
    pub fn new() -> Self {
        Self {
            attributes: vec![Attribute {
                name: "id".into(),
                data_type: "number".into(),
            }],
            element: "varchar2(100)".into(),
            limit: 10,
            ..Self::default()
        }
    }
}

pub fn source(owner: &str, def: &TypeDef) -> Result<String, String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    let name = source_name(owner, &def.name);
    match def.kind {
        TypeKind::Object => {
            if def.attributes.is_empty() {
                return Err("An object type needs an attribute".into());
            }
            let mut seen = std::collections::HashSet::new();
            for (i, a) in def.attributes.iter().enumerate() {
                if a.name.trim().is_empty() {
                    return Err(format!("Attribute {} has no name", i + 1));
                }
                if a.data_type.trim().is_empty() {
                    return Err(format!("Attribute {} has no type", a.name.trim()));
                }
                if !seen.insert(ident(&a.name)) {
                    return Err(format!("Attribute {} appears twice", a.name.trim()));
                }
            }
            let lines: Vec<String> = def
                .attributes
                .iter()
                .map(|a| format!("  {} {}", a.name.trim(), a.data_type.trim()))
                .collect();
            let not_final = if def.not_final { " not final" } else { "" };
            Ok(format!(
                "create or replace type {name} as object (\n{}\n){not_final};\n",
                lines.join(",\n")
            ))
        }
        TypeKind::Varray | TypeKind::Table => {
            if def.element.trim().is_empty() {
                return Err("Enter the element type".into());
            }
            let collection = match def.kind {
                TypeKind::Varray => format!("varray({})", def.limit.max(1)),
                _ => "table".to_owned(),
            };
            Ok(format!(
                "create or replace type {name} as {collection} of {};\n",
                def.element.trim()
            ))
        }
    }
}

fn types() -> Vec<String> {
    [
        "number",
        "varchar2(100)",
        "date",
        "timestamp",
        "clob",
        "blob",
        "raw(16)",
    ]
    .iter()
    .map(|t| (*t).to_owned())
    .collect()
}

pub fn ui(ui: &mut Ui, def: &mut TypeDef) {
    let types = types();
    form_grid(ui, "type", |ui| {
        name_field(ui, &mut def.name, false);
        form_row(ui, "Kind", |ui| {
            ui.radio_value(&mut def.kind, TypeKind::Object, "object");
            ui.radio_value(&mut def.kind, TypeKind::Table, "nested table")
                .on_hover_text("A collection of any size");
            ui.radio_value(&mut def.kind, TypeKind::Varray, "varray")
                .on_hover_text("A collection with a maximum size, kept in order");
        });
        match def.kind {
            TypeKind::Object => {
                form_row(ui, "Options", |ui| {
                    ui.checkbox(&mut def.not_final, "not final")
                        .on_hover_text("Other types can be derived from it");
                });
            }
            kind => {
                form_row(ui, "Of", |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut def.element)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("element type")
                            .desired_width(240.0),
                    );
                    if let Some(t) = picker(ui, "element", &types) {
                        def.element = t;
                    }
                });
                if kind == TypeKind::Varray {
                    form_row(ui, "At most", |ui| {
                        ui.add(egui::DragValue::new(&mut def.limit).range(1..=2_147_483_647));
                        ui.weak("elements");
                    });
                }
            }
        }
    });
    if def.kind != TypeKind::Object {
        return;
    }
    ui.add_space(8.0);
    ui.label(RichText::new("Attributes").strong());
    let mut remove = None;
    let mut swap = None;
    let n = def.attributes.len();
    egui::Grid::new("type-attributes")
        .num_columns(4)
        .spacing([8.0, 4.0])
        .show(ui, |ui| {
            let height = ui.spacing().interact_size.y;
            for (i, a) in def.attributes.iter_mut().enumerate() {
                ui.add_sized(
                    [180.0, height],
                    egui::TextEdit::singleline(&mut a.name)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("name"),
                );
                ui.horizontal(|ui| {
                    ui.add_sized(
                        [200.0, height],
                        egui::TextEdit::singleline(&mut a.data_type)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("type"),
                    );
                    if let Some(t) = picker(ui, ("attribute", i), &types) {
                        a.data_type = t;
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(i > 0, egui::Button::new("↑").small())
                        .clicked()
                    {
                        swap = Some((i - 1, i));
                    }
                    if ui
                        .add_enabled(i + 1 < n, egui::Button::new("↓").small())
                        .clicked()
                    {
                        swap = Some((i, i + 1));
                    }
                });
                if ui.small_button("×").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
                ui.end_row();
            }
        });
    if let Some((a, b)) = swap {
        def.attributes.swap(a, b);
    }
    if let Some(i) = remove {
        def.attributes.remove(i);
    }
    if ui.button("Add attribute").clicked() {
        def.attributes.push(Attribute::default());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_types_and_collections() {
        let mut def = TypeDef::new();
        def.name = "address_t".into();
        def.attributes.push(Attribute {
            name: "city".into(),
            data_type: "varchar2(50)".into(),
        });
        def.not_final = true;
        assert_eq!(
            source("HR", &def).unwrap(),
            "create or replace type hr.address_t as object (\n  id number,\n  city varchar2(50)\n) not final;\n"
        );
        def.attributes[1].name = "ID".into();
        assert_eq!(source("HR", &def), Err("Attribute ID appears twice".into()));
        def.kind = TypeKind::Varray;
        def.element = "hr.address_t".into();
        assert_eq!(
            source("HR", &def).unwrap(),
            "create or replace type hr.address_t as varray(10) of hr.address_t;\n"
        );
        def.kind = TypeKind::Table;
        assert_eq!(
            source("HR", &def).unwrap(),
            "create or replace type hr.address_t as table of hr.address_t;\n"
        );
    }
}
