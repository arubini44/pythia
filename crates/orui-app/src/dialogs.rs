//! Modal dialogs and popups: connect, command palette, object finder,
//! statement history, confirmations.

use std::time::Duration;

use egui::{Key, RichText, Sense, Ui};
use orui_config::{ConnectionProfile, Connections as SavedConnections, Privilege};
use orui_db::{DbObject, ObjectType};
use orui_ui::icons::Icon;

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
    /// Display name of the saved profile being edited, if any.
    editing: Option<String>,
    search: String,
    confirm_delete: bool,
    focus_requested: bool,
}

pub struct ConnectRequest {
    pub profile: ConnectionProfile,
    pub password: String,
    pub save_profile: bool,
}

pub enum ConnectAction {
    Connect(ConnectRequest),
    /// Save the profile, replacing the one named `previous` (if renamed).
    Save {
        profile: ConnectionProfile,
        previous: Option<String>,
        password: Option<String>,
    },
    Delete(String),
}

/// Accent colors offered for connections.
const COLORS: [[u8; 3]; 8] = [
    [0x3b, 0x82, 0xf6],
    [0x14, 0xb8, 0xa6],
    [0x22, 0xc5, 0x5e],
    [0xea, 0xb3, 0x08],
    [0xf9, 0x73, 0x16],
    [0xa8, 0x55, 0xf7],
    [0xec, 0x48, 0x99],
    [0x64, 0x74, 0x8b],
];

impl ConnectDialog {
    pub fn show(&mut self, last: Option<&ConnectionProfile>) {
        self.open = true;
        self.error = None;
        self.focus_requested = false;
        self.confirm_delete = false;
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
        self.editing = Some(profile.display_name());
        self.form = profile;
        self.confirm_delete = false;
        self.focus_requested = false;
    }

    fn new_profile(&mut self) {
        self.form = ConnectionProfile::default();
        self.password.clear();
        self.editing = None;
        self.confirm_delete = false;
        self.focus_requested = false;
    }

    pub fn ui(&mut self, ctx: &egui::Context, saved: &SavedConnections) -> Option<ConnectAction> {
        if !self.open {
            return None;
        }
        let mut action = None;
        let modal = egui::Modal::new(egui::Id::new("connect-dialog")).show(ctx, |ui| {
            ui.set_width(if saved.profiles.is_empty() {
                460.0
            } else {
                720.0
            });
            ui.horizontal_top(|ui| {
                if !saved.profiles.is_empty() {
                    ui.vertical(|ui| {
                        ui.set_width(240.0);
                        self.list_ui(ui, saved, &mut action);
                    });
                    ui.separator();
                }
                ui.vertical(|ui| self.form_ui(ui, &mut action));
            });
        });
        if modal.should_close() && !self.connecting {
            self.open = false;
        }
        action
    }

