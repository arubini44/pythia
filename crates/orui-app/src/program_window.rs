//! Program window: edit and compile stored PL/SQL (spec and body as tabs).

use std::collections::HashMap;

use egui::{RichText, Ui};
use orui_db::{CompileError, DbEvent, DbRequest, MetaQuery, MetaResult, ObjectType};
use serde::{Deserialize, Serialize};

use crate::documents::{DocContext, Effects, ObjectRef, Target, request_id};
use crate::editor::{Editor, Marker, MarkerKind};

#[derive(Serialize, Deserialize)]
pub struct ProgramWindow {
    pub object: ObjectRef,
    pub parts: Vec<Part>,
    pub active: usize,
    #[serde(default = "yes")]
    pub show_outline: bool,
    #[serde(skip)]
    state: State,
}

fn yes() -> bool {
    true
}

#[derive(Serialize, Deserialize)]
pub struct Part {
    pub object_type: ObjectType,
    pub editor: Editor,
    /// Text as last loaded or compiled, to detect unsaved changes.
    pub saved: String,
    #[serde(skip)]
    pub errors: Vec<CompileError>,
    #[serde(skip)]
    assist: crate::assist::Assist,
}

#[derive(Default)]
struct State {
    /// Source requests in flight, by part index.
    loading: HashMap<u64, usize>,
    /// Compile requests in flight: request id → part index.
    compiling: HashMap<u64, usize>,
    /// Parts still to compile after the current one (compile all).
    compile_queue: Vec<usize>,
    message: Option<(bool, String)>,
    /// Line to show once the part's source has loaded: (part, line).
    pending_goto: Option<(usize, usize)>,
}

impl ProgramWindow {
    pub fn new(object: ObjectRef) -> Self {
        let spec_type = match object.object_type {
            ObjectType::PackageBody => ObjectType::Package,
            ObjectType::TypeBody => ObjectType::Type,
            other => other,
        };
        let mut parts = vec![Part::new(spec_type)];
        if let Some(body) = spec_type.body() {
            parts.push(Part::new(body));
        }
        let active = parts
            .iter()
            .position(|p| p.object_type == object.object_type)
            .unwrap_or(0);
        Self {
            show_outline: true,
            object: ObjectRef {
                object_type: spec_type,
                ..object
            },
            parts,
            active,
            state: State::default(),
        }
    }

    /// Shows `line` of the part with `object_type` (now, or once loaded).
    pub fn goto_line(&mut self, object_type: ObjectType, line: usize) {
        let Some(i) = self.parts.iter().position(|p| p.object_type == object_type) else {
            return;
        };
        self.active = i;
        if self.state.loading.values().any(|p| *p == i) {
            self.state.pending_goto = Some((i, line));
        } else {
            self.parts[i].editor.goto(line, 1);
        }
    }

    pub fn title(&self) -> String {
        self.object.name.to_lowercase()
    }

    pub fn is_busy(&self) -> bool {
        !self.state.loading.is_empty() || !self.state.compiling.is_empty()
    }

    pub fn is_dirty(&self) -> bool {
        self.parts.iter().any(Part::is_dirty)
    }

    pub fn line_col(&self) -> (usize, usize) {
        self.parts
            .get(self.active)
            .map_or((1, 1), |p| p.editor.line_col())
    }

    /// Loads the source of all parts. Unsaved edits are kept.
    pub fn load(&mut self) -> Effects {
        let mut effects = Effects::default();
        if self.is_dirty() {
            return effects;
        }
        for (i, part) in self.parts.iter().enumerate() {
            let id = request_id();
            self.state.loading.insert(id, i);
            effects.request(
                Target::Main,
                DbRequest::Meta {
                    id,
                    query: MetaQuery::Source {
                        owner: self.object.owner.clone(),
                        name: self.object.name.clone(),
                        object_type: part.object_type,
                    },
                },
            );
        }
        effects
    }

    /// Compiles the active part, or all parts (spec first).
    pub fn compile(&mut self, all: bool) -> Effects {
        if self.is_busy() {
            return Effects::default();
        }
        self.state.compile_queue = if all {
            (0..self.parts.len()).rev().collect()
        } else {
            vec![self.active]
        };
        self.state.message = None;
        self.compile_next()
    }

