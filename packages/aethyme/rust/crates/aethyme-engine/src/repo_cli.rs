//! Native `aethyme repo` basics (python-retirement Phase 1 closeout).
//!
//! Only the engine-facing basics live here: `ingest`, `inspect`,
//! `clear-cache`, `warm`, and `engine-info`. The rest of the repo group
//! (skills deploy/compile, onboarding/agents overrides, commit hygiene,
//! telemetry, record-wrapper-invocation) is native too since the
//! Phase 3 flip — [`run`] returns [`Outcome::Delegate`] for those and
//! the router falls through to `aethyme_enhance::repo_cli`.
//!
//! Sanctioned reinterpretations (recorded in cli-surface-v1.md): the
//! transport/cache era ended, so `engine-info` now reports binary/
//! store/daemon status instead of transports; `ingest` reports engine
//! snapshot + store state instead of the Python cache key; `clear-cache`
//! clears the whole legacy output-cache root. `inspect` and `warm` are
//! byte-parity ports.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::graph_cli::{object_key_order, pretty_json, render_number};
use crate::map::RepositoryMap;

/// Result of dispatching a repo subcommand.
pub enum Outcome {
    Handled(Result<(), String>),
    /// Not a flipped subcommand — the router should delegate to Python.
    Delegate,
}

/// Run `repo <subcommand> ...`. `args` excludes the leading `repo`.
pub fn run(args: &[String]) -> Outcome {
    let Some(subcommand) = args.first() else {
        return Outcome::Delegate;
    };
    match subcommand.as_str() {
        "ingest" | "inspect" | "clear-cache" | "warm" | "engine-info" => {
            Outcome::Handled(dispatch(subcommand.clone().as_str(), &args[1..]))
        }
        _ => Outcome::Delegate,
    }
}

fn dispatch(subcommand: &str, rest: &[String]) -> Result<(), String> {
    if subcommand == "engine-info" {
        return engine_info(rest);
    }
    let repo = {
        let raw = rest
            .iter()
            .find(|a| !a.starts_with("--"))
            .ok_or_else(|| format!("usage: aethyme repo {subcommand} <repo_path>"))?;
        let path = PathBuf::from(raw);
        if !path.is_dir() {
            return Err(format!("repository path is not a directory: {raw}"));
        }
        path.canonicalize().map_err(|e| e.to_string())?
    };
    match subcommand {
        "ingest" => ingest(&repo),
        "inspect" => inspect(&repo, rest),
        "clear-cache" => clear_cache(&repo),
        "warm" => warm(&repo),
        _ => unreachable!("gated by run()"),
    }
}

fn build_map(repo: &Path) -> Result<RepositoryMap, String> {
    Ok(
        RepositoryMap::build_with_fragment_preference(repo, false, |_| {})
            .map_err(|e| e.to_string())?
            .0,
    )
}

fn store_path(repo: &Path) -> PathBuf {
    repo.join(".aethyme").join("graph_store.redb")
}

fn ingest(repo: &Path) -> Result<(), String> {
    let map = build_map(repo)?;
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    println!("Repository: {name}");
    println!("Path: {}", repo.display());
    println!("Files: {}", map.files.len());
    let store = store_path(repo);
    println!(
        "Graph store: {}",
        if store.is_file() {
            "present"
        } else {
            "missing (run aethyme-engine-cli index)"
        }
    );
    Ok(())
}

