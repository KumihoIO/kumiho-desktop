//! Desktop's local-only Cloud backup handoff. The server owns archive validation
//! and atomic restore; Desktop stages the selected file and serializes CE starts.
use crate::util::{ce_binary, command, kumiho_home};
use crate::AppState;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::Manager;

const MAX_BYTES: usize = 64 * 1024 * 1024;
static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct Staging(PathBuf);
impl Staging {
    fn new() -> Result<Self, String> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "kumiho-import-{}-{stamp}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        #[allow(unused_mut)]
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| "Could not create private import staging folder.")?;
        Ok(Self(path))
    }
    fn write(&self, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
        let path = self.0.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&path)
            .and_then(|mut f| f.write_all(bytes))
            .map_err(|_| "Could not stage the backup or CE configuration.")?;
        Ok(path)
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// No shell, no inherited Kumiho overrides, and an isolated working directory:
// a downloaded filename or shell environment must not select another database.
fn cli(
    binary: &Path,
    staging: &Staging,
    args: &[&std::ffi::OsStr],
    config: Option<&Path>,
    timeout: Duration,
) -> Result<(bool, String), String> {
    let mut cmd = command(binary.to_str().ok_or("Invalid CE executable path.")?);
    cmd.args(args).current_dir(&staging.0).stdin(Stdio::null());
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("KUMIHO_")
        {
            cmd.env_remove(key);
        }
    }
    cmd.env("RUST_LOG", "off");
    if let Some(config) = config {
        cmd.arg("--config").arg(config);
    } else {
        // Older binaries treat unknown commands as server startup. Never let a
        // capability probe accept the EULA or connect using real credentials.
        cmd.env("KUMIHO_EULA_ACCEPTED", "false");
    }
    let output_path = staging.0.join("command-output");
    let output = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&output_path)
        .map_err(|_| "Could not capture import result.")?;
    cmd.stdout(output.try_clone().map_err(|e| e.to_string())?)
        .stderr(output);
    let mut child = cmd
        .spawn()
        .map_err(|_| "Could not launch the installed CE server.")?;
    let until = Instant::now() + timeout;
    let result = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status.success()),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(50)),
            status => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(if status.is_err() { "Could not monitor the import process." } else { "The operation timed out. Its result is uncertain; keep your backup and verify the database before retrying." }.to_string());
            }
        }
    };
    let mut output = String::new();
    fs::File::open(&output_path)
        .and_then(|f| f.take(16384).read_to_string(&mut output))
        .map_err(|_| "Could not read import result.")?;
    Ok((result?, output))
}

fn failure(stage: &str, output: &str) -> String {
    if output.contains("dedicated empty") || output.contains("already contains") {
        return "This CE database already contains data. Import requires an empty database; nothing was replaced. Keep your existing data and configure a separate empty database.".into();
    }
    if stage == "verify" {
        return "Import completed, but the follow-up verification failed. Keep the backup, leave CE stopped, and verify the database before retrying an import.".into();
    }
    if stage == "check" {
        return "The backup failed validation. Choose an original .kumiho download (maximum 64 MiB) with a valid checksum and supported graph data.".into();
    }
    "Import failed. Check that Neo4j is running, CE settings and EULA acceptance are complete, and the database is empty. Keep the backup; do not delete Cloud data.".into()
}

fn perform_import(binary: &Path, config: &Path, archive: &str) -> Result<String, String> {
    if archive.is_empty() || archive.len() > MAX_BYTES {
        return Err("Choose a nonempty .kumiho backup no larger than 64 MiB.".into());
    }
    let staging = Staging::new()?;
    let file = staging.write("backup.kumiho", archive.as_bytes())?;
    let config_bytes = fs::read(config)
        .map_err(|_| "Set up Community Edition first; its saved configuration is missing.")?;
    let config = staging.write("server.toml", &config_bytes)?;
    let (supported, help) = cli(
        binary,
        &staging,
        &["import".as_ref(), "--help".as_ref()],
        None,
        Duration::from_secs(10),
    )?;
    if !supported || !help.contains("kumiho_server import FILE") {
        return Err("This CE server does not support backup import. Update CE to a release with the import command, then retry.".into());
    }
    for stage in ["check", "import", "verify"] {
        let args = if stage == "import" {
            vec!["import".as_ref(), file.as_os_str()]
        } else {
            vec!["transfer".as_ref(), stage.as_ref(), file.as_os_str()]
        };
        let (ok, output) = cli(
            binary,
            &staging,
            &args,
            Some(&config),
            Duration::from_secs(300),
        )?;
        if !ok {
            return Err(failure(stage, &output));
        }
    }
    Ok("Backup imported and verified. Start Community Edition to view your projects and memories. Keep the original backup until you have checked your data.".into())
}

