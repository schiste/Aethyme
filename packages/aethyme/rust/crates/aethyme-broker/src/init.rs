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

const PYTEST_SAFE_COMMAND: &str = "python3 -c \"import os, sys; sys.path = [p for p in sys.path if p not in ('', os.getcwd())]; import pytest; raise SystemExit(pytest.console_main())\" -q";

pub const ACTIVATION_MARKER_RELPATH: &str = "aethyme-broker/enrollment.json";
pub const ACTIVATION_MARKER_CONTENT: &str =
    "{\"schema_version\":1,\"coordination\":\"required\"}\n";

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
    checks.push(check_git_output());
    let (repo, checkout_root, main_root) = match crate::GitRepo::discover(repo_hint) {
        Ok(repo) => {
            let checkout_root = repo.root().to_path_buf();
            let main_root = repo.main_root()?;
            checks.push(Check {
                id: "certify.git-repo",
                status: CheckStatus::Pass,
                detail: "inside a git repository".into(),
            });
            match repo.head_commit() {
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
            (repo, checkout_root, main_root)
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
    checks.push(check_binary_version(&main_root));

    // Document requirements: presence + validity, never generation.
    checks.push(if checkout_root.join(".aethyme/gates.toml").exists() {
        validate_gates(&checkout_root)
    } else {
        Check {
            id: "certify.gates",
            status: CheckStatus::Warn,
            detail: "no gates.toml — broker runs conflict-only (no verification); \
                     `aethyme broker gates draft` can draft one"
                .into(),
        }
    });
    checks.push(check_config_valid(&checkout_root));
    checks.push(check_retention_policy(&checkout_root));
    checks.extend(check_enrollment_visibility(&repo, &checkout_root));
    checks.push(check_gitignore_contract(&checkout_root));
    checks.push(Check {
        id: "certify.agents-protocol",
        status: protocol_status(&checkout_root),
        detail: protocol_detail(&checkout_root),
    });
    checks.push(Check {
        id: "certify.graph",
        status: if checkout_root.join(".aethyme/graph").is_dir() {
            CheckStatus::Pass
        } else {
            CheckStatus::Skipped
        },
        detail: if checkout_root.join(".aethyme/graph").is_dir() {
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
        let retention = &report.retention;
        let pending = retention.pending_recovery_digest.is_some();
        let eligible =
            retention.candidate_rows + retention.candidate_files + retention.candidate_worktrees;
        checks.push(Check {
            id: "certify.retention",
            status: if pending || eligible > 0 {
                CheckStatus::Warn
            } else {
                CheckStatus::Pass
            },
            detail: if let Some(digest) = &retention.pending_recovery_digest {
                format!(
                    "confirmed GC recovery is pending; resume with `aethyme broker gc apply --confirm {digest}`"
                )
            } else {
                format!(
                    "{} eligible rows, {} files, {} worktrees; {} reclaimable bytes; {} protected findings",
                    retention.candidate_rows,
                    retention.candidate_files,
                    retention.candidate_worktrees,
                    retention.estimated_reclaimable_bytes,
                    retention.blockers,
                )
            },
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

fn check_retention_policy(main_root: &Path) -> Check {
    match crate::load_retention_policy(main_root) {
        Ok(policy) => Check {
            id: "certify.retention-policy",
            status: CheckStatus::Pass,
            detail: format!(
                "retention schema {} valid; startup recovery budget {}ms",
                policy.schema_version, policy.startup_budget_ms
            ),
        },
        Err(error) => Check {
            id: "certify.retention-policy",
            status: CheckStatus::Fail,
            detail: error.to_string(),
        },
    }
}

/// Deterministic scaffolding: ONLY what the broker needs to work, and
/// ONLY artifacts whose content is identical for every repo (fixed
/// config skeleton, fixed .gitignore block, fixed database schema).
/// Adaptive generation (gates drafting, docs, charts) lives elsewhere.
pub fn scaffold(repo_hint: &Path) -> Result<InitReport, BrokerOpError> {
    let mut checks = Vec::new();
    let repo = crate::GitRepo::discover(repo_hint).map_err(BrokerOpError::Git)?;
    let checkout_root = repo.root().to_path_buf();
    let main_root = repo.main_root()?;

    checks.push(ensure_file(
        &checkout_root.join(".aethyme/config.toml"),
        "scaffold.config-toml",
        || CONFIG_TEMPLATE.to_string(),
    ));
    checks.push(ensure_file(
        &repo.git_common_dir()?.join(ACTIVATION_MARKER_RELPATH),
        "scaffold.shared-activation",
        || ACTIVATION_MARKER_CONTENT.to_string(),
    ));
    checks.push(ensure_gitignore_block(&checkout_root));

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

/// Checkout-local scaffolding for an opt-in Aethyme activation.
///
/// This intentionally omits the tracked `.gitignore` contract. The caller
/// owns local Git exclusions, while broker configuration, gate drafts, and
/// the database retain their ordinary repository-relative locations so the
/// rest of the broker needs no local-mode branches.
pub fn scaffold_local(repo_hint: &Path) -> Result<InitReport, BrokerOpError> {
    let mut checks = Vec::new();
    let repo = crate::GitRepo::discover(repo_hint).map_err(BrokerOpError::Git)?;
    let checkout_root = repo.root().to_path_buf();
    let main_root = repo.main_root()?;

    checks.push(ensure_file(
        &checkout_root.join(".aethyme/config.toml"),
        "scaffold-local.config-toml",
        || CONFIG_TEMPLATE.to_string(),
    ));
    checks.push(ensure_file(
        &repo.git_common_dir()?.join(ACTIVATION_MARKER_RELPATH),
        "scaffold-local.shared-activation",
        || ACTIVATION_MARKER_CONTENT.to_string(),
    ));

    let db_existed = main_root.join(crate::BROKER_DB_RELPATH).exists();
    let store = crate::BrokerStore::open_in_repo(&main_root)?;
    let integrity = store.integrity_check()?;
    checks.push(Check {
        id: "scaffold-local.broker-db",
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
    let checkout_root = repo.root();
    let check = match draft_gate_config(checkout_root) {
        Some(draft) => ensure_file(
            &checkout_root.join(".aethyme/gates.toml"),
            "gates.draft",
            || draft,
        ),
        None if checkout_root.join(".aethyme/gates.toml").exists() => Check {
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

/// Current config.toml schema. The top-level `schema` key is optional;
/// absent means "schema 1".
pub const CONFIG_SCHEMA_VERSION: i64 = 1;

/// The known schema-1 surface: (section, keys). Anything outside this
/// list (or the top-level `schema` key) certifies with a WARN, never a
/// FAIL — configs written for a newer schema must keep working here.
const CONFIG_KNOWN_KEYS: &[(&str, &[&str])] =
    &[("promote", &["mode", "branch"]), ("leases", &["ignore"])];

fn check_enrollment_visibility(repo: &crate::GitRepo, checkout_root: &Path) -> Vec<Check> {
    let marker = repo
        .git_common_dir()
        .map(|common| common.join(ACTIVATION_MARKER_RELPATH));
    let activation = match marker.as_ref().map(std::fs::read_to_string) {
        Ok(Ok(content)) if content == ACTIVATION_MARKER_CONTENT => Check {
            id: "certify.shared-activation",
            status: CheckStatus::Pass,
            detail: "shared Git metadata marks broker coordination as required".into(),
        },
        Ok(Ok(_)) => Check {
            id: "certify.shared-activation",
            status: CheckStatus::Fail,
            detail: "shared enrollment marker is invalid; re-run `aethyme broker scaffold`"
                .into(),
        },
        _ => Check {
            id: "certify.shared-activation",
            status: CheckStatus::Warn,
            detail: "shared enrollment marker is absent; sibling worktrees cannot discover local enrollment until `aethyme broker scaffold` runs".into(),
        },
    };
    let activated = activation.status == CheckStatus::Pass;
    let checkout = Check {
        id: "certify.checkout-enrollment",
        status: if !activated || checkout_root.join(".aethyme/config.toml").exists() {
            CheckStatus::Pass
        } else {
            CheckStatus::Warn
        },
        detail: if !activated {
            "repository is not activated in shared Git metadata".into()
        } else if checkout_root.join(".aethyme/config.toml").exists() {
            "this checkout contains the enrollment configuration".into()
        } else {
            "repository is enrolled locally, but this checkout does not contain the enrollment commit; do not commit or publish from this checkout"
                .into()
        },
    };

    let upstream_ref = repo
        .symbolic_ref("refs/remotes/origin/HEAD")
        .or_else(|| repo.tracking_upstream().map(|(name, _)| name));
    let upstream = match (activated, upstream_ref) {
        (false, _) => Check {
            id: "certify.upstream-enrollment",
            status: CheckStatus::Skipped,
            detail: "shared enrollment is not active".into(),
        },
        (true, None) => Check {
            id: "certify.upstream-enrollment",
            status: CheckStatus::Warn,
            detail: "no local remote-default ref is available; fetch and certify again before first publication".into(),
        },
        (true, Some(reference)) => match repo.path_exists_at(&reference, ".aethyme/config.toml") {
            Ok(true) => Check {
                id: "certify.upstream-enrollment",
                status: CheckStatus::Pass,
                detail: format!("{reference} contains the enrollment configuration"),
            },
            Ok(false) => Check {
                id: "certify.upstream-enrollment",
                status: CheckStatus::Warn,
                detail: format!(
                    "enrollment is absent from {reference}; sibling worktrees based on upstream remain unenrolled — submit and publish the enrollment through broker ship"
                ),
            },
            Err(error) => Check {
                id: "certify.upstream-enrollment",
                status: CheckStatus::Warn,
                detail: format!("cannot inspect {reference}: {error}"),
            },
        },
    };
    vec![activation, checkout, upstream]
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
    let value = match text.parse::<toml::Value>() {
        Ok(value) => value,
        Err(err) => {
            return Check {
                id: "certify.config",
                status: CheckStatus::Fail,
                detail: format!("config.toml invalid: {err}"),
            };
        }
    };
    let unknown = unknown_config_keys(&value);
    if !unknown.is_empty() {
        return Check {
            id: "certify.config",
            status: CheckStatus::Warn,
            detail: format!(
                "config.toml has unknown key(s): {} — ignored by this \
                 version (schema {CONFIG_SCHEMA_VERSION}); typo, or a \
                 newer schema?",
                unknown.join(", ")
            ),
        };
    }
    Check {
        id: "certify.config",
        status: CheckStatus::Pass,
        detail: format!("config.toml valid (schema {CONFIG_SCHEMA_VERSION})"),
    }
}

/// Dotted paths of keys outside the known schema-1 surface, sorted for
/// deterministic reports. A non-integer or unrecognized `schema` value
/// is reported as unknown too (still warn-only).
fn unknown_config_keys(value: &toml::Value) -> Vec<String> {
    let mut unknown = Vec::new();
    let Some(table) = value.as_table() else {
        return unknown;
    };
    for (key, entry) in table {
        if key == "schema" {
            if entry.as_integer() != Some(CONFIG_SCHEMA_VERSION) {
                unknown.push(format!("schema = {entry}"));
            }
            continue;
        }
        match CONFIG_KNOWN_KEYS.iter().find(|(section, _)| section == key) {
            None => unknown.push(key.clone()),
            Some((_, known)) => {
                if let Some(section) = entry.as_table() {
                    for inner in section.keys() {
                        if !known.contains(&inner.as_str()) {
                            unknown.push(format!("{key}.{inner}"));
                        }
                    }
                }
            }
        }
    }
    unknown.sort();
    unknown
}

fn check_gitignore_contract(main_root: &Path) -> Check {
    let existing = std::fs::read_to_string(main_root.join(".gitignore")).unwrap_or_default();
    let satisfied = gitignore_contract_satisfied(&existing);
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
    let minimum = crate::release_compatibility::minimum_git_version_parts();
    if (major, minor) >= minimum {
        Check {
            id: "certify.git-version",
            status: CheckStatus::Pass,
            detail: format!(
                "git {version} (≥ {} required for merge simulation)",
                crate::MINIMUM_GIT_VERSION
            ),
        }
    } else {
        Check {
            id: "certify.git-version",
            status: CheckStatus::Fail,
            detail: format!(
                "git {version} — merge simulation needs git ≥ {}",
                crate::MINIMUM_GIT_VERSION
            ),
        }
    }
}

fn check_git_output() -> Check {
    let Some(resolved_git) = which_program("git") else {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Fail,
            detail: "git not found on PATH; output behavior cannot be certified".into(),
        };
    };
    let resolved = resolved_git.display();
    let probe = match tempfile::tempdir() {
        Ok(probe) => probe,
        Err(err) => {
            return Check {
                id: "certify.git-output",
                status: CheckStatus::Skipped,
                detail: format!(
                    "could not create a temporary repository to probe git output ({err}); resolved: {resolved}"
                ),
            };
        }
    };
    let empty_template = probe.path().join("empty-template");
    let probe_repo = probe.path().join("repo");
    if let Err(err) =
        std::fs::create_dir(&empty_template).and_then(|()| std::fs::create_dir(&probe_repo))
    {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Skipped,
            detail: format!(
                "could not prepare the temporary git probe ({err}); resolved: {resolved}"
            ),
        };
    }
    let empty_config = probe.path().join("empty-gitconfig");
    if let Err(err) = std::fs::write(&empty_config, []) {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Skipped,
            detail: format!(
                "could not isolate the temporary git probe ({err}); resolved: {resolved}"
            ),
        };
    }

    let mut init = isolated_git_probe(&probe_repo, &empty_config);
    let init_output = init
        .arg("init")
        .arg("-q")
        .arg(format!("--template={}", empty_template.display()))
        .output();
    let Ok(init_output) = init_output else {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Skipped,
            detail: format!(
                "could not execute git in the temporary output probe; resolved: {resolved}"
            ),
        };
    };
    if !init_output.status.success() {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Skipped,
            detail: format!(
                "git init failed in the temporary output probe (exit {}); resolved: {resolved}",
                init_output.status
            ),
        };
    }

    let status_output = isolated_git_probe(&probe_repo, &empty_config)
        .arg("status")
        .arg("--porcelain=v1")
        .arg("-z")
        .arg("--untracked-files=all")
        .output();
    let Ok(status_output) = status_output else {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Fail,
            detail: format!(
                "git status could not execute in a clean temporary repository; resolved: {resolved}"
            ),
        };
    };
    if !status_output.status.success() {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Fail,
            detail: format!(
                "git status failed in a clean temporary repository (exit {}); resolved: {resolved}",
                status_output.status
            ),
        };
    }
    if !status_output.stdout.is_empty() {
        return Check {
            id: "certify.git-output",
            status: CheckStatus::Fail,
            detail: format!(
                "git status --porcelain emitted {} bytes on a clean temporary repository; a PATH wrapper is rewriting git output (resolved: {resolved})",
                status_output.stdout.len()
            ),
        };
    }

    Check {
        id: "certify.git-output",
        status: CheckStatus::Pass,
        detail: format!("git preserves known-empty porcelain output (resolved: {resolved})"),
    }
}

fn isolated_git_probe(repo: &Path, empty_config: &Path) -> std::process::Command {
    let mut command = std::process::Command::new("git");
    command
        .current_dir(repo)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", empty_config)
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE");
    command
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

fn check_binary_version(main_root: &Path) -> Check {
    let report = crate::version::inspect_version(main_root);
    let status = match report.status {
        crate::VersionDriftStatus::BehindIntegration
        | crate::VersionDriftStatus::ReleaseBehindIntegration
        | crate::VersionDriftStatus::Unknown => CheckStatus::Warn,
        crate::VersionDriftStatus::Current
        | crate::VersionDriftStatus::AheadOfIntegration
        | crate::VersionDriftStatus::NotAethymeSource => CheckStatus::Pass,
    };
    Check {
        id: "certify.binary-version",
        status,
        detail: report.message,
    }
}

fn which_aethyme() -> Option<std::path::PathBuf> {
    which_program("aethyme")
}

fn which_program(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    let cwd = std::env::current_dir().ok()?;
    std::env::split_paths(&path)
        .map(|dir| {
            let dir = if dir.is_absolute() {
                dir
            } else {
                cwd.join(dir)
            };
            dir.join(program)
        })
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
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
.aethyme/reports/
.aethyme/run/
.aethyme/worktrees/
.aethyme/broker-action-required.md
.aethyme/broker-advisory.md
.aethyme/generated/experience-status.json
.aethyme/generated/experience-status.md
.aethyme/generated/experience-telemetry.jsonl
# aethyme-broker:end
";

fn ensure_gitignore_block(main_root: &Path) -> Check {
    let path = main_root.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    // Hand-maintained repos qualify when every runtime entry is present.
    // A managed block from an older release is upgraded in place below.
    if gitignore_contract_satisfied(&existing) {
        return Check {
            id: "scaffold.gitignore",
            status: CheckStatus::Pass,
            detail: ".gitignore covers broker runtime state".into(),
        };
    }
    let (updated, detail) = if let Some(updated) = replace_managed_gitignore_block(&existing) {
        (updated, "updated the aethyme-broker block in .gitignore")
    } else {
        let mut updated = existing;
        if !updated.is_empty() && !updated.ends_with('\n') {
            updated.push('\n');
        }
        if !updated.is_empty() {
            updated.push('\n');
        }
        updated.push_str(GITIGNORE_BLOCK);
        (updated, "appended the aethyme-broker block to .gitignore")
    };
    match std::fs::write(&path, updated) {
        Ok(()) => Check {
            id: "scaffold.gitignore",
            status: CheckStatus::Created,
            detail: detail.into(),
        },
        Err(err) => Check {
            id: "scaffold.gitignore",
            status: CheckStatus::Fail,
            detail: format!("cannot update .gitignore: {err}"),
        },
    }
}

fn gitignore_contract_satisfied(existing: &str) -> bool {
    GITIGNORE_BLOCK
        .lines()
        .filter(|line| !line.starts_with('#'))
        .all(|required| existing.lines().any(|have| have.trim() == required))
}

fn replace_managed_gitignore_block(existing: &str) -> Option<String> {
    const BEGIN: &str = "# aethyme-broker:begin";
    const END: &str = "# aethyme-broker:end";
    let start = existing.find(BEGIN)?;
    let end_marker = start + existing[start..].find(END)?;
    let mut end = end_marker + END.len();
    if existing.as_bytes().get(end) == Some(&b'\r') {
        end += 1;
    }
    if existing.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    let mut updated = String::with_capacity(existing.len() + GITIGNORE_BLOCK.len());
    updated.push_str(&existing[..start]);
    updated.push_str(GITIGNORE_BLOCK);
    updated.push_str(&existing[end..]);
    Some(updated)
}

const CONFIG_TEMPLATE: &str = "\
# Aethyme broker configuration (generated by `aethyme init`; edit freely).
#
# Known sections/keys (schema 1):
#   schema            optional; the config schema version (currently 1)
#   [promote] mode    \"auto\" | \"manual\"
#   [promote] branch  integration branch name
#   [leases]  ignore  paths never leased (trailing / = directory prefix)
# Unknown keys are ignored at runtime; `aethyme certify` warns on them.

schema = 1

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
pub fn draft_gate_config(main_root: &Path) -> Option<String> {
    let mut gates = String::from(
        "# Draft generated by `aethyme init` from this repo's manifests.\n\
         # REVIEW EVERY GATE: commands, triggers, and cost tiers are guesses.\n\
         # Rules: commands run with cwd = the worktree under test (bare —\n\
         # no venv/node_modules); gate outputs must be gitignored.\n",
    );
    let mut found = false;
    let mut found_test_gate = false;

    // Detector order is fixed: cargo, go, node, python, then Makefile
    // fallback. Never reorder — determinism contract.
    if manifest_exists(main_root, &["Cargo.toml", "rust/Cargo.toml"]) {
        found = true;
        found_test_gate = true;
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
        found_test_gate = true;
        gates.push_str(
            "\n[[gate]]\nname = \"go-test\"\ncommand = \"go test ./...\"\ncost = 2\ntriggers = [\"**/*.go\", \"go.mod\", \"go.sum\"]\n",
        );
    }
    if let Some(scripts) = package_json_scripts(main_root) {
        if scripts.iter().any(|script| script == "lint") {
            found = true;
            let command = node_script_command(main_root, "lint");
            let _ = write!(
                gates,
                "\n[[gate]]\nname = \"js-lint\"\ncommand = \"{command}\"\ncost = 1\ntriggers = [\"**/*.js\", \"**/*.jsx\", \"**/*.ts\", \"**/*.tsx\", \"package.json\"]\n"
            );
        }
        if scripts.iter().any(|script| script == "test") {
            found = true;
            found_test_gate = true;
            let command = node_script_command(main_root, "test");
            let _ = write!(
                gates,
                "\n[[gate]]\nname = \"js-test\"\ncommand = \"{command}\"\ncost = 2\ntriggers = [\"**/*.js\", \"**/*.jsx\", \"**/*.ts\", \"**/*.tsx\", \"package.json\"]\n"
            );
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
            found_test_gate = true;
            let command = toml_basic_string(PYTEST_SAFE_COMMAND);
            let _ = write!(
                gates,
                "\n[[gate]]\nname = \"pytest\"\ncommand = \"{command}\"\ncost = 2\ntriggers = [\"**/*.py\", \"pyproject.toml\"]\n",
            );
        }
    }
    if !found_test_gate && makefile_has_test_target(main_root) {
        found = true;
        gates.push_str(
            "\n[[gate]]\nname = \"make-test\"\ncommand = \"make test\"\ncost = 2\ntriggers = [\"**\"]\n",
        );
    }

    if !found {
        return None;
    }
    Some(gates)
}

fn toml_basic_string(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
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

#[derive(Clone, Copy)]
enum NodeRunner {
    Pnpm,
    Yarn,
    Npm,
}

fn node_runner(main_root: &Path) -> NodeRunner {
    if main_root.join("pnpm-lock.yaml").exists() {
        NodeRunner::Pnpm
    } else if main_root.join("yarn.lock").exists() {
        NodeRunner::Yarn
    } else {
        NodeRunner::Npm
    }
}

fn node_script_command(main_root: &Path, script: &str) -> String {
    match (node_runner(main_root), script) {
        (NodeRunner::Pnpm, _) => format!("pnpm {script}"),
        (NodeRunner::Yarn, _) => format!("yarn {script}"),
        (NodeRunner::Npm, "test") => "npm test --silent".into(),
        (NodeRunner::Npm, _) => format!("npm run {script} --silent"),
    }
}

fn makefile_has_test_target(main_root: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(main_root.join("Makefile")) else {
        return false;
    };
    text.lines().any(|line| {
        if line.starts_with('\t') {
            return false;
        }
        let line = line.trim_start();
        !line.starts_with('#') && line.starts_with("test:")
    })
}

// ── document / control helpers ───────────────────────────────────────

fn protocol_status(main_root: &Path) -> CheckStatus {
    let has_protocol = ["AGENTS.md", "CLAUDE.md"].iter().any(|name| {
        std::fs::read_to_string(main_root.join(name))
            .map(|text| text.contains("Broker Coordination"))
            .unwrap_or(false)
    });
    let local_protocol = main_root.join(".aethyme/local/enabled").is_file()
        && std::fs::read_to_string(main_root.join(".aethyme/local/AGENTS.md"))
            .map(|text| text.contains("Broker Coordination"))
            .unwrap_or(false);
    if has_protocol || local_protocol {
        CheckStatus::Pass
    } else if broker_is_configured(main_root) {
        CheckStatus::Fail
    } else {
        CheckStatus::Skipped
    }
}

fn broker_is_configured(main_root: &Path) -> bool {
    main_root.join(".aethyme/config.toml").is_file()
        || main_root.join(".aethyme/gates.toml").is_file()
}

fn protocol_detail(main_root: &Path) -> String {
    if protocol_status(main_root) == CheckStatus::Pass {
        if main_root.join(".aethyme/local/enabled").is_file() {
            "local agent protocol active through .aethyme/local/AGENTS.md".into()
        } else {
            "agent protocol present in AGENTS.md/CLAUDE.md".into()
        }
    } else if broker_is_configured(main_root) {
        "configured repository is missing the generated Broker Coordination policy — \
         run `aethyme deploy --repo .`; agents will not follow the loop without it"
            .into()
    } else {
        "repository is not deployed — run `aethyme deploy --repo .` to initialize \
         broker state and install mandatory agent policy"
            .into()
    }
}

fn validate_gates(main_root: &Path) -> Check {
    match crate::gates::load_gates(main_root) {
        Ok(gates) => match crate::GitRepo::discover(main_root)
            .and_then(|repository| repository.is_tracked(".aethyme/gates.toml"))
        {
            Ok(true) => Check {
                id: "certify.gates",
                status: CheckStatus::Pass,
                detail: format!("gates.toml valid — {} gate(s), cheap-first", gates.len()),
            },
            Ok(false) => Check {
                id: "certify.gates",
                status: CheckStatus::Warn,
                detail: format!(
                    "gates.toml valid — {} gate(s), but .aethyme/gates.toml is untracked and will be absent from spawned worktrees and submitted trees; review and commit it",
                    gates.len()
                ),
            },
            Err(error) => Check {
                id: "certify.gates",
                status: CheckStatus::Fail,
                detail: format!("cannot determine whether gates.toml is tracked: {error}"),
            },
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
