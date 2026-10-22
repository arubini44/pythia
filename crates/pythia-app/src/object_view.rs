//! Object view: describe a table, view or code object (columns, details, DDL).

use std::collections::HashMap;

use egui::{RichText, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};
use pythia_db::{
    DbEvent, DbRequest, Detail, MetaQuery, MetaResult, ObjectType, TableColumn as Col,
};
use pythia_ui::ResultGrid;
use serde::{Deserialize, Serialize};

use crate::documents::{
    DocContext, Effects, ObjectRef, OpenRequest, TableData, Target, request_id,
};
use crate::editor::Editor;

#[derive(Serialize, Deserialize)]
pub struct ObjectView {
    pub object: ObjectRef,
    #[serde(skip)]
    state: State,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
enum Tab {
    #[default]
    Columns,
    Detail(Detail),
    Ddl,
}

pub enum Loaded<T> {
    Loading,
    Ready(T),
    Failed(String),
}

#[derive(Default)]
struct State {
    tab: Option<Tab>,
    columns: Option<Loaded<Vec<Col>>>,
    details: HashMap<Detail, Loaded<TableData>>,
    grids: HashMap<Detail, pythia_ui::GridState>,
    ddl: Option<Loaded<Editor>>,
    pending: HashMap<u64, Tab>,
}

impl ObjectView {
    pub fn new(object: ObjectRef) -> Self {
        Self {
            object,
            state: State::default(),
        }
    }

    pub fn title(&self) -> String {
        self.object.name.to_lowercase()
    }

    pub fn is_loading(&self) -> bool {
        !self.state.pending.is_empty()
    }

    fn has_columns(&self) -> bool {
        self.object.object_type.has_data()
    }

    fn tabs(&self) -> Vec<Tab> {
        let mut tabs = Vec::new();
        if self.has_columns() {
            tabs.push(Tab::Columns);
        }
        let details = if self.object.object_type == ObjectType::Table {
            Detail::TABLE
        } else {
            Detail::CODE
        };
        tabs.extend(details.iter().map(|d| Tab::Detail(*d)));
        tabs.push(Tab::Ddl);
        tabs
    }

    /// Clears everything and loads the current tab again.
    pub fn load(&mut self) -> Effects {
        let tab = self.state.tab.unwrap_or_else(|| self.tabs()[0]);
        self.state = State {
            tab: Some(tab),
            ..State::default()
        };
        self.ensure(tab)
    }

    fn ensure(&mut self, tab: Tab) -> Effects {
        let mut effects = Effects::default();
        let o = &self.object;
        let query = match tab {
            Tab::Columns if self.state.columns.is_none() => {
                self.state.columns = Some(Loaded::Loading);
                MetaQuery::Columns {
                    owner: o.owner.clone(),
                    table: o.name.clone(),
                }
            }
            Tab::Detail(detail) if !self.state.details.contains_key(&detail) => {
                self.state.details.insert(detail, Loaded::Loading);
                MetaQuery::Details {
                    owner: o.owner.clone(),
                    name: o.name.clone(),
                    detail,
                }
            }
            Tab::Ddl if self.state.ddl.is_none() => {
                self.state.ddl = Some(Loaded::Loading);
                MetaQuery::Ddl {
                    owner: o.owner.clone(),
                    name: o.name.clone(),
                    object_type: o.object_type,
                }
            }
            _ => return effects,
        };
        let id = request_id();
        self.state.pending.insert(id, tab);
        effects.request(Target::Main, DbRequest::Meta { id, query });
        effects
    }

