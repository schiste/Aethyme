//! Experimental, advisory symbol references and exact observation bindings
//! (plan §6.12, D39, T66). Existing graph IDs remain opaque and unchanged.
//!
//! Producer-local symbols are scoped by producer and document. External
//! package symbols retain package, version and producer data. Unknown scope
//! kinds remain readable but cannot participate in a mapping. A mapping is
//! only a claim until the broker adapter resolves its exact canonical binding,
//! observations and two independent origin records.

use super::canonical_json::{self, Object, Value};
use super::record::{
    FieldKind, FieldSpec, Record, RecordError, RecordId, RecordSchema, StateReading,
};
use super::source_snapshot::{EntryKind, SourceEntry, SourceSnapshot, SourceSnapshotId};

pub const SYMBOL_REF_SCHEMA_NAME: &str = "aethyme.symbol-ref/experimental-v0";
pub const OBSERVED_SYMBOL_REVISION_SCHEMA_NAME: &str =
    "aethyme.observed-symbol-revision/experimental-v0";
pub const SYMBOL_ORIGIN_EVIDENCE_SCHEMA_NAME: &str =
    "aethyme.symbol-origin-evidence/experimental-v0";
pub const EXPLICIT_SYMBOL_MAPPING_EVIDENCE_SCHEMA_NAME: &str =
    "aethyme.explicit-symbol-mapping-evidence/experimental-v0";
pub const SYMBOL_MAPPING_CLAIM_SCHEMA_NAME: &str =
    "aethyme.symbol-mapping-claim/experimental-v0";
pub const SYMBOL_MAPPING_AUTHORITY: &str = "advisory_symbol_mapping";

/// Symbol records are deliberately much smaller than the general record
/// profile so an adapter cannot be made to allocate a large opaque payload.
pub const MAX_SYMBOL_RECORD_BYTES: usize = 16 * 1024;
pub const MAX_SYMBOL_TEXT_BYTES: usize = 2048;

const CLAIM_STATES: &[&str] = &["proposed", "unknown"];

const fn field(name: &'static str, required: bool, kind: FieldKind) -> FieldSpec {
    FieldSpec {
        name,
        required,
        kind,
        capability: None,
    }
}

pub static SYMBOL_REF_SCHEMA: RecordSchema = RecordSchema {
    name: SYMBOL_REF_SCHEMA_NAME,
    fields: &[
        field("project_id", true, FieldKind::String),
        field("clone_id", true, FieldKind::String),
        field("scope", true, FieldKind::Opaque),
    ],
    capabilities: &[],
};

pub static OBSERVED_SYMBOL_REVISION_SCHEMA: RecordSchema = RecordSchema {
    name: OBSERVED_SYMBOL_REVISION_SCHEMA_NAME,
    fields: &[
        field("symbol_ref", true, FieldKind::String),
        field("source_snapshot", true, FieldKind::String),
        field("profile", true, FieldKind::String),
        field("fact_revision", true, FieldKind::String),
        field("origin_record", true, FieldKind::String),
    ],
    capabilities: &[],
};

pub static SYMBOL_ORIGIN_EVIDENCE_SCHEMA: RecordSchema = RecordSchema {
    name: SYMBOL_ORIGIN_EVIDENCE_SCHEMA_NAME,
    fields: &[
        field("authority", true, FieldKind::String),
        field("project_id", true, FieldKind::String),
        field("clone_id", true, FieldKind::String),
        field("symbol_ref", true, FieldKind::String),
        field("source_snapshot", true, FieldKind::String),
        field("profile", true, FieldKind::String),
        field("fact_revision", true, FieldKind::String),
        field("origin_namespace", true, FieldKind::String),
    ],
    capabilities: &[],
};

pub static EXPLICIT_SYMBOL_MAPPING_EVIDENCE_SCHEMA: RecordSchema = RecordSchema {
    name: EXPLICIT_SYMBOL_MAPPING_EVIDENCE_SCHEMA_NAME,
    fields: &[
        field("authority", true, FieldKind::String),
        field("project_id", true, FieldKind::String),
        field("left_clone_id", true, FieldKind::String),
        field("right_clone_id", true, FieldKind::String),
        field("left_source_snapshot", true, FieldKind::String),
        field("right_source_snapshot", true, FieldKind::String),
        field("profile", true, FieldKind::String),
        field("left_symbol_ref", true, FieldKind::String),
        field("right_symbol_ref", true, FieldKind::String),
        field("left_observation", true, FieldKind::String),
        field("right_observation", true, FieldKind::String),
        field("left_origin", true, FieldKind::String),
        field("right_origin", true, FieldKind::String),
    ],
    capabilities: &[],
};

pub static SYMBOL_MAPPING_CLAIM_SCHEMA: RecordSchema = RecordSchema {
    name: SYMBOL_MAPPING_CLAIM_SCHEMA_NAME,
    fields: &[
        field("authority", true, FieldKind::String),
        field("binding", true, FieldKind::String),
        field("evidence", true, FieldKind::StringSet),
        field("left_observation", true, FieldKind::String),
        field("right_observation", true, FieldKind::String),
        field("state", true, FieldKind::State(CLAIM_STATES)),
    ],
    capabilities: &[],
};

