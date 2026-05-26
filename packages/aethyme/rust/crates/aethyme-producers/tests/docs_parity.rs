//! Parity gate for [`DocsProducer`] vs. the engine's legacy
//! `passes::docs::build`.
//!
//! Phase 4.7.5 ports the docs pass out of the engine and onto the
//! producer crate. Until commit 4.7.10 deletes `passes/docs.rs`,
//! both implementations live in the tree, and this test pins them
//! against the same fixture so any drift trips here.
//!
//! ## What this test compares — and what it does NOT
//!
//! The C1 cutover (`docs/architecture/phase4-7-c1-cutover.md`)
//! intentionally drops four things from the producer side under
//! **Path 2**:
//!
//! - **`title`** — the engine scraped the first `# ` heading from
//!   the body. Structured headings move to the Phase 4.8 parser
//!   layer (`DocSection`). No engine fact to compare against.
//! - **`doc_type` semantic tags** — the engine stamped strings like
//!   `"readme"` / `"architecture"`. The producer carries
//!   [`NonCodeFormat`](aethyme_graph_schema::NonCodeFormat) instead
//!   (a parsing-class enum).
//! - **`Defines` / `Documents` / `References` edges** — the engine
//!   used fuzzy token matching to wire docs to areas, files,
//!   configs, and symbols. We dropped that wholesale; the producer
//!   emits no edges from this fragment.
//! - **Area linking** — comes back in commit 4.7.7 when
//!   `populate_from_fragments` is wired up.
//!
//! What's left to compare is the **set of paths classified as
//! documentation files**. Both sides apply the same predicate chain
//! (generated → cache → doc) to the same fixture; the producer's
//! `classify_doc` is the structural complement of the configs
//! producer's `classify_non_code` over that chain. This test pins
//! that translation.
//!
//! ## Why we project to paths, not IDs
//!
//! Engine `DocNode.id` is `"doc:<repo>:<path>"`. Producer
//! `NonCodeFile` IDs are hash-encoded
//! [`NodeId`](aethyme_graph_schema::NodeId)s of shape
//! `"non_code_file:<repo>:<26-char-base32-blake3>"`. The two ID
//! schemes share no prefix to strip. The natural common projection
//! is `path`, which both sides preserve verbatim.
//!
//! ## Why we pass a `CodePass` and `None` configs we don't compare
//!
//! `docs::build(root, &structure, &code, configs)` requires a
//! `CodePass` for the symbol-linking step and an optional
//! `ConfigsPass` for config-linking — both feed *edges* only. The
//! `docs` vec itself is derived purely from `structure.files`
//! filtered to `FileRole::Doc`, so `configs = None` leaves it
//! unchanged. We build the `CodePass` because the signature demands
//! it and pass `None` for configs; the resulting edges are never
//! compared. When 4.7.10 deletes the engine pass, both vanish with
//! the import.
//!
//! ## Adapter stubs
//!
//! `RepoFile` (engine) and [`RepoFileView`](aethyme_producers::RepoFileView)
//! disagree on shape — same trick as `configs_parity.rs`: stub
//! `content_hash` to a deterministic string keyed off the path so
//! the producer's downstream construction is satisfied.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use aethyme_engine::passes::code as engine_code;
use aethyme_engine::passes::docs as engine_docs;
use aethyme_engine::passes::structure as engine_structure;
use aethyme_engine::repo::{discover_repo, RepoSnapshot};
use aethyme_graph_storage::{bootstrap_repo, FragmentStore};
use aethyme_producers::{
    DocsProducer, OverlayProducer, ProducerCtx, RepoFileView, RepoView,
};
use tempfile::TempDir;

/// Owning adapter that bridges an engine [`RepoSnapshot`] into the
/// producer's [`RepoView`] trait. Owns its files so the parity test
/// can drop the snapshot after building it.
struct SnapshotAdapter {
    name: String,
    root_path: String,
    files: Vec<RepoFileView>,
}

