//! Path-scoped reads, adjacency, relation views, and doc/config links.

use super::*;

pub(super) fn prefix_end(prefix: &str) -> String {
    format!("{prefix}\u{10ffff}")
}

pub(super) fn ids_under_path_from<D: ReadableDatabase>(
    db: &D,
    table: MultimapTableDefinition<&str, &str>,
    prefix: &str,
) -> Result<Vec<String>, GraphStoreError> {
    ids_under_path_limited_from(db, table, prefix, usize::MAX)
}

pub(super) fn ids_under_path_limited_from<D: ReadableDatabase>(
    db: &D,
    table: MultimapTableDefinition<&str, &str>,
    prefix: &str,
    limit: usize,
) -> Result<Vec<String>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(table)?;
    let end = prefix_end(prefix);
    let mut ids = BTreeSet::new();
    for entry in t.range(prefix..end.as_str())? {
        let (key, values) = entry?;
        if !key.value().starts_with(prefix) {
            continue;
        }
        for value in values {
            ids.insert(value?.value().to_string());
            if ids.len() >= limit {
                return Ok(ids.into_iter().collect());
            }
        }
    }
    Ok(ids.into_iter().collect())
}

pub(super) fn sort_nodes(nodes: &mut [StoredNode]) {
    nodes.sort_by(|left, right| {
        left.path()
            .unwrap_or("")
            .cmp(right.path().unwrap_or(""))
            .then_with(|| left.kind().cmp(&right.kind()))
            .then_with(|| left.id().cmp(right.id()))
    });
}

pub(super) fn nodes_under_path_from<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
) -> Result<Vec<StoredNode>, GraphStoreError> {
    let ids = ids_under_path_from(db, NODES_BY_PATH, prefix)?;
    let txn = db.begin_read()?;
    let mut nodes = Vec::new();
    for id in ids {
        if let Some(node) = get_node_in_txn(&txn, &id)? {
            nodes.push(node);
        }
    }
    sort_nodes(&mut nodes);
    Ok(nodes)
}

pub(super) fn functions_under_path_from<D: ReadableDatabase>(
    db: &D,
    prefix: &str,
) -> Result<Vec<FunctionNode>, GraphStoreError> {
    let ids = ids_under_path_from(db, FUNCTIONS_BY_PATH, prefix)?;
    let txn = db.begin_read()?;
    let mut functions = Vec::new();
    for id in ids {
        if let Some(function) = read_table_node::<FunctionNode>(&txn, FUNCTIONS, &id)? {
            functions.push(function);
        }
    }
    functions.sort_by(|left, right| {
        left.file_path
            .cmp(&right.file_path)
            .then_with(|| left.line.cmp(&right.line))
            .then_with(|| left.name.cmp(&right.name))
            .then_with(|| left.id.cmp(&right.id))
    });
    Ok(functions)
}

pub(super) fn function_ids_for_path_from<D: ReadableDatabase>(
    db: &D,
    path: &str,
    limit: usize,
) -> Result<BoundedFunctionIds, GraphStoreError> {
    let txn = db.begin_read()?;
    let table = txn.open_multimap_table(FUNCTIONS_BY_PATH)?;
    let mut ids = Vec::new();
    for row in table.get(path)? {
        if ids.len() == limit {
            return Ok(BoundedFunctionIds {
                ids,
                truncated: true,
            });
        }
        ids.push(row?.value().to_string());
    }
    Ok(BoundedFunctionIds {
        ids,
        truncated: false,
    })
}

pub(super) fn resolve_file_path_from<D: ReadableDatabase>(
    db: &D,
    path: &str,
) -> Result<Option<FileNode>, GraphStoreError> {
    let ids = {
        let txn = db.begin_read()?;
        let t = txn.open_multimap_table(NODES_BY_PATH)?;
        let mut ids = Vec::new();
        for row in t.get(path)? {
            ids.push(row?.value().to_string());
        }
        ids
    };
    let txn = db.begin_read()?;
    for id in ids {
        if let Some(file) = read_table_node::<FileNode>(&txn, FILES, &id)? {
            return Ok(Some(file));
        }
    }
    Ok(None)
}

