//! Parity gate for [`ConfigsProducer`] vs. the engine's legacy
//! `passes::configs::build`.
//!
//! Phase 4.7.4 ports the configs pass out of the engine and onto
//! the producer crate. Until commit 4.7.10 deletes
//! `passes/configs.rs`, both implementations live in the tree, and
//! this test pins them against the same fixture so any drift trips
//! here.
//!
//! ## What this test compares — and what it does NOT
//!
//! The C1 cutover (`docs/architecture/phase4-7-c1-cutover.md`)
//! intentionally drops three things from the producer side under
//! **Path 2**:
//!
//! - **`config_type` semantic tags** — the engine stamped strings
//!   like `"manifest"` / `"runtime"`. The producer carries
//!   [`NonCodeFormat`](aethyme_graph_schema::NonCodeFormat) instead
//!   (a parsing-class enum). There is no engine fact to compare
//!   against.
//! - **`Defines` / `Configures` / `EntrypointFor` edges** — the
//!   engine used fuzzy token matching to wire configs to code
//!   they reference. We dropped that wholesale; the producer emits
//!   no edges from this fragment.
//! - **Area linking** — comes back in commit 4.7.7 when
//!   `populate_from_fragments` is wired up.
//!
//! What's left to compare is the **set of paths classified as
//! operational configs**. Both sides apply the same predicate
//! chain (generated → cache → doc → operational-config) to the
//! same fixture; the producer's `classify_non_code` was a
//! copy-translation of the engine's `is_operational_config_path`
//! plus the early-reject gates from `passes/structure.rs::classify_file`.
//! This test pins that translation.
//!
//! ## Why we project to paths, not IDs
//!
//! Engine `ConfigNode.id` is a salted, generated string. Producer
//! `NonCodeFile` IDs are hash-encoded
//! [`NodeId`](aethyme_graph_schema::NodeId)s of shape
//! `"non_code_file:<repo>:<26-char-base32-blake3>"`. The two ID
//! schemes share no prefix to strip. The natural common projection
//! is `path`, which both sides preserve verbatim.
//!
//! ## Why we need a `CodePass` we don't compare against
//!
//! The engine's `configs::build(root, &structure, &code)` requires
//! a `CodePass` for the `link_config_to_entrypoints` step we
//! dropped. We construct one anyway because the signature demands
//! it — the resulting edges are filtered out before comparison.
//! When 4.7.10 deletes the engine pass, both the `CodePass`
//! construction and the import vanish with it.
//!
//! ## Adapter stubs
//!
//! `RepoFile` (engine) and [`RepoFileView`](aethyme_producers::RepoFileView)
//! disagree on shape — same trick as `structure_parity.rs`: stub
//! `content_hash` to a deterministic string keyed off the path so
//! the producer's downstream construction is satisfied.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use aethyme_engine::passes::code as engine_code;
use aethyme_engine::passes::configs as engine_configs;
use aethyme_engine::passes::structure as engine_structure;
use aethyme_engine::repo::{discover_repo, RepoSnapshot};
use aethyme_graph_storage::{bootstrap_repo, FragmentStore};
use aethyme_producers::{
    ConfigsProducer, OverlayProducer, ProducerCtx, RepoFileView, RepoView,
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
                // `classify_non_code`). Keying on path keeps the
                // stub deterministic across runs.
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
/// `fs::read_to_string(root.join(path))` call has something to
/// read (it falls back to `.unwrap_or_default()` on error, but
/// keeping the bytes real avoids any "engine treated this as
/// empty, producer treated it as present" footgun).
const FIXTURE: &[(&str, &str)] = &[
    // Accept — Toml allow-list (Cargo.toml repo-root).
    ("Cargo.toml", "[package]\nname = \"x\"\n"),
    // Accept — Json allow-list (package.json).
    ("package.json", "{\"name\": \"x\"}\n"),
    // Accept — Json allow-list (tsconfig.json).
    ("tsconfig.json", "{\"compilerOptions\": {}}\n"),
    // Accept — Toml allow-list (pyproject.toml).
    ("pyproject.toml", "[project]\nname = \"x\"\n"),
    // Accept — path-prefix-gated Yaml under .github/workflows/.
    (".github/workflows/ci.yml", "name: ci\non: push\n"),
    // Accept — Plain by bare filename (Dockerfile).
    ("Dockerfile", "FROM alpine\n"),
    // Accept — Other("godot") for project.godot.
    ("project.godot", "[application]\n"),
    // Accept — dotfile prefix (.env at repo root).
    (".env", "FOO=bar\n"),
    // Accept — path-prefix-gated Json under config/.
    ("config/app.json", "{}\n"),
    // Reject — source file, not a config.
    ("src/lib.rs", "pub fn x() {}\n"),
    // Reject — doc (Markdown is the docs producer's territory).
    ("README.md", "# Title\n"),
    // Reject — generated path.
    ("src/generated/api.ts", "export const x = 1;\n"),
    // Reject — lockfile (filtered by `.lock` suffix).
    ("Cargo.lock", "# lock\n"),
    // Reject — same extension outside an operational dir.
    ("content/object-templates.json", "{}\n"),
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
/// `ProducerCtx` is satisfied. Lives in a separate tempdir from
/// the fixture so `bootstrap_repo`'s layout doesn't bleed into
/// the directory `discover_repo` is about to walk.
fn store_fixture() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir for store");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");
    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

#[test]
fn configs_producer_matches_engine_pass() {
    // -- Build the fixture on disk so discover_repo has real bytes -
    let fixture_tmp = tempfile::tempdir().expect("tempdir for fixture");
    write_fixture(fixture_tmp.path());

    // -- Engine side: discover + structure + code + configs --------
    // The configs pass requires both structure and code passes as
    // inputs. We don't compare against the code-pass output — it
    // exists to satisfy the engine's signature only.
    let snapshot =
        discover_repo(fixture_tmp.path()).expect("engine discover_repo");
    let structure_pass = engine_structure::build(&snapshot);
    let code_pass = engine_code::build(fixture_tmp.path(), &structure_pass);
    let engine_pass = engine_configs::build(
        fixture_tmp.path(),
        &structure_pass,
        &code_pass,
    );

    // -- Producer side: adapter + ConfigsProducer ------------------
    let adapter = SnapshotAdapter::from_snapshot(&snapshot);
    let (_store_tmp, store) = store_fixture();
    let ctx = ProducerCtx::with_repo(&store, &adapter);
    let fragment = ConfigsProducer
        .produce(&ctx)
        .expect("configs producer");
    let overlay = fragment.payload();

    // -- Project both sides to path sets and assert set-equality --
    // Under Path 2 the producer's overlay carries only `files`;
    // edges, config_type tags, and area links are out of scope.
    // The natural common projection is path.
    let engine_paths: BTreeSet<&str> = engine_pass
        .configs
        .iter()
        .map(|c| c.path.as_str())
        .collect();
    let producer_paths: BTreeSet<&str> =
        overlay.files.iter().map(|f| f.path()).collect();

    assert_eq!(
        engine_paths, producer_paths,
        "operational-config path set diverges between engine and producer"
    );
}
