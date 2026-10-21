//! Import data from CSV or Excel into a new or existing table.
//!
//! Drop a file on the window (or pick one), check the preview and inferred
//! types, choose the target, import. Rows are inserted with array DML on the
//! window's own session; the transaction is committed only if asked to.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;

use egui::{RichText, Ui};
use orui_data::import::{self, ColumnInfo, CsvOptions, Sheet};
use orui_db::oracle::Connection;
use orui_db::{Column, DbError, DbEvent, DbRequest, MetaQuery, MetaResult, TableColumn};
use orui_ui::icons::{Icon, icon_text_button};
use orui_ui::{GridState, ResultGrid};
use serde::{Deserialize, Serialize};

use crate::documents::{DocContext, Effects, Target, quote_ident, request_id};

#[derive(Default, Serialize, Deserialize)]
pub struct ImportWindow {
    #[serde(skip)]
    state: State,
}

#[derive(Default)]
struct State {
    path: Option<PathBuf>,
    picking: Option<Receiver<Option<PathBuf>>>,
    sheets: Vec<String>,
    sheet_name: String,
    delimiter: Option<u8>,
    header: Option<bool>,
    data: Option<Result<Loaded, String>>,
    grid: GridState,
    target: TargetTable,
    table: String,
    existing: Option<Result<Vec<TableColumn>, String>>,
    columns_request: Option<u64>,
    /// Target column per source column (existing table).
    mapping: Vec<Option<String>>,
    commit: bool,
    run: Option<Run>,
    message: Option<(bool, String)>,
}

struct Loaded {
    sheet: Sheet,
    info: Vec<ColumnInfo>,
    /// Preview columns (header + inferred type).
    columns: Vec<Column>,
}

struct Run {
    id: u64,
    progress: Arc<AtomicUsize>,
    total: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TargetTable {
    #[default]
    New,
    Existing,
}

impl ImportWindow {
    pub fn title(&self) -> String {
        match &self.state.path {
            Some(p) => format!(
                "Import {}",
                p.file_name()
                    .map(|n| n.to_string_lossy())
                    .unwrap_or_default()
            ),
            None => "Import data".into(),
        }
    }

    pub fn is_busy(&self) -> bool {
        self.state.run.is_some()
    }

    pub fn load(&mut self, path: PathBuf) {
        let s = &mut self.state;
        s.sheets = if is_workbook(&path) {
            import::workbook_sheets(&path).unwrap_or_default()
        } else {
            Vec::new()
        };
        if !s.sheets.contains(&s.sheet_name) {
            s.sheet_name = s.sheets.first().cloned().unwrap_or_default();
        }
        if s.table.is_empty() {
            let stem = path
                .file_stem()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            s.table = import::column_name(&stem);
        }
        s.path = Some(path);
        self.reload();
    }

    fn reload(&mut self) {
        let s = &mut self.state;
        let Some(path) = &s.path else { return };
        let sheet = if is_workbook(path) {
            import::read_workbook(path, &s.sheet_name, s.header)
        } else {
            std::fs::read(path)
                .map_err(orui_data::Error::from)
                .and_then(|bytes| {
                    import::read_csv(
                        &bytes,
                        CsvOptions {
                            delimiter: s.delimiter,
                            header: s.header,
                        },
                    )
                })
        };
        s.data = Some(sheet.map_err(|e| e.to_string()).map(|sheet| {
            let info = import::infer(&sheet);
            let columns = sheet
                .headers
                .iter()
                .zip(&info)
                .map(|(h, i)| Column {
                    name: h.clone(),
                    type_name: import::oracle_type(i),
                    numeric: matches!(i.kind, import::Kind::Number { .. }),
                })
                .collect();
            Loaded {
                sheet,
                info,
                columns,
            }
        }));
        s.grid = GridState::default();
        self.auto_map();
    }

    /// Maps source columns to target columns with the same (normalized) name.
    fn auto_map(&mut self) {
        let s = &mut self.state;
        let Some(Ok(data)) = &s.data else { return };
        let targets: Vec<String> = match &s.existing {
            Some(Ok(cols)) => cols.iter().map(|c| c.name.clone()).collect(),
            _ => Vec::new(),
        };
        s.mapping = data
            .sheet
            .headers
            .iter()
            .map(|h| {
                let wanted = import::column_name(h);
                targets.iter().find(|t| **t == wanted).cloned()
            })
            .collect();
    }

