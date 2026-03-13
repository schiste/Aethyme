use std::collections::{BTreeMap, BTreeSet};

use crate::edge::EdgeKind;
use crate::file::FileRole;
use crate::map::RepositoryMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalAssessment {
    pub score: u16,
    pub level: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphSignals {
    pub boundary_clarity: SignalAssessment,
    pub entrypoint_clarity: SignalAssessment,
    pub config_hygiene: SignalAssessment,
    pub hidden_coupling: SignalAssessment,
    pub parser_visibility: SignalAssessment,
}

pub fn evaluate_graph_signals(map: &RepositoryMap) -> GraphSignals {
    GraphSignals {
        boundary_clarity: boundary_clarity(map),
        entrypoint_clarity: entrypoint_clarity(map),
        config_hygiene: config_hygiene(map),
        hidden_coupling: hidden_coupling(map),
        parser_visibility: parser_visibility(map),
    }
}

fn boundary_clarity(map: &RepositoryMap) -> SignalAssessment {
    let semantic_edges = map
        .edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::Imports | EdgeKind::Calls | EdgeKind::References | EdgeKind::Configures | EdgeKind::Documents
            )
        })
        .collect::<Vec<_>>();
    let cross_area = semantic_edges
        .iter()
        .filter(|edge| {
            let from = map.area_id_for_target(&edge.from);
            let to = map.area_id_for_target(&edge.to);
            matches!((from, to), (Some(left), Some(right)) if left != right)
        })
        .count();
    let source_files = map
        .files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Source | FileRole::Test))
        .collect::<Vec<_>>();
    let assigned_sources = source_files.iter().filter(|file| file.area_id.is_some()).count();
    let generic_source_names = source_files
        .iter()
        .filter(|file| is_generic_name(file.name.as_str()))
        .count();

    let semantic_total = semantic_edges.len();
    let cross_penalty = ratio_scaled(cross_area, semantic_total, 45);
    let generic_penalty = ratio_scaled(generic_source_names, source_files.len(), 20);
    let assignment_bonus = ratio_scaled(assigned_sources, source_files.len(), 15);
    let score = clamp_score(60 + assignment_bonus - cross_penalty - generic_penalty);

    SignalAssessment {
        score,
        level: signal_level(score).to_string(),
        evidence: vec![
            format!("cross-area semantic edges: {cross_area}/{semantic_total}"),
            format!("source files with area assignment: {assigned_sources}/{}", source_files.len()),
            format!("generic source file names: {generic_source_names}"),
        ],
    }
}

fn entrypoint_clarity(map: &RepositoryMap) -> SignalAssessment {
    let entrypoint_edges = map
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::EntrypointFor))
        .filter(|edge| {
            let target = map.display_for(&edge.to);
            is_code_path(target.as_str())
        })
        .collect::<Vec<_>>();
    let configs_with_entrypoints = entrypoint_edges
        .iter()
        .filter_map(|edge| config_path(map, &edge.from))
        .collect::<BTreeSet<_>>();

    let mut entrypoints_by_area = BTreeMap::<String, BTreeSet<String>>::new();
    for edge in &entrypoint_edges {
        if let Some(config_path) = config_path(map, &edge.from) {
            if let Some(area_id) = map.area_id_for_target(&edge.from) {
                entrypoints_by_area
                    .entry(area_id)
                    .or_default()
                    .insert(config_path);
            }
        }
    }
    let ambiguous_areas = entrypoints_by_area
        .values()
        .filter(|configs| configs.len() > 1)
        .count();
    let score = clamp_score(
        30 + (entrypoint_edges.len() as i32 * 18) + (configs_with_entrypoints.len() as i32 * 10)
            - (ambiguous_areas as i32 * 12),
    );

    SignalAssessment {
        score,
        level: signal_level(score).to_string(),
        evidence: vec![
            format!("direct code entrypoint edges: {}", entrypoint_edges.len()),
            format!("configs with entrypoints: {}", configs_with_entrypoints.len()),
            format!("areas with ambiguous entrypoints: {ambiguous_areas}"),
        ],
    }
}

