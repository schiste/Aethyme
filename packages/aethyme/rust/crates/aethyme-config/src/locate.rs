//! Source locations, and the span-preserving TOML parse.

use std::path::Path;

use crate::{ConfigSourceError, ConfigSpans, join_index_path, join_key_path};

/// A 1-based line and column (in characters) in a configuration file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Location {
    pub line: usize,
    pub column: usize,
}

/// Byte offset to [`Location`] for one source text.
pub(crate) struct LineIndex<'a> {
    text: &'a str,
    starts: Vec<usize>,
}

impl<'a> LineIndex<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let mut starts = vec![0];
        starts.extend(
            text.bytes()
                .enumerate()
                .filter(|(_, byte)| *byte == b'\n')
                .map(|(index, _)| index + 1),
        );
        Self { text, starts }
    }

    pub(crate) fn locate(&self, offset: usize) -> Location {
        let offset = offset.min(self.text.len());
        let line = self.starts.partition_point(|start| *start <= offset);
        let start = self.starts[line - 1];
        let column = self
            .text
            .get(start..offset)
            .map_or(offset - start, |prefix| prefix.chars().count())
            + 1;
        Location { line, column }
    }
}

pub(crate) fn parse_toml(
    path: &Path,
    text: &str,
) -> Result<(toml::Value, ConfigSpans), ConfigSourceError> {
    let index = LineIndex::new(text);
    let table: toml::Table = toml::from_str(text).map_err(|error| ConfigSourceError::Invalid {
        path: path.to_path_buf(),
        location: error.span().map(|span| index.locate(span.start)),
        key_path: None,
        code: "toml_syntax",
        message: error.message().trim().to_string(),
    })?;
    let mut spans = ConfigSpans::default();
    // The value already parsed, so a span parse failure only loses locations.
    if let Ok(document) = toml_edit::ImDocument::parse(text) {
        record_table(document.as_table(), "", &index, &mut spans);
    }
    Ok((toml::Value::Table(table), spans))
}

fn record_table(
    table: &dyn toml_edit::TableLike,
    prefix: &str,
    index: &LineIndex<'_>,
    spans: &mut ConfigSpans,
) {
    for (key, item) in table.iter() {
        let path = join_key_path(prefix, key);
        let span = table
            .get_key_value(key)
            .and_then(|(key, _)| key.span())
            .or_else(|| item.span());
        if let Some(span) = span {
            spans.insert(path.clone(), index.locate(span.start));
        }
        record_item(item, &path, index, spans);
    }
}

fn record_item(item: &toml_edit::Item, path: &str, index: &LineIndex<'_>, spans: &mut ConfigSpans) {
    match item {
        toml_edit::Item::Table(table) => record_table(table, path, index, spans),
        toml_edit::Item::ArrayOfTables(tables) => {
            for (position, table) in tables.iter().enumerate() {
                let item_path = join_index_path(path, position);
                if let Some(span) = table.span() {
                    spans.insert(item_path.clone(), index.locate(span.start));
                }
                record_table(table, &item_path, index, spans);
            }
        }
        toml_edit::Item::Value(value) => record_value(value, path, index, spans),
        toml_edit::Item::None => {}
    }
}

fn record_value(
    value: &toml_edit::Value,
    path: &str,
    index: &LineIndex<'_>,
    spans: &mut ConfigSpans,
) {
    match value {
        toml_edit::Value::Array(array) => {
            for (position, item) in array.iter().enumerate() {
                let item_path = join_index_path(path, position);
                if let Some(span) = item.span() {
                    spans.insert(item_path.clone(), index.locate(span.start));
                }
                record_value(item, &item_path, index, spans);
            }
        }
        toml_edit::Value::InlineTable(table) => record_table(table, path, index, spans),
        _ => {}
    }
}
