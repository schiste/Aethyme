//! Per-environment resource accounting and `env calibrate` (#1218).
//!
//! Pool limits and profile units start as guesses. The supervisor samples
//! every active environment's memory and CPU on a low-frequency tick and
//! keeps a bounded history per profile; `env calibrate` turns that history
//! into a proposed `units` per profile and `limit` per pool, printed as a diff
//! of `environments.toml` and never written.
//!
//! Measurement, per runtime (`method`):
//!
//! - `process`: the summed resident size and CPU of the component's process
//!   group, from one bounded `ps` snapshot (`ps_rss`). Like the pressure
//!   report this is observational: pages shared between processes are
//!   counted once per process.
//! - `compose` / `container`: `<engine> stats --no-stream` on the recorded
//!   full ids (`engine_stats`).
//! - `hooks` / `static`: nothing runs, so nothing is measured.
//!
//! Unknown stays `null`, never `0`. Container engines that run in a VM
//! (Docker Desktop, Colima, OrbStack, Podman machine on macOS) reserve VM
//! memory that container stats never show and that parking does not return;
//! calibrate reports it separately and subtracts it from the budget.

use std::collections::BTreeMap;
use std::process::Command;
use std::time::Duration;

use serde::Serialize;

use super::drivers::ComponentIdentity;
use super::engine::resolve;
use super::spec::{Engine, EnvironmentsFile};

/// Samples kept per (repository, profile).
pub const HISTORY_PER_PROFILE: usize = 200;
/// Fewer samples than this per profile is "insufficient history".
pub const MIN_SAMPLES: usize = 10;
pub const DEFAULT_RAM_FRACTION: f64 = 0.5;
pub const SAMPLE_INTERVAL: Duration = Duration::from_secs(60);
const PS_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Sample {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu_pct: Option<f64>,
    pub method: String,
    pub sampled_at: i64,
}

/// One row of a bounded process-table snapshot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProcessRow {
    pub pid: i32,
    pub pgid: i32,
    pub rss_bytes: u64,
    pub cpu_pct: f64,
}

/// One `ps` snapshot of every process: shared by all environments sampled in
/// a tick, so the cost is one process per tick, not one per environment.
pub fn process_table() -> Result<Vec<ProcessRow>, String> {
    let mut command = Command::new("ps");
    command.args(["-A", "-o", "pid=,pgid=,rss=,%cpu="]);
    let output = match crate::bounded_output::output_limited(
        &mut command,
        Duration::from_secs(10),
        PS_LIMIT,
    )
    .map_err(|error| format!("ps: {error}"))?
    {
        crate::bounded_output::LimitedOutput::Completed(output) if output.status.success() => {
            output
        }
        _ => return Err("ps did not complete".into()),
    };
    Ok(parse_process_table(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

pub fn parse_process_table(text: &str) -> Vec<ProcessRow> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some(ProcessRow {
                pid: fields.next()?.parse().ok()?,
                pgid: fields.next()?.parse().ok()?,
                rss_bytes: fields.next()?.parse::<u64>().ok()?.checked_mul(1024)?,
                cpu_pct: fields.next()?.parse().ok()?,
            })
        })
        .collect()
}

/// `docker stats` memory such as `123.4MiB / 7.6GiB` -> bytes of the left side.
pub fn parse_mem_usage(text: &str) -> Option<u64> {
    let used = text.split('/').next()?.trim();
    let split = used.find(|c: char| c.is_ascii_alphabetic())?;
    let (number, unit) = used.split_at(split);
    let number: f64 = number.trim().parse().ok()?;
    let factor: f64 = match unit.trim() {
        "B" => 1.0,
        "KiB" | "kB" | "KB" => 1024.0,
        "MiB" | "MB" => 1024.0 * 1024.0,
        "GiB" | "GB" => 1024.0 * 1024.0 * 1024.0,
        "TiB" | "TB" => 1024.0_f64.powi(4),
        _ => return None,
    };
    Some((number * factor) as u64)
}

fn engine_memory(engine: Engine, ids: &[String]) -> Option<(u64, f64)> {
    if ids.is_empty() {
        return None;
    }
    let cli = resolve(Some(engine)).ok()?;
    let mut args: Vec<&str> = vec![
        "stats",
        "--no-stream",
        "--format",
        "{{.MemUsage}}\t{{.CPUPerc}}",
    ];
    args.extend(ids.iter().map(String::as_str));
    let out = cli
        .run(&args, std::path::Path::new("/"), &BTreeMap::new())
        .ok()?;
    let mut bytes = 0u64;
    let mut cpu = 0f64;
    for line in out.lines().filter(|l| !l.trim().is_empty()) {
        let (memory, percent) = line.split_once('\t')?;
        bytes = bytes.checked_add(parse_mem_usage(memory)?)?;
        cpu += percent
            .trim()
            .trim_end_matches('%')
            .parse::<f64>()
            .unwrap_or(0.0);
    }
    Some((bytes, cpu))
}

