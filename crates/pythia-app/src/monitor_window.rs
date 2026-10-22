//! Monitoring document: hosts the `pythia_monitor` views on a dedicated session.

use std::collections::HashMap;

use egui::Ui;
use pythia_db::{DbEvent, DbRequest};
use pythia_monitor::{Monitor, MonitorAction, MonitorTab, Token};
use serde::{Deserialize, Serialize};

use crate::documents::{DocContext, Effects, Target, request_id};

/// Which view a monitor document opens with (persisted with the layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum MonitorStart {
    #[default]
    Dashboard,
    Sessions,
    Blocking,
    Awr,
    Trace,
}

#[derive(Serialize, Deserialize)]
pub struct MonitorWindow {
    pub start: MonitorStart,
    #[serde(skip)]
    state: Option<State>,
}

struct State {
    monitor: Monitor,
    /// Request id → monitor token.
    tokens: HashMap<u64, Token>,
    /// egui time of the last frame, handed to the monitor with results.
    now: f64,
}

impl MonitorWindow {
    pub fn new(start: MonitorStart) -> Self {
        Self { start, state: None }
    }

    fn state(&mut self) -> &mut State {
        let start = self.start;
        self.state.get_or_insert_with(|| {
            let mut monitor = Monitor::new(match start {
                MonitorStart::Dashboard => MonitorTab::Dashboard,
                MonitorStart::Sessions | MonitorStart::Blocking => MonitorTab::Sessions,
                MonitorStart::Awr => MonitorTab::Awr,
                MonitorStart::Trace => MonitorTab::Trace,
            });
            if start == MonitorStart::Blocking {
                monitor.sessions.show_blocking();
            }
            State {
                monitor,
                tokens: HashMap::new(),
                now: 0.0,
            }
        })
    }

    pub fn title(&self) -> String {
        self.state
            .as_ref()
            .map_or_else(|| "Monitor".to_owned(), |s| s.monitor.title())
    }

    pub fn is_busy(&self) -> bool {
        self.state.as_ref().is_some_and(|s| !s.tokens.is_empty())
    }

    pub fn on_event(&mut self, event: DbEvent) -> Effects {
        if let DbEvent::TaskDone { id, result } = event {
            let state = self.state();
            if let Some(token) = state.tokens.remove(&id) {
                let now = state.now;
                state.monitor.on_task_done(token, result, now);
            }
        }
        Effects::default()
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        if !cx.connected {
            ui.centered_and_justified(|ui| ui.weak("Connect to see the monitor"));
            return effects;
        }
        let state = self.state();
        state.now = ui.input(|i| i.time);
        let out = state.monitor.ui(ui);
        for request in out.requests {
            let id = request_id();
            state.tokens.insert(id, request.token);
            effects.request(
                Target::Own,
                DbRequest::Task {
                    id,
                    task: request.task,
                },
            );
        }
        for action in out.actions {
            match action {
                MonitorAction::OpenPath(path) => open_path(&path, &mut effects),
                MonitorAction::CopyText(text) => ui.ctx().copy_text(text),
            }
        }
        effects
    }
}

/// Opens a file with the system's default application.
fn open_path(path: &std::path::Path, effects: &mut Effects) {
    if let Err(e) = crate::desktop::open_path(path) {
        effects
            .log
            .push(format!("Could not open {}: {e}", path.display()));
    }
}
