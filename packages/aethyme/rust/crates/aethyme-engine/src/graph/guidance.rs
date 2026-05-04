use std::collections::BTreeSet;

use crate::context_pack::{Anchor, AnchorKind};
use crate::map::RepositoryMap;
use crate::model::risk::RiskLevel;
use crate::model::scope::{ScopeBoundary, ScopeItem, ScopeKind};
use crate::model::task::TaskKind;

pub fn build_in_scope(map: &RepositoryMap, anchors: &[Anchor], max_files: usize) -> ScopeBoundary {
    let mut boundary = ScopeBoundary::default();
    let mut files: Vec<String> = Vec::new();
    let primary_areas = primary_area_names(map, anchors);
    let primary_area_set: BTreeSet<String> = primary_areas.iter().cloned().collect();

    for anchor in anchors {
        match anchor.kind {
            AnchorKind::File | AnchorKind::Symbol => {
                if let Some(file) = anchor
                    .file
                    .as_deref()
                    .or_else(|| file_for_symbol(map, &anchor.id))
                {
                    if !primary_area_set.is_empty()
                        && !file_in_primary_areas(map, file, &primary_area_set)
                    {
                        continue;
                    }
                    if !files.iter().any(|f| f == file) {
                        files.push(file.to_string());
                    }
                }
            }
            AnchorKind::Folder => {
                let area_name = map
                    .areas
                    .iter()
                    .find(|area| area.id == anchor.id || area.name == anchor.id)
                    .map(|area| area.name.clone())
                    .unwrap_or_else(|| anchor.id.clone());
                push_unique_area(&mut boundary, area_name, "primary top-level area");
            }
        }
    }

    for area in &primary_areas {
        push_unique_area(&mut boundary, area.clone(), "primary top-level area");
    }

    for file in files.into_iter().take(max_files) {
        boundary.files.push(ScopeItem::new(
            file.clone(),
            ScopeKind::File,
            "anchor-adjacent file",
        ));
        for function in map
            .functions
            .iter()
            .filter(|function| function.file_path == file)
        {
            if !primary_area_set.is_empty()
                && !function
                    .area_id
                    .as_deref()
                    .and_then(|area_id| area_name(map, area_id))
                    .is_some_and(|area| primary_area_set.contains(&area))
            {
                continue;
            }
            boundary.symbols.push(ScopeItem::new(
                format!("{}::{}", function.file_path, function.name),
                ScopeKind::Symbol,
                "function defined in in-scope file",
            ));
        }
        for class in map.classes.iter().filter(|class| class.file_path == file) {
            if !primary_area_set.is_empty()
                && !class
                    .area_id
                    .as_deref()
                    .and_then(|area_id| area_name(map, area_id))
                    .is_some_and(|area| primary_area_set.contains(&area))
            {
                continue;
            }
            boundary.symbols.push(ScopeItem::new(
                format!("{}::{}", class.file_path, class.name),
                ScopeKind::Symbol,
                "class defined in in-scope file",
            ));
        }
    }

    boundary.sort();
    boundary
}

pub fn build_out_of_scope(
    map: &RepositoryMap,
    anchors: &[Anchor],
    task_kind: &TaskKind,
) -> ScopeBoundary {
    let mut boundary = ScopeBoundary::default();
    if matches!(task_kind, TaskKind::ExplainRepo) {
        return boundary;
    }
    let primary_areas = primary_area_names(map, anchors);
    if !primary_areas.is_empty() {
        for area in &map.areas {
            if !primary_areas.contains(&area.name) {
                boundary.areas.push(ScopeItem::new(
                    area.name.clone(),
                    ScopeKind::Area,
                    "outside the matched primary area",
                ));
            }
        }
    }
    let anchor_files: BTreeSet<String> = anchors
        .iter()
        .filter_map(|anchor| {
            anchor
                .file
                .clone()
                .or_else(|| file_for_symbol(map, &anchor.id).map(String::from))
        })
        .collect();

    for risk in &map.risk_flags {
        if matches!(risk.level, RiskLevel::Low) {
            continue;
        }
        let risk_anchor = anchor_files.iter().any(|file| risk.scope == *file);
        if !risk_anchor {
            boundary.areas.push(ScopeItem::new(
                risk.scope.clone(),
                ScopeKind::Area,
                format!("high-risk area: {}", risk.reason),
            ));
        }
    }

    boundary.sort();
    boundary
}

