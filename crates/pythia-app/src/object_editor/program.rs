//! Procedures, functions and packages: name, parameters, return type and
//! `AUTHID` become the unit's first source, which is written further (and
//! compiled) in a program window.

use egui::{RichText, Ui};
use pythia_db::ObjectType;

use super::{form_grid, form_row, ident, name_field, picker, qualified};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    In,
    Out,
    InOut,
}

impl Mode {
    fn keyword(self) -> &'static str {
        match self {
            Mode::In => "in",
            Mode::Out => "out",
            Mode::InOut => "in out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Param {
    pub name: String,
    pub mode: Mode,
    pub data_type: String,
    /// Empty: none.
    pub default: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Subprogram {
    pub function: bool,
    pub name: String,
    pub params: Vec<Param>,
    /// A function's return type.
    pub returns: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProgramDef {
    /// Procedure, function or package.
    pub kind: ObjectType,
    pub name: String,
    /// `AUTHID CURRENT_USER`: runs with the caller's privileges.
    pub invoker_rights: bool,
    /// The procedure or function itself (one), or a package's first
    /// subprograms.
    pub subprograms: Vec<Subprogram>,
}

impl ProgramDef {
    pub fn new(kind: ObjectType) -> Self {
        let subprograms = match kind {
            ObjectType::Package => Vec::new(),
            _ => vec![Subprogram {
                function: kind == ObjectType::Function,
                returns: if kind == ObjectType::Function {
                    "number".into()
                } else {
                    String::new()
                },
                ..Subprogram::default()
            }],
        };
        Self {
            kind,
            name: String::new(),
            invoker_rights: false,
            subprograms,
        }
    }
}

/// The name a source refers to: `hr.calc_bonus`, quoted parts as they are.
pub fn source_name(owner: &str, name: &str) -> String {
    let lower = |part: &str| {
        if part.starts_with('"') {
            part.to_owned()
        } else {
            part.to_lowercase()
        }
    };
    let qualified = qualified(owner, name);
    // The owner never contains a dot unquoted, so the first one splits.
    match qualified.split_once('.') {
        Some((o, n)) if !o.contains('"') || o.ends_with('"') => {
            format!("{}.{}", lower(o), lower(n))
        }
        _ => lower(&qualified),
    }
}

/// The unqualified name as `END name` repeats it.
pub(super) fn end_name(name: &str) -> String {
    let n = crate::documents::quote_ident(&ident(name));
    if n.starts_with('"') {
        n
    } else {
        n.to_lowercase()
    }
}

/// Parameter list (with a leading line break), or nothing.
pub(super) fn params(list: &[Param], indent: &str) -> String {
    if list.is_empty() {
        return String::new();
    }
    let lines: Vec<String> = list
        .iter()
        .map(|p| {
            let default = match p.default.trim() {
                "" => String::new(),
                d => format!(" default {d}"),
            };
            format!(
                "{indent}  {} {} {}{default}",
                p.name.trim(),
                p.mode.keyword(),
                p.data_type.trim()
            )
        })
        .collect();
    format!("(\n{}\n{indent})", lines.join(",\n"))
}

/// `procedure p(…)` / `function f(…) return t`, without `is` or `;`.
fn header(sub: &Subprogram, name: &str, indent: &str) -> String {
    let kind = if sub.function {
        "function"
    } else {
        "procedure"
    };
    let mut text = format!("{kind} {name}{}", params(&sub.params, indent));
    if sub.function {
        text.push_str(&format!(
            "{}return {}",
            if sub.params.is_empty() {
                " ".to_owned()
            } else {
                format!("\n{indent}")
            },
            sub.returns.trim()
        ));
    }
    text
}

fn validate(def: &ProgramDef) -> Result<(), String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    for (i, sub) in def.subprograms.iter().enumerate() {
        let what = if def.kind == ObjectType::Package {
            if sub.name.trim().is_empty() {
                return Err(format!("Subprogram {} has no name", i + 1));
            }
            format!("{}: ", sub.name.trim())
        } else {
            String::new()
        };
        let mut seen = std::collections::HashSet::new();
        for (j, p) in sub.params.iter().enumerate() {
            if p.name.trim().is_empty() {
                return Err(format!("{what}parameter {} has no name", j + 1));
            }
            if p.data_type.trim().is_empty() {
                return Err(format!("{what}parameter {} has no type", p.name.trim()));
            }
            if !seen.insert(ident(&p.name)) {
                return Err(format!("{what}parameter {} appears twice", p.name.trim()));
            }
        }
        if sub.function && sub.returns.trim().is_empty() {
            return Err(format!("{what}enter the return type"));
        }
    }
    Ok(())
}

/// The first sources: the unit, or a package's spec and body.
pub fn sources(owner: &str, def: &ProgramDef) -> Result<Vec<String>, String> {
    validate(def)?;
    let name = source_name(owner, &def.name);
    let end = end_name(&def.name);
    let authid = if def.invoker_rights {
        "\nauthid current_user"
    } else {
        ""
    };
    match def.kind {
        ObjectType::Package => {
            let declarations: Vec<String> = def
                .subprograms
                .iter()
                .map(|s| format!("  {};", header(s, s.name.trim(), "  ")))
                .collect();
            let declarations = if declarations.is_empty() {
                String::new()
            } else {
                format!("{}\n", declarations.join("\n\n"))
            };
            let spec =
                format!("create or replace package {name}{authid}\nis\n{declarations}end {end};\n");
            let body = match crate::stubs::package_body(&spec, None) {
                Some(stubs) => {
                    // The generator names the body as the outline does,
                    // without the schema.
                    let rest = stubs
                        .body
                        .split_once('\n')
                        .map_or("", |(_, rest)| rest)
                        .to_owned();
                    format!("create or replace package body {name} as\n{rest}")
                }
                None => format!("create or replace package body {name} as\nend {end};\n"),
            };
            Ok(vec![spec, body])
        }
        _ => {
            let sub = def.subprograms.first().cloned().unwrap_or_default();
            let result = if sub.function {
                "return null;"
            } else {
                "null;"
            };
            Ok(vec![format!(
                "create or replace {}{authid}\nis\nbegin\n  {result}\nend {end};\n",
                header(&sub, &name, "")
            )])
        }
    }
}

/// Types offered for parameters and return values.
fn types() -> Vec<String> {
    [
        "number",
        "pls_integer",
        "varchar2",
        "date",
        "timestamp",
        "boolean",
        "clob",
        "blob",
        "sys_refcursor",
    ]
    .iter()
    .map(|t| (*t).to_owned())
    .collect()
}

pub(super) fn params_ui(ui: &mut Ui, id: &str, list: &mut Vec<Param>) {
    let types = types();
    let mut remove = None;
    let mut swap = None;
    let n = list.len();
    if !list.is_empty() {
        egui::Grid::new(id)
            .num_columns(6)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for title in ["Name", "Mode", "Type", "Default", "", ""] {
                    ui.strong(title);
                }
                ui.end_row();
                let height = ui.spacing().interact_size.y;
                for (i, p) in list.iter_mut().enumerate() {
                    ui.add_sized(
                        [160.0, height],
                        egui::TextEdit::singleline(&mut p.name)
                            .font(egui::TextStyle::Monospace)
                            .hint_text("p_name"),
                    );
                    egui::ComboBox::from_id_salt((id, "mode", i))
                        .selected_text(p.mode.keyword())
                        .width(70.0)
                        .show_ui(ui, |ui| {
                            for mode in [Mode::In, Mode::Out, Mode::InOut] {
                                ui.selectable_value(&mut p.mode, mode, mode.keyword());
                            }
                        });
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [220.0, height],
                            egui::TextEdit::singleline(&mut p.data_type)
                                .font(egui::TextStyle::Monospace)
                                .hint_text("type or table.column%type"),
                        );
                        if let Some(t) = picker(ui, (id, "type", i), &types) {
                            p.data_type = t;
                        }
                    });
                    ui.add_enabled_ui(p.mode == Mode::In, |ui| {
                        ui.add_sized(
                            [110.0, height],
                            egui::TextEdit::singleline(&mut p.default)
                                .font(egui::TextStyle::Monospace)
                                .hint_text("none"),
                        )
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
    }
    if let Some((a, b)) = swap {
        list.swap(a, b);
    }
    if let Some(i) = remove {
        list.remove(i);
    }
    if ui.button("Add parameter").clicked() {
        list.push(Param {
            name: format!("p_{}", list.len() + 1),
            data_type: "number".into(),
            ..Param::default()
        });
    }
}

pub(super) fn returns_ui(ui: &mut Ui, id: &str, returns: &mut String) {
    ui.add(
        egui::TextEdit::singleline(returns)
            .font(egui::TextStyle::Monospace)
            .desired_width(200.0),
    );
    if let Some(t) = picker(ui, (id, "returns"), &types()) {
        *returns = t;
    }
}

pub fn ui(ui: &mut Ui, def: &mut ProgramDef) {
    form_grid(ui, "program", |ui| {
        name_field(ui, &mut def.name, false);
        if def.kind == ObjectType::Function
            && let Some(sub) = def.subprograms.first_mut()
        {
            form_row(ui, "Returns", |ui| {
                returns_ui(ui, "function", &mut sub.returns)
            });
        }
        form_row(ui, "Rights", |ui| {
            ui.radio_value(&mut def.invoker_rights, false, "definer")
                .on_hover_text("Runs with the owner's privileges (the default)");
            ui.radio_value(&mut def.invoker_rights, true, "current user")
                .on_hover_text("AUTHID CURRENT_USER: runs with the caller's privileges");
        });
    });
    ui.add_space(8.0);
    if def.kind != ObjectType::Package {
        ui.label(RichText::new("Parameters").strong());
        if let Some(sub) = def.subprograms.first_mut() {
            params_ui(ui, "params", &mut sub.params);
        }
        return;
    }
    ui.label(RichText::new("Subprograms").strong());
    ui.weak("Declared in the specification; the body gets a stub for each.");
    let mut remove = None;
    for (i, sub) in def.subprograms.iter_mut().enumerate() {
        ui.add_space(4.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.radio_value(&mut sub.function, false, "procedure");
                ui.radio_value(&mut sub.function, true, "function");
                ui.add(
                    egui::TextEdit::singleline(&mut sub.name)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("name")
                        .desired_width(200.0),
                );
                if sub.function {
                    ui.label("returns");
                    returns_ui(ui, &format!("sub-{i}"), &mut sub.returns);
                }
                if ui.small_button("×").on_hover_text("Remove").clicked() {
                    remove = Some(i);
                }
            });
            params_ui(ui, &format!("sub-params-{i}"), &mut sub.params);
        });
    }
    if let Some(i) = remove {
        def.subprograms.remove(i);
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("Add procedure").clicked() {
            def.subprograms.push(Subprogram::default());
        }
        if ui.button("Add function").clicked() {
            def.subprograms.push(Subprogram {
                function: true,
                returns: "number".into(),
                ..Subprogram::default()
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(name: &str, mode: Mode, data_type: &str, default: &str) -> Param {
        Param {
            name: name.into(),
            mode,
            data_type: data_type.into(),
            default: default.into(),
        }
    }

    #[test]
    fn procedures_and_functions() {
        let mut def = ProgramDef::new(ObjectType::Procedure);
        assert!(sources("HR", &def).is_err());
        def.name = "raise_salary".into();
        assert_eq!(
            sources("HR", &def).unwrap(),
            [
                "create or replace procedure hr.raise_salary\nis\nbegin\n  null;\nend raise_salary;\n"
            ]
        );
        def.subprograms[0].params = vec![
            param("p_emp_id", Mode::In, "employees.employee_id%type", ""),
            param("p_pct", Mode::In, "number", "10"),
            param("p_new", Mode::Out, "number", ""),
        ];
        def.invoker_rights = true;
        assert_eq!(
            sources("HR", &def).unwrap(),
            [
                "create or replace procedure hr.raise_salary(\n  p_emp_id in employees.employee_id%type,\n  \
              p_pct in number default 10,\n  p_new out number\n)\nauthid current_user\nis\nbegin\n  null;\n\
              end raise_salary;\n"
            ]
        );
        def.subprograms[0].params[2].name = "P_PCT".into();
        assert_eq!(
            sources("HR", &def),
            Err("parameter P_PCT appears twice".into())
        );

        let mut f = ProgramDef::new(ObjectType::Function);
        f.name = "\"Bonus\"".into();
        assert_eq!(
            sources("HR", &f).unwrap(),
            [
                "create or replace function hr.\"Bonus\" return number\nis\nbegin\n  return null;\nend \"Bonus\";\n"
            ]
        );
        f.subprograms[0].returns.clear();
        assert_eq!(sources("HR", &f), Err("enter the return type".into()));
    }

    #[test]
    fn packages_get_a_body() {
        let mut def = ProgramDef::new(ObjectType::Package);
        def.name = "orders_api".into();
        def.subprograms = vec![
            Subprogram {
                function: false,
                name: "cancel".into(),
                params: vec![param("p_id", Mode::In, "number", "")],
                returns: String::new(),
            },
            Subprogram {
                function: true,
                name: "total".into(),
                params: Vec::new(),
                returns: "number".into(),
            },
        ];
        let s = sources("APP", &def).unwrap();
        assert_eq!(
            s[0],
            "create or replace package app.orders_api\nis\n  procedure cancel(\n    p_id in number\n  );\n\n  \
             function total return number;\nend orders_api;\n"
        );
        assert!(
            s[1].starts_with("create or replace package body app.orders_api as\n"),
            "{}",
            s[1]
        );
        assert!(s[1].contains("procedure cancel("), "{}", s[1]);
        assert!(s[1].contains("function total return number is"), "{}", s[1]);
        assert!(s[1].trim_end().ends_with("end orders_api;"), "{}", s[1]);

        def.subprograms.clear();
        let s = sources("APP", &def).unwrap();
        assert_eq!(
            s[0],
            "create or replace package app.orders_api\nis\nend orders_api;\n"
        );
        assert!(s[1].starts_with("create or replace package body app.orders_api as\n"));
    }
}
