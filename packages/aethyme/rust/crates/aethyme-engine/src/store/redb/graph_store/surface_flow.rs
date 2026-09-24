//! Surface/Flow reads: entrypoints, surface paths, credential flows,
//! middleware and forwarding chains, behavior tests, subsystems and
//! task-class coverage.

use super::*;

pub(super) const FLOW_QUERY_LIMIT: usize = 50;
pub(super) const FLOW_EDGE_LOOKUP_LIMIT: usize = 128;
pub(super) const FLOW_CHAIN_ROOT_LIMIT: usize = 8;
pub(super) const FLOW_CHAIN_STEP_LIMIT: usize = 32;
pub(super) const FLOW_CHAIN_DEPTH_LIMIT: usize = 4;
pub(super) const SUBSYSTEM_NODE_LIMIT: usize = 6;

pub(super) fn push_unique_edge_kind(values: &mut Vec<EdgeKind>, value: EdgeKind) {
    if !values.contains(&value) {
        values.push(value);
        values.sort();
    }
}

pub(super) fn bounded_query_terms<S: AsRef<str>>(tokens: &[S]) -> Vec<String> {
    let mut terms = Vec::new();
    for raw in tokens {
        let trimmed = raw.as_ref().trim();
        if trimmed.len() >= 2 {
            push_unique_string(&mut terms, trimmed.to_string());
        }
        for token in symbol_query_tokens(trimmed) {
            if token.len() >= 2 {
                push_unique_string(&mut terms, token);
            }
        }
    }
    terms
}

pub(super) fn node_text_matches_term(node: &NodeDisplay, term: &str) -> bool {
    let needle = symbol_index_key(term);
    if needle.is_empty() {
        return false;
    }
    let mut haystack = format!("{} {}", node.display, node.name).to_ascii_lowercase();
    if let Some(path) = &node.path {
        haystack.push(' ');
        haystack.push_str(&path.to_ascii_lowercase());
    }
    haystack.contains(needle.as_str())
}

pub(super) fn generic_entrypoint_term(term: &str) -> bool {
    matches!(
        symbol_index_key(term).as_str(),
        "entrypoint"
            | "entrypoints"
            | "route"
            | "routes"
            | "endpoint"
            | "endpoints"
            | "handler"
            | "handlers"
            | "worker"
            | "workers"
            | "proxy"
            | "proxies"
            | "webhook"
            | "webhooks"
            | "cli"
            | "job"
            | "jobs"
            | "ingress"
            | "surface"
            | "surfaces"
    )
}

pub(super) fn generic_surface_term(term: &str) -> bool {
    generic_entrypoint_term(term)
        || matches!(
            symbol_index_key(term).as_str(),
            "middleware"
                | "middlewares"
                | "credential"
                | "credentials"
                | "auth"
                | "authorization"
                | "test"
                | "tests"
                | "behavior"
                | "queue"
                | "queues"
        )
}

pub(super) fn generic_credential_term(term: &str) -> bool {
    matches!(
        symbol_index_key(term).as_str(),
        "auth"
            | "authorize"
            | "authorization"
            | "credential"
            | "credentials"
            | "token"
            | "tokens"
            | "jwt"
            | "apikey"
            | "api"
            | "key"
            | "keys"
            | "session"
            | "sessions"
            | "middleware"
            | "route"
            | "routes"
    )
}

pub(super) fn is_ingress_candidate_kind(kind: StoredNodeKind) -> bool {
    matches!(
        kind,
        StoredNodeKind::File
            | StoredNodeKind::RouteSurface
            | StoredNodeKind::WorkerSurface
            | StoredNodeKind::ProxySurface
            | StoredNodeKind::WebhookSurface
            | StoredNodeKind::CliSurface
            | StoredNodeKind::JobSurface
            | StoredNodeKind::QueueSurface
    )
}

