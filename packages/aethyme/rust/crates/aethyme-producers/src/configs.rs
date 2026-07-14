//! Configs overlay producer.
//!
//! Synthesizes the repo's non-code-file plane — one
//! [`NonCodeFile`] per operational config file discovered by the
//! [`RepoView`](crate::RepoView). Replaces the engine's
//! `passes/configs.rs` as part of the Phase 4.7 Variant C1 cutover
//! (`docs/architecture/phase4-7-c1-cutover.md`).
//!
//! ## What this producer drops vs. the engine's pass
//!
//! Per the **Path 2** scope agreed for commit 4.7.4, this producer
//! is intentionally *narrower* than the engine pass it replaces:
//!
//! - **No `Configures` / `EntrypointFor` edges.** The engine used
//!   fuzzy token-based matching (`config_tokens`, manifest/godot/
//!   package entrypoint parsing) to wire configs to the code they
//!   reference. That heuristic was never load-bearing — it produced
//!   a sparse, noisy fact set — and lifting it across the cutover
//!   means re-litigating its design decisions inside the new
//!   contract. We drop it here and revisit if a downstream consumer
//!   actually asks for it.
//! - **No per-key `ConfigValue` nodes.** Per-key parsing is
//!   deferred to **Phase 4.8**, which gets a real parser layer
//!   (TOML/JSON first, YAML later). The schema's `ConfigValue`
//!   already exists and is a strict, structured fact; populating
//!   it with regex output would lock in a downgrade.
//! - **No `config_type` tag.** The engine stamped a semantic
//!   string ("manifest", "runtime", ...). `NonCodeFile` carries
//!   [`NonCodeFormat`] instead — a parsing-class enum, committed
//!   forever. See the module-level §"Format mapping" note below.
//! - **No area linking.** Areas come back in commit 4.7.7 when
//!   `populate_from_fragments` is wired up; configs join the area
//!   plane there, not here.
//!
//! What remains is the **file-level identification fact**: "this
//! path is an operational config file, and here's the format you
//! should reach for to parse it." That's the minimum useful slice
//! for downstream queries and the maximum we can ship without
//! re-litigating engine-era design decisions.
//!
//! ## Predicate chain — mirrors the engine byte-for-byte
//!
//! The acceptance predicate is a copy-translation of the engine's
//! `passes/structure.rs` chain that sets `FileRole::Config`:
//!
//! 1. **NOT generated** — `path` does not contain `"generated"`,
//!    not `.min.js`, not `.lock`.
//! 2. **NOT cache** — no `__pycache__`, `.pytest_cache`,
//!    `.mypy_cache` segment.
//! 3. **NOT doc** — not `.md`/`.mdx`/`.rst`/`readme`.
//! 4. **IS operational config path** — see `is_operational_config_path`
//!    below; matches a fixed allow-list of exact filenames, dotfile
//!    prefixes, and path-prefix-gated extensions.
//!
//! The predicate remains a direct port of the old engine branch so
//! historical fixture expectations continue to hold after the pass
//! pipeline deletion.
//!
//! ## Format mapping — parsing-class, not semantic-class
//!
//! [`NonCodeFormat`] is "which parser to reach for", not "what the
//! file means":
//!
//! | Path shape                              | Format            |
//! |-----------------------------------------|-------------------|
//! | `*.toml`                                | `Toml`            |
//! | `*.yaml`, `*.yml`                       | `Yaml`            |
//! | `*.json`, `*.jsonc`                     | `Json`            |
//! | `project.godot`                         | `Other("godot")`  |
//! | `dockerfile`, `docker-compose.*`        | `Plain`           |
//! | `.env`, `.gitignore`, other dotfiles    | `Plain`           |
//!
//! A consumer that needs the old semantic intent ("is this a
//! manifest? a runtime config?") joins against a separate fact
//! kind — we do not widen `NonCodeFormat` to carry that role.
//!
//! ## Determinism
//!
//! Per `docs/architecture/graph-schema.md §5.4`, on-disk bytes
//! must be reproducible. This producer:
//!
//! - iterates `view.files()` in whatever order the view gives,
//! - constructs `NonCodeFile`s eagerly (the constructor is
//!   fallible — sort-by-path before construction wouldn't change
//!   the eventual ordering since the path *is* the sort key),
//! - sorts the resulting vec by `path()` so the bincode encoding
//!   is stable across runs.
//!
//! `tests/configs_determinism.rs` exercises
//! [`assert_overlay_producer_is_deterministic`](crate::assert_overlay_producer_is_deterministic)
//! against a fixture spanning every `NonCodeFormat` variant the
//! producer emits, so any future regression (a `HashMap`-keyed
//! intermediate, a wall-clock stamp, a parallel collect) trips
//! here rather than at the §5.7 CI gate.

use aethyme_graph_schema::{NonCodeFile, NonCodeFormat};
use aethyme_graph_storage::OverlayFragment;
use serde::{Deserialize, Serialize};

use crate::{OverlayProducer, ProducerCtx, ProducerError};

