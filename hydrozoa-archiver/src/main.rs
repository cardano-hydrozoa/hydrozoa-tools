//! The archiver binary: load config, open the store, and run passes until stopped.

use anyhow::{Context as _, Result};
use clap::Parser;
use hydrozoa_archiver::{ArchiveStore, config, pass, watermark};
use hydrozoa_store::{HeadStore, SUPPORTED_STORE_VERSION, unique_secondary};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = config::Args::parse();
    let cfg = config::load(&args.config)?;

    let secondary = unique_secondary(&cfg.secondary_path);
    let source = HeadStore::open(&cfg.store_path, &secondary)
        .with_context(|| format!("opening the node's store at {}", cfg.store_path.display()))?;
    source.catch_up()?;

    let meta = source.meta()?;
    if meta.version != SUPPORTED_STORE_VERSION {
        warn!(
            found = meta.version,
            supported = SUPPORTED_STORE_VERSION,
            "the node's store is a schema version this build does not know; \
             refusing rather than copying bytes it may be misreading"
        );
        anyhow::bail!(
            "store schema version {} is not {}",
            meta.version,
            SUPPORTED_STORE_VERSION
        );
    }
    // Every family, not just the journals: the archive is meant to restore a node, which needs the
    // snapshots, confirmations and reverse indices too.
    let families = source.families().to_vec();
    if !source.unknown_families().is_empty() {
        warn!(
            families = ?source.unknown_families(),
            "the node's store holds column families this build does not recognise; \
             they will NOT be archived"
        );
    }

    if let Some(identity) = meta.identity.as_ref() {
        info!(
            head_id = %identity.head_id,
            peer = %identity.own_peer_id,
            families = families.len(),
            "archiving"
        );
    }

    if args.dry_run {
        return dry_run(&source);
    }

    let archive = ArchiveStore::open(&cfg.archive_path, &families, &meta)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()?;

    let mut last_full: Option<Instant> = None;
    loop {
        let full = args.once
            || last_full.is_none_or(|t| t.elapsed() >= Duration::from_secs(cfg.full_interval_secs));

        match pass::run(&source, &archive, full) {
            Ok(report) => {
                if full {
                    last_full = Some(Instant::now());
                }
                if report.total_copied() > 0 || full {
                    info!(
                        copied = report.total_copied(),
                        families = report.copied.len(),
                        full,
                        "pass complete"
                    );
                }
                for gap in &report.gaps {
                    error!("{gap}");
                }
                send_watermark(&client, &cfg, &report);
            }
            // A pass is all-or-nothing only per batch, and every batch is idempotent -- a retry
            // re-copies from the archive's own tip. So a failed pass is logged and retried, never
            // fatal: taking the archiver down is what turns a transient read error into an
            // unbounded store.
            Err(e) => error!(err = ?e, "pass failed; retrying"),
        }

        if args.once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(cfg.tail_interval_secs));
    }
}

/// Post the pass's watermarks, if a node is configured. Failure is logged, never fatal: the archive
/// is already durable, and the only consequence of an unsent report is that the node retains more.
fn send_watermark(
    client: &reqwest::blocking::Client,
    cfg: &config::Config,
    report: &pass::PassReport,
) {
    let Some(node_url) = cfg.node_url.as_deref() else {
        return;
    };
    let body = watermark::WatermarkReport::from_pass(report);
    if body.is_empty() {
        return;
    }
    let credentials = cfg
        .admin_username
        .as_deref()
        .zip(cfg.admin_password.as_deref());

    match watermark::report(client, node_url, credentials, &body) {
        Ok(response) if !response.effective_floor.is_empty() => {
            info!(floor = ?response.effective_floor, "the node adopted a retention floor")
        }
        Ok(_) => info!(families = body.watermarks.len(), "watermark reported"),
        Err(e) => warn!(err = ?e, "could not report the watermark; the node will retain more"),
    }
}

/// Report what a pass would copy, touching nothing.
fn dry_run(source: &HeadStore) -> Result<()> {
    println!("{:<28} {:>12} {:>12}", "family", "floor", "tip");
    for cf in source.families() {
        if cf.is_journal() {
            let floor = source.floor(*cf)?;
            let tip = source.tip(*cf)?;
            println!(
                "{:<28} {:>12} {:>12}",
                cf.name(),
                floor.map(|f| f.to_string()).unwrap_or_else(|| "-".into()),
                tip.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
            );
        } else {
            let count = source.scan_raw(*cf)?.count();
            println!("{:<28} {:>12} {:>12}", cf.name(), "-", count);
        }
    }
    Ok(())
}
