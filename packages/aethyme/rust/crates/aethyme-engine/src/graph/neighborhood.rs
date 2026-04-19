use std::collections::BTreeSet;

use crate::map::RepositoryMap;

pub fn dependency_frontier(map: &RepositoryMap, target: &str) -> Vec<String> {
    let seeds = map.matching_target_ids(target);
    let mut frontier = BTreeSet::new();
    for edge in &map.edges {
        if seeds
            .iter()
            .any(|seed| seed == &edge.from || edge.from.ends_with(seed) || edge.to.ends_with(seed))
            && !seeds.iter().any(|seed| seed == &edge.to)
        {
            frontier.insert(map.display_for(&edge.to));
        }
    }
    frontier.into_iter().collect()
}

pub fn impact_frontier(map: &RepositoryMap, target: &str) -> Vec<String> {
    let seeds = map.matching_target_ids(target);
    let mut frontier = BTreeSet::new();
    for edge in &map.edges {
        if seeds
            .iter()
            .any(|seed| seed == &edge.to || edge.to.ends_with(seed) || edge.from.ends_with(seed))
            && !seeds.iter().any(|seed| seed == &edge.from)
        {
            frontier.insert(map.display_for(&edge.from));
        }
    }
    frontier.into_iter().collect()
}
