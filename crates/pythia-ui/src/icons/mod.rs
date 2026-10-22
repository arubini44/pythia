//! Pythia's icon set: hand-drawn 16×16 SVG icons rendered crisply at any zoom.
//!
//! Icons come in two variants:
//! - **color**: semantic hues for object types and key actions (tables blue,
//!   packages purple, execute green, ...). Monochrome-only icons fall back to
//!   the text color.
//! - **mono**: white, tinted at draw time, e.g. to the current text color.
//!
//! Call [`install`] once at startup so egui can rasterize SVGs, then use
//! [`Icon::image`] (or the button/label helpers) wherever an icon is shown.

mod defs;

use std::borrow::Cow;
use std::sync::OnceLock;

use egui::load::Bytes;
use egui::{Color32, Image, ImageSource, Rect, Response, Sense, Ui, Vec2, WidgetText, vec2};

pub use defs::palette;

/// Registers egui's image loaders (including SVG). Call once, e.g. in the
/// `eframe` app creator.
pub fn install(ctx: &egui::Context) {
    egui_extras::install_image_loaders(ctx);
}

/// Standard icon sizes in points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconSize {
    /// Tree rows, tabs, dense lists.
    Small,
    /// Toolbars, menus, buttons.
    Normal,
    /// Dialog headers, empty states.
    Large,
}

impl IconSize {
    pub fn points(self) -> f32 {
        match self {
            IconSize::Small => 14.0,
            IconSize::Normal => 16.0,
            IconSize::Large => 20.0,
        }
    }
}

macro_rules! icons {
    ($($name:ident),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Icon { $($name),* }

        impl Icon {
            pub const ALL: &[Icon] = &[$(Icon::$name),*];
        }
    };
}

icons! {
    // session / actions
    Connect, Disconnect, Execute, ExecuteCurrent, Stop, Commit, Rollback,
    NewSql, NewProgram, Open, Save, Refresh, Search, Filter, Settings, Palette,
    Explain, History, Copy, Export, Compile,
    // database objects
    Database, Schema, Folder, FolderOpen, Table, View, MaterializedView,
    Package, PackageBody, Procedure, Function, Type, TypeBody, Trigger,
    Sequence, Synonym, Index, DbLink, Java, Job, Column, PrimaryKey,
    ForeignKey, Constraint,
    // status
    Error, Warning, Info, Success, Lock, Production, Transaction,
    // generic ui
    Close, ChevronRight, ChevronDown, Plus, Minus, Sun, Moon, Bug, Breakpoint,
    Output, Grid, Text, Log, Edit, Trash, Star, Bookmark, Chart,
}

struct Rendered {
    color_uri: String,
    color_svg: String,
    mono_uri: String,
    mono_svg: String,
    colored: bool,
}

fn rendered(icon: Icon) -> &'static Rendered {
    static CACHE: OnceLock<Vec<Rendered>> = OnceLock::new();
    let all = CACHE.get_or_init(|| {
        Icon::ALL
            .iter()
            .map(|&icon| {
                let def = defs::def(icon);
                let (a, b) = def.colors.unwrap_or(("#ffffff", "#ffffff"));
                Rendered {
                    color_uri: format!("bytes://pythia-icon/{}.svg", def.name),
                    color_svg: svg_document(def.body, a, b),
                    mono_uri: format!("bytes://pythia-icon/{}-mono.svg", def.name),
                    mono_svg: svg_document(def.body, "#ffffff", "#ffffff"),
                    colored: def.colors.is_some(),
                }
            })
            .collect()
    });
    &all[icon as usize]
}

fn svg_document(body: &str, a: &str, b: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" viewBox="0 0 16 16" fill="none" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        body.replace("$A", a).replace("$B", b)
    )
}

