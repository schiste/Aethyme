//! Determinism gate for [`DocsProducer`].
//!
//! Per `docs/architecture/graph-schema.md §5.4`, every overlay's
//! on-disk bytes must be reproducible across runs. This test wires
//! `DocsProducer` to
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
//! 1. **Span every `NonCodeFormat` the producer emits** — Markdown
//!    (`.md`/`.mdx`), `Other("rst")`, Plain (bare `README`) — so a
//!    regression in `format_for_doc_path` (e.g. someone routes
//!    `.rst` to `Plain` by mistake) shows up. Without the full
//!    spread, the determinism harness would pass while the producer
//!    silently re-labelled files.
//! 2. **Include rejected paths** — a `.rs` source file, a `.toml`
//!    config, and a `generated`-segment Markdown file — so the
//!    predicate's reject branches are also exercised. The generated
//!    case in particular pins gate ordering: `contains("generated")`
//!    must fire before the doc branch.
//!
//! The fixture is ordered intentionally non-alphabetically in the
//! source so the producer's internal `sort_by(path)` canonicalization
//! is doing real work — if it weren't, the harness would silently
//! accept input-order bytes into the on-disk envelope.

use aethyme_graph_storage::{FragmentStore, bootstrap_repo};
use aethyme_producers::{
    DocsProducer, ProducerCtx, RepoFileView, RepoView, assert_overlay_producer_is_deterministic,
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
        // Markdown — bare `.md`.
        mock_file("README.md", Some("markdown"), 1024),
        // REJECT: source file, not a doc.
        mock_file("src/lib.rs", Some("rust"), 2048),
        // Other("rst") — reStructuredText.
        mock_file("docs/setup.rst", None, 512),
        // REJECT: config, not a doc (configs producer's territory).
        mock_file("Cargo.toml", Some("toml"), 384),
        // Markdown — `.mdx`.
        mock_file("docs/intro.mdx", Some("mdx"), 768),
        // REJECT: generated segment beats the doc branch.
        mock_file("docs/generated-api.md", Some("markdown"), 640),
        // Plain — bare README, no extension.
        mock_file("docs/README", None, 256),
        // Markdown — nested path.
        mock_file("docs/architecture/overview.md", Some("markdown"), 896),
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

/// Stand up a real on-disk store under a tempdir. The docs producer
/// doesn't read from the store, but [`ProducerCtx`] requires one and
/// `FragmentStore::open` insists the bootstrap layout exists. TempDir
/// must outlive the store — drop order matters because the store
/// retains the directory the tempdir will clean up.
fn store_fixture() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");
    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

#[test]
fn docs_producer_is_deterministic() {
    let (_tmp, store) = store_fixture();
    let repo = fixture_repo();
    let ctx = ProducerCtx::with_repo(&store, &repo);

    // The harness internally calls produce() twice, encodes via
    // write_overlay_bytes, and asserts byte-equality. No panic =
    // pass.
    assert_overlay_producer_is_deterministic(&DocsProducer, &ctx);
}
