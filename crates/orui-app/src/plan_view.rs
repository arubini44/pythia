//! Execution plan as a collapsible tree with cost bars.

use std::collections::HashSet;

use egui::{Align, Layout, RichText, Sense, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};
use orui_db::{Cell, Column};

#[derive(Debug, Clone, PartialEq)]
pub struct PlanRow {
    pub id: u32,
    pub parent: Option<u32>,
    pub depth: usize,
    pub operation: String,
    pub object: String,
    pub cost: Option<f64>,
    pub rows: Option<f64>,
    pub bytes: Option<f64>,
    pub time: Option<f64>,
    pub access: Option<String>,
    pub filter: Option<String>,
}

/// Reads PLAN_TABLE rows (as returned by `MetaQuery::ExplainPlan`).
pub fn rows(columns: &[Column], data: &[Vec<Cell>]) -> Vec<PlanRow> {
    let idx = |name: &str| {
        columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
    };
    let (id, parent, depth, op, obj, cost, rows, bytes, time, access, filter) = (
        idx("ID"),
        idx("PARENT_ID"),
        idx("DEPTH"),
        idx("OPERATION"),
        idx("OBJECT"),
        idx("COST"),
        idx("ROWS"),
        idx("BYTES"),
        idx("TIME"),
        idx("ACCESS_PRED"),
        idx("FILTER_PRED"),
    );
    let text = |row: &Vec<Cell>, i: Option<usize>| i.and_then(|i| row.get(i)).cloned().flatten();
    let num = |row: &Vec<Cell>, i: Option<usize>| text(row, i).and_then(|v| v.parse::<f64>().ok());
    data.iter()
        .map(|r| PlanRow {
            id: num(r, id).unwrap_or(0.0) as u32,
            parent: num(r, parent).map(|p| p as u32),
            depth: num(r, depth).unwrap_or(0.0) as usize,
            operation: text(r, op).unwrap_or_default(),
            object: text(r, obj).unwrap_or_default(),
            cost: num(r, cost),
            rows: num(r, rows),
            bytes: num(r, bytes),
            time: num(r, time),
            access: text(r, access),
            filter: text(r, filter),
        })
        .collect()
}

#[derive(Default)]
pub struct PlanView {
    collapsed: HashSet<u32>,
    selected: Option<u32>,
}

