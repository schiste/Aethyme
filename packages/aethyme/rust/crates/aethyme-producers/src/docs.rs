//! Docs overlay producer.
//!
//! Synthesizes the repo's documentation-file plane — one
//! [`NonCodeFile`] per documentation file discovered by the
//! [`RepoView`](crate::RepoView). Replaces the engine's
//! `passes/docs.rs` as part of the Phase 4.7 Variant C1 cutover
//! (`docs/architecture/phase4-7-c1-cutover.md`).
//!
//! ## The structural complement of the configs producer
//!
//! The engine's `passes/structure.rs::classify_file` runs a single
//! ordered chain — generated → cache → **doc** → config — and a file
//! lands in exactly one role. This producer claims the *doc* branch;
//! [`ConfigsProducer`](crate::ConfigsProducer) claims the *config*
//! branch. The two share the generated/cache gates verbatim, so a
//! path like `docs/generated-api.md` is excluded from **both**
//! overlays: `contains("generated")` fires before the doc branch is
//! ever reached. Mirroring that gate ordering — not just the accept
//! set — is what keeps both producers aligned with the fragment-era
//! file-role classifier.
//!
//! ## What this producer drops vs. the engine's pass
//!
//! Per the **Path 2** scope (the same narrowing applied to configs
//! in 4.7.4), this producer is intentionally *narrower* than the
//! engine pass it replaces:
//!
//! - **No `title`.** The engine read the first `# ` heading out of
//!   the file body. Extracting structured headings is a parser-layer
//!   job deferred to **Phase 4.8**; the schema's
//!   [`DocSection`](aethyme_graph_schema) is the structured home for
//!   it. Stamping a regex-scraped title onto `NonCodeFile` would
//!   lock in a downgrade — the same reasoning that kept per-key
//!   `ConfigValue` out of the configs producer.
//! - **No `doc_type` tag.** The engine stamped a semantic string
//!   ("readme", "architecture", "guide", "spec", ...). `NonCodeFile`
//!   carries [`NonCodeFormat`] instead — a parsing-class enum,
//!   committed forever. A consumer that needs the old semantic
//!   intent joins against a separate fact kind; we do not widen
//!   `NonCodeFormat` to carry a role.
//! - **No `Defines` / `Documents` / `References` edges.** The engine
//!   wired docs to areas, files, configs, and symbols with fuzzy
//!   token matching (identifier tokens, backtick-quoted explicit
//!   mentions). That heuristic produced a noisy fact set; lifting it
//!   across the cutover means re-litigating its design inside the new
//!   contract. We drop it wholesale and revisit if a downstream
//!   consumer actually asks for it.
//! - **No area linking.** Areas come back in commit 4.7.7 when
//!   `populate_from_fragments` is wired up; docs join the area plane
//!   there, not here.
//!
//! What remains is the **file-level identification fact**: "this
//! path is a documentation file, and here's the format you should
//! reach for to parse it."
//!
//! ## Predicate chain — mirrors the engine byte-for-byte
//!
//! The acceptance predicate is a copy-translation of the engine's
//! `passes/structure.rs` chain that sets `FileRole::Doc`:
//!
//! 1. **NOT generated** — `path` does not contain `"generated"`,
//!    not `.min.js`, not `.lock`.
//! 2. **NOT cache** — no `__pycache__`, `.pytest_cache`,
//!    `.mypy_cache` segment.
//! 3. **IS doc** — ends with `.md`, `.mdx`, `.rst`, or `readme`.
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
//! | Path shape          | Format            |
//! |---------------------|-------------------|
//! | `*.md`, `*.mdx`     | `Markdown`        |
//! | `*.rst`             | `Other("rst")`    |
//! | bare `README`       | `Plain`           |
//!
//! reStructuredText gets its own discriminant because it is a
//! distinct markup grammar from CommonMark — a future parser layer
//! must route them to different frontends. A bare `README` with no
//! recognized extension carries no structured grammar we can name,
//! so it falls to `Plain`.
//!
//! ## Determinism
//!
//! Per `docs/architecture/graph-schema.md §5.4`, on-disk bytes must
//! be reproducible. This producer constructs `NonCodeFile`s eagerly
//! then sorts the resulting vec by `path()` so the bincode encoding
//! is stable across runs. `tests/docs_determinism.rs` exercises
//! [`assert_overlay_producer_is_deterministic`](crate::assert_overlay_producer_is_deterministic)
//! against a fixture spanning every `NonCodeFormat` variant this
//! producer emits.

use aethyme_graph_schema::{NonCodeFile, NonCodeFormat};
use aethyme_graph_storage::OverlayFragment;
use serde::{Deserialize, Serialize};

use crate::{OverlayProducer, ProducerCtx, ProducerError};

/// The payload of the `docs` overlay.
///
/// Single field — a sorted list of [`NonCodeFile`]s — because under
/// Path 2 the producer is intentionally narrowed to the file-level
/// identification fact. Titles, `doc_type` tags, edges, and area
/// links are all out of scope for 4.7.5. If a future commit
/// re-introduces any of those, they get *new* fields here or
/// (preferably) a separate fragment kind — they never weaken the
/// meaning of an existing field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocsOverlay {
    /// Every documentation file in the repo, sorted by `path()` for
    /// deterministic bincode encoding.
    pub files: Vec<NonCodeFile>,
}

