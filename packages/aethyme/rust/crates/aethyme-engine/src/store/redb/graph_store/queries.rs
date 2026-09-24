//! The public query API of `GraphStore` and `ReadOnlyGraphStore`.

use super::*;

impl GraphStore {
    /// Read a typed node by canonical id.
    pub fn get_node(&self, id: &str) -> Result<Option<StoredNode>, GraphStoreError> {
        get_node_from(&self.db, id)
    }

    /// Read typed nodes by canonical id, preserving input order and omitting
    /// missing ids.
    pub fn get_nodes<S: AsRef<str>>(&self, ids: &[S]) -> Result<Vec<StoredNode>, GraphStoreError> {
        get_nodes_from(&self.db, ids)
    }

    /// Read a display-ready projection for one typed node.
    pub fn node_display(&self, id: &str) -> Result<Option<NodeDisplay>, GraphStoreError> {
        node_display_from(&self.db, id)
    }

    /// Return the area id associated with a node id or exact file path.
    pub fn area_for_node(&self, id_or_path: &str) -> Result<Option<String>, GraphStoreError> {
        area_for_node_from(&self.db, id_or_path)
    }

    /// Return children via outgoing `contains` / `defines` edges.
    pub fn children(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        children_from(&self.db, id, kind)
    }

    /// Return parents via incoming `contains` / `defines` / `belongs_to` edges.
    pub fn parents(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        parents_from(&self.db, id, kind)
    }

    /// Return a display-ready relation view for rendered graph flows.
    pub fn relation_view(
        &self,
        id: &str,
        relation: GraphRelation,
    ) -> Result<RedbRelationView, GraphStoreError> {
        relation_view_from(&self.db, id, relation)
    }

    /// Return docs attached to a node through `documents` edges.
    pub fn docs_for(&self, id: &str) -> Result<Vec<DocNode>, GraphStoreError> {
        docs_for_from(&self.db, id)
    }

    /// Return configs attached to a node through `configures` or
    /// `entrypoint_for` edges.
    pub fn configs_for(&self, id: &str) -> Result<Vec<ConfigNode>, GraphStoreError> {
        configs_for_from(&self.db, id)
    }

    /// Return risk flags attached to a node id, exact path, or path prefix.
    pub fn risk_for_node_or_path(
        &self,
        id_or_path: &str,
    ) -> Result<Vec<RiskFlag>, GraphStoreError> {
        risks_for_node_or_path_from(&self.db, id_or_path)
    }

