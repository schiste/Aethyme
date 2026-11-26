//! `aethyme storage`: read the ledger; run or start the worker.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Value, json};

use super::check::{self, Summary, human};
use super::{Budgets, Class, Ledger, policy, root_kind_name};

const USAGE: &str = "aethyme storage — what Aethyme stores on this host, and its budgets

  storage status [--json]   the ledger: per class bytes, entries, oldest,
                            budget, what is reclaimable now and why not;
                            reads the ledger only (milliseconds)
  storage run [--json]      one worker pass in the foreground: measure a few
                            entries, evict what is over budget or expired
  storage worker            the same pass, detached (started by broker
                            commands when a budget or floor is breached)
  storage adopt <dir>       make <dir> a storage root: writes the marker
                            file the worker requires before it inventories
                            a directory other than the host state and cache
  storage archives [--json] recovery archives: landed or not, created,
                            expires, kept
  storage keep-archive <id> [--unkeep | --dismiss]
                            exempt an archive from expiry by age (a marker
                            beside it), or lift that; --dismiss lets an
                            archive holding unlanded work expire at its age
                            (by default such an archive is kept and named)

Budgets: defaults, overridden by <host state>/storage.toml [budget].
";

pub fn run(args: &[String]) -> u8 {
    let json_output = args.iter().any(|arg| arg == "--json");
    if args.iter().any(|arg| arg == "-h" || arg == "--help") {
        print!("{USAGE}");
        return crate::exit_status::SUCCESS;
    }
    match args.first().map(String::as_str) {
        None => {
            eprint!("{USAGE}");
            crate::exit_status::USAGE
        }
        Some("help") => {
            print!("{USAGE}");
            crate::exit_status::SUCCESS
        }
        Some("status") => status(json_output),
        Some("run") => match check::run_worker_pass() {
            Ok(ledger) => {
                let report = ledger.last_pass.clone().unwrap_or_default();
                if json_output {
                    println!(
                        "{}",
                        json!({"schema": "aethyme.storage-pass/v1", "pass": report})
                    );
                } else {
                    println!(
                        "storage pass: measured {}, evicted {} ({}), skipped {}",
                        report.measured.len(),
                        report.evicted.len(),
                        human(report.bytes_reclaimed),
                        report.skipped.len()
                    );
                    for line in &report.skipped {
                        println!("  kept {line}");
                    }
                }
                crate::exit_status::SUCCESS
            }
            Err(message) => {
                eprintln!("Error: {message}");
                crate::exit_status::FAILED
            }
        },
        Some("archives") => archives(json_output),
        Some("keep-archive") => {
            let Some(id) = args.get(1).filter(|arg| !arg.starts_with('-')) else {
                eprintln!("Error: storage keep-archive needs an archive id\n\n{USAGE}");
                return crate::exit_status::USAGE;
            };
            let Some(host_state) = crate::host_state::default_host_state_dir() else {
                eprintln!("Error: the host state directory cannot be resolved");
                return crate::exit_status::ENVIRONMENT;
            };
            if args.iter().any(|arg| arg == "--dismiss") {
                return match dismiss_archive(&host_state, id) {
                    Ok(path) => {
                        println!("dismissed (expires at its age): {}", path.display());
                        crate::exit_status::SUCCESS
                    }
                    Err(message) => {
                        eprintln!("Error: {message}");
                        crate::exit_status::REFUSED
                    }
                };
            }
            let unkeep = args.iter().any(|arg| arg == "--unkeep");
            match keep_archive(&host_state, id, !unkeep) {
                Ok(path) => {
                    println!(
                        "{} {}",
                        if unkeep { "no longer kept:" } else { "kept:" },
                        path.display()
                    );
                    crate::exit_status::SUCCESS
                }
                Err(message) => {
                    eprintln!("Error: {message}");
                    crate::exit_status::REFUSED
                }
            }
        }
        Some("adopt") => {
            let Some(target) = args.get(1).filter(|arg| !arg.starts_with('-')) else {
                eprintln!("Error: storage adopt needs a directory\n\n{USAGE}");
                return crate::exit_status::USAGE;
            };
            let Some(host_state) = crate::host_state::default_host_state_dir() else {
                eprintln!("Error: the host state directory cannot be resolved");
                return crate::exit_status::ENVIRONMENT;
            };
            match check::adopt(&host_state, Path::new(target)) {
                Ok(path) => {
                    println!("adopted {} as a storage root", path.display());
                    crate::exit_status::SUCCESS
                }
                Err(message) => {
                    eprintln!("Error: {message}");
                    crate::exit_status::REFUSED
                }
            }
        }
        Some("worker") => {
            // The detached storage worker: nobody waits on it.
            let _detached_worker = crate::file_lock::DetachedBackgroundWorker::enter();
            match check::run_worker_pass() {
                Ok(_) => crate::exit_status::SUCCESS,
                Err(_) => crate::exit_status::FAILED,
            }
        }
        Some(other) => {
            eprintln!("Error: unknown storage command {other:?}\n\n{USAGE}");
            crate::exit_status::USAGE
        }
    }
}

