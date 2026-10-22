//! Program window: edit and compile stored PL/SQL (spec and body as tabs).

use std::collections::HashMap;

use egui::{RichText, Ui};
use pythia_db::{CompileError, DbEvent, DbRequest, MetaQuery, MetaResult, ObjectType};
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
    /// Subprogram to show once all parts have loaded.
    pending_member: Option<String>,
    /// LAST_DDL_TIME of each part when loaded or last compiled here.
    ddl_times: HashMap<usize, Option<String>>,
    /// Requests reading LAST_DDL_TIME: id → (part, purpose).
    ddl_requests: HashMap<u64, (usize, DdlCheck)>,
    /// The part was changed in the database by someone else: (part, time).
    conflict: Option<(usize, String)>,
    /// Concurrent-change checks still running before a compile.
    checks_pending: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DdlCheck {
    /// Remember the time (after load or compile).
    Remember,
    /// Compare before compiling.
    BeforeCompile,
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

    /// Shows a subprogram: its implementation in the body, else its
    /// declaration in the spec (now, or once the source has loaded).
    pub fn goto_member(&mut self, member: &str) {
        self.state.pending_member = Some(member.to_owned());
        if self.state.loading.is_empty() {
            self.show_member();
        }
    }

    fn show_member(&mut self) {
        let Some(member) = self.state.pending_member.take() else {
            return;
        };
        // Bodies come after specs; look there first.
        for i in (0..self.parts.len()).rev() {
            let part = &mut self.parts[i];
            if let Some(span) = member_span(part.editor.text(), &member) {
                self.active = i;
                part.editor.select(span);
                return;
            }
        }
        self.state.message = Some((false, format!("{} not found", member.to_lowercase())));
    }

    pub fn active_editor(&mut self) -> Option<&mut Editor> {
        self.parts.get_mut(self.active).map(|p| &mut p.editor)
    }

    /// Toggles a breakpoint on the cursor line of the active part.
    pub fn toggle_breakpoint(&self) -> Effects {
        let mut effects = Effects::default();
        if let Some(part) = self.parts.get(self.active) {
            let line = part.editor.line_col().0 as u32;
            effects
                .breakpoints
                .push((debug_unit(&self.object, part.object_type), line));
        }
        effects
    }

    /// File name for saving the active part, e.g. `emp_pkg.pkb`.
    pub fn file_name(&self) -> String {
        let ext = match self.parts.get(self.active).map(|p| p.object_type) {
            Some(ObjectType::Package) => "pks",
            Some(ObjectType::PackageBody) => "pkb",
            Some(ObjectType::Procedure) => "prc",
            Some(ObjectType::Function) => "fnc",
            Some(ObjectType::Type) => "tps",
            Some(ObjectType::TypeBody) => "tpb",
            Some(ObjectType::Trigger) => "trg",
            _ => "sql",
        };
        format!("{}.{ext}", self.object.name.to_lowercase())
    }

    /// Source of the active part, terminated for SQL*Plus.
    pub fn active_source(&self) -> String {
        let text = self
            .parts
            .get(self.active)
            .map_or("", |p| p.editor.text())
            .trim_end();
        format!("{text}\n/\n")
    }

    pub fn title(&self) -> String {
        self.object.name.to_lowercase()
    }

    pub fn is_busy(&self) -> bool {
        !self.state.loading.is_empty()
            || !self.state.compiling.is_empty()
            || self.state.checks_pending > 0
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
        self.state.conflict = None;
        for i in 0..self.parts.len() {
            effects.extend(self.ddl_time(i, DdlCheck::Remember));
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
        self.state.conflict = None;
        // Check every part for changes by others before compiling anything
        // (compiling the spec touches the body's DDL time).
        let known: Vec<usize> = self
            .state
            .compile_queue
            .iter()
            .copied()
            .filter(|i| self.state.ddl_times.get(i).is_some_and(Option::is_some))
            .collect();
        if known.is_empty() {
            return self.compile_part();
        }
        let mut effects = Effects::default();
        self.state.checks_pending = known.len();
        for part in known {
            effects.extend(self.ddl_time(part, DdlCheck::BeforeCompile));
        }
        effects
    }

    #[cfg(test)]
    pub fn has_conflict(&self) -> bool {
        self.state.conflict.is_some()
    }

    #[cfg(test)]
    pub fn message(&self) -> Option<(bool, String)> {
        self.state.message.clone()
    }

    /// Compiles despite a detected concurrent change.
    pub fn overwrite(&mut self) -> Effects {
        self.state.conflict = None;
        self.compile_part()
    }

    /// Reads the part's LAST_DDL_TIME from the database.
    fn ddl_time(&mut self, part: usize, check: DdlCheck) -> Effects {
        let mut effects = Effects::default();
        let (owner, name) = (self.object.owner.clone(), self.object.name.clone());
        let object_type = self.parts[part].object_type.dictionary_name();
        let id = request_id();
        self.state.ddl_requests.insert(id, (part, check));
        let task: pythia_db::TaskFn = Box::new(move |conn: &pythia_db::oracle::Connection| {
            let time = conn
                .query_row_as::<Option<String>>(
                    "select max(to_char(last_ddl_time, 'YYYY-MM-DD HH24:MI:SS')) from all_objects \
                      where owner = :1 and object_name = :2 and object_type = :3",
                    &[&owner, &name, &object_type],
                )
                .map_err(pythia_db::DbError::from)?;
            Ok(Box::new(time) as Box<dyn std::any::Any + Send>)
        });
        effects.request(Target::Main, DbRequest::Task { id, task });
        effects
    }

    /// Compiles the next queued part without checking.
    fn compile_part(&mut self) -> Effects {
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
        if let DbEvent::TaskDone { id, result } = &event
            && let Some((part, check)) = self.state.ddl_requests.remove(id)
        {
            let now: Option<String> = match result {
                Ok(any) => any.downcast_ref::<Option<String>>().cloned().flatten(),
                Err(_) => None,
            };
            match check {
                DdlCheck::Remember => {
                    self.state.ddl_times.insert(part, now);
                }
                DdlCheck::BeforeCompile => {
                    self.state.checks_pending = self.state.checks_pending.saturating_sub(1);
                    let known = self.state.ddl_times.get(&part).cloned().flatten();
                    if let (Some(known), Some(now)) = (known, now)
                        && now > known
                        && self.state.conflict.is_none()
                    {
                        self.active = part;
                        self.state.conflict = Some((part, now));
                    }
                    if self.state.checks_pending == 0 && self.state.conflict.is_none() {
                        return self.compile_part();
                    }
                }
            }
            return Effects::default();
        }
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
                    if self.state.loading.is_empty() {
                        self.show_member();
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
                    let mut effects = self.ddl_time(i, DdlCheck::Remember);
                    effects.extend(self.compile_part());
                    return effects;
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
            if self.parts.len() == 2
                && self.parts[0].object_type == ObjectType::Package
                && ui
                    .button("Generate body")
                    .on_hover_text("Create the body from the specification, or add stubs for missing subprograms")
                    .clicked()
            {
                let spec = self.parts[0].editor.text().to_owned();
                let body = self.parts[1].editor.text().to_owned();
                self.state.message = Some(match crate::stubs::package_body(&spec, Some(&body)) {
                    Some(stubs) if stubs.added > 0 => {
                        let len = body.len();
                        self.parts[1].editor.replace(0..len, &stubs.body, 1);
                        self.active = 1;
                        (false, format!("Added {} stub(s) to the body — compile to save", stubs.added))
                    }
                    Some(_) => (false, "The body already implements everything in the spec".into()),
                    None => (true, "Couldn't read the package specification".into()),
                });
            }
            if self.parts.len() == 2
                && self.parts[0].object_type == ObjectType::Package
                && ui
                    .button("Update spec")
                    .on_hover_text("Copy changed subprogram signatures from the body into the specification")
                    .clicked()
            {
                let spec = self.parts[0].editor.text().to_owned();
                let body = self.parts[1].editor.text().to_owned();
                self.state.message = Some(match crate::stubs::sync_spec(&spec, &body) {
                    Some(sync) if !sync.updated.is_empty() => {
                        let len = spec.len();
                        self.parts[0].editor.replace(0..len, &sync.spec, 1);
                        self.active = 0;
                        (
                            false,
                            format!(
                                "Updated {} in the spec — compile to save",
                                sync.updated.join(", ")
                            ),
                        )
                    }
                    Some(_) => (false, "The spec matches the body".into()),
                    None => (true, "Couldn't read the package specification".into()),
                });
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

        if let Some((conflicted, time)) = self.state.conflict.clone() {
            let what = match self.parts[conflicted].object_type {
                ObjectType::PackageBody | ObjectType::TypeBody => "body",
                ObjectType::Package | ObjectType::Type => "specification",
                _ => "source",
            };
            let warn = ui.visuals().warn_fg_color;
            egui::Frame::new().fill(warn.gamma_multiply(0.15)).corner_radius(6.0).inner_margin(8.0).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(pythia_ui::icons::Icon::Warning.image(ui));
                    ui.label(
                        RichText::new(format!(
                            "The {what} was changed in the database at {time}, after you opened it. Compiling would overwrite those changes."
                        ))
                        .color(warn),
                    );
                    if ui.button("Overwrite").clicked() {
                        effects.extend(self.overwrite());
                    }
                    if ui.button("Reload from database").on_hover_text("Discards your changes to this part").clicked() {
                        self.state.conflict = None;
                        self.state.compile_queue.clear();
                        for part in &mut self.parts {
                            part.saved = part.editor.text().to_owned();
                        }
                        effects.extend(self.load());
                    }
                    if ui.button("Cancel").clicked() {
                        self.state.conflict = None;
                        self.state.compile_queue.clear();
                    }
                });
            });
            ui.add_space(4.0);
        }
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
            let mut out = part.editor.ui(
                ui,
                ("program-editor", cx.id, self.active),
                &cx.settings.editor,
            );
            let unit = debug_unit(&self.object, part.object_type);
            // While debugging, hovering a variable shows its value.
            if let (Some(offset), Some(debug)) = (out.hover_request, cx.debug)
                && crate::editor::debug_hover(&mut part.editor, offset, debug, &unit)
            {
                out.hover_request = None;
            }
            effects.extend(part.assist.handle(&mut part.editor, &out, cx));
            effects.extend(crate::editor_menu::show(
                &out.response,
                &mut part.editor,
                &mut part.assist,
                cx,
            ));
            part.assist.background(&mut part.editor, ui);
            if let Some(line) = out.gutter_clicked {
                effects.breakpoints.push((unit.clone(), line as u32));
            }
            if let Some(debug) = cx.debug {
                let (lines, current) = debug.markers(&unit);
                part.editor.set_debug_lines(&lines, current);
            }
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

/// The name of the declaration `member` (a subprogram, variable, type, …)
/// in `text`.
fn member_span(text: &str, member: &str) -> Option<std::ops::Range<usize>> {
    use pythia_lsp_core::{OutlineItem, OutlineKind};
    fn find(items: &[OutlineItem], member: &str) -> Option<std::ops::Range<usize>> {
        items.iter().find_map(|item| {
            let declaration = !matches!(item.kind, OutlineKind::Statement | OutlineKind::Block);
            if declaration && item.name.eq_ignore_ascii_case(member) {
                Some(item.name_span.clone())
            } else {
                find(&item.children, member)
            }
        })
    }
    find(&pythia_lsp_core::outline(text), member)
}

/// The debugger's name for a program window part.
pub fn debug_unit(object: &ObjectRef, object_type: ObjectType) -> pythia_debugger::Unit {
    use pythia_debugger::UnitType;
    let unit_type = match object_type {
        ObjectType::Package => UnitType::Package,
        ObjectType::PackageBody => UnitType::PackageBody,
        ObjectType::Function => UnitType::Function,
        ObjectType::Trigger => UnitType::Trigger,
        ObjectType::Type => UnitType::Type,
        ObjectType::TypeBody => UnitType::TypeBody,
        _ => UnitType::Procedure,
    };
    pythia_debugger::Unit::new(object.owner.clone(), object.name.clone(), unit_type)
}

/// One outline entry with its children, indented by depth.
fn outline_item(
    ui: &mut Ui,
    item: &pythia_lsp_core::OutlineItem,
    depth: usize,
    jump: &mut Option<std::ops::Range<usize>>,
) {
    use pythia_lsp_core::OutlineKind as K;
    use pythia_ui::icons::{Icon, IconSize};
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