    /// Find function/class symbols by exact simple name, case-insensitive.
    pub fn find_symbols(
        &self,
        name: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<SymbolLookup>, GraphStoreError> {
        find_symbols_from(&self.db, name, kind)
    }

    /// Return bounded function/class symbol candidates for exact, prefix,
    /// component, path, and area signals.
    pub fn symbols_matching(&self, query: &str) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, SymbolMatchOptions::default())
    }

    /// Same as `symbols_matching`, with caller-provided bounds and filters.
    pub fn symbols_matching_with(
        &self,
        query: &str,
        options: SymbolMatchOptions,
    ) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, options)
    }

    /// Return bounded typed anchor candidates for tokenized task text.
    pub fn task_anchor_candidates<S: AsRef<str>>(
        &self,
        task_tokens: &[S],
        limit: usize,
    ) -> Result<Vec<TaskAnchorCandidate>, GraphStoreError> {
        task_anchor_candidates_from(&self.db, task_tokens, limit)
    }

    /// Return bounded symbol/path seeds for usage-boundary flows.
    pub fn usage_boundary_candidates(
        &self,
        scope: &str,
        symbol_kind: Option<StoredNodeKind>,
        limit: usize,
    ) -> Result<Vec<UsageBoundaryCandidate>, GraphStoreError> {
        usage_boundary_candidates_from(&self.db, scope, symbol_kind, limit)
    }

    /// Return bounded ingress candidates for task tokens without scanning all
    /// nodes or all edges.
    pub fn entrypoints_for_task<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        entrypoints_for_task_from(&self.db, tokens)
    }

    /// Return bounded repo-relative paths with matching Surface/Flow behavior.
    pub fn surface_paths_for_behavior<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfacePathCandidate>, GraphStoreError> {
        surface_paths_for_behavior_from(&self.db, tokens)
    }

    /// Return bounded credential issue/store/use/validation candidates.
    pub fn credential_flow_candidates<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        credential_flow_candidates_from(&self.db, tokens)
    }

    /// Return middleware/auth/header relations around a route, surface, or
    /// exact file path.
    pub fn middleware_chain_for_route(
        &self,
        route_or_file: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        middleware_chain_for_route_from(&self.db, route_or_file)
    }

    /// Return a bounded forwarding/header chain starting from a surface.
    pub fn forwarding_chain_for_surface(
        &self,
        surface: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        forwarding_chain_for_surface_from(&self.db, surface)
    }

    /// Return bounded subsystem slices matching task tokens.
    pub fn subsystems_matching<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SubsystemCandidate>, GraphStoreError> {
        subsystems_matching_from(&self.db, tokens)
    }

    /// Return behavior-test surfaces directly linked to a surface, symbol, or
    /// owning file.
    pub fn tests_for_surface_or_symbol(
        &self,
        id: &str,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        tests_for_surface_or_symbol_from(&self.db, id)
    }

    /// Return a bounded coverage projection for a generic task class.
    pub fn coverage_for_task_class(
        &self,
        task_class: &str,
    ) -> Result<TaskClassCoverage, GraphStoreError> {
        coverage_for_task_class_from(&self.db, task_class)
    }

    /// Return all typed nodes whose indexed path starts with `prefix`.
    pub fn nodes_under_path(&self, prefix: &str) -> Result<Vec<StoredNode>, GraphStoreError> {
        nodes_under_path_from(&self.db, prefix)
    }

    /// Return all functions whose file path starts with `prefix`.
    pub fn functions_under_path(&self, prefix: &str) -> Result<Vec<FunctionNode>, GraphStoreError> {
        functions_under_path_from(&self.db, prefix)
    }

    /// Return at most `limit` callable ids for one exact file path.
    pub fn function_ids_for_path(
        &self,
        path: &str,
        limit: usize,
    ) -> Result<BoundedFunctionIds, GraphStoreError> {
        function_ids_for_path_from(&self.db, path, limit)
    }

    /// Return incoming or outgoing adjacency, optionally filtered by edge kind.
    pub fn neighbors(
        &self,
        id: &str,
        direction: NeighborDirection,
        kind: Option<EdgeKind>,
    ) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        neighbors_from(&self.db, id, direction, kind)
    }

    /// Resolve an exact repo-relative path to the persisted file node.
    pub fn resolve_file_path(&self, path: &str) -> Result<Option<FileNode>, GraphStoreError> {
        resolve_file_path_from(&self.db, path)
    }

    /// Bounded V2 overview slice over typed nodes plus existing overview data.
    pub fn overview_v2(&self, limits: OverviewV2Limits) -> Result<OverviewV2, GraphStoreError> {
        overview_v2_from(&self.db, limits)
    }

    /// List all areas, optionally filtered by depth (1 = top-level, 2 =
    /// nested under top-level, etc). Depth is computed from `path_prefix`.
    pub fn list_areas(&self, depth: Option<u32>) -> Result<Vec<AreaNode>, GraphStoreError> {
        list_areas_from(&self.db, depth)
    }

    /// Outgoing adjacency rows for `entity_id`. Each row carries the partner
    /// (`other`) so callers don't need a second lookup.
    pub fn edges_from(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_OUT, entity_id)
    }

    /// Incoming adjacency rows for `entity_id`. The `O(in_degree)` shape
    /// from the dead-code algorithm fix — the whole reason EDGES_IN exists
    /// from day one in this schema.
    pub fn edges_to(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_IN, entity_id)
    }

    /// One-shot summary used by the `query-overview` CLI command:
    /// repo metadata + top-N areas at depth 1 + first-N entrypoint files +
    /// top-N risks (by RiskLevel descending).
    pub fn overview(
        &self,
        area_limit: usize,
        entrypoint_limit: usize,
        risk_limit: usize,
    ) -> Result<Overview, GraphStoreError> {
        overview_from(&self.db, area_limit, entrypoint_limit, risk_limit)
    }
}

impl ReadOnlyGraphStore {
    /// Read previously-written repo metadata, if any.
    pub fn repo_metadata(&self) -> Result<Option<RepoMetadata>, GraphStoreError> {
        repo_metadata_from(&self.db)
    }