    fn compile_next(&mut self) -> Effects {
        let mut effects = Effects::default();
        while let Some(i) = self.state.compile_queue.pop() {
            let sql = runnable(self.parts[i].editor.text());
            if sql.trim().is_empty() {
                continue;
            }
            let id = request_id();
            self.state.compiling.insert(id, i);
            effects.request(
                Target::Own,
                DbRequest::Execute {
                    id,
                    sql,
                    max_rows: 1,
                    binds: Vec::new(),
                },
            );
            break;
        }
        effects
    }

    pub fn on_event(&mut self, event: DbEvent) -> Effects {
        match event {
            DbEvent::Meta { id, result } => {
                if let Some(i) = self.state.loading.remove(&id) {
                    match result {
                        Ok(MetaResult::Text(text)) => {
                            let part = &mut self.parts[i];
                            part.editor.set_text(text.clone());
                            part.saved = text;
                            if let Some((p, line)) = self.state.pending_goto
                                && p == i
                            {
                                self.state.pending_goto = None;
                                part.editor.goto(line, 1);
                            }
                        }
                        // A package without a body yet: offer an empty one.
                        Err(_) if i > 0 => {}
                        Err(e) => self.state.message = Some((true, e.to_string())),
                        Ok(_) => {}
                    }
                }
            }
            DbEvent::Executed { id, summary, .. } => {
                if let Some(i) = self.state.compiling.remove(&id) {
                    let part = &mut self.parts[i];
                    part.saved = part.editor.text().to_owned();
                    part.errors = summary
                        .compiled
                        .as_ref()
                        .map(|c| c.errors.clone())
                        .unwrap_or_default();
                    part.editor.set_markers(markers(&part.errors));
                    let failed = part.errors.iter().any(|e| !e.warning);
                    self.state.message = Some((failed, summary.to_string()));
                    if failed {
                        self.active = i;
                        self.state.compile_queue.clear();
                    }
                    return self.compile_next();
                }
            }
            DbEvent::Failed { id, error, .. } => {
                if let Some(i) = self.state.compiling.remove(&id) {
                    self.active = i;
                    self.state.compile_queue.clear();
                    self.state.message = Some((true, error.to_string()));
                }
            }
            DbEvent::Error(error) | DbEvent::ConnectFailed(error) => {
                self.state.compiling.clear();
                self.state.message = Some((true, error.to_string()));
            }
            _ => {}
        }
        Effects::default()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        ui.horizontal(|ui| {
            for (i, part) in self.parts.iter().enumerate() {
                let label = match part.object_type {
                    ObjectType::PackageBody | ObjectType::TypeBody => "Body",
                    ObjectType::Package | ObjectType::Type => "Specification",
                    _ => "Source",
                };
                let mut text = RichText::new(if part.is_dirty() {
                    format!("{label} •")
                } else {
                    label.to_owned()
                });
                if part.errors.iter().any(|e| !e.warning) {
                    text = text.color(ui.visuals().error_fg_color);
                }
                if ui.selectable_label(self.active == i, text).clicked() {
                    self.active = i;
                }
            }
            ui.separator();
            let busy = self.is_busy();
            if ui
                .add_enabled(!busy && cx.connected, egui::Button::new("Compile"))
                .on_hover_text("F8")
                .clicked()
            {
                effects.extend(self.compile(false));
            }
            if self.parts.len() > 1
                && ui
                    .add_enabled(!busy && cx.connected, egui::Button::new("Compile all"))
                    .clicked()
            {
                effects.extend(self.compile(true));
            }
            if ui
                .add_enabled(!busy && cx.connected, egui::Button::new("Reload"))
                .on_hover_text("Discard changes and load the source from the database")
                .clicked()
            {
                for part in &mut self.parts {
                    part.saved = part.editor.text().to_owned();
                }
                effects.extend(self.load());
            }
            ui.toggle_value(&mut self.show_outline, "Outline");
            if busy {
                ui.spinner();
            }
            if let Some((error, text)) = &self.state.message {
                let color = if *error {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.label(RichText::new(text).color(color));
            }
        });

        let Some(part) = self.parts.get_mut(self.active) else {
            return effects;
        };
        if !part.errors.is_empty() {
            egui::Panel::bottom(egui::Id::new(("program-errors", cx.id)))
                .resizable(true)
                .default_size(110.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            let mut goto = None;
                            for e in &part.errors {
                                let color = if e.warning {
                                    ui.visuals().warn_fg_color
                                } else {
                                    ui.visuals().error_fg_color
                                };
                                let text = RichText::new(format!(
                                    "{:>5}:{:<4} {}",
                                    e.line, e.position, e.text
                                ))
                                .monospace()
                                .color(color);
                                if ui.link(text).clicked() {
                                    goto = Some((e.line as usize, e.position as usize));
                                }
                            }
                            if let Some((line, col)) = goto {
                                part.editor.goto(line, col);
                            }
                        });
                });
        }
        if self.show_outline {
            egui::Panel::right(egui::Id::new(("program-outline", cx.id)))
                .resizable(true)
                .default_size(240.0)
                .show(ui, |ui| {
                    let mut jump = None;
                    egui::ScrollArea::vertical()
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            if part.assist.outline().is_empty() {
                                ui.weak("No declarations");
                            }
                            for item in part.assist.outline() {
                                outline_item(ui, item, 0, &mut jump);
                            }
                        });
                    if let Some(range) = jump {
                        part.editor.select(range);
                    }
                });
        }
        egui::CentralPanel::default().show(ui, |ui| {
            let out = part.editor.ui(
                ui,
                ("program-editor", cx.id, self.active),
                &cx.settings.editor,
            );
            effects.extend(part.assist.handle(&mut part.editor, &out, cx));
            part.assist.background(&mut part.editor, ui);
        });
        effects
    }
}

