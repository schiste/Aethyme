//! Linker pass (Phase 4.5): resolve `UnresolvedSymbol` placeholders
//! to concrete `NodeId`s by consulting the global symbol index.
//!
//! ## How this fits into the pipeline
//!
//! Phase 4.4 added stage 1 of cross-file edge resolution: each
//! language indexer emits one `UnresolvedSymbol` placeholder per
//! imported name, plus an `Imports` edge from the importing file to
//! the placeholder. Call-capable indexers can also emit `Calls`
//! edges to placeholders. The placeholder is a dead-end until this
//! module walks every fragment on disk, looks up each placeholder
//! against the global symbol index, and rewrites supported edge
//! targets to point at real concrete nodes when a unique resolution
//! exists.
//!
//! ## What it is NOT
//!
//! - Not a name-binding pass. Python-level semantics like `from a
//!   import x; x = y; x()` would need full scope analysis. The
//!   linker resolves calls through import bindings and same-module
//!   symbols only when those facts are unambiguous.
//! - Not a type-resolution pass. We resolve to whatever the global
//!   index says exists — the only consumer hint we use is the
//!   placeholder's `expected_kind` (`Some(Module)` for plain
//!   `import X`).
//! - Not a cross-language resolver. Today we match by name across
//!   all languages, but most cross-language imports (e.g. a TS file
//!   "importing" a Rust crate via WASM bindings) won't have
//!   symmetric SymbolRecord shapes anyway. When that work lands it
//!   will set the schema's `Edge.language_boundary` flag via the
//!   existing `with_language_boundary(BindingKind)` builder.
//!
//! ## Resolution policy
//!
//! For each placeholder, look at every incoming edge (in the same
//! fragment — placeholders are local artifacts of the importing
//! file). For each supported edge:
//!
//! 1. If the edge says `is_namespace=true` (whole-module import):
//!    look up the import_path as a module — match a SymbolRecord
//!    whose module name equals `import_path` and kind is `Module`,
//!    or (fallback) whose synthesized module is a *file-level* node
//!    matching the path. The first non-ambiguous hit wins.
//! 2. If the edge says `is_named=true`:
//!    split `import_path` on the last `.` into `(module_part,
//!    symbol_part)`. Look up symbol_part in module_part's shard
//!    first (fast). If exactly one match, resolve.
//! 3. For `Calls` edges: first reuse exact local import bindings
//!    (`from util import helper as h; h()`), then receiver-qualified
//!    targets whose receiver is itself a resolved local import
//!    binding (`import util; util.helper()`), then same-module bare
//!    calls. Calls do not use whole-repo name-only fallback because
//!    that can confuse callback parameters or locals with unrelated
//!    symbols.
//! 4. If no resolution is found OR multiple ambiguous candidates
//!    exist, leave the edge pointing at the placeholder. The
//!    placeholder survives.
//!
//! "Multiple ambiguous candidates" is conservative on purpose. A
//! future commit can layer smarter heuristics (longest module-path
//! prefix match, language-of-importer filter) on top without
//! changing the wire format.
//!
//! ## Orphan removal
//!
//! After all edges in a fragment have been considered, any
//! placeholder whose every incoming edge was rewritten is now
//! orphaned (nothing points at it). Orphans are removed from the
//! fragment's node list. Placeholders that retained at least one
//! incoming edge (resolution failed) survive.
//!
//! ## Parallelism + atomicity
//!
//! Linking is naturally parallel: each fragment is rewritten
//! independently. We use rayon over the source-path list. Writes
//! go through `write_fragment`'s atomic-rename so a SIGKILL
//! mid-link can't leave fragments in a torn state.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rayon::prelude::*;

use aethyme_graph_schema::{Edge, EdgeAttributes, EdgeKind, Node, NodeId, NodeKind};
use aethyme_graph_storage::{
    Fragment, FragmentBuildError, FragmentReadError, FragmentStore, FragmentWriteError,
    IndexShardReadError, StoreOpenError, SymbolRecord, write_fragment,
};

use crate::context::IndexerContext;

/// Public summary of one `link_repo` invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LinkSummary {
    pub fragments_visited: usize,
    pub fragments_rewritten: usize,
    pub placeholders_seen: usize,
    pub placeholders_resolved: usize,
    pub edges_rewritten: usize,
    pub orphans_removed: usize,
}

