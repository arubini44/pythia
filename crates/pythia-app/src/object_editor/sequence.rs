//! Sequence: increment, bounds, cycle, cache and order; `RESTART` for an
//! existing sequence.

use egui::Ui;
use pythia_db::DbError;
use pythia_db::oracle::Connection;

use super::{form_grid, form_row, is_integer, name_field, qualified};
use crate::documents::quote_ident;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SequenceDef {
    pub name: String,
    pub increment: String,
    /// Empty: no bound.
    pub min: String,
    pub max: String,
    /// First value of a new sequence, or the value to restart an existing
    /// one at (empty: keep going).
    pub start: String,
    pub cycle: bool,
    /// 0: `NOCACHE`.
    pub cache: u32,
    pub order: bool,
    /// Next value the sequence hands out (`LAST_NUMBER`), for information.
    pub last_number: Option<String>,
}

impl SequenceDef {
    pub fn new() -> Self {
        Self {
            increment: "1".into(),
            start: "1".into(),
            cache: 20,
            ..Self::default()
        }
    }
}

pub fn load(conn: &Connection, owner: &str, name: &str) -> Result<SequenceDef, DbError> {
    let (min, max, increment, cycle, order, cache, last) = conn
        .query_row_as::<(String, String, String, String, String, u32, String)>(
            "select to_char(min_value), to_char(max_value), to_char(increment_by), cycle_flag,
                    order_flag, cache_size, to_char(last_number)
               from all_sequences where sequence_owner = :1 and sequence_name = :2",
            &[&owner, &name],
        )
        .map_err(|e| match e.db_error().map(|d| d.code()) {
            Some(1403) => DbError::new(format!("Sequence {owner}.{name} not found")),
            _ => e.into(),
        })?;
    let descending = increment.starts_with('-');
    // Oracle stores "no bound" as a huge value (or -1 as the top of a
    // descending sequence); show those as empty.
    let unbounded = |v: &str| {
        let digits = v.trim_start_matches('-');
        digits.len() >= 27 && digits.chars().all(|c| c == '9')
    };
    Ok(SequenceDef {
        name: quote_ident(name),
        increment,
        min: if unbounded(&min) { String::new() } else { min },
        max: if unbounded(&max) || descending && max == "-1" {
            String::new()
        } else {
            max
        },
        start: String::new(),
        cycle: cycle == "Y",
        cache,
        order: order == "Y",
        last_number: Some(last),
    })
}

/// Statements creating the sequence, or altering `original` into `def`.
pub fn statements(
    owner: &str,
    original: Option<&SequenceDef>,
    def: &SequenceDef,
) -> Result<Vec<String>, String> {
    if def.name.trim().is_empty() {
        return Err("Enter a name".into());
    }
    for (label, value, optional) in [
        ("Increment", &def.increment, false),
        ("Minimum", &def.min, true),
        ("Maximum", &def.max, true),
        ("Start", &def.start, true),
    ] {
        let value = value.trim();
        if value.is_empty() && optional {
            continue;
        }
        if !is_integer(value) {
            return Err(format!("{label} must be a whole number"));
        }
    }
    if def
        .increment
        .trim()
        .trim_start_matches('-')
        .trim_start_matches('0')
        .is_empty()
    {
        return Err("Increment can't be 0".into());
    }
    let name = qualified(owner, &def.name);
    let min = |d: &SequenceDef| match d.min.trim() {
        "" => "nominvalue".to_owned(),
        v => format!("minvalue {v}"),
    };
    let max = |d: &SequenceDef| match d.max.trim() {
        "" => "nomaxvalue".to_owned(),
        v => format!("maxvalue {v}"),
    };
    let cycle = |d: &SequenceDef| if d.cycle { "cycle" } else { "nocycle" };
    let cache = |d: &SequenceDef| match d.cache {
        0 | 1 => "nocache".to_owned(),
        n => format!("cache {n}"),
    };
    let order = |d: &SequenceDef| if d.order { "order" } else { "noorder" };
    let Some(original) = original else {
        let mut clauses = vec![format!("increment by {}", def.increment.trim())];
        if !def.start.trim().is_empty() {
            clauses.push(format!("start with {}", def.start.trim()));
        }
        clauses.extend([
            min(def),
            max(def),
            cycle(def).into(),
            cache(def),
            order(def).into(),
        ]);
        return Ok(vec![format!(
            "create sequence {name}\n  {}",
            clauses.join("\n  ")
        )]);
    };
    let mut clauses = Vec::new();
    if def.increment.trim() != original.increment.trim() {
        clauses.push(format!("increment by {}", def.increment.trim()));
    }
    if min(def) != min(original) {
        clauses.push(min(def));
    }
    if max(def) != max(original) {
        clauses.push(max(def));
    }
    if def.cycle != original.cycle {
        clauses.push(cycle(def).into());
    }
    if cache(def) != cache(original) {
        clauses.push(cache(def));
    }
    if def.order != original.order {
        clauses.push(order(def).into());
    }
    // Oracle 18c and later.
    if !def.start.trim().is_empty() {
        clauses.push(format!("restart start with {}", def.start.trim()));
    }
    if clauses.is_empty() {
        return Ok(Vec::new());
    }
    Ok(vec![format!(
        "alter sequence {name}\n  {}",
        clauses.join("\n  ")
    )])
}

