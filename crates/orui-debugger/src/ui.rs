//! Debugger UI: a session driver the app hosts, plus an egui panel with
//! toolbar, call stack, variables and breakpoints.
//!
//! Keys (chosen not to clash with orui's PL/SQL Developer keymap, where F5 is
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
use crate::types::{
    Breakpoint, DebugCommand, DebugEvent, Frame, Location, StopReason, Unit, Variable,
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
    /// Breakpoints, kept across sessions.
    pub breakpoints: BTreeSet<Breakpoint>,
    /// Line a breakpoint actually bound to (or an error), per breakpoint.
    bound: BTreeMap<Breakpoint, Result<u32, String>>,
    pub stack: Vec<Frame>,
    pub variables: Vec<Variable>,
    pub log: Vec<String>,
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
        self.variables.clear();
        self.bound.clear();
        self.log.clear();
        match Debugger::listen("0.0.0.0:0", notify) {
            Ok(debugger) => {
                for bp in &self.breakpoints {
                    debugger.send(DebugCommand::SetBreakpoint(bp.clone()));
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
            }
            d.send(command);
        }
    }

    pub fn toggle_breakpoint(&mut self, unit: Unit, line: u32) {
        let bp = Breakpoint { unit, line };
        if self.breakpoints.remove(&bp) {
            self.bound.remove(&bp);
            self.command(DebugCommand::ClearBreakpoint(bp));
        } else {
            self.breakpoints.insert(bp.clone());
            self.command(DebugCommand::SetBreakpoint(bp));
        }
    }

    /// Breakpoint lines and the current execution line for a unit, for
    /// editor gutter markers.
    pub fn markers(&self, unit: &Unit) -> (Vec<u32>, Option<u32>) {
        let lines = self
            .breakpoints
            .iter()
            .filter(|b| &b.unit == unit)
            .map(|b| b.line)
            .collect();
        let current = match (&self.state, self.stack.first()) {
            (State::Suspended { .. }, Some(f)) if &f.location.unit == unit => f.location.line,
            _ => None,
        };
        (lines, current)
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
                DebugEvent::Suspended {
                    reason,
                    location,
                    stack,
                    variables,
                } => {
                    self.state = State::Suspended { reason };
                    self.stack = stack;
                    self.variables = variables;
                    actions.push(DebugAction::ShowLocation(location));
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
                    self.log.push("Debug session ended".into());
                    actions.push(DebugAction::Finished);
                }
                DebugEvent::Error(e) => self.log.push(e),
            }
        }
        actions
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
            let (text, color) = match &self.state {
                State::Idle => ("Not debugging".to_owned(), ui.visuals().weak_text_color()),
                State::Waiting => (
                    "Waiting for the session…".to_owned(),
                    ui.visuals().warn_fg_color,
                ),
                State::Running => ("Running".to_owned(), Color32::from_rgb(0x3f, 0xb9, 0x50)),
                State::Suspended { reason } => {
                    let why = match reason {
                        StopReason::Breakpoint => "breakpoint",
                        StopReason::Step => "step",
                        StopReason::Pause => "pause",
                    };
                    let at = self.stack.first().map(frame_label).unwrap_or_default();
                    (
                        format!("Suspended ({why}) at {at}"),
                        ui.visuals().warn_fg_color,
                    )
                }
                State::Finished => ("Finished".to_owned(), ui.visuals().weak_text_color()),
            };
            ui.label(RichText::new(text).color(color));
        });
        ui.separator();

        ui.columns(3, |cols| {
            cols[0].strong("Call stack");
            egui::ScrollArea::vertical()
                .id_salt("dbg-stack")
                .show(&mut cols[0], |ui| {
                    for (i, frame) in self.stack.iter().enumerate() {
                        let text = RichText::new(frame_label(frame)).monospace();
                        if ui.selectable_label(i == 0, text).clicked() {
                            actions.push(DebugAction::ShowLocation(frame.location.clone()));
                        }
                    }
                    if self.stack.is_empty() {
                        ui.weak("—");
                    }
                });

            cols[1].strong("Variables");
            egui::ScrollArea::vertical()
                .id_salt("dbg-vars")
                .show(&mut cols[1], |ui| {
                    for (i, var) in self.variables.iter().enumerate() {
                        variable_ui(ui, var, i);
                    }
                    if self.variables.is_empty() {
                        ui.weak("—");
                    }
                });

            cols[2].strong("Breakpoints");
            egui::ScrollArea::vertical()
                .id_salt("dbg-bps")
                .show(&mut cols[2], |ui| {
                    let mut remove = None;
                    for bp in &self.breakpoints {
                        ui.horizontal(|ui| {
                            if ui.small_button("×").on_hover_text("Remove").clicked() {
                                remove = Some(bp.clone());
                            }
                            let label = match self.bound.get(bp) {
                                Some(Ok(line)) if *line != bp.line => {
                                    format!("{} : {} → {line}", unit_label(&bp.unit), bp.line)
                                }
                                _ => format!("{} : {}", unit_label(&bp.unit), bp.line),
                            };
                            let mut text = RichText::new(label).monospace();
                            if let Some(Err(e)) = self.bound.get(bp) {
                                text = text.color(ui.visuals().warn_fg_color);
                                ui.label(text).on_hover_text(e);
                            } else {
                                ui.label(text);
                            }
                        });
                    }
                    if let Some(bp) = remove {
                        self.toggle_breakpoint(bp.unit, bp.line);
                    }
                    if self.breakpoints.is_empty() {
                        ui.weak("Click the editor gutter or press Ctrl+B");
                    }
                    if !self.log.is_empty() {
                        ui.separator();
                        for line in self.log.iter().rev().take(6) {
                            ui.weak(line);
                        }
                    }
                });
        });
        actions
    }
}

fn unit_label(unit: &Unit) -> String {
    match unit.unit_type {
        crate::types::UnitType::Block => "anonymous block".into(),
        _ => unit.name.to_lowercase(),
    }
}

fn frame_label(frame: &Frame) -> String {
    let line = frame
        .location
        .line
        .map(|l| format!(":{l}"))
        .unwrap_or_default();
    format!("{}{line}", unit_label(&frame.location.unit))
}

fn variable_ui(ui: &mut Ui, var: &Variable, index: usize) {
    let value = match &var.value {
        Some(v) => RichText::new(v).monospace(),
        None => RichText::new("NULL")
            .monospace()
            .italics()
            .color(ui.visuals().weak_text_color()),
    };
    if var.children.is_empty() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(var.name.to_lowercase()).monospace().strong());
            ui.label(value);
            ui.weak(&var.type_name);
        });
    } else {
        let summary = var
            .value
            .as_deref()
            .map(|v| format!("  {v}"))
            .unwrap_or_default();
        let header = format!("{}  {}{summary}", var.name.to_lowercase(), var.type_name);
        egui::CollapsingHeader::new(RichText::new(header).monospace())
            .id_salt(("var", index, &var.name))
            .show(ui, |ui| {
                for (i, child) in var.children.iter().enumerate() {
                    variable_ui(ui, child, i);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::UnitType;

    #[test]
    fn markers_follow_breakpoints() {
        let mut s = DebugSession::default();
        let unit = Unit::new("ORUI", "P", UnitType::Procedure);
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
}
