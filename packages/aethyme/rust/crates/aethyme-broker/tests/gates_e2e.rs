//! End-to-end gate runner tests (issues #15-#18): real repos, real gate
//! processes, tree-hash caching across sessions, cheap-first ordering
//! with fail-fast, and cancellation of superseded runs.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

use aethyme_broker::{
    AdvisoryEvidence, AdvisorySeverity, Broker, CachePolicy, GRAPH_IMPACT_MAX_DEPTH,
    GRAPH_IMPACT_MAX_NODES, GRAPH_IMPACT_RESULT_LIMIT, GateFailureClass, GateProgressSink,
    GateStatus, GitRepo, GraphImpactLookup, GraphImpactProvider, GraphImpactQuery,
    GraphImpactStatus, NewAdvisory,
};
use aethyme_graph_indexer::{IndexerContext, WalkOptions, index_repo_to_disk, link_repo};
use aethyme_graph_storage::bootstrap_repo;

#[derive(Clone)]
struct FixedGraphImpactProvider {
    lookup: GraphImpactLookup,
}

impl GraphImpactProvider for FixedGraphImpactProvider {
    fn name(&self) -> &str {
        "test_graph"
    }

    fn lookup(&self, query: &GraphImpactQuery<'_>) -> GraphImpactLookup {
        assert_eq!(query.max_results, GRAPH_IMPACT_RESULT_LIMIT);
        assert_eq!(query.max_depth, GRAPH_IMPACT_MAX_DEPTH);
        assert_eq!(query.max_nodes, GRAPH_IMPACT_MAX_NODES);
        assert!(!query.changed_files.is_empty());
        self.lookup.clone()
    }
}

#[derive(Default)]
struct CapturedProgress {
    lines: Mutex<Vec<String>>,
}

impl CapturedProgress {
    fn lines(&self) -> Vec<String> {
        self.lines.lock().unwrap().clone()
    }
}

impl GateProgressSink for CapturedProgress {
    fn report(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_string());
    }
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        // SAFETY: this test restores the variable via Drop. Other tests do
        // not depend on this value, and a shorter heartbeat is harmless.
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: restoring process environment at test teardown; see set().
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

fn sh(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap()
        .status;
    assert!(status.success(), "git {args:?} failed");
}

fn init_repo(root: &Path) {
    sh(root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/app.py"), "x = 1\n").unwrap();
    std::fs::write(root.join("docs.md"), "docs\n").unwrap();
    // Gate outputs must be gitignored: the working-tree hash respects
    // .gitignore, so ignored artifacts don't bust the result cache. The
    // .aethyme runtime-state lines mirror the scaffold's gitignore block —
    // without them a run against the MAIN checkout (gates run --all) would
    // bust its own cache by writing broker.db/logs between runs.
    std::fs::write(
        root.join(".gitignore"),
        "gate-markers.txt\nslow-finished.txt\n\
         .aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n.aethyme/worktrees/\n",
    )
    .unwrap();
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", "init"]);
}

fn write_gates(root: &Path, body: &str) {
    std::fs::create_dir_all(root.join(".aethyme")).unwrap();
    std::fs::write(root.join(".aethyme/gates.toml"), body).unwrap();
}

fn commit_all(root: &Path, message: &str) {
    sh(root, &["add", "-A"]);
    sh(root, &["commit", "-qm", message]);
}

fn add_worktree(root: &Path, name: &str) -> std::path::PathBuf {
    let path = root.join(".aethyme/worktrees").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    sh(
        root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            &format!("agent/{name}"),
            path.to_str().unwrap(),
            "main",
        ],
    );
    path
}

fn refresh_committed_graph(root: &Path) {
    bootstrap_repo(root, env!("CARGO_PKG_VERSION")).unwrap();
    let context =
        IndexerContext::new("fixture", root.to_path_buf(), env!("CARGO_PKG_VERSION")).unwrap();
    index_repo_to_disk(&context, &WalkOptions::default()).unwrap();
    link_repo(&context).unwrap();
}

