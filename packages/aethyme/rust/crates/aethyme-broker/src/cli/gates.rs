//! `broker gates`, `hooks`, `trust`, `quick-test` and `verify-loop`: verification.

use super::*;

pub(super) fn aethyme_gates_load(
    main_root: &std::path::Path,
) -> Result<Vec<crate::Gate>, UsageError> {
    Ok(crate::load_gates(main_root)?)
}

pub(super) fn gate_status_label(
    status: crate::GateStatus,
    failure_class: Option<crate::GateFailureClass>,
) -> String {
    match failure_class {
        Some(class) => format!("{}/{}", status.as_str(), class.as_str()),
        None => status.as_str().to_string(),
    }
}

pub(super) const GATE_FAILURE_TAIL_LINES: usize = 20;

pub(super) const GATE_FAILURE_TAIL_BYTES: usize = 16 * 1024;

pub(super) fn render_gate_failure_tail(outcome: &crate::gates::GateRunOutcome) {
    if outcome.status == crate::GateStatus::Pass {
        return;
    }
    let Some(path) = outcome.log_path.as_deref() else {
        return;
    };
    let Ok(bytes) = std::fs::read(path) else {
        return;
    };
    let start = bytes.len().saturating_sub(GATE_FAILURE_TAIL_BYTES);
    let text = String::from_utf8_lossy(&bytes[start..]);
    // aethyme writes an environment header into every gate log, so it has to
    // be held out of the count and the label here: reporting our own line as
    // "gate <name> output" would misattribute it, and on a gate that failed in
    // one line it would double the tail an operator is asked to read.
    let (environment, produced): (Vec<&str>, Vec<&str>) = text
        .lines()
        .partition(|line| line.starts_with(crate::git::SUBPROCESS_PATH_NOTE_PREFIX));
    let mut lines = produced
        .into_iter()
        .rev()
        .take(GATE_FAILURE_TAIL_LINES)
        .collect::<Vec<_>>();
    lines.reverse();
    if lines.is_empty() && environment.is_empty() {
        return;
    }
    if !lines.is_empty() {
        eprintln!(
            "gate {} output (last {} line(s)):",
            outcome.gate,
            lines.len()
        );
        for line in lines {
            eprintln!("  {line}");
        }
    }
    // After the output, not before: the failure is what the operator came for,
    // and this is the context for deciding whether to believe it.
    for line in environment {
        eprintln!("  {line}");
    }
}

pub(super) fn render_hook_reports(
    reports: &[crate::HookReport],
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(reports)?);
    } else {
        for report in reports {
            out!(
                "{:<12} {:<10} {}",
                report.hook,
                report.state.as_str(),
                report.path
            );
        }
    }
    Ok(())
}

pub(super) fn render_quick_test_report(
    report: &crate::QuickTestReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    if report.skipped {
        out!("{}", report.message);
        return Ok(());
    }
    out!("{}", report.message);
    if report.chau7.detected {
        out!(
            "Chau7 runtime markers detected: {}",
            report.chau7.markers.join(", ")
        );
    }
    for step in &report.steps {
        out!("{:<8} {:<20} {}", step.status, step.name, step.detail);
    }
    if let Some(gate) = &report.gate_fixture {
        out!("gate fixture: {}", gate.gate_name);
        out!("  passing entry: q{}", gate.passing_entry_id);
        for outcome in &gate.passing_outcomes {
            out!(
                "    {} {}{} (tree {})",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" },
                short_commit(&outcome.tree_hash),
            );
        }
        out!(
            "  failing entry: q{} ({})",
            gate.failing_entry_id,
            gate.failing_entry_status.as_str()
        );
        for outcome in &gate.failing_outcomes {
            out!(
                "    {} {}{} (tree {})",
                outcome.gate,
                outcome.status.as_str(),
                if outcome.cached { " (cached)" } else { "" },
                short_commit(&outcome.tree_hash),
            );
        }
    }
    out!(
        "temporary repo removed: {}",
        if report.temp_repo_removed {
            "yes"
        } else {
            "no"
        }
    );
    if let Some(head) = &report.integration_head {
        out!("integration head: {}", &head[..12.min(head.len())]);
    }
    Ok(())
}

