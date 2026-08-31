//! Embedded onboarding terminal.
//!
//! One real PTY (ConPTY on Windows, OpenPTY elsewhere via portable-pty) runs
//! PowerShell on Windows and the user's default shell on macOS/Linux. The shell
//! executes `revka onboard` and exits with its status, so the wizard behaves as
//! it does in a user-opened terminal without hiding the completion signal.
//! Output streams to the webview through a Tauri channel; the reader thread
//! reassembles split UTF-8 sequences so Korean text survives chunk boundaries.

#[cfg(windows)]
use portable_pty::ChildKiller;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use tauri::ipc::Channel;

const INITIAL_ROWS: u16 = 24;
const INITIAL_COLS: u16 = 80;

#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PtyEvent {
    Data {
        data: String,
    },
    Exit {
        success: bool,
        cleanup_error: Option<String>,
    },
}

pub struct PtySession {
    writer: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
    #[cfg(windows)]
    killer: Box<dyn ChildKiller + Send + Sync>,
    process_tree: Arc<crate::process_tree::ProcessTree>,
    exit_cleanup: Arc<ExitCleanup>,
    exited: Arc<AtomicBool>,
}

#[derive(Default)]
struct ExitCleanupState {
    running: bool,
    error: Option<String>,
}

#[derive(Default)]
struct ExitCleanup {
    state: Mutex<ExitCleanupState>,
    completed: Condvar,
}

impl ExitCleanup {
    /// Publish cleanup as an atomic running → completed transition. A close
    /// racing the PTY wait thread blocks in `wait_error` until the result that
    /// disarmed the Unix PGID is visible.
    fn run(&self, cleanup: impl FnOnce() -> Result<(), String>) -> Option<String> {
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.running = true;
            state.error = None;
        }
        let error = cleanup().err();
        {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.running = false;
            state.error = error.clone();
            self.completed.notify_all();
        }
        error
    }

    fn wait_error(&self) -> Option<String> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.running {
            state = self
                .completed
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.error.clone()
    }
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

struct ShellCommand {
    program: PathBuf,
    args: Vec<OsString>,
    /// Windows starts an idle PowerShell, assigns it to the Job Object, and
    /// only then injects the command. That closes the spawn-before-assignment
    /// race where Revka could otherwise create sidecars outside the job.
    startup_input: Option<&'static str>,
    label: String,
}

#[cfg(any(windows, test))]
fn powershell_shell(shell: PathBuf) -> ShellCommand {
    let executable = shell
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("PowerShell");
    let label = if executable.eq_ignore_ascii_case("pwsh.exe") || executable == "pwsh" {
        "PowerShell 7 · Revka CLI"
    } else {
        "Windows PowerShell · Revka CLI"
    };
    ShellCommand {
        program: shell,
        args: vec!["-NoLogo".into(), "-NoProfile".into()],
        startup_input: Some(
            "& $env:KUMIHO_REVKA_BIN onboard; $code=$LASTEXITCODE; if ($null -eq $code) { $code=1 }; exit $code\r",
        ),
        label: label.into(),
    }
}

#[cfg(any(not(windows), test))]
fn unix_shell(shell: PathBuf) -> ShellCommand {
    let name = shell
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("shell")
        .to_owned();
    let normalized = name.to_ascii_lowercase();
    let (program, command, label) = match normalized.as_str() {
        // Alias expansion happens before parameter expansion, and `exec`
        // receives the absolute path stored in KUMIHO_REVKA_BIN. Consequently
        // neither a `revka` alias/function nor another executable earlier on
        // PATH can intercept the wizard. Replacing the shell also preserves
        // Revka's exit status as the PTY child's exit status.
        "sh" | "bash" | "zsh" | "dash" | "ash" | "ksh" | "ksh93" | "mksh" | "yash" | "fish" => (
            shell,
            "exec \"$KUMIHO_REVKA_BIN\" onboard",
            format!("{name} · Revka CLI"),
        ),
        // Nushell addresses environment values through `$env` but its `exec`
        // command has the same replace-and-forward-exit-status contract on
        // Unix as the POSIX/fish builtins.
        "nu" | "nushell" => (
            shell,
            "exec $env.KUMIHO_REVKA_BIN onboard",
            format!("{name} · Revka CLI"),
        ),
        // An unknown shell may not support either command language or even
        // the conventional `-c` flag. Falling back explicitly is safer than
        // opening a blank terminal or accidentally running a PATH shadow.
        _ => (
            PathBuf::from("/bin/sh"),
            "exec \"$KUMIHO_REVKA_BIN\" onboard",
            format!("sh (fallback from {name}) · Revka CLI"),
        ),
    };
    ShellCommand {
        program,
        args: vec!["-c".into(), command.into()],
        startup_input: None,
        label,
    }
}