/// The status document, from the ledger and summary only.
pub fn status_value(host_state: &Path) -> Value {
    let started = std::time::Instant::now();
    let ledger = Ledger::load(host_state);
    let summary = Summary::load(host_state);
    let (budgets, budget_error) = Budgets::load(host_state);
    let class_bytes = ledger.class_bytes();
    // The gate cache's budget is per repository: its class budget is the sum
    // of the budgets of the repositories it holds, each shown with its bytes.
    let gate_repositories = super::evict::gate_cache_by_repository(&ledger);
    let gate_budget: u64 = gate_repositories
        .keys()
        .map(|repository| budgets.gate_budget(repository))
        .fold(0, u64::saturating_add);
    let mut classes: BTreeMap<Class, Value> = BTreeMap::new();
    for class in class_bytes.keys() {
        let items: Vec<&super::Item> = ledger
            .items
            .iter()
            .filter(|item| item.class == *class)
            .collect();
        let oldest = items
            .iter()
            .map(|item| item.oldest_ms)
            .filter(|ms| *ms > 0)
            .min();
        let reclaimable: u64 = summary
            .reclaimable
            .iter()
            .filter(|entry| entry.class == *class)
            .map(|entry| entry.bytes)
            .sum();
        let why_not = match policy(*class) {
            super::Policy::ReportOnly(why) => Some(why),
            _ => None,
        };
        let telemetry = ledger.telemetry.get(class).cloned().unwrap_or_default();
        let budget_bytes = match class {
            Class::GateCache if !gate_repositories.is_empty() => Some(gate_budget),
            _ => budgets.class_bytes(*class),
        };
        classes.insert(
            *class,
            json!({
                "bytes": class_bytes.get(class),
                "entries": items.iter().map(|item| item.entries).sum::<u64>(),
                "oldest_ms": oldest,
                "oldest_age_ms": oldest.map(|ms| super::now_ms().saturating_sub(ms)),
                "budget_bytes": budget_bytes,
                "reclaimable_bytes": reclaimable,
                "why_not": why_not,
                "unmeasured": items.iter().filter(|item| item.measured_at_ms == 0).count(),
                "evictions": telemetry.evictions,
                "bytes_reclaimed": telemetry.bytes_reclaimed,
                "over_budget_since_ms": telemetry.over_budget_since_ms,
                "over_budget_ms_total": telemetry.over_budget_ms_total,
            }),
        );
    }
    if let Some(gate) = classes.get_mut(&Class::GateCache)
        && let Some(object) = gate.as_object_mut()
    {
        object.insert(
            "repositories".into(),
            gate_repositories
                .iter()
                .map(|(repository, bytes)| {
                    (
                        repository.clone(),
                        json!({"bytes": bytes, "budget_bytes": budgets.gate_budget(repository)}),
                    )
                })
                .collect::<serde_json::Map<String, Value>>()
                .into(),
        );
        object.insert(
            "last_prune".into(),
            json!(
                ledger
                    .last_pass
                    .as_ref()
                    .and_then(|pass| pass.gate_cache_prune.clone())
            ),
        );
    }
    let unclassified: Vec<Value> = ledger
        .items
        .iter()
        .filter(|item| item.class == Class::Unclassified)
        .map(|item| {
            json!({"root": root_kind_name(item.root_kind), "name": item.name, "bytes": item.bytes})
        })
        .collect();
    let roots: Vec<Value> = ledger
        .roots
        .iter()
        .map(|root| {
            let volume = super::device_of(&root.path)
                .and_then(|device| ledger.volumes.iter().find(|volume| volume.device == device));
            json!({
                "kind": root_kind_name(root.kind),
                "path": root.path,
                "bytes": ledger.root_bytes().get(&root.kind),
                "budget_bytes": budgets.root_total(root.kind),
                "free_bytes": volume.map(|volume| volume.free_bytes),
                "floor_bytes": budgets.free_floor_bytes,
                "floor_breaches": volume.map(|volume| volume.floor_breaches),
            })
        })
        .collect();
    // Checkout folders are counted, never walked: the class carries counts
    // per repository and the bytes the deleter measured removing stripped
    // regenerable directories.
    let checkouts = super::checkout_counts(host_state);
    let scratch = super::session_scratch_usage(host_state);
    let archive_usage = super::recovery_archive_usage(host_state);
    let staging = super::unfinished_archive_staging(host_state);
    let mut classes = serde_json::to_value(&classes).unwrap_or_else(|_| json!({}));
    // Recovery archives by location: the pool's volume (new archives) and
    // host state (older ones), as the retirement lane last measured them.
    if !archive_usage.is_empty()
        && let Some(object) = classes.as_object_mut()
    {
        let sum = |pick: fn(&super::RecoveryArchiveUsage) -> &super::ArchiveLocationUsage| {
            json!({
                "archives": archive_usage.values().map(|usage| pick(usage).archives).sum::<usize>(),
                "bytes": archive_usage.values().map(|usage| pick(usage).bytes).sum::<u64>(),
            })
        };
        object.insert(
            "recovery_archives_by_location".into(),
            json!({
                "pool": sum(|usage| &usage.pool),
                "host_state": sum(|usage| &usage.host_state),
                "over_bound": archive_usage.values().filter(|usage| usage.over_bound).count(),
                "unique_kept_over_bound": archive_usage.values().map(|usage| usage.unique_kept_over_bound).sum::<usize>(),
                "evicted_total": archive_usage.values().map(|usage| usage.evicted_total).sum::<usize>(),
                "evicted_bytes_total": archive_usage.values().map(|usage| usage.evicted_bytes_total).sum::<u64>(),
                "partial": archive_usage.values().any(|usage| usage.partial),
                "repositories": archive_usage,
                "unfinished_staging": {
                    "directories": staging.len(),
                    "bytes": staging.values().flatten().sum::<u64>(),
                    "unmeasured": staging.values().filter(|bytes| bytes.is_none()).count(),
                },
                "why_not": "measured by checkout retirement; over [cleanup] recovery_archive_bound_gib only fully landed archives are removed",
            }),
        );
    }
    // Session scratch directories live in the worktree pools and go with
    // their session (checkout retirement); the class reports what the
    // retirement lane measured.
    if !scratch.is_empty()
        && let Some(object) = classes.as_object_mut()
    {
        object.insert(
            "session_scratch".into(),
            json!({
                "directories": scratch.values().map(|usage| usage.directories).sum::<usize>(),
                "bytes": scratch.values().map(|usage| usage.bytes).sum::<u64>(),
                "kept": scratch.values().map(|usage| usage.kept).sum::<usize>(),
                "refused": scratch.values().map(|usage| usage.refused).sum::<usize>(),
                "partial": scratch.values().any(|usage| usage.partial),
                "bytes_freed_total": scratch.values().map(|usage| usage.bytes_freed_total).sum::<u64>(),
                "repositories": scratch,
                "why_not": "removed with their session by checkout retirement, not by the storage worker",
            }),
        );
    }
    if !checkouts.is_empty()
        && let Some(object) = classes.as_object_mut()
    {
        object.insert(
            "checkouts".into(),
            json!({
                "folders": checkouts.values().map(|counts| counts.folders).sum::<usize>(),
                "closed_folders": checkouts.values().map(|counts| counts.closed_folders).sum::<usize>(),
                "closed_with_regenerable": checkouts.values().map(|counts| counts.closed_with_regenerable).sum::<usize>(),
                "bytes_freed_by_strip": checkouts.values().map(|counts| counts.bytes_freed_by_strip).sum::<u64>(),
                "sessions_created_today": checkouts.values().map(|counts| counts.sessions_created_today).sum::<usize>(),
                "checkouts_retired_today": checkouts.values().map(|counts| counts.checkouts_retired_today).sum::<usize>(),
                "repositories": checkouts,
                "why_not": "counts only: checkout folders are not walked for bytes",
            }),
        );
    }
    json!({
        "schema": "aethyme.storage-status/v1",
        "updated_at_ms": ledger.updated_at_ms,
        "classes": classes,
        "roots": roots,
        "over_budget": summary.over_budget,
        "unclassified": unclassified,
        "unlanded_archives": summary.unlanded_archives,
        "archive_expiry": {
            "telemetry": ledger.archive_telemetry,
            "expiring_within_7_days": summary.archives_expiring_total,
            "paused_total": ledger.archive_telemetry.paused,
            "notices_reset_total": ledger.archive_telemetry.notices_reset,
            "last_pass_paused": ledger.last_pass.as_ref().and_then(|pass| pass.age_expiry_paused.clone()),
            "last_pass_notices_reset": ledger.last_pass.as_ref().map_or(0, |pass| pass.notices_reset),
        },
        // Host temp folders: not Aethyme's roots; read by the worker, never
        // deleted (`storage.host-temp-large`).
        "host_temp": super::host_temp::Reading::load(host_state),
        "artifact_max_age": artifact_max_age_status(&ledger, &budgets, super::now_ms()),
        "budgets": budgets,
        "budget_error": budget_error,
        "last_pass": ledger.last_pass,
        "phases_ms": {"read": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)},
    })
}

