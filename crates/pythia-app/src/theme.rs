//! Visual design: palettes for dark and light mode, spacing and rounding.

use egui::style::{Selection, WidgetVisuals};
use egui::{Color32, CornerRadius, Frame, Margin, Shadow, Stroke, Style, Theme, Vec2};
use pythia_config::{AppearanceSettings, GridSettings, ScrollBars};

/// Pending transaction, warnings.
pub const WARN: Color32 = Color32::from_rgb(0xe0, 0x9b, 0x2d);
/// Production connections, destructive actions.
pub const DANGER: Color32 = Color32::from_rgb(0xe5, 0x48, 0x4d);
pub const ACCENT: Color32 = Color32::from_rgb(0x3b, 0x82, 0xf6);
/// Something checked out fine (e.g. a connection test).
pub const SUCCESS: Color32 = Color32::from_rgb(0x36, 0xa8, 0x5f);

struct Palette {
    /// Window background behind panels (darkest in dark mode).
    base: Color32,
    /// Panels, tab bars, toolbars.
    panel: Color32,
    /// Editors, grids, text fields.
    surface: Color32,
    /// Floating windows, menus, popups.
    raised: Color32,
    /// Text fields; must stand out from `raised` (dialogs) and `panel`.
    field: Color32,
    border: Color32,
    text: Color32,
    text_weak: Color32,
    hover: Color32,
    active: Color32,
    selection: Color32,
    stripe: Color32,
    error: Color32,
    warn: Color32,
    link: Color32,
}

const DARK: Palette = Palette {
    base: Color32::from_rgb(0x17, 0x18, 0x1b),
    panel: Color32::from_rgb(0x1e, 0x1f, 0x23),
    surface: Color32::from_rgb(0x1a, 0x1b, 0x1e),
    raised: Color32::from_rgb(0x27, 0x29, 0x2e),
    field: Color32::from_rgb(0x1a, 0x1b, 0x1e),
    border: Color32::from_rgb(0x32, 0x35, 0x3b),
    text: Color32::from_rgb(0xdc, 0xde, 0xe3),
    text_weak: Color32::from_rgb(0x8b, 0x90, 0x9a),
    hover: Color32::from_rgb(0x2c, 0x2f, 0x35),
    active: Color32::from_rgb(0x35, 0x39, 0x41),
    selection: Color32::from_rgba_premultiplied(0x23, 0x4e, 0x94, 0xb0),
    stripe: Color32::from_rgb(0x1f, 0x20, 0x24),
    error: Color32::from_rgb(0xf2, 0x6d, 0x6d),
    warn: Color32::from_rgb(0xe8, 0xb3, 0x4a),
    link: Color32::from_rgb(0x6c, 0xa6, 0xff),
};

const LIGHT: Palette = Palette {
    base: Color32::from_rgb(0xe9, 0xeb, 0xef),
    panel: Color32::from_rgb(0xf4, 0xf5, 0xf7),
    surface: Color32::from_rgb(0xff, 0xff, 0xff),
    raised: Color32::from_rgb(0xff, 0xff, 0xff),
    field: Color32::from_rgb(0xec, 0xee, 0xf2),
    border: Color32::from_rgb(0xd9, 0xdc, 0xe1),
    text: Color32::from_rgb(0x1f, 0x23, 0x28),
    text_weak: Color32::from_rgb(0x6a, 0x71, 0x7c),
    hover: Color32::from_rgb(0xe6, 0xe8, 0xec),
    active: Color32::from_rgb(0xd8, 0xdc, 0xe3),
    selection: Color32::from_rgba_premultiplied(0xa6, 0xc8, 0xff, 0xc0),
    stripe: Color32::from_rgb(0xf7, 0xf8, 0xfa),
    error: Color32::from_rgb(0xc8, 0x2b, 0x2b),
    warn: Color32::from_rgb(0xa8, 0x62, 0x00),
    link: Color32::from_rgb(0x1d, 0x5f, 0xd6),
};

pub fn install(ctx: &egui::Context) {
    pythia_ui::icons::install(ctx);
    set_fonts(ctx, None, None);
    apply(
        ctx,
        &AppearanceSettings::default(),
        &GridSettings::default(),
    );
}