/// One-shot helper: walk every fragment in the repo's graph store,
/// resolve `UnresolvedSymbol` placeholders, rewrite + persist.
///
/// Only `ctx.repo_root()` is consulted — `repo_name` and
/// `engine_version` are inputs to indexing (they shape NodeIds and
/// the bootstrap version file), but the linker operates purely on
/// what's already on disk, so it ignores them. Callers that don't
/// have an `IndexerContext` lying around should prefer
/// [`link_repo_path`] (path in, no context construction needed) or
/// [`link_with_store`] (already-opened store).
///
/// Idempotent: re-running on an already-linked repo is a near-no-op
/// (the only placeholders left are the unresolvable ones, which
/// resolve again to the same outcome). Tests cover this.
pub fn link_repo(ctx: &IndexerContext) -> Result<LinkSummary, LinkError> {
    let store = FragmentStore::open(ctx.repo_root()).map_err(LinkError::OpenStore)?;
    link_with_store(&store)
}

/// Same as [`link_repo`] but uses an already-opened
/// [`FragmentStore`]. Useful for callers that want to reuse the
/// store across phases.
pub fn link_with_store(store: &FragmentStore) -> Result<LinkSummary, LinkError> {
    // Phase A: build the in-memory global symbol index.
    //
    // We deliberately materialize this once rather than calling
    // `find_symbols` per placeholder — the linker hits every
    // placeholder × at-least-one-shard, which on MediaWiki-scale
    // (~10K modules, ~40K methods) would be tens of millions of
    // shard reads. The in-memory map fits in RAM (bounded by total
    // named-symbol count, ~7K for Aethyme self).
    let global_index = GlobalSymbolIndex::build(store)?;

    // Phase B: walk every fragment, resolve, rewrite.
    let source_paths = store
        .list_indexed_source_paths()
        .map_err(LinkError::OpenStore)?;

    // Rayon-parallel: each fragment's resolution is independent.
    // Per-fragment failures bubble up via collect::<Result>.
    let per_fragment: Vec<FragmentResolution> = source_paths
        .par_iter()
        .map(|source_path| -> Result<FragmentResolution, LinkError> {
            link_one_fragment(store, source_path, &global_index)
        })
        .collect::<Result<Vec<_>, _>>()?;

    // Phase C: aggregate stats from each fragment's resolution.
    let mut summary = LinkSummary {
        fragments_visited: per_fragment.len(),
        ..Default::default()
    };
    for r in &per_fragment {
        summary.placeholders_seen += r.placeholders_seen;
        summary.placeholders_resolved += r.placeholders_resolved;
        summary.edges_rewritten += r.edges_rewritten;
        summary.orphans_removed += r.orphans_removed;
        if r.was_rewritten {
            summary.fragments_rewritten += 1;
        }
    }
    Ok(summary)
}

