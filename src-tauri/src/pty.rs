//! Embedded onboarding terminal.
//!
//! One real PTY (ConPTY on Windows, OpenPTY elsewhere via portable-pty) runs
//! the host's interactive shell — detected per OS — and Desktop "types"
//! `"<revka> onboard"` into it, so the wizard renders and reads input exactly
//! as it does in a user-opened terminal. Output streams to the webview through
//! a Tauri channel; the reader thread reassembles split UTF-8 sequences so
//! Korean text survives chunk boundaries.

use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use tauri::ipc::Channel;

const INITIAL_ROWS: u16 = 24;
const INITIAL_COLS: u16 = 80;

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PtyEvent {
    Data { data: String },
    Exit,
}

pub struct PtySession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
}

impl PtySession {
    pub fn write(&mut self, data: &str) -> Result<(), String> {
        self.writer
            .write_all(data.as_bytes())
            .map_err(|e| format!("could not write to the terminal: {e}"))?;
        self.writer
            .flush()
            .map_err(|e| format!("could not write to the terminal: {e}"))
    }

    pub fn resize(&self, rows: u16, cols: u16) -> Result<(), String> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| format!("could not resize the terminal: {e}"))
    }
}

/// Which shell the onboarding terminal will run. The label is shown in the UI;
/// the PowerShell flag decides how a quoted command must be invoked.
#[derive(Serialize)]
pub struct ShellChoice {
    pub label: String,
    /// PowerShell parses `"exe" args` in expression mode and rejects it —
    /// such commands need the `&` call operator.
    #[serde(skip)]
    pub power_shell: bool,
}

fn find_in_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    std::env::split_paths(&path_var)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

fn windows_shell() -> Option<(String, String, Vec<String>, bool)> {
    if let Some(pwsh) = find_in_path("pwsh.exe") {
        return Some((
            "PowerShell 7".into(),
            pwsh.to_string_lossy().into_owned(),
            vec!["-NoLogo".into()],
            true,
        ));
    }
    let system_root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
    let powershell = Path::new(&system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    if powershell.is_file() {
        return Some((
            "Windows PowerShell".into(),
            powershell.to_string_lossy().into_owned(),
            vec!["-NoLogo".into()],
            true,
        ));
    }
    if let Some(comspec) = std::env::var_os("ComSpec") {
        // Some wrappers store ComSpec with surrounding quotes; strip them so
        // the path actually resolves.
        let trimmed = comspec.to_string_lossy().trim_matches('"').to_string();
        if Path::new(&trimmed).is_file() {
            return Some(("Command Prompt".into(), trimmed, Vec::new(), false));
        }
    }
    None
}

#[cfg(not(windows))]
fn unix_shell() -> Option<(String, String, Vec<String>, bool)> {
    if let Some(shell) = std::env::var_os("SHELL") {
        let shell = PathBuf::from(shell);
        if shell.is_file() {
            let label = shell
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "shell".into());
            return Some((
                label,
                shell.to_string_lossy().into_owned(),
                Vec::new(),
                false,
            ));
        }
    }
    for candidate in ["/bin/bash", "/bin/sh"] {
        if Path::new(candidate).is_file() {
            let label = Path::new(candidate)
                .file_stem()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "shell".into());
            return Some((label, candidate.into(), Vec::new(), false));
        }
    }
    None
}

fn detect_shell() -> Result<(String, String, Vec<String>, bool), String> {
    #[cfg(windows)]
    {
        windows_shell().ok_or_else(|| "no usable shell found on this machine".to_string())
    }
    #[cfg(not(windows))]
    {
        unix_shell().ok_or_else(|| "no usable shell found on this machine".to_string())
    }
}

/// Reassemble streamed bytes into complete UTF-8 text, buffering an incomplete
/// multi-byte tail until its continuation bytes arrive.
struct Utf8Feeder {
    tail: Vec<u8>,
}

impl Utf8Feeder {
    fn new() -> Self {
        Self { tail: Vec::new() }
    }