/// The payload of the `configs` overlay.
///
/// Single field — a sorted list of [`NonCodeFile`]s — because
/// under Path 2 the producer is intentionally narrowed to the
/// file-level identification fact. Edges, per-key values, and
/// `config_type` tags are all out of scope for 4.7.4. If a future
/// commit re-introduces any of those, they get *new* fields here
/// or (preferably) a separate fragment kind — they never weaken
/// the meaning of an existing field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigsOverlay {
    /// Every operational config file in the repo, sorted by
    /// `path()` for deterministic bincode encoding.
    pub files: Vec<NonCodeFile>,
}

/// Zero-sized handle that implements [`OverlayProducer`] for the
/// configs overlay. Stateless on purpose — see the analogous note
/// on [`StructureProducer`](crate::StructureProducer).
pub struct ConfigsProducer;

impl OverlayProducer for ConfigsProducer {
    type Payload = ConfigsOverlay;

    fn kind(&self) -> &'static str {
        "configs"
    }

    fn producer_version(&self) -> &'static str {
        "configs/0.1.0"
    }

    fn produce(
        &self,
        ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError> {
        let view = ctx.repo_view()?;
        let repo = view.name();

        // Walk the view, classify each path, drop the misses, mint
        // a NonCodeFile for the hits. `classify_non_code` is the
        // single point of policy — any future predicate change
        // routes through it.
        let mut files: Vec<NonCodeFile> = view
            .files()
            .iter()
            .filter_map(|f| classify_non_code(&f.path).map(|format| (f.path.as_str(), format)))
            .map(|(path, format)| NonCodeFile::new(repo, path, format))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ProducerError::Other(format!("NonCodeFile::new failed: {e}")))?;

        // Canonical ordering. Path is unique within a repo so this
        // is a total order — no tiebreaker needed.
        files.sort_by(|a, b| a.path().cmp(b.path()));

        let payload = ConfigsOverlay { files };

        OverlayFragment::new(self.kind(), self.producer_version(), payload)
            .map_err(ProducerError::from)
    }
}

/// Classify a path as a non-code file, returning the parsing
/// format if accepted, or `None` if rejected.
///
/// Mirrors the engine's `passes/structure.rs` precedence:
/// generated → cache → doc → operational config → (else, not a
/// config). The format mapping picks the *parser class* the
/// caller should reach for; see the module-level format-mapping
/// table for the full policy.
fn classify_non_code(path: &str) -> Option<NonCodeFormat> {
    let lower = path.to_ascii_lowercase();

    // 1. Generated — engine's `is_generated`.
    if lower.contains("generated") || lower.ends_with(".min.js") || lower.ends_with(".lock") {
        return None;
    }

    // 2. Cache — engine's first branch in `classify_file`.
    if lower.contains("__pycache__")
        || lower.contains(".pytest_cache")
        || lower.contains(".mypy_cache")
    {
        return None;
    }

    // 3. Doc — engine's second branch. Markdown is owned by the
    // future docs producer (commit 4.7.5), not this one.
    if lower.ends_with(".md")
        || lower.ends_with(".mdx")
        || lower.ends_with(".rst")
        || lower.ends_with("readme")
    {
        return None;
    }

    // 4. Operational config — engine's third branch.
    if !is_operational_config_path(&lower) {
        return None;
    }

    Some(format_for_config_path(&lower))
}

/// Allow-list predicate for operational config paths. Mirrors
/// engine's `passes/structure.rs::is_operational_config_path`
/// byte-for-byte.
fn is_operational_config_path(lower_path: &str) -> bool {
    let file_name = lower_path.rsplit('/').next().unwrap_or(lower_path);

    if matches!(
        file_name,
        "cargo.toml"
            | "package.json"
            | "pyproject.toml"
            | "project.godot"
            | "dockerfile"
            | "docker-compose.yml"
            | "docker-compose.yaml"
            | "tsconfig.json"
            | "jsconfig.json"
            | "turbo.json"
            | "biome.json"
            | "deno.json"
            | "deno.jsonc"
            | "pnpm-workspace.yaml"
            | "pnpm-workspace.yml"
    ) {
        return true;
    }

    if file_name.starts_with(".env")
        || file_name.starts_with(".gitignore")
        || file_name.starts_with(".dockerignore")
        || file_name.starts_with(".npmrc")
        || file_name.starts_with(".yarnrc")
        || file_name.starts_with(".prettierrc")
        || file_name.starts_with(".eslintrc")
        || file_name.starts_with(".stylelintrc")
        || file_name.starts_with(".editorconfig")
    {
        return true;
    }

    if lower_path.starts_with(".github/workflows/")
        || lower_path.contains("/.github/workflows/")
        || lower_path.starts_with("config/")
        || lower_path.contains("/config/")
        || lower_path.starts_with("configs/")
        || lower_path.contains("/configs/")
        || lower_path.starts_with("deploy/")
        || lower_path.contains("/deploy/")
        || lower_path.starts_with("deployment/")
        || lower_path.contains("/deployment/")
        || lower_path.starts_with("infra/")
        || lower_path.contains("/infra/")
        || lower_path.starts_with("k8s/")
        || lower_path.contains("/k8s/")
        || lower_path.starts_with("helm/")
        || lower_path.contains("/helm/")
    {
        return lower_path.ends_with(".yaml")
            || lower_path.ends_with(".yml")
            || lower_path.ends_with(".toml")
            || lower_path.ends_with(".json");
    }

    lower_path.ends_with(".env")
}

