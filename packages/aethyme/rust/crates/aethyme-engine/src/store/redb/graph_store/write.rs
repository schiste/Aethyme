//! Writes: the typed insert wrappers that own each node kind's table and
//! secondary-index contract, and the rotating `IndexSession`.

use super::*;

// ── Typed write wrappers ────────────────────────────────────────────────────
// Thin shims over IndexSession primitives. The mapping is structured ID →
// raw redb key (no sanitization), entity → bincoded value, plus the secondary
// indexes each kind requires. Mirrors the surface of `super::super::write`.
//
// These wrappers are intentionally small: each one owns the table/index
// contract for its node kind so the CLI writer cannot forget a secondary index
// when adding a new persisted kind.

pub(super) fn symbol_index_key(name: &str) -> String {
    name.to_ascii_lowercase()
}

pub(super) fn symbol_components(name: &str) -> BTreeSet<String> {
    split_symbol_components(name)
        .into_iter()
        .map(|component| component.to_ascii_lowercase())
        .filter(|component| !component.is_empty())
        .collect()
}

pub(super) fn split_symbol_components(name: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = name.chars().collect();

    for (idx, ch) in chars.iter().copied().enumerate() {
        if !ch.is_ascii_alphanumeric() {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            continue;
        }

        if current.is_empty() {
            current.push(ch);
            continue;
        }

        let prev = chars[idx - 1];
        let lc_uc = (prev.is_ascii_lowercase() || prev.is_ascii_digit()) && ch.is_ascii_uppercase();
        let acronym_break = prev.is_ascii_uppercase()
            && ch.is_ascii_uppercase()
            && idx + 1 < chars.len()
            && chars[idx + 1].is_ascii_lowercase();
        if lc_uc || acronym_break {
            out.push(std::mem::take(&mut current));
        }
        current.push(ch);
    }

    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Insert (or overwrite) an area. Key = `area.id` (e.g. `area:Repo:src`).
pub fn insert_area(session: &mut IndexSession<'_>, area: &AreaNode) -> Result<(), GraphStoreError> {
    session.insert_node(AREAS, &area.id, area)
}

/// Insert (or overwrite) the repository container. Key = `repo:<name>`.
pub fn insert_repository(
    session: &mut IndexSession<'_>,
    repository: &RepositoryNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(REPOSITORIES, &repository.id, repository)
}

/// Insert (or overwrite) a directory/container and index it by its relative
/// path.
pub fn insert_directory(
    session: &mut IndexSession<'_>,
    directory: &DirectoryNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(DIRECTORIES, &directory.id, directory)?;
    session.add_path_index(NODES_BY_PATH, &directory.path, &directory.id)?;
    Ok(())
}

/// Insert (or overwrite) a file. Key = `file.id` (e.g. `file:Repo:src/lib.rs`).
/// Also adds `path → file.id` to NODES_BY_PATH so a path can be resolved to
/// the structured ID.
pub fn insert_file(session: &mut IndexSession<'_>, file: &FileNode) -> Result<(), GraphStoreError> {
    session.insert_node(FILES, &file.id, file)?;
    session.add_path_index(NODES_BY_PATH, &file.path, &file.id)?;
    Ok(())
}

/// Insert (or overwrite) a function. Also indexes by file path and
/// ASCII-lowercased simple name.
pub fn insert_function(
    session: &mut IndexSession<'_>,
    function: &FunctionNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(FUNCTIONS, function.id.as_str(), function)?;
    session.add_path_index(
        FUNCTIONS_BY_PATH,
        function.file_path.as_str(),
        function.id.as_str(),
    )?;
    session.add_path_index(
        NODES_BY_PATH,
        function.file_path.as_str(),
        function.id.as_str(),
    )?;
    let name_key = symbol_index_key(function.name.as_str());
    session.add_symbol_index(&name_key, function.id.as_str())?;
    for component in symbol_components(function.name.as_str()) {
        session.add_symbol_component_index(&component, function.id.as_str())?;
    }
    for component in symbol_components(function.file_path.as_str()) {
        session.add_symbol_path_component_index(&component, function.id.as_str())?;
    }
    Ok(())
}

/// Insert (or overwrite) a class-like symbol. Also indexes by file path and
/// ASCII-lowercased simple name.
pub fn insert_class(
    session: &mut IndexSession<'_>,
    class: &ClassNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(CLASSES, class.id.as_str(), class)?;
    session.add_path_index(NODES_BY_PATH, class.file_path.as_str(), class.id.as_str())?;
    let name_key = symbol_index_key(class.name.as_str());
    session.add_symbol_index(&name_key, class.id.as_str())?;
    for component in symbol_components(class.name.as_str()) {
        session.add_symbol_component_index(&component, class.id.as_str())?;
    }
    for component in symbol_components(class.file_path.as_str()) {
        session.add_symbol_path_component_index(&component, class.id.as_str())?;
    }
    Ok(())
}

/// Insert (or overwrite) a documentation node and index it by path.
pub fn insert_doc(session: &mut IndexSession<'_>, doc: &DocNode) -> Result<(), GraphStoreError> {
    session.insert_node(DOCS, &doc.id, doc)?;
    session.add_path_index(NODES_BY_PATH, &doc.path, &doc.id)?;
    Ok(())
}

/// Insert (or overwrite) a configuration node and index it by path.
pub fn insert_config(
    session: &mut IndexSession<'_>,
    config: &ConfigNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(CONFIGS, &config.id, config)?;
    session.add_path_index(NODES_BY_PATH, &config.path, &config.id)?;
    Ok(())
}

/// Insert (or overwrite) a Surface/Flow node. Also indexes by owning file path
/// and simple name so task anchors can find routes, middleware, workers, and
/// credential operations without scanning the full store.
pub fn insert_surface(
    session: &mut IndexSession<'_>,
    surface: &SurfaceNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(SURFACES, surface.id.as_str(), surface)?;
    session.add_path_index(
        NODES_BY_PATH,
        surface.file_path.as_str(),
        surface.id.as_str(),
    )?;
    let name_key = symbol_index_key(surface.name.as_str());
    session.add_symbol_index(&name_key, surface.id.as_str())?;
    for component in symbol_components(surface.name.as_str()) {
        session.add_symbol_component_index(&component, surface.id.as_str())?;
    }
    for component in symbol_components(surface.detail.as_str()) {
        session.add_symbol_component_index(&component, surface.id.as_str())?;
    }
    for component in symbol_components(surface.file_path.as_str()) {
        session.add_symbol_path_component_index(&component, surface.id.as_str())?;
    }
    Ok(())
}

/// Insert (or overwrite) an unresolved/import placeholder and index it by the
/// source file path where the unresolved reference was observed.
pub fn insert_unresolved(
    session: &mut IndexSession<'_>,
    unresolved: &UnresolvedNode,
) -> Result<(), GraphStoreError> {
    session.insert_node(UNRESOLVED, unresolved.id.as_str(), unresolved)?;
    session.add_path_index(
        NODES_BY_PATH,
        unresolved.file_path.as_str(),
        unresolved.id.as_str(),
    )?;
    Ok(())
}

/// Insert one logical edge. Composes into `IndexSession::insert_edge` with
/// `Edge`'s fields unpacked.
pub fn insert_edge(session: &mut IndexSession<'_>, edge: &Edge) -> Result<(), GraphStoreError> {
    session.insert_edge(
        edge.from.as_str(),
        edge.to.as_str(),
        edge.kind.clone(),
        edge.confidence,
        edge.source.clone(),
    )
}

/// Insert a risk flag under its scope.
pub fn insert_risk(session: &mut IndexSession<'_>, risk: &RiskFlag) -> Result<(), GraphStoreError> {
    session.add_risk(&risk.scope, risk)
}

/// One open redb write transaction that accepts many node/edge inserts and
/// commits/rotates based on a policy.
///
/// "Op" counts every primary-table or adjacency insert; secondary-index
/// updates are folded into the parent op (one logical edge insert =
/// 2 adjacency rows, counted as 1 op). Bytes count actual bincode payload.
pub struct IndexSession<'db> {
    pub(super) db: &'db Database,
    pub(super) txn: Option<WriteTransaction>,
    pub(super) durability: IndexDurability,
    pub(super) ops_since_rotate: usize,
    pub(super) bytes_since_rotate: usize,
}

