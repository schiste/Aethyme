//! Determinism gate for [`ConfigsProducer`].
//!
//! Per `docs/architecture/graph-schema.md §5.4`, every overlay's
//! on-disk bytes must be reproducible across runs. This test wires
//! `ConfigsProducer` to
//! [`assert_overlay_producer_is_deterministic`](aethyme_producers::assert_overlay_producer_is_deterministic)
//! against an in-memory fixture so any future regression that
//! introduces order-dependence (a `HashMap` keyed on path, a
//! wall-clock stamp, a parallel collect, a non-deterministic
//! tiebreaker) trips here before the CI §5.7 determinism gate ever
//! sees it.
//!
//! ## Fixture shape
//!
//! Two jobs at once:
//!
//! 1. **Span every `NonCodeFormat` the producer emits** — Toml,
//!    Json, Yaml, Plain, `Other("godot")` — so a regression in
//!    `format_for_config_path` (e.g. someone routes `.yml` to
//!    `Plain` by mistake) shows up. Without the full spread, the
//!    determinism harness would pass while the producer silently
//!    re-labelled files.
//! 2. **Include rejected paths** — a `.rs` source file and a `.md`
//!    doc — so the predicate's reject branches are also exercised.
//!    A regression that swapped the lowercase string match for a
//!    non-deterministic `HashSet` lookup would otherwise sneak past.
//!
//! The fixture is ordered intentionally non-alphabetically in the
//! source so the producer's internal `sort_by(path)` canonicalization
//! is doing real work — if it weren't, the harness would silently
//! accept input-order bytes into the on-disk envelope.

use aethyme_graph_storage::{FragmentStore, bootstrap_repo};
use aethyme_producers::{
    ConfigsProducer, ProducerCtx, RepoFileView, RepoView, assert_overlay_producer_is_deterministic,
};
use tempfile::TempDir;

/// Owning mock of a discovered repo. Implements [`RepoView`] by
/// borrowing its own fields — no engine dep, no parser invocation,
/// no filesystem walk. The fixture's only job is to satisfy the
/// trait so the producer has something to classify.
struct MockRepoView {
    name: String,
    root_path: String,
    files: Vec<RepoFileView>,
}

impl RepoView for MockRepoView {
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

/// The fixture. Each entry has a comment naming the format variant
/// (for accepted paths) or the reject branch (for excluded paths).
/// Ordering is intentionally scrambled — alphabetical would let the
/// test pass even if the producer's `sort_by` line disappeared.
fn fixture_repo() -> MockRepoView {
    let files = vec![
        // Toml — repo-root allow-list hit.
        mock_file("Cargo.toml", Some("toml"), 1024),
        // REJECT: source file, not a config.
        mock_file("src/lib.rs", Some("rust"), 2048),
        // Json — repo-root allow-list hit.
        mock_file("package.json", Some("json"), 768),
        // Yaml — path-prefix-gated extension under .github/workflows/.
        mock_file(".github/workflows/ci.yml", Some("yaml"), 512),
        // REJECT: doc, not a config.
        mock_file("README.md", None, 384),
        // Plain — Dockerfile by bare filename.
        mock_file("Dockerfile", None, 256),
        // Other("godot") — bespoke variant for project.godot.
        mock_file("project.godot", None, 640),
        // Plain — dotfile by .env prefix.
        mock_file(".env", None, 128),
        // Json — tsconfig.json allow-list hit.
        mock_file("tsconfig.json", Some("json"), 512),
        // Toml — pyproject.toml allow-list hit.
        mock_file("pyproject.toml", Some("toml"), 384),
    ];
    MockRepoView {
        name: "fixture-repo".to_string(),
        root_path: "/tmp/fixture-repo".to_string(),
        files,
    }
}

fn mock_file(path: &str, language: Option<&str>, byte_size: u64) -> RepoFileView {
    RepoFileView {
        path: path.to_string(),
        language: language.map(str::to_string),
        byte_size,
        // Stable per-path stub. The producer doesn't read this field
        // when minting NonCodeFile (the schema type has no
        // content_hash slot), so any reproducible string suffices.
        content_hash: format!("stub-hash-{path}"),
    }
}

/// Stand up a real on-disk store under a tempdir. The configs
/// producer doesn't read from the store, but [`ProducerCtx`]
/// requires one and `FragmentStore::open` insists the bootstrap
/// layout exists. TempDir must outlive the store — drop order
/// matters because the store retains the directory the tempdir
/// will clean up.
fn store_fixture() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");
    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

#[test]
fn configs_producer_is_deterministic() {
    let (_tmp, store) = store_fixture();
    let repo = fixture_repo();
    let ctx = ProducerCtx::with_repo(&store, &repo);

    // The harness internally calls produce() twice, encodes via
    // write_overlay_bytes, and asserts byte-equality. No panic =
    // pass.
    assert_overlay_producer_is_deterministic(&ConfigsProducer, &ctx);
}
