use super::*;

/// What a session recorded about its own scope, and why more was not derived.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ScopeCaptureReport {
    pub declared: usize,
    pub derived: usize,
    /// Present when nothing could be derived. Stated rather than implied, so
    /// "no scopes" is never mistaken for "no collisions possible".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
}

/// Symbols a task is likely to touch, according to committed graph state.
///
/// Mirrors the degradation the impact reader already uses: the store is
/// repository opt-in, so its absence is an ordinary condition to report, not a
/// failure to raise.
pub(super) fn derive_scopes_from_task(
    repo_root: &Path,
    task: &str,
) -> (Vec<String>, Option<String>) {
    use aethyme_engine::graph::navigation::task_scope_view_redb;
    use aethyme_engine::model::task::TaskInput;
    use aethyme_engine::store::redb::graph_store::GraphStore;

    let store_path = repo_root.join(".aethyme/graph_store.redb");
    if !store_path.is_file() {
        return (
            Vec::new(),
            Some(
                "graph store is absent, so no scope could be derived from the task; \
                 declare scopes explicitly to compare intent across sessions"
                    .to_string(),
            ),
        );
    }
    let store = match GraphStore::open_read_only(repo_root) {
        Ok(store) => store,
        Err(error) => {
            return (
                Vec::new(),
                Some(format!("graph store could not be opened: {error}")),
            );
        }
    };
    match task_scope_view_redb(&store, &TaskInput::from_task_text(task)) {
        Ok(view) => (view.in_scope_symbols, None),
        Err(error) => (
            Vec::new(),
            Some(format!("task scope could not be resolved: {error}")),
        ),
    }
}
