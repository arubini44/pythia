//! Debugger UI: a session driver the app hosts, plus an egui panel with
//! toolbar, call stack, variables, watches and breakpoints.
//!
//! Keys (chosen not to clash with Pythia's PL/SQL Developer keymap, where F5 is
//! explain plan, F8 execute and F10/Shift+F10 commit/rollback):
//!
//! | Key        | Action                                   |
//! |------------|------------------------------------------|
//! | Ctrl+F9    | Start debugging (handled by the app)     |
//! | F9         | Continue                                 |
//! | F6         | Step over                                |
//! | F7         | Step into                                |
//! | Shift+F7   | Step out                                 |
//! | Shift+F9   | Stop (terminate the call)                |
//! | Ctrl+B     | Toggle breakpoint at cursor (app)        |

use std::collections::{BTreeMap, BTreeSet};

use egui::{Color32, Key, KeyboardShortcut, Modifiers, RichText, Ui};

use crate::Debugger;
use crate::condition;
use crate::types::{
    Breakpoint, BreakpointOptions, DebugCommand, DebugEvent, Frame, Location, StopReason, Unit,
    UnitType, Variable, Watch,
};

pub const CONTINUE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F9);
pub const STEP_OVER: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F6);
pub const STEP_INTO: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F7);
pub const STEP_OUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::F7);
pub const STOP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::SHIFT, Key::F9);
pub const START: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F9);
pub const TOGGLE_BREAKPOINT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::B);

/// What the app should do for the debugger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebugAction {
    /// Run `block` on a dedicated session under the debugger:
    /// `DbRequest::Task { task: target::debug_task(host, port, block) }`.
    RunTarget {
        host: String,
        port: u16,
        block: String,
    },
    /// Execution stopped here: open the unit and mark the line.
    ShowLocation(Location),
    /// The session ended: clear execution markers.
    Finished,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum State {
    #[default]
    Idle,
    /// Listening; waiting for the target session to connect.
    Waiting,
    Running,
    Suspended {
        reason: StopReason,
    },
    Finished,
}

/// Debugger state for one connection, hosted by the app.
#[derive(Default)]
pub struct DebugSession {
    debugger: Option<Debugger>,
    pub state: State,
    /// Breakpoints, kept across sessions (including disabled ones).
    pub breakpoints: BTreeSet<Breakpoint>,
    /// Breakpoints kept in the list but not set.
    pub disabled: BTreeSet<Breakpoint>,
    /// Conditions and hit counts.
    pub options: BTreeMap<Breakpoint, BreakpointOptions>,
    /// Hits (condition true) in the current session.
    pub hits: BTreeMap<Breakpoint, u32>,
    /// Line a breakpoint actually bound to (or an error), per breakpoint.
    bound: BTreeMap<Breakpoint, Result<u32, String>>,
    pub stack: Vec<Frame>,
    /// The selected call stack frame: `variables`, the watches and hover
    /// values are its. Every stop selects the innermost frame with source.
    pub frame: usize,
    pub variables: Vec<Variable>,
    /// Watched expressions, kept across sessions, and their last values.
    pub watches: Vec<String>,
    pub watch_values: Vec<Watch>,
    pub break_on_exception: bool,
    /// The exception execution stopped at.
    pub exception: Option<String>,
    pub log: Vec<String>,
    /// Text of the "add watch" field.
    new_watch: String,
    /// The breakpoint whose condition is being edited, with the draft
    /// condition and hit count.
    editing: Option<(Breakpoint, String, u32)>,
}

impl DebugSession {
    pub fn is_active(&self) -> bool {
        matches!(
            self.state,
            State::Waiting | State::Running | State::Suspended { .. }
        )
    }

    pub fn is_suspended(&self) -> bool {
        matches!(self.state, State::Suspended { .. })
    }

    /// Starts listening and asks the app to run `block` under the debugger.
    /// `host` is how the database reaches this machine.
    pub fn start(
        &mut self,
        block: String,
        host: String,
        notify: impl Fn() + Send + 'static,
    ) -> Vec<DebugAction> {
        self.stop();
        self.stack.clear();
        self.frame = 0;
        self.variables.clear();
        self.watch_values.clear();
        self.exception = None;
        self.bound.clear();
        self.hits.clear();
        self.log.clear();
        match Debugger::listen("0.0.0.0:0", notify) {
            Ok(debugger) => {
                for bp in self.breakpoints.difference(&self.disabled) {
                    debugger.send(DebugCommand::SetBreakpoint(bp.clone()));
                }
                for (bp, options) in &self.options {
                    debugger.send(DebugCommand::SetBreakpointOptions(
                        bp.clone(),
                        options.clone(),
                    ));
                }
                if !self.watches.is_empty() {
                    debugger.send(DebugCommand::SetWatches(self.watches.clone()));
                }
                if self.break_on_exception {
                    debugger.send(DebugCommand::BreakOnException(true));
                }
                let port = debugger.port();
                self.debugger = Some(debugger);
                self.state = State::Waiting;
                self.log.push(format!(
                    "Waiting for the session to connect to {host}:{port}…"
                ));
                vec![DebugAction::RunTarget { host, port, block }]
            }
            Err(e) => {
                self.log.push(format!("Could not start the debugger: {e}"));
                self.state = State::Finished;
                Vec::new()
            }
        }
    }

