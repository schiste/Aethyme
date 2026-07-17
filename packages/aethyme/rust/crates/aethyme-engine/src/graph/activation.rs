use std::collections::{HashMap, VecDeque};

use crate::context_pack::Anchor;
use crate::graph::neighborhood::matching_target_ids_redb;
use crate::map::RepositoryMap;
use crate::model::edge::EdgeKind;
use crate::model::task::TaskKind;
use crate::store::redb::graph_store::{
    GraphRelation, GraphStoreError, NeighborDirection, ReadOnlyGraphStore,
};

const EDGE_KIND_COUNT: usize = 9;

pub struct HormoneProfile {
    pub weights: [f64; EDGE_KIND_COUNT],
    pub decay: f64,
    pub threshold: f64,
    pub max_hops: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeActivation {
    pub id: String,
    pub activation: f64,
    pub depth: usize,
    pub source_seed: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActivationMap {
    pub activations: Vec<NodeActivation>,
    pub seed_count: usize,
    pub total_nodes_visited: usize,
    pub max_depth_reached: usize,
}

fn edge_kind_index(kind: &EdgeKind) -> usize {
    match kind {
        EdgeKind::Contains => 0,
        EdgeKind::BelongsTo => 1,
        EdgeKind::Defines => 2,
        EdgeKind::Imports => 3,
        EdgeKind::Calls => 4,
        EdgeKind::References => 5,
        EdgeKind::Documents => 6,
        EdgeKind::Configures => 7,
        EdgeKind::EntrypointFor => 8,
    }
}

struct AdjacencyIndex {
    outgoing: HashMap<String, Vec<(String, EdgeKind)>>,
    incoming: HashMap<String, Vec<(String, EdgeKind)>>,
}

impl AdjacencyIndex {
    fn build(map: &RepositoryMap) -> Self {
        let mut outgoing: HashMap<String, Vec<(String, EdgeKind)>> = HashMap::new();
        let mut incoming: HashMap<String, Vec<(String, EdgeKind)>> = HashMap::new();
        for edge in &map.edges {
            outgoing
                .entry(edge.from.to_string())
                .or_default()
                .push((edge.to.to_string(), edge.kind.clone()));
            incoming
                .entry(edge.to.to_string())
                .or_default()
                .push((edge.from.to_string(), edge.kind.clone()));
        }
        Self { outgoing, incoming }
    }

    fn neighbors(&self, node_id: &str) -> Vec<(&str, &EdgeKind)> {
        let mut result = Vec::new();
        if let Some(out) = self.outgoing.get(node_id) {
            for (to, kind) in out {
                result.push((to.as_str(), kind));
            }
        }
        if let Some(inc) = self.incoming.get(node_id) {
            for (from, kind) in inc {
                result.push((from.as_str(), kind));
            }
        }
        result
    }
}

// Edge kind indices: Contains=0, BelongsTo=1, Defines=2, Imports=3, Calls=4,
//                    References=5, Documents=6, Configures=7, EntrypointFor=8

pub fn hormone_profile(kind: &TaskKind) -> HormoneProfile {
    match kind {
        TaskKind::NavigateConfigOwnership => HormoneProfile {
            //           Con   Bel   Def   Imp   Cal   Ref   Doc   Cfg   Ent
            weights: [0.5, 0.5, 0.4, 0.3, 0.2, 0.2, 0.4, 0.95, 0.9],
            decay: 0.55,
            threshold: 0.05,
            max_hops: 4,
        },
        TaskKind::ChangeSymbol => HormoneProfile {
            weights: [0.4, 0.3, 0.6, 0.9, 0.85, 0.7, 0.3, 0.2, 0.2],
            decay: 0.6,
            threshold: 0.05,
            max_hops: 3,
        },
        TaskKind::TraceImpact => HormoneProfile {
            weights: [0.3, 0.3, 0.5, 0.8, 0.9, 0.7, 0.2, 0.4, 0.5],
            decay: 0.55,
            threshold: 0.04,
            max_hops: 4,
        },
        TaskKind::ExplainRepo => HormoneProfile {
            weights: [0.7, 0.5, 0.5, 0.4, 0.3, 0.3, 0.8, 0.6, 0.7],
            decay: 0.5,
            threshold: 0.03,
            max_hops: 5,
        },
        _ => HormoneProfile {
            weights: [0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5],
            decay: 0.5,
            threshold: 0.05,
            max_hops: 3,
        },
    }
}

pub fn spread_activation(
    map: &RepositoryMap,
    anchors: &[Anchor],
    profile: &HormoneProfile,
) -> ActivationMap {
    let adj = AdjacencyIndex::build(map);

    // Resolve anchor IDs to all matching graph node IDs.
    // Anchors use human-readable IDs (area names, file paths) but edges
    // reference graph node IDs with prefixes (area:X, file:X, dir:Repo:X).
    let mut seed_ids: Vec<String> = Vec::new();
    for anchor in anchors {
        let resolved = map.matching_target_ids(&anchor.id);
        for id in resolved {
            if !seed_ids.contains(&id) {
                seed_ids.push(id);
            }
        }
        // Also seed from the file path if present (anchors often carry both)
        if let Some(file) = &anchor.file {
            let file_resolved = map.matching_target_ids(file);
            for id in file_resolved {
                if !seed_ids.contains(&id) {
                    seed_ids.push(id);
                }
            }
        }
    }
    let seed_count = seed_ids.len();

    // BFS state: node_id → (best_activation, depth, source_seed)
    let mut best: HashMap<String, (f64, usize, String)> = HashMap::new();
    let mut queue: VecDeque<(String, f64, usize, String)> = VecDeque::new();
    let mut total_visited = 0usize;
    let mut max_depth = 0usize;

    for seed in &seed_ids {
        best.insert(seed.clone(), (1.0, 0, seed.clone()));
        queue.push_back((seed.clone(), 1.0, 0, seed.clone()));
        total_visited += 1;
    }

    while let Some((node_id, current_activation, depth, source)) = queue.pop_front() {
        if depth >= profile.max_hops {
            continue;
        }

        for (neighbor, edge_kind) in adj.neighbors(&node_id) {
            let weight = profile.weights[edge_kind_index(edge_kind)];
            let propagated = current_activation * weight * profile.decay;

            if propagated < profile.threshold {
                continue;
            }

            let next_depth = depth + 1;
            let is_new = match best.get(neighbor) {
                Some((existing, _, _)) => {
                    if propagated > *existing {
                        // Max collision: keep higher activation
                        best.insert(
                            neighbor.to_string(),
                            (propagated, next_depth, source.clone()),
                        );
                        true
                    } else {
                        false
                    }
                }
                None => {
                    best.insert(
                        neighbor.to_string(),
                        (propagated, next_depth, source.clone()),
                    );
                    total_visited += 1;
                    true
                }
            };

            if is_new {
                if next_depth > max_depth {
                    max_depth = next_depth;
                }
                queue.push_back((neighbor.to_string(), propagated, next_depth, source.clone()));
            }
        }
    }

    let mut activations: Vec<NodeActivation> = best
        .into_iter()
        .map(|(id, (activation, depth, source_seed))| NodeActivation {
            id,
            activation,
            depth,
            source_seed,
        })
        .collect();

    activations.sort_by(|a, b| {
        b.activation
            .partial_cmp(&a.activation)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    ActivationMap {
        activations,
        seed_count,
        total_nodes_visited: total_visited,
        max_depth_reached: max_depth,
    }
}

pub fn spread_activation_redb(
    store: &ReadOnlyGraphStore,
    anchors: &[Anchor],
    profile: &HormoneProfile,
) -> Result<ActivationMap, GraphStoreError> {
    let mut seed_ids = Vec::new();
    for anchor in anchors {
        for id in resolve_activation_seed_redb(store, &anchor.id)? {
            if !seed_ids.contains(&id) {
                seed_ids.push(id);
            }
        }
        if let Some(file) = &anchor.file {
            for id in resolve_activation_seed_redb(store, file)? {
                if !seed_ids.contains(&id) {
                    seed_ids.push(id);
                }
            }
        }
    }
    spread_activation_ids_redb(store, seed_ids, profile)
}

pub fn spread_from_seed(map: &RepositoryMap, seed_id: &str, max_hops: usize) -> ActivationMap {
    // Try resolving the seed_id and also probe with a file-path anchor
    let anchor = Anchor::new(
        crate::context_pack::AnchorKind::File,
        seed_id,
        Some(seed_id),
        "single-seed activation",
    );
    let profile = HormoneProfile {
        weights: [0.5; EDGE_KIND_COUNT],
        decay: 0.5,
        threshold: 0.05,
        max_hops,
    };
    spread_activation(map, &[anchor], &profile)
}

pub fn spread_from_seed_redb(
    store: &ReadOnlyGraphStore,
    seed_id: &str,
    max_hops: usize,
) -> Result<ActivationMap, GraphStoreError> {
    let anchor = Anchor::new(
        crate::context_pack::AnchorKind::File,
        seed_id,
        Some(seed_id),
        "single-seed activation",
    );
    let profile = HormoneProfile {
        weights: [0.5; EDGE_KIND_COUNT],
        decay: 0.5,
        threshold: 0.05,
        max_hops,
    };
    spread_activation_redb(store, &[anchor], &profile)
}

fn resolve_activation_seed_redb(
    store: &ReadOnlyGraphStore,
    value: &str,
) -> Result<Vec<String>, GraphStoreError> {
    matching_target_ids_redb(store, value)
}

fn spread_activation_ids_redb(
    store: &ReadOnlyGraphStore,
    seed_ids: Vec<String>,
    profile: &HormoneProfile,
) -> Result<ActivationMap, GraphStoreError> {
    let seed_count = seed_ids.len();
    let mut best: HashMap<String, (f64, usize, String)> = HashMap::new();
    let mut queue: VecDeque<(String, f64, usize, String)> = VecDeque::new();
    let mut total_visited = 0usize;
    let mut max_depth = 0usize;

    for seed in &seed_ids {
        best.insert(seed.clone(), (1.0, 0, seed.clone()));
        queue.push_back((seed.clone(), 1.0, 0, seed.clone()));
        total_visited += 1;
    }

    while let Some((node_id, current_activation, depth, source)) = queue.pop_front() {
        if depth >= profile.max_hops {
            continue;
        }

        for (neighbor, edge_kind) in activation_neighbors_redb(store, &node_id)? {
            let weight = profile.weights[edge_kind_index(&edge_kind)];
            let propagated = current_activation * weight * profile.decay;

            if propagated < profile.threshold {
                continue;
            }

            let next_depth = depth + 1;
            let is_new = match best.get(neighbor.as_str()) {
                Some((existing, _, _)) => {
                    if propagated > *existing {
                        best.insert(neighbor.clone(), (propagated, next_depth, source.clone()));
                        true
                    } else {
                        false
                    }
                }
                None => {
                    best.insert(neighbor.clone(), (propagated, next_depth, source.clone()));
                    total_visited += 1;
                    true
                }
            };

            if is_new {
                max_depth = max_depth.max(next_depth);
                queue.push_back((neighbor, propagated, next_depth, source.clone()));
            }
        }
    }

    let mut activations = best
        .into_iter()
        .map(|(id, (activation, depth, source_seed))| NodeActivation {
            id,
            activation,
            depth,
            source_seed,
        })
        .collect::<Vec<_>>();

    activations.sort_by(|a, b| {
        b.activation
            .partial_cmp(&a.activation)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    Ok(ActivationMap {
        activations,
        seed_count,
        total_nodes_visited: total_visited,
        max_depth_reached: max_depth,
    })
}

fn activation_neighbors_redb(
    store: &ReadOnlyGraphStore,
    node_id: &str,
) -> Result<Vec<(String, EdgeKind)>, GraphStoreError> {
    let mut neighbors = Vec::new();
    for record in store.neighbors(node_id, NeighborDirection::Outgoing, None)? {
        neighbors.push((record.other.to_string(), record.kind));
    }
    for record in store.neighbors(node_id, NeighborDirection::Incoming, None)? {
        neighbors.push((record.other.to_string(), record.kind));
    }
    for relation in [
        GraphRelation::Callers,
        GraphRelation::Callees,
        GraphRelation::Docs,
        GraphRelation::Configs,
        GraphRelation::Imports,
        GraphRelation::Importers,
        GraphRelation::References,
    ] {
        for item in store.relation_view(node_id, relation)?.items {
            neighbors.push((item.node.id, item.edge_kind));
        }
    }
    neighbors.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
    neighbors.dedup();
    Ok(neighbors)
}

impl ActivationMap {
    pub fn activated_set(&self, min_activation: f64) -> std::collections::HashSet<String> {
        self.activations
            .iter()
            .filter(|node| node.activation >= min_activation)
            .map(|node| node.id.clone())
            .collect()
    }

    pub fn top_activated(&self, limit: usize) -> Vec<(String, f64)> {
        self.activations
            .iter()
            .take(limit)
            .map(|node| (node.id.clone(), node.activation))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::map::RepositoryMap;
    use crate::model::task::TaskInput;

    fn test_repo(suffix: &str) -> (std::path::PathBuf, RepositoryMap) {
        let root = std::env::temp_dir().join(format!("aethyme_activation_{suffix}"));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("GameEngine/src")).expect("create engine dir");
        fs::create_dir_all(root.join("Other")).expect("create other dir");
        fs::write(
            root.join("GameEngine/src/lib.rs"),
            "pub fn main() {}\npub fn helper() {}\n",
        )
        .expect("write entrypoint");
        fs::write(
            root.join("GameEngine/Cargo.toml"),
            "[package]\nname='demo'\n",
        )
        .expect("write manifest");
        fs::write(root.join("Other/project.godot"), "[application]\n").expect("write config");
        fs::write(root.join("README.md"), "# Demo\n").expect("write readme");

        let map = RepositoryMap::build(&root).expect("build map");
        (root, map)
    }

    #[test]
    fn seeds_have_activation_one() {
        let (root, map) = test_repo("seeds");
        let task = TaskInput::from_task_text(
            "Find the manifest that manages the main code entrypoint in the GameEngine area",
        );
        let anchors = crate::graph::anchors::resolve_anchors(&map, &task, 3);
        let profile = hormone_profile(&task.kind);
        let activation = spread_activation(&map, &anchors, &profile);

        // After matching_target_ids resolution, seeds are graph node IDs.
        // All seed nodes (depth 0) should have activation 1.0.
        let seed_nodes: Vec<&NodeActivation> = activation
            .activations
            .iter()
            .filter(|n| n.depth == 0)
            .collect();
        assert!(!seed_nodes.is_empty(), "should have at least one seed node");
        for node in &seed_nodes {
            assert!(
                (node.activation - 1.0).abs() < f64::EPSILON,
                "seed {} should have activation 1.0, got {}",
                node.id,
                node.activation
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn activation_decays_with_distance() {
        let (root, map) = test_repo("decays");
        let task = TaskInput::from_task_text(
            "Find the manifest that manages the main code entrypoint in the GameEngine area",
        );
        let anchors = crate::graph::anchors::resolve_anchors(&map, &task, 3);
        let profile = hormone_profile(&task.kind);
        let activation = spread_activation(&map, &anchors, &profile);

        // Non-seed nodes (depth > 0) should have activation < 1.0
        let non_seed_nodes: Vec<&NodeActivation> = activation
            .activations
            .iter()
            .filter(|n| n.depth > 0)
            .collect();

        for node in &non_seed_nodes {
            assert!(
                node.activation < 1.0,
                "non-seed {} should have activation < 1.0, got {}",
                node.id,
                node.activation
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn threshold_cutoff_limits_propagation() {
        let (root, map) = test_repo("threshold");
        let task = TaskInput::from_task_text("Explain this repo");
        let anchors = crate::graph::anchors::resolve_anchors(&map, &task, 5);
        let profile = hormone_profile(&task.kind);
        let activation = spread_activation(&map, &anchors, &profile);

        for node in &activation.activations {
            assert!(
                node.activation >= profile.threshold,
                "node {} has activation {} below threshold {}",
                node.id,
                node.activation,
                profile.threshold,
            );
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn navigate_config_amplifies_configures_over_calls() {
        let profile = hormone_profile(&TaskKind::NavigateConfigOwnership);
        let configures_weight = profile.weights[edge_kind_index(&EdgeKind::Configures)];
        let calls_weight = profile.weights[edge_kind_index(&EdgeKind::Calls)];
        assert!(
            configures_weight > calls_weight,
            "NavigateConfig should amplify Configures ({}) over Calls ({})",
            configures_weight,
            calls_weight,
        );
    }

    #[test]
    fn spread_from_seed_returns_activation_map() {
        let (root, map) = test_repo("from_seed");
        let activation = spread_from_seed(&map, "GameEngine/Cargo.toml", 3);

        assert!(
            activation.seed_count >= 1,
            "should resolve at least one seed"
        );
        assert!(!activation.activations.is_empty());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn activated_set_filters_by_threshold() {
        let (root, map) = test_repo("activated_set");
        let task = TaskInput::from_task_text("Explain this repo");
        let anchors = crate::graph::anchors::resolve_anchors(&map, &task, 5);
        let profile = hormone_profile(&task.kind);
        let activation = spread_activation(&map, &anchors, &profile);

        let high_set = activation.activated_set(0.5);
        let low_set = activation.activated_set(0.1);

        assert!(
            high_set.len() <= low_set.len(),
            "higher threshold should yield fewer or equal nodes",
        );

        let _ = fs::remove_dir_all(&root);
    }
}
