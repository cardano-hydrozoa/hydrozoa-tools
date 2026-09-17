//! Read a hydrozoa head's RocksDB store without stopping the node.
//!
//! The store is opened as a RocksDB **secondary**: a read-only handle that catches up from the
//! primary's WAL and manifest on demand. The primary keeps writing throughout and never learns
//! this reader exists, so nothing here can stall or corrupt a running head. That is the property
//! an archiver needs — it copies data out of a live node — and the reason it is the shape the
//! reader takes.
//!
//! # What the archiver gets
//!
//! [`HeadStore::open`] discovers the column families from the store itself rather than from a head
//! config, which the reader does not have. [`HeadStore::journals`] lists the append streams,
//! [`HeadStore::scan`] walks one from any index, and [`HeadStore::tip`] gives the highest index in
//! one — together, enough to copy a range out and record where the copy reached.
//!
//! [`HeadStore::meta`] reads the schema version and the identity stamp, so an archive can record
//! which head, under which configuration, written by which peer, it holds.
//!
//! # The storage contract
//!
//! Transcribed from hydrozoa's `multisig/persistence/` (`Cf`, `JournalKey`, `JournalValue`,
//! `ArrivalStamp`, `StoreVersion`, `StoreIdentity`) and `proto/request_record.proto`. Hydrozoa is
//! the only writer; this crate only reads.
//!
//! | family | key | value |
//! |---|---|---|
//! | `Block`, `Stack` | index, 4-byte BE | `[stamp:12][brief, circe JSON]` |
//! | `Request:<peer>` | index, 8-byte BE | `[stamp:12][RequestRecord protobuf]` |
//! | `SoftAck:<peer>`, `HardAck:<peer>`, `HubHardAck:<hub>` | index, 4-byte BE | `[stamp:12][ack, circe JSON]` |
//! | everything else | family-specific | no stamp prefix |
//!
//! Only the six journal types carry the [`journal::ArrivalStamp`] prefix. The snapshot,
//! confirmation and reverse-index families are plain key-value, and [`Cf::is_journal`] is the test
//! that keeps a reader from framing one as the other.
//!
//! Journal payloads are handed back encoded. An archiver copies them verbatim and never has to
//! understand them, so this crate carries no codec for hydrozoa's wire types — only the Request
//! lane's protobuf, which it needs to read a request's kind.

pub mod cf;
pub mod journal;
pub mod meta;
pub mod request_record;

pub use cf::{Cf, PeerId};
pub use journal::{ArrivalStamp, Entry, STAMP_WIDTH};
pub use meta::{SUPPORTED_STORE_VERSION, StoreIdentity, StoreMeta};
pub use request_record::{RequestBody, RequestRecord};

use anyhow::{Context as _, Result, bail};
use rocksdb::{DB, Direction, IteratorMode, Options};
use std::path::{Path, PathBuf};

/// A read-only secondary handle on one head peer's store.
pub struct HeadStore {
    db: DB,
    families: Vec<Cf>,
    /// Family names present on disk that this build does not recognise — reported, never skipped
    /// silently, because an unknown family is data an archiver would otherwise drop.
    unknown_families: Vec<String>,
}

impl HeadStore {
    /// Open `primary` as a secondary, keeping the secondary's own state under `secondary`.
    ///
    /// `secondary` must be a path this process may write and must not be shared with another
    /// reader — RocksDB keeps per-secondary bookkeeping there. [`unique_secondary`] builds one
    /// that will not collide.
    ///
    /// Every column family has to be named at open time, so the set is read off the store first.
    /// A family this build does not know is still opened (otherwise its data would be invisible)
    /// and listed in [`HeadStore::unknown_families`].
    pub fn open(primary: &Path, secondary: &Path) -> Result<HeadStore> {
        let mut opts = Options::default();
        opts.create_if_missing(false);

        let names = DB::list_cf(&opts, primary)
            .with_context(|| format!("listing column families of {}", primary.display()))?;

        let mut families = Vec::new();
        let mut unknown_families = Vec::new();
        for name in &names {
            if name == "default" {
                continue;
            }
            match Cf::parse(name) {
                Some(cf) => families.push(cf),
                None => unknown_families.push(name.clone()),
            }
        }
        families.sort();

        let db =
            DB::open_cf_as_secondary(&opts, primary, secondary, &names).with_context(|| {
                format!(
                    "opening {} as a secondary under {}",
                    primary.display(),
                    secondary.display()
                )
            })?;

        Ok(HeadStore {
            db,
            families,
            unknown_families,
        })
    }