#[test]
fn session_and_full_tree_gates_enforce_committed_graph_authority() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        "[[gate]]\nname='source-check'\ncommand='echo ran >> gate-markers.txt'\ntriggers=['src/**']\n",
    );
    std::fs::write(
        tmp.path().join(".aethyme/config.toml"),
        "[graph]\nauthority='committed_fragments'\nrepository='fixture'\n",
    )
    .unwrap();
    refresh_committed_graph(tmp.path());
    commit_all(tmp.path(), "configure authoritative graph");

    let mut broker = Broker::open(tmp.path()).unwrap();
    let full_tree = broker.run_all_gates(tmp.path()).unwrap();
    assert_eq!(full_tree.len(), 1, "fresh CI-equivalent tree passes");
    std::fs::remove_file(tmp.path().join("gate-markers.txt")).unwrap();

    let worktree = add_worktree(tmp.path(), "stale-graph-gates");
    let session = broker.adopt(&worktree, None).unwrap();
    std::fs::write(worktree.join("src/app.py"), "x = 2\n").unwrap();
    let status_before = GitRepo::discover(&worktree).unwrap().dirty_paths().unwrap();

    let error = broker.run_gates(session.id).unwrap_err();
    let aethyme_broker::BrokerOpError::GraphIntegrityRejected(rejection) = error else {
        panic!("unexpected gate error: {error}");
    };
    assert_eq!(
        rejection.status,
        aethyme_broker::GraphIntegrityStatus::Stale
    );
    assert_eq!(rejection.tree_hash.len(), 40);
    assert_eq!(rejection.policy_digest.len(), 64);
    assert!(
        rejection
            .changed_paths
            .iter()
            .any(|path| path == ".aethyme/graph/src/app.py.bin")
    );
    assert!(!worktree.join("gate-markers.txt").exists());
    assert_eq!(
        GitRepo::discover(&worktree).unwrap().dirty_paths().unwrap(),
        status_before,
        "refusal must not rewrite the caller worktree or index"
    );

    refresh_committed_graph(&worktree);
    let refreshed = broker.run_gates(session.id).unwrap();
    assert_eq!(refreshed.len(), 1);
    assert_eq!(refreshed[0].status, GateStatus::Pass);
}

#[test]
fn semantic_gate_advice_is_advisory_and_preserves_path_selection() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "always-check"
command = "true"
cost = 0

[[gate]]
name = "python-check"
command = "true"
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "docs-check"
command = "true"
cost = 1
triggers = ["docs/**"]
"#,
    );
    commit_all(tmp.path(), "add gates");

    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "semantic");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 42\n").unwrap();

    let report = broker.semantic_gate_advice(session.id).unwrap();
    assert_eq!(report.session_id, session.id);
    assert_eq!(report.mode, "advisory");
    assert!(!report.enforced);
    assert_eq!(report.changed_files, vec!["src/app.py".to_string()]);
    assert_eq!(report.path_selected_gates.len(), 2);
    assert_eq!(report.path_selected_gates[0].gate, "always-check");
    assert_eq!(report.path_selected_gates[0].triggered_by, None);
    assert_eq!(report.path_selected_gates[0].reason, "always runs");
    assert_eq!(report.path_selected_gates[1].gate, "python-check");
    assert_eq!(
        report.path_selected_gates[1].triggered_by.as_deref(),
        Some("src/app.py")
    );
    assert_eq!(report.path_selected_gates[1].reason, "path trigger");
    assert!(report.semantic_suggested_gates.is_empty());
    assert_eq!(report.semantic.provider, "caller_frontier");
    assert_eq!(report.semantic.status, GraphImpactStatus::GraphMissing);
    assert!(report.semantic.reason.contains("graph_store.redb"));
    assert!(report.semantic.impacted_paths.is_empty());
    assert!(!report.semantic.truncated);
    assert!(report.next_action.contains("gates run"));

    assert_eq!(
        broker.affected_gates(session.id).unwrap(),
        vec![
            ("always-check".to_string(), None),
            ("python-check".to_string(), Some("src/app.py".to_string()))
        ]
    );
}

#[test]
fn ready_graph_impact_is_bounded_and_never_expands_enforced_gates() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "always-check"
command = "true"
cost = 0

