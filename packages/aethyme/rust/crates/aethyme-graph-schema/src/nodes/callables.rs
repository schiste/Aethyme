//! Callable node kinds: Function, Method, Lambda.
//!
//! All three implement the [`Callable`] trait. Required fields per
//! schema doc §3.2.

use serde::{Deserialize, Serialize};

use crate::common::{SourceRange, Visibility};
use crate::{NodeId, NodeKind};

/// Parameter representation in a callable's inline signature.
///
/// Distinct from the `Parameter` node kind (commit 1.10): this is
/// the signature-level view (name + type + default value), whereas
/// the `Parameter` node is a first-class graph node addressable by
/// other graph nodes (e.g., future dataflow edges). The two carry
/// different information at different layers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParameterSignature {
    pub name: Box<str>,
    /// Type annotation, if the language has them and the parameter
    /// is annotated. Languages without static types omit this.
    pub type_str: Option<Box<str>>,
    /// Default value text, if any. Kept as a string rather than a
    /// typed value because default values can be arbitrary
    /// expressions in most languages.
    pub default_value: Option<Box<str>>,
}

/// Common protocol satisfied by every callable node kind.
///
/// Function, Method, and Lambda all expose this. Consumers that
/// don't care which specific kind they're working with (e.g.,
/// "find all callables in this file") use the trait; consumers
/// that need kind-specific fields (Method's receiver_type, Lambda's
/// assigned_name) match on the concrete struct.
pub trait Callable {
    fn id(&self) -> &NodeId;
    fn name(&self) -> &str;
    fn signature(&self) -> &str;
    fn parameters(&self) -> &[ParameterSignature];
    fn return_type(&self) -> Option<&str>;
    fn source_range(&self) -> SourceRange;
    fn visibility(&self) -> Visibility;
}

// ─── Function ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Function {
    id: NodeId,
    name: Box<str>,
    signature: Box<str>,
    parameters: Vec<ParameterSignature>,
    return_type: Option<Box<str>>,
    source_range: SourceRange,
    visibility: Visibility,
    is_top_level: bool,
}

impl Function {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        signature: &str,
        parameters: Vec<ParameterSignature>,
        return_type: Option<&str>,
        source_range: SourceRange,
        visibility: Visibility,
        is_top_level: bool,
    ) -> Result<Self, FunctionConstructionError> {
        if name.is_empty() {
            return Err(FunctionConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(FunctionConstructionError::EmptyFilePath);
        }
        let id = NodeId::new(NodeKind::Function, repo, file_path, name)
            .map_err(FunctionConstructionError::Id)?;
        Ok(Function {
            id,
            name: name.into(),
            signature: signature.into(),
            parameters,
            return_type: return_type.map(Into::into),
            source_range,
            visibility,
            is_top_level,
        })
    }

    pub fn is_top_level(&self) -> bool {
        self.is_top_level
    }
}

