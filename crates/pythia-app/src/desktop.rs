//! Starting other programs: helper tools and the system's default
//! application for a file or folder.

use std::path::Path;
use std::process::Command;

/// A command for a helper program such as `git`. On Windows it runs without
/// a console window: Pythia is a GUI program there, so every console program
/// it starts would otherwise open one.
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Opens a file or folder with the system's default application.
pub fn open_path(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    let mut open = {
        let mut c = command("cmd");
        c.args(["/C", "start", ""]);
        c
    };
    #[cfg(target_os = "macos")]
    let mut open = command("open");
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let mut open = command("xdg-open");
    open.arg(path).spawn().map(drop)
}