    /// Catch up to the primary's latest flushed state.
    ///
    /// A secondary sees nothing new until it is told to look. An archiver calls this at the top of
    /// each pass; what it then reads is a consistent snapshot as of this call.
    ///
    /// The primary's *unflushed* memtable is not visible, so the tip a secondary sees trails the
    /// primary's own. That is the right direction to be wrong in for an archiver: it can only
    /// archive less than exists, never claim data it has not seen.
    pub fn catch_up(&self) -> Result<()> {
        self.db
            .try_catch_up_with_primary()
            .context("catching up with the primary")
    }

    /// Every column family the store holds, known to this build.
    pub fn families(&self) -> &[Cf] {
        &self.families
    }

    /// Family names on disk this build does not recognise — a store written by a newer hydrozoa.
    pub fn unknown_families(&self) -> &[String] {
        &self.unknown_families
    }

    /// The journals: the append streams that carry arrival stamps and can be scanned in order.
    pub fn journals(&self) -> Vec<Cf> {
        self.families
            .iter()
            .copied()
            .filter(|c| c.is_journal())
            .collect()
    }

    /// The highest index present in `cf`, or `None` if the journal is empty.
    ///
    /// Read by seeking to the end rather than counting, so it costs one seek whatever the journal
    /// holds.
    pub fn tip(&self, cf: Cf) -> Result<Option<u64>> {
        let handle = self.handle(cf)?;
        let mut iter = self.db.iterator_cf(&handle, IteratorMode::End);
        match iter.next() {
            None => Ok(None),
            Some(kv) => {
                let (key, _) = kv.with_context(|| format!("seeking the end of {cf}"))?;
                Ok(Some(journal::decode_key(cf, &key)?))
            }
        }
    }

    /// The lowest index present in `cf`, or `None` if the journal is empty.
    ///
    /// On a store that has been trimmed this is the retention floor: the oldest entry still
    /// servable from the journal.
    pub fn floor(&self, cf: Cf) -> Result<Option<u64>> {
        let handle = self.handle(cf)?;
        let mut iter = self.db.iterator_cf(&handle, IteratorMode::Start);
        match iter.next() {
            None => Ok(None),
            Some(kv) => {
                let (key, _) = kv.with_context(|| format!("seeking the start of {cf}"))?;
                Ok(Some(journal::decode_key(cf, &key)?))
            }
        }
    }