/// Compliance with the artifact max age, per class it applies to: the age
/// of the least recently used artifact still there (`oldest_age_ms`) against
/// the max age (`within`: `null` when unknown or the max age is off).
/// Gate cache entries and the Node.js compile cache come from the ledger;
/// gate cache units and the classes outside the ledger from the last pass.
pub fn artifact_max_age_status(ledger: &Ledger, budgets: &Budgets, now: u64) -> Value {
    let max_age_ms = budgets.artifact_max_age().map(super::evict::duration_ms);
    let within = |age: Option<u64>| -> Value {
        match (age, max_age_ms) {
            (Some(age), Some(max)) => json!(age <= max),
            _ => Value::Null,
        }
    };
    let oldest_child = |class: Class, name: Option<&str>| -> Option<u64> {
        ledger
            .items
            .iter()
            .filter(|item| item.class == class && item.measured)
            .filter(|item| name.is_none_or(|name| item.name == name))
            .flat_map(|item| item.children.iter())
            .map(|child| child.newest_ms.max(child.ctime_ms))
            .filter(|ms| *ms > 0)
            .min()
            .map(|ms| now.saturating_sub(ms))
    };
    let mut classes = serde_json::Map::new();
    let gate = oldest_child(Class::GateCache, None);
    classes.insert(
        "gate_cache".into(),
        json!({"oldest_age_ms": gate, "within": within(gate)}),
    );
    let node = oldest_child(
        Class::ToolCaches,
        Some(crate::node_compile_cache::CACHE_DIRECTORY),
    );
    classes.insert(
        "node_compile_cache".into(),
        json!({
            "oldest_age_ms": node,
            // Today's directory is kept whatever its age; past days go an
            // hour after their last change.
            "rule": "past UTC days go an hour after their last change; today's stays",
        }),
    );
    let last = ledger.last_pass.as_ref();
    if let Some(prune) = last.and_then(|pass| pass.gate_cache_prune.as_ref()) {
        classes.insert(
            "gate_cache_units".into(),
            json!({
                "pruned_for_pile_up": prune.units,
                "pruned_bytes": prune.bytes,
                "kept_in_use": prune.in_use,
                "kept_not_local": prune.not_local,
                "stop": prune.stop,
            }),
        );
    }
    let age = last.and_then(|pass| pass.artifact_age.as_ref());
    if let Some(age) = age {
        for (class, outcome) in &age.classes {
            let mut value = json!(outcome);
            if let Some(object) = value.as_object_mut() {
                object.insert("within".into(), within(outcome.oldest_kept_age_ms));
            }
            classes.insert(class.clone(), value);
        }
    }
    json!({
        "max_age_hours": budgets.artifact_max_age_hours,
        "max_age_ms": max_age_ms,
        "classes": classes,
        "stop": age.map(|age| age.stop.clone()),
    })
}

