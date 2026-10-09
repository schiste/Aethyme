//! The decision brief: an agent's bounded hand-off of the choices that are not
//! obvious from the change itself (plan §5.5, N4; D11; #654).
//!
//! The broker captures source, base, paths and test facts mechanically. The
//! brief carries only what an agent decided and why, in at most
//! [`MAX_BRIEF_TOKENS`] tokens. It is attributed data for the next reader, not
//! instructions and not a transcript: full conversations and reasoning are
//! never captured.
//!
//! ## Token profile `aethyme-brief-tokens/v0` (D11)
//!
//! Tokens are counted by a written rule, not a model vocabulary, so every
//! client (Rust, TypeScript, a shell script) gets exactly the same count with
//! no dependency or licensed asset. Each Unicode scalar value belongs to one
//! class:
//!
//! | Class | Characters | Cost |
//! |---|---|---|
//! | letter | ASCII `A`–`Z`, `a`–`z` | 1 token per started group of 4 in a run |
//! | digit | ASCII `0`–`9` | 1 token per started group of 2 in a run |
//! | two-byte | `U+0080`–`U+07FF` (Latin accents, Greek, Cyrillic, Hebrew, Arabic, …) | 1 token per started group of 2 in a run |
//! | space | ASCII space, tab, line feed, carriage return | free; ends a run |
//! | other | everything else (ASCII punctuation, CJK, emoji, …) | 1 token each; ends a run |
//!
//! A run is a maximal sequence of one class. Counting is per string and
//! additive, so field order and JSON structure never change the total: the
//! brief's count is the sum over its prose strings. The classes depend only on
//! code point ranges, not on Unicode tables, so the count cannot drift with a
//! Unicode version.
//!
//! These are Aethyme tokens, not a model's. Calibrated on 27 brief-like
//! strings in 12 scripts, the profile counted 1.07–2.00 times OpenAI's
//! `o200k_base` (mean 1.45) and never fewer, so 150 profile tokens stay within
//! about 150 model tokens. The calibration is evidence for the bound, not part
//! of the definition.
//!
//! ## Brief rules
//!
//! - `intent` is required. `decisions` (`scope_ref`, `choice`, `reason`),
//!   `preserves`, `assumptions` and `deferred` are optional lists of at most
//!   [`MAX_LIST_ENTRIES`] each. A mechanical change can give a short intent
//!   and nothing else; nothing asks for invented rationale.
//! - Every agent-authored string counts toward the budget, `scope_ref`
//!   included, and `scope_ref` must look like an identifier, so it cannot
//!   carry prose. Prose bytes are capped separately ([`MAX_PROSE_BYTES`]),
//!   because whitespace is free.
//! - No empty strings, no control characters other than line feed, no bidi
//!   controls (they can make text read differently from its bytes).
//! - Over a limit is refused, never truncated. Every problem is reported at
//!   once, each with its path (`decisions[1].reason`), so an agent can fix the
//!   brief in one round.
//!
//! ## Decision file and record
//!
//! An agent writes a **decision file**: a JSON object with only the brief
//! fields. Unknown members are refused, because there a typo (`assumption`)
//! would otherwise silently drop text. The tool turns it into a **brief
//! record** ([`Brief::to_record`]), adding `schema` and `token_profile` and
//! deriving `requires` from the schema; the agent never sets those. A record
//! follows the record rules of [`super::record`]: a newer writer's unknown
//! members are carried, bounded by [`MAX_BRIEF_RECORD_BYTES`].
//!
//! A revised brief is a new record with a new ID; it never edits the captured
//! source or an earlier brief. Whether a contribution must carry a brief is a
//! policy decision outside this module; an absent brief is reported as absent,
//! never filled in.

use super::canonical_json::{self, Object, Value};
use super::record::{FieldKind, FieldSpec, Record, RecordId, RecordSchema};

/// Name of the counting profile; recorded in every brief record.
pub const TOKEN_PROFILE: &str = "aethyme-brief-tokens/v0";

/// The `schema` of a brief record.
pub const BRIEF_SCHEMA_NAME: &str = "aethyme.decision-brief/experimental-v0";

/// Most tokens of agent-authored text in one brief (N4).
pub const MAX_BRIEF_TOKENS: usize = 150;