/// Map an already-accepted config path to its parser class.
///
/// Caller MUST have already passed the path through
/// `is_operational_config_path` — this helper does not re-validate.
/// Extension checks are ordered most-specific to least so
/// `docker-compose.yml` (matched by allow-list) still routes to
/// `Yaml`, while bare `Dockerfile` (no extension) falls to
/// `Plain`.
fn format_for_config_path(lower_path: &str) -> NonCodeFormat {
    let file_name = lower_path.rsplit('/').next().unwrap_or(lower_path);

    // Godot's project file gets its own discriminant — it's an
    // INI-shaped format that neither Toml nor Plain models well,
    // and downstream consumers may special-case it.
    if file_name == "project.godot" {
        return NonCodeFormat::Other("godot".into());
    }

    if lower_path.ends_with(".toml") {
        return NonCodeFormat::Toml;
    }
    if lower_path.ends_with(".yaml") || lower_path.ends_with(".yml") {
        return NonCodeFormat::Yaml;
    }
    if lower_path.ends_with(".json") || lower_path.ends_with(".jsonc") {
        return NonCodeFormat::Json;
    }

    // Dockerfile, docker-compose without an extension wouldn't hit
    // here (allow-list lowercases match exactly), dotfiles like
    // `.env`/`.gitignore`/`.prettierrc`, and bare `.env` at repo
    // root all land in Plain. The schema's Plain variant is the
    // catch-all for "we know it's a config, we don't know a
    // structured parser for it."
    NonCodeFormat::Plain
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rejects_generated_paths() {
        assert!(classify_non_code("src/generated/api.ts").is_none());
        assert!(classify_non_code("vendor.min.js").is_none());
        assert!(classify_non_code("Cargo.lock").is_none());
    }

    #[test]
    fn classify_rejects_cache_paths() {
        assert!(classify_non_code("src/__pycache__/foo.pyc").is_none());
        assert!(classify_non_code(".pytest_cache/v/cache/nodeids").is_none());
        assert!(classify_non_code(".mypy_cache/3.11/foo.json").is_none());
    }

    #[test]
    fn classify_rejects_doc_paths() {
        assert!(classify_non_code("README.md").is_none());
        assert!(classify_non_code("docs/intro.mdx").is_none());
        assert!(classify_non_code("docs/setup.rst").is_none());
        assert!(classify_non_code("README").is_none());
    }

    #[test]
    fn classify_rejects_non_config_source() {
        assert!(classify_non_code("src/lib.rs").is_none());
        assert!(classify_non_code("content/object-templates.json").is_none());
    }

    #[test]
    fn classify_maps_toml() {
        assert_eq!(classify_non_code("Cargo.toml"), Some(NonCodeFormat::Toml));
        assert_eq!(
            classify_non_code("pyproject.toml"),
            Some(NonCodeFormat::Toml)
        );
    }

    #[test]
    fn classify_maps_json() {
        assert_eq!(classify_non_code("package.json"), Some(NonCodeFormat::Json));
        assert_eq!(
            classify_non_code("tsconfig.json"),
            Some(NonCodeFormat::Json)
        );
        assert_eq!(classify_non_code("deno.jsonc"), Some(NonCodeFormat::Json));
    }

    #[test]
    fn classify_maps_yaml() {
        assert_eq!(
            classify_non_code(".github/workflows/ci.yml"),
            Some(NonCodeFormat::Yaml)
        );
        assert_eq!(
            classify_non_code("docker-compose.yaml"),
            Some(NonCodeFormat::Yaml)
        );
    }

    #[test]
    fn classify_maps_godot() {
        assert_eq!(
            classify_non_code("project.godot"),
            Some(NonCodeFormat::Other("godot".into()))
        );
    }

    #[test]
    fn classify_maps_dockerfile_and_dotfiles_to_plain() {
        assert_eq!(classify_non_code("Dockerfile"), Some(NonCodeFormat::Plain));
        assert_eq!(classify_non_code(".env"), Some(NonCodeFormat::Plain));
        assert_eq!(classify_non_code(".gitignore"), Some(NonCodeFormat::Plain));
        assert_eq!(classify_non_code(".prettierrc"), Some(NonCodeFormat::Plain));
    }

    #[test]
    fn classify_gates_extensions_on_path_prefix() {
        // .json outside an operational dir → not a config.
        assert!(classify_non_code("content/object-templates.json").is_none());
        // Same extension under config/ → accepted.
        assert_eq!(
            classify_non_code("config/app.json"),
            Some(NonCodeFormat::Json)
        );
        assert_eq!(
            classify_non_code("infra/terraform.toml"),
            Some(NonCodeFormat::Toml)
        );
    }
}