fn status(json_output: bool) -> u8 {
    let Some(host_state) = crate::host_state::default_host_state_dir() else {
        eprintln!("Error: the host state directory cannot be resolved");
        return crate::exit_status::ENVIRONMENT;
    };
    let value = status_value(&host_state);
    if json_output {
        println!("{value}");
        return crate::exit_status::SUCCESS;
    }
    if value["updated_at_ms"].as_u64().unwrap_or(0) == 0 {
        println!("storage: not measured yet (the worker starts with the next broker command)");
    }
    if let Some(error) = value["budget_error"].as_str() {
        println!("Warning: {error}; default storage budgets in use");
    }
    match value["artifact_max_age"]["max_age_hours"].as_u64() {
        Some(0) | None => println!("artifact max age: off"),
        Some(hours) => {
            let over: Vec<&str> = value["artifact_max_age"]["classes"]
                .as_object()
                .map(|classes| {
                    classes
                        .iter()
                        .filter(|(_, entry)| entry["within"] == json!(false))
                        .map(|(class, _)| class.as_str())
                        .collect()
                })
                .unwrap_or_default();
            if over.is_empty() {
                println!("artifact max age: {hours}h, every class within it");
            } else {
                println!(
                    "artifact max age: {hours}h; older artifacts still in: {}",
                    over.join(", ")
                );
            }
        }
    }
    // Roots and one total first.
    let total: u64 = value["classes"]
        .as_object()
        .map(|classes| {
            classes
                .values()
                .filter_map(|entry| entry["bytes"].as_u64())
                .sum()
        })
        .unwrap_or(0);
    println!(
        "storage: {} measured across {} root(s)",
        human(total),
        value["roots"].as_array().map_or(0, Vec::len)
    );
    if let Some(roots) = value["roots"].as_array() {
        for root in roots {
            println!(
                "  {:<14} {}  {} of {}  free {}",
                root["kind"].as_str().unwrap_or("?"),
                root["path"].as_str().unwrap_or("?"),
                human(root["bytes"].as_u64().unwrap_or(0)),
                human(root["budget_bytes"].as_u64().unwrap_or(0)),
                root["free_bytes"]
                    .as_u64()
                    .map_or_else(|| "unknown".to_string(), human)
            );
        }
    }
    if let Some(classes) = value["classes"].as_object() {
        for (class, entry) in classes {
            if class == "session_scratch" {
                println!(
                    "{class:<24} {} in {} director(ies), {} kept, {} freed  (removed with their session)",
                    human(entry["bytes"].as_u64().unwrap_or(0)),
                    entry["directories"].as_u64().unwrap_or(0),
                    entry["kept"].as_u64().unwrap_or(0),
                    human(entry["bytes_freed_total"].as_u64().unwrap_or(0)),
                );
                continue;
            }
            if class == "checkouts" {
                println!(
                    "{class:<24} {} folder(s), {} closed, {} closed holding regenerable directories, {} freed by strip; today {} created, {} retired  (counts only)",
                    entry["folders"].as_u64().unwrap_or(0),
                    entry["closed_folders"].as_u64().unwrap_or(0),
                    entry["closed_with_regenerable"].as_u64().unwrap_or(0),
                    human(entry["bytes_freed_by_strip"].as_u64().unwrap_or(0)),
                    entry["sessions_created_today"].as_u64().unwrap_or(0),
                    entry["checkouts_retired_today"].as_u64().unwrap_or(0),
                );
                continue;
            }
            let budget = entry["budget_bytes"]
                .as_u64()
                .map_or_else(|| "-".to_string(), human);
            println!(
                "{class:<24} {:>10}  budget {:>10}  reclaimable {:>10}{}",
                human(entry["bytes"].as_u64().unwrap_or(0)),
                budget,
                human(entry["reclaimable_bytes"].as_u64().unwrap_or(0)),
                entry["why_not"]
                    .as_str()
                    .map(|why| format!("  ({why})"))
                    .unwrap_or_default()
            );
        }
    }
    let staging = &value["classes"]["recovery_archives_by_location"]["unfinished_staging"];
    if staging["directories"].as_u64().unwrap_or(0) > 0 {
        println!(
            "unfinished archive staging: {} director(ies), {} measured (see `aethyme storage archives`)",
            staging["directories"],
            human(staging["bytes"].as_u64().unwrap_or(0))
        );
    }
    if let Some(over) = value["over_budget"].as_array() {
        for entry in over {
            println!(
                "over budget: {} {} > {}",
                entry[0].as_str().unwrap_or("?"),
                human(entry[1].as_u64().unwrap_or(0)),
                human(entry[2].as_u64().unwrap_or(0))
            );
        }
    }
    let host_temp = super::host_temp::Reading::load(&host_state);
    for root in &host_temp.roots {
        let growth = host_temp
            .growth(root.kind, super::now_ms())
            .map(|(bytes, hours)| {
                let sign = if bytes < 0 { "-" } else { "+" };
                format!(", {sign}{} in {hours} h", human(bytes.unsigned_abs()))
            })
            .unwrap_or_default();
        println!(
            "host temp: {} holds {}{} in {}{} entries{growth}, free {}  (not Aethyme's; never deleted)",
            root.kind.label(),
            if root.partial { "at least " } else { "" },
            human(root.bytes),
            root.entries,
            if root.entries_capped { "+" } else { "" },
            root.free_bytes.map_or_else(|| "unknown".to_string(), human),
        );
    }
    let expiry = &value["archive_expiry"];
    println!(
        "archive age expiry: paused {} time(s), {} notice(s) restarted{}",
        expiry["paused_total"].as_u64().unwrap_or(0),
        expiry["notices_reset_total"].as_u64().unwrap_or(0),
        expiry["last_pass_paused"]
            .as_str()
            .map_or_else(String::new, |why| format!(
                "; last pass paused ({why}), {} notice(s) restarted",
                expiry["last_pass_notices_reset"].as_u64().unwrap_or(0)
            ))
    );
    if let Some(archives) = value["unlanded_archives"].as_array()
        && !archives.is_empty()
    {
        let first: Vec<&str> = archives.iter().filter_map(Value::as_str).take(5).collect();
        println!(
            "kept over budget, work not landed: {} archive(s), e.g. {}{}",
            archives.len(),
            first.join(", "),
            if archives.len() > 5 { ", ..." } else { "" }
        );
    }
    crate::exit_status::SUCCESS
}

