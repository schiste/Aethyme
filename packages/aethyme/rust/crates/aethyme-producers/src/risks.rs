//! Risk overlay producer.
//!
//! Synthesizes the repo's risk plane — a sorted, de-duplicated list
//! of [`RiskFlag`]s flagging files that touch sensitive surfaces
//! (auth, secrets, migrations, ...), files that are shared core
//! (high import in-degree), and files containing destructively-named
//! functions. Replaces the engine's `passes/overlays.rs::detect_risks`
//! as part of the Phase 4.7 Variant C1 cutover
//! (`docs/architecture/phase4-7-c1-cutover.md`).
//!
//! ## The first fragment-reading producer
//!
//! Unlike [`StructureProducer`](crate::StructureProducer),
//! [`ConfigsProducer`](crate::ConfigsProducer),
//! [`DocsProducer`](crate::DocsProducer), and
//! [`PackageProducer`](crate::PackageProducer) — which classify only
//! the [`RepoView`](crate::RepoView)'s flat file listing — this
//! producer reads the *linked graph* off the store. Two of its three
//! risk sources need facts that only exist after linking:
//!
//! - **`path_risks`** comes from the [`RepoView`] file paths, same as
//!   the path-classification producers.
//! - **`shared_core_risks`** counts distinct importers per file by
//!   walking `Imports` edges across every fragment.
//! - **`destructive_risks`** scans function-node names for
//!   destructive verbs, grouped by the file each fragment belongs to.
//!
//! ## Two independent extraction pipelines — why risk facts are scoped
//!
//! The engine derives risks from its own `RepositoryMap`; this
//! producer derives them from the `aethyme-graph-indexer` fragment
//! store. These are **independent extraction pipelines** that need
//! not agree on the exact symbol/import sets they recover. The
//! path-derived subset (Auth / Permissions / Secrets / Migrations /
//! Infra / Billing) is intentionally kept byte-for-byte with the old
//! predicate, while `SharedCore` and `Destructive` are pinned by
//! `tests/risks_determinism.rs` plus the hand-built-fragment unit
//! tests at the bottom of this file.
//!
//! ## What this producer drops vs. the engine's pass
//!
//! `passes/overlays.rs` also exposes `graph_annotations`, which
//! re-projects risks (and docs/configs/entrypoints) into
//! `GraphAnnotation`s keyed by `NodeId`. That projection is an engine
//! concern bound to the `RepositoryMap` id scheme; the producer emits
//! the risk *facts* and leaves annotation assembly to a later layer.
//!
//! ## Thresholds & patterns — ported verbatim
//!
//! The shared-core thresholds and destructive verb list are copied
//! byte-for-byte from the engine pass so a future reader can diff the
//! two and see they match:
//!
//! - `SHARED_CORE_HIGH_THRESHOLD = 5` → in-degree ≥ 5 is `Medium`
//!   ("shared core").
//! - `SHARED_CORE_LOW_THRESHOLD = 3` → in-degree ≥ 3 (but < 5) is
//!   `Low` ("moderate coupling"); below 3 emits nothing.
//! - `DESTRUCTIVE_PATTERNS = [delete, drop, destroy, remove,
//!   truncate, reset, purge, wipe]`.
//!
//! ## Producer-local risk types
//!
//! The engine's `RiskArea` / `RiskLevel` / `RiskFlag` are *model*
//! types, not `aethyme-graph-schema` types — there is nothing in the
//! schema to reuse. We mirror them here with identical field order
//! (`scope, area, level, reason`) and identical enum-variant order,
//! because `detect_risks` ends in `.sort().dedup()` and the derived
//! [`Ord`] is what makes the on-disk bytes canonical. Reordering a
//! variant would silently change the sort and break determinism.
//!
//! ## Determinism
//!
//! Per `docs/architecture/graph-schema.md §5.4`, on-disk bytes must
//! be reproducible. `produce` collects all flags, then `sort` +
//! `dedup`s the vec — exactly mirroring `detect_risks` — so the
//! bincode encoding is stable regardless of `HashMap` iteration order
//! inside `shared_core_risks` / `destructive_risks`.

use std::collections::{HashMap, HashSet};

