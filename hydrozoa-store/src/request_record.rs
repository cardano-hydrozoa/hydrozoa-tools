//! The Request journal's payload, vendored from hydrozoa's `proto/request_record.proto`.
//!
//! The Request lane is the one journal whose payload is not hydrozoa's circe wire form. It stores
//! a protobuf record instead, because — in `JournalKey`'s words — "this lane is written on the
//! admission hot path and read by the L2 ledger process in another language".
//!
//! The schema is transcribed here rather than generated, so the build needs no `protoc`. Hydrozoa
//! owns it and is its only writer; the golden fixtures under `tests/golden/` are the drift check,
//! copied from hydrozoa's `src/test/resources/golden/request-record/`. The proto file names that
//! arrangement explicitly: "a reader in another repo vendors a copy and asserts it decodes them to
//! the same values."

use prost::Message as _;

/// One entry in a head peer's Request journal.
///
/// Field 5 is deliberately absent upstream — it carried a content hash of the body, taken up
/// separately alongside pinning request hashes in block bodies, and is expected back. The gap in
/// the tags is the schema's, not a transcription slip.
#[derive(Clone, PartialEq, prost::Message)]
pub struct RequestRecord {
    /// The assigning head peer's number.
    #[prost(uint32, tag = "1")]
    pub head_peer_number: u32,
    /// That peer's per-author sequential request number.
    #[prost(uint64, tag = "2")]
    pub request_number: u64,
    /// Exactly one body is present; which one is the request's kind.
    #[prost(oneof = "RequestBody", tags = "3, 4")]
    pub body: Option<RequestBody>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub enum RequestBody {
    #[prost(message, tag = "3")]
    Deposit(DepositBody),
    #[prost(message, tag = "4")]
    Transaction(TransactionBody),
}

/// A deposit: the CBOR-encoded L1 deposit transaction, and the opaque L2 payload it pins.
#[derive(Clone, PartialEq, prost::Message)]
pub struct DepositBody {
    #[prost(bytes = "vec", tag = "1")]
    pub l1_payload: Vec<u8>,
    #[prost(bytes = "vec", tag = "2")]
    pub l2_payload: Vec<u8>,
}

/// An L2 transaction: one opaque payload, passed to the L2 ledger unmodified.
#[derive(Clone, PartialEq, prost::Message)]
pub struct TransactionBody {
    #[prost(bytes = "vec", tag = "1")]
    pub l2_payload: Vec<u8>,
}

impl RequestRecord {
    /// Decode a record from a journal payload — the stored value with its arrival stamp already
    /// stripped (see [`crate::journal::unframe`]).
    pub fn decode_payload(payload: &[u8]) -> anyhow::Result<RequestRecord> {
        Ok(RequestRecord::decode(payload)?)
    }

    /// `"deposit"` or `"transaction"`, or `None` for a record carrying neither.
    ///
    /// A record with no body is not a decode failure — proto3 makes every field optional — but it
    /// is a record hydrozoa does not write, so a reader reports it rather than assuming a kind.
    pub fn kind(&self) -> Option<&'static str> {
        match self.body {
            Some(RequestBody::Deposit(_)) => Some("deposit"),
            Some(RequestBody::Transaction(_)) => Some("transaction"),
            None => None,
        }
    }
}
