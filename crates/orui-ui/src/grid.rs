//! Result grid for query results.
//!
//! Rows are virtualized (only visible rows are laid out), so scrolling through
//! millions of fetched rows stays cheap. Supports client-side sorting,
//! rectangular selection with mouse and keyboard, copy (plain, with header or
//! in any export format), aggregates of the selection, a transposed record
//! view and a viewer for long values.

use std::cmp::Ordering;
use std::ops::RangeInclusive;

use egui::{Align, Key, Layout, RichText, Sense, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};
use orui_data::Format;
use orui_db::{Cell, Column};

/// Per-result view state, owned by whoever owns the rows.
#[derive(Default)]
pub struct GridState {
    sort: Option<(usize, bool)>,
    /// View order: indices into `rows` (identity when unsorted).
    order: Vec<usize>,
    /// Row count `order` was computed for.
    ordered_rows: usize,
    /// Selection anchor and cursor as (view row, column).
    anchor: Option<(usize, usize)>,
    cursor: Option<(usize, usize)>,
    dragging: bool,
    pub record_view: bool,
    viewer: Option<(usize, usize)>,
    scroll_to_cursor: bool,
}

pub struct ResultGrid<'a> {
    columns: &'a [Column],
    rows: &'a [Vec<Cell>],
    state: &'a mut GridState,
    id: egui::Id,
}

pub struct GridOutput {
    /// The last fetched row is visible: time to fetch more if available.
    pub reached_end: bool,
    /// Summary of the selection (count/sum/avg/min/max) for a status line.
    pub selection_summary: Option<String>,
}

impl GridState {
    fn sync(&mut self, rows: &[Vec<Cell>], columns: &[Column]) {
        if self.ordered_rows == rows.len() && self.order.len() == rows.len() {
            return;
        }
        self.order = (0..rows.len()).collect();
        self.ordered_rows = rows.len();
        if let Some((col, asc)) = self.sort {
            sort_order(
                &mut self.order,
                rows,
                col,
                columns.get(col).is_some_and(|c| c.numeric),
                asc,
            );
        }
    }

    fn set_sort(&mut self, col: usize, rows: &[Vec<Cell>], columns: &[Column]) {
        // Cycle: ascending → descending → unsorted.
        self.sort = match self.sort {
            Some((c, true)) if c == col => Some((col, false)),
            Some((c, false)) if c == col => None,
            _ => Some((col, true)),
        };
        self.ordered_rows = usize::MAX;
        self.sync(rows, columns);
    }

    fn selection(&self) -> Option<(RangeInclusive<usize>, RangeInclusive<usize>)> {
        let (ar, ac) = self.anchor?;
        let (cr, cc) = self.cursor.unwrap_or((ar, ac));
        Some((ar.min(cr)..=ar.max(cr), ac.min(cc)..=ac.max(cc)))
    }

    fn is_selected(&self, row: usize, col: usize) -> bool {
        self.selection()
            .is_some_and(|(r, c)| r.contains(&row) && c.contains(&col))
    }

    fn select(&mut self, cell: (usize, usize), extend: bool) {
        if !extend || self.anchor.is_none() {
            self.anchor = Some(cell);
        }
        self.cursor = Some(cell);
    }

    fn row_index(&self, view_row: usize) -> usize {
        self.order.get(view_row).copied().unwrap_or(view_row)
    }
}

fn sort_order(order: &mut [usize], rows: &[Vec<Cell>], col: usize, numeric: bool, asc: bool) {
    let value = |i: usize| rows[i].get(col).and_then(|c| c.as_deref());
    order.sort_by(|&a, &b| {
        // NULLs always last, like Oracle's default for ascending order.
        let ord = match (value(a), value(b)) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => return Ordering::Greater,
            (Some(_), None) => return Ordering::Less,
            (Some(x), Some(y)) if numeric => match (x.parse::<f64>(), y.parse::<f64>()) {
                (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
                _ => x.cmp(y),
            },
            (Some(x), Some(y)) => x.cmp(y),
        };
        if asc { ord } else { ord.reverse() }
    });
}