fn config_hygiene(map: &RepositoryMap) -> SignalAssessment {
    let operational_configs = map
        .configs
        .iter()
        .filter(|config| is_operational_config(config.path.as_str(), config.config_type.as_str()))
        .collect::<Vec<_>>();
    let linked_configs = operational_configs
        .iter()
        .filter(|config| {
            config.area_id.is_some()
                || map
                    .edges
                    .iter()
                    .any(|edge| edge.from == config.id && matches!(edge.kind, EdgeKind::Configures | EdgeKind::EntrypointFor))
        })
        .count();

    let mut families_by_area = BTreeMap::<String, BTreeSet<String>>::new();
    for config in &operational_configs {
        let area_key = config.area_id.clone().unwrap_or_else(|| "repo".to_string());
        families_by_area
            .entry(area_key)
            .or_default()
            .insert(config_family_key(config.config_type.as_str(), config.path.as_str()));
    }
    let duplicate_families = operational_configs.len()
        .saturating_sub(families_by_area.values().map(|families| families.len()).sum::<usize>());
    let noisy_configs = map.configs.len().saturating_sub(operational_configs.len());

    let linked_bonus = ratio_scaled(linked_configs, operational_configs.len(), 35);
    let duplicate_penalty = (duplicate_families.min(5) as i32) * 8;
    let noisy_penalty = (noisy_configs.min(8) as i32) * 3;
    let score = clamp_score(50 + linked_bonus - duplicate_penalty - noisy_penalty);

    SignalAssessment {
        score,
        level: signal_level(score).to_string(),
        evidence: vec![
            format!("operational configs: {}", operational_configs.len()),
            format!("linked configs: {linked_configs}/{}", operational_configs.len()),
            format!("duplicate config families: {duplicate_families}"),
        ],
    }
}

fn hidden_coupling(map: &RepositoryMap) -> SignalAssessment {
    let semantic_edges = map
        .edges
        .iter()
        .filter(|edge| {
            matches!(
                edge.kind,
                EdgeKind::Calls | EdgeKind::References | EdgeKind::Configures | EdgeKind::EntrypointFor
            )
        })
        .collect::<Vec<_>>();
    let low_confidence = semantic_edges.iter().filter(|edge| edge.confidence < 800).count();
    let high_confidence = semantic_edges.iter().filter(|edge| edge.confidence >= 900).count();
    let cross_area = semantic_edges
        .iter()
        .filter(|edge| {
            let from = map.area_id_for_target(&edge.from);
            let to = map.area_id_for_target(&edge.to);
            matches!((from, to), (Some(left), Some(right)) if left != right)
        })
        .count();

    let low_penalty = ratio_scaled(low_confidence, semantic_edges.len(), 45);
    let cross_penalty = ratio_scaled(cross_area, semantic_edges.len(), 20);
    let high_bonus = ratio_scaled(high_confidence, semantic_edges.len(), 15);
    let score = clamp_score(65 + high_bonus - low_penalty - cross_penalty);

    SignalAssessment {
        score,
        level: signal_level(score).to_string(),
        evidence: vec![
            format!("low-confidence semantic edges: {low_confidence}/{}", semantic_edges.len()),
            format!("high-confidence semantic edges: {high_confidence}/{}", semantic_edges.len()),
            format!("cross-area semantic edges: {cross_area}/{}", semantic_edges.len()),
        ],
    }
}

fn parser_visibility(map: &RepositoryMap) -> SignalAssessment {
    let source_files = map
        .files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Source | FileRole::Test))
        .collect::<Vec<_>>();
    let supported_sources = source_files
        .iter()
        .filter(|file| {
            file.language
                .as_deref()
                .map(is_supported_language)
                .unwrap_or(false)
        })
        .count();
    let semantic_files = source_files
        .iter()
        .filter(|file| {
            map.functions.iter().any(|function| function.file_id == file.id)
                || map.classes.iter().any(|class| class.file_id == file.id)
                || map
                    .edges
                    .iter()
                    .any(|edge| edge.from == file.id && matches!(edge.kind, EdgeKind::Imports | EdgeKind::Defines))
        })
        .count();

    let support_bonus = ratio_scaled(supported_sources, source_files.len(), 55);
    let semantic_bonus = ratio_scaled(semantic_files, source_files.len(), 35);
    let score = clamp_score(10 + support_bonus + semantic_bonus);

    SignalAssessment {
        score,
        level: signal_level(score).to_string(),
        evidence: vec![
            format!("supported source files: {supported_sources}/{}", source_files.len()),
            format!("source files with semantic extraction: {semantic_files}/{}", source_files.len()),
            format!("total extracted functions/classes: {}", map.functions.len() + map.classes.len()),
        ],
    }
}

