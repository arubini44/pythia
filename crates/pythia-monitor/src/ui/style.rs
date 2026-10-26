//! Shared visual building blocks: wait class colors, cards, number formats.

use egui::{Color32, CornerRadius, Frame, Margin, RichText, Stroke, Ui};
use egui_extras::{Column, TableBuilder};

/// Wait class colors, following Oracle Enterprise Manager conventions.
pub fn wait_class_color(class: &str) -> Color32 {
    match class {
        "CPU" => Color32::from_rgb(0x2e, 0xb8, 0x2e),
        "User I/O" => Color32::from_rgb(0x1f, 0x5f, 0xe6),
        "System I/O" => Color32::from_rgb(0x2f, 0x9b, 0xe8),
        "Concurrency" => Color32::from_rgb(0x8b, 0x1a, 0x00),
        "Application" => Color32::from_rgb(0xd0, 0x34, 0x1a),
        "Commit" => Color32::from_rgb(0xe4, 0x7a, 0x10),
        "Configuration" => Color32::from_rgb(0x7a, 0x5a, 0x18),
        "Administrative" => Color32::from_rgb(0x71, 0x73, 0x54),
        "Network" => Color32::from_rgb(0xa8, 0x9a, 0x70),
        "Scheduler" => Color32::from_rgb(0x8f, 0xd1, 0x8f),
        "Cluster" => Color32::from_rgb(0xc9, 0xc2, 0xaf),
        "Queueing" => Color32::from_rgb(0xc2, 0xb7, 0x9b),
        "Idle" => Color32::from_rgb(0x9a, 0x9a, 0x9a),
        _ => Color32::from_rgb(0xf0, 0x6e, 0xaa),
    }
}

/// Accent palette for charts and highlights.
pub struct Palette;

impl Palette {
    pub const BLUE: Color32 = Color32::from_rgb(0x3d, 0x8b, 0xfd);
    pub const GREEN: Color32 = Color32::from_rgb(0x2e, 0xb8, 0x2e);
    pub const ORANGE: Color32 = Color32::from_rgb(0xe8, 0x8a, 0x1a);
    pub const PURPLE: Color32 = Color32::from_rgb(0x9b, 0x6b, 0xdf);
    pub const RED: Color32 = Color32::from_rgb(0xe0, 0x44, 0x3c);
    pub const TEAL: Color32 = Color32::from_rgb(0x1f, 0xa8, 0xa0);
}

/// Severity → color for thresholds (normal, warning, critical).
pub fn level_color(ui: &Ui, value: f64, warn: f64, crit: f64) -> Color32 {
    if value >= crit {
        Palette::RED
    } else if value >= warn {
        Palette::ORANGE
    } else {
        ui.visuals().text_color()
    }
}

/// A subtle card frame used for KPI tiles and sections.
pub fn card(ui: &Ui) -> Frame {
    let v = ui.visuals();
    let fill = if v.dark_mode {
        v.faint_bg_color.gamma_multiply(1.6)
    } else {
        v.faint_bg_color
    };
    Frame::new()
        .fill(fill)
        .stroke(Stroke::new(
            1.0,
            v.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.7),
        ))
        .corner_radius(CornerRadius::same(8))
        .inner_margin(Margin::symmetric(12, 10))
}

/// Section with a heading inside a card, filling the available width.
pub fn section<R>(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    card(ui)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(title).strong());
            ui.add_space(4.0);
            add(ui)
        })
        .inner
}

/// A KPI tile: small caption, big value, optional detail line.
pub struct Kpi {
    pub caption: String,
    pub value: String,
    pub detail: Option<String>,
    pub color: Option<Color32>,
    /// The tile opens something: this says what, on hover.
    pub link: Option<String>,
}

impl Kpi {
    pub fn new(caption: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            caption: caption.into(),
            value: value.into(),
            detail: None,
            color: None,
            link: None,
        }
    }

    /// Makes the tile a link; `hint` says where it leads.
    pub fn link(mut self, hint: impl Into<String>) -> Self {
        self.link = Some(hint.into());
        self
    }

    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn color(mut self, color: Option<Color32>) -> Self {
        self.color = color;
        self
    }
}