[[gate]]
name = "python-check"
command = "true"
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "docs-check"
command = "true"
cost = 1
triggers = ["docs/**"]
"#,
    );
    commit_all(tmp.path(), "add gates");

    let wt = add_worktree(tmp.path(), "semantic-ready");
    let mut setup = Broker::open(tmp.path()).unwrap();
    let session = setup.adopt(&wt, None).unwrap();
    drop(setup);
    std::fs::write(wt.join("src/app.py"), "x = 42\n").unwrap();

    let provider_paths = (0..GRAPH_IMPACT_RESULT_LIMIT + 8)
        .map(|index| format!("docs/impact-{index:02}.md"))
        .collect();
    let provider = FixedGraphImpactProvider {
        lookup: GraphImpactLookup::ready(provider_paths, false, "ready fixture"),
    };
    let mut broker = Broker::open_with_graph_impact_provider(tmp.path(), provider).unwrap();

    let report = broker.semantic_gate_advice(session.id).unwrap();
    assert_eq!(report.semantic.status, GraphImpactStatus::Ready);
    assert_eq!(
        report.semantic.impacted_paths.len(),
        GRAPH_IMPACT_RESULT_LIMIT
    );
    assert!(report.semantic.truncated);
    assert_eq!(report.semantic.result_limit, GRAPH_IMPACT_RESULT_LIMIT);
    assert_eq!(report.semantic_suggested_gates.len(), 1);
    assert_eq!(report.semantic_suggested_gates[0].gate, "docs-check");
    assert_eq!(
        report.semantic_suggested_gates[0].reason,
        "incoming Calls frontier"
    );

    assert_eq!(
        broker.affected_gates(session.id).unwrap(),
        vec![
            ("always-check".to_string(), None),
            ("python-check".to_string(), Some("src/app.py".to_string()))
        ],
        "semantic advice must not expand the enforced selector"
    );
    let executed = broker.run_gates(session.id).unwrap();
    assert_eq!(
        executed
            .iter()
            .map(|outcome| outcome.gate.as_str())
            .collect::<Vec<_>>(),
        vec!["always-check", "python-check"],
        "semantic suggestions must not reach gate execution"
    );
}

#[test]
fn warm_graph_caller_chain_suggests_but_does_not_run_a_gate() {
    use aethyme_engine::model::edge::{Edge, EdgeKind};
    use aethyme_engine::model::file::{FileNode, FileRole};
    use aethyme_engine::model::function::FunctionNode;
    use aethyme_engine::model::intern::InternedStr;
    use aethyme_engine::store::redb::graph_store::{
        GraphStore, insert_edge, insert_file, insert_function,
    };

    fn function(file: &FileNode, name: &str) -> FunctionNode {
        FunctionNode::new(
            "Repo",
            InternedStr::from(file.id.clone()),
            InternedStr::from(file.path.clone()),
            None,
            None,
            InternedStr::from("rust"),
            InternedStr::from(name),
            1,
            InternedStr::from(format!("fn {name}()")),
        )
    }

    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "changed-check"
command = "true"
cost = 1
triggers = ["src/core.rs"]

[[gate]]
name = "caller-check"
command = "true"
cost = 1
triggers = ["src/service.rs"]
"#,
    );
    commit_all(tmp.path(), "add gates");

    let changed_file = FileNode::new(
        "Repo",
        "src/core.rs",
        Some("rust".into()),
        FileRole::Source,
        10,
        100,
        false,
        None,
    );
    let caller_file = FileNode::new(
        "Repo",
        "src/service.rs",
        Some("rust".into()),
        FileRole::Source,
        10,
        100,
        false,
        None,
    );
    let changed_function = function(&changed_file, "changed");
    let caller_function = function(&caller_file, "caller");
    let graph = GraphStore::open(tmp.path()).unwrap();
    let mut index = graph.begin_index().unwrap();
    insert_file(&mut index, &changed_file).unwrap();
    insert_file(&mut index, &caller_file).unwrap();
    insert_function(&mut index, &changed_function).unwrap();
    insert_function(&mut index, &caller_function).unwrap();
    insert_edge(
        &mut index,
        &Edge::new(
            caller_function.id.as_str(),
            changed_function.id.as_str(),
            EdgeKind::Calls,
            1000,
            "test",
        ),
    )
    .unwrap();
    index.commit().unwrap();
    drop(graph);

    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "semantic-warm");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::create_dir_all(wt.join("src")).unwrap();
    std::fs::write(wt.join("src/core.rs"), "fn changed() {}\n").unwrap();

    let report = broker.semantic_gate_advice(session.id).unwrap();
    assert_eq!(report.semantic.status, GraphImpactStatus::Ready);
    assert_eq!(report.semantic.impacted_paths, vec!["src/service.rs"]);
    assert_eq!(report.semantic.frontier_max_depth, 2);
    assert_eq!(report.semantic.frontier_max_nodes, 128);
    assert_eq!(report.semantic.frontier_visited_nodes, 2);
    assert!(!report.semantic.truncated);
    assert_eq!(report.semantic_suggested_gates.len(), 1);
    let suggestion = &report.semantic_suggested_gates[0];
    assert_eq!(suggestion.gate, "caller-check");
    let chain = suggestion.chain.as_ref().expect("explainable caller chain");
    assert_eq!(chain.changed_file, "src/core.rs");
    assert_eq!(chain.caller_file, "src/service.rs");
    assert_eq!(chain.suggested_gate, "caller-check");
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["semantic_suggested_gates"][0]["chain"]["changed_file"],
        "src/core.rs"
    );
    assert_eq!(
        json["semantic_suggested_gates"][0]["chain"]["caller_file"],
        "src/service.rs"
    );
    assert_eq!(
        json["semantic_suggested_gates"][0]["chain"]["suggested_gate"],
        "caller-check"
    );

    assert_eq!(
        broker.affected_gates(session.id).unwrap(),
        vec![("changed-check".to_string(), Some("src/core.rs".to_string()))]
    );
    let executed = broker.run_gates(session.id).unwrap();
    assert_eq!(executed.len(), 1);
    assert_eq!(executed[0].gate, "changed-check");
}

