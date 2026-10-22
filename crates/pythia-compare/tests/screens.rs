//! Drives the compare views like a host app would (running their tasks on
//! real connections) and renders them offscreen to PNGs for review.
//!
//! Needs `PYTHIA_TEST_DB_DBA` (see `roundtrip.rs`) and `PYTHIA_SCREENSHOT_DIR`.

mod common;

use std::collections::VecDeque;
use std::path::PathBuf;

use common::{connect, setup, teardown};
use egui_kittest::Harness;
use pythia_compare::ui::{CompareRequest, DataCompare, Endpoint, SchemaCompare};
use pythia_db::oracle::Connection;

struct Host {
    schema: SchemaCompare,
    data: DataCompare,
    show_data: bool,
    /// (issued by the data view?, request) — hosts route results by id.
    queue: VecDeque<(bool, CompareRequest)>,
}

const CONNECTIONS: [&str; 2] = ["dev@FREEPDB1", "test@FREEPDB1"];

fn harness(dark: bool) -> Harness<'static, Host> {
    let host = Host {
        schema: SchemaCompare::new(),
        data: DataCompare::new(),
        show_data: false,
        queue: VecDeque::new(),
    };
    let h = Harness::builder()
        .with_size(egui::vec2(1400.0, 860.0))
        .with_pixels_per_point(1.5)
        .wgpu()
        .build_ui_state(
            |ui, host: &mut Host| {
                pythia_ui::icons::install(ui.ctx());
                let connections: Vec<String> = CONNECTIONS.iter().map(|c| c.to_string()).collect();
                egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                    let data = host.show_data;
                    let out = if data {
                        host.data.ui(ui, &connections)
                    } else {
                        host.schema.ui(ui, &connections)
                    };
                    host.queue
                        .extend(out.requests.into_iter().map(|r| (data, r)));
                });
            },
            host,
        );
    h.ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });
    h
}

/// Runs queued tasks on the "connections" until the views are idle.
fn pump(h: &mut Harness<'_, Host>, conns: &[Connection; 2]) {
    for _ in 0..10_000 {
        h.run_steps(1);
        let Some((data, req)) = h.state_mut().queue.pop_front() else {
            if h.state().schema.is_busy() || h.state().data.is_busy() {
                continue;
            }
            return;
        };
        let conn = &conns[CONNECTIONS
            .iter()
            .position(|c| *c == req.connection)
            .expect("known connection")];
        let result = (req.task)(conn);
        let host = h.state_mut();
        if data {
            let (handled, follow_up) = host.data.on_task_done(req.id, result);
            assert!(handled);
            host.queue.extend(follow_up.into_iter().map(|r| (true, r)));
        } else {
            assert!(host.schema.on_task_done(req.id, result));
        }
    }
    panic!("views never became idle");
}

fn save(h: &mut Harness<'_, Host>, name: &str) {
    let dir = PathBuf::from(std::env::var("PYTHIA_SCREENSHOT_DIR").unwrap());
    std::fs::create_dir_all(&dir).unwrap();
    h.run_steps(3);
    let image = h.render().expect("render");
    let path = dir.join(format!("{name}.png"));
    image.save(&path).unwrap();
    eprintln!("wrote {}", path.display());
}

#[test]
fn compare_screens() {
    let (Some(admin), Some(c1), Some(c2)) = (connect(), connect(), connect()) else {
        eprintln!("PYTHIA_TEST_DB_DBA not set, skipping");
        return;
    };
    if std::env::var_os("PYTHIA_SCREENSHOT_DIR").is_none() {
        eprintln!("PYTHIA_SCREENSHOT_DIR not set, skipping");
        return;
    }
    setup(&admin);
    let conns = [c1, c2];
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        for dark in [true, false] {
            let suffix = if dark { "dark" } else { "light" };
            let mut h = harness(dark);
            pump(&mut h, &conns);
            save(&mut h, &format!("compare-01-setup-{suffix}"));

            let host = h.state_mut();
            host.schema.source = Endpoint {
                connection: Some(CONNECTIONS[0].into()),
                schema: "PYTHIA_CMP_A".into(),
            };
            host.schema.target = Endpoint {
                connection: Some(CONNECTIONS[1].into()),
                schema: "PYTHIA_CMP_B".into(),
            };
            let out = host.schema.compare();
            host.queue
                .extend(out.requests.into_iter().map(|r| (false, r)));
            pump(&mut h, &conns);
            let diff = h.state().schema.result().expect("schema compare finished");
            assert!(!diff.is_equal());
            assert!(diff.errors.is_empty(), "{:?}", diff.errors);

            h.state_mut().schema.select_object("EMP");
            save(&mut h, &format!("compare-02-schema-table-{suffix}"));
            h.state_mut().schema.select_object("PKG");
            save(&mut h, &format!("compare-03-schema-code-{suffix}"));
            h.state_mut().schema.show_script(true);
            save(&mut h, &format!("compare-04-schema-script-{suffix}"));

            let host = h.state_mut();
            host.show_data = true;
            host.data.source = Endpoint {
                connection: Some(CONNECTIONS[0].into()),
                schema: "PYTHIA_CMP_A".into(),
            };
            host.data.target = Endpoint {
                connection: Some(CONNECTIONS[1].into()),
                schema: "PYTHIA_CMP_B".into(),
            };
            host.data.source_table = "DEPT".into();
            host.data.options.chunk_size = 2;
            let out = host.data.compare();
            host.queue
                .extend(out.requests.into_iter().map(|r| (true, r)));
            pump(&mut h, &conns);
            let d = h.state().data.result().expect("data compare finished");
            assert_eq!(
                (d.equal, d.changed, d.only_in_source, d.only_in_target),
                (1, 1, 1, 1)
            );
            save(&mut h, &format!("compare-05-data-{suffix}"));
        }
    }));
    teardown(&admin);
    if let Err(e) = result {
        std::panic::resume_unwind(e);
    }
}