    /// Terminates the running call, if any.
    pub fn stop(&mut self) {
        if let Some(d) = self.debugger.take() {
            d.send(DebugCommand::Terminate);
        }
        if self.is_active() {
            self.state = State::Finished;
        }
    }

    pub fn command(&mut self, command: DebugCommand) {
        if let Some(d) = &self.debugger {
            if matches!(
                command,
                DebugCommand::Resume
                    | DebugCommand::StepOver
                    | DebugCommand::StepInto
                    | DebugCommand::StepOut
            ) {
                self.state = State::Running;
                self.exception = None;
            }
            d.send(command);
        }
    }

    /// Adds a breakpoint, enables a disabled one, or removes it.
    pub fn toggle_breakpoint(&mut self, unit: Unit, line: u32) {
        let bp = Breakpoint { unit, line };
        if self.disabled.contains(&bp) {
            self.set_enabled(&bp, true);
        } else if self.breakpoints.remove(&bp) {
            self.bound.remove(&bp);
            self.hits.remove(&bp);
            self.options.remove(&bp);
            self.command(DebugCommand::ClearBreakpoint(bp));
        } else {
            self.breakpoints.insert(bp.clone());
            self.command(DebugCommand::SetBreakpoint(bp));
        }
    }

    /// Enables or disables a breakpoint without forgetting it.
    pub fn set_enabled(&mut self, bp: &Breakpoint, enabled: bool) {
        if !self.breakpoints.contains(bp) {
            return;
        }
        if enabled {
            if self.disabled.remove(bp) {
                self.command(DebugCommand::SetBreakpoint(bp.clone()));
                if let Some(options) = self.options.get(bp).cloned() {
                    self.command(DebugCommand::SetBreakpointOptions(bp.clone(), options));
                }
            }
        } else if self.disabled.insert(bp.clone()) {
            self.bound.remove(bp);
            self.command(DebugCommand::ClearBreakpoint(bp.clone()));
        }
    }

    /// Sets a breakpoint's condition and hit count.
    pub fn set_options(&mut self, bp: &Breakpoint, options: BreakpointOptions) {
        if options.is_empty() {
            self.options.remove(bp);
        } else {
            self.options.insert(bp.clone(), options.clone());
        }
        self.command(DebugCommand::SetBreakpointOptions(bp.clone(), options));
    }

    pub fn add_watch(&mut self, expression: &str) {
        let expression = expression.trim();
        if expression.is_empty()
            || self
                .watches
                .iter()
                .any(|w| w.eq_ignore_ascii_case(expression))
        {
            return;
        }
        self.watches.push(expression.to_owned());
        self.command(DebugCommand::SetWatches(self.watches.clone()));
    }

    pub fn remove_watch(&mut self, index: usize) {
        if index < self.watches.len() {
            let removed = self.watches.remove(index);
            self.watch_values.retain(|w| w.expression != removed);
            self.command(DebugCommand::SetWatches(self.watches.clone()));
        }
    }

    pub fn set_break_on_exception(&mut self, on: bool) {
        self.break_on_exception = on;
        self.command(DebugCommand::BreakOnException(on));
    }

    /// The frame execution is shown at while suspended: the innermost one
    /// with source lines (code without debug information, such as SYS
    /// packages, has none).
    pub fn current_frame(&self) -> Option<&Frame> {
        if !self.is_suspended() {
            return None;
        }
        self.stack.get(self.execution_frame())
    }

    /// Index of [`Self::current_frame`] in the stack.
    fn execution_frame(&self) -> usize {
        self.stack
            .iter()
            .position(|f| f.location.line.is_some())
            .unwrap_or(0)
    }

    /// Selects a call stack frame while suspended: its variables are shown
    /// and watches and hover values come from it (the debugger reports
    /// them). Returns where to show its source.
    pub fn select_frame(&mut self, index: usize) -> Option<DebugAction> {
        let location = self.stack.get(index)?.location.clone();
        if self.is_suspended() && index != self.frame {
            self.frame = index;
            // Not the previous frame's values meanwhile.
            self.variables.clear();
            self.watch_values.clear();
            self.command(DebugCommand::SelectFrame(index));
        }
        Some(DebugAction::ShowLocation(location))
    }

