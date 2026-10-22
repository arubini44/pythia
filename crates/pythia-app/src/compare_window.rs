//! Schema and data compare documents (`pythia_compare` views).
//!
//! A compare talks to two connections; its tasks run on this document's own
//! session on each of them, so a long data compare never blocks other work.

use egui::Ui;
use pythia_compare::ui::{CompareAction, CompareOutput, DataCompare, SchemaCompare};
use pythia_db::{DbEvent, DbRequest};
use serde::{Deserialize, Serialize};

use crate::documents::{DocContext, Effects, OpenRequest, Target};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareKind {
    Schema,
    Data,
}

#[derive(Serialize, Deserialize)]
pub struct CompareWindow {
    pub kind: CompareKind,
    #[serde(skip)]
    view: Option<View>,
}

enum View {
    Schema(Box<SchemaCompare>),
    Data(Box<DataCompare>),
}

impl CompareWindow {
    pub fn new(kind: CompareKind, connection: Option<String>) -> Self {
        let mut window = Self { kind, view: None };
        let view = window.view();
        // Start with the current connection on both sides.
        match view {
            View::Schema(v) => {
                v.source.connection = connection.clone();
                v.target.connection = connection;
            }
            View::Data(v) => {
                v.source.connection = connection.clone();
                v.target.connection = connection;
            }
        }
        window
    }

    fn view(&mut self) -> &mut View {
        let kind = self.kind;
        self.view.get_or_insert_with(|| match kind {
            CompareKind::Schema => View::Schema(Box::new(SchemaCompare::new())),
            CompareKind::Data => View::Data(Box::new(DataCompare::new())),
        })
    }

    pub fn title(&self) -> String {
        match &self.view {
            Some(View::Schema(v)) => v.title(),
            Some(View::Data(v)) => v.title(),
            None => match self.kind {
                CompareKind::Schema => "Schema compare".into(),
                CompareKind::Data => "Data compare".into(),
            },
        }
    }

    pub fn is_busy(&self) -> bool {
        match &self.view {
            Some(View::Schema(v)) => v.is_busy(),
            Some(View::Data(v)) => v.is_busy(),
            None => false,
        }
    }

    pub fn on_event(&mut self, event: DbEvent) -> Effects {
        let mut effects = Effects::default();
        let DbEvent::TaskDone { id, result } = event else {
            return effects;
        };
        match self.view() {
            View::Schema(v) => {
                v.on_task_done(id, result);
            }
            View::Data(v) => {
                let (_, follow_up) = v.on_task_done(id, result);
                forward(
                    CompareOutput {
                        requests: follow_up,
                        actions: Vec::new(),
                    },
                    &mut effects,
                );
            }
        }
        effects
    }

    pub fn ui(&mut self, ui: &mut Ui, cx: &DocContext) -> Effects {
        let mut effects = Effects::default();
        let out = match self.view() {
            View::Schema(v) => v.ui(ui, cx.connections),
            View::Data(v) => v.ui(ui, cx.connections),
        };
        forward(out, &mut effects);
        effects
    }
}

fn forward(out: CompareOutput, effects: &mut Effects) {
    for request in out.requests {
        effects.request(
            Target::Connection(request.connection),
            DbRequest::Task {
                id: request.id,
                task: request.task,
            },
        );
    }
    for action in out.actions {
        match action {
            CompareAction::OpenSql { connection, text } => {
                effects.open.push(OpenRequest::Sql {
                    connection,
                    text,
                    run: false,
                });
            }
        }
    }
}