use aethyme_graph_schema::{EdgeKind, Node, NodeId, NodeKind};
use aethyme_graph_storage::{Fragment, FragmentStore, OverlayFragment};
use serde::{Deserialize, Serialize};

use crate::{OverlayProducer, ProducerCtx, ProducerError};

const SHARED_CORE_HIGH_THRESHOLD: usize = 5;
const SHARED_CORE_LOW_THRESHOLD: usize = 3;

const DESTRUCTIVE_PATTERNS: &[&str] = &[
    "delete", "drop", "destroy", "remove", "truncate", "reset", "purge", "wipe",
];

/// A coarse risk category. Producer-local mirror of the engine's
/// `model::risk::RiskArea`. Variant order is load-bearing: it feeds
/// the derived [`Ord`] that canonicalizes the overlay's bytes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskArea {
    Auth,
    Permissions,
    Secrets,
    Migrations,
    Infra,
    Billing,
    SharedCore,
    Destructive,
    UserDefined(String),
}

/// Severity of a flagged surface. Producer-local mirror of the
/// engine's `model::risk::RiskLevel`; variant order matters for the
/// derived [`Ord`] (Low < Medium < High).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// One flagged surface: a scope (a file path), its risk area and
/// level, and a human-readable reason. Field order is fixed at
/// `scope, area, level, reason` to match the engine's derived sort.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct RiskFlag {
    pub scope: String,
    pub area: RiskArea,
    pub level: RiskLevel,
    pub reason: String,
}

impl RiskFlag {
    pub fn new(
        scope: impl Into<String>,
        area: RiskArea,
        level: RiskLevel,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            scope: scope.into(),
            area,
            level,
            reason: reason.into(),
        }
    }
}

/// The payload of the `risks` overlay.
///
/// Single field — a sorted, de-duplicated list of [`RiskFlag`]s. The
/// `sort` + `dedup` in [`RisksProducer::produce`] is what makes the
/// bincode encoding deterministic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RisksOverlay {
    /// Every risk flag detected in the repo, sorted and de-duplicated
    /// for deterministic bincode encoding.
    pub risks: Vec<RiskFlag>,
}

/// Zero-sized handle that implements [`OverlayProducer`] for the
/// risks overlay. Stateless on purpose — see the analogous note on
/// [`DocsProducer`](crate::DocsProducer).
pub struct RisksProducer;

impl OverlayProducer for RisksProducer {
    type Payload = RisksOverlay;

    fn kind(&self) -> &'static str {
        "risks"
    }

    fn producer_version(&self) -> &'static str {
        "risks/0.1.0"
    }

    fn produce(
        &self,
        ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError> {
        let view = ctx.repo_view()?;

        // Source 1: path-based flags from the flat file listing.
        let mut risks: Vec<RiskFlag> = Vec::new();
        for file in view.files() {
            risks.extend(path_risks(&file.path));
        }

        // Sources 2 & 3 read the linked graph off the store.
        let fragments = read_all_fragments(ctx.store)?;
        risks.extend(shared_core_risks(&fragments));
        risks.extend(destructive_risks(&fragments));

        // Canonical ordering. `RiskFlag`'s derived `Ord` (scope,
        // area, level, reason) gives a total order; `dedup` after a
        // sort collapses the duplicates the engine also collapses.
        risks.sort();
        risks.dedup();

        let payload = RisksOverlay { risks };

        OverlayFragment::new(self.kind(), self.producer_version(), payload)
            .map_err(ProducerError::from)
    }
}

/// Read every fragment in the store into memory so the pure risk-
/// source helpers can borrow a `&[Fragment]`. The store API surfaces
/// `StoreOpenError` / `FragmentReadError` (not `StoreLookupError`),
/// so these map to [`ProducerError::Other`] rather than the typed
/// `Storage` bucket.
fn read_all_fragments(store: &FragmentStore) -> Result<Vec<Fragment>, ProducerError> {
    let paths = store
        .list_indexed_source_paths()
        .map_err(|e| ProducerError::Other(format!("list_indexed_source_paths failed: {e}")))?;
    let mut fragments = Vec::with_capacity(paths.len());
    for path in paths {
        let fragment = store
            .read_fragment(&path)
            .map_err(|e| ProducerError::Other(format!("read_fragment({path}) failed: {e}")))?;
        fragments.push(fragment);
    }
    Ok(fragments)
}