pub(super) fn render_verify_loop_report(report: &crate::VerifyLoopReport) {
    out!(
        "Broker verify-loop: {}",
        if report.ok { "passed" } else { "failed" }
    );
    out!(
        "  integration tested: {} @ {}",
        report.integration_branch,
        short_commit(&report.tested_integration_head)
    );
    out!(
        "  integration current: {} @ {}",
        report.integration_branch,
        short_commit(&report.current_integration_head)
    );
    if report.integration_moved {
        out!(
            "  warning: integration moved during verification; tested old tip {}, current tip {}; rerun needed",
            short_commit(&report.tested_integration_head),
            short_commit(&report.current_integration_head)
        );
    }
    out!("Steps:");
    for step in &report.steps {
        out!(
            "  {:<22} {:<5} {} ({}ms)",
            step.name,
            step.status.as_str(),
            step.detail,
            step.duration_ms
        );
    }
    if let Some(quick) = &report.quick_test
        && let Some(head) = &quick.integration_head
    {
        out!("  quick-test temp integration: {}", short_commit(head));
    }
    if let Some(doctor) = &report.doctor {
        out!(
            "  doctor version: {} — {}",
            doctor.version.status.as_str(),
            doctor.version.message
        );
    }
    if report.source_tests.attempted {
        out!(
            "  source test command: {}",
            report.source_tests.command.join(" ")
        );
        if report.source_tests.status != crate::VerifyLoopStepStatus::Pass {
            for line in report
                .source_tests
                .stderr_tail
                .iter()
                .chain(report.source_tests.stdout_tail.iter())
                .take(8)
            {
                out!("    {line}");
            }
        }
    }
    if report.ok {
        out!("Next: none");
    } else if report.integration_moved {
        out!("Next: rerun `aethyme broker verify-loop` on the current integration tip.");
    } else {
        out!("Next: fix the failed step above, then rerun `aethyme broker verify-loop`.");
    }
}

pub(super) struct CliGateDoctorProgress;

impl crate::GateProgressSink for CliGateDoctorProgress {
    fn report(&self, line: &str) {
        eprintln!("{line}");
    }
}

pub(super) fn render_gate_doctor(report: &crate::GateDoctorReport) {
    out!(
        "Gate doctor: advisory only at {}",
        short_commit(&report.source_head)
    );
    out!(
        "  repository: {} tracked file(s), {} source file(s)",
        report.tracked_file_count,
        report.source_file_count
    );
    out!("  gates:");
    for gate in &report.gates {
        out!(
            "    [{}] {} — timeout {}; coverage {}% ({}/{} source paths)",
            gate.cost,
            gate.name,
            gate.timeout_seconds
                .map(|seconds| format!("{seconds}s"))
                .unwrap_or_else(|| "unbounded".into()),
            gate.repository_coverage_percent,
            gate.matched_source_paths,
            report.source_file_count,
        );
    }
    if report.findings.is_empty() {
        out!("  findings: none");
    } else {
        out!("  findings:");
        for finding in &report.findings {
            out!(
                "    {:?}/{:?} {:?}{} — {}",
                finding.severity,
                finding.confidence,
                finding.id,
                finding
                    .gate
                    .as_ref()
                    .map(|gate| format!(" [{gate}]"))
                    .unwrap_or_default(),
                finding.summary,
            );
            for evidence in &finding.evidence {
                out!("      evidence: {evidence}");
            }
            out!("      next: {}", finding.remediation);
        }
    }
    if let Some(probe) = &report.probe {
        out!(
            "  probe: {}",
            if probe.passed {
                "passed"
            } else {
                "did not pass"
            }
        );
        out!(
            "    exact HEAD: {}",
            short_commit(&probe.worktree.exact_head)
        );
        out!("    selected: {}", probe.selected_gates.join(", "));
        out!("    normal result cache: untouched");
        for outcome in &probe.outcomes {
            out!(
                "    {}: {}{}",
                outcome.gate,
                gate_status_label(outcome.status, outcome.failure_class),
                outcome
                    .duration_ms
                    .map(|duration| format!(" in {duration}ms"))
                    .unwrap_or_default(),
            );
        }
        for (kind, paths) in [
            ("tracked", &probe.mutations.tracked),
            ("untracked", &probe.mutations.untracked),
            ("ignored", &probe.mutations.ignored),
        ] {
            if !paths.is_empty() {
                out!("    {kind} mutations: {}", capped_join(paths, 8));
            }
        }
    } else {
        out!("  probe: not run (use --probe explicitly)");
    }
}

