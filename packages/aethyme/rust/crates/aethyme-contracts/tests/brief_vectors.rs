//! The decision brief and `aethyme-brief-tokens/v0` against golden vectors
//! produced by an independent reference implementation
//! (`fixtures/experimental-v0/brief_tokens.json`, `briefs.json`). A client in
//! another language must pass the same files (D11: identical counts).

use std::collections::HashMap;

use aethyme_contracts::experimental_v0::brief::{
    BRIEF_SCHEMA_NAME, Brief, BriefErrors, MAX_BRIEF_RECORD_BYTES, MAX_BRIEF_TOKENS,
    MAX_LIST_ENTRIES, MAX_PROSE_BYTES, MAX_SCOPE_REF_BYTES, TOKEN_PROFILE, count_tokens,
};
use data_encoding::HEXLOWER;
use serde::Deserialize;

#[derive(Deserialize)]
struct TokenVectors {
    profile: String,
    cases: Vec<TokenCase>,
}

#[derive(Deserialize)]
struct TokenCase {
    text: String,
    tokens: usize,
}

#[derive(Deserialize)]
struct BriefVectors {
    profile: String,
    schema: String,
    limits: Limits,
    valid: Vec<ValidCase>,
    invalid_decision_files: Vec<InvalidCase>,
    valid_records: Vec<ValidRecordCase>,
    invalid_records: Vec<InvalidCase>,
}

#[derive(Deserialize)]
struct Limits {
    max_tokens: usize,
    max_prose_bytes: usize,
    max_list_entries: usize,
    max_scope_ref_bytes: usize,
    max_record_bytes: usize,
}

#[derive(Deserialize)]
struct ValidCase {
    name: String,
    input: Input,
    tokens: usize,
    record_hex: String,
    id: String,
    equivalence_class: String,
}

#[derive(Deserialize)]
struct ValidRecordCase {
    name: String,
    input: Input,
    id: String,
}

