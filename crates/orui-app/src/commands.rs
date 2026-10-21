//! All user actions as commands.
//!
//! Menus, toolbar, keyboard shortcuts and the command palette all dispatch a
//! [`Command`], so every action is reachable from each of them (and from
//! macros later on).

use egui::{Key, KeyboardShortcut, Modifiers};
use orui_config::KeymapPreset;

macro_rules! commands {
    ($($variant:ident => $label:literal,)*) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Command {
            $($variant,)*
        }

        impl Command {
            pub const ALL: &[Command] = &[$(Command::$variant,)*];

            pub fn label(self) -> &'static str {
                match self {
                    $(Command::$variant => $label,)*
                }
            }
        }
    };
}

commands! {
    NewSqlWindow => "File: New SQL Window",
    OpenFile => "File: Open…",
    SaveFile => "File: Save",
    SaveFileAs => "File: Save As…",
    CloseTab => "File: Close Window",
    CloseOtherTabs => "File: Close Other Windows",
    ReopenClosed => "File: Reopen Closed Window",
    Settings => "File: Settings…",
    Connect => "Session: Connect…",
    Disconnect => "Session: Disconnect",
    ExecuteAll => "Session: Execute (selection or all)",
    ExecuteCurrent => "Session: Execute Current Statement",
    ExplainPlan => "Session: Explain Plan",
    ExecuteWithStatistics => "Session: Run with Statistics (actual plan)",
    Cancel => "Session: Break (cancel execution)",
    Commit => "Session: Commit",
    Rollback => "Session: Rollback",
    ToggleAutocommit => "Session: Toggle Auto-Commit",
    History => "Edit: Recall Statement (history)",
    Format => "Edit: Format Code",
    FindObject => "Tools: Find Database Object…",
    FindInSource => "Tools: Find in Database Source…",
    SearchData => "Tools: Search Data in Tables…",
    ImportData => "Tools: Import Data (CSV, Excel)…",
    CompareSchemas => "Tools: Compare Schemas…",
    CompareData => "Tools: Compare Table Data…",
    DebugStart => "Debug: Debug Block Under Cursor",
    ToggleBreakpoint => "Debug: Toggle Breakpoint",
    DebugContinue => "Debug: Continue",
    DebugStepOver => "Debug: Step Over",
    DebugStepInto => "Debug: Step Into",
    DebugStepOut => "Debug: Step Out",
    DebugStop => "Debug: Stop",
    ShowDebugger => "View: Debugger",
    MonitorDashboard => "Monitor: Dashboard",
    MonitorSessions => "Monitor: Sessions",
    MonitorBlocking => "Monitor: Blocking Sessions",
    MonitorAwr => "Monitor: AWR / ASH Reports",
    MonitorTrace => "Monitor: Trace Analyzer (tkprof)",
    ShowPalette => "View: Command Palette",
    ShowBrowser => "View: Object Browser",
    ShowLog => "View: Log",
    ThemeDark => "View: Dark Theme",
    ThemeLight => "View: Light Theme",
    ThemeSystem => "View: System Theme",
    ZoomIn => "View: Zoom In",
    ZoomOut => "View: Zoom Out",
    ZoomReset => "View: Reset Zoom",
    ResetLayout => "View: Reset Layout",
    About => "Help: About orui",
    Quit => "File: Exit",
}

impl Command {
    /// Label without the category prefix, for menus.
    pub fn short_label(self) -> &'static str {
        let label = self.label();
        label.split_once(": ").map_or(label, |(_, rest)| rest)
    }
}

/// Maps commands to keyboard shortcuts.
pub struct Keymap {
    bindings: Vec<(Command, KeyboardShortcut)>,
}

