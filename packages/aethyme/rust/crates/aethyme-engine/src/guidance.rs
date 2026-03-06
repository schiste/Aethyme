use std::collections::BTreeSet;

use crate::context_pack::{Anchor, AnchorKind};
use crate::map::RepositoryMap;
use crate::scope::{ScopeBoundary, ScopeItem, ScopeKind};
use crate::task::TaskKind;

pub fn build_in_scope(map: &RepositoryMap, anchors: &[Anchor], max_files: usize) -> ScopeBoundary {
    let mut boundary = ScopeBoundary::default();
    let mut files = Vec::new();

    for anchor in anchors {
        match anchor.kind {
            AnchorKind::File | AnchorKind::Symbol => {
                if let Some(file) = anchor.file.as_ref().or_else(|| file_for_symbol(map, &anchor.id)) {
                    if !files.contains(file) {
                        files.push(file.clone());
                    }
                }
            }
            AnchorKind::Folder => {
                let item = ScopeItem::new(anchor.id.clone(), ScopeKind::Folder, "primary top-level area");
                if !boundary.areas.contains(&item) {
                    boundary.areas.push(item);
                }
            }
        }
    }

    for file in files.into_iter().take(max_files) {
        boundary.files.push(ScopeItem::new(file.clone(), ScopeKind::File, "anchor-adjacent file"));
        for function in map.functions.iter().filter(|function| function.file_path == file) {
            boundary.symbols.push(ScopeItem::new(
                format!("{}::{}", function.file_path, function.name),
                ScopeKind::Symbol,
                "function defined in in-scope file",
            ));
        }
        for class in map.classes.iter().filter(|class| class.file_path == file) {
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

pub fn build_out_of_scope(map: &RepositoryMap, anchors: &[Anchor], task_kind: &TaskKind) -> ScopeBoundary {
    let mut boundary = ScopeBoundary::default();
    if matches!(task_kind, TaskKind::ExplainRepo) {
        return boundary;
    }
    let anchor_files: BTreeSet<String> = anchors
        .iter()
        .filter_map(|anchor| anchor.file.clone().or_else(|| file_for_symbol(map, &anchor.id).cloned()))
        .collect();

    for risk in &map.risk_flags {
        let high_risk_anchor = anchor_files.iter().any(|file| risk.scope == *file);
        if !high_risk_anchor {
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

fn file_for_symbol<'a>(map: &'a RepositoryMap, symbol_id: &str) -> Option<&'a String> {
    map.functions
        .iter()
        .find(|function| function.id == symbol_id)
        .map(|function| &function.file_path)
        .or_else(|| map.classes.iter().find(|class| class.id == symbol_id).map(|class| &class.file_path))
}
