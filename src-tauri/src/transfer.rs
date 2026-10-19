//! Transfer pillar — restore a Cloud `.kumiho` workspace backup into Community
//! Edition. The archive format and restore engine live in `kumiho_server`
//! (`transfer check`, `import`, `transfer verify`); Desktop only validates the
//! request, makes sure no writer is running, and relays the server's output.

use crate::util::{ce_binary, command, kumiho_home};
use crate::AppState;
use tauri::State;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Mirrors the server's archive size limit (64 MiB envelope).
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const CHECK_TIMEOUT: Duration = Duration::from_secs(120);
const RESTORE_TIMEOUT: Duration = Duration::from_secs(30 * 60);

static IMPORTING: AtomicBool = AtomicBool::new(false);

/// True while an import/verify is running; `ce_start` refuses to launch CE then.
pub fn import_in_progress() -> bool {
    IMPORTING.load(Ordering::SeqCst)
}

struct ImportGuard;
impl ImportGuard {
    fn acquire() -> Result<Self, String> {
        IMPORTING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .map(|_| ImportGuard)
            .map_err(|_| "another import is already running".to_string())
    }
}
impl Drop for ImportGuard {
    fn drop(&mut self) {
        IMPORTING.store(false, Ordering::SeqCst);
    }
}

/// Canonical absolute path, so the child's working directory can never change
/// how it resolves.
fn archive_path(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim().trim_matches(|c| c == '"' || c == '\'');
    if trimmed.is_empty() {
        return Err("choose a .kumiho backup file first".into());
    }
    let expanded = match trimmed.strip_prefix("~/").or_else(|| trimmed.strip_prefix("~\\")) {
        Some(rest) => dirs::home_dir().ok_or("cannot resolve ~")?.join(rest),
        None => PathBuf::from(trimmed),
    };
    let path = std::fs::canonicalize(&expanded)
        .map_err(|e| format!("cannot read {}: {e}", expanded.display()))?;
    let is_kumiho = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("kumiho"))
        .unwrap_or(false);
    if !is_kumiho {
        return Err("only .kumiho backups can be imported (the .cypher script is for manual Neo4j restores)".into());
    }
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() == 0 {
        return Err("the backup file is empty — download it again".into());
    }
    if meta.len() > MAX_ARCHIVE_BYTES {
        return Err("the backup is larger than the 64 MiB import limit".into());
    }
    Ok(path)
}

struct Finished {
    success: bool,
    status: String,
    output: String,
}

/// Run to completion or kill after `timeout`, capturing stdout+stderr.
fn run_with_timeout(mut cmd: std::process::Command, timeout: Duration) -> Result<Finished, String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run kumiho_server: {e}"))?;
    let drain = |mut r: Box<dyn Read + Send>| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = r.read_to_end(&mut buf);
            String::from_utf8_lossy(&buf).trim().to_string()
        })
    };
    let out = drain(Box::new(child.stdout.take().ok_or("no stdout")?));
    let err = drain(Box::new(child.stderr.take().ok_or("no stderr")?));
    let deadline = Instant::now() + timeout;
    let status = loop {
        let polled = match child.try_wait() {
            Ok(polled) => polled,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e.to_string());
            }
        };
        match polled {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    };
    let (out, err) = (out.join().unwrap_or_default(), err.join().unwrap_or_default());
    let output = [out, err].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n");
    Ok(Finished { success: status.success(), status: status.to_string(), output })
}

fn server_command() -> Result<std::process::Command, String> {
    let bin = ce_binary().ok_or("kumiho_server is not installed yet — install Community Edition first")?;
    let mut cmd = command(&bin.to_string_lossy());
    // Ambient KUMIHO_* overrides still apply with --config; don't inherit any.
    for (key, _) in std::env::vars() {
        if key.starts_with("KUMIHO_") {
            cmd.env_remove(key);
        }
    }
    // Neutral cwd: nothing next to a download can be picked up as settings.
    if let Some(home) = kumiho_home() {
        cmd.current_dir(home);
    }
    Ok(cmd)
}

