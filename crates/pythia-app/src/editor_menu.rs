//! The editors' context menu: what can be done with the name under the
//! mouse (the object browser's actions for database objects) and editing
//! commands.

use egui::{Key, KeyboardShortcut, Modifiers, RichText};
use pythia_ui::icons::Icon;

use crate::assist::{Assist, ContextTarget, Request};
use crate::browser::{MenuOptions, object_menu};
use crate::commands::{Command, Keymap};
use crate::documents::{DocContext, Effects};
use crate::editor::Editor;

/// Shows the context menu of an editor on right-click; call every frame
/// after `Editor::ui` and `Assist::handle` with the editor's response.
pub fn show(
    response: &egui::Response,
    editor: &mut Editor,
    assist: &mut Assist,
    cx: &DocContext,
) -> Effects {
    let mut effects = Effects::default();
    let menu = response.context_menu(|ui| {
        ui.set_min_width(220.0);
        let ctx = ui.ctx().clone();
        let shortcut =
            |key: Key| ctx.format_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, key));
        match assist.context().map(|c| (c.offset, c.target.clone())) {
            Some((_, ContextTarget::Looking)) => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.weak("Looking up…");
                });
                ui.separator();
            }
            Some((_, ContextTarget::Object { object, member })) => {
                if let Some(connection) = &cx.connection {
                    let icon = Icon::for_object_type(object.object_type.dictionary_name())
                        .unwrap_or(Icon::Text);
                    let name = match &member {
                        Some(m) => format!("{}.{}", object.qualified(), m).to_lowercase(),
                        None => object.qualified().to_lowercase(),
                    };
                    ui.horizontal(|ui| {
                        ui.add(icon.sized(ui, pythia_ui::icons::IconSize::Small));
                        ui.label(RichText::new(name).monospace().weak());
                    });
                    let options = MenuOptions {
                        favorite: cx.marks.is_some_and(|m| m.is_favorite(connection, &object)),
                        drop: false,
                        reveal: true,
                        member,
                    };
                    if let Some(action) = object_menu(ui, &object, &options) {
                        effects.object_actions.push((connection.clone(), action));
                    }
                    ui.separator();
                }
            }
            Some((offset, ContextTarget::Local(_))) => {
                if ui.button("Go to declaration").clicked() {
                    assist.request(Request::Definition, offset);
                    ui.close();
                }
                if ui.button("Peek declaration").clicked() {
                    assist.request(Request::Peek, offset);
                    ui.close();
                }
                if ui.button("Find references").clicked() {
                    assist.request(Request::References, offset);
                    ui.close();
                }
                ui.separator();
            }
            Some((_, ContextTarget::Nothing)) | None => {}
        }

        let selected = editor.selection().is_some();
        let item = |ui: &mut egui::Ui, label: &str, shortcut: String, enabled: bool| {
            ui.add_enabled(enabled, egui::Button::new(label).shortcut_text(shortcut))
                .clicked()
        };
        let keymap = Keymap::with_overrides(cx.settings.keymap, &cx.settings.keybindings);
        let mut command = |ui: &mut egui::Ui, command: Command| {
            if item(
                ui,
                command.short_label(),
                keymap.shortcut_text(ui.ctx(), command),
                true,
            ) {
                effects.commands.push(command);
                ui.close();
            }
        };
        command(ui, Command::GoToSymbol);
        if assist.companion_is_spec().is_some() {
            command(ui, Command::SwitchSpecBody);
        }
        ui.separator();

        if item(ui, "Cut", shortcut(Key::X), selected) {
            if let Some(range) = editor.selection() {
                ui.ctx().copy_text(editor.text()[range].to_owned());
                editor.insert("");
            }
            ui.close();
        }
        if item(ui, "Copy", shortcut(Key::C), selected) {
            if let Some(range) = editor.selection() {
                ui.ctx().copy_text(editor.text()[range].to_owned());
            }
            ui.close();
        }
        if item(ui, "Paste", shortcut(Key::V), true) {
            // The clipboard arrives as a paste event in the editor.
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::RequestPaste);
            editor.inner().request_focus();
            ui.close();
        }
        if item(ui, "Select all", shortcut(Key::A), true) {
            editor.select_all();
            ui.close();
        }
        ui.separator();
        let comment = shortcut(Key::Slash).replace("Slash", "/");
        if item(ui, "Toggle comment", comment, true) {
            editor.toggle_comment(ui.input(|i| i.time));
            ui.close();
        }
        command(ui, Command::Format);
    });
    if menu.is_some() {
        // The word's hover would cover the menu.
        editor.inner().hide_hover();
    }
    effects
}
