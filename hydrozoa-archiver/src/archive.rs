//! The archive: a second RocksDB carrying the same column families as the node's store.
//!
//! Keeping the node's own layout is what makes a restore a copy rather than a conversion — a node
//! can be pointed at a restored archive and open it. It also means this crate writes values
//! **exactly as they were stored**, arrival-stamp prefix and all, so a restored journal sorts the
//! way the original did.

use anyhow::{Context as _, Result, bail};
use hydrozoa_store::{Cf, StoreIdentity, StoreMeta, journal, meta};
use rocksdb::{ColumnFamilyDescriptor, DB, Options, WriteBatch, WriteOptions};
use std::path::Path;

/// A writable archive.
pub struct ArchiveStore {
    db: DB,
}

impl ArchiveStore {
    /// Open the archive at `path`, creating it if absent, with one family per entry in `families`.
    ///
    /// `source` is the node store's metadata. On a fresh archive it is stamped in; on an existing
    /// one it is **checked**, and a mismatch refuses the open.
    ///
    /// That check is the point of carrying `StoreIdentity` into the archive at all. Archives are
    /// per-node, so a directory of them all look alike, and without the stamp nothing stops an
    /// archiver appending peer 1's journals into peer 0's archive — producing a file that looks
    /// whole, restores onto a node, and means something else. Hydrozoa stamps its own stores for
    /// exactly this reason; an archive inherits both the stamp and the reason.
    pub fn open(path: &Path, families: &[Cf], source: &StoreMeta) -> Result<ArchiveStore> {
        let mut opts = Options::default();
        opts.create_if_missing(true);
        opts.create_missing_column_families(true);

        let descriptors: Vec<_> = families
            .iter()
            .map(|cf| ColumnFamilyDescriptor::new(cf.name(), Options::default()))
            .collect();

        let db = DB::open_cf_descriptors(&opts, path, descriptors)
            .with_context(|| format!("opening the archive at {}", path.display()))?;

        let archive = ArchiveStore { db };
        archive.stamp_or_check(source)?;
        Ok(archive)
    }

    /// Stamp a fresh archive with the source's identity, or verify an existing one matches.
    fn stamp_or_check(&self, source: &StoreMeta) -> Result<()> {
        let Some(expected) = source.identity.as_ref() else {
            bail!(
                "the node's store carries no identity stamp; refusing to archive a store whose \
                 head and peer cannot be established"
            );
        };

        match self.identity()? {
            None => self.write_identity(source.version, expected),
            Some(found) if &found == expected => Ok(()),
            Some(found) => bail!(
                "this archive belongs to a different head, configuration or peer than the store \
                 being archived.\n  archive: head {} peer {} ({})\n  store:   head {} peer {} ({})",
                found.head_id,
                found.own_peer_id,
                found.head_params_hash,
                expected.head_id,
                expected.own_peer_id,
                expected.head_params_hash,
            ),
        }
    }

    /// The archive's own identity stamp, if it has one.
    pub fn identity(&self) -> Result<Option<StoreIdentity>> {
        let handle = self.handle(Cf::Meta)?;
        let read = |key: &[u8]| -> Result<Option<Vec<u8>>> {
            self.db.get_cf(&handle, key).with_context(|| {
                format!("reading {} from the archive", String::from_utf8_lossy(key))
            })
        };
        let assembled = meta::assemble(
            read(meta::VERSION_KEY)?.as_deref(),
            read(meta::HEAD_PARAMS_HASH_KEY)?.as_deref(),
            read(meta::HEAD_ID_KEY)?.as_deref(),
            read(meta::HEAD_ADDRESS_KEY)?.as_deref(),
            read(meta::OWN_PEER_ID_KEY)?.as_deref(),
        );
        match assembled {
            Ok(m) => Ok(m.identity),
            // No store_version row at all: a brand new archive, not a broken one.
            Err(_) if read(meta::VERSION_KEY)?.is_none() => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn write_identity(&self, version: u32, identity: &StoreIdentity) -> Result<()> {
        let handle = self.handle(Cf::Meta)?;
        let mut batch = WriteBatch::default();
        batch.put_cf(&handle, meta::VERSION_KEY, version.to_be_bytes());
        batch.put_cf(
            &handle,
            meta::HEAD_PARAMS_HASH_KEY,
            identity.head_params_hash.as_bytes(),
        );
        batch.put_cf(&handle, meta::HEAD_ID_KEY, identity.head_id.as_bytes());
        batch.put_cf(
            &handle,
            meta::HEAD_ADDRESS_KEY,
            identity.head_address.as_bytes(),
        );
        batch.put_cf(
            &handle,
            meta::OWN_PEER_ID_KEY,
            identity.own_peer_id.to_wire_int().to_be_bytes(),
        );
        self.commit(batch)
    }

    /// The highest journal index this archive holds for `cf`, or `None` if it holds none.
    ///
    /// This is the archiver's resume point, and the reason it keeps no state file of its own: the
    /// archive already knows how far it got, so a crashed pass costs a little re-copying and
    /// nothing else.
    pub fn tip(&self, cf: Cf) -> Result<Option<u64>> {
        if !cf.is_journal() {
            bail!("{cf} is not a journal; it has no tip to resume from");
        }
        let handle = self.handle(cf)?;
        let mut iter = self.db.iterator_cf(&handle, rocksdb::IteratorMode::End);
        match iter.next() {
            None => Ok(None),
            Some(kv) => {
                let (key, _) = kv.with_context(|| format!("seeking the end of {cf}"))?;
                Ok(Some(journal::decode_key(cf, &key)?))
            }
        }
    }

    /// Write a batch of raw `(key, value)` pairs into `cf`, committed atomically.
    pub fn put_raw(&self, cf: Cf, entries: &[(Vec<u8>, Vec<u8>)]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let handle = self.handle(cf)?;
        let mut batch = WriteBatch::default();
        for (key, value) in entries {
            batch.put_cf(&handle, key, value);
        }
        self.commit(batch)
    }

    /// Commit a batch, fsyncing it.
    ///
    /// Every write here is synced, because the watermark reported to the node means "durably in
    /// the archive". Hydrozoa itself does not fsync — `RocksDbBackendStore` never sets the option —
    /// so the archiver's own fsync is the only durability barrier in the chain, and a watermark
    /// sent ahead of it would authorise deleting data that exists nowhere.
    fn commit(&self, batch: WriteBatch) -> Result<()> {
        let mut opts = WriteOptions::default();
        opts.set_sync(true);
        self.db
            .write_opt(batch, &opts)
            .context("writing to the archive")
    }

    fn handle(&self, cf: Cf) -> Result<impl rocksdb::AsColumnFamilyRef + use<'_>> {
        self.db
            .cf_handle(&cf.name())
            .with_context(|| format!("{cf} is not present in this archive"))
    }
}