#[test]
fn semantic_suggestions_never_affect_gate_runs_or_submit() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "changed-check"
command = "true"
cost = 1
triggers = ["src/app.py"]

[[gate]]
name = "semantic-only-failure"
command = "false"
cost = 1
triggers = ["docs/**"]
"#,
    );
    commit_all(tmp.path(), "add gates");

    let wt = add_worktree(tmp.path(), "semantic-submit");
    let mut setup = Broker::open(tmp.path()).unwrap();
    let session = setup.adopt(&wt, None).unwrap();
    drop(setup);
    std::fs::write(wt.join("src/app.py"), "x = 42\n").unwrap();
    commit_all(&wt, "change app");

    let provider = FixedGraphImpactProvider {
        lookup: GraphImpactLookup::ready(
            vec!["docs/semantic-impact.md".into()],
            false,
            "semantic-only fixture",
        ),
    };
    let mut broker = Broker::open_with_graph_impact_provider(tmp.path(), provider).unwrap();
    let advice = broker.semantic_gate_advice(session.id).unwrap();
    assert_eq!(advice.path_selected_gates.len(), 1);
    assert_eq!(advice.path_selected_gates[0].gate, "changed-check");
    assert_eq!(advice.semantic_suggested_gates.len(), 1);
    assert_eq!(
        advice.semantic_suggested_gates[0].gate,
        "semantic-only-failure"
    );

    let executed = broker.run_gates(session.id).unwrap();
    assert_eq!(
        executed
            .iter()
            .map(|outcome| outcome.gate.as_str())
            .collect::<Vec<_>>(),
        vec!["changed-check"],
        "gates run must ignore the failing semantic-only suggestion"
    );

    let submitted = broker.submit(session.id).unwrap();
    assert!(
        submitted.promoted,
        "submit must promote when every path-selected gate passes"
    );
    assert_eq!(
        submitted
            .gate_outcomes
            .iter()
            .map(|outcome| outcome.gate.as_str())
            .collect::<Vec<_>>(),
        vec!["changed-check"],
        "submit must ignore the failing semantic-only suggestion"
    );
}

#[test]
fn degraded_graph_provider_outcomes_are_successful_advisory_reports() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "python-check"
command = "true"
cost = 1
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");

    let wt = add_worktree(tmp.path(), "semantic-degraded");
    let mut setup = Broker::open(tmp.path()).unwrap();
    let session = setup.adopt(&wt, None).unwrap();
    drop(setup);
    std::fs::write(wt.join("src/app.py"), "x = 42\n").unwrap();

    let outcomes = [
        (
            GraphImpactLookup::graph_missing("cold graph fixture"),
            GraphImpactStatus::GraphMissing,
            "cold graph fixture",
        ),
        (
            GraphImpactLookup::graph_stale("stale graph fixture"),
            GraphImpactStatus::GraphStale,
            "stale graph fixture",
        ),
        (
            GraphImpactLookup::provider_error("provider failure fixture"),
            GraphImpactStatus::ProviderError,
            "provider failure fixture",
        ),
    ];

    for (lookup, expected_status, expected_reason) in outcomes {
        let provider = FixedGraphImpactProvider { lookup };
        let mut broker = Broker::open_with_graph_impact_provider(tmp.path(), provider).unwrap();
        let report = broker
            .semantic_gate_advice(session.id)
            .expect("degraded graph state must remain a successful broker report");

        assert_eq!(report.semantic.status, expected_status);
        assert_eq!(report.semantic.reason, expected_reason);
        assert!(report.semantic.impacted_paths.is_empty());
        assert!(report.semantic_suggested_gates.is_empty());
        assert_eq!(report.path_selected_gates.len(), 1);
        assert_eq!(report.path_selected_gates[0].gate, "python-check");
        assert!(!report.enforced);
    }
}

#[test]
fn selection_run_cache_and_fail_fast() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "cheap-marker"
command = "echo cheap-ran >> gate-markers.txt"
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "expensive-fail"
command = "echo expensive-ran >> gate-markers.txt; exit 3"
cost = 2
triggers = ["**/*.py"]

