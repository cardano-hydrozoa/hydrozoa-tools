//! Data collection. Every source is an HTTP poller running as a tokio task,
//! updates its own slice of the shared state, and degrades to "unavailable"
//! on its own without taking the others with it.
//!
//! All of it is hydrozoa's own HTTP surface: `/head/stats`, `/ready`,
//! `/version`, `/head/blocks/{n}`.

use crate::config::{Config, HeadTarget};
use crate::state::{HeadStats, LatestBlockInfo, Shared};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::time::{Duration, Instant};

pub fn spawn_all(rt: &tokio::runtime::Runtime, cfg: &Config, app: &Shared) {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .expect("failed to build http client");

    for (i, head) in cfg.heads.iter().enumerate() {
        rt.spawn(head_stats_loop(
            i,
            head.clone(),
            client.clone(),
            app.clone(),
        ));
        rt.spawn(head_block_loop(
            i,
            head.clone(),
            client.clone(),
            app.clone(),
        ));
    }
}

async fn head_stats_loop(i: usize, head: HeadTarget, client: reqwest::Client, app: Shared) {
    loop {
        let stats: Option<HeadStats> =
            match client.get(format!("{}/head/stats", head.url)).send().await {
                Ok(r) if r.status().is_success() => r.json().await.ok(),
                _ => None,
            };
        let ready: Option<String> = match client.get(format!("{}/ready", head.url)).send().await {
            Ok(r) => r
                .json::<Value>()
                .await
                .ok()
                .and_then(|v| v["status"].as_str().map(str::to_string)),
            Err(_) => None,
        };
        let need_version = { app.lock().unwrap().heads[i].version.is_none() };
        let version: Option<String> = if need_version {
            match client.get(format!("{}/version", head.url)).send().await {
                Ok(r) => r
                    .json::<Value>()
                    .await
                    .ok()
                    .and_then(|v| v["version"].as_str().map(str::to_string)),
                Err(_) => None,
            }
        } else {
            None
        };

        {
            let mut a = app.lock().unwrap();
            let h = &mut a.heads[i];
            match stats {
                Some(s) => {
                    h.reachable = Some(true);
                    h.stats_at = Some(Instant::now());
                    h.req_hist.push(s.local_requests.rate.now.round() as u64);
                    h.blk_hist_x10
                        .push((s.blocks.block_rate.now * 10.0).round() as u64);
                    h.mempool_hist.push(s.mempool_size.max(0) as u64);
                    h.max_headroom = h.max_headroom.max(s.sequencer_headroom);
                    if let Some(prev) = &h.stats
                        && s.local_requests.rejected_backpressure
                            > prev.local_requests.rejected_backpressure
                    {
                        h.backpressure_seen_at = Some(Instant::now());
                    }
                    h.stats = Some(s);
                }
                None => h.reachable = Some(false),
            }
            if ready.is_some() {
                h.ready = ready;
            } else if h.reachable == Some(false) {
                h.ready = None;
            }
            if version.is_some() {
                h.version = version;
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Track the newest block and keep its details (version, confirmation status,
/// fallback deadline) fresh, galloping forward from the last tip found.
async fn head_block_loop(i: usize, head: HeadTarget, client: reqwest::Client, app: Shared) {
    let mut last_tip: Option<u64> = None;
    let mut last_hard: Option<u64> = None;
    let mut tick = 0u64;
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let hint = last_tip.unwrap_or(1);
        let Some(tip) = find_tip(&client, &head.url, hint).await else {
            continue;
        };
        last_tip = Some(tip);
        if let Some(v) = fetch_block(&client, &head.url, tip).await {
            let info = parse_block(&v);
            app.lock().unwrap().heads[i].latest_block = Some(info);
        }
        // The hard-confirmation frontier moves only on stacks (minutes), so
        // search it every 5th poll. Hard confirmations form a prefix of the
        // chain, which makes the exponential search exact.
        if tick.is_multiple_of(5) {
            last_hard = find_last(
                &client,
                &head.url,
                last_hard.unwrap_or(1),
                is_hard_confirmed,
            )
            .await;
            app.lock().unwrap().heads[i].hard_tip = last_hard;
        }
        tick += 1;
    }
}

async fn fetch_block(client: &reqwest::Client, base: &str, n: u64) -> Option<Value> {
    match client.get(format!("{base}/head/blocks/{n}")).send().await {
        Ok(r) if r.status().is_success() => r.json().await.ok(),
        _ => None,
    }
}

/// Exponential search for the highest block number whose details pass `test`,
/// starting from `hint`. Works for any monotone-prefix property (existence;
/// HARD_CONFIRMED status). A handful of small requests per call (log of the
/// distance from the hint).
async fn find_last(
    client: &reqwest::Client,
    base: &str,
    hint: u64,
    test: fn(&Value) -> bool,
) -> Option<u64> {
    let passes = |n: u64| async move {
        match fetch_block(client, base, n).await {
            Some(v) => test(&v),
            None => false,
        }
    };
    let mut lo = hint.max(1);
    if !passes(lo).await {
        loop {
            if lo <= 1 {
                return None;
            }
            lo /= 2;
            if passes(lo).await {
                break;
            }
        }
    }
    let mut step = 1u64;
    while passes(lo + step).await {
        lo += step;
        step = (step * 2).min(4096);
    }
    let mut hi = lo + step;
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if passes(mid).await {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    Some(lo)
}

async fn find_tip(client: &reqwest::Client, base: &str, hint: u64) -> Option<u64> {
    find_last(client, base, hint, |_| true).await
}

fn is_hard_confirmed(v: &Value) -> bool {
    v["status"]["type"].as_str() == Some("HARD_CONFIRMED")
}

fn parse_iso(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn parse_block(v: &Value) -> LatestBlockInfo {
    LatestBlockInfo {
        number: v["number"].as_u64().unwrap_or(0),
        block_type: v["blockType"].as_str().unwrap_or("?").to_string(),
        status: v["status"]["type"].as_str().unwrap_or("?").to_string(),
        version_major: v["header"]["versionMajor"].as_i64().unwrap_or(-1),
        version_minor: v["header"]["versionMinor"].as_i64().unwrap_or(-1),
        fallback_at: v["header"]["fallbackTxStartTime"]
            .as_str()
            .and_then(parse_iso),
        forced_major_at: v["header"]["forcedMajorBlockWakeupTime"]
            .as_str()
            .and_then(parse_iso),
    }
}
