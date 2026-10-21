//! Charts of query results: any result can be shown as a bar or line chart.
//! The x axis is a text or date column (or the row number), the series are
//! numeric columns. The chart follows the grid's filter and sort order.

use egui::{Color32, RichText, Ui};
use egui_plot::{Bar, BarChart, Line, Plot, PlotPoints};
use orui_db::{Cell, Column};
use orui_ui::GridState;

/// Points beyond this are not drawn; the chart says so.
const MAX_POINTS: usize = 20_000;

/// Categorical series colors (light, dark), assigned in this fixed order.
const SERIES: [(&str, &str); 8] = [
    ("#2a78d6", "#3987e5"),
    ("#eb6834", "#d95926"),
    ("#1baf7a", "#199e70"),
    ("#eda100", "#c98500"),
    ("#e87ba4", "#d55181"),
    ("#008300", "#008300"),
    ("#6250d6", "#9085e9"),
    ("#e34948", "#e66767"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Bar,
    Line,
}

#[derive(Debug, Clone)]
pub struct ChartState {
    pub kind: ChartKind,
    /// Column labelling the x axis; `None` uses the row number.
    pub x: Option<usize>,
    /// Numeric columns drawn as series.
    pub series: Vec<usize>,
}

impl ChartState {
    /// Sensible defaults: the first non-numeric column on the x axis, the
    /// numeric columns as series, a line for dates or many rows.
    pub fn guess(columns: &[Column], rows: usize) -> Self {
        let x = columns.iter().position(|c| !c.numeric && !is_rowid(c));
        let series: Vec<usize> = columns
            .iter()
            .enumerate()
            .filter(|(i, c)| c.numeric && Some(*i) != x)
            .map(|(i, _)| i)
            .take(SERIES.len())
            .collect();
        let temporal = x.is_some_and(|i| is_temporal(&columns[i]));
        let kind = if temporal || rows > 60 {
            ChartKind::Line
        } else {
            ChartKind::Bar
        };
        Self { kind, x, series }
    }
}

fn is_rowid(c: &Column) -> bool {
    c.type_name.eq_ignore_ascii_case("ROWID") || c.name.eq_ignore_ascii_case("ROWID")
}

fn is_temporal(c: &Column) -> bool {
    let t = c.type_name.to_ascii_uppercase();
    t.starts_with("DATE") || t.starts_with("TIMESTAMP")
}

/// Parses a fetched number; tolerates a decimal comma.
pub fn parse_number(cell: &Cell) -> Option<f64> {
    let s = cell.as_deref()?.trim();
    s.parse::<f64>()
        .ok()
        .or_else(|| s.replace(',', ".").parse().ok())
        .filter(|v: &f64| v.is_finite())
}

fn series_color(slot: usize, dark: bool) -> Color32 {
    let (light, dark_hex) = SERIES[slot % SERIES.len()];
    Color32::from_hex(if dark { dark_hex } else { light }).unwrap_or(Color32::GRAY)
}

fn short(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        label.to_owned()
    } else {
        let mut s: String = label.chars().take(max - 1).collect();
        s.push('…');
        s
    }
}

