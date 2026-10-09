//! The canonical JSON profile every v0 record is encoded in (plan §5.3).
//!
//! Records are exchanged as JSON and identified by a digest of one canonical
//! byte form, so two implementations must agree on which inputs are valid and
//! on the exact bytes each valid input canonicalizes to. Any JSON parser
//! accepts more than that: duplicate keys (most keep the last), `1.0` vs `1`,
//! integers beyond what JavaScript can represent, lone UTF-16 surrogates. This
//! module is a strict parser for a small profile instead.
//!
//! ## Profile (v0)
//!
//! Input is JSON (RFC 8259) restricted as follows. Each refusal has a stable
//! code ([`CanonicalJsonError::code`]) shared with the golden vectors.
//!
//! - UTF-8 without a byte order mark; at most [`MAX_DOCUMENT_BYTES`].
//! - Containers nest at most [`MAX_DEPTH`] deep and hold at most
//!   [`MAX_CONTAINER_ENTRIES`] entries. Strings, keys included, decode to at
//!   most [`MAX_STRING_BYTES`].
//! - **No `null`.** An optional value is absent; one meaning has one encoding.
//! - **Integers only**, in `±(2^53 − 1)`, so that a JavaScript consumer reads
//!   them without precision loss. No fraction, exponent or `-0`. A larger
//!   quantity or a counter is a decimal string, chosen by the record schema.
//! - Strings are Unicode scalar values: lone surrogate escapes and Unicode
//!   noncharacters are refused (I-JSON, RFC 7493).
//! - **Duplicate keys are refused**, compared after unescaping (`"a"` and
//!   `"a"` are the same key).
//!
//! ## Canonical form
//!
//! The JSON Canonicalization Scheme (RFC 8785) restricted to this profile: no
//! insignificant whitespace; object members sorted by the UTF-16 code units of
//! their keys; strings escape only `"`, `\`, and control characters (`\b`,
//! `\f`, `\n`, `\r`, `\t`, otherwise `\u00xx` in lowercase hex), everything
//! else is literal UTF-8; integers in plain decimal. Because the profile has no
//! floating-point numbers, the hardest part of RFC 8785 (ECMAScript number
//! formatting) never arises.
//!
//! Canonicalization never alters content: no Unicode normalization, no
//! trimming, no case folding. Two inputs canonicalize to the same bytes if and
//! only if they are the same JSON value.

/// Largest accepted document, in bytes.
pub const MAX_DOCUMENT_BYTES: usize = 1 << 20;

/// Deepest accepted container nesting; a top-level object is depth 1.
pub const MAX_DEPTH: usize = 32;

/// Largest accepted string or key, in decoded UTF-8 bytes.
pub const MAX_STRING_BYTES: usize = 64 * 1024;

/// Most members of one object or items of one array.
pub const MAX_CONTAINER_ENTRIES: usize = 4096;

/// Largest integer magnitude a JavaScript number holds exactly: `2^53 − 1`.
pub const MAX_SAFE_INTEGER: i64 = (1 << 53) - 1;

/// A JSON value in the profile. There is no `Null` and no floating point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Bool(bool),
    Integer(i64),
    String(String),
    Array(Vec<Value>),
    Object(Object),
}

/// Object members, unique and kept in canonical (UTF-16 key) order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Object {
    members: Vec<(String, Value)>,
}

impl Object {
    /// Build an object, refusing duplicate keys. Input order does not matter.
    pub fn new(mut members: Vec<(String, Value)>) -> Result<Self, CanonicalJsonError> {
        members.sort_by(|a, b| utf16_order(&a.0, &b.0));
        if let Some(pair) = members.windows(2).find(|pair| pair[0].0 == pair[1].0) {
            return Err(CanonicalJsonError::DuplicateKey {
                key: pair[0].0.clone(),
            });
        }
        Ok(Self { members })
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.members
            .binary_search_by(|(k, _)| utf16_order(k, key))
            .ok()
            .map(|index| &self.members[index].1)
    }

    /// Members in canonical order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.members.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }
}

