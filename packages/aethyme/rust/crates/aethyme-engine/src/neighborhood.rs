use std::collections::BTreeSet;

use crate::map::RepositoryMap;

pub fn dependency_frontier(map: &RepositoryMap, target: &str) -> Vec<String> {
    let mut frontier = BTreeSet::new();
    for edge in &map.edges {
        if edge.from == target || edge.from.ends_with(target) {
            frontier.insert(edge.to.clone());
        }
        if edge.from.ends_with(target) {
            frontier.insert(edge.to.clone());
        }
    }
    frontier.into_iter().collect()
}

pub fn impact_frontier(map: &RepositoryMap, target: &str) -> Vec<String> {
    let mut frontier = BTreeSet::new();
    for edge in &map.edges {
        if edge.to == target || edge.to.ends_with(target) {
            frontier.insert(edge.from.clone());
        }
    }
    frontier.into_iter().collect()
}
