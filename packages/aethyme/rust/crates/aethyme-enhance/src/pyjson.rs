//! Order-preserving JSON with Python-compatible serialization.
//!
//! The enhance pipeline's byte-parity contract requires re-emitting user
//! JSON (overrides, settings.local.json) exactly as Python's
//! `json.loads` → `json.dumps` round trip does: insertion-ordered
//! objects, `ensure_ascii` escapes, Python `repr` float rendering, and
//! Python's default separators. `serde_json` cannot do this without
//! `preserve_order` (forbidden by the migration decisions), so this is
//! the sanctioned textual/ordered-emission implementation — the same
//! discipline as `aethyme_engine::graph_cli::pretty_json`, extended to
//! parsing.
//!
//! Fidelity notes (all mirroring CPython `json`):
//! - objects keep first-occurrence key positions; duplicate keys replace
//!   the value in place (dict semantics),
//! - integers round-trip as integers (i128; wider literals keep their
//!   digit string verbatim, which is what Python's arbitrary-precision
//!   int repr produces anyway),
//! - floats render via the `repr` shortest-round-trip algorithm with
//!   Python's fixed/exponent threshold (`< 1e16`, `>= 1e-4`),
//! - `NaN`/`Infinity`/`-Infinity` are accepted and re-emitted,
//! - non-ASCII characters escape to `\uXXXX` (surrogate pairs above the
//!   BMP). Lone surrogates in input are rejected (Python tolerates them;
//!   they cannot exist in a Rust `String` — accepted divergence).

use std::fmt::Write as _;

/// An order-preserving JSON value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i128),
    /// Integer literal too wide for i128, kept verbatim.
    BigInt(String),
    Float(f64),
    Str(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

impl Value {
    pub fn object() -> Value {
        Value::Object(Vec::new())
    }

    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }

    pub fn int(n: i128) -> Value {
        Value::Int(n)
    }

    pub fn is_object(&self) -> bool {
        matches!(self, Value::Object(_))
    }

    pub fn is_array(&self) -> bool {
        matches!(self, Value::Array(_))
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Vec<(String, Value)>> {
        match self {
            Value::Object(entries) => Some(entries),
            _ => None,
        }
    }

    /// Dict-style lookup. Returns `None` for missing keys and non-objects.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// `dict[key] = value`: replace in place when present, append otherwise.
    pub fn set(&mut self, key: &str, value: Value) {
        let Value::Object(entries) = self else {
            *self = Value::Object(vec![(key.to_string(), value)]);
            return;
        };
        if let Some(slot) = entries.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value;
        } else {
            entries.push((key.to_string(), value));
        }
    }

    /// `dict.pop(key, None)`-ish removal (order of the rest preserved).
    pub fn remove(&mut self, key: &str) -> Option<Value> {
        let Value::Object(entries) = self else {
            return None;
        };
        let idx = entries.iter().position(|(k, _)| k == key)?;
        Some(entries.remove(idx).1)
    }

    /// Python truthiness: `None`, `False`, `0`, `0.0`, `""`, `[]`, `{}`
    /// are falsy; everything else is truthy.
    pub fn truthy(&self) -> bool {
        match self {
            Value::Null => false,
            Value::Bool(b) => *b,
            Value::Int(n) => *n != 0,
            Value::BigInt(_) => true,
            Value::Float(f) => *f != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::Array(items) => !items.is_empty(),
            Value::Object(entries) => !entries.is_empty(),
        }
    }

    /// Python `str(value)` for f-string interpolation of scalars.
    /// Containers use a best-effort repr (only reachable through
    /// malformed overrides, where Python's own behavior is a crash or
    /// repr-ish text).
    pub fn py_str(&self) -> String {
        match self {
            Value::Null => "None".to_string(),
            Value::Bool(b) => py_bool(*b).to_string(),
            Value::Int(n) => n.to_string(),
            Value::BigInt(digits) => digits.clone(),
            Value::Float(f) => py_float_repr(*f),
            Value::Str(s) => s.clone(),
            Value::Array(_) | Value::Object(_) => dumps_compact(self),
        }
    }
}