pub(super) fn is_credential_candidate_kind(kind: StoredNodeKind) -> bool {
    matches!(
        kind,
        StoredNodeKind::File
            | StoredNodeKind::Function
            | StoredNodeKind::Class
            | StoredNodeKind::RouteSurface
            | StoredNodeKind::WorkerSurface
            | StoredNodeKind::ProxySurface
            | StoredNodeKind::WebhookSurface
            | StoredNodeKind::MiddlewareInstallation
            | StoredNodeKind::CredentialOperation
    )
}

pub(super) fn is_surface_or_file_kind(kind: StoredNodeKind) -> bool {
    kind == StoredNodeKind::File || is_surface_stored_kind(kind)
}

pub(super) fn surface_flow_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Authorizes,
        EdgeKind::Exposes,
        EdgeKind::ForwardsTo,
        EdgeKind::InstallsMiddleware,
        EdgeKind::IssuesCredential,
        EdgeKind::RewritesHeader,
        EdgeKind::StoresCredential,
        EdgeKind::TestedBy,
        EdgeKind::UsesCredential,
        EdgeKind::ValidatesCredential,
    ]
}

pub(super) fn credential_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Authorizes,
        EdgeKind::IssuesCredential,
        EdgeKind::RewritesHeader,
        EdgeKind::StoresCredential,
        EdgeKind::UsesCredential,
        EdgeKind::ValidatesCredential,
    ]
}

pub(super) fn middleware_edge_kinds() -> Vec<EdgeKind> {
    vec![
        EdgeKind::Authorizes,
        EdgeKind::InstallsMiddleware,
        EdgeKind::RewritesHeader,
        EdgeKind::UsesCredential,
        EdgeKind::ValidatesCredential,
    ]
}

pub(super) fn merge_surface_flow_candidate(
    candidates: &mut BTreeMap<String, SurfaceFlowCandidate>,
    node: NodeDisplay,
    token: &str,
    signals: SymbolMatchSignals,
    relation_kind: Option<EdgeKind>,
    rank: i32,
) {
    candidates
        .entry(node.id.clone())
        .and_modify(|existing| {
            existing.signals.merge(signals);
            if !existing.matched_tokens.iter().any(|value| value == token) {
                existing.matched_tokens.push(token.to_string());
                existing.matched_tokens.sort();
            }
            if let Some(kind) = relation_kind.clone() {
                push_unique_edge_kind(&mut existing.relation_kinds, kind);
            }
            existing.rank = existing.rank.max(rank);
        })
        .or_insert_with(|| {
            let mut relation_kinds = Vec::new();
            if let Some(kind) = relation_kind {
                push_unique_edge_kind(&mut relation_kinds, kind);
            }
            SurfaceFlowCandidate {
                node,
                signals,
                matched_tokens: vec![token.to_string()],
                relation_kinds,
                rank,
            }
        });
}

pub(super) fn node_display_from_symbol(symbol: &SymbolLookup) -> NodeDisplay {
    NodeDisplay {
        id: symbol.id.clone(),
        kind: symbol.kind,
        display: format!("{}::{}", symbol.path, symbol.name),
        name: symbol.name.clone(),
        path: Some(symbol.path.clone()),
        language: Some(symbol.language.clone()),
        area_id: symbol.area_id.clone(),
    }
}

pub(super) fn relation_kinds_for_node_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    allowed: &[EdgeKind],
) -> Result<Vec<EdgeKind>, GraphStoreError> {
    let mut out = Vec::new();
    for direction in [NeighborDirection::Outgoing, NeighborDirection::Incoming] {
        for edge in neighbors_from(db, id, direction, None)? {
            if allowed.contains(&edge.kind) {
                push_unique_edge_kind(&mut out, edge.kind);
            }
        }
    }
    Ok(out)
}

