//! Modal dialogs: connect, command palette, confirmations.

use egui::{Key, RichText};
use orui_config::{ConnectionProfile, Connections, Privilege};

use crate::commands::{Command, Keymap, fuzzy_score};

#[derive(Default)]
pub struct ConnectDialog {
    pub open: bool,
    pub connecting: bool,
    pub error: Option<String>,
    form: ConnectionProfile,
    password: String,
    save_profile: bool,
    focus_requested: bool,
}

pub struct ConnectRequest {
    pub profile: ConnectionProfile,
    pub password: String,
    pub save_profile: bool,
}

impl ConnectDialog {
    pub fn show(&mut self, last: Option<&ConnectionProfile>) {
        self.open = true;
        self.error = None;
        self.focus_requested = false;
        self.save_profile = true;
        if let Some(last) = last {
            self.form = last.clone();
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context, saved: &Connections) -> Option<ConnectRequest> {
        if !self.open {
            return None;
        }
        let mut request = None;
        let modal = egui::Modal::new(egui::Id::new("connect-dialog")).show(ctx, |ui| {
            ui.set_width(420.0);
            ui.heading("Connect");
            ui.add_space(6.0);

            if !saved.profiles.is_empty() {
                egui::ComboBox::from_label("Saved")
                    .selected_text(self.form.display_name())
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for p in &saved.profiles {
                            let marker = if p.production { "🔴 " } else { "" };
                            if ui
                                .selectable_label(false, format!("{marker}{}", p.display_name()))
                                .clicked()
                            {
                                self.form = p.clone();
                                self.password.clear();
                            }
                        }
                    });
                ui.add_space(4.0);
            }

            let mut submit = false;
            egui::Grid::new("connect-form")
                .num_columns(2)
                .spacing([8.0, 6.0])
                .show(ui, |ui| {
                    ui.label("Username");
                    let user = ui.add(
                        egui::TextEdit::singleline(&mut self.form.username).desired_width(280.0),
                    );
                    if !self.focus_requested {
                        user.request_focus();
                        self.focus_requested = true;
                    }
                    ui.end_row();

                    ui.label("Password");
                    let pw = ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .desired_width(280.0),
                    );
                    submit |= pw.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.end_row();

                    ui.label("Database");
                    let db = ui.add(
                        egui::TextEdit::singleline(&mut self.form.connect_string)
                            .hint_text("TNS alias or host:1521/service")
                            .desired_width(280.0),
                    );
                    submit |= db.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.end_row();

                    ui.label("Connect as");
                    egui::ComboBox::from_id_salt("privilege")
                        .selected_text(self.form.privilege.label())
                        .show_ui(ui, |ui| {
                            for p in Privilege::ALL {
                                ui.selectable_value(&mut self.form.privilege, p, p.label());
                            }
                        });
                    ui.end_row();

                    ui.label("Name");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.name)
                            .hint_text("optional")
                            .desired_width(280.0),
                    );
                    ui.end_row();
                });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.checkbox(&mut self.form.production, "Production")
                    .on_hover_text("Red marker and confirmation before commit");
                ui.checkbox(&mut self.form.read_only, "Read-only")
                    .on_hover_text("Only queries are allowed on this connection");
                ui.checkbox(&mut self.save_profile, "Save connection");
            });

            if let Some(error) = &self.error {
                ui.add_space(6.0);
                ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
            }

            ui.add_space(8.0);
            ui.separator();
            ui.horizontal(|ui| {
                let can_connect = !self.connecting && !self.form.connect_string.trim().is_empty();
                if self.connecting {
                    ui.spinner();
                    ui.label("Connecting…");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        self.open = false;
                    }
                    if ui
                        .add_enabled(can_connect, egui::Button::new("Connect"))
                        .clicked()
                        || (submit && can_connect)
                    {
                        self.error = None;
                        self.connecting = true;
                        let mut profile = self.form.clone();
                        profile.username = profile.username.trim().to_owned();
                        profile.connect_string = profile.connect_string.trim().to_owned();
                        request = Some(ConnectRequest {
                            profile,
                            password: self.password.clone(),
                            save_profile: self.save_profile,
                        });
                    }
                });
            });
        });
        if modal.should_close() && !self.connecting {
            self.open = false;
        }
        request
    }

    /// Called once the connection is established.
    pub fn connected(&mut self) {
        self.open = false;
        self.connecting = false;
        self.password.clear();
    }

    pub fn failed(&mut self, error: String) {
        self.open = true;
        self.connecting = false;
        self.error = Some(error);
    }
}