/// Styles both themes, and the result grids, after the settings.
pub fn apply(ctx: &egui::Context, appearance: &AppearanceSettings, grid: &GridSettings) {
    ctx.style_mut_of(Theme::Dark, |s| style(s, &DARK, appearance));
    ctx.style_mut_of(Theme::Light, |s| style(s, &LIGHT, appearance));
    let rgb = |[r, g, b]: [u8; 3]| Color32::from_rgb(r, g, b);
    let family = if grid.interface_font {
        egui::FontFamily::Proportional
    } else {
        egui::FontFamily::Monospace
    };
    pythia_ui::set_grid_style(
        ctx,
        pythia_ui::GridStyle {
            font: Some(egui::FontId::new(grid.font_size.clamp(6.0, 40.0), family)),
            striped: grid.striped,
            row_lines: grid.row_lines,
            null_text: grid.null_text.clone(),
            null_color: grid.null_color.map(rgb),
        },
    );
}

/// Installs the fonts, with `code_font` (font file bytes) first for code
/// and `ui_font` first for everything else.
pub fn set_fonts(ctx: &egui::Context, code_font: Option<Vec<u8>>, ui_font: Option<Vec<u8>>) {
    // The UI font lacks arrows and some symbols; fall back to the
    // monospace font (Hack) before the emoji fonts. A font of the user's
    // goes before both, so they remain its fallbacks.
    let mut fonts = egui::FontDefinitions::default();
    if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        family.insert(1.min(family.len()), "Hack".to_owned());
    }
    for (name, bytes, family) in [
        ("code", code_font, egui::FontFamily::Monospace),
        ("ui", ui_font, egui::FontFamily::Proportional),
    ] {
        let Some(bytes) = bytes else { continue };
        fonts.font_data.insert(
            name.to_owned(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        if let Some(family) = fonts.families.get_mut(&family) {
            family.insert(0, name.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

const OBJECT_NAMES: &str = "object-names";

/// Font of object names in the object browser: the code font or the
/// interface font, as set.
pub fn object_names(style: &Style) -> egui::FontId {
    style
        .text_styles
        .get(&egui::TextStyle::Name(OBJECT_NAMES.into()))
        .cloned()
        .unwrap_or_else(|| egui::TextStyle::Monospace.resolve(style))
}

/// The built-in look's body text size, which the other sizes relate to.
const BASE_FONT_SIZE: f32 = 13.0;

fn style(style: &mut Style, p: &Palette, a: &AppearanceSettings) {
    use egui::{FontId, TextStyle};
    let size = a.font_size.clamp(6.0, 40.0);
    let scale = size / BASE_FONT_SIZE;
    for (text_style, font) in [
        (TextStyle::Small, FontId::proportional(9.0 * scale)),
        (TextStyle::Body, FontId::proportional(size)),
        (TextStyle::Button, FontId::proportional(size)),
        (TextStyle::Heading, FontId::proportional(18.0 * scale)),
        (TextStyle::Monospace, FontId::monospace(size)),
        (
            TextStyle::Name(OBJECT_NAMES.into()),
            if a.names_in_code_font {
                FontId::monospace(size)
            } else {
                FontId::proportional(size)
            },
        ),
    ] {
        style.text_styles.insert(text_style, font);
    }

    let d = &a.density;
    let (padding, gap) = (d.padding.clamp(0.0, 20.0), d.spacing.clamp(0.0, 20.0));
    let margin = d.margin.clamp(0.0, 40.0);
    let radius = |r: u8| CornerRadius::same(if a.rounded { r } else { 0 });
    let spacing = &mut style.spacing;
    spacing.item_spacing = Vec2::new(gap + 3.0, gap);
    spacing.button_padding = Vec2::new((padding * 2.0 + 2.0).max(4.0), padding);
    // A line of body text plus the padding: the rows of lists and tables
    // (see `pythia_ui::row_height`) and widgets line up.
    spacing.interact_size = Vec2::new(36.0, 16.0 * scale + 2.0 * padding);
    spacing.menu_margin = Margin::same((margin / 2.0).clamp(2.0, 6.0) as i8);
    spacing.window_margin = Margin::same(margin as i8);
    spacing.indent = d.indent.clamp(0.0, 64.0);
    spacing.scroll = match a.scroll_bars {
        ScrollBars::Floating => egui::style::ScrollStyle::floating(),
        ScrollBars::Visible => egui::style::ScrollStyle {
            dormant_handle_opacity: 0.6,
            active_handle_opacity: 0.8,
            ..egui::style::ScrollStyle::solid()
        },
    };
    spacing.scroll.bar_width = a.scroll_bar_width.clamp(2.0, 24.0);
    style.animation_time = if a.animations {
        Style::default().animation_time
    } else {
        0.0
    };

    let accent = a
        .accent
        .map_or(ACCENT, |[r, g, b]| Color32::from_rgb(r, g, b));
    let v = &mut style.visuals;
    v.panel_fill = p.panel;
    v.window_fill = p.raised;
    v.extreme_bg_color = p.surface;
    v.text_edit_bg_color = Some(p.field);
    v.code_bg_color = p.surface;
    v.faint_bg_color = p.stripe;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.window_corner_radius = radius(10);
    v.menu_corner_radius = radius(8);
    let shadow_alpha = if v.dark_mode { 110 } else { 40 };
    v.window_shadow = Shadow {
        offset: [0, 8],
        blur: 28,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    v.popup_shadow = Shadow {
        offset: [0, 4],
        blur: 16,
        spread: 0,
        color: Color32::from_black_alpha(shadow_alpha),
    };
    if !a.shadows {
        v.window_shadow = Shadow::NONE;
        v.popup_shadow = Shadow::NONE;
    }
    // A chosen accent tints selections too.
    let selection = if a.accent.is_some() {
        let alpha = if v.dark_mode { 0x90 } else { 0x60 };
        Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), alpha)
    } else {
        p.selection
    };
    v.selection = Selection {
        bg_fill: selection,
        stroke: Stroke::new(1.0, p.text),
    };
    v.hyperlink_color = p.link;
    v.error_fg_color = p.error;
    v.warn_fg_color = p.warn;
    v.striped = true;
    v.text_cursor.stroke = Stroke::new(2.0, accent);

    let widget = |bg: Color32, stroke: Color32, fg: Color32| WidgetVisuals {
        bg_fill: bg,
        weak_bg_fill: bg,
        bg_stroke: Stroke::new(1.0, stroke),
        corner_radius: radius(5),
        fg_stroke: Stroke::new(1.0, fg),
        expansion: 0.0,
    };
    let w = &mut v.widgets;
    w.noninteractive = widget(p.panel, p.border, p.text);
    w.noninteractive.weak_bg_fill = p.panel;
    w.inactive = widget(p.hover, Color32::TRANSPARENT, p.text);
    // No stroke rather than a transparent one: egui takes the stroke width
    // off the button padding, and buttons without an idle frame (icon
    // buttons) only draw the stroke on hover, so they would grow by it.
    w.inactive.bg_stroke = Stroke::NONE;
    w.hovered = widget(p.active, p.border, p.text);
    w.active = widget(p.active, accent, p.text);
    w.open = widget(p.active, p.border, p.text);
    // Weak text (labels marked .weak()) uses this.
    v.weak_text_color = Some(p.text_weak);
}

/// The window's background behind the panels: in the dock's gaps and
/// outside the tabs' rounded corners.
pub fn base(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        DARK.base
    } else {
        LIGHT.base
    }
}

/// Frame of the toolbar strip below the menu.
pub fn toolbar_frame(style: &Style) -> Frame {
    let (x, y) = bar_margin(style);
    Frame::new()
        .fill(style.visuals.panel_fill)
        .inner_margin(Margin::symmetric(x + 2, y + 1))
}

/// Frame of the status bar.
pub fn status_frame(style: &Style) -> Frame {
    let (x, y) = bar_margin(style);
    Frame::new()
        .fill(style.visuals.window_fill)
        .inner_margin(Margin::symmetric(x + 4, y))
        .stroke(Stroke::new(
            1.0,
            style.visuals.widgets.noninteractive.bg_stroke.color,
        ))
}

/// Margins of the toolbar and the status bar, which follow the density:
/// (menu margin, vertical padding).
fn bar_margin(style: &Style) -> (i8, i8) {
    (
        style.spacing.menu_margin.left,
        style.spacing.button_padding.y.round() as i8,
    )
}

/// The dock's style: tabs as high as other widgets.
pub fn dock_style(style: &Style) -> egui_dock::Style {
    let mut dock = egui_dock::Style::from_egui(style);
    dock.tab_bar.height = style.spacing.interact_size.y + 2.0;
    dock
}

#[cfg(test)]
mod tests {
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    /// Every character the UI's source writes has a glyph in the fonts
    /// Pythia installs; one without (`✕` in a chip) is drawn as a box
    /// (#91). Comments and unit tests aren't shown, so they're skipped.
    #[test]
    fn ui_characters_have_glyphs() {
        fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    sources(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    out.push(path);
                }
            }
        }
        let mut h = Harness::new_ui(|ui| {
            ui.label("Pythia");
        });
        super::install(&h.ctx);
        h.run();
        let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = Vec::new();
        for krate in std::fs::read_dir(&crates).unwrap().flatten() {
            let src = krate.path().join("src");
            if src.is_dir() {
                sources(&src, &mut files);
            }
        }
        assert!(files.len() > 50, "found {} sources", files.len());
        // The proportional family's fonts, as installed.
        let fonts: Vec<(Vec<u8>, u32)> = h.ctx.fonts(|f| {
            let defs = f.definitions();
            defs.families[&egui::FontFamily::Proportional]
                .iter()
                .map(|name| {
                    let data = &defs.font_data[name];
                    (data.font.to_vec(), data.index)
                })
                .collect()
        });
        let has_glyph = |c: char| {
            use skrifa::MetadataProvider;
            fonts.iter().any(|(bytes, index)| {
                skrifa::FontRef::from_index(bytes, *index)
                    .is_ok_and(|font| font.charmap().map(c).is_some())
            })
        };
        assert!(has_glyph('→') && has_glyph('×') && !has_glyph('\u{10FFFF}'));
        let mut missing = Vec::new();
        for file in files {
            let name = file.file_name().unwrap().to_string_lossy();
            if name == "screenshots.rs" || name == "test_db.rs" || name == "tests.rs" {
                continue;
            }
            // CRLF where git checks out with it (Windows, #94).
            let text = std::fs::read_to_string(&file)
                .unwrap()
                .replace("\r\n", "\n");
            let shown = text.split("\n#[cfg(test)]\nmod tests").next().unwrap();
            for (n, line) in shown.lines().enumerate() {
                if line.trim_start().starts_with("//") {
                    continue;
                }
                for c in line.chars().filter(|c| !c.is_ascii()) {
                    if !has_glyph(c) {
                        missing.push(format!(
                            "{c:?} (U+{:04X}) {}:{}",
                            c as u32,
                            file.display(),
                            n + 1
                        ));
                    }
                }
            }
        }
        assert!(missing.is_empty(), "no glyph for:\n{}", missing.join("\n"));
    }

    /// The default settings give the look Pythia had before they existed.
    #[test]
    fn default_appearance_is_the_built_in_look() {
        use egui::{CornerRadius, Margin, Vec2};
        let ctx = egui::Context::default();
        super::install(&ctx);
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let style = ctx.style_of(theme);
            let s = &style.spacing;
            assert_eq!(s.item_spacing, Vec2::new(8.0, 5.0));
            assert_eq!(s.button_padding, Vec2::new(8.0, 3.0));
            assert_eq!(s.interact_size, Vec2::new(36.0, 22.0));
            assert_eq!(s.menu_margin, Margin::same(6));
            assert_eq!(s.window_margin, Margin::same(14));
            assert_eq!(s.indent, 16.0);
            assert_eq!(
                (s.scroll.floating, s.scroll.bar_width),
                (true, 8.0),
                "scroll bars"
            );
            let mut text_styles = style.text_styles.clone();
            text_styles.retain(|t, _| !matches!(t, egui::TextStyle::Name(_)));
            assert_eq!(text_styles, egui::style::default_text_styles());
            assert_eq!(super::object_names(&style), egui::FontId::monospace(13.0));
            assert_eq!(
                super::toolbar_frame(&style).inner_margin,
                Margin::symmetric(8, 4)
            );
            assert_eq!(
                super::status_frame(&style).inner_margin,
                Margin::symmetric(10, 3)
            );
            assert_eq!(super::dock_style(&style).tab_bar.height, 24.0);
            let v = &style.visuals;
            assert_eq!(v.widgets.inactive.corner_radius, CornerRadius::same(5));
            assert_eq!(v.window_corner_radius, CornerRadius::same(10));
            assert_ne!(v.window_shadow, egui::Shadow::NONE);
            assert_eq!(v.text_cursor.stroke.color, super::ACCENT);
            assert_eq!(style.animation_time, egui::Style::default().animation_time);
        }
    }

    /// Font size, density and look settings style both themes.
    #[test]
    fn appearance_settings_style_both_themes() {
        use egui::{Color32, CornerRadius, Margin, TextStyle, Vec2};
        use pythia_config::{AppearanceSettings, Density, GridSettings, ScrollBars};
        let ctx = egui::Context::default();
        super::install(&ctx);
        let accent = Color32::from_rgb(0x10, 0x80, 0x40);
        let appearance = AppearanceSettings {
            font_size: 15.6,
            density: Density::COMPACT,
            names_in_code_font: false,
            accent: Some([0x10, 0x80, 0x40]),
            rounded: false,
            shadows: false,
            scroll_bars: ScrollBars::Visible,
            scroll_bar_width: 12.0,
            animations: false,
            ..Default::default()
        };
        super::apply(&ctx, &appearance, &GridSettings::default());
        for theme in [egui::Theme::Dark, egui::Theme::Light] {
            let style = ctx.style_of(theme);
            let size = |t: TextStyle| style.text_styles[&t].size;
            assert_eq!(size(TextStyle::Body), 15.6);
            assert_eq!(size(TextStyle::Button), 15.6);
            assert!((size(TextStyle::Small) - 10.8).abs() < 0.01);
            assert!((size(TextStyle::Heading) - 21.6).abs() < 0.01);
            assert_eq!(
                super::object_names(&style),
                egui::FontId::proportional(15.6)
            );
            let s = &style.spacing;
            assert_eq!(s.item_spacing, Vec2::new(6.0, 3.0));
            assert_eq!(s.button_padding, Vec2::new(6.0, 2.0));
            assert!((s.interact_size.y - (16.0 * 1.2 + 4.0)).abs() < 0.01);
            assert_eq!(s.window_margin, Margin::same(6));
            assert_eq!(s.menu_margin, Margin::same(3));
            assert_eq!(s.indent, 12.0);
            assert_eq!((s.scroll.floating, s.scroll.bar_width), (false, 12.0));
            assert_eq!(
                super::toolbar_frame(&style).inner_margin,
                Margin::symmetric(5, 3)
            );
            assert_eq!(
                super::status_frame(&style).inner_margin,
                Margin::symmetric(7, 2)
            );
            let v = &style.visuals;
            assert_eq!(v.widgets.inactive.corner_radius, CornerRadius::ZERO);
            assert_eq!(v.menu_corner_radius, CornerRadius::ZERO);
            assert_eq!(
                (v.window_shadow, v.popup_shadow),
                (egui::Shadow::NONE, egui::Shadow::NONE)
            );
            assert_eq!(v.text_cursor.stroke.color, accent);
            assert_eq!(v.widgets.active.bg_stroke.color, accent);
            let selection = v.selection.bg_fill.to_srgba_unmultiplied();
            assert_eq!(selection[..3], [0x10, 0x80, 0x40]);
            assert_eq!(style.animation_time, 0.0);
        }
        // Back to the defaults: nothing of the above stays.
        super::apply(
            &ctx,
            &AppearanceSettings::default(),
            &GridSettings::default(),
        );
        let style = ctx.style_of(egui::Theme::Dark);
        assert_eq!(style.text_styles[&TextStyle::Body].size, 13.0);
        assert_eq!(super::object_names(&style), egui::FontId::monospace(13.0));
        assert_eq!(style.spacing.window_margin, Margin::same(14));
    }

    /// An interface font goes first; the built-in fonts stay its fallbacks,
    /// so symbols it lacks still have glyphs (#91).
    #[test]
    fn interface_font_keeps_the_fallbacks() {
        let font = "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf";
        let Ok(bytes) = std::fs::read(font) else {
            return;
        };
        let ctx = egui::Context::default();
        super::install(&ctx);
        super::set_fonts(&ctx, None, Some(bytes));
        // Nothing is rendered.
        ctx.run_ui(egui::RawInput::default(), |_| {})
            .textures_delta
            .clear();
        let (proportional, monospace) = ctx.fonts(|f| {
            let defs = f.definitions();
            (
                defs.families[&egui::FontFamily::Proportional].clone(),
                defs.families[&egui::FontFamily::Monospace].clone(),
            )
        });
        assert_eq!(proportional[0], "ui");
        assert!(
            proportional.contains(&"Hack".to_owned()),
            "{proportional:?}"
        );
        assert!(proportional.len() > 2, "{proportional:?}");
        assert!(!monospace.contains(&"ui".to_owned()), "{monospace:?}");
    }

    /// Buttons without an idle frame (icon buttons) keep their size on hover.
    #[test]
    fn frameless_button_keeps_size_on_hover() {
        let mut h = Harness::new_ui(|ui| {
            ui.add(egui::Button::new("Run").frame_when_inactive(false));
        });
        super::install(&h.ctx);
        h.run();
        let idle = h.get_by_label("Run").rect();
        h.get_by_label("Run").hover();
        h.run();
        assert_eq!(h.get_by_label("Run").rect(), idle);
    }
}