pub(super) fn render_semantic_gate_advice(report: &crate::SemanticGateAdvice) {
    out!("Semantic gate selection: advisory only");
    out!("  session: {}", report.session_id);
    out!("  enforced by this command: no");
    if report.changed_files.is_empty() {
        out!("  changed files: none");
    } else {
        out!("  changed files: {}", capped_join(&report.changed_files, 8));
    }
    out!(
        "  semantic source: {} ({})",
        report.semantic.provider,
        report.semantic.status.as_str()
    );
    out!("    {}", report.semantic.reason);
    if !report.semantic.impacted_paths.is_empty() {
        out!(
            "  semantic impact paths: {}",
            capped_join(&report.semantic.impacted_paths, 8)
        );
    }
    if report.semantic.truncated {
        out!(
            "  semantic impact result: truncated at {} paths",
            report.semantic.result_limit
        );
    }

    if report.path_selected_gates.is_empty() {
        out!("  path-selected gates: none");
    } else {
        out!("  path-selected gates:");
        for gate in &report.path_selected_gates {
            match &gate.triggered_by {
                Some(path) => out!("    - {} (triggered by {})", gate.gate, path),
                None => out!("    - {} (always runs)", gate.gate),
            }
        }
    }

    if report.semantic_suggested_gates.is_empty() {
        out!("  semantic suggestions: none");
    } else {
        out!("  semantic suggestions:");
        for gate in &report.semantic_suggested_gates {
            match &gate.chain {
                Some(chain) => out!(
                    "    - {} ({} -> {} -> {})",
                    gate.gate,
                    chain.changed_file,
                    chain.caller_file,
                    chain.suggested_gate
                ),
                None => match &gate.triggered_by {
                    Some(path) => out!("    - {} (via {})", gate.gate, path),
                    None => out!("    - {} ({})", gate.gate, gate.reason),
                },
            }
        }
    }
    out!("  next: {}", report.next_action);
}

/// `aethyme broker trust [status]`. Never opens (or creates) the broker
/// database for the check itself: trust is host state.
pub(super) fn run_trust_command(parsed: &Parsed) -> Result<(), UsageError> {
    use crate::broker::gate_trust;

    let status_only = match parsed.positional.as_slice() {
        [] => false,
        [action] if action == "status" => true,
        _ => {
            return Err(UsageError::Message(
                "trust accepts no arguments other than `status`".into(),
            ));
        }
    };
    let dir = match parsed.repository.as_deref() {
        Some(path) => PathBuf::from(path),
        None => std::env::current_dir()
            .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?,
    };
    let status = gate_trust::status(&dir)?;
    if status_only {
        if parsed.json {
            out!("{}", serde_json::to_string_pretty(&status)?);
        } else {
            render_trust_sources(&status.sources);
            out!("record: {}", status.record_path);
            match &status.next_action {
                Some(next) => out!("not trusted; next: {next}"),
                None => out!("trusted"),
            }
        }
        return Ok(());
    }
    let pending = status
        .sources
        .iter()
        .filter(|source| !source.trusted)
        .cloned()
        .collect::<Vec<_>>();
    let escape = gate_trust::test_escape_enabled();
    if !pending.is_empty() && !escape {
        use std::io::IsTerminal as _;
        if !std::io::stdin().is_terminal() {
            return Err(UsageError::Exit {
                message: format!(
                    "refusing to trust without an interactive terminal: approving the \
                     commands a repository runs is a human decision, and agents run without \
                     a terminal. Run `{}` yourself in a terminal.",
                    gate_trust::trust_command(Path::new(&status.repository))
                ),
                code: crate::exit_status::REFUSED,
            });
        }
        eprintln!(
            "Repository {} defines these commands. Aethyme runs them as you when an agent \
             submits, runs gates, commits, or prepares a session:",
            status.repository
        );
        for source in &pending {
            eprintln!(
                "\n{} policy sha256 {}:",
                source.source, source.policy.policy_sha256
            );
            for command in &source.policy.commands {
                eprintln!(
                    "  [{}] {}: {}",
                    command.source, command.name, command.command
                );
            }
        }
        eprint!("\nTrust these commands on this machine? [y/N] ");
        let mut answer = String::new();
        std::io::stdin()
            .read_line(&mut answer)
            .map_err(|err| UsageError::Message(format!("cannot read the answer: {err}")))?;
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "YES" | "Yes") {
            return Err(UsageError::Exit {
                message: "not trusted; nothing was recorded".into(),
                code: crate::exit_status::REFUSED,
            });
        }
    }
    let source = if pending.is_empty() || !escape {
        "interactive"
    } else {
        "test_escape"
    };
    let report = gate_trust::trust(&dir, source)?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_trust_sources(&report.sources);
        if report.recorded.is_empty() {
            out!("Nothing to record: every policy that runs commands was already trusted.");
        } else {
            out!(
                "Trusted {} policy digest(s) for {}.",
                report.recorded.len(),
                report.repository
            );
        }
    }
    Ok(())
}

