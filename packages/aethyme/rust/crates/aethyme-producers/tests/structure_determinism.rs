//! Determinism gate for [`StructureProducer`].
//!
//! Per `docs/architecture/graph-schema.md §5.4`, every overlay's
//! on-disk bytes must be reproducible across runs. The producer
//! crate ships
//! [`assert_overlay_producer_is_deterministic`](aethyme_producers::assert_overlay_producer_is_deterministic),
//! which calls `produce()` twice and byte-compares the encoded
//! envelopes. This test wires `StructureProducer` to that harness
//! against an in-memory fixture so any future regression that
//! introduces order-dependence (a `HashMap`, a wall-clock stamp,
//! a parallel collect) trips here before the CI §5.7 determinism
//! gate ever sees it.
//!
//! The fixture is intentionally tiny but spans the two interesting
//! shapes the producer must handle: a top-level file under no
//! directory, files under a single-level directory (`docs/`,
//! `tests/`), and files under a nested directory (`src/parser/`).
//! That's enough to exercise both the `parent_dir → ...` and
//! `repo → top_level` branches of the producer's edge logic.

use aethyme_graph_storage::{FragmentStore, bootstrap_repo};
use aethyme_producers::{
    ProducerCtx, RepoFileView, RepoView, StructureProducer,
    assert_overlay_producer_is_deterministic,
};
use tempfile::TempDir;

/// Owning mock of a discovered repo. Implements [`RepoView`] by
/// borrowing its own fields — no engine dep, no parser invocation,
/// no filesystem walk. The fixture's only job is to satisfy the
/// trait so the producer has something to mint nodes from.
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

/// The 6-file fixture. Ordered intentionally non-alphabetically in
/// the source so the producer's internal `BTreeSet` / `sort_by`
/// canonicalization is doing real work — if it weren't, the
/// harness would silently pass while accepting input-order bytes
/// into the on-disk envelope.
fn fixture_repo() -> MockRepoView {
    let files = vec![
        mock_file("src/parser/token.rs", Some("rust"), 1024),
        mock_file("docs/intro.md", None, 512),
        mock_file("src/lib.rs", Some("rust"), 2048),
        mock_file("tests/integration.rs", Some("rust"), 768),
        mock_file("docs/setup.md", None, 384),
        mock_file("src/parser/mod.rs", Some("rust"), 1536),
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
        // Stable per-path stub; the producer doesn't interpret the
        // hash, just stamps it on the File node, so any reproducible
        // string suffices for the determinism test.
        content_hash: format!("stub-hash-{path}"),
    }
}

/// Stand up a real on-disk store under a tempdir. The structure
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
fn structure_producer_is_deterministic() {
    let (_tmp, store) = store_fixture();
    let repo = fixture_repo();
    let ctx = ProducerCtx::with_repo(&store, &repo);

    // The harness internally calls produce() twice, encodes via
    // write_overlay_bytes, and asserts byte-equality. No panic =
    // pass.
    assert_overlay_producer_is_deterministic(&StructureProducer, &ctx);
}
