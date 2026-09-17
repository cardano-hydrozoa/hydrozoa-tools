//! End-to-end: build a store shaped like hydrozoa's, archive it, and check what came out.

use hydrozoa_store::{Cf, HeadStore, PeerId, journal, meta, unique_secondary};
use rocksdb::{ColumnFamilyDescriptor, DB, Options, WriteBatch, WriteOptions};
use std::path::{Path, PathBuf};

use hydrozoa_archiver::{ArchiveStore, pass};

/// The families a small two-peer head would open.
fn families() -> Vec<Cf> {
    let mut v = hydrozoa_store::cf::FIXED.to_vec();
    v.extend([
        Cf::Request(0),
        Cf::Request(1),
        Cf::SoftAck(0),
        Cf::HardAck(PeerId::Head(0)),
        Cf::HardAck(PeerId::Coil(3)),
        Cf::HubHardAck(0),
    ]);
    v.sort();
    v
}

struct Node {
    db: DB,
    path: PathBuf,
}

impl Node {
    fn create(path: &Path) -> Node {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);
        let mut names: Vec<String> = families().iter().map(|c| c.name()).collect();
        names.push("default".into());
        let descs: Vec<_> = names
            .into_iter()
            .map(|n| ColumnFamilyDescriptor::new(n, Options::default()))
            .collect();
        let db = DB::open_cf_descriptors(&opts, path, descs).unwrap();
        let node = Node {
            db,
            path: path.to_path_buf(),
        };
        node.stamp_identity();
        node
    }

    /// The identity stamp hydrozoa writes on a fresh store.
    fn stamp_identity(&self) {
        let h = self.db.cf_handle(&Cf::Meta.name()).unwrap();
        let mut b = WriteBatch::default();
        b.put_cf(&h, meta::VERSION_KEY, 3u32.to_be_bytes());
        b.put_cf(&h, meta::HEAD_PARAMS_HASH_KEY, b"deadbeef");
        b.put_cf(&h, meta::HEAD_ID_KEY, b"cafe01");
        b.put_cf(&h, meta::HEAD_ADDRESS_KEY, b"addr_test1qxyz");
        // Head peer 1 -> wire int 3.
        b.put_cf(&h, meta::OWN_PEER_ID_KEY, 3u32.to_be_bytes());
        self.write(b);
    }

    fn write(&self, batch: WriteBatch) {
        let mut wo = WriteOptions::default();
        wo.set_sync(true);
        self.db.write_opt(batch, &wo).unwrap();
    }

    /// Append one journal entry, framed the way hydrozoa frames them.
    fn append(&self, cf: Cf, index: u64, generation: u32, nanos: u64, payload: &[u8]) {
        let stamp = journal::ArrivalStamp {
            generation,
            monotonic_nanos: nanos,
        };
        let mut framed = stamp.to_bytes().to_vec();
        framed.extend_from_slice(payload);
        let h = self.db.cf_handle(&cf.name()).unwrap();
        let mut b = WriteBatch::default();
        b.put_cf(&h, journal::encode_key(cf, index).unwrap(), framed);
        self.write(b);
    }

    /// Write into a non-journal family, which carries no stamp prefix.
    fn put_plain(&self, cf: Cf, key: &[u8], value: &[u8]) {
        let h = self.db.cf_handle(&cf.name()).unwrap();
        let mut b = WriteBatch::default();
        b.put_cf(&h, key, value);
        self.write(b);
    }

    /// Delete a journal range, as retention will.
    fn trim(&self, cf: Cf, below: u64) {
        let h = self.db.cf_handle(&cf.name()).unwrap();
        let mut b = WriteBatch::default();
        for i in 0..below {
            b.delete_cf(&h, journal::encode_key(cf, i).unwrap());
        }
        self.write(b);
    }

    fn open_reader(&self, scratch: &Path) -> HeadStore {
        let store = HeadStore::open(&self.path, &unique_secondary(scratch)).unwrap();
        store.catch_up().unwrap();
        store
    }
}

/// Read a family straight out of the archive, for comparison against the node.
fn archive_dump(path: &Path, cf: Cf) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut opts = Options::default();
    opts.create_if_missing(false);
    let names: Vec<String> = families()
        .iter()
        .map(|c| c.name())
        .chain(std::iter::once("default".to_string()))
        .collect();
    let db = DB::open_cf_for_read_only(&opts, path, &names, false).unwrap();
    let h = db.cf_handle(&cf.name()).unwrap();
    db.iterator_cf(&h, rocksdb::IteratorMode::Start)
        .map(|kv| {
            let (k, v) = kv.unwrap();
            (k.to_vec(), v.to_vec())
        })
        .collect()
}

fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let store = d.path().join("store");
    let scratch = d.path().join("scratch");
    let archive = d.path().join("archive");
    (d, store, scratch, archive)
}

/// The core promise: what comes out of the archive is byte-for-byte what went into the node,
/// arrival stamps included. Anything less and a restored journal would not replay in the same
/// order the original did.
#[test]
fn a_pass_copies_journal_entries_byte_for_byte() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    for i in 0..5u64 {
        node.append(Cf::Block, i, 1, 1000 + i, format!("block-{i}").as_bytes());
    }
    for i in 0..3u64 {
        node.append(Cf::Request(0), i, 1, 2000 + i, b"req");
    }

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();
    let report = pass::run(&source, &archive, true).unwrap();
    drop(archive);

    assert_eq!(report.watermarks.get(&Cf::Block), Some(&4));
    assert_eq!(report.watermarks.get(&Cf::Request(0)), Some(&2));
    assert!(report.gaps.is_empty());

    for cf in [Cf::Block, Cf::Request(0)] {
        let from_node: Vec<_> = source.scan_raw(cf).unwrap().map(|kv| kv.unwrap()).collect();
        assert_eq!(archive_dump(&archive_path, cf), from_node, "{cf}");
    }
}

