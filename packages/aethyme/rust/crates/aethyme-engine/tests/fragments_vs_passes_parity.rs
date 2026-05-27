//! Cutover gate for Phase 4.7.7: the gated `--from-fragments` build
//! path must reproduce the legacy passes build.
//!
//! `RepositoryMap::build_from_fragments` reuses the structure + code
//! passes verbatim but re-derives the **overlay** planes (docs, configs,
//! risks, and their edges) by running the `aethyme-producers` overlays
//! in-process against the on-disk graph, instead of the engine's legacy
//! `passes::{docs,configs,overlays}`. This test pins the two build paths
//! against one fixture so the cutover cannot silently drift the map the
//! daemon serves.
//!
//! ## What is compared — and at what fidelity
//!
//! The contract is **byte-equal where `§5.7` covers the field, structural
//! otherwise** (`docs/architecture/phase4-7-c1-cutover.md`, row 4.7.7):
//!
//! - **snapshot, areas, directories, files, classes, functions, symbols**
//!   — byte-equal. Both paths run the *same* `discover_repo` +
//!   `passes::structure` + `passes::code`, so these planes are identical
//!   by construction; `assert_eq!` is the strongest honest assertion.
//! - **docs / configs** — set-equal on the projection
//!   `(id, file_id, path, area_id)`. The producers are intentionally
//!   *lossy*: `DocsProducer` drops `title`/`doc_type`, `ConfigsProducer`
//!   drops `config_type`. `populate_from_fragments` stamps `""` for the
//!   dropped fields, so the full structs cannot be equal — but the
//!   *identity* of each overlay node (its id, the file it annotates, the
//!   area it rolls up to) must match exactly.
//! - **risk_flags** — set-equal on the 4-tuple `(scope, area, level,
//!   reason)` **filtered to the six path-derived areas**. The remaining
//!   areas (`SharedCore`/`Destructive`) read *graph content* — in-memory
//!   map edges/functions on the passes side vs on-disk fragments on the
//!   producer side — and the two extraction front-ends need not converge
//!   node-for-node. Pinning them would test pipeline convergence, not
//!   risk-policy parity. This mirrors the `risks_parity.rs` precedent in
//!   `aethyme-producers`.
//! - **edges** — fragments ⊆ passes. The fragments path assembles only
//!   structure + code edges; the passes path additionally folds in the
//!   docs/configs overlay edges (`Documents`, etc.). So the fragments
//!   edge set is a (proper, given the fixture's doc) subset.
//! - **graph** — excluded. It is a pure derivation of the rest of the
//!   map (`build_graph_with_profile`) and adds no independent signal.
//!
//! ## Why this test must NOT be vacuous
//!
//! `build_from_fragments` is *gated*: it only takes the fragments path
//! when `<root>/.aethyme/graph/` exists, else it silently falls back to
//! the legacy passes. So the fixture **bootstraps the store at the same
//! root the engine maps** — not a side tempdir — and asserts the graph
//! dir is present before building. Two further guards make a silent
//! fallback impossible to pass off as success:
//!
//! 1. **Positive proof the fragments branch ran**: the docs *pass*
//!    always fills a non-empty `title` (it reads the file's H1), while
//!    the docs *producer* drops it. So every fragments-build doc has an
//!    empty `title`/`doc_type`, and at least one passes-build doc does
//!    not. If the gate had fallen through to passes, the fragments map
//!    would carry titles and that assertion would trip.
//! 2. **Non-emptiness**: the fixture is asserted to yield ≥1 doc, ≥1
//!    config, and ≥1 path risk, so a degenerate empty-vs-empty match
//!    cannot pass silently.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use aethyme_engine::map::RepositoryMap;
use aethyme_graph_storage::bootstrap_repo;
use tempfile::TempDir;

/// The risk areas derived purely from a file's path shape — the only
/// areas both build paths compute from the same structure-filtered file
/// list. See the module doc for why content-derived areas are excluded.
const PATH_AREAS: &[&str] =
    &["Auth", "Permissions", "Secrets", "Migrations", "Infra", "Billing"];

/// Identity projection of an overlay (doc/config) node: the fields both
/// build paths must agree on. The lossy fields (`title`, `doc_type`,
/// `config_type`) are deliberately absent.
type OverlayKey = (String, String, String, String);

/// Type-agnostic risk projection, mirroring `risks_parity.rs`.
type RiskKey = (String, String, String, String);

fn write_file(root: &Path, rel: &str, body: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("rel has a parent"))
        .expect("create fixture dirs");
    fs::write(path, body).expect("write fixture file");
}