    pub fn on_event(&mut self, event: DbEvent) -> Effects {
        let s = &mut self.state;
        match event {
            DbEvent::Meta { id, result } if s.columns_request == Some(id) => {
                s.columns_request = None;
                s.existing = Some(match result {
                    Ok(MetaResult::Columns(cols)) if cols.is_empty() => {
                        Err(format!("Table {} not found", s.table))
                    }
                    Ok(MetaResult::Columns(cols)) => Ok(cols),
                    Ok(_) => Err("unexpected result".into()),
                    Err(e) => Err(e.to_string()),
                });
                self.auto_map();
            }
            DbEvent::TaskDone { id, result } if s.run.as_ref().is_some_and(|r| r.id == id) => {
                s.run = None;
                s.message = Some(match result.map(|any| any.downcast::<usize>()) {
                    Ok(Ok(n)) if s.commit => (false, format!("Imported {n} rows and committed")),
                    Ok(Ok(n)) => (
                        false,
                        format!("Imported {n} rows — commit (F10) or roll back (Shift+F10)"),
                    ),
                    Ok(Err(_)) => (true, "unexpected result".into()),
                    Err(e) => (true, e.to_string()),
                });
            }
            _ => {}
        }
        Effects::default()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        if let Some(rx) = &self.state.picking
            && let Ok(picked) = rx.try_recv()
        {
            self.state.picking = None;
            if let Some(path) = picked {
                self.load(path);
            }
        }

        self.source_ui(ui);
        ui.separator();
        if !matches!(self.state.data, Some(Ok(_))) {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.add(Icon::Open.sized(ui, orui_ui::icons::IconSize::Large));
                ui.heading("Drop a CSV or Excel file here");
                ui.weak(
                    "Delimiter, header row, numbers and date formats are detected automatically.",
                );
                if let Some(Err(e)) = &self.state.data {
                    ui.add_space(8.0);
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            });
            return effects;
        }

        egui::Panel::bottom(egui::Id::new(("import-target", cx.id)))
            .resizable(true)
            .default_size(220.0)
            .show(ui, |ui| effects.extend(self.target_ui(ui, cx)));
        egui::CentralPanel::default().show(ui, |ui| {
            let s = &mut self.state;
            if let Some(Ok(data)) = &s.data {
                ui.weak(format!(
                    "{} rows · {} columns · types inferred from the data",
                    data.sheet.rows.len(),
                    data.columns.len()
                ));
                ResultGrid::new(
                    &data.columns,
                    &data.sheet.rows,
                    &mut s.grid,
                    ("import-preview", cx.id),
                )
                .show(ui);
            }
        });
        effects
    }

