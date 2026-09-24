//! Node decoding and lookup by id, plus node display projection.

use super::*;

pub(super) fn area_depth(area: &AreaNode) -> u32 {
    (area.path_prefix.matches('/').count() + 1) as u32
}

pub(super) fn node_kind_from_id(id: &str) -> Option<StoredNodeKind> {
    if id.starts_with("repo:") {
        Some(StoredNodeKind::Repository)
    } else if id.starts_with("dir:") {
        Some(StoredNodeKind::Directory)
    } else if id.starts_with("file:") {
        Some(StoredNodeKind::File)
    } else if id.starts_with("area:") {
        Some(StoredNodeKind::Area)
    } else if id.starts_with("fn:") {
        Some(StoredNodeKind::Function)
    } else if id.starts_with("class:") {
        Some(StoredNodeKind::Class)
    } else if id.starts_with("doc:") {
        Some(StoredNodeKind::Doc)
    } else if id.starts_with("config:") {
        Some(StoredNodeKind::Config)
    } else if id.starts_with("behavior_test_surface:") {
        Some(StoredNodeKind::BehaviorTestSurface)
    } else if id.starts_with("cli_surface:") {
        Some(StoredNodeKind::CliSurface)
    } else if id.starts_with("credential_operation:") {
        Some(StoredNodeKind::CredentialOperation)
    } else if id.starts_with("job_surface:") {
        Some(StoredNodeKind::JobSurface)
    } else if id.starts_with("middleware_installation:") {
        Some(StoredNodeKind::MiddlewareInstallation)
    } else if id.starts_with("proxy_surface:") {
        Some(StoredNodeKind::ProxySurface)
    } else if id.starts_with("queue_surface:") {
        Some(StoredNodeKind::QueueSurface)
    } else if id.starts_with("route_surface:") {
        Some(StoredNodeKind::RouteSurface)
    } else if id.starts_with("webhook_surface:") {
        Some(StoredNodeKind::WebhookSurface)
    } else if id.starts_with("worker_surface:") {
        Some(StoredNodeKind::WorkerSurface)
    } else if id.starts_with("unresolved_symbol:") || id.starts_with("import:") {
        Some(StoredNodeKind::Unresolved)
    } else {
        None
    }
}

pub(super) fn read_table_node<T: for<'de> Deserialize<'de>>(
    txn: &ReadTransaction,
    table: TableDefinition<&str, &[u8]>,
    id: &str,
) -> Result<Option<T>, GraphStoreError> {
    let t = txn.open_table(table)?;
    let Some(value) = t.get(id)? else {
        return Ok(None);
    };
    Ok(Some(bincode::deserialize(value.value())?))
}

pub(super) fn get_node_in_txn(
    txn: &ReadTransaction,
    id: &str,
) -> Result<Option<StoredNode>, GraphStoreError> {
    let Some(kind) = node_kind_from_id(id) else {
        return Ok(None);
    };
    match kind {
        StoredNodeKind::Repository => {
            Ok(read_table_node::<RepositoryNode>(txn, REPOSITORIES, id)?
                .map(StoredNode::Repository))
        }
        StoredNodeKind::Directory => {
            Ok(read_table_node::<DirectoryNode>(txn, DIRECTORIES, id)?.map(StoredNode::Directory))
        }
        StoredNodeKind::File => {
            Ok(read_table_node::<FileNode>(txn, FILES, id)?.map(StoredNode::File))
        }
        StoredNodeKind::Area => {
            Ok(read_table_node::<AreaNode>(txn, AREAS, id)?.map(StoredNode::Area))
        }
        StoredNodeKind::Function => {
            Ok(read_table_node::<FunctionNode>(txn, FUNCTIONS, id)?.map(StoredNode::Function))
        }
        StoredNodeKind::Class => {
            Ok(read_table_node::<ClassNode>(txn, CLASSES, id)?.map(StoredNode::Class))
        }
        StoredNodeKind::Doc => Ok(read_table_node::<DocNode>(txn, DOCS, id)?.map(StoredNode::Doc)),
        StoredNodeKind::Config => {
            Ok(read_table_node::<ConfigNode>(txn, CONFIGS, id)?.map(StoredNode::Config))
        }
        StoredNodeKind::BehaviorTestSurface
        | StoredNodeKind::CliSurface
        | StoredNodeKind::CredentialOperation
        | StoredNodeKind::JobSurface
        | StoredNodeKind::MiddlewareInstallation
        | StoredNodeKind::ProxySurface
        | StoredNodeKind::QueueSurface
        | StoredNodeKind::RouteSurface
        | StoredNodeKind::WebhookSurface
        | StoredNodeKind::WorkerSurface => {
            Ok(read_table_node::<SurfaceNode>(txn, SURFACES, id)?.map(StoredNode::Surface))
        }
        StoredNodeKind::Unresolved => {
            Ok(read_table_node::<UnresolvedNode>(txn, UNRESOLVED, id)?.map(StoredNode::Unresolved))
        }
    }
}

pub(super) fn get_node_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Option<StoredNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    get_node_in_txn(&txn, id)
}

pub(super) fn get_nodes_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    ids: &[S],
) -> Result<Vec<StoredNode>, GraphStoreError> {
    let txn = db.begin_read()?;
    let mut out = Vec::new();
    for id in ids {
        if let Some(node) = get_node_in_txn(&txn, id.as_ref())? {
            out.push(node);
        }
    }
    Ok(out)
}