/// Flag a file by its *path shape* alone. Verbatim port of the
/// engine's `passes/overlays.rs::path_risks` — the predicate set, the
/// levels, and the reason strings are byte-for-byte identical so the
/// historical path-risk fixtures continue to hold.
fn path_risks(path: &str) -> Vec<RiskFlag> {
    let lower = path.to_ascii_lowercase();
    let mut risks = Vec::new();
    if lower.contains("auth") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Auth,
            RiskLevel::High,
            "authentication boundary",
        ));
    }
    if lower.contains("permission") || lower.contains("rbac") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Permissions,
            RiskLevel::High,
            "permission boundary",
        ));
    }
    if lower.contains("secret") || lower.contains("token") || lower.contains("credential") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Secrets,
            RiskLevel::High,
            "sensitive credential surface",
        ));
    }
    if lower.contains("migration") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Migrations,
            RiskLevel::High,
            "schema change area",
        ));
    }
    if lower.contains("deploy") || lower.contains("infra") || lower.contains("terraform") {
        let level = if lower.contains("helper") || lower.contains("util") {
            RiskLevel::Low
        } else {
            RiskLevel::Medium
        };
        risks.push(RiskFlag::new(
            path,
            RiskArea::Infra,
            level,
            "infrastructure surface",
        ));
    }
    if lower.contains("billing") || lower.contains("invoice") {
        risks.push(RiskFlag::new(
            path,
            RiskArea::Billing,
            RiskLevel::Medium,
            "billing logic",
        ));
    }
    risks
}

/// Flag files that are imported by many others ("shared core").
///
/// Port of the engine's `shared_core_risks`, translated from the
/// `RepositoryMap` edge model to the fragment edge model:
///
/// - Engine: `EdgeKind::Imports` && `edge.to.starts_with("file:")`.
/// - Producer: [`EdgeKind::Imports`] && the destination
///   [`NodeId`]'s [`kind`](NodeId::kind) is [`NodeKind::File`].
///
/// File-targeted import edges arise from *namespace* imports, which
/// the linker resolves to a `File` node (named imports resolve to
/// symbol nodes and are not counted — same as the engine's `file:`
/// prefix gate). In-degree is the count of **distinct importers**
/// (`src_id`) per imported file, mirroring the engine's
/// `HashSet<edge.from>`.
fn shared_core_risks(fragments: &[Fragment]) -> Vec<RiskFlag> {
    // dst file NodeId -> set of distinct importer NodeIds.
    let mut importers: HashMap<&NodeId, HashSet<&NodeId>> = HashMap::new();
    // dst file NodeId -> its path, for the risk scope.
    let mut file_id_to_path: HashMap<&NodeId, &str> = HashMap::new();

    for fragment in fragments {
        for node in fragment.nodes() {
            if let Node::File(file) = node {
                file_id_to_path.insert(file.id(), file.path());
            }
        }
        for edge in fragment.edges() {
            if edge.kind() == EdgeKind::Imports && edge.dst_id().kind() == NodeKind::File {
                importers
                    .entry(edge.dst_id())
                    .or_default()
                    .insert(edge.src_id());
            }
        }
    }

    let mut risks = Vec::new();
    for (file_id, from_set) in &importers {
        let in_degree = from_set.len();
        if in_degree < SHARED_CORE_LOW_THRESHOLD {
            continue;
        }
        // Fall back to the raw id string if no File node carried the
        // path — same defensive default as the engine.
        let scope = file_id_to_path
            .get(file_id)
            .copied()
            .unwrap_or_else(|| file_id.as_str());
        if in_degree >= SHARED_CORE_HIGH_THRESHOLD {
            risks.push(RiskFlag::new(
                scope,
                RiskArea::SharedCore,
                RiskLevel::Medium,
                format!("imported by {in_degree} files (shared core)"),
            ));
        } else {
            risks.push(RiskFlag::new(
                scope,
                RiskArea::SharedCore,
                RiskLevel::Low,
                format!("imported by {in_degree} files (moderate coupling)"),
            ));
        }
    }
    risks
}

