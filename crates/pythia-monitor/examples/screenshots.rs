//! Renders the monitor views against a live database to PNG files, for
//! reviewing the visual design without a display.
//!
//! ```sh
//! PYTHIA_MONITOR_DB=system/oracle@localhost:1521/FREEPDB1 \
//!   cargo run -p pythia-monitor --example screenshots -- /tmp/out
//! ```

use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use pythia_db::oracle::Connection;
use pythia_monitor::{Monitor, MonitorRequest, MonitorTab};

struct State {
    monitor: Monitor,
    pending: Vec<MonitorRequest>,
}

fn connect(spec: &str) -> Connection {
    let (cred, db) = spec.rsplit_once('@').expect("user/password@db");
    let (u, p) = cred.split_once('/').expect("user/password@db");
    Connection::connect(u, p, db).expect("connect")
}

/// Steps the UI and runs requested tasks until nothing is pending.
fn settle(harness: &mut Harness<'_, State>, conn: &Connection, start: Instant) {
    for _ in 0..20 {
        harness.step();
        let pending = std::mem::take(&mut harness.state_mut().pending);
        if pending.is_empty() {
            continue;
        }
        for req in pending {
            let result = (req.task)(conn);
            let now = start.elapsed().as_secs_f64();
            harness
                .state_mut()
                .monitor
                .on_task_done(req.token, result, now);
        }
    }
    harness.step();
}

fn main() {
    let spec = std::env::var("PYTHIA_MONITOR_DB")
        .unwrap_or_else(|_| "system/oracle@localhost:1521/FREEPDB1".into());
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| std::env::temp_dir().display().to_string());
    std::fs::create_dir_all(&out).unwrap();
    let conn = connect(&spec);

    // A small blocking chain so the blocking tree has something to show.
    let _ = conn.execute("drop table pythia_shot_lock purge", &[]);
    conn.execute(
        "create table pythia_shot_lock (id number primary key, v number)",
        &[],
    )
    .unwrap();
    conn.execute("insert into pythia_shot_lock values (1, 0)", &[])
        .unwrap();
    conn.commit().unwrap();
    let holder = connect(&spec);
    holder
        .execute("update pythia_shot_lock set v = 1 where id = 1", &[])
        .unwrap();
    let mut waiters = Vec::new();
    for _ in 0..2 {
        let c = connect(&spec);
        let (tx, rx) = mpsc::channel::<()>();
        waiters.push((
            tx,
            thread::spawn(move || {
                let _ = c.execute("update pythia_shot_lock set v = 2 where id = 1", &[]);
                let _ = rx.recv();
                let _ = c.rollback();
            }),
        ));
    }
    thread::sleep(Duration::from_secs(2));

    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        let start = Instant::now();
        let mut monitor = Monitor::default();
        monitor.trace.load_text(
            "free23_10046.trc",
            include_str!("../tests/fixtures/free23_10046.trc"),
        );
        let state = State {
            monitor,
            pending: Vec::new(),
        };
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1440.0, 960.0))
            .with_theme(theme)
            .with_step_dt(0.5)
            .wgpu()
            .build_ui_state(
                |ui, state: &mut State| {
                    egui::CentralPanel::default().show(ui, |ui| {
                        let out = state.monitor.ui(ui);
                        state.pending.extend(out.requests);
                    });
                },
                state,
            );

        // Several dashboard samples with some load in between.
        harness.state_mut().monitor.dashboard.refresh = pythia_monitor::ui::Refresh::S2;
        for round in 0..6 {
            for _ in 0..(200 * (round + 1)) {
                let _ = conn
                    .query_row_as::<i64>("select count(*) from all_objects where rownum < 50", &[]);
            }
            settle(&mut harness, &conn, start);
            for _ in 0..4 {
                harness.step();
            }
            thread::sleep(Duration::from_millis(700));
        }
        settle(&mut harness, &conn, start);
        let name = if theme == egui::Theme::Dark {
            "dark"
        } else {
            "light"
        };
        let shots: [(MonitorTab, &str); 4] = [
            (MonitorTab::Dashboard, "dashboard"),
            (MonitorTab::Sessions, "sessions"),
            (MonitorTab::Awr, "awr"),
            (MonitorTab::Trace, "trace"),
        ];
        for (tab, label) in shots {
            harness.state_mut().monitor.set_tab(tab);
            settle(&mut harness, &conn, start);
            let image = harness.render().expect("render");
            let path = format!("{out}/monitor-{label}-{name}.png");
            image.save(&path).unwrap();
            println!("{path}");
        }
        // Blocking tree and a session detail.
        harness.state_mut().monitor.set_tab(MonitorTab::Sessions);
        harness.state_mut().monitor.sessions.show_blocking();
        settle(&mut harness, &conn, start);
        let image = harness.render().expect("render");
        let path = format!("{out}/monitor-blocking-{name}.png");
        image.save(&path).unwrap();
        println!("{path}");

        let holder_sid: i64 = holder
            .query_row_as(
                "select to_number(sys_context('USERENV', 'SID')) from dual",
                &[],
            )
            .unwrap();
        let serial: i64 = conn
            .query_row_as(
                "select serial# from v$session where sid = :1",
                &[&holder_sid],
            )
            .unwrap();
        harness.state_mut().monitor.open_session(holder_sid, serial);
        settle(&mut harness, &conn, start);
        let image = harness.render().expect("render");
        let path = format!("{out}/monitor-detail-{name}.png");
        image.save(&path).unwrap();
        println!("{path}");
    }

    holder.rollback().unwrap();
    for (tx, handle) in waiters {
        let _ = tx.send(());
        handle.join().unwrap();
    }
    conn.execute("drop table pythia_shot_lock purge", &[])
        .unwrap();
}
