//! Settings window. Changes apply immediately and are saved on close.

use egui::{RichText, Ui};
use orui_config::{Settings, ThemeChoice};

#[derive(Default)]
pub struct SettingsDialog {
    pub open: bool,
    section: Section,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Section {
    #[default]
    Appearance,
    Editor,
    CodeAssist,
    Query,
}

impl Section {
    const ALL: [Section; 4] = [
        Section::Appearance,
        Section::Editor,
        Section::CodeAssist,
        Section::Query,
    ];

    fn label(self) -> &'static str {
        match self {
            Section::Appearance => "Appearance",
            Section::Editor => "Editor",
            Section::CodeAssist => "Code assistance",
            Section::Query => "Queries",
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
            .default_size([620.0, 420.0])
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
                        }
                    });
                });
            });
        self.open = open;
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