pub(super) fn surface_flow_candidates_from<D, S, F, G>(
    db: &D,
    tokens: &[S],
    allowed_kind: F,
    relation_kinds: &[EdgeKind],
    generic_term: G,
    limit: usize,
) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError>
where
    D: ReadableDatabase,
    S: AsRef<str>,
    F: Fn(StoredNodeKind) -> bool,
    G: Fn(&str) -> bool,
{
    if limit == 0 {
        return Ok(Vec::new());
    }
    let terms = bounded_query_terms(tokens);
    if terms.is_empty() {
        return Ok(Vec::new());
    }

    let mut candidates = BTreeMap::new();
    for term in &terms {
        for symbol in symbols_matching_from(
            db,
            term,
            SymbolMatchOptions {
                limit: FLOW_QUERY_LIMIT,
                ..SymbolMatchOptions::default()
            },
        )? {
            if !allowed_kind(symbol.symbol.kind) {
                continue;
            }
            merge_surface_flow_candidate(
                &mut candidates,
                node_display_from_symbol(&symbol.symbol),
                term,
                symbol.signals,
                None,
                symbol.rank,
            );
        }
    }

    let txn = db.begin_read()?;
    for kind in relation_kinds {
        for edge in edges_by_kind_limited_from(db, kind.clone(), FLOW_EDGE_LOOKUP_LIMIT)? {
            for endpoint in [edge.from.as_str(), edge.to.as_str()] {
                let Some(node) = display_from_id_in_txn(&txn, endpoint)? else {
                    continue;
                };
                if !allowed_kind(node.kind) {
                    continue;
                }
                let Some(term) = terms
                    .iter()
                    .find(|term| generic_term(term) || node_text_matches_term(&node, term))
                else {
                    continue;
                };
                merge_surface_flow_candidate(
                    &mut candidates,
                    node,
                    term,
                    SymbolMatchSignals {
                        path: true,
                        ..SymbolMatchSignals::default()
                    },
                    Some(kind.clone()),
                    80,
                );
            }
        }
    }
    drop(txn);

    let relation_family = surface_flow_edge_kinds();
    for candidate in candidates.values_mut() {
        for kind in relation_kinds_for_node_from(db, &candidate.node.id, &relation_family)? {
            push_unique_edge_kind(&mut candidate.relation_kinds, kind);
        }
        candidate.rank += candidate.matched_tokens.len() as i32 * 25;
        candidate.rank += candidate.relation_kinds.len() as i32 * 20;
        if is_surface_stored_kind(candidate.node.kind) {
            candidate.rank += 15;
        }
    }

    let mut out = candidates.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.matched_tokens.len().cmp(&left.matched_tokens.len()))
            .then_with(|| right.relation_kinds.len().cmp(&left.relation_kinds.len()))
            .then_with(|| left.node.display.cmp(&right.node.display))
            .then_with(|| left.node.kind.cmp(&right.node.kind))
            .then_with(|| left.node.id.cmp(&right.node.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn entrypoints_for_task_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
    surface_flow_candidates_from(
        db,
        tokens,
        is_ingress_candidate_kind,
        &[
            EdgeKind::EntrypointFor,
            EdgeKind::Exposes,
            EdgeKind::ForwardsTo,
        ],
        generic_entrypoint_term,
        FLOW_QUERY_LIMIT,
    )
}

pub(super) fn surface_paths_for_behavior_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SurfacePathCandidate>, GraphStoreError> {
    let candidates = surface_flow_candidates_from(
        db,
        tokens,
        is_surface_stored_kind,
        &surface_flow_edge_kinds(),
        generic_surface_term,
        FLOW_QUERY_LIMIT,
    )?;
    let mut paths: BTreeMap<String, SurfacePathCandidate> = BTreeMap::new();
    for candidate in candidates {
        let Some(path) = candidate.node.path.clone() else {
            continue;
        };
        paths
            .entry(path.clone())
            .and_modify(|existing| {
                existing.rank += candidate.rank;
                for token in &candidate.matched_tokens {
                    push_unique_string(&mut existing.matched_tokens, token.clone());
                }
                for kind in &candidate.relation_kinds {
                    push_unique_edge_kind(&mut existing.relation_kinds, kind.clone());
                }
                if existing.surfaces.len() < SUBSYSTEM_NODE_LIMIT
                    && !existing
                        .surfaces
                        .iter()
                        .any(|surface| surface.id == candidate.node.id)
                {
                    existing.surfaces.push(candidate.node.clone());
                }
            })
            .or_insert_with(|| SurfacePathCandidate {
                path,
                surfaces: vec![candidate.node],
                matched_tokens: candidate.matched_tokens,
                relation_kinds: candidate.relation_kinds,
                rank: candidate.rank,
            });
    }
    let mut out = paths.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.matched_tokens.len().cmp(&left.matched_tokens.len()))
            .then_with(|| left.path.cmp(&right.path))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

pub(super) fn credential_flow_candidates_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SurfaceFlowCandidate>, GraphStoreError> {
    let credential_kinds = credential_edge_kinds();
    let mut candidates = surface_flow_candidates_from(
        db,
        tokens,
        is_credential_candidate_kind,
        &credential_kinds,
        generic_credential_term,
        FLOW_QUERY_LIMIT,
    )?;
    candidates.retain(|candidate| {
        candidate.node.kind == StoredNodeKind::CredentialOperation
            || candidate
                .relation_kinds
                .iter()
                .any(|kind| credential_kinds.contains(kind))
            || candidate
                .matched_tokens
                .iter()
                .any(|token| generic_credential_term(token))
    });
    Ok(candidates)
}

pub(super) fn resolve_query_nodes_from<D: ReadableDatabase>(
    db: &D,
    query: &str,
    limit: usize,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let mut out = BTreeMap::new();
    if let Some(node) = node_display_from(db, trimmed)? {
        out.insert(node.id.clone(), node);
    }
    if let Some(file) = resolve_file_path_from(db, trimmed)? {
        let node = node_display_from_node(StoredNode::File(file));
        out.insert(node.id.clone(), node);
    }
    for symbol in symbols_matching_from(
        db,
        trimmed,
        SymbolMatchOptions {
            limit,
            ..SymbolMatchOptions::default()
        },
    )? {
        let node = node_display_from_symbol(&symbol.symbol);
        out.insert(node.id.clone(), node);
        if out.len() >= limit {
            break;
        }
    }
    if trimmed.contains('/') {
        let ids = ids_under_path_limited_from(db, NODES_BY_PATH, trimmed, limit)?;
        let txn = db.begin_read()?;
        for id in ids {
            if let Some(node) = display_from_id_in_txn(&txn, &id)? {
                out.insert(node.id.clone(), node);
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    let mut nodes = out.into_values().collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        left.display
            .cmp(&right.display)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.id.cmp(&right.id))
    });
    nodes.truncate(limit);
    Ok(nodes)
}

pub(super) fn relation_steps_for_node_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
    directions: &[NeighborDirection],
    kinds: &[EdgeKind],
    limit: usize,
) -> Result<Vec<FlowRelationStep>, GraphStoreError> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut steps = BTreeMap::new();
    for direction in directions {
        for edge in neighbors_from(db, id, *direction, None)? {
            if !kinds.contains(&edge.kind) {
                continue;
            }
            let (from_id, to_id) = match direction {
                NeighborDirection::Outgoing => (id, edge.other.as_str()),
                NeighborDirection::Incoming => (edge.other.as_str(), id),
            };
            let Some(from) = node_display_from(db, from_id)? else {
                continue;
            };
            let Some(to) = node_display_from(db, to_id)? else {
                continue;
            };
            steps.insert(
                (from.id.clone(), to.id.clone(), edge.kind.clone()),
                FlowRelationStep {
                    from,
                    to,
                    edge_kind: edge.kind,
                    confidence: edge.confidence,
                    source: edge.source.to_string(),
                },
            );
            if steps.len() >= limit {
                break;
            }
        }
        if steps.len() >= limit {
            break;
        }
    }
    let mut out = steps.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.edge_kind
            .cmp(&right.edge_kind)
            .then_with(|| left.from.display.cmp(&right.from.display))
            .then_with(|| left.to.display.cmp(&right.to.display))
            .then_with(|| left.from.id.cmp(&right.from.id))
            .then_with(|| left.to.id.cmp(&right.to.id))
    });
    out.truncate(limit);
    Ok(out)
}