pub(super) fn render_trust_sources(sources: &[crate::broker::gate_trust::PolicySource]) {
    if sources.is_empty() {
        out!("This repository defines no gate or prepare commands.");
    }
    for source in sources {
        out!(
            "{} policy {} ({}):",
            source.source,
            source.policy.policy_sha256,
            if source.trusted {
                "trusted"
            } else {
                "not trusted"
            }
        );
        for command in &source.policy.commands {
            out!(
                "  [{}] {}: {}",
                command.source,
                command.name,
                command.command
            );
        }
    }
}

/// `broker gates`.
pub(super) fn run_gates(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message(
            "gates requires an action: draft, validate, doctor, manifest, scope, affected, semantic, run, or pre-push"
                .into(),
        ))?;
    match action {
        "draft" => {
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let report = crate::init::draft_gates(&cwd)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                for check in &report.checks {
                    out!(
                        "{:<8} {}",
                        format!("{:?}", check.status).to_lowercase(),
                        check.detail
                    );
                }
            }
        }
        "validate" => {
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let checkout = crate::GitRepo::discover(&cwd)?;
            let gates = aethyme_gates_load(checkout.root())?;
            if parsed.json {
                let summary: Vec<_> = gates
                    .iter()
                    .map(|g| {
                        serde_json::json!({
                            "name": g.name, "command": g.command,
                            "cost": g.cost, "triggers": g.triggers,
                            "cache": g.cache,
                            "timeout_seconds": g.timeout_seconds,
                            "resources": g.resources,
                            "resource_ttl_seconds": g.resource_ttl_seconds,
                            "resource_wait_seconds": g.resource_wait_seconds,
                            "managed_cache": g.managed_cache,
                            "definition_hash": g.definition_hash,
                        })
                    })
                    .collect();
                out!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                out!("gates.toml OK — {} gate(s), cheap-first:", gates.len());
                for gate in gates {
                    out!(
                        "  [{}] {} — {} (triggers: {}{}; timeout: {}; resources: {}; definition: {})",
                        gate.cost,
                        gate.name,
                        gate.command,
                        if gate.triggers.is_empty() {
                            "always".to_string()
                        } else {
                            gate.triggers.join(", ")
                        },
                        if gate.cache { "" } else { "; cache: off" },
                        gate.timeout_seconds
                            .map(|seconds| format!("{seconds}s"))
                            .unwrap_or_else(|| "unbounded".into()),
                        gate.resources.len(),
                        &gate.definition_hash[..12],
                    );
                }
            }
        }
        "doctor" => {
            if parsed.positional.len() != 1 {
                return Err(UsageError::Message(
                    "gates doctor does not accept positional arguments".into(),
                ));
            }
            if parsed.session.is_some() || parsed.all || parsed.no_cache {
                return Err(UsageError::Message(
                    "gates doctor accepts --probe, --only <gate>, and --json; it does not use session, --all, or --no-cache"
                        .into(),
                ));
            }
            if parsed.only.is_some() && !parsed.probe {
                return Err(UsageError::Message(
                    "gates doctor --only <gate> requires --probe".into(),
                ));
            }
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let checkout = crate::GitRepo::discover(&cwd)?;
            let report = if parsed.probe {
                crate::probe_gate_quality(
                    &checkout,
                    parsed.only.as_deref(),
                    &CliGateDoctorProgress,
                )?
            } else {
                crate::inspect_gate_quality(&checkout)?
            };
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_gate_doctor(&report);
            }
        }
        "manifest" => {
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let checkout = crate::GitRepo::discover(&cwd)?;
            let head = parsed.head.as_deref().unwrap_or("HEAD");
            let (head_sha, gates) = crate::load_gates_at_commit(&checkout, head)?;
            let graph_policy =
                crate::graph_integrity::load_graph_policy_at_commit(&checkout, &head_sha)?;
            let manifest = crate::gate_scope_manifest_with_graph(&gates, &graph_policy);
            if parsed.json {
                out!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "policy_head_sha": head_sha,
                        "manifest": manifest,
                    }))?
                );
            } else {
                out!(
                    "Gate scope manifest {} at {}",
                    &manifest.manifest_sha256[..12],
                    &head_sha[..12]
                );
                out!("  schema: {}", manifest.schema_version);
                out!("  gates: {}", manifest.gates.len());
                out!("  semantic suggestions enforced: false");
                out!(
                    "  graph integrity: {} (policy {})",
                    if manifest.graph_integrity.enforced {
                        "enforced"
                    } else {
                        "disabled"
                    },
                    short_commit(&manifest.graph_integrity.policy_sha256)
                );
                for gate in manifest.gates {
                    out!(
                        "  [{}] {} (triggers: {}; cache: {}; timeout: {}; resources: {})",
                        gate.cost,
                        gate.name,
                        if gate.triggers.is_empty() {
                            "always".into()
                        } else {
                            gate.triggers.join(", ")
                        },
                        if gate.cache { "use" } else { "disabled" },
                        gate.timeout_seconds
                            .map(|seconds| format!("{seconds}s"))
                            .unwrap_or_else(|| "unbounded".into()),
                        gate.resources.len()
                    );
                }
            }
        }
        "scope" => {
            let base = parsed.base.as_deref().ok_or(UsageError::Message(
                "gates scope requires --base <ref> and --head <ref>".into(),
            ))?;
            let head = parsed.head.as_deref().ok_or(UsageError::Message(
                "gates scope requires --base <ref> and --head <ref>".into(),
            ))?;
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let checkout = crate::GitRepo::discover(&cwd)?;
            let (head_sha, gates) = crate::load_gates_at_commit(&checkout, head)?;
            let graph_policy =
                crate::graph_integrity::load_graph_policy_at_commit(&checkout, &head_sha)?;
            let report = crate::evaluate_gate_scope_with_graph(
                &checkout,
                &gates,
                &graph_policy,
                base,
                head,
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Gate scope {}..{} (manifest {})",
                    &report.base_sha[..12],
                    &report.head_sha[..12],
                    &report.manifest_sha256[..12]
                );
                out!("  changed paths: {}", report.changed_paths.len());
                out!(
                    "  graph integrity: {} (policy {})",
                    if report.graph_integrity.enforced {
                        "enforced"
                    } else {
                        "disabled"
                    },
                    short_commit(&report.graph_integrity.policy_sha256)
                );
                if report.selected_gates.is_empty() {
                    out!("  selected gates: none");
                } else {
                    out!("  selected gates:");
                    for selection in report.selected_gates {
                        match selection.triggered_by {
                            Some(path) => out!("    {} ({path})", selection.gate),
                            None => out!("    {} (always)", selection.gate),
                        }
                    }
                }
                out!("  semantic suggestions: advisory, not included");
            }
        }
        "affected" => {
            let session = parsed.session.ok_or(UsageError::Message(
                "gates affected requires --session <id>".into(),
            ))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let selections = broker.affected_gates(session)?;
            if parsed.json {
                let out: Vec<_> = selections
                    .iter()
                    .map(|(gate, why)| serde_json::json!({"gate": gate, "triggered_by": why}))
                    .collect();
                out!("{}", serde_json::to_string_pretty(&out)?);
            } else if selections.is_empty() {
                out!("No gates affected by this session's diff.");
            } else {
                for (gate, why) in selections {
                    match why {
                        Some(path) => out!("{gate}  (triggered by {path})"),
                        None => out!("{gate}  (always runs)"),
                    }
                }
            }
        }
        "semantic" => {
            let session = parsed.session.ok_or(UsageError::Message(
                "gates semantic requires --session <id>".into(),
            ))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.semantic_gate_advice(session)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                render_semantic_gate_advice(&report);
            }
        }
        "run" if parsed.all => {
            if parsed.session.is_some() {
                return Err(UsageError::Message(
                    "gates run takes --session <id> or --all, not both".into(),
                ));
            }
            let cwd = std::env::current_dir()
                .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let policy = if parsed.no_cache {
                crate::CachePolicy::Bypass
            } else {
                crate::CachePolicy::Use
            };
            let outcomes = if let Some(gate) = parsed.only.as_deref() {
                broker.run_named_gate_for_checkout_with_policy(&cwd, gate, policy)?
            } else {
                broker.run_all_gates_with_policy(&cwd, policy)?
            };
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&outcomes)?);
            } else {
                for outcome in &outcomes {
                    out!(
                        "{:<20} {:<10} {}{} (tree {})",
                        outcome.gate,
                        gate_status_label(outcome.status, outcome.failure_class),
                        if outcome.cached { "(cached) " } else { "" },
                        outcome
                            .duration_ms
                            .map(|ms| format!("{ms}ms"))
                            .unwrap_or_default(),
                        short_commit(&outcome.tree_hash),
                    );
                    render_gate_failure_tail(outcome);
                }
            }
            // Unlike --session runs, --all is the CI entrypoint:
            // the exit code must be conclusive in --json mode too.
            if outcomes
                .iter()
                .any(|outcome| outcome.status != crate::GateStatus::Pass)
            {
                return Err(UsageError::Message("one or more gates did not pass".into()));
            }
        }
        "run" => {
            let session = parsed.session.ok_or(UsageError::Message(
                "gates run requires --session <id> (or --all)".into(),
            ))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let policy = if parsed.no_cache {
                crate::CachePolicy::Bypass
            } else {
                crate::CachePolicy::Use
            };
            let outcomes = if let Some(gate) = parsed.only.as_deref() {
                broker.run_named_gate_with_policy(session, gate, policy)?
            } else {
                broker.run_gates_with_policy(session, policy)?
            };
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&outcomes)?);
            } else if outcomes.is_empty() {
                out!("No gates affected — nothing to run.");
            } else {
                let mut failed = false;
                for outcome in &outcomes {
                    out!(
                        "{:<20} {:<10} {}{} (tree {})",
                        outcome.gate,
                        gate_status_label(outcome.status, outcome.failure_class),
                        if outcome.cached { "(cached) " } else { "" },
                        outcome
                            .duration_ms
                            .map(|ms| format!("{ms}ms"))
                            .unwrap_or_default(),
                        short_commit(&outcome.tree_hash),
                    );
                    render_gate_failure_tail(outcome);
                    failed |= outcome.status.as_str() != "pass";
                }
                if failed {
                    return Err(UsageError::Message("one or more gates did not pass".into()));
                }
            }
        }
        "pre-push" => {
            if parsed.session.is_some() || parsed.all {
                return Err(UsageError::Message(
                    "gates pre-push does not take --session or --all; it always validates the complete pushed tree".into(),
                ));
            }
            let remote = parsed.positional.get(1).ok_or(UsageError::Message(
                "gates pre-push requires Git's <remote-name> argument".into(),
            ))?;
            if parsed.positional.len() > 3 {
                return Err(UsageError::Message(
                    "gates pre-push takes only Git's <remote-name> and optional <remote-url> arguments".into(),
                ));
            }
            let mut hook_input = String::new();
            std::io::stdin()
                .read_to_string(&mut hook_input)
                .map_err(|error| {
                    UsageError::Message(format!("cannot read pre-push stdin: {error}"))
                })?;
            let cwd = std::env::current_dir()
                .map_err(|error| UsageError::Message(format!("cannot resolve cwd: {error}")))?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.run_pre_push_gates(
                &cwd,
                remote,
                &hook_input,
                if parsed.no_cache {
                    crate::CachePolicy::Bypass
                } else {
                    crate::CachePolicy::Use
                },
            )?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else if report.plan.pushed_sha.is_none() {
                out!("Pre-push: deletion-only update; no content gates required.");
            } else {
                for outcome in &report.gate_outcomes {
                    out!(
                        "{:<20} {:<10} {}{} (tree {})",
                        outcome.gate,
                        gate_status_label(outcome.status, outcome.failure_class),
                        if outcome.cached { "(cached) " } else { "" },
                        outcome
                            .duration_ms
                            .map(|ms| format!("{ms}ms"))
                            .unwrap_or_default(),
                        short_commit(&outcome.tree_hash),
                    );
                }
                out!(
                    "Pre-push: verified {} for {} ref update(s) to {}.",
                    short_commit(report.plan.pushed_sha.as_deref().unwrap_or_default()),
                    report.plan.updates.len(),
                    report.plan.remote,
                );
            }
            if report
                .gate_outcomes
                .iter()
                .any(|outcome| outcome.status != crate::GateStatus::Pass)
            {
                return Err(UsageError::Message(
                    "one or more pre-push gates did not pass".into(),
                ));
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown gates action {other:?} — expected draft, validate, manifest, scope, affected, semantic, run, or pre-push"
            )));
        }
    }
    Ok(())
}

