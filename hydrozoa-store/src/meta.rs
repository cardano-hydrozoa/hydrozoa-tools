//! The `Meta` family: what schema a store is written in, and what it belongs to.
//!
//! Mirrors hydrozoa's `persistence/{StoreVersion,StoreIdentity}.scala`. Both are name-keyed UTF-8
//! point lookups in `Cf.Meta`.

use crate::cf::{Cf, PeerId};
use anyhow::{Result, bail};

/// The schema version this build was written against.
///
/// Hydrozoa refuses to open a store whose version it does not recognise, and so does this reader:
/// the format still churns during development, a change rebuilds the store rather than migrating
/// it, and the bump is what stops an old store being misread instead of rejected.
pub const SUPPORTED_STORE_VERSION: u32 = 3;

pub const VERSION_KEY: &[u8] = b"store_version";
pub const HEAD_PARAMS_HASH_KEY: &[u8] = b"head_params_hash";
pub const HEAD_ID_KEY: &[u8] = b"head_id";
pub const HEAD_ADDRESS_KEY: &[u8] = b"head_address";
pub const OWN_PEER_ID_KEY: &[u8] = b"own_peer_id";

/// What a store belongs to: one head, under one configuration, written by one peer.
///
/// Stamped when a fresh store is initialized. An archiver checks it for the reason hydrozoa
/// stamps it — pointing at the wrong store does not otherwise fail, it just proceeds on data that
/// means something else — and records it so an archive says which head it came from without
/// needing that head's config alongside.
///
/// `head_id` and `head_address` are stored in their readable forms (hex and bech32), because a
/// store dump has no config to decode them against.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoreIdentity {
    pub head_params_hash: String,
    pub head_id: String,
    pub head_address: String,
    pub own_peer_id: PeerId,
}

/// A store's `Meta` family, as far as a reader cares.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StoreMeta {
    pub version: u32,
    /// `None` for a store predating the identity stamp, or one never initialized.
    pub identity: Option<StoreIdentity>,
}

/// Decode `store_version` from its 4-byte big-endian form.
pub fn decode_version(bytes: &[u8]) -> Result<u32> {
    if bytes.len() != 4 {
        bail!("store version: expected 4 bytes, got {}", bytes.len());
    }
    Ok(u32::from_be_bytes(bytes.try_into().unwrap()))
}

/// Decode `own_peer_id` from the 4-byte big-endian packed peer id.
pub fn decode_own_peer_id(bytes: &[u8]) -> Result<PeerId> {
    if bytes.len() != 4 {
        bail!("own peer id: expected 4 bytes, got {}", bytes.len());
    }
    Ok(PeerId::from_wire_int(u32::from_be_bytes(
        bytes.try_into().unwrap(),
    )))
}

/// Assemble a [`StoreMeta`] from the raw `Meta` rows.
///
/// A partially written identity stamp is an error rather than `None`. Hydrozoa writes all four
/// fields in one step on a fresh open, so a store holding some of them is one this build does not
/// understand — and treating it as unstamped would bless whatever store the caller happened to
/// point at, which is the mistake the stamp exists to catch.
pub fn assemble(
    version: Option<&[u8]>,
    head_params_hash: Option<&[u8]>,
    head_id: Option<&[u8]>,
    head_address: Option<&[u8]>,
    own_peer_id: Option<&[u8]>,
) -> Result<StoreMeta> {
    let version = match version {
        Some(v) => decode_version(v)?,
        None => bail!(
            "{} has no store_version row; it is not a hydrozoa store",
            Cf::Meta
        ),
    };

    let present = [head_params_hash, head_id, head_address, own_peer_id]
        .iter()
        .filter(|f| f.is_some())
        .count();
    let identity = match present {
        0 => None,
        4 => Some(StoreIdentity {
            head_params_hash: String::from_utf8_lossy(head_params_hash.unwrap()).into_owned(),
            head_id: String::from_utf8_lossy(head_id.unwrap()).into_owned(),
            head_address: String::from_utf8_lossy(head_address.unwrap()).into_owned(),
            own_peer_id: decode_own_peer_id(own_peer_id.unwrap())?,
        }),
        n => bail!("the store's identity stamp has {n} of its 4 fields; it is partially written"),
    };

    Ok(StoreMeta { version, identity })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_round_trips_through_its_bytes() {
        assert_eq!(decode_version(&3u32.to_be_bytes()).unwrap(), 3);
        assert!(decode_version(&[0, 0, 3]).is_err());
    }

    #[test]
    fn own_peer_id_decodes_the_packed_kind_tag() {
        assert_eq!(
            decode_own_peer_id(&5u32.to_be_bytes()).unwrap(),
            PeerId::Head(2)
        );
        assert_eq!(
            decode_own_peer_id(&4u32.to_be_bytes()).unwrap(),
            PeerId::Coil(2)
        );
    }

    #[test]
    fn a_store_with_no_version_row_is_not_a_hydrozoa_store() {
        assert!(assemble(None, None, None, None, None).is_err());
    }

    #[test]
    fn an_unstamped_store_reads_as_no_identity() {
        let meta = assemble(Some(&3u32.to_be_bytes()), None, None, None, None).unwrap();
        assert_eq!(meta.version, 3);
        assert_eq!(meta.identity, None);
    }

    #[test]
    fn a_partially_stamped_store_is_an_error_not_an_unstamped_one() {
        let err = assemble(
            Some(&3u32.to_be_bytes()),
            Some(b"abcd"),
            Some(b"beef"),
            None,
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("partially written"), "{err}");
    }

    #[test]
    fn a_fully_stamped_store_reads_all_four_fields() {
        let meta = assemble(
            Some(&3u32.to_be_bytes()),
            Some(b"deadbeef"),
            Some(b"cafe"),
            Some(b"addr_test1..."),
            Some(&1u32.to_be_bytes()),
        )
        .unwrap()
        .identity
        .unwrap();
        assert_eq!(meta.head_params_hash, "deadbeef");
        assert_eq!(meta.own_peer_id, PeerId::Head(0));
    }
}