impl Icon {
    /// Stable kebab-case name, e.g. `package-body`.
    pub fn name(self) -> &'static str {
        defs::def(self).name
    }

    /// True if the icon has semantic colors; false for monochrome-only icons.
    pub fn is_colored(self) -> bool {
        rendered(self).colored
    }

    /// The SVG document of the color (`mono = false`) or white variant.
    pub fn svg(self, mono: bool) -> &'static str {
        let r = rendered(self);
        if mono { &r.mono_svg } else { &r.color_svg }
    }

    /// Image source for the color or white variant.
    pub fn source(self, mono: bool) -> ImageSource<'static> {
        let r = rendered(self);
        let (uri, svg) = if mono {
            (&r.mono_uri, &r.mono_svg)
        } else {
            (&r.color_uri, &r.color_svg)
        };
        ImageSource::Bytes {
            uri: Cow::Borrowed(uri.as_str()),
            bytes: Bytes::Static(svg.as_bytes()),
        }
    }

    /// The icon as it should normally appear: semantic colors if it has them,
    /// otherwise tinted with the ui's text color. 16pt.
    pub fn image(self, ui: &Ui) -> Image<'static> {
        if self.is_colored() {
            Image::new(self.source(false)).fit_to_exact_size(Vec2::splat(IconSize::Normal.points()))
        } else {
            self.mono(ui.visuals().text_color())
        }
    }

    /// The white variant tinted with `color`. 16pt.
    pub fn mono(self, color: Color32) -> Image<'static> {
        Image::new(self.source(true))
            .tint(color)
            .fit_to_exact_size(Vec2::splat(IconSize::Normal.points()))
    }

    /// Same as [`Icon::image`] at a given size.
    pub fn sized(self, ui: &Ui, size: IconSize) -> Image<'static> {
        self.image(ui).fit_to_exact_size(Vec2::splat(size.points()))
    }

    /// Icon for an Oracle dictionary object type (`ALL_OBJECTS.OBJECT_TYPE`).
    pub fn for_object_type(object_type: &str) -> Option<Icon> {
        Some(match object_type.trim().to_ascii_uppercase().as_str() {
            "TABLE" | "TABLE PARTITION" | "TABLE SUBPARTITION" => Icon::Table,
            "VIEW" => Icon::View,
            "MATERIALIZED VIEW" => Icon::MaterializedView,
            "PACKAGE" => Icon::Package,
            "PACKAGE BODY" => Icon::PackageBody,
            "PROCEDURE" => Icon::Procedure,
            "FUNCTION" => Icon::Function,
            "TYPE" => Icon::Type,
            "TYPE BODY" => Icon::TypeBody,
            "TRIGGER" => Icon::Trigger,
            "SEQUENCE" => Icon::Sequence,
            "SYNONYM" => Icon::Synonym,
            "INDEX" | "INDEX PARTITION" | "LOB" => Icon::Index,
            "DATABASE LINK" => Icon::DbLink,
            "JAVA SOURCE" | "JAVA CLASS" | "JAVA RESOURCE" => Icon::Java,
            "JOB" | "SCHEDULE" | "PROGRAM" | "CHAIN" => Icon::Job,
            "DIRECTORY" => Icon::Folder,
            "LIBRARY" => Icon::Package,
            "USER" | "SCHEMA" => Icon::Schema,
            "COLUMN" => Icon::Column,
            "CONSTRAINT" => Icon::Constraint,
            _ => return None,
        })
    }
}

/// Flat icon-only button: 16pt icon in a 24pt hit area, background on hover.
pub fn icon_button(ui: &mut Ui, icon: Icon, tooltip: &str) -> Response {
    let button = egui::Button::image(icon_for_button(ui, icon))
        .frame_when_inactive(false)
        .image_tint_follows_text_color(!icon.is_colored())
        .min_size(vec2(24.0, 24.0));
    let response = ui.add(button);
    if tooltip.is_empty() {
        response
    } else {
        response.on_hover_text(tooltip)
    }
}

/// Flat button with an icon and a label.
pub fn icon_text_button(ui: &mut Ui, icon: Icon, text: impl Into<WidgetText>) -> Response {
    let button = egui::Button::image_and_text(icon_for_button(ui, icon), text)
        .frame_when_inactive(false)
        .image_tint_follows_text_color(!icon.is_colored())
        .min_size(vec2(0.0, 24.0));
    ui.add(button)
}

/// Icon followed by a label, e.g. a tree row or a status bar entry.
pub fn icon_label(ui: &mut Ui, icon: Icon, text: impl Into<WidgetText>) -> Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        ui.add(icon.image(ui));
        ui.label(text)
    })
    .inner
}

/// Mono icons in buttons are white and tinted by the button's text color.
fn icon_for_button(ui: &Ui, icon: Icon) -> Image<'static> {
    if icon.is_colored() {
        icon.image(ui)
    } else {
        icon.mono(Color32::WHITE)
    }
}

/// Paints a small red badge into the bottom-right corner of an icon `rect`,
/// marking invalid objects. The ring uses the panel color so the badge stays
/// legible on top of the icon.
pub fn paint_invalid_badge(ui: &Ui, rect: Rect) {
    let r = rect.width() * 0.2;
    let center = rect.right_bottom() - vec2(r, r) + vec2(0.5, 0.5);
    let painter = ui.painter();
    painter.circle_filled(center, r + 1.25, ui.visuals().panel_fill);
    painter.circle_filled(center, r, Color32::from_rgb(0xe2, 0x50, 0x4a));
}

/// Adds an icon and returns its response with `sense`, for custom widgets
/// (e.g. clickable gutter markers).
pub fn icon_widget(ui: &mut Ui, icon: Icon, size: IconSize, sense: Sense) -> Response {
    ui.add(icon.sized(ui, size).sense(sense))
}

#[cfg(test)]
mod tests;