fn inspect(repo: &Path, rest: &[String]) -> Result<(), String> {
    let json_output = rest.iter().any(|a| a == "--json-output");
    let mode = rest
        .windows(2)
        .find(|pair| pair[0] == "--mode")
        .map(|pair| pair[1].as_str())
        .unwrap_or("full");
    let map = build_map(repo)?;

    let mut raw = Vec::new();
    match mode {
        "brief" => raw = crate::json::inspect_brief(&map).into_bytes(),
        "structure" => {
            crate::json::write_inspect_structure(&mut raw, &map).map_err(|e| e.to_string())?
        }
        "full" => crate::json::write_repository_map(&mut raw, &map).map_err(|e| e.to_string())?,
        other => return Err(format!("unsupported inspect mode: {other}")),
    }
    let raw = String::from_utf8(raw).map_err(|e| e.to_string())?;

    if json_output {
        println!("{}", pretty_json(&raw));
        return Ok(());
    }
    let payload: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let snapshot = &payload["snapshot"];
    println!("Root: {}", snapshot["root"].as_str().unwrap_or_default());
    let join = |v: &Value| {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    };
    println!("Languages: {}", join(&snapshot["languages"]));
    println!(
        "Top-level directories: {}",
        join(&snapshot["top_level_dirs"])
    );
    let file_count = snapshot
        .get("file_count")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| {
            snapshot
                .get("files")
                .and_then(Value::as_array)
                .map(|a| a.len() as i64)
                .unwrap_or(0)
        });
    println!("Files: {file_count}");
    if mode == "full" {
        let len = |key: &str| {
            payload
                .get(key)
                .and_then(Value::as_array)
                .map(|a| a.len())
                .unwrap_or(0)
        };
        println!("Symbols: {}", len("symbols"));
        println!("Edges: {}", len("edges"));
    }
    for (key, label) in [
        ("entrypoints", "Entrypoints"),
        ("key_configs", "Key configs"),
    ] {
        if let Some(items) = payload.get(key).and_then(Value::as_array) {
            if !items.is_empty() {
                println!("{label}: {}", join(&payload[key]));
            }
        }
    }
    if let Some(signals) = payload.get("signals").and_then(Value::as_object) {
        if !signals.is_empty() {
            println!("Signals:");
            for name in object_key_order(&raw, "signals") {
                let Some(signal) = signals.get(&name) else {
                    continue;
                };
                println!(
                    "- {}: {} ({})",
                    name.replace('_', " "),
                    render_number(&signal["score"]),
                    signal["level"].as_str().unwrap_or_default(),
                );
            }
        }
    }
    Ok(())
}

fn clear_cache(repo: &Path) -> Result<(), String> {
    // Legacy Python output cache (snapshot-keyed). The cache retires
    // with the transports; clearing the root is the coarse, correct
    // interpretation until Phase 6 deletes the concept entirely.
    let cache_root = std::env::var("AETHYME_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/aethyme-cache"));
    if cache_root.is_dir() {
        std::fs::remove_dir_all(&cache_root).map_err(|e| e.to_string())?;
    }
    println!("Cleared cache for {}", repo.display());
    Ok(())
}

fn warm(repo: &Path) -> Result<(), String> {
    build_map(repo)?;
    println!("Map cache warmed for {}", repo.display());
    Ok(())
}

fn engine_info(rest: &[String]) -> Result<(), String> {
    let json_output = rest.iter().any(|a| a == "--json-output");
    let check = rest.iter().any(|a| a == "--check");
    let repo = rest
        .windows(2)
        .find(|pair| pair[0] == "--repo")
        .map(|pair| PathBuf::from(&pair[1]))
        .unwrap_or_else(|| PathBuf::from("."));
    let repo = repo.canonicalize().unwrap_or(repo);

    let binary = std::env::current_exe()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let version = env!("CARGO_PKG_VERSION");
    let store = store_path(&repo);
    let store_present = store.is_file();
    let daemon_pidfile = crate::daemon::pidfile_path_for(&repo);
    let daemon_running = std::fs::read_to_string(&daemon_pidfile)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
        .map(|pid| unsafe { libc::kill(pid, 0) == 0 })
        .unwrap_or(false);
    let ready = store_present;

    if json_output {
        println!(
            "{}",
            serde_json::json!({
                "binary": binary,
                "version": version,
                "repo": repo.display().to_string(),
                "graph_store_present": store_present,
                "graph_store_path": store.display().to_string(),
                "daemon_running": daemon_running,
                "ready": ready,
            })
        );
    } else {
        println!("Binary: {binary}");
        println!("Version: {version}");
        println!("Repo: {}", repo.display());
        println!(
            "Graph store: {} ({})",
            if store_present { "present" } else { "missing" },
            store.display()
        );
        println!(
            "Daemon: {}",
            if daemon_running {
                "running"
            } else {
                "not running"
            }
        );
        println!("Ready: {}", if ready { "yes" } else { "no" });
    }
    if check && !ready {
        return Err(format!(
            "engine not ready: graph store missing at {} (run aethyme-engine-cli index --repo <repo>)",
            store.display()
        ));
    }
    Ok(())
}
