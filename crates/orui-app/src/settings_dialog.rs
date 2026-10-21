//! Settings window. Changes apply immediately and are saved on close.

use egui::{RichText, Ui};
use orui_config::{Settings, ThemeChoice};

#[derive(Default)]
pub struct SettingsDialog {
    pub open: bool,
    section: Section,
    keys: KeysState,
}

#[derive(Default)]
struct KeysState {
    filter: String,
    /// Waiting for the new shortcut of this command.
    recording: Option<crate::commands::Command>,
    /// What the last change did, e.g. which command lost its shortcut.
    notice: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Section {
    #[default]
    Appearance,
    Editor,
    CodeAssist,
    Query,
    Formatter,
    Keyboard,
    Debugger,
}

impl Section {
    const ALL: [Section; 7] = [
        Section::Appearance,
        Section::Editor,
        Section::CodeAssist,
        Section::Query,
        Section::Formatter,
        Section::Keyboard,
        Section::Debugger,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Editor => "Editor",
            Section::CodeAssist => "Code assistance",
            Section::Query => "Queries",
            Section::Formatter => "Formatter",
            Section::Keyboard => "Keyboard",
            Section::Debugger => "Debugger",
        }
    }
}

pub enum SettingsResult {
    /// Nothing changed this frame.
    None,
    /// Settings changed; apply them (theme, zoom) right away.
    Changed,
    /// The window was closed; persist the settings.
    Closed,
}

impl SettingsDialog {
    /// Recording a shortcut: the app must not run commands for key presses.
    pub fn is_recording(&self) -> bool {
        self.open && self.keys.recording.is_some()
    }

    #[cfg(test)]
    pub fn show_keyboard(&mut self) {
        self.section = Section::Keyboard;
    }

    #[cfg(test)]
    pub fn show_formatter(&mut self) {
        self.section = Section::Formatter;
    }

    pub fn ui(&mut self, ctx: &egui::Context, settings: &mut Settings) -> SettingsResult {
        if !self.open {
            return SettingsResult::None;
        }
        let before = settings.clone();
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size([780.0, 600.0])
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(150.0);
                        for s in Section::ALL {
                            ui.selectable_value(&mut self.section, s, s.label());
                        }
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.heading(self.section.label());
                        ui.add_space(8.0);
                        match self.section {
                            Section::Appearance => appearance(ui, settings),
                            Section::Editor => editor(ui, settings),
                            Section::CodeAssist => code_assist(ui, settings),
                            Section::Query => query(ui, settings),
                            Section::Formatter => formatter(ui, settings),
                            Section::Keyboard => keyboard(ui, settings, &mut self.keys),
                            Section::Debugger => debugger(ui, settings),
                        }
                    });
                });
            });
        self.open = open;
        if !open {
            self.keys.recording = None;
        }
        if !open {
            SettingsResult::Closed
        } else if *settings != before {
            SettingsResult::Changed
        } else {
            SettingsResult::None
        }
    }
}

fn row(ui: &mut Ui, label: &str, hint: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal(|ui| {
        ui.add_sized([180.0, 20.0], egui::Label::new(label));
        add(ui);
    });
    if !hint.is_empty() {
        ui.horizontal(|ui| {
            ui.add_space(186.0);
            ui.label(RichText::new(hint).small().weak());
        });
    }
    ui.add_space(4.0);
}

fn appearance(ui: &mut Ui, s: &mut Settings) {
    row(ui, "Theme", "", |ui| {
        ui.selectable_value(&mut s.theme, ThemeChoice::System, "System");
        ui.selectable_value(&mut s.theme, ThemeChoice::Light, "Light");
        ui.selectable_value(&mut s.theme, ThemeChoice::Dark, "Dark");
    });
    row(
        ui,
        "Interface zoom",
        "Ctrl+Plus / Ctrl+Minus / Ctrl+0",
        |ui| {
            ui.add(
                egui::Slider::new(&mut s.ui_scale, 0.5..=3.0)
                    .step_by(0.05)
                    .fixed_decimals(2),
            );
        },
    );
}

fn editor(ui: &mut Ui, s: &mut Settings) {
    let e = &mut s.editor;
    row(ui, "Font size", "", |ui| {
        ui.add(egui::Slider::new(&mut e.font_size, 8.0..=32.0).step_by(0.5));
    });
    row(
        ui,
        "Font",
        "A TrueType/OpenType file; empty uses the built-in Hack",
        |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut e.font_file)
                    .hint_text("Hack (built in)")
                    .desired_width(260.0),
            );
            if ui.button("Browse…").clicked()
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("Fonts", &["ttf", "otf", "ttc"])
                    .pick_file()
            {
                e.font_file = path.display().to_string();
            }
            if !e.font_file.is_empty() && ui.small_button("×").clicked() {
                e.font_file.clear();
            }
        },
    );
    row(ui, "Tab width", "Tab inserts spaces", |ui| {
        ui.add(egui::Slider::new(&mut e.tab_width, 1..=8));
    });
    row(ui, "Line numbers", "", |ui| {
        ui.checkbox(&mut e.show_line_numbers, "");
    });
    row(ui, "Highlight current line", "", |ui| {
        ui.checkbox(&mut e.highlight_current_line, "");
    });
    row(ui, "Show whitespace", "Spaces as ·, tabs as →", |ui| {
        ui.checkbox(&mut e.show_whitespace, "");
    });
}

