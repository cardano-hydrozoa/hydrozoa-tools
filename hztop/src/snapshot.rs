//! One JSON document describing everything the dashboard knows.
//!
//! Emitted by `--json` (and whenever stdout is not a terminal), so a script or
//! an agent reads the same state the terminal draws. One builder, so the two
//! can never drift.

use crate::state::App;
use chrono::Utc;
use serde_json::{Value, json};

pub fn build(a: &App) -> Value {
    let now = Utc::now().timestamp();
    let active_for = |key: &str| -> Option<i64> {
        a.alert_log
            .iter()
            .rev()
            .find(|e| !e.cleared && e.ident() == key)
            .map(|e| (now - e.at_unix).max(0))
    };
    let heads: Vec<Value> = a
        .heads
        .iter()
        .map(|h| {
            json!({
                "name": h.name,
                "reachable": h.reachable,
                "ready": h.ready,
                "version": h.version,
                "tip": h.latest_block.as_ref().map(|b| json!({
                    "number": b.number,
                    "block_type": b.block_type,
                    "status": b.status,
                    "version": format!("{}.{}", b.version_major, b.version_minor),
                    "fallback_in_secs": b.fallback_at.map(|t| (t - Utc::now()).num_seconds()),
                })),
                "hard_tip": h.hard_tip,
                "stats": h.stats.as_ref().map(|s| json!({
                    "uptime_seconds": s.uptime_seconds,
                    "req_now": s.local_requests.rate.now,
                    "req_1m": s.local_requests.rate.load1m,
                    "blocks_now": s.blocks.block_rate.now,
                    "minor": s.blocks.minor,
                    "major": s.blocks.major,
                    "mempool": s.mempool_size,
                    "headroom": s.sequencer_headroom,
                    "secs_since_hard": s.stacks.seconds_since_last_hard_confirm,
                    "soft_ms": s.block_timings.soft_consensus.avg_millis,
                    "rejected_screening": s.local_requests.rejected_screening,
                    "rejected_backpressure": s.local_requests.rejected_backpressure,
                })),
                "req_hist": h.req_hist.tail(60),
                // Who is actually driving this head. `local` is listed
                // alongside the peers deliberately -- the interesting reading
                // is the SHARE, and a peer row means nothing without the local
                // row beside it.
                "peers": h.stats.as_ref().map(|s| {
                    let mut rows = vec![json!({
                        "peer": "local",
                        "total": s.local_requests.total,
                        "now": s.local_requests.rate.now,
                        "load1m": s.local_requests.rate.load1m,
                        "load5m": s.local_requests.rate.load5m,
                        "live": true,
                    })];
                    rows.extend(s.peer_requests.iter().map(|p| json!({
                        "peer": p.peer,
                        "total": p.total,
                        "now": p.rate.now,
                        "load1m": p.rate.load1m,
                        "load5m": p.rate.load5m,
                        // A peer that has sent nothing for a minute is not
                        // necessarily gone, but it is not carrying load either,
                        // and that is the distinction worth seeing.
                        "live": p.rate.load1m > 0.0,
                    })));
                    rows
                }),
                // The slowest blocks this head led. A tail here is where a
                // consensus stall shows up first -- long before the alert
                // thresholds fire.
                "slowest": h.stats.as_ref().map(|s| s.block_timings.lead.top.iter().take(8).map(|t| json!({
                    "block_number": t.block_number,
                    "millis": t.millis,
                    "requests": t.requests,
                })).collect::<Vec<_>>()),
            })
        })
        .collect();

    json!({
        "generated_at": now,
        "alerts": a.alerts.iter().map(|al| json!({
            "crit": al.crit,
            "text": al.text,
            "active_secs": active_for(&al.key),
        })).collect::<Vec<_>>(),
        "alert_log": a.alert_log.iter().rev().take(50).map(|e| json!({
            "at": e.at_unix, "text": e.text, "crit": e.crit, "cleared": e.cleared,
        })).collect::<Vec<_>>(),
        "heads": heads,
    })
}
