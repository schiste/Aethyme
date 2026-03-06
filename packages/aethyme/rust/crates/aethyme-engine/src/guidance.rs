use std::collections::BTreeSet;

use crate::context_pack::Anchor;
use crate::map::RepositoryMap;
use crate::scope::{ScopeBoundary, ScopeItem, ScopeKind};
use crate::task::TaskKind;

pub fn build_in_scope(map: &RepositoryMap, anchors: &[Anchor], max_files: usize) -> ScopeBoundary {
    let mut boundary = ScopeBoundary::default();
    let mut files = BTreeSet::new();

    for anchor in anchors {
        if let Some(file) = &anchor.file {
            files.insert(file.clone());
        }
    }

    for file in files.into_iter().take(max_files) {
        boundary.files.push(ScopeItem::new(file, ScopeKind::File, "anchor-adjacent file"));
    }

    for symbol in map.symbols.iter().take(max_files) {
        if boundary.files.iter().any(|item| item.value == symbol.file) {
            boundary.symbols.push(ScopeItem::new(
                format!("{}::{}", symbol.file, symbol.name),
                ScopeKind::Symbol,
                "symbol defined in in-scope file",
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
    let anchor_files: BTreeSet<String> = anchors
        .iter()
        .filter_map(|anchor| anchor.file.clone())
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
    let mut order: Vec<String> = anchors
        .iter()
        .filter_map(|anchor| anchor.file.clone())
        .collect();
    order.sort();
    order.dedup();
    order
}