    /// Enabled breakpoint lines and the current execution line for a unit,
    /// for editor gutter markers.
    pub fn markers(&self, unit: &Unit) -> (Vec<u32>, Option<u32>) {
        let lines = self
            .breakpoints
            .iter()
            .filter(|b| &b.unit == unit && !self.disabled.contains(b))
            .map(|b| b.line)
            .collect();
        let current = self
            .current_frame()
            .filter(|f| &f.location.unit == unit)
            .and_then(|f| f.location.line);
        (lines, current)
    }

    /// The value of `expression` (a variable, `record.field`, …) while the
    /// selected frame is in `unit`, as hover text. Anonymous blocks match
    /// any block.
    pub fn hover(&self, unit: &Unit, expression: &str) -> Option<(String, String)> {
        if !self.is_suspended() {
            return None;
        }
        let selected = &self.stack.get(self.frame)?.location.unit;
        let same = selected == unit
            || (selected.unit_type == UnitType::Block && unit.unit_type == UnitType::Block);
        if !same {
            return None;
        }
        let path = condition::parse_path(expression)?;
        let var = condition::lookup(&self.variables, &path).or_else(|| {
            self.watch_values
                .iter()
                .find(|w| condition::parse_path(&w.expression).as_ref() == Some(&path))
                .and_then(|w| w.value.as_ref().ok())
        })?;
        let title = format!("{} {}", expression.trim(), var.type_name)
            .trim()
            .to_owned();
        Some((title, describe(var)))
    }

    /// Drains debugger events; call every frame.
    pub fn poll(&mut self) -> Vec<DebugAction> {
        let mut actions = Vec::new();
        let Some(debugger) = &self.debugger else {
            return actions;
        };
        let mut events = Vec::new();
        while let Some(e) = debugger.try_recv() {
            events.push(e);
        }
        for event in events {
            self.on_event(event, &mut actions);
        }
        actions
    }

    fn on_event(&mut self, event: DebugEvent, actions: &mut Vec<DebugAction>) {
        match event {
            DebugEvent::Listening { .. } => {}
            DebugEvent::Connected { version } => {
                self.state = State::Running;
                self.log.push(format!("Connected: {version}"));
            }
            DebugEvent::BreakpointBound {
                breakpoint,
                line,
                error,
            } => {
                let entry = match (line, error) {
                    (Some(l), _) => Ok(l),
                    (None, e) => Err(e.unwrap_or_else(|| "not set".into())),
                };
                if let Err(e) = &entry {
                    self.log.push(format!(
                        "Breakpoint at {} line {}: {e}",
                        breakpoint.unit, breakpoint.line
                    ));
                }
                self.bound.insert(breakpoint, entry);
            }
            DebugEvent::BreakpointHit { breakpoint, hits } => {
                self.hits.insert(breakpoint, hits);
            }
            DebugEvent::Suspended {
                reason,
                location,
                stack,
                frame,
                variables,
                watches,
                exception,
            } => {
                self.state = State::Suspended { reason };
                self.stack = stack;
                self.frame = frame;
                self.variables = variables;
                self.watch_values = watches;
                if let Some(e) = &exception {
                    self.log.push(format!("Exception: {e}"));
                }
                self.exception = exception;
                let shown = self.current_frame().map(|f| f.location.clone());
                actions.push(DebugAction::ShowLocation(shown.unwrap_or(location)));
            }
            DebugEvent::Watches(values) => self.watch_values = values,
            // Only the latest selection (a click may still be on its way).
            DebugEvent::FrameSelected {
                frame,
                variables,
                watches,
            } if frame == self.frame => {
                self.variables = variables;
                self.watch_values = watches;
            }
            DebugEvent::FrameSelected { .. } => {}
            DebugEvent::SteppedOver(units) => {
                for unit in units {
                    self.log.push(format!(
                        "Step into went over {unit}: it was compiled without debug \
                         information (Settings → Debugger)"
                    ));
                }
            }
            DebugEvent::Resumed => {
                if !self.is_suspended() {
                    self.state = State::Running;
                }
            }
            DebugEvent::Terminated => {
                self.state = State::Finished;
                self.debugger = None;
                self.stack.clear();
                self.frame = 0;
                self.exception = None;
                self.log.push("Debug session ended".into());
                actions.push(DebugAction::Finished);
            }
            DebugEvent::Error(e) => self.log.push(e),
        }
    }