/// An older binary treats unknown subcommands as "start the server", which would
/// block forever, so require the import usage text before running anything.
fn ensure_import_supported() -> Result<(), String> {
    let mut cmd = server_command()?;
    // A build that predates --help would start the server; park it on an
    // ephemeral loopback port so the probe can never collide with real CE.
    cmd.env("KUMIHO_SERVER_ADDR", "127.0.0.1:0").arg("--help");
    let probe = run_with_timeout(cmd, PROBE_TIMEOUT)
        .map_err(|e| format!("could not confirm import support ({e}) — update Community Edition and retry"))?;
    if probe.success && probe.output.contains("import FILE") {
        Ok(())
    } else {
        Err("this kumiho_server build does not support .kumiho import — update Community Edition and retry".into())
    }
}

/// The CE settings Desktop wrote; passing it explicitly disables ambient
/// `.env`/`config/` discovery in the server.
fn ce_config() -> Result<PathBuf, String> {
    let cfg = kumiho_home().ok_or("cannot find ~/.kumiho")?.join("server.toml");
    if cfg.is_file() {
        Ok(cfg)
    } else {
        Err("Community Edition is not set up yet — finish CE setup before importing".into())
    }
}

fn step(label: &str, args: &[&str], file: &Path, config: Option<&Path>, timeout: Duration) -> Result<String, String> {
    let mut cmd = server_command()?;
    cmd.args(args).arg(file);
    if let Some(config) = config {
        cmd.arg("--config").arg(config);
    }
    let done = run_with_timeout(cmd, timeout).map_err(|e| format!("{label} failed: {e}"))?;
    if done.success {
        return Ok(done.output);
    }
    let reason = failure_reason(&done.output);
    let hint = failure_hint(&done.output);
    Err(format!("{label} failed: {reason}{hint}\n({})\n{}", done.status, done.output))
}

/// Last meaningful line, with the server's Debug wrapper (`Error: Variant("…")`) removed.
fn failure_reason(output: &str) -> String {
    let line = output.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("no output").trim();
    let line = line.strip_prefix("Error: ").unwrap_or(line);
    let unwrapped = line
        .split_once("(\"")
        .and_then(|(_, rest)| rest.strip_suffix("\")"))
        .unwrap_or(line);
    unwrapped.replace("\\\"", "\"")
}

fn failure_hint(output: &str) -> &'static str {
    let lower = output.to_ascii_lowercase();
    if lower.contains("not empty") || lower.contains("occupied") || lower.contains("receipt") {
        " — the import needs a dedicated, empty Neo4j database. Community Edition fills its database on first start, so point CE at a new empty database (re-run setup) and try again."
    } else {
        ""
    }
}

fn ce_is_running() -> Result<bool, String> {
    let cfg = crate::config::desktop_config_get();
    let port = if cfg.server_port == 0 { crate::run::CE_PORT } else { cfg.server_port };
    if crate::run::port_open(port) || crate::run::port_open(crate::run::CE_PORT) {
        return Ok(true);
    }
    crate::run::any_ce_process_running()
        .map_err(|e| format!("could not confirm Community Edition is stopped ({e}) — stop it manually and retry"))
}

fn require_stopped() -> Result<(), String> {
    if ce_is_running()? {
        return Err("stop the Community Edition server before importing — the restore needs exclusive access to the database".into());
    }
    Ok(())
}

/// Claim the import slot and prove no CE is running. Holding `ce_start` while the
/// slot is taken and the check runs closes the window where a start could pass its
/// own flag check, then spawn after we looked; `ce_start` re-checks the flag under
/// that lock.
fn begin_exclusive(state: &State<'_, AppState>) -> Result<ImportGuard, String> {
    let guard = ImportGuard::acquire()?;
    let _start = state.ce_start.lock().map_err(|e| e.to_string())?;
    require_stopped()?;
    Ok(guard)
}

/// Offline validation of the archive. It runs the server binary (including a
/// support probe), so it follows the same "CE stopped" rule as the import.
#[tauri::command(async)]
pub fn ce_import_check(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let file = archive_path(&path)?;
    let _guard = begin_exclusive(&state)?;
    ensure_import_supported()?;
    step("check", &["transfer", "check"], &file, None, CHECK_TIMEOUT)
}