pub static SYMBOL_IDENTITY_SCHEMAS: [&RecordSchema; 5] = [
    &SYMBOL_REF_SCHEMA,
    &OBSERVED_SYMBOL_REVISION_SCHEMA,
    &SYMBOL_ORIGIN_EVIDENCE_SCHEMA,
    &EXPLICIT_SYMBOL_MAPPING_EVIDENCE_SCHEMA,
    &SYMBOL_MAPPING_CLAIM_SCHEMA,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolScopeKind {
    EngineNode,
    ProducerLocal,
    ExternalPackage,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SymbolIdentityError {
    #[error("symbol record is larger than {MAX_SYMBOL_RECORD_BYTES} bytes")]
    TooLarge,
    #[error("symbol record is not canonical JSON")]
    NonCanonical,
    #[error("symbol record field {0:?} is malformed or out of scope")]
    InvalidField(&'static str),
    #[error(transparent)]
    Record(#[from] RecordError),
}

impl SymbolIdentityError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::NonCanonical => "non_canonical",
            Self::InvalidField(_) => "invalid_field",
            Self::Record(error) => error.code(),
        }
    }
}

/// Decode one bounded, canonical record and validate its exact known fields.
/// Unknown scope *values* are retained, but unknown record schemas or fields
/// are refused. This function does not establish mapping authority.
pub fn decode_symbol_identity_record(input: &[u8]) -> Result<Record, SymbolIdentityError> {
    if input.len() > MAX_SYMBOL_RECORD_BYTES {
        return Err(SymbolIdentityError::TooLarge);
    }
    let value = canonical_json::parse(input).map_err(RecordError::from)?;
    let canonical_input = value.to_canonical_bytes();
    if canonical_input != input {
        return Err(SymbolIdentityError::NonCanonical);
    }
    let Value::Object(raw) = value else {
        return Err(SymbolIdentityError::InvalidField("record"));
    };
    let record = Record::decode(input, &SYMBOL_IDENTITY_SCHEMAS)?;
    let known_fields = SYMBOL_IDENTITY_SCHEMAS
        .iter()
        .find(|schema| schema.name == record.schema())
        .map(|schema| schema.fields)
        .ok_or(SymbolIdentityError::InvalidField("schema"))?;
    for (name, _) in raw.iter() {
        if name != "schema" && !known_fields.iter().any(|field| field.name == name) {
            return Err(SymbolIdentityError::InvalidField("unknown_record_field"));
        }
    }
    validate_record(&record)?;
    Ok(record)
}

/// Read the scope discriminator of a validated SymbolRef record.
pub fn symbol_scope_kind(record: &Record) -> Option<SymbolScopeKind> {
    if record.schema() != SYMBOL_REF_SCHEMA_NAME {
        return None;
    }
    let Value::Object(scope) = record.get("scope")? else {
        return None;
    };
    let Value::String(kind) = scope.get("kind")? else {
        return None;
    };
    Some(match kind.as_str() {
        "engine_node" => SymbolScopeKind::EngineNode,
        "producer_local" => SymbolScopeKind::ProducerLocal,
        "external_package" => SymbolScopeKind::ExternalPackage,
        _ => SymbolScopeKind::Unknown,
    })
}

fn validate_record(record: &Record) -> Result<(), SymbolIdentityError> {
    match record.schema() {
        SYMBOL_REF_SCHEMA_NAME => {
            record_id(record, "project_id")?;
            record_id(record, "clone_id")?;
            validate_scope(record.get("scope"))?;
        }
        OBSERVED_SYMBOL_REVISION_SCHEMA_NAME => {
            record_id(record, "symbol_ref")?;
            source_id(record, "source_snapshot")?;
            record_id(record, "profile")?;
            record_id(record, "fact_revision")?;
            record_id(record, "origin_record")?;
        }
        SYMBOL_ORIGIN_EVIDENCE_SCHEMA_NAME => {
            advisory(record)?;
            record_id(record, "project_id")?;
            record_id(record, "clone_id")?;
            record_id(record, "symbol_ref")?;
            source_id(record, "source_snapshot")?;
            record_id(record, "profile")?;
            record_id(record, "fact_revision")?;
            bounded_text(record, "origin_namespace")?;
        }
        EXPLICIT_SYMBOL_MAPPING_EVIDENCE_SCHEMA_NAME => {
            advisory(record)?;
            record_id(record, "project_id")?;
            let left_clone = record_id(record, "left_clone_id")?;
            let right_clone = record_id(record, "right_clone_id")?;
            if left_clone == right_clone {
                return Err(SymbolIdentityError::InvalidField("right_clone_id"));
            }
            source_id(record, "left_source_snapshot")?;
            source_id(record, "right_source_snapshot")?;
            record_id(record, "profile")?;
            for field in [
                "left_symbol_ref",
                "right_symbol_ref",
                "left_observation",
                "right_observation",
                "left_origin",
                "right_origin",
            ] {
                record_id(record, field)?;
            }
            if required_text(record, "left_origin")? == required_text(record, "right_origin")? {
                return Err(SymbolIdentityError::InvalidField("right_origin"));
            }
        }
        SYMBOL_MAPPING_CLAIM_SCHEMA_NAME => {
            advisory(record)?;
            record_id(record, "binding")?;
            record_id(record, "left_observation")?;
            record_id(record, "right_observation")?;
            let evidence = match record.get("evidence") {
                Some(Value::Array(items)) => items,
                _ => return Err(SymbolIdentityError::InvalidField("evidence")),
            };
            if evidence.len() != 2 {
                return Err(SymbolIdentityError::InvalidField("evidence"));
            }
            for item in evidence {
                let Value::String(id) = item else {
                    return Err(SymbolIdentityError::InvalidField("evidence"));
                };
                RecordId::parse(id)?;
            }
            // A missing, unknown or newer state remains non-positive. The
            // broker adapter establishes a mapping only for `proposed`.
            match record.state("state") {
                StateReading::Known("proposed")
                | StateReading::Unknown
                | StateReading::Unrecognized(_) => {}
                StateReading::Known(_) => return Err(SymbolIdentityError::InvalidField("state")),
            }
        }
        _ => return Err(SymbolIdentityError::InvalidField("schema")),
    }
    Ok(())
}

fn validate_scope(value: Option<&Value>) -> Result<(), SymbolIdentityError> {
    let Some(Value::Object(scope)) = value else {
        return Err(SymbolIdentityError::InvalidField("scope"));
    };
    let Some(Value::String(kind)) = scope.get("kind") else {
        return Err(SymbolIdentityError::InvalidField("scope.kind"));
    };
    match kind.as_str() {
        "engine_node" => {
            exact_keys(scope, &["kind", "node_id"], "scope")?;
            bounded_scope_text(scope, "node_id")?;
        }
        "producer_local" => {
            exact_keys(
                scope,
                &["kind", "producer_namespace", "document", "local_symbol"],
                "scope",
            )?;
            bounded_scope_text(scope, "producer_namespace")?;
            bounded_scope_text(scope, "local_symbol")?;
            let document = scope_text(scope, "document")?;
            if document.len() > 4096
                || SourceSnapshot::new([SourceEntry::from_content(
                    document.as_bytes().to_vec(),
                    EntryKind::Regular,
                    b"",
                )])
                .is_err()
            {
                return Err(SymbolIdentityError::InvalidField("scope.document"));
            }
        }
        "external_package" => {
            exact_keys(
                scope,
                &["kind", "producer_namespace", "package", "version", "symbol"],
                "scope",
            )?;
            for field in ["producer_namespace", "package", "version", "symbol"] {
                bounded_scope_text(scope, field)?;
            }
        }
        // A future producer may extend this closed vocabulary. It is preserved
        // in the record but never parsed as a known symbol or mapped.
        _ => {}
    }
    Ok(())
}

fn exact_keys(
    object: &Object,
    expected: &[&str],
    field: &'static str,
) -> Result<(), SymbolIdentityError> {
    if object.len() != expected.len() || object.iter().any(|(key, _)| !expected.contains(&key)) {
        return Err(SymbolIdentityError::InvalidField(field));
    }
    Ok(())
}

fn advisory(record: &Record) -> Result<(), SymbolIdentityError> {
    if required_text(record, "authority")? == SYMBOL_MAPPING_AUTHORITY {
        Ok(())
    } else {
        Err(SymbolIdentityError::InvalidField("authority"))
    }
}

fn record_id(record: &Record, field: &'static str) -> Result<RecordId, SymbolIdentityError> {
    RecordId::parse(required_text(record, field)?).map_err(Into::into)
}

fn source_id(record: &Record, field: &'static str) -> Result<SourceSnapshotId, SymbolIdentityError> {
    SourceSnapshotId::parse(required_text(record, field)?).map_err(|_| {
        SymbolIdentityError::InvalidField(field)
    })
}

fn required_text<'a>(record: &'a Record, field: &'static str) -> Result<&'a str, SymbolIdentityError> {
    let Some(Value::String(value)) = record.get(field) else {
        return Err(SymbolIdentityError::InvalidField(field));
    };
    if value.is_empty() || value.len() > MAX_SYMBOL_TEXT_BYTES {
        return Err(SymbolIdentityError::InvalidField(field));
    }
    Ok(value)
}

fn bounded_text(record: &Record, field: &'static str) -> Result<(), SymbolIdentityError> {
    required_text(record, field).map(|_| ())
}

fn scope_text<'a>(scope: &'a Object, field: &'static str) -> Result<&'a str, SymbolIdentityError> {
    let Some(Value::String(value)) = scope.get(field) else {
        return Err(SymbolIdentityError::InvalidField(field));
    };
    if value.is_empty() || value.len() > MAX_SYMBOL_TEXT_BYTES {
        return Err(SymbolIdentityError::InvalidField(field));
    }
    Ok(value)
}

fn bounded_scope_text(scope: &Object, field: &'static str) -> Result<(), SymbolIdentityError> {
    scope_text(scope, field).map(|_| ())
}