/// One kept-at-age archive: its session, how to restore it and how to
/// dismiss it.
fn unlanded_at_age_line(host_state: &Path, id: &str) -> String {
    let manifest: Option<Value> = archive_path(host_state, id)
        .ok()
        .and_then(|path| std::fs::read(path.join("manifest.json")).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let session = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("session_id"))
        .and_then(Value::as_i64)
        .map_or_else(
            || "session unknown".to_string(),
            |id| format!("session {id}"),
        );
    let restore = manifest
        .as_ref()
        .and_then(|manifest| manifest.get("worktree_path"))
        .and_then(Value::as_str)
        .map_or_else(
            || "restore: see the archive's README.md".to_string(),
            |path| {
                format!(
                    "restore: {}",
                    crate::broker::restore_command(Path::new(path))
                )
            },
        );
    format!(
        "recovery archive {id} ({session}) holds unlanded work; {restore}; dismiss: aethyme storage keep-archive {id} --dismiss"
    )
}

/// Advice for repositories whose recovery archives stay over their bound
/// because the archives hold work that exists nowhere else (never removed
/// for size).
pub(crate) fn recovery_archive_bound_advice(
    usage: &BTreeMap<String, super::RecoveryArchiveUsage>,
) -> Option<crate::StatusAdvice> {
    let over: Vec<&super::RecoveryArchiveUsage> = usage
        .values()
        .filter(|usage| usage.over_bound && usage.unique_kept_over_bound > 0)
        .collect();
    if over.is_empty() {
        return None;
    }
    let unique: usize = over.iter().map(|usage| usage.unique_kept_over_bound).sum();
    let sessions: Vec<i64> = over
        .iter()
        .flat_map(|usage| usage.unique_kept_sessions.iter().copied())
        .collect();
    Some(crate::StatusAdvice {
        id: "storage.recovery-archives-over-bound",
        severity: crate::StatusAdviceSeverity::Warning,
        reason: "recovery archives holding work that exists nowhere else are never removed for size; restore and land that work, or raise [cleanup] recovery_archive_bound_gib",
        summary: format!(
            "{} repositor{} over the recovery archive bound with {unique} archive(s) of unique work kept",
            over.len(),
            if over.len() == 1 { "y" } else { "ies" }
        ),
        session_id: None,
        queue_entry_id: None,
        evidence: over
            .iter()
            .map(|usage| {
                format!(
                    "pool {} archive(s) {}, host state {} archive(s) {}, bound {}",
                    usage.pool.archives,
                    human(usage.pool.bytes),
                    usage.host_state.archives,
                    human(usage.host_state.bytes),
                    human(usage.bound_bytes)
                )
            })
            .chain((!sessions.is_empty()).then(|| {
                format!(
                    "sessions holding unique work: {}",
                    sessions
                        .iter()
                        .map(i64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            }))
            .collect(),
        commands: vec![
            "aethyme storage archives".into(),
            "aethyme storage status --json".into(),
        ],
    })
}

/// How many unlanded recovery archives `storage.over-budget` names one by one.
const UNLANDED_ARCHIVE_EXAMPLES: usize = 5;

/// The `storage.over-budget` lines for archives kept because their work is
/// not landed: a few by name and a count. Every such archive used to be its
/// own line, and on a host that kept thousands this one advice entry was most
/// of the full status document. `aethyme storage status --json` lists them all.
pub(super) fn unlanded_archive_evidence(archives: &[String]) -> Vec<String> {
    let mut evidence: Vec<String> = archives
        .iter()
        .take(UNLANDED_ARCHIVE_EXAMPLES)
        .map(|id| format!("recovery archive {id} holds unlanded work and is kept"))
        .collect();
    if archives.len() > UNLANDED_ARCHIVE_EXAMPLES {
        evidence.push(format!(
            "and {} more recovery archive(s) holding unlanded work ({} in all); \
             `aethyme storage status --json` lists every one",
            archives.len() - UNLANDED_ARCHIVE_EXAMPLES,
            archives.len()
        ));
    }
    evidence
}

/// `storage.budget-config-invalid`: `storage.toml` cannot be read, so the
/// worker runs on default budgets (an older binary on the host rejects keys
/// it does not know in the same way).
fn budget_config_invalid_advice(error: String) -> crate::StatusAdvice {
    crate::StatusAdvice {
        id: "storage.budget-config-invalid",
        severity: crate::StatusAdviceSeverity::Warning,
        reason: "the host's storage.toml cannot be read; the storage worker runs on default budgets until it is fixed",
        summary: "storage.toml is invalid: default storage budgets in use".into(),
        session_id: None,
        queue_entry_id: None,
        evidence: vec![error],
        commands: vec!["aethyme storage status".into()],
    }
}

/// `broker status` advice: over budget, below floor, unclassified. Class
/// names and sizes only; archive ids are paths inside Aethyme's own root.
pub fn status_advice(host_state: &Path) -> Vec<crate::StatusAdvice> {
    let summary = Summary::load(host_state);
    let mut advice = Vec::new();
    if let (_, Some(error)) = Budgets::load(host_state) {
        crate::phase_telemetry::add(
            "storage_budget_config_invalid_advised",
            std::time::Duration::ZERO,
        );
        advice.push(budget_config_invalid_advice(error));
    }
    if !summary.over_budget.is_empty() {
        let mut evidence: Vec<String> = summary
            .over_budget
            .iter()
            .map(|(name, bytes, budget)| {
                format!("{name}: {} over a {} budget", human(*bytes), human(*budget))
            })
            .collect();
        evidence.extend(unlanded_archive_evidence(&summary.unlanded_archives));
        advice.push(crate::StatusAdvice {
            id: "storage.over-budget",
            severity: crate::StatusAdviceSeverity::Notice,
            reason: "a class of Aethyme's storage is over its budget; the storage worker evicts what is safe to remove",
            summary: format!("{} storage budget(s) exceeded", summary.over_budget.len()),
            session_id: None,
            queue_entry_id: None,
            evidence,
            commands: vec!["aethyme storage status".into()],
        });
    }
    let floor = summary.floor_bytes;
    let mut short = Vec::new();
    for root in &summary.roots {
        if let Some(free) = crate::disk_headroom::available_bytes(&root.path)
            && floor > 0
            && free < floor
        {
            short.push(format!(
                "{}: {} free, floor {}",
                root_kind_name(root.kind),
                human(free),
                human(floor)
            ));
        }
    }
    if !short.is_empty() {
        advice.push(crate::StatusAdvice {
            id: "storage.below-floor",
            severity: crate::StatusAdviceSeverity::Warning,
            reason: "a volume holding Aethyme's data is below its free-space floor; regenerable data is evicted first and space-hungry commands refuse until it recovers",
            summary: format!("{} volume(s) below the free-space floor", short.len()),
            session_id: None,
            queue_entry_id: None,
            evidence: short,
            commands: vec!["aethyme storage status".into()],
        });
    }
    if summary.archives_expiring_total > 0 {
        let mut evidence: Vec<String> = summary
            .archives_expiring
            .iter()
            .map(|archive| {
                format!(
                    "{} (pool {}, session {}, {}) expires at {} ms",
                    archive.id,
                    archive.pool,
                    archive
                        .session_id
                        .map_or_else(|| "unknown".to_string(), |id| id.to_string()),
                    human(archive.bytes),
                    archive.expires_ms
                )
            })
            .collect();
        if summary.archives_expiring_total > summary.archives_expiring.len() as u64 {
            evidence.push(format!(
                "and {} more",
                summary.archives_expiring_total - summary.archives_expiring.len() as u64
            ));
        }
        advice.push(crate::StatusAdvice {
            id: "storage.archives-expiring",
            severity: crate::StatusAdviceSeverity::Notice,
            reason: "recovery archives expire by age; keep one with the command shown",
            summary: format!(
                "{} recovery archive(s) expire within 7 days",
                summary.archives_expiring_total
            ),
            session_id: None,
            queue_entry_id: None,
            evidence,
            commands: summary
                .archives_expiring
                .first()
                .map(|archive| format!("aethyme storage keep-archive {}", archive.id))
                .into_iter()
                .chain(["aethyme storage archives".to_string()])
                .collect(),
        });
    }
    if let Some(entry) = super::host_temp::advice(
        &super::host_temp::Reading::load(host_state),
        &Budgets::load(host_state).0,
        super::now_ms(),
    ) {
        advice.push(entry);
    }
    let scratch = super::session_scratch_usage(host_state);
    if let Some(entry) = session_scratch_advice(&scratch) {
        advice.push(entry);
    }
    if let Some(entry) = command_temp_backlog_advice(&scratch) {
        advice.push(entry);
    }
    advice.extend(recovery_archive_bound_advice(
        &super::recovery_archive_usage(host_state),
    ));
    if !summary.unlanded_archives_at_age.is_empty() {
        advice.push(crate::StatusAdvice {
            id: "storage.unlanded-archives-at-age",
            severity: crate::StatusAdviceSeverity::Warning,
            reason: "a recovery archive holding unlanded commits or uncommitted changes reached its retention age; it is kept, never expired by age alone, until its owner restores the work or dismisses it",
            summary: format!(
                "{} recovery archive(s) with unlanded work kept past their retention age",
                summary.unlanded_archives_at_age.len()
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: summary
                .unlanded_archives_at_age
                .iter()
                .take(UNLANDED_ARCHIVE_EXAMPLES)
                .map(|id| unlanded_at_age_line(host_state, id))
                .collect(),
            commands: vec!["aethyme storage archives".into()],
        });
    }
    let refused: usize = scratch.values().map(|usage| usage.refused).sum();
    if refused > 0 {
        advice.push(crate::StatusAdvice {
            id: "storage.session-scratch-unidentified",
            severity: crate::StatusAdviceSeverity::Warning,
            reason: "scratch directories the broker could not prove are the ones it created (also after a remount) are kept and never removed; inspect them by hand",
            summary: format!("{refused} session scratch director{} kept unidentified", if refused == 1 { "y" } else { "ies" }),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![format!("{refused} kept with a refusal keep reason")],
            commands: vec!["aethyme storage status --json".into()],
        });
    }
    if let Some(item) = staged_deletion_advice(
        crate::host_state::default_host_cache_dir()
            .map(|cache| check::staged_deletions(&cache))
            .unwrap_or_default(),
    ) {
        advice.push(item);
    }
    if summary.unclassified_entries > 0 {
        advice.push(crate::StatusAdvice {
            id: "storage.unclassified",
            severity: crate::StatusAdviceSeverity::Notice,
            reason: "entries under Aethyme's roots that match none of its names are listed and never deleted",
            summary: format!(
                "{} unclassified entr{} ({})",
                summary.unclassified_entries,
                if summary.unclassified_entries == 1 { "y" } else { "ies" },
                human(summary.unclassified_bytes)
            ),
            session_id: None,
            queue_entry_id: None,
            evidence: vec![format!("unclassified: {}", human(summary.unclassified_bytes))],
            commands: vec!["aethyme storage status --json".into()],
        });
    }
    advice
}

/// `storage.staged-deletion-pending`: entries moved aside for the storage
/// worker (a gate cache prune's staging directory, an interrupted eviction)
/// still present after [`check::STAGED_DELETION_WAIT`]. The next command
/// starts the worker when none runs; this says the space has not come back.
pub(super) fn staged_deletion_advice(
    staged: Vec<check::StagedDeletion>,
) -> Option<crate::StatusAdvice> {
    let wait_ms = u64::try_from(check::STAGED_DELETION_WAIT.as_millis()).unwrap_or(u64::MAX);
    let waiting: Vec<_> = staged
        .into_iter()
        .filter(|staged| staged.age_ms >= wait_ms)
        .collect();
    if waiting.is_empty() {
        return None;
    }
    let known: u64 = waiting.iter().filter_map(|staged| staged.bytes).sum();
    let unmeasured = waiting
        .iter()
        .filter(|staged| staged.bytes.is_none())
        .count();
    let oldest_minutes = waiting
        .iter()
        .map(|staged| staged.age_ms)
        .max()
        .unwrap_or(0)
        / 60_000;
    let mut evidence = vec![format!(
        "{} staged in the host cache root, the oldest {oldest_minutes} minute(s) ago",
        waiting.len()
    )];
    if unmeasured > 0 {
        evidence.push(format!("{unmeasured} of them without a recorded size"));
    }
    Some(crate::StatusAdvice {
        id: "storage.staged-deletion-pending",
        severity: crate::StatusAdviceSeverity::Notice,
        reason: "space moved aside for deletion has not been reclaimed yet; the storage worker deletes it in the background, never on a command's path",
        summary: format!(
            "{} awaiting deletion by the storage worker",
            if known > 0 {
                human(known)
            } else {
                "staged entries".to_string()
            }
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec!["aethyme storage status --json".into()],
    })
}

/// `storage.session-scratch-large`: one repository's session scratch
/// directories, or one of them, hold more than their advice threshold.
pub(super) fn session_scratch_advice(
    usage: &std::collections::BTreeMap<String, super::SessionScratchUsage>,
) -> Option<crate::StatusAdvice> {
    let evidence: Vec<String> = usage
        .values()
        .filter(|usage| {
            usage.bytes > super::SESSION_SCRATCH_ADVICE_BYTES
                || usage.largest_bytes > super::SESSION_SCRATCH_ADVICE_ONE_BYTES
        })
        .map(|usage| {
            format!(
                "{} in {} scratch director(ies); the largest, {}, belongs to session {}",
                human(usage.bytes),
                usage.directories,
                human(usage.largest_bytes),
                usage
                    .largest_session_id
                    .map_or_else(|| "unknown".to_string(), |id| id.to_string())
            )
        })
        .collect();
    if evidence.is_empty() {
        return None;
    }
    Some(crate::StatusAdvice {
        id: "storage.session-scratch-large",
        severity: crate::StatusAdviceSeverity::Notice,
        reason: "session scratch directories are large; each is removed when its session is finished, closed or cleaned, so finish idle sessions or clean up inside the scratch directory",
        summary: format!(
            "session scratch is large in {} repositor{}",
            evidence.len(),
            if evidence.len() == 1 { "y" } else { "ies" }
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec!["aethyme storage status --json".into()],
    })
}

/// `storage.command-temp-backlog`: one repository's finished command runs
/// (temp directories of commands the broker ran) await removal past their
/// advice threshold.
pub(super) fn command_temp_backlog_advice(
    usage: &std::collections::BTreeMap<String, super::SessionScratchUsage>,
) -> Option<crate::StatusAdvice> {
    let evidence: Vec<String> = usage
        .values()
        .filter(|usage| {
            usage.finished_runs >= super::COMMAND_TEMP_BACKLOG_ADVICE_RUNS
                || usage.finished_run_bytes >= super::COMMAND_TEMP_BACKLOG_ADVICE_BYTES
        })
        .map(|usage| {
            format!(
                "{} finished command run(s) holding {} await removal",
                usage.finished_runs,
                human(usage.finished_run_bytes)
            )
        })
        .collect();
    if evidence.is_empty() {
        return None;
    }
    Some(crate::StatusAdvice {
        id: "storage.command-temp-backlog",
        severity: crate::StatusAdviceSeverity::Notice,
        reason: "temp directories of finished commands wait for the checkout retirement run, which moves them to the trash for the paced deleter; it runs in the background, never on a command's path",
        summary: format!(
            "finished command temp directories are piling up in {} repositor{}",
            evidence.len(),
            if evidence.len() == 1 { "y" } else { "ies" }
        ),
        session_id: None,
        queue_entry_id: None,
        evidence,
        commands: vec!["aethyme storage status --json".into()],
    })
}

/// The directory of archive `id` (`<pool>/<archive>`, optionally prefixed
/// with `recovery-archives/`), checked to be a plain path inside the host
/// state's archive root.
pub fn archive_path(host_state: &Path, id: &str) -> Result<std::path::PathBuf, String> {
    let id = id.strip_prefix("recovery-archives/").unwrap_or(id);
    let parts: Vec<&str> = id.split('/').collect();
    let plain = |part: &str| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.ends_with(super::KEEP_SUFFIX)
            && !part.ends_with(super::DISMISS_SUFFIX)
    };
    if parts.len() != 2 || !parts.iter().all(|part| plain(part)) {
        return Err(format!("{id:?} is not an archive id (<pool>/<archive>)"));
    }
    let root = host_state.join("recovery-archives");
    if !super::evict::link_free(&root, id) {
        return Err(format!("no archive {id} (or a link on its path)"));
    }
    let path = root.join(id);
    if !path.join("manifest.json").is_file() {
        return Err(format!("{id} has no manifest; not a recovery archive"));
    }
    Ok(path)
}

/// Write an archive's dismissal marker: its owner lets it expire at its
/// retention age although it holds unlanded work.
pub fn dismiss_archive(host_state: &Path, id: &str) -> Result<std::path::PathBuf, String> {
    let path = archive_path(host_state, id)?;
    let marker = super::dismiss_marker(&path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW).mode(0o644);
    }
    match options.open(&marker) {
        Ok(mut file) => std::io::Write::write_all(
            &mut file,
            b"Dismissed by `aethyme storage keep-archive --dismiss`: may expire by age.\n",
        )
        .map_err(|error| error.to_string())?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    crate::phase_telemetry::add("storage_archive_dismissed", std::time::Duration::ZERO);
    Ok(path)
}

/// Write (or with `keep = false`, remove) an archive's keep marker.
pub fn keep_archive(host_state: &Path, id: &str, keep: bool) -> Result<std::path::PathBuf, String> {
    let path = archive_path(host_state, id)?;
    let marker = super::keep_marker(&path);
    let existing = std::fs::symlink_metadata(&marker).ok();
    if existing
        .as_ref()
        .is_some_and(|metadata| !metadata.file_type().is_file())
    {
        return Err(format!(
            "{} exists and is not a plain file (a link?); remove it first",
            marker.display()
        ));
    }
    if keep {
        if existing.is_some() {
            // Already kept.
            return Ok(path);
        }
        // Created new, never through a link.
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW).mode(0o644);
        }
        let mut file = match options.open(&marker) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Ok(path);
            }
            Err(error) => return Err(error.to_string()),
        };
        std::io::Write::write_all(
            &mut file,
            b"Kept by `aethyme storage keep-archive`: never expired by age.\n",
        )
        .map_err(|error| error.to_string())?;
    } else if existing.is_some() {
        std::fs::remove_file(&marker).map_err(|error| error.to_string())?;
    }
    Ok(path)
}