/// RFC 8785 member order: compare keys as sequences of UTF-16 code units. This
/// differs from code point order for characters outside the Basic
/// Multilingual Plane (`U+1F600` sorts before `U+FB01`).
pub fn utf16_order(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

impl Value {
    /// The canonical bytes of this value. A value built in code rather than by
    /// [`parse`] must still satisfy the profile; parse the result to check.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write_canonical(&mut out);
        out
    }

    fn write_canonical(&self, out: &mut Vec<u8>) {
        match self {
            Self::Bool(true) => out.extend_from_slice(b"true"),
            Self::Bool(false) => out.extend_from_slice(b"false"),
            Self::Integer(n) => out.extend_from_slice(n.to_string().as_bytes()),
            Self::String(s) => write_string(s, out),
            Self::Array(items) => {
                out.push(b'[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    item.write_canonical(out);
                }
                out.push(b']');
            }
            Self::Object(object) => {
                out.push(b'{');
                for (index, (key, value)) in object.iter().enumerate() {
                    if index > 0 {
                        out.push(b',');
                    }
                    write_string(key, out);
                    out.push(b':');
                    value.write_canonical(out);
                }
                out.push(b'}');
            }
        }
    }
}

fn write_string(s: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in s.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            c if (c as u32) < 0x20 => {
                out.extend_from_slice(format!("\\u{:04x}", c as u32).as_bytes());
            }
            c => {
                let mut buffer = [0; 4];
                out.extend_from_slice(c.encode_utf8(&mut buffer).as_bytes());
            }
        }
    }
    out.push(b'"');
}

/// Why a document is outside the profile. `offset` is a byte offset into the
/// input, for diagnostics only; it is not part of the shared contract.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CanonicalJsonError {
    #[error("document is larger than {MAX_DOCUMENT_BYTES} bytes")]
    TooLarge,
    #[error("document starts with a UTF-8 byte order mark")]
    ByteOrderMark,
    #[error("document is not valid UTF-8")]
    InvalidUtf8,
    #[error("invalid JSON at byte {offset}: expected {expected}")]
    Syntax {
        offset: usize,
        expected: &'static str,
    },
    #[error("unexpected data after the JSON value at byte {offset}")]
    TrailingData { offset: usize },
    #[error("containers nest deeper than {MAX_DEPTH} at byte {offset}")]
    TooDeep { offset: usize },
    #[error("container holds more than {MAX_CONTAINER_ENTRIES} entries at byte {offset}")]
    TooManyEntries { offset: usize },
    #[error("duplicate object key {key:?}")]
    DuplicateKey { key: String },
    #[error("null at byte {offset}: omit an absent value instead")]
    NullValue { offset: usize },
    #[error(
        "non-integer number at byte {offset}: use an integer, or a decimal string for non-integer quantities"
    )]
    NonIntegerNumber { offset: usize },
    #[error("integer at byte {offset} is outside ±(2^53 − 1): use a decimal string")]
    IntegerOutOfRange { offset: usize },
    #[error("-0 at byte {offset}: write 0")]
    NegativeZero { offset: usize },
    #[error("lone UTF-16 surrogate escape at byte {offset}")]
    LoneSurrogate { offset: usize },
    #[error("Unicode noncharacter at byte {offset}")]
    Noncharacter { offset: usize },
    #[error("string at byte {offset} is longer than {MAX_STRING_BYTES} bytes")]
    StringTooLong { offset: usize },
}

impl CanonicalJsonError {
    /// The stable refusal code shared with other implementations and the
    /// golden vectors.
    pub fn code(&self) -> &'static str {
        match self {
            Self::TooLarge => "too_large",
            Self::ByteOrderMark => "byte_order_mark",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::Syntax { .. } => "syntax",
            Self::TrailingData { .. } => "trailing_data",
            Self::TooDeep { .. } => "too_deep",
            Self::TooManyEntries { .. } => "too_many_entries",
            Self::DuplicateKey { .. } => "duplicate_key",
            Self::NullValue { .. } => "null_value",
            Self::NonIntegerNumber { .. } => "non_integer_number",
            Self::IntegerOutOfRange { .. } => "integer_out_of_range",
            Self::NegativeZero { .. } => "negative_zero",
            Self::LoneSurrogate { .. } => "lone_surrogate",
            Self::Noncharacter { .. } => "noncharacter",
            Self::StringTooLong { .. } => "string_too_long",
        }
    }
}

