//! The analysis result envelope against golden vectors produced by an
//! independent reference implementation (`fixtures/experimental-v0/analysis.json`):
//! AQ0's "no stronger claim than actual coverage", T78's degradations, and
//! T85's old/new producer cases.

use aethyme_contracts::experimental_v0::RecordId;
use aethyme_contracts::experimental_v0::analysis::{
    ANALYSIS_RESULT_SCHEMA_NAME, AnalysisEnvelope, Cursor, ProfileRef, compare_profiles,
};
use data_encoding::HEXLOWER;
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    schema: String,
    valid: Vec<ValidCase>,
    invalid: Vec<InvalidCase>,
}

#[derive(Deserialize)]
struct ValidCase {
    name: String,
    input: Input,
    status: String,
    absence_is_evidence: bool,
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
    serde_json::from_str(include_str!("../fixtures/experimental-v0/analysis.json"))
        .expect("vectors parse")
}

#[test]
fn schema_matches_the_reference() {
    assert_eq!(vectors().schema, ANALYSIS_RESULT_SCHEMA_NAME);
}

#[test]
fn every_degradation_reads_as_the_reference_says() {
    for case in vectors().valid {
        let envelope = AnalysisEnvelope::from_record(&hex(&case.input.hex))
            .unwrap_or_else(|e| panic!("{}: unexpected refusal: {e}", case.name));
        assert_eq!(envelope.status().as_str(), case.status, "{}", case.name);
        assert_eq!(
            envelope.absence_is_evidence(),
            case.absence_is_evidence,
            "{}",
            case.name
        );
    }
}

/// Re-encoding keeps the canonical bytes and ID, and never upgrades a
/// dimension: unknown and unrecognized values are omitted, so they read back
/// as unknown.
#[test]
fn round_trip_keeps_identity_and_never_upgrades_a_reading() {
    for case in vectors().valid {
        let envelope = AnalysisEnvelope::from_record(&hex(&case.input.hex)).unwrap();
        let (bytes, id) = envelope.to_record();
        let again = AnalysisEnvelope::from_record(&bytes).unwrap();
        assert_eq!(again, envelope, "{}", case.name);
        assert_eq!(again.status().as_str(), case.status, "{}", case.name);
        // A newer producer's unrecognized value is dropped on re-encoding; that is
        // why readers forward the original record instead of re-encoding it.
        if !case.name.contains("unrecognized") {
            assert_eq!(HEXLOWER.encode(&bytes), case.canonical_hex, "{}", case.name);
            assert_eq!(id.as_str(), case.id, "{}", case.name);
        }
    }
}

#[test]
fn invalid_envelopes_are_refused_with_the_expected_code() {
    for case in vectors().invalid {
        let error = AnalysisEnvelope::from_record(&hex(&case.input.hex))
            .err()
            .unwrap_or_else(|| panic!("{}: accepted an invalid envelope", case.name));
        assert_eq!(error.code(), case.error, "{}: {error}", case.name);
    }
}

#[test]
fn only_identical_profiles_compare() {
    let pinned = |byte: &str| {
        ProfileRef::Pinned(RecordId::parse(&format!("sha256:{}", byte.repeat(32))).unwrap())
    };
    let legacy = |name: &str| ProfileRef::Legacy(name.to_string());
    assert!(compare_profiles(&pinned("ab"), &pinned("ab")).is_ok());
    assert!(
        compare_profiles(
            &legacy("aethyme-engine/0.8.26/graph-impact-calls"),
            &legacy("aethyme-engine/0.8.26/graph-impact-calls")
        )
        .is_ok()
    );
    for (base, candidate) in [
        (pinned("ab"), pinned("cd")),
        (
            legacy("aethyme-engine/0.8.26/graph-impact-calls"),
            legacy("aethyme-engine/0.8.27/graph-impact-calls"),
        ),
        (
            legacy("aethyme-engine/0.8.26/graph-impact-calls"),
            legacy("aethyme-engine/0.8.26/graph-impact-imports"),
        ),
        (legacy("x"), pinned("ab")),
    ] {
        assert_eq!(
            compare_profiles(&base, &candidate).unwrap_err().code(),
            "incompatible_analysis_profile"
        );
    }
}

/// T78: a cursor from an older view or another query is refused, so pages
/// from different generations never mix.
#[test]
fn cursors_pin_one_view_and_one_query() {
    let cursor = Cursor {
        view: "view-7".into(),
        query_digest: "q1".into(),
        offset: 50,
    };
    assert_eq!(cursor.check("view-7", "q1"), Ok(50));
    assert_eq!(
        cursor.check("view-8", "q1").unwrap_err().code(),
        "stale_cursor"
    );
    assert_eq!(
        cursor.check("view-7", "q2").unwrap_err().code(),
        "stale_cursor"
    );
}