/// Draws one tile with the given outer width; true if a linked tile was
/// clicked.
pub fn kpi(ui: &mut Ui, width: f32, tile: &Kpi) -> bool {
    let card = card(ui).show(ui, |ui| {
        // Card margins (12 + 12) and stroke (1 + 1) are outside the content.
        ui.set_width((width - 26.0).max(20.0));
        ui.set_min_height(54.0);
        ui.vertical(|ui| {
            ui.add(
                egui::Label::new(
                    RichText::new(&tile.caption)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                )
                .truncate(),
            );
            let mut v = RichText::new(&tile.value).size(22.0).strong();
            if let Some(c) = tile.color {
                v = v.color(c);
            }
            ui.add(egui::Label::new(v).truncate());
            if let Some(d) = &tile.detail {
                ui.add(
                    egui::Label::new(
                        RichText::new(d)
                            .small()
                            .color(ui.visuals().weak_text_color()),
                    )
                    .truncate(),
                );
            }
        });
    });
    let Some(hint) = &tile.link else {
        return false;
    };
    let response = card.response.interact(egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_stroke(
            response.rect,
            8.0,
            egui::Stroke::new(1.0, ui.visuals().hyperlink_color),
            egui::StrokeKind::Inside,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hint)
        .clicked()
}

/// Lays out tiles in rows that fill the available width (at least
/// `min_width` each). Returns the linked tile clicked, if any.
pub fn kpi_grid(ui: &mut Ui, tiles: &[Kpi], min_width: f32) -> Option<usize> {
    let gap = 8.0;
    let avail = ui.available_width();
    let max_per_row =
        (((avail + gap) / (min_width + gap)).floor() as usize).clamp(1, tiles.len().max(1));
    // Balance the rows: 11 tiles at up to 8 per row become 6 + 5, not 8 + 3.
    let rows = tiles.len().div_ceil(max_per_row).max(1);
    let per_row = tiles.len().div_ceil(rows).max(1);
    let width = ((avail - gap * (per_row as f32 - 1.0)) / per_row as f32).floor();
    let mut clicked = None;
    for (r, row) in tiles.chunks(per_row).enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, t) in row.iter().enumerate() {
                if kpi(ui, width, t) {
                    clicked = Some(r * per_row + i);
                }
            }
        });
        ui.add_space(gap);
    }
    clicked
}

/// A horizontal usage bar with a label, e.g. tablespace usage.
pub fn usage_bar(ui: &mut Ui, fraction: f32, color: Color32, text: String) {
    bar(ui, fraction, color, text, egui::Sense::hover());
}

/// A usage bar that opens something (`hint` says what); true if clicked.
pub fn link_bar(ui: &mut Ui, fraction: f32, color: Color32, text: String, hint: &str) -> bool {
    let response = bar(ui, fraction, color, text, egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_stroke(
            response.rect,
            4.0,
            egui::Stroke::new(1.0, ui.visuals().hyperlink_color),
            egui::StrokeKind::Inside,
        );
    }
    response
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hint)
        .clicked()
}

fn bar(
    ui: &mut Ui,
    fraction: f32,
    color: Color32,
    text: String,
    sense: egui::Sense,
) -> egui::Response {
    let height = 16.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), sense);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, &text));
    let painter = ui.painter_at(rect);
    let bg = ui.visuals().extreme_bg_color;
    painter.rect_filled(rect, 4.0, bg);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    painter.rect_filled(fill, 4.0, color.gamma_multiply(0.85));
    painter.text(
        rect.left_center() + egui::vec2(6.0, 0.0),
        egui::Align2::LEFT_CENTER,
        text,
        egui::FontId::proportional(11.5),
        ui.visuals().strong_text_color(),
    );
    response
}

/// A stacked horizontal bar of (label, value, color) segments with a legend.
/// With a `hint`, segments and legend entries are links: returns the one
/// clicked.
pub fn stacked_bar(
    ui: &mut Ui,
    segments: &[(String, f64, Color32)],
    hint: Option<&str>,
) -> Option<usize> {
    let total: f64 = segments.iter().map(|s| s.1).sum();
    let height = 22.0;
    let sense = if hint.is_some() {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), height), sense);
    let mut clicked = None;
    // The segment under the pointer.
    let at = |x: f32| -> Option<usize> {
        let mut left = rect.left();
        segments.iter().position(|(_, value, _)| {
            left += rect.width() * (*value / total) as f32;
            x < left
        })
    };
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 5.0, ui.visuals().extreme_bg_color);
    if total > 0.0 {
        let mut x = rect.left();
        for (label, value, color) in segments {
            let w = rect.width() * (*value / total) as f32;
            let seg = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(w, height));
            painter.rect_filled(seg, 0.0, *color);
            if w > 46.0 {
                painter.text(
                    seg.center(),
                    egui::Align2::CENTER_CENTER,
                    format!("{label} {value}"),
                    egui::FontId::proportional(11.0),
                    Color32::WHITE,
                );
            }
            x += w;
        }
    }
    if let Some(hint) = hint
        && total > 0.0
    {
        if response.clicked()
            && let Some(pos) = response.interact_pointer_pos()
        {
            clicked = at(pos.x);
        }
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(hint);
    }
    ui.horizontal_wrapped(|ui| {
        for (i, (label, value, color)) in segments.iter().enumerate() {
            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().rect_filled(r, 2.0, *color);
            let text = RichText::new(format!("{label} {value}")).small();
            match hint {
                Some(hint) => {
                    if ui.link(text).on_hover_text(hint).clicked() {
                        clicked = Some(i);
                    }
                }
                None => {
                    ui.label(text);
                }
            }
            ui.add_space(6.0);
        }
    });
    clicked
}

