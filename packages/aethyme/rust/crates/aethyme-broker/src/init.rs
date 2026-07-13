//! `aethyme init` — the certification pipeline (strategy: "airport
//! certification"). Three Rust-native phases today:
//!
//!   1. certify  — tech preflight (git version, repo, HEAD, binary shadowing)
//!   2. regulate — generate the document requirements (gates.toml draft,
//!                 config.toml, .gitignore block)
//!   5. control  — broker store ready, doctor, gates validate
//!
//! (Phases 3 `document` and 4 `chart` compose in the Python enhance stack
//! and the graph indexer; they are reported as `skipped` until wired.)
//!
//! Determinism contract:
//! - No network, no clocks: generated files contain no timestamps and no
//!   absolute paths; detector order is fixed; report ordering is fixed.
//! - Idempotent: a second run changes nothing on disk (byte-identical),
//!   and reports `exists` instead of `created`.
//! - `--check` is strictly read-only and reports the same findings for
//!   the same repository state, with a scriptable exit code.

use std::fmt::Write as _;
use std::path::Path;

use crate::broker::{Broker, BrokerOpError};

/// Machine-readable outcome of one certification check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// Requirement met (or, in write mode, already in place).
    Pass,
    /// Written by this run (write mode only).
    Created,
    /// Advisory problem — certification proceeds.
    Warn,
    /// Requirement not met — certification fails.
    Fail,
    /// Phase not yet available in the Rust pipeline.
    Skipped,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Check {
    /// Stable identifier, e.g. `certify.git-version`.
    pub id: &'static str,
    pub status: CheckStatus,
    pub detail: String,
}

#[derive(Debug, serde::Serialize)]
pub struct InitReport {
    pub check_mode: bool,
    pub checks: Vec<Check>,
}

impl InitReport {
    pub fn certified(&self) -> bool {
        self.checks.iter().all(|c| c.status != CheckStatus::Fail)
    }
}

/// Run the pipeline. `check_mode` = read-only inspection.
pub fn run(repo_hint: &Path, check_mode: bool) -> Result<InitReport, BrokerOpError> {
    let mut checks = Vec::new();

    // ── phase 1: certify ─────────────────────────────────────────────
    checks.push(check_git_version());
    let main_root = match crate::GitRepo::discover(repo_hint) {
        Ok(repo) => {
            let root = repo.main_root()?;
            checks.push(Check {
                id: "certify.git-repo",
                status: CheckStatus::Pass,
                detail: "inside a git repository".into(),
            });
            match crate::GitRepo::discover(&root)?.head_commit() {
                Ok(_) => checks.push(Check {
                    id: "certify.head-commit",
                    status: CheckStatus::Pass,
                    detail: "repository has at least one commit".into(),
                }),
                Err(_) => checks.push(Check {
                    id: "certify.head-commit",
                    status: CheckStatus::Fail,
                    detail: "no commits yet — the broker needs a HEAD to diff against; \
                             make an initial commit"
                        .into(),
                }),
            }
            root
        }
        Err(_) => {
            checks.push(Check {
                id: "certify.git-repo",
                status: CheckStatus::Fail,
                detail: "not a git repository — run `git init` and make one commit".into(),
            });
            return Ok(InitReport { check_mode, checks });
        }
    };
    checks.push(check_binary_shadowing());

    // ── phase 2: regulate ────────────────────────────────────────────
    // A gates.toml is only drafted when manifests were recognized —
    // writing a file that defines nothing would be dishonest config.
    // Without one the broker runs in conflict-only mode.
    checks.push(match draft_gates_toml(&main_root) {
        Some(draft) => ensure_file(
            &main_root.join(".aethyme/gates.toml"),
            "regulate.gates-toml",
            check_mode,
            || draft,
        ),
        None if main_root.join(".aethyme/gates.toml").exists() => Check {
            id: "regulate.gates-toml",
            status: CheckStatus::Pass,
            detail: ".aethyme/gates.toml present (never overwritten)".into(),
        },
        None => Check {
            id: "regulate.gates-toml",
            status: CheckStatus::Warn,
            detail: "no manifests recognized — define .aethyme/gates.toml yourself;                      until then the broker runs conflict-only (no verification)"
                .into(),
        },
    });
    checks.push(ensure_file(
        &main_root.join(".aethyme/config.toml"),
        "regulate.config-toml",
        check_mode,
        || CONFIG_TEMPLATE.to_string(),
    ));
    checks.push(ensure_gitignore_block(&main_root, check_mode));

    // ── phases 3 & 4: document / chart (not yet in the Rust pipeline) ─
    checks.push(Check {
        id: "document.agents-protocol",
        status: protocol_status(&main_root),
        detail: protocol_detail(&main_root),
    });
    checks.push(Check {
        id: "chart.graph",
        status: if main_root.join(".aethyme/graph").is_dir() {
            CheckStatus::Pass
        } else {
            CheckStatus::Skipped
        },
        detail: if main_root.join(".aethyme/graph").is_dir() {
            "graph fragments present".into()
        } else {
            "no graph fragments — optional; run aethyme-graph-index to build the charts".into()
        },
    });

    // ── phase 5: control ─────────────────────────────────────────────
    let db_path = main_root.join(crate::BROKER_DB_RELPATH);
    if check_mode && !db_path.exists() {
        checks.push(Check {
            id: "control.broker-db",
            status: CheckStatus::Warn,
            detail: "broker database not created yet (created on first init/adopt)".into(),
        });
    } else {
        // Opening creates + migrates the db (write mode) or verifies it
        // opens and passes integrity (both modes when it exists).
        let existed = db_path.exists();
        let mut broker = Broker::open(&main_root)?;
        let report = broker.doctor()?;
        checks.push(Check {
            id: "control.broker-db",
            status: if report.integrity == "ok" {
                if existed {
                    CheckStatus::Pass
                } else {
                    CheckStatus::Created
                }
            } else {
                CheckStatus::Fail
            },
            detail: format!("integrity: {}", report.integrity),
        });
        for id in &report.missing_worktrees {
            checks.push(Check {
                id: "control.missing-worktree",
                status: CheckStatus::Warn,
                detail: format!("session {id} worktree is missing (cleanup or re-adopt)"),
            });
        }
    }
    checks.push(validate_gates(&main_root));

    Ok(InitReport { check_mode, checks })
}