[[gate]]
name = "never-reached"
command = "echo never >> gate-markers.txt"
cost = 3
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");
    let mut broker = Broker::open(tmp.path()).unwrap();

    // Docs-only session: zero gates selected (#16 acceptance).
    let wt_docs = add_worktree(tmp.path(), "docs");
    let docs = broker.adopt(&wt_docs, None).unwrap();
    std::fs::write(wt_docs.join("docs.md"), "changed docs\n").unwrap();
    assert!(broker.affected_gates(docs.id).unwrap().is_empty());
    assert!(broker.run_gates(docs.id).unwrap().is_empty());

    // Python session: cheap-first, fail-fast at the failing gate (#17).
    let wt_py = add_worktree(tmp.path(), "py");
    let py = broker.adopt(&wt_py, None).unwrap();
    std::fs::write(wt_py.join("src/app.py"), "x = 2\n").unwrap();

    let affected = broker.affected_gates(py.id).unwrap();
    assert_eq!(affected.len(), 3);
    assert_eq!(affected[0].1.as_deref(), Some("src/app.py"), "--why works");

    let outcomes = broker.run_gates(py.id).unwrap();
    assert_eq!(outcomes.len(), 2, "fail-fast stops before never-reached");
    assert_eq!(outcomes[0].gate, "cheap-marker");
    assert_eq!(outcomes[0].status, GateStatus::Pass);
    assert!(!outcomes[0].cached);
    assert_eq!(outcomes[1].gate, "expensive-fail");
    assert_eq!(outcomes[1].status, GateStatus::Fail);
    assert_eq!(
        outcomes[1].failure_class,
        Some(GateFailureClass::TestFailure)
    );
    assert_eq!(outcomes[1].exit_code, Some(3));
    let markers = std::fs::read_to_string(wt_py.join("gate-markers.txt")).unwrap();
    assert!(!markers.contains("never"));

    // Same tree, same session, run again: pure cache, no re-execution.
    // (gate-markers.txt is gitignored, so writing it did not change the
    // working-tree hash — the real-world contract for gate outputs.)
    let outcomes = broker.run_gates(py.id).unwrap();
    assert!(outcomes.iter().all(|o| o.cached), "all cache hits");
    assert_eq!(outcomes[0].failure_class, None);
    assert_eq!(
        outcomes[1].failure_class,
        Some(GateFailureClass::CachedPriorFail)
    );
    let markers_after = std::fs::read_to_string(wt_py.join("gate-markers.txt")).unwrap();
    assert_eq!(markers, markers_after, "cached rerun executed nothing");

    // Cross-agent dedup (#17): a second worktree with IDENTICAL content
    // hashes to the same tree → cache hits without running.
    let wt_py2 = add_worktree(tmp.path(), "py2");
    let py2 = broker.adopt(&wt_py2, None).unwrap();
    std::fs::write(wt_py2.join("src/app.py"), "x = 2\n").unwrap();
    let outcomes = broker.run_gates(py2.id).unwrap();
    assert!(
        outcomes.iter().all(|o| o.cached),
        "identical tree in another session reuses results: {outcomes:?}"
    );
    assert_eq!(
        outcomes[1].failure_class,
        Some(GateFailureClass::CachedPriorFail)
    );
}

#[test]
fn gate_commands_receive_worker_suffix_and_owner_paths() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "env-check"
command = 'test -n "$AETHYME_GATE_WORKER_ID" && test "$AETHYME_TEST_DB_SUFFIX" = "$AETHYME_GATE_WORKER_ID" && case "$AETHYME_GATE_OWNER_PATHS" in *src/app.py*) exit 0 ;; *) exit 9 ;; esac'
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");

    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "env");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 2\n").unwrap();

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].gate, "env-check");
    assert_eq!(outcomes[0].status, GateStatus::Pass);
    assert!(outcomes[0].log_path.as_deref().unwrap().contains("-s"));
}

#[test]
fn cache_false_gate_reruns_for_the_same_tree() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "metadata-sensitive"
command = "echo run >> gate-markers.txt"
cache = false
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");
    let gates = aethyme_broker::load_gates(tmp.path()).unwrap();
    assert!(!gates[0].cache);
    let mut broker = Broker::open(tmp.path()).unwrap();

    let wt = add_worktree(tmp.path(), "no-cache");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 3\n").unwrap();

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(!outcomes[0].cached, "first run executes");

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(
        !outcomes[0].cached,
        "cache=false gate must not reuse same-tree results"
    );
    let markers = std::fs::read_to_string(wt.join("gate-markers.txt")).unwrap();
    assert_eq!(markers.lines().count(), 2, "gate command ran twice");
}