impl<'a> ResultGrid<'a> {
    pub fn new(
        columns: &'a [Column],
        rows: &'a [Vec<Cell>],
        state: &'a mut GridState,
        id: impl egui::AsId,
    ) -> Self {
        Self {
            columns,
            rows,
            state,
            id: egui::Id::new(id),
        }
    }

    pub fn show(self, ui: &mut Ui) -> GridOutput {
        let Self {
            columns,
            rows,
            state,
            id,
        } = self;
        state.sync(rows, columns);
        let mut out = GridOutput {
            reached_end: false,
            selection_summary: None,
        };

        let focus_id = id.with("focus");
        if ui.memory(|m| m.has_focus(focus_id)) {
            handle_keys(ui, state, columns, rows);
        }
        if state.record_view {
            record_view(ui, columns, rows, state, id);
        } else {
            out.reached_end = table(ui, columns, rows, state, id, focus_id);
        }
        out.selection_summary = summary(columns, rows, state);
        viewer(ui.ctx(), columns, rows, state, id);
        out
    }
}

fn handle_keys(ui: &mut Ui, state: &mut GridState, columns: &[Column], rows: &[Vec<Cell>]) {
    let (row_count, col_count) = (rows.len(), columns.len());
    if row_count == 0 || col_count == 0 {
        return;
    }
    let (r, c) = state.cursor.unwrap_or((0, 0));
    let page = 20;
    let (mut nr, mut nc) = (r, c);
    let mut extend = false;
    let mut copy = None;
    let mut open_viewer = false;
    let mut select_all = false;
    ui.input(|i| {
        extend = i.modifiers.shift;
        let ctrl = i.modifiers.command;
        for e in &i.events {
            match e {
                egui::Event::Key {
                    key, pressed: true, ..
                } => match key {
                    Key::ArrowUp => nr = nr.saturating_sub(1),
                    Key::ArrowDown => nr = (nr + 1).min(row_count - 1),
                    Key::ArrowLeft => nc = nc.saturating_sub(1),
                    Key::ArrowRight => nc = (nc + 1).min(col_count - 1),
                    Key::PageUp => nr = nr.saturating_sub(page),
                    Key::PageDown => nr = (nr + page).min(row_count - 1),
                    Key::Home if ctrl => (nr, nc) = (0, 0),
                    Key::End if ctrl => (nr, nc) = (row_count - 1, col_count - 1),
                    Key::Home => nc = 0,
                    Key::End => nc = col_count - 1,
                    Key::A if ctrl => select_all = true,
                    Key::Enter => open_viewer = true,
                    _ => {}
                },
                egui::Event::Copy => copy = Some(i.modifiers.shift),
                _ => {}
            }
        }
    });
    if select_all {
        state.anchor = Some((0, 0));
        state.cursor = Some((row_count - 1, col_count - 1));
    } else if (nr, nc) != (r, c) {
        state.select((nr, nc), extend);
        state.scroll_to_cursor = true;
    }
    if open_viewer {
        state.viewer = state.cursor;
    }
    if let Some(with_header) = copy {
        ui.ctx()
            .copy_text(selection_text(columns, rows, state, with_header));
    }
}