impl<'db> IndexSession<'db> {
    /// Insert (or overwrite) a node row in the given primary table.
    ///
    /// Caller picks the table; higher-level helpers such as `insert_file`,
    /// `insert_area`, and `insert_function` layer path/symbol secondary-index
    /// writes on top of this primitive.
    pub fn insert_node(
        &mut self,
        table: TableDefinition<&str, &[u8]>,
        key: &str,
        value: &impl Serialize,
    ) -> Result<(), GraphStoreError> {
        let bytes = bincode::serialize(value)?;
        let written = bytes.len();
        {
            let txn = self
                .txn
                .as_ref()
                .expect("IndexSession invariant: txn present");
            let mut t = txn.open_table(table)?;
            t.insert(key, bytes.as_slice())?;
        }
        self.ops_since_rotate += 1;
        self.bytes_since_rotate += written;
        if self.should_rotate() {
            self.rotate()?;
        }
        Ok(())
    }

    /// Insert one logical edge as two adjacency rows: `(src → kind/dst)` in
    /// EDGES_OUT and `(dst → kind/src)` in EDGES_IN. Counted as one op.
    pub fn insert_edge(
        &mut self,
        src: &str,
        dst: &str,
        kind: EdgeKind,
        confidence: u16,
        source: InternedStr,
    ) -> Result<(), GraphStoreError> {
        let out_record = AdjacencyRecord {
            kind: kind.clone(),
            other: InternedStr::from(dst),
            confidence,
            source: source.clone(),
        };
        let in_record = AdjacencyRecord {
            kind,
            other: InternedStr::from(src),
            confidence,
            source,
        };
        let kind_key = edge_kind_label(&out_record.kind);
        let edge_record = Edge::new(
            src,
            dst,
            out_record.kind.clone(),
            confidence,
            out_record.source.clone(),
        );
        let out_bytes = bincode::serialize(&out_record)?;
        let in_bytes = bincode::serialize(&in_record)?;
        let edge_bytes = bincode::serialize(&edge_record)?;
        let written = out_bytes.len() + in_bytes.len() + edge_bytes.len();
        {
            let txn = self
                .txn
                .as_ref()
                .expect("IndexSession invariant: txn present");
            let mut out = txn.open_multimap_table(EDGES_OUT)?;
            out.insert(src, out_bytes.as_slice())?;
            let mut inv = txn.open_multimap_table(EDGES_IN)?;
            inv.insert(dst, in_bytes.as_slice())?;
            let mut by_kind = txn.open_multimap_table(EDGES_BY_KIND)?;
            by_kind.insert(kind_key, edge_bytes.as_slice())?;
        }
        self.ops_since_rotate += 1;
        self.bytes_since_rotate += written;
        if self.should_rotate() {
            self.rotate()?;
        }
        Ok(())
    }