#[test]
fn session_gate_runs_use_the_session_worktree_gate_config() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "config-source"
command = "echo main-run >> gate-markers.txt"
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add main gates");

    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "worktree-gates");
    let session = broker.adopt(&wt, None).unwrap();
    write_gates(
        &wt,
        r#"
[[gate]]
name = "config-source"
command = "echo worktree-run >> gate-markers.txt"
cache = false
triggers = ["**/*.py"]
"#,
    );
    std::fs::write(wt.join("src/app.py"), "x = 4\n").unwrap();

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(!outcomes[0].cached);
    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(
        !outcomes[0].cached,
        "cache=false from the session worktree must control reruns"
    );

    let markers = std::fs::read_to_string(wt.join("gate-markers.txt")).unwrap();
    assert_eq!(
        markers.lines().collect::<Vec<_>>(),
        vec!["worktree-run", "worktree-run"]
    );
}

#[test]
fn cargo_target_dir_infra_errors_are_not_cached_failures() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "cargo-test"
command = "echo cargo-run >> gate-markers.txt; printf '%s\n' 'error: extern location for aethyme_broker does not exist: target/debug/deps/libaethyme_broker.rlib' >&2; exit 101 # cargo test"
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "cargo-infra");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 4\n").unwrap();

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].gate, "cargo-test");
    assert_eq!(outcomes[0].status, GateStatus::Error);
    assert_eq!(
        outcomes[0].failure_class,
        Some(GateFailureClass::ResourceContention)
    );
    assert_eq!(outcomes[0].exit_code, Some(101));
    assert!(!outcomes[0].cached);

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0].status,
        GateStatus::Error,
        "the same infra-shaped cargo failure is still an error"
    );
    assert_eq!(
        outcomes[0].failure_class,
        Some(GateFailureClass::ResourceContention)
    );
    assert!(
        !outcomes[0].cached,
        "error rows must never satisfy the tree-hash gate cache"
    );

    let markers = std::fs::read_to_string(wt.join("gate-markers.txt")).unwrap();
    assert_eq!(
        markers.lines().count(),
        2,
        "a second run re-executes instead of reusing a poisoned fail cache"
    );

    let tree = aethyme_broker::GitRepo::discover(&wt)
        .unwrap()
        .working_tree_hash()
        .unwrap();
    assert!(
        broker
            .store()
            .cached_gate_result("cargo-test", &tree)
            .unwrap()
            .is_none(),
        "cargo target-dir infrastructure errors are non-conclusive"
    );
}

#[test]
fn environment_and_timeout_failures_are_classified_and_not_cached() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "missing-tool"
command = "echo env-run >> gate-markers.txt; definitely_missing_aethyme_gate_tool_123"
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "timeout"
command = "echo timeout-run >> gate-markers.txt; printf '%s\n' 'command timed out' >&2; exit 124"
cost = 2
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "env-timeout");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 5\n").unwrap();

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1, "environment error fail-fasts");
    assert_eq!(outcomes[0].status, GateStatus::Error);
    assert_eq!(
        outcomes[0].failure_class,
        Some(GateFailureClass::Environment)
    );
    assert_eq!(outcomes[0].exit_code, Some(127));

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(!outcomes[0].cached);
    let markers = std::fs::read_to_string(wt.join("gate-markers.txt")).unwrap();
    assert_eq!(
        markers.lines().filter(|line| *line == "env-run").count(),
        2,
        "environment failures must rerun instead of poisoning the cache"
    );

    write_gates(
        &wt,
        r#"
[[gate]]
name = "timeout"
command = "echo timeout-run >> gate-markers.txt; printf '%s\n' 'command timed out' >&2; exit 124"
triggers = ["**/*.py"]
"#,
    );
    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, GateStatus::Error);
    assert_eq!(outcomes[0].failure_class, Some(GateFailureClass::Timeout));
    assert_eq!(outcomes[0].exit_code, Some(124));

    let outcomes = broker.run_gates(session.id).unwrap();
    assert_eq!(outcomes.len(), 1);
    assert!(!outcomes[0].cached);
    let markers = std::fs::read_to_string(wt.join("gate-markers.txt")).unwrap();
    assert_eq!(
        markers
            .lines()
            .filter(|line| *line == "timeout-run")
            .count(),
        2,
        "timeout failures must rerun instead of poisoning the cache"
    );
}