/// Most bytes of agent-authored text in one brief, counted independently of
/// tokens because whitespace costs no tokens.
pub const MAX_PROSE_BYTES: usize = 2048;

/// Most entries in each list field.
pub const MAX_LIST_ENTRIES: usize = 8;

/// Longest `scope_ref`, in bytes.
pub const MAX_SCOPE_REF_BYTES: usize = 64;

/// Largest brief record, including members carried for newer writers.
pub const MAX_BRIEF_RECORD_BYTES: usize = 8192;

const LIST_FIELDS: [&str; 3] = ["preserves", "assumptions", "deferred"];
const DECISION_FIELDS: [&str; 3] = ["scope_ref", "choice", "reason"];

/// The brief record schema. No field is gated by a capability: every v0
/// reader interprets all of them.
pub static BRIEF_SCHEMA: RecordSchema = RecordSchema {
    name: BRIEF_SCHEMA_NAME,
    fields: &[
        FieldSpec {
            name: "token_profile",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "intent",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "decisions",
            required: false,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "preserves",
            required: false,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "assumptions",
            required: false,
            kind: FieldKind::Opaque,
            capability: None,
        },
        FieldSpec {
            name: "deferred",
            required: false,
            kind: FieldKind::Opaque,
            capability: None,
        },
    ],
    capabilities: &[],
};

