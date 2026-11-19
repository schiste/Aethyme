use super::*;
use aethyme_engine::store::redb::graph_store::{
    GraphRelation, GraphStore, ReadOnlyGraphStore, StoredNode,
};

/// What a session recorded about its own scope, and why more was not derived.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScopeCaptureReport {
    pub declared: usize,
    pub derived: usize,
    /// Present when nothing could be derived. Stated rather than implied, so
    /// "no scopes" is never mistaken for "no collisions possible".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_context: Option<TaskContext>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskContext {
    pub source_commit: String,
    pub paths: Vec<String>,
    pub symbols: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub source_anchors: Vec<TaskSourceAnchor>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub observed_baseline: Vec<TaskGateBaseline>,
    /// Gates that would select the suggested paths. Actual selection uses
    /// the full change and the policy of the candidate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggested_gates: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskSourceLocation {
    pub symbol: String,
    pub path: String,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskSourceAnchor {
    #[serde(flatten)]
    pub source: TaskSourceLocation,
    pub callers: Vec<TaskSourceLocation>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskGateBaseline {
    pub gate: String,
    pub run_id: i64,
    pub status: crate::GateStatus,
    pub observed_at_ms: i64,
}

impl TaskContext {
    const MAX_PATHS: usize = 8;
    pub(super) fn bound_for_output(&mut self) {
        self.symbols.truncate(8);
    }

    const MAX_GATES: usize = 8;
}

/// A graph can assist session startup only when it describes the session's
/// chosen commit, which may be newer than the primary checkout or stacked.
pub(super) fn derive_task_context(
    broker_store: &BrokerStore,
    main_root: &Path,
    worktree: &Path,
    commit: &str,
    task: &str,
) -> Result<TaskContext, String> {
    use aethyme_engine::graph::navigation::task_scope_with_anchors_redb;
    use aethyme_engine::model::task::TaskInput;
    let _span = crate::phase_telemetry::span("start_task_context");
    let warmed = super::context_warmup::cache_root(main_root);
    for root in [worktree, main_root, warmed.as_path()] {
        if !GraphStore::final_path(root).is_file() {
            continue;
        }
        let Ok(store) = GraphStore::open_read_only(root) else {
            continue;
        };
        if !matches!(store.repo_metadata(), Ok(Some(metadata)) if metadata.commit_hash.as_deref() == Some(commit))
        {
            continue;
        }
        let (view, anchors) =
            task_scope_with_anchors_redb(&store, &TaskInput::from_task_text(task))
                .map_err(|error| format!("task scope could not be resolved: {error}"))?;
        let paths: Vec<_> = view
            .in_scope_files
            .into_iter()
            .take(TaskContext::MAX_PATHS)
            .collect();
        let source_anchors = anchors
            .iter()
            .filter_map(|anchor| source_anchor(&store, &anchor.id))
            .take(4)
            .collect();
        let (suggested_gates, observed_baseline) =
            gate_hints(broker_store, worktree, commit, &paths)
                .map_or((None, Vec::new()), |(hints, baseline)| {
                    (Some(hints), baseline)
                });
        return Ok(TaskContext {
            source_commit: commit.to_owned(),
            paths,
            symbols: view.in_scope_symbols,
            source_anchors,
            observed_baseline,
            suggested_gates,
        });
    }
    Err("no readable graph store matches the session's recorded checkout commit; declare scopes explicitly to compare intent across sessions".into())
}

fn source_location(store: &ReadOnlyGraphStore, id: &str) -> Option<TaskSourceLocation> {
    let (symbol, path, line) = match store.get_node(id).ok().flatten()? {
        StoredNode::Function(node) => (node.qualified_name, node.file_path, node.line),
        StoredNode::Class(node) => (node.qualified_name, node.file_path, node.line),
        _ => return None,
    };
    Some(TaskSourceLocation {
        symbol: symbol.to_string(),
        path: path.to_string(),
        line,
    })
}

fn source_anchor(store: &ReadOnlyGraphStore, id: &str) -> Option<TaskSourceAnchor> {
    let source = source_location(store, id)?;
    let callers = store
        .relation_view(id, GraphRelation::Callers)
        .ok()
        .into_iter()
        .flat_map(|view| view.items)
        .filter_map(|caller| source_location(store, &caller.node.id))
        .take(2)
        .collect();
    Some(TaskSourceAnchor { source, callers })
}

fn gate_hints(
    store: &BrokerStore,
    worktree: &Path,
    commit: &str,
    paths: &[String],
) -> Option<(Vec<String>, Vec<TaskGateBaseline>)> {
    let repo = GitRepo::discover(worktree).ok()?;
    let gates = match repo.config_toml_at_commit(commit, "gates").ok()? {
        Some(config) => crate::gates::parse_gates(&config).ok()?,
        None => Vec::new(),
    };
    let selections = crate::gates::select_gates(&gates, paths);
    let gates: Vec<_> = selections
        .iter()
        .take(TaskContext::MAX_GATES)
        .map(|selection| selection.gate)
        .collect();
    let tree = (!gates.is_empty())
        .then(|| repo.commit_tree_id(commit).ok())
        .flatten();
    let baseline = gates
        .iter()
        .filter_map(|gate| {
            let result = store
                .cached_gate_result_for_definition(
                    &gate.name,
                    tree.as_deref()?,
                    &gate.definition_hash,
                )
                .ok()
                .flatten()?;
            Some(TaskGateBaseline {
                gate: gate.name.clone(),
                run_id: result.id,
                status: result.status,
                observed_at_ms: result.created_at,
            })
        })
        .collect();
    Some((
        gates.iter().map(|gate| gate.name.clone()).collect(),
        baseline,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aethyme_engine::store::redb::graph_store::RepoMetadata;

    #[test]
    fn startup_context_requires_the_sessions_recorded_revision() {
        let root = tempfile::tempdir().unwrap();
        let worktree = tempfile::tempdir().unwrap();
        let store = GraphStore::open(root.path()).unwrap();
        store
            .set_repo_metadata(&RepoMetadata {
                root_path: root.path().display().to_string(),
                commit_hash: Some("old".into()),
                indexed_at_unix: 0,
                file_count: 0,
                languages: Vec::new(),
            })
            .unwrap();
        drop(store);

        let broker_store = BrokerStore::open(&root.path().join("test.db")).unwrap();
        let result = derive_task_context(
            &broker_store,
            root.path(),
            worktree.path(),
            "new",
            "change handler",
        );
        assert!(result.unwrap_err().contains("recorded checkout commit"));
    }
}