/// Parse a document in the profile. The first violation found is reported.
pub fn parse(input: &[u8]) -> Result<Value, CanonicalJsonError> {
    if input.len() > MAX_DOCUMENT_BYTES {
        return Err(CanonicalJsonError::TooLarge);
    }
    if input.starts_with(b"\xEF\xBB\xBF") {
        return Err(CanonicalJsonError::ByteOrderMark);
    }
    let text = std::str::from_utf8(input).map_err(|_| CanonicalJsonError::InvalidUtf8)?;
    let mut parser = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_whitespace();
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.pos != parser.bytes.len() {
        return Err(CanonicalJsonError::TrailingData { offset: parser.pos });
    }
    Ok(value)
}

/// Parse and return the canonical bytes in one step.
pub fn canonicalize(input: &[u8]) -> Result<Vec<u8>, CanonicalJsonError> {
    parse(input).map(|value| value.to_canonical_bytes())
}

struct Parser<'a> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn syntax(&self, expected: &'static str) -> CanonicalJsonError {
        CanonicalJsonError::Syntax {
            offset: self.pos,
            expected,
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, byte: u8, expected: &'static str) -> Result<(), CanonicalJsonError> {
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.syntax(expected))
        }
    }

    fn literal(&mut self, word: &[u8]) -> bool {
        if self.bytes[self.pos..].starts_with(word) {
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    /// `depth` is the nesting depth of the container this value sits in.
    fn value(&mut self, depth: usize) -> Result<Value, CanonicalJsonError> {
        match self.peek() {
            Some(b'{') => self.object(depth + 1),
            Some(b'[') => self.array(depth + 1),
            Some(b'"') => self.string().map(Value::String),
            Some(b'-' | b'0'..=b'9') => self.integer(),
            Some(b't') if self.literal(b"true") => Ok(Value::Bool(true)),
            Some(b'f') if self.literal(b"false") => Ok(Value::Bool(false)),
            Some(b'n') if self.bytes[self.pos..].starts_with(b"null") => {
                Err(CanonicalJsonError::NullValue { offset: self.pos })
            }
            _ => Err(self.syntax("a value")),
        }
    }

    fn enter(&self, depth: usize) -> Result<(), CanonicalJsonError> {
        if depth > MAX_DEPTH {
            Err(CanonicalJsonError::TooDeep { offset: self.pos })
        } else {
            Ok(())
        }
    }

    fn object(&mut self, depth: usize) -> Result<Value, CanonicalJsonError> {
        self.enter(depth)?;
        self.pos += 1;
        let mut members = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(Value::Object(Object::default()));
        }
        loop {
            if self.peek() != Some(b'"') {
                return Err(self.syntax("a string key"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':', "':'")?;
            self.skip_whitespace();
            let value = self.value(depth)?;
            members.push((key, value));
            if members.len() > MAX_CONTAINER_ENTRIES {
                return Err(CanonicalJsonError::TooManyEntries { offset: self.pos });
            }
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_whitespace();
                }
                Some(b'}') => {
                    self.pos += 1;
                    return Object::new(members).map(Value::Object);
                }
                _ => return Err(self.syntax("',' or '}'")),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Value, CanonicalJsonError> {
        self.enter(depth)?;
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(Value::Array(items));
        }
        loop {
            items.push(self.value(depth)?);
            if items.len() > MAX_CONTAINER_ENTRIES {
                return Err(CanonicalJsonError::TooManyEntries { offset: self.pos });
            }
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_whitespace();
                }
                Some(b']') => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                _ => return Err(self.syntax("',' or ']'")),
            }
        }
    }

    fn integer(&mut self) -> Result<Value, CanonicalJsonError> {
        let start = self.pos;
        let negative = self.peek() == Some(b'-');
        if negative {
            self.pos += 1;
        }
        let digits_start = self.pos;
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.syntax("a digit")),
        }
        if matches!(self.peek(), Some(b'.' | b'e' | b'E')) {
            return Err(CanonicalJsonError::NonIntegerNumber { offset: start });
        }
        // Too many digits for an i64 fails to parse, which is also out of range.
        let magnitude: i64 = match self.text[digits_start..self.pos].parse() {
            Ok(n) if n <= MAX_SAFE_INTEGER => n,
            _ => return Err(CanonicalJsonError::IntegerOutOfRange { offset: start }),
        };
        if negative && magnitude == 0 {
            return Err(CanonicalJsonError::NegativeZero { offset: start });
        }
        Ok(Value::Integer(if negative {
            -magnitude
        } else {
            magnitude
        }))
    }

    fn string(&mut self) -> Result<String, CanonicalJsonError> {
        let start = self.pos;
        self.pos += 1;
        let mut out = String::new();
        loop {
            if out.len() > MAX_STRING_BYTES {
                return Err(CanonicalJsonError::StringTooLong { offset: start });
            }
            let Some(byte) = self.peek() else {
                return Err(self.syntax("'\"'"));
            };
            match byte {
                b'"' => {
                    if out.len() > MAX_STRING_BYTES {
                        return Err(CanonicalJsonError::StringTooLong { offset: start });
                    }
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    let escape_at = self.pos;
                    self.pos += 1;
                    let ch = match self.peek() {
                        Some(b'"') => '"',
                        Some(b'\\') => '\\',
                        Some(b'/') => '/',
                        Some(b'b') => '\u{8}',
                        Some(b'f') => '\u{c}',
                        Some(b'n') => '\n',
                        Some(b'r') => '\r',
                        Some(b't') => '\t',
                        Some(b'u') => {
                            self.pos += 1;
                            let ch = self.unicode_escape(escape_at)?;
                            self.push_char(&mut out, ch, escape_at)?;
                            continue;
                        }
                        _ => return Err(self.syntax("an escape character")),
                    };
                    self.pos += 1;
                    out.push(ch);
                }
                0x00..=0x1F => return Err(self.syntax("an escaped control character")),
                _ => {
                    // The document is valid UTF-8, so the next char is whole.
                    let ch = self.text[self.pos..].chars().next().expect("non-empty");
                    let at = self.pos;
                    self.pos += ch.len_utf8();
                    self.push_char(&mut out, ch, at)?;
                }
            }
        }
    }

    fn push_char(&self, out: &mut String, ch: char, at: usize) -> Result<(), CanonicalJsonError> {
        let code = ch as u32;
        if (0xFDD0..=0xFDEF).contains(&code) || code & 0xFFFE == 0xFFFE {
            return Err(CanonicalJsonError::Noncharacter { offset: at });
        }
        out.push(ch);
        Ok(())
    }

    /// Decode the hex after `\u`, combining a surrogate pair when present.
    fn unicode_escape(&mut self, escape_at: usize) -> Result<char, CanonicalJsonError> {
        let lone = CanonicalJsonError::LoneSurrogate { offset: escape_at };
        let first = self.hex4()?;
        let code = match first {
            0xD800..=0xDBFF => {
                if !self.bytes[self.pos..].starts_with(b"\\u") {
                    return Err(lone);
                }
                self.pos += 2;
                let second = self.hex4()?;
                if !(0xDC00..=0xDFFF).contains(&second) {
                    return Err(lone);
                }
                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
            }
            0xDC00..=0xDFFF => return Err(lone),
            scalar => scalar,
        };
        Ok(char::from_u32(code).expect("surrogates handled above"))
    }

    fn hex4(&mut self) -> Result<u32, CanonicalJsonError> {
        let digits = self
            .bytes
            .get(self.pos..self.pos + 4)
            .filter(|d| d.iter().all(u8::is_ascii_hexdigit))
            .ok_or_else(|| self.syntax("four hex digits"))?;
        self.pos += 4;
        let digits = std::str::from_utf8(digits).expect("ASCII hex");
        Ok(u32::from_str_radix(digits, 16).expect("validated hex"))
    }
}
