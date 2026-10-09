//! The canonical JSON profile against golden vectors produced by an independent
//! reference implementation (`fixtures/experimental-v0/canonical_json.json`).
//! A non-Rust implementation must pass the same file.

use aethyme_contracts::experimental_v0::canonical_json::{
    self, MAX_CONTAINER_ENTRIES, MAX_DEPTH, MAX_DOCUMENT_BYTES, MAX_SAFE_INTEGER, MAX_STRING_BYTES,
};
use data_encoding::HEXLOWER;
use serde::Deserialize;
use sha2::{Digest, Sha256};

#[derive(Deserialize)]
struct Vectors {
    limits: Limits,
    valid: Vec<ValidCase>,
    invalid: Vec<InvalidCase>,
}

#[derive(Deserialize)]
struct Limits {
    max_document_bytes: usize,
    max_depth: usize,
    max_string_bytes: usize,
    max_container_entries: usize,
    max_safe_integer: i64,
}

#[derive(Deserialize)]
struct ValidCase {
    name: String,
    input: Input,
    canonical: Canonical,
}

#[derive(Deserialize)]
struct InvalidCase {
    name: String,
    input: Input,
    error: String,
}

/// Inline hex, or `[prefix, unit, count, suffix]` (hex, hex, n, hex) for
/// inputs at the size limits.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Input {
    Hex(String),
    Repeat(String, String, usize, String),
}

/// Canonical output as hex, or its SHA-256 when it is large.
#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Canonical {
    Hex(String),
    Sha256(String),
}

fn hex(text: &str) -> Vec<u8> {
    HEXLOWER.decode(text.as_bytes()).expect("vector hex")
}

impl Input {
    fn bytes(&self) -> Vec<u8> {
        match self {
            Self::Hex(h) => hex(h),
            Self::Repeat(prefix, unit, count, suffix) => {
                let mut out = hex(prefix);
                out.extend(hex(unit).repeat(*count));
                out.extend(hex(suffix));
                out
            }
        }
    }
}

fn vectors() -> Vectors {
    serde_json::from_str(include_str!(
        "../fixtures/experimental-v0/canonical_json.json"
    ))
    .expect("golden vectors parse")
}

#[test]
fn limits_match_the_reference() {
    let limits = vectors().limits;
    assert_eq!(limits.max_document_bytes, MAX_DOCUMENT_BYTES);
    assert_eq!(limits.max_depth, MAX_DEPTH);
    assert_eq!(limits.max_string_bytes, MAX_STRING_BYTES);
    assert_eq!(limits.max_container_entries, MAX_CONTAINER_ENTRIES);
    assert_eq!(limits.max_safe_integer, MAX_SAFE_INTEGER);
}

#[test]
fn valid_vectors_canonicalize_byte_for_byte() {
    for case in vectors().valid {
        let canonical = canonical_json::canonicalize(&case.input.bytes())
            .unwrap_or_else(|e| panic!("{}: unexpected refusal: {e}", case.name));
        match &case.canonical {
            Canonical::Hex(expected) => {
                assert_eq!(&HEXLOWER.encode(&canonical), expected, "{}", case.name)
            }
            Canonical::Sha256(expected) => assert_eq!(
                &HEXLOWER.encode(&Sha256::digest(&canonical)),
                expected,
                "{}",
                case.name
            ),
        }
    }
}

/// Canonical output is itself valid input and a fixed point.
#[test]
fn canonical_output_is_a_fixed_point() {
    for case in vectors().valid {
        let once = canonical_json::canonicalize(&case.input.bytes()).unwrap();
        let twice = canonical_json::canonicalize(&once)
            .unwrap_or_else(|e| panic!("{}: canonical form refused: {e}", case.name));
        assert_eq!(once, twice, "{}", case.name);
    }
}

#[test]
fn invalid_vectors_are_refused_with_the_expected_code() {
    for case in vectors().invalid {
        let error = canonical_json::parse(&case.input.bytes())
            .err()
            .unwrap_or_else(|| panic!("{}: accepted an invalid document", case.name));
        assert_eq!(error.code(), case.error, "{}: {error}", case.name);
    }
}
