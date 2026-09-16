//! Journal framing: the arrival stamp every journal value carries, and the index its key encodes.
//!
//! Mirrors hydrozoa's `persistence/{ArrivalStamp,JournalValue,JournalKey}.scala`.

use crate::cf::Cf;
use anyhow::{Result, bail};

/// Width of the arrival-stamp prefix on every journal value, in bytes.
pub const STAMP_WIDTH: usize = 12;

/// The durable `(generation, monotonic_nanos)` pair that orders entries across journals.
///
/// `generation` is a per-process boot counter held in the store and bumped at startup, so the
/// order survives a restart that resets the process clock. `monotonic_nanos` is strictly
/// increasing within one process. Encoded big-endian as `[generation:4][monotonic_nanos:8]`, so
/// raw byte order already is `(generation, monotonic_nanos)` order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct ArrivalStamp {
    pub generation: u32,
    pub monotonic_nanos: u64,
}

impl ArrivalStamp {
    /// Read a stamp from the first [`STAMP_WIDTH`] bytes.
    ///
    /// A value too short to hold one is store corruption, not an empty payload: hydrozoa frames
    /// every journal write, so there is no such thing as an unstamped journal value.
    pub fn from_bytes(bytes: &[u8]) -> Result<ArrivalStamp> {
        if bytes.len() < STAMP_WIDTH {
            bail!(
                "journal value is {} bytes, too short for a {STAMP_WIDTH}-byte arrival stamp",
                bytes.len()
            );
        }
        Ok(ArrivalStamp {
            generation: u32::from_be_bytes(bytes[0..4].try_into().unwrap()),
            monotonic_nanos: u64::from_be_bytes(bytes[4..12].try_into().unwrap()),
        })
    }

    pub fn to_bytes(self) -> [u8; STAMP_WIDTH] {
        let mut out = [0u8; STAMP_WIDTH];
        out[0..4].copy_from_slice(&self.generation.to_be_bytes());
        out[4..12].copy_from_slice(&self.monotonic_nanos.to_be_bytes());
        out
    }
}

/// One entry read out of a journal: where it sits in its own stream, when it arrived, and the
/// payload bytes with the stamp prefix stripped.
///
/// The payload is handed back **encoded**. An archiver copies it verbatim and never needs to
/// understand it; a reader that does want the typed form decodes it with the codec for that
/// journal (only the Request lane has one here — see [`crate::request_record`]).
#[derive(Clone, Debug)]
pub struct Entry {
    /// The column family, which is also the journal's identity and its author.
    pub cf: Cf,
    /// The within-journal index: `blockNum`, `requestNum`, `softAckNum`, and so on.
    pub index: u64,
    pub stamp: ArrivalStamp,
    /// The wire payload, stamp prefix removed.
    pub payload: Vec<u8>,
}

