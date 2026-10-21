//! Renders the debugger panel offscreen for visual review.
//! `ORUI_SCREENSHOT_DIR=<dir> cargo test -p orui-debugger --test panel_screenshot`

use egui_kittest::Harness;
use orui_debugger::ui::{DebugSession, State};
use orui_debugger::{Frame, Location, StopReason, Unit, UnitType, Variable};

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
    let Some(dir) = std::env::var_os("ORUI_SCREENSHOT_DIR") else {
        return;
    };
    let demo = Unit::new("ORUI", "DBG_DEMO", UnitType::Procedure);
    let mut session = DebugSession::default();
    session.toggle_breakpoint(demo.clone(), 10);
    session.toggle_breakpoint(Unit::new("ORUI", "DBG_PKG", UnitType::PackageBody), 5);
    session.state = State::Suspended {
        reason: StopReason::Breakpoint,
    };
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
                unit: Unit::new("ORUI", "B1", UnitType::Block),
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