pub(super) fn insert_chain_step(
    steps: &mut BTreeMap<(String, String, EdgeKind), FlowRelationStep>,
    step: FlowRelationStep,
) {
    steps
        .entry((
            step.from.id.clone(),
            step.to.id.clone(),
            step.edge_kind.clone(),
        ))
        .or_insert(step);
}

pub(super) fn middleware_chain_for_route_from<D: ReadableDatabase>(
    db: &D,
    route_or_file: &str,
) -> Result<FlowChain, GraphStoreError> {
    let roots = resolve_query_nodes_from(db, route_or_file, FLOW_CHAIN_ROOT_LIMIT)?;
    let mut steps = BTreeMap::new();
    let middleware_kinds = middleware_edge_kinds();
    for root in &roots {
        for step in relation_steps_for_node_from(
            db,
            &root.id,
            &[NeighborDirection::Outgoing, NeighborDirection::Incoming],
            &middleware_kinds,
            FLOW_CHAIN_STEP_LIMIT,
        )? {
            insert_chain_step(&mut steps, step);
        }

        for edge in neighbors_from(
            db,
            &root.id,
            NeighborDirection::Incoming,
            Some(EdgeKind::Exposes),
        )? {
            for step in relation_steps_for_node_from(
                db,
                edge.other.as_str(),
                &[NeighborDirection::Outgoing],
                &middleware_kinds,
                FLOW_CHAIN_STEP_LIMIT,
            )? {
                insert_chain_step(&mut steps, step);
            }
        }

        if root.kind != StoredNodeKind::File
            && let Some(path) = &root.path
            && let Some(file) = resolve_file_path_from(db, path)?
        {
            for step in relation_steps_for_node_from(
                db,
                &file.id,
                &[NeighborDirection::Outgoing],
                &middleware_kinds,
                FLOW_CHAIN_STEP_LIMIT,
            )? {
                insert_chain_step(&mut steps, step);
            }
        }
    }
    let mut out = steps.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.edge_kind
            .cmp(&right.edge_kind)
            .then_with(|| left.from.display.cmp(&right.from.display))
            .then_with(|| left.to.display.cmp(&right.to.display))
            .then_with(|| left.from.id.cmp(&right.from.id))
            .then_with(|| left.to.id.cmp(&right.to.id))
    });
    out.truncate(FLOW_CHAIN_STEP_LIMIT);
    Ok(FlowChain { roots, steps: out })
}