    /// Debugger shortcuts; call before the editors see the input.
    pub fn handle_keys(&mut self, ctx: &egui::Context) {
        if !self.is_active() {
            return;
        }
        let pressed = |s: &KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(s));
        // Shifted variants first so F7 doesn't swallow Shift+F7.
        if pressed(&STEP_OUT) {
            self.command(DebugCommand::StepOut);
        } else if pressed(&STOP) {
            self.stop();
        } else if pressed(&CONTINUE) {
            self.command(DebugCommand::Resume);
        } else if pressed(&STEP_OVER) {
            self.command(DebugCommand::StepOver);
        } else if pressed(&STEP_INTO) {
            self.command(DebugCommand::StepInto);
        }
    }

    /// The debugger panel. Returns actions (e.g. jumping to a stack frame).
    pub fn ui(&mut self, ui: &mut Ui) -> Vec<DebugAction> {
        let mut actions = Vec::new();
        self.toolbar(ui);
        ui.separator();
        ui.columns(4, |cols| {
            cols[0].strong("Call stack");
            let mut select = None;
            egui::ScrollArea::vertical()
                .id_salt("dbg-stack")
                .show(&mut cols[0], |ui| {
                    for (i, frame) in self.stack.iter().enumerate() {
                        let text = RichText::new(frame_label(frame)).monospace();
                        if ui.selectable_label(i == self.frame, text).clicked() {
                            select = Some(i);
                        }
                    }
                    if self.stack.is_empty() {
                        ui.weak("—");
                    }
                    if !self.log.is_empty() {
                        ui.add_space(6.0);
                        // Not justified: that would stretch the words.
                        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
                            for line in self.log.iter().rev().take(5) {
                                ui.add(
                                    egui::Label::new(RichText::new(line).small().weak()).truncate(),
                                )
                                .on_hover_text(line);
                            }
                        });
                    }
                });
            if let Some(action) = select.and_then(|i| self.select_frame(i)) {
                actions.push(action);
            }

            cols[1].horizontal(|ui| {
                ui.strong("Variables");
                // Say whose, when it isn't where execution stopped.
                if self.is_suspended()
                    && self.frame != self.execution_frame()
                    && let Some(frame) = self.stack.get(self.frame)
                {
                    ui.add(
                        egui::Label::new(RichText::new(frame_label(frame)).monospace().weak())
                            .truncate(),
                    );
                }
            });
            let mut watch = None;
            egui::ScrollArea::vertical()
                .id_salt("dbg-vars")
                .show(&mut cols[1], |ui| {
                    for (i, var) in self.variables.iter().enumerate() {
                        variable_ui(ui, var, "", i, &mut watch);
                    }
                    if self.variables.is_empty() {
                        ui.weak("—");
                    }
                });
            if let Some(expression) = watch {
                self.add_watch(&expression);
            }

            self.watches_ui(&mut cols[2]);
            self.breakpoints_ui(&mut cols[3]);
        });
        actions
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        let suspended = self.is_suspended();
        let active = self.is_active();
        ui.horizontal(|ui| {
            let button = |ui: &mut Ui, label: &str, shortcut: &KeyboardShortcut, enabled: bool| {
                let text = format!("{label}  {}", ui.ctx().format_shortcut(shortcut));
                ui.add_enabled(enabled, egui::Button::new(text)).clicked()
            };
            if button(ui, "Continue", &CONTINUE, suspended) {
                self.command(DebugCommand::Resume);
            }
            if button(ui, "Step over", &STEP_OVER, suspended) {
                self.command(DebugCommand::StepOver);
            }
            if button(ui, "Step into", &STEP_INTO, suspended) {
                self.command(DebugCommand::StepInto);
            }
            if button(ui, "Step out", &STEP_OUT, suspended) {
                self.command(DebugCommand::StepOut);
            }
            if ui
                .add_enabled(active && !suspended, egui::Button::new("Pause"))
                .clicked()
            {
                self.command(DebugCommand::Pause);
            }
            if button(ui, "Stop", &STOP, active) {
                self.stop();
            }
            ui.separator();
            let mut on = self.break_on_exception;
            if ui
                .checkbox(&mut on, "Break on exceptions")
                .on_hover_text("Stop where an exception is raised, handled or not")
                .changed()
            {
                self.set_break_on_exception(on);
            }
            ui.separator();
            let (text, color) = match &self.state {
                State::Idle => ("Not debugging".to_owned(), ui.visuals().weak_text_color()),
                State::Waiting => (
                    "Waiting for the session…".to_owned(),
                    ui.visuals().warn_fg_color,
                ),
                State::Running => ("Running".to_owned(), Color32::from_rgb(0x3f, 0xb9, 0x50)),
                State::Suspended { reason } => {
                    let at = self.current_frame().map(frame_label).unwrap_or_default();
                    let text = match reason {
                        StopReason::Breakpoint => format!("Stopped at breakpoint, {at}"),
                        StopReason::Step => format!("Stopped after step, {at}"),
                        StopReason::Pause => format!("Paused at {at}"),
                        StopReason::Exception => format!(
                            "Exception at {at}: {}",
                            self.exception.as_deref().unwrap_or("raised")
                        ),
                    };
                    let color = if *reason == StopReason::Exception {
                        ui.visuals().error_fg_color
                    } else {
                        ui.visuals().warn_fg_color
                    };
                    (text, color)
                }
                State::Finished => ("Finished".to_owned(), ui.visuals().weak_text_color()),
            };
            ui.add(egui::Label::new(RichText::new(text).color(color)).truncate());
        });
    }

    fn watches_ui(&mut self, ui: &mut Ui) {
        ui.strong("Watches");
        let mut remove = None;
        let stale = !self.is_suspended();
        egui::ScrollArea::vertical()
            .id_salt("dbg-watches")
            .max_height((ui.available_height() - 28.0).max(40.0))
            .show(ui, |ui| {
                for (i, expression) in self.watches.iter().enumerate() {
                    let value = self
                        .watch_values
                        .iter()
                        .find(|w| &w.expression == expression);
                    ui.horizontal(|ui| {
                        if ui.small_button("×").on_hover_text("Remove watch").clicked() {
                            remove = Some(i);
                        }
                        match value.map(|w| &w.value) {
                            Some(Ok(var)) if var.children.is_empty() => {
                                ui.label(RichText::new(expression).monospace().strong());
                                ui.label(value_text(ui, var, stale));
                            }
                            Some(Ok(var)) => {
                                let mut named = var.clone();
                                named.name = expression.clone();
                                ui.vertical(|ui| {
                                    let mut ignore = None;
                                    variable_ui(ui, &named, "", 1000 + i, &mut ignore);
                                });
                            }
                            Some(Err(e)) => {
                                ui.label(RichText::new(expression).monospace().strong());
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(e).small().color(ui.visuals().warn_fg_color),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(e);
                            }
                            None => {
                                ui.label(RichText::new(expression).monospace().strong());
                                ui.weak("—");
                            }
                        }
                    });
                }
                if self.watches.is_empty() {
                    ui.weak("Right-click a variable or type an expression below");
                }
            });
        if let Some(i) = remove {
            self.remove_watch(i);
        }
        let response = ui.add(
            egui::TextEdit::singleline(&mut self.new_watch)
                .hint_text("Add watch: l_rec.name, pkg.g_var")
                .desired_width(f32::INFINITY),
        );
        if response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            let expression = std::mem::take(&mut self.new_watch);
            self.add_watch(&expression);
            response.request_focus();
        }
    }

    fn breakpoints_ui(&mut self, ui: &mut Ui) {
        ui.strong("Breakpoints");
        let mut remove = None;
        let mut enable = None;
        let mut edit = None;
        let mut apply = None;
        egui::ScrollArea::vertical()
            .id_salt("dbg-bps")
            .show(ui, |ui| {
                for bp in &self.breakpoints {
                    let enabled = !self.disabled.contains(bp);
                    ui.horizontal(|ui| {
                        let mut on = enabled;
                        if ui.checkbox(&mut on, "").on_hover_text("Enabled").changed() {
                            enable = Some((bp.clone(), on));
                        }
                        let label = match self.bound.get(bp) {
                            Some(Ok(line)) if *line != bp.line => {
                                format!("{} : {} → {line}", unit_label(&bp.unit), bp.line)
                            }
                            _ => format!("{} : {}", unit_label(&bp.unit), bp.line),
                        };
                        let mut text = RichText::new(label).monospace();
                        if !enabled {
                            text = text.color(ui.visuals().weak_text_color());
                        }
                        let response = if let Some(Err(e)) = self.bound.get(bp) {
                            ui.label(text.color(ui.visuals().warn_fg_color))
                                .on_hover_text(e)
                        } else {
                            ui.label(text)
                        };
                        if let Some(hits) = self.hits.get(bp) {
                            ui.weak(format!("{hits}×"))
                                .on_hover_text("Hits in this session");
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("×").on_hover_text("Remove").clicked() {
                                remove = Some(bp.clone());
                            }
                            if ui
                                .small_button("if…")
                                .on_hover_text("Condition and hit count")
                                .clicked()
                            {
                                edit = Some(bp.clone());
                            }
                        });
                        response.context_menu(|ui| {
                            if ui.button("Condition…").clicked() {
                                edit = Some(bp.clone());
                                ui.close();
                            }
                            if ui
                                .button(if enabled { "Disable" } else { "Enable" })
                                .clicked()
                            {
                                enable = Some((bp.clone(), !enabled));
                                ui.close();
                            }
                            if ui.button("Remove").clicked() {
                                remove = Some(bp.clone());
                                ui.close();
                            }
                        });
                    });
                    let editing = self.editing.as_mut().filter(|(b, ..)| b == bp);
                    if let Some((_, condition, hit_count)) = editing {
                        ui.indent(("bp-edit", bp.line, &bp.unit.name), |ui| {
                            let r = ui.add(
                                egui::TextEdit::singleline(condition)
                                    .hint_text("Condition, e.g. l_total > 3")
                                    .font(egui::TextStyle::Monospace)
                                    .desired_width(f32::INFINITY),
                            );
                            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                            ui.horizontal(|ui| {
                                ui.label("Stop from hit");
                                ui.add(egui::DragValue::new(hit_count).range(0..=100_000))
                                    .on_hover_text("0 or 1: every hit");
                                if ui.button("Apply").clicked() || enter {
                                    let condition = condition.trim();
                                    apply = Some((
                                        bp.clone(),
                                        BreakpointOptions {
                                            condition: (!condition.is_empty())
                                                .then(|| condition.to_owned()),
                                            hit_count: (*hit_count > 1).then_some(*hit_count),
                                        },
                                    ));
                                }
                                if ui.button("Cancel").clicked() {
                                    edit = Some(bp.clone());
                                }
                            });
                        });
                    } else if let Some(options) = self.options.get(bp) {
                        let mut parts = Vec::new();
                        if let Some(c) = &options.condition {
                            parts.push(format!("if {c}"));
                        }
                        if let Some(n) = options.hit_count {
                            parts.push(format!("from hit {n}"));
                        }
                        ui.indent(("bp-opts", bp.line, &bp.unit.name), |ui| {
                            let text = parts.join(" · ");
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&text)
                                        .monospace()
                                        .color(ui.visuals().hyperlink_color),
                                )
                                .truncate(),
                            )
                            .on_hover_text(text);
                        });
                    }
                }
                if self.breakpoints.is_empty() {
                    ui.weak("Click the editor gutter or press Ctrl+B");
                }
            });
        if let Some((bp, on)) = enable {
            self.set_enabled(&bp, on);
        }
        if let Some(bp) = remove {
            self.editing = None;
            self.disabled.remove(&bp);
            self.toggle_breakpoint(bp.unit, bp.line);
        }
        if let Some((bp, options)) = apply {
            self.editing = None;
            self.set_options(&bp, options);
        } else if let Some(bp) = edit {
            self.editing = match self.editing.take() {
                Some((b, ..)) if b == bp => None,
                _ => {
                    let options = self.options.get(&bp).cloned().unwrap_or_default();
                    Some((
                        bp,
                        options.condition.unwrap_or_default(),
                        options.hit_count.unwrap_or(0),
                    ))
                }
            };
        }
    }
}