    pub fn on_event(&mut self, event: DbEvent) -> Effects {
        let DbEvent::Meta { id, result } = event else {
            return Effects::default();
        };
        let Some(tab) = self.state.pending.remove(&id) else {
            return Effects::default();
        };
        let failed = |e: pythia_db::DbError| e.to_string();
        match (tab, result) {
            (Tab::Columns, Ok(MetaResult::Columns(cols))) => {
                self.state.columns = Some(Loaded::Ready(cols))
            }
            (Tab::Columns, Err(e)) => self.state.columns = Some(Loaded::Failed(failed(e))),
            (Tab::Detail(d), Ok(MetaResult::Table { columns, rows })) => {
                self.state.details.insert(d, Loaded::Ready((columns, rows)));
            }
            (Tab::Detail(d), Err(e)) => {
                self.state.details.insert(d, Loaded::Failed(failed(e)));
            }
            (Tab::Ddl, Ok(MetaResult::Text(text))) => {
                self.state.ddl = Some(Loaded::Ready(Editor::new(text)))
            }
            (Tab::Ddl, Err(e)) => self.state.ddl = Some(Loaded::Failed(failed(e))),
            _ => {}
        }
        Effects::default()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext, connection: Option<&str>) -> Effects {
        let mut effects = Effects::default();
        let tabs = self.tabs();
        let mut tab = self.state.tab.unwrap_or(tabs[0]);

        ui.horizontal_wrapped(|ui| {
            if let Some(icon) =
                pythia_ui::icons::Icon::for_object_type(self.object.object_type.dictionary_name())
            {
                ui.add(icon.image(ui));
            }
            ui.label(RichText::new(format!("{}.{}", self.object.owner, self.object.name)).strong());
            ui.weak(self.object.object_type.dictionary_name().to_lowercase());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let Some(connection) = connection else {
                    return;
                };
                if ui.button("Refresh").clicked() {
                    effects.extend(self.load());
                }
                if self.object.object_type.has_source() && ui.button("Edit").clicked() {
                    effects.open.push(OpenRequest::Program {
                        connection: connection.to_owned(),
                        object: self.object.clone(),
                    });
                }
                if self.object.object_type.has_data() && ui.button("Query data").clicked() {
                    effects.open.push(OpenRequest::Sql {
                        connection: connection.to_owned(),
                        text: format!("select * from {}", self.object.qualified()),
                        run: true,
                    });
                }
            });
        });
        ui.horizontal(|ui| {
            for t in &tabs {
                let label = match t {
                    Tab::Columns => "Columns",
                    Tab::Detail(d) => d.label(),
                    Tab::Ddl => "DDL",
                };
                ui.selectable_value(&mut tab, *t, label);
            }
        });
        ui.separator();
        if self.state.tab != Some(tab) {
            self.state.tab = Some(tab);
        }
        if cx.connected {
            effects.extend(self.ensure(tab));
        }

        match tab {
            Tab::Columns => match &self.state.columns {
                Some(Loaded::Ready(cols)) => columns_table(ui, cols),
                other => loading_ui(ui, other.as_ref()),
            },
            Tab::Detail(d) => match self.state.details.get(&d) {
                Some(Loaded::Ready((_, rows))) if rows.is_empty() => {
                    ui.weak(format!("No {}", d.label().to_lowercase()));
                }
                Some(Loaded::Ready((columns, rows))) => {
                    let grid = self.state.grids.entry(d).or_default();
                    ResultGrid::new(columns, rows, grid, ("object-detail", cx.id, d.label()))
                        .show(ui);
                }
                other => loading_ui(ui, other),
            },
            Tab::Ddl => match &mut self.state.ddl {
                Some(Loaded::Ready(editor)) => {
                    if let Some(connection) = connection
                        && ui.button("Open in SQL window").clicked()
                    {
                        effects.open.push(OpenRequest::Sql {
                            connection: connection.to_owned(),
                            text: editor.text().to_owned(),
                            run: false,
                        });
                    }
                    editor.ui_read_only(ui, ("object-ddl", cx.id), &cx.settings.editor);
                }
                other => loading_ui(ui, other.as_ref()),
            },
        }
        effects
    }
}

fn loading_ui<T>(ui: &mut Ui, state: Option<&Loaded<T>>) {
    match state {
        Some(Loaded::Failed(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
        _ => {
            ui.spinner();
        }
    }
}

fn columns_table(ui: &mut Ui, cols: &[Col]) {
    let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .auto_shrink(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .column(TableColumn::initial(28.0))
        .column(TableColumn::initial(200.0).clip(true))
        .column(TableColumn::initial(170.0).clip(true))
        .column(TableColumn::initial(50.0))
        .column(TableColumn::initial(140.0).clip(true))
        .column(TableColumn::remainder().clip(true))
        .header(row_height, |mut h| {
            for title in ["", "Name", "Type", "Null", "Default", "Comment"] {
                h.col(|ui| {
                    ui.strong(title);
                });
            }
        })
        .body(|body| {
            body.rows(row_height, cols.len(), |mut row| {
                let c = &cols[row.index()];
                row.col(|ui| {
                    if c.pk_position.is_some() {
                        ui.add(pythia_ui::icons::Icon::PrimaryKey.image(ui))
                            .on_hover_text("Primary key");
                    }
                });
                row.col(|ui| {
                    ui.label(RichText::new(&c.name).monospace());
                });
                row.col(|ui| {
                    ui.label(RichText::new(&c.data_type).monospace());
                });
                row.col(|ui| {
                    if c.nullable {
                        ui.weak("Y");
                    } else {
                        ui.label("N");
                    }
                });
                row.col(|ui| {
                    ui.label(RichText::new(c.default.as_deref().unwrap_or("")).monospace());
                });
                row.col(|ui| {
                    ui.label(c.comment.as_deref().unwrap_or(""));
                });
            });
        });
}