pub(super) fn neighbors_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    direction: NeighborDirection,
    kind: Option<EdgeKind>,
) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
    let table = match direction {
        NeighborDirection::Outgoing => EDGES_OUT,
        NeighborDirection::Incoming => EDGES_IN,
    };
    let mut rows = collect_adjacency(db, table, id)?;
    if let Some(expected) = kind {
        rows.retain(|row| row.kind == expected);
    }
    rows.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.other.cmp(&right.other))
            .then_with(|| left.source.cmp(&right.source))
    });
    Ok(rows)
}

pub(super) fn edge_kind_label(kind: &EdgeKind) -> &'static str {
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
        EdgeKind::Authorizes => "authorizes",
        EdgeKind::Exposes => "exposes",
        EdgeKind::ForwardsTo => "forwards_to",
        EdgeKind::InstallsMiddleware => "installs_middleware",
        EdgeKind::IssuesCredential => "issues_credential",
        EdgeKind::StoresCredential => "stores_credential",
        EdgeKind::UsesCredential => "uses_credential",
        EdgeKind::ValidatesCredential => "validates_credential",
        EdgeKind::RewritesHeader => "rewrites_header",
        EdgeKind::TestedBy => "tested_by",
    }
}

pub(super) fn edges_by_kind_limited_from<D: ReadableDatabase>(
    db: &D,
    kind: EdgeKind,
    limit: usize,
) -> Result<Vec<Edge>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let txn = db.begin_read()?;
    let t = txn.open_multimap_table(EDGES_BY_KIND)?;
    let mut edges = Vec::new();
    for row in t.get(edge_kind_label(&kind))? {
        let edge: Edge = bincode::deserialize(row?.value())?;
        edges.push(edge);
        if edges.len() >= limit {
            break;
        }
    }
    edges.sort();
    edges.dedup();
    edges.truncate(limit);
    Ok(edges)
}

pub(super) fn relation_specs(relation: GraphRelation) -> Vec<(NeighborDirection, Vec<EdgeKind>)> {
    match relation {
        GraphRelation::Children => vec![(
            NeighborDirection::Outgoing,
            vec![
                EdgeKind::Contains,
                EdgeKind::Defines,
                EdgeKind::Authorizes,
                EdgeKind::Exposes,
                EdgeKind::ForwardsTo,
                EdgeKind::InstallsMiddleware,
                EdgeKind::IssuesCredential,
                EdgeKind::StoresCredential,
                EdgeKind::UsesCredential,
                EdgeKind::ValidatesCredential,
                EdgeKind::RewritesHeader,
                EdgeKind::TestedBy,
            ],
        )],
        GraphRelation::Parents => vec![(
            NeighborDirection::Incoming,
            vec![
                EdgeKind::Contains,
                EdgeKind::Defines,
                EdgeKind::BelongsTo,
                EdgeKind::Authorizes,
                EdgeKind::Exposes,
                EdgeKind::ForwardsTo,
                EdgeKind::InstallsMiddleware,
                EdgeKind::IssuesCredential,
                EdgeKind::StoresCredential,
                EdgeKind::UsesCredential,
                EdgeKind::ValidatesCredential,
                EdgeKind::RewritesHeader,
                EdgeKind::TestedBy,
            ],
        )],
        GraphRelation::Callers => vec![(NeighborDirection::Incoming, vec![EdgeKind::Calls])],
        GraphRelation::Callees => vec![(NeighborDirection::Outgoing, vec![EdgeKind::Calls])],
        GraphRelation::Docs => vec![
            (NeighborDirection::Outgoing, vec![EdgeKind::Documents]),
            (NeighborDirection::Incoming, vec![EdgeKind::Documents]),
        ],
        GraphRelation::Configs => vec![
            (
                NeighborDirection::Outgoing,
                vec![EdgeKind::Configures, EdgeKind::EntrypointFor],
            ),
            (
                NeighborDirection::Incoming,
                vec![EdgeKind::Configures, EdgeKind::EntrypointFor],
            ),
        ],
        GraphRelation::Imports => vec![(NeighborDirection::Outgoing, vec![EdgeKind::Imports])],
        GraphRelation::Importers => vec![(NeighborDirection::Incoming, vec![EdgeKind::Imports])],
        GraphRelation::References => vec![
            (
                NeighborDirection::Outgoing,
                vec![
                    EdgeKind::References,
                    EdgeKind::Authorizes,
                    EdgeKind::Exposes,
                    EdgeKind::ForwardsTo,
                    EdgeKind::InstallsMiddleware,
                    EdgeKind::IssuesCredential,
                    EdgeKind::RewritesHeader,
                    EdgeKind::StoresCredential,
                    EdgeKind::TestedBy,
                    EdgeKind::UsesCredential,
                    EdgeKind::ValidatesCredential,
                ],
            ),
            (
                NeighborDirection::Incoming,
                vec![
                    EdgeKind::References,
                    EdgeKind::Authorizes,
                    EdgeKind::Exposes,
                    EdgeKind::ForwardsTo,
                    EdgeKind::InstallsMiddleware,
                    EdgeKind::IssuesCredential,
                    EdgeKind::RewritesHeader,
                    EdgeKind::StoresCredential,
                    EdgeKind::TestedBy,
                    EdgeKind::UsesCredential,
                    EdgeKind::ValidatesCredential,
                ],
            ),
        ],
    }
}

