//! The tiny cross-run state file: last-viewed screen and the alert log.
//! Lives in $XDG_STATE_HOME/hztop/state.json (~/.local/state by default).
//! Best-effort on both ends — a missing or corrupt file just means defaults.

use crate::state::{ALERT_LOG_CAP, AlertEvent, Screen};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default)]
pub struct Persisted {
    #[serde(default)]
    pub screen: Option<char>,
    #[serde(default)]
    pub alert_log: Vec<AlertEvent>,
}

fn state_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("hztop/state.json"))
}

pub fn load() -> (Option<Screen>, VecDeque<AlertEvent>) {
    let Some(p) = state_path() else {
        return (None, VecDeque::new());
    };
    let Ok(bytes) = std::fs::read(&p) else {
        return (None, VecDeque::new());
    };
    let s: Persisted = serde_json::from_slice(&bytes).unwrap_or_default();
    let mut log: VecDeque<AlertEvent> = s.alert_log.into();
    while log.len() > ALERT_LOG_CAP {
        log.pop_front();
    }
    (s.screen.and_then(Screen::from_key), log)
}

/// Write the session's screen and alert log, replacing what is there.
///
/// Write-to-temp then rename, so a crash mid-write leaves the previous file
/// rather than a truncated one. Two hztop sessions on the same box overwrite
/// each other's newest events, which an ops log tolerates.
pub fn save(screen: Screen, alert_log: &VecDeque<AlertEvent>) {
    let Some(p) = state_path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let s = Persisted {
        screen: Some(screen.key()),
        alert_log: alert_log.iter().cloned().collect(),
    };
    if let Ok(json) = serde_json::to_vec_pretty(&s) {
        let tmp = p.with_extension(format!("tmp{}", std::process::id()));
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}