pub(super) fn forwarding_chain_for_surface_from<D: ReadableDatabase>(
    db: &D,
    surface: &str,
) -> Result<FlowChain, GraphStoreError> {
    let roots = resolve_query_nodes_from(db, surface, FLOW_CHAIN_ROOT_LIMIT)?;
    let mut steps = BTreeMap::new();
    let mut seen_nodes = BTreeSet::new();
    let mut frontier = roots.iter().map(|node| node.id.clone()).collect::<Vec<_>>();
    let forwarding_kinds = vec![EdgeKind::ForwardsTo, EdgeKind::RewritesHeader];
    for _ in 0..FLOW_CHAIN_DEPTH_LIMIT {
        let mut next = Vec::new();
        for id in frontier {
            if !seen_nodes.insert(id.clone()) {
                continue;
            }
            for step in relation_steps_for_node_from(
                db,
                &id,
                &[NeighborDirection::Outgoing],
                &forwarding_kinds,
                FLOW_CHAIN_STEP_LIMIT,
            )? {
                if step.edge_kind == EdgeKind::ForwardsTo && !seen_nodes.contains(&step.to.id) {
                    next.push(step.to.id.clone());
                }
                insert_chain_step(&mut steps, step);
                if steps.len() >= FLOW_CHAIN_STEP_LIMIT {
                    break;
                }
            }
            if steps.len() >= FLOW_CHAIN_STEP_LIMIT {
                break;
            }
        }
        if next.is_empty() || steps.len() >= FLOW_CHAIN_STEP_LIMIT {
            break;
        }
        next.sort();
        next.dedup();
        frontier = next;
    }
    let mut out = steps.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.edge_kind
            .cmp(&right.edge_kind)
            .then_with(|| left.from.display.cmp(&right.from.display))
            .then_with(|| left.to.display.cmp(&right.to.display))
            .then_with(|| left.from.id.cmp(&right.from.id))
            .then_with(|| left.to.id.cmp(&right.to.id))
    });
    out.truncate(FLOW_CHAIN_STEP_LIMIT);
    Ok(FlowChain { roots, steps: out })
}

