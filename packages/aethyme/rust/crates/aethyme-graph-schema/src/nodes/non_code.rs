//! Non-code node kinds: DocSection, Docstring, Comment, ConfigValue.
//!
//! Required fields per schema doc §3.5. These are the
//! documentation-as-graph kinds — they let edges like `Documents`,
//! `Decides`, `References`, and `Configures` connect doc / comment /
//! config content into the code graph.

use serde::{Deserialize, Serialize};

use crate::common::SourceRange;
use crate::{NodeId, NodeKind};

// ─── DocSection ──────────────────────────────────────────────────────

/// A heading-bounded section of a structured non-code file
/// (markdown, RST, asciidoc, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DocSection {
    id: NodeId,
    enclosing_non_code_file_id: NodeId,
    heading: Box<str>,
    source_range: SourceRange,
}

impl DocSection {
    pub fn new(
        repo: &str,
        file_path: &str,
        heading: &str,
        source_range: SourceRange,
        enclosing_non_code_file_id: NodeId,
    ) -> Result<Self, DocSectionConstructionError> {
        if file_path.is_empty() {
            return Err(DocSectionConstructionError::EmptyFilePath);
        }
        if heading.is_empty() {
            return Err(DocSectionConstructionError::EmptyHeading);
        }
        // Synthesize the symbol name from the enclosing file's hash
        // suffix + start line so two sections with the same heading
        // in the same doc get distinct IDs.
        let symbol = format!(
            "section#{}:{}:{}",
            enclosing_non_code_file_id.hash_suffix(),
            source_range.start_line(),
            heading,
        );
        let id = NodeId::new(NodeKind::DocSection, repo, file_path, &symbol)
            .map_err(DocSectionConstructionError::Id)?;
        Ok(DocSection {
            id,
            enclosing_non_code_file_id,
            heading: heading.into(),
            source_range,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn enclosing_non_code_file_id(&self) -> &NodeId {
        &self.enclosing_non_code_file_id
    }
    pub fn heading(&self) -> &str {
        &self.heading
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocSectionConstructionError {
    EmptyFilePath,
    EmptyHeading,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for DocSectionConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFilePath => f.write_str("DocSection: file_path must not be empty"),
            Self::EmptyHeading => f.write_str("DocSection: heading must not be empty"),
            Self::Id(e) => {
                write!(f, "DocSection: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for DocSectionConstructionError {}

// ─── Docstring ───────────────────────────────────────────────────────

/// A docstring attached to a callable / class / module. Distinct
/// from [`Comment`]: docstrings have a well-defined target
/// (the enclosing symbol), comments may or may not.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Docstring {
    id: NodeId,
    target_symbol_id: NodeId,
    source_range: SourceRange,
    /// Hash of the docstring text. The text body itself is NOT
    /// stored in this node — it's available via the indexed-reference
    /// store (per schema doc §3.5). This keeps the graph payload
    /// compact when many nodes have long docstrings.
    text_hash: Box<str>,
}

impl Docstring {
    pub fn new(
        repo: &str,
        file_path: &str,
        target_symbol_id: NodeId,
        source_range: SourceRange,
        text_hash: &str,
    ) -> Result<Self, DocstringConstructionError> {
        if file_path.is_empty() {
            return Err(DocstringConstructionError::EmptyFilePath);
        }
        if text_hash.is_empty() {
            return Err(DocstringConstructionError::EmptyTextHash);
        }
        // Docstring ID is keyed on target_symbol_id so the same
        // target always produces the same docstring NodeId.
        let symbol = format!("doc#{}", target_symbol_id.hash_suffix());
        let id = NodeId::new(NodeKind::Docstring, repo, file_path, &symbol)
            .map_err(DocstringConstructionError::Id)?;
        Ok(Docstring {
            id,
            target_symbol_id,
            source_range,
            text_hash: text_hash.into(),
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn target_symbol_id(&self) -> &NodeId {
        &self.target_symbol_id
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
    pub fn text_hash(&self) -> &str {
        &self.text_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocstringConstructionError {
    EmptyFilePath,
    EmptyTextHash,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for DocstringConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFilePath => f.write_str("Docstring: file_path must not be empty"),
            Self::EmptyTextHash => f.write_str("Docstring: text_hash must not be empty"),
            Self::Id(e) => {
                write!(f, "Docstring: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for DocstringConstructionError {}

// ─── Comment ─────────────────────────────────────────────────────────

/// Recognized tags for first-class comments. Untagged inline
/// comments are stored as attributes on the enclosing statement,
/// not as nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentTag {
    Todo,
    Fixme,
    Note,
    Decision,
    SeeAlso,
}

impl CommentTag {
    pub const fn name(self) -> &'static str {
        match self {
            CommentTag::Todo => "todo",
            CommentTag::Fixme => "fixme",
            CommentTag::Note => "note",
            CommentTag::Decision => "decision",
            CommentTag::SeeAlso => "see_also",
        }
    }
}

pub const ALL_COMMENT_TAGS: &[CommentTag] = &[
    CommentTag::Todo,
    CommentTag::Fixme,
    CommentTag::Note,
    CommentTag::Decision,
    CommentTag::SeeAlso,
];

/// An inline source comment carrying a recognized tag.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Comment {
    id: NodeId,
    /// Optional: comments inside function bodies attach to the
    /// enclosing statement. Comments at module top level have no
    /// enclosing statement.
    target_statement_id: Option<NodeId>,
    start_line: u32,
    tag: CommentTag,
    text_hash: Box<str>,
}

impl Comment {
    pub fn new(
        repo: &str,
        file_path: &str,
        start_line: u32,
        tag: CommentTag,
        text_hash: &str,
        target_statement_id: Option<NodeId>,
    ) -> Result<Self, CommentConstructionError> {
        if file_path.is_empty() {
            return Err(CommentConstructionError::EmptyFilePath);
        }
        if start_line == 0 {
            return Err(CommentConstructionError::StartLineZero);
        }
        if text_hash.is_empty() {
            return Err(CommentConstructionError::EmptyTextHash);
        }
        let symbol = format!("comment#{}:{start_line}:{}", tag.name(), text_hash);
        let id = NodeId::new(NodeKind::Comment, repo, file_path, &symbol)
            .map_err(CommentConstructionError::Id)?;
        Ok(Comment {
            id,
            target_statement_id,
            start_line,
            tag,
            text_hash: text_hash.into(),
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn target_statement_id(&self) -> Option<&NodeId> {
        self.target_statement_id.as_ref()
    }
    pub fn start_line(&self) -> u32 {
        self.start_line
    }
    pub fn tag(&self) -> CommentTag {
        self.tag
    }
    pub fn text_hash(&self) -> &str {
        &self.text_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentConstructionError {
    EmptyFilePath,
    StartLineZero,
    EmptyTextHash,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for CommentConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFilePath => f.write_str("Comment: file_path must not be empty"),
            Self::StartLineZero => f.write_str("Comment: start_line must be >= 1"),
            Self::EmptyTextHash => f.write_str("Comment: text_hash must not be empty"),
            Self::Id(e) => write!(f, "Comment: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for CommentConstructionError {}

// ─── ConfigValue ─────────────────────────────────────────────────────

/// A single addressable config value (e.g., `database.url` in a
/// YAML/TOML file). Per-key granularity so `Configures` edges
/// connect code that reads each value.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ConfigValue {
    id: NodeId,
    enclosing_non_code_file_id: NodeId,
    /// Dotted path within the config file, e.g. "database.url".
    config_path: Box<str>,
    value_hash: Box<str>,
    source_range: SourceRange,
}

impl ConfigValue {
    pub fn new(
        repo: &str,
        file_path: &str,
        config_path: &str,
        value_hash: &str,
        source_range: SourceRange,
        enclosing_non_code_file_id: NodeId,
    ) -> Result<Self, ConfigValueConstructionError> {
        if file_path.is_empty() {
            return Err(ConfigValueConstructionError::EmptyFilePath);
        }
        if config_path.is_empty() {
            return Err(ConfigValueConstructionError::EmptyConfigPath);
        }
        if value_hash.is_empty() {
            return Err(ConfigValueConstructionError::EmptyValueHash);
        }
        let symbol = format!(
            "config#{}:{}",
            enclosing_non_code_file_id.hash_suffix(),
            config_path,
        );
        let id = NodeId::new(NodeKind::ConfigValue, repo, file_path, &symbol)
            .map_err(ConfigValueConstructionError::Id)?;
        Ok(ConfigValue {
            id,
            enclosing_non_code_file_id,
            config_path: config_path.into(),
            value_hash: value_hash.into(),
            source_range,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn enclosing_non_code_file_id(&self) -> &NodeId {
        &self.enclosing_non_code_file_id
    }
    pub fn config_path(&self) -> &str {
        &self.config_path
    }
    pub fn value_hash(&self) -> &str {
        &self.value_hash
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigValueConstructionError {
    EmptyFilePath,
    EmptyConfigPath,
    EmptyValueHash,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for ConfigValueConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFilePath => f.write_str("ConfigValue: file_path must not be empty"),
            Self::EmptyConfigPath => f.write_str("ConfigValue: config_path must not be empty"),
            Self::EmptyValueHash => f.write_str("ConfigValue: value_hash must not be empty"),
            Self::Id(e) => {
                write!(f, "ConfigValue: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for ConfigValueConstructionError {}
