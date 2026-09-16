//! The shared state every poller writes its own slice of, and the draw reads.

use serde::Deserialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use chrono::{DateTime, Utc};

pub type Shared = Arc<Mutex<App>>;

/// Samples kept per sparkline. Wider than any terminal, so the graph scrolls
/// rather than rescaling as the window changes.
const HISTORY: usize = 600;

/// Which screen is showing. `c`/`1` and `a`/`2` select them, and the
/// last-viewed screen persists across runs (see persist.rs).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum Screen {
    #[default]
    Consensus,
    Alerts,
}

impl Screen {
    pub const ALL: [Screen; 2] = [Screen::Consensus, Screen::Alerts];

    pub fn key(self) -> char {
        match self {
            Screen::Consensus => 'c',
            Screen::Alerts => 'a',
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Screen::Consensus => "consensus",
            Screen::Alerts => "alerts",
        }
    }

    pub fn from_key(c: char) -> Option<Screen> {
        Screen::ALL.into_iter().find(|s| s.key() == c)
    }

    pub fn next(self) -> Screen {
        let i = Screen::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Screen::ALL[(i + 1) % Screen::ALL.len()]
    }

    pub fn prev(self) -> Screen {
        let i = Screen::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Screen::ALL[(i + Screen::ALL.len() - 1) % Screen::ALL.len()]
    }
}

/// One raised/cleared transition in the alert log. Timestamps are unix
/// seconds so the log serializes without pulling chrono's serde feature.
/// `key` is the alert's stable identity (see alerts.rs); entries written by
/// older builds have an empty key and match by text instead.
#[derive(Clone, serde::Serialize, Deserialize)]
pub struct AlertEvent {
    pub at_unix: i64,
    #[serde(default)]
    pub key: String,
    pub text: String,
    pub crit: bool,
    pub cleared: bool,
}

impl AlertEvent {
    /// The identity used for pairing and dedup: the key when present, the
    /// text for entries from before keys existed.
    pub fn ident(&self) -> &str {
        if self.key.is_empty() {
            &self.text
        } else {
            &self.key
        }
    }
}

pub const ALERT_LOG_CAP: usize = 200;

/// A fixed-length sample history, oldest first.
#[derive(Default)]
pub struct Ring {
    buf: VecDeque<u64>,
}

impl Ring {
    pub fn push(&mut self, v: u64) {
        if self.buf.len() == HISTORY {
            self.buf.pop_front();
        }
        self.buf.push_back(v);
    }

    /// The most recent `width` samples, oldest first.
    pub fn tail(&self, width: usize) -> Vec<u64> {
        let skip = self.buf.len().saturating_sub(width);
        self.buf.iter().skip(skip).copied().collect()
    }
}

// --- hydrozoa /head/stats (PeerStatsView) ---------------------------------

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct RateStats {
    pub now: f64,
    pub load1m: f64,
    pub load5m: f64,
    pub load15m: f64,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct LocalRequestStats {
    pub total: u64,
    pub rate: RateStats,
    pub rejected_screening: u64,
    pub rejected_backpressure: u64,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PeerRequestStats {
    pub peer: i64,
    pub total: u64,
    pub rate: RateStats,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BlockStats {
    pub minor: u64,
    pub major: u64,
    pub avg_events: f64,
    pub max_events: u64,
    pub block_rate: RateStats,
    pub request_rate: RateStats,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct StackStats {
    pub total: u64,
    pub last_stack_number: i64,
    pub seconds_since_last_hard_confirm: i64,
    pub mean_inter_stack_gap_seconds: f64,
    pub avg_blocks_absorbed: f64,
    pub max_blocks_absorbed: u64,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BlockTiming {
    pub block_number: u64,
    pub millis: u64,
    pub requests: u64,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct TimingStats {
    pub count: u64,
    pub avg_millis: f64,
    pub avg_millis_per_request: f64,
    pub top: Vec<BlockTiming>,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct BlockTimings {
    pub lead: TimingStats,
    pub replay: TimingStats,
    pub soft_consensus: TimingStats,
}

#[derive(Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct HeadStats {
    pub uptime_seconds: u64,
    pub local_requests: LocalRequestStats,
    pub peer_requests: Vec<PeerRequestStats>,
    pub blocks: BlockStats,
    pub stacks: StackStats,
    pub block_timings: BlockTimings,
    pub mempool_size: i64,
    pub leader_mempool_drain: i64,
    pub sequencer_headroom: i64,
}

/// What `/head/blocks/{n}` says about the newest block.
#[derive(Clone, Default)]
pub struct LatestBlockInfo {
    pub number: u64,
    pub block_type: String,
    pub status: String,
    pub version_major: i64,
    pub version_minor: i64,
    pub fallback_at: Option<DateTime<Utc>>,
    pub forced_major_at: Option<DateTime<Utc>>,
}

// --- per-source states ------------------------------------------------------

pub struct HeadState {
    pub name: String,
    /// None until the first poll completes; a fresh start is unknown, not down.
    pub reachable: Option<bool>,
    /// Body of /ready: initializing | active | finalized | handed-off-to-rule-based
    pub ready: Option<String>,
    pub version: Option<String>,
    pub stats: Option<HeadStats>,
    pub stats_at: Option<Instant>,
    pub latest_block: Option<LatestBlockInfo>,
    /// Newest HARD_CONFIRMED block at this peer (hard confirmations form a
    /// prefix of the chain, so this is the exact hard-confirmation frontier).
    pub hard_tip: Option<u64>,
    pub req_hist: Ring,
    /// blocks/s in tenths, so sub-1 rates still register on the sparkline.
    pub blk_hist_x10: Ring,
    pub mempool_hist: Ring,
    /// Highest sequencer headroom ever seen: a proxy for window capacity.
    pub max_headroom: i64,
    /// Set when rejectedBackpressure grew between two consecutive samples.
    pub backpressure_seen_at: Option<Instant>,
}

impl HeadState {
    pub fn new(name: String) -> Self {
        Self {
            name,
            reachable: None,
            ready: None,
            version: None,
            stats: None,
            stats_at: None,
            latest_block: None,
            hard_tip: None,
            req_hist: Ring::default(),
            blk_hist_x10: Ring::default(),
            mempool_hist: Ring::default(),
            max_headroom: 0,
            backpressure_seen_at: None,
        }
    }
}

pub struct App {
    pub heads: Vec<HeadState>,
    pub selected: usize,
    pub screen: Screen,
    /// Current alerts, recomputed once per UI tick (main loop), read by draw.
    pub alerts: Vec<crate::alerts::Alert>,
    /// Raised/cleared transitions, newest last. Loaded from and saved to the
    /// state file, capped at ALERT_LOG_CAP.
    pub alert_log: VecDeque<AlertEvent>,
}

impl App {
    pub fn new(heads: Vec<String>) -> Self {
        Self {
            heads: heads.into_iter().map(HeadState::new).collect(),
            selected: 0,
            screen: Screen::default(),
            alerts: Vec::new(),
            alert_log: VecDeque::new(),
        }
    }
}
