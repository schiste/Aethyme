//! Strict YAML 1.2 subset to `toml::Value`.
//!
//! Parsing happens in two passes over one text. The typed pass (noyalib's
//! loader, YAML 1.2 core schema, duplicate keys as errors, merge keys left as
//! ordinary keys so they can be refused, alias expansion capped) produces the
//! value. The lossless CST pass supplies what the typed value cannot: key and
//! value spans for diagnostics, the tags and non-string keys the typed value
//! has already resolved away, and the source text of an integer that the
//! loader widened to a float.

use std::path::Path;

use noyalib::cst::{GreenChild, GreenNode, SyntaxKind};
use noyalib::{DuplicateKeyPolicy, MergeKeyPolicy, ParserConfig};

use crate::locate::{LineIndex, Location};
use crate::{
    ConfigSourceError, ConfigSpans, YAML_ALIAS_EXPANSION_CAP, YAML_NODE_CAP, join_index_path,
    join_key_path,
};

const MERGE_KEY_MESSAGE: &str =
    "YAML merge keys (`<<`) are not accepted; repeat the keys or alias the whole value";

/// Largest YAML configuration file accepted, in bytes.
const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

fn parser_config() -> ParserConfig {
    ParserConfig::new()
        .duplicate_key_policy(DuplicateKeyPolicy::Error)
        .merge_key_policy(MergeKeyPolicy::AsOrdinary)
        .max_alias_expansions(YAML_ALIAS_EXPANSION_CAP)
        .max_nodes(YAML_NODE_CAP)
        .max_depth(64)
        .max_document_length(MAX_DOCUMENT_BYTES)
        .max_total_scalar_bytes(MAX_DOCUMENT_BYTES * 4)
}

struct Context<'a> {
    path: &'a Path,
    text: &'a str,
    index: LineIndex<'a>,
    document: noyalib::cst::Document,
    spans: ConfigSpans,
}

impl Context<'_> {
    fn error(
        &self,
        location: Option<Location>,
        key_path: &str,
        code: &'static str,
        message: impl Into<String>,
    ) -> ConfigSourceError {
        ConfigSourceError::Invalid {
            path: self.path.to_path_buf(),
            location,
            key_path: (!key_path.is_empty()).then(|| key_path.to_string()),
            code,
            message: message.into(),
        }
    }

    /// Location of the node at `query`: its key when it is a mapping entry,
    /// otherwise its value.
    fn locate(&self, query: &str) -> Option<Location> {
        self.document
            .key_span(query)
            .or_else(|| self.document.span_at(query))
            .map(|(start, _)| self.index.locate(start))
    }
}

pub(crate) fn parse_yaml(
    path: &Path,
    text: &str,
) -> Result<(toml::Value, ConfigSpans), ConfigSourceError> {
    let index = LineIndex::new(text);
    let value: noyalib::Value = noyalib::from_str_with_config(text, &parser_config())
        .map_err(|error| loader_error(path, &error))?;
    let document =
        noyalib::cst::parse_document(text).map_err(|error| loader_error(path, &error))?;
    let mut context = Context {
        path,
        text,
        index,
        document,
        spans: ConfigSpans::default(),
    };
    check_syntax(&context, &context.document.syntax().clone(), 0)?;
    let table = match value {
        noyalib::Value::Null => {
            if let Some(location) = root_value_location(&context) {
                return Err(context.error(
                    Some(location),
                    "",
                    "yaml_null",
                    "null is not accepted at the top level; use an empty document for an empty configuration",
                ));
            }
            toml::Table::new()
        }
        noyalib::Value::Mapping(mapping) => convert_mapping(&mut context, mapping, "", "")?,
        _ => {
            return Err(context.error(
                Some(Location { line: 1, column: 1 }),
                "",
                "config_not_a_table",
                "the top level of a configuration file must be a mapping",
            ));
        }
    };
    Ok((toml::Value::Table(table), context.spans))
}

fn root_value_location(context: &Context<'_>) -> Option<Location> {
    let mut offset = 0;
    for child in context.document.syntax().children() {
        let kind = match child {
            GreenChild::Token { kind, .. } => *kind,
            GreenChild::Node(node) => node.kind(),
        };
        if !is_trivia(kind)
            && !matches!(
                kind,
                SyntaxKind::Directive | SyntaxKind::DocStart | SyntaxKind::DocEnd
            )
        {
            return Some(context.index.locate(offset));
        }
        offset += child.text_len();
    }
    None
}