    /// Read a typed node by canonical id.
    pub fn get_node(&self, id: &str) -> Result<Option<StoredNode>, GraphStoreError> {
        get_node_from(&self.db, id)
    }

    /// Read typed nodes by canonical id, preserving input order and omitting
    /// missing ids.
    pub fn get_nodes<S: AsRef<str>>(&self, ids: &[S]) -> Result<Vec<StoredNode>, GraphStoreError> {
        get_nodes_from(&self.db, ids)
    }

    /// Read a display-ready projection for one typed node.
    pub fn node_display(&self, id: &str) -> Result<Option<NodeDisplay>, GraphStoreError> {
        node_display_from(&self.db, id)
    }

    /// Return the area id associated with a node id or exact file path.
    pub fn area_for_node(&self, id_or_path: &str) -> Result<Option<String>, GraphStoreError> {
        area_for_node_from(&self.db, id_or_path)
    }

    /// Return children via outgoing `contains` / `defines` edges.
    pub fn children(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        children_from(&self.db, id, kind)
    }

    /// Return parents via incoming `contains` / `defines` / `belongs_to` edges.
    pub fn parents(
        &self,
        id: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        parents_from(&self.db, id, kind)
    }

    /// Return a display-ready relation view for rendered graph flows.
    pub fn relation_view(
        &self,
        id: &str,
        relation: GraphRelation,
    ) -> Result<RedbRelationView, GraphStoreError> {
        relation_view_from(&self.db, id, relation)
    }

    /// Return docs attached to a node through `documents` edges.
    pub fn docs_for(&self, id: &str) -> Result<Vec<DocNode>, GraphStoreError> {
        docs_for_from(&self.db, id)
    }

    /// Return configs attached to a node through `configures` or
    /// `entrypoint_for` edges.
    pub fn configs_for(&self, id: &str) -> Result<Vec<ConfigNode>, GraphStoreError> {
        configs_for_from(&self.db, id)
    }

    /// Return risk flags attached to a node id, exact path, or path prefix.
    pub fn risk_for_node_or_path(
        &self,
        id_or_path: &str,
    ) -> Result<Vec<RiskFlag>, GraphStoreError> {
        risks_for_node_or_path_from(&self.db, id_or_path)
    }

    /// Find function/class symbols by exact simple name, case-insensitive.
    pub fn find_symbols(
        &self,
        name: &str,
        kind: Option<StoredNodeKind>,
    ) -> Result<Vec<SymbolLookup>, GraphStoreError> {
        find_symbols_from(&self.db, name, kind)
    }