pub(super) fn tests_for_surface_or_symbol_from<D: ReadableDatabase>(
    db: &D,
    id: &str,
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    let roots = resolve_query_nodes_from(db, id, FLOW_CHAIN_ROOT_LIMIT)?;
    let mut tests = BTreeMap::new();
    for root in &roots {
        let mut seed_ids = vec![root.id.clone()];
        if let Some(path) = &root.path
            && let Some(file) = resolve_file_path_from(db, path)?
        {
            push_unique_string(&mut seed_ids, file.id);
        }
        for seed_id in seed_ids {
            for step in relation_steps_for_node_from(
                db,
                &seed_id,
                &[NeighborDirection::Outgoing, NeighborDirection::Incoming],
                &[EdgeKind::TestedBy],
                FLOW_CHAIN_STEP_LIMIT,
            )? {
                for node in [step.from, step.to] {
                    if node.kind == StoredNodeKind::BehaviorTestSurface {
                        tests.insert(node.id.clone(), node);
                    }
                }
            }
        }
    }
    let mut out = tests.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.display
            .cmp(&right.display)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

pub(super) fn behavior_tests_for_task_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<NodeDisplay>, GraphStoreError> {
    let candidates = surface_flow_candidates_from(
        db,
        tokens,
        |kind| kind == StoredNodeKind::BehaviorTestSurface,
        &[EdgeKind::TestedBy, EdgeKind::ValidatesCredential],
        generic_surface_term,
        FLOW_QUERY_LIMIT,
    )?;
    let mut tests = BTreeMap::new();
    for candidate in candidates {
        tests.insert(candidate.node.id.clone(), candidate.node);
    }
    let mut out = tests.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        left.display
            .cmp(&right.display)
            .then_with(|| left.id.cmp(&right.id))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

pub(super) fn subsystem_path_for_node<D: ReadableDatabase>(
    db: &D,
    node: &NodeDisplay,
) -> Result<(Option<String>, String), GraphStoreError> {
    if let Some(area_id) = &node.area_id {
        let txn = db.begin_read()?;
        if let Some(area) = read_table_node::<AreaNode>(&txn, AREAS, area_id)? {
            return Ok((Some(area.id), area.path_prefix));
        }
    }
    let path = node.path.as_deref().unwrap_or(node.display.as_str());
    let prefix = path
        .split('/')
        .take(2)
        .collect::<Vec<_>>()
        .join("/")
        .trim_matches('/')
        .to_string();
    Ok((node.area_id.clone(), prefix))
}

pub(super) fn merge_subsystem_candidate<D: ReadableDatabase>(
    db: &D,
    groups: &mut BTreeMap<String, SubsystemCandidate>,
    node: NodeDisplay,
    matched_tokens: &[String],
    rank: i32,
) -> Result<(), GraphStoreError> {
    let (id, path_prefix) = subsystem_path_for_node(db, &node)?;
    if path_prefix.is_empty() {
        return Ok(());
    }
    groups
        .entry(path_prefix.clone())
        .and_modify(|existing| {
            existing.rank += rank;
            for token in matched_tokens {
                push_unique_string(&mut existing.matched_tokens, token.clone());
            }
            if existing.nodes.len() < SUBSYSTEM_NODE_LIMIT
                && !existing
                    .nodes
                    .iter()
                    .any(|candidate| candidate.id == node.id)
            {
                existing.nodes.push(node.clone());
            }
        })
        .or_insert_with(|| SubsystemCandidate {
            id,
            path_prefix,
            matched_tokens: matched_tokens.to_vec(),
            nodes: vec![node],
            rank,
        });
    Ok(())
}

pub(super) fn subsystems_matching_from<D: ReadableDatabase, S: AsRef<str>>(
    db: &D,
    tokens: &[S],
) -> Result<Vec<SubsystemCandidate>, GraphStoreError> {
    let terms = bounded_query_terms(tokens);
    if terms.is_empty() {
        return Ok(Vec::new());
    }
    let mut groups = BTreeMap::new();
    for candidate in surface_flow_candidates_from(
        db,
        tokens,
        is_surface_or_file_kind,
        &surface_flow_edge_kinds(),
        generic_surface_term,
        FLOW_QUERY_LIMIT,
    )? {
        merge_subsystem_candidate(
            db,
            &mut groups,
            candidate.node,
            &candidate.matched_tokens,
            candidate.rank,
        )?;
    }
    let anchors = task_anchor_candidates_from(db, &terms, FLOW_QUERY_LIMIT)?;
    for anchor in anchors {
        merge_subsystem_candidate(
            db,
            &mut groups,
            anchor.node,
            &anchor.matched_tokens,
            anchor.signals.signal_count() as i32 * 30,
        )?;
    }
    let mut out = groups.into_values().collect::<Vec<_>>();
    out.sort_by(|left, right| {
        right
            .rank
            .cmp(&left.rank)
            .then_with(|| right.matched_tokens.len().cmp(&left.matched_tokens.len()))
            .then_with(|| left.path_prefix.cmp(&right.path_prefix))
    });
    out.truncate(FLOW_QUERY_LIMIT);
    Ok(out)
}

pub(super) fn coverage_tokens_for_task_class(task_class: &str) -> Vec<String> {
    let mut tokens = bounded_query_terms(&[task_class]);
    let lower = symbol_index_key(task_class);
    if lower.contains("auth") || lower.contains("token") || lower.contains("credential") {
        for token in ["auth", "token", "credential", "middleware", "route", "test"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    if lower.contains("route")
        || lower.contains("entrypoint")
        || lower.contains("ingress")
        || lower.contains("surface")
    {
        for token in ["route", "worker", "proxy", "webhook", "cli", "job"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    if lower.contains("config") {
        for token in ["config", "entrypoint", "middleware"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    if lower.contains("usage") || lower.contains("caller") || lower.contains("impact") {
        for token in ["route", "middleware", "auth", "test"] {
            push_unique_string(&mut tokens, token.to_string());
        }
    }
    tokens
}

pub(super) fn coverage_for_task_class_from<D: ReadableDatabase>(
    db: &D,
    task_class: &str,
) -> Result<TaskClassCoverage, GraphStoreError> {
    let tokens = coverage_tokens_for_task_class(task_class);
    let entrypoints = entrypoints_for_task_from(db, &tokens)?;
    let surface_paths = surface_paths_for_behavior_from(db, &tokens)?;
    let credential_flows = credential_flow_candidates_from(db, &tokens)?;
    let subsystems = subsystems_matching_from(db, &tokens)?;

    let mut tests = BTreeMap::new();
    for id in entrypoints
        .iter()
        .chain(credential_flows.iter())
        .take(10)
        .map(|candidate| candidate.node.id.as_str())
    {
        for test in tests_for_surface_or_symbol_from(db, id)? {
            tests.insert(test.id.clone(), test);
        }
    }
    for test in behavior_tests_for_task_from(db, &tokens)? {
        tests.insert(test.id.clone(), test);
    }
    let tests = tests.into_values().collect::<Vec<_>>();

    let mut missing = Vec::new();
    if entrypoints.is_empty() {
        missing.push("entrypoints".to_string());
    }
    if surface_paths.is_empty() {
        missing.push("surface_paths".to_string());
    }
    if credential_flows.is_empty()
        && tokens
            .iter()
            .any(|token| generic_credential_term(token.as_str()))
    {
        missing.push("credential_flows".to_string());
    }
    if tests.is_empty() {
        missing.push("behavior_tests".to_string());
    }

    Ok(TaskClassCoverage {
        task_class: task_class.to_string(),
        tokens,
        entrypoints,
        surface_paths,
        credential_flows,
        subsystems,
        tests,
        missing,
    })
}