pub fn ui(
    ui: &mut Ui,
    state: &mut ChartState,
    columns: &[Column],
    rows: &[Vec<Cell>],
    grid: &mut GridState,
    id: egui::Id,
) {
    let numeric: Vec<usize> = (0..columns.len()).filter(|&i| columns[i].numeric).collect();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.kind, ChartKind::Bar, "Bars");
        ui.selectable_value(&mut state.kind, ChartKind::Line, "Lines");
        ui.separator();
        ui.label("X axis");
        let current = state.x.map_or("Row number", |i| columns[i].name.as_str());
        egui::ComboBox::from_id_salt(id.with("x"))
            .selected_text(current)
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut state.x, None, "Row number");
                for (i, c) in columns.iter().enumerate() {
                    ui.selectable_value(&mut state.x, Some(i), &c.name);
                }
            });
        ui.separator();
        let label = match state.series.len() {
            0 => "No series".to_owned(),
            1 => columns[state.series[0]].name.clone(),
            n => format!("{n} series"),
        };
        ui.menu_button(label, |ui| {
            if numeric.is_empty() {
                ui.weak("The result has no numeric columns");
            }
            for &i in &numeric {
                let mut on = state.series.contains(&i);
                let full = !on && state.series.len() >= SERIES.len();
                if ui
                    .add_enabled(!full, egui::Checkbox::new(&mut on, &columns[i].name))
                    .changed()
                {
                    if on {
                        state.series.push(i);
                        state.series.sort_unstable();
                    } else {
                        state.series.retain(|&s| s != i);
                    }
                }
            }
        });
    });

    if state.series.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Nothing to chart").strong());
            ui.weak(if numeric.is_empty() {
                "Charts need at least one numeric column."
            } else {
                "Pick one or more numeric columns as series."
            });
        });
        return;
    }

    let order = grid.view_order(rows, columns);
    let shown = order.len().min(MAX_POINTS);
    let order = &order[..shown];
    let labels: Vec<String> = order
        .iter()
        .enumerate()
        .map(|(n, &r)| match state.x {
            Some(x) => rows[r][x].clone().unwrap_or_else(|| "(null)".into()),
            None => (n + 1).to_string(),
        })
        .collect();
    if shown < rows.len() && shown == MAX_POINTS {
        ui.weak(format!("Showing the first {MAX_POINTS} rows"));
    }

    let dark = ui.visuals().dark_mode;
    // Colors follow the column, so toggling a series doesn't repaint the others.
    let slot = |col: usize| numeric.iter().position(|&n| n == col).unwrap_or(0);
    let names: Vec<(String, Color32)> = state
        .series
        .iter()
        .map(|&c| (columns[c].name.clone(), series_color(slot(c), dark)))
        .collect();
    let values: Vec<Vec<Option<f64>>> = state
        .series
        .iter()
        .map(|&c| order.iter().map(|&r| parse_number(&rows[r][c])).collect())
        .collect();

    if names.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            for (name, color) in &names {
                let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().rect_filled(rect, 2.0, *color);
                ui.label(name);
                ui.add_space(8.0);
            }
        });
    }
    let tick_labels = labels.clone();
    let hover_labels = labels.clone();
    let hover_names = names.clone();
    let n_labels = labels.len();
    let kind = state.kind;
    Plot::new(id.with("plot"))
        .allow_scroll(false)
        .include_y(0.0)
        .show_grid([false, true])
        .y_axis_min_width(40.0)
        .x_axis_formatter(move |mark, range| {
            let i = mark.value.round();
            if (mark.value - i).abs() > 1e-6 || i < 0.0 || i as usize >= n_labels {
                return String::new();
            }
            // Fewer characters per label the more labels are visible.
            let visible = (range.end() - range.start()).max(1.0);
            let max = if visible > 30.0 { 10 } else { 18 };
            short(&tick_labels[i as usize], max)
        })
        .label_formatter(move |hover| match hover {
            egui_plot::HoverPosition::NearDataPoint {
                plot_name,
                position,
                ..
            } if kind == ChartKind::Line => {
                let i = position.x.round() as usize;
                let label = hover_labels.get(i).map_or("", String::as_str);
                Some(format!("{label}\n{plot_name}: {}", number(position.y)))
            }
            _ => None,
        })
        .show(ui, |plot| match kind {
            ChartKind::Line => {
                for ((name, color), values) in hover_names.iter().zip(&values) {
                    let points: Vec<[f64; 2]> = values
                        .iter()
                        .enumerate()
                        .filter_map(|(i, v)| v.map(|v| [i as f64, v]))
                        .collect();
                    plot.line(
                        Line::new(name.clone(), PlotPoints::from(points))
                            .color(*color)
                            .width(2.0),
                    );
                }
            }
            ChartKind::Bar => {
                let n = values.len() as f64;
                let width = 0.8 / n;
                for (s, ((name, color), values)) in names.iter().zip(&values).enumerate() {
                    let offset = (s as f64 - (n - 1.0) / 2.0) * width;
                    let bars: Vec<Bar> = values
                        .iter()
                        .enumerate()
                        .filter_map(|(i, v)| {
                            v.map(|v| {
                                Bar::new(i as f64 + offset, v)
                                    .width(width * 0.92)
                                    .name(&labels[i])
                                    .fill(*color)
                                    .stroke(egui::Stroke::NONE)
                            })
                        })
                        .collect();
                    let series = name.clone();
                    plot.bar_chart(
                        BarChart::new(name.clone(), bars)
                            .color(*color)
                            .element_formatter(Box::new(move |bar, _| {
                                format!("{}\n{series}: {}", bar.name, number(bar.value))
                            })),
                    );
                }
            }
        });
}

/// A value for a tooltip: integers without decimals, others with up to 4.
fn number(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        let s = format!("{v:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn col(name: &str, type_name: &str, numeric: bool) -> Column {
        Column {
            name: name.into(),
            type_name: type_name.into(),
            numeric,
        }
    }

    #[test]
    fn guesses_axis_and_series() {
        let cols = [
            col("ROWID", "ROWID", false),
            col("DEPT", "VARCHAR2", false),
            col("EMPLOYEES", "NUMBER", true),
            col("SALARY", "NUMBER", true),
        ];
        let c = ChartState::guess(&cols, 10);
        assert_eq!(c.x, Some(1));
        assert_eq!(c.series, vec![2, 3]);
        assert_eq!(c.kind, ChartKind::Bar);

        let cols = [col("DAY", "DATE", false), col("N", "NUMBER", true)];
        assert_eq!(ChartState::guess(&cols, 5).kind, ChartKind::Line);
    }

    #[test]
    fn parses_numbers() {
        assert_eq!(parse_number(&Some("12.5".into())), Some(12.5));
        assert_eq!(parse_number(&Some("12,5".into())), Some(12.5));
        assert_eq!(parse_number(&Some("-.5".into())), Some(-0.5));
        assert_eq!(parse_number(&None), None);
        assert_eq!(parse_number(&Some("abc".into())), None);
        assert_eq!(number(3.0), "3");
        assert_eq!(number(2.50), "2.5");
    }
}