fn table(
    ui: &mut Ui,
    columns: &[Column],
    rows: &[Vec<Cell>],
    state: &mut GridState,
    id: egui::Id,
    focus_id: egui::Id,
) -> bool {
    let mut reached_end = false;
    let visuals = ui.visuals().clone();
    let null_color = visuals.weak_text_color();
    let selected_bg = visuals.selection.bg_fill;
    let cursor_stroke = egui::Stroke::new(1.5, visuals.hyperlink_color);
    let header_bg = visuals.faint_bg_color;
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 6.0;
    let row_digits = rows.len().max(1).to_string().len();
    let mono = egui::TextStyle::Monospace.resolve(ui.style());
    let char_width = ui.fonts_mut(|f| f.glyph_width(&mono, '0'));
    let shift = ui.input(|i| i.modifiers.shift);
    let pointer_down = ui.input(|i| i.pointer.primary_down());
    if !pointer_down {
        state.dragging = false;
    }
    let scroll_to = state
        .scroll_to_cursor
        .then_some(state.cursor)
        .flatten()
        .map(|(r, _)| r);
    state.scroll_to_cursor = false;

    let mut clicked: Option<(usize, usize)> = None;
    let mut sort_clicked = None;
    let mut row_clicked = None;
    let mut open_viewer = None;
    let mut menu_cell = None;

    egui::ScrollArea::horizontal()
        .id_salt(id.with("h"))
        .auto_shrink(false)
        .show(ui, |ui| {
            let mut builder = TableBuilder::new(ui)
                .id_salt(id)
                .striped(true)
                .resizable(true)
                .auto_shrink(false)
                .sense(Sense::click_and_drag())
                .cell_layout(Layout::left_to_right(Align::Center))
                .column(TableColumn::exact(row_digits as f32 * char_width + 18.0));
            if let Some(row) = scroll_to {
                builder = builder.scroll_to_row(row, None);
            }
            for (c, col) in columns.iter().enumerate() {
                // Fit the header and the first rows' values, within limits.
                let content = rows
                    .iter()
                    .take(200)
                    .filter_map(|r| r.get(c)?.as_deref())
                    .map(|v| v.lines().next().unwrap_or_default().chars().count())
                    .max()
                    .unwrap_or(4);
                let chars = content.max(col.name.chars().count() + 2).max(4) as f32;
                let width = (chars * char_width + 16.0).min(char_width * 48.0);
                builder = builder.column(TableColumn::initial(width).at_least(30.0).clip(true));
            }
            builder
                .header(row_height + 4.0, |mut header| {
                    header.col(|ui| {
                        ui.painter().rect_filled(ui.max_rect(), 0.0, header_bg);
                    });
                    for (c, col) in columns.iter().enumerate() {
                        header.col(|ui| {
                            ui.painter().rect_filled(ui.max_rect(), 0.0, header_bg);
                            let arrow = match state.sort {
                                Some((s, true)) if s == c => " ▲",
                                Some((s, false)) if s == c => " ▼",
                                _ => "",
                            };
                            let text = RichText::new(format!("{}{arrow}", col.name)).strong();
                            let layout = if col.numeric {
                                Layout::right_to_left(Align::Center)
                            } else {
                                Layout::left_to_right(Align::Center)
                            };
                            ui.with_layout(layout, |ui| {
                                let r = ui.add(
                                    egui::Label::new(text)
                                        .sense(Sense::click())
                                        .selectable(false),
                                );
                                if r.clicked() {
                                    sort_clicked = Some(c);
                                }
                                r.on_hover_text(format!("{}\nClick to sort", col.type_name));
                            });
                        });
                    }
                })
                .body(|body| {
                    body.rows(row_height, rows.len(), |mut row| {
                        let view_row = row.index();
                        let data = &rows[state.row_index(view_row)];
                        if view_row + 1 == rows.len() {
                            reached_end = true;
                        }
                        let (_, number) = row.col(|ui| {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new((view_row + 1).to_string())
                                        .monospace()
                                        .color(null_color),
                                );
                            });
                        });
                        if number.clicked() {
                            row_clicked = Some(view_row);
                        }
                        for (c, col) in columns.iter().enumerate() {
                            let selected = state.is_selected(view_row, c);
                            let is_cursor = state.cursor == Some((view_row, c));
                            let (_, response) = row.col(|ui| {
                                let rect = ui.max_rect();
                                if selected {
                                    ui.painter().rect_filled(rect, 0.0, selected_bg);
                                }
                                if is_cursor {
                                    ui.painter().rect_stroke(
                                        rect.shrink(1.0),
                                        2.0,
                                        cursor_stroke,
                                        egui::StrokeKind::Inside,
                                    );
                                }
                                let layout = if col.numeric {
                                    Layout::right_to_left(Align::Center)
                                } else {
                                    Layout::left_to_right(Align::Center)
                                };
                                ui.with_layout(layout, |ui| match data.get(c) {
                                    Some(Some(text)) => {
                                        let first = text.lines().next().unwrap_or_default();
                                        let more =
                                            if first.len() < text.len() { " …" } else { "" };
                                        let label = egui::Label::new(
                                            RichText::new(format!("{first}{more}")).monospace(),
                                        )
                                        .truncate()
                                        .selectable(false);
                                        ui.add(label);
                                    }
                                    _ => {
                                        ui.label(
                                            RichText::new("NULL")
                                                .monospace()
                                                .italics()
                                                .color(null_color),
                                        );
                                    }
                                });
                            });
                            if response.drag_started() || response.clicked() {
                                clicked = Some((view_row, c));
                                state.dragging = response.drag_started();
                            } else if state.dragging && pointer_down && response.contains_pointer()
                            {
                                state.cursor = Some((view_row, c));
                            }
                            if response.double_clicked() {
                                open_viewer = Some((view_row, c));
                            }
                            if response.secondary_clicked() {
                                menu_cell = Some((view_row, c));
                            }
                            response.context_menu(|ui| context_menu(ui, columns, rows, state));
                        }
                    });
                });
        });

    if let Some(c) = sort_clicked {
        state.set_sort(c, rows, columns);
    }
    if let Some(cell) = clicked {
        state.select(cell, shift);
        ui.memory_mut(|m| m.request_focus(focus_id));
    }
    if let Some(cell) = menu_cell
        && !state.is_selected(cell.0, cell.1)
    {
        state.select(cell, false);
    }
    if let Some(row) = row_clicked
        && !columns.is_empty()
    {
        let anchor_row = match state.anchor {
            Some((r, _)) if shift => r,
            _ => row,
        };
        state.anchor = Some((anchor_row, 0));
        state.cursor = Some((row, columns.len() - 1));
        ui.memory_mut(|m| m.request_focus(focus_id));
    }
    if let Some(cell) = open_viewer {
        state.viewer = Some(cell);
    }
    // Arrow keys and Tab stay in the grid while it has focus.
    ui.memory_mut(|m| {
        if m.has_focus(focus_id) {
            m.set_focus_lock_filter(
                focus_id,
                egui::EventFilter {
                    tab: true,
                    horizontal_arrows: true,
                    vertical_arrows: true,
                    escape: false,
                },
            );
        }
    });
    reached_end
}

