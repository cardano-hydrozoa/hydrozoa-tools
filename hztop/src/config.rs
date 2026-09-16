use clap::Parser;
use figment::{
    Figment,
    providers::{Env, Format, Json},
};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "hztop",
    about = "At-a-glance operator dashboard for a Hydrozoa head"
)]
pub struct Args {
    /// Config file(s), merged left to right on top of the built-in defaults.
    /// When omitted, the first existing of $HZTOP_CONFIG,
    /// ~/.config/hztop/config.json, /etc/hztop/config.json is used.
    #[clap(short, long)]
    pub config: Vec<PathBuf>,

    /// Screen to open: c(onsensus), a(lerts). Omitted: the screen from the
    /// previous run.
    #[clap(value_parser = ["c", "a"])]
    pub screen: Option<String>,

    /// Print one JSON snapshot of everything the dashboard knows, then exit.
    /// Implied when stdout is not a terminal, so `hztop | jq ...`, a script, or
    /// an agent gets machine-readable output instead of a TUI that cannot draw.
    #[clap(long)]
    pub json: bool,

    /// How long `--json` waits for the heads before printing. It prints
    /// whatever has arrived when this expires, so an unreachable head delays
    /// the answer but never withholds it.
    #[clap(long, default_value_t = 10_000)]
    pub json_timeout_ms: u64,
}

/// The default config search path, used when no `-c` is given: $HZTOP_CONFIG,
/// then ~/.config/hztop/config.json, then /etc/hztop/config.json. First hit
/// wins; no hit means built-in defaults only.
fn default_config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HZTOP_CONFIG") {
        return Some(PathBuf::from(p));
    }
    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".config/hztop/config.json");
        if p.exists() {
            return Some(p);
        }
    }
    let etc = PathBuf::from("/etc/hztop/config.json");
    etc.exists().then_some(etc)
}

#[derive(Debug, Deserialize, Clone)]
pub struct HeadTarget {
    pub name: String,
    /// Base URL of a hydrozoa head peer, e.g. "http://127.0.0.1:8080".
    pub url: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub heads: Vec<HeadTarget>,
    /// When true, skip painting the app background and inherit the terminal
    /// theme. Default false: paint an explicit uniform background (some
    /// terminal stacks render default-background cells inconsistently).
    #[serde(default)]
    pub transparent_background: bool,

    pub hard_confirm_warn_secs: i64,
    pub hard_confirm_crit_secs: i64,
    pub fallback_warn_secs: i64,
    pub fallback_crit_secs: i64,
}

pub fn load_config(paths: &[PathBuf]) -> anyhow::Result<Config> {
    let mut figment = Figment::new().merge(Json::string(include_str!("../config.default.json")));
    let paths = if paths.is_empty() {
        default_config_path().into_iter().collect()
    } else {
        paths.to_vec()
    };
    for path in paths {
        figment = figment.merge(Json::file(path));
    }
    Ok(figment.merge(Env::prefixed("HZTOP_")).extract()?)
}
