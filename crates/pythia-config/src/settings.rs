use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeymapPreset {
    /// Allround Automations PL/SQL Developer compatible bindings.
    #[default]
    PlsqlDeveloper,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: ThemeChoice,
    pub keymap: KeymapPreset,
    /// UI zoom factor on top of the OS scale factor.
    pub ui_scale: f32,
    /// Rows fetched per round trip and shown before "fetch more".
    pub fetch_size: usize,
    pub editor: EditorSettings,
    /// Host name or address the database uses to reach Pythia's debugger
    /// (empty: this machine's host name).
    pub debug_host: String,
    /// Code formatter rules.
    pub format: pythia_plsql::FormatOptions,
    /// Shortcut overrides on top of the preset: command id → shortcut text
    /// like `Ctrl+Shift+F` (empty: no shortcut).
    pub keybindings: std::collections::BTreeMap<String, String>,
    /// The invalid objects indicator and recompiling.
    pub invalid: InvalidObjectSettings,
}

/// Which schemas the invalid objects indicator counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InvalidScope {
    /// The connection's current schema.
    #[default]
    CurrentSchema,
    /// Every schema that isn't maintained by Oracle.
    UserSchemas,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InvalidObjectSettings {
    pub scope: InvalidScope,
    /// How often the count is refreshed in the background (0: only on
    /// connect and after compiles and DDL).
    pub refresh_minutes: u32,
    /// Sessions compiling at the same time when recompiling.
    pub parallelism: usize,
}

impl Default for InvalidObjectSettings {
    fn default() -> Self {
        Self {
            scope: InvalidScope::default(),
            refresh_minutes: 5,
            parallelism: 1,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            keymap: KeymapPreset::default(),
            ui_scale: 1.0,
            fetch_size: 500,
            editor: EditorSettings::default(),
            debug_host: String::new(),
            format: pythia_plsql::FormatOptions::default(),
            keybindings: Default::default(),
            invalid: InvalidObjectSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub font_size: f32,
    /// A TTF/OTF file for code (empty: the built-in Hack).
    pub font_file: String,
    pub tab_width: usize,
    pub show_line_numbers: bool,
    pub show_whitespace: bool,
    pub highlight_current_line: bool,
    /// Offer completions while typing (Ctrl+Space always works).
    pub auto_complete: bool,
    /// Identifier characters typed before completions appear.
    pub auto_complete_min_chars: usize,
    pub auto_complete_delay_ms: u64,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            font_file: String::new(),
            tab_width: 4,
            show_line_numbers: true,
            show_whitespace: false,
            highlight_current_line: true,
            auto_complete: true,
            auto_complete_min_chars: 2,
            auto_complete_delay_ms: 150,
        }
    }
}