// ── certify helpers ──────────────────────────────────────────────────

fn check_git_version() -> Check {
    let output = std::process::Command::new("git").arg("--version").output();
    let Ok(output) = output else {
        return Check {
            id: "certify.git-version",
            status: CheckStatus::Fail,
            detail: "git not found on PATH".into(),
        };
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let version = text
        .split_whitespace()
        .nth(2)
        .unwrap_or_default()
        .to_string();
    let mut parts = version.split('.').filter_map(|p| p.parse::<u32>().ok());
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    if major > 2 || (major == 2 && minor >= 38) {
        Check {
            id: "certify.git-version",
            status: CheckStatus::Pass,
            detail: format!("git {version} (≥ 2.38 required for merge simulation)"),
        }
    } else {
        Check {
            id: "certify.git-version",
            status: CheckStatus::Fail,
            detail: format!("git {version} — merge simulation needs git ≥ 2.38"),
        }
    }
}

fn check_binary_shadowing() -> Check {
    // If `aethyme` on PATH resolves somewhere other than the running
    // binary, a pip entrypoint (or stale build) is shadowing it (#31).
    let current = std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok());
    let on_path = which_aethyme().and_then(|p| p.canonicalize().ok());
    match (current, on_path) {
        (Some(current), Some(on_path)) if current == on_path => Check {
            id: "certify.binary-path",
            status: CheckStatus::Pass,
            detail: "the running aethyme is the one on PATH".into(),
        },
        (_, None) => Check {
            id: "certify.binary-path",
            status: CheckStatus::Warn,
            detail: "aethyme is not on PATH — agents will need the full binary path".into(),
        },
        (_, Some(on_path)) => Check {
            id: "certify.binary-path",
            status: CheckStatus::Warn,
            detail: format!(
                "PATH resolves aethyme to {} — a different binary shadows this one (#31)",
                on_path.display()
            ),
        },
    }
}