impl Callable for Function {
    fn id(&self) -> &NodeId {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn signature(&self) -> &str {
        &self.signature
    }
    fn parameters(&self) -> &[ParameterSignature] {
        &self.parameters
    }
    fn return_type(&self) -> Option<&str> {
        self.return_type.as_deref()
    }
    fn source_range(&self) -> SourceRange {
        self.source_range
    }
    fn visibility(&self) -> Visibility {
        self.visibility
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FunctionConstructionError {
    EmptyName,
    EmptyFilePath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for FunctionConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("Function: name must not be empty"),
            Self::EmptyFilePath => f.write_str("Function: file_path must not be empty"),
            Self::Id(e) => write!(f, "Function: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for FunctionConstructionError {}

// ─── Method ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Method {
    id: NodeId,
    name: Box<str>,
    signature: Box<str>,
    parameters: Vec<ParameterSignature>,
    return_type: Option<Box<str>>,
    source_range: SourceRange,
    visibility: Visibility,
    /// The enclosing type's NodeId (a Class, Struct, Trait,
    /// Interface, or Enum).
    receiver_type: NodeId,
    is_static: bool,
    is_virtual: bool,
}

impl Method {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        signature: &str,
        parameters: Vec<ParameterSignature>,
        return_type: Option<&str>,
        source_range: SourceRange,
        visibility: Visibility,
        receiver_type: NodeId,
        is_static: bool,
        is_virtual: bool,
    ) -> Result<Self, MethodConstructionError> {
        if name.is_empty() {
            return Err(MethodConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(MethodConstructionError::EmptyFilePath);
        }
        // Fold the receiver_type's hash suffix into the symbol name
        // before NodeId hashing. Without this, two methods with the
        // same name on different classes/structs/traits in the same
        // file (e.g. `__init__` on multiple classes, `new` on
        // multiple structs) would produce identical NodeIds, which
        // would be a correctness bug: the methods ARE distinct
        // nodes. The receiver's hash suffix is a stable component
        // of its NodeId, so it adds no instability the receiver
        // didn't already have.
        let symbol_name = format!("{name}#receiver:{}", receiver_type.hash_suffix(),);
        let id = NodeId::new(NodeKind::Method, repo, file_path, &symbol_name)
            .map_err(MethodConstructionError::Id)?;
        Ok(Method {
            id,
            name: name.into(),
            signature: signature.into(),
            parameters,
            return_type: return_type.map(Into::into),
            source_range,
            visibility,
            receiver_type,
            is_static,
            is_virtual,
        })
    }

    pub fn receiver_type(&self) -> &NodeId {
        &self.receiver_type
    }
    pub fn is_static(&self) -> bool {
        self.is_static
    }
    pub fn is_virtual(&self) -> bool {
        self.is_virtual
    }
}

impl Callable for Method {
    fn id(&self) -> &NodeId {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn signature(&self) -> &str {
        &self.signature
    }
    fn parameters(&self) -> &[ParameterSignature] {
        &self.parameters
    }
    fn return_type(&self) -> Option<&str> {
        self.return_type.as_deref()
    }
    fn source_range(&self) -> SourceRange {
        self.source_range
    }
    fn visibility(&self) -> Visibility {
        self.visibility
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MethodConstructionError {
    EmptyName,
    EmptyFilePath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for MethodConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("Method: name must not be empty"),
            Self::EmptyFilePath => f.write_str("Method: file_path must not be empty"),
            Self::Id(e) => write!(f, "Method: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for MethodConstructionError {}

// ─── Lambda ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lambda {
    id: NodeId,
    /// For lambdas assigned to a named variable, the variable name.
    /// For inline-passed lambdas, a synthetic positional anchor like
    /// "<anon@line:42>" — set by the indexer at parse time.
    name: Box<str>,
    signature: Box<str>,
    parameters: Vec<ParameterSignature>,
    return_type: Option<Box<str>>,
    source_range: SourceRange,
    visibility: Visibility,
    enclosing_callable_id: NodeId,
    /// Set when the lambda is assigned to a named variable
    /// (`const handler = () => {...}`). None for inline-passed
    /// lambdas (`.map(x => x.id)`).
    assigned_name: Option<Box<str>>,
}

impl Lambda {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        signature: &str,
        parameters: Vec<ParameterSignature>,
        return_type: Option<&str>,
        source_range: SourceRange,
        visibility: Visibility,
        enclosing_callable_id: NodeId,
        assigned_name: Option<&str>,
    ) -> Result<Self, LambdaConstructionError> {
        if name.is_empty() {
            return Err(LambdaConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(LambdaConstructionError::EmptyFilePath);
        }
        let id = NodeId::new(NodeKind::Lambda, repo, file_path, name)
            .map_err(LambdaConstructionError::Id)?;
        Ok(Lambda {
            id,
            name: name.into(),
            signature: signature.into(),
            parameters,
            return_type: return_type.map(Into::into),
            source_range,
            visibility,
            enclosing_callable_id,
            assigned_name: assigned_name.map(Into::into),
        })
    }

    pub fn enclosing_callable_id(&self) -> &NodeId {
        &self.enclosing_callable_id
    }
    pub fn assigned_name(&self) -> Option<&str> {
        self.assigned_name.as_deref()
    }
}

impl Callable for Lambda {
    fn id(&self) -> &NodeId {
        &self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn signature(&self) -> &str {
        &self.signature
    }
    fn parameters(&self) -> &[ParameterSignature] {
        &self.parameters
    }
    fn return_type(&self) -> Option<&str> {
        self.return_type.as_deref()
    }
    fn source_range(&self) -> SourceRange {
        self.source_range
    }
    fn visibility(&self) -> Visibility {
        self.visibility
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LambdaConstructionError {
    EmptyName,
    EmptyFilePath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for LambdaConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("Lambda: name must not be empty"),
            Self::EmptyFilePath => f.write_str("Lambda: file_path must not be empty"),
            Self::Id(e) => write!(f, "Lambda: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for LambdaConstructionError {}