    /// Walk every entry in `cf` as raw `(key, value)` pairs, in key order.
    ///
    /// Works for any family, journal or not — which is what the eighteen fixed families need,
    /// since none of them has a journal index to resume from. The value comes back exactly as
    /// stored: for a journal that includes the arrival-stamp prefix, because a copy that keeps the
    /// framed bytes keeps the ordering with them.
    pub fn scan_raw(
        &self,
        cf: Cf,
    ) -> Result<impl Iterator<Item = Result<(Vec<u8>, Vec<u8>)>> + use<'_>> {
        let handle = self.handle(cf)?;
        let iter = self.db.iterator_cf(&handle, IteratorMode::Start);
        Ok(iter.map(move |kv| {
            let (key, value) = kv.with_context(|| format!("scanning {cf}"))?;
            Ok((key.to_vec(), value.to_vec()))
        }))
    }

    /// Walk `cf` from `from` (inclusive) to the end, in index order.
    ///
    /// The scan is lazy: entries are decoded as the iterator is consumed, so a caller that stops
    /// early pays only for what it read. Index order is byte order here, which is what makes a
    /// seek to `from` a seek and not a filter.
    pub fn scan(&self, cf: Cf, from: u64) -> Result<impl Iterator<Item = Result<Entry>> + use<'_>> {
        if !cf.is_journal() {
            bail!("{cf} is not a journal; it cannot be scanned in arrival order");
        }
        let handle = self.handle(cf)?;
        let start = journal::encode_key(cf, from)?;
        let iter = self
            .db
            .iterator_cf(&handle, IteratorMode::From(&start, Direction::Forward));

        Ok(iter.map(move |kv| {
            let (key, value) = kv.with_context(|| format!("scanning {cf}"))?;
            let index = journal::decode_key(cf, &key)?;
            let (stamp, payload) =
                journal::unframe(&value).with_context(|| format!("reading {cf} entry {index}"))?;
            Ok(Entry {
                cf,
                index,
                stamp,
                payload: payload.to_vec(),
            })
        }))
    }

    /// Read one journal entry by index.
    pub fn get(&self, cf: Cf, index: u64) -> Result<Option<Entry>> {
        if !cf.is_journal() {
            bail!("{cf} is not a journal; it has no indexed entries");
        }
        let handle = self.handle(cf)?;
        let key = journal::encode_key(cf, index)?;
        let Some(value) = self
            .db
            .get_cf(&handle, &key)
            .with_context(|| format!("reading {cf} entry {index}"))?
        else {
            return Ok(None);
        };
        let (stamp, payload) =
            journal::unframe(&value).with_context(|| format!("reading {cf} entry {index}"))?;
        Ok(Some(Entry {
            cf,
            index,
            stamp,
            payload: payload.to_vec(),
        }))
    }

    /// The store's schema version and identity stamp.
    ///
    /// Reads but does not enforce: [`StoreMeta::version`] is compared against
    /// [`SUPPORTED_STORE_VERSION`] by the caller, which knows whether reading an unfamiliar store
    /// is worth attempting. Hydrozoa itself refuses the open.
    pub fn meta(&self) -> Result<StoreMeta> {
        let handle = self.handle(Cf::Meta)?;
        let read = |key: &[u8]| -> Result<Option<Vec<u8>>> {
            self.db.get_cf(&handle, key).with_context(|| {
                format!("reading {} from {}", String::from_utf8_lossy(key), Cf::Meta)
            })
        };
        let version = read(meta::VERSION_KEY)?;
        let head_params_hash = read(meta::HEAD_PARAMS_HASH_KEY)?;
        let head_id = read(meta::HEAD_ID_KEY)?;
        let head_address = read(meta::HEAD_ADDRESS_KEY)?;
        let own_peer_id = read(meta::OWN_PEER_ID_KEY)?;

        meta::assemble(
            version.as_deref(),
            head_params_hash.as_deref(),
            head_id.as_deref(),
            head_address.as_deref(),
            own_peer_id.as_deref(),
        )
    }

    fn handle(&self, cf: Cf) -> Result<impl rocksdb::AsColumnFamilyRef + use<'_>> {
        self.db
            .cf_handle(&cf.name())
            .with_context(|| format!("{cf} is not present in this store"))
    }
}

/// A secondary path that will not collide with a concurrent reader of the same store.
///
/// RocksDB keeps per-secondary bookkeeping under this path and two handles sharing one will
/// corrupt each other's view, so the pid is appended.
pub fn unique_secondary(base: &Path) -> PathBuf {
    let mut name = base.file_name().unwrap_or_default().to_os_string();
    name.push(format!("-{}", std::process::id()));
    base.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secondary_path_is_per_process() {
        let p = unique_secondary(Path::new("/tmp/archive-secondary"));
        assert_eq!(
            p,
            PathBuf::from(format!("/tmp/archive-secondary-{}", std::process::id()))
        );
    }

    #[test]
    fn opening_a_path_that_is_not_a_store_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(HeadStore::open(&dir.path().join("nope"), &dir.path().join("sec")).is_err());
    }
}
