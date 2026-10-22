//! SVG sources of all icons, drawn on a 16×16 grid.
//!
//! Conventions: 1.5px strokes with round caps and joins, content kept inside
//! 1.5..14.5 so strokes have ~1px of breathing room, soft fills at 20–35%
//! opacity for "surfaces" and solid fills only for status glyphs. `$A` is the
//! primary and `$B` the secondary color; both become white in the monochrome
//! variant, which is tinted at draw time. Status glyphs (error, warning, ...)
//! cut their mark out with a mask so they work in both variants.

use super::Icon;

/// Semantic palette. Mid lightness and moderate saturation so every hue
/// reads on dark (#1e1f22) and light (#ffffff) backgrounds.
pub mod palette {
    pub const BLUE: &str = "#3d8fe0";
    pub const TEAL: &str = "#1fa596";
    pub const PURPLE: &str = "#9b6ce3";
    pub const ORANGE: &str = "#e8793a";
    pub const AMBER: &str = "#d99a1a";
    pub const YELLOW: &str = "#e8a910";
    pub const PINK: &str = "#de5b93";
    pub const GRAY: &str = "#8a919b";
    pub const CYAN: &str = "#159fc2";
    pub const INDIGO: &str = "#6f7ee8";
    pub const GREEN: &str = "#36a85f";
    pub const RED: &str = "#e2504a";
    pub const GOLD: &str = "#e0aa1c";
    pub const STEEL: &str = "#6d97cf";
    pub const SLATE: &str = "#8090a8";
    pub const FOLDER: &str = "#d7a23a";
}

pub struct Def {
    pub name: &'static str,
    pub body: &'static str,
    /// Semantic colors (`$A`, `$B`); `None` for monochrome-only icons.
    pub colors: Option<(&'static str, &'static str)>,
}

const fn mono(name: &'static str, body: &'static str) -> Def {
    Def {
        name,
        body,
        colors: None,
    }
}

const fn color(name: &'static str, a: &'static str, b: &'static str, body: &'static str) -> Def {
    Def {
        name,
        body,
        colors: Some((a, b)),
    }
}

// Shared shapes, spliced into several icons.
macro_rules! doc_outline {
    () => {
        r#"<path d="M8 14.25H4.25a1 1 0 0 1-1-1V2.75a1 1 0 0 1 1-1h5l3.5 3.5V8" stroke="$A"/><path d="M9.25 1.75v2.5a1 1 0 0 0 1 1h2.5" stroke="$A"/>"#
    };
}
macro_rules! plus_badge {
    () => {
        r#"<path d="M12.25 9.75v4.5M10 12h4.5" stroke="$B"/>"#
    };
}
macro_rules! badge_square {
    () => {
        r#"<rect x="2" y="2" width="12" height="12" rx="3" stroke="$A" fill="$A" fill-opacity=".18"/>"#
    };
}
macro_rules! badge_square_solid {
    () => {
        r#"<rect x="2" y="2" width="12" height="12" rx="3" stroke="$A" fill="$A" fill-opacity=".45"/>"#
    };
}
macro_rules! key_shape {
    () => {
        r#"<circle cx="5" cy="8" r="2.75" stroke="$A" fill="$A" fill-opacity=".25"/><path d="M7.75 8h6.25M12.5 8v2.5M10.25 8v1.75" stroke="$A"/>"#
    };
}
macro_rules! eye_shape {
    () => {
        r#"<path d="M1.75 8S4 3.75 8 3.75 14.25 8 14.25 8 12 12.25 8 12.25 1.75 8 1.75 8z" stroke="$A" fill="$A" fill-opacity=".15"/><circle cx="8" cy="8" r="1.9" fill="$A"/>"#
    };
}
macro_rules! cube {
    ($fill:literal) => {
        concat!(
            r#"<path d="M8 1.75l5.5 2.9v6.7L8 14.25l-5.5-2.9v-6.7z" stroke="$A" fill="$A" fill-opacity=""#,
            $fill,
            r#""/><path d="M2.75 4.8L8 7.6l5.25-2.8M8 7.6v6.4" stroke="$A"/>"#
        )
    };
}

use palette::*;