impl Keymap {
    pub fn new(preset: KeymapPreset) -> Self {
        let ctrl = Modifiers::COMMAND;
        let shift = Modifiers::SHIFT;
        let none = Modifiers::NONE;
        let bindings = match preset {
            KeymapPreset::PlsqlDeveloper => vec![
                (Command::NewSqlWindow, KeyboardShortcut::new(ctrl, Key::N)),
                (Command::OpenFile, KeyboardShortcut::new(ctrl, Key::O)),
                (
                    Command::ReopenClosed,
                    KeyboardShortcut::new(ctrl | shift, Key::T),
                ),
                (Command::DebugStart, KeyboardShortcut::new(ctrl, Key::F9)),
                (
                    Command::ToggleBreakpoint,
                    KeyboardShortcut::new(ctrl, Key::B),
                ),
                (
                    Command::SaveFileAs,
                    KeyboardShortcut::new(ctrl | shift, Key::S),
                ),
                (Command::SaveFile, KeyboardShortcut::new(ctrl, Key::S)),
                (Command::CloseTab, KeyboardShortcut::new(ctrl, Key::W)),
                (Command::Settings, KeyboardShortcut::new(ctrl, Key::Comma)),
                (Command::ExecuteAll, KeyboardShortcut::new(none, Key::F8)),
                (
                    Command::ExecuteCurrent,
                    KeyboardShortcut::new(ctrl, Key::Enter),
                ),
                (Command::ExplainPlan, KeyboardShortcut::new(none, Key::F5)),
                (
                    Command::ExecuteWithStatistics,
                    KeyboardShortcut::new(ctrl, Key::F5),
                ),
                (Command::Cancel, KeyboardShortcut::new(shift, Key::Escape)),
                (Command::Commit, KeyboardShortcut::new(none, Key::F10)),
                (Command::Rollback, KeyboardShortcut::new(shift, Key::F10)),
                (Command::History, KeyboardShortcut::new(ctrl, Key::E)),
                (Command::Format, KeyboardShortcut::new(ctrl | shift, Key::F)),
                (
                    Command::FindObject,
                    KeyboardShortcut::new(ctrl | shift, Key::O),
                ),
                (
                    Command::FindInSource,
                    KeyboardShortcut::new(ctrl | Modifiers::ALT, Key::F),
                ),
                (
                    Command::ShowPalette,
                    KeyboardShortcut::new(ctrl | shift, Key::P),
                ),
                (Command::ZoomIn, KeyboardShortcut::new(ctrl, Key::Plus)),
                (Command::ZoomIn, KeyboardShortcut::new(ctrl, Key::Equals)),
                (Command::ZoomOut, KeyboardShortcut::new(ctrl, Key::Minus)),
                (Command::ZoomReset, KeyboardShortcut::new(ctrl, Key::Num0)),
            ],
        };
        Self { bindings }
    }

    pub fn shortcut(&self, command: Command) -> Option<&KeyboardShortcut> {
        self.bindings
            .iter()
            .find(|(c, _)| *c == command)
            .map(|(_, s)| s)
    }

    /// Formatted shortcut for menus and tooltips (empty if none).
    pub fn shortcut_text(&self, ctx: &egui::Context, command: Command) -> String {
        self.shortcut(command)
            .map(|s| ctx.format_shortcut(s))
            .unwrap_or_default()
    }

    /// Consumes pressed shortcuts and returns the triggered commands.
    pub fn triggered(&self, ctx: &egui::Context) -> Vec<Command> {
        // Most specific shortcuts first, so Shift+F10 isn't eaten by F10.
        let mut bindings: Vec<_> = self.bindings.iter().collect();
        bindings.sort_by_key(|(_, s)| std::cmp::Reverse(modifier_count(s.modifiers)));
        ctx.input_mut(|input| {
            bindings
                .into_iter()
                .filter(|(_, s)| input.consume_shortcut(s))
                .map(|(c, _)| *c)
                .collect()
        })
    }
}

fn modifier_count(m: Modifiers) -> u32 {
    u32::from(m.alt) + u32::from(m.ctrl || m.command) + u32::from(m.shift) + u32::from(m.mac_cmd)
}

/// Fuzzy subsequence match. Returns a score (higher is better) or `None`.
///
/// Consecutive matches and matches at word starts score higher, so "ex cur"
/// ranks "Execute Current Statement" above "Execute (selection or all)".
pub fn fuzzy_score(query: &str, candidate: &str) -> Option<i32> {
    let mut score = 0;
    let mut prev_match: Option<usize> = None;
    let cand: Vec<char> = candidate.chars().collect();
    let mut pos = 0;
    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let q = q.to_ascii_lowercase();
        let matches = |i: &usize| cand[*i].to_ascii_lowercase() == q;
        let word_start = |i: &usize| *i == 0 || !cand[*i - 1].is_alphanumeric();
        // Prefer continuing the current run, then the start of a later word,
        // then any occurrence.
        let found = Some(pos)
            .filter(|i| prev_match.is_some() && *i < cand.len() && matches(i))
            .or_else(|| (pos..cand.len()).find(|i| matches(i) && word_start(i)))
            .or_else(|| (pos..cand.len()).find(matches))?;
        score += 1;
        if prev_match == Some(found.wrapping_sub(1)) {
            score += 5;
        }
        if word_start(&found) {
            score += 3;
        }
        prev_match = Some(found);
        pos = found + 1;
    }
    Some(score - cand.len() as i32 / 10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy() {
        assert!(fuzzy_score("xyz", "Session: Commit").is_none());
        assert!(fuzzy_score("", "anything").is_some());
        let current = fuzzy_score("ex cur", Command::ExecuteCurrent.label()).unwrap();
        let all = fuzzy_score("ex cur", Command::ExecuteAll.label());
        assert!(all.is_none_or(|all| current > all));
    }

    #[test]
    fn labels_unique() {
        let mut labels: Vec<_> = Command::ALL.iter().map(|c| c.label()).collect();
        labels.sort();
        labels.dedup();
        assert_eq!(labels.len(), Command::ALL.len());
    }
}