/// One line per recovery archive the ledger knows, with a live landed check.
pub fn archives_value(host_state: &Path) -> Value {
    let ledger = Ledger::load(host_state);
    let (budgets, _) = Budgets::load(host_state);
    // Live answers: every default tip is read afresh for this listing.
    super::landing::shared().expire_tips();
    let mut rows = Vec::new();
    for item in ledger
        .items
        .iter()
        .filter(|item| item.class == Class::RecoveryArchives)
    {
        for child in item
            .children
            .iter()
            .filter(|child| !super::is_partial_archive_name(&child.name))
        {
            let path = item.path().join(&child.name);
            let landed = super::evict::archive_landed(&path);
            let kept = super::keep_marker(&path).exists();
            let notice = ledger.archive_notices.get(&child.name);
            let mut probe = child.clone();
            probe.kept = kept;
            rows.push(json!({
                "id": child.name,
                "session_id": child.session_id,
                "bytes": child.bytes,
                "landed": landed.is_ok(),
                "not_landed_because": landed.err(),
                "created_ms": child.created_ms,
                "expires_ms": super::archive_expiry(&probe, &budgets),
                "noticed_at_ms": notice.map(|notice| notice.noticed_at_ms),
                "kept": kept,
            }));
        }
    }
    let staging: Vec<Value> = super::unfinished_archive_staging(host_state)
        .into_iter()
        .map(|(path, bytes)| json!({"path": path.to_string_lossy(), "bytes": bytes}))
        .collect();
    json!({
        "schema": "aethyme.storage-archives/v1",
        "unlanded_archive_days": budgets.unlanded_archive_days,
        "archives": rows,
        "unfinished_staging": staging,
    })
}

