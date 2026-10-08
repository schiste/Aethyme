//! Conservative links from structured documentation, configuration, and tests.
//!
//! Links require explicit references and a unique repository-wide code symbol.
//! Plain prose, ambiguous names, and unsupported syntax stay unlinked.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use aethyme_graph_schema::{
    Confidence, ConfigValue, DocSection, Edge, EdgeAttributes, EdgeKind, Node, NodeId, NodeKind,
    ReferenceKindHint, Source, SourceRange,
};
use aethyme_graph_storage::{Fragment, FragmentBuildError};

use crate::filesystem::IndexedFile;
use crate::pipeline::BuiltFragment;

pub(crate) const MAX_NON_CODE_CONTENT_BYTES: u64 = 5 * 1024 * 1024;

pub(crate) struct PendingNonCode {
    path: String,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    references: Vec<PendingReference>,
}

struct PendingReference {
    source: NodeId,
    target: String,
    attributes: EdgeAttributes,
}

struct ConfigScalar {
    path: String,
    value: String,
    line: u32,
}

pub(crate) fn should_read(indexed: &IndexedFile) -> bool {
    matches!(&indexed.top_node, Node::NonCodeFile(_))
        && matches!(
            indexed.language.as_ref(),
            "markdown" | "yaml" | "json" | "toml"
        )
}

pub(crate) fn index_non_code(
    repo: &str,
    indexed: &IndexedFile,
    content: &str,
) -> Option<PendingNonCode> {
    if !should_read(indexed) {
        return None;
    }
    let Node::NonCodeFile(owner) = &indexed.top_node else {
        return None;
    };
    let mut pending = PendingNonCode {
        path: indexed.source_path.to_string(),
        nodes: Vec::new(),
        edges: Vec::new(),
        references: Vec::new(),
    };
    match indexed.language.as_ref() {
        "markdown" => add_markdown(repo, indexed, owner.id(), content, &mut pending),
        "yaml" => {
            for scalar in yaml_scalars(content) {
                add_config(repo, indexed, owner.id(), scalar, &mut pending);
            }
        }
        "json" => {
            for scalar in json_scalars(content) {
                add_config(repo, indexed, owner.id(), scalar, &mut pending);
            }
        }
        "toml" => {
            for scalar in toml_scalars(content) {
                add_config(repo, indexed, owner.id(), scalar, &mut pending);
            }
        }
        _ => return None,
    }
    Some(pending)
}

pub(crate) fn apply(
    fragments: &mut [BuiltFragment],
    pending: Vec<PendingNonCode>,
) -> Result<(), FragmentBuildError> {
    let symbols = unique_code_symbols(fragments);
    let mut pending_by_path: BTreeMap<String, PendingNonCode> = pending
        .into_iter()
        .map(|item| (item.path.clone(), item))
        .collect();

    for built in fragments {
        let mut nodes = built.fragment.nodes().to_vec();
        let mut edges = built.fragment.edges().to_vec();
        let mut changed = false;

        if let Some(item) = pending_by_path.remove(built.source_path.as_ref()) {
            changed |= !item.nodes.is_empty() || !item.edges.is_empty();
            nodes.extend(item.nodes);
            edges.extend(item.edges);
            for reference in item.references {
                if let Some(target) = symbols.get(&reference.target) {
                    edges.push(derived_edge(
                        reference.source,
                        target.clone(),
                        reference.attributes,
                    ));
                }
            }
        }

        if is_test_file(&built.source_path) {
            let test_ids: HashSet<NodeId> = built
                .fragment
                .nodes()
                .iter()
                .filter(|node| node.kind() == NodeKind::Function)
                .filter(|node| node.name().is_some_and(is_test_function))
                .map(|node| node.id().clone())
                .collect();
            if !test_ids.is_empty() {
                let placeholders: HashMap<NodeId, String> = built
                    .fragment
                    .nodes()
                    .iter()
                    .filter_map(|node| match node {
                        Node::UnresolvedSymbol(symbol) => {
                            Some((symbol.id().clone(), symbol.name().to_owned()))
                        }
                        _ => None,
                    })
                    .collect();
                for call in built.fragment.edges() {
                    if call.kind() != EdgeKind::Calls || !test_ids.contains(call.src_id()) {
                        continue;
                    }
                    let Some(name) = placeholders.get(call.dst_id()) else {
                        continue;
                    };
                    let Some(target) = symbols.get(name) else {
                        continue;
                    };
                    edges.push(derived_edge(
                        call.src_id().clone(),
                        target.clone(),
                        EdgeAttributes::Tests {
                            assertion_count: None,
                        },
                    ));
                    changed = true;
                }
            }
        }

        if changed {
            built.fragment = Fragment::new(&built.source_path, nodes, edges)?;
        }
    }
    Ok(())
}

