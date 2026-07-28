use std::collections::BTreeMap;

use crate::model::intern::InternedStr;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum SurfaceKind {
    BehaviorTestSurface,
    CliSurface,
    CredentialOperation,
    JobSurface,
    MiddlewareInstallation,
    ProxySurface,
    QueueSurface,
    RouteSurface,
    WebhookSurface,
    WorkerSurface,
}

impl SurfaceKind {
    pub const fn label(self) -> &'static str {
        match self {
            SurfaceKind::BehaviorTestSurface => "behavior_test_surface",
            SurfaceKind::CliSurface => "cli_surface",
            SurfaceKind::CredentialOperation => "credential_operation",
            SurfaceKind::JobSurface => "job_surface",
            SurfaceKind::MiddlewareInstallation => "middleware_installation",
            SurfaceKind::ProxySurface => "proxy_surface",
            SurfaceKind::QueueSurface => "queue_surface",
            SurfaceKind::RouteSurface => "route_surface",
            SurfaceKind::WebhookSurface => "webhook_surface",
            SurfaceKind::WorkerSurface => "worker_surface",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub struct SurfaceNode {
    pub id: InternedStr,
    pub kind: SurfaceKind,
    pub name: InternedStr,
    pub file_id: InternedStr,
    pub file_path: InternedStr,
    pub area_id: Option<InternedStr>,
    pub language: InternedStr,
    pub line: usize,
    pub detail: InternedStr,
    pub metadata: BTreeMap<String, String>,
}

impl SurfaceNode {
    pub fn display(&self) -> String {
        format!("{}::{}", self.file_path, self.name)
    }
}
