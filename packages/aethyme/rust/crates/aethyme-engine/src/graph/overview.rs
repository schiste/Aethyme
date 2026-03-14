use crate::context_pack::{DependencyEdge, ImpactItem, RepoOverview};
use crate::model::edge::EdgeKind;
use crate::model::file::FileRole;
use crate::map::RepositoryMap;
use crate::graph::signals::evaluate_graph_signals;

pub fn repo_overview_seed(map: &RepositoryMap) -> Vec<String> {
    let mut seed = Vec::new();
    for doc in &map.docs {
        if matches!(doc.doc_type.as_str(), "readme" | "architecture") && !seed.contains(&doc.path) {
            seed.push(doc.path.clone());
        }
    }
    for area in map.areas.iter().filter(|area| !area.inferred) {
        if !seed.contains(&area.name) {
            seed.push(area.name.clone());
        }
    }
    for function in map.functions.iter().filter(|function| function.name == "main") {
        if !seed.contains(&function.file_path) {
            seed.push(function.file_path.clone());
        }
    }
    for config in &map.configs {
        if !seed.contains(&config.path) {
            seed.push(config.path.clone());
        }
    }
    seed
}

pub fn build_repo_overview(map: &RepositoryMap, navigation_order: &[String]) -> RepoOverview {
    let signals = evaluate_graph_signals(map);
    let code_area_limit = if signals.boundary_clarity.score < 55 { 2 } else { 3 };
    let reference_area_limit = if signals.parser_visibility.score < 60 { 3 } else { 2 };
    let entrypoint_limit = if signals.entrypoint_clarity.score >= 70 { 3 } else { 1 };
    let key_config_limit = if signals.config_hygiene.score >= 70 { 3 } else { 2 };
    let overview_docs = map
        .docs
        .iter()
        .filter(|doc| doc.doc_type == "readme" || doc.doc_type == "architecture")
        .map(|doc| doc.path.clone())
        .take(3)
        .collect::<Vec<_>>();

    let mut area_candidates = map
        .areas
        .iter()
        .filter(|area| !area.inferred && !area.name.starts_with('.'))
        .map(|area| {
            let score = area_score(map, area.id.as_str(), area.name.as_str());
            let profile = area_profile(map, area.id.as_str());
            (score, profile, area.name.clone())
        })
        .filter(|(_, _, name)| {
            navigation_order.iter().any(|item| item == name)
                || overview_docs.iter().any(|path| path.starts_with(&format!("{name}/")))
        })
        .collect::<Vec<_>>();
    area_candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.code_bearing.cmp(&left.1.code_bearing))
            .then_with(|| left.2.cmp(&right.2))
    });
    let code_areas = select_top_areas(area_candidates.clone(), code_area_limit, true);
    let reference_areas = select_top_areas(area_candidates, reference_area_limit, false);

    let mut subarea_candidates = map
        .areas
        .iter()
        .filter(|area| area.inferred)
        .map(|area| {
            let score = subarea_score(map, area.id.as_str(), area.name.as_str());
            (score, area.name.clone())
        })
        .filter(|(_, name)| {
            code_areas
                .iter()
                .any(|area| name.starts_with(&format!("{area}/")) || name == area)
        })
        .collect::<Vec<_>>();
    subarea_candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let subareas = subarea_candidates
        .into_iter()
        .take(4)
        .map(|(_, name)| name)
        .collect::<Vec<_>>();

    let mut entrypoints = map
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::EntrypointFor))
        .filter_map(|edge| {
            let display = map.display_for(&edge.to);
            if display.ends_with(".py")
                || display.ends_with(".rs")
                || display.ends_with(".ts")
                || display.ends_with(".js")
                || display.ends_with(".gd")
            {
                Some((edge.confidence, display))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    entrypoints.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    let entrypoints = entrypoints
        .into_iter()
        .map(|(_, display)| display)
        .fold(Vec::new(), |mut acc, item| {
            if !acc.contains(&item) && acc.len() < entrypoint_limit {
                acc.push(item);
            }
            acc
        });

    let mut key_configs = map
        .configs
        .iter()
        .filter(|config| {
            if code_areas.is_empty() {
                return config_is_overview_eligible(config.path.as_str(), config.config_type.as_str());
            }
            config_is_overview_eligible(config.path.as_str(), config.config_type.as_str())
                && config
                .area_id
                .as_deref()
                .map(|area_id| code_areas.iter().any(|area| area_id.ends_with(area)))
                .unwrap_or(false)
        })
        .map(|config| {
            let score = config_score(map, config.id.as_str(), config.path.as_str(), config.config_type.as_str());
            (score, config.config_type.clone(), config.path.clone())
        })
        .collect::<Vec<_>>();
    key_configs.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.2.cmp(&right.2)));
    let mut key_config_paths = Vec::new();
    let mut seen_families = std::collections::BTreeSet::<String>::new();
    let mut seen_paths = std::collections::BTreeSet::<String>::new();
    for (_, config_type, path) in key_configs {
        let family = config_family_key(config_type.as_str(), path.as_str());
        if seen_families.contains(&family) || seen_paths.contains(&path) {
            continue;
        }
        seen_families.insert(family);
        seen_paths.insert(path.clone());
        key_config_paths.push(path);
        if key_config_paths.len() == key_config_limit {
            break;
        }
    }
    let key_configs = key_config_paths;

    let representative_code_files = navigation_order
        .iter()
        .filter(|item| is_code_like_path(item))
        .take(5)
        .cloned()
        .collect::<Vec<_>>();
    let representative_docs = navigation_order
        .iter()
        .filter(|item| is_doc_like_path(item))
        .take(5)
        .cloned()
        .collect::<Vec<_>>();

    RepoOverview {
        overview_docs,
        code_areas,
        reference_areas,
        subareas,
        entrypoints,
        key_configs,
        representative_code_files,
        representative_docs,
    }
}

