//! The partial-knowledge kind: UnresolvedSymbol.
//!
//! Used when an indexer sees a reference (call, import, type use) to
//! a symbol it cannot locate — e.g., dynamic Python, an unparsed
//! third-party dependency, a missing file. Distinct from omitting the
//! reference entirely: the edge still exists, it just points to an
//! UnresolvedSymbol placeholder.
//!
//! Per the schema doc's "required-when-applicable" rule, this kind
//! exists so we never relax a required field on a real kind (Function,
//! Method, etc.) just to accommodate partial knowledge.

use serde::{Deserialize, Serialize};

use crate::{NodeId, NodeKind};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UnresolvedSymbol {
    id: NodeId,
    name: Box<str>,
    /// What kind the indexer guessed the symbol would be, if it had
    /// a guess. None when no inference was possible.
    expected_kind: Option<NodeKind>,
    /// The node that triggered the unresolved reference. Used to
    /// disambiguate same-named unresolved symbols from different
    /// call sites.
    referenced_from_id: NodeId,
}

impl UnresolvedSymbol {
    pub fn new(
        repo: &str,
        file_path: &str,
        name: &str,
        expected_kind: Option<NodeKind>,
        referenced_from_id: NodeId,
    ) -> Result<Self, UnresolvedSymbolConstructionError> {
        if name.is_empty() {
            return Err(UnresolvedSymbolConstructionError::EmptyName);
        }
        if file_path.is_empty() {
            return Err(UnresolvedSymbolConstructionError::EmptyFilePath);
        }
        // The symbol component for the NodeId folds in the
        // referenced_from_id's hash suffix so two unresolved
        // references to `foo` from different call sites become
        // distinct UnresolvedSymbol nodes. Without this, every
        // unresolved `print` call in a Python repo would collapse
        // to one node.
        let symbol = format!("unresolved#{}:{name}", referenced_from_id.hash_suffix(),);
        let id = NodeId::new(NodeKind::UnresolvedSymbol, repo, file_path, &symbol)
            .map_err(UnresolvedSymbolConstructionError::Id)?;
        Ok(UnresolvedSymbol {
            id,
            name: name.into(),
            expected_kind,
            referenced_from_id,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn expected_kind(&self) -> Option<NodeKind> {
        self.expected_kind
    }
    pub fn referenced_from_id(&self) -> &NodeId {
        &self.referenced_from_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnresolvedSymbolConstructionError {
    EmptyName,
    EmptyFilePath,
    Id(crate::NodeIdConstructionError),
}

impl std::fmt::Display for UnresolvedSymbolConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyName => f.write_str("UnresolvedSymbol: name must not be empty"),
            Self::EmptyFilePath => f.write_str("UnresolvedSymbol: file_path must not be empty"),
            Self::Id(e) => write!(f, "UnresolvedSymbol: ID construction failed: {e}"),
        }
    }
}

impl std::error::Error for UnresolvedSymbolConstructionError {}