pub fn build_out_of_scope_activated(
    map: &RepositoryMap,
    anchors: &[Anchor],
    task_kind: &TaskKind,
    activated_set: &std::collections::HashSet<String>,
) -> ScopeBoundary {
    let mut boundary = ScopeBoundary::default();
    if matches!(task_kind, TaskKind::ExplainRepo) {
        return boundary;
    }

    let primary_areas = primary_area_names(map, anchors);

    // Only mark areas as out-of-scope if they have zero activated nodes
    if !primary_areas.is_empty() {
        for area in &map.areas {
            if primary_areas.contains(&area.name) {
                continue;
            }
            let area_has_activation = map
                .files
                .iter()
                .filter(|file| file.area_id.as_deref() == Some(area.id.as_str()))
                .any(|file| activated_set.contains(&file.id) || activated_set.contains(&file.path));

            if area_has_activation {
                // Partially activated — don't block it, but note caution
                boundary.areas.push(ScopeItem::new(
                    area.name.clone(),
                    ScopeKind::Area,
                    "partially activated — exercise caution",
                ));
            } else {
                boundary.areas.push(ScopeItem::new(
                    area.name.clone(),
                    ScopeKind::Area,
                    "outside the matched primary area",
                ));
            }
        }
    }

    // Only include risks on activated nodes
    let anchor_files: BTreeSet<String> = anchors
        .iter()
        .filter_map(|anchor| {
            anchor
                .file
                .clone()
                .or_else(|| file_for_symbol(map, &anchor.id).map(String::from))
        })
        .collect();

    for risk in &map.risk_flags {
        if matches!(risk.level, RiskLevel::Low) {
            continue;
        }
        let risk_anchor = anchor_files.iter().any(|file| risk.scope == *file);
        if risk_anchor {
            continue;
        }
        // Only include if the risk scope is on an activated node
        if activated_set.contains(&risk.scope) {
            boundary.areas.push(ScopeItem::new(
                risk.scope.clone(),
                ScopeKind::Area,
                format!("high-risk area: {}", risk.reason),
            ));
        }
    }

    boundary.sort();
    boundary
}

pub fn navigation_order(anchors: &[Anchor]) -> Vec<String> {
    let mut order = Vec::new();
    for anchor in anchors {
        let value = anchor.file.clone().unwrap_or_else(|| anchor.id.clone());
        if !order.contains(&value) {
            order.push(value);
        }
    }
    order
}

fn file_for_symbol<'a>(map: &'a RepositoryMap, symbol_id: &str) -> Option<&'a str> {
    map.functions
        .iter()
        .find(|function| function.id == symbol_id)
        .map(|function| function.file_path.as_str())
        .or_else(|| {
            map.classes
                .iter()
                .find(|class| class.id == symbol_id)
                .map(|class| class.file_path.as_str())
        })
}

fn primary_area_names(map: &RepositoryMap, anchors: &[Anchor]) -> Vec<String> {
    let folder_areas = anchors
        .iter()
        .filter_map(|anchor| match anchor.kind {
            AnchorKind::Folder => map
                .areas
                .iter()
                .find(|area| area.id == anchor.id || area.name == anchor.id)
                .map(|area| area.name.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if !folder_areas.is_empty() {
        let mut unique = Vec::new();
        for area in folder_areas {
            if !unique.contains(&area) {
                unique.push(area);
            }
        }
        return unique;
    }

    let mut areas = Vec::new();
    for anchor in anchors {
        match anchor.kind {
            AnchorKind::Folder => {}
            AnchorKind::File => {
                if let Some(area) =
                    file_area_name(map, anchor.file.as_deref().unwrap_or(&anchor.id))
                    && !areas.contains(&area)
                {
                    areas.push(area);
                }
            }
            AnchorKind::Symbol => {
                if let Some(file) = anchor
                    .file
                    .as_deref()
                    .or_else(|| file_for_symbol(map, &anchor.id))
                    && let Some(area) = file_area_name(map, file)
                    && !areas.contains(&area)
                {
                    areas.push(area);
                }
            }
        }
    }
    areas
}

fn file_in_primary_areas(
    map: &RepositoryMap,
    file_path: &str,
    primary_areas: &BTreeSet<String>,
) -> bool {
    file_area_name(map, file_path).is_some_and(|area| primary_areas.contains(&area))
}

fn file_area_name(map: &RepositoryMap, file_path: &str) -> Option<String> {
    map.files
        .iter()
        .find(|file| file.path == file_path)
        .and_then(|file| file.area_id.as_deref())
        .and_then(|area_id| area_name(map, area_id))
}

fn area_name(map: &RepositoryMap, area_id: &str) -> Option<String> {
    map.areas
        .iter()
        .find(|area| area.id == area_id)
        .map(|area| area.name.clone())
}

fn push_unique_area(boundary: &mut ScopeBoundary, value: String, reason: &str) {
    let item = ScopeItem::new(value, ScopeKind::Area, reason);
    if !boundary.areas.contains(&item) {
        boundary.areas.push(item);
    }
}