pub fn def(icon: Icon) -> Def {
    match icon {
        // ----- session / actions ------------------------------------------------
        Icon::Connect => color(
            "connect",
            BLUE,
            BLUE,
            r#"<path d="M6.25 2v3.75M9.75 2v3.75" stroke="$A"/><path d="M4.25 5.75h7.5v2.5a3.75 3.75 0 0 1-7.5 0z" stroke="$A" fill="$A" fill-opacity=".25"/><path d="M8 12v2.25" stroke="$A"/>"#,
        ),
        Icon::Disconnect => color(
            "disconnect",
            GRAY,
            RED,
            r#"<path d="M5.25 1.75v3.5M8.25 1.75v3.5" stroke="$A"/><path d="M3.25 5.25h7v2.25a3.5 3.5 0 0 1-7 0z" stroke="$A" fill="$A" fill-opacity=".2"/><path d="M6.75 11v3.25" stroke="$A"/><path d="M10.5 10.5l3.25 3.25M13.75 10.5l-3.25 3.25" stroke="$B"/>"#,
        ),
        Icon::Execute => color(
            "execute",
            GREEN,
            GREEN,
            r#"<path d="M4.75 3.3v9.4a.9.9 0 0 0 1.37.77l7.5-4.7a.9.9 0 0 0 0-1.54l-7.5-4.7a.9.9 0 0 0-1.37.77z" stroke="$A" fill="$A" stroke-width="1"/>"#,
        ),
        Icon::ExecuteCurrent => color(
            "execute-current",
            GREEN,
            GREEN,
            r#"<path d="M2.5 4.4v7.2a.75.75 0 0 0 1.15.63l5.6-3.6a.75.75 0 0 0 0-1.26l-5.6-3.6A.75.75 0 0 0 2.5 4.4z" stroke="$A" fill="$A" stroke-width="1"/><path d="M12.5 3v10M11 2.75h3M11 13.25h3" stroke="$B"/>"#,
        ),
        Icon::Stop => color(
            "stop",
            RED,
            RED,
            r#"<rect x="3.25" y="3.25" width="9.5" height="9.5" rx="2.25" fill="$A"/>"#,
        ),
        Icon::Commit => color(
            "commit",
            GREEN,
            GREEN,
            r#"<circle cx="8" cy="8" r="6.25" stroke="$A" fill="$A" fill-opacity=".18"/><path d="M5.1 8.2l2 2 3.8-4.1" stroke="$A"/>"#,
        ),
        Icon::Rollback => color(
            "rollback",
            AMBER,
            AMBER,
            r#"<path d="M5.25 2.75 2.5 5.5l2.75 2.75" stroke="$A"/><path d="M2.75 5.5h6.5a4 4 0 0 1 0 8H6" stroke="$A"/>"#,
        ),
        Icon::NewSql => color(
            "new-sql",
            BLUE,
            GREEN,
            concat!(
                doc_outline!(),
                r#"<path d="M5.5 7.25h4.5M5.5 9.75h2" stroke="$A"/>"#,
                plus_badge!()
            ),
        ),
        Icon::NewProgram => color(
            "new-program",
            PURPLE,
            GREEN,
            concat!(
                doc_outline!(),
                r#"<path d="M6.5 6.75 5 8.25l1.5 1.5" stroke="$A"/>"#,
                plus_badge!()
            ),
        ),
        Icon::Open => mono(
            "open",
            r#"<path d="M1.75 12V4a1.25 1.25 0 0 1 1.25-1.25h3l1.5 1.5h4.5A1.25 1.25 0 0 1 13.25 5.5V7" stroke="$A"/><path d="M1.9 12.6l1.7-4.5a1.25 1.25 0 0 1 1.17-.8h8.95a.75.75 0 0 1 .7 1.02l-1.55 4.13a1.25 1.25 0 0 1-1.17.8H2.6a.75.75 0 0 1-.7-.65z" stroke="$A" fill="$A" fill-opacity=".2"/>"#,
        ),
        Icon::Save => mono(
            "save",
            r#"<path d="M2.25 3.25a1 1 0 0 1 1-1h7.6l2.9 2.9v7.6a1 1 0 0 1-1 1H3.25a1 1 0 0 1-1-1z" stroke="$A"/><path d="M5 2.25v3h5v-3" stroke="$A"/><path d="M4.75 13.75V9.5h6.5v4.25" stroke="$A" fill="$A" fill-opacity=".25"/>"#,
        ),
        Icon::Refresh => mono(
            "refresh",
            r#"<path d="M13.25 8a5.25 5.25 0 1 1-1.54-3.71" stroke="$A"/><path d="M12.25 1.75v3h-3" stroke="$A"/>"#,
        ),
        Icon::Search => mono(
            "search",
            r#"<circle cx="7" cy="7" r="4.5" stroke="$A"/><path d="M10.4 10.4l3.6 3.6" stroke="$A"/>"#,
        ),
        Icon::Filter => mono(
            "filter",
            r#"<path d="M2.25 2.75h11.5l-4.5 5.5v4.5l-2.5 1.5v-6z" stroke="$A" fill="$A" fill-opacity=".15"/>"#,
        ),
        Icon::Settings => mono(
            "settings",
            r#"<path d="M6.40 3.74L6.79 2.02L9.21 2.02L9.60 3.74L9.88 3.86L11.37 2.92L13.08 4.63L12.14 6.12L12.26 6.40L13.98 6.79L13.98 9.21L12.26 9.60L12.14 9.88L13.08 11.37L11.37 13.08L9.88 12.14L9.60 12.26L9.21 13.98L6.79 13.98L6.40 12.26L6.12 12.14L4.63 13.08L2.92 11.37L3.86 9.88L3.74 9.60L2.02 9.21L2.02 6.79L3.74 6.40L3.86 6.12L2.92 4.63L4.63 2.92L6.12 3.86z" stroke="$A" stroke-width="1.25"/><circle cx="8" cy="8" r="2" stroke="$A"/>"#,
        ),
        Icon::Palette => mono(
            "palette",
            r#"<path d="M10 4v8a2 2 0 1 0 2-2H4a2 2 0 1 0 2 2V4a2 2 0 1 0-2 2h8a2 2 0 1 0-2-2" stroke="$A"/>"#,
        ),
        Icon::Explain => color(
            "explain",
            CYAN,
            CYAN,
            r#"<circle cx="4" cy="3.5" r="1.75" stroke="$A" fill="$A" fill-opacity=".35"/><circle cx="11.75" cy="8" r="1.75" stroke="$A" fill="$A" fill-opacity=".35"/><circle cx="11.75" cy="12.5" r="1.75" stroke="$A" fill="$A" fill-opacity=".35"/><path d="M4 5.25v7.25h6M4 8h6" stroke="$A"/>"#,
        ),
        Icon::History => mono(
            "history",
            r#"<path d="M2.75 8a5.25 5.25 0 1 0 1.54-3.71" stroke="$A"/><path d="M3.75 1.75v3h3" stroke="$A"/><path d="M8 5.5V8l1.75 1.25" stroke="$A"/>"#,
        ),
        Icon::Copy => mono(
            "copy",
            r#"<rect x="5.25" y="5.25" width="8.5" height="8.5" rx="1.5" stroke="$A" fill="$A" fill-opacity=".15"/><path d="M10.75 5V3.75a1.5 1.5 0 0 0-1.5-1.5h-5.5a1.5 1.5 0 0 0-1.5 1.5v5.5a1.5 1.5 0 0 0 1.5 1.5H5" stroke="$A"/>"#,
        ),
        Icon::Export => mono(
            "export",
            r#"<path d="M13.75 9.5v2.75a1.5 1.5 0 0 1-1.5 1.5h-8.5a1.5 1.5 0 0 1-1.5-1.5V9.5" stroke="$A"/><path d="M8 10V2.5M5 5.25l3-3 3 3" stroke="$A"/>"#,
        ),
        Icon::Compile => color(
            "compile",
            ORANGE,
            ORANGE,
            r#"<path d="M8.84 2.22l4.95 4.95-2.12 2.12-4.95-4.95z" stroke="$A" fill="$A" fill-opacity=".3"/><path d="M2.5 13.5l6.69-6.69" stroke="$A"/>"#,
        ),
        // ----- database objects -------------------------------------------------
        Icon::Database => color(
            "database",
            STEEL,
            STEEL,
            r#"<ellipse cx="8" cy="4" rx="5.25" ry="2.1" stroke="$A" fill="$A" fill-opacity=".3"/><path d="M2.75 4v8c0 1.16 2.35 2.1 5.25 2.1s5.25-.94 5.25-2.1V4" stroke="$A"/><path d="M2.75 8c0 1.16 2.35 2.1 5.25 2.1s5.25-.94 5.25-2.1" stroke="$A"/>"#,
        ),
        Icon::Schema => color(
            "schema",
            INDIGO,
            INDIGO,
            r#"<circle cx="8" cy="5.25" r="2.9" stroke="$A" fill="$A" fill-opacity=".25"/><path d="M2.75 14c.45-2.85 2.55-4.5 5.25-4.5s4.8 1.65 5.25 4.5" stroke="$A"/>"#,
        ),
        Icon::Folder => color(
            "folder",
            FOLDER,
            FOLDER,
            r#"<path d="M1.75 4a1.25 1.25 0 0 1 1.25-1.25h3l1.5 1.5h5.5A1.25 1.25 0 0 1 14.25 5.5v6.75a1.25 1.25 0 0 1-1.25 1.25H3a1.25 1.25 0 0 1-1.25-1.25z" stroke="$A" fill="$A" fill-opacity=".45"/><path d="M1.75 6.75h12.5" stroke="$A"/>"#,
        ),
        Icon::FolderOpen => color(
            "folder-open",
            FOLDER,
            FOLDER,
            r#"<path d="M1.75 12V4a1.25 1.25 0 0 1 1.25-1.25h3l1.5 1.5h4.5A1.25 1.25 0 0 1 13.25 5.5V7" stroke="$A"/><path d="M1.9 12.6l1.7-4.5a1.25 1.25 0 0 1 1.17-.8h8.95a.75.75 0 0 1 .7 1.02l-1.55 4.13a1.25 1.25 0 0 1-1.17.8H2.6a.75.75 0 0 1-.7-.65z" stroke="$A" fill="$A" fill-opacity=".5"/>"#,
        ),
        Icon::Table => color(
            "table",
            BLUE,
            BLUE,
            r#"<path d="M1.75 6.25V4.25a1.5 1.5 0 0 1 1.5-1.5h9.5a1.5 1.5 0 0 1 1.5 1.5v2z" fill="$A" fill-opacity=".4"/><rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" stroke="$A"/><path d="M1.75 6.25h12.5M1.75 9.75h12.5M6 6.25v7" stroke="$A"/>"#,
        ),
        Icon::View => color("view", TEAL, TEAL, eye_shape!()),
        Icon::MaterializedView => color(
            "materialized-view",
            TEAL,
            TEAL,
            r#"<path d="M1.75 6.5S4 2.5 8 2.5s6.25 4 6.25 4S12 10.5 8 10.5 1.75 6.5 1.75 6.5z" stroke="$A" fill="$A" fill-opacity=".15"/><circle cx="8" cy="6.5" r="1.75" fill="$A"/><path d="M2.75 13.5h10.5" stroke="$A"/>"#,
        ),
        Icon::Package => color("package", PURPLE, PURPLE, cube!(".15")),
        Icon::PackageBody => color("package-body", PURPLE, PURPLE, cube!(".5")),
        Icon::Procedure => color(
            "procedure",
            ORANGE,
            ORANGE,
            concat!(
                badge_square!(),
                r#"<path d="M6.25 11.25v-6.5h2.25a2 2 0 0 1 0 4H6.25" stroke="$A"/>"#
            ),
        ),
        Icon::Function => color(
            "function",
            AMBER,
            AMBER,
            concat!(
                badge_square!(),
                r#"<path d="M10.25 4.75h-.75a1.5 1.5 0 0 0-1.5 1.5v4a1.5 1.5 0 0 1-1.5 1.5h-.75M6 7.75h3.75" stroke="$A"/>"#
            ),
        ),
        Icon::Type => color(
            "type",
            INDIGO,
            INDIGO,
            concat!(
                badge_square!(),
                r#"<path d="M5.25 5h5.5M8 5v6.25" stroke="$A"/>"#
            ),
        ),
        Icon::TypeBody => color(
            "type-body",
            INDIGO,
            INDIGO,
            concat!(
                badge_square_solid!(),
                r#"<path d="M5.25 5h5.5M8 5v6.25" stroke="$A"/>"#
            ),
        ),
        Icon::Trigger => color(
            "trigger",
            YELLOW,
            YELLOW,
            r#"<path d="M9.25 1.75 3.75 9h4.25l-1.25 5.25L12.25 7H8z" stroke="$A" fill="$A" fill-opacity=".35"/>"#,
        ),
        Icon::Sequence => color(
            "sequence",
            PINK,
            PINK,
            r#"<path d="M6.5 2.25l-1.25 11.5M11 2.25 9.75 13.75M2.75 5.75h10.75M2.25 10.25h10.75" stroke="$A"/>"#,
        ),
        Icon::Synonym => color(
            "synonym",
            GRAY,
            GRAY,
            r#"<rect x="2" y="2" width="12" height="12" rx="3" stroke="$A" fill="$A" fill-opacity=".15"/><path d="M5.5 10.75v-.5a3.5 3.5 0 0 1 3.5-3.5h1.5M8.75 4.75l2 2-2 2" stroke="$A"/>"#,
        ),
        Icon::Index => color(
            "index",
            CYAN,
            CYAN,
            r#"<path d="M2.25 3.75h6.5M2.25 8h4.5M2.25 12.25h2.5" stroke="$A"/><path d="M12 2.75v10.5M9.75 11 12 13.25 14.25 11" stroke="$A"/>"#,
        ),
        Icon::DbLink => color(
            "db-link",
            STEEL,
            STEEL,
            r#"<path d="M6.9 9.1a2.6 2.6 0 0 0 3.68 0l2.27-2.27a2.6 2.6 0 0 0-3.68-3.68l-.7.7" stroke="$A"/><path d="M9.1 6.9a2.6 2.6 0 0 0-3.68 0L3.15 9.17a2.6 2.6 0 0 0 3.68 3.68l.7-.7" stroke="$A"/>"#,
        ),
        Icon::Java => color(
            "java",
            ORANGE,
            RED,
            r#"<path d="M2.75 7h8v3a3.5 3.5 0 0 1-3.5 3.5h-1A3.5 3.5 0 0 1 2.75 10z" stroke="$A" fill="$A" fill-opacity=".25"/><path d="M10.75 8h.75a1.75 1.75 0 0 1 0 3.5h-1" stroke="$A"/><path d="M5.25 2c-.6.85.6 1.4 0 2.5M8.25 2c-.6.85.6 1.4 0 2.5" stroke="$B"/>"#,
        ),
        Icon::Job => color(
            "job",
            TEAL,
            TEAL,
            r#"<circle cx="8" cy="9" r="5.25" stroke="$A" fill="$A" fill-opacity=".15"/><path d="M8 6.25V9l1.75 1.25M6.25 1.75h3.5M8 1.75v2" stroke="$A"/>"#,
        ),
        Icon::Column => color(
            "column",
            BLUE,
            BLUE,
            r#"<path d="M6 2.75h4v10.5H6z" fill="$A" fill-opacity=".45"/><rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" stroke="$A" stroke-opacity=".55"/><path d="M6 2.75v10.5M10 2.75v10.5" stroke="$A"/>"#,
        ),
        Icon::PrimaryKey => color("primary-key", GOLD, GOLD, key_shape!()),
        Icon::ForeignKey => color("foreign-key", STEEL, STEEL, key_shape!()),
        Icon::Constraint => color(
            "constraint",
            SLATE,
            SLATE,
            r#"<path d="M8 1.75l5.25 2v4c0 3.1-2.2 5.4-5.25 6.5-3.05-1.1-5.25-3.4-5.25-6.5v-4z" stroke="$A" fill="$A" fill-opacity=".18"/><path d="M5.6 8l1.65 1.65L10.4 6.5" stroke="$A"/>"#,
        ),
        // ----- status -----------------------------------------------------------
        Icon::Error => color(
            "error",
            RED,
            RED,
            r##"<mask id="m"><rect width="16" height="16" fill="#fff"/><path d="M5.9 5.9l4.2 4.2M10.1 5.9l-4.2 4.2" stroke="#000"/></mask><circle cx="8" cy="8" r="6.5" fill="$A" mask="url(#m)"/>"##,
        ),
        Icon::Warning => color(
            "warning",
            AMBER,
            AMBER,
            r##"<mask id="m"><rect width="16" height="16" fill="#fff"/><path d="M8 6v3.25" stroke="#000"/><circle cx="8" cy="11.6" r=".95" fill="#000"/></mask><path d="M6.84 2.55a1.33 1.33 0 0 1 2.32 0l5.6 9.8a1.33 1.33 0 0 1-1.16 2H2.4a1.33 1.33 0 0 1-1.16-2z" fill="$A" mask="url(#m)"/>"##,
        ),
        Icon::Info => color(
            "info",
            BLUE,
            BLUE,
            r##"<mask id="m"><rect width="16" height="16" fill="#fff"/><path d="M8 7.25v4" stroke="#000"/><circle cx="8" cy="4.9" r=".95" fill="#000"/></mask><circle cx="8" cy="8" r="6.5" fill="$A" mask="url(#m)"/>"##,
        ),
        Icon::Success => color(
            "success",
            GREEN,
            GREEN,
            r##"<mask id="m"><rect width="16" height="16" fill="#fff"/><path d="M5.1 8.2l2 2 3.8-4.1" stroke="#000"/></mask><circle cx="8" cy="8" r="6.5" fill="$A" mask="url(#m)"/>"##,
        ),
        Icon::Lock => mono(
            "lock",
            r#"<rect x="3" y="7" width="10" height="7.25" rx="1.75" stroke="$A" fill="$A" fill-opacity=".2"/><path d="M5.25 7V5.25a2.75 2.75 0 0 1 5.5 0V7" stroke="$A"/><path d="M8 10v1.5" stroke="$A"/>"#,
        ),
        Icon::Production => color(
            "production",
            RED,
            RED,
            r##"<mask id="m"><rect width="16" height="16" fill="#fff"/><path d="M8 5v3.5" stroke="#000"/><circle cx="8" cy="10.9" r=".95" fill="#000"/></mask><path d="M8 1.25l5.75 2.2v4.3c0 3.4-2.4 5.9-5.75 7.1-3.35-1.2-5.75-3.7-5.75-7.1v-4.3z" fill="$A" mask="url(#m)"/>"##,
        ),
        Icon::Transaction => color(
            "transaction",
            AMBER,
            AMBER,
            r#"<path d="M2.75 5.25h9.75M10 2.75l2.5 2.5L10 7.75" stroke="$A"/><path d="M13.25 10.75H3.5M6 8.25l-2.5 2.5 2.5 2.5" stroke="$A"/>"#,
        ),
        // ----- generic ui -------------------------------------------------------
        Icon::Close => mono(
            "close",
            r#"<path d="M4.25 4.25l7.5 7.5M11.75 4.25l-7.5 7.5" stroke="$A"/>"#,
        ),
        Icon::ChevronRight => mono(
            "chevron-right",
            r#"<path d="M6.25 3.5 10.75 8l-4.5 4.5" stroke="$A"/>"#,
        ),
        Icon::ChevronDown => mono(
            "chevron-down",
            r#"<path d="M3.5 6.25 8 10.75l4.5-4.5" stroke="$A"/>"#,
        ),
        Icon::Plus => mono("plus", r#"<path d="M8 3v10M3 8h10" stroke="$A"/>"#),
        Icon::Minus => mono("minus", r#"<path d="M3 8h10" stroke="$A"/>"#),
        Icon::Sun => mono(
            "sun",
            r#"<circle cx="8" cy="8" r="2.75" stroke="$A" fill="$A" fill-opacity=".25"/><path d="M12.60 8.00L14.25 8.00M11.25 11.25L12.42 12.42M8.00 12.60L8.00 14.25M4.75 11.25L3.58 12.42M3.40 8.00L1.75 8.00M4.75 4.75L3.58 3.58M8.00 3.40L8.00 1.75M11.25 4.75L12.42 3.58" stroke="$A"/>"#,
        ),
        Icon::Moon => mono(
            "moon",
            r#"<path d="M13.5 9.75A5.75 5.75 0 1 1 6.25 2.5a4.6 4.6 0 0 0 7.25 7.25z" stroke="$A" fill="$A" fill-opacity=".2"/>"#,
        ),
        Icon::Bug => color(
            "bug",
            RED,
            RED,
            r#"<path d="M5.5 5.5a2.5 2.5 0 0 1 5 0" stroke="$A"/><rect x="4.75" y="5.5" width="6.5" height="8.5" rx="3.25" stroke="$A" fill="$A" fill-opacity=".2"/><path d="M8 8.5V14M2.25 8.5h2.5M11.25 8.5h2.5M2.5 12.25l2.3-.75M13.5 12.25l-2.3-.75M5.75 2l1 1.4M10.25 2l-1 1.4" stroke="$A"/>"#,
        ),
        Icon::Breakpoint => color(
            "breakpoint",
            RED,
            RED,
            r#"<circle cx="8" cy="8" r="5" fill="$A"/>"#,
        ),
        Icon::Output => mono(
            "output",
            r#"<rect x="1.75" y="2.25" width="12.5" height="11.5" rx="2" stroke="$A"/><path d="M4.5 6 6.5 7.75 4.5 9.5M8 10h3.5" stroke="$A"/>"#,
        ),
        Icon::Grid => mono(
            "grid",
            r#"<rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" stroke="$A"/><path d="M1.75 6.25h12.5M1.75 9.75h12.5M6 2.75v10.5M10 2.75v10.5" stroke="$A"/>"#,
        ),
        Icon::Text => mono(
            "text",
            r#"<path d="M2.5 3.5h11M2.5 6.5h11M2.5 9.5h11M2.5 12.5h6.5" stroke="$A"/>"#,
        ),
        Icon::Log => mono(
            "log",
            r#"<circle cx="2.9" cy="4" r="1" fill="$A"/><circle cx="2.9" cy="8" r="1" fill="$A"/><circle cx="2.9" cy="12" r="1" fill="$A"/><path d="M6 4h7.75M6 8h7.75M6 12h5" stroke="$A"/>"#,
        ),
        Icon::Edit => mono(
            "edit",
            r#"<path d="M10.5 2.75a1.77 1.77 0 0 1 2.5 2.5L5.5 12.75 2.25 13.75l1-3.25z" stroke="$A" fill="$A" fill-opacity=".15"/><path d="M9.25 4l2.75 2.75" stroke="$A"/>"#,
        ),
        Icon::Trash => mono(
            "trash",
            r#"<path d="M2.25 4.25h11.5M6.25 4.25V2.75h3.5v1.5" stroke="$A"/><path d="M3.75 4.25l.7 8.6a1.25 1.25 0 0 0 1.25 1.15h4.6a1.25 1.25 0 0 0 1.25-1.15l.7-8.6" stroke="$A" fill="$A" fill-opacity=".15"/><path d="M6.6 7v4M9.4 7v4" stroke="$A"/>"#,
        ),
        Icon::Star => color(
            "star",
            GOLD,
            GOLD,
            r#"<path d="M8 1.9l1.85 3.85 4.2.55-3.08 2.92.78 4.18L8 11.35 4.25 13.4l.78-4.18L1.95 6.3l4.2-.55z" stroke="$A" fill="$A" fill-opacity=".35"/>"#,
        ),
        Icon::Chart => color(
            "chart",
            BLUE,
            ORANGE,
            r#"<path d="M2 1.75v12.5h12.25" stroke="$A"/><rect x="4.25" y="8" width="2.25" height="4.5" rx=".6" fill="$A" fill-opacity=".85"/><rect x="7.75" y="4.75" width="2.25" height="7.75" rx=".6" fill="$B" fill-opacity=".85"/><rect x="11.25" y="6.5" width="2.25" height="6" rx=".6" fill="$A" fill-opacity=".85"/>"#,
        ),
        Icon::Bookmark => color(
            "bookmark",
            BLUE,
            BLUE,
            r#"<path d="M4 2.75a1 1 0 0 1 1-1h6a1 1 0 0 1 1 1v11.5L8 11.25l-4 3z" stroke="$A" fill="$A" fill-opacity=".25"/>"#,
        ),
    }
}