fn config_path(map: &RepositoryMap, id: &str) -> Option<String> {
    map.configs
        .iter()
        .find(|config| config.id == id)
        .map(|config| config.path.clone())
}

fn is_generic_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .to_ascii_lowercase();
    matches!(stem.as_str(), "utils" | "helpers" | "common" | "misc" | "temp")
}

fn is_supported_language(language: &str) -> bool {
    matches!(
        language,
        "python"
            | "rust"
            | "typescript"
            | "javascript"
            | "php"
            | "go"
            | "java"
            | "kotlin"
            | "ruby"
            | "c_sharp"
            | "c"
            | "cpp"
            | "swift"
            | "scala"
            | "elixir"
    )
}

fn is_code_path(path: &str) -> bool {
    path.ends_with(".py")
        || path.ends_with(".rs")
        || path.ends_with(".ts")
        || path.ends_with(".js")
        || path.ends_with(".tsx")
        || path.ends_with(".jsx")
        || path.ends_with(".php")
        || path.ends_with(".go")
        || path.ends_with(".java")
        || path.ends_with(".kt")
        || path.ends_with(".rb")
        || path.ends_with(".cs")
        || path.ends_with(".c")
        || path.ends_with(".cpp")
        || path.ends_with(".swift")
        || path.ends_with(".scala")
        || path.ends_with(".ex")
        || path.ends_with(".gd")
}

fn is_operational_config(path: &str, config_type: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    if lowered.contains("autosave") || lowered.contains("backup") || lowered.contains("/cache/") {
        return false;
    }
    matches!(
        config_type,
        "manifest" | "project" | "runtime" | "build" | "cargo" | "package" | "godot_project"
    ) || lowered.ends_with("cargo.toml")
        || lowered.ends_with("package.json")
        || lowered.ends_with("project.godot")
        || lowered.ends_with(".toml")
        || lowered.ends_with(".yaml")
        || lowered.ends_with(".yml")
}

fn config_family_key(config_type: &str, path: &str) -> String {
    let lowered = path.to_ascii_lowercase();
    if lowered.ends_with("cargo.toml") {
        return "cargo".to_string();
    }
    if lowered.ends_with("package.json") {
        return "package".to_string();
    }
    if lowered.ends_with("project.godot") {
        return "godot_project".to_string();
    }
    if config_type.is_empty() {
        path.rsplit('.').next().unwrap_or("config").to_string()
    } else {
        config_type.to_string()
    }
}

fn ratio_scaled(numerator: usize, denominator: usize, scale: i32) -> i32 {
    if denominator == 0 {
        return 0;
    }
    ((numerator as f32 / denominator as f32) * scale as f32).round() as i32
}

fn clamp_score(score: i32) -> u16 {
    score.clamp(0, 100) as u16
}

fn signal_level(score: u16) -> &'static str {
    match score {
        80..=100 => "strong",
        55..=79 => "mixed",
        _ => "weak",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::map::RepositoryMap;

    use super::evaluate_graph_signals;

    #[test]
    fn evaluate_graph_signals_returns_all_metrics() {
        let root = std::env::temp_dir().join("aethyme_engine_signals_test");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/core")).expect("create repo");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(root.join("Cargo.toml"), "[package]\nname='demo'\n").expect("write cargo");
        fs::write(
            root.join("src/core/main.py"),
            "def main():\n    return helper()\n\ndef helper():\n    return 1\n",
        )
        .expect("write source");

        let map = RepositoryMap::build(&root).expect("build map");
        let signals = evaluate_graph_signals(&map);

        assert!(signals.boundary_clarity.score <= 100);
        assert!(!signals.entrypoint_clarity.evidence.is_empty());
        assert!(!signals.config_hygiene.evidence.is_empty());
        assert!(!signals.hidden_coupling.evidence.is_empty());
        assert!(!signals.parser_visibility.evidence.is_empty());
    }
}
