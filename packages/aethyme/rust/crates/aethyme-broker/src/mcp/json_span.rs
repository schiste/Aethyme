//! A JSON reader that keeps byte spans, for editing a file in place.
//!
//! [`super::repair`] rewrites one server entry of an agent's JSON
//! configuration and must leave every other byte (key order, indentation,
//! spacing, unknown keys, other entries) exactly as it was. Re-serialising a
//! parsed value cannot promise that, so this reader records where each value
//! starts and ends, and the caller replaces only those ranges.
//!
//! It accepts exactly RFC 8259 JSON (no comments, no trailing commas) and
//! decodes strings, including `\u` escapes and surrogate pairs.

/// A parsed value and its byte range `start..end` in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub start: usize,
    pub end: usize,
    pub kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    Object(Vec<Member>),
    Array(Vec<Node>),
    String(String),
    /// A number, `true`, `false` or `null`.
    Scalar,
}

/// One object member: the decoded key, where the key starts, and the value.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub key: String,
    pub key_start: usize,
    pub value: Node,
}

impl Node {
    /// The member called `key` of an object (the last one, as JSON readers do).
    pub fn get(&self, key: &str) -> Option<&Member> {
        match &self.kind {
            Kind::Object(members) => members.iter().rev().find(|member| member.key == key),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match &self.kind {
            Kind::String(text) => Some(text),
            _ => None,
        }
    }

    pub fn members(&self) -> &[Member] {
        match &self.kind {
            Kind::Object(members) => members,
            _ => &[],
        }
    }

    /// Every element as a string, or `None` when one is not a string.
    pub fn string_array(&self) -> Option<Vec<String>> {
        match &self.kind {
            Kind::Array(items) => items
                .iter()
                .map(|item| item.as_str().map(str::to_string))
                .collect(),
            _ => None,
        }
    }
}

const MAX_DEPTH: usize = 256;

/// Parse a whole document. The error names a byte offset, never content.
pub fn parse(text: &str) -> Result<Node, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        text,
        position: 0,
    };
    parser.skip_whitespace();
    let node = parser.value(0)?;
    parser.skip_whitespace();
    if parser.position != parser.bytes.len() {
        return Err(format!("unexpected data at byte {}", parser.position));
    }
    Ok(node)
}

struct Parser<'a> {
    bytes: &'a [u8],
    text: &'a str,
    position: usize,
}

