//! The record envelope against golden vectors produced by an independent
//! reference implementation (`fixtures/experimental-v0/records.json`).
//!
//! The vectors describe two reader versions ("old" and "new") of one
//! illustrative schema, so they double as the compatibility fixtures: what an
//! old reader accepts, how it reads values it does not know, and which records
//! it must refuse.

use std::collections::{BTreeMap, HashMap};

use aethyme_contracts::experimental_v0::canonical_json::Value;
use aethyme_contracts::experimental_v0::record::RECORD_DOMAIN;
use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, StateReading,
};
use data_encoding::HEXLOWER;
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    domain_hex: String,
    readers: HashMap<String, Vec<SchemaCase>>,
    valid: Vec<ValidCase>,
    invalid: Vec<InvalidCase>,
}

#[derive(Deserialize)]
struct SchemaCase {
    name: String,
    capabilities: Vec<String>,
    fields: Vec<FieldCase>,
}

#[derive(Deserialize)]
struct FieldCase {
    name: String,
    required: bool,
    kind: String,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    capability: Option<String>,
}

#[derive(Deserialize)]
struct ValidCase {
    name: String,
    reader: String,
    input: Input,
    canonical_hex: String,
    id: String,
    states: BTreeMap<String, String>,
    equivalence_class: String,
}

#[derive(Deserialize)]
struct InvalidCase {
    name: String,
    reader: String,
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
    serde_json::from_str(include_str!("../fixtures/experimental-v0/records.json"))
        .expect("golden vectors parse")
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// Build the reader's `'static` schemas from the vector file. Leaking is fine
/// in a test process.
fn readers(vectors: &Vectors) -> HashMap<String, Vec<&'static RecordSchema>> {
    vectors
        .readers
        .iter()
        .map(|(reader, schemas)| {
            let schemas = schemas
                .iter()
                .map(|schema| {
                    let fields: Vec<FieldSpec> = schema
                        .fields
                        .iter()
                        .map(|field| FieldSpec {
                            name: leak(field.name.clone()),
                            required: field.required,
                            capability: field.capability.clone().map(leak),
                            kind: match field.kind.as_str() {
                                "boolean" => FieldKind::Boolean,
                                "integer" => FieldKind::Integer,
                                "string" => FieldKind::String,
                                "decimal_string" => FieldKind::DecimalString,
                                "string_set" => FieldKind::StringSet,
                                "opaque" => FieldKind::Opaque,
                                "state" => FieldKind::State(Box::leak(
                                    field.values.iter().cloned().map(leak).collect(),
                                )),
                                other => panic!("unknown field kind {other}"),
                            },
                        })
                        .collect();
                    &*Box::leak(Box::new(RecordSchema {
                        name: leak(schema.name.clone()),
                        fields: Box::leak(fields.into_boxed_slice()),
                        capabilities: Box::leak(
                            schema.capabilities.iter().cloned().map(leak).collect(),
                        ),
                    }))
                })
                .collect();
            (reader.clone(), schemas)
        })
        .collect()
}

fn reading(state: StateReading<'_>) -> String {
    match state {
        StateReading::Known(value) => format!("known:{value}"),
        StateReading::Unknown => "unknown".into(),
        StateReading::Unrecognized(value) => format!("unrecognized:{value}"),
    }
}

#[test]
fn domain_header_matches_the_reference() {
    assert_eq!(hex(&vectors().domain_hex), RECORD_DOMAIN);
}

#[test]
fn valid_vectors_match_canonical_bytes_id_and_state_readings() {
    let vectors = vectors();
    let readers = readers(&vectors);
    for case in &vectors.valid {
        let record = Record::decode(&hex(&case.input.hex), &readers[&case.reader])
            .unwrap_or_else(|e| panic!("{}: unexpected refusal: {e}", case.name));
        assert_eq!(
            HEXLOWER.encode(&record.canonical_bytes()),
            case.canonical_hex,
            "{}: canonical bytes",
            case.name
        );
        assert_eq!(record.id().as_str(), case.id, "{}: id", case.name);
        for (field, expected) in &case.states {
            assert_eq!(
                &reading(record.state(field)),
                expected,
                "{}: state {field}",
                case.name
            );
        }
    }
}

/// Same equivalence class, same ID; different class, different ID.
#[test]
fn equivalent_encodings_share_an_id_and_only_they_do() {
    let vectors = vectors();
    let mut class_of_id: HashMap<&str, &str> = HashMap::new();
    let mut id_of_class: HashMap<&str, &str> = HashMap::new();
    for case in &vectors.valid {
        let class = case.equivalence_class.as_str();
        assert_eq!(
            *id_of_class.entry(class).or_insert(&case.id),
            case.id,
            "{}",
            case.name
        );
        assert_eq!(
            *class_of_id.entry(&case.id).or_insert(class),
            class,
            "{}",
            case.name
        );
    }
}

/// A reading that is not `Known` never matches a positive value: this is the
/// "missing never means accepted" rule, checked over every valid vector.
#[test]
fn unknown_and_unrecognized_states_never_match_a_value() {
    let vectors = vectors();
    let readers = readers(&vectors);
    for case in &vectors.valid {
        let record = Record::decode(&hex(&case.input.hex), &readers[&case.reader]).unwrap();
        for field in case.states.keys() {
            let state = record.state(field);
            if !matches!(state, StateReading::Known(_)) {
                for value in ["accepted", "captured", "shared", "superseded", "unknown"] {
                    assert!(!state.is(value), "{}: {field} read as {value}", case.name);
                }
            }
        }
    }
}

#[test]
fn invalid_vectors_are_refused_with_the_expected_code() {
    let vectors = vectors();
    let readers = readers(&vectors);
    for case in &vectors.invalid {
        let error = Record::decode(&hex(&case.input.hex), &readers[&case.reader])
            .err()
            .unwrap_or_else(|| panic!("{}: accepted an invalid record", case.name));
        assert_eq!(error.code(), case.error, "{}: {error}", case.name);
    }
}

#[test]
fn record_ids_round_trip_and_only_the_canonical_form_parses() {
    for case in vectors().valid {
        assert_eq!(RecordId::parse(&case.id).unwrap().as_str(), case.id);
        let upper = case.id.to_uppercase().replacen("SHA256", "sha256", 1);
        assert_eq!(RecordId::parse(&upper).unwrap_err().code(), "malformed_id");
    }
}

/// Writers take `requires` from the schema, so a gated field can never be
/// written without its capability, and an ungated one adds nothing.
#[test]
fn required_capabilities_follow_the_fields_present() {
    let vectors = vectors();
    let readers = readers(&vectors);
    let new = readers["new"][0];
    assert_eq!(
        new.required_capabilities(["schema", "subject", "sharing", "note"]),
        ["retention-receipt"]
    );
    assert!(new.required_capabilities(["subject", "note"]).is_empty());
    // Every record the new reader accepts already declares what it needs.
    for case in vectors.valid.iter().filter(|case| case.reader == "new") {
        let record = Record::decode(&hex(&case.input.hex), &readers["new"]).unwrap();
        let fields: Vec<String> = ["sharing", "note", "subject"]
            .into_iter()
            .filter(|field| record.get(field).is_some())
            .map(String::from)
            .collect();
        for capability in new.required_capabilities(fields.iter().map(String::as_str)) {
            let declared = record.get("requires").is_some_and(|requires| {
                matches!(requires, Value::Array(items)
                    if items.contains(&Value::String(capability.into())))
            });
            assert!(declared, "{}: {capability} not declared", case.name);
        }
    }
}
