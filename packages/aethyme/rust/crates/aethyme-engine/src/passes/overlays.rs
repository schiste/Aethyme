use crate::edge::EdgeKind;
use crate::graph::GraphAnnotation;
use crate::map::RepositoryMap;
use crate::risk::{RiskArea, RiskFlag, RiskLevel};

pub fn detect_risks(map: &RepositoryMap) -> Vec<RiskFlag> {
    let mut risks = Vec::new();
    for file in &map.files {
        risks.extend(path_risks(&file.path));
    }
    risks.sort();
    risks.dedup();
    risks
}

pub fn graph_annotations(map: &RepositoryMap) -> Vec<GraphAnnotation> {
    let mut annotations = Vec::new();
    for risk in &map.risk_flags {
        if let Some(file) = map.files.iter().find(|file| file.path == risk.scope) {
            annotations.push(GraphAnnotation {
                target_id: file.id.clone(),
                kind: "risk".to_string(),
                value: format!("{:?}", risk.area).to_ascii_lowercase(),
                confidence: 1000,
                source: "risk-overlay".to_string(),
                reason: risk.reason.clone(),
            });
        }
    }
    for doc in &map.docs {
        annotations.push(GraphAnnotation {
            target_id: doc.id.clone(),
            kind: "doc_type".to_string(),
            value: doc.doc_type.clone(),
            confidence: 900,
            source: "docs".to_string(),
            reason: format!("documentation classified as {}", doc.doc_type),
        });
    }
    for config in &map.configs {
        annotations.push(GraphAnnotation {
            target_id: config.id.clone(),
            kind: "config_type".to_string(),
            value: config.config_type.clone(),
            confidence: 900,
            source: "config".to_string(),
            reason: format!("configuration classified as {}", config.config_type),
        });
    }
    for edge in &map.edges {
        if matches!(edge.kind, EdgeKind::EntrypointFor) {
            annotations.push(GraphAnnotation {
                target_id: edge.from.clone(),
                kind: "navigation".to_string(),
                value: "entrypoint".to_string(),
                confidence: edge.confidence,
                source: edge.source.clone(),
                reason: "edge inferred as navigation entrypoint".to_string(),
            });
        }
    }
    annotations.sort();
    annotations.dedup();
    annotations
}

fn path_risks(path: &str) -> Vec<RiskFlag> {
    let lower = path.to_ascii_lowercase();
    let mut risks = Vec::new();
    if lower.contains("auth") {
        risks.push(RiskFlag::new(path, RiskArea::Auth, RiskLevel::High, "authentication boundary"));
    }
    if lower.contains("permission") || lower.contains("rbac") {
        risks.push(RiskFlag::new(path, RiskArea::Permissions, RiskLevel::High, "permission boundary"));
    }
    if lower.contains("secret") || lower.contains("token") || lower.contains("credential") {
        risks.push(RiskFlag::new(path, RiskArea::Secrets, RiskLevel::High, "sensitive credential surface"));
    }
    if lower.contains("migration") {
        risks.push(RiskFlag::new(path, RiskArea::Migrations, RiskLevel::High, "schema change area"));
    }
    if lower.contains("deploy") || lower.contains("infra") || lower.contains("terraform") {
        risks.push(RiskFlag::new(path, RiskArea::Infra, RiskLevel::High, "infrastructure surface"));
    }
    if lower.contains("billing") || lower.contains("invoice") {
        risks.push(RiskFlag::new(path, RiskArea::Billing, RiskLevel::High, "billing logic"));
    }
    risks
}
