//! One archival pass: catch up once, copy what is new, fsync.

use crate::archive::ArchiveStore;
use anyhow::{Context as _, Result};
use hydrozoa_store::{Cf, HeadStore};
use std::collections::BTreeMap;
use tracing::{info, warn};

/// How many entries to accumulate before committing. Each commit fsyncs, so this trades archive
/// durability granularity against syscalls; a crash costs at most one batch of re-copying.
const BATCH: usize = 1024;

/// What one pass did.
#[derive(Debug, Default)]
pub struct PassReport {
    /// Highest index now durably archived, per journal. This is what gets reported to the node.
    pub watermarks: BTreeMap<Cf, u64>,
    /// Entries copied this pass, per family.
    pub copied: BTreeMap<Cf, u64>,
    /// Journals where the node had already deleted entries this archive never saw.
    pub gaps: Vec<Gap>,
}

impl PassReport {
    pub fn total_copied(&self) -> u64 {
        self.copied.values().sum()
    }
}

/// A journal whose oldest surviving entry sits above where this archive left off.
///
/// The node deleted data the archive never took a copy of. Whatever was in that range is gone from
/// both sides, and no later pass can recover it — so this is reported, never silently skipped.
#[derive(Debug, Clone)]
pub struct Gap {
    pub cf: Cf,
    /// The next index this archive expected to copy.
    pub expected_from: u64,
    /// The oldest index the node still holds.
    pub node_floor: u64,
}

impl std::fmt::Display for Gap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}: entries {}..{} were deleted before this archive copied them",
            self.cf,
            self.expected_from,
            self.node_floor - 1
        )
    }
}

/// Run one pass.
///
/// `full` copies the non-journal families too. They have no index to resume from — snapshots are
/// rewritten in place, confirmations key by block or stack, the reverse indices key by request id —
/// so each one costs a whole-family scan and they run on a slower cadence than the journal tail.
///
/// **One catch-up, at the top, for the whole pass.** A secondary's view only moves when told to, so
/// everything copied below comes from a single consistent moment: the snapshots line up with the
/// journals they are supposed to agree with, by construction rather than by timing. Catching up
/// per family instead would silently interleave two different moments into one archive.
pub fn run(source: &HeadStore, archive: &ArchiveStore, full: bool) -> Result<PassReport> {
    source.catch_up()?;

    let mut report = PassReport::default();

    for cf in source.journals() {
        copy_journal(source, archive, cf, &mut report)?;
    }

    if full {
        for cf in source.families() {
            if !cf.is_journal() {
                copy_family(source, archive, *cf, &mut report)?;
            }
        }
    }

    Ok(report)
}

/// Copy everything new in one journal, resuming from what the archive already holds.
fn copy_journal(
    source: &HeadStore,
    archive: &ArchiveStore,
    cf: Cf,
    report: &mut PassReport,
) -> Result<()> {
    let archived_tip = archive.tip(cf)?;
    let node_floor = source.floor(cf)?;

    // Where to resume. A fresh archive starts at the node's own floor rather than 0: a store that
    // has already been trimmed has no entries below it, and asking for them would look like a gap
    // on every pass forever.
    let from = match (archived_tip, node_floor) {
        (Some(tip), _) => tip + 1,
        (None, Some(floor)) => {
            if floor > 0 {
                info!(%cf, floor, "archive is empty; starting from the node's retention floor");
            }
            floor
        }
        (None, None) => 0,
    };

    // A gap only counts once the archive has something to be behind: an empty archive starting
    // above 0 is late, not holed. Here the node has deleted a range this archive already expected.
    if let (Some(_), Some(floor)) = (archived_tip, node_floor)
        && floor > from
    {
        let gap = Gap {
            cf,
            expected_from: from,
            node_floor: floor,
        };
        warn!(%cf, expected_from = from, node_floor = floor, "{gap}");
        report.gaps.push(gap);
    }

    let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(BATCH);
    let mut highest = archived_tip;
    let mut copied = 0u64;

    for entry in source.scan(cf, from)? {
        let entry = entry?;
        // Stored framed, exactly as the node stored it: the arrival stamp is what orders entries
        // across journals at replay, so an archive that drops it cannot be restored from.
        batch.push((
            hydrozoa_store::journal::encode_key(cf, entry.index)?,
            entry.framed(),
        ));
        highest = Some(entry.index);
        copied += 1;

        if batch.len() >= BATCH {
            archive.put_raw(cf, &batch)?;
            batch.clear();
        }
    }
    archive.put_raw(cf, &batch)?;

    if copied > 0 {
        report.copied.insert(cf, copied);
    }
    if let Some(tip) = highest {
        report.watermarks.insert(cf, tip);
    }
    Ok(())
}

/// Copy a non-journal family wholesale.
///
/// No resume point exists for these: `DepositMap` and `Treasury` are single blobs rewritten in
/// place, so the newest value simply replaces the archived one. Re-writing an unchanged key is
/// cheap and idempotent, which is why this needs no change detection.
fn copy_family(
    source: &HeadStore,
    archive: &ArchiveStore,
    cf: Cf,
    report: &mut PassReport,
) -> Result<()> {
    let mut batch: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(BATCH);
    let mut copied = 0u64;

    for kv in source
        .scan_raw(cf)
        .with_context(|| format!("scanning {cf}"))?
    {
        batch.push(kv?);
        copied += 1;
        if batch.len() >= BATCH {
            archive.put_raw(cf, &batch)?;
            batch.clear();
        }
    }
    archive.put_raw(cf, &batch)?;

    if copied > 0 {
        report.copied.insert(cf, copied);
    }
    Ok(())
}
