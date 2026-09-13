//! Which reclamation goes first, and whether the budget can be met at all.
//!
//! `retained_bytes_budget` used to set a boolean and nothing else: the broker
//! reported that it was over budget and then reclaimed in session-id order,
//! which is insertion order wearing a policy's clothes. A budget that is only
//! ever reported is a gauge (#176).
//!
//! Two things make it a budget. Ordering: over budget, the largest retentions
//! go first, because `gc apply` under a time budget drains the plan in order
//! and stops, so whatever sits at the front is what actually gets reclaimed.
//! And sufficiency: a plan says whether applying all of it would bring
//! retention back under budget, so "the budget cannot be met" is a thing the
//! broker states rather than something an operator infers from two numbers.

use std::cmp::Ordering;

/// The rule a plan ordered itself by, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimOrder {
    /// Within budget: age is the binding policy, so the longest-retained goes
    /// first and reclamation stays predictable run to run.
    OldestFirst,
    /// Over budget: bytes are the binding pressure. A bounded apply that gets
    /// through three of forty candidates should have spent that time on the
    /// three that matter.
    LargestFirst,
}

impl ReclaimOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            ReclaimOrder::OldestFirst => "oldest_first",
            ReclaimOrder::LargestFirst => "largest_first",
        }
    }
}

/// The two facts any reclamation can be ranked by, plus a tiebreak.
///
/// Deliberately not `reclaim::ReclaimCandidate`, which is a thing to remove.
/// This is a view onto one, carrying only what ordering is allowed to consider
/// -- so worktrees, build caches and orphaned roots can be ranked by one rule
/// without that rule knowing what any of them are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReclaimRanking {
    /// Stable identity, used only to keep equal candidates in a fixed order.
    pub id: i64,
    /// When this stopped being live. Smaller is older.
    pub retained_since_ms: i64,
    pub estimated_bytes: u64,
}

/// Whether a budget is set and exceeded.
///
/// A budget of `0` is off, not "no bytes allowed" -- the same convention the
/// rest of the retention policy uses for a disabled limit.
pub fn over_budget(retained_bytes: u64, budget: u64) -> bool {
    budget > 0 && retained_bytes >= budget
}

/// How far over the budget the retained bytes are. `0` when within it or when
/// no budget is set.
pub fn deficit_bytes(retained_bytes: u64, budget: u64) -> u64 {
    if budget == 0 {
        0
    } else {
        retained_bytes.saturating_sub(budget)
    }
}

/// Whether applying everything in a plan would bring retention under budget.
///
/// This is the question the boolean never answered. Being over budget with a
/// plan that closes the gap is a queue of work; being over budget with a plan
/// that cannot is a policy that will never be satisfied, and the two should
/// not read the same.
pub fn clears_budget(retained_bytes: u64, budget: u64, reclaimable_bytes: u64) -> bool {
    if budget == 0 {
        return true;
    }
    retained_bytes.saturating_sub(reclaimable_bytes) < budget
}

pub fn order_for(retained_bytes: u64, budget: u64) -> ReclaimOrder {
    if over_budget(retained_bytes, budget) {
        ReclaimOrder::LargestFirst
    } else {
        ReclaimOrder::OldestFirst
    }
}

/// Total ordering, so a plan is byte-identical across runs on unchanged input.
///
/// Both rules fall through to the other criterion and then to `id`. Ties are
/// common -- empty worktrees all measure zero, worktrees closed by one sweep
/// share a timestamp -- and a plan whose order wobbles between runs produces a
/// digest that wobbles with it, which would invalidate an operator's
/// confirmation for no reason at all.
pub fn compare(order: ReclaimOrder, left: &ReclaimRanking, right: &ReclaimRanking) -> Ordering {
    match order {
        ReclaimOrder::OldestFirst => left
            .retained_since_ms
            .cmp(&right.retained_since_ms)
            .then(right.estimated_bytes.cmp(&left.estimated_bytes))
            .then(left.id.cmp(&right.id)),
        ReclaimOrder::LargestFirst => right
            .estimated_bytes
            .cmp(&left.estimated_bytes)
            .then(left.retained_since_ms.cmp(&right.retained_since_ms))
            .then(left.id.cmp(&right.id)),
    }
}

