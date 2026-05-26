//! Determinism gate for [`RisksProducer`].
//!
//! Per `docs/architecture/graph-schema.md §5.4`, every overlay's
//! on-disk bytes must be reproducible across runs. This test wires
//! `RisksProducer` to
//! [`assert_overlay_producer_is_deterministic`](aethyme_producers::assert_overlay_producer_is_deterministic)
//! so any future regression that leaks order-dependence into the
//! overlay envelope trips here before the CI §5.7 determinism gate
//! ever sees it.
//!
//! ## Why this producer needs a *richer* fixture than its siblings
//!
//! `RisksProducer` is the **first fragment-reading producer**. The
//! docs/configs/structure producers derive their entire payload from
//! the [`RepoView`](aethyme_producers::RepoView) path list, so their
//! determinism fixtures are pure in-memory mocks. Risks fuses three
//! independent sources:
//!
//! 1. **`path_risks`** — classified from `view.files()` paths alone.
//! 2. **`shared_core_risks`** — derived from `Imports` edges read out
//!    of the on-disk fragments, keyed through a `HashMap`/`HashSet`.
//! 3. **`destructive_risks`** — derived from `Function`-name nodes
//!    read out of the on-disk fragments.
//!
//! Sources 2 and 3 are the dangerous ones for determinism: both walk
//! `HashMap`s whose iteration order is unstable run-to-run. The
//! producer defends against that with a final `sort()` + `dedup()` on
//! the fused vec — and *this* fixture is what proves the defence
//! holds, because it deliberately feeds all three sources at once. A
//! `path_risks`-only fixture would pass the harness even if the
//! HashMap-order bug were live.
//!
//! ## Fixture shape
//!
//! - **`RepoView` files** span the `path_risks` predicate surface:
//!   an `auth` path (High/Auth), an `infra`+`helper` path (the Low
//!   downgrade branch), a `billing` path (Medium), and a clean path
//!   that must produce nothing.
//! - **An importer fragment** wires four distinct importer files at a
//!   single `core/db.ts` target (in-degree 4 ⇒ Low/SharedCore),
//!   exercising source 2.
//! - **A destructive-function fragment** carries a `deleteUser`
//!   function (⇒ Medium/Destructive), exercising source 3.
//!
//! The fragments are written into the **same** `FragmentStore` the
//! `ProducerCtx` wraps — the producer reads `ctx.store`, so a fixture
//! that wrote elsewhere would silently exercise only source 1.
//!
//! The `RepoView` file list is ordered non-alphabetically on purpose
//! so the producer's internal `sort()` is doing real work; if it
//! weren't, the harness would accept input-order bytes.

use aethyme_graph_schema::{
    Confidence, Edge, EdgeAttributes, File, Function, Node,
    ParameterSignature, Source, SourceRange, Visibility,
};
use aethyme_graph_storage::{
    bootstrap_repo, write_fragment, Fragment, FragmentStore,
};
use aethyme_producers::{
    assert_overlay_producer_is_deterministic, ProducerCtx, RepoFileView,
    RepoView, RisksProducer,
};
use tempfile::TempDir;

const REPO: &str = "fixture-repo";

/// Owning mock of a discovered repo. Implements [`RepoView`] by
/// borrowing its own fields — no engine dep, no filesystem walk. Only
/// the path list matters to `path_risks`; the other fields exist to
/// satisfy the trait.
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

/// `RepoView` fixture spanning the `path_risks` accept/reject surface.
/// Scrambled ordering: alphabetical would let the test pass even if
/// the producer's `sort()` line vanished.
fn fixture_repo() -> MockRepoView {
    let files = vec![
        // Billing → Medium.
        mock_file("services/billing/invoice.ts"),
        // Auth → High.
        mock_file("src/auth/login.ts"),
        // REJECT: clean path, no risk predicate matches.
        mock_file("src/utils/format.ts"),
        // Infra + helper → the Low downgrade branch.
        mock_file("infra/helpers/dns.ts"),
    ];
    MockRepoView {
        name: REPO.to_string(),
        root_path: "/tmp/fixture-repo".to_string(),
        files,
    }
}

fn mock_file(path: &str) -> RepoFileView {
    RepoFileView {
        path: path.to_string(),
        language: Some("typescript".to_string()),
        byte_size: 128,
        content_hash: format!("stub-hash-{path}"),
    }
}

/// Stand up a real on-disk store under a tempdir and seed it with the
/// two fragments that drive the shared-core and destructive sources.
/// TempDir must outlive the store — drop order matters because the
/// store retains the directory the tempdir cleans up.
fn store_fixture() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");

    // Seed fragments BEFORE opening the store so the first
    // `list_indexed_source_paths()` already sees them.
    write_fragment(
        tmp.path(),
        "imports.ts",
        &importer_fragment("core/db.ts", &["a.ts", "b.ts", "c.ts", "d.ts"]),
    )
    .expect("write importer fragment");
    write_fragment(
        tmp.path(),
        "db/users.ts",
        &fn_fragment("db/users.ts", &["deleteUser", "fetchUser"]),
    )
    .expect("write destructive fragment");

    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

/// Importer fragment: a `File` node per distinct importer in
/// `src_paths`, each wired by a namespace `Imports` edge to the single
/// `dst_path` File. In-degree = number of distinct importers; mirrors
/// the unit-test helper in `src/risks.rs`.
fn importer_fragment(dst_path: &str, src_paths: &[&str]) -> Fragment {
    let dst = File::new(REPO, dst_path, "typescript", 100, "h")
        .expect("dst node");
    let dst_id = dst.id().clone();

    let mut nodes: Vec<Node> = Vec::new();
    let mut edges: Vec<Edge> = Vec::new();
    for src_path in src_paths {
        let src = File::new(REPO, src_path, "typescript", 100, "h")
            .expect("src node");
        let edge = Edge::new(
            src.id().clone(),
            dst_id.clone(),
            EdgeAttributes::Imports {
                import_path: (*src_path).into(),
                is_namespace: true,
                is_default: false,
                is_named: false,
            },
            Source::Code,
            Confidence::FULL,
        );
        nodes.push(src.into());
        edges.push(edge);
    }
    nodes.push(dst.into());
    Fragment::new("imports.ts", nodes, edges).expect("fragment")
}

/// Destructive fragment: one `Function` node per name on `file_path`.
fn fn_fragment(file_path: &str, fn_names: &[&str]) -> Fragment {
    let mut nodes: Vec<Node> = Vec::new();
    for name in fn_names {
        let func = Function::new(
            REPO,
            file_path,
            name,
            &format!("fn {name}()"),
            Vec::<ParameterSignature>::new(),
            None,
            SourceRange::new(1, 1).unwrap(),
            Visibility::Public,
            true,
        )
        .expect("fn node");
        nodes.push(func.into());
    }
    Fragment::new(file_path, nodes, vec![]).expect("fragment")
}

#[test]
fn risks_producer_is_deterministic() {
    let (_tmp, store) = store_fixture();
    let repo = fixture_repo();
    let ctx = ProducerCtx::with_repo(&store, &repo);

    // The harness calls produce() twice, encodes via
    // write_overlay_bytes, and asserts byte-equality. With all three
    // risk sources live (path + shared-core + destructive), this pins
    // the final sort()/dedup() against HashMap iteration order. No
    // panic = pass.
    assert_overlay_producer_is_deterministic(&RisksProducer, &ctx);
}