    fn list_ui(
        &mut self,
        ui: &mut Ui,
        saved: &SavedConnections,
        action: &mut Option<ConnectAction>,
    ) {
        ui.horizontal(|ui| {
            ui.strong("Connections");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::icons_ext::small_icon_button(ui, Icon::Plus, "New connection").clicked() {
                    self.new_profile();
                }
            });
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search…")
                .desired_width(f32::INFINITY),
        );
        ui.add_space(4.0);
        let needle = self.search.to_lowercase();
        let mut folders: Vec<&str> = saved.profiles.iter().map(|p| p.folder.as_str()).collect();
        folders.sort_unstable();
        folders.dedup();
        let mut picked = None;
        let mut connect = None;
        egui::ScrollArea::vertical()
            .max_height(360.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for folder in folders {
                    let profiles: Vec<&ConnectionProfile> = saved
                        .profiles
                        .iter()
                        .filter(|p| p.folder == folder)
                        .filter(|p| {
                            needle.is_empty()
                                || p.display_name().to_lowercase().contains(&needle)
                                || p.connect_string.to_lowercase().contains(&needle)
                        })
                        .collect();
                    if profiles.is_empty() {
                        continue;
                    }
                    if !folder.is_empty() {
                        ui.horizontal(|ui| {
                            ui.add(Icon::Folder.sized(ui, orui_ui::icons::IconSize::Small));
                            ui.label(RichText::new(folder).small().strong());
                        });
                    }
                    for p in profiles {
                        let name = p.display_name();
                        let selected = self.editing.as_deref() == Some(name.as_str());
                        let r = ui.horizontal(|ui| {
                            if !folder.is_empty() {
                                ui.add_space(10.0);
                            }
                            let color = if p.production {
                                ui.visuals().error_fg_color
                            } else {
                                p.color.map_or(ui.visuals().weak_text_color(), |[r, g, b]| {
                                    egui::Color32::from_rgb(r, g, b)
                                })
                            };
                            let (dot, _) =
                                ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
                            ui.painter().circle_filled(dot.center(), 4.0, color);
                            let mut text = RichText::new(&name);
                            if p.production {
                                text = text.color(ui.visuals().error_fg_color);
                            }
                            let r = ui.add(
                                egui::Button::selectable(selected, text).frame_when_inactive(false),
                            );
                            r.on_hover_text(format!(
                                "{}@{}{}",
                                p.username,
                                p.connect_string,
                                if p.production { "\nProduction" } else { "" }
                            ))
                        });
                        let r = r.inner;
                        if r.clicked() {
                            picked = Some(p.clone());
                        }
                        if r.double_clicked() {
                            connect = Some(p.clone());
                        }
                    }
                }
            });
        if let Some(p) = picked {
            self.pick(p);
        }
        if let Some(p) = connect {
            self.pick(p);
            *action = self.connect_request();
        }
    }

    fn form_ui(&mut self, ui: &mut Ui, action: &mut Option<ConnectAction>) {
        ui.heading(if self.editing.is_some() {
            "Connection"
        } else {
            "New connection"
        });
        ui.add_space(6.0);
        let mut submit = false;
        egui::Grid::new("connect-form")
            .num_columns(2)
            .spacing([10.0, 8.0])
            .show(ui, |ui| {
                ui.label("Username");
                let user = ui
                    .add(egui::TextEdit::singleline(&mut self.form.username).desired_width(280.0));
                if !self.focus_requested {
                    if self.form.username.is_empty() {
                        user.request_focus();
                    }
                    self.focus_requested = true;
                }
                ui.end_row();

                ui.label("Password");
                let pw = ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(280.0),
                );
                if self.focus_requested
                    && !self.form.username.is_empty()
                    && self.password.is_empty()
                    && !pw.has_focus()
                    && ui.memory(|m| m.focused().is_none())
                {
                    pw.request_focus();
                }
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
                        .hint_text("optional, e.g. HR dev")
                        .desired_width(280.0),
                );
                ui.end_row();

                ui.label("Folder");
                ui.add(
                    egui::TextEdit::singleline(&mut self.form.folder)
                        .hint_text("optional, e.g. customer/prod")
                        .desired_width(280.0),
                );
                ui.end_row();

                ui.label("Color");
                ui.horizontal(|ui| {
                    for c in COLORS {
                        let color = egui::Color32::from_rgb(c[0], c[1], c[2]);
                        let (rect, r) =
                            ui.allocate_exact_size(egui::vec2(18.0, 18.0), Sense::click());
                        let selected = self.form.color == Some(c);
                        ui.painter().circle_filled(
                            rect.center(),
                            if selected { 8.0 } else { 6.5 },
                            color,
                        );
                        if selected {
                            ui.painter().circle_stroke(
                                rect.center(),
                                9.0,
                                egui::Stroke::new(1.5, ui.visuals().text_color()),
                            );
                        }
                        if r.clicked() {
                            self.form.color = if selected { None } else { Some(c) };
                        }
                    }
                });
                ui.end_row();
            });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(&mut self.form.production, "Production")
                .on_hover_text("Red marker everywhere and confirmation before commit");
            ui.checkbox(&mut self.form.read_only, "Read-only")
                .on_hover_text("Only queries are allowed");
            ui.checkbox(&mut self.form.save_password, "Save password")
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
            let valid = !self.form.connect_string.trim().is_empty();
            if self.connecting {
                ui.spinner();
                ui.label("Connecting…");
            }
            if let Some(name) = self.editing.clone() {
                if self.confirm_delete {
                    ui.label("Delete this connection?");
                    if ui
                        .button(RichText::new("Delete").color(ui.visuals().error_fg_color))
                        .clicked()
                    {
                        *action = Some(ConnectAction::Delete(name));
                        self.new_profile();
                    }
                    if ui.button("Keep").clicked() {
                        self.confirm_delete = false;
                    }
                } else if crate::icons_ext::small_icon_button(ui, Icon::Trash, "Delete connection")
                    .clicked()
                {
                    self.confirm_delete = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let connect = ui.add_enabled(
                    valid && !self.connecting,
                    egui::Button::new(RichText::new("Connect").strong()),
                );
                if connect.clicked() || (submit && valid && !self.connecting) {
                    *action = self.connect_request();
                }
                if ui.add_enabled(valid, egui::Button::new("Save")).clicked() {
                    let profile = self.cleaned();
                    let password = profile.save_password.then(|| self.password.clone());
                    *action = Some(ConnectAction::Save {
                        previous: self.editing.clone(),
                        profile: profile.clone(),
                        password,
                    });
                    self.editing = Some(profile.display_name());
                }
                if ui.button("Cancel").clicked() {
                    self.open = false;
                    self.connecting = false;
                }
            });
        });
    }

    fn cleaned(&self) -> ConnectionProfile {
        let mut profile = self.form.clone();
        profile.username = profile.username.trim().to_owned();
        profile.connect_string = profile.connect_string.trim().to_owned();
        profile.name = profile.name.trim().to_owned();
        profile.folder = profile.folder.trim().trim_matches('/').to_owned();
        profile
    }

    fn connect_request(&mut self) -> Option<ConnectAction> {
        if self.form.connect_string.trim().is_empty() {
            return None;
        }
        self.error = None;
        self.connecting = true;
        Some(ConnectAction::Connect(ConnectRequest {
            profile: self.cleaned(),
            password: self.password.clone(),
            save_profile: true,
        }))
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