#[test]
fn slow_gate_emits_heartbeat_progress() {
    let _env = EnvGuard::set("AETHYME_GATE_HEARTBEAT_SECS", "1");
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "heartbeat"
command = "sleep 2"
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "heartbeat");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 3\n").unwrap();

    let progress = CapturedProgress::default();
    let outcomes = broker
        .run_gates_with_progress(session.id, &progress)
        .unwrap();

    assert_eq!(outcomes.len(), 1);
    assert_eq!(outcomes[0].status, GateStatus::Pass);
    let lines = progress.lines();
    assert!(
        lines
            .iter()
            .any(|line| line.starts_with("gate heartbeat running... ") && line.ends_with('s')),
        "expected heartbeat progress line, got {lines:?}"
    );
}

#[test]
fn outstanding_advisory_is_repeated_immediately_before_expensive_gate_execution() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "cheap"
command = "true"
cost = 1
triggers = ["**/*.py"]

[[gate]]
name = "expensive"
command = "true"
cost = 2
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "advisory-before-expensive");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "x = 3\n").unwrap();
    let advisory = broker
        .persist_advisory(NewAdvisory {
            identity: "gate-boundary-advisory".into(),
            session_id: Some(session.id),
            severity: AdvisorySeverity::Warning,
            queue_entry_id: None,
            integration_sha: Some("c".repeat(40)),
            paths: vec!["src/app.py".into()],
            evidence: vec![AdvisoryEvidence {
                kind: "safe_next_action".into(),
                summary: "aethyme broker status --json".into(),
            }],
        })
        .unwrap();

    let progress = CapturedProgress::default();
    let outcomes = broker
        .run_gates_with_progress(session.id, &progress)
        .unwrap();
    assert_eq!(outcomes.len(), 2);
    assert!(
        outcomes
            .iter()
            .all(|outcome| outcome.status == GateStatus::Pass)
    );
    let lines = progress.lines();
    let cheap_started = lines
        .iter()
        .position(|line| line.starts_with("gate cheap started"))
        .unwrap();
    let notice = lines
        .iter()
        .position(|line| line.contains(&format!("Aethyme advisory {}", advisory.id)))
        .unwrap();
    let expensive_started = lines
        .iter()
        .position(|line| line.starts_with("gate expensive started"))
        .unwrap();
    assert!(cheap_started < notice);
    assert!(notice < expensive_started);
}

#[test]
fn resubmit_with_new_tree_cancels_obsolete_slow_run() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "slow"
command = "sleep 30; echo done >> slow-finished.txt"
triggers = ["**/*.py"]
"#,
    );
    commit_all(tmp.path(), "add gates");
    let mut broker = Broker::open(tmp.path()).unwrap();
    let wt = add_worktree(tmp.path(), "slow");
    let session = broker.adopt(&wt, None).unwrap();
    std::fs::write(wt.join("src/app.py"), "v1\n").unwrap();

    // Launch the slow gate run on a second Broker handle (separate SQLite
    // connection — cancellation must work across connections/processes).
    let db_dir = tmp.path().to_path_buf();
    let session_id = session.id;
    let runner = std::thread::spawn(move || {
        let mut broker = Broker::open(&db_dir).unwrap();
        broker.run_gates(session_id)
    });

    // Give the gate time to actually start.
    std::thread::sleep(std::time::Duration::from_millis(800));

    // The agent edits again → new tree → the obsolete run is cancelled
    // (run_gates does this automatically; tested here in isolation so the
    // test doesn't have to sit through the NEW slow gate).
    std::fs::write(wt.join("src/app.py"), "v2\n").unwrap();
    let cancelled = broker.cancel_obsolete_gate_runs(session.id).unwrap();
    assert_eq!(cancelled, vec!["slow".to_string()]);

    let old = runner.join().unwrap().unwrap();
    assert_eq!(old.len(), 1);
    assert_eq!(
        old[0].status,
        GateStatus::Cancelled,
        "a killed run is not a verdict on the code — it must never record \
         a conclusive fail: {old:?}"
    );
    assert!(
        !wt.join("slow-finished.txt").exists(),
        "the superseded slow gate never completed"
    );

    // A cancelled row exists for the old tree.
    let events = broker.store().events_after(0, i64::MAX).unwrap();
    assert!(
        events.iter().any(|e| e.kind == "gate.cancelled"),
        "cancellation recorded"
    );

    // The poisoning scenario: the agent reverts to the v1 content, so the
    // old tree hash recurs. The killed run's row must NOT satisfy the
    // cache — a cached fail here would reject a perfectly good
    // submission without ever running the gate.
    std::fs::write(wt.join("src/app.py"), "v1\n").unwrap();
    let v1_tree = aethyme_broker::GitRepo::discover(&wt)
        .unwrap()
        .working_tree_hash()
        .unwrap();
    assert!(
        broker
            .store()
            .cached_gate_result("slow", &v1_tree)
            .unwrap()
            .is_none(),
        "killed run must not leave a conclusive cached result for its tree"
    );
}