pub fn overview_dependencies(map: &RepositoryMap, overview: &RepoOverview) -> Vec<DependencyEdge> {
    let mut edges = Vec::new();
    let focus = overview
        .code_areas
        .iter()
        .chain(overview.entrypoints.iter())
        .chain(overview.representative_code_files.iter())
        .cloned()
        .collect::<Vec<_>>();

    for focus_item in focus {
        for source_id in map.matching_target_ids(&focus_item) {
            for edge in &map.edges {
                if edge.from != source_id {
                    continue;
                }
                if !matches!(edge.kind, EdgeKind::Contains | EdgeKind::Defines | EdgeKind::EntrypointFor | EdgeKind::Configures) {
                    continue;
                }
                let target = map.display_for(&edge.to);
                if target.starts_with("doc:") || target.starts_with("repo:") {
                    continue;
                }
                edges.push(DependencyEdge {
                    from: map.display_for(&edge.from),
                    to: target,
                    kind: edge_kind_label(&edge.kind).to_string(),
                });
            }
        }
    }

    edges.sort();
    edges.dedup();
    edges.truncate(6);
    edges
}

pub fn overview_impact(_map: &RepositoryMap, overview: &RepoOverview) -> Vec<ImpactItem> {
    overview
        .entrypoints
        .iter()
        .take(2)
        .map(|entry| ImpactItem {
            symbol: entry.clone(),
            file: entry.clone(),
            reason: "entrypoint candidate".to_string(),
        })
        .collect()
}

fn edge_kind_label(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::Contains => "contains",
        EdgeKind::BelongsTo => "belongs_to",
        EdgeKind::Defines => "defines",
        EdgeKind::Imports => "imports",
        EdgeKind::Calls => "calls",
        EdgeKind::References => "references",
        EdgeKind::Documents => "documents",
        EdgeKind::Configures => "configures",
        EdgeKind::EntrypointFor => "entrypoint_for",
    }
}

fn area_score(map: &RepositoryMap, area_id: &str, area_name: &str) -> i32 {
    let files = map
        .files
        .iter()
        .filter(|file| file.area_id.as_deref() == Some(area_id))
        .collect::<Vec<_>>();
    let source_count = files.iter().filter(|file| matches!(file.role, FileRole::Source)).count() as i32;
    let config_count = files.iter().filter(|file| matches!(file.role, FileRole::Config)).count() as i32;
    let doc_count = files.iter().filter(|file| matches!(file.role, FileRole::Doc)).count() as i32;
    let asset_count = files.iter().filter(|file| matches!(file.role, FileRole::Asset)).count() as i32;
    let test_count = files.iter().filter(|file| matches!(file.role, FileRole::Test)).count() as i32;
    let unknown_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Unknown | FileRole::Generated | FileRole::Cache | FileRole::Binary))
        .count() as i32;

    let function_count = map
        .functions
        .iter()
        .filter(|function| function.area_id.as_deref() == Some(area_id))
        .count() as i32;

    let class_count = map
        .classes
        .iter()
        .filter(|class| class.area_id.as_deref() == Some(area_id))
        .count() as i32;

    let architecture_docs = map
        .docs
        .iter()
        .filter(|doc| doc.area_id.as_deref() == Some(area_id))
        .filter(|doc| doc.doc_type == "architecture")
        .count() as i32;
    let readme_docs = map
        .docs
        .iter()
        .filter(|doc| doc.area_id.as_deref() == Some(area_id))
        .filter(|doc| doc.doc_type == "readme")
        .count() as i32;

    let config_score = map.configs.iter().filter(|config| config.area_id.as_deref() == Some(area_id)).map(|config| config_weight(config.config_type.as_str())).sum::<i32>();

    let entrypoint_score = map
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::EntrypointFor))
        .filter(|edge| {
            map.area_id_for_target(&edge.from).as_deref() == Some(area_id)
                || map.area_id_for_target(&edge.to).as_deref() == Some(area_id)
        })
        .count() as i32
        * 16;

    let code_presence_bonus = if source_count > 0 || function_count > 0 || class_count > 0 || config_count > 0 {
        40
    } else {
        0
    };
    let doc_score = std::cmp::min(doc_count, 4) * 2 + architecture_docs * 6 + readme_docs * 4;
    let unknown_penalty = if source_count == 0 && function_count == 0 && class_count == 0 {
        unknown_count * 2
    } else {
        unknown_count
    };
    let docs_bonus = overview_doc_bonus(map, area_name);

    code_presence_bonus
        + source_count * 10
        + function_count * 20
        + class_count * 24
        + config_score
        + entrypoint_score
        + doc_score
        + asset_count
        + test_count * 2
        + docs_bonus
        - unknown_penalty
}