/// Count `text` under `aethyme-brief-tokens/v0`.
pub fn count_tokens(text: &str) -> usize {
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Class {
        Letter,
        Digit,
        TwoByte,
    }
    let per_token = |class| match class {
        Class::Letter => 4,
        Class::Digit | Class::TwoByte => 2,
    };
    let mut total = 0;
    let mut run: Option<(Class, usize)> = None;
    for ch in text.chars() {
        let class = match ch {
            'A'..='Z' | 'a'..='z' => Some(Class::Letter),
            '0'..='9' => Some(Class::Digit),
            '\u{80}'..='\u{7FF}' => Some(Class::TwoByte),
            _ => None,
        };
        if let (Some((current, length)), Some(next)) = (run, class)
            && current == next
        {
            run = Some((current, length + 1));
            continue;
        }
        if let Some((current, length)) = run.take() {
            total += length.div_ceil(per_token(current));
        }
        match class {
            Some(next) => run = Some((next, 1)),
            None if matches!(ch, ' ' | '\t' | '\n' | '\r') => {}
            None => total += 1,
        }
    }
    if let Some((current, length)) = run {
        total += length.div_ceil(per_token(current));
    }
    total
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Brief {
    pub intent: String,
    pub decisions: Vec<Decision>,
    pub preserves: Vec<String>,
    pub assumptions: Vec<String>,
    pub deferred: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    /// A short identifier for what the decision is about (`search-control`).
    pub scope_ref: String,
    pub choice: String,
    pub reason: String,
}

impl Brief {
    /// Read an agent's decision file. Unknown members are refused.
    pub fn from_decision_file(input: &[u8]) -> Result<Self, BriefErrors> {
        let object = match canonical_json::parse(input) {
            Ok(Value::Object(object)) => object,
            Ok(_) => return Err(BriefError::at("", BriefErrorKind::NotAnObject).into()),
            Err(error) => return Err(BriefError::at("", BriefErrorKind::Json(error)).into()),
        };
        let mut errors = Vec::new();
        for (name, _) in object.iter() {
            if name != "intent" && name != "decisions" && !LIST_FIELDS.contains(&name) {
                errors.push(BriefError::at(name, BriefErrorKind::UnknownField));
            }
        }
        let brief = read_fields(|name| object.get(name), false, &mut errors);
        finish(brief, errors)
    }

    /// Read a brief record, checking the record rules, the token profile and
    /// every brief rule. Returns the brief and the record's ID.
    pub fn from_record(input: &[u8]) -> Result<(Self, RecordId), BriefErrors> {
        if input.len() > MAX_BRIEF_RECORD_BYTES {
            return Err(BriefError::at("", BriefErrorKind::RecordTooLarge).into());
        }
        let record = Record::decode(input, &[&BRIEF_SCHEMA])
            .map_err(|error| BriefError::at("", BriefErrorKind::Record(error)))?;
        match record.get("token_profile") {
            Some(Value::String(profile)) if profile == TOKEN_PROFILE => {}
            other => {
                let profile = match other {
                    Some(Value::String(profile)) => profile.clone(),
                    _ => String::new(),
                };
                return Err(BriefError::at(
                    "token_profile",
                    BriefErrorKind::UnsupportedProfile { profile },
                )
                .into());
            }
        }
        let mut errors = Vec::new();
        let brief = read_fields(|name| record.get(name), true, &mut errors);
        finish(brief, errors).map(|brief| (brief, record.id()))
    }

    /// Every rule this brief breaks, in field order; empty when valid.
    pub fn validate(&self) -> Vec<BriefError> {
        let mut errors = Vec::new();
        check_prose("intent", &self.intent, &mut errors);
        check_entries("decisions", self.decisions.len(), &mut errors);
        for (index, decision) in self.decisions.iter().enumerate() {
            let path = format!("decisions[{index}].scope_ref");
            if !is_scope_ref(&decision.scope_ref) {
                errors.push(BriefError::at(&path, BriefErrorKind::InvalidScopeRef));
            }
            check_prose(
                &format!("decisions[{index}].choice"),
                &decision.choice,
                &mut errors,
            );
            check_prose(
                &format!("decisions[{index}].reason"),
                &decision.reason,
                &mut errors,
            );
        }
        for (name, list) in self.lists() {
            check_entries(name, list.len(), &mut errors);
            for (index, text) in list.iter().enumerate() {
                check_prose(&format!("{name}[{index}]"), text, &mut errors);
            }
        }
        let bytes: usize = self.prose().map(str::len).sum();
        if bytes > MAX_PROSE_BYTES {
            errors.push(BriefError::at("", BriefErrorKind::OverByteBudget { bytes }));
        }
        let tokens = self.token_count();
        if tokens > MAX_BRIEF_TOKENS {
            errors.push(BriefError::at(
                "",
                BriefErrorKind::OverTokenBudget { tokens },
            ));
        }
        errors
    }

    /// Tokens of agent-authored text, `scope_ref`s included.
    pub fn token_count(&self) -> usize {
        self.prose().map(count_tokens).sum()
    }

    /// The canonical brief record and its ID. `schema`, `token_profile` and
    /// `requires` come from the tool; empty lists are omitted, so a brief has
    /// one encoding.
    ///
    /// # Panics
    ///
    /// If the brief is invalid: validate it (or build it with a `from_*`
    /// constructor) first.
    pub fn to_record(&self) -> (Vec<u8>, RecordId) {
        let errors = self.validate();
        assert!(
            errors.is_empty(),
            "to_record on an invalid brief: {errors:?}"
        );
        let text = |s: &str| Value::String(s.to_string());
        let mut members = vec![
            ("schema".to_string(), text(BRIEF_SCHEMA_NAME)),
            ("token_profile".to_string(), text(TOKEN_PROFILE)),
            ("intent".to_string(), text(&self.intent)),
        ];
        if !self.decisions.is_empty() {
            let decisions = self
                .decisions
                .iter()
                .map(|d| {
                    let members = vec![
                        ("scope_ref".to_string(), text(&d.scope_ref)),
                        ("choice".to_string(), text(&d.choice)),
                        ("reason".to_string(), text(&d.reason)),
                    ];
                    Value::Object(Object::new(members).expect("distinct keys"))
                })
                .collect();
            members.push(("decisions".to_string(), Value::Array(decisions)));
        }
        for (name, list) in self.lists() {
            if !list.is_empty() {
                members.push((
                    name.to_string(),
                    Value::Array(list.iter().map(|s| text(s)).collect()),
                ));
            }
        }
        let requires =
            BRIEF_SCHEMA.required_capabilities(members.iter().map(|(name, _)| name.as_str()));
        if !requires.is_empty() {
            members.push((
                "requires".to_string(),
                Value::Array(requires.into_iter().map(text).collect()),
            ));
        }
        let bytes =
            Value::Object(Object::new(members).expect("distinct keys")).to_canonical_bytes();
        let (_, id) = Self::from_record(&bytes).expect("a valid brief encodes to a valid record");
        (bytes, id)
    }

    fn lists(&self) -> [(&'static str, &Vec<String>); 3] {
        [
            (LIST_FIELDS[0], &self.preserves),
            (LIST_FIELDS[1], &self.assumptions),
            (LIST_FIELDS[2], &self.deferred),
        ]
    }

    fn prose(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.intent.as_str())
            .chain(
                self.decisions
                    .iter()
                    .flat_map(|d| [d.scope_ref.as_str(), d.choice.as_str(), d.reason.as_str()]),
            )
            .chain(
                self.lists()
                    .into_iter()
                    .flat_map(|(_, list)| list.iter().map(String::as_str)),
            )
    }
}

fn finish(brief: Brief, mut errors: Vec<BriefError>) -> Result<Brief, BriefErrors> {
    // Shape errors first; content rules only make sense on a well-formed brief.
    if errors.is_empty() {
        errors = brief.validate();
    }
    if errors.is_empty() {
        Ok(brief)
    } else {
        Err(BriefErrors(errors))
    }
}

/// Read the brief fields through `get`, recording shape errors. In a record
/// (`in_record`), an empty list is refused: it would be a second encoding of
/// an absent one.
fn read_fields<'a>(
    get: impl Fn(&str) -> Option<&'a Value>,
    in_record: bool,
    errors: &mut Vec<BriefError>,
) -> Brief {
    let mut brief = Brief::default();
    match get("intent") {
        Some(Value::String(text)) => brief.intent = text.clone(),
        Some(_) => errors.push(BriefError::at("intent", BriefErrorKind::WrongType)),
        None => errors.push(BriefError::at("intent", BriefErrorKind::MissingField)),
    }
    if let Some(items) = read_list("decisions", get("decisions"), in_record, errors) {
        for (index, item) in items.iter().enumerate() {
            let path = format!("decisions[{index}]");
            let Value::Object(object) = item else {
                errors.push(BriefError::at(&path, BriefErrorKind::WrongType));
                continue;
            };
            for (name, _) in object.iter() {
                if !DECISION_FIELDS.contains(&name) {
                    errors.push(BriefError::at(
                        &format!("{path}.{name}"),
                        BriefErrorKind::UnknownField,
                    ));
                }
            }
            let mut field = |name: &str| match object.get(name) {
                Some(Value::String(text)) => text.clone(),
                other => {
                    let kind = if other.is_some() {
                        BriefErrorKind::WrongType
                    } else {
                        BriefErrorKind::MissingField
                    };
                    errors.push(BriefError::at(&format!("{path}.{name}"), kind));
                    String::new()
                }
            };
            let decision = Decision {
                scope_ref: field("scope_ref"),
                choice: field("choice"),
                reason: field("reason"),
            };
            brief.decisions.push(decision);
        }
    }
    for name in LIST_FIELDS {
        let Some(items) = read_list(name, get(name), in_record, errors) else {
            continue;
        };
        let mut texts = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            match item {
                Value::String(text) => texts.push(text.clone()),
                _ => errors.push(BriefError::at(
                    &format!("{name}[{index}]"),
                    BriefErrorKind::WrongType,
                )),
            }
        }
        match name {
            "preserves" => brief.preserves = texts,
            "assumptions" => brief.assumptions = texts,
            _ => brief.deferred = texts,
        }
    }
    brief
}