impl PlanView {
    pub fn ui(&mut self, ui: &mut Ui, plan: &[PlanRow], id: egui::Id) {
        if plan.is_empty() {
            ui.weak("No plan");
            return;
        }
        let max_cost = plan
            .iter()
            .filter_map(|r| r.cost)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        // Rows hidden below a collapsed ancestor.
        let mut hidden_below: Option<usize> = None;
        let visible: Vec<&PlanRow> = plan
            .iter()
            .filter(|r| {
                if let Some(d) = hidden_below {
                    if r.depth > d {
                        return false;
                    }
                    hidden_below = None;
                }
                if self.collapsed.contains(&r.id) {
                    hidden_below = Some(r.depth);
                }
                true
            })
            .collect();
        let has_children = |id: u32| plan.iter().any(|r| r.parent == Some(id));
        let accent = ui.visuals().hyperlink_color;
        let row_height = ui.text_style_height(&egui::TextStyle::Body) + 8.0;
        let selected_bg = ui.visuals().selection.bg_fill;
        let mut toggle = None;
        let mut select = None;

        let details_height = if self.selected.is_some() { 90.0 } else { 0.0 };
        egui::Panel::bottom(id.with("predicates"))
            .exact_size(details_height)
            .show_separator_line(self.selected.is_some())
            .show(ui, |ui| {
                if let Some(row) = self.selected.and_then(|s| plan.iter().find(|r| r.id == s)) {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.label(RichText::new(format!("{} {}", row.id, row.operation)).strong());
                        if let Some(a) = &row.access {
                            ui.label(RichText::new(format!("access: {a}")).monospace());
                        }
                        if let Some(f) = &row.filter {
                            ui.label(RichText::new(format!("filter: {f}")).monospace());
                        }
                        if row.access.is_none() && row.filter.is_none() {
                            ui.weak("No predicates");
                        }
                    });
                }
            });

        TableBuilder::new(ui)
            .id_salt(id)
            .striped(true)
            .resizable(true)
            .auto_shrink(false)
            .sense(Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(TableColumn::initial(380.0).at_least(160.0).clip(true))
            .column(TableColumn::initial(180.0).clip(true))
            .column(TableColumn::initial(80.0))
            .column(TableColumn::initial(80.0))
            .column(TableColumn::initial(150.0))
            .column(TableColumn::remainder())
            .header(row_height, |mut h| {
                for (title, right) in [
                    ("Operation", false),
                    ("Object", false),
                    ("Rows", true),
                    ("Bytes", true),
                    ("Cost", false),
                    ("Time", true),
                ] {
                    h.col(|ui| {
                        if right {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.strong(title)
                            });
                        } else {
                            ui.strong(title);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(row_height, visible.len(), |mut row| {
                    let r = visible[row.index()];
                    let selected = self.selected == Some(r.id);
                    let (_, resp) = row.col(|ui| {
                        if selected {
                            ui.painter().rect_filled(ui.max_rect(), 0.0, selected_bg);
                        }
                        ui.add_space(r.depth as f32 * 16.0);
                        if has_children(r.id) {
                            let open = !self.collapsed.contains(&r.id);
                            if ui
                                .add(
                                    egui::Button::new(if open { "⏷" } else { "⏵" })
                                        .frame(false)
                                        .small(),
                                )
                                .clicked()
                            {
                                toggle = Some(r.id);
                            }
                        } else {
                            ui.add_space(14.0);
                        }
                        let mut text = RichText::new(&r.operation).monospace();
                        if r.operation.contains("FULL") {
                            // Full scans are where tuning usually starts.
                            text = text.color(ui.visuals().warn_fg_color);
                        }
                        ui.label(text);
                        if r.access.is_some() || r.filter.is_some() {
                            ui.weak("*");
                        }
                    });
                    if resp.clicked() {
                        select = Some(r.id);
                    }
                    row.col(|ui| {
                        ui.label(RichText::new(&r.object).monospace());
                    });
                    row.col(|ui| right_num(ui, r.rows));
                    row.col(|ui| right_bytes(ui, r.bytes));
                    row.col(|ui| {
                        if let Some(cost) = r.cost {
                            let width = 80.0;
                            let (rect, _) =
                                ui.allocate_exact_size(egui::vec2(width, 8.0), Sense::hover());
                            ui.painter().rect_filled(
                                rect,
                                3.0,
                                ui.visuals().faint_bg_color.gamma_multiply(2.0),
                            );
                            let filled = egui::Rect::from_min_size(
                                rect.min,
                                egui::vec2(width * (cost / max_cost) as f32, rect.height()),
                            );
                            ui.painter().rect_filled(filled, 3.0, accent);
                            ui.label(RichText::new(fmt_num(cost)).monospace());
                        }
                    });
                    row.col(|ui| {
                        if let Some(t) = r.time {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(RichText::new(fmt_time(t)).monospace());
                            });
                        }
                    });
                });
            });
        if let Some(id) = toggle
            && !self.collapsed.remove(&id)
        {
            self.collapsed.insert(id);
        }
        if let Some(id) = select {
            self.selected = if self.selected == Some(id) {
                None
            } else {
                Some(id)
            };
        }
    }
}

fn right_num(ui: &mut Ui, v: Option<f64>) {
    if let Some(v) = v {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(fmt_num(v)).monospace());
        });
    }
}

fn right_bytes(ui: &mut Ui, v: Option<f64>) {
    if let Some(v) = v {
        let text = match v {
            v if v >= 1e9 => format!("{:.1}G", v / 1e9),
            v if v >= 1e6 => format!("{:.1}M", v / 1e6),
            v if v >= 1e3 => format!("{:.1}K", v / 1e3),
            v => format!("{v:.0}"),
        };
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(text).monospace());
        });
    }
}

/// Large numbers with K/M suffixes.
pub fn fmt_num(v: f64) -> String {
    match v {
        v if v >= 1e9 => format!("{:.1}G", v / 1e9),
        v if v >= 1e6 => format!("{:.1}M", v / 1e6),
        v if v >= 1e4 => format!("{:.0}K", v / 1e3),
        v => format!("{v:.0}"),
    }
}

/// PLAN_TABLE.TIME is in seconds.
fn fmt_time(seconds: f64) -> String {
    let s = seconds as u64;
    format!("{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}