impl Part {
    fn new(object_type: ObjectType) -> Self {
        Self {
            object_type,
            editor: Editor::default(),
            saved: String::new(),
            errors: Vec::new(),
            assist: Default::default(),
        }
    }

    fn is_dirty(&self) -> bool {
        self.editor.text() != self.saved
    }
}

/// One outline entry with its children, indented by depth.
fn outline_item(
    ui: &mut Ui,
    item: &orui_lsp_core::OutlineItem,
    depth: usize,
    jump: &mut Option<std::ops::Range<usize>>,
) {
    use orui_lsp_core::OutlineKind as K;
    use orui_ui::icons::{Icon, IconSize};
    let icon = match item.kind {
        K::PackageSpec => Icon::Package,
        K::PackageBody => Icon::PackageBody,
        K::Procedure => Icon::Procedure,
        K::Function => Icon::Function,
        K::Trigger => Icon::Trigger,
        K::Type | K::TypeDecl => Icon::Type,
        K::TypeBody => Icon::TypeBody,
        K::Cursor => Icon::Grid,
        K::Exception => Icon::Warning,
        K::Variable | K::Constant => Icon::Column,
        K::Block | K::Statement => Icon::Text,
    };
    // Variables of nested scopes would only add noise.
    if depth > 1
        && matches!(
            item.kind,
            K::Variable | K::Constant | K::Statement | K::Block
        )
    {
        return;
    }
    ui.horizontal(|ui| {
        ui.add_space(depth as f32 * 14.0);
        ui.spacing_mut().item_spacing.x = 5.0;
        ui.add(icon.sized(ui, IconSize::Small));
        let r = ui.add(
            egui::Button::new(RichText::new(item.name.to_lowercase()).monospace()).frame(false),
        );
        if r.clicked() {
            *jump = Some(item.name_span.clone());
        }
        if !item.detail.is_empty() {
            r.on_hover_text(&item.detail);
        }
    });
    for child in &item.children {
        outline_item(ui, child, depth + 1, jump);
    }
}

/// The part's text as a single statement: drops a trailing `/` line.
fn runnable(text: &str) -> String {
    let trimmed = text.trim_end();
    let trimmed = trimmed
        .strip_suffix('/')
        .filter(|t| t.ends_with('\n'))
        .unwrap_or(trimmed);
    trimmed.trim_end().to_owned()
}

fn markers(errors: &[CompileError]) -> Vec<Marker> {
    errors
        .iter()
        .map(|e| Marker {
            line: e.line as usize,
            kind: if e.warning {
                MarkerKind::Warning
            } else {
                MarkerKind::Error
            },
            message: e.text.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_slash() {
        assert_eq!(
            runnable("create package p as end;\n/\n"),
            "create package p as end;"
        );
        assert_eq!(runnable("x := 4/\n"), "x := 4/");
    }

    #[test]
    fn spec_and_body_parts() {
        let w = ProgramWindow::new(ObjectRef {
            owner: "HR".into(),
            name: "PKG".into(),
            object_type: ObjectType::PackageBody,
        });
        assert_eq!(w.parts.len(), 2);
        assert_eq!(w.active, 1);
        assert_eq!(w.object.object_type, ObjectType::Package);
    }
}