#[cfg(windows)]
fn program_on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(name))
            .find(|candidate| candidate.is_file())
    })
}

#[cfg(windows)]
fn platform_shell(_revka: &Path) -> ShellCommand {
    let shell = program_on_path("pwsh.exe")
        .or_else(|| program_on_path("powershell.exe"))
        .unwrap_or_else(|| PathBuf::from("powershell.exe"));
    powershell_shell(shell)
}

#[cfg(not(windows))]
fn platform_shell(_revka: &Path) -> ShellCommand {
    // portable-pty resolves $SHELL, then the user's passwd entry, then /bin/sh.
    // That matters for macOS GUI launches where SHELL may not be inherited.
    let shell = PathBuf::from(CommandBuilder::new_default_prog().get_shell());
    unix_shell(shell)
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
                        None => {
                            self.tail = bytes[valid..].to_vec();
                            return out;
                        }
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

/// Spawn the platform shell in a fresh PTY, stream its output through
/// `on_data`, and register a split-out killer in `state`. Output draining and
/// process waiting use separate threads: on Windows the retained ConPTY master
/// can prevent EOF until Desktop handles Exit and drops the session.
pub fn spawn_command_session(
    state: &crate::AppState,
    revka: &Path,
    on_data: Channel<PtyEvent>,
) -> Result<String, String> {
    stop_session(state)?;
    let shell = platform_shell(revka);
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: INITIAL_ROWS,
            cols: INITIAL_COLS,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| format!("could not open a pseudo terminal: {e}"))?;

    let mut cmd = CommandBuilder::new(&shell.program);
    cmd.args(&shell.args);
    cmd.env("KUMIHO_REVKA_BIN", revka.as_os_str());
    // Revka falls back to quick setup when a caller is not interactive. This
    // PTY is the full wizard by contract, so make that intent explicit even on
    // platforms with imperfect TTY detection.
    cmd.env("REVKA_INTERACTIVE", "1");
    cmd.env("TERM", "xterm-256color");
    if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }

    let process_tree = Arc::new(crate::process_tree::ProcessTree::new()?);
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("could not start {}: {e}", shell.program.display()))?;
    if let Err(error) = process_tree.assign_pty_child(child.as_ref()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    drop(pair.slave);

    let mut reader = match pair.master.try_clone_reader() {
        Ok(reader) => reader,
        Err(e) => {
            let _ = process_tree.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not read from the terminal: {e}"));
        }
    };
    let mut writer = match pair.master.take_writer() {
        Ok(writer) => writer,
        Err(e) => {
            let _ = process_tree.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not write to the terminal: {e}"));
        }
    };
    if let Some(startup_input) = shell.startup_input {
        if let Err(error) = writer
            .write_all(startup_input.as_bytes())
            .and_then(|_| writer.flush())
        {
            let _ = process_tree.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("could not start Revka in PowerShell: {error}"));
        }
    }

    #[cfg(windows)]
    let killer = child.clone_killer();
    let exited = Arc::new(AtomicBool::new(false));
    let exit_cleanup = Arc::new(ExitCleanup::default());
    let exit_process_tree = Arc::clone(&process_tree);
    let wait_exit_cleanup = Arc::clone(&exit_cleanup);
    {
        let mut guard = state.revka_pty.lock().map_err(|e| e.to_string())?;
        *guard = Some(PtySession {
            writer,
            master: pair.master,
            #[cfg(windows)]
            killer,
            process_tree,
            exit_cleanup,
            exited: Arc::clone(&exited),
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
        // Publish the reap before cleanup completion can wake a concurrent
        // close. On Unix the fallback killer is deliberately unavailable: in
        // portable-pty it stores only a numeric PID, which may already have
        // been reused after this wait returned.
        exited.store(true, Ordering::Release);
        // Clean descendants immediately when the PTY leader exits. Keeping a
        // raw Unix PGID until the user later closes a failed/cancelled modal
        // would risk targeting an unrelated group if that numeric ID is reused.
        // A later explicit stop retries if this best-effort cleanup fails.
        let cleanup_error =
            wait_exit_cleanup.run(|| exit_process_tree.terminate_after_leader_exit());
        let _ = exit_events.send(PtyEvent::Exit {
            success,
            cleanup_error,
        });
    });
    Ok(shell.label)
}

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

