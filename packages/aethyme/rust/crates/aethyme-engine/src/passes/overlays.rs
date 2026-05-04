use std::collections::{HashMap, HashSet};

use crate::map::RepositoryMap;
use crate::model::edge::EdgeKind;
use crate::model::graph::GraphAnnotation;
use crate::model::risk::{RiskArea, RiskFlag, RiskLevel};

const SHARED_CORE_HIGH_THRESHOLD: usize = 5;
const SHARED_CORE_LOW_THRESHOLD: usize = 3;

const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "delete", "drop", "destroy", "remove", "truncate", "reset", "purge", "wipe",
];

pub fn detect_risks(map: &RepositoryMap) -> Vec<RiskFlag> {
    let mut risks = Vec::new();
    for file in &map.files {
        risks.extend(path_risks(&file.path));
    }
    risks.extend(shared_core_risks(map));
    risks.extend(destructive_risks(map));
    risks.sort();
    risks.dedup();
    risks
}

fn shared_core_risks(map: &RepositoryMap) -> Vec<RiskFlag> {
    let mut importers: HashMap<&str, HashSet<&str>> = HashMap::new();
    for edge in &map.edges {
        if matches!(edge.kind, EdgeKind::Imports) && edge.to.starts_with("file:") {
            importers.entry(&edge.to).or_default().insert(&edge.from);
        }
    }

    let file_id_to_path: HashMap<&str, &str> = map
        .files
        .iter()
        .map(|file| (file.id.as_str(), file.path.as_str()))
        .collect();

    let mut risks = Vec::new();
    for (file_id, from_set) in &importers {
        let in_degree = from_set.len();
        if in_degree < SHARED_CORE_LOW_THRESHOLD {
            continue;
        }
        let path = file_id_to_path.get(file_id).copied().unwrap_or(file_id);
        if in_degree >= SHARED_CORE_HIGH_THRESHOLD {
            risks.push(RiskFlag::new(
                path,
                RiskArea::SharedCore,
                RiskLevel::Medium,
                format!("imported by {in_degree} files (shared core)"),
            ));
        } else {
            risks.push(RiskFlag::new(
                path,
                RiskArea::SharedCore,
                RiskLevel::Low,
                format!("imported by {in_degree} files (moderate coupling)"),
            ));
        }
    }
    risks
}

fn destructive_risks(map: &RepositoryMap) -> Vec<RiskFlag> {
    let mut matches_by_file: HashMap<&str, Vec<&str>> = HashMap::new();
    for function in &map.functions {
        let lower = function.name.to_ascii_lowercase();
        if DESTRUCTIVE_PATTERNS
            .iter()
            .any(|pattern| lower.contains(pattern))
        {
            matches_by_file
                .entry(&function.file_path)
                .or_default()
                .push(&function.name);
        }
    }

    let mut risks = Vec::new();
    for (file_path, names) in &matches_by_file {
        risks.push(RiskFlag::new(
            *file_path,
            RiskArea::Destructive,
            RiskLevel::Medium,
            format!("destructive operations: {}", names.join(", ")),
        ));
    }
    risks
}

pub fn graph_annotations(map: &RepositoryMap) -> Vec<GraphAnnotation> {
    let mut annotations = Vec::new();
    let file_ids_by_path = map
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.id.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    annotations.reserve(map.risk_flags.len() + map.docs.len() + map.configs.len());
    for risk in &map.risk_flags {
        if let Some(file_id) = file_ids_by_path.get(risk.scope.as_str()) {
            let (confidence, level_str) = match risk.level {
                RiskLevel::High => (1000, "high"),
                RiskLevel::Medium => (800, "medium"),
                RiskLevel::Low => (600, "low"),
            };
            annotations.push(GraphAnnotation {
                target_id: file_id.clone(),
                kind: "risk".to_string(),
                value: format!("{:?}", risk.area).to_ascii_lowercase(),
                confidence,
                source: "risk-overlay".to_string(),
                reason: format!("{} ({})", risk.reason, level_str),
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
                target_id: edge.from.to_string(),
                kind: "navigation".to_string(),
                value: "entrypoint".to_string(),
                confidence: edge.confidence,
                source: edge.source.to_string(),
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
        risks.push(RiskFlag::new(
            path,
            RiskArea::Auth,
            RiskLevel::High,
            "authentication boundary",
        ));
    }
    if lower.contains("permission") || lower.contains("rbac") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Permissions,
            RiskLevel::High,
            "permission boundary",
        ));
    }
    if lower.contains("secret") || lower.contains("token") || lower.contains("credential") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Secrets,
            RiskLevel::High,
            "sensitive credential surface",
        ));
    }
    if lower.contains("migration") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Migrations,
            RiskLevel::High,
            "schema change area",
        ));
    }
    if lower.contains("deploy") || lower.contains("infra") || lower.contains("terraform") {
        let level = if lower.contains("helper") || lower.contains("util") {
            RiskLevel::Low
        } else {
            RiskLevel::Medium
        };
        risks.push(RiskFlag::new(
            path,
            RiskArea::Infra,
            level,
            "infrastructure surface",
        ));
    }
    if lower.contains("billing") || lower.contains("invoice") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Billing,
            RiskLevel::Medium,
            "billing logic",
        ));
    }
    risks
}