/// Python `str(bool)`.
pub fn py_bool(b: bool) -> &'static str {
    if b {
        "True"
    } else {
        "False"
    }
}

// ── parsing ─────────────────────────────────────────────────────────────────

/// Parse strict JSON with CPython `json.loads` semantics (including the
/// non-standard `NaN`/`Infinity` constants Python accepts by default).
pub fn loads(text: &str) -> Result<Value, String> {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    skip_ws(bytes, &mut pos);
    let value = parse_value(text, bytes, &mut pos)?;
    skip_ws(bytes, &mut pos);
    if pos != bytes.len() {
        return Err(format!("Extra data: char {pos}"));
    }
    Ok(value)
}

fn skip_ws(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn parse_value(text: &str, bytes: &[u8], pos: &mut usize) -> Result<Value, String> {
    let Some(&c) = bytes.get(*pos) else {
        return Err("Expecting value".to_string());
    };
    match c {
        b'{' => parse_object(text, bytes, pos),
        b'[' => parse_array(text, bytes, pos),
        b'"' => parse_string(text, bytes, pos).map(Value::Str),
        b't' => parse_literal(bytes, pos, b"true", Value::Bool(true)),
        b'f' => parse_literal(bytes, pos, b"false", Value::Bool(false)),
        b'n' => parse_literal(bytes, pos, b"null", Value::Null),
        b'N' => parse_literal(bytes, pos, b"NaN", Value::Float(f64::NAN)),
        b'I' => parse_literal(bytes, pos, b"Infinity", Value::Float(f64::INFINITY)),
        b'-' if bytes.get(*pos + 1) == Some(&b'I') => {
            *pos += 1;
            parse_literal(bytes, pos, b"Infinity", Value::Float(f64::NEG_INFINITY))
        }
        b'-' | b'0'..=b'9' => parse_number(text, bytes, pos),
        _ => Err(format!("Expecting value: char {}", *pos)),
    }
}

fn parse_literal(
    bytes: &[u8],
    pos: &mut usize,
    literal: &[u8],
    value: Value,
) -> Result<Value, String> {
    if bytes.len() >= *pos + literal.len() && &bytes[*pos..*pos + literal.len()] == literal {
        *pos += literal.len();
        Ok(value)
    } else {
        Err(format!("Expecting value: char {}", *pos))
    }
}

fn parse_object(text: &str, bytes: &[u8], pos: &mut usize) -> Result<Value, String> {
    *pos += 1; // consume '{'
    let mut object = Value::object();
    skip_ws(bytes, pos);
    if bytes.get(*pos) == Some(&b'}') {
        *pos += 1;
        return Ok(object);
    }
    loop {
        skip_ws(bytes, pos);
        if bytes.get(*pos) != Some(&b'"') {
            return Err(format!(
                "Expecting property name enclosed in double quotes: char {}",
                *pos
            ));
        }
        let key = parse_string(text, bytes, pos)?;
        skip_ws(bytes, pos);
        if bytes.get(*pos) != Some(&b':') {
            return Err(format!("Expecting ':' delimiter: char {}", *pos));
        }
        *pos += 1;
        skip_ws(bytes, pos);
        let value = parse_value(text, bytes, pos)?;
        // dict semantics: duplicate keys replace in place.
        object.set(&key, value);
        skip_ws(bytes, pos);
        match bytes.get(*pos) {
            Some(&b',') => {
                *pos += 1;
            }
            Some(&b'}') => {
                *pos += 1;
                return Ok(object);
            }
            _ => return Err(format!("Expecting ',' delimiter: char {}", *pos)),
        }
    }
}

fn parse_array(text: &str, bytes: &[u8], pos: &mut usize) -> Result<Value, String> {
    *pos += 1; // consume '['
    let mut items = Vec::new();
    skip_ws(bytes, pos);
    if bytes.get(*pos) == Some(&b']') {
        *pos += 1;
        return Ok(Value::Array(items));
    }
    loop {
        skip_ws(bytes, pos);
        items.push(parse_value(text, bytes, pos)?);
        skip_ws(bytes, pos);
        match bytes.get(*pos) {
            Some(&b',') => {
                *pos += 1;
            }
            Some(&b']') => {
                *pos += 1;
                return Ok(Value::Array(items));
            }
            _ => return Err(format!("Expecting ',' delimiter: char {}", *pos)),
        }
    }
}

fn parse_string(text: &str, bytes: &[u8], pos: &mut usize) -> Result<String, String> {
    *pos += 1; // consume '"'
    let mut out = String::new();
    loop {
        let start = *pos;
        // Fast path: run of plain bytes.
        while let Some(&c) = bytes.get(*pos) {
            if c == b'"' || c == b'\\' || c < 0x20 {
                break;
            }
            *pos += 1;
        }
        out.push_str(&text[start..*pos]);
        match bytes.get(*pos) {
            None => return Err("Unterminated string".to_string()),
            Some(&b'"') => {
                *pos += 1;
                return Ok(out);
            }
            Some(&b'\\') => {
                *pos += 1;
                let Some(&esc) = bytes.get(*pos) else {
                    return Err("Unterminated string".to_string());
                };
                *pos += 1;
                match esc {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000C}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => {
                        let hi = parse_hex4(bytes, pos)?;
                        if (0xD800..0xDC00).contains(&hi) {
                            // Try surrogate pair.
                            if bytes.get(*pos) == Some(&b'\\') && bytes.get(*pos + 1) == Some(&b'u')
                            {
                                let save = *pos;
                                *pos += 2;
                                let lo = parse_hex4(bytes, pos)?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    let combined = 0x10000
                                        + ((hi - 0xD800) << 10) as u32
                                        + (lo - 0xDC00) as u32;
                                    out.push(
                                        char::from_u32(combined).ok_or("Invalid surrogate pair")?,
                                    );
                                    continue;
                                }
                                *pos = save;
                            }
                            return Err("Unpaired surrogate escape".to_string());
                        }
                        if (0xDC00..0xE000).contains(&hi) {
                            return Err("Unpaired surrogate escape".to_string());
                        }
                        out.push(char::from_u32(hi as u32).ok_or("Invalid \\u escape")?);
                    }
                    _ => return Err(format!("Invalid \\escape: char {}", *pos - 1)),
                }
            }
            Some(_) => {
                return Err(format!("Invalid control character: char {}", *pos));
            }
        }
    }
}