/// Flag files containing destructively-named functions.
///
/// Port of the engine's `destructive_risks`. The engine grouped
/// matches by `function.file_path`; in the fragment model every node
/// belongs to exactly one fragment whose [`file_path`](Fragment::file_path)
/// *is* that source file, so we group by fragment path for free. Name
/// order within the joined reason follows `fragment.nodes()` order,
/// which `Fragment::new` sorts — so the string is stable run-to-run.
fn destructive_risks(fragments: &[Fragment]) -> Vec<RiskFlag> {
    let mut risks = Vec::new();
    for fragment in fragments {
        let mut names: Vec<&str> = Vec::new();
        for node in fragment.nodes() {
            if let Node::Function(_) = node {
                if let Some(name) = node.name() {
                    let lower = name.to_ascii_lowercase();
                    if DESTRUCTIVE_PATTERNS
                        .iter()
                        .any(|pattern| lower.contains(pattern))
                    {
                        names.push(name);
                    }
                }
            }
        }
        if !names.is_empty() {
            risks.push(RiskFlag::new(
                fragment.file_path(),
                RiskArea::Destructive,
                RiskLevel::Medium,
                format!("destructive operations: {}", names.join(", ")),
            ));
        }
    }
    risks
}

#[cfg(test)]
mod tests {
    use super::*;
    use aethyme_graph_schema::{
        Callable, Confidence, Edge, EdgeAttributes, File, Function, ParameterSignature, Source,
        SourceRange, Visibility,
    };

    const REPO: &str = "test-repo";

    // ── path_risks ────────────────────────────────────────────────