fn unique_code_symbols(fragments: &[BuiltFragment]) -> HashMap<String, NodeId> {
    let mut candidates: BTreeMap<String, Vec<NodeId>> = BTreeMap::new();
    for node in fragments
        .iter()
        .flat_map(|built| built.fragment.nodes())
        .filter(|node| is_code_symbol(node.kind()))
    {
        let Some(name) = node.name() else { continue };
        let ids = candidates.entry(name.to_owned()).or_default();
        if !ids.iter().any(|id| id == node.id()) {
            ids.push(node.id().clone());
        }
    }
    candidates
        .into_iter()
        .filter_map(|(name, mut ids)| {
            (ids.len() == 1).then(|| (name, ids.pop().expect("one candidate was checked")))
        })
        .collect()
}

fn is_code_symbol(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function
            | NodeKind::Method
            | NodeKind::Class
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Struct
            | NodeKind::Trait
            | NodeKind::TypeAlias
            | NodeKind::GlobalVariable
            | NodeKind::BehaviorTestSurface
            | NodeKind::CliSurface
            | NodeKind::CredentialOperation
            | NodeKind::JobSurface
            | NodeKind::MiddlewareInstallation
            | NodeKind::ProxySurface
            | NodeKind::QueueSurface
            | NodeKind::RouteSurface
            | NodeKind::WebhookSurface
            | NodeKind::WorkerSurface
    )
}

fn derived_edge(source: NodeId, target: NodeId, attributes: EdgeAttributes) -> Edge {
    Edge::new(
        source,
        target,
        attributes,
        Source::Derived,
        Confidence::from_milli(850).expect("850 is within confidence range"),
    )
}

fn add_markdown(
    repo: &str,
    indexed: &IndexedFile,
    owner: &NodeId,
    content: &str,
    pending: &mut PendingNonCode,
) {
    let lines: Vec<&str> = content.lines().collect();
    let headings = markdown_headings(&lines);
    for (index, heading) in headings.iter().enumerate() {
        let end = headings
            .get(index + 1)
            .map_or(lines.len() as u32, |next| next.line.saturating_sub(1))
            .max(heading.line);
        let Ok(range) = SourceRange::new(heading.line, end) else {
            continue;
        };
        let Ok(section) = DocSection::new(
            repo,
            &indexed.source_path,
            &heading.title,
            range,
            owner.clone(),
        ) else {
            continue;
        };
        let section_id = section.id().clone();
        pending.nodes.push(Node::DocSection(section));
        pending.edges.push(Edge::new(
            owner.clone(),
            section_id.clone(),
            EdgeAttributes::Contains,
            Source::Structure,
            Confidence::FULL,
        ));
        for target in section_references(&lines, heading.line, end) {
            pending.references.push(PendingReference {
                source: section_id.clone(),
                target,
                attributes: EdgeAttributes::Documents,
            });
        }
    }
}

struct Heading {
    line: u32,
    title: String,
}

fn markdown_headings(lines: &[&str]) -> Vec<Heading> {
    let mut headings = Vec::new();
    let mut fence = None;
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index].trim_start();
        if let Some(marker) = fence_marker(line) {
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            index += 1;
            continue;
        }
        if fence.is_some() {
            index += 1;
            continue;
        }
        if let Some(title) = atx_heading(line) {
            headings.push(Heading {
                line: (index + 1) as u32,
                title,
            });
            index += 1;
            continue;
        }
        if index + 1 < lines.len()
            && !line.trim().is_empty()
            && let Some(title) = setext_heading(line, lines[index + 1].trim())
        {
            headings.push(Heading {
                line: (index + 1) as u32,
                title,
            });
            index += 2;
            continue;
        }
        index += 1;
    }
    headings
}