fn archives(json_output: bool) -> u8 {
    let Some(host_state) = crate::host_state::default_host_state_dir() else {
        eprintln!("Error: the host state directory cannot be resolved");
        return crate::exit_status::ENVIRONMENT;
    };
    let value = archives_value(&host_state);
    if json_output {
        println!("{value}");
        return crate::exit_status::SUCCESS;
    }
    let rows = value["archives"].as_array().cloned().unwrap_or_default();
    let staging = value["unfinished_staging"].as_array();
    if rows.is_empty() && staging.is_some_and(|staging| !staging.is_empty()) {
        println!("no finished recovery archive");
    } else {
        println!("{} recovery archive(s)", rows.len());
    }
    for row in rows {
        println!(
            "  {}  {}  {}  created {}  expires {}{}",
            row["id"].as_str().unwrap_or("?"),
            human(row["bytes"].as_u64().unwrap_or(0)),
            if row["landed"] == true {
                "landed"
            } else {
                "unlanded"
            },
            row["created_ms"]
                .as_u64()
                .map_or_else(|| "unknown".to_string(), |ms| ms.to_string()),
            row["expires_ms"]
                .as_u64()
                .map_or_else(|| "never".to_string(), |ms| ms.to_string()),
            if row["kept"] == true { "  kept" } else { "" }
        );
    }
    for entry in staging.into_iter().flatten() {
        println!(
            "  unfinished staging  {}  {}  (copies of an attempt that did not finish; never expired)",
            entry["path"].as_str().unwrap_or("?"),
            entry["bytes"]
                .as_u64()
                .map_or_else(|| "size unknown".to_string(), human)
        );
    }
    crate::exit_status::SUCCESS
}
