//! Packages overlay producer.
//!
//! Synthesizes the repo's build/distribution plane — one
//! [`Package`] per manifest file discovered by the
//! [`RepoView`](crate::RepoView). Replaces the manifest-tagged
//! subset of the engine's `passes/configs.rs` as part of the Phase
//! 4.7 Variant C1 cutover
//! (`docs/architecture/phase4-7-c1-cutover.md`).
//!
//! ## Scope (commit 4.7.4b)
//!
//! This producer is intentionally *narrow*. The first cut emits
//! Packages only for the three manifest kinds that the engine's
//! [`classify_config_type`] tags as `"manifest"`:
//!
//! | Manifest        | `manifest_kind`  |
//! |-----------------|------------------|
//! | `Cargo.toml`    | `cargo`          |
//! | `package.json`  | `npm`            |
//! | `pyproject.toml`| `pyproject`      |
//!
//! That narrowing keeps this producer focused on manifest facts. Any
//! future widening of "manifest" semantics should update the
//! producer tests first so the deliberate scope remains obvious.
//!
//! The broader manifest set the engine *also* recognizes
//! (`project.godot`, `Dockerfile`, `tsconfig.json`, `turbo.json`,
//! `biome.json`, `deno.json[c]`, `pnpm-workspace.{yaml,yml}`) is
//! deferred to a follow-up commit *after* the engine pass deletion.
//! Those manifests carry different `config_type` tags (`project`,
//! `runtime`, `config`) in the engine, so they aren't in the original
//! narrow baseline for this producer — adding them here would silently widen
//! what this producer means by "package".
//!
//! ## What this producer drops vs. the engine's pass
//!
//! - **No manifest parsing.** The schema's [`Package::name`] is
//!   documented as "parsed from the manifest when present, falling
//!   back to the directory basename." Real parsing lands with the
//!   TOML/JSON parser layer in **Phase 4.8**; until then we use the
//!   directory-basename fallback unconditionally. For repo-root
//!   manifests (no parent directory), we fall back further to the
//!   repo name.
//! - **No `Defines` / `Configures` / `EntrypointFor` edges.** Same
//!   reasoning as the configs producer — the engine's fuzzy
//!   token-based matching wasn't load-bearing.
//! - **No area linking.** Returns in commit 4.7.7 via
//!   `populate_from_fragments`.
//!
//! ## Determinism
//!
//! Per `docs/architecture/graph-schema.md §5.4`, on-disk bytes
//! must be reproducible. This producer:
//!
//! - iterates `view.files()` in whatever order the view gives,
//! - constructs `Package`s eagerly (the constructor is fallible),
//! - sorts the resulting vec by `manifest_path()` so the bincode
//!   encoding is stable across runs. `manifest_path` is unique
//!   within a repo (one Package per manifest file), so this is a
//!   total order with no tiebreaker needed.
//!
//! `tests/packages_determinism.rs` exercises
//! [`assert_overlay_producer_is_deterministic`](crate::assert_overlay_producer_is_deterministic)
//! against a fixture spanning every emitted manifest kind, so any
//! future regression (a `HashMap`-keyed intermediate, a wall-clock
//! stamp, a parallel collect) trips here rather than at the §5.7
//! CI gate.
//!

use aethyme_graph_schema::Package;
use aethyme_graph_storage::OverlayFragment;
use serde::{Deserialize, Serialize};

use crate::{OverlayProducer, ProducerCtx, ProducerError};

/// The payload of the `packages` overlay.
///
/// Single field — a sorted list of [`Package`]s — because under
/// commit 4.7.4b's narrow scope the producer emits only the
/// node-level fact. Edges (intra-repo dependency graph, package
/// ↔ area links) come back in later commits or stay deferred.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackagesOverlay {
    /// Every package in the repo, sorted by `manifest_path()` for
    /// deterministic bincode encoding.
    pub packages: Vec<Package>,
}

/// Zero-sized handle that implements [`OverlayProducer`] for the
/// packages overlay. Stateless on purpose — see the analogous note
/// on [`StructureProducer`](crate::StructureProducer).
pub struct PackageProducer;