fn read_list<'a>(
    name: &str,
    value: Option<&'a Value>,
    in_record: bool,
    errors: &mut Vec<BriefError>,
) -> Option<&'a Vec<Value>> {
    match value {
        None => None,
        Some(Value::Array(items)) if in_record && items.is_empty() => {
            errors.push(BriefError::at(name, BriefErrorKind::Empty));
            None
        }
        Some(Value::Array(items)) => Some(items),
        Some(_) => {
            errors.push(BriefError::at(name, BriefErrorKind::WrongType));
            None
        }
    }
}

fn check_entries(path: &str, count: usize, errors: &mut Vec<BriefError>) {
    if count > MAX_LIST_ENTRIES {
        errors.push(BriefError::at(
            path,
            BriefErrorKind::TooManyEntries { count },
        ));
    }
}

fn check_prose(path: &str, text: &str, errors: &mut Vec<BriefError>) {
    if text
        .chars()
        .all(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r'))
    {
        errors.push(BriefError::at(path, BriefErrorKind::Empty));
    } else if let Some(ch) = text.chars().find(|&ch| is_forbidden(ch)) {
        errors.push(BriefError::at(
            path,
            BriefErrorKind::ForbiddenCharacter { ch },
        ));
    }
}

/// Control characters other than line feed, and bidi controls, which can make
/// text display differently from its bytes.
fn is_forbidden(ch: char) -> bool {
    matches!(ch,
        '\0'..='\u{9}' | '\u{B}'..='\u{1F}' | '\u{7F}'..='\u{9F}'
        | '\u{61C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// `[a-z0-9][a-z0-9._/-]*`, at most [`MAX_SCOPE_REF_BYTES`].
fn is_scope_ref(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_SCOPE_REF_BYTES
        && matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9')
        && bytes
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'/' | b'-'))
}

/// One problem with a brief, at a path such as `decisions[1].reason` (empty
/// for the whole brief).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefError {
    pub path: String,
    pub kind: BriefErrorKind,
}