#[derive(Deserialize)]
struct InvalidCase {
    name: String,
    input: Input,
    errors: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct Input {
    hex: String,
}

fn hex(text: &str) -> Vec<u8> {
    HEXLOWER.decode(text.as_bytes()).expect("vector hex")
}

fn token_vectors() -> TokenVectors {
    serde_json::from_str(include_str!(
        "../fixtures/experimental-v0/brief_tokens.json"
    ))
    .expect("token vectors parse")
}

fn brief_vectors() -> BriefVectors {
    serde_json::from_str(include_str!("../fixtures/experimental-v0/briefs.json"))
        .expect("brief vectors parse")
}

fn codes(errors: &BriefErrors) -> Vec<(String, String)> {
    errors
        .0
        .iter()
        .map(|error| (error.path.clone(), error.code().to_string()))
        .collect()
}

#[test]
fn profile_schema_and_limits_match_the_reference() {
    let vectors = brief_vectors();
    assert_eq!(token_vectors().profile, TOKEN_PROFILE);
    assert_eq!(vectors.profile, TOKEN_PROFILE);
    assert_eq!(vectors.schema, BRIEF_SCHEMA_NAME);
    assert_eq!(vectors.limits.max_tokens, MAX_BRIEF_TOKENS);
    assert_eq!(vectors.limits.max_prose_bytes, MAX_PROSE_BYTES);
    assert_eq!(vectors.limits.max_list_entries, MAX_LIST_ENTRIES);
    assert_eq!(vectors.limits.max_scope_ref_bytes, MAX_SCOPE_REF_BYTES);
    assert_eq!(vectors.limits.max_record_bytes, MAX_BRIEF_RECORD_BYTES);
}

#[test]
fn token_counts_match_the_reference_exactly() {
    for case in token_vectors().cases {
        assert_eq!(count_tokens(&case.text), case.tokens, "{:?}", case.text);
    }
}

#[test]
fn valid_decision_files_count_encode_and_identify_like_the_reference() {
    for case in brief_vectors().valid {
        let brief = Brief::from_decision_file(&hex(&case.input.hex))
            .unwrap_or_else(|e| panic!("{}: unexpected refusal:\n{e}", case.name));
        assert_eq!(brief.token_count(), case.tokens, "{}: tokens", case.name);
        let (record, id) = brief.to_record();
        assert_eq!(
            HEXLOWER.encode(&record),
            case.record_hex,
            "{}: record",
            case.name
        );
        assert_eq!(id.as_str(), case.id, "{}: id", case.name);
        let (read_back, read_id) = Brief::from_record(&record).unwrap();
        assert_eq!(
            (read_back, read_id),
            (brief, id),
            "{}: round trip",
            case.name
        );
    }
}

#[test]
fn equivalent_briefs_share_an_id_and_only_they_do() {
    let mut id_of_class: HashMap<String, String> = HashMap::new();
    let mut class_of_id: HashMap<String, String> = HashMap::new();
    for case in brief_vectors().valid {
        let class = id_of_class
            .entry(case.equivalence_class.clone())
            .or_insert(case.id.clone());
        assert_eq!(*class, case.id, "{}", case.name);
        let id = class_of_id
            .entry(case.id.clone())
            .or_insert(case.equivalence_class.clone());
        assert_eq!(*id, case.equivalence_class, "{}", case.name);
    }
}

/// Every problem is reported, in order, with its path: an agent fixes the
/// brief in one round instead of one error at a time.
#[test]
fn invalid_decision_files_report_every_problem_with_its_path() {
    for case in brief_vectors().invalid_decision_files {
        let errors = Brief::from_decision_file(&hex(&case.input.hex))
            .err()
            .unwrap_or_else(|| panic!("{}: accepted an invalid brief", case.name));
        assert_eq!(codes(&errors), case.errors, "{}:\n{errors}", case.name);
    }
}

#[test]
fn records_carry_unknown_members_and_refuse_what_they_must() {
    let vectors = brief_vectors();
    for case in vectors.valid_records {
        let (_, id) = Brief::from_record(&hex(&case.input.hex))
            .unwrap_or_else(|e| panic!("{}: unexpected refusal:\n{e}", case.name));
        assert_eq!(id.as_str(), case.id, "{}", case.name);
    }
    for case in vectors.invalid_records {
        let errors = Brief::from_record(&hex(&case.input.hex))
            .err()
            .unwrap_or_else(|| panic!("{}: accepted an invalid record", case.name));
        assert_eq!(codes(&errors), case.errors, "{}:\n{errors}", case.name);
    }
}

/// The error text is what an agent reads: it must say what to do, not just
/// what is wrong.
#[test]
fn over_budget_errors_tell_the_agent_how_much_to_cut() {
    let input = format!(r#"{{"intent":"{}"}}"#, ["abcd"; 151].join(" "));
    let message = Brief::from_decision_file(input.as_bytes())
        .unwrap_err()
        .to_string();
    assert!(message.contains("151 tokens"), "{message}");
    assert!(message.contains("Shorten it by 1"), "{message}");
    assert!(message.contains("never truncated"), "{message}");
}

#[derive(Deserialize)]
struct Calibration {
    cases: Vec<CalibrationCase>,
}

#[derive(Deserialize)]
struct CalibrationCase {
    text: String,
    reference_tokens: usize,
}

/// The profile's promise: 150 profile tokens never exceed 150 tokens of the
/// reference model tokenizer on the calibration corpus. The reference counts
/// are measured once and stored, so no tokenizer dependency is needed here.
#[test]
fn profile_never_counts_fewer_than_the_reference_tokenizer() {
    let calibration: Calibration = serde_json::from_str(include_str!(
        "../fixtures/experimental-v0/brief_calibration.json"
    ))
    .expect("calibration parses");
    for case in calibration.cases {
        let ours = count_tokens(&case.text);
        assert!(
            ours >= case.reference_tokens,
            "{:?}: {ours} < {}",
            case.text,
            case.reference_tokens
        );
    }
}
