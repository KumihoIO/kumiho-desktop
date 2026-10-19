//! Transfer pillar — restore a Cloud `.kumiho` workspace backup into Community
//! Edition. The archive format and restore engine live in `kumiho_server`
//! (`transfer check`, `import`, `transfer verify`); Desktop only validates the
//! request, makes sure no writer is running, and relays the server's output.

use crate::util::{ce_binary, command};
use std::path::{Path, PathBuf};
use std::process::Output;

/// Mirrors the server's archive size limit (64 MiB envelope).
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

fn archive_path(path: &str) -> Result<PathBuf, String> {
    let trimmed = path.trim().trim_matches(|c| c == '"' || c == '\'');
    if trimmed.is_empty() {
        return Err("choose a .kumiho backup file first".into());
    }
    let path = PathBuf::from(trimmed);
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

fn run_server(args: &[&str], file: &Path) -> Result<Output, String> {
    let bin = ce_binary().ok_or("kumiho_server is not installed yet — install Community Edition first")?;
    command(&bin.to_string_lossy())
        .args(args)
        .arg(file)
        // Import resolves ~/.kumiho/server.toml itself; keep the cwd neutral so
        // stray .env files next to a download cannot redirect the target.
        .current_dir(file.parent().unwrap_or_else(|| Path::new(".")))
        .output()
        .map_err(|e| format!("could not run kumiho_server: {e}"))
}

fn text(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !err.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str(&err);
    }
    s
}

fn step(label: &str, args: &[&str], file: &Path) -> Result<String, String> {
    let out = run_server(args, file)?;
    let body = text(&out);
    if out.status.success() {
        return Ok(body);
    }
    let unsupported = body.contains("Usage") || body.to_ascii_lowercase().contains("unknown");
    let hint = if unsupported {
        "\nThis kumiho_server build may predate .kumiho import — update Community Edition and retry."
    } else {
        ""
    };
    Err(format!("{label} failed ({}):\n{body}{hint}", out.status))
}

/// Offline validation: reads only the file, never touches Neo4j or the CE config.
#[tauri::command(async)]
pub fn ce_import_check(path: String) -> Result<String, String> {
    let file = archive_path(&path)?;
    step("check", &["transfer", "check"], &file)
}

/// Check, import, then verify. Refuses while CE is running because the restore
/// needs exclusive access to a dedicated empty database.
#[tauri::command(async)]
pub fn ce_import(path: String) -> Result<String, String> {
    let file = archive_path(&path)?;
    if crate::run::port_open(crate::run::CE_PORT) || crate::run::any_ce_process_running()? {
        return Err("stop the Community Edition server before importing — the restore needs exclusive access to the database".into());
    }
    let checked = step("check", &["transfer", "check"], &file)?;
    let imported = step("import", &["import"], &file)?;
    let verified = step("verify", &["transfer", "verify"], &file)?;
    Ok([checked, imported, verified]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_kumiho_and_missing_files() {
        assert!(archive_path("").is_err());
        assert!(archive_path("backup.cypher").unwrap_err().contains(".kumiho"));
        assert!(archive_path("/definitely/not/here.kumiho").is_err());
    }

    #[test]
    fn accepts_quoted_path_to_a_real_archive() {
        let dir = std::env::temp_dir().join(format!("kumiho-transfer-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("b.KUMIHO");
        std::fs::write(&file, b"{}").unwrap();
        let quoted = format!("\"{}\"", file.display());
        assert_eq!(archive_path(&quoted).unwrap(), file);
        std::fs::write(&file, b"").unwrap();
        assert!(archive_path(&quoted).unwrap_err().contains("empty"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