fn context_menu(ui: &mut Ui, columns: &[Column], rows: &[Vec<Cell>], state: &mut GridState) {
    if ui.button("Copy").clicked() {
        ui.ctx()
            .copy_text(selection_text(columns, rows, state, false));
        ui.close();
    }
    if ui.button("Copy with header").clicked() {
        ui.ctx()
            .copy_text(selection_text(columns, rows, state, true));
        ui.close();
    }
    ui.menu_button("Copy as", |ui| {
        for format in Format::ALL
            .iter()
            .filter(|f| f.is_text() && **f != Format::Tsv)
        {
            if ui.button(format.label()).clicked() {
                let (cols, data) = selection_data(columns, rows, state);
                if let Ok(bytes) =
                    orui_data::export_to_vec(*format, &cols, &data, &orui_data::Options::default())
                {
                    ui.ctx()
                        .copy_text(String::from_utf8_lossy(&bytes).into_owned());
                }
                ui.close();
            }
        }
    });
    ui.separator();
    if ui.button("View value…").clicked() {
        state.viewer = state.cursor;
        ui.close();
    }
    if ui
        .button(if state.record_view {
            "Grid view"
        } else {
            "Single record view"
        })
        .clicked()
    {
        state.record_view = !state.record_view;
        ui.close();
    }
}

