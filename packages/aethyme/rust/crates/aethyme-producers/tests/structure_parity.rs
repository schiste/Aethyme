//! Parity gate for [`StructureProducer`] vs. the engine's legacy
//! `passes::structure::build`.
//!
//! Phase 4.7.3 ports the structure pass out of the engine and onto
//! the producer crate. Until commit 4.7.10 deletes `passes/structure.rs`,
//! both implementations live in the tree, and this test pins them
//! against the same fixture so any drift trips here.
//!
//! ## What this test compares — and what it does NOT
//!
//! The C1 cutover (`docs/architecture/phase4-7-c1-cutover.md`)
//! intentionally drops three things from the producer side:
//!
//! - [`AreaNode`s](aethyme_engine::model::area::AreaNode) — areas
//!   come back in commit 4.7.7 when `populate_from_fragments` is
//!   wired up.
//! - `BelongsTo` edges — they only exist for area routing.
//! - Inferred-subarea cross-routing — engine emits `area→area`
//!   `Contains` and `area→repo` `BelongsTo`; the producer collapses
//!   to parent_dir | repo branches only.
//!
//! So the test's comparable surface is **dir-sourced `Contains`
//! edges**: edges whose `from` is a `dir:{repo}:` ID on the engine
//! side, or whose `src` resolves to a `Directory` node on the
//! producer side. Top-level-dir routing (`repo → area`) and
//! inferred-area routing both get filtered out on the engine side;
//! repo-sourced edges (`repo → dir`, `repo → file`) get filtered
//! out on the producer side. Everything that's left is what both
//! implementations are supposed to agree on.
//!
//! ## Why we project to paths, not IDs
//!
//! Engine IDs are strings like `"dir:repo:src/parser"` —
//! path-recoverable by string-stripping the `"dir:{repo}:"` prefix.
//! Producer IDs are hash-encoded
//! [`NodeId`](aethyme_graph_schema::NodeId)s of shape
//! `"<kind>:<repo>:<26-char-base32-blake3>"` — the path is hashed
//! in, not preserved as a substring. To compare the two we project
//! both sides down to `(parent_path, child_path)` tuples and assert
//! set-equality on that.
//!
//! ## Adapter stubs
//!
//! `RepoFile` (engine) and [`RepoFileView`](aethyme_producers::RepoFileView)
//! disagree on shape: the engine snapshot has `line_count` (not on
//! the view) and the view has `content_hash` (not on the engine
//! snapshot). The adapter stubs `content_hash` with a deterministic
//! string keyed off the path so [`File::new`]'s `EmptyContentHash`
//! validation passes; the producer doesn't interpret the hash, only
//! stamps it on the node, so any reproducible string works.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use aethyme_engine::model::edge::EdgeKind;
use aethyme_engine::passes::structure as engine_structure;
use aethyme_engine::repo::{discover_repo, RepoSnapshot};
use aethyme_graph_storage::{bootstrap_repo, FragmentStore};
use aethyme_producers::{
    OverlayProducer, ProducerCtx, RepoFileView, RepoView, StructureProducer,
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
                // and the producer doesn't interpret the value —
                // it just gets stamped on the File node. Keying on
                // path keeps the stub deterministic across runs.
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

/// Six files spanning a top-level file, a single-level dir, a
/// nested dir, and a dir with multiple children. Content bodies
/// are tiny but non-empty so `discover_repo`'s line-count pass
/// has something to read.
const FIXTURE: &[(&str, &str)] = &[
    ("src/parser/token.rs", "pub struct Token;\n"),
    ("docs/intro.md", "# Intro\n"),
    ("src/lib.rs", "pub mod parser;\n"),
    ("tests/integration.rs", "#[test] fn it_works() {}\n"),
    ("docs/setup.md", "# Setup\n"),
    ("src/parser/mod.rs", "mod token;\n"),
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
/// `ProducerCtx` is satisfied. The store lives in a *different*
/// tempdir from the fixture: `bootstrap_repo` writes its layout
/// into the directory you give it, and we don't want it scribbling
/// inside the fixture we're about to hand to `discover_repo`.
fn store_fixture() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir for store");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");
    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

#[test]
fn structure_producer_matches_engine_pass() {
    // -- Build the fixture on disk so discover_repo has real bytes -
    let fixture_tmp = tempfile::tempdir().expect("tempdir for fixture");
    write_fixture(fixture_tmp.path());

    // -- Engine side: discover + legacy structure pass ------------
    let snapshot =
        discover_repo(fixture_tmp.path()).expect("engine discover_repo");
    let engine_pass = engine_structure::build(&snapshot);
    let repo_name = snapshot.repo_name();

    // -- Producer side: adapter + StructureProducer ---------------
    let adapter = SnapshotAdapter::from_snapshot(&snapshot);
    let (_store_tmp, store) = store_fixture();
    let ctx = ProducerCtx::with_repo(&store, &adapter);
    let fragment = StructureProducer
        .produce(&ctx)
        .expect("structure producer");
    let overlay = fragment.payload();

    // -- 1. Directory paths must match exactly --------------------
    // The engine and the producer both implied-enumerate from the
    // same `snapshot.files`, so this assertion would only fail if
    // the producer's helper (parent_directories) diverged from the
    // engine's helper of the same name. They were copy-translated
    // — this guards against future drift.
    let engine_dirs: BTreeSet<&str> = engine_pass
        .directories
        .iter()
        .map(|d| d.path.as_str())
        .collect();
    let producer_dirs: BTreeSet<&str> =
        overlay.directories.iter().map(|d| d.path()).collect();
    assert_eq!(
        engine_dirs, producer_dirs,
        "directory path set diverges between engine and producer"
    );

    // -- 2. File paths must match exactly -------------------------
    let engine_files: BTreeSet<&str> = engine_pass
        .files
        .iter()
        .map(|f| f.path.as_str())
        .collect();
    let producer_files: BTreeSet<&str> =
        overlay.files.iter().map(|f| f.path()).collect();
    assert_eq!(
        engine_files, producer_files,
        "file path set diverges between engine and producer"
    );

    // -- 3. Dir-sourced Contains edges must match -----------------
    // Engine: filter to `Contains` kind whose `from` is a
    // `dir:{repo}:` ID, then strip the prefix off both endpoints
    // to project to a (parent_path, child_path) tuple. The `to`
    // side may be either `dir:` or `file:` — both get stripped.
    let dir_prefix = format!("dir:{repo_name}:");
    let file_prefix = format!("file:{repo_name}:");
    let engine_edges: BTreeSet<(String, String)> = engine_pass
        .edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Contains)
        .filter_map(|e| {
            let from = e.from.as_str().strip_prefix(&dir_prefix)?;
            // `to` is either a dir or a file ID; try both.
            let to = e
                .to
                .as_str()
                .strip_prefix(&dir_prefix)
                .or_else(|| e.to.as_str().strip_prefix(&file_prefix))?;
            Some((from.to_string(), to.to_string()))
        })
        .collect();

    // Producer: build a NodeId → path lookup across directories +
    // files. Skip edges whose `src` is the repository node — those
    // are the producer's repo-sourced `Contains` edges, which the
    // engine routes through the `area:` plane and which we've
    // filtered out above. What's left is the producer's
    // dir-sourced `Contains` set.
    let mut id_to_path: BTreeMap<_, &str> = BTreeMap::new();
    for d in &overlay.directories {
        id_to_path.insert(d.id().clone(), d.path());
    }
    for f in &overlay.files {
        id_to_path.insert(f.id().clone(), f.path());
    }
    let repo_id = overlay.repository.id();
    let producer_edges: BTreeSet<(String, String)> = overlay
        .edges
        .iter()
        .filter(|e| e.src_id() != repo_id)
        .map(|e| {
            let src = id_to_path
                .get(e.src_id())
                .expect("edge src not in directories or files");
            let dst = id_to_path
                .get(e.dst_id())
                .expect("edge dst not in directories or files");
            (src.to_string(), dst.to_string())
        })
        .collect();

    assert_eq!(
        engine_edges, producer_edges,
        "dir-sourced Contains edge set diverges between engine and producer"
    );
}
