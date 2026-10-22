//! Pythia – a fast, native Oracle PL/SQL IDE.

// Release builds are GUI programs on Windows, without a console window;
// debug builds keep it for development.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod ai;
mod app;
mod assist;
mod browser;
mod catalog;
mod chart_view;
mod commands;
mod compare_window;
mod connections;
mod data_edit;
mod data_search;
mod desktop;
mod dialogs;
mod documents;
mod editor;
mod editor_menu;
mod export_tables;
mod git;
mod history;
mod icons_ext;
mod import_window;
mod invalid_window;
mod jobs_window;
mod monitor_window;
mod object_editor;
mod object_files;
mod object_view;
mod plan_view;
mod profiler;
mod program_window;
mod project;
mod relations;
mod reports;
#[cfg(test)]
mod screenshots;
mod secrets;
mod settings_dialog;
mod source_search;
mod sql_window;
mod statistics;
mod stubs;
mod table_editor;
mod table_partitions;
mod table_parts;
mod test_script;
mod text_diff;
mod text_file;
mod theme;
mod to_file;

use std::fs::{self, File};
use std::sync::Mutex;

use pythia_config::Paths;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn main() -> anyhow::Result<()> {
    attach_parent_console();
    let paths = Paths::detect();
    // Before logging creates the new directories.
    let moved = paths.as_ref().map(Paths::adopt_legacy).unwrap_or_default();
    let paths = match paths {
        Ok(paths) => Some(paths),
        Err(e) => {
            startup_error(&format!(
                "Pythia could not find its settings folder ({e:#}). It starts without saved settings and connections."
            ));
            None
        }
    };
    init_logging(paths.as_ref());
    install_panic_hook(paths.as_ref());
    info!(version = env!("CARGO_PKG_VERSION"), "starting Pythia");
    for problem in moved {
        warn!("settings from before the rename to Pythia: {problem}");
    }

    let result = run(eframe::Renderer::Wgpu, paths.clone()).or_else(|e| {
        warn!("wgpu renderer failed ({e}), falling back to OpenGL");
        run(eframe::Renderer::Glow, paths.clone())
    });
    if let Err(e) = &result {
        let log = paths
            .as_ref()
            .map(|p| {
                format!(
                    "\n\nDetails are in {}",
                    p.log_dir().join("pythia.log").display()
                )
            })
            .unwrap_or_default();
        startup_error(&format!(
            "Pythia could not open its window: neither Vulkan/DirectX/Metal nor OpenGL works here ({e}).{log}"
        ));
    }
    result.map_err(|e| anyhow::anyhow!("{e}"))
}

/// Reports a problem at startup where it's seen: there may be no console
/// (a GUI program on Windows, or started from the desktop), so also in a
/// message box.
fn startup_error(text: &str) {
    error!("{text}");
    eprintln!("pythia: {text}");
    let _ = rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Pythia")
        .set_description(text)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
}

/// Started from a terminal, a Windows GUI program has no console of its own;
/// attaching to the terminal's shows the log output there.
fn attach_parent_console() {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        unsafe extern "system" {
            fn AttachConsole(process_id: u32) -> i32;
        }
        const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
        // Fails when there's no parent console, e.g. started from Explorer.
        unsafe {
            AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

fn run(renderer: eframe::Renderer, paths: Option<Paths>) -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("Pythia")
            .with_app_id("pythia")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0])
            .with_icon(window_icon()),
        persistence_path: paths.as_ref().map(|p| p.data_dir.join("workspace.ron")),
        ..Default::default()
    };
    eframe::run_native(
        "pythia",
        options,
        Box::new(move |cc| Ok(Box::new(app::PythiaApp::new(cc, paths)))),
    )
}

/// The application icon, embedded at build time.
fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/icon/pythia-256.png"))
        .expect("embedded icon is a valid PNG")
}

/// Logs to stderr and to `<data dir>/logs/pythia.log`. `RUST_LOG` overrides the filter.
fn init_logging(paths: Option<&Paths>) {
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn,eframe=warn"))
    };
    let file = paths.and_then(|p| {
        let dir = p.log_dir();
        fs::create_dir_all(&dir).ok()?;
        File::create(dir.join("pythia.log")).ok()
    });
    let file_layer = file.map(|f| {
        fmt::layer()
            .with_ansi(false)
            .with_writer(Mutex::new(f))
            .with_filter(filter())
    });
    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_writer(std::io::stderr)
                .with_filter(filter()),
        )
        .with(file_layer)
        .init();
}

/// Logs panics and writes a crash report next to the log file.
fn install_panic_hook(paths: Option<&Paths>) {
    let crash_dir = paths.map(Paths::log_dir);
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        error!("panic: {panic}\n{backtrace}");
        if let Some(dir) = &crash_dir {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            let _ = fs::write(
                dir.join(format!("crash-{stamp}.txt")),
                format!("{panic}\n\n{backtrace}"),
            );
        }
        default_hook(panic);
    }));
}
