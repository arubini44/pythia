//! Modal dialogs and popups: connect, command palette, object finder,
//! statement history, confirmations.

use std::time::Duration;

use egui::{Key, RichText};
use orui_config::{ConnectionProfile, Connections as SavedConnections, Privilege};
use orui_db::{DbObject, ObjectType};

use crate::commands::{Command, Keymap, fuzzy_score};
use crate::documents::ObjectRef;
use crate::history::History;
use crate::secrets;

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
            self.pick(last.clone());
        }
    }

    fn pick(&mut self, profile: ConnectionProfile) {
        self.password = if profile.save_password {
            secrets::load(&profile.display_name()).unwrap_or_default()
        } else {
            String::new()
        };
        self.form = profile;
    }

    pub fn ui(&mut self, ctx: &egui::Context, saved: &SavedConnections) -> Option<ConnectRequest> {
        if !self.open {
            return None;
        }
        let mut request = None;
        let modal = egui::Modal::new(egui::Id::new("connect-dialog")).show(ctx, |ui| {
            ui.set_width(440.0);
            ui.heading("Connect to Oracle");
            ui.add_space(8.0);

            if !saved.profiles.is_empty() {
                let mut picked = None;
                egui::ComboBox::from_label("Saved connection")
                    .selected_text(self.form.display_name())
                    .width(300.0)
                    .show_ui(ui, |ui| {
                        for p in &saved.profiles {
                            let marker = if p.production { "● " } else { "" };
                            let text = RichText::new(format!("{marker}{}", p.display_name()));
                            let text = if p.production {
                                text.color(ui.visuals().error_fg_color)
                            } else {
                                text
                            };
                            if ui.selectable_label(false, text).clicked() {
                                picked = Some(p.clone());
                            }
                        }
                    });
                if let Some(p) = picked {
                    self.pick(p);
                }
                ui.add_space(6.0);
            }

            let mut submit = false;
            egui::Grid::new("connect-form")
                .num_columns(2)
                .spacing([10.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Username");
                    let user = ui.add(
                        egui::TextEdit::singleline(&mut self.form.username).desired_width(290.0),
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
                            .desired_width(290.0),
                    );
                    submit |= pw.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    ui.end_row();

                    ui.label("Database");
                    let db = ui.add(
                        egui::TextEdit::singleline(&mut self.form.connect_string)
                            .hint_text("TNS alias or host:1521/service")
                            .desired_width(290.0),
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
                            .desired_width(290.0),
                    );
                    ui.end_row();

                    ui.label("Folder");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.folder)
                            .hint_text("e.g. customer/prod")
                            .desired_width(290.0),
                    );
                    ui.end_row();
                });

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.checkbox(&mut self.form.production, "Production")
                    .on_hover_text("Red marker and confirmation before commit");
                ui.checkbox(&mut self.form.read_only, "Read-only")
                    .on_hover_text("Only queries are allowed");
                ui.checkbox(&mut self.save_profile, "Save connection");
                ui.add_enabled(
                    self.save_profile,
                    egui::Checkbox::new(&mut self.form.save_password, "Save password"),
                )
                .on_hover_text("Stored in the operating system's keychain");
            });

            if let Some(error) = &self.error {
                ui.add_space(8.0);
                egui::Frame::new()
                    .fill(ui.visuals().error_fg_color.gamma_multiply(0.12))
                    .corner_radius(6.0)
                    .inner_margin(8.0)
                    .show(ui, |ui| {
                        ui.label(RichText::new(error).color(ui.visuals().error_fg_color));
                    });
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                let can_connect = !self.connecting && !self.form.connect_string.trim().is_empty();
                if self.connecting {
                    ui.spinner();
                    ui.label("Connecting…");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Cancel").clicked() {
                        self.open = false;
                        self.connecting = false;
                    }
                    let connect = ui.add_enabled(
                        can_connect,
                        egui::Button::new(RichText::new("Connect").strong()),
                    );
                    if connect.clicked() || (submit && can_connect) {
                        self.error = None;
                        self.connecting = true;
                        let mut profile = self.form.clone();
                        profile.username = profile.username.trim().to_owned();
                        profile.connect_string = profile.connect_string.trim().to_owned();
                        profile.save_password &= self.save_profile;
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
        let labels: Vec<(String, String)> = matches
            .iter()
            .map(|(_, c)| (c.label().to_owned(), keymap.shortcut_text(ctx, *c)))
            .collect();
        let picked = popup_list(
            ctx,
            "command-palette",
            "Type a command…",
            &mut self.query,
            &mut self.selected,
            &labels,
        );
        match picked {
            PopupResult::Picked(i) => {
                self.open = false;
                matches.get(i).map(|(_, c)| *c)
            }
            PopupResult::Closed => {
                self.open = false;
                None
            }
            PopupResult::Open => None,
        }
    }
}