/// Measure one environment from its recorded component identities.
pub fn sample(identities: &[ComponentIdentity], table: &[ProcessRow], at: i64) -> Sample {
    let mut bytes: Option<u64> = None;
    let mut cpu: Option<f64> = None;
    let mut methods: Vec<&str> = Vec::new();
    let mut unknown = false;
    for identity in identities {
        let measured = match identity {
            ComponentIdentity::Process(process) => {
                let rows: Vec<&ProcessRow> =
                    table.iter().filter(|r| r.pgid == process.pgid).collect();
                if rows.is_empty() {
                    None
                } else {
                    methods.push("ps_rss");
                    Some((
                        rows.iter().map(|r| r.rss_bytes).sum::<u64>(),
                        rows.iter().map(|r| r.cpu_pct).sum::<f64>(),
                    ))
                }
            }
            ComponentIdentity::Containers { engine, ids, .. } => {
                let measured = engine_memory(*engine, ids);
                if measured.is_some() {
                    methods.push("engine_stats");
                }
                measured
            }
            ComponentIdentity::None => continue,
        };
        match measured {
            Some((b, c)) => {
                bytes = Some(bytes.unwrap_or(0).saturating_add(b));
                cpu = Some(cpu.unwrap_or(0.0) + c);
            }
            None => unknown = true,
        }
    }
    methods.sort();
    methods.dedup();
    let method = if methods.is_empty() {
        "none".to_string()
    } else {
        methods.join("+")
    };
    Sample {
        // A component that runs but could not be measured makes the total
        // unknown, not smaller.
        bytes: if unknown { None } else { bytes },
        cpu_pct: if unknown { None } else { cpu },
        method,
        sampled_at: at,
    }
}

/// Physical memory of this host, if the platform says.
pub fn host_memory_bytes() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let out = Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&out.stdout).trim().parse().ok()
    } else {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        kib.checked_mul(1024)
    }
}

