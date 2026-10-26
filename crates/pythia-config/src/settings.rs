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
    /// SQL windows commit after each statement. Never on production
    /// connections, where Commit asks first.
    pub autocommit: bool,
    /// SQL windows keep a connection of their own and can be moved to
    /// another open connection. Off by default: the app has one active
    /// connection, which the SQL windows follow.
    pub switch_window_connection: bool,
    pub editor: EditorSettings,
    /// Host name or address the database uses to reach Pythia's debugger
    /// (empty: this machine's host name).
    pub debug_host: String,
    /// Program windows compile with debug information, so the debugger can
    /// step into their units without a breakpoint there. Off by default:
    /// Oracle runs such code at a lower optimization level, which shouldn't
    /// end up in shared or production databases by accident. Units with
    /// breakpoints get debug information before a debug run either way.
    pub compile_debug_info: bool,
    /// Code formatter rules.
    pub format: pythia_plsql::FormatOptions,
    /// Named formatter rule sets to switch between and share.
    pub format_profiles: std::collections::BTreeMap<String, pythia_plsql::FormatOptions>,
    /// The profile the rules were last taken from or saved as.
    pub format_profile: Option<String>,
    /// When code is formatted without asking.
    pub format_auto: FormatTriggers,
    /// Shortcut overrides on top of the preset: command id → shortcut text
    /// like `Ctrl+Shift+F` (empty: no shortcut).
    pub keybindings: std::collections::BTreeMap<String, String>,
    /// The invalid objects indicator and recompiling.
    pub invalid: InvalidObjectSettings,
    /// The AI assistant.
    pub ai: AiSettings,
}

/// When code is formatted automatically (all off by default).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FormatTriggers {
    /// Before saving a file and before compiling a program window.
    pub on_save: bool,
    /// The statements pasted text lands in.
    pub on_paste: bool,
    /// A SQL statement when its `;` is typed.
    pub on_statement_end: bool,
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
            autocommit: false,
            switch_window_connection: false,
            editor: EditorSettings::default(),
            debug_host: String::new(),
            compile_debug_info: false,
            format: pythia_plsql::FormatOptions::default(),
            format_profiles: Default::default(),
            format_profile: None,
            format_auto: FormatTriggers::default(),
            keybindings: Default::default(),
            invalid: InvalidObjectSettings::default(),
            ai: AiSettings::default(),
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
    /// Rows the completion list shows at most (fewer if the window has no room).
    pub completion_rows: usize,
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
            completion_rows: 15,
        }
    }
}

/// AI backends and what may be sent to them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiSettings {
    pub backends: Vec<AiBackend>,
    /// Id of the backend used unless another is picked (the first if unset).
    pub default_backend: String,
    /// Rows of query results may be sent to the model.
    pub send_result_data: bool,
    /// Values entered for bind variables may be sent to the model.
    pub send_bind_values: bool,
    /// Seconds to wait for an answer.
    pub timeout_secs: u64,
}

impl Default for AiSettings {
    fn default() -> Self {
        Self {
            backends: Vec::new(),
            default_backend: String::new(),
            send_result_data: false,
            send_bind_values: false,
            timeout_secs: 300,
        }
    }
}

impl AiSettings {
    pub fn backend(&self, id: &str) -> Option<&AiBackend> {
        self.backends.iter().find(|b| b.id == id)
    }

    /// The default backend, or the first one.
    pub fn default_backend(&self) -> Option<&AiBackend> {
        self.backend(&self.default_backend)
            .or_else(|| self.backends.first())
    }
}

/// How Pythia talks to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AiKind {
    /// The Anthropic Messages API.
    #[default]
    Anthropic,
    /// A hosted API compatible with OpenAI's chat completions.
    OpenAi,
    /// A model server with an OpenAI-compatible API and no key (Ollama,
    /// llama.cpp's server, LM Studio).
    LocalServer,
    /// A command line tool: the prompt on stdin, the answer on stdout.
    Cli,
}

impl AiKind {
    pub const ALL: [AiKind; 4] = [
        AiKind::Anthropic,
        AiKind::OpenAi,
        AiKind::LocalServer,
        AiKind::Cli,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AiKind::Anthropic => "Anthropic API",
            AiKind::OpenAi => "OpenAI-compatible API",
            AiKind::LocalServer => "Local model server",
            AiKind::Cli => "Command line tool",
        }
    }

    /// Requests carry an API key from the keychain.
    pub fn uses_key(self) -> bool {
        matches!(self, AiKind::Anthropic | AiKind::OpenAi)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AiBackend {
    /// Stable id; also names the API key's keychain entry.
    pub id: String,
    pub name: String,
    pub kind: AiKind,
    /// URL of the messages or chat completions endpoint.
    pub endpoint: String,
    pub model: String,
    /// Command line of a command line tool, run by the system's shell.
    pub command: String,
    /// Longest answer in tokens (0: the server's default; the Anthropic API
    /// needs a limit and gets 16000).
    pub max_tokens: u32,
    /// Characters of context sent at most, so requests fit the model.
    pub context_chars: usize,
}

impl Default for AiBackend {
    fn default() -> Self {
        Self::new(AiKind::default(), String::new())
    }
}

impl AiBackend {
    /// A backend of `kind` with the usual endpoint.
    pub fn new(kind: AiKind, id: String) -> Self {
        let (name, endpoint, model, max_tokens, context_chars) = match kind {
            AiKind::Anthropic => (
                "Claude",
                "https://api.anthropic.com/v1/messages",
                "claude-opus-5-5",
                16000,
                400_000,
            ),
            AiKind::OpenAi => (
                "OpenAI-compatible",
                "https://api.openai.com/v1/chat/completions",
                "",
                0,
                200_000,
            ),
            AiKind::LocalServer => (
                "Ollama",
                "http://localhost:11434/v1/chat/completions",
                "",
                0,
                24_000,
            ),
            AiKind::Cli => ("Command line", "", "", 0, 100_000),
        };
        Self {
            id,
            name: name.to_owned(),
            kind,
            endpoint: endpoint.to_owned(),
            model: model.to_owned(),
            command: String::new(),
            max_tokens,
            context_chars,
        }
    }

    /// A fresh id for a new backend.
    pub fn new_id() -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!("{:x}", nanos & u128::from(u64::MAX))
    }
}
