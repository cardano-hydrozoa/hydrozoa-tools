use clap::Parser;
use figment::{
    Figment,
    providers::{Env, Format, Json},
};
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "hydrozoa-archiver",
    about = "Copy a Hydrozoa head's store out from under a running node"
)]
pub struct Args {
    /// Config file(s), merged left to right on top of the built-in defaults.
    #[clap(short, long)]
    pub config: Vec<PathBuf>,

    /// Run a single pass and exit, instead of looping. The pass is a full one.
    #[clap(long)]
    pub once: bool,

    /// Report what a pass would copy, and copy nothing.
    #[clap(long)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    /// The node's store directory — opened read-only, as a RocksDB secondary. The node keeps
    /// writing to it throughout and is never told this reader exists.
    pub store_path: PathBuf,
    /// Scratch directory for this process's secondary instance. Must be writable, and must not be
    /// shared with another reader: RocksDB does not lock it, so two readers sharing one corrupt
    /// each other's view with no error. The process id is appended for that reason.
    pub secondary_path: PathBuf,
    /// Where the archive lives. A RocksDB in the node's own layout.
    pub archive_path: PathBuf,

    /// Seconds between journal tails.
    #[serde(default = "default_tail_interval_secs")]
    pub tail_interval_secs: u64,
    /// Seconds between full passes, which also copy the non-journal families.
    ///
    /// Slower than the tail on purpose: those families have no resume point, so each pass rescans
    /// them whole and costs the same whether anything changed or not.
    #[serde(default = "default_full_interval_secs")]
    pub full_interval_secs: u64,

    /// The node's admin API, e.g. "http://127.0.0.1:8080". Unset means archive but never report,
    /// which leaves the node retaining everything — the safe direction.
    #[serde(default)]
    pub node_url: Option<String>,
    #[serde(default)]
    pub admin_username: Option<String>,
    #[serde(default)]
    pub admin_password: Option<String>,
}

fn default_tail_interval_secs() -> u64 {
    5
}

fn default_full_interval_secs() -> u64 {
    300
}

pub fn load(paths: &[PathBuf]) -> anyhow::Result<Config> {
    let mut figment = Figment::new().merge(Json::string(include_str!("../config.default.json")));
    for path in paths {
        figment = figment.merge(Json::file(path));
    }
    Ok(figment
        .merge(Env::prefixed("HYDROZOA_ARCHIVER_"))
        .extract()?)
}