/// Sort in place by `order`, reading each item's ranking facts through `key`.
pub fn sort_by_order<T>(items: &mut [T], order: ReclaimOrder, key: impl Fn(&T) -> ReclaimRanking) {
    items.sort_by(|left, right| compare(order, &key(left), &key(right)));
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_073_741_824;

    fn candidate(id: i64, retained_since_ms: i64, estimated_bytes: u64) -> ReclaimRanking {
        ReclaimRanking {
            id,
            retained_since_ms,
            estimated_bytes,
        }
    }

    fn ids(mut items: Vec<ReclaimRanking>, order: ReclaimOrder) -> Vec<i64> {
        sort_by_order(&mut items, order, |item| *item);
        items.iter().map(|item| item.id).collect()
    }

    #[test]
    fn a_zero_budget_is_off_rather_than_immediately_exceeded() {
        assert!(!over_budget(u64::MAX, 0));
        assert_eq!(deficit_bytes(u64::MAX, 0), 0);
        assert!(clears_budget(u64::MAX, 0, 0));
        assert_eq!(order_for(u64::MAX, 0), ReclaimOrder::OldestFirst);
    }

    #[test]
    fn the_budget_binds_at_the_boundary() {
        assert!(!over_budget(GB - 1, GB));
        assert!(over_budget(GB, GB));
        assert_eq!(order_for(GB, GB), ReclaimOrder::LargestFirst);
    }

    #[test]
    fn within_budget_the_longest_retained_goes_first() {
        let items = vec![candidate(1, 300, 9 * GB), candidate(2, 100, GB)];
        assert_eq!(ids(items, ReclaimOrder::OldestFirst), vec![2, 1]);
    }

    #[test]
    fn over_budget_the_largest_goes_first_even_when_it_is_the_newest() {
        // The point of the switch: a bounded apply that only gets through one
        // candidate should spend that time on the 9 GB, not the 1 GB that
        // happens to have been sitting there longer.
        let items = vec![candidate(1, 300, 9 * GB), candidate(2, 100, GB)];
        assert_eq!(ids(items, ReclaimOrder::LargestFirst), vec![1, 2]);
    }

    #[test]
    fn equal_candidates_keep_a_fixed_order_in_both_rules() {
        let items = vec![
            candidate(3, 100, GB),
            candidate(1, 100, GB),
            candidate(2, 100, GB),
        ];
        assert_eq!(ids(items.clone(), ReclaimOrder::OldestFirst), vec![1, 2, 3]);
        assert_eq!(ids(items, ReclaimOrder::LargestFirst), vec![1, 2, 3]);
    }

    #[test]
    fn each_rule_falls_through_to_the_other_before_reaching_the_tiebreak() {
        // Same age, different sizes: the oldest-first rule still has to put
        // them in a defensible order, and the useful one is largest first.
        let same_age = vec![candidate(1, 100, GB), candidate(2, 100, 9 * GB)];
        assert_eq!(ids(same_age, ReclaimOrder::OldestFirst), vec![2, 1]);

        // Same size, different ages: largest-first falls through to oldest.
        let same_size = vec![candidate(1, 300, GB), candidate(2, 100, GB)];
        assert_eq!(ids(same_size, ReclaimOrder::LargestFirst), vec![2, 1]);
    }

    #[test]
    fn a_plan_that_closes_the_gap_reads_differently_from_one_that_cannot() {
        // The measured case: 37.4 GB retained against a 1 GB budget, with
        // 3.2 MB reclaimable. Over budget either way; only one is actionable.
        let retained = 37 * GB;
        assert!(over_budget(retained, GB));
        assert!(!clears_budget(retained, GB, 3_200_000));
        assert!(clears_budget(retained, GB, retained));
    }

    #[test]
    fn clearing_the_budget_means_strictly_under_it() {
        // `over_budget` triggers at equality, so a plan that lands exactly on
        // the budget has not cleared it and must not claim to have.
        assert!(!clears_budget(2 * GB, GB, GB));
        assert!(clears_budget(2 * GB, GB, GB + 1));
    }

    #[test]
    fn the_deficit_is_what_has_to_go() {
        assert_eq!(deficit_bytes(3 * GB, GB), 2 * GB);
        assert_eq!(deficit_bytes(GB / 2, GB), 0);
    }

    #[test]
    fn no_two_orders_read_alike() {
        assert_ne!(
            ReclaimOrder::OldestFirst.as_str(),
            ReclaimOrder::LargestFirst.as_str()
        );
    }
}
