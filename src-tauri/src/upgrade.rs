//! Upgrade pillar — the CE ↔ Cloud comparison that powers the funnel: run
//! free/private on your machine, upgrade to hosted + team when you outgrow it.

use serde::Serialize;

#[derive(Serialize)]
pub struct Edition {
    pub name: String,
    pub tagline: String,
    pub features: Vec<String>,
}

#[derive(Serialize)]
pub struct UpgradeInfo {
    /// "ce" or "cloud" — the launcher's current mode.
    pub current: String,
    pub ce: Edition,
    pub cloud: Edition,
    pub signup_url: String,
}

#[tauri::command]
pub fn upgrade_status() -> UpgradeInfo {
    let ce_mode = std::env::var("KUMIHO_CLAUDE_MODE")
        .map(|v| v == "ce")
        .unwrap_or(false);
    UpgradeInfo {
        current: if ce_mode { "ce".into() } else { "cloud".into() },
        ce: Edition {
            name: "Community Edition".into(),
            tagline: "Local, private, free".into(),
            features: vec![
                "Runs on your machine (loopback only)".into(),
                "Single user".into(),
                "No data caps".into(),
                "You run and manage the server".into(),
            ],
        },
        cloud: Edition {
            name: "Kumiho Cloud".into(),
            tagline: "Hosted, team-ready".into(),
            features: vec![
                "Managed, always-on server".into(),
                "Multi-user + memory sharing".into(),
                "Cross-device sync".into(),
                "Enterprise controls".into(),
            ],
        },
        signup_url: "https://kumiho.io".into(),
    }
}