#[cfg(windows)]
fn fallback_kill(session: &mut PtySession, should_kill: bool) -> Option<String> {
    should_kill
        .then(|| session.killer.kill().err().map(|error| error.to_string()))
        .flatten()
}

#[cfg(not(windows))]
fn fallback_kill(_session: &mut PtySession, _should_kill: bool) -> Option<String> {
    // Unix's portable-pty killer signals a raw numeric PID. ProcessTree owns
    // the process group and preserves it on failure, so a retry is safer than
    // signalling a leader PID that may already have been reaped and reused.
    None
}

pub fn stop_session(state: &crate::AppState) -> Result<(), String> {
    let mut guard = state.revka_pty.lock().map_err(|e| e.to_string())?;
    if let Some(mut session) = guard.take() {
        let tree_error = session.process_tree.terminate().err();
        let prior_cleanup_error = session.exit_cleanup.wait_error();
        let should_fallback_kill = tree_error.is_some() && !session.exited.load(Ordering::Acquire);
        let kill_error = fallback_kill(&mut session, should_fallback_kill);
        // Dropping the master closes the PTY; the reader thread then exits.
        if tree_error.is_some() || prior_cleanup_error.is_some() || kill_error.is_some() {
            let errors = [
                prior_cleanup_error.as_ref(),
                tree_error.as_ref(),
                kill_error.as_ref(),
            ]
            .into_iter()
            .flatten()
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
            let message = format!("could not fully stop the onboarding terminal: {errors}");
            // The UI keeps the modal open and offers a retry. Preserve the
            // process-group/job handle so that retry is real rather than a
            // false-success no-op against an already removed session.
            *guard = Some(session);
            return Err(message);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{powershell_shell, unix_shell, Utf8Feeder};
    use std::path::PathBuf;

    #[test]
    fn korean_text_split_across_chunks_survives() {
        let mut feeder = Utf8Feeder::new();
        let bytes = "메모리".as_bytes();
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

    #[test]
    fn powershell_waits_for_job_assignment_before_starting_revka() {
        let shell = powershell_shell(PathBuf::from("pwsh.exe"));
        assert_eq!(shell.label, "PowerShell 7 · Revka CLI");
        assert_eq!(shell.args, vec!["-NoLogo", "-NoProfile"]);
        assert_eq!(
            shell.startup_input,
            Some("& $env:KUMIHO_REVKA_BIN onboard; $code=$LASTEXITCODE; if ($null -eq $code) { $code=1 }; exit $code\r")
        );
    }

    #[test]
    fn unix_posix_default_shell_runs_the_exact_revka_binary() {
        let shell = unix_shell(PathBuf::from("/bin/zsh"));
        assert_eq!(shell.program, PathBuf::from("/bin/zsh"));
        assert_eq!(shell.label, "zsh · Revka CLI");
        assert_eq!(shell.args, vec!["-c", "exec \"$KUMIHO_REVKA_BIN\" onboard"]);
        assert_eq!(shell.startup_input, None);
    }

    #[test]
    fn unix_fish_default_shell_runs_the_exact_revka_binary() {
        let shell = unix_shell(PathBuf::from("/usr/local/bin/fish"));
        assert_eq!(shell.program, PathBuf::from("/usr/local/bin/fish"));
        assert_eq!(shell.label, "fish · Revka CLI");
        assert_eq!(shell.args, vec!["-c", "exec \"$KUMIHO_REVKA_BIN\" onboard"]);
    }

    #[test]
    fn unix_nushell_uses_its_environment_syntax() {
        let shell = unix_shell(PathBuf::from("/opt/homebrew/bin/nu"));
        assert_eq!(shell.program, PathBuf::from("/opt/homebrew/bin/nu"));
        assert_eq!(shell.label, "nu · Revka CLI");
        assert_eq!(shell.args, vec!["-c", "exec $env.KUMIHO_REVKA_BIN onboard"]);
    }

    #[test]
    fn unix_unknown_default_shell_falls_back_safely() {
        let shell = unix_shell(PathBuf::from("/usr/local/bin/custom-shell"));
        assert_eq!(shell.program, PathBuf::from("/bin/sh"));
        assert_eq!(shell.label, "sh (fallback from custom-shell) · Revka CLI");
        assert_eq!(shell.args, vec!["-c", "exec \"$KUMIHO_REVKA_BIN\" onboard"]);
    }
}