impl Entry {
    /// Re-frame the entry as hydrozoa stored it: `[stamp:12][payload…]`.
    ///
    /// An archive that keeps the framed bytes keeps the arrival order with them, so a restored
    /// store sorts the same way the original did.
    pub fn framed(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(STAMP_WIDTH + self.payload.len());
        out.extend_from_slice(&self.stamp.to_bytes());
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Encode a journal index as the key bytes for `cf` — 4 or 8 bytes, big-endian.
///
/// Big-endian fixed width is what makes a lexicographic range scan a numeric range scan, which is
/// the property both replay and a prefix-trim rely on.
pub fn encode_key(cf: Cf, index: u64) -> Result<Vec<u8>> {
    match cf.key_width() {
        Some(8) => Ok(index.to_be_bytes().to_vec()),
        Some(4) => {
            let n: u32 = index
                .try_into()
                .map_err(|_| anyhow::anyhow!("{cf} counts in u32; {index} does not fit"))?;
            Ok(n.to_be_bytes().to_vec())
        }
        _ => bail!("{cf} is not a journal; it has no index key"),
    }
}

/// Decode a journal key back to its index, given the family it came from.
pub fn decode_key(cf: Cf, bytes: &[u8]) -> Result<u64> {
    let width = match cf.key_width() {
        Some(w) => w,
        None => bail!("{cf} is not a journal; it has no index key"),
    };
    if bytes.len() != width {
        bail!("{cf} key is {} bytes; expected {width}", bytes.len());
    }
    Ok(match width {
        8 => u64::from_be_bytes(bytes.try_into().unwrap()),
        _ => u32::from_be_bytes(bytes.try_into().unwrap()) as u64,
    })
}

/// Split a stored journal value into its stamp and its payload.
pub fn unframe(framed: &[u8]) -> Result<(ArrivalStamp, &[u8])> {
    let stamp = ArrivalStamp::from_bytes(framed)?;
    Ok((stamp, &framed[STAMP_WIDTH..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cf::PeerId;

    #[test]
    fn a_stamp_round_trips_through_its_bytes() {
        let s = ArrivalStamp {
            generation: 7,
            monotonic_nanos: 1_234_567_890_123,
        };
        assert_eq!(ArrivalStamp::from_bytes(&s.to_bytes()).unwrap(), s);
    }

    /// The §5.4 merge orders entries by comparing the raw prefix, so `Ord` on the decoded stamp
    /// and `Ord` on its bytes have to agree.
    #[test]
    fn stamp_byte_order_matches_stamp_order() {
        let mut stamps = [
            ArrivalStamp {
                generation: 2,
                monotonic_nanos: 1,
            },
            ArrivalStamp {
                generation: 1,
                monotonic_nanos: u64::MAX,
            },
            ArrivalStamp {
                generation: 1,
                monotonic_nanos: 5,
            },
        ];
        stamps.sort();
        let mut bytes: Vec<_> = stamps.iter().map(|s| s.to_bytes()).collect();
        let sorted_by_value = bytes.clone();
        bytes.sort();
        assert_eq!(bytes, sorted_by_value);
    }

    #[test]
    fn a_value_shorter_than_a_stamp_is_rejected() {
        assert!(ArrivalStamp::from_bytes(&[0u8; STAMP_WIDTH - 1]).is_err());
        assert!(unframe(&[0u8; 3]).is_err());
    }

    #[test]
    fn an_empty_payload_is_not_a_short_value() {
        let (stamp, payload) = unframe(&[0u8; STAMP_WIDTH]).unwrap();
        assert_eq!(stamp, ArrivalStamp::default());
        assert!(payload.is_empty());
    }

    #[test]
    fn request_keys_are_eight_bytes_and_every_other_journal_four() {
        assert_eq!(encode_key(Cf::Request(0), 1).unwrap().len(), 8);
        assert_eq!(encode_key(Cf::Block, 1).unwrap().len(), 4);
        assert_eq!(
            encode_key(Cf::HardAck(PeerId::Coil(0)), 1).unwrap().len(),
            4
        );
    }

    #[test]
    fn keys_round_trip_and_sort_in_index_order() {
        for cf in [Cf::Block, Cf::Request(1)] {
            let mut keys: Vec<_> = [9u64, 1, 300, 2]
                .iter()
                .map(|i| encode_key(cf, *i).unwrap())
                .collect();
            keys.sort();
            let indices: Vec<_> = keys.iter().map(|k| decode_key(cf, k).unwrap()).collect();
            assert_eq!(indices, vec![1, 2, 9, 300], "{cf}");
        }
    }

    #[test]
    fn a_non_journal_family_has_no_index_key() {
        assert!(encode_key(Cf::Meta, 0).is_err());
        assert!(decode_key(Cf::Meta, &[0, 0, 0, 0]).is_err());
    }

    #[test]
    fn a_block_index_past_u32_is_refused_rather_than_truncated() {
        assert!(encode_key(Cf::Block, u64::from(u32::MAX) + 1).is_err());
    }
}