/// A second pass resumes from the archive's own tip rather than re-copying, and needs no state
/// file to do it.
#[test]
fn a_second_pass_copies_only_what_is_new() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    for i in 0..4u64 {
        node.append(Cf::Block, i, 1, i, b"v");
    }

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();

    let first = pass::run(&source, &archive, false).unwrap();
    assert_eq!(first.copied.get(&Cf::Block), Some(&4));

    // Nothing new: a pass must copy nothing rather than re-copy everything.
    let second = pass::run(&source, &archive, false).unwrap();
    assert_eq!(second.copied.get(&Cf::Block), None);
    assert_eq!(second.watermarks.get(&Cf::Block), Some(&3));

    for i in 4..7u64 {
        node.append(Cf::Block, i, 1, i, b"v");
    }
    let third = pass::run(&source, &archive, false).unwrap();
    assert_eq!(third.copied.get(&Cf::Block), Some(&3));
    assert_eq!(third.watermarks.get(&Cf::Block), Some(&6));
}

/// The failure this whole design exists to prevent: the node deleted entries the archive never
/// copied. It must be reported, never silently skipped, because nothing can recover that range.
#[test]
fn a_range_deleted_before_it_was_archived_is_reported_as_a_gap() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    for i in 0..3u64 {
        node.append(Cf::Block, i, 1, i, b"v");
    }

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();
    pass::run(&source, &archive, false).unwrap();

    // The node moves on and trims past where the archive got to.
    for i in 3..10u64 {
        node.append(Cf::Block, i, 1, i, b"v");
    }
    node.trim(Cf::Block, 7);

    let report = pass::run(&source, &archive, false).unwrap();
    let gap = report
        .gaps
        .iter()
        .find(|g| g.cf == Cf::Block)
        .expect("the lost range was not reported");
    assert_eq!(gap.expected_from, 3);
    assert_eq!(gap.node_floor, 7);
    // It still copies what survives -- a gap is a report, not a halt.
    assert_eq!(report.watermarks.get(&Cf::Block), Some(&9));
}

/// A fresh archive against an already-trimmed store is late, not holed: it starts at the node's
/// floor and reports no gap. Without this every pass forever would claim a gap at the same place.
#[test]
fn a_fresh_archive_against_a_trimmed_store_starts_at_the_floor() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    for i in 0..10u64 {
        node.append(Cf::Block, i, 1, i, b"v");
    }
    node.trim(Cf::Block, 6);

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();
    let report = pass::run(&source, &archive, false).unwrap();

    assert!(report.gaps.is_empty(), "a late start is not a gap");
    assert_eq!(report.copied.get(&Cf::Block), Some(&4));
    assert_eq!(report.watermarks.get(&Cf::Block), Some(&9));
}

/// Non-journal families have no resume point, so a full pass rewrites them. The newest snapshot
/// must win rather than accumulate.
#[test]
fn a_rewritten_snapshot_is_replaced_not_accumulated() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    node.put_plain(Cf::DepositMap, b"deposits", b"first");

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();
    pass::run(&source, &archive, true).unwrap();

    node.put_plain(Cf::DepositMap, b"deposits", b"second");
    pass::run(&source, &archive, true).unwrap();
    drop(archive);

    assert_eq!(
        archive_dump(&archive_path, Cf::DepositMap),
        vec![(b"deposits".to_vec(), b"second".to_vec())]
    );
}

/// A tail pass leaves the non-journal families alone -- that is the whole point of the two
/// cadences, since those cost a full scan whether anything changed or not.
#[test]
fn a_tail_pass_does_not_touch_the_non_journal_families() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    node.put_plain(Cf::Treasury, b"treasury", b"v");
    node.append(Cf::Block, 0, 1, 0, b"v");

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();
    let report = pass::run(&source, &archive, false).unwrap();

    assert_eq!(report.copied.get(&Cf::Block), Some(&1));
    assert_eq!(report.copied.get(&Cf::Treasury), None);
}

/// Per-node archives all look alike on disk. The identity stamp is what stops peer 1's journals
/// being appended into peer 0's archive -- producing a file that looks whole and means something
/// else. Hydrozoa stamps its stores for this reason; the archive inherits both.
#[test]
fn an_archive_refuses_a_store_belonging_to_another_peer() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    node.append(Cf::Block, 0, 1, 0, b"v");

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();

    // Same head, different peer.
    let mut other = meta.clone();
    other.identity.as_mut().unwrap().own_peer_id = PeerId::Head(7);
    let err = match ArchiveStore::open(&archive_path, source.families(), &other) {
        Ok(_) => panic!("an archive accepted a store belonging to another peer"),
        Err(e) => e,
    };
    assert!(
        err.to_string().contains("different head"),
        "unexpected error: {err}"
    );

    // ...and a different head entirely.
    let mut other = meta.clone();
    other.identity.as_mut().unwrap().head_id = "beef99".into();
    assert!(ArchiveStore::open(&archive_path, source.families(), &other).is_err());
}

/// An archive reopened against the store it belongs to is fine, and keeps what it had.
#[test]
fn an_archive_reopens_against_its_own_store() {
    let (_d, store_path, scratch, archive_path) = dirs();
    let node = Node::create(&store_path);
    node.append(Cf::Block, 0, 1, 0, b"v");

    let source = node.open_reader(&scratch);
    let meta = source.meta().unwrap();
    {
        let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();
        pass::run(&source, &archive, true).unwrap();
    }
    let archive = ArchiveStore::open(&archive_path, source.families(), &meta).unwrap();
    assert_eq!(archive.tip(Cf::Block).unwrap(), Some(0));
}