fn parse_hex4(bytes: &[u8], pos: &mut usize) -> Result<u16, String> {
    if bytes.len() < *pos + 4 {
        return Err("Invalid \\uXXXX escape".to_string());
    }
    let hex = std::str::from_utf8(&bytes[*pos..*pos + 4]).map_err(|_| "Invalid \\uXXXX escape")?;
    let value = u16::from_str_radix(hex, 16).map_err(|_| "Invalid \\uXXXX escape")?;
    *pos += 4;
    Ok(value)
}

fn parse_number(text: &str, bytes: &[u8], pos: &mut usize) -> Result<Value, String> {
    let start = *pos;
    if bytes.get(*pos) == Some(&b'-') {
        *pos += 1;
    }
    // Integer part: 0 | [1-9][0-9]*
    match bytes.get(*pos) {
        Some(&b'0') => {
            *pos += 1;
        }
        Some(&(b'1'..=b'9')) => {
            while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
                *pos += 1;
            }
        }
        _ => return Err(format!("Expecting value: char {start}")),
    }
    let mut is_float = false;
    if bytes.get(*pos) == Some(&b'.') {
        is_float = true;
        *pos += 1;
        if !matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            return Err(format!("Expecting value: char {start}"));
        }
        while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            *pos += 1;
        }
    }
    if matches!(bytes.get(*pos), Some(&b'e') | Some(&b'E')) {
        is_float = true;
        *pos += 1;
        if matches!(bytes.get(*pos), Some(&b'+') | Some(&b'-')) {
            *pos += 1;
        }
        if !matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            return Err(format!("Expecting value: char {start}"));
        }
        while matches!(bytes.get(*pos), Some(b'0'..=b'9')) {
            *pos += 1;
        }
    }
    let literal = &text[start..*pos];
    if is_float {
        literal
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|e| e.to_string())
    } else {
        match literal.parse::<i128>() {
            Ok(n) => Ok(Value::Int(n)),
            Err(_) => Ok(Value::BigInt(literal.to_string())),
        }
    }
}