enum PopupResult {
    Open,
    Picked(usize),
    Closed,
}

/// A palette-style popup: search field plus a keyboard navigable list of
/// (label, detail) rows.
fn popup_list(
    ctx: &egui::Context,
    id: &str,
    hint: &str,
    query: &mut String,
    selected: &mut usize,
    rows: &[(String, String)],
) -> PopupResult {
    *selected = (*selected).min(rows.len().saturating_sub(1));
    let (up, down, enter, escape) = ctx.input_mut(|i| {
        (
            i.consume_key(egui::Modifiers::NONE, Key::ArrowUp),
            i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
            i.consume_key(egui::Modifiers::NONE, Key::Enter),
            i.consume_key(egui::Modifiers::NONE, Key::Escape),
        )
    });
    if up {
        *selected = selected.saturating_sub(1);
    }
    if down && *selected + 1 < rows.len() {
        *selected += 1;
    }
    let mut result = PopupResult::Open;
    let area = egui::Area::new(egui::Id::new(id))
        .anchor(egui::Align2::CENTER_TOP, [0.0, 72.0])
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style())
                .inner_margin(8.0)
                .show(ui, |ui| {
                    ui.set_width(560.0);
                    let edit = ui.add(
                        egui::TextEdit::singleline(query)
                            .hint_text(hint)
                            .desired_width(f32::INFINITY)
                            .margin(6.0),
                    );
                    edit.request_focus();
                    if edit.changed() {
                        *selected = 0;
                    }
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical()
                        .max_height(360.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (i, (label, detail)) in rows.iter().enumerate() {
                                let button =
                                    egui::Button::selectable(i == *selected, label.as_str())
                                        .right_text(RichText::new(detail.as_str()).weak())
                                        .min_size(egui::vec2(ui.available_width(), 24.0));
                                let response = ui.add(button);
                                if i == *selected && (up || down) {
                                    response.scroll_to_me(None);
                                }
                                if response.clicked() {
                                    result = PopupResult::Picked(i);
                                }
                            }
                            if rows.is_empty() {
                                ui.weak("No matches");
                            }
                        });
                });
        });
    let clicked_outside = ctx.input(|i| i.pointer.any_click())
        && !ctx
            .pointer_interact_pos()
            .is_some_and(|p| area.response.rect.contains(p));
    if enter && !rows.is_empty() {
        result = PopupResult::Picked(*selected);
    } else if escape || clicked_outside {
        result = PopupResult::Closed;
    }
    result
}

/// Quick object finder across all visible schemas.
#[derive(Default)]
pub struct ObjectFinder {
    pub open: bool,
    query: String,
    selected: usize,
    /// Query text the current results / pending request belong to.
    searched: String,
    pub pending: Option<u64>,
    results: Vec<DbObject>,
    changed_at: Option<f64>,
}

pub enum FinderAction {
    /// Search for this pattern now.
    Search(String),
    Open(ObjectRef),
}