#[derive(Default)]
pub struct CommandPalette {
    pub open: bool,
    query: String,
    selected: usize,
}

impl CommandPalette {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.query.clear();
        self.selected = 0;
    }

    pub fn ui(&mut self, ctx: &egui::Context, keymap: &Keymap) -> Option<Command> {
        if !self.open {
            return None;
        }
        let mut matches: Vec<(i32, Command)> = Command::ALL
            .iter()
            .filter(|c| **c != Command::ShowPalette)
            .filter_map(|c| fuzzy_score(&self.query, c.label()).map(|s| (s, *c)))
            .collect();
        matches.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        self.selected = self.selected.min(matches.len().saturating_sub(1));

        let (up, down, enter, escape) = ctx.input_mut(|i| {
            (
                i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
                i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                i.consume_key(egui::Modifiers::NONE, Key::Enter),
                i.consume_key(egui::Modifiers::NONE, Key::Escape),
            )
        });
        if up {
            self.selected = self.selected.saturating_sub(1);
        }
        if down && self.selected + 1 < matches.len() {
            self.selected += 1;
        }

        let mut chosen = None;
        egui::Area::new(egui::Id::new("command-palette"))
            .anchor(egui::Align2::CENTER_TOP, [0.0, 60.0])
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(480.0);
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("Type a command…")
                            .desired_width(f32::INFINITY),
                    );
                    edit.request_focus();
                    if edit.changed() {
                        self.selected = 0;
                    }
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(320.0)
                        .show(ui, |ui| {
                            for (i, (_, command)) in matches.iter().enumerate() {
                                let shortcut = keymap
                                    .shortcut(*command)
                                    .map(|s| ctx.format_shortcut(s))
                                    .unwrap_or_default();
                                let button =
                                    egui::Button::selectable(i == self.selected, command.label())
                                        .shortcut_text(shortcut)
                                        .min_size(egui::vec2(ui.available_width(), 0.0));
                                let response = ui.add(button);
                                if i == self.selected && (up || down) {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    chosen = Some(*command);
                                }
                            }
                            if matches.is_empty() {
                                ui.weak("No matching commands");
                            }
                        });
                });
            });

        if enter {
            chosen = matches.get(self.selected).map(|(_, c)| *c);
        }
        if chosen.is_some() || escape {
            self.open = false;
        }
        chosen
    }
}

/// An action that needs confirmation first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmAction {
    Commit,
    Disconnect,
    Quit,
}

pub struct Confirm {
    pub action: ConfirmAction,
    pub title: String,
    pub text: String,
    pub yes: &'static str,
}

/// Shows the confirmation; returns `Some(true)` on yes, `Some(false)` on no.
pub fn confirm_ui(ctx: &egui::Context, confirm: &Confirm) -> Option<bool> {
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("confirm")).show(ctx, |ui| {
        ui.set_max_width(380.0);
        ui.heading(&confirm.title);
        ui.add_space(6.0);
        ui.label(&confirm.text);
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button(confirm.yes).clicked() {
                answer = Some(true);
            }
            if ui.button("Cancel").clicked() {
                answer = Some(false);
            }
        });
    });
    if modal.should_close() && answer.is_none() {
        answer = Some(false);
    }
    answer
}
