//! Risks, repo metadata, edge counts, typed listings and overviews.

use super::*;

pub(super) fn risk_key_candidates(path: &str) -> BTreeSet<String> {
    let mut keys = BTreeSet::new();
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return keys;
    }
    keys.insert(trimmed.to_string());
    keys.insert(format!("{trimmed}/"));
    let mut current = trimmed;
    while let Some((parent, _)) = current.rsplit_once('/') {
        keys.insert(parent.to_string());
        keys.insert(format!("{parent}/"));
        current = parent;
    }
    keys
}

pub(super) fn risk_path_from<D: ReadableDatabase>(
    db: &D,
    id_or_path: &str,
) -> Result<String, GraphStoreError> {
    let txn = db.begin_read()?;
    if let Some(node) = get_node_in_txn(&txn, id_or_path)? {
        return Ok(path_from_node(&node).unwrap_or_else(|| id_or_path.to_string()));
    }
    Ok(id_or_path.to_string())
}

pub(super) fn risks_for_node_or_path_from<D: ReadableDatabase>(
    db: &D,
    id_or_path: &str,
) -> Result<Vec<RiskFlag>, GraphStoreError> {
    let path = risk_path_from(db, id_or_path)?;
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(RISK_FLAGS)?;
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for key in risk_key_candidates(&path) {
        for row in t.get(key.as_str())? {
            let risk: RiskFlag = bincode::deserialize(row?.value())?;
            if seen.insert((risk.scope.clone(), risk.reason.clone())) {
                out.push(risk);
            }
        }
    }
    let prefix = path.trim_matches('/');
    if !prefix.is_empty() {
        let end = prefix_end(prefix);
        for entry in t.range(prefix..end.as_str())? {
            let (key, values) = entry?;
            if !key.value().starts_with(prefix) {
                continue;
            }
            for value in values {
                let risk: RiskFlag = bincode::deserialize(value?.value())?;
                if seen.insert((risk.scope.clone(), risk.reason.clone())) {
                    out.push(risk);
                }
            }
            if out.len() >= 50 {
                break;
            }
        }
    }
    out.sort_by(|left, right| {
        right
            .level
            .cmp(&left.level)
            .then_with(|| left.scope.cmp(&right.scope))
            .then_with(|| left.reason.cmp(&right.reason))
    });
    out.truncate(50);
    Ok(out)
}

pub(super) fn repo_metadata_from<D: ReadableDatabase>(
    db: &D,
) -> Result<Option<RepoMetadata>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_table(META)?;
    let Some(value) = t.get(META_KEY_REPO_METADATA)? else {
        return Ok(None);
    };
    let meta: RepoMetadata = bincode::deserialize(value.value())?;
    Ok(Some(meta))
}

pub(super) fn collect_adjacency<D: ReadableDatabase>(
    db: &D,
    table: MultimapTableDefinition<&str, &[u8]>,
    key: &str,
) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = match txn.open_multimap_table(table) {
        Ok(t) => t,
        Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out = Vec::new();
    for r in t.get(key)? {
        let row = r?;
        let rec: AdjacencyRecord = bincode::deserialize(row.value())?;
        out.push(rec);
    }
    Ok(out)
}

pub(super) fn edge_count_from<D: ReadableDatabase>(db: &D) -> Result<u64, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(EDGES_OUT)?;
    let mut count = 0;
    for entry in t.iter()? {
        let (_, values) = entry?;
        for value in values {
            value?;
            count += 1;
        }
    }
    Ok(count)
}

pub(super) fn all_edges_from<D: ReadableDatabase>(db: &D) -> Result<Vec<Edge>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(EDGES_OUT)?;
    let mut edges = Vec::new();
    for entry in t.iter()? {
        let (key, values) = entry?;
        let from = key.value().to_string();
        for value in values {
            let row = value?;
            let rec: AdjacencyRecord = bincode::deserialize(row.value())?;
            edges.push(Edge::new(
                from.clone(),
                rec.other.as_str(),
                rec.kind,
                rec.confidence,
                rec.source.as_str(),
            ));
        }
    }
    edges.sort();
    Ok(edges)
}

pub(super) fn list_areas_from<D: ReadableDatabase>(
    db: &D,
    depth: Option<u32>,
) -> Result<Vec<AreaNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_table(AREAS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        let area: AreaNode = bincode::deserialize(value.value())?;
        if let Some(d) = depth
            && area_depth(&area) != d
        {
            continue;
        }
        out.push(area);
    }
    // Stable sort by depth then path_prefix so callers don't see redb's
    // iteration order (which is sorted-by-key but the keys are structured
    // IDs, not path_prefixes).
    out.sort_by(|a, b| {
        area_depth(a)
            .cmp(&area_depth(b))
            .then_with(|| a.path_prefix.cmp(&b.path_prefix))
    });
    Ok(out)
}

