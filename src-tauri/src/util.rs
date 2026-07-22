//! Shared helpers.

use std::path::PathBuf;
use std::process::Command;

/// A Command that never flashes a console window on Windows (CREATE_NO_WINDOW),
/// so the desktop app's shell-outs (kumiho_server, docker, host CLIs) stay quiet.
pub fn command(program: &str) -> Command {
    #[allow(unused_mut)]
    let mut c = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

/// `~/.kumiho`
pub fn kumiho_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".kumiho"))
}

/// The installed CE server binary, if present.
pub fn ce_binary() -> Option<PathBuf> {
    let name = if cfg!(windows) { "kumiho_server.exe" } else { "kumiho_server" };
    let p = kumiho_home()?.join("bin").join(name);
    p.exists().then_some(p)
}