/// `broker hooks`.
pub(super) fn run_hooks(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or(UsageError::Message(
            "hooks requires an action: install, uninstall, status, or snippet".into(),
        ))?;
    // Hook management needs only the git repo — never the broker
    // db, so `hooks install` on a fresh clone creates no state.
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    match action {
        "install" => {
            let repo = crate::GitRepo::discover(&cwd)?;
            let binary = std::env::current_exe().map_err(|err| {
                UsageError::Message(format!("cannot resolve the aethyme binary: {err}"))
            })?;
            let reports = crate::hooks::install(&repo, &binary)?;
            render_hook_reports(&reports, parsed.json)?;
            if !parsed.json {
                out!(
                    "Hooks are shared by every worktree. Uninstall any time with \
                     `aethyme broker hooks uninstall`."
                );
            }
        }
        "uninstall" => {
            let repo = crate::GitRepo::discover(&cwd)?;
            let reports = crate::hooks::uninstall(&repo)?;
            render_hook_reports(&reports, parsed.json)?;
        }
        "status" => {
            let repo = crate::GitRepo::discover(&cwd)?;
            let reports = crate::hooks::status(&repo)?;
            render_hook_reports(&reports, parsed.json)?;
        }
        "snippet" => {
            let hook = parsed.positional.get(1).ok_or_else(|| {
                UsageError::Message(
                    "hooks snippet requires pre-commit, post-commit, or pre-push".into(),
                )
            })?;
            if parsed.positional.len() != 2 {
                return Err(UsageError::Message(
                    "hooks snippet accepts exactly one hook name".into(),
                ));
            }
            let binary = std::env::current_exe().map_err(|err| {
                UsageError::Message(format!("cannot resolve the aethyme binary: {err}"))
            })?;
            let snippet = crate::hooks::snippet(hook, &binary)?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&snippet)?);
            } else {
                out!("{}", snippet.snippet);
            }
        }
        // Internal entry points the installed shims call.
        "pre-commit" => {
            if let Err(err) = crate::hooks::run_pre_commit(&cwd) {
                if let Some(code) = err.exit_code() {
                    return Err(UsageError::Exit {
                        message: err.to_string(),
                        code,
                    });
                }
                return Err(err.into());
            }
        }
        "post-commit" => crate::hooks::run_post_commit(&cwd),
        "pre-push" => {
            if parsed.positional.len() > 3 {
                return Err(UsageError::Message(
                    "hooks pre-push takes Git's remote name and optional URL only".into(),
                ));
            }
            let mut updates = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut updates).map_err(
                |error| UsageError::Message(format!("cannot read pre-push ref updates: {error}")),
            )?;
            crate::hooks::run_pre_push(&cwd, &updates)?;
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown hooks action {other:?} — expected install, uninstall, status, snippet, pre-commit, post-commit, or pre-push"
            )));
        }
    }
    Ok(())
}

/// `broker quick-test`.
pub(super) fn run_quick_test(parsed: Parsed) -> Result<(), UsageError> {
    let mode = if parsed.chau7 {
        crate::QuickTestMode::Chau7
    } else {
        crate::QuickTestMode::Generic
    };
    let report = crate::run_broker_quick_test_with_options(
        mode,
        crate::QuickTestOptions {
            with_gate: parsed.with_gate,
        },
    )?;
    render_quick_test_report(&report, parsed.json)?;
    Ok(())
}

/// `broker verify-loop` | `broker e2e`.
pub(super) fn run_verify_loop(parsed: Parsed) -> Result<(), UsageError> {
    let mut broker = open_broker(parsed.read_only_snapshot)?;
    let cwd = std::env::current_dir()
        .map_err(|err| UsageError::Message(format!("cannot resolve cwd: {err}")))?;
    let report = broker.verify_loop_from(&cwd)?;
    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        render_verify_loop_report(&report);
        if !report.ok {
            return Err(UsageError::Message("broker verify-loop failed".into()));
        }
    }
    Ok(())
}
