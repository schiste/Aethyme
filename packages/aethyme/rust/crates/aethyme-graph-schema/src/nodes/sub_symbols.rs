//! Sub-symbol node kinds: Field, GlobalVariable, Parameter,
//! Statement, Expression.
//!
//! Required fields per schema doc §3.4. Statement and Expression are
//! tier-2/tier-3 materialization respectively — they exist in the
//! schema but are only emitted to fragments when an intent asks for
//! statement- or expression-level evidence.

use serde::{Deserialize, Serialize};

use crate::common::{SourceRange, Visibility};
use crate::{NodeId, NodeKind};

// ─── Field ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Field {
    id: NodeId,
    name: Box<str>,
    type_str: Box<str>,
    enclosing_type_id: NodeId,
    visibility: Visibility,
}

impl Field {
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        type_str: &str,
        enclosing_type_id: NodeId,
        visibility: Visibility,
    ) -> Result<Self, FieldConstructionError> {
        if name.is_empty() {
            return Err(FieldConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(FieldConstructionError::EmptyFilePath);
        }
        if type_str.is_empty() {
            return Err(FieldConstructionError::EmptyType);
        }
        let id = NodeId::new(NodeKind::Field, repo, file_path, name)
            .map_err(FieldConstructionError::Id)?;
        Ok(Field {
            id,
            name: name.into(),
            type_str: type_str.into(),
            enclosing_type_id,
            visibility,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn type_str(&self) -> &str {
        &self.type_str
    }
    pub fn enclosing_type_id(&self) -> &NodeId {
        &self.enclosing_type_id
    }
    pub fn visibility(&self) -> Visibility {
        self.visibility
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldConstructionError {
    EmptyName,
    EmptyFilePath,
    EmptyType,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for FieldConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("Field: name must not be empty"),
            Self::EmptyFilePath => {
                f.write_str("Field: file_path must not be empty")
            }
            Self::EmptyType => f.write_str("Field: type_str must not be empty"),
            Self::Id(e) => write!(f, "Field: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for FieldConstructionError {}

// ─── GlobalVariable ──────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GlobalVariable {
    id: NodeId,
    name: Box<str>,
    /// Optional because dynamically-typed languages (Python, JS)
    /// often have globals without type annotations.
    type_str: Option<Box<str>>,
    source_range: SourceRange,
}

impl GlobalVariable {
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        type_str: Option<&str>,
        source_range: SourceRange,
    ) -> Result<Self, GlobalVariableConstructionError> {
        if name.is_empty() {
            return Err(GlobalVariableConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(GlobalVariableConstructionError::EmptyFilePath);
        }
        let id = NodeId::new(NodeKind::GlobalVariable, repo, file_path, name)
            .map_err(GlobalVariableConstructionError::Id)?;
        Ok(GlobalVariable {
            id,
            name: name.into(),
            type_str: type_str.map(Into::into),
            source_range,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn type_str(&self) -> Option<&str> {
        self.type_str.as_deref()
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobalVariableConstructionError {
    EmptyName,
    EmptyFilePath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for GlobalVariableConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => {
                f.write_str("GlobalVariable: name must not be empty")
            }
            Self::EmptyFilePath => {
                f.write_str("GlobalVariable: file_path must not be empty")
            }
            Self::Id(e) => {
                write!(f, "GlobalVariable: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for GlobalVariableConstructionError {}

// ─── Parameter ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Parameter {
    id: NodeId,
    name: Box<str>,
    type_str: Option<Box<str>>,
    position: u16,
    enclosing_callable_id: NodeId,
}

impl Parameter {
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        type_str: Option<&str>,
        position: u16,
        enclosing_callable_id: NodeId,
    ) -> Result<Self, ParameterConstructionError> {
        if name.is_empty() {
            return Err(ParameterConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(ParameterConstructionError::EmptyFilePath);
        }
        // Per-callable parameter names disambiguate inside the file;
        // mangle the symbol name with the callable's hash suffix to
        // ensure cross-callable uniqueness within the same file.
        let symbol = format!(
            "{}#{}:{name}",
            enclosing_callable_id.kind().name(),
            enclosing_callable_id.hash_suffix(),
        );
        let id = NodeId::new(NodeKind::Parameter, repo, file_path, &symbol)
            .map_err(ParameterConstructionError::Id)?;
        Ok(Parameter {
            id,
            name: name.into(),
            type_str: type_str.map(Into::into),
            position,
            enclosing_callable_id,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn type_str(&self) -> Option<&str> {
        self.type_str.as_deref()
    }
    pub fn position(&self) -> u16 {
        self.position
    }
    pub fn enclosing_callable_id(&self) -> &NodeId {
        &self.enclosing_callable_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterConstructionError {
    EmptyName,
    EmptyFilePath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for ParameterConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => {
                f.write_str("Parameter: name must not be empty")
            }
            Self::EmptyFilePath => {
                f.write_str("Parameter: file_path must not be empty")
            }
            Self::Id(e) => {
                write!(f, "Parameter: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for ParameterConstructionError {}

// ─── Statement ───────────────────────────────────────────────────────

/// A statement inside a function/method body. Top-level AST
/// statements only (direct children of the body). Tier-2
/// materialization — see schema doc §3.4.
///
/// Symbol-name component of the NodeId is synthesized from
/// `(enclosing_callable_id, position_in_body, kind_tag)` because
/// statements don't have inherent names.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Statement {
    id: NodeId,
    source_range: SourceRange,
    /// Statement kind from the parser: "assign", "if", "return",
    /// "for", "while", etc. Language-specific values are admitted
    /// (e.g., Python's "with"); the canonical English-snake_case
    /// names are conventional but not enforced.
    kind_tag: Box<str>,
    enclosing_callable_id: NodeId,
    /// Zero-based position of this statement within the enclosing
    /// callable's body. Used for ID synthesis and ordering.
    position_in_body: u32,
}

impl Statement {
    pub fn new(
        repo: &str,
        file_path: &str,
        kind_tag: &str,
        source_range: SourceRange,
        enclosing_callable_id: NodeId,
        position_in_body: u32,
    ) -> Result<Self, StatementConstructionError> {
        if file_path.is_empty() {
            return Err(StatementConstructionError::EmptyFilePath);
        }
        if kind_tag.is_empty() {
            return Err(StatementConstructionError::EmptyKindTag);
        }
        let symbol = format!(
            "{}#{}:stmt{position_in_body}:{kind_tag}",
            enclosing_callable_id.kind().name(),
            enclosing_callable_id.hash_suffix(),
        );
        let id = NodeId::new(NodeKind::Statement, repo, file_path, &symbol)
            .map_err(StatementConstructionError::Id)?;
        Ok(Statement {
            id,
            source_range,
            kind_tag: kind_tag.into(),
            enclosing_callable_id,
            position_in_body,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
    pub fn kind_tag(&self) -> &str {
        &self.kind_tag
    }
    pub fn enclosing_callable_id(&self) -> &NodeId {
        &self.enclosing_callable_id
    }
    pub fn position_in_body(&self) -> u32 {
        self.position_in_body
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatementConstructionError {
    EmptyFilePath,
    EmptyKindTag,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for StatementConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFilePath => {
                f.write_str("Statement: file_path must not be empty")
            }
            Self::EmptyKindTag => {
                f.write_str("Statement: kind_tag must not be empty")
            }
            Self::Id(e) => {
                write!(f, "Statement: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for StatementConstructionError {}

// ─── Expression ──────────────────────────────────────────────────────

/// An expression inside a statement. The schema's deepest first-class
/// kind. Tier-3 materialization.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Expression {
    id: NodeId,
    source_range: SourceRange,
    kind_tag: Box<str>,
    enclosing_statement_id: NodeId,
    position_in_statement: u32,
}

impl Expression {
    pub fn new(
        repo: &str,
        file_path: &str,
        kind_tag: &str,
        source_range: SourceRange,
        enclosing_statement_id: NodeId,
        position_in_statement: u32,
    ) -> Result<Self, ExpressionConstructionError> {
        if file_path.is_empty() {
            return Err(ExpressionConstructionError::EmptyFilePath);
        }
        if kind_tag.is_empty() {
            return Err(ExpressionConstructionError::EmptyKindTag);
        }
        let symbol = format!(
            "stmt#{}:expr{position_in_statement}:{kind_tag}",
            enclosing_statement_id.hash_suffix(),
        );
        let id =
            NodeId::new(NodeKind::Expression, repo, file_path, &symbol)
                .map_err(ExpressionConstructionError::Id)?;
        Ok(Expression {
            id,
            source_range,
            kind_tag: kind_tag.into(),
            enclosing_statement_id,
            position_in_statement,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }
    pub fn kind_tag(&self) -> &str {
        &self.kind_tag
    }
    pub fn enclosing_statement_id(&self) -> &NodeId {
        &self.enclosing_statement_id
    }
    pub fn position_in_statement(&self) -> u32 {
        self.position_in_statement
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpressionConstructionError {
    EmptyFilePath,
    EmptyKindTag,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for ExpressionConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyFilePath => {
                f.write_str("Expression: file_path must not be empty")
            }
            Self::EmptyKindTag => {
                f.write_str("Expression: kind_tag must not be empty")
            }
            Self::Id(e) => {
                write!(f, "Expression: ID construction failed: {e}")
            }
        }
    }
}

impl std::error::Error for ExpressionConstructionError {}