fn which_aethyme() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("aethyme"))
        .find(|candidate| candidate.is_file())
}

// ── regulate helpers ─────────────────────────────────────────────────

fn ensure_file(
    path: &Path,
    id: &'static str,
    check_mode: bool,
    generate: impl FnOnce() -> String,
) -> Check {
    if path.exists() {
        return Check {
            id,
            status: CheckStatus::Pass,
            detail: format!("{} present (never overwritten)", rel(path)),
        };
    }
    if check_mode {
        return Check {
            id,
            status: CheckStatus::Warn,
            detail: format!("{} missing — init would create a draft", rel(path)),
        };
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(path, generate()) {
        Ok(()) => Check {
            id,
            status: CheckStatus::Created,
            detail: format!("{} written — review the draft", rel(path)),
        },
        Err(err) => Check {
            id,
            status: CheckStatus::Fail,
            detail: format!("cannot write {}: {err}", rel(path)),
        },
    }
}

fn rel(path: &Path) -> String {
    // Reports never contain absolute paths (determinism across machines):
    // show from the `.aethyme`/repo-relative tail.
    let text = path.to_string_lossy();
    match text.rfind(".aethyme/") {
        Some(idx) => text[idx..].to_string(),
        None => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| text.into_owned()),
    }
}

const GITIGNORE_BLOCK: &str = "\
# aethyme-broker:begin (managed block — do not edit inside)
.aethyme/broker.db*
.aethyme/logs/
.aethyme/run/
.aethyme/worktrees/
.aethyme/broker-action-required.md
# aethyme-broker:end
";