fn loader_error(path: &Path, error: &noyalib::Error) -> ConfigSourceError {
    let location = error
        .location()
        .filter(|location| location.line() > 0)
        .map(|location| Location {
            line: location.line(),
            column: location.column().max(1),
        });
    let (code, message) = match error {
        noyalib::Error::DuplicateKeyAt { key, .. } => (
            "yaml_duplicate_key",
            format!("duplicate key `{key}`; each key may appear once"),
        ),
        noyalib::Error::MoreThanOneDocument => (
            "yaml_multiple_documents",
            "multi-document YAML streams are not accepted; keep one document per file".to_string(),
        ),
        noyalib::Error::RepetitionLimitExceeded | noyalib::Error::Budget(..) => (
            "yaml_alias_budget",
            format!(
                "YAML exceeds the configuration expansion budget (at most \
                 {YAML_ALIAS_EXPANSION_CAP} expansions per anchor, {YAML_NODE_CAP} nodes): {error}"
            ),
        ),
        _ => ("yaml_syntax", error.to_string()),
    };
    // Some variants carry their location only inside the message.
    let location = location.or_else(|| match error {
        noyalib::Error::DuplicateKeyAt { location, .. } => Some(Location {
            line: location.line(),
            column: location.column().max(1),
        }),
        _ => None,
    });
    ConfigSourceError::Invalid {
        path: path.to_path_buf(),
        location,
        key_path: match error {
            noyalib::Error::DuplicateKeyAt { path, .. } if !path.is_empty() => Some(path.clone()),
            _ => None,
        },
        code,
        message,
    }
}