fn fence_marker(line: &str) -> Option<char> {
    let marker = line.chars().next()?;
    if marker != '\x60' && marker != '~' {
        return None;
    }
    (line
        .chars()
        .take_while(|character| *character == marker)
        .count()
        >= 3)
        .then_some(marker)
}

fn atx_heading(line: &str) -> Option<String> {
    let level = line
        .chars()
        .take_while(|character| *character == '#')
        .count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = line.get(level..)?;
    if !rest.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }
    let title = rest.trim().trim_end_matches('#').trim();
    (!title.is_empty()).then(|| title.to_owned())
}

fn setext_heading(line: &str, underline: &str) -> Option<String> {
    let marker = underline.chars().next()?;
    if (marker != '=' && marker != '-') || underline.chars().any(|character| character != marker) {
        return None;
    }
    Some(line.trim().to_owned())
}

fn section_references(lines: &[&str], start: u32, end: u32) -> BTreeSet<String> {
    let mut references = BTreeSet::new();
    let mut fence = None;
    for line in lines
        .iter()
        .skip(start.saturating_sub(1) as usize)
        .take((end - start + 1) as usize)
    {
        if let Some(marker) = fence_marker(line.trim_start()) {
            if fence == Some(marker) {
                fence = None;
            } else if fence.is_none() {
                fence = Some(marker);
            }
            continue;
        }
        if fence.is_some() {
            continue;
        }
        for span in inline_code_spans(line) {
            for token in span.split_whitespace() {
                if let Some(name) = doc_symbol_name(token) {
                    references.insert(name);
                }
            }
        }
        for anchor in markdown_link_anchors(line) {
            if let Some(name) = doc_symbol_name(anchor) {
                references.insert(name);
            }
        }
    }
    references
}

fn inline_code_spans(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'\x60' {
            cursor += 1;
            continue;
        }
        let run = bytes[cursor..]
            .iter()
            .take_while(|byte| **byte == b'\x60')
            .count();
        let start = cursor + run;
        let mut search = start;
        let mut close = None;
        while search < bytes.len() {
            if bytes[search] != b'\x60' {
                search += 1;
                continue;
            }
            let close_run = bytes[search..]
                .iter()
                .take_while(|byte| **byte == b'\x60')
                .count();
            if close_run == run {
                close = Some(search);
                cursor = search + close_run;
                break;
            }
            search += close_run;
        }
        if let Some(close) = close {
            spans.push(&line[start..close]);
        } else {
            break;
        }
    }
    spans
}

fn markdown_link_anchors(line: &str) -> Vec<&str> {
    let mut anchors = Vec::new();
    let mut cursor = 0;
    while let Some(start) = line[cursor..].find("](") {
        let target_start = cursor + start + 2;
        let Some(relative_end) = line[target_start..].find(')') else {
            break;
        };
        let target_end = target_start + relative_end;
        if let Some((_, anchor)) = line[target_start..target_end].rsplit_once('#') {
            let anchor = anchor.split_whitespace().next().unwrap_or(anchor);
            if !anchor.is_empty() {
                anchors.push(anchor);
            }
        }
        cursor = target_end + 1;
    }
    anchors
}

fn doc_symbol_name(raw: &str) -> Option<String> {
    let token = raw.trim().trim_matches(|character: char| {
        matches!(
            character,
            '\x60' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';' | ':'
        )
    });
    let token = token.strip_suffix("()").unwrap_or(token);
    let token = token.trim_matches(|character: char| {
        matches!(
            character,
            '\x60' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ',' | ';'
        )
    });
    let leaf = token.rsplit(['#', '.', '/', '\\', ':']).next()?;
    is_identifier(leaf).then(|| leaf.to_owned())
}

