//! Standalone monitor window for trying the views.
//!
//! ```sh
//! PYTHIA_MONITOR_DB=system/oracle@localhost:1521/FREEPDB1 cargo run -p pythia-monitor --example monitor
//! ```

use std::collections::HashMap;

use pythia_config::Privilege;
use pythia_db::{ConnectParams, DbEvent, DbRequest, Session};
use pythia_monitor::{Monitor, MonitorAction, MonitorTask, Token};

struct App {
    session: Session,
    monitor: Monitor,
    /// Request id → monitor token.
    tasks: HashMap<u64, Token>,
    next_id: u64,
    status: String,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let now = ui.input(|i| i.time);
        while let Some(event) = self.session.try_recv() {
            match event {
                DbEvent::Connected(info) => {
                    self.status = format!("Connected · Oracle {}", info.version)
                }
                DbEvent::ConnectFailed(e) => self.status = format!("Connect failed: {e}"),
                DbEvent::TaskDone { id, result } => {
                    if let Some(token) = self.tasks.remove(&id) {
                        self.monitor.on_task_done(token, result, now);
                    }
                }
                _ => {}
            }
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.weak(&self.status);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            let out = self.monitor.ui(ui);
            for req in out.requests {
                let id = self.next_id;
                self.next_id += 1;
                self.tasks.insert(id, req.token);
                self.session.send(match req.task {
                    MonitorTask::Oracle(task) => DbRequest::Task { id, task },
                    MonitorTask::Client(task) => DbRequest::Run { id, task },
                });
            }
            for action in out.actions {
                match action {
                    MonitorAction::OpenPath(path) => {
                        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
                    }
                    MonitorAction::CopyText(text) => ui.ctx().copy_text(text),
                    // No assistant here (analyzing isn't offered).
                    MonitorAction::Analyze(_) => {}
                }
            }
        });
    }
}

fn main() -> eframe::Result {
    let spec = std::env::var("PYTHIA_MONITOR_DB")
        .unwrap_or_else(|_| "system/oracle@localhost:1521/FREEPDB1".into());
    let (cred, db) = spec.rsplit_once('@').expect("user/password@connect_string");
    let (user, pass) = cred.split_once('/').expect("user/password@connect_string");
    let params = ConnectParams {
        kind: Default::default(),
        username: user.into(),
        password: pass.into(),
        connect_string: db.into(),
        privilege: Privilege::Normal,
        init_sql: Vec::new(),
        ssh: None,
    };
    eframe::run_native(
        "Pythia monitor",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1280.0, 860.0]),
            ..Default::default()
        },
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            let session = Session::connect(params, move || ctx.request_repaint());
            Ok(Box::new(App {
                session,
                monitor: Monitor::default(),
                tasks: HashMap::new(),
                next_id: 1,
                status: "Connecting…".into(),
            }))
        }),
    )
}
