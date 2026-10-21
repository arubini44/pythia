//! All user actions as commands.
//!
//! Menus, toolbar, keyboard shortcuts and the command palette all dispatch a
//! [`Command`], so every action is reachable from each of them (and from
//! macros later on).

use egui::{Key, KeyboardShortcut, Modifiers};
use orui_config::KeymapPreset;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    NewSqlWindow,
    CloseTab,
    Connect,
    Disconnect,
    ExecuteAll,
    ExecuteCurrent,
    Cancel,
    Commit,
    Rollback,
    ToggleAutocommit,
    ShowPalette,
    ShowObjectBrowser,
    ShowOutput,
    ThemeDark,
    ThemeLight,
    ThemeSystem,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ResetLayout,
    About,
    Quit,
}

impl Command {
    pub const ALL: &[Command] = &[
        Command::NewSqlWindow,
        Command::CloseTab,
        Command::Connect,
        Command::Disconnect,
        Command::ExecuteAll,
        Command::ExecuteCurrent,
        Command::Cancel,
        Command::Commit,
        Command::Rollback,
        Command::ToggleAutocommit,
        Command::ShowPalette,
        Command::ShowObjectBrowser,
        Command::ShowOutput,
        Command::ThemeDark,
        Command::ThemeLight,
        Command::ThemeSystem,
        Command::ZoomIn,
        Command::ZoomOut,
        Command::ZoomReset,
        Command::ResetLayout,
        Command::About,
        Command::Quit,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Command::NewSqlWindow => "File: New SQL Window",
            Command::CloseTab => "File: Close Window",
            Command::Connect => "Session: Connect…",
            Command::Disconnect => "Session: Disconnect",
            Command::ExecuteAll => "Session: Execute (selection or all)",
            Command::ExecuteCurrent => "Session: Execute Current Statement",
            Command::Cancel => "Session: Break (cancel execution)",
            Command::Commit => "Session: Commit",
            Command::Rollback => "Session: Rollback",
            Command::ToggleAutocommit => "Session: Toggle Auto-Commit",
            Command::ShowPalette => "View: Command Palette",
            Command::ShowObjectBrowser => "View: Object Browser",
            Command::ShowOutput => "View: Log",
            Command::ThemeDark => "View: Dark Theme",
            Command::ThemeLight => "View: Light Theme",
            Command::ThemeSystem => "View: System Theme",
            Command::ZoomIn => "View: Zoom In",
            Command::ZoomOut => "View: Zoom Out",
            Command::ZoomReset => "View: Reset Zoom",
            Command::ResetLayout => "View: Reset Layout",
            Command::About => "Help: About orui",
            Command::Quit => "File: Exit",
        }
    }

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
                (Command::CloseTab, KeyboardShortcut::new(ctrl, Key::W)),
                (Command::ExecuteAll, KeyboardShortcut::new(none, Key::F8)),
                (
                    Command::ExecuteCurrent,
                    KeyboardShortcut::new(ctrl, Key::Enter),
                ),
                (Command::Cancel, KeyboardShortcut::new(shift, Key::Escape)),
                (Command::Commit, KeyboardShortcut::new(none, Key::F10)),
                (Command::Rollback, KeyboardShortcut::new(shift, Key::F10)),
                (
                    Command::ShowPalette,
                    KeyboardShortcut::new(ctrl | shift, Key::P),
                ),
                (Command::ZoomIn, KeyboardShortcut::new(ctrl, Key::Plus)),
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
        if found == 0 || !cand[found - 1].is_alphanumeric() {
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
    fn every_command_listed() {
        assert_eq!(Command::ALL.len(), 22);
    }
}