pub fn ui(ui: &mut Ui, def: &mut SequenceDef, existing: bool) {
    form_grid(ui, "sequence", |ui| {
        name_field(ui, &mut def.name, existing);
        form_row(ui, "Increment by", |ui| {
            ui.add(egui::TextEdit::singleline(&mut def.increment).desired_width(120.0));
        });
        form_row(ui, "Minimum", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.min)
                    .hint_text("none")
                    .desired_width(120.0),
            );
        });
        form_row(ui, "Maximum", |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut def.max)
                    .hint_text("none")
                    .desired_width(240.0),
            );
        });
        let start = if existing { "Restart at" } else { "Start with" };
        form_row(ui, start, |ui| {
            let hint = if existing { "keep going" } else { "minimum" };
            ui.add(
                egui::TextEdit::singleline(&mut def.start)
                    .hint_text(hint)
                    .desired_width(120.0),
            );
            if let Some(last) = &def.last_number {
                ui.weak(format!("next value: {last}"))
                    .on_hover_text("LAST_NUMBER; cached values may already be handed out");
            }
        });
        form_row(ui, "Cache", |ui| {
            ui.add(egui::DragValue::new(&mut def.cache).range(0..=1_000_000));
            ui.weak(if def.cache <= 1 { "no cache" } else { "values" });
        });
        form_row(ui, "Options", |ui| {
            ui.checkbox(&mut def.cycle, "cycle")
                .on_hover_text("Start over at the minimum after the maximum");
            ui.checkbox(&mut def.order, "order")
                .on_hover_text("Guarantee order across RAC instances");
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_alters() {
        let mut def = SequenceDef::new();
        def.name = "ORDER_SEQ".into();
        def.start = "1000".into();
        assert_eq!(
            statements("HR", None, &def).unwrap(),
            [
                "create sequence HR.ORDER_SEQ\n  increment by 1\n  start with 1000\n  nominvalue\n  nomaxvalue\n  nocycle\n  cache 20\n  noorder"
            ]
        );
        let mut original = def.clone();
        original.start.clear();
        let mut edited = original.clone();
        assert!(
            statements("HR", Some(&original), &edited)
                .unwrap()
                .is_empty()
        );
        edited.cache = 0;
        edited.max = "99999".into();
        edited.start = "5000".into();
        assert_eq!(
            statements("HR", Some(&original), &edited).unwrap(),
            ["alter sequence HR.ORDER_SEQ\n  maxvalue 99999\n  nocache\n  restart start with 5000"]
        );
        edited.increment = "0".into();
        assert!(statements("HR", Some(&original), &edited).is_err());
        edited.increment = "x".into();
        assert!(statements("HR", Some(&original), &edited).is_err());
    }
}