pub(super) fn node_display_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Option<NodeDisplay>, GraphStoreError> {
    let txn = db.begin_read()?;
    display_from_id_in_txn(&txn, id)
}

pub(super) fn area_id_from_node(node: &StoredNode) -> Option<String> {
    match node {
        StoredNode::Repository(_) => None,
        StoredNode::Directory(node) => node.area_id.clone(),
        StoredNode::File(node) => node.area_id.clone(),
        StoredNode::Area(node) => Some(node.id.clone()),
        StoredNode::Function(node) => node.area_id.as_ref().map(|id| id.to_string()),
        StoredNode::Class(node) => node.area_id.as_ref().map(|id| id.to_string()),
        StoredNode::Doc(node) => node.area_id.clone(),
        StoredNode::Config(node) => node.area_id.clone(),
        StoredNode::Surface(node) => node.area_id.as_ref().map(|id| id.to_string()),
        StoredNode::Unresolved(node) => node.area_id.as_ref().map(|id| id.to_string()),
    }
}

pub(super) fn path_from_node(node: &StoredNode) -> Option<String> {
    node.path().map(str::to_string)
}

pub(super) fn area_for_node_from<D: ReadableDatabase>(
    db: &D,
    id_or_path: &str,
) -> Result<Option<String>, GraphStoreError> {
    let txn = db.begin_read()?;
    if let Some(node) = get_node_in_txn(&txn, id_or_path)? {
        return Ok(area_id_from_node(&node));
    }
    drop(txn);
    Ok(resolve_file_path_from(db, id_or_path)?.and_then(|file| file.area_id))
}

pub(super) fn symbol_lookup_from_node(node: StoredNode) -> Option<SymbolLookup> {
    match node {
        StoredNode::Function(function) => Some(SymbolLookup {
            id: function.id.to_string(),
            kind: StoredNodeKind::Function,
            name: function.name.to_string(),
            path: function.file_path.to_string(),
            line: function.line,
            signature: function.signature.to_string(),
            language: function.language.to_string(),
            area_id: function.area_id.map(|id| id.to_string()),
        }),
        StoredNode::Class(class) => Some(SymbolLookup {
            id: class.id.to_string(),
            kind: StoredNodeKind::Class,
            name: class.name.to_string(),
            path: class.file_path.to_string(),
            line: class.line,
            signature: class.signature.to_string(),
            language: class.language.to_string(),
            area_id: class.area_id.map(|id| id.to_string()),
        }),
        StoredNode::Surface(surface) => Some(SymbolLookup {
            id: surface.id.to_string(),
            kind: stored_kind_from_surface_kind(surface.kind),
            name: surface.name.to_string(),
            path: surface.file_path.to_string(),
            line: surface.line,
            signature: surface.detail.to_string(),
            language: surface.language.to_string(),
            area_id: surface.area_id.map(|id| id.to_string()),
        }),
        _ => None,
    }
}

pub(super) fn node_display_from_node(node: StoredNode) -> NodeDisplay {
    match node {
        StoredNode::Repository(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Repository,
            display: node.name.clone(),
            name: node.name,
            path: Some(node.root_path),
            language: None,
            area_id: None,
        },
        StoredNode::Directory(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Directory,
            display: node.path.clone(),
            name: node.name,
            path: Some(node.path),
            language: None,
            area_id: node.area_id,
        },
        StoredNode::File(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::File,
            display: node.path.clone(),
            name: node.name,
            path: Some(node.path),
            language: node.language,
            area_id: node.area_id,
        },
        StoredNode::Area(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Area,
            display: node.path_prefix.clone(),
            name: node.name,
            path: Some(node.path_prefix),
            language: None,
            area_id: None,
        },
        StoredNode::Function(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: StoredNodeKind::Function,
            display: node.qualified_name.to_string(),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
        StoredNode::Class(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: StoredNodeKind::Class,
            display: node.qualified_name.to_string(),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
        StoredNode::Doc(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Doc,
            display: node.path.clone(),
            name: node.title,
            path: Some(node.path),
            language: None,
            area_id: node.area_id,
        },
        StoredNode::Config(node) => NodeDisplay {
            id: node.id,
            kind: StoredNodeKind::Config,
            display: node.path.clone(),
            name: node.config_type,
            path: Some(node.path),
            language: None,
            area_id: node.area_id,
        },
        StoredNode::Surface(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: stored_kind_from_surface_kind(node.kind),
            display: node.display(),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
        StoredNode::Unresolved(node) => NodeDisplay {
            id: node.id.to_string(),
            kind: StoredNodeKind::Unresolved,
            display: format!("{}::{}", node.file_path, node.name),
            name: node.name.to_string(),
            path: Some(node.file_path.to_string()),
            language: Some(node.language.to_string()),
            area_id: node.area_id.map(|id| id.to_string()),
        },
    }
}

pub(super) fn display_from_id_in_txn(
    txn: &ReadTransaction,
    id: &str,
) -> Result<Option<NodeDisplay>, GraphStoreError> {
    Ok(get_node_in_txn(txn, id)?.map(node_display_from_node))
}