impl ObjectFinder {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.selected = 0;
    }

    pub fn results(&mut self, id: u64, objects: Vec<DbObject>) {
        if self.pending == Some(id) {
            self.pending = None;
            self.results = objects;
            self.selected = 0;
        }
    }

    pub fn ui(&mut self, ctx: &egui::Context) -> Option<FinderAction> {
        if !self.open {
            return None;
        }
        let rows: Vec<(String, String)> = self
            .results
            .iter()
            .map(|o| {
                let invalid = if o.valid { "" } else { "  (invalid)" };
                (
                    o.name.clone(),
                    format!(
                        "{} · {}{invalid}",
                        o.object_type.dictionary_name().to_lowercase(),
                        o.owner
                    ),
                )
            })
            .collect();
        let before = self.query.clone();
        let result = popup_list(
            ctx,
            "object-finder",
            "Find table, view, package…",
            &mut self.query,
            &mut self.selected,
            &rows,
        );
        let now = ctx.input(|i| i.time);
        if self.query != before {
            self.changed_at = Some(now);
        }
        let mut action = None;
        // Debounce typing: search 250 ms after the last keystroke.
        if let Some(t) = self.changed_at {
            if now - t > 0.25 {
                self.changed_at = None;
                if self.query.trim().len() >= 2 && self.query != self.searched {
                    self.searched = self.query.clone();
                    action = Some(FinderAction::Search(self.query.trim().to_owned()));
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(260));
            }
        }
        match result {
            PopupResult::Picked(i) => {
                self.open = false;
                self.results.get(i).map(|o| {
                    FinderAction::Open(ObjectRef {
                        owner: o.owner.clone(),
                        name: o.name.clone(),
                        object_type: o.object_type,
                    })
                })
            }
            PopupResult::Closed => {
                self.open = false;
                None
            }
            PopupResult::Open => action,
        }
    }
}

/// Recall a previously executed statement.
#[derive(Default)]
pub struct HistoryDialog {
    pub open: bool,
    query: String,
    selected: usize,
    pub all_connections: bool,
}

impl HistoryDialog {
    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.selected = 0;
    }

    /// Returns the picked statement.
    pub fn ui(
        &mut self,
        ctx: &egui::Context,
        history: &History,
        connection: Option<&str>,
    ) -> Option<String> {
        if !self.open {
            return None;
        }
        let filter = if self.all_connections {
            None
        } else {
            connection
        };
        let query = self.query.clone();
        let entries: Vec<_> = history.search(filter, &query).take(300).collect();
        let rows: Vec<(String, String)> = entries
            .iter()
            .map(|e| {
                let one_line: String = e.sql.split_whitespace().collect::<Vec<_>>().join(" ");
                let label = if one_line.chars().count() > 90 {
                    format!("{}…", one_line.chars().take(90).collect::<String>())
                } else {
                    one_line
                };
                let times = if e.count > 1 {
                    format!("{}× · ", e.count)
                } else {
                    String::new()
                };
                (label, format!("{times}{}", ago(e.at)))
            })
            .collect();
        let picked = popup_list(
            ctx,
            "history",
            "Search executed statements…",
            &mut self.query,
            &mut self.selected,
            &rows,
        );
        match picked {
            PopupResult::Picked(i) => {
                self.open = false;
                entries.get(i).map(|e| e.sql.clone())
            }
            PopupResult::Closed => {
                self.open = false;
                None
            }
            PopupResult::Open => None,
        }
    }
}

fn ago(at: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let secs = now.saturating_sub(at);
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{} min ago", secs / 60),
        3600..86400 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86400),
    }
}

/// An action that needs confirmation first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmAction {
    Commit,
    Disconnect(String),
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
        ui.set_max_width(400.0);
        ui.heading(&confirm.title);
        ui.add_space(6.0);
        ui.label(&confirm.text);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.button(RichText::new(confirm.yes).strong()).clicked() {
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

/// Object types a finder result opens as program windows.
pub fn opens_as_program(t: ObjectType) -> bool {
    t.has_source()
}