impl OverlayProducer for PackageProducer {
    type Payload = PackagesOverlay;

    fn kind(&self) -> &'static str {
        "packages"
    }

    fn producer_version(&self) -> &'static str {
        "packages/0.1.0"
    }

    fn produce(
        &self,
        ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError> {
        let view = ctx.repo_view()?;
        let repo = view.name();

        // Walk the view, classify each path, drop the misses, mint
        // a Package for each hit. `classify_manifest` is the single
        // point of policy — any future predicate change routes
        // through it.
        let mut packages: Vec<Package> = view
            .files()
            .iter()
            .filter_map(|f| {
                classify_manifest(&f.path)
                    .map(|kind| (f.path.as_str(), kind))
            })
            .map(|(manifest_path, kind)| {
                let dir = parent_dir_or_root(manifest_path);
                let name = package_name(dir, repo);
                Package::new(repo, dir, &name, manifest_path, kind)
            })
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| {
                ProducerError::Other(format!("Package::new failed: {e}"))
            })?;

        // Canonical ordering. `manifest_path` is unique within a
        // repo (one Package per manifest file) so this is a total
        // order — no tiebreaker needed.
        packages.sort_by(|a, b| a.manifest_path().cmp(b.manifest_path()));

        let payload = PackagesOverlay { packages };

        OverlayFragment::new(self.kind(), self.producer_version(), payload)
            .map_err(ProducerError::from)
    }
}

/// Classify a path as a package manifest, returning the
/// `manifest_kind` tag if accepted, or `None` if rejected.
///
/// Mirrors the engine's structure → configs predicate chain
/// (generated → cache → doc → operational config) followed by the
/// narrow 3-name basename equality that the engine's
/// `classify_config_type` ultimately tags as `"manifest"`.
///
/// The reject chain at the top is mostly defensive for the three
/// accepted basenames — `generated/Cargo.toml` could otherwise
/// sneak through. Keeping the chain identical to the configs
/// producer means policy changes only need to land in one place
/// per pass, never copy-pasted across producers.
fn classify_manifest(path: &str) -> Option<&'static str> {
    let lower = path.to_ascii_lowercase();

    // 1. Generated — engine's `is_generated`.
    if lower.contains("generated")
        || lower.ends_with(".min.js")
        || lower.ends_with(".lock")
    {
        return None;
    }

    // 2. Cache — engine's first branch in `classify_file`.
    if lower.contains("__pycache__")
        || lower.contains(".pytest_cache")
        || lower.contains(".mypy_cache")
    {
        return None;
    }

    // 3. Doc — engine's second branch.
    if lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || lower.ends_with(".rst")
        || lower.ends_with("readme")
    {
        return None;
    }

    // 4. Manifest kind — basename equality against the narrow
    // 3-name set. Engine's `classify_config_type` uses `ends_with`
    // for the same three names, but every path that reaches it
    // already passed `is_operational_config_path`'s basename gate,
    // so basename equality is the actual binding predicate.
    let file_name = lower.rsplit('/').next().unwrap_or(&lower);
    match file_name {
        "cargo.toml" => Some("cargo"),
        "package.json" => Some("npm"),
        "pyproject.toml" => Some("pyproject"),
        _ => None,
    }
}

/// Extract the manifest's parent directory, relative to the repo
/// root. Returns `"."` for repo-root manifests (e.g. top-level
/// `Cargo.toml`) so the [`Package::new`] non-empty-path invariant
/// is never violated.
///
/// The producer chose `"."` rather than the empty string because
/// `Package::new` rejects empty inputs; `"."` is the conventional
/// POSIX "current directory" marker and round-trips cleanly when
/// downstream consumers join it with sibling paths.
fn parent_dir_or_root(manifest_path: &str) -> &str {
    match manifest_path.rsplit_once('/') {
        Some((parent, _)) => parent,
        None => ".",
    }
}