/// Check, import, then verify. Refuses while CE is running because the restore
/// needs exclusive access to a dedicated empty database.
#[tauri::command(async)]
pub fn ce_import(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let file = archive_path(&path)?;
    let _guard = begin_exclusive(&state)?;
    ensure_import_supported()?;
    let config = ce_config()?;
    let checked = step("check", &["transfer", "check"], &file, None, CHECK_TIMEOUT)?;
    let imported = step("import", &["import"], &file, Some(&config), RESTORE_TIMEOUT).map_err(|e| {
        if e.contains("timed out") {
            format!("{e}\nThe import was stopped part-way; its transaction does not commit unless it finishes, but check the Neo4j database before retrying.")
        } else {
            e
        }
    })?;
    // The import is committed at this point; say so if verification fails so
    // nobody retries a full import into the now-occupied database.
    let verified = step("verify", &["transfer", "verify"], &file, Some(&config), RESTORE_TIMEOUT).map_err(|e| {
        format!("The import completed and was committed, but verification did not pass. Use \"Verify only\" to retry; do not import again.\n{e}\n{imported}")
    })?;
    Ok([checked, imported, verified]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Re-run verification against an already imported database.
#[tauri::command(async)]
pub fn ce_import_verify(state: State<'_, AppState>, path: String) -> Result<String, String> {
    let file = archive_path(&path)?;
    let _guard = begin_exclusive(&state)?;
    ensure_import_supported()?;
    let config = ce_config()?;
    step("verify", &["transfer", "verify"], &file, Some(&config), RESTORE_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kumiho-transfer-{label}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rejects_non_kumiho_and_missing_files() {
        assert!(archive_path("").is_err());
        assert!(archive_path("/definitely/not/here.kumiho").is_err());
        let dir = temp_dir("ext");
        let cypher = dir.join("b.cypher");
        std::fs::write(&cypher, b"x").unwrap();
        assert!(archive_path(&cypher.to_string_lossy()).unwrap_err().contains(".kumiho"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn canonicalizes_quoted_paths_and_rejects_empty_archives() {
        let dir = temp_dir("quoted");
        let file = dir.join("b.KUMIHO");
        std::fs::write(&file, b"{}").unwrap();
        let quoted = format!("\"{}\"", file.display());
        let resolved = archive_path(&quoted).unwrap();
        assert!(resolved.is_absolute());
        assert_eq!(resolved, std::fs::canonicalize(&file).unwrap());
        std::fs::write(&file, b"").unwrap();
        assert!(archive_path(&quoted).unwrap_err().contains("empty"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failure_reason_unwraps_the_servers_debug_error() {
        let raw = "some log\nError: Invalid(\"target Neo4j database is not empty; refusing\")";
        assert_eq!(failure_reason(raw), "target Neo4j database is not empty; refusing");
        assert_eq!(failure_reason("plain failure"), "plain failure");
        assert_eq!(failure_reason(""), "no output");
        assert!(failure_hint(raw).contains("empty Neo4j database"));
        assert_eq!(failure_hint("checksum mismatch"), "");
    }

    #[test]
    fn import_guard_is_exclusive_and_released() {
        let first = ImportGuard::acquire().unwrap();
        assert!(import_in_progress());
        assert!(ImportGuard::acquire().is_err());
        drop(first);
        assert!(!import_in_progress());
    }

    #[cfg(unix)]
    #[test]
    fn captures_output_and_reports_exit_status() {
        let mut ok = std::process::Command::new("sh");
        ok.args(["-c", "echo out; echo err >&2"]);
        let done = run_with_timeout(ok, Duration::from_secs(5)).unwrap();
        assert!(done.success);
        assert_eq!(done.output, "out\nerr");

        let mut bad = std::process::Command::new("sh");
        bad.args(["-c", "echo nope; exit 3"]);
        let done = run_with_timeout(bad, Duration::from_secs(5)).unwrap();
        assert!(!done.success);
        assert!(done.status.contains('3') && done.output == "nope");
    }

    #[cfg(unix)]
    #[test]
    fn kills_a_child_that_outlives_the_timeout() {
        let mut slow = std::process::Command::new("sleep");
        slow.arg("30");
        let started = Instant::now();
        let err = run_with_timeout(slow, Duration::from_millis(200)).err().unwrap();
        assert!(err.contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }
}
