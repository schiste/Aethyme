//! Symbol identity and observation records against independently frozen
//! canonical JSON and SHA-256 vectors.

use std::collections::BTreeMap;

use aethyme_contracts::experimental_v0::record::RECORD_DOMAIN;
use aethyme_contracts::experimental_v0::symbol_identity::{
    MAX_SYMBOL_RECORD_BYTES, SymbolScopeKind, decode_symbol_identity_record, symbol_scope_kind,
};
use aethyme_contracts::experimental_v0::{Record, StateReading};
use data_encoding::HEXLOWER;
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    schema: String,
    independent_reference: IndependentReference,
    valid: Vec<ValidCase>,
    invalid: Vec<InvalidCase>,
}

#[derive(Deserialize)]
struct IndependentReference {
    canonical_json: String,
    record_domain_hex: String,
}

#[derive(Deserialize)]
struct ValidCase {
    name: String,
    input: Input,
    canonical_hex: String,
    id: String,
}

#[derive(Deserialize)]
struct InvalidCase {
    name: String,
    input: Input,
    error: String,
}

#[derive(Deserialize)]
struct Input {
    hex: String,
}

fn hex(text: &str) -> Vec<u8> {
    HEXLOWER.decode(text.as_bytes()).expect("vector hex")
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!("../fixtures/experimental-v0/symbol_identity.json"))
        .expect("symbol identity vectors parse")
}

#[test]
fn frozen_vectors_bind_the_independent_canonical_profile_and_domain() {
    let vector_set = vectors();
    assert_eq!(
        vector_set.schema,
        "aethyme.symbol-identity-vectors/experimental-v0"
    );
    assert_eq!(
        vector_set.independent_reference.canonical_json,
        "RFC 8785 restricted profile; object keys ordered by UTF-16 code units"
    );
    assert_eq!(
        hex(&vector_set.independent_reference.record_domain_hex),
        RECORD_DOMAIN
    );

    let mut by_name = BTreeMap::new();
    for case in vector_set.valid {
        let record = decode_symbol_identity_record(&hex(&case.input.hex))
            .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        assert_eq!(
            HEXLOWER.encode(&record.canonical_bytes()),
            case.canonical_hex,
            "{}",
            case.name
        );
        assert_eq!(record.id().as_str(), case.id, "{}", case.name);
        by_name.insert(case.name, record);
    }

    let local_one = by_name
        .get("same producer local string in document one")
        .expect("document one vector");
    let local_two = by_name
        .get("same producer local string in document two")
        .expect("document two vector");
    let other_producer = by_name
        .get("same document local string from another producer")
        .expect("second producer vector");
    assert_ne!(local_one.id(), local_two.id());
    assert_ne!(local_one.id(), other_producer.id());

    let unknown = by_name
        .get("unknown producer scope stays opaque")
        .expect("unknown scope vector");
    assert_eq!(symbol_scope_kind(unknown), Some(SymbolScopeKind::Unknown));

    let established = by_name
        .get("serialized established is unrecognized")
        .expect("serialized established vector");
    assert!(matches!(
        established.state("state"),
        StateReading::Unrecognized("established")
    ));

    // Existing Record semantics accept an explicit `unknown` state, then drop
    // it from canonical bytes and identity. This is distinct from the unknown
    // scope kind above, which is retained but never interpreted.
    let explicit_unknown = vectors()
        .valid
        .into_iter()
        .find(|case| case.name == "explicit unknown claim")
        .expect("explicit unknown vector");
    let input = hex(&explicit_unknown.input.hex);
    assert!(std::str::from_utf8(&input)
        .expect("UTF-8 vector")
        .contains("\"state\":\"unknown\""));
    let decoded = decode_symbol_identity_record(&input).expect("unknown state is accepted");
    assert_eq!(decoded.canonical_bytes(), hex(&explicit_unknown.canonical_hex));
    assert_eq!(decoded.id().as_str(), explicit_unknown.id);
    assert_eq!(decoded.state("state"), StateReading::Unknown);
}

#[test]
fn malformed_ids_and_noncanonical_or_oversized_inputs_fail_closed() {
    for case in vectors().invalid {
        let error = decode_symbol_identity_record(&hex(&case.input.hex))
            .err()
            .unwrap_or_else(|| panic!("{}: accepted invalid vector", case.name));
        assert_eq!(error.code(), case.error, "{}: {error}", case.name);
    }

    let valid = vectors()
        .valid
        .into_iter()
        .find(|case| case.name == "engine symbol in clone A")
        .expect("engine symbol vector");
    let mut noncanonical = hex(&valid.input.hex);
    noncanonical.push(b' ');
    assert_eq!(
        decode_symbol_identity_record(&noncanonical)
            .unwrap_err()
            .code(),
        "non_canonical"
    );
    assert_eq!(
        decode_symbol_identity_record(&vec![b' '; MAX_SYMBOL_RECORD_BYTES + 1])
            .unwrap_err()
            .code(),
        "too_large"
    );
}

#[test]
fn symbol_record_schema_is_selected_from_the_encoded_schema() {
    let input = hex(
        &vectors()
            .valid
            .into_iter()
            .find(|case| case.name == "engine symbol in clone A")
            .expect("engine symbol vector")
            .input
            .hex,
    );
    let record = decode_symbol_identity_record(&input).expect("symbol ref");
    assert_eq!(record.schema(), "aethyme.symbol-ref/experimental-v0");
    let _: Record = record;
}