/// Derive the package name from the manifest's parent directory,
/// falling back to the repo name when the manifest sits at the
/// repo root.
///
/// This is the Phase 4.7 placeholder — Phase 4.8 will parse the
/// manifest's own `name` field (cargo `[package].name`, npm
/// `"name"`, pyproject `[project].name`) and use the basename only
/// as a fallback when that field is absent or unparseable. The
/// schema's [`Package::name`] doc already promises that fallback
/// shape, so this code matches the documented behavior even while
/// the parsing layer is unwritten.
fn package_name(dir: &str, repo: &str) -> String {
    if dir == "." {
        // Repo-root manifest: there is no parent basename, so use
        // the repo name. Without this fallback, root manifests
        // would all get name="." which is technically non-empty
        // but semantically meaningless.
        return repo.to_string();
    }
    // dir basename. `rsplit('/').next()` returns the last segment
    // (or the whole string if no `/`). Both cases yield a sensible
    // name for nested packages like `packages/foo/Cargo.toml`
    // ("foo") or top-level dirs like `frontend/package.json`
    // ("frontend").
    dir.rsplit('/').next().unwrap_or(repo).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_accepts_cargo() {
        assert_eq!(classify_manifest("Cargo.toml"), Some("cargo"));
        assert_eq!(
            classify_manifest("crates/aethyme-engine/Cargo.toml"),
            Some("cargo")
        );
    }

    #[test]
    fn classify_accepts_npm() {
        assert_eq!(classify_manifest("package.json"), Some("npm"));
        assert_eq!(
            classify_manifest("packages/web/package.json"),
            Some("npm")
        );
    }

    #[test]
    fn classify_accepts_pyproject() {
        assert_eq!(classify_manifest("pyproject.toml"), Some("pyproject"));
        assert_eq!(
            classify_manifest("services/api/pyproject.toml"),
            Some("pyproject")
        );
    }

    #[test]
    fn classify_rejects_other_manifests() {
        // These are real manifests the engine recognizes, but their
        // `config_type` tag was not "manifest" in the old engine pass,
        // so keep them deferred rather than widening this producer
        // silently.
        assert!(classify_manifest("project.godot").is_none());
        assert!(classify_manifest("Dockerfile").is_none());
        assert!(classify_manifest("tsconfig.json").is_none());
        assert!(classify_manifest("turbo.json").is_none());
        assert!(classify_manifest("biome.json").is_none());
        assert!(classify_manifest("deno.json").is_none());
        assert!(classify_manifest("pnpm-workspace.yaml").is_none());
    }

    #[test]
    fn classify_rejects_non_manifest_source() {
        assert!(classify_manifest("src/lib.rs").is_none());
        assert!(classify_manifest("README.md").is_none());
        assert!(classify_manifest(".env").is_none());
    }

    #[test]
    fn classify_rejects_generated_paths() {
        // Defensive: a `Cargo.toml` under `generated/` would
        // otherwise be accepted by basename equality.
        assert!(classify_manifest("generated/Cargo.toml").is_none());
        assert!(classify_manifest("vendor.min.js").is_none());
        assert!(classify_manifest("Cargo.lock").is_none());
    }

    #[test]
    fn classify_rejects_cache_paths() {
        assert!(classify_manifest("src/__pycache__/foo.pyc").is_none());
    }

    #[test]
    fn classify_rejects_doc_paths() {
        assert!(classify_manifest("README.md").is_none());
    }

    #[test]
    fn parent_dir_handles_root_and_nested() {
        assert_eq!(parent_dir_or_root("Cargo.toml"), ".");
        assert_eq!(parent_dir_or_root("packages/foo/Cargo.toml"), "packages/foo");
        assert_eq!(parent_dir_or_root("frontend/package.json"), "frontend");
    }

    #[test]
    fn package_name_falls_back_to_repo_for_root() {
        assert_eq!(package_name(".", "myrepo"), "myrepo");
    }

    #[test]
    fn package_name_uses_dir_basename_for_nested() {
        assert_eq!(package_name("packages/foo", "myrepo"), "foo");
        assert_eq!(package_name("frontend", "myrepo"), "frontend");
    }
}
