//! Type: an object type with attributes and methods (member, static,
//! constructors, a map or order method), or a collection (`VARRAY`,
//! nested table) of an element type. Becomes the type's first source and,
//! with methods, a body with a stub per method, written further (and
//! compiled) in a program window.

use egui::{RichText, Ui};

use super::program::{Mode, Param, end_name, params, params_ui, returns_ui, source_name};
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

/// What a method is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MethodKind {
    /// Called on an object (`SELF`).
    #[default]
    Member,
    /// Called on the type.
    Static,
    /// Builds an object: `CONSTRUCTOR FUNCTION t(…) RETURN SELF AS RESULT`.
    Constructor,
    /// Gives objects an order by mapping them to a scalar.
    Map,
    /// Compares an object with another one: negative, zero or positive.
    Order,
}

impl MethodKind {
    fn label(self) -> &'static str {
        match self {
            MethodKind::Member => "member",
            MethodKind::Static => "static",
            MethodKind::Constructor => "constructor",
            MethodKind::Map => "map",
            MethodKind::Order => "order",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Method {
    pub kind: MethodKind,
    /// A member or static function (the others are always functions).
    pub function: bool,
    /// Empty for a constructor (it's named after the type).
    pub name: String,
    pub params: Vec<Param>,
    pub returns: String,
}

impl Method {
    fn is_function(&self) -> bool {
        match self.kind {
            MethodKind::Member | MethodKind::Static => self.function,
            _ => true,
        }
    }

    /// `member function f(…) return t`, without `is` or `,`; `type_name`
    /// is the bare name of the type.
    fn header(&self, type_name: &str, indent: &str) -> String {
        match self.kind {
            MethodKind::Constructor => format!(
                "constructor function {type_name}{} return self as result",
                params(&self.params, indent)
            ),
            MethodKind::Map => format!(
                "map member function {} return {}",
                self.name.trim(),
                self.returns.trim()
            ),
            MethodKind::Order => format!(
                "order member function {}(p_other {type_name}) return integer",
                self.name.trim()
            ),
            kind => {
                let what = if self.function {
                    "function"
                } else {
                    "procedure"
                };
                let mut text = format!(
                    "{} {what} {}{}",
                    kind.label(),
                    self.name.trim(),
                    params(&self.params, indent)
                );
                if self.function {
                    text.push_str(&format!(" return {}", self.returns.trim()));
                }
                text
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDef {
    pub name: String,
    pub kind: TypeKind,
    /// An object type's attributes.
    pub attributes: Vec<Attribute>,
    pub methods: Vec<Method>,
    /// Subtypes can be derived from it (`NOT FINAL`).
    pub not_final: bool,
    /// Objects of it can be made (else only of its subtypes).
    pub instantiable: bool,
    /// A collection's element type.
    pub element: String,
    /// A `VARRAY`'s maximum size.
    pub limit: u32,
}

impl Default for TypeDef {
    fn default() -> Self {
        Self {
            name: String::new(),
            kind: TypeKind::Object,
            attributes: Vec::new(),
            methods: Vec::new(),
            not_final: false,
            instantiable: true,
            element: String::new(),
            limit: 0,
        }
    }
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

fn validate_methods(def: &TypeDef) -> Result<(), String> {
    if def
        .methods
        .iter()
        .filter(|m| matches!(m.kind, MethodKind::Map | MethodKind::Order))
        .count()
        > 1
    {
        return Err("A type has one map or order method".into());
    }
    for (i, m) in def.methods.iter().enumerate() {
        let what = if m.kind == MethodKind::Constructor {
            "constructor".to_owned()
        } else if m.name.trim().is_empty() {
            return Err(format!("Method {} has no name", i + 1));
        } else {
            m.name.trim().to_owned()
        };
        if m.is_function()
            && m.kind != MethodKind::Constructor
            && m.kind != MethodKind::Order
            && m.returns.trim().is_empty()
        {
            return Err(format!("{what}: enter the return type"));
        }
        let mut seen = std::collections::HashSet::new();
        for (j, p) in m.params.iter().enumerate() {
            if p.name.trim().is_empty() {
                return Err(format!("{what}: parameter {} has no name", j + 1));
            }
            if p.data_type.trim().is_empty() {
                return Err(format!("{what}: parameter {} has no type", p.name.trim()));
            }
            if !seen.insert(ident(&p.name)) {
                return Err(format!("{what}: parameter {} appears twice", p.name.trim()));
            }
        }
    }
    Ok(())
}

/// The type's first sources: the spec, and with methods the body.
pub fn sources(owner: &str, def: &TypeDef) -> Result<Vec<String>, String> {
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
            validate_methods(def)?;
            if !def.instantiable && !def.not_final {
                return Err("A type that isn't instantiable must be not final".into());
            }
            let bare = end_name(&def.name);
            let lines: Vec<String> = def
                .attributes
                .iter()
                .map(|a| format!("  {} {}", a.name.trim(), a.data_type.trim()))
                .chain(
                    def.methods
                        .iter()
                        .map(|m| format!("  {}", m.header(&bare, "  "))),
                )
                .collect();
            let mut options = String::new();
            if def.not_final {
                options.push_str(" not final");
            }
            if !def.instantiable {
                options.push_str(" not instantiable");
            }
            let spec = format!(
                "create or replace type {name} as object (\n{}\n){options};\n",
                lines.join(",\n")
            );
            if def.methods.is_empty() {
                return Ok(vec![spec]);
            }
            let stubs: Vec<String> = def
                .methods
                .iter()
                .map(|m| {
                    let result = match m.kind {
                        MethodKind::Constructor => "return;",
                        _ if m.is_function() => "return null; -- TODO: implement",
                        _ => "null; -- TODO: implement",
                    };
                    let end = match m.kind {
                        MethodKind::Constructor => "end;".to_owned(),
                        _ => format!("end {};", end_name(&m.name)),
                    };
                    format!(
                        "  {} is\n  begin\n    {result}\n  {end}\n",
                        m.header(&bare, "  ")
                    )
                })
                .collect();
            let body = format!(
                "create or replace type body {name} as\n{}end;\n",
                stubs.join("\n")
            );
            Ok(vec![spec, body])
        }
        TypeKind::Varray | TypeKind::Table => {
            if def.element.trim().is_empty() {
                return Err("Enter the element type".into());
            }
            let collection = match def.kind {
                TypeKind::Varray => format!("varray({})", def.limit.max(1)),
                _ => "table".to_owned(),
            };
            Ok(vec![format!(
                "create or replace type {name} as {collection} of {};\n",
                def.element.trim()
            )])
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
                    let mut abstract_type = !def.instantiable;
                    if ui
                        .checkbox(&mut abstract_type, "not instantiable")
                        .on_hover_text("Only its subtypes have objects (needs not final)")
                        .changed()
                    {
                        def.instantiable = !abstract_type;
                        if abstract_type {
                            def.not_final = true;
                        }
                    }
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
    ui.add_space(8.0);
    methods_ui(ui, def);
}

/// The methods: kind, name, parameters, return type; a body stub each.
fn methods_ui(ui: &mut Ui, def: &mut TypeDef) {
    ui.label(RichText::new("Methods").strong());
    ui.weak("Declared in the type; the body gets a stub for each.");
    let mut remove = None;
    let has_map_or_order = def
        .methods
        .iter()
        .any(|m| matches!(m.kind, MethodKind::Map | MethodKind::Order));
    for (i, m) in def.methods.iter_mut().enumerate() {
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt(("method-kind", i))
                    .selected_text(m.kind.label())
                    .width(110.0)
                    .show_ui(ui, |ui| {
                        for (kind, hint) in [
                            (MethodKind::Member, "Called on an object"),
                            (MethodKind::Static, "Called on the type"),
                            (MethodKind::Constructor, "Builds an object"),
                            (MethodKind::Map, "Orders objects by a scalar it returns"),
                            (MethodKind::Order, "Compares an object with another"),
                        ] {
                            ui.selectable_value(&mut m.kind, kind, kind.label())
                                .on_hover_text(hint);
                        }
                    });
                if matches!(m.kind, MethodKind::Member | MethodKind::Static) {
                    ui.radio_value(&mut m.function, false, "procedure");
                    ui.radio_value(&mut m.function, true, "function");
                }
                if m.kind == MethodKind::Constructor {
                    ui.weak("named after the type, returns self");
                } else {
                    ui.add(
                        egui::TextEdit::singleline(&mut m.name)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("name")
                            .desired_width(180.0),
                    );
                }
                let returns = match m.kind {
                    MethodKind::Map => true,
                    MethodKind::Member | MethodKind::Static => m.function,
                    _ => false,
                };
                if returns {
                    ui.label("returns");
                    returns_ui(ui, &format!("method-{i}"), &mut m.returns);
                } else if m.kind == MethodKind::Order {
                    ui.weak("(p_other) returns integer");
                }
                if ui.small_button("×").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
            });
            if !matches!(m.kind, MethodKind::Map | MethodKind::Order) {
                params_ui(ui, &format!("method-params-{i}"), &mut m.params);
            }
        });
    }
    if let Some(i) = remove {
        def.methods.remove(i);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Add member function").clicked() {
            def.methods.push(Method {
                function: true,
                returns: "number".into(),
                ..Method::default()
            });
        }
        if ui.button("Add member procedure").clicked() {
            def.methods.push(Method::default());
        }
        if ui.button("Add constructor").clicked() {
            let params = def
                .attributes
                .iter()
                .map(|a| Param {
                    name: format!("p_{}", a.name.trim().to_lowercase()),
                    mode: Mode::In,
                    data_type: a.data_type.trim().to_owned(),
                    default: String::new(),
                })
                .collect();
            def.methods.push(Method {
                kind: MethodKind::Constructor,
                params,
                ..Method::default()
            });
        }
        if ui
            .add_enabled(!has_map_or_order, egui::Button::new("Add map method"))
            .on_disabled_hover_text("A type has one map or order method")
            .clicked()
        {
            def.methods.push(Method {
                kind: MethodKind::Map,
                name: "sort_key".into(),
                returns: "varchar2".into(),
                ..Method::default()
            });
        }
    });
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
            sources("HR", &def).unwrap()[0],
            "create or replace type hr.address_t as object (\n  id number,\n  city varchar2(50)\n) not final;\n"
        );
        def.attributes[1].name = "ID".into();
        assert_eq!(
            sources("HR", &def).map(|s| s[0].clone()),
            Err("Attribute ID appears twice".into())
        );
        // Methods: a body with a stub per method.
        def.attributes.truncate(1);
        def.methods = vec![
            Method {
                function: true,
                name: "label".into(),
                returns: "varchar2".into(),
                ..Method::default()
            },
            Method {
                kind: MethodKind::Static,
                name: "reset".into(),
                params: vec![Param {
                    name: "p_id".into(),
                    mode: Mode::In,
                    data_type: "number".into(),
                    default: String::new(),
                }],
                ..Method::default()
            },
            Method {
                kind: MethodKind::Constructor,
                params: vec![Param {
                    name: "p_id".into(),
                    mode: Mode::In,
                    data_type: "number".into(),
                    default: String::new(),
                }],
                ..Method::default()
            },
            Method {
                kind: MethodKind::Order,
                name: "compare".into(),
                ..Method::default()
            },
        ];
        def.instantiable = false;
        let s = sources("HR", &def).unwrap();
        assert_eq!(
            s[0],
            "create or replace type hr.address_t as object (\n  id number,\n  \
             member function label return varchar2,\n  static procedure reset(\n    \
             p_id in number\n  ),\n  constructor function address_t(\n    p_id in number\n  ) \
             return self as result,\n  order member function compare(p_other address_t) return integer\n) \
             not final not instantiable;\n"
        );
        assert!(s[1].starts_with("create or replace type body hr.address_t as\n  member function label return varchar2 is\n  begin\n    return null;"), "{}", s[1]);
        assert!(s[1].contains("  end label;\n"), "{}", s[1]);
        assert!(
            s[1].contains("return self as result is\n  begin\n    return;\n  end;\n"),
            "{}",
            s[1]
        );
        assert!(s[1].ends_with("end compare;\nend;\n"), "{}", s[1]);
        def.not_final = false;
        assert!(
            sources("HR", &def).is_err(),
            "not instantiable needs not final"
        );
        def.not_final = true;
        def.methods.push(Method {
            kind: MethodKind::Map,
            name: "k".into(),
            returns: "number".into(),
            ..Method::default()
        });
        assert!(sources("HR", &def).is_err(), "map and order");
        def.methods.clear();
        def.kind = TypeKind::Varray;
        def.element = "hr.address_t".into();
        assert_eq!(
            sources("HR", &def).unwrap()[0],
            "create or replace type hr.address_t as varray(10) of hr.address_t;\n"
        );
        def.kind = TypeKind::Table;
        assert_eq!(
            sources("HR", &def).unwrap()[0],
            "create or replace type hr.address_t as table of hr.address_t;\n"
        );
    }
}
