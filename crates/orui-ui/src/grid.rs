//! Result grid for query results.
//!
//! Rows are virtualized (only visible rows are laid out), so scrolling through
//! millions of fetched rows stays cheap. Column virtualization, editing, sort
//! and filter come with M5.

use egui::{Align, Layout, RichText, Ui};
use egui_extras::{Column as TableColumn, TableBuilder};
use orui_db::{Cell, Column};

pub struct ResultGrid<'a> {
    columns: &'a [Column],
    rows: &'a [Vec<Cell>],
    id_salt: egui::Id,
}

pub struct GridOutput {
    /// The last fetched row is visible: time to fetch more if available.
    pub reached_end: bool,
    /// Text copied to the clipboard this frame, if any.
    pub copied: Option<String>,
}

/// Selected cell as (row, column).
#[derive(Clone, Copy, Default)]
struct Selection(Option<(usize, usize)>);

impl<'a> ResultGrid<'a> {
    pub fn new(columns: &'a [Column], rows: &'a [Vec<Cell>], id_salt: impl egui::AsId) -> Self {
        Self {
            columns,
            rows,
            id_salt: egui::Id::new(id_salt),
        }
    }

    pub fn show(self, ui: &mut Ui) -> GridOutput {
        let sel_id = self.id_salt.with("selection");
        let mut selection: Selection = ui.data(|d| d.get_temp(sel_id)).unwrap_or_default();
        let mut reached_end = false;
        let null_color = ui.visuals().weak_text_color();
        let selected_bg = ui.visuals().selection.bg_fill;
        let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 4.0;
        let row_digits = self.rows.len().max(1).to_string().len();

        egui::ScrollArea::horizontal()
            .id_salt(self.id_salt.with("h"))
            .show(ui, |ui| {
                let mut table = TableBuilder::new(ui)
                    .id_salt(self.id_salt)
                    .striped(true)
                    .resizable(true)
                    .auto_shrink(false)
                    .cell_layout(Layout::left_to_right(Align::Center))
                    .column(TableColumn::exact(row_digits as f32 * 8.0 + 16.0));
                for col in self.columns {
                    let width = (col.name.len().max(8) as f32 * 8.0).min(300.0);
                    table = table.column(TableColumn::initial(width).at_least(30.0).clip(true));
                }
                table
                    .header(row_height + 2.0, |mut header| {
                        header.col(|_| {});
                        for col in self.columns {
                            header.col(|ui| {
                                ui.strong(&col.name).on_hover_text(&col.type_name);
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(row_height, self.rows.len(), |mut row| {
                            let r = row.index();
                            if r + 1 == self.rows.len() {
                                reached_end = true;
                            }
                            row.col(|ui| {
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    ui.label(
                                        RichText::new((r + 1).to_string())
                                            .monospace()
                                            .color(null_color),
                                    );
                                });
                            });
                            for (c, col) in self.columns.iter().enumerate() {
                                let is_selected = selection.0 == Some((r, c));
                                let (_, response) = row.col(|ui| {
                                    if is_selected {
                                        ui.painter().rect_filled(ui.max_rect(), 0.0, selected_bg);
                                    }
                                    let layout = if col.numeric {
                                        Layout::right_to_left(Align::Center)
                                    } else {
                                        Layout::left_to_right(Align::Center)
                                    };
                                    ui.with_layout(layout, |ui| match &self.rows[r].get(c) {
                                        Some(Some(text)) => {
                                            // Only the first line of multi-line values fits in a cell.
                                            let first = text.lines().next().unwrap_or_default();
                                            ui.add(
                                                egui::Label::new(RichText::new(first).monospace())
                                                    .truncate()
                                                    .selectable(false),
                                            );
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
                                if response.interact(egui::Sense::click()).clicked() {
                                    selection.0 = Some((r, c));
                                }
                            }
                        });
                    });
            });

        let copied = self.handle_copy(ui, selection);
        ui.data_mut(|d| d.insert_temp(sel_id, selection));
        GridOutput {
            reached_end,
            copied,
        }
    }

    fn handle_copy(&self, ui: &Ui, selection: Selection) -> Option<String> {
        let (r, c) = selection.0?;
        let copy = ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
        if !copy {
            return None;
        }
        let text = self.rows.get(r)?.get(c)?.clone().unwrap_or_default();
        ui.ctx().copy_text(text.clone());
        Some(text)
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

    #[test]
    fn tsv() {
        let cols = vec![
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
        ];
        let rows = vec![
            vec![Some("1".into()), None],
            vec![Some("2".into()), Some("x".into())],
        ];
        assert_eq!(to_tsv(&cols, &rows), "A\tB\n1\t\n2\tx");
    }
}