impl Parser<'_> {
    fn error<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.position))
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    fn expect(&mut self, byte: u8) -> Result<(), String> {
        if self.peek() == Some(byte) {
            self.position += 1;
            Ok(())
        } else {
            self.error(&format!("expected '{}'", byte as char))
        }
    }

    fn value(&mut self, depth: usize) -> Result<Node, String> {
        if depth > MAX_DEPTH {
            return self.error("nesting too deep");
        }
        let start = self.position;
        let kind = match self.peek() {
            Some(b'{') => self.object(depth)?,
            Some(b'[') => self.array(depth)?,
            Some(b'"') => Kind::String(self.string()?),
            Some(b't') => self.literal("true")?,
            Some(b'f') => self.literal("false")?,
            Some(b'n') => self.literal("null")?,
            Some(b'-' | b'0'..=b'9') => self.number()?,
            _ => return self.error("expected a value"),
        };
        Ok(Node {
            start,
            end: self.position,
            kind,
        })
    }

    fn literal(&mut self, word: &str) -> Result<Kind, String> {
        if self.text[self.position..].starts_with(word) {
            self.position += word.len();
            Ok(Kind::Scalar)
        } else {
            self.error("invalid literal")
        }
    }

    fn digits(&mut self) -> usize {
        let start = self.position;
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
        self.position - start
    }

    fn number(&mut self) -> Result<Kind, String> {
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => {
                self.digits();
            }
            _ => return self.error("invalid number"),
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            if self.digits() == 0 {
                return self.error("invalid number");
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if self.digits() == 0 {
                return self.error("invalid number");
            }
        }
        Ok(Kind::Scalar)
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(digits) = self.text.get(self.position..self.position + 4) else {
            return self.error("truncated escape");
        };
        let value = u32::from_str_radix(digits, 16).or_else(|_| self.error("invalid escape"))?;
        self.position += 4;
        Ok(value)
    }

    fn string(&mut self) -> Result<String, String> {
        self.expect(b'"')?;
        let mut out = String::new();
        loop {
            let run_start = self.position;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.position += 1;
            }
            out.push_str(&self.text[run_start..self.position]);
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.position += 1;
                    let escape = self.peek();
                    self.position += 1;
                    match escape {
                        Some(b'"') => out.push('"'),
                        Some(b'\\') => out.push('\\'),
                        Some(b'/') => out.push('/'),
                        Some(b'b') => out.push('\u{8}'),
                        Some(b'f') => out.push('\u{c}'),
                        Some(b'n') => out.push('\n'),
                        Some(b'r') => out.push('\r'),
                        Some(b't') => out.push('\t'),
                        Some(b'u') => {
                            let first = self.hex4()?;
                            let code = if (0xD800..0xDC00).contains(&first) {
                                if !self.text[self.position..].starts_with("\\u") {
                                    return self.error("unpaired surrogate");
                                }
                                self.position += 2;
                                let second = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&second) {
                                    return self.error("unpaired surrogate");
                                }
                                0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00)
                            } else {
                                first
                            };
                            match char::from_u32(code) {
                                Some(c) => out.push(c),
                                None => return self.error("invalid escape"),
                            }
                        }
                        _ => return self.error("invalid escape"),
                    }
                }
                _ => return self.error("unterminated string"),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Kind, String> {
        self.expect(b'{')?;
        let mut members = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(Kind::Object(members));
        }
        loop {
            self.skip_whitespace();
            let key_start = self.position;
            let key = self.string()?;
            self.skip_whitespace();
            self.expect(b':')?;
            self.skip_whitespace();
            let value = self.value(depth + 1)?;
            members.push(Member {
                key,
                key_start,
                value,
            });
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Kind::Object(members));
                }
                _ => return self.error("expected ',' or '}'"),
            }
        }
    }

    fn array(&mut self, depth: usize) -> Result<Kind, String> {
        self.expect(b'[')?;
        let mut items = Vec::new();
        self.skip_whitespace();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(Kind::Array(items));
        }
        loop {
            self.skip_whitespace();
            items.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => self.position += 1,
                Some(b']') => {
                    self.position += 1;
                    return Ok(Kind::Array(items));
                }
                _ => return self.error("expected ',' or ']'"),
            }
        }
    }
}

/// One replacement of `start..end` by `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// Apply non-overlapping edits.
pub fn apply(source: &str, mut edits: Vec<Edit>) -> Result<String, String> {
    edits.sort_by_key(|edit| edit.start);
    for pair in edits.windows(2) {
        if pair[0].end > pair[1].start {
            return Err("overlapping edits".into());
        }
    }
    let mut out = String::with_capacity(source.len() + 256);
    let mut cursor = 0;
    for edit in &edits {
        if edit.start < cursor || edit.end > source.len() || edit.start > edit.end {
            return Err("edit out of range".into());
        }
        out.push_str(&source[cursor..edit.start]);
        out.push_str(&edit.text);
        cursor = edit.end;
    }
    out.push_str(&source[cursor..]);
    Ok(out)
}

/// A JSON string literal for `text`.
pub fn quote(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// The whitespace that starts the line holding byte `at`.
pub fn line_indent(source: &str, at: usize) -> &str {
    let line_start = source[..at].rfind('\n').map_or(0, |index| index + 1);
    let line = &source[line_start..];
    let width = line
        .bytes()
        .take_while(|byte| *byte == b' ' || *byte == b'\t')
        .count();
    &line[..width]
}

/// The file's indentation unit: the leading whitespace of its first
/// indented line, else two spaces.
pub fn indent_unit(source: &str) -> &str {
    for line in source.lines() {
        let width = line
            .bytes()
            .take_while(|byte| *byte == b' ' || *byte == b'\t')
            .count();
        if width > 0 && width < line.len() {
            return &line[..width];
        }
    }
    "  "
}

/// `items` as a JSON array: on one line, or one item per line at
/// `indent + unit` with the closing bracket at `indent`.
pub fn render_string_array(items: &[String], multiline: bool, indent: &str, unit: &str) -> String {
    if items.is_empty() {
        return "[]".into();
    }
    let quoted: Vec<String> = items.iter().map(|item| quote(item)).collect();
    if !multiline {
        return format!("[{}]", quoted.join(", "));
    }
    let inner = format!("{indent}{unit}");
    let body: Vec<String> = quoted.iter().map(|item| format!("{inner}{item}")).collect();
    format!("[\n{}\n{indent}]", body.join(",\n"))
}