    #[test]
    fn path_risks_flags_auth_high() {
        let flags = path_risks("src/auth/login.ts");
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].area, RiskArea::Auth);
        assert_eq!(flags[0].level, RiskLevel::High);
        assert_eq!(flags[0].reason, "authentication boundary");
    }

    #[test]
    fn path_risks_infra_helper_is_low_not_medium() {
        // `infra` + `helper` downgrades to Low; plain infra is Medium.
        let helper = path_risks("infra/helpers/dns.ts");
        assert_eq!(helper[0].area, RiskArea::Infra);
        assert_eq!(helper[0].level, RiskLevel::Low);

        let plain = path_risks("infra/main.tf");
        assert_eq!(plain[0].level, RiskLevel::Medium);
    }

    #[test]
    fn path_risks_stacks_multiple_areas() {
        // A path matching several predicates emits one flag per match.
        let flags = path_risks("services/auth/billing/token_migration.ts");
        // RiskArea mirrors the engine and so deliberately does NOT
        // derive `Hash` — collect into a Vec and membership-test.
        let areas: Vec<RiskArea> = flags.iter().map(|f| f.area.clone()).collect();
        assert!(areas.contains(&RiskArea::Auth));
        assert!(areas.contains(&RiskArea::Secrets)); // "token"
        assert!(areas.contains(&RiskArea::Migrations));
        assert!(areas.contains(&RiskArea::Billing));
    }

    #[test]
    fn path_risks_clean_path_is_empty() {
        assert!(path_risks("src/utils/format.ts").is_empty());
    }

    // ── shared_core_risks ─────────────────────────────────────────

    /// Build an importer fragment: a `File` node for `src_path` plus
    /// `count` `Imports` edges all pointing at distinct importer files
    /// → the single `dst_path` File. We mint distinct *source* File
    /// nodes (one per edge) so in-degree counts distinct importers.
    fn importer_fragment(dst_path: &str, src_paths: &[&str]) -> Fragment {
        let dst = File::new(REPO, dst_path, "typescript", 100, "h").expect("dst node");
        let dst_id = dst.id().clone();

        let mut nodes: Vec<Node> = Vec::new();
        let mut edges: Vec<Edge> = Vec::new();
        for src_path in src_paths {
            let src = File::new(REPO, src_path, "typescript", 100, "h").expect("src node");
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
        // Carry the dst File node here too so file_id_to_path resolves
        // the scope to a path rather than the raw id.
        nodes.push(dst.into());
        Fragment::new("imports.ts", nodes, edges).expect("fragment")
    }

    #[test]
    fn shared_core_below_low_threshold_is_silent() {
        // in-degree 2 (< LOW=3) → no flag.
        let frag = importer_fragment("core/db.ts", &["a.ts", "b.ts"]);
        assert!(shared_core_risks(&[frag]).is_empty());
    }

    #[test]
    fn shared_core_at_low_threshold_is_low() {
        // in-degree 3 (== LOW, < HIGH) → Low / moderate coupling.
        let frag = importer_fragment("core/db.ts", &["a.ts", "b.ts", "c.ts"]);
        let flags = shared_core_risks(&[frag]);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].area, RiskArea::SharedCore);
        assert_eq!(flags[0].level, RiskLevel::Low);
        assert_eq!(flags[0].scope, "core/db.ts");
        assert_eq!(flags[0].reason, "imported by 3 files (moderate coupling)");
    }

    #[test]
    fn shared_core_at_high_threshold_is_medium() {
        // in-degree 5 (== HIGH) → Medium / shared core.
        let frag = importer_fragment("core/db.ts", &["a.ts", "b.ts", "c.ts", "d.ts", "e.ts"]);
        let flags = shared_core_risks(&[frag]);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].level, RiskLevel::Medium);
        assert_eq!(flags[0].reason, "imported by 5 files (shared core)");
    }

    #[test]
    fn shared_core_counts_distinct_importers_not_edges() {
        // Two edges from the SAME importer collapse to in-degree 1.
        let frag = importer_fragment("core/db.ts", &["a.ts", "a.ts"]);
        assert!(shared_core_risks(&[frag]).is_empty());
    }

    #[test]
    fn shared_core_ignores_non_file_import_targets() {
        // An Imports edge to a Function node must not be counted.
        let func = Function::new(
            REPO,
            "lib.ts",
            "helper",
            "fn helper()",
            Vec::<ParameterSignature>::new(),
            None,
            SourceRange::new(1, 1).unwrap(),
            Visibility::Public,
            true,
        )
        .expect("fn node");
        let func_id = func.id().clone();

        let mut nodes: Vec<Node> = vec![func.into()];
        let mut edges: Vec<Edge> = Vec::new();
        for src in ["a.ts", "b.ts", "c.ts"] {
            let s = File::new(REPO, src, "typescript", 1, "h").unwrap();
            edges.push(Edge::new(
                s.id().clone(),
                func_id.clone(),
                EdgeAttributes::Imports {
                    import_path: src.into(),
                    is_namespace: false,
                    is_default: false,
                    is_named: true,
                },
                Source::Code,
                Confidence::FULL,
            ));
            nodes.push(s.into());
        }
        let frag = Fragment::new("imports.ts", nodes, edges).unwrap();
        assert!(shared_core_risks(&[frag]).is_empty());
    }

    // ── destructive_risks ─────────────────────────────────────────

    /// Build a fragment whose source file carries the named functions.
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
    fn destructive_flags_matching_function_names() {
        let frag = fn_fragment("db/users.ts", &["deleteUser", "fetchUser"]);
        let flags = destructive_risks(&[frag]);
        assert_eq!(flags.len(), 1);
        assert_eq!(flags[0].area, RiskArea::Destructive);
        assert_eq!(flags[0].level, RiskLevel::Medium);
        assert_eq!(flags[0].scope, "db/users.ts");
        // Only the destructive name appears in the reason.
        assert_eq!(flags[0].reason, "destructive operations: deleteUser");
    }

    #[test]
    fn destructive_matches_every_pattern() {
        // One function per pattern, all in one file → all listed.
        let names = [
            "deleteIt",
            "dropTable",
            "destroyAll",
            "removeKey",
            "truncateLog",
            "resetState",
            "purgeCache",
            "wipeDisk",
        ];
        let frag = fn_fragment("ops.ts", &names);
        let flags = destructive_risks(&[frag]);
        assert_eq!(flags.len(), 1);
        // Every pattern matched → 8 names joined.
        assert_eq!(flags[0].reason.matches(", ").count(), 7);
    }

    #[test]
    fn destructive_clean_file_is_silent() {
        let frag = fn_fragment("ops.ts", &["fetch", "render", "compute"]);
        assert!(destructive_risks(&[frag]).is_empty());
    }
}
