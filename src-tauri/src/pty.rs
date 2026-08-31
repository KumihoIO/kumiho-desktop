//! Embedded onboarding terminal.
//!
//! One real PTY (ConPTY on Windows, OpenPTY elsewhere via portable-pty) runs
//! `revka onboard` directly, so the wizard renders and reads input exactly as
//! it does in a user-opened terminal. Running the CLI directly matters: an
//! interactive shell would stay alive after the wizard and hide the only
//! reliable completion signal Revka currently exposes — its process exit.
//! Output streams to the webview through a Tauri channel; the reader thread
//! reassembles split UTF-8 sequences so Korean text survives chunk boundaries.

use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::io::{Read, Write};
use std::path::Path;
use tauri::ipc::Channel;

const INITIAL_ROWS: u16 = 24;
const INITIAL_COLS: u16 = 80;

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PtyEvent {
    Data { data: String },
    Exit { success: bool },
}

pub struct PtySession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
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

/// Spawn a command directly in a fresh PTY, stream its output through
/// `on_data`, and register a split-out killer in `state`. Output draining and
/// process waiting use separate threads: on Windows the retained ConPTY master
/// can prevent EOF until Desktop handles Exit and drops the session.
pub fn spawn_command_session(
    state: &crate::AppState,
    program: &Path,
    args: &[&str],
    on_data: Channel<PtyEvent>,
) -> Result<(), String> {
    stop_session(state)?;
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("could not open a pseudo terminal: {e}"))?;

    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("could not start {}: {e}", program.display()))?;
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

    let killer = child.clone_killer();
    {
        let mut guard = state.revka_pty.lock().map_err(|e| e.to_string())?;
        *guard = Some(PtySession {
            writer,
            master: pair.master,
            killer,
        });
    }

    let exit_events = on_data.clone();
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
    });
    std::thread::spawn(move || {
        let success = child.wait().is_ok_and(|status| status.success());
        let _ = exit_events.send(PtyEvent::Exit { success });
    });
    Ok(())
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
        let _ = session.killer.kill();
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