/// Seed a small but predicate-spanning repo:
///
/// - `src/auth/login.ts`            → Auth/High path risk, + an import
///   edge into `src/utils/format.ts` (code plane has real content when
///   grammars are available; equally empty on both sides when not).
/// - `services/billing/invoice.ts`  → Billing/Medium path risk.
/// - `infra/helpers/dns.ts`         → Infra path risk (Low downgrade).
/// - `src/utils/format.ts`          → clean control (no path risk).
/// - `docs/overview.md`             → a doc whose H1 the passes lift
///   into `title` (the positive-proof lever).
/// - `package.json`                 → a config.
fn seed_fixture(root: &Path) {
    write_file(
        root,
        "src/auth/login.ts",
        "import { hash } from '../utils/format';\n\
         export function login() {\n  return hash('secret');\n}\n",
    );
    write_file(
        root,
        "src/utils/format.ts",
        "export function hash(s: string) {\n  return s;\n}\n",
    );
    write_file(
        root,
        "services/billing/invoice.ts",
        "export function invoice() {}\n",
    );
    write_file(
        root,
        "infra/helpers/dns.ts",
        "export function resolve() {}\n",
    );
    write_file(
        root,
        "docs/overview.md",
        "# Project Overview\n\nA fixture repository.\n",
    );
    write_file(root, "package.json", "{\n  \"name\": \"fixture\"\n}\n");
}

fn doc_keys(map: &RepositoryMap) -> BTreeSet<OverlayKey> {
    map.docs
        .iter()
        .map(|d| {
            (
                d.id.clone(),
                d.file_id.clone(),
                d.path.clone(),
                format!("{:?}", d.area_id),
            )
        })
        .collect()
}

fn config_keys(map: &RepositoryMap) -> BTreeSet<OverlayKey> {
    map.configs
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.file_id.clone(),
                c.path.clone(),
                format!("{:?}", c.area_id),
            )
        })
        .collect()
}

/// Risk flags projected to the comparable 4-tuple and filtered to the
/// path-derived areas. Both sides classify the *same* file list, so this
/// surface must match exactly.
fn path_risk_keys(map: &RepositoryMap) -> BTreeSet<RiskKey> {
    map.risk_flags
        .iter()
        .map(|f| {
            (
                f.scope.clone(),
                format!("{:?}", f.area),
                format!("{:?}", f.level),
                f.reason.clone(),
            )
        })
        .filter(|(_, area, _, _)| PATH_AREAS.contains(&area.as_str()))
        .collect()
}

#[test]
fn fragments_build_matches_passes_build() {
    let tmp = TempDir::new().expect("tempdir");
    let root = tmp.path();
    seed_fixture(root);

    // Materialize `<root>/.aethyme/graph/` so the `--from-fragments`
    // gate fires. Without this the build silently falls back to passes
    // and the whole test would compare passes-to-passes.
    bootstrap_repo(root, "test-engine-0.0.0").expect("bootstrap store");
    assert!(
        root.join(".aethyme").join("graph").is_dir(),
        "bootstrap must create the graph dir, else the gate is vacuous"
    );

    let (passes, _) =
        RepositoryMap::build_no_cache(root).expect("passes build");
    let (frag, _) =
        RepositoryMap::build_from_fragments(root).expect("fragments build");

    // -- Structure + code planes: byte-identical -------------------
    assert_eq!(frag.snapshot, passes.snapshot, "snapshot diverged");
    assert_eq!(frag.areas, passes.areas, "areas diverged");
    assert_eq!(
        frag.directories, passes.directories,
        "directories diverged"
    );
    assert_eq!(frag.files, passes.files, "files diverged");
    assert_eq!(frag.classes, passes.classes, "classes diverged");
    assert_eq!(frag.functions, passes.functions, "functions diverged");
    assert_eq!(frag.symbols, passes.symbols, "symbols diverged");

    // -- Overlay planes: identity (key) equality ------------------
    assert_eq!(
        doc_keys(&frag),
        doc_keys(&passes),
        "doc node identity set diverged"
    );
    assert_eq!(
        config_keys(&frag),
        config_keys(&passes),
        "config node identity set diverged"
    );
    assert_eq!(
        path_risk_keys(&frag),
        path_risk_keys(&passes),
        "path-risk set diverged"
    );

    // -- Edges: fragments drop overlay edges ⇒ subset -------------
    let frag_edges: BTreeSet<_> = frag.edges.iter().cloned().collect();
    let passes_edges: BTreeSet<_> = passes.edges.iter().cloned().collect();
    assert!(
        frag_edges.is_subset(&passes_edges),
        "fragments edges must be a subset of passes edges"
    );

    // -- Non-vacuousness: the fixture actually exercises overlays --
    assert!(!frag.docs.is_empty(), "fixture must yield >=1 doc");
    assert!(!frag.configs.is_empty(), "fixture must yield >=1 config");
    assert!(
        !path_risk_keys(&frag).is_empty(),
        "fixture must yield >=1 path risk"
    );

    // -- Positive proof the fragments branch (not passes) ran ------
    // The docs producer drops title+doc_type; the docs PASS always
    // fills them from the file. So a genuine fragments build leaves
    // every doc blank, while the passes build carries a real title.
    assert!(
        frag.docs
            .iter()
            .all(|d| d.title.is_empty() && d.doc_type.is_empty()),
        "fragments docs must have empty title+doc_type"
    );
    assert!(
        passes.docs.iter().any(|d| !d.title.is_empty()),
        "passes docs must carry a real title (proves the planes differ)"
    );
    assert!(
        frag.configs.iter().all(|c| c.config_type.is_empty()),
        "fragments configs must have empty config_type"
    );
}