fn cst_query(parent: &str, key: &str) -> String {
    format!(
        "{parent}[\"{}\"]",
        key.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

fn convert_mapping(
    context: &mut Context<'_>,
    mapping: noyalib::Mapping,
    display: &str,
    query: &str,
) -> Result<toml::Table, ConfigSourceError> {
    let mut table = toml::Table::new();
    for (key, value) in mapping {
        let child_display = join_key_path(display, &key);
        let child_query = cst_query(query, &key);
        let location = context.locate(&child_query);
        if let Some(location) = location {
            context.spans.insert(child_display.clone(), location);
        }
        if key == "<<" {
            return Err(context.error(
                location,
                &child_display,
                "yaml_merge_key",
                MERGE_KEY_MESSAGE,
            ));
        }
        let converted = convert_value(context, value, &child_display, &child_query, location)?;
        table.insert(key, converted);
    }
    Ok(table)
}

fn convert_value(
    context: &mut Context<'_>,
    value: noyalib::Value,
    display: &str,
    query: &str,
    location: Option<Location>,
) -> Result<toml::Value, ConfigSourceError> {
    Ok(match value {
        noyalib::Value::Null => {
            return Err(context.error(
                location,
                display,
                "yaml_null",
                "null is not accepted (TOML has no null); omit the key to leave it unset",
            ));
        }
        noyalib::Value::Bool(flag) => toml::Value::Boolean(flag),
        noyalib::Value::String(text) => {
            let integer = context
                .document
                .span_at(query)
                .map(|(start, end)| strip_node_properties(&context.text[start..end]))
                .map(parse_core_integer);
            match integer {
                Some(CoreInteger::InRange(integer)) => toml::Value::Integer(integer),
                Some(CoreInteger::OutOfRange) => {
                    return Err(context.error(
                        location,
                        display,
                        "yaml_integer_range",
                        "integer out of range: configuration integers are 64-bit signed, as in TOML",
                    ));
                }
                _ => toml::Value::String(text),
            }
        }
        noyalib::Value::Number(number) => match number {
            noyalib::Number::Integer(integer) => toml::Value::Integer(integer),
            noyalib::Number::Float(float) => {
                let integer = context
                    .document
                    .span_at(query)
                    .map(|(start, end)| strip_node_properties(&context.text[start..end]))
                    .map(parse_core_integer)
                    .unwrap_or_else(|| {
                        if float.fract() == 0.0 && float.abs() >= 9.223_372_036_854_776e18 {
                            CoreInteger::OutOfRange
                        } else {
                            CoreInteger::NotInteger
                        }
                    });
                match integer {
                    CoreInteger::InRange(integer) => toml::Value::Integer(integer),
                    CoreInteger::OutOfRange => {
                        return Err(context.error(
                            location,
                            display,
                            "yaml_integer_range",
                            "integer out of range: configuration integers are 64-bit signed, as in TOML",
                        ));
                    }
                    CoreInteger::NotInteger => toml::Value::Float(float),
                }
            }
            #[allow(unreachable_patterns)]
            _ => {
                return Err(context.error(
                    location,
                    display,
                    "yaml_integer_range",
                    "integer out of range: configuration integers are 64-bit signed, as in TOML",
                ));
            }
        },
        noyalib::Value::Sequence(items) => {
            let mut array = Vec::with_capacity(items.len());
            for (position, item) in items.into_iter().enumerate() {
                let item_display = join_index_path(display, position);
                let item_query = format!("{query}[{position}]");
                let item_location = context.locate(&item_query).or(location);
                if let Some(item_location) = item_location {
                    context.spans.insert(item_display.clone(), item_location);
                }
                array.push(convert_value(
                    context,
                    item,
                    &item_display,
                    &item_query,
                    item_location,
                )?);
            }
            toml::Value::Array(array)
        }
        noyalib::Value::Mapping(mapping) => {
            toml::Value::Table(convert_mapping(context, mapping, display, query)?)
        }
        noyalib::Value::Tagged(tagged) => {
            return Err(context.error(
                location,
                display,
                "yaml_tag",
                format!("YAML tags are not accepted (found `{}`)", tagged.tag()),
            ));
        }
    })
}

/// Drop a leading anchor (`&name`) from a value's source text.
fn strip_node_properties(source: &str) -> &str {
    let mut rest = source.trim();
    while let Some(after) = rest.strip_prefix('&') {
        rest = after
            .split_once(char::is_whitespace)
            .map_or("", |(_, tail)| tail)
            .trim_start();
    }
    rest
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CoreInteger {
    NotInteger,
    InRange(i64),
    OutOfRange,
}

/// YAML 1.2 core-schema integer forms. The noyalib value pass can leave signed
/// radix literals as strings; parse their source spelling here so the loader
/// still preserves their integer meaning and refuses values outside i64.
fn parse_core_integer(text: &str) -> CoreInteger {
    let text = strip_node_properties(text).trim();
    let (negative, unsigned) = if let Some(digits) = text.strip_prefix('-') {
        (true, digits)
    } else {
        (false, text.strip_prefix('+').unwrap_or(text))
    };
    let (radix, digits) = if let Some(digits) = unsigned.strip_prefix("0o") {
        (8, digits)
    } else if let Some(digits) = unsigned.strip_prefix("0x") {
        (16, digits)
    } else {
        (10, unsigned)
    };
    let valid = match radix {
        8 => !digits.is_empty() && digits.bytes().all(|byte| (b'0'..=b'7').contains(&byte)),
        16 => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
        _ => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
    };
    if !valid {
        return CoreInteger::NotInteger;
    }
    let Ok(magnitude) = u128::from_str_radix(digits, radix) else {
        return CoreInteger::OutOfRange;
    };
    let limit = if negative {
        1_u128 << 63
    } else {
        i64::MAX as u128
    };
    if magnitude > limit {
        return CoreInteger::OutOfRange;
    }
    let integer = if negative && magnitude == (1_u128 << 63) {
        i64::MIN
    } else if negative {
        -(magnitude as i64)
    } else {
        magnitude as i64
    };
    CoreInteger::InRange(integer)
}

fn is_core_integer(text: &str) -> bool {
    !matches!(parse_core_integer(text), CoreInteger::NotInteger)
}

fn is_core_float(text: &str) -> bool {
    let unsigned = text.strip_prefix(['-', '+']).unwrap_or(text);
    if matches!(unsigned, ".inf" | ".Inf" | ".INF") || matches!(text, ".nan" | ".NaN" | ".NAN") {
        return true;
    }
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(at) => (&unsigned[..at], Some(&unsigned[at + 1..])),
        None => (unsigned, None),
    };
    let mantissa_ok = match mantissa.split_once('.') {
        Some((whole, fraction)) => {
            (!whole.is_empty() || !fraction.is_empty())
                && whole.bytes().all(|b| b.is_ascii_digit())
                && fraction.bytes().all(|b| b.is_ascii_digit())
                && !(whole.is_empty() && fraction.is_empty())
        }
        None => !mantissa.is_empty() && mantissa.bytes().all(|b| b.is_ascii_digit()),
    };
    let exponent_ok = exponent.is_none_or(|exponent| {
        let digits = exponent.strip_prefix(['-', '+']).unwrap_or(exponent);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    });
    mantissa_ok && exponent_ok
}

/// What a plain scalar resolves to under the core schema, when not a string.
fn plain_scalar_type(text: &str) -> Option<&'static str> {
    match text {
        "" | "~" | "null" | "Null" | "NULL" => Some("null"),
        "true" | "True" | "TRUE" | "false" | "False" | "FALSE" => Some("boolean"),
        _ if is_core_integer(text) => Some("integer"),
        _ if is_core_float(text) => Some("float"),
        _ => None,
    }
}

fn is_trivia(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::Whitespace | SyntaxKind::Newline | SyntaxKind::Comment | SyntaxKind::Bom
    )
}