fn is_identifier(value: &str) -> bool {
    let mut characters = value.chars();
    matches!(characters.next(), Some(character) if character == '_' || character.is_ascii_alphabetic())
        && characters.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn config_target(value: &str) -> Option<String> {
    let value = value.trim();
    if value.contains("://") {
        return None;
    }
    if let Some((path, anchor)) = value.rsplit_once('#') {
        if !(path.contains('/') || path.contains('.')) {
            return None;
        }
        return is_identifier(anchor).then(|| anchor.to_owned());
    }
    let parts: Vec<&str> = value.split('.').collect();
    if parts.len() < 2 || !parts.iter().all(|part| is_identifier(part)) {
        return None;
    }
    parts.last().map(|part| (*part).to_owned())
}

fn add_config(
    repo: &str,
    indexed: &IndexedFile,
    owner: &NodeId,
    scalar: ConfigScalar,
    pending: &mut PendingNonCode,
) {
    let Ok(range) = SourceRange::new(scalar.line, scalar.line) else {
        return;
    };
    let hash = blake3::hash(scalar.value.as_bytes()).to_hex().to_string();
    let Ok(value) = ConfigValue::new(
        repo,
        &indexed.source_path,
        &scalar.path,
        &hash,
        range,
        owner.clone(),
    ) else {
        return;
    };
    let id = value.id().clone();
    pending.nodes.push(Node::ConfigValue(value));
    pending.edges.push(Edge::new(
        owner.clone(),
        id.clone(),
        EdgeAttributes::Contains,
        Source::Structure,
        Confidence::FULL,
    ));
    if let Some(target) = config_target(&scalar.value) {
        pending.references.push(PendingReference {
            source: id,
            target,
            attributes: EdgeAttributes::References {
                kind_hint: ReferenceKindHint::Mentions,
            },
        });
    }
}

fn yaml_scalars(content: &str) -> Vec<ConfigScalar> {
    let mut values = Vec::new();
    let mut parents: Vec<(usize, String)> = Vec::new();
    for (index, raw) in content.lines().enumerate() {
        let line = strip_config_comment(raw).trim_end();
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('-') || trimmed.starts_with('{') {
            continue;
        }
        let Some((raw_key, raw_value)) = trimmed.split_once(':') else {
            continue;
        };
        let Some(key) = config_key(raw_key) else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        while parents.last().is_some_and(|(level, _)| *level >= indent) {
            parents.pop();
        }
        if raw_value.trim().is_empty() {
            parents.push((indent, key));
            continue;
        }
        let Some(value) = simple_config_value(raw_value) else {
            continue;
        };
        let mut path: Vec<&str> = parents.iter().map(|(_, key)| key.as_str()).collect();
        path.push(&key);
        values.push(ConfigScalar {
            path: path.join("."),
            value,
            line: (index + 1) as u32,
        });
    }
    values
}

fn toml_scalars(content: &str) -> Vec<ConfigScalar> {
    let mut values = Vec::new();
    let mut section = String::new();
    for (index, raw) in content.lines().enumerate() {
        let line = strip_config_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            section = line
                .trim_start_matches('[')
                .trim_end_matches(']')
                .trim()
                .to_owned();
            continue;
        }
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let Some(key) = config_key(raw_key) else {
            continue;
        };
        let Some(value) = simple_config_value(raw_value) else {
            continue;
        };
        let path = if section.is_empty() {
            key
        } else {
            format!("{section}.{key}")
        };
        values.push(ConfigScalar {
            path,
            value,
            line: (index + 1) as u32,
        });
    }
    values
}

fn config_key(raw: &str) -> Option<String> {
    let key = raw.trim();
    let key = if key.len() >= 2
        && ((key.starts_with('"') && key.ends_with('"'))
            || (key.starts_with('\'') && key.ends_with('\'')))
    {
        &key[1..key.len() - 1]
    } else {
        key
    };
    (!key.is_empty()).then(|| key.to_owned())
}

fn simple_config_value(raw: &str) -> Option<String> {
    let value = strip_config_comment(raw)
        .trim()
        .trim_end_matches(',')
        .trim();
    if value.is_empty()
        || value.starts_with('[')
        || value.starts_with('{')
        || value == "|"
        || value == ">"
    {
        return None;
    }
    if value.starts_with('"') && value.ends_with('"') {
        return serde_json::from_str::<String>(value).ok();
    }
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        return Some(value[1..value.len() - 1].replace("''", "'"));
    }
    Some(value.to_owned())
}

