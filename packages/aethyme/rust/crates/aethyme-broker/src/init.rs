//! Certification and scaffolding — two deliberately separate concerns
//! (decision 2026-07-13: certification covers ONLY highly deterministic
//! topics; anything that adapts to the repo sits elsewhere).
//!
//! **`certify()`** — the certification method. Strictly read-only, no
//! judgment, no generation: verifies facts (git version, repo, HEAD,
//! binary shadowing, config presence + validity, gitignore contract,
//! protocol presence, db integrity). Same repository state → same
//! report, with a scriptable exit code. This is the recurring
//! inspection; it never writes a byte.
//!
//! **`scaffold()`** — deterministic writes only: the artifacts the
//! broker itself needs, whose bytes are ALWAYS identical regardless of
//! the repo (config.toml skeleton, .gitignore block, broker database
//! with its fixed schema). Never overwrites; a second run changes
//! nothing. Certify and scaffold are the "always exactly the same"
//! pair — one reads, one writes.
//!
//! **`draft_gates()`** — adaptive, deliberately separate (`aethyme
//! broker gates draft`): sniffs the repo's manifests and drafts a
//! gates.toml. Its output depends on the repo, which is exactly why it
//! is neither certification nor scaffolding.
//!
//! Shared rules: no network, no clocks; generated files contain no
//! timestamps and no absolute paths; report ordering is fixed.

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

/// The certification method: read-only, deterministic checks only.
pub fn certify(repo_hint: &Path) -> Result<InitReport, BrokerOpError> {
    let mut checks = Vec::new();

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
            return Ok(InitReport {
                check_mode: true,
                checks,
            });
        }
    };
    checks.push(check_binary_shadowing());

    // Document requirements: presence + validity, never generation.
    checks.push(if main_root.join(".aethyme/gates.toml").exists() {
        validate_gates(&main_root)
    } else {
        Check {
            id: "certify.gates",
            status: CheckStatus::Warn,
            detail: "no gates.toml — broker runs conflict-only (no verification); \
                     `aethyme broker scaffold` can draft one"
                .into(),
        }
    });
    checks.push(check_config_valid(&main_root));
    checks.push(check_gitignore_contract(&main_root));
    checks.push(Check {
        id: "certify.agents-protocol",
        status: protocol_status(&main_root),
        detail: protocol_detail(&main_root),
    });
    checks.push(Check {
        id: "certify.graph",
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

    // Broker state: verified only when it exists (certification creates
    // nothing — the db appears on first adopt/scaffold use).
    let db_path = main_root.join(crate::BROKER_DB_RELPATH);
    if db_path.exists() {
        let mut broker = Broker::open(&main_root)?;
        let report = broker.doctor()?;
        checks.push(Check {
            id: "certify.broker-db",
            status: if report.integrity == "ok" {
                CheckStatus::Pass
            } else {
                CheckStatus::Fail
            },
            detail: format!("integrity: {}", report.integrity),
        });
        for id in &report.missing_worktrees {
            checks.push(Check {
                id: "certify.missing-worktree",
                status: CheckStatus::Warn,
                detail: format!("session {id} worktree is missing (cleanup or re-adopt)"),
            });
        }
    } else {
        checks.push(Check {
            id: "certify.broker-db",
            status: CheckStatus::Warn,
            detail: "broker database not created yet (appears on first adopt)".into(),
        });
    }

    Ok(InitReport {
        check_mode: true,
        checks,
    })
}

/// Deterministic scaffolding: ONLY what the broker needs to work, and
/// ONLY artifacts whose content is identical for every repo (fixed
/// config skeleton, fixed .gitignore block, fixed database schema).
/// Adaptive generation (gates drafting, docs, charts) lives elsewhere.
pub fn scaffold(repo_hint: &Path) -> Result<InitReport, BrokerOpError> {
    let mut checks = Vec::new();
    let repo = crate::GitRepo::discover(repo_hint).map_err(BrokerOpError::Git)?;
    let main_root = repo.main_root()?;

    checks.push(ensure_file(
        &main_root.join(".aethyme/config.toml"),
        "scaffold.config-toml",
        || CONFIG_TEMPLATE.to_string(),
    ));
    checks.push(ensure_gitignore_block(&main_root));

    let db_existed = main_root.join(crate::BROKER_DB_RELPATH).exists();
    let store = crate::BrokerStore::open_in_repo(&main_root)?;
    let integrity = store.integrity_check()?;
    checks.push(Check {
        id: "scaffold.broker-db",
        status: if integrity != "ok" {
            CheckStatus::Fail
        } else if db_existed {
            CheckStatus::Pass
        } else {
            CheckStatus::Created
        },
        detail: format!("integrity: {integrity}"),
    });

    Ok(InitReport {
        check_mode: false,
        checks,
    })
}

/// Adaptive gate drafting (`aethyme broker gates draft`): sniff the
/// repo's manifests and write a draft gates.toml. Never overwrites.
/// NOT scaffolding, NOT certification — output depends on the repo.
pub fn draft_gates(repo_hint: &Path) -> Result<InitReport, BrokerOpError> {
    let repo = crate::GitRepo::discover(repo_hint).map_err(BrokerOpError::Git)?;
    let main_root = repo.main_root()?;
    let check = match draft_gates_toml(&main_root) {
        Some(draft) => ensure_file(
            &main_root.join(".aethyme/gates.toml"),
            "gates.draft",
            || draft,
        ),
        None if main_root.join(".aethyme/gates.toml").exists() => Check {
            id: "gates.draft",
            status: CheckStatus::Pass,
            detail: ".aethyme/gates.toml present (never overwritten)".into(),
        },
        None => Check {
            id: "gates.draft",
            status: CheckStatus::Warn,
            detail: "no manifests recognized — define .aethyme/gates.toml yourself; \
                     until then the broker runs conflict-only (no verification)"
                .into(),
        },
    };
    Ok(InitReport {
        check_mode: false,
        checks: vec![check],
    })
}

/// Machine-readable outcome of one guided `aethyme init` run: the three
/// phases in execution order. `None` phases did not run — see each
/// field's doc for the (single) reason why.
#[derive(Debug, serde::Serialize)]
pub struct GuidedInitReport {
    /// Phase 1 — read-only certification (always runs).
    pub certify: InitReport,
    /// Phase 2 — deterministic scaffolding. `None` when certification
    /// failed: init stops before writing anything.
    pub scaffold: Option<InitReport>,
    /// Phase 3 — adaptive gate drafting. `None` when it was skipped
    /// because `.aethyme/gates.toml` already existed (never overwritten)
    /// or because certification failed.
    pub gates: Option<InitReport>,
    /// True when this run wrote anything at all; a second invocation on
    /// the same repository must report `false`.
    pub changed: bool,
}

impl GuidedInitReport {
    pub fn certified(&self) -> bool {
        self.certify.certified()
            && self.scaffold.as_ref().is_none_or(InitReport::certified)
            && self.gates.as_ref().is_none_or(InitReport::certified)
    }
}

/// `aethyme init` — one guided pass over the whole setup: certify
/// (read-only), then scaffold (deterministic, only-if-missing writes),
/// then gate drafting (adaptive, only when no gates.toml exists yet).
/// Pure composition of the three phases above — init adds no setup
/// logic of its own, which is what makes a second run a no-op.
pub fn guided_init(repo_hint: &Path) -> Result<GuidedInitReport, BrokerOpError> {
    let certify = certify(repo_hint)?;
    if !certify.certified() {
        return Ok(GuidedInitReport {
            certify,
            scaffold: None,
            gates: None,
            changed: false,
        });
    }
    let repo = crate::GitRepo::discover(repo_hint).map_err(BrokerOpError::Git)?;
    let main_root = repo.main_root()?;
    let had_gates = main_root.join(".aethyme/gates.toml").exists();
    let scaffold = scaffold(repo_hint)?;
    let gates = if had_gates {
        None
    } else {
        Some(draft_gates(repo_hint)?)
    };
    let changed = scaffold
        .checks
        .iter()
        .chain(gates.iter().flat_map(|report| report.checks.iter()))
        .any(|check| check.status == CheckStatus::Created);
    Ok(GuidedInitReport {
        certify,
        scaffold: Some(scaffold),
        gates,
        changed,
    })
}

fn check_config_valid(main_root: &Path) -> Check {
    let path = main_root.join(".aethyme/config.toml");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Check {
            id: "certify.config",
            status: CheckStatus::Warn,
            detail: "no config.toml — defaults apply (auto-promote, aethyme/integration)".into(),
        };
    };
    match text.parse::<toml::Value>() {
        Ok(_) => Check {
            id: "certify.config",
            status: CheckStatus::Pass,
            detail: "config.toml present and parseable".into(),
        },
        Err(err) => Check {
            id: "certify.config",
            status: CheckStatus::Fail,
            detail: format!("config.toml invalid: {err}"),
        },
    }
}

