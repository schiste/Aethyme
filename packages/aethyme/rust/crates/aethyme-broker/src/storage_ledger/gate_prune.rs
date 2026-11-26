//! Pruning superseded workspace build units out of over-budget gate caches,
//! before any whole entry is evicted.
//!
//! A gate cache entry (`<host cache>/gates/<repository>/<key>`) is a cargo
//! target shared by every gate run of the repository. Each version bump or
//! source change of a workspace crate gives its units a new hash, and cargo
//! keeps every earlier generation beside the new one, so a busy entry grows
//! without bound while what the next build needs stays the same size; on
//! 2026-11-26 one held 108 generations of one workspace crate's artefacts.
//! Evicting the whole entry instead makes the next gate rebuild everything.
//!
//! So every pass while the artifact max age is on (otherwise only while a
//! repository's gate caches are over its budget, or a root is under
//! pressure), each entry, largest first, is pruned with
//! [`crate::cargo_target_prune`] limited to local packages: superseded
//! generations of workspace (path) packages not used for the max age (a day
//! when it is off; [`crate::cargo_target_prune::worker_prunable`]), never a
//! registry or git dependency's. The prune
//! runs only while this worker holds the entry's exclusive gate cache lease
//! (a gate holding it keeps the entry untouched), only renames units into a
//! staging directory beside the cache (`.aethyme-storage-evicting-*`), and
//! releases the lease before the staged units are deleted. All of it stops
//! at one deadline per pass; whatever the deadline leaves staged is swept at
//! the end of the pass. Cargo rebuilds a unit a prune removed, so a wrong
//! prune costs build time, never a wrong result.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::check::WorkerLock;
use super::{Budgets, Class, Ledger, RootKind, evict};

/// The longest one pass spends pruning gate caches, deletion included.
pub const GATE_PRUNE_BUDGET: Duration = Duration::from_secs(60);

/// What one pass's gate cache prune did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatePruneReport {
    /// Entries pruned (lease taken, units judged).
    pub entries: u64,
    /// Superseded workspace units staged, their logical bytes, and the
    /// candidates found.
    pub units: u64,
    pub bytes: u64,
    pub candidates: u64,
    /// Superseded units kept because their package is not local.
    pub not_local: u64,
    /// Superseded local units kept because they were used within the max
    /// age (a day when it is off).
    #[serde(default)]
    pub in_use: u64,
    /// Entries judged because the max age prunes every pass, not because
    /// their repository was over budget.
    #[serde(default)]
    pub for_pile_up: u64,
    /// Entries left alone because a gate held their lease.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub leased: Vec<String>,
    /// Why the step stopped: `done` (every over-budget entry was judged),
    /// `deadline`, `lock_lost` (the worker lock was taken over) or
    /// `no_registry` (the lease registry cannot be read: nothing pruned).
    pub stop: String,
}

/// The gate cache entries to prune, in order: every entry while the artifact
/// max age is on (no pile-up of superseded generations, whatever the
/// budget), else those of each repository over its budget (every
/// repository's under pressure on their root); largest first, as `(item
/// index, child name)`.
pub fn entries_to_prune(
    ledger: &Ledger,
    budgets: &Budgets,
    aggressive: &BTreeSet<RootKind>,
) -> Vec<(usize, String)> {
    let totals = evict::gate_cache_by_repository(ledger);
    let now = super::now_ms();
    let mut entries: Vec<(u64, usize, String)> = Vec::new();
    for (index, item) in ledger.items.iter().enumerate() {
        if item.class != Class::GateCache || !item.measured {
            continue;
        }
        let pressing = aggressive.contains(&item.root_kind);
        for child in &item.children {
            let Some((repository, _)) = child.name.split_once('/') else {
                continue;
            };
            // An entry unused for the max age goes whole this pass; pruning
            // it would only make it look used.
            if evict::gate_entry_expired(child, budgets, now) {
                continue;
            }
            let over =
                totals.get(repository).copied().unwrap_or(0) > budgets.gate_budget(repository);
            if over || pressing || budgets.artifact_max_age().is_some() {
                entries.push((child.bytes, index, child.name.clone()));
            }
        }
    }
    entries.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.2.cmp(&right.2)));
    entries
        .into_iter()
        .map(|(_, index, name)| (index, name))
        .collect()
}

/// Inputs of [`prune_pass`] beside the ledger.
pub struct PruneInputs<'a> {
    pub budgets: &'a Budgets,
    pub aggressive: &'a BTreeSet<RootKind>,
    pub registry: &'a Path,
    /// Leases held when the pass began; `None` when the registry cannot be
    /// read.
    pub leases: Option<&'a BTreeSet<(String, String)>>,
    /// Pause after each entry, except under pressure.
    pub pace: Duration,
    pub deadline: Instant,
}