    /// Append `node_id` under `path` in a multimap path index
    /// (FUNCTIONS_BY_PATH or NODES_BY_PATH). No counter increment — these
    /// are tiny side effects of a parent insert.
    pub fn add_path_index(
        &mut self,
        table: MultimapTableDefinition<&str, &str>,
        path: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(table)?;
        t.insert(path, node_id)?;
        Ok(())
    }

    /// Append `node_id` under lowercased `name` in SYMBOL_BY_NAME.
    /// No counter increment — folded into a parent symbol insert.
    pub fn add_symbol_index(
        &mut self,
        name_lower: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(SYMBOL_BY_NAME)?;
        t.insert(name_lower, node_id)?;
        Ok(())
    }

    /// Append `node_id` under a lowercased symbol component in
    /// SYMBOL_BY_COMPONENT. No counter increment — folded into a parent symbol
    /// insert.
    pub fn add_symbol_component_index(
        &mut self,
        component_lower: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(SYMBOL_BY_COMPONENT)?;
        t.insert(component_lower, node_id)?;
        Ok(())
    }

    /// Append `node_id` under a lowercased file-path component in
    /// SYMBOL_BY_PATH_COMPONENT. No counter increment — folded into a parent
    /// symbol insert.
    pub fn add_symbol_path_component_index(
        &mut self,
        component_lower: &str,
        node_id: &str,
    ) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .as_ref()
            .expect("IndexSession invariant: txn present");
        let mut t = txn.open_multimap_table(SYMBOL_BY_PATH_COMPONENT)?;
        t.insert(component_lower, node_id)?;
        Ok(())
    }

    /// Append a bincoded risk record under `scope` in RISK_FLAGS.
    pub fn add_risk(&mut self, scope: &str, value: &impl Serialize) -> Result<(), GraphStoreError> {
        let bytes = bincode::serialize(value)?;
        let written = bytes.len();
        {
            let txn = self
                .txn
                .as_ref()
                .expect("IndexSession invariant: txn present");
            let mut t = txn.open_multimap_table(RISK_FLAGS)?;
            t.insert(scope, bytes.as_slice())?;
        }
        self.ops_since_rotate += 1;
        self.bytes_since_rotate += written;
        if self.should_rotate() {
            self.rotate()?;
        }
        Ok(())
    }

    /// Commit all pending writes. Consumes the session.
    pub fn commit(mut self) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .take()
            .expect("IndexSession invariant: txn present");
        txn.commit()?;
        Ok(())
    }

    /// Force a rotation now (commit current txn, open a fresh one). Useful at
    /// natural pipeline boundaries (e.g. between indexing passes).
    pub fn rotate(&mut self) -> Result<(), GraphStoreError> {
        let txn = self
            .txn
            .take()
            .expect("IndexSession invariant: txn present");
        txn.commit()?;
        let mut txn = self.db.begin_write()?;
        self.durability.apply(&mut txn)?;
        self.txn = Some(txn);
        self.ops_since_rotate = 0;
        self.bytes_since_rotate = 0;
        Ok(())
    }

    // Rotate policy rationale
    //
    // Returning `true` after an insert triggers commit + fresh transaction.
    // Returning `false` keeps batching.
    //
    // GraphStore sees many small writes (one node = one row, one edge = two
    // adjacency rows). On MediaWiki the ballpark is ~25k files + ~80k
    // functions + ~10k classes + ~1M edges = O(1M) ops. Per-op payload is
    // small (tens to a few hundred bytes).
    //
    // V1 uses the hybrid policy because neither single counter is sufficient:
    // ops bound fsync latency on tiny-row workloads, while bytes bound dirty
    // page growth when payloads get larger. The current constants are
    // ROTATE_EVERY_OPS = 4096 and ROTATE_EVERY_BYTES = 8 MiB. On a MediaWiki-
    // scale run with about 1M logical ops, the ops threshold yields roughly
    // 244 commits, which was acceptable in the initial profile runs. Tune the
    // constants only from measured index profiles, not from eval-score pressure.
    //
    // Constraints to respect:
    //   - `ops_since_rotate` and `bytes_since_rotate` reset on rotate.
    //   - returning `true` is ALWAYS safe (just slower); returning `false`
    //     too aggressively risks unbounded memory growth on big repos.
    pub(super) fn should_rotate(&self) -> bool {
        self.ops_since_rotate >= ROTATE_EVERY_OPS || self.bytes_since_rotate >= ROTATE_EVERY_BYTES
    }
}
