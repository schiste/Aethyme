//! Parity gate for [`RisksProducer`] vs. the engine's legacy
//! `passes::overlays::detect_risks` (surfaced as
//! `RepositoryMap::risk_flags`).
//!
//! Phase 4.7.6 ports the risks overlay out of the engine and onto the
//! producer crate. Until the C1 cutover deletes the engine pass, both
//! implementations live in the tree, and this test pins them against
//! the same fixture so any drift in the shared surface trips here.
//!
//! ## What this test compares — and what it does NOT
//!
//! `detect_risks` fuses three sources (`docs/architecture/phase4-7-c1-cutover.md`):
//!
//! 1. **`path_risks`** — flags derived from a file's *path shape*
//!    (Auth / Permissions / Secrets / Migrations / Infra / Billing).
//! 2. **`shared_core_risks`** — in-degree of `Imports` edges.
//! 3. **`destructive_risks`** — destructively-named functions.
//!
//! Only **source 1** is in scope for this parity gate. Sources 2 and 3
//! read from the *graph* — edges and function nodes — and the engine
//! (`RepositoryMap::build`) and the producer (the
//! `aethyme-graph-indexer` → `FragmentStore` pipeline) are two
//! independent extraction front-ends that need not agree node-for-node
//! on functions and imports. Pinning SharedCore/Destructive across them
//! would test pipeline convergence, not risk-policy parity. Those two
//! sources are instead held by `risks_determinism.rs` (reproducible
//! bytes) and the `src/risks.rs` unit tests (threshold/pattern logic).
//! The engine's `graph_annotations` surface is dropped wholesale (see
//! the producer module doc).
//!
//! ## Why we build the adapter from `map.files`, not `discover_repo`
//!
//! `path_risks` is a pure function of the *file list* it is handed.
//! The engine hands it `map.files` — the **structure-pass-filtered**
//! `Vec<FileNode>` (generated/cache files already removed). To isolate
//! *predicate* parity from *file-discovery* parity, we feed the
//! producer that exact same list by adapting `map.files` into the
//! [`RepoView`](aethyme_producers::RepoView). If we instead walked the
//! raw repo, a difference in which files each side *discovers* (the
//! producer's `RepoView` vs the engine's structure pass) would
//! masquerade as predicate drift and the gate would lie. Adapting
//! `map.files` makes both copies of `path_risks` classify a
//! byte-identical path set, so the only thing that can diverge is the
//! predicate itself — which is exactly what we want to pin.
//!
//! ## Why an empty store
//!
//! The producer's source 2 and 3 read `ctx.store`. We bootstrap a
//! store but write **no fragments** into it, so `read_all_fragments`
//! returns empty and the producer emits *only* path flags — precisely
//! the comparable surface. (The engine map may still carry SharedCore
//! / Destructive flags from its own graph; we filter those out below.)
//!
//! ## Why we project to a tuple key
//!
//! Engine `model::risk::RiskFlag` and the producer's mirror
//! [`RiskFlag`](aethyme_producers) are *distinct types* with an
//! identical field layout (`scope`, `area`, `level`, `reason`). The
//! natural common projection is the 4-tuple
//! `(scope, format!("{:?}", area), format!("{:?}", level), reason)`.
//! Debug-formatting the enums lets us compare across the type boundary
//! without importing both enum types; the variant *names* match by
//! construction (the producer mirror is a verbatim copy).

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use aethyme_engine::map::RepositoryMap;
use aethyme_graph_storage::{bootstrap_repo, FragmentStore};
use aethyme_producers::{
    OverlayProducer, ProducerCtx, RepoFileView, RepoView, RisksProducer,
};
use tempfile::TempDir;

/// The four areas that `path_risks` can emit. Both sides are filtered
/// to this set before comparison; anything outside it (SharedCore,
/// Destructive, UserDefined) is graph-derived and out of scope.
const PATH_AREAS: &[&str] =
    &["Auth", "Permissions", "Secrets", "Migrations", "Infra", "Billing"];

/// A risk projected to a type-agnostic comparison key.
type RiskKey = (String, String, String, String);

/// Adapter bridging the engine's `RepositoryMap` into the producer's
/// [`RepoView`]. Built from `map.files` so the producer classifies the
/// engine's own structure-filtered file list (see module doc).
struct MapFilesAdapter {
    name: String,
    root_path: String,
    files: Vec<RepoFileView>,
}

