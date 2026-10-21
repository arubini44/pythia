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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: ThemeChoice::default(),
            keymap: KeymapPreset::default(),
            ui_scale: 1.0,
            fetch_size: 500,
            editor: EditorSettings::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSettings {
    pub font_size: f32,
    pub tab_width: usize,
    pub show_line_numbers: bool,
}

impl Default for EditorSettings {
    fn default() -> Self {
        Self {
            font_size: 13.0,
            tab_width: 4,
            show_line_numbers: true,
        }
    }
}