    fn source_ui(&mut self, ui: &mut Ui) {
        let mut reload = false;
        ui.horizontal(|ui| {
            if icon_text_button(ui, Icon::Open, "Choose file…").clicked()
                && self.state.picking.is_none()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                std::thread::spawn(move || {
                    let picked = rfd::FileDialog::new()
                        .add_filter(
                            "Data files",
                            &["csv", "tsv", "txt", "xlsx", "xls", "xlsb", "ods"],
                        )
                        .pick_file();
                    let _ = tx.send(picked);
                });
                self.state.picking = Some(rx);
            }
            let s = &mut self.state;
            match &s.path {
                Some(p) => {
                    ui.label(RichText::new(p.display().to_string()).monospace());
                }
                None => {
                    ui.weak("or drop a file on this window");
                }
            }
            if s.path.is_none() {
                return;
            }
            ui.separator();
            if !s.sheets.is_empty() {
                egui::ComboBox::from_id_salt("import-sheet")
                    .selected_text(&s.sheet_name)
                    .show_ui(ui, |ui| {
                        for name in s.sheets.clone() {
                            reload |= ui
                                .selectable_value(&mut s.sheet_name, name.clone(), name)
                                .changed();
                        }
                    });
            } else {
                let label = match s.delimiter {
                    None => "Auto",
                    Some(b',') => "Comma",
                    Some(b';') => "Semicolon",
                    Some(b'\t') => "Tab",
                    Some(_) => "Pipe",
                };
                ui.label("Delimiter");
                egui::ComboBox::from_id_salt("import-delimiter")
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for (value, text) in [
                            (None, "Auto"),
                            (Some(b','), "Comma"),
                            (Some(b';'), "Semicolon"),
                            (Some(b'\t'), "Tab"),
                            (Some(b'|'), "Pipe"),
                        ] {
                            reload |= ui.selectable_value(&mut s.delimiter, value, text).changed();
                        }
                    });
            }
            let header = match s.header {
                None => "Auto",
                Some(true) => "Yes",
                Some(false) => "No",
            };
            ui.label("Header row");
            egui::ComboBox::from_id_salt("import-header")
                .selected_text(header)
                .show_ui(ui, |ui| {
                    for (value, text) in [(None, "Auto"), (Some(true), "Yes"), (Some(false), "No")]
                    {
                        reload |= ui.selectable_value(&mut s.header, value, text).changed();
                    }
                });
        });
        if reload {
            self.reload();
        }
    }

    fn target_ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        let s = &mut self.state;
        let Some(Ok(data)) = &s.data else {
            return effects;
        };
        ui.horizontal(|ui| {
            ui.strong("Import into");
            ui.selectable_value(&mut s.target, TargetTable::New, "New table");
            ui.selectable_value(&mut s.target, TargetTable::Existing, "Existing table");
            ui.add(
                egui::TextEdit::singleline(&mut s.table)
                    .hint_text("table or owner.table")
                    .desired_width(220.0),
            );
            if s.target == TargetTable::Existing
                && ui
                    .add_enabled(
                        cx.connected && !s.table.trim().is_empty(),
                        egui::Button::new("Load columns"),
                    )
                    .clicked()
            {
                let (owner, table) = split_name(
                    &s.table,
                    cx.catalog.map(orui_lsp_core::Catalog::current_schema),
                );
                let id = request_id();
                s.columns_request = Some(id);
                s.existing = None;
                effects.request(
                    Target::Main,
                    DbRequest::Meta {
                        id,
                        query: MetaQuery::Columns { owner, table },
                    },
                );
            }
        });
        ui.add_space(4.0);
        let mut ready = !s.table.trim().is_empty();
        let mut start = false;
        egui::ScrollArea::vertical()
            .max_height(ui.available_height() - 40.0)
            .auto_shrink([false, true])
            .show(ui, |ui| match s.target {
                TargetTable::New => {
                    ui.weak("The table is created with these columns:");
                    ui.label(RichText::new(create_table_sql(&s.table, data)).monospace());
                }
                TargetTable::Existing => match &s.existing {
                    None if s.columns_request.is_some() => {
                        ui.spinner();
                    }
                    None => {
                        ready = false;
                        ui.weak("Load the target table's columns to map the file's columns.");
                    }
                    Some(Err(e)) => {
                        ready = false;
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                    Some(Ok(targets)) => {
                        egui::Grid::new("import-mapping")
                            .num_columns(3)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                for (i, header) in data.sheet.headers.iter().enumerate() {
                                    ui.label(RichText::new(header).strong());
                                    ui.weak(&data.columns[i].type_name);
                                    let selected = s.mapping.get(i).cloned().flatten();
                                    egui::ComboBox::from_id_salt(("map", i))
                                        .selected_text(selected.as_deref().unwrap_or("(skip)"))
                                        .show_ui(ui, |ui| {
                                            if ui
                                                .selectable_label(selected.is_none(), "(skip)")
                                                .clicked()
                                            {
                                                s.mapping[i] = None;
                                            }
                                            for t in targets {
                                                let label = format!(
                                                    "{}  {}",
                                                    t.name,
                                                    t.data_type.to_lowercase()
                                                );
                                                if ui
                                                    .selectable_label(
                                                        selected.as_deref() == Some(&t.name),
                                                        label,
                                                    )
                                                    .clicked()
                                                {
                                                    s.mapping[i] = Some(t.name.clone());
                                                }
                                            }
                                        });
                                    ui.end_row();
                                }
                            });
                        ready &= s.mapping.iter().any(Option::is_some);
                    }
                },
            });
        ui.horizontal(|ui| {
            ui.checkbox(&mut s.commit, "Commit when done");
            if let Some(run) = &s.run {
                let done = run.progress.load(Ordering::Relaxed);
                ui.add(
                    egui::ProgressBar::new(done as f32 / run.total.max(1) as f32)
                        .text(format!("{done} / {}", run.total))
                        .desired_width(240.0),
                );
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(100));
            } else if let Some((error, text)) = &s.message {
                let color = if *error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().text_color()
                };
                ui.colored_label(color, text);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = ready && cx.connected && s.run.is_none();
                let label = format!("Import {} rows", data.sheet.rows.len());
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::image_and_text(Icon::Execute.image(ui), label),
                    )
                    .clicked()
                {
                    start = true;
                }
            });
        });
        if start {
            effects.extend(self.start());
        }
        effects
    }

    /// Sets up a new-table import (tests).
    #[cfg(test)]
    pub fn test_new_table(&mut self, table: &str, commit: bool) -> Effects {
        self.state.target = TargetTable::New;
        self.state.table = table.to_owned();
        self.state.commit = commit;
        self.start()
    }

    #[cfg(test)]
    pub fn test_message(&self) -> Option<(bool, String)> {
        self.state.message.clone()
    }

    fn start(&mut self) -> Effects {
        let mut effects = Effects::default();
        let s = &mut self.state;
        let Some(Ok(data)) = &s.data else {
            return effects;
        };
        let table = s.table.trim().to_owned();
        // (source column index, target column name) pairs.
        let pairs: Vec<(usize, String)> = match s.target {
            TargetTable::New => (0..data.sheet.headers.len())
                .map(|i| (i, import::column_name(&data.sheet.headers[i])))
                .collect(),
            TargetTable::Existing => s
                .mapping
                .iter()
                .enumerate()
                .filter_map(|(i, m)| Some((i, m.clone()?)))
                .collect(),
        };
        let create = (s.target == TargetTable::New).then(|| create_table_sql(&table, data));
        let names: Vec<String> = pairs.iter().map(|(_, n)| quote_ident(n)).collect();
        let binds: Vec<String> = pairs
            .iter()
            .enumerate()
            .map(|(n, (i, _))| import::bind_expr(n + 1, data.info.get(*i)))
            .collect();
        let insert = format!(
            "insert into {} ({}) values ({})",
            table_ident(&table),
            names.join(", "),
            binds.join(", ")
        );
        let rows: Vec<Vec<Option<String>>> = data
            .sheet
            .rows
            .iter()
            .map(|row| {
                pairs
                    .iter()
                    .map(|(i, _)| {
                        import::bind_value(
                            row.get(*i).and_then(|v| v.as_deref()),
                            data.info.get(*i),
                        )
                    })
                    .collect()
            })
            .collect();
        let progress = Arc::new(AtomicUsize::new(0));
        let commit = s.commit;
        let id = request_id();
        s.run = Some(Run {
            id,
            progress: progress.clone(),
            total: rows.len(),
        });
        s.message = None;
        let task = Box::new(move |conn: &Connection| {
            let n = run_import(conn, create.as_deref(), &insert, &rows, &progress, commit)?;
            Ok(Box::new(n) as Box<dyn std::any::Any + Send>)
        });
        effects.request(Target::Own, DbRequest::Task { id, task });
        effects
    }
}