pub(super) fn relation_items_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    relation: GraphRelation,
    kind_filter: Option<StoredNodeKind>,
) -> Result<Vec<RedbRelationItem>, GraphStoreError> {
    let mut adjacency = Vec::new();
    for (direction, kinds) in relation_specs(relation) {
        for kind in kinds {
            adjacency.extend(neighbors_from(db, id, direction, Some(kind))?);
        }
    }
    let txn = db.begin_read()?;
    let mut items = Vec::new();
    for edge in adjacency {
        let Some(node) = display_from_id_in_txn(&txn, edge.other.as_str())? else {
            continue;
        };
        if let Some(expected) = kind_filter
            && node.kind != expected
        {
            continue;
        }
        items.push(RedbRelationItem {
            relation: edge_kind_label(&edge.kind).to_string(),
            edge_kind: edge.kind,
            confidence: edge.confidence,
            source: edge.source.to_string(),
            node,
        });
    }
    items.sort_by(|left, right| {
        left.node
            .display
            .cmp(&right.node.display)
            .then_with(|| left.node.kind.cmp(&right.node.kind))
            .then_with(|| left.node.id.cmp(&right.node.id))
            .then_with(|| left.edge_kind.cmp(&right.edge_kind))
    });
    items.dedup();
    Ok(items)
}

pub(super) fn relation_view_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    relation: GraphRelation,
) -> Result<RedbRelationView, GraphStoreError> {
    Ok(RedbRelationView {
        target: node_display_from(db, id)?,
        relation,
        items: relation_items_from(db, id, relation, None)?,
    })
}

pub(super) fn children_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    kind_filter: Option<StoredNodeKind>,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    Ok(
        relation_items_from(db, id, GraphRelation::Children, kind_filter)?
            .into_iter()
            .map(|item| item.node)
            .collect(),
    )
}

pub(super) fn parents_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    kind_filter: Option<StoredNodeKind>,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    Ok(
        relation_items_from(db, id, GraphRelation::Parents, kind_filter)?
            .into_iter()
            .map(|item| item.node)
            .collect(),
    )
}

pub(super) fn docs_for_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Vec<DocNode>, GraphStoreError> {
    let ids = relation_items_from(db, id, GraphRelation::Docs, Some(StoredNodeKind::Doc))?
        .into_iter()
        .map(|item| item.node.id)
        .collect::<Vec<_>>();
    let txn = db.begin_read()?;
    let mut docs = Vec::new();
    for id in ids {
        if let Some(doc) = read_table_node::<DocNode>(&txn, DOCS, &id)? {
            docs.push(doc);
        }
    }
    docs.sort();
    docs.dedup();
    Ok(docs)
}

pub(super) fn configs_for_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Vec<ConfigNode>, GraphStoreError> {
    let ids = relation_items_from(db, id, GraphRelation::Configs, Some(StoredNodeKind::Config))?
        .into_iter()
        .map(|item| item.node.id)
        .collect::<Vec<_>>();
    let txn = db.begin_read()?;
    let mut configs = Vec::new();
    for id in ids {
        if let Some(config) = read_table_node::<ConfigNode>(&txn, CONFIGS, &id)? {
            configs.push(config);
        }
    }
    configs.sort();
    configs.dedup();
    Ok(configs)
}
