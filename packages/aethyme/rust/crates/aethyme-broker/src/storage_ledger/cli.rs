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
        Some("worker") => match check::run_worker_pass() {
            Ok(_) => crate::exit_status::SUCCESS,
            Err(_) => crate::exit_status::FAILED,
        },
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
        classes.insert(
            *class,
            json!({
                "bytes": class_bytes.get(class),
                "entries": items.iter().map(|item| item.entries).sum::<u64>(),
                "oldest_ms": oldest,
                "budget_bytes": budgets.class_bytes(*class),
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
    json!({
        "schema": "aethyme.storage-status/v1",
        "updated_at_ms": ledger.updated_at_ms,
        "classes": classes,
        "roots": roots,
        "over_budget": summary.over_budget,
        "unclassified": unclassified,
        "unlanded_archives": summary.unlanded_archives,
        "budgets": budgets,
        "budget_error": budget_error,
        "last_pass": ledger.last_pass,
        "phases_ms": {"read": u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)},
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
    if let Some(classes) = value["classes"].as_object() {
        for (class, entry) in classes {
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
    if let Some(archives) = value["unlanded_archives"].as_array()
        && !archives.is_empty()
    {
        println!(
            "kept over budget, work not landed: {}",
            archives
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    crate::exit_status::SUCCESS
}

/// `broker status` advice: over budget, below floor, unclassified. Class
/// names and sizes only; archive ids are paths inside Aethyme's own root.
pub fn status_advice(host_state: &Path) -> Vec<crate::StatusAdvice> {
    let summary = Summary::load(host_state);
    let mut advice = Vec::new();
    if !summary.over_budget.is_empty() {
        let mut evidence: Vec<String> = summary
            .over_budget
            .iter()
            .map(|(name, bytes, budget)| {
                format!("{name}: {} over a {} budget", human(*bytes), human(*budget))
            })
            .collect();
        evidence.extend(
            summary
                .unlanded_archives
                .iter()
                .map(|id| format!("recovery archive {id} holds unlanded work and is kept")),
        );
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