/// Walk the lossless tree for what the typed value cannot show: tags and
/// non-string mapping keys.
fn check_syntax(
    context: &Context<'_>,
    node: &GreenNode,
    offset: usize,
) -> Result<(), ConfigSourceError> {
    let mut position = offset;
    let mut expect_flow_key = false;
    let mut key_done = false;
    let is_entry = node.kind() == SyntaxKind::MappingEntry;
    let is_flow_mapping = node.kind() == SyntaxKind::FlowMapping;
    for child in node.children() {
        let start = position;
        position += child.text_len();
        let kind = match child {
            GreenChild::Token { kind, .. } => *kind,
            GreenChild::Node(inner) => inner.kind(),
        };
        if is_trivia(kind) {
            continue;
        }
        if kind == SyntaxKind::TagMark {
            let tag = child.token_text(context.text, start).unwrap_or("!");
            return Err(context.error(
                Some(context.index.locate(start)),
                "",
                "yaml_tag",
                format!("YAML tags are not accepted (found `{tag}`)"),
            ));
        }
        if is_flow_mapping {
            match kind {
                SyntaxKind::OpenBrace | SyntaxKind::Comma => {
                    expect_flow_key = true;
                    if let GreenChild::Node(inner) = child {
                        check_syntax(context, inner, start)?;
                    }
                    continue;
                }
                SyntaxKind::ColonIndicator | SyntaxKind::CloseBrace => {
                    expect_flow_key = false;
                    continue;
                }
                SyntaxKind::AnchorMark => continue,
                _ if expect_flow_key => {
                    expect_flow_key = false;
                    check_key(context, child, kind, start)?;
                }
                _ => {}
            }
        } else if is_entry && !key_done {
            match kind {
                SyntaxKind::ColonIndicator => {
                    key_done = true;
                    continue;
                }
                SyntaxKind::AnchorMark => continue,
                _ => {
                    key_done = true;
                    check_key(context, child, kind, start)?;
                }
            }
        }
        if let GreenChild::Node(inner) = child {
            check_syntax(context, inner, start)?;
        }
    }
    Ok(())
}

fn check_key(
    context: &Context<'_>,
    child: &GreenChild,
    kind: SyntaxKind,
    start: usize,
) -> Result<(), ConfigSourceError> {
    let refuse = |what: String| {
        Err(context.error(
            Some(context.index.locate(start)),
            "",
            "yaml_non_string_key",
            format!(
                "mapping keys must be strings; this key is {what}. Quote it to use it as a string"
            ),
        ))
    };
    match kind {
        SyntaxKind::PlainScalar => {
            let text = child.token_text(context.text, start).unwrap_or("").trim();
            if text == "<<" {
                return Err(context.error(
                    Some(context.index.locate(start)),
                    "",
                    "yaml_merge_key",
                    MERGE_KEY_MESSAGE,
                ));
            }
            match plain_scalar_type(text) {
                Some(kind) => refuse(format!("a YAML {kind} (`{text}`)")),
                None => Ok(()),
            }
        }
        SyntaxKind::SingleQuotedScalar
        | SyntaxKind::DoubleQuotedScalar
        | SyntaxKind::LiteralScalar
        | SyntaxKind::FoldedScalar => Ok(()),
        SyntaxKind::QuestionIndicator => refuse("an explicit complex key (`?`)".to_string()),
        SyntaxKind::AliasMark => refuse("an alias".to_string()),
        SyntaxKind::FlowMapping | SyntaxKind::FlowSequence => refuse("a collection".to_string()),
        _ => Ok(()),
    }
}
