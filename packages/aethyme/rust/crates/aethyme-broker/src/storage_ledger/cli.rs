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
  storage keep-archive <id> [--unkeep]
                            exempt an archive from expiry by age (a marker
                            beside it), or lift that

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
        "archive_expiry": {
            "telemetry": ledger.archive_telemetry,
            "expiring_within_7_days": summary.archives_expiring_total,
            "paused_total": ledger.archive_telemetry.paused,
            "notices_reset_total": ledger.archive_telemetry.notices_reset,
            "last_pass_paused": ledger.last_pass.as_ref().and_then(|pass| pass.age_expiry_paused.clone()),
            "last_pass_notices_reset": ledger.last_pass.as_ref().map_or(0, |pass| pass.notices_reset),
        },
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

/// The directory of archive `id` (`<pool>/<archive>`, optionally prefixed
/// with `recovery-archives/`), checked to be a plain path inside the host
/// state's archive root.
pub fn archive_path(host_state: &Path, id: &str) -> Result<std::path::PathBuf, String> {
    let id = id.strip_prefix("recovery-archives/").unwrap_or(id);
    let parts: Vec<&str> = id.split('/').collect();
    let plain = |part: &str| {
        !part.is_empty() && part != "." && part != ".." && !part.ends_with(super::KEEP_SUFFIX)
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
    let mut rows = Vec::new();
    for item in ledger
        .items
        .iter()
        .filter(|item| item.class == Class::RecoveryArchives)
    {
        for child in &item.children {
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
    json!({
        "schema": "aethyme.storage-archives/v1",
        "unlanded_archive_days": budgets.unlanded_archive_days,
        "archives": rows,
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
    println!("{} recovery archive(s)", rows.len());
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
    crate::exit_status::SUCCESS
}