fn unit_label(unit: &Unit) -> String {
    match unit.unit_type {
        UnitType::Block => "anonymous block".into(),
        _ => unit.name.to_lowercase(),
    }
}

/// `unit:line`, with the subprogram for packages and types, e.g.
/// `orders_api.load_order:31`.
fn frame_label(frame: &Frame) -> String {
    let unit = &frame.location.unit;
    let line = frame
        .location
        .line
        .map(|l| format!(":{l}"))
        .unwrap_or_default();
    let subprogram = match frame.method.as_str() {
        // The package's initialization section (its BEGIN … END).
        "<clinit>" => Some("(initialization)".to_owned()),
        // Generated names, e.g. in code without debug information.
        m if m.starts_with('<') || m.is_empty() => None,
        m => Some(m.to_lowercase()),
    };
    match (unit.unit_type, subprogram) {
        (
            UnitType::Package | UnitType::PackageBody | UnitType::Type | UnitType::TypeBody,
            Some(subprogram),
        ) => format!("{}.{subprogram}{line}", unit_label(unit)),
        _ => format!("{}{line}", unit_label(unit)),
    }
}

fn value_text(ui: &Ui, var: &Variable, stale: bool) -> RichText {
    let text = match &var.value {
        Some(v) => RichText::new(v).monospace(),
        None => RichText::new("NULL")
            .monospace()
            .italics()
            .color(ui.visuals().weak_text_color()),
    };
    if stale {
        text.color(ui.visuals().weak_text_color())
    } else {
        text
    }
}