/// The selected block as columns + rows (in view order); everything if
/// nothing is selected.
pub fn selection_data(
    columns: &[Column],
    rows: &[Vec<Cell>],
    state: &GridState,
) -> (Vec<Column>, Vec<Vec<Cell>>) {
    let Some((r, c)) = state.selection().filter(|(_, c)| *c.end() < columns.len()) else {
        return (columns.to_vec(), rows.to_vec());
    };
    let cols = columns[c.clone()].to_vec();
    let data = r
        .filter_map(|vr| rows.get(state.row_index(vr)))
        .map(|row| c.clone().map(|i| row.get(i).cloned().flatten()).collect())
        .collect();
    (cols, data)
}

fn selection_text(
    columns: &[Column],
    rows: &[Vec<Cell>],
    state: &GridState,
    header: bool,
) -> String {
    let (cols, data) = selection_data(columns, rows, state);
    let mut lines = Vec::with_capacity(data.len() + 1);
    if header {
        lines.push(
            cols.iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>()
                .join("\t"),
        );
    }
    lines.extend(data.iter().map(|r| {
        r.iter()
            .map(|c| c.as_deref().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\t")
    }));
    lines.join("\n")
}

fn summary(columns: &[Column], rows: &[Vec<Cell>], state: &GridState) -> Option<String> {
    let (r, c) = state.selection()?;
    let cells = (r.end() - r.start() + 1) * (c.end() - c.start() + 1);
    if cells < 2 {
        return None;
    }
    let mut numbers = Vec::new();
    let mut non_null = 0;
    for vr in r {
        let Some(row) = rows.get(state.row_index(vr)) else {
            continue;
        };
        for ci in c.clone() {
            if let Some(Some(v)) = row.get(ci) {
                non_null += 1;
                if columns.get(ci).is_some_and(|col| col.numeric)
                    && let Ok(n) = v.parse::<f64>()
                {
                    numbers.push(n);
                }
            }
        }
    }
    if numbers.is_empty() {
        return Some(format!("{cells} cells · {non_null} not null"));
    }
    let sum: f64 = numbers.iter().sum();
    let min = numbers.iter().copied().fold(f64::INFINITY, f64::min);
    let max = numbers.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    Some(format!(
        "Count {non_null} · Sum {} · Avg {} · Min {} · Max {}",
        fmt_num(sum),
        fmt_num(sum / numbers.len() as f64),
        fmt_num(min),
        fmt_num(max)
    ))
}

fn fmt_num(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{n:.0}")
    } else {
        format!("{n:.4}").trim_end_matches('0').to_owned()
    }
}

fn record_view(
    ui: &mut Ui,
    columns: &[Column],
    rows: &[Vec<Cell>],
    state: &mut GridState,
    id: egui::Id,
) {
    let (mut r, c) = state.cursor.unwrap_or((0, 0));
    ui.horizontal(|ui| {
        if ui.button("◀").clicked() {
            r = r.saturating_sub(1);
        }
        ui.label(format!("Record {} of {}", r + 1, rows.len()));
        if ui.button("▶").clicked() && r + 1 < rows.len() {
            r += 1;
        }
        if ui.button("Grid view").clicked() {
            state.record_view = false;
        }
    });
    state.cursor = Some((r, c));
    state.anchor = state.cursor;
    let Some(row) = rows.get(state.row_index(r)) else {
        return;
    };
    let null_color = ui.visuals().weak_text_color();
    egui::ScrollArea::vertical()
        .id_salt(id.with("record"))
        .auto_shrink(false)
        .show(ui, |ui| {
            egui::Grid::new(id.with("record-grid"))
                .striped(true)
                .num_columns(3)
                .spacing([16.0, 6.0])
                .show(ui, |ui| {
                    for (c, col) in columns.iter().enumerate() {
                        ui.label(RichText::new(&col.name).strong());
                        ui.weak(&col.type_name);
                        match row.get(c).and_then(|v| v.as_deref()) {
                            Some(v) => {
                                ui.add(egui::Label::new(RichText::new(v).monospace()).wrap())
                            }
                            None => ui.label(RichText::new("NULL").italics().color(null_color)),
                        };
                        ui.end_row();
                    }
                });
        });
}

