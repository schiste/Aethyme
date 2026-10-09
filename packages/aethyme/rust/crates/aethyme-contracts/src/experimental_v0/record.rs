//! Versioned records: the envelope, digest and compatibility rules every v0
//! record shares (plan §5.3, §5.6, §6.19; #653).
//!
//! This module fixes how a record is decoded, identified and read by an older
//! or newer client. It does not define the concrete records (ContributionRevision,
//! receipts, …): their fields wait for E1 and for two real consumers (§5.1). A
//! consumer describes each record type with a [`RecordSchema`].
//!
//! ## Envelope
//!
//! A record is a JSON object in the [canonical profile](super::canonical_json)
//! with three reserved members:
//!
//! - `schema` (required): the record type and version, e.g.
//!   `aethyme.contribution-revision/experimental-v0`. A reader that does not
//!   know it refuses the record (`unsupported_schema`); there is no "closest
//!   version" fallback.
//! - `requires` (optional): a set of capability names. A writer lists every
//!   capability a reader must understand to interpret the record safely; a
//!   reader refuses a record requiring one it lacks (`unsupported_capability`).
//!   This is how a newer writer stops an older reader from acting on a record
//!   it would misread. An empty set is the same as an absent one.
//! - `extensions` (optional): an object a reader carries without interpreting.
//!   It is part of the digest, so it survives a round trip unchanged.
//!
//! ## Identity
//!
//! A record's ID is `sha256:` over `"aethyme record v0" NUL` followed by its
//! canonical bytes. Before hashing, set-valued fields are sorted (by UTF-16 code
//! units, like object keys) and refused if they repeat a member, so two encodings
//! of the same record always share an ID. Order-significant lists are arrays and
//! keep their order. A record never contains its own ID, a signature, a locator
//! or an observation time (§5.3); those live beside it.
//!
//! ## Missing never means positive
//!
//! A state field (`capture`, `acceptance`, …) is read through
//! [`Record::state`], which distinguishes a known value from `Unknown` (absent,
//! or explicitly `"unknown"`) and `Unrecognized` (a value from a newer writer).
//! An explicit `"unknown"` is dropped on decode, so it and an absent field are
//! one encoding with one ID.
//! Neither of the last two can be matched as `accepted`, `complete` or
//! `trusted`, so an old record or an old reader degrades to "unknown" and never
//! to a stronger claim (D18, D47, T85).

use super::canonical_json::{self, CanonicalJsonError, Object, Value, utf16_order};
use super::digest;

/// Domain-separation header hashed before a record's canonical bytes.
pub const RECORD_DOMAIN: &[u8] = b"aethyme record v0\0";

/// Longest accepted decimal string: enough for any unsigned 256-bit value.
pub const MAX_DECIMAL_DIGITS: usize = 78;

/// The reserved envelope members, which no schema may redeclare.
pub const RESERVED_FIELDS: [&str; 3] = ["schema", "requires", "extensions"];

/// The value `"unknown"`, valid in every state field.
pub const UNKNOWN_STATE: &str = "unknown";

/// What one reader knows about one record type and version.
#[derive(Debug)]
pub struct RecordSchema {
    /// The exact `schema` value, e.g. `aethyme.example/experimental-v0`.
    pub name: &'static str,
    pub fields: &'static [FieldSpec],
    /// Capabilities this reader understands for this schema.
    pub capabilities: &'static [&'static str],
}

impl RecordSchema {
    fn field(&self, name: &str) -> Option<&FieldSpec> {
        self.fields.iter().find(|field| field.name == name)
    }
}

#[derive(Debug)]
pub struct FieldSpec {
    pub name: &'static str,
    pub required: bool,
    pub kind: FieldKind,
}

#[derive(Debug)]
pub enum FieldKind {
    Boolean,
    /// An integer within the profile's safe range.
    Integer,
    String,
    /// A non-negative integer as a decimal string with no leading zeros, for
    /// counters and quantities beyond the safe integer range.
    DecimalString,
    /// A set of strings: order-insensitive, sorted before hashing, no repeats.
    StringSet,
    /// An orthogonal state with these known values. Always optional: absent
    /// reads as `Unknown`, whatever `required` says.
    State(&'static [&'static str]),
    /// Any profile value, carried as-is.
    Opaque,
}

/// How a state field reads. Only `Known` can match a specific value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateReading<'a> {
    Known(&'a str),
    /// Absent, or explicitly `"unknown"`.
    Unknown,
    /// A value this reader's schema does not list: written by a newer client.
    Unrecognized(&'a str),
}

impl StateReading<'_> {
    /// True only for a known reading of exactly `value`.
    pub fn is(&self, value: &str) -> bool {
        matches!(self, Self::Known(known) if *known == value)
    }
}