pub fn bytes(b: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut v = b.max(0.0);
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{v:.0} {}", UNITS[i])
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// Compact number: 950, 12.3k, 4.5M.
pub fn compact(n: f64) -> String {
    let a = n.abs();
    if a >= 1e9 {
        format!("{:.1}G", n / 1e9)
    } else if a >= 1e6 {
        format!("{:.1}M", n / 1e6)
    } else if a >= 1e4 {
        format!("{:.1}k", n / 1e3)
    } else if a >= 100.0 || n.fract() == 0.0 {
        format!("{n:.0}")
    } else {
        format!("{n:.1}")
    }
}

/// Duration in seconds as `1d 2h`, `3h 4m`, `5m 6s`, `7.8s`.
pub fn duration(secs: f64) -> String {
    let s = secs.max(0.0);
    if s < 60.0 {
        return if s < 10.0 {
            format!("{s:.1}s")
        } else {
            format!("{s:.0}s")
        };
    }
    let total = s as u64;
    let (d, h, m, sec) = (
        total / 86400,
        total / 3600 % 24,
        total / 60 % 60,
        total % 60,
    );
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m {sec}s")
    }
}

/// Shows a list of section errors in a muted warning box.
pub fn section_errors(ui: &mut Ui, errors: &[(&'static str, String)]) {
    if errors.is_empty() {
        return;
    }
    let color = ui.visuals().warn_fg_color;
    Frame::new()
        .stroke(Stroke::new(1.0, color.gamma_multiply(0.6)))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            for (section, message) in errors {
                ui.label(
                    RichText::new(format!("⚠ {section}: {message}"))
                        .color(color)
                        .small(),
                );
            }
        });
}

/// A centered explanation for an unavailable feature.
pub fn unavailable(ui: &mut Ui, title: &str, reason: &str) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.set_max_width(520.0);
        ui.label(RichText::new(title).heading());
        ui.add_space(6.0);
        ui.label(RichText::new(reason).color(ui.visuals().weak_text_color()));
    });
}

/// A plain table: (title, width, right-aligned) columns, the last one
/// taking the rest. Its cells show their full text on hover. The cells of
/// the `links` columns are ids (a thread, a session), links saying
/// `link_hover`: returns the one clicked.
pub(crate) fn text_table(
    ui: &mut Ui,
    id: &str,
    columns: &[(&str, f32, bool)],
    rows: &[Vec<String>],
    links: &[usize],
    link_hover: &str,
) -> Option<i64> {
    let mut clicked = None;
    let h = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
    let mut builder = TableBuilder::new(ui)
        .id_salt(id)
        .striped(true)
        .vscroll(false)
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
    let last = columns.len().saturating_sub(1);
    for (i, (_, width, _)) in columns.iter().enumerate() {
        builder = builder.column(if i == last {
            Column::remainder().clip(true)
        } else {
            Column::exact(*width).clip(true)
        });
    }
    builder
        .header(h, |mut r| {
            for (title, ..) in columns {
                r.col(|ui| {
                    ui.strong(*title);
                });
            }
        })
        .body(|body| {
            body.rows(h, rows.len(), |mut row| {
                let cells = &rows[row.index()];
                for (i, (_, _, right)) in columns.iter().enumerate() {
                    let text = cells.get(i).map_or("", String::as_str);
                    row.col(|ui| {
                        let label = if i == last {
                            RichText::new(text.replace('\n', " ")).monospace()
                        } else {
                            RichText::new(text)
                        };
                        let layout = if *right {
                            egui::Layout::right_to_left(egui::Align::Center)
                        } else {
                            egui::Layout::left_to_right(egui::Align::Center)
                        };
                        ui.with_layout(layout, |ui| {
                            if links.contains(&i) && !text.is_empty() {
                                if ui.link(label).on_hover_text(link_hover).clicked() {
                                    clicked = text.parse().ok();
                                }
                            } else {
                                ui.add(egui::Label::new(label).truncate())
                                    .on_hover_text(text);
                            }
                        });
                    });
                }
            });
        });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(1536.0), "1.5 KB");
        assert_eq!(bytes(3.0 * 1024.0 * 1024.0 * 1024.0), "3.0 GB");
        assert_eq!(compact(950.0), "950");
        assert_eq!(compact(12_345.0), "12.3k");
        assert_eq!(compact(4_500_000.0), "4.5M");
        assert_eq!(compact(2.25), "2.2");
        assert_eq!(duration(5.25), "5.2s");
        assert_eq!(duration(125.0), "2m 5s");
        assert_eq!(duration(7300.0), "2h 1m");
        assert_eq!(duration(90000.0), "1d 1h");
    }
}
