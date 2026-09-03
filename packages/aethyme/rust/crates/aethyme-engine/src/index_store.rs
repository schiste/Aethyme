//! Atomic materialization of committed graph fragments into the local redb store.
//!
//! The store is derived, disposable state. A complete store is built at the
//! staging path and becomes visible only after its metadata commit succeeds.

use std::path::Path;

use crate::map::RepositoryMap;
use crate::model::repository::RepositoryNode;
use crate::store::redb::graph_store::{self as gs, GraphStore, IndexDurability, RepoMetadata};

pub fn materialize_graph_store(
    repo_root: &Path,
    map: &RepositoryMap,
    source_commit: &str,
) -> Result<(), String> {
    let canonical = repo_root
        .canonicalize()
        .map_err(|error| format!("resolve graph store repository: {error}"))?;
    let store = GraphStore::reset_staging(&canonical).map_err(|error| error.to_string())?;
    let mut session = store
        .begin_index_with_durability(IndexDurability::None)
        .map_err(|error| error.to_string())?;

    let repo_name = map.snapshot.repo_name();
    let repository = RepositoryNode::new(&repo_name, &map.snapshot.root);
    gs::insert_repository(&mut session, &repository).map_err(|error| error.to_string())?;
    for area in &map.areas {
        gs::insert_area(&mut session, area).map_err(|error| error.to_string())?;
    }
    for directory in &map.directories {
        gs::insert_directory(&mut session, directory).map_err(|error| error.to_string())?;
    }
    for file in &map.files {
        gs::insert_file(&mut session, file).map_err(|error| error.to_string())?;
    }
    for class in &map.classes {
        gs::insert_class(&mut session, class).map_err(|error| error.to_string())?;
    }
    for function in &map.functions {
        gs::insert_function(&mut session, function).map_err(|error| error.to_string())?;
    }
    for surface in &map.surfaces {
        gs::insert_surface(&mut session, surface).map_err(|error| error.to_string())?;
    }
    for doc in &map.docs {
        gs::insert_doc(&mut session, doc).map_err(|error| error.to_string())?;
    }
    for config in &map.configs {
        gs::insert_config(&mut session, config).map_err(|error| error.to_string())?;
    }
    for unresolved in &map.unresolved {
        gs::insert_unresolved(&mut session, unresolved).map_err(|error| error.to_string())?;
    }
    for edge in &map.edges {
        gs::insert_edge(&mut session, edge).map_err(|error| error.to_string())?;
    }
    for risk in &map.risk_flags {
        gs::insert_risk(&mut session, risk).map_err(|error| error.to_string())?;
    }
    session.commit().map_err(|error| error.to_string())?;

    let indexed_at_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    store
        .set_repo_metadata(&RepoMetadata {
            root_path: canonical.to_string_lossy().into_owned(),
            commit_hash: Some(source_commit.to_string()),
            indexed_at_unix,
            file_count: map.files.len() as u64,
            languages: map.snapshot.languages.clone(),
        })
        .map_err(|error| error.to_string())?;
    drop(store);
    GraphStore::publish_staging(&canonical).map_err(|error| error.to_string())
}