/// Process one fragment: load it, resolve placeholders, rewrite
/// edges, drop orphans, and persist if anything changed.
fn link_one_fragment(
    store: &FragmentStore,
    source_path: &str,
    global_index: &GlobalSymbolIndex,
) -> Result<FragmentResolution, LinkError> {
    let fragment = store
        .read_fragment(source_path)
        .map_err(|e| LinkError::ReadFragment {
            source_path: source_path.to_string(),
            inner: e,
        })?;

    // Collect placeholders in this fragment, keyed by NodeId for
    // O(1) edge-target lookup below.
    let placeholders: HashMap<&NodeId, &Node> = fragment
        .nodes()
        .iter()
        .filter(|n| n.kind() == NodeKind::UnresolvedSymbol)
        .map(|n| (n.id(), n))
        .collect();
    let placeholders_seen = placeholders.len();

    if placeholders.is_empty() {
        // Fragment has no placeholders → nothing to do, no rewrite.
        return Ok(FragmentResolution {
            placeholders_seen: 0,
            placeholders_resolved: 0,
            edges_rewritten: 0,
            orphans_removed: 0,
            was_rewritten: false,
        });
    }

    // Track which placeholders had at least one of their incoming
    // edges successfully rewritten. Used for the
    // placeholders_resolved stat — distinct from the orphan check
    // below, which only depends on whether ANY edge still points
    // at the placeholder.
    let mut resolved_placeholders: HashSet<NodeId> = HashSet::new();
    let mut edges_rewritten = 0usize;
    let local_import_bindings = local_import_bindings(&fragment, &placeholders, global_index);

    let new_edges: Vec<Edge> = fragment
        .edges()
        .iter()
        .map(|edge| {
            if !placeholders.contains_key(edge.dst_id()) {
                // Edge points at something else — pass through.
                return edge.clone();
            }
            let placeholder = placeholders[edge.dst_id()];
            match global_index.resolve_edge(
                source_path,
                placeholder,
                edge.attributes(),
                &local_import_bindings,
            ) {
                Some(target) => {
                    edges_rewritten += 1;
                    resolved_placeholders.insert(edge.dst_id().clone());
                    edge.clone().with_dst_id(target.node_id)
                }
                None => edge.clone(),
            }
        })
        .collect();

    // A placeholder is an orphan iff no edge in the rewritten
    // edge list points at it. This single check subsumes both
    // "resolved with no surviving edges" and "had a non-Imports
    // edge that kept it alive" — `surviving_node_ids` already
    // reflects whichever case applies.
    let surviving_node_ids: HashSet<&NodeId> =
        new_edges.iter().map(Edge::dst_id).collect::<HashSet<_>>();
    let mut orphans_removed = 0usize;
    let new_nodes: Vec<Node> = fragment
        .nodes()
        .iter()
        .filter(|n| {
            if n.kind() != NodeKind::UnresolvedSymbol {
                return true;
            }
            if surviving_node_ids.contains(n.id()) {
                return true;
            }
            orphans_removed += 1;
            false
        })
        .cloned()
        .collect();

    let placeholders_resolved = resolved_placeholders.len();
    let was_rewritten = edges_rewritten > 0 || orphans_removed > 0;
    if !was_rewritten {
        return Ok(FragmentResolution {
            placeholders_seen,
            placeholders_resolved,
            edges_rewritten,
            orphans_removed,
            was_rewritten,
        });
    }
    let new_fragment = Fragment::new(fragment.file_path(), new_nodes, new_edges).map_err(|e| {
        LinkError::RebuildFragment {
            source_path: source_path.to_string(),
            inner: e,
        }
    })?;
    write_fragment(store.repo_root(), source_path, &new_fragment).map_err(|e| {
        LinkError::WriteFragment {
            source_path: source_path.to_string(),
            inner: e,
        }
    })?;
    Ok(FragmentResolution {
        placeholders_seen,
        placeholders_resolved,
        edges_rewritten,
        orphans_removed,
        was_rewritten,
    })
}

// Per-fragment running totals returned from the parallel map and
// aggregated in `link_with_store`.
struct FragmentResolution {
    placeholders_seen: usize,
    placeholders_resolved: usize,
    edges_rewritten: usize,
    orphans_removed: usize,
    was_rewritten: bool,
}

fn local_import_bindings(
    fragment: &Fragment,
    placeholders: &HashMap<&NodeId, &Node>,
    global_index: &GlobalSymbolIndex,
) -> HashMap<String, Vec<ResolvedRecord>> {
    let mut bindings: HashMap<String, Vec<ResolvedRecord>> = HashMap::new();
    for edge in fragment.edges() {
        if edge.kind() != EdgeKind::Imports {
            continue;
        }
        let Some(placeholder) = placeholders.get(edge.dst_id()) else {
            continue;
        };
        let placeholder = *placeholder;
        let Node::UnresolvedSymbol(p) = placeholder else {
            continue;
        };
        if let Some(target) = global_index.resolve_imports_edge(placeholder, edge.attributes()) {
            bindings
                .entry(p.name().to_string())
                .or_default()
                .push(target);
        }
    }

    for records in bindings.values_mut() {
        records.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        records.dedup_by(|left, right| left.node_id == right.node_id);
    }
    bindings
}

// ─── Global symbol index ─────────────────────────────────────────────
//
// In-memory mirror of every SymbolRecord across the repo's shards.
// Keyed by both `name` (slow path: ambiguity check) and
// `(module, name)` (fast path: precise lookup for from-import
// edges). Built once per linker run.