/// Zero-sized handle that implements [`OverlayProducer`] for the
/// docs overlay. Stateless on purpose — see the analogous note on
/// [`ConfigsProducer`](crate::ConfigsProducer).
pub struct DocsProducer;

impl OverlayProducer for DocsProducer {
    type Payload = DocsOverlay;

    fn kind(&self) -> &'static str {
        "docs"
    }

    fn producer_version(&self) -> &'static str {
        "docs/0.1.0"
    }

    fn produce(
        &self,
        ctx: &ProducerCtx<'_>,
    ) -> Result<OverlayFragment<Self::Payload>, ProducerError> {
        let view = ctx.repo_view()?;
        let repo = view.name();

        // Walk the view, classify each path, drop the misses, mint a
        // NonCodeFile for the hits. `classify_doc` is the single
        // point of policy — any future predicate change routes
        // through it.
        let mut files: Vec<NonCodeFile> = view
            .files()
            .iter()
            .filter_map(|f| classify_doc(&f.path).map(|format| (f.path.as_str(), format)))
            .map(|(path, format)| NonCodeFile::new(repo, path, format))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ProducerError::Other(format!("NonCodeFile::new failed: {e}")))?;

        // Canonical ordering. Path is unique within a repo so this
        // is a total order — no tiebreaker needed.
        files.sort_by(|a, b| a.path().cmp(b.path()));

        let payload = DocsOverlay { files };

        OverlayFragment::new(self.kind(), self.producer_version(), payload)
            .map_err(ProducerError::from)
    }
}

/// Classify a path as a documentation file, returning the parsing
/// format if accepted, or `None` if rejected.
///
/// Mirrors the engine's `passes/structure.rs` precedence:
/// generated → cache → doc. The format mapping picks the *parser
/// class* the caller should reach for; see the module-level
/// format-mapping table for the full policy.
fn classify_doc(path: &str) -> Option<NonCodeFormat> {
    let lower = path.to_ascii_lowercase();

    // 1. Generated — engine's `is_generated`. Must come first so
    // `docs/generated-api.md` lands in neither overlay.
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

    // 3. Doc — engine's second branch. This is the exact set the
    // configs producer *rejects* at its own step 3. `None` here
    // means the path is neither generated/cache nor a doc.
    format_for_doc_path(&lower)
}

/// Map a path to its doc parser class, or `None` if it is not a
/// documentation file.
///
/// Extension checks are ordered most-specific to least: `.md`/`.mdx`
/// before the bare-`readme` fallback so `readme.md` routes to
/// `Markdown` (matched by `.md`) rather than `Plain`.
fn format_for_doc_path(lower_path: &str) -> Option<NonCodeFormat> {
    if lower_path.ends_with(".md") || lower_path.ends_with(".mdx") {
        return Some(NonCodeFormat::Markdown);
    }
    if lower_path.ends_with(".rst") {
        return Some(NonCodeFormat::Other("rst".into()));
    }
    // Bare `README` (or any path ending in those 6 chars without a
    // recognized markup extension). The engine's `ends_with("readme")`
    // branch accepts it as a Doc; we have no structured grammar to
    // name, so it is Plain.
    if lower_path.ends_with("readme") {
        return Some(NonCodeFormat::Plain);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_rejects_generated_paths() {
        // `contains("generated")` must beat the doc branch, even for
        // an otherwise-Markdown path.
        assert!(classify_doc("docs/generated-api.md").is_none());
        assert!(classify_doc("vendor.min.js").is_none());
        assert!(classify_doc("Cargo.lock").is_none());
    }

    #[test]
    fn classify_rejects_cache_paths() {
        assert!(classify_doc(".pytest_cache/README.md").is_none());
        assert!(classify_doc("src/__pycache__/notes.md").is_none());
        assert!(classify_doc(".mypy_cache/3.11/readme").is_none());
    }

    #[test]
    fn classify_rejects_non_doc_files() {
        assert!(classify_doc("src/lib.rs").is_none());
        assert!(classify_doc("Cargo.toml").is_none());
        assert!(classify_doc("package.json").is_none());
        assert!(classify_doc(".github/workflows/ci.yml").is_none());
    }

    #[test]
    fn classify_maps_markdown() {
        assert_eq!(classify_doc("README.md"), Some(NonCodeFormat::Markdown));
        assert_eq!(
            classify_doc("docs/intro.mdx"),
            Some(NonCodeFormat::Markdown)
        );
        assert_eq!(
            classify_doc("docs/architecture/overview.md"),
            Some(NonCodeFormat::Markdown)
        );
    }

    #[test]
    fn classify_maps_rst() {
        assert_eq!(
            classify_doc("docs/setup.rst"),
            Some(NonCodeFormat::Other("rst".into()))
        );
    }

    #[test]
    fn classify_maps_bare_readme_to_plain() {
        assert_eq!(classify_doc("README"), Some(NonCodeFormat::Plain));
        assert_eq!(classify_doc("docs/README"), Some(NonCodeFormat::Plain));
    }

    #[test]
    fn classify_prefers_markdown_over_readme_fallback() {
        // `readme.md` ends with both ".md" and "readme"; the .md
        // branch must win so it routes to Markdown, not Plain.
        assert_eq!(classify_doc("README.md"), Some(NonCodeFormat::Markdown));
    }
}