fn run_import(
    conn: &Connection,
    create: Option<&str>,
    insert: &str,
    rows: &[Vec<Option<String>>],
    progress: &AtomicUsize,
    commit: bool,
) -> Result<usize, DbError> {
    // Numbers are bound as dot-decimal text.
    conn.execute("alter session set nls_numeric_characters = '.,'", &[])?;
    if let Some(ddl) = create {
        conn.execute(ddl, &[])?;
    }
    let mut batch = conn.batch(insert, 1000).build()?;
    for (i, row) in rows.iter().enumerate() {
        let params: Vec<&dyn orui_db::oracle::sql_type::ToSql> = row
            .iter()
            .map(|v| v as &dyn orui_db::oracle::sql_type::ToSql)
            .collect();
        batch.append_row(&params).map_err(|e| row_error(e, i))?;
        progress.store(i + 1, Ordering::Relaxed);
    }
    batch.execute().map_err(|e| row_error(e, rows.len()))?;
    if commit {
        conn.commit()?;
    }
    Ok(rows.len())
}

fn row_error(e: orui_db::oracle::Error, row: usize) -> DbError {
    let mut error = DbError::from(e);
    error.message = format!("{} (in the batch up to row {row})", error.message);
    error
}

fn create_table_sql(table: &str, data: &Loaded) -> String {
    let columns: Vec<String> = data
        .sheet
        .headers
        .iter()
        .zip(&data.info)
        .map(|(h, info)| {
            let null = if info.nullable { "" } else { " not null" };
            format!(
                "  {} {}{null}",
                quote_ident(&import::column_name(h)).to_lowercase(),
                import::oracle_type(info).to_lowercase()
            )
        })
        .collect();
    format!(
        "create table {} (\n{}\n)",
        table_ident(table).to_lowercase(),
        columns.join(",\n")
    )
}

/// `owner.table` or `table`, uppercased unless quoted.
fn table_ident(name: &str) -> String {
    name.split('.')
        .map(|p| {
            if p.starts_with('"') {
                p.to_owned()
            } else {
                quote_ident(&p.trim().to_uppercase())
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn split_name(name: &str, current_schema: Option<&str>) -> (String, String) {
    let clean = |p: &str| {
        if p.starts_with('"') {
            p.trim_matches('"').to_owned()
        } else {
            p.trim().to_uppercase()
        }
    };
    match name.split_once('.') {
        Some((owner, table)) => (clean(owner), clean(table)),
        None => (current_schema.unwrap_or_default().to_owned(), clean(name)),
    }
}

fn is_workbook(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("xlsx" | "xls" | "xlsb" | "xlsm" | "ods")
    )
}