#[derive(Debug)]
struct GlobalSymbolIndex {
    /// All records grouped by `symbol`. Used when the placeholder
    /// has a name but the linker has no module hint, OR as a
    /// last-resort fallback for `is_namespace` lookups.
    by_name: HashMap<String, Vec<ResolvedRecord>>,
    /// All records grouped by `(module, symbol)`. Used to satisfy
    /// `from M import X` lookups where M is known.
    by_module_and_name: HashMap<(String, String), Vec<ResolvedRecord>>,
    /// Module-name → its file-level node id (for whole-module
    /// `import X` resolution). The module shard stores symbols
    /// *inside* a module; this map points at the file that
    /// realizes the module.
    module_to_file_node: HashMap<String, ResolvedRecord>,
}

#[derive(Debug, Clone)]
struct ResolvedRecord {
    module: String,
    symbol: String,
    /// Kept for future kind-aware resolution heuristics (e.g.
    /// prefer Module over Function when the placeholder's
    /// expected_kind is Module). Currently informational.
    #[allow(dead_code)]
    kind: NodeKind,
    node_id: NodeId,
    /// Source path the record came from. Currently unused at
    /// runtime but invaluable for debugging an ambiguous-match
    /// scenario; preserved so the field remains available for
    /// future tools (e.g. a `--explain-resolution` query CLI).
    #[allow(dead_code)]
    file: String,
}

impl GlobalSymbolIndex {
    fn build(store: &FragmentStore) -> Result<Self, LinkError> {
        // Two passes:
        //   1. Walk fragments to populate `module_to_file_node`
        //      (needed for namespace-import resolution).
        //   2. Walk index shards to populate the symbol maps.
        let mut module_to_file_node: HashMap<String, ResolvedRecord> = HashMap::new();

        for source_path in store
            .list_indexed_source_paths()
            .map_err(LinkError::OpenStore)?
        {
            let fragment =
                store
                    .read_fragment(&source_path)
                    .map_err(|e| LinkError::ReadFragment {
                        source_path: source_path.clone(),
                        inner: e,
                    })?;
            for node in fragment.nodes() {
                if let Node::File(_) = node {
                    let module = synthesize_module_name(&source_path);
                    module_to_file_node.insert(
                        module.clone(),
                        ResolvedRecord {
                            module,
                            symbol: source_path.clone(),
                            kind: NodeKind::File,
                            node_id: node.id().clone(),
                            file: source_path.clone(),
                        },
                    );
                }
            }
        }

        let mut by_name: HashMap<String, Vec<ResolvedRecord>> = HashMap::new();
        let mut by_module_and_name: HashMap<(String, String), Vec<ResolvedRecord>> = HashMap::new();

        let modules = store.list_modules().map_err(LinkError::OpenStore)?;
        for module in &modules {
            let records =
                store
                    .read_index_shard(module)
                    .map_err(|e| LinkError::ReadIndexShard {
                        module: module.clone(),
                        inner: e,
                    })?;
            for record in records {
                if record.kind == NodeKind::UnresolvedSymbol {
                    // Imports-from-imports: skip these in the
                    // global index. We don't want one file's
                    // placeholder to resolve to another file's
                    // placeholder — that'd just be a redirected
                    // dead end.
                    continue;
                }
                let resolved = symbol_record_to_resolved(&record);
                by_name
                    .entry(resolved.symbol.clone())
                    .or_default()
                    .push(resolved.clone());
                by_module_and_name
                    .entry((resolved.module.clone(), resolved.symbol.clone()))
                    .or_default()
                    .push(resolved);
            }
        }

        Ok(GlobalSymbolIndex {
            by_name,
            by_module_and_name,
            module_to_file_node,
        })
    }

    fn resolve_edge(
        &self,
        source_path: &str,
        placeholder: &Node,
        attrs: &EdgeAttributes,
        local_import_bindings: &HashMap<String, Vec<ResolvedRecord>>,
    ) -> Option<ResolvedRecord> {
        match attrs {
            EdgeAttributes::Imports { .. } => self.resolve_imports_edge(placeholder, attrs),
            EdgeAttributes::Calls => {
                self.resolve_calls_edge(source_path, placeholder, local_import_bindings)
            }
            _ => None,
        }
    }

