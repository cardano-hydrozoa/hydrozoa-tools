//! The vendored `RequestRecord` schema, held against hydrozoa's own golden fixtures.
//!
//! `proto/request_record.proto` asks for exactly this: "Golden fixtures pinning this encoding live
//! in the Hydrozoa repo at `src/test/resources/golden/request-record/`; a reader in another repo
//! vendors a copy and asserts it decodes them to the same values."
//!
//! The fixtures under `tests/golden/` are byte-for-byte copies of those files, and the cases below
//! are transcribed from `RequestRecordCodecTest.scala`. If hydrozoa regenerates them, this fails
//! — which is the point, since the encoding is a store format this crate parses.

use hydrozoa_store::{RequestBody, RequestRecord};

/// The deterministic payload hydrozoa's fixtures are built from:
/// `Array.tabulate(length)(i => (i * 37 + 11).toByte)`.
fn payload(length: usize) -> Vec<u8> {
    (0..length)
        .map(|i| (i.wrapping_mul(37) + 11) as u8)
        .collect()
}

fn golden(name: &str) -> Vec<u8> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/");
    let hex_text = std::fs::read_to_string(format!("{path}{name}.hex"))
        .unwrap_or_else(|e| panic!("reading golden fixture {name}: {e}"));
    hex::decode(hex_text.trim()).unwrap_or_else(|e| panic!("decoding golden fixture {name}: {e}"))
}

/// The four cases, chosen upstream to cover the encoding's edges: proto3 default omission (peer 0
/// and request 0 encode as absent fields), both body arms, a request number past 2^32, and a
/// realistic transaction payload.
fn cases() -> Vec<(&'static str, RequestRecord)> {
    vec![
        (
            "transaction-zero-id",
            RequestRecord {
                head_peer_number: 0,
                request_number: 0,
                body: Some(RequestBody::Transaction(
                    hydrozoa_store::request_record::TransactionBody {
                        l2_payload: payload(4),
                    },
                )),
            },
        ),
        (
            "transaction-large-number",
            RequestRecord {
                head_peer_number: 5,
                request_number: (1u64 << 32) + 12345,
                body: Some(RequestBody::Transaction(
                    hydrozoa_store::request_record::TransactionBody {
                        l2_payload: payload(16),
                    },
                )),
            },
        ),
        (
            "transaction-realistic",
            RequestRecord {
                head_peer_number: 2,
                request_number: 913,
                body: Some(RequestBody::Transaction(
                    hydrozoa_store::request_record::TransactionBody {
                        l2_payload: payload(800),
                    },
                )),
            },
        ),
        (
            "deposit-basic",
            RequestRecord {
                head_peer_number: 3,
                request_number: 7,
                body: Some(RequestBody::Deposit(
                    hydrozoa_store::request_record::DepositBody {
                        l1_payload: payload(220),
                        l2_payload: payload(96),
                    },
                )),
            },
        ),
    ]
}

#[test]
fn every_golden_fixture_decodes_to_its_case() {
    for (name, expected) in cases() {
        let decoded = RequestRecord::decode_payload(&golden(name))
            .unwrap_or_else(|e| panic!("decoding {name}: {e}"));
        assert_eq!(decoded, expected, "{name} decoded to something else");
    }
}

#[test]
fn every_case_re_encodes_to_its_golden_bytes() {
    use prost::Message as _;
    for (name, record) in cases() {
        assert_eq!(
            hex::encode(record.encode_to_vec()),
            hex::encode(golden(name)),
            "{name} re-encoded differently; if hydrozoa changed the encoding, revendor the fixtures"
        );
    }
}

#[test]
fn a_records_kind_comes_from_which_body_is_present() {
    for (name, record) in cases() {
        let expected = if name.starts_with("deposit") {
            "deposit"
        } else {
            "transaction"
        };
        assert_eq!(record.kind(), Some(expected), "{name}");
    }
}

/// Hydrozoa rejects a body-less record outright. This crate reports it as an unknown kind instead:
/// an archiver copies the bytes either way, and refusing to read a journal entry it can still
/// archive would be the worse failure.
#[test]
fn a_record_with_no_body_reads_as_an_unknown_kind() {
    use prost::Message as _;
    let bodyless = RequestRecord {
        head_peer_number: 3,
        request_number: 7,
        body: None,
    };
    let decoded = RequestRecord::decode_payload(&bodyless.encode_to_vec()).unwrap();
    assert_eq!(decoded.kind(), None);
}

/// Proto3 forward compatibility: hydrozoa's field 5 is reserved for a body content hash that is
/// coming back. A build that does not know a field must skip it, not fail.
#[test]
fn a_field_written_by_a_newer_build_is_skipped_not_misread() {
    let mut bytes = golden("deposit-basic");
    // Field 9, length-delimited (wire type 2): tag byte 0x4a, then a 4-byte payload.
    bytes.extend_from_slice(&[0x4a, 0x04, b'n', b'e', b'w', b'!']);

    let decoded = RequestRecord::decode_payload(&bytes).unwrap();
    let (_, expected) = cases()
        .into_iter()
        .find(|(n, _)| *n == "deposit-basic")
        .unwrap();
    assert_eq!(decoded, expected);
}