fn code_assist(ui: &mut Ui, s: &mut Settings) {
    let e = &mut s.editor;
    row(
        ui,
        "Complete while typing",
        "Ctrl+Space always opens completions",
        |ui| {
            ui.checkbox(&mut e.auto_complete, "");
        },
    );
    ui.add_enabled_ui(e.auto_complete, |ui| {
        row(
            ui,
            "After characters",
            "Identifier characters typed before suggestions appear",
            |ui| {
                ui.add(egui::Slider::new(&mut e.auto_complete_min_chars, 1..=6));
            },
        );
        row(
            ui,
            "Delay",
            "Pause after typing before suggestions appear",
            |ui| {
                ui.add(egui::Slider::new(&mut e.auto_complete_delay_ms, 0..=1000).suffix(" ms"));
            },
        );
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new("Typing . after a table alias or package always completes immediately.")
            .small()
            .weak(),
    );
}

fn query(ui: &mut Ui, s: &mut Settings) {
    row(
        ui,
        "Rows per fetch",
        "Rows loaded at once; scrolling to the end fetches more",
        |ui| {
            ui.add(
                egui::DragValue::new(&mut s.fetch_size)
                    .range(10..=100_000)
                    .speed(10),
            );
        },
    );
}

fn debugger(ui: &mut Ui, s: &mut Settings) {
    row(
        ui,
        "Debugger host",
        "How the database server reaches this machine (the database connects back to orui). \
         Empty: this computer's host name. For a local container: host.containers.internal",
        |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut s.debug_host)
                    .hint_text(orui_debugger::target::default_host())
                    .desired_width(260.0),
            );
        },
    );
}

const FORMAT_SAMPLE: &str = "select e.employee_id, e.last_name as name, d.department_name as dept from employees e join departments d on d.department_id = e.department_id where e.salary > 1000 and e.hire_date > sysdate - 30;\n\ncreate or replace procedure raise(p_id in number, p_pct in number default 5) is\n  l_old number; l_new_salary number;\nbegin\n  select salary into l_old from employees where employee_id = p_id;\n  l_new_salary := l_old * (1 + p_pct / 100);\n  if l_new_salary > 10000 then raise_application_error(-20001, 'too much'); end if;\nend;\n/\n";

fn formatter(ui: &mut Ui, s: &mut Settings) {
    use orui_plsql::{Case, CommaStyle};
    let f = &mut s.format;
    let case = |ui: &mut Ui, value: &mut Case| {
        ui.selectable_value(value, Case::Lower, "lower");
        ui.selectable_value(value, Case::Upper, "UPPER");
        ui.selectable_value(value, Case::Preserve, "as written");
    };
    row(ui, "Keywords", "", |ui| case(ui, &mut f.keyword_case));
    row(
        ui,
        "Identifiers",
        "Quoted identifiers are never changed",
        |ui| case(ui, &mut f.identifier_case),
    );
    row(ui, "Indent", "", |ui| {
        ui.add(egui::Slider::new(&mut f.indent, 1..=8).suffix(" spaces"));
    });
    row(ui, "Line width", "", |ui| {
        ui.add(egui::Slider::new(&mut f.max_width, 60..=200));
    });
    row(ui, "Commas", "", |ui| {
        ui.selectable_value(&mut f.comma, CommaStyle::Trailing, "a,  b");
        ui.selectable_value(&mut f.comma, CommaStyle::Leading, ", b");
    });
    row(ui, "Align", "", |ui| {
        ui.checkbox(&mut f.align_declarations, "declarations");
        ui.checkbox(&mut f.align_assignments, ":=");
        ui.checkbox(&mut f.align_aliases, "aliases");
    });
    row(ui, "JOIN … ON", "", |ui| {
        ui.checkbox(&mut f.on_new_line, "ON on its own line");
    });
    ui.add_space(6.0);
    ui.label(RichText::new("Preview").strong());
    let preview = orui_plsql::format(FORMAT_SAMPLE, f).text;
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(6.0)
        .inner_margin(8.0)
        .show(ui, |ui| {
            egui::ScrollArea::both().max_height(360.0).show(ui, |ui| {
                ui.add(egui::Label::new(RichText::new(preview).monospace()).extend());
            });
        });
}