// ── serialization ───────────────────────────────────────────────────────────

/// `json.dumps(value)` — Python's default separators `", "` / `": "`.
pub fn dumps_compact(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, None, 0);
    out
}

/// `json.dumps(value, indent=2)`.
pub fn dumps_indent2(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, Some(2), 0);
    out
}

fn write_value(out: &mut String, value: &Value, indent: Option<usize>, level: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Int(n) => {
            let _ = write!(out, "{n}");
        }
        Value::BigInt(digits) => out.push_str(digits),
        Value::Float(f) => out.push_str(&py_json_float(*f)),
        Value::Str(s) => write_json_string(out, s),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    push_item_separator(out, indent);
                }
                push_newline_indent(out, indent, level + 1);
                write_value(out, item, indent, level + 1);
            }
            push_newline_indent(out, indent, level);
            out.push(']');
        }
        Value::Object(entries) => {
            if entries.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (key, item)) in entries.iter().enumerate() {
                if i > 0 {
                    push_item_separator(out, indent);
                }
                push_newline_indent(out, indent, level + 1);
                write_json_string(out, key);
                out.push_str(": ");
                write_value(out, item, indent, level + 1);
            }
            push_newline_indent(out, indent, level);
            out.push('}');
        }
    }
}

fn push_item_separator(out: &mut String, indent: Option<usize>) {
    // With indent, Python uses "," + newline; without, ", ".
    if indent.is_some() {
        out.push(',');
    } else {
        out.push_str(", ");
    }
}

fn push_newline_indent(out: &mut String, indent: Option<usize>, level: usize) {
    if let Some(width) = indent {
        out.push('\n');
        for _ in 0..(width * level) {
            out.push(' ');
        }
    }
}