    fn feed(&mut self, chunk: &[u8]) -> String {
        let mut bytes = std::mem::take(&mut self.tail);
        bytes.extend_from_slice(chunk);
        let mut out = String::new();
        loop {
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    out.push_str(text);
                    return out;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    out.push_str(&String::from_utf8_lossy(&bytes[..valid]));
                    match error.error_len() {
                        // Input ended mid-sequence: keep the tail for next chunk.
                        None => {
                            self.tail = bytes[valid..].to_vec();
                            return out;
                        }
                        // Genuinely invalid sequence: one replacement char,
                        // drop it, and keep decoding what follows.
                        Some(invalid_len) => {
                            out.push('\u{FFFD}');
                            bytes.drain(..valid + invalid_len);
                            if bytes.is_empty() {
                                return out;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Spawn the host shell in a fresh PTY, stream its output through `on_data`,
/// and register the session in `state`. Returns the shell label for the UI.
pub fn spawn_session(
    state: &crate::AppState,
    revka_bin: &Path,
    on_data: Channel<PtyEvent>,
) -> Result<ShellChoice, String> {
    stop_session(state)?;
    let (label, program, args, power_shell) = detect_shell()?;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("could not open a pseudo terminal: {e}"))?;

    let mut cmd = CommandBuilder::new(&program);
    cmd.args(&args);
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }
    // Put Desktop's Revka first on PATH so `revka` resolves inside the
    // terminal even before any User-PATH entry exists.
    if let Some(bin_dir) = revka_bin.parent() {
        let path_key = if cfg!(windows) { "Path" } else { "PATH" };
        let current = std::env::var_os(path_key).unwrap_or_default();
        let mut parts = vec![bin_dir.to_path_buf()];
        parts.extend(std::env::split_paths(&current));
        cmd.env(
            path_key,
            std::env::join_paths(parts).map_err(|e| e.to_string())?,
        );
    }

    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("could not start {label}: {e}"))?;
    // Nothing else needs the slave side; keeping it open would block EOFs.
    drop(pair.slave);

    // From here on the child exists — any failure must kill it explicitly or
    // an unregistered shell outlives the session.
    let mut reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(e) => {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not read from the terminal: {e}"));
        }
    };
    let writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(e) => {
            let mut child = child;
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not write to the terminal: {e}"));
        }
    };

    std::thread::spawn(move || {
        let mut feeder = Utf8Feeder::new();
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(read) => {
                    let text = feeder.feed(&buffer[..read]);
                    if !text.is_empty() && on_data.send(PtyEvent::Data { data: text }).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = on_data.send(PtyEvent::Exit);
    });

    let registration = state.revka_pty.lock().map(|mut guard| {
        *guard = Some(PtySession {
            writer,
            master: pair.master,
            child,
        });
    });
    if let Err(e) = registration {
        return Err(e.to_string());
    }
    Ok(ShellChoice { label, power_shell })
}

/// Type the quoted command into the running shell, as if the user typed it.
pub fn type_command(state: &crate::AppState, command_line: &str) -> Result<(), String> {
    write_input(state, &format!("{command_line}\r"))
}

/// Forward raw keystrokes from the webview terminal widget.
pub fn write_input(state: &crate::AppState, data: &str) -> Result<(), String> {
    let mut guard = state.revka_pty.lock().map_err(|e| e.to_string())?;
    let session = guard.as_mut().ok_or("no terminal session is running")?;
    session.write(data)
}

pub fn resize(state: &crate::AppState, rows: u16, cols: u16) -> Result<(), String> {
    let guard = state.revka_pty.lock().map_err(|e| e.to_string())?;
    let session = guard.as_ref().ok_or("no terminal session is running")?;
    session.resize(rows, cols)
}

pub fn stop_session(state: &crate::AppState) -> Result<(), String> {
    let mut guard = state.revka_pty.lock().map_err(|e| e.to_string())?;
    if let Some(mut session) = guard.take() {
        let _ = session.child.kill();
        let _ = session.child.wait();
        // Dropping the master closes the pty; the reader thread then exits.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Utf8Feeder;

    #[test]
    fn korean_text_split_across_chunks_survives() {
        let mut feeder = Utf8Feeder::new();
        let bytes = "메모리".as_bytes();
        // 3-byte UTF-8 sequences cut at every boundary.
        let mut out = feeder.feed(&bytes[..2]);
        out.push_str(&feeder.feed(&bytes[2..5]));
        out.push_str(&feeder.feed(&bytes[5..]));
        assert_eq!(out, "메모리");
    }

    #[test]
    fn ascii_passes_through_and_invalid_bytes_become_replacements() {
        let mut feeder = Utf8Feeder::new();
        assert_eq!(feeder.feed(b"hello "), "hello ");
        assert_eq!(feeder.feed(b"world"), "world");
        assert_eq!(feeder.feed(&[0xff, b'a']), "\u{FFFD}a");
    }
}
