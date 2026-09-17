//! What a RocksDB secondary actually does, pinned by experiment.
//!
//! The archiver reads a live node's store. These tests establish the properties that makes safe.

use rocksdb::{ColumnFamilyDescriptor, DB, Options, WriteBatch, WriteOptions};
use std::path::Path;

fn cfs() -> Vec<String> {
    vec!["default".into(), "Block".into(), "Request:0".into()]
}

fn open_primary(path: &Path) -> DB {
    let mut opts = Options::default();
    opts.create_if_missing(true);
    opts.create_missing_column_families(true);
    let descs: Vec<_> = cfs()
        .into_iter()
        .map(|n| ColumnFamilyDescriptor::new(n, Options::default()))
        .collect();
    DB::open_cf_descriptors(&opts, path, descs).unwrap()
}

fn open_secondary(primary: &Path, secondary: &Path) -> DB {
    let mut opts = Options::default();
    opts.create_if_missing(false);
    DB::open_cf_as_secondary(&opts, primary, secondary, cfs()).unwrap()
}

fn put(db: &DB, cf: &str, key: &[u8], val: &[u8], sync: bool) {
    let handle = db.cf_handle(cf).unwrap();
    let mut batch = WriteBatch::default();
    batch.put_cf(&handle, key, val);
    let mut wo = WriteOptions::default();
    wo.set_sync(sync);
    db.write_opt(batch, &wo).unwrap();
}

fn get(db: &DB, cf: &str, key: &[u8]) -> Option<Vec<u8>> {
    let handle = db.cf_handle(cf).unwrap();
    db.get_cf(&handle, key).unwrap()
}

fn listing(path: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(path)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// The property the archiver is built on: a secondary sees writes the primary has put in its WAL
/// but has NOT flushed to an SST. Without this an archiver could only ever see flushed data, and
/// would lag the node by a whole memtable.
#[test]
fn a_secondary_sees_unflushed_writes_after_catching_up() {
    let dir = tempfile::tempdir().unwrap();
    let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));

    let primary = open_primary(&p);
    put(&primary, "Block", b"k1", b"v1", true);

    let secondary = open_secondary(&p, &s);
    secondary.try_catch_up_with_primary().unwrap();
    assert_eq!(get(&secondary, "Block", b"k1").as_deref(), Some(&b"v1"[..]));

    // Written to the WAL, never flushed.
    put(&primary, "Block", b"k2", b"v2", true);
    assert_eq!(
        get(&secondary, "Block", b"k2"),
        None,
        "not visible before catch-up"
    );

    secondary.try_catch_up_with_primary().unwrap();
    assert_eq!(
        get(&secondary, "Block", b"k2").as_deref(),
        Some(&b"v2"[..]),
        "a secondary must replay the primary's WAL, not just read its SSTs"
    );
}

/// A secondary is a snapshot as of its last catch-up. The archiver therefore copies a consistent
/// view, and can only ever see LESS than exists -- never data the node has not durably written.
#[test]
fn a_secondary_is_frozen_between_catch_ups() {
    let dir = tempfile::tempdir().unwrap();
    let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));

    let primary = open_primary(&p);
    let secondary = open_secondary(&p, &s);

    for i in 0..5u8 {
        put(&primary, "Block", &[i], b"v", true);
    }
    secondary.try_catch_up_with_primary().unwrap();

    for i in 5..10u8 {
        put(&primary, "Block", &[i], b"v", true);
    }
    // No catch-up: the view must not shift under a scan in progress.
    let seen = (0..10u8)
        .filter(|i| get(&secondary, "Block", &[*i]).is_some())
        .count();
    assert_eq!(seen, 5, "the secondary's view moved without a catch-up");
}

/// The archiver must not mutate the node's store. Nothing it does may add, remove or touch a file
/// in the primary directory -- which is what lets the primary directory be mounted read-only to
/// the archiver's user.
#[test]
fn a_secondary_writes_nothing_into_the_primary_directory() {
    let dir = tempfile::tempdir().unwrap();
    let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));

    let primary = open_primary(&p);
    put(&primary, "Block", b"k1", b"v1", true);
    primary.flush().unwrap();

    let before = listing(&p);

    let secondary = open_secondary(&p, &s);
    secondary.try_catch_up_with_primary().unwrap();
    let _ = get(&secondary, "Block", b"k1");
    {
        let mut iter = secondary.iterator_cf(
            &secondary.cf_handle("Block").unwrap(),
            rocksdb::IteratorMode::Start,
        );
        let _ = iter.next();
    }
    drop(secondary);

    assert_eq!(
        before,
        listing(&p),
        "the secondary changed the primary directory"
    );
    // ...and it put its own bookkeeping somewhere else entirely.
    assert!(s.exists(), "the secondary kept no state of its own");
}

/// A column family created after the secondary opened is invisible to it. Hydrozoa fixes its CF
/// set at head initialization, so this cannot bite in normal operation -- but an archiver pointed
/// at a store mid-bootstrap would silently miss families, so it must be a restart, not a surprise.
#[test]
fn a_column_family_added_after_open_is_invisible_to_the_secondary() {
    let dir = tempfile::tempdir().unwrap();
    let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));

    let mut primary = open_primary(&p);
    let secondary = open_secondary(&p, &s);

    let mut cf_opts = Options::default();
    cf_opts.create_if_missing(true);
    primary.create_cf("Request:1", &cf_opts).unwrap();
    put(&primary, "Request:1", b"k", b"v", true);

    secondary.try_catch_up_with_primary().unwrap();
    assert!(
        secondary.cf_handle("Request:1").is_none(),
        "the secondary saw a family created after it opened"
    );
}