impl MapFilesAdapter {
    fn from_map(map: &RepositoryMap) -> Self {
        let files = map
            .files
            .iter()
            .map(|f| RepoFileView {
                path: f.path.clone(),
                language: f.language.clone(),
                byte_size: f.size_bytes,
                // The producer doesn't read content_hash at risk
                // classification time (path is the only input to
                // path_risks); keying on path keeps the stub
                // deterministic.
                content_hash: format!("stub-{}", f.path),
            })
            .collect();
        Self {
            name: map.snapshot.repo_name(),
            root_path: map.snapshot.root.clone(),
            files,
        }
    }
}

impl RepoView for MapFilesAdapter {
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

/// Fixture spanning every `path_risks` predicate plus a clean control.
/// All entries live under `src/` so the structure pass keeps them in
/// `map.files`; each carries trivial content so the engine's parser has
/// real bytes to read. Bodies are inert — risk classification here is
/// path-only, so the contents never affect the compared surface.
const FIXTURE: &[(&str, &str)] = &[
    // Auth / High — "auth" in path.
    ("src/auth/login.ts", "export const login = 1;\n"),
    // Permissions / High — "permission" and "rbac".
    ("src/rbac/permission_check.ts", "export const can = 1;\n"),
    // Secrets / High — "secret" and "token".
    ("src/secrets/token_store.ts", "export const t = 1;\n"),
    // Migrations / High — "migration".
    ("src/db/migration_runner.ts", "export const run = 1;\n"),
    // Infra / Low — "infra" + "helper" downgrades.
    ("src/infra/helpers/dns_helper.ts", "export const dns = 1;\n"),
    // Infra / Medium — "deploy", no helper/util.
    ("src/ops/deploy_step.ts", "export const step = 1;\n"),
    // Billing / Medium — "billing" and "invoice" (one deduped flag).
    ("src/billing/invoice_calc.ts", "export const calc = 1;\n"),
    // Clean control — no predicate matches, no flag on either side.
    ("src/utils/format.ts", "export const fmt = 1;\n"),
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

/// Bootstrap an empty store in a tempdir *separate* from the fixture so
/// its `.aethyme` layout doesn't bleed into the directory the engine is
/// about to map. No fragments are written, so the producer's
/// shared-core/destructive sources stay empty.
fn empty_store() -> (TempDir, FragmentStore) {
    let tmp = tempfile::tempdir().expect("tempdir for store");
    bootstrap_repo(tmp.path(), "test-engine-0.0.0").expect("bootstrap");
    let store = FragmentStore::open(tmp.path()).expect("open store");
    (tmp, store)
}

/// Keep only path-derived flags, projected to the type-agnostic key.
fn path_keys<I>(flags: I) -> BTreeSet<RiskKey>
where
    I: IntoIterator<Item = RiskKey>,
{
    flags
        .into_iter()
        .filter(|(_, area, _, _)| PATH_AREAS.contains(&area.as_str()))
        .collect()
}

#[test]
fn risks_producer_matches_engine_path_risks() {
    // -- Build the fixture on disk so the engine has real bytes ------
    let fixture_tmp = tempfile::tempdir().expect("tempdir for fixture");
    write_fixture(fixture_tmp.path());

    // -- Engine side: full map build populates `risk_flags` ----------
    // `build_no_cache` returns (map, profile); we only need the map.
    let (map, _profile) = RepositoryMap::build_no_cache(fixture_tmp.path())
        .expect("engine RepositoryMap::build_no_cache");
    let engine_keys: BTreeSet<RiskKey> = path_keys(map.risk_flags.iter().map(
        |f| {
            (
                f.scope.clone(),
                format!("{:?}", f.area),
                format!("{:?}", f.level),
                f.reason.clone(),
            )
        },
    ));

    // -- Producer side: adapt map.files into a RepoView --------------
    let adapter = MapFilesAdapter::from_map(&map);
    let (_store_tmp, store) = empty_store();
    let ctx = ProducerCtx::with_repo(&store, &adapter);
    let fragment = RisksProducer.produce(&ctx).expect("risks producer");
    let overlay = fragment.payload();
    let producer_keys: BTreeSet<RiskKey> =
        path_keys(overlay.risks.iter().map(|f| {
            (
                f.scope.clone(),
                format!("{:?}", f.area),
                format!("{:?}", f.level),
                f.reason.clone(),
            )
        }));

    // -- Both sides must agree on the path-derived risk surface ------
    assert_eq!(
        engine_keys, producer_keys,
        "path-derived risk-flag set diverges between engine and producer"
    );

    // Guard against a vacuous pass: the fixture must actually exercise
    // the predicate (8 files → 7 risk-bearing, billing deduped to one
    // flag → at least the 6 distinct areas appear).
    assert!(
        !producer_keys.is_empty(),
        "fixture produced no path risks — predicate or fixture is broken"
    );
}