fn strip_config_comment(value: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    let mut previous = ' ';
    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            previous = character;
            continue;
        }
        if character == '\\' && quote == Some('"') {
            escaped = true;
            previous = character;
            continue;
        }
        if let Some(active) = quote {
            if character == active {
                quote = None;
            }
        } else if character == '"' || character == '\'' {
            quote = Some(character);
        } else if character == '#' && (index == 0 || previous.is_whitespace()) {
            return &value[..index];
        }
        previous = character;
    }
    value
}

#[derive(Clone)]
enum JsonKind {
    ObjectStart,
    ObjectEnd,
    ArrayStart,
    ArrayEnd,
    Colon,
    Comma,
    String(String),
    Scalar(String),
}

#[derive(Clone)]
struct JsonToken {
    kind: JsonKind,
    line: u32,
}

fn json_scalars(content: &str) -> Vec<ConfigScalar> {
    if serde_json::from_str::<serde_json::Value>(content).is_err() {
        return Vec::new();
    }
    let Some(tokens) = tokenize_json(content) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    let mut path = Vec::new();
    let mut cursor = 0;
    if parse_json_value(&tokens, &mut cursor, &mut path, &mut values) && cursor == tokens.len() {
        values
    } else {
        Vec::new()
    }
}

fn tokenize_json(json: &str) -> Option<Vec<JsonToken>> {
    let bytes = json.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    let mut line = 1_u32;
    while index < bytes.len() {
        match bytes[index] {
            b' ' | b'\t' | b'\r' => index += 1,
            b'\n' => {
                line = line.saturating_add(1);
                index += 1;
            }
            b'{' => {
                tokens.push(JsonToken {
                    kind: JsonKind::ObjectStart,
                    line,
                });
                index += 1;
            }
            b'}' => {
                tokens.push(JsonToken {
                    kind: JsonKind::ObjectEnd,
                    line,
                });
                index += 1;
            }
            b'[' => {
                tokens.push(JsonToken {
                    kind: JsonKind::ArrayStart,
                    line,
                });
                index += 1;
            }
            b']' => {
                tokens.push(JsonToken {
                    kind: JsonKind::ArrayEnd,
                    line,
                });
                index += 1;
            }
            b':' => {
                tokens.push(JsonToken {
                    kind: JsonKind::Colon,
                    line,
                });
                index += 1;
            }
            b',' => {
                tokens.push(JsonToken {
                    kind: JsonKind::Comma,
                    line,
                });
                index += 1;
            }
            b'"' => {
                let start = index;
                let token_line = line;
                index += 1;
                let mut escaped = false;
                while index < bytes.len() {
                    let byte = bytes[index];
                    if byte == b'\n' {
                        line = line.saturating_add(1);
                    }
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
                let raw = std::str::from_utf8(&bytes[start..index]).ok()?;
                let value = serde_json::from_str::<String>(raw).ok()?;
                tokens.push(JsonToken {
                    kind: JsonKind::String(value),
                    line: token_line,
                });
            }
            _ => {
                let start = index;
                while index < bytes.len()
                    && !matches!(
                        bytes[index],
                        b' ' | b'\t' | b'\r' | b'\n' | b'{' | b'}' | b'[' | b']' | b':' | b','
                    )
                {
                    index += 1;
                }
                if start == index {
                    return None;
                }
                let value = std::str::from_utf8(&bytes[start..index]).ok()?.to_owned();
                tokens.push(JsonToken {
                    kind: JsonKind::Scalar(value),
                    line,
                });
            }
        }
    }
    Some(tokens)
}

fn parse_json_value(
    tokens: &[JsonToken],
    cursor: &mut usize,
    path: &mut Vec<String>,
    output: &mut Vec<ConfigScalar>,
) -> bool {
    let Some(token) = tokens.get(*cursor) else {
        return false;
    };
    match &token.kind {
        JsonKind::ObjectStart => {
            *cursor += 1;
            if matches!(
                tokens.get(*cursor).map(|item| &item.kind),
                Some(JsonKind::ObjectEnd)
            ) {
                *cursor += 1;
                return true;
            }
            loop {
                let Some(JsonToken {
                    kind: JsonKind::String(key),
                    ..
                }) = tokens.get(*cursor)
                else {
                    return false;
                };
                let key = key.clone();
                *cursor += 1;
                if !matches!(
                    tokens.get(*cursor).map(|item| &item.kind),
                    Some(JsonKind::Colon)
                ) {
                    return false;
                }
                *cursor += 1;
                path.push(key);
                let parsed = parse_json_value(tokens, cursor, path, output);
                path.pop();
                if !parsed {
                    return false;
                }
                match tokens.get(*cursor).map(|item| &item.kind) {
                    Some(JsonKind::Comma) => *cursor += 1,
                    Some(JsonKind::ObjectEnd) => {
                        *cursor += 1;
                        return true;
                    }
                    _ => return false,
                }
            }
        }
        JsonKind::ArrayStart => {
            *cursor += 1;
            if matches!(
                tokens.get(*cursor).map(|item| &item.kind),
                Some(JsonKind::ArrayEnd)
            ) {
                *cursor += 1;
                return true;
            }
            let mut element = 0;
            loop {
                path.push(format!("[{element}]"));
                let parsed = parse_json_value(tokens, cursor, path, output);
                path.pop();
                if !parsed {
                    return false;
                }
                element += 1;
                match tokens.get(*cursor).map(|item| &item.kind) {
                    Some(JsonKind::Comma) => *cursor += 1,
                    Some(JsonKind::ArrayEnd) => {
                        *cursor += 1;
                        return true;
                    }
                    _ => return false,
                }
            }
        }
        JsonKind::String(value) | JsonKind::Scalar(value) => {
            if !path.is_empty() {
                output.push(ConfigScalar {
                    path: path.join("."),
                    value: value.clone(),
                    line: token.line,
                });
            }
            *cursor += 1;
            true
        }
        JsonKind::ObjectEnd | JsonKind::ArrayEnd | JsonKind::Colon | JsonKind::Comma => false,
    }
}

fn is_test_file(path: &str) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    let name = parts.last().copied().unwrap_or_default();
    parts.contains(&"tests")
        || name.starts_with("test_")
        || name.ends_with("_test.py")
        || name.ends_with(".test.ts")
        || name.ends_with(".test.tsx")
        || name.ends_with(".spec.ts")
        || name.ends_with(".spec.tsx")
}

