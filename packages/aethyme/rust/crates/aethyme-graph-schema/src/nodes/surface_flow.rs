//! Surface/Flow graph nodes.
//!
//! These nodes model behavior-level system boundaries and credential flow
//! facts: routes, workers, proxies, middleware installation, credential
//! operations, and live-behavior tests. The concrete kind is carried by the
//! surrounding [`crate::Node`] variant; this payload holds the common fields.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{InvalidSourceRange, NodeId, NodeIdConstructionError, NodeKind, SourceRange};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceFlowNode {
    id: NodeId,
    path: Box<str>,
    name: Box<str>,
    detail: Box<str>,
    source_range: SourceRange,
    metadata: BTreeMap<Box<str>, Box<str>>,
}

impl SurfaceFlowNode {
    pub fn new(
        kind: NodeKind,
        repo: &str,
        file_path: &str,
        name: &str,
        detail: &str,
        source_range: SourceRange,
        metadata: BTreeMap<Box<str>, Box<str>>,
    ) -> Result<Self, SurfaceFlowNodeConstructionError> {
        if !is_surface_flow_kind(kind) {
            return Err(SurfaceFlowNodeConstructionError::WrongKind { kind });
        }
        if file_path.is_empty() {
            return Err(SurfaceFlowNodeConstructionError::EmptyPath);
        }
        if name.trim().is_empty() {
            return Err(SurfaceFlowNodeConstructionError::EmptyName);
        }
        let id = NodeId::new(kind, repo, file_path, name)
            .map_err(SurfaceFlowNodeConstructionError::Id)?;
        Ok(Self {
            id,
            path: file_path.into(),
            name: name.into(),
            detail: detail.into(),
            source_range,
            metadata,
        })
    }

    pub fn id(&self) -> &NodeId {
        &self.id
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn detail(&self) -> &str {
        &self.detail
    }

    pub fn source_range(&self) -> SourceRange {
        self.source_range
    }

    pub fn metadata(&self) -> &BTreeMap<Box<str>, Box<str>> {
        &self.metadata
    }
}

pub const fn is_surface_flow_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::BehaviorTestSurface
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SurfaceFlowNodeConstructionError {
    WrongKind { kind: NodeKind },
    EmptyPath,
    EmptyName,
    Id(NodeIdConstructionError),
    Range(InvalidSourceRange),
}

impl std::fmt::Display for SurfaceFlowNodeConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongKind { kind } => {
                write!(
                    f,
                    "SurfaceFlowNode: {kind:?} is not a Surface/Flow node kind"
                )
            }
            Self::EmptyPath => f.write_str("SurfaceFlowNode: path must not be empty"),
            Self::EmptyName => f.write_str("SurfaceFlowNode: name must not be empty"),
            Self::Id(e) => write!(f, "SurfaceFlowNode id: {e}"),
            Self::Range(e) => write!(f, "SurfaceFlowNode range: {e}"),
        }
    }
}

impl std::error::Error for SurfaceFlowNodeConstructionError {}