/// A variable and its fields/elements; right-click offers "Add watch" with
/// the variable's full path (`parent` is the path of the enclosing one).
fn variable_ui(
    ui: &mut Ui,
    var: &Variable,
    parent: &str,
    index: usize,
    watch: &mut Option<String>,
) {
    let name = var.name.to_lowercase();
    let path = if parent.is_empty() {
        name.clone()
    } else if name.starts_with('(') {
        format!("{parent}{name}")
    } else {
        format!("{parent}.{name}")
    };
    let menu = |ui: &mut Ui, watch: &mut Option<String>| {
        if ui.button(format!("Watch {path}")).clicked() {
            *watch = Some(path.clone());
            ui.close();
        }
        if let Some(v) = &var.value
            && ui.button("Copy value").clicked()
        {
            ui.ctx().copy_text(v.clone());
            ui.close();
        }
    };
    if var.children.is_empty() {
        let response = ui
            .horizontal(|ui| {
                ui.label(RichText::new(&name).monospace().strong());
                ui.label(value_text(ui, var, false));
                ui.weak(&var.type_name);
            })
            .response
            .interact(egui::Sense::click());
        response.context_menu(|ui| menu(ui, watch));
    } else {
        let summary = var
            .value
            .as_deref()
            .map(|v| format!("  {v}"))
            .unwrap_or_default();
        let header = format!("{name}  {}{summary}", var.type_name);
        let response = egui::CollapsingHeader::new(RichText::new(header).monospace())
            .id_salt(("var", index, &var.name, parent))
            .show(ui, |ui| {
                for (i, child) in var.children.iter().enumerate() {
                    variable_ui(ui, child, &path, i, watch);
                }
            });
        response.header_response.context_menu(|ui| menu(ui, watch));
    }
}