fn is_test_function(name: &str) -> bool {
    name.starts_with("test_") || name.ends_with("_test")
}

#[cfg(test)]
mod tests {
    use super::{config_target, markdown_headings, section_references, yaml_scalars};

    #[test]
    fn markdown_only_links_explicit_code_and_fragment_references() {
        let lines = [
            "# API",
            "Call \x60handle_request()\x60 from the client.",
            "[source](src/handlers.py#handle_request)",
            "Plain handle_request prose is not a reference.",
            "\x60\x60\x60python",
            "# this is code, not a section",
            "\x60also_not_a_reference\x60",
            "\x60\x60\x60",
        ];
        let headings = markdown_headings(&lines);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].title, "API");
        assert_eq!(
            section_references(&lines, 1, lines.len() as u32),
            ["handle_request".to_owned()].into_iter().collect()
        );
    }

    #[test]
    fn markdown_setext_sections_keep_heading_boundaries() {
        let lines = [
            "Overview",
            "========",
            "See \x60start_server\x60.",
            "Next",
            "----",
            "No link.",
        ];
        let headings = markdown_headings(&lines);
        assert_eq!(headings.len(), 2);
        assert_eq!(headings[0].title, "Overview");
        assert_eq!(headings[1].title, "Next");
    }

    #[test]
    fn config_links_require_qualified_names() {
        assert_eq!(
            config_target("src.handlers#handle_request"),
            Some("handle_request".to_owned())
        );
        assert_eq!(
            config_target("app.handlers.handle_request"),
            Some("handle_request".to_owned())
        );
        assert_eq!(config_target("handle_request"), None);
        assert_eq!(config_target("https://example.test/#handle_request"), None);
    }

    #[test]
    fn yaml_values_keep_dotted_paths_and_source_lines() {
        let values = yaml_scalars("server:\n  handler: src.handlers#handle_request\n");
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].path, "server.handler");
        assert_eq!(values[0].value, "src.handlers#handle_request");
        assert_eq!(values[0].line, 2);
    }
}