impl SnapshotAdapter {
    fn from_snapshot(snapshot: &RepoSnapshot) -> Self {
        let name = snapshot.repo_name();
        let files = snapshot
            .files
            .iter()
            .map(|f| RepoFileView {
                path: f.path.clone(),
                language: f.language.clone(),
                byte_size: f.size_bytes,
                // Stub: engine RepoFile has no content_hash field,
                // and the producer doesn't read this slot at
                // classification time (path is the only input to
                // `classify_doc`). Keying on path keeps the stub
                // deterministic across runs.
                content_hash: format!("stub-{}", f.path),
            })
            .collect();
        Self {
            name,
            root_path: snapshot.root.clone(),
            files,
        }
    }
}

impl RepoView for SnapshotAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    fn root_path(&self) -> &str {
        &self.root_path
    }

    fn vcs(&self) -> &str {
        "git"
    }

    fn files(&self) -> &[RepoFileView] {
        &self.files
    }
}

/// Fixture spanning every accept/reject decision in the predicate
/// chain. Each entry carries a tiny non-empty body so the engine's
/// `fs::read_to_string(root.join(path))` call (used for title/token
/// extraction we don't compare) has something to read.
const FIXTURE: &[(&str, &str)] = &[
    // Accept — Markdown by `.md`.
    ("README.md", "# Readme\n\nHello.\n"),
    // Accept — Markdown by `.mdx`.
    ("docs/intro.mdx", "# Intro\n\nWelcome.\n"),
    // Accept — Markdown, nested path.
    ("docs/architecture/overview.md", "# Overview\n\nLayers.\n"),
    // Accept — Other("rst").
    ("docs/setup.rst", "Setup\n=====\n\nSteps.\n"),
    // Accept — bare README, no extension → Plain.
    ("docs/README", "plain readme body\n"),
    // Reject — generated segment beats the doc branch.
    ("docs/generated-api.md", "# Generated\n\nDo not edit.\n"),
    // Reject — source file, not a doc.
    ("src/lib.rs", "pub fn x() {}\n"),
    // Reject — config (configs producer's territory).
    ("Cargo.toml", "[package]\nname = \"x\"\n"),
    // Reject — config json.
    ("package.json", "{\"name\": \"x\"}\n"),
];

fn write_fixture(root: &Path) {
    for (rel, body) in FIXTURE {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir -p fixture parent");
        }
        fs::write(&path, body).expect("write fixture file");
    }
}

/// Bootstrap a fragment store next to the fixture so the
/// `ProducerCtx` is satisfied. Lives in a separate tempdir from the
/// fixture so `bootstrap_repo`'s layout doesn't bleed into the
/// directory `discover_repo` is about to walk.
fn store_fixture() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir for store");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");
    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

#[test]
fn docs_producer_matches_engine_pass() {
    // -- Build the fixture on disk so discover_repo has real bytes -
    let fixture_tmp = tempfile::tempdir().expect("tempdir for fixture");
    write_fixture(fixture_tmp.path());

    // -- Engine side: discover + structure + code + docs ----------
    // The docs pass requires structure + code; the code pass output
    // only feeds symbol-linking edges we don't compare. `configs =
    // None` leaves the `docs` vec unchanged (configs feed edges
    // only).
    let snapshot =
        discover_repo(fixture_tmp.path()).expect("engine discover_repo");
    let structure_pass = engine_structure::build(&snapshot);
    let code_pass = engine_code::build(fixture_tmp.path(), &structure_pass);
    let engine_pass = engine_docs::build(
        fixture_tmp.path(),
        &structure_pass,
        &code_pass,
        None,
    );

    // -- Producer side: adapter + DocsProducer --------------------
    let adapter = SnapshotAdapter::from_snapshot(&snapshot);
    let (_store_tmp, store) = store_fixture();
    let ctx = ProducerCtx::with_repo(&store, &adapter);
    let fragment = DocsProducer.produce(&ctx).expect("docs producer");
    let overlay = fragment.payload();

    // -- Project both sides to path sets and assert set-equality --
    // Under Path 2 the producer's overlay carries only `files`;
    // edges, title, doc_type tags, and area links are out of scope.
    // The natural common projection is path.
    let engine_paths: BTreeSet<&str> =
        engine_pass.docs.iter().map(|d| d.path.as_str()).collect();
    let producer_paths: BTreeSet<&str> =
        overlay.files.iter().map(|f| f.path()).collect();

    assert_eq!(
        engine_paths, producer_paths,
        "documentation-file path set diverges between engine and producer"
    );
}