fn subarea_score(map: &RepositoryMap, area_id: &str, area_name: &str) -> i32 {
    let files = map
        .files
        .iter()
        .filter(|file| file.path.starts_with(&format!("{area_name}/")))
        .collect::<Vec<_>>();
    let source_count = files.iter().filter(|file| matches!(file.role, FileRole::Source)).count() as i32;
    let config_count = files.iter().filter(|file| matches!(file.role, FileRole::Config)).count() as i32;
    let doc_count = files.iter().filter(|file| matches!(file.role, FileRole::Doc)).count() as i32;
    let unknown_count = files
        .iter()
        .filter(|file| matches!(file.role, FileRole::Unknown | FileRole::Generated | FileRole::Cache | FileRole::Binary))
        .count() as i32;

    let symbol_score = map
        .functions
        .iter()
        .filter(|function| function.area_id.as_deref() == Some(area_id))
        .count() as i32
        * 12
        + map
            .classes
            .iter()
            .filter(|class| class.area_id.as_deref() == Some(area_id))
            .count() as i32
            * 14;

    let config_score = map
        .configs
        .iter()
        .filter(|config| config.path.starts_with(&format!("{area_name}/")))
        .map(|config| config_weight(config.config_type.as_str()))
        .sum::<i32>();

    source_count * 8 + config_count * 6 + symbol_score + config_score + std::cmp::min(doc_count, 3) * 2 - unknown_count
}

fn config_score(map: &RepositoryMap, config_id: &str, config_path: &str, config_type: &str) -> i32 {
    let mut entrypoint_code_targets = 0;
    let mut entrypoint_area_targets = 0;
    let mut configures_code_targets = 0;
    let mut configures_area_targets = 0;

    for edge in map.edges.iter().filter(|edge| edge.from == config_id) {
        let target_is_code_artifact = map.files.iter().any(|file| file.id == edge.to && matches!(file.role, FileRole::Source | FileRole::Test))
            || map.functions.iter().any(|function| function.id == edge.to)
            || map.classes.iter().any(|class| class.id == edge.to);
        let target_is_area = map.areas.iter().any(|area| area.id == edge.to);

        match edge.kind {
            EdgeKind::EntrypointFor if target_is_code_artifact => entrypoint_code_targets += 1,
            EdgeKind::EntrypointFor if target_is_area => entrypoint_area_targets += 1,
            EdgeKind::Configures if target_is_code_artifact => configures_code_targets += 1,
            EdgeKind::Configures if target_is_area => configures_area_targets += 1,
            _ => {}
        }
    }

    let relationship_score = entrypoint_code_targets.min(2) * 28
        + entrypoint_area_targets.min(1) * 6
        + configures_code_targets.min(3) * 10
        + configures_area_targets.min(1) * 4;

    let path_bonus = if config_path.ends_with("Cargo.toml") {
        18
    } else if config_path.ends_with("package.json") {
        14
    } else if config_path.ends_with("project.godot") {
        8
    } else {
        0
    };

    relationship_score + config_weight(config_type) + path_bonus
}

fn config_weight(config_type: &str) -> i32 {
    match config_type {
        "manifest" => 12,
        "project" => 10,
        "runtime" => 8,
        "toml" => 6,
        "yaml" => 5,
        "config" => 4,
        "json" => 2,
        _ => 1,
    }
}

fn config_family_key(config_type: &str, path: &str) -> String {
    let basename = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    format!("{config_type}:{basename}")
}