/// The digest identity of a record: `sha256:<64 lowercase hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RecordId(String);

impl RecordId {
    /// Parse an encoded ID; only the exact canonical form is accepted.
    pub fn parse(encoded: &str) -> Result<Self, RecordError> {
        if digest::is_canonical(encoded) {
            Ok(Self(encoded.to_string()))
        } else {
            Err(RecordError::MalformedId {
                encoded: encoded.to_string(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RecordId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A decoded record: validated against one schema, in canonical form.
#[derive(Debug, Clone)]
pub struct Record {
    schema: &'static RecordSchema,
    object: Object,
}

impl Record {
    /// Decode `input` with the first schema whose name matches its `schema`
    /// member. The first violation found is reported.
    pub fn decode(input: &[u8], schemas: &[&'static RecordSchema]) -> Result<Self, RecordError> {
        let Value::Object(object) = canonical_json::parse(input)? else {
            return Err(RecordError::NotAnObject);
        };
        let schema_name = match object.get("schema") {
            Some(Value::String(name)) => name,
            Some(_) => {
                return Err(RecordError::WrongType {
                    field: "schema".into(),
                });
            }
            None => {
                return Err(RecordError::MissingField {
                    field: "schema".into(),
                });
            }
        };
        let schema = schemas
            .iter()
            .copied()
            .find(|schema| schema.name == schema_name)
            .ok_or_else(|| RecordError::UnsupportedSchema {
                schema: schema_name.clone(),
            })?;

        let mut members = Vec::with_capacity(object.len());
        // `None` drops a member whose meaning equals its absence (an empty
        // `requires`, an explicit "unknown" state), so both spellings encode,
        // and hash, as absent.
        for (name, value) in object.iter() {
            let kept = match name {
                "schema" => Some(value.clone()),
                "requires" => {
                    let set = string_set(name, value)?;
                    if let Some(missing) = set
                        .iter()
                        .find(|capability| !schema.capabilities.contains(&string_of(capability)))
                    {
                        return Err(RecordError::UnsupportedCapability {
                            capability: string_of(missing).to_string(),
                        });
                    }
                    (!set.is_empty()).then_some(Value::Array(set))
                }
                "extensions" => match value {
                    Value::Object(_) => Some(value.clone()),
                    _ => return Err(RecordError::WrongType { field: name.into() }),
                },
                _ => match schema.field(name) {
                    Some(spec) => {
                        let checked = check_field(spec, value)?;
                        let unknown_state = matches!(spec.kind, FieldKind::State(_))
                            && checked == Value::String(UNKNOWN_STATE.into());
                        (!unknown_state).then_some(checked)
                    }
                    None => {
                        admit_unknown_field(schema, name, value)?;
                        Some(value.clone())
                    }
                },
            };
            if let Some(value) = kept {
                members.push((name.to_string(), value));
            }
        }
        if let Some(missing) = schema.fields.iter().find(|field| {
            field.required
                && !matches!(field.kind, FieldKind::State(_))
                && object.get(field.name).is_none()
        }) {
            return Err(RecordError::MissingField {
                field: missing.name.into(),
            });
        }
        let object = Object::new(members).expect("keys came from a valid object");
        Ok(Self { schema, object })
    }

    pub fn schema(&self) -> &'static str {
        self.schema.name
    }

    pub fn get(&self, field: &str) -> Option<&Value> {
        self.object.get(field)
    }

    /// The bytes the ID hashes, after the domain header.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        Value::Object(self.object.clone()).to_canonical_bytes()
    }

    pub fn id(&self) -> RecordId {
        let mut preimage = RECORD_DOMAIN.to_vec();
        preimage.extend_from_slice(&self.canonical_bytes());
        RecordId(digest::encode(&preimage))
    }

    /// Read a state field.
    ///
    /// # Panics
    ///
    /// If `field` is not a state field of this record's schema: that is a bug
    /// in the caller, not a property of the record.
    pub fn state(&self, field: &str) -> StateReading<'_> {
        let Some(FieldSpec {
            kind: FieldKind::State(known),
            ..
        }) = self.schema.field(field)
        else {
            panic!("{field:?} is not a state field of {}", self.schema.name);
        };
        match self.object.get(field) {
            None => StateReading::Unknown,
            Some(Value::String(value)) if value == UNKNOWN_STATE => StateReading::Unknown,
            Some(Value::String(value)) if known.contains(&value.as_str()) => {
                StateReading::Known(value)
            }
            Some(Value::String(value)) => StateReading::Unrecognized(value),
            Some(_) => unreachable!("decode checked that state fields are strings"),
        }
    }
}

/// Decide whether a reader accepts a top-level member its schema does not
/// declare, outside `extensions`. Typically an old reader meeting a field a
/// newer writer added.
fn admit_unknown_field(
    schema: &RecordSchema,
    name: &str,
    value: &Value,
) -> Result<(), RecordError> {
    // TODO(#653): choose the policy; see the PR discussion.
    let _ = (schema, value);
    Err(RecordError::UnknownField { field: name.into() })
}

fn check_field(spec: &FieldSpec, value: &Value) -> Result<Value, RecordError> {
    let wrong_type = || RecordError::WrongType {
        field: spec.name.into(),
    };
    match (&spec.kind, value) {
        (FieldKind::Boolean, Value::Bool(_))
        | (FieldKind::Integer, Value::Integer(_))
        | (FieldKind::String, Value::String(_))
        | (FieldKind::State(_), Value::String(_))
        | (FieldKind::Opaque, _) => Ok(value.clone()),
        (FieldKind::DecimalString, Value::String(digits)) => {
            let canonical = !digits.is_empty()
                && digits.len() <= MAX_DECIMAL_DIGITS
                && digits.bytes().all(|b| b.is_ascii_digit())
                && (digits == "0" || !digits.starts_with('0'));
            if canonical {
                Ok(value.clone())
            } else {
                Err(RecordError::InvalidDecimal {
                    field: spec.name.into(),
                })
            }
        }
        (FieldKind::StringSet, _) => string_set(spec.name, value).map(Value::Array),
        _ => Err(wrong_type()),
    }
}

/// Validate a set of strings and return it in canonical order.
fn string_set(field: &str, value: &Value) -> Result<Vec<Value>, RecordError> {
    let Value::Array(items) = value else {
        return Err(RecordError::WrongType {
            field: field.into(),
        });
    };
    if !items.iter().all(|item| matches!(item, Value::String(_))) {
        return Err(RecordError::WrongType {
            field: field.into(),
        });
    }
    let mut items = items.clone();
    items.sort_by(|a, b| utf16_order(string_of(a), string_of(b)));
    if let Some(pair) = items.windows(2).find(|pair| pair[0] == pair[1]) {
        return Err(RecordError::DuplicateSetMember {
            field: field.into(),
            member: string_of(&pair[0]).to_string(),
        });
    }
    Ok(items)
}

fn string_of(value: &Value) -> &str {
    match value {
        Value::String(s) => s,
        _ => unreachable!("checked to be a string"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    #[error(transparent)]
    Json(#[from] CanonicalJsonError),
    #[error("a record must be a JSON object")]
    NotAnObject,
    #[error("unsupported record schema {schema:?}")]
    UnsupportedSchema { schema: String },
    #[error("record requires capability {capability:?}, which this reader does not support")]
    UnsupportedCapability { capability: String },
    #[error("missing required field {field:?}")]
    MissingField { field: String },
    #[error("field {field:?} has the wrong type")]
    WrongType { field: String },
    #[error("field {field:?} is not declared by this schema")]
    UnknownField { field: String },
    #[error("field {field:?} must be a decimal string without leading zeros")]
    InvalidDecimal { field: String },
    #[error("set field {field:?} repeats {member:?}")]
    DuplicateSetMember { field: String, member: String },
    #[error("malformed record id {encoded:?}: expected sha256:<64 lowercase hex>")]
    MalformedId { encoded: String },
}

impl RecordError {
    /// The stable refusal code shared with the golden vectors. Profile
    /// violations keep their [`CanonicalJsonError::code`].
    pub fn code(&self) -> &'static str {
        match self {
            Self::Json(error) => error.code(),
            Self::NotAnObject => "not_an_object",
            Self::UnsupportedSchema { .. } => "unsupported_schema",
            Self::UnsupportedCapability { .. } => "unsupported_capability",
            Self::MissingField { .. } => "missing_field",
            Self::WrongType { .. } => "wrong_type",
            Self::UnknownField { .. } => "unknown_field",
            Self::InvalidDecimal { .. } => "invalid_decimal",
            Self::DuplicateSetMember { .. } => "duplicate_set_member",
            Self::MalformedId { .. } => "malformed_id",
        }
    }
}