#[tauri::command]
pub async fn ce_import_backup(app: tauri::AppHandle, archive: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        let _guard = state
            .ce_start
            .try_lock()
            .map_err(|_| "Another CE operation is running. Wait for it to finish.")?;
        if crate::config::desktop_config_get().mode != "ce" {
            return Err("Select Community Edition before importing a backup.".into());
        }
        crate::run::ensure_ce_stopped_for_import(&state)?;
        let home = kumiho_home().ok_or("No home directory.")?;
        let binary = ce_binary().ok_or("Install Community Edition before importing a backup.")?;
        perform_import(&binary, &home.join("server.toml"), &archive)
    })
    .await
    .map_err(|_| {
        "The import task ended unexpectedly. Keep your backup and verify before retrying."
            .to_string()
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn staging_is_private_unique_and_removed() {
        let first = Staging::new().unwrap();
        let second = Staging::new().unwrap();
        assert_ne!(first.0, second.0);
        let path = first.write("backup.kumiho", b"synthetic").unwrap();
        assert!(first.write("backup.kumiho", b"replace").is_err());
        let dir = first.0.clone();
        assert_eq!(fs::read(path).unwrap(), b"synthetic");
        drop(first);
        assert!(!dir.exists());
    }
    #[test]
    fn errors_distinguish_occupied_database_and_post_commit_verification() {
        assert!(failure("import", "requires a dedicated empty database")
            .contains("nothing was replaced"));
        assert!(failure("verify", "mismatch").contains("Import completed"));
        assert!(!failure("import", "secret=value").contains("secret=value"));
        assert!(perform_import(Path::new("missing"), Path::new("missing"), "").is_err());
    }
    #[test]
    fn unsupported_binary_and_timeout_do_not_report_success() {
        let stage = Staging::new().unwrap();
        let config = stage.write("test.toml", b"eula_accepted = false").unwrap();
        let executable = std::env::current_exe().unwrap();
        assert!(perform_import(&executable, &config, "{}")
            .unwrap_err()
            .contains("does not support"));
        let err = cli(
            &executable,
            &stage,
            &[
                "--ignored".as_ref(),
                "--exact".as_ref(),
                "backup::tests::slow_cli_fixture".as_ref(),
            ],
            None,
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(err.contains("timed out"));
    }
    #[test]
    #[ignore = "subprocess fixture for timeout test"]
    fn slow_cli_fixture() {
        std::thread::sleep(Duration::from_secs(5));
    }

    #[test]
    #[ignore = "requires explicitly selected disposable Neo4j configuration and a built CE binary"]
    fn real_ce_import() {
        let binary = std::env::var_os("KUMIHO_IMPORT_TEST_BINARY").expect("test binary");
        let config = std::env::var_os("KUMIHO_IMPORT_TEST_CONFIG").expect("disposable DB config");
        let archive = fs::read_to_string(
            std::env::var_os("KUMIHO_IMPORT_TEST_ARCHIVE").expect("synthetic archive"),
        )
        .unwrap();
        let mut corrupt: serde_json::Value = serde_json::from_str(&archive).unwrap();
        corrupt["sha256"] = serde_json::Value::String("0".repeat(64));
        assert!(
            perform_import(Path::new(&binary), Path::new(&config), &corrupt.to_string())
                .unwrap_err()
                .contains("failed validation")
        );
        assert!(
            perform_import(Path::new(&binary), Path::new(&config), &archive)
                .unwrap()
                .contains("imported and verified")
        );
        assert!(
            perform_import(Path::new(&binary), Path::new(&config), &archive)
                .unwrap_err()
                .contains("already contains data")
        );
    }
}
