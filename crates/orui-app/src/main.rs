//! orui – a fast, native Oracle PL/SQL IDE.

mod app;
mod assist;
mod browser;
mod catalog;
mod commands;
mod connections;
mod data_edit;
mod dialogs;
mod documents;
mod editor;
mod history;
mod monitor_window;
mod object_view;
mod program_window;
#[cfg(test)]
mod screenshots;
mod secrets;
mod sql_window;
mod theme;

use std::fs::{self, File};
use std::sync::Mutex;

use orui_config::Paths;
use tracing::{error, info, warn};
use tracing_subscriber::{EnvFilter, fmt, prelude::*};

fn main() -> anyhow::Result<()> {
    let paths = Paths::detect()
        .inspect_err(|e| eprintln!("orui: {e:#}"))
        .ok();
    init_logging(paths.as_ref());
    install_panic_hook(paths.as_ref());
    info!(version = env!("CARGO_PKG_VERSION"), "starting orui");

    match run(eframe::Renderer::Wgpu, paths.clone()) {
        Ok(()) => Ok(()),
        Err(e) => {
            warn!("wgpu renderer failed ({e}), falling back to OpenGL");
            run(eframe::Renderer::Glow, paths).map_err(|e| anyhow::anyhow!("{e}"))
        }
    }
}

fn run(renderer: eframe::Renderer, paths: Option<Paths>) -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer,
        viewport: egui::ViewportBuilder::default()
            .with_title("orui")
            .with_app_id("orui")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 400.0]),
        persistence_path: paths.as_ref().map(|p| p.data_dir.join("workspace.ron")),
        ..Default::default()
    };
    eframe::run_native(
        "orui",
        options,
        Box::new(move |cc| Ok(Box::new(app::OruiApp::new(cc, paths)))),
    )
}

/// Logs to stderr and to `<data dir>/logs/orui.log`. `RUST_LOG` overrides the filter.
fn init_logging(paths: Option<&Paths>) {
    let filter = || {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,wgpu=warn,naga=warn,eframe=warn"))
    };
    let file = paths.and_then(|p| {
        let dir = p.log_dir();
        fs::create_dir_all(&dir).ok()?;
        File::create(dir.join("orui.log")).ok()
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