pub(super) fn overview_from<D: ReadableDatabase>(
    db: &D,
    area_limit: usize,
    entrypoint_limit: usize,
    risk_limit: usize,
) -> Result<Overview, GraphStoreError> {
    let repo = repo_metadata_from(db)?;

    // Top areas at depth 1, in path_prefix order.
    let mut areas = list_areas_from(db, Some(1))?;
    areas.truncate(area_limit);

    // Entrypoints: scan EDGES_OUT once, collect distinct sources whose
    // adjacency carries kind = EntrypointFor. O(E) but no per-file lookups.
    // Resolve src node_id → file path via FILES so the output matches what
    // surreal returned.
    let entrypoint_paths = {
        let txn = db.begin_read()?;
        let edges = txn.open_multimap_table(EDGES_OUT)?;
        let files = txn.open_table(FILES)?;
        let mut seen = std::collections::BTreeSet::new();
        let mut paths = Vec::new();
        'outer: for kv in edges.iter()? {
            let (key, values) = kv?;
            let src = key.value().to_string();
            let mut has_entrypoint = false;
            for v in values {
                let row = v?;
                let rec: AdjacencyRecord = bincode::deserialize(row.value())?;
                if matches!(rec.kind, EdgeKind::EntrypointFor) {
                    has_entrypoint = true;
                    break;
                }
            }
            if !has_entrypoint || !seen.insert(src.clone()) {
                continue;
            }
            let Some(blob) = files.get(src.as_str())? else {
                continue;
            };
            let file: FileNode = bincode::deserialize(blob.value())?;
            paths.push(file.path);
            if paths.len() >= entrypoint_limit {
                break 'outer;
            }
        }
        paths
    };

    // Risks: collect from RISK_FLAGS multimap, sort by level desc, truncate.
    let risks = {
        let txn = db.begin_read()?;
        let t = txn.open_multimap_table(RISK_FLAGS)?;
        let mut all = Vec::new();
        for kv in t.iter()? {
            let (_, values) = kv?;
            for v in values {
                let row = v?;
                let risk: RiskFlag = bincode::deserialize(row.value())?;
                all.push(risk);
            }
        }
        all.sort_by(|a, b| b.level.cmp(&a.level));
        all.truncate(risk_limit);
        all
    };

    Ok(Overview {
        repo,
        areas,
        entrypoint_paths,
        risks,
    })
}

pub(super) fn list_files_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<FileNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(FILES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<FileNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn list_functions_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<FunctionNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(FUNCTIONS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<FunctionNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn list_classes_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<ClassNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(CLASSES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<ClassNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn list_docs_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<DocNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(DOCS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<DocNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn list_configs_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<ConfigNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(CONFIGS)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<ConfigNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn list_surfaces_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<SurfaceNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(SURFACES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<SurfaceNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn read_repository_from<D: ReadableDatabase>(
    db: &D,
) -> Result<Option<RepositoryNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    let t = txn.open_table(REPOSITORIES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<RepositoryNode>(value.value())?);
    }
    out.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(out.into_iter().next())
}

pub(super) fn list_directories_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<DirectoryNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(DIRECTORIES)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<DirectoryNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn list_unresolved_from<D: ReadableDatabase>(
    db: &D,
    limit: usize,
) -> Result<Vec<UnresolvedNode>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_table(UNRESOLVED)?;
    let mut out = Vec::new();
    for entry in t.iter()? {
        let (_, value) = entry?;
        out.push(bincode::deserialize::<UnresolvedNode>(value.value())?);
    }
    out.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn overview_v2_from<D: ReadableDatabase>(
    db: &D,
    limits: OverviewV2Limits,
) -> Result<OverviewV2, GraphStoreError> {
    let overview = overview_from(
        db,
        limits.area_limit,
        limits.entrypoint_limit,
        limits.risk_limit,
    )?;
    Ok(OverviewV2 {
        repo: overview.repo,
        repository: read_repository_from(db)?,
        areas: overview.areas,
        directories: list_directories_from(db, limits.directory_limit)?,
        entrypoint_paths: overview.entrypoint_paths,
        risks: overview.risks,
        files: list_files_from(db, limits.file_limit)?,
        functions: list_functions_from(db, limits.function_limit)?,
        classes: list_classes_from(db, limits.class_limit)?,
        docs: list_docs_from(db, limits.doc_limit)?,
        configs: list_configs_from(db, limits.config_limit)?,
        surfaces: list_surfaces_from(db, limits.surface_limit)?,
        unresolved: list_unresolved_from(db, limits.unresolved_limit)?,
    })
}