fn config_is_overview_eligible(path: &str, config_type: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    if lowered.contains("autosave") || lowered.contains("/cache/") || lowered.contains("/tmp/") {
        return false;
    }

    match config_type {
        "manifest" | "project" | "runtime" | "toml" | "yaml" | "config" => true,
        "json" => {
            let basename = lowered.rsplit('/').next().unwrap_or(lowered.as_str());
            basename.contains("config")
                || basename.contains("settings")
                || basename.contains("package")
                || basename.contains("manifest")
                || basename.contains("project")
                || basename.contains("schema")
                || basename.contains("template")
        }
        _ => false,
    }
}

fn overview_doc_bonus(map: &RepositoryMap, area_name: &str) -> i32 {
    map.docs
        .iter()
        .filter(|doc| doc.path.starts_with(&format!("{area_name}/")))
        .filter(|doc| matches!(doc.doc_type.as_str(), "readme" | "architecture"))
        .count() as i32
        * 6
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AreaProfile {
    code_bearing: bool,
}

fn area_profile(map: &RepositoryMap, area_id: &str) -> AreaProfile {
    let source_count = map
        .files
        .iter()
        .filter(|file| file.area_id.as_deref() == Some(area_id))
        .filter(|file| matches!(file.role, FileRole::Source))
        .count();
    let function_count = map
        .functions
        .iter()
        .filter(|function| function.area_id.as_deref() == Some(area_id))
        .count();
    let class_count = map
        .classes
        .iter()
        .filter(|class| class.area_id.as_deref() == Some(area_id))
        .count();
    let entrypoint_count = map
        .edges
        .iter()
        .filter(|edge| matches!(edge.kind, EdgeKind::EntrypointFor))
        .filter(|edge| {
            map.area_id_for_target(&edge.from).as_deref() == Some(area_id)
                || map.area_id_for_target(&edge.to).as_deref() == Some(area_id)
        })
        .count();
    AreaProfile {
        code_bearing: source_count > 0
            || function_count > 0
            || class_count > 0
            || entrypoint_count > 0,
    }
}

fn select_top_areas(candidates: Vec<(i32, AreaProfile, String)>, limit: usize, code_bearing: bool) -> Vec<String> {
    let matching_count = candidates
        .iter()
        .filter(|(_, profile, _)| profile.code_bearing == code_bearing)
        .count();
    let prefer_matching = matching_count >= limit;

    let mut selected = Vec::new();
    if prefer_matching {
        for (_, profile, name) in &candidates {
            if profile.code_bearing == code_bearing && !selected.contains(name) {
                selected.push(name.clone());
                if selected.len() == limit {
                    return selected;
                }
            }
        }
    }

    if !code_bearing {
        return selected;
    }

    for (_, _, name) in candidates {
        if !selected.contains(&name) {
            selected.push(name);
            if selected.len() == limit {
                break;
            }
        }
    }
    selected
}

fn is_code_like_path(path: &str) -> bool {
    path.ends_with(".py")
        || path.ends_with(".rs")
        || path.ends_with(".ts")
        || path.ends_with(".tsx")
        || path.ends_with(".js")
        || path.ends_with(".jsx")
        || path.ends_with(".gd")
        || path.ends_with(".go")
}

fn is_doc_like_path(path: &str) -> bool {
    path.ends_with(".md")
        || path.ends_with(".mdx")
        || path.contains("documentation/")
        || path.contains("/docs/")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{build_repo_overview, repo_overview_seed};
    use crate::map::RepositoryMap;

    #[test]
    fn overview_key_configs_prefers_diverse_roles_over_duplicate_project_files() {
        let root = std::env::temp_dir().join("aethyme_engine_overview_config_diversity");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("godot")).expect("create godot dir");
        fs::create_dir_all(root.join("GameEngine/godot")).expect("create nested godot dir");
        fs::create_dir_all(root.join("GameEngine/rust/addgame/src")).expect("create rust dir");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");
        fs::write(root.join("godot/project.godot"), "[application]\nrun/main_scene=\"res://main.tscn\"\n").expect("write root godot project");
        fs::write(root.join("GameEngine/godot/project.godot"), "[application]\nrun/main_scene=\"res://editor.tscn\"\n").expect("write nested godot project");
        fs::write(root.join("GameEngine/rust/addgame/Cargo.toml"), "[package]\nname=\"demo\"\n").expect("write cargo manifest");
        fs::write(root.join("GameEngine/rust/addgame/src/lib.rs"), "pub fn main() {}\n").expect("write rust source");

        let map = RepositoryMap::build(&root).expect("build repository map");
        let overview = build_repo_overview(&map, &repo_overview_seed(&map));

        assert!(overview.key_configs.contains(&"GameEngine/rust/addgame/Cargo.toml".to_string()));
        assert_eq!(
            overview
                .key_configs
                .iter()
                .filter(|path| path.ends_with("project.godot"))
                .count(),
            1
        );

        let _ = fs::remove_dir_all(&root);
    }
}
