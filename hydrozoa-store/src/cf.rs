//! The column families a hydrozoa store holds, and how their names encode an author.
//!
//! Mirrors hydrozoa's `multisig/persistence/Cf.scala`. The set is config-derived there
//! (`Cf.mkAll` takes the head's membership), but a reader has no head config, so [`Cf::parse`]
//! recovers the same set from the names RocksDB reports for a store on disk.

use std::fmt;

/// Which peer authored a satellite journal.
///
/// Hydrozoa's `PeerId.toWireInt` packs this into one int — the peer number shifted left a bit,
/// with the low bit tagging the kind — and `Cf.HardAck` embeds that int in the family name.
/// Head is 1, coil is 0.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum PeerId {
    Head(u32),
    Coil(u32),
}

impl PeerId {
    /// Decode the packed form `Cf.HardAck` names itself with.
    pub fn from_wire_int(i: u32) -> PeerId {
        if i & 1 == 1 {
            PeerId::Head(i >> 1)
        } else {
            PeerId::Coil(i >> 1)
        }
    }

    /// The packed form, as the family name spells it.
    pub fn to_wire_int(self) -> u32 {
        match self {
            PeerId::Head(n) => (n << 1) | 1,
            PeerId::Coil(n) => n << 1,
        }
    }

    pub fn is_coil(self) -> bool {
        matches!(self, PeerId::Coil(_))
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PeerId::Head(n) => write!(f, "head peer {n}"),
            PeerId::Coil(n) => write!(f, "coil peer {n}"),
        }
    }
}

/// One column family in a hydrozoa store.
///
/// The fixed families are present on every store. The satellite families are one per author, and
/// the family *is* the author discriminant — which is why the keys inside carry no author prefix
/// (see [`crate::journal`]).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum Cf {
    // Fixed — one each.
    Block,
    Stack,
    BlockResult,
    SoftConfirmation,
    HardConfirmation,
    RequestHighWater,
    CoilStampMark,
    L2CommandNumber,
    UnsignedStack,
    DepositMap,
    Treasury,
    EvacuationMap,
    RequestBlockIndex,
    DepositDecisionIndex,
    WithdrawalEffectIndex,
    BlockStackIndex,
    EffectStackIndex,
    Meta,

    // Per-author satellites.
    Request(u32),
    SoftAck(u32),
    HardAck(PeerId),
    HubHardAck(u32),
}

/// The fixed families, in the order `Cf.fixed` lists them.
pub const FIXED: [Cf; 18] = [
    Cf::Block,
    Cf::Stack,
    Cf::BlockResult,
    Cf::SoftConfirmation,
    Cf::HardConfirmation,
    Cf::RequestHighWater,
    Cf::CoilStampMark,
    Cf::L2CommandNumber,
    Cf::UnsignedStack,
    Cf::DepositMap,
    Cf::Treasury,
    Cf::EvacuationMap,
    Cf::RequestBlockIndex,
    Cf::DepositDecisionIndex,
    Cf::WithdrawalEffectIndex,
    Cf::BlockStackIndex,
    Cf::EffectStackIndex,
    Cf::Meta,
];

impl Cf {
    /// The on-disk family name, exactly as hydrozoa writes it.
    pub fn name(self) -> String {
        match self {
            Cf::Block => "Block".into(),
            Cf::Stack => "Stack".into(),
            Cf::BlockResult => "BlockResult".into(),
            Cf::SoftConfirmation => "SoftConfirmation".into(),
            Cf::HardConfirmation => "HardConfirmation".into(),
            Cf::RequestHighWater => "RequestHighWater".into(),
            Cf::CoilStampMark => "CoilStampMark".into(),
            Cf::L2CommandNumber => "L2CommandNumber".into(),
            Cf::UnsignedStack => "UnsignedStack".into(),
            Cf::DepositMap => "DepositMap".into(),
            Cf::Treasury => "Treasury".into(),
            Cf::EvacuationMap => "EvacuationMap".into(),
            Cf::RequestBlockIndex => "RequestBlockIndex".into(),
            Cf::DepositDecisionIndex => "DepositDecisionIndex".into(),
            Cf::WithdrawalEffectIndex => "WithdrawalEffectIndex".into(),
            Cf::BlockStackIndex => "BlockStackIndex".into(),
            Cf::EffectStackIndex => "EffectStackIndex".into(),
            Cf::Meta => "Meta".into(),
            Cf::Request(p) => format!("Request:{p}"),
            Cf::SoftAck(p) => format!("SoftAck:{p}"),
            Cf::HardAck(p) => format!("HardAck:{}", p.to_wire_int()),
            Cf::HubHardAck(h) => format!("HubHardAck:{h}"),
        }
    }