/// Hover text for a variable: its value, or its fields/elements.
fn describe(var: &Variable) -> String {
    const MAX: usize = 12;
    let value = |v: &Variable| v.value.clone().unwrap_or_else(|| "NULL".into());
    if var.children.is_empty() {
        return value(var);
    }
    let mut lines: Vec<String> = var.value.iter().cloned().collect();
    for child in var.children.iter().take(MAX) {
        let shown = if child.children.is_empty() {
            value(child)
        } else {
            format!("{} …", child.type_name)
        };
        lines.push(format!("{} = {shown}", child.name.to_lowercase()));
    }
    if var.children.len() > MAX {
        lines.push(format!("… {} more", var.children.len() - MAX));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, value: Option<&str>, children: Vec<Variable>) -> Variable {
        Variable {
            name: name.into(),
            type_name: "NUMBER".into(),
            value: value.map(Into::into),
            children,
        }
    }

    #[test]
    fn markers_follow_breakpoints() {
        let mut s = DebugSession::default();
        let unit = Unit::new("PYTHIA", "P", UnitType::Procedure);
        s.toggle_breakpoint(unit.clone(), 5);
        s.toggle_breakpoint(unit.clone(), 9);
        s.toggle_breakpoint(unit.clone(), 5);
        assert_eq!(s.markers(&unit), (vec![9], None));
        s.state = State::Suspended {
            reason: StopReason::Breakpoint,
        };
        s.stack = vec![Frame {
            location: Location {
                unit: unit.clone(),
                line: Some(9),
            },
            method: "P".into(),
        }];
        assert_eq!(s.markers(&unit).1, Some(9));
    }

    fn frame(unit: &Unit, method: &str, line: Option<u32>) -> Frame {
        Frame {
            location: Location {
                unit: unit.clone(),
                line,
            },
            method: method.into(),
        }
    }

    #[test]
    fn execution_shown_in_the_nearest_frame_with_source() {
        // Paused inside code without debug information.
        let sys = Unit::new("SYS", "DBMS_SESSION", UnitType::PackageBody);
        let api = Unit::new("PYTHIA", "API", UnitType::PackageBody);
        let mut s = DebugSession {
            state: State::Suspended {
                reason: StopReason::Pause,
            },
            stack: vec![
                frame(&sys, "<procedure$29>", None),
                frame(&api, "SPIN", Some(69)),
            ],
            ..Default::default()
        };
        assert_eq!(s.current_frame().map(|f| f.method.as_str()), Some("SPIN"));
        assert_eq!(s.markers(&api).1, Some(69));
        assert_eq!(s.markers(&sys).1, None);
        s.state = State::Running;
        assert!(s.current_frame().is_none());
        assert_eq!(s.markers(&api).1, None);
    }

    #[test]
    fn frame_labels_name_the_subprogram() {
        let body = Unit::new("PYTHIA", "ORDERS_API", UnitType::PackageBody);
        let proc = Unit::new("PYTHIA", "DBG_DEMO", UnitType::Procedure);
        let block = Unit::new("PYTHIA", "B1", UnitType::Block);
        let sys = Unit::new("SYS", "DBMS_SESSION", UnitType::PackageBody);
        assert_eq!(
            frame_label(&frame(&body, "LOAD_ORDER", Some(31))),
            "orders_api.load_order:31"
        );
        assert_eq!(
            frame_label(&frame(&body, "<clinit>", Some(40))),
            "orders_api.(initialization):40"
        );
        assert_eq!(
            frame_label(&frame(&proc, "DBG_DEMO", Some(3))),
            "dbg_demo:3"
        );
        assert_eq!(
            frame_label(&frame(&block, "BLOCK", Some(4))),
            "anonymous block:4"
        );
        assert_eq!(
            frame_label(&frame(&sys, "<procedure$29>", None)),
            "dbms_session"
        );
    }

    #[test]
    fn disabled_breakpoints_stay_listed() {
        let mut s = DebugSession::default();
        let unit = Unit::new("PYTHIA", "P", UnitType::Procedure);
        s.toggle_breakpoint(unit.clone(), 5);
        let bp = Breakpoint {
            unit: unit.clone(),
            line: 5,
        };
        s.set_enabled(&bp, false);
        assert_eq!(s.markers(&unit).0, Vec::<u32>::new());
        assert!(s.breakpoints.contains(&bp));
        // Toggling a disabled breakpoint enables it again.
        s.toggle_breakpoint(unit.clone(), 5);
        assert_eq!(s.markers(&unit).0, vec![5]);
        s.set_options(
            &bp,
            BreakpointOptions {
                condition: Some("x > 1".into()),
                hit_count: None,
            },
        );
        assert!(s.options.contains_key(&bp));
        s.toggle_breakpoint(unit, 5);
        assert!(s.breakpoints.is_empty() && s.options.is_empty());
    }

    #[test]
    fn hover_values_while_stopped() {
        let mut s = DebugSession::default();
        let unit = Unit::new("PYTHIA", "P", UnitType::Procedure);
        s.stack = vec![Frame {
            location: Location {
                unit: unit.clone(),
                line: Some(3),
            },
            method: "P".into(),
        }];
        s.variables = vec![
            var("L_TOTAL", Some("6"), vec![]),
            var("L_REC", None, vec![var("A", Some("7"), vec![])]),
        ];
        s.watch_values = vec![Watch {
            expression: "pkg.g_count".into(),
            value: Ok(var("G_COUNT", Some("2"), vec![])),
        }];
        assert_eq!(s.hover(&unit, "l_total"), None, "only while suspended");
        s.state = State::Suspended {
            reason: StopReason::Step,
        };
        assert_eq!(s.hover(&unit, "L_Total").unwrap().1, "6");
        assert_eq!(s.hover(&unit, "l_rec.a").unwrap().1, "7");
        assert_eq!(s.hover(&unit, "l_rec").unwrap().1, "a = 7");
        assert_eq!(s.hover(&unit, "PKG.G_COUNT").unwrap().1, "2");
        assert_eq!(s.hover(&unit, "l_other"), None);
        let other = Unit::new("PYTHIA", "Q", UnitType::Procedure);
        assert_eq!(s.hover(&other, "l_total"), None, "other unit");
    }

    #[test]
    fn selected_frame_has_the_variables() {
        let api = Unit::new("PYTHIA", "API", UnitType::PackageBody);
        let pricing = Unit::new("PYTHIA", "PRICING", UnitType::PackageBody);
        let mut s = DebugSession::default();
        let mut actions = Vec::new();
        let stop = |variables| DebugEvent::Suspended {
            reason: StopReason::Breakpoint,
            location: Location {
                unit: pricing.clone(),
                line: Some(15),
            },
            stack: vec![
                frame(&pricing, "LINE_AMOUNT", Some(15)),
                frame(&api, "PROCESS", Some(41)),
            ],
            frame: 0,
            variables,
            watches: Vec::new(),
            exception: None,
        };
        s.on_event(stop(vec![var("P_QTY", Some("2"), vec![])]), &mut actions);
        assert_eq!(s.hover(&pricing, "p_qty").unwrap().1, "2");
        assert_eq!(s.hover(&api, "l_total"), None);

        // The caller: its source is shown, its values come from the debugger.
        let shown = s.select_frame(1);
        assert_eq!(
            shown,
            Some(DebugAction::ShowLocation(Location {
                unit: api.clone(),
                line: Some(41)
            }))
        );
        assert_eq!(s.frame, 1);
        assert!(s.variables.is_empty(), "not the other frame's values");
        assert_eq!(s.hover(&pricing, "p_qty"), None);
        s.on_event(
            DebugEvent::FrameSelected {
                frame: 1,
                variables: vec![var("L_TOTAL", Some("0"), vec![])],
                watches: Vec::new(),
            },
            &mut actions,
        );
        assert_eq!(s.hover(&api, "l_total").unwrap().1, "0");
        assert_eq!(s.hover(&pricing, "l_total"), None);
        // An answer for an earlier selection doesn't replace it.
        s.on_event(
            DebugEvent::FrameSelected {
                frame: 0,
                variables: Vec::new(),
                watches: Vec::new(),
            },
            &mut actions,
        );
        assert_eq!(s.variables.len(), 1);
        // The next stop starts at the innermost frame again.
        s.on_event(stop(Vec::new()), &mut actions);
        assert_eq!(s.frame, 0);
        assert_eq!(
            s.current_frame().map(|f| f.method.as_str()),
            Some("LINE_AMOUNT")
        );
    }

    #[test]
    fn stepping_over_code_without_debug_information_is_logged() {
        let mut s = DebugSession::default();
        s.on_event(
            DebugEvent::SteppedOver(vec![Unit::new("PYTHIA", "PRICING", UnitType::PackageBody)]),
            &mut Vec::new(),
        );
        assert_eq!(
            s.log,
            [
                "Step into went over package body PYTHIA.PRICING: it was compiled without debug \
              information (Settings → Debugger)"
            ]
        );
    }
}
