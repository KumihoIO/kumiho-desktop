//! Upgrade pillar — the CE ↔ Cloud story that powers the funnel. Copy is the
//! real kumiho.io positioning (Community Edition vs Kumiho Cloud + tiers).

use serde::Serialize;

#[derive(Serialize)]
pub struct Edition {
    pub badge: String,
    pub title: String,
    pub blurb: String,
    pub bullets: Vec<String>,
}

#[derive(Serialize)]
pub struct Tier {
    pub name: String,
    pub monthly: String,
    pub yearly: String,
    pub nodes: String,
    pub badge: Option<String>,
}

#[derive(Serialize)]
pub struct UpgradeInfo {
    pub current: String,
    pub tagline: String,
    pub ce: Edition,
    pub cloud: Edition,
    pub tiers: Vec<Tier>,
    pub signup_url: String,
    pub compare_url: String,
}

#[tauri::command]
pub fn upgrade_status() -> UpgradeInfo {
    let current = std::env::var("KUMIHO_CLAUDE_MODE")
        .map(|v| if v == "ce" { "ce" } else { "cloud" })
        .unwrap_or("ce")
        .to_string();
    UpgradeInfo {
        current,
        tagline: "The memory layer AI agents can trust.".into(),
        ce: Edition {
            badge: "Free · Single-user · Self-hosted".into(),
            title: "Run Kumiho Server. On your own machine.".into(),
            blurb: "A free, single-user build of the Kumiho graph server. Runs entirely on your \
                    machine, talks only to a local Neo4j (and optional Redis), needs no account or \
                    cloud connection."
                .into(),
            bullets: vec![
                "You run Neo4j (and optionally Redis) locally".into(),
                "No data caps — your single-user graph is unlimited".into(),
                "No account, token, or network connection required".into(),
                "Single-user and local only — not a shared backend".into(),
            ],
        },
        cloud: Edition {
            badge: "Recommended for teams & agents".into(),
            title: "Kumiho Cloud".into(),
            blurb: "Managed infrastructure we run for you. Nothing to install, multi-user and \
                    agent-ready, with AI Cognitive Memory and cross-session recall included."
                .into(),
            bullets: vec![
                "We host Neo4j, Redis, and the server".into(),
                "5,000 nodes free — real evaluation territory".into(),
                "Team collaboration and shared access".into(),
                "Upgrade in place to Creator / Studio for more scale".into(),
            ],
        },
        tiers: vec![
            Tier { name: "Free".into(), monthly: "$0".into(), yearly: "$0".into(), nodes: "5,000".into(), badge: None },
            Tier { name: "Creator".into(), monthly: "$40".into(), yearly: "$399".into(), nodes: "30,000".into(), badge: None },
            Tier { name: "Studio".into(), monthly: "$99".into(), yearly: "$989".into(), nodes: "500,000".into(), badge: Some("Most Popular".into()) },
            Tier { name: "Studio Pro".into(), monthly: "$170".into(), yearly: "$1,699".into(), nodes: "2,000,000".into(), badge: None },
            Tier { name: "Enterprise".into(), monthly: "Contact".into(), yearly: "Contact".into(), nodes: "Unlimited".into(), badge: None },
        ],
        signup_url: "https://kumiho.io/signup".into(),
        compare_url: "https://kumiho.io/pricing".into(),
    }
}
