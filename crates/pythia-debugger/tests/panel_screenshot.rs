//! Renders the debugger panel offscreen for visual review.
//! `PYTHIA_SCREENSHOT_DIR=<dir> cargo test -p pythia-debugger --test panel_screenshot`

use egui_kittest::Harness;
use pythia_debugger::ui::{DebugSession, State};
use pythia_debugger::{
    Breakpoint, BreakpointOptions, Frame, Location, StopReason, Unit, UnitType, Variable, Watch,
};

fn var(name: &str, ty: &str, value: Option<&str>, children: Vec<Variable>) -> Variable {
    Variable {
        name: name.into(),
        type_name: ty.into(),
        value: value.map(Into::into),
        children,
    }
}

#[test]
fn panel() {
    let Some(dir) = std::env::var_os("PYTHIA_SCREENSHOT_DIR") else {
        return;
    };
    let demo = Unit::new("PYTHIA", "DBG_DEMO", UnitType::Procedure);
    let mut session = DebugSession::default();
    let pkg = Unit::new("PYTHIA", "DBG_PKG", UnitType::PackageBody);
    session.toggle_breakpoint(demo.clone(), 10);
    session.toggle_breakpoint(demo.clone(), 13);
    session.toggle_breakpoint(pkg.clone(), 5);
    let at10 = Breakpoint {
        unit: demo.clone(),
        line: 10,
    };
    session.set_options(
        &at10,
        BreakpointOptions {
            condition: Some("i = 2 and l_name is not null".into()),
            hit_count: Some(2),
        },
    );
    session.hits.insert(at10, 2);
    session.set_enabled(&Breakpoint { unit: pkg, line: 5 }, false);
    session.watches = vec![
        "l_rec.b".into(),
        "dbg_pkg.g_count".into(),
        "l_arr".into(),
        "l_missing".into(),
    ];
    session.watch_values = vec![
        Watch {
            expression: "l_rec.b".into(),
            value: Ok(var("B", "VARCHAR2", Some("bee"), vec![])),
        },
        Watch {
            expression: "dbg_pkg.g_count".into(),
            value: Ok(var("G_COUNT", "NUMBER", Some("41"), vec![])),
        },
        Watch {
            expression: "l_arr".into(),
            value: Ok(var(
                "L_ARR",
                "T_ARR",
                Some("3 element(s)"),
                vec![
                    var("(1)", "NUMBER", Some("10"), vec![]),
                    var("(2)", "NUMBER", Some("20"), vec![]),
                ],
            )),
        },
        Watch {
            expression: "l_missing".into(),
            value: Err("l_missing is unknown here".into()),
        },
    ];
    session.state = State::Suspended {
        reason: StopReason::Exception,
    };
    session.exception = Some("ORA-01476 (zero_divide)".into());
    session.break_on_exception = true;
    session.stack = vec![
        Frame {
            location: Location {
                unit: demo.clone(),
                line: Some(10),
            },
            method: "DBG_DEMO".into(),
        },
        Frame {
            location: Location {
                unit: Unit::new("PYTHIA", "B1", UnitType::Block),
                line: None,
            },
            method: "<procedure$1>".into(),
        },
    ];
    session.variables = vec![
        var("P_N", "NUMBER", Some("3"), vec![]),
        var("L_TOTAL", "NUMBER", Some("1"), vec![]),
        var("L_NAME", "VARCHAR2", Some("step 1"), vec![]),
        var("L_FLAG", "BOOLEAN", Some("true"), vec![]),
        var("L_DATE", "DATE", Some("29-FEB-24 12.00.00 AM"), vec![]),
        var("L_NOTE", "VARCHAR2", None, vec![]),
        var(
            "L_ARR",
            "T_ARR",
            Some("3 element(s)"),
            vec![
                var("(1)", "NUMBER", Some("10"), vec![]),
                var("(2)", "NUMBER", Some("20"), vec![]),
                var("(3)", "NUMBER", Some("30"), vec![]),
            ],
        ),
        var(
            "L_REC",
            "T_REC",
            None,
            vec![
                var("A", "NUMBER", Some("7"), vec![]),
                var("B", "VARCHAR2", Some("bee"), vec![]),
            ],
        ),
    ];
    for dark in [true, false] {
        let mut s = DebugSession::default();
        s.breakpoints = session.breakpoints.clone();
        s.disabled = session.disabled.clone();
        s.options = session.options.clone();
        s.hits = session.hits.clone();
        s.watches = session.watches.clone();
        s.watch_values = session.watch_values.clone();
        s.exception = session.exception.clone();
        s.break_on_exception = session.break_on_exception;
        s.state = session.state.clone();
        s.stack = session.stack.clone();
        s.variables = session.variables.clone();
        let mut harness = Harness::builder()
            .with_size(egui::vec2(1100.0, 320.0))
            .with_pixels_per_point(1.5)
            .wgpu()
            .build_ui(move |ui| {
                egui::Frame::central_panel(ui.style()).show(ui, |ui| {
                    s.ui(ui);
                });
            });
        harness.ctx.set_theme(if dark {
            egui::Theme::Dark
        } else {
            egui::Theme::Light
        });
        harness.run_steps(4);
        let image = harness.render().expect("render");
        let path = std::path::Path::new(&dir).join(format!(
            "debugger-{}.png",
            if dark { "dark" } else { "light" }
        ));
        image.save(&path).unwrap();
        eprintln!("wrote {}", path.display());
    }
}