/// Prune every over-budget gate cache entry (see the module), lowering the
/// ledger's sizes by what was staged. `None` when no entry was over budget.
pub fn prune_pass(
    ledger: &mut Ledger,
    inputs: &PruneInputs<'_>,
    mut lock: Option<&mut WorkerLock>,
) -> Option<GatePruneReport> {
    let entries = entries_to_prune(ledger, inputs.budgets, inputs.aggressive);
    if entries.is_empty() {
        return None;
    }
    let _span = crate::phase_telemetry::span("storage_gate_cache_prune");
    let mut report = GatePruneReport {
        stop: "done".into(),
        ..GatePruneReport::default()
    };
    let totals = evict::gate_cache_by_repository(ledger);
    let rule = crate::cargo_target_prune::WorkerRule {
        now: std::time::SystemTime::now(),
        unused_for: inputs
            .budgets
            .artifact_max_age()
            .unwrap_or(crate::cargo_target_prune::ACCESS_TIME_WINDOW),
    };
    for (index, name) in entries {
        let Some(item) = ledger.items.get(index) else {
            continue;
        };
        let pressing = inputs.aggressive.contains(&item.root_kind);
        let Some((repository, key)) = name.split_once('/') else {
            continue;
        };
        let (repository, key) = (repository.to_string(), key.to_string());
        let over_budget = pressing
            || totals.get(&repository).copied().unwrap_or(0)
                > inputs.budgets.gate_budget(&repository);
        if Instant::now() >= inputs.deadline {
            report.stop = "deadline".into();
            break;
        }
        if lock
            .as_deref()
            .is_some_and(|lock| lock.lost() || !lock.held())
        {
            report.stop = "lock_lost".into();
            break;
        }
        let Some(leases) = inputs.leases else {
            report.stop = "no_registry".into();
            break;
        };
        let relative = format!("{}/{name}", item.name);
        if leases.contains(&(repository.clone(), key.clone())) {
            crate::phase_telemetry::add("storage_gate_cache_prune_leased", Duration::ZERO);
            report.leased.push(relative);
            continue;
        }
        if !evict::link_free(&item.root, &relative) {
            continue;
        }
        let root = item.root.clone();
        let target = root.join(&relative);
        let staging = root.join(format!(
            ".aethyme-storage-evicting-{}-{}-pruned",
            std::process::id(),
            super::now_ms()
        ));
        let pruned = {
            let _span = crate::phase_telemetry::span("storage_gate_cache_prune_entry");
            evict::with_gate_cache_lease(inputs.registry, &repository, &key, || {
                let outcome = crate::cargo_target_prune::prune_superseded_units_in(
                    &target,
                    &staging,
                    u64::MAX,
                    inputs.deadline,
                    crate::cargo_target_prune::UnitScope::Worker(rule),
                );
                if outcome.units > 0 {
                    // The next gate counts the units cargo rebuilt with a
                    // pruned hash (a misjudged prune), as after its own.
                    record_pruned_units(&target, &outcome.staged);
                }
                outcome
            })
        };
        let outcome = match pruned {
            Ok(outcome) => outcome,
            Err(_) => {
                // A gate took the lease after the pass began.
                crate::phase_telemetry::add("storage_gate_cache_prune_leased", Duration::ZERO);
                report.leased.push(relative);
                continue;
            }
        };
        report.entries += 1;
        if !over_budget {
            report.for_pile_up += 1;
        }
        report.in_use += outcome.in_use;
        for _ in 0..outcome.in_use {
            crate::phase_telemetry::add("storage_gate_cache_prune_in_use", Duration::ZERO);
        }
        report.units += outcome.units;
        report.bytes = report.bytes.saturating_add(outcome.bytes);
        report.candidates += outcome.candidates;
        report.not_local += outcome.not_local;
        for _ in 0..outcome.units {
            crate::phase_telemetry::add("storage_gate_cache_pruned_unit", Duration::ZERO);
        }
        if let Some(item) = ledger.items.get_mut(index) {
            item.bytes = item.bytes.saturating_sub(outcome.bytes);
            if let Some(child) = item.children.iter_mut().find(|child| child.name == name) {
                child.bytes = child.bytes.saturating_sub(outcome.bytes);
            }
        }
        if staging.exists() {
            // The lease is released: the staged units are out of the tree.
            let _span = crate::phase_telemetry::span("storage_gate_cache_prune_delete");
            let mut beat = || {
                if let Some(lock) = lock.as_deref_mut() {
                    lock.beat();
                }
            };
            crate::warn_unrecorded(
                "delete pruned gate cache units",
                evict::delete_tree(&staging, &mut beat, Some(inputs.deadline)).map(drop),
            );
        }
        if outcome.deadline_stopped {
            report.stop = "deadline".into();
            break;
        }
        if !pressing && !inputs.pace.is_zero() {
            std::thread::sleep(inputs.pace);
        }
    }
    crate::phase_telemetry::add(
        match report.stop.as_str() {
            "deadline" => "storage_gate_cache_prune_stop_deadline",
            "lock_lost" => "storage_gate_cache_prune_stop_lock_lost",
            "no_registry" => "storage_gate_cache_prune_stop_no_registry",
            _ => "storage_gate_cache_prune_stop_done",
        },
        Duration::ZERO,
    );
    crate::phase_telemetry::attach(
        "storage_gate_cache_prune",
        serde_json::json!({
            "entries": report.entries,
            "units": report.units,
            "bytes": report.bytes,
            "candidates": report.candidates,
            "not_local": report.not_local,
            "in_use": report.in_use,
            "for_pile_up": report.for_pile_up,
            "leased": report.leased.len(),
            "stop": report.stop,
        }),
    );
    Some(report)
}

/// Add `staged` to the entry's pruned-units record (one unit per line),
/// which the next gate reads after its build to count rebuilt units and
/// then removes.
fn record_pruned_units(target: &Path, staged: &[String]) {
    let marker = target.join(crate::gates::PRUNED_UNITS_MARKER);
    let mut lines: Vec<String> = std::fs::read_to_string(&marker)
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    lines.extend(staged.iter().cloned());
    crate::warn_unrecorded(
        "record the pruned gate cache units",
        std::fs::write(&marker, lines.join("\n")),
    );
}