fn check_gitignore_contract(main_root: &Path) -> Check {
    let existing = std::fs::read_to_string(main_root.join(".gitignore")).unwrap_or_default();
    let satisfied = existing.contains("aethyme-broker:begin")
        || GITIGNORE_BLOCK
            .lines()
            .filter(|line| !line.starts_with('#'))
            .all(|line| existing.lines().any(|have| have.trim() == line));
    if satisfied {
        Check {
            id: "certify.gitignore",
            status: CheckStatus::Pass,
            detail: ".gitignore covers broker runtime state".into(),
        }
    } else {
        Check {
            id: "certify.gitignore",
            status: CheckStatus::Warn,
            detail: ".gitignore is missing broker entries — \
                     `aethyme broker scaffold` appends the managed block"
                .into(),
        }
    }
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

fn ensure_file(path: &Path, id: &'static str, generate: impl FnOnce() -> String) -> Check {
    if path.exists() {
        return Check {
            id,
            status: CheckStatus::Pass,
            detail: format!("{} present (never overwritten)", rel(path)),
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

fn ensure_gitignore_block(main_root: &Path) -> Check {
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
            id: "scaffold.gitignore",
            status: CheckStatus::Pass,
            detail: ".gitignore covers broker runtime state".into(),
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
            id: "scaffold.gitignore",
            status: CheckStatus::Created,
            detail: "appended the aethyme-broker block to .gitignore".into(),
        },
        Err(err) => Check {
            id: "scaffold.gitignore",
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
            id: "certify.gates",
            status: CheckStatus::Pass,
            detail: format!("gates.toml valid — {} gate(s), cheap-first", gates.len()),
        },
        Err(crate::gates::GateConfigError::Missing(_)) => Check {
            id: "certify.gates",
            status: CheckStatus::Warn,
            detail: "no gates.toml — broker runs in conflict-only mode (no verification)".into(),
        },
        Err(err) => Check {
            id: "certify.gates",
            status: CheckStatus::Fail,
            detail: format!("gates.toml invalid: {err}"),
        },
    }
}