/// Memory a VM-based container engine reserves on this host, if any.
pub fn engine_vm_bytes() -> Option<(String, u64)> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let cli = resolve(None).ok()?;
    if cli.engine == Engine::Container {
        return None; // one VM per container; stopping returns its memory
    }
    let out = cli
        .run(
            &["info", "--format", "{{.MemTotal}}"],
            std::path::Path::new("/"),
            &BTreeMap::new(),
        )
        .ok()?;
    let bytes = out.trim().parse().ok()?;
    Some((cli.engine.as_str().to_string(), bytes))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProfileFootprint {
    pub profile: String,
    pub samples: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub peak_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed_units: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Calibration {
    pub host_memory_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_vm: Option<(String, u64)>,
    pub ram_fraction: f64,
    pub budget_bytes: u64,
    pub unit_bytes: u64,
    pub profiles: Vec<ProfileFootprint>,
    /// Proposed limit per pool.
    pub pools: BTreeMap<String, u32>,
    /// Profiles with too little history to calibrate.
    pub insufficient: Vec<String>,
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    let index = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// Propose units and pool limits from footprints. A unit is the smallest
/// calibrated profile's p95; a pool's limit is how many such units fit in
/// `fraction` of RAM minus the engine VM, so the sum of p95 footprints of a
/// full pool stays within it.
pub fn calibrate(
    file: &EnvironmentsFile,
    history: &BTreeMap<String, Vec<u64>>,
    host_memory_bytes: u64,
    engine_vm: Option<(String, u64)>,
    ram_fraction: f64,
) -> Result<Calibration, String> {
    let mut profiles = Vec::new();
    let mut insufficient = Vec::new();
    for profile in &file.profiles {
        let mut values = history.get(&profile.name).cloned().unwrap_or_default();
        values.sort_unstable();
        if values.len() < MIN_SAMPLES {
            insufficient.push(profile.name.clone());
            profiles.push(ProfileFootprint {
                profile: profile.name.clone(),
                samples: values.len(),
                p50_bytes: None,
                p95_bytes: None,
                peak_bytes: None,
                proposed_units: None,
            });
            continue;
        }
        profiles.push(ProfileFootprint {
            profile: profile.name.clone(),
            samples: values.len(),
            p50_bytes: Some(percentile(&values, 0.5)),
            p95_bytes: Some(percentile(&values, 0.95)),
            peak_bytes: values.last().copied(),
            proposed_units: None,
        });
    }
    // A unit is sized by the smallest profile that consumes pool capacity;
    // zero-unit profiles (static, hooks-only) must not shrink it.
    let consumes = |name: &str| file.profiles.iter().any(|p| p.name == name && p.units > 0);
    let unit_bytes = profiles
        .iter()
        .filter(|p| consumes(&p.profile))
        .filter_map(|p| p.p95_bytes)
        .filter(|b| *b > 0)
        .min()
        .ok_or_else(|| {
            format!(
                "insufficient history: every profile has fewer than {MIN_SAMPLES} samples ({})",
                insufficient.join(", ")
            )
        })?;
    for footprint in &mut profiles {
        // Zero-unit profiles (static, hooks-only) take no pool capacity.
        let consumes = file
            .profiles
            .iter()
            .any(|p| p.name == footprint.profile && p.units > 0);
        footprint.proposed_units = footprint
            .p95_bytes
            .filter(|_| consumes)
            .map(|p95| p95.div_ceil(unit_bytes).max(1) as u32);
    }
    let vm = engine_vm.as_ref().map_or(0, |(_, bytes)| *bytes);
    let budget_bytes = ((host_memory_bytes as f64 * ram_fraction) as u64).saturating_sub(vm);
    let limit = (budget_bytes / unit_bytes).max(1) as u32;
    let pools = file
        .pools
        .keys()
        .map(|name| (name.clone(), limit))
        .collect();
    Ok(Calibration {
        host_memory_bytes,
        engine_vm,
        ram_fraction,
        budget_bytes,
        unit_bytes,
        profiles,
        pools,
        insufficient,
    })
}

/// The environments file rewritten with the proposal, as a unified diff.
/// Only `units = ` in profile tables and `limit = ` in pool tables change.
pub fn proposal_diff(original: &str, calibration: &Calibration) -> String {
    let units: BTreeMap<&str, u32> = calibration
        .profiles
        .iter()
        .filter_map(|p| p.proposed_units.map(|u| (p.profile.as_str(), u)))
        .collect();
    let mut section = String::new();
    let mut profile: Option<String> = None;
    let mut rewritten = String::with_capacity(original.len());
    for line in original.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = trimmed.to_string();
            if trimmed == "[[profiles]]" {
                profile = None;
            }
        }
        if section == "[[profiles]]"
            && let Some(name) = trimmed
                .strip_prefix("name")
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('='))
        {
            profile = Some(name.trim().trim_matches('"').to_string());
        }
        let key = trimmed.split('=').next().unwrap_or_default().trim();
        let replaced = if section == "[[profiles]]" && key == "units" {
            profile
                .as_deref()
                .and_then(|p| units.get(p))
                .map(|u| format!("units = {u}"))
        } else if key == "limit"
            && let Some(pool) = section
                .strip_prefix("[pools.")
                .and_then(|s| s.strip_suffix(']'))
        {
            calibration.pools.get(pool).map(|l| format!("limit = {l}"))
        } else {
            None
        };
        rewritten.push_str(replaced.as_deref().unwrap_or(line));
        rewritten.push('\n');
    }
    similar::TextDiff::from_lines(original, &rewritten)
        .unified_diff()
        .header(
            ".aethyme/environments.toml",
            ".aethyme/environments.toml (proposed)",
        )
        .to_string()
}

impl super::lifecycle::Lifecycle {
    /// Sample every active environment once, from one shared process-table
    /// snapshot. Returns how many were sampled.
    pub fn sample_active(&mut self, now: i64) -> Result<usize, super::lifecycle::LifecycleError> {
        let table = process_table().unwrap_or_default();
        let mut sampled = 0;
        for record in self.store.list(None)? {
            if record.phase != super::store::EnvironmentPhase::Active {
                continue;
            }
            let identities: Vec<ComponentIdentity> = self
                .store
                .components(&record.env_id)?
                .into_iter()
                .filter_map(|c| c.identity)
                .filter_map(|v| serde_json::from_value(v).ok())
                .collect();
            let measured = sample(&identities, &table, now);
            self.store
                .record_sample(&record, &measured, HISTORY_PER_PROFILE)?;
            sampled += 1;
        }
        Ok(sampled)
    }
}