fn keyboard(ui: &mut Ui, s: &mut Settings, st: &mut KeysState) {
    use crate::commands::{Command, Keymap, shortcut_string};
    let preset = Keymap::new(s.keymap);
    let current = Keymap::with_overrides(s.keymap, &s.keybindings);

    if let Some(command) = st.recording {
        // The next key press (with modifiers) becomes the shortcut.
        let pressed = ui.ctx().input_mut(|i| {
            let found = i.events.iter().find_map(|e| match e {
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => Some((*key, *modifiers)),
                _ => None,
            });
            if let Some((key, modifiers)) = found {
                i.consume_key(modifiers, key);
            }
            found
        });
        match pressed {
            Some((egui::Key::Escape, m)) if m.is_none() => st.recording = None,
            Some((key, m)) => {
                let mut modifiers = egui::Modifiers::NONE;
                if m.command || m.ctrl || m.mac_cmd {
                    modifiers = modifiers | egui::Modifiers::COMMAND;
                }
                if m.shift {
                    modifiers = modifiers | egui::Modifiers::SHIFT;
                }
                if m.alt {
                    modifiers = modifiers | egui::Modifiers::ALT;
                }
                let shortcut = egui::KeyboardShortcut::new(modifiers, key);
                st.notice = None;
                if let Some(other) = current.command_for(&shortcut).filter(|c| *c != command) {
                    s.keybindings.insert(other.id().to_owned(), String::new());
                    st.notice = Some(format!(
                        "{} was taken from \"{}\"",
                        shortcut_string(&shortcut),
                        other.label()
                    ));
                }
                if preset.shortcut(command) == Some(&shortcut) {
                    s.keybindings.remove(command.id());
                } else {
                    s.keybindings
                        .insert(command.id().to_owned(), shortcut_string(&shortcut));
                }
                st.recording = None;
            }
            None => {}
        }
    }

    ui.horizontal(|ui| {
        ui.add(
            egui::TextEdit::singleline(&mut st.filter)
                .hint_text("Filter commands or shortcuts…")
                .desired_width(260.0),
        );
        if !s.keybindings.is_empty()
            && ui
                .button("Reset all")
                .on_hover_text("Back to the PL/SQL Developer compatible preset")
                .clicked()
        {
            s.keybindings.clear();
            st.notice = None;
        }
    });
    if let Some(notice) = &st.notice {
        ui.label(
            RichText::new(notice)
                .small()
                .color(ui.visuals().warn_fg_color),
        );
    }
    ui.add_space(4.0);
    let needle = st.filter.trim().to_lowercase();
    let current = Keymap::with_overrides(s.keymap, &s.keybindings);
    egui::ScrollArea::vertical()
        .max_height(440.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            egui::Grid::new("keybindings")
                .num_columns(3)
                .striped(true)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    for &command in Command::ALL {
                        let text = current
                            .shortcut(command)
                            .map(shortcut_string)
                            .unwrap_or_default();
                        if !needle.is_empty()
                            && !command.label().to_lowercase().contains(&needle)
                            && !text.to_lowercase().contains(&needle)
                        {
                            continue;
                        }
                        let changed = s.keybindings.contains_key(command.id());
                        let mut label = RichText::new(command.label());
                        if changed {
                            label = label.strong();
                        }
                        ui.label(label);
                        let recording = st.recording == Some(command);
                        let button = if recording {
                            egui::Button::new(
                                RichText::new("Press keys… (Esc cancels)")
                                    .color(ui.visuals().hyperlink_color),
                            )
                        } else if text.is_empty() {
                            egui::Button::new(RichText::new("—").weak())
                        } else {
                            egui::Button::new(RichText::new(&text).monospace())
                        };
                        if ui
                            .add_sized([200.0, 20.0], button)
                            .on_hover_text("Click, then press the new shortcut")
                            .clicked()
                        {
                            st.recording = (!recording).then_some(command);
                        }
                        ui.horizontal(|ui| {
                            if !text.is_empty()
                                && ui.small_button("×").on_hover_text("No shortcut").clicked()
                            {
                                s.keybindings.insert(command.id().to_owned(), String::new());
                            }
                            if changed
                                && ui
                                    .small_button("Reset")
                                    .on_hover_text(match preset.shortcut(command) {
                                        Some(d) => format!("Default: {}", shortcut_string(d)),
                                        None => "Default: none".into(),
                                    })
                                    .clicked()
                            {
                                s.keybindings.remove(command.id());
                            }
                        });
                        ui.end_row();
                    }
                });
        });
    ui.add_space(6.0);
    ui.label(
        RichText::new("Editor keys (folding, bookmarks, multi-cursor) are fixed.")
            .small()
            .weak(),
    );
}