/// Python `json` string escaping with `ensure_ascii=True`.
fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{0008}' => out.push_str("\\b"),
            '\u{000C}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c if (c as u32) < 0x7F => out.push(c),
            c => {
                let code = c as u32;
                if code > 0xFFFF {
                    // Surrogate pair, as Python emits for astral chars.
                    let v = code - 0x10000;
                    let _ = write!(
                        out,
                        "\\u{:04x}\\u{:04x}",
                        0xD800 + (v >> 10),
                        0xDC00 + (v & 0x3FF)
                    );
                } else {
                    let _ = write!(out, "\\u{code:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// Float rendering inside JSON: Python emits `NaN`/`Infinity` tokens.
fn py_json_float(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    py_float_repr(f)
}

/// CPython `repr(float)`: shortest round-trip digits, fixed notation for
/// decimal exponents in `[-4, 16)`, exponent notation (2+ digit signed
/// exponent) outside.
pub fn py_float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf" } else { "-inf" }.to_string();
    }
    if f == 0.0 {
        return if f.is_sign_negative() { "-0.0" } else { "0.0" }.to_string();
    }
    let negative = f < 0.0;
    // Rust's LowerExp is shortest-round-trip: "d[.ddd]e[-]X".
    let formatted = format!("{:e}", f.abs());
    let (mantissa, exp_str) = formatted.split_once('e').expect("LowerExp always has 'e'");
    let exp: i32 = exp_str.parse().expect("exponent is an integer");
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let body = if (-4..16).contains(&exp) {
        format_fixed(&digits, exp)
    } else {
        format_exponent(&digits, exp)
    };
    if negative {
        format!("-{body}")
    } else {
        body
    }
}

fn format_fixed(digits: &str, exp: i32) -> String {
    if exp >= 0 {
        let int_len = (exp as usize) + 1;
        if digits.len() <= int_len {
            let mut int_part = digits.to_string();
            int_part.push_str(&"0".repeat(int_len - digits.len()));
            format!("{int_part}.0")
        } else {
            format!("{}.{}", &digits[..int_len], &digits[int_len..])
        }
    } else {
        format!("0.{}{}", "0".repeat((-exp as usize) - 1), digits)
    }
}

fn format_exponent(digits: &str, exp: i32) -> String {
    let mantissa = if digits.len() == 1 {
        digits.to_string()
    } else {
        format!("{}.{}", &digits[..1], &digits[1..])
    };
    if exp < 0 {
        format!("{mantissa}e-{:02}", -exp)
    } else {
        format!("{mantissa}e+{exp:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_reemits_ordered_object() {
        let value = loads(r#"{"b": 1, "a": [true, null], "b": 2}"#).unwrap();
        // Duplicate key keeps first position, last value (dict semantics).
        assert_eq!(dumps_compact(&value), r#"{"b": 2, "a": [true, null]}"#);
    }

    #[test]
    fn indent2_matches_python_shapes() {
        let value = loads(r#"{"a": {"b": []}, "c": [1, {"d": "x"}]}"#).unwrap();
        assert_eq!(
            dumps_indent2(&value),
            "{\n  \"a\": {\n    \"b\": []\n  },\n  \"c\": [\n    1,\n    {\n      \"d\": \"x\"\n    }\n  ]\n}"
        );
    }

    #[test]
    fn empty_containers_stay_compact() {
        assert_eq!(dumps_indent2(&loads("{}").unwrap()), "{}");
        assert_eq!(dumps_indent2(&loads("[]").unwrap()), "[]");
    }

    #[test]
    fn strings_escape_like_ensure_ascii() {
        let value = Value::str("é\n\t\"x\"\u{1F600}\u{001B}");
        assert_eq!(
            dumps_compact(&value),
            "\"\\u00e9\\n\\t\\\"x\\\"\\ud83d\\ude00\\u001b\""
        );
    }

    #[test]
    fn float_repr_matches_python() {
        assert_eq!(py_float_repr(100.0), "100.0");
        assert_eq!(py_float_repr(0.5), "0.5");
        assert_eq!(py_float_repr(1e16), "1e+16");
        assert_eq!(py_float_repr(1e-4), "0.0001");
        assert_eq!(py_float_repr(1e-5), "1e-05");
        assert_eq!(py_float_repr(-1753689600.123456), "-1753689600.123456");
        assert_eq!(
            py_float_repr(1.2345678901234567e19),
            "1.2345678901234567e+19"
        );
        assert_eq!(py_float_repr(0.0), "0.0");
    }

    #[test]
    fn numbers_round_trip() {
        let value = loads(r#"[1, -0, 1.50, 1e2, 9999999999999999999999999]"#).unwrap();
        assert_eq!(
            dumps_compact(&value),
            "[1, 0, 1.5, 100.0, 9999999999999999999999999]"
        );
    }

    #[test]
    fn python_constants_accepted() {
        let value = loads("[NaN, Infinity, -Infinity]").unwrap();
        assert_eq!(dumps_compact(&value), "[NaN, Infinity, -Infinity]");
    }

    #[test]
    fn rejects_trailing_data_and_bad_syntax() {
        assert!(loads("{} x").is_err());
        assert!(loads("{'a': 1}").is_err());
        assert!(loads("[1,]").is_err());
        assert!(loads("01").is_err());
    }
}