    /// Recover a family from its name. `None` for a name this build does not know — a store
    /// written by a newer hydrozoa, which the caller reports rather than silently skips.
    pub fn parse(name: &str) -> Option<Cf> {
        if let Some(fixed) = FIXED.iter().find(|c| c.name() == name) {
            return Some(*fixed);
        }
        let (kind, num) = name.split_once(':')?;
        let num: u32 = num.parse().ok()?;
        match kind {
            "Request" => Some(Cf::Request(num)),
            "SoftAck" => Some(Cf::SoftAck(num)),
            "HardAck" => Some(Cf::HardAck(PeerId::from_wire_int(num))),
            "HubHardAck" => Some(Cf::HubHardAck(num)),
            _ => None,
        }
    }

    /// Whether this family is a *journal* — an arrival-stamped, index-ordered append stream.
    ///
    /// Only journals carry the 12-byte stamp prefix and only journals are replayable, so this is
    /// the test that decides whether [`crate::journal`] may read a family at all. The snapshot and
    /// reverse-index families are plain key-value and are read by point lookup instead.
    pub fn is_journal(self) -> bool {
        matches!(
            self,
            Cf::Block
                | Cf::Stack
                | Cf::Request(_)
                | Cf::SoftAck(_)
                | Cf::HardAck(_)
                | Cf::HubHardAck(_)
        )
    }

    /// Width of the key in this journal, in bytes. `Request` counts in `u64`; every other journal
    /// counts in `u32`. Both are big-endian, so byte order matches numeric order.
    pub fn key_width(self) -> Option<usize> {
        match self {
            Cf::Request(_) => Some(8),
            c if c.is_journal() => Some(4),
            _ => None,
        }
    }

    /// The author, for a satellite journal. `None` for the spines, which every head peer
    /// round-robin-authors, and for the fixed families.
    pub fn author(self) -> Option<PeerId> {
        match self {
            Cf::Request(p) | Cf::SoftAck(p) | Cf::HubHardAck(p) => Some(PeerId::Head(p)),
            Cf::HardAck(p) => Some(p),
            _ => None,
        }
    }
}

impl fmt::Display for Cf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fixed_family_round_trips_through_its_name() {
        for cf in FIXED {
            assert_eq!(Cf::parse(&cf.name()), Some(cf), "{cf}");
        }
    }

    #[test]
    fn satellite_families_round_trip_through_their_name() {
        let cases = [
            Cf::Request(3),
            Cf::SoftAck(0),
            Cf::HardAck(PeerId::Head(2)),
            Cf::HardAck(PeerId::Coil(41)),
            Cf::HubHardAck(1),
        ];
        for cf in cases {
            assert_eq!(Cf::parse(&cf.name()), Some(cf), "{cf}");
        }
    }

    /// The low bit tags the kind, so a head peer's and a coil peer's journals never collide on
    /// the same number -- the whole point of packing the id rather than naming the number.
    #[test]
    fn head_and_coil_peers_with_the_same_number_get_different_families() {
        assert_ne!(
            Cf::HardAck(PeerId::Head(7)).name(),
            Cf::HardAck(PeerId::Coil(7)).name()
        );
        assert_eq!(Cf::HardAck(PeerId::Head(7)).name(), "HardAck:15");
        assert_eq!(Cf::HardAck(PeerId::Coil(7)).name(), "HardAck:14");
    }

    #[test]
    fn an_unknown_family_name_is_reported_not_guessed() {
        assert_eq!(Cf::parse("Bogus"), None);
        assert_eq!(Cf::parse("Bogus:1"), None);
        assert_eq!(Cf::parse("Request:notanumber"), None);
    }

    #[test]
    fn only_journals_have_a_key_width() {
        assert_eq!(Cf::Request(0).key_width(), Some(8));
        assert_eq!(Cf::Block.key_width(), Some(4));
        assert_eq!(Cf::HardAck(PeerId::Coil(0)).key_width(), Some(4));
        assert_eq!(Cf::Meta.key_width(), None);
        assert_eq!(Cf::DepositMap.key_width(), None);
    }
}