    /// Return bounded function/class symbol candidates for exact, prefix,
    /// component, path, and area signals.
    pub fn symbols_matching(&self, query: &str) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, SymbolMatchOptions::default())
    }

    /// Same as `symbols_matching`, with caller-provided bounds and filters.
    pub fn symbols_matching_with(
        &self,
        query: &str,
        options: SymbolMatchOptions,
    ) -> Result<Vec<SymbolCandidate>, GraphStoreError> {
        symbols_matching_from(&self.db, query, options)
    }

    /// Return bounded typed anchor candidates for tokenized task text.
    pub fn task_anchor_candidates<S: AsRef<str>>(
        &self,
        task_tokens: &[S],
        limit: usize,
    ) -> Result<Vec<TaskAnchorCandidate>, GraphStoreError> {
        task_anchor_candidates_from(&self.db, task_tokens, limit)
    }

    /// Return bounded symbol/path seeds for usage-boundary flows.
    pub fn usage_boundary_candidates(
        &self,
        scope: &str,
        symbol_kind: Option<StoredNodeKind>,
        limit: usize,
    ) -> Result<Vec<UsageBoundaryCandidate>, GraphStoreError> {
        usage_boundary_candidates_from(&self.db, scope, symbol_kind, limit)
    }

    /// Return bounded ingress candidates for task tokens without scanning all
    /// nodes or all edges.
    pub fn entrypoints_for_task<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        entrypoints_for_task_from(&self.db, tokens)
    }

    /// Return bounded repo-relative paths with matching Surface/Flow behavior.
    pub fn surface_paths_for_behavior<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfacePathCandidate>, GraphStoreError> {
        surface_paths_for_behavior_from(&self.db, tokens)
    }

    /// Return bounded credential issue/store/use/validation candidates.
    pub fn credential_flow_candidates<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
        credential_flow_candidates_from(&self.db, tokens)
    }

    /// Return middleware/auth/header relations around a route, surface, or
    /// exact file path.
    pub fn middleware_chain_for_route(
        &self,
        route_or_file: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        middleware_chain_for_route_from(&self.db, route_or_file)
    }

    /// Return a bounded forwarding/header chain starting from a surface.
    pub fn forwarding_chain_for_surface(
        &self,
        surface: &str,
    ) -> Result<FlowChain, GraphStoreError> {
        forwarding_chain_for_surface_from(&self.db, surface)
    }

    /// Return bounded subsystem slices matching task tokens.
    pub fn subsystems_matching<S: AsRef<str>>(
        &self,
        tokens: &[S],
    ) -> Result<Vec<SubsystemCandidate>, GraphStoreError> {
        subsystems_matching_from(&self.db, tokens)
    }

    /// Return behavior-test surfaces directly linked to a surface, symbol, or
    /// owning file.
    pub fn tests_for_surface_or_symbol(
        &self,
        id: &str,
    ) -> Result<Vec<NodeDisplay>, GraphStoreError> {
        tests_for_surface_or_symbol_from(&self.db, id)
    }

    /// Return a bounded coverage projection for a generic task class.
    pub fn coverage_for_task_class(
        &self,
        task_class: &str,
    ) -> Result<TaskClassCoverage, GraphStoreError> {
        coverage_for_task_class_from(&self.db, task_class)
    }

    /// Return all typed nodes whose indexed path starts with `prefix`.
    pub fn nodes_under_path(&self, prefix: &str) -> Result<Vec<StoredNode>, GraphStoreError> {
        nodes_under_path_from(&self.db, prefix)
    }

    /// Return all functions whose file path starts with `prefix`.
    pub fn functions_under_path(&self, prefix: &str) -> Result<Vec<FunctionNode>, GraphStoreError> {
        functions_under_path_from(&self.db, prefix)
    }

    /// Return at most `limit` callable ids for one exact file path.
    pub fn function_ids_for_path(
        &self,
        path: &str,
        limit: usize,
    ) -> Result<BoundedFunctionIds, GraphStoreError> {
        function_ids_for_path_from(&self.db, path, limit)
    }

    /// Return incoming or outgoing adjacency, optionally filtered by edge kind.
    pub fn neighbors(
        &self,
        id: &str,
        direction: NeighborDirection,
        kind: Option<EdgeKind>,
    ) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        neighbors_from(&self.db, id, direction, kind)
    }

    /// Resolve an exact repo-relative path to the persisted file node.
    pub fn resolve_file_path(&self, path: &str) -> Result<Option<FileNode>, GraphStoreError> {
        resolve_file_path_from(&self.db, path)
    }

    /// Bounded V2 overview slice over typed nodes plus existing overview data.
    pub fn overview_v2(&self, limits: OverviewV2Limits) -> Result<OverviewV2, GraphStoreError> {
        overview_v2_from(&self.db, limits)
    }

    /// List all areas, optionally filtered by depth.
    pub fn list_areas(&self, depth: Option<u32>) -> Result<Vec<AreaNode>, GraphStoreError> {
        list_areas_from(&self.db, depth)
    }

    /// Outgoing adjacency rows for `entity_id`.
    pub fn edges_from(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_OUT, entity_id)
    }

    /// Incoming adjacency rows for `entity_id`.
    pub fn edges_to(&self, entity_id: &str) -> Result<Vec<AdjacencyRecord>, GraphStoreError> {
        collect_adjacency(&self.db, EDGES_IN, entity_id)
    }

    pub(crate) fn edge_count(&self) -> Result<u64, GraphStoreError> {
        edge_count_from(&self.db)
    }

    /// Return all persisted logical edges from the outgoing adjacency table.
    pub fn all_edges(&self) -> Result<Vec<Edge>, GraphStoreError> {
        all_edges_from(&self.db)
    }

    /// One-shot summary for query commands.
    pub fn overview(
        &self,
        area_limit: usize,
        entrypoint_limit: usize,
        risk_limit: usize,
    ) -> Result<Overview, GraphStoreError> {
        overview_from(&self.db, area_limit, entrypoint_limit, risk_limit)
    }
}