fn viewer(
    ctx: &egui::Context,
    columns: &[Column],
    rows: &[Vec<Cell>],
    state: &mut GridState,
    id: egui::Id,
) {
    let Some((r, c)) = state.viewer else { return };
    let value = rows
        .get(state.row_index(r))
        .and_then(|row| row.get(c))
        .cloned()
        .flatten();
    let name = columns.get(c).map_or("", |col| col.name.as_str());
    let pretty = value
        .as_deref()
        .and_then(|v| serde_json::from_str::<serde_json::Value>(v.trim()).ok())
        .filter(|j| j.is_object() || j.is_array())
        .and_then(|j| serde_json::to_string_pretty(&j).ok());
    let mut text = pretty.clone().or_else(|| value.clone()).unwrap_or_default();
    let mut open = true;
    egui::Window::new(format!("{name} — row {}", r + 1))
        .id(id.with("viewer"))
        .open(&mut open)
        .default_size([560.0, 380.0])
        .resizable(true)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                match &value {
                    None => ui.weak("NULL"),
                    Some(v) => ui.weak(format!("{} characters", v.chars().count())),
                };
                if pretty.is_some() {
                    ui.weak("· JSON, formatted");
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(value.clone().unwrap_or_default());
                    }
                });
            });
            ui.separator();
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .frame(egui::Frame::NONE),
                );
            });
        });
    if !open {
        state.viewer = None;
    }
}

/// Renders rows as tab separated text with a header line.
pub fn to_tsv(columns: &[Column], rows: &[Vec<Cell>]) -> String {
    let mut out = columns
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>()
        .join("\t");
    for row in rows {
        out.push('\n');
        let line: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap_or("")).collect();
        out.push_str(&line.join("\t"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<Column> {
        vec![
            Column {
                name: "A".into(),
                type_name: "NUMBER".into(),
                numeric: true,
            },
            Column {
                name: "B".into(),
                type_name: "VARCHAR2(10)".into(),
                numeric: false,
            },
        ]
    }

    fn rows() -> Vec<Vec<Cell>> {
        vec![
            vec![Some("10".into()), Some("x".into())],
            vec![None, Some("a".into())],
            vec![Some("9".into()), None],
        ]
    }

    #[test]
    fn tsv() {
        assert_eq!(to_tsv(&cols(), &rows()[..1]), "A\tB\n10\tx");
    }

    #[test]
    fn numeric_sort_nulls_last() {
        let (c, r) = (cols(), rows());
        let mut s = GridState::default();
        s.sync(&r, &c);
        s.set_sort(0, &r, &c);
        assert_eq!(s.order, [2, 0, 1]);
        s.set_sort(0, &r, &c);
        assert_eq!(s.order, [0, 2, 1]);
        s.set_sort(0, &r, &c);
        assert_eq!(s.order, [0, 1, 2]);
    }

    #[test]
    fn selection_copy_and_summary() {
        let (c, r) = (cols(), rows());
        let mut s = GridState::default();
        s.sync(&r, &c);
        s.select((0, 0), false);
        s.select((2, 1), true);
        assert_eq!(selection_text(&c, &r, &s, true), "A\tB\n10\tx\n\ta\n9\t");
        assert_eq!(
            summary(&c, &r, &s).unwrap(),
            "Count 4 · Sum 19 · Avg 9.5 · Min 9 · Max 10"
        );
    }
}