    /// Try to resolve a placeholder + the import edge that points
    /// at it. Returns the unique matched record on success, None
    /// if the placeholder cannot be resolved unambiguously.
    fn resolve_imports_edge(
        &self,
        placeholder: &Node,
        attrs: &EdgeAttributes,
    ) -> Option<ResolvedRecord> {
        let Node::UnresolvedSymbol(p) = placeholder else {
            return None; // Caller error — fail closed.
        };
        let EdgeAttributes::Imports {
            import_path,
            is_namespace,
            is_named,
            ..
        } = attrs
        else {
            return None; // Same: Imports edge expected.
        };

        let import_path: &str = import_path;
        let binding = p.name();

        if *is_namespace {
            // `import X` or `import X.Y`. The placeholder's name is
            // the binding (`X`); the edge's import_path is the full
            // module path. Resolve by *file*, not by symbol —
            // there's no symbol-level entry for a module-as-a-whole
            // in the shards (those record per-name entries).
            return self.module_to_file_node.get(import_path).cloned();
        }

        if *is_named {
            // `from M import X`. import_path is `M.X` (possibly with
            // leading dots for relative imports — we skip those for
            // now since they need the importing-file's package
            // context to resolve).
            if import_path.starts_with('.') {
                return None;
            }
            let (module_part, symbol_part) = match import_path.rsplit_once('.') {
                Some(pair) => pair,
                None => ("", import_path),
            };
            // Fast path: look up (module, symbol). One record only?
            // Resolve. Multiple? Ambiguous → skip.
            if !module_part.is_empty()
                && let Some(hits) = self
                    .by_module_and_name
                    .get(&(module_part.to_string(), symbol_part.to_string()))
                && hits.len() == 1
            {
                return Some(hits[0].clone());
            }
            // Fallback: maybe the imported name is itself a
            // submodule (e.g. `from package import submodule`).
            // Look for a module file whose name matches the dotted
            // path.
            if let Some(file_rec) = self.module_to_file_node.get(import_path) {
                return Some(file_rec.clone());
            }
            // Last resort: name-only across all shards. Only resolve
            // if exactly one record exists with that name.
            if let Some(hits) = self.by_name.get(symbol_part)
                && hits.len() == 1
            {
                return Some(hits[0].clone());
            }
            // Even-laster resort: try by_name on the *binding* (in
            // case symbol_part computation was off — defensive).
            if symbol_part != binding
                && let Some(hits) = self.by_name.get(binding)
                && hits.len() == 1
            {
                return Some(hits[0].clone());
            }
        }

        None
    }

    fn resolve_calls_edge(
        &self,
        source_path: &str,
        placeholder: &Node,
        local_import_bindings: &HashMap<String, Vec<ResolvedRecord>>,
    ) -> Option<ResolvedRecord> {
        let Node::UnresolvedSymbol(p) = placeholder else {
            return None; // Caller error — fail closed.
        };
        let name = p.name();
        if name.starts_with('.') || name == "*" {
            return None;
        }

        if let Some(target) = unique_import_binding(local_import_bindings, name)
            && is_call_target_kind(target.kind)
        {
            return Some(target);
        }

        if let Some(target) = self.resolve_local_namespace_call(name, local_import_bindings) {
            return Some(target);
        }

        if !name.contains('.') {
            let module = synthesize_module_name(source_path);
            return self.unique_call_target(&module, name);
        }

        None
    }

    fn resolve_local_namespace_call(
        &self,
        name: &str,
        local_import_bindings: &HashMap<String, Vec<ResolvedRecord>>,
    ) -> Option<ResolvedRecord> {
        let (module_part, symbol_part) = name.rsplit_once('.')?;
        if module_part.is_empty() || symbol_part.is_empty() {
            return None;
        }
        let root = module_part.split('.').next()?;
        let root_target = unique_import_binding(local_import_bindings, root)?;
        if !is_namespace_target_kind(root_target.kind) {
            return None;
        }
        let module = call_module_from_import_binding(module_part, root, &root_target)?;
        self.unique_call_target(&module, symbol_part)
    }

    fn unique_call_target(&self, module: &str, symbol: &str) -> Option<ResolvedRecord> {
        let hits = self
            .by_module_and_name
            .get(&(module.to_string(), symbol.to_string()))?;
        let callable_hits = hits
            .iter()
            .filter(|record| is_call_target_kind(record.kind))
            .collect::<Vec<_>>();
        if callable_hits.len() == 1 {
            return Some(callable_hits[0].clone());
        }
        None
    }
}

