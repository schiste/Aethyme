//! Parity gate for [`PackageProducer`] vs. the manifest-tagged
//! subset of the engine's legacy `passes::configs::build`.
//!
//! Phase 4.7.4b ports the *manifest* slice of the configs pass out
//! of the engine and onto the producer crate as the dedicated
//! [`PackageProducer`]. Until commit 4.7.10 deletes
//! `passes/configs.rs`, both implementations live in the tree, and
//! this test pins them against the same fixture so any drift trips
//! here.
//!
//! ## What this test compares — and what it does NOT
//!
//! The producer under commit 4.7.4b is intentionally *narrower*
//! than the engine's configs pass:
//!
//! - The engine emits `ConfigNode`s for every operational config
//!   (manifests, runtime, tsconfig, dockerfile, godot, …) tagged
//!   with a free-form `config_type` string.
//! - The producer emits `Package`s for **only** the three manifest
//!   kinds the engine tags as `config_type == "manifest"`
//!   (`cargo`, `npm`, `pyproject`).
//!
//! So the parity oracle is the engine's emitted configs **filtered**
//! by `config_type == "manifest"`. Anything broader would emit
//! Packages the engine never claimed as manifests, and we'd be
//! comparing against a moving target.
//!
//! Out of scope for this comparison (and deferred to later commits
//! per `docs/architecture/phase4-7-c1-cutover.md`):
//!
//! - **Manifest parsing** (`Package::name` from the manifest body) —
//!   lands in Phase 4.8 with the TOML/JSON parser layer. The
//!   producer currently uses the directory-basename fallback.
//! - **`Defines` / `Configures` / `EntrypointFor` edges** — the
//!   engine used fuzzy token matching; we dropped that wholesale.
//! - **Area linking** — comes back in commit 4.7.7 via
//!   `populate_from_fragments`.
//! - **The other manifest kinds** (godot, dockerfile, tsconfig,
//!   turbo, biome, deno, pnpm-workspace) — they don't carry the
//!   `"manifest"` `config_type` tag in the engine, so they aren't
//!   in this oracle. A follow-up commit will widen the producer's
//!   accept set *after* the engine pass is deleted.
//!
//! ## Why we project to manifest paths, not IDs
//!
//! Engine `ConfigNode.id` is a salted, generated string. Producer
//! `Package` IDs are hash-encoded
//! [`NodeId`](aethyme_graph_schema::NodeId)s with a
//! `"package:<repo>:<26-char-base32-blake3>"` shape. The two ID
//! schemes share no prefix to strip. The natural common projection
//! is the on-disk path of the manifest file, which both sides
//! preserve verbatim — `ConfigNode.path` on the engine side and
//! `Package::manifest_path()` on the producer side.
//!
//! ## Why we need a `CodePass` we don't compare against
//!
//! The engine's `configs::build(root, &structure, &code)` requires
//! a `CodePass` for the `link_config_to_entrypoints` step we
//! dropped. We construct one anyway because the signature demands
//! it — the resulting edges are filtered out before comparison
//! (we only look at the `configs` vec on the returned pass).
//! When 4.7.10 deletes the engine pass, both the `CodePass`
//! construction and the imports vanish with it.
//!
//! ## Adapter stubs
//!
//! `RepoFile` (engine) and [`RepoFileView`](aethyme_producers::RepoFileView)
//! disagree on shape — same trick as `structure_parity.rs` and
//! `configs_parity.rs`: stub `content_hash` to a deterministic
//! string keyed off the path so the producer's downstream
//! construction is satisfied.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use aethyme_engine::passes::code as engine_code;
use aethyme_engine::passes::configs as engine_configs;
use aethyme_engine::passes::structure as engine_structure;
use aethyme_engine::repo::{discover_repo, RepoSnapshot};
use aethyme_graph_storage::{bootstrap_repo, FragmentStore};
use aethyme_producers::{
    OverlayProducer, PackageProducer, ProducerCtx, RepoFileView, RepoView,
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
                // `classify_manifest`). Keying on path keeps the
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

/// Fixture spanning every accept/reject decision in the manifest
/// predicate chain. Each entry carries a tiny non-empty body so the
/// engine's `fs::read_to_string(root.join(path))` has something to
/// read.
const FIXTURE: &[(&str, &str)] = &[
    // Accept — cargo at repo root.
    ("Cargo.toml", "[package]\nname = \"x\"\n"),
    // Accept — cargo nested in a crates/ subdirectory.
    ("crates/foo/Cargo.toml", "[package]\nname = \"foo\"\n"),
    // Accept — npm at repo root.
    ("package.json", "{\"name\": \"x\"}\n"),
    // Accept — npm nested in a packages/ workspace.
    ("packages/web/package.json", "{\"name\": \"web\"}\n"),
    // Accept — pyproject at repo root.
    ("pyproject.toml", "[project]\nname = \"x\"\n"),
    // Accept — pyproject nested in a services/ subdirectory.
    ("services/api/pyproject.toml", "[project]\nname = \"api\"\n"),
    // Reject — source file, not a manifest.
    ("src/lib.rs", "pub fn x() {}\n"),
    // Reject — doc (engine tags as doc, producer rejects in the
    // doc branch of classify_manifest).
    ("README.md", "# Title\n"),
    // Reject — generated path. Engine drops via `is_generated`;
    // producer drops via the `contains("generated")` reject branch.
    ("generated/Cargo.toml", "[package]\nname = \"gen\"\n"),
    // Reject — lockfile. Engine drops via `.lock` suffix; producer
    // drops via the same `.lock` suffix in the generated branch.
    ("Cargo.lock", "# lock\n"),
    // Reject — tsconfig is a manifest the engine recognizes but
    // tags as `config_type = "config"`, not `"manifest"`. Filtered
    // out of the parity oracle on the engine side.
    ("tsconfig.json", "{\"compilerOptions\": {}}\n"),
    // Reject — Dockerfile is a manifest the engine recognizes but
    // tags as `config_type = "runtime"`, not `"manifest"`. Filtered
    // out of the parity oracle on the engine side.
    ("Dockerfile", "FROM alpine\n"),
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
fn packages_producer_matches_engine_manifest_subset() {
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

    // -- Producer side: adapter + PackageProducer ------------------
    let adapter = SnapshotAdapter::from_snapshot(&snapshot);
    let (_store_tmp, store) = store_fixture();
    let ctx = ProducerCtx::with_repo(&store, &adapter);
    let fragment = PackageProducer
        .produce(&ctx)
        .expect("packages producer");
    let overlay = fragment.payload();

    // -- Project both sides to manifest-path sets and assert equal -
    // Engine side: filter to ConfigNodes the engine tags as
    // `"manifest"`. That's the binding oracle — the producer
    // deliberately narrowed scope to this subset for 4.7.4b.
    let engine_paths: BTreeSet<&str> = engine_pass
        .configs
        .iter()
        .filter(|c| c.config_type == "manifest")
        .map(|c| c.path.as_str())
        .collect();

    // Producer side: project to `manifest_path()`. `Package` IDs
    // are blake3-hashed NodeIds with no shared prefix to the
    // engine's salted ConfigNode.id, so path is the natural
    // common projection.
    let producer_paths: BTreeSet<&str> =
        overlay.packages.iter().map(|p| p.manifest_path()).collect();

    assert_eq!(
        engine_paths, producer_paths,
        "manifest-path set diverges between engine and producer"
    );
}