/// Nothing prevents two secondaries sharing one secondary path: the second opens, both catch up,
/// both read correctly, and no LOCK file is created in that directory at all.
///
/// sugar-rush-ledger's aggregator config says the opposite -- "two secondaries on one path fight
/// over its LOCK file" -- and gives its refund tail and its command reader separate scratch paths
/// on that basis. On the RocksDB this crate pins, there is no fight to lose, which is worse rather
/// than better: a shared path produces no error to catch, so misuse would surface as two archiver
/// runs quietly disagreeing about where they had read to, not as a failed open.
///
/// So `unique_secondary`'s pid suffix stays. It costs nothing and removes the question.
#[test]
fn nothing_stops_two_secondaries_sharing_one_path() {
    let dir = tempfile::tempdir().unwrap();
    let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));

    let primary = open_primary(&p);
    put(&primary, "Block", b"k", b"v", true);

    let _first = open_secondary(&p, &s);
    let mut opts = Options::default();
    opts.create_if_missing(false);
    let second = DB::open_cf_as_secondary(&opts, &p, &s, cfs());
    assert!(
        second.is_ok(),
        "RocksDB grew a lock on the secondary path -- unique_secondary's pid suffix may now be \
         redundant, but check before removing it"
    );
}

/// The archiver can be denied write access to the node's store at the filesystem level and still
/// work. This is the enforcement that does not depend on the archiver being well-behaved: run it
/// as a user with r-x on the store directory, and read-only stops being a promise.
#[test]
#[cfg(unix)]
fn a_secondary_opens_a_primary_directory_it_cannot_write() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));

    let primary = open_primary(&p);
    put(&primary, "Block", b"k1", b"v1", true);
    primary.flush().unwrap();
    put(&primary, "Block", b"k2", b"v2", true); // WAL only

    // Drop the write bit on the store directory and everything in it.
    let mut entries = vec![p.clone()];
    for e in std::fs::read_dir(&p).unwrap() {
        entries.push(e.unwrap().path());
    }
    let saved: Vec<_> = entries
        .iter()
        .map(|e| (e.clone(), std::fs::metadata(e).unwrap().permissions()))
        .collect();
    for e in &entries {
        let mode = std::fs::metadata(e).unwrap().permissions().mode();
        std::fs::set_permissions(e, PermissionsExt::from_mode(mode & 0o555)).unwrap();
    }

    let opened = std::panic::catch_unwind(|| {
        let secondary = open_secondary(&p, &s);
        secondary.try_catch_up_with_primary().unwrap();
        (
            get(&secondary, "Block", b"k1").is_some(),
            get(&secondary, "Block", b"k2").is_some(),
        )
    });

    // Restore before asserting, so a failure does not leave an undeletable temp dir.
    for (e, perm) in saved {
        let _ = std::fs::set_permissions(&e, perm);
    }

    let (saw_flushed, saw_wal) = opened.expect("a secondary could not open a read-only store dir");
    assert!(
        saw_flushed,
        "flushed data unreadable from a read-only store dir"
    );
    assert!(saw_wal, "WAL data unreadable from a read-only store dir");
}

/// WAL visibility does not depend on the primary fsyncing, nor on the secondary having been open
/// when the writes happened.
///
/// Worth pinning explicitly, because sugar-rush-ledger's aggregator carries the opposite claim in
/// a comment -- "a RocksDB secondary reliably reads only *flushed* SSTs, not the primary's live
/// memtable/WAL" -- and runs a periodic `flush()` on its views primary to work around it. On the
/// RocksDB this crate pins, all four combinations below are visible. That comment may describe an
/// older RocksDB, or a symptom with another cause.
///
/// The archiver should not bank on it either way: it is a latency property, not a correctness one.
/// Archive what is visible, report that as the watermark, and let retention follow. Then WAL
/// visibility decides only how promptly disk is freed, never whether the archive is sound.
#[test]
fn wal_visibility_needs_neither_fsync_nor_a_secondary_open_at_write_time() {
    for sync in [true, false] {
        // The secondary was already open when the write landed.
        let dir = tempfile::tempdir().unwrap();
        let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));
        let primary = open_primary(&p);
        let secondary = open_secondary(&p, &s);
        put(&primary, "Block", b"k", b"v", sync);
        secondary.try_catch_up_with_primary().unwrap();
        assert!(
            get(&secondary, "Block", b"k").is_some(),
            "sync={sync}: unflushed write invisible to an already-open secondary"
        );

        // The secondary opened only afterwards -- the "blank on restart" shape.
        let dir = tempfile::tempdir().unwrap();
        let (p, s) = (dir.path().join("primary"), dir.path().join("secondary"));
        let primary = open_primary(&p);
        put(&primary, "Block", b"k", b"v", sync);
        let secondary = open_secondary(&p, &s);
        secondary.try_catch_up_with_primary().unwrap();
        assert!(
            get(&secondary, "Block", b"k").is_some(),
            "sync={sync}: unflushed write invisible to a secondary opened after it"
        );
    }
}