fn unique_import_binding(
    local_import_bindings: &HashMap<String, Vec<ResolvedRecord>>,
    binding: &str,
) -> Option<ResolvedRecord> {
    let records = local_import_bindings.get(binding)?;
    if records.len() == 1 {
        return Some(records[0].clone());
    }
    None
}

fn is_call_target_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Function | NodeKind::Class | NodeKind::Method
    )
}

fn is_namespace_target_kind(kind: NodeKind) -> bool {
    matches!(kind, NodeKind::File | NodeKind::Module | NodeKind::Package)
}

fn call_module_from_import_binding(
    module_part: &str,
    root_binding: &str,
    root_target: &ResolvedRecord,
) -> Option<String> {
    if module_part == root_binding {
        return Some(root_target.module.clone());
    }

    let tail = module_part.strip_prefix(root_binding)?.strip_prefix('.')?;
    if is_same_or_child_module(&root_target.module, root_binding) {
        if is_same_or_child_module(module_part, &root_target.module) {
            return Some(module_part.to_string());
        }
        return None;
    }

    Some(format!("{}.{}", root_target.module, tail))
}

fn is_same_or_child_module(module: &str, ancestor: &str) -> bool {
    module == ancestor
        || module
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('.'))
}

fn symbol_record_to_resolved(record: &SymbolRecord) -> ResolvedRecord {
    ResolvedRecord {
        module: record.module.to_string(),
        symbol: record.symbol.to_string(),
        kind: record.kind,
        node_id: record.node_id.clone(),
        file: record.file.to_string(),
    }
}

/// Mirror of `pipeline::synthesize_module_name`. Kept local to avoid
/// crossing the linker's "input is only the on-disk graph store"
/// boundary (the linker doesn't depend on the indexer's filesystem
/// walker — it works purely off what's been written).
fn synthesize_module_name(source_path: &str) -> String {
    let without_ext = source_path
        .rsplit_once('.')
        .map(|(stem, _)| stem)
        .unwrap_or(source_path);
    without_ext.replace('/', ".")
}

// ─── Error type ──────────────────────────────────────────────────────

/// Errors from a linker invocation. Granular enough to point at
/// which fragment / module failed.
#[derive(Debug)]
pub enum LinkError {
    OpenStore(StoreOpenError),
    ReadFragment {
        source_path: String,
        inner: FragmentReadError,
    },
    ReadIndexShard {
        module: String,
        inner: IndexShardReadError,
    },
    RebuildFragment {
        source_path: String,
        inner: FragmentBuildError,
    },
    WriteFragment {
        source_path: String,
        inner: FragmentWriteError,
    },
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OpenStore(e) => write!(f, "link_repo: open store: {e}"),
            Self::ReadFragment { source_path, inner } => {
                write!(f, "link_repo: read fragment {source_path:?}: {inner}")
            }
            Self::ReadIndexShard { module, inner } => {
                write!(f, "link_repo: read index shard {module:?}: {inner}")
            }
            Self::RebuildFragment { source_path, inner } => {
                write!(f, "link_repo: rebuild fragment {source_path:?}: {inner}")
            }
            Self::WriteFragment { source_path, inner } => {
                write!(f, "link_repo: write fragment {source_path:?}: {inner}")
            }
        }
    }
}

impl std::error::Error for LinkError {}

/// Convenience entry point: open a [`FragmentStore`] at the given
/// path and run the linker. Used by the binary CLI.
pub fn link_repo_path(repo_root: PathBuf) -> Result<LinkSummary, LinkError> {
    let store = FragmentStore::open(repo_root).map_err(LinkError::OpenStore)?;
    link_with_store(&store)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn synthesize_module_name_matches_pipeline() {
        // Quick sanity that the local helper matches the pipeline
        // one — they must stay aligned or linker lookups by module
        // diverge from indexer-emitted shard module names.
        assert_eq!(synthesize_module_name("src/cli.py"), "src.cli");
        assert_eq!(synthesize_module_name("a/b/c.rs"), "a.b.c");
        assert_eq!(synthesize_module_name("README.md"), "README");
    }
}