fn ensure_gitignore_block(main_root: &Path, check_mode: bool) -> Check {
    let path = main_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    // Consider the contract satisfied if the managed block OR all its
    // entries are already present (hand-maintained repos qualify).
    let satisfied = existing.contains("aethyme-broker:begin")
        || GITIGNORE_BLOCK
            .lines()
            .filter(|line| !line.starts_with('#'))
            .all(|line| existing.lines().any(|have| have.trim() == line));
    if satisfied {
        return Check {
            id: "regulate.gitignore",
            status: CheckStatus::Pass,
            detail: ".gitignore covers broker runtime state".into(),
        };
    }
    if check_mode {
        return Check {
            id: "regulate.gitignore",
            status: CheckStatus::Warn,
            detail: ".gitignore is missing broker entries — init would append the managed block"
                .into(),
        };
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    if !updated.is_empty() {
        updated.push('\n');
    }
    updated.push_str(GITIGNORE_BLOCK);
    match std::fs::write(&path, updated) {
        Ok(()) => Check {
            id: "regulate.gitignore",
            status: CheckStatus::Created,
            detail: "appended the aethyme-broker block to .gitignore".into(),
        },
        Err(err) => Check {
            id: "regulate.gitignore",
            status: CheckStatus::Fail,
            detail: format!("cannot update .gitignore: {err}"),
        },
    }
}

const CONFIG_TEMPLATE: &str = "\
# Aethyme broker configuration (generated by `aethyme init`; edit freely).

[promote]
# \"auto\" (default): verified submissions promote to the local integration
# branch immediately. \"manual\" holds them for `aethyme broker promote`.
mode = \"auto\"
# branch = \"aethyme/integration\"

# [leases]
# ignore = [\"generated/\"]   # entries ending in / are directory prefixes
";

/// Deterministic gates.toml draft from manifest sniffing. Detectors run
/// in fixed order; every generated gate is commented as a draft. Detection
/// is deliberately coarse — the draft is a starting point, not a verdict.
/// Returns `None` when no manifests were recognized (no file is written).
fn draft_gates_toml(main_root: &Path) -> Option<String> {
    let mut gates = String::from(
        "# Draft generated by `aethyme init` from this repo's manifests.\n\
         # REVIEW EVERY GATE: commands, triggers, and cost tiers are guesses.\n\
         # Rules: commands run with cwd = the worktree under test (bare —\n\
         # no venv/node_modules); gate outputs must be gitignored.\n",
    );
    let mut found = false;

    // Detector order is fixed and alphabetical by ecosystem: cargo, go,
    // node, python. Never reorder — determinism contract.
    if manifest_exists(main_root, &["Cargo.toml", "rust/Cargo.toml"]) {
        found = true;
        let manifest = if main_root.join("Cargo.toml").exists() {
            "Cargo.toml"
        } else {
            "rust/Cargo.toml"
        };
        let _ = write!(
            gates,
            "\n[[gate]]\nname = \"cargo-test\"\ncommand = \"cargo test --workspace --quiet --manifest-path {manifest}\"\ncost = 3\ntriggers = [\"**/*.rs\", \"**/Cargo.toml\"]\n"
        );
    }
    if manifest_exists(main_root, &["go.mod"]) {
        found = true;
        gates.push_str(
            "\n[[gate]]\nname = \"go-test\"\ncommand = \"go test ./...\"\ncost = 2\ntriggers = [\"**/*.go\", \"go.mod\"]\n",
        );
    }
    if let Some(scripts) = package_json_scripts(main_root) {
        for (script, cost) in [("lint", 1), ("test", 2)] {
            if scripts.contains(&script.to_string()) {
                found = true;
                let _ = write!(
                    gates,
                    "\n[[gate]]\nname = \"npm-{script}\"\ncommand = \"npm run {script}\"\ncost = {cost}\ntriggers = [\"**/*.js\", \"**/*.jsx\", \"**/*.ts\", \"**/*.tsx\", \"package.json\"]\n"
                );
            }
        }
    }
    if let Ok(pyproject) = std::fs::read_to_string(main_root.join("pyproject.toml")) {
        if pyproject.contains("ruff") {
            found = true;
            gates.push_str(
                "\n[[gate]]\nname = \"ruff\"\ncommand = \"python3 -m ruff check .\"\ncost = 1\ntriggers = [\"**/*.py\", \"pyproject.toml\"]\n",
            );
        }
        if pyproject.contains("pytest") {
            found = true;
            gates.push_str(
                "\n[[gate]]\nname = \"pytest\"\ncommand = \"python3 -m pytest -q\"\ncost = 2\ntriggers = [\"**/*.py\", \"pyproject.toml\"]\n",
            );
        }
    }

    if !found {
        return None;
    }
    Some(gates)
}

fn manifest_exists(main_root: &Path, candidates: &[&str]) -> bool {
    candidates.iter().any(|c| main_root.join(c).exists())
}

fn package_json_scripts(main_root: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(main_root.join("package.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let mut scripts: Vec<String> = value.get("scripts")?.as_object()?.keys().cloned().collect();
    scripts.sort();
    Some(scripts)
}

// ── document / control helpers ───────────────────────────────────────

fn protocol_status(main_root: &Path) -> CheckStatus {
    let has_protocol = ["AGENTS.md", "CLAUDE.md"].iter().any(|name| {
        std::fs::read_to_string(main_root.join(name))
            .map(|text| text.contains("Broker Coordination"))
            .unwrap_or(false)
    });
    if has_protocol {
        CheckStatus::Pass
    } else {
        CheckStatus::Skipped
    }
}

fn protocol_detail(main_root: &Path) -> String {
    if protocol_status(main_root) == CheckStatus::Pass {
        "agent protocol present in AGENTS.md/CLAUDE.md".into()
    } else {
        "no Broker Coordination section found — run `aethyme enhance deploy` \
         (Aethyme-enhanced repos) or add the protocol to AGENTS.md; \
         agents will not follow the loop without it"
            .into()
    }
}

fn validate_gates(main_root: &Path) -> Check {
    match crate::gates::load_gates(main_root) {
        Ok(gates) => Check {
            id: "control.gates-valid",
            status: CheckStatus::Pass,
            detail: format!("gates.toml valid — {} gate(s), cheap-first", gates.len()),
        },
        Err(crate::gates::GateConfigError::Missing(_)) => Check {
            id: "control.gates-valid",
            status: CheckStatus::Warn,
            detail: "no gates.toml — broker runs in conflict-only mode (no verification)".into(),
        },
        Err(err) => Check {
            id: "control.gates-valid",
            status: CheckStatus::Fail,
            detail: format!("gates.toml invalid: {err}"),
        },
    }
}