impl BriefError {
    fn at(path: &str, kind: BriefErrorKind) -> Self {
        Self {
            path: path.to_string(),
            kind,
        }
    }

    /// The stable refusal code shared with the golden vectors.
    pub fn code(&self) -> &'static str {
        match &self.kind {
            BriefErrorKind::Json(error) => error.code(),
            BriefErrorKind::Record(error) => error.code(),
            BriefErrorKind::NotAnObject => "not_an_object",
            BriefErrorKind::RecordTooLarge => "record_too_large",
            BriefErrorKind::UnsupportedProfile { .. } => "unsupported_profile",
            BriefErrorKind::UnknownField => "unknown_field",
            BriefErrorKind::MissingField => "missing_field",
            BriefErrorKind::WrongType => "wrong_type",
            BriefErrorKind::Empty => "empty",
            BriefErrorKind::ForbiddenCharacter { .. } => "forbidden_character",
            BriefErrorKind::InvalidScopeRef => "invalid_scope_ref",
            BriefErrorKind::TooManyEntries { .. } => "too_many_entries",
            BriefErrorKind::OverByteBudget { .. } => "over_byte_budget",
            BriefErrorKind::OverTokenBudget { .. } => "over_token_budget",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BriefErrorKind {
    #[error(transparent)]
    Json(canonical_json::CanonicalJsonError),
    #[error(transparent)]
    Record(super::record::RecordError),
    #[error("a brief must be a JSON object")]
    NotAnObject,
    #[error("brief record is larger than {MAX_BRIEF_RECORD_BYTES} bytes")]
    RecordTooLarge,
    #[error("unsupported token profile {profile:?}; this reader counts with {TOKEN_PROFILE}")]
    UnsupportedProfile { profile: String },
    #[error(
        "not a brief field; a decision file holds only intent, decisions, preserves, assumptions and deferred (schema, token_profile and requires are added by the tool)"
    )]
    UnknownField,
    #[error("missing")]
    MissingField,
    #[error("wrong type")]
    WrongType,
    #[error("empty; omit it instead")]
    Empty,
    #[error("contains {ch:?}: control and bidi characters are not allowed (line feed is)")]
    ForbiddenCharacter { ch: char },
    #[error(
        "not an identifier: use lowercase letters, digits and . _ / -, at most {MAX_SCOPE_REF_BYTES} bytes, put prose in choice or reason"
    )]
    InvalidScopeRef,
    #[error("{count} entries; at most {MAX_LIST_ENTRIES}")]
    TooManyEntries { count: usize },
    #[error("{bytes} bytes of text; at most {MAX_PROSE_BYTES}. Shorten it; it is never truncated")]
    OverByteBudget { bytes: usize },
    #[error(
        "{tokens} tokens ({TOKEN_PROFILE}); at most {MAX_BRIEF_TOKENS}. Shorten it by {}; it is never truncated. Keep only choices a reader could not see from the change",
        tokens - MAX_BRIEF_TOKENS
    )]
    OverTokenBudget { tokens: usize },
}

/// Every problem found in one brief, reported together so an agent can fix
/// them in one round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BriefErrors(pub Vec<BriefError>);

impl From<BriefError> for BriefErrors {
    fn from(error: BriefError) -> Self {
        Self(vec![error])
    }
}

impl std::fmt::Display for BriefErrors {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "the decision brief has {} problem(s):", self.0.len())?;
        for error in &self.0 {
            let path = if error.path.is_empty() {
                "brief"
            } else {
                &error.path
            };
            writeln!(f, "- {path}: {}", error.kind)?;
        }
        Ok(())
    }
}

impl std::error::Error for BriefErrors {}
