use std::collections::BTreeSet;

use crate::map::RepositoryMap;
use crate::store::redb::graph_store::{
    GraphStoreError, OverviewV2Limits, ReadOnlyGraphStore, StoredNodeKind,
};

pub fn dependency_frontier(map: &RepositoryMap, target: &str) -> Vec<String> {
    let seeds = map.matching_target_ids(target);
    let mut frontier = BTreeSet::new();
    for edge in &map.edges {
        if seeds
            .iter()
            .any(|seed| seed == &edge.from || edge.from.ends_with(seed) || edge.to.ends_with(seed))
            && !seeds.iter().any(|seed| seed == &edge.to)
        {
            frontier.insert(map.display_for(&edge.to));
        }
    }
    frontier.into_iter().collect()
}

pub fn impact_frontier(map: &RepositoryMap, target: &str) -> Vec<String> {
    let seeds = map.matching_target_ids(target);
    let mut frontier = BTreeSet::new();
    for edge in &map.edges {
        if seeds
            .iter()
            .any(|seed| seed == &edge.to || edge.to.ends_with(seed) || edge.from.ends_with(seed))
            && !seeds.iter().any(|seed| seed == &edge.from)
        {
            frontier.insert(map.display_for(&edge.from));
        }
    }
    frontier.into_iter().collect()
}

pub fn dependency_frontier_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<Vec<String>, GraphStoreError> {
    let seeds = matching_target_ids_redb(store, target)?;
    let mut frontier = BTreeSet::new();
    for edge in store.all_edges()? {
        if seeds.iter().any(|seed| {
            seed == edge.from.as_str()
                || edge.from.as_str().ends_with(seed)
                || edge.to.as_str().ends_with(seed)
        }) && !seeds.iter().any(|seed| seed == edge.to.as_str())
        {
            frontier.insert(display_for_redb_id(store, edge.to.as_str())?);
        }
    }
    Ok(frontier.into_iter().collect())
}

pub fn impact_frontier_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<Vec<String>, GraphStoreError> {
    let seeds = matching_target_ids_redb(store, target)?;
    let mut frontier = BTreeSet::new();
    for edge in store.all_edges()? {
        if seeds.iter().any(|seed| {
            seed == edge.to.as_str()
                || edge.to.as_str().ends_with(seed)
                || edge.from.as_str().ends_with(seed)
        }) && !seeds.iter().any(|seed| seed == edge.from.as_str())
        {
            frontier.insert(display_for_redb_id(store, edge.from.as_str())?);
        }
    }
    Ok(frontier.into_iter().collect())
}

pub(crate) fn matching_target_ids_redb(
    store: &ReadOnlyGraphStore,
    target: &str,
) -> Result<Vec<String>, GraphStoreError> {
    let target = target.trim();
    let mut ids = Vec::new();
    if target.is_empty() {
        return Ok(ids);
    }

    if store.node_display(target)?.is_some() {
        push_unique_string(&mut ids, target.to_string());
    }
    if let Some(file) = store.resolve_file_path(target)? {
        push_unique_string(&mut ids, file.id);
    }
    for area in store.list_areas(None)? {
        if area.id == target
            || area.name.eq_ignore_ascii_case(target)
            || area.path_prefix.eq_ignore_ascii_case(target)
        {
            push_unique_string(&mut ids, area.id);
        }
    }
    let overview = store.overview_v2(OverviewV2Limits {
        area_limit: usize::MAX,
        directory_limit: usize::MAX,
        entrypoint_limit: usize::MAX,
        risk_limit: usize::MAX,
        file_limit: usize::MAX,
        function_limit: usize::MAX,
        class_limit: usize::MAX,
        doc_limit: usize::MAX,
        config_limit: usize::MAX,
        surface_limit: usize::MAX,
        unresolved_limit: usize::MAX,
    })?;
    for directory in overview.directories {
        if directory.id == target
            || directory.path.eq_ignore_ascii_case(target)
            || directory.name.eq_ignore_ascii_case(target)
        {
            push_unique_string(&mut ids, directory.id);
        }
    }
    for class in overview.classes {
        if class.id.as_str() == target
            || class.name.as_str().eq_ignore_ascii_case(target)
            || class.qualified_name.as_str().eq_ignore_ascii_case(target)
        {
            push_unique_string(&mut ids, class.id.to_string());
        }
    }
    for function in overview.functions {
        if function.id.as_str() == target
            || function.name.as_str().eq_ignore_ascii_case(target)
            || function
                .qualified_name
                .as_str()
                .eq_ignore_ascii_case(target)
        {
            push_unique_string(&mut ids, function.id.to_string());
        }
    }
    for doc in overview.docs {
        if doc.id == target
            || doc.path.eq_ignore_ascii_case(target)
            || doc.title.eq_ignore_ascii_case(target)
        {
            push_unique_string(&mut ids, doc.id);
        }
    }
    for config in overview.configs {
        if config.id == target || config.path.eq_ignore_ascii_case(target) {
            push_unique_string(&mut ids, config.id);
        }
    }
    for unresolved in overview.unresolved {
        if unresolved.id.as_str() == target || unresolved.name.as_str().eq_ignore_ascii_case(target)
        {
            push_unique_string(&mut ids, unresolved.id.to_string());
        }
    }
    for file in overview.files {
        if file.id == target
            || file.path.eq_ignore_ascii_case(target)
            || file.name.eq_ignore_ascii_case(target)
        {
            push_unique_string(&mut ids, file.id);
        }
    }
    for node in store.nodes_under_path(target)? {
        if node.kind() == StoredNodeKind::Unresolved {
            continue;
        }
        if node.id() == target
            || node
                .path()
                .map(|path| path.eq_ignore_ascii_case(target))
                .unwrap_or(false)
        {
            push_unique_string(&mut ids, node.id().to_string());
        }
    }
    if let Some((path, name)) = target.rsplit_once("::") {
        for symbol in store.find_symbols(name, None)? {
            if symbol.path.eq_ignore_ascii_case(path)
                || format!("{}::{}", symbol.path, symbol.name).eq_ignore_ascii_case(target)
            {
                push_unique_string(&mut ids, symbol.id);
            }
        }
    }
    if ids.is_empty() {
        for symbol in store.find_symbols(target, None)? {
            push_unique_string(&mut ids, symbol.id);
        }
    }
    if ids.is_empty() {
        ids.push(target.to_string());
    }
    Ok(ids)
}

pub(crate) fn display_for_redb_id(
    store: &ReadOnlyGraphStore,
    id: &str,
) -> Result<String, GraphStoreError> {
    Ok(match store.node_display(id)? {
        Some(node)
            if matches!(
                node.kind,
                StoredNodeKind::Directory | StoredNodeKind::Repository
            ) =>
        {
            id.to_string()
        }
        Some(node) => node.display,
        None => id.to_string(),
    })
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}