#[test]
fn run_all_runs_every_gate_in_cost_order_ignoring_diff_selection() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    // Triggers deliberately match NOTHING in the repo: diff selection
    // would pick zero of these gates, --all must still run them all.
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "expensive"
command = "echo expensive >> gate-markers.txt"
cost = 3
triggers = ["**/*.nomatch"]

[[gate]]
name = "cheap"
command = "echo cheap >> gate-markers.txt"
cost = 1
triggers = ["**/*.nomatch"]

[[gate]]
name = "mid"
command = "echo mid >> gate-markers.txt"
cost = 2
triggers = ["**/*.nomatch"]
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let tree_hash = aethyme_broker::GitRepo::discover(tmp.path())
        .unwrap()
        .working_tree_hash()
        .unwrap();

    // CI shape: run against the main checkout itself — clean tree, no
    // worktree, no session ever adopted.
    let progress = CapturedProgress::default();
    let outcomes = broker
        .run_all_gates_with_progress(tmp.path(), &progress)
        .unwrap();
    assert_eq!(
        outcomes.iter().map(|o| o.gate.as_str()).collect::<Vec<_>>(),
        vec!["cheap", "mid", "expensive"],
        "every gate runs, cost order, despite zero trigger matches"
    );
    assert!(outcomes.iter().all(|o| o.status == GateStatus::Pass));
    assert!(outcomes.iter().all(|o| !o.cached), "first run executes");
    assert!(outcomes.iter().all(|o| o.tree_hash == tree_hash));
    let markers = std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap();
    assert_eq!(markers, "cheap\nmid\nexpensive\n");

    // The existing streaming surface is reused: started/finished lines.
    let lines = progress.lines();
    assert!(
        lines.iter().any(|line| {
            line.starts_with("gate cheap started") && line.contains(&tree_hash[..12])
        }),
        "streaming progress present: {lines:?}"
    );

    // Same tree again: the tree-hash result cache answers, nothing runs.
    let outcomes = broker.run_all_gates(tmp.path()).unwrap();
    assert!(
        outcomes.iter().all(|o| o.cached),
        "second run is pure cache"
    );
    assert!(outcomes.iter().all(|o| o.tree_hash == tree_hash));
    let markers_after = std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap();
    assert_eq!(markers, markers_after, "cached rerun executed nothing");

    let cached_before = broker
        .store()
        .cached_gate_result("cheap", &tree_hash)
        .unwrap()
        .unwrap();
    let bypassed = broker
        .run_all_gates_with_policy(tmp.path(), CachePolicy::Bypass)
        .unwrap();
    assert!(
        bypassed.iter().all(|outcome| !outcome.cached),
        "bypass executes every gate despite valid cache rows"
    );
    let markers_after_bypass =
        std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap();
    assert_eq!(
        markers_after_bypass,
        "cheap\nmid\nexpensive\ncheap\nmid\nexpensive\n"
    );
    let cached_after = broker
        .store()
        .cached_gate_result("cheap", &tree_hash)
        .unwrap()
        .unwrap();
    assert!(
        cached_after.id > cached_before.id,
        "bypassed execution stores a fresh cache row"
    );

    let reused_fresh = broker.run_all_gates(tmp.path()).unwrap();
    assert!(reused_fresh.iter().all(|outcome| outcome.cached));
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap(),
        markers_after_bypass,
        "normal run reuses the fresh bypass result"
    );
}

#[test]
fn run_all_fails_fast_on_first_failing_gate() {
    let tmp = tempfile::tempdir().unwrap();
    init_repo(tmp.path());
    write_gates(
        tmp.path(),
        r#"
[[gate]]
name = "cheap-pass"
command = "echo cheap >> gate-markers.txt"
cost = 1

[[gate]]
name = "mid-fail"
command = "echo failing >> gate-markers.txt; exit 7"
cost = 2

[[gate]]
name = "never-reached"
command = "echo never >> gate-markers.txt"
cost = 3
"#,
    );
    let mut broker = Broker::open(tmp.path()).unwrap();
    let outcomes = broker.run_all_gates(tmp.path()).unwrap();
    assert_eq!(outcomes.len(), 2, "fail-fast stops before never-reached");
    assert_eq!(outcomes[0].status, GateStatus::Pass);
    assert_eq!(outcomes[1].gate, "mid-fail");
    assert_eq!(outcomes[1].status, GateStatus::Fail);
    assert_eq!(outcomes[1].exit_code, Some(7));
    let markers = std::fs::read_to_string(tmp.path().join("gate-markers.txt")).unwrap();
    assert!(!markers.contains("never"));
}
