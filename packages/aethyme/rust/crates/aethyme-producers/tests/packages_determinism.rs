//! Determinism gate for [`PackageProducer`].
//!
//! Per `docs/architecture/graph-schema.md §5.4`, every overlay's
//! on-disk bytes must be reproducible across runs. This test wires
//! `PackageProducer` to
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
//! 1. **Span every `manifest_kind` the producer emits under commit
//!    4.7.4b's narrow scope** — `cargo`, `npm`, `pyproject` — at
//!    both repo-root and nested locations. The nested entries
//!    exercise the `parent_dir_or_root` branch that does string
//!    splitting, while the root entries exercise the `"."` sentinel
//!    branch. A regression that swapped `rsplit('/')` for `split('/')`
//!    or that broke the root fallback would shift `Package::path`
//!    and the encoded bytes would diverge.
//! 2. **Include rejected paths** — `.rs` source, `.md` doc,
//!    `Cargo.lock`, generated path, `tsconfig.json`, `Dockerfile` —
//!    so the predicate's reject branches are also exercised. A
//!    regression that swapped the basename match for a substring
//!    match (accepting `Dockerfile.tsconfig.json` etc.) would shift
//!    the package set and the harness would catch it.
//!
//! The fixture is ordered intentionally non-alphabetically in the
//! source so the producer's `sort_by(manifest_path)` canonicalization
//! is doing real work — if it weren't, the harness would silently
//! accept input-order bytes into the on-disk envelope.

use aethyme_graph_storage::{bootstrap_repo, FragmentStore};
use aethyme_producers::{
    assert_overlay_producer_is_deterministic, PackageProducer, ProducerCtx,
    RepoFileView, RepoView,
};
use tempfile::TempDir;

/// Owning mock of a discovered repo. Implements [`RepoView`] by
/// borrowing its own fields — no engine dep, no manifest parsing,
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

/// The fixture. Each entry has a comment naming the manifest_kind
/// (for accepted paths) or the reject branch (for excluded paths).
/// Ordering is intentionally scrambled — alphabetical would let the
/// test pass even if the producer's `sort_by` line disappeared.
fn fixture_repo() -> MockRepoView {
    let files = vec![
        // npm — nested under packages/.
        mock_file("packages/web/package.json", Some("json"), 512),
        // REJECT: source file, not a manifest.
        mock_file("src/lib.rs", Some("rust"), 2048),
        // cargo — repo-root.
        mock_file("Cargo.toml", Some("toml"), 1024),
        // REJECT: doc, not a manifest.
        mock_file("README.md", None, 384),
        // pyproject — nested under services/.
        mock_file("services/api/pyproject.toml", Some("toml"), 384),
        // REJECT: tsconfig.json is a manifest the engine recognizes
        // but tags as `config`, not `manifest` — out of scope for
        // 4.7.4b. Producer must skip it to preserve parity.
        mock_file("tsconfig.json", Some("json"), 256),
        // cargo — nested under crates/.
        mock_file("crates/foo/Cargo.toml", Some("toml"), 640),
        // REJECT: lockfile, suffix-filtered by `.lock`.
        mock_file("Cargo.lock", None, 16384),
        // npm — repo-root.
        mock_file("package.json", Some("json"), 768),
        // REJECT: generated path. Defensive — basename equality
        // would otherwise accept this.
        mock_file("generated/Cargo.toml", Some("toml"), 128),
        // REJECT: Dockerfile is a manifest in the broader sense but
        // tagged `runtime` by the engine, not `manifest`. Out of
        // scope for 4.7.4b.
        mock_file("Dockerfile", None, 256),
        // pyproject — repo-root.
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
        // when minting Package (the schema type has no content_hash
        // slot), so any reproducible string suffices.
        content_hash: format!("stub-hash-{path}"),
    }
}

/// Stand up a real on-disk store under a tempdir. The packages
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
fn packages_producer_is_deterministic() {
    let (_tmp, store) = store_fixture();
    let repo = fixture_repo();
    let ctx = ProducerCtx::with_repo(&store, &repo);

    // The harness internally calls produce() twice, encodes via
    // write_overlay_bytes, and asserts byte-equality. No panic =
    // pass.
    assert_overlay_producer_is_deterministic(&PackageProducer, &ctx);
}
