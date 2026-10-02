//! Advisory gate-policy diagnostics and disposable runtime probes.
//!
//! Findings never alter gate selection or execution. Every heuristic carries
//! explicit confidence and bounded, redacted evidence so callers can review
//! the recommendation without exposing executable command text.

use std::collections::BTreeMap;

use crate::Gate;

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum GateDiagnosticId {
    MissingTimeout,
    InvalidTimeout,
    UnknownRetentionField,
    InvalidRetentionConfig,
    NoCheapGate,
    NoFullGate,
    TriggerMatchesNoTrackedPath,
    ExpensiveBroadTrigger,
    UncoveredSourceArea,
    MissingResourceIsolation,
    OriginlessCoordinationKey,
    FixedResourceIdentifier,
    SharedWritableCache,
    MainCheckoutAssumption,
    MissingFailureClassificationEvidence,
    InconsistentFailureClassification,
    DuplicateGate,
    NestedVerificationSlot,
    ProbeMutatesTracked,
    ProbeCreatesUntracked,
    ProbeCreatesIgnored,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GateDoctorGate {
    pub name: String,
    pub cost: i64,
    pub timeout_seconds: Option<u64>,
    pub trigger_count: usize,
    pub matched_tracked_paths: usize,
    pub matched_source_paths: usize,
    pub repository_coverage_percent: u8,
    pub execution_definition_hash: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GateDoctorReport {
    pub schema_version: u32,
    pub advisory_only: bool,
    pub source_head: String,
    pub tracked_file_count: usize,
    pub source_file_count: usize,
    pub gates: Vec<GateDoctorGate>,
    pub findings: Vec<GateDiagnostic>,
    pub maintainer_history_state: String,
    pub maintainer_history_reason: Option<String>,
    pub maintainer_advisories: Vec<crate::MaintainerRecommendation>,
    pub probe: Option<GateProbeReport>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GateProbeReport {
    pub schema_version: u32,
    pub selected_gates: Vec<String>,
    pub worktree: GateProbeWorktree,
    pub outcomes: Vec<GateProbeOutcome>,
    pub mutations: GateProbeMutations,
    pub passed: bool,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GateProbeWorktree {
    pub exact_head: String,
    pub detached: bool,
    pub clean_before: bool,
    pub configuration_source: String,
    pub result_cache: String,
    pub dependency_preparation: String,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GateProbeOutcome {
    pub gate: String,
    pub status: crate::GateStatus,
    pub failure_class: Option<crate::GateFailureClass>,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub definition_hash: String,
    pub acquired_declared_resources: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct GateProbeMutations {
    pub tracked: Vec<String>,
    pub untracked: Vec<String>,
    pub ignored: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GateDoctorError {
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error(transparent)]
    Config(#[from] crate::GateConfigError),
    #[error("cannot inspect gates at exact HEAD {head}: {message}")]
    InvalidPolicy { head: String, message: String },
    #[error("gate probe failed: {0}")]
    Probe(String),
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum GateDiagnosticSeverity {
    Notice,
    Warning,
}

#[derive(Debug, Clone, Copy, serde::Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum GateDiagnosticConfidence {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct GateDiagnostic {
    pub id: GateDiagnosticId,
    pub gate: Option<String>,
    pub severity: GateDiagnosticSeverity,
    pub confidence: GateDiagnosticConfidence,
    pub summary: String,
    pub evidence: Vec<String>,
    pub remediation: String,
}

fn finding(
    id: GateDiagnosticId,
    gate: Option<&str>,
    severity: GateDiagnosticSeverity,
    confidence: GateDiagnosticConfidence,
    summary: impl Into<String>,
    evidence: Vec<String>,
    remediation: impl Into<String>,
) -> GateDiagnostic {
    debug_assert!(!evidence.is_empty());
    GateDiagnostic {
        id,
        gate: gate.map(str::to_string),
        severity,
        confidence,
        summary: summary.into(),
        evidence,
        remediation: remediation.into(),
    }
}

/// Report a verification slot that had to be placed inside the repository.
///
/// The slot is a checkout, and a checkout nested inside the tree under test is
/// not isolated from it: anything walking upward for a workspace root -- the
/// ordinary discovery idiom -- resolves to the *enclosing* checkout whenever
/// the slot is absent or half-built, and answers with the wrong tree instead
/// of failing. Outside the repository the same walk finds nothing and fails
/// loudly, which is the property gate isolation actually depends on (#149).
///
/// Placement is decided at runtime from what the process can write, so only a
/// live inspection can say whether it landed outside. Nothing here is
/// actionable from the gate definitions alone, which is why it is not a static
/// diagnostic.
fn nested_verification_slot_finding(
    placement: &crate::verification::SlotPlacement,
) -> Option<GateDiagnostic> {
    if !placement.inside_repository {
        return None;
    }
    let mut evidence = vec![format!(
        "placed  {} (fallback)",
        placement.directory.display()
    )];
    if let Some(preferred) = &placement.preferred {
        evidence.push(format!("wanted  {}", preferred.display()));
    }
    evidence.extend(
        placement
            .refusals
            .iter()
            .map(|refusal| format!("because {refusal}")),
    );
    evidence.push(
        "risk    ancestor-walk discovery started below this slot resolves to the enclosing checkout instead of failing"
            .into(),
    );
    Some(finding(
        GateDiagnosticId::NestedVerificationSlot,
        None,
        GateDiagnosticSeverity::Warning,
        GateDiagnosticConfidence::High,
        "verification slot is nested inside the repository",
        evidence,
        "Give the broker a writable directory outside this checkout: set AETHYME_HOST_STATE_DIR, or lift the sandbox restriction confining writes to the checkout.",
    ))
}

/// Inspect only gate definitions. Repository-path coverage is added by the
/// exact-HEAD inspector so this function stays deterministic and easy to use
/// for drafts and in-memory policies.
pub fn static_gate_diagnostics(gates: &[Gate]) -> Vec<GateDiagnostic> {
    let mut findings = Vec::new();

    if !gates.iter().any(|gate| gate.cost <= 1) {
        findings.push(finding(
            GateDiagnosticId::NoCheapGate,
            None,
            GateDiagnosticSeverity::Warning,
            GateDiagnosticConfidence::High,
            "no cheap gate is configured",
            vec![format!(
                "all {} configured gate(s) have cost greater than 1",
                gates.len()
            )],
            "Add a fast cost 0 or 1 gate that gives agents early feedback.",
        ));
    }
    if !gates.iter().any(is_repository_wide) {
        findings.push(finding(
            GateDiagnosticId::NoFullGate,
            None,
            GateDiagnosticSeverity::Warning,
            GateDiagnosticConfidence::High,
            "no repository-wide gate is configured",
            vec!["no gate has empty triggers or a repository-wide glob".into()],
            "Add an explicit full gate for CI and reviewed pre-push validation.",
        ));
    }

    for gate in gates {
        let lower = gate.command.to_ascii_lowercase();
        if gate.timeout_seconds.is_none() {
            findings.push(finding(
                GateDiagnosticId::MissingTimeout,
                Some(&gate.name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                format!("gate {:?} has no explicit execution timeout", gate.name),
                vec!["timeout_seconds is absent; execution remains unbounded".into()],
                "Set a reviewed positive timeout_seconds value.",
            ));
        }
        if command_needs_isolation(&lower) && gate.resources.is_empty() {
            findings.push(finding(
                GateDiagnosticId::MissingResourceIsolation,
                Some(&gate.name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::Medium,
                format!(
                    "gate {:?} appears to use a shared service without declared broker resources",
                    gate.name
                ),
                vec![format!(
                    "command family {} was detected and resources is empty",
                    service_family(&lower)
                )],
                "Declare the required tcp_port, exclusive_key, or capacity resources and consume the broker-provided environment.",
            ));
        }
        if fixed_resource_identifier(&gate.command) {
            findings.push(finding(
                GateDiagnosticId::FixedResourceIdentifier,
                Some(&gate.name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::Medium,
                format!("gate {:?} appears to use a fixed shared identifier", gate.name),
                vec!["a literal database port, database name, or Docker project assignment was detected without an Aethyme worker/resource variable".into()],
                "Use the environment returned by the declared broker resource lease or AETHYME_GATE_WORKER_ID/AETHYME_TEST_DB_SUFFIX.",
            ));
        }
        if gate.managed_cache.is_none() && shared_writable_cache(&lower) {
            findings.push(finding(
                GateDiagnosticId::SharedWritableCache,
                Some(&gate.name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::Medium,
                format!("gate {:?} names a shared writable cache", gate.name),
                vec!["an explicit writable cache directory is present but managed_cache is absent".into()],
                "Declare managed_cache and route writes through AETHYME_GATE_CACHE_DIR, or make the cache worktree-local.",
            ));
        }
        if tied_to_main(&lower) {
            findings.push(finding(
                GateDiagnosticId::MainCheckoutAssumption,
                Some(&gate.name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                format!("gate {:?} appears tied to the main branch", gate.name),
                vec!["the command names a main checkout, origin/main, or refs/heads/main".into()],
                "Make the command operate on its current worktree and exact checked-out HEAD.",
            ));
        }
        if masks_exit_status(&lower) {
            findings.push(finding(
                GateDiagnosticId::InconsistentFailureClassification,
                Some(&gate.name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                format!("gate {:?} can mask a failing command", gate.name),
                vec!["the command contains an unconditional success or disables shell error propagation".into()],
                "Preserve the validation command's nonzero exit status so the broker can classify it.",
            ));
        } else if contains_pipeline(&lower) && !lower.contains("pipefail") {
            findings.push(finding(
                GateDiagnosticId::MissingFailureClassificationEvidence,
                Some(&gate.name),
                GateDiagnosticSeverity::Notice,
                GateDiagnosticConfidence::Medium,
                format!(
                    "gate {:?} uses a pipeline without pipefail evidence",
                    gate.name
                ),
                vec!["a shell pipeline is present and no pipefail contract is visible".into()],
                "Use a wrapper that preserves every failing stage's exit status.",
            ));
        }
    }

    let mut equivalent: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for gate in gates {
        equivalent
            .entry(equivalence_key(gate))
            .or_default()
            .push(gate.name.clone());
    }
    for names in equivalent.values_mut() {
        names.sort();
        if names.len() > 1 {
            findings.push(finding(
                GateDiagnosticId::DuplicateGate,
                None,
                GateDiagnosticSeverity::Notice,
                GateDiagnosticConfidence::High,
                "multiple gates have equivalent commands and triggers",
                vec![format!("equivalent gates: {}", names.join(", "))],
                "Keep one gate or make the distinct validation responsibility explicit.",
            ));
        }
    }

    sort_findings(&mut findings);
    findings
}

/// Inspect gate definitions and trigger coverage against the exact committed
/// HEAD. Dirty and untracked files are deliberately not inputs.
pub fn inspect_gate_quality(repo: &crate::GitRepo) -> Result<GateDoctorReport, GateDoctorError> {
    let head = repo.head_commit()?;
    let text = repo
        .file_at_commit(&head, crate::GATES_CONFIG_RELPATH)?
        .ok_or_else(|| {
            crate::GateConfigError::Missing(repo.root().join(crate::GATES_CONFIG_RELPATH))
        })?;
    let (gates, mut findings) = parse_doctor_gates(&head, &text)?;

    // #170: with no `origin` the coordination key falls back to the main
    // checkout's absolute path. That is stable across this repository's
    // worktrees, but it is not the cross-clone identity a declared pool
    // reads as, and it rotates if the checkout moves. Resolved once: it
    // costs a `git remote` call.
    let key_source = crate::gates::repository_key(repo).1;
    let tracked = repo.tracked_files_at(&head)?;
    let sources = tracked
        .iter()
        .filter(|path| is_source_path(path))
        .cloned()
        .collect::<Vec<_>>();
    let mut summaries = Vec::new();
    for gate in &gates {
        let matched = tracked.iter().filter(|path| gate.matches(path)).count();
        let matched_sources = sources.iter().filter(|path| gate.matches(path)).count();
        let percent = if tracked.is_empty() {
            0
        } else {
            ((matched * 100) / tracked.len()).min(100) as u8
        };
        summaries.push(GateDoctorGate {
            name: gate.name.clone(),
            cost: gate.cost,
            timeout_seconds: gate.timeout_seconds,
            trigger_count: gate.triggers.len(),
            matched_tracked_paths: matched,
            matched_source_paths: matched_sources,
            repository_coverage_percent: percent,
            execution_definition_hash: gate.definition_hash.clone(),
        });

        // Raised per gate, and only for a gate that actually stakes
        // something on the key: a gate declaring neither a resource pool nor
        // a managed cache loses nothing to a path-derived key, so saying so
        // would be noise.
        if key_source == crate::gates::RepositoryKeySource::MainCheckoutPath {
            let mut staked = Vec::new();
            if !gate.resources.is_empty() {
                staked.push(format!("{} declared resource(s)", gate.resources.len()));
            }
            if let Some(cache) = &gate.managed_cache {
                staked.push(format!("managed cache {:?}", cache.key));
            }
            if !staked.is_empty() {
                findings.push(finding(
                    GateDiagnosticId::OriginlessCoordinationKey,
                    Some(&gate.name),
                    GateDiagnosticSeverity::Notice,
                    GateDiagnosticConfidence::High,
                    format!(
                        "gate {:?} coordinates under a path-derived repository key",
                        gate.name
                    ),
                    vec![
                        "no `origin` remote is configured, so the coordination key is the main checkout's absolute path".into(),
                        format!("at stake: {}", staked.join(", ")),
                    ],
                    "Configure an `origin` remote, or accept that this gate shares its pool and managed cache only with checkouts of this path on this machine.",
                ));
            }
        }

        let unmatched = gate
            .triggers
            .iter()
            .filter(|trigger| !tracked.iter().any(|path| trigger_matches(trigger, path)))
            .cloned()
            .collect::<Vec<_>>();
        if !unmatched.is_empty() {
            findings.push(finding(
                GateDiagnosticId::TriggerMatchesNoTrackedPath,
                Some(&gate.name),
                GateDiagnosticSeverity::Notice,
                GateDiagnosticConfidence::High,
                format!(
                    "gate {:?} has triggers that match no tracked path",
                    gate.name
                ),
                vec![format!("unmatched triggers: {}", unmatched.join(", "))],
                "Remove stale triggers or point them at tracked repository paths.",
            ));
        }
        if gate.cost > 1 && tracked.len() >= 5 && percent >= 80 {
            findings.push(finding(
                GateDiagnosticId::ExpensiveBroadTrigger,
                Some(&gate.name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                format!(
                    "expensive gate {:?} is triggered by most tracked files",
                    gate.name
                ),
                vec![format!(
                    "cost {} gate matches {matched}/{} tracked paths ({percent}%)",
                    gate.cost,
                    tracked.len()
                )],
                "Narrow its triggers or split a cheap targeted gate from the repository-wide gate.",
            ));
        }
    }

    let mut source_areas: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in &sources {
        source_areas
            .entry(source_area(path))
            .or_default()
            .push(path.clone());
    }
    for (area, paths) in source_areas {
        if !paths
            .iter()
            .any(|path| gates.iter().any(|gate| gate.matches(path)))
        {
            findings.push(finding(
                GateDiagnosticId::UncoveredSourceArea,
                None,
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                format!("source area {area:?} is not covered by any gate trigger"),
                vec![format!("{area} contains {} tracked source path(s)", paths.len())],
                "Add a gate trigger for this source area or document why it is intentionally unvalidated.",
            ));
        }
    }

    // Retention is repository-local configuration rather than a committed
    // gate definition. Inspect it here because this is the other read-only
    // health surface operators use when checking whether the broker can keep
    // its runtime state bounded.
    if let Ok(root) = repo.main_root() {
        match crate::load_retention_policy_report(&root) {
            Ok(report) => {
                for warning in report.warnings {
                    findings.push(finding(
                        GateDiagnosticId::UnknownRetentionField,
                        None,
                        GateDiagnosticSeverity::Warning,
                        GateDiagnosticConfidence::High,
                        "retention configuration contains an unknown field",
                        vec![warning.to_string()],
                        "Remove or correct the named field in `.aethyme/broker.toml`, or upgrade the binary if the field is intentional.",
                    ));
                }
            }
            Err(error) => findings.push(finding(
                GateDiagnosticId::InvalidRetentionConfig,
                None,
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                "retention configuration cannot be loaded",
                vec![error.to_string()],
                "Fix the named retention field in `.aethyme/broker.toml`, then rerun `aethyme broker gc plan`.",
            )),
        }

        // Planning creates the directory it selects, which is the directory a
        // slot would use anyway, so asking costs nothing a gate run would not
        // have already spent.
        let placement = crate::verification::plan_slot_placement(&root, "merge-sim");
        findings.extend(nested_verification_slot_finding(&placement));
    }

    sort_findings(&mut findings);
    let history = repo
        .main_root()
        .map(|root| crate::recommendations::inspect_history_recommendations(&root))
        .unwrap_or_else(|error| crate::recommendations::RecommendationInspection {
            state: "inaccessible",
            reason: Some(error.to_string()),
            recommendations: Vec::new(),
        });
    Ok(GateDoctorReport {
        schema_version: 2,
        advisory_only: true,
        source_head: head,
        tracked_file_count: tracked.len(),
        source_file_count: sources.len(),
        gates: summaries,
        findings,
        maintainer_history_state: history.state.into(),
        maintainer_history_reason: history.reason,
        maintainer_advisories: history.recommendations,
        probe: None,
    })
}

/// Run all gates, or one explicitly selected gate, in a disposable detached
/// worktree at exact committed HEAD. Runtime rows and logs live in a temporary
/// broker-owned directory, so the repository's normal gate cache is neither
/// read nor populated.
pub fn probe_gate_quality(
    repo: &crate::GitRepo,
    only: Option<&str>,
    progress: &dyn crate::GateProgressSink,
) -> Result<GateDoctorReport, GateDoctorError> {
    let mut report = inspect_gate_quality(repo)?;
    let head = report.source_head.clone();
    let (_, gates) = crate::load_gates_at_commit(repo, &head)
        .map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    if let Some(name) = only
        && !gates.iter().any(|gate| gate.name == name)
    {
        return Err(GateDoctorError::Probe(format!(
            "no configured gate named {name:?}"
        )));
    }
    let selected_gates = gates
        .iter()
        .filter(|gate| only.is_none_or(|name| gate.name == name))
        .map(|gate| gate.name.clone())
        .collect::<Vec<_>>();

    let main_root = repo.main_root()?;
    // A probe runs the committed gates for real, so it needs the same trust
    // as any other run of repository-defined commands.
    crate::broker::gate_trust::policy_at_commit(repo, &head)
        .and_then(|policy| {
            crate::broker::gate_trust::require_trusted_standalone(&main_root, &policy)
        })
        .map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    let mut slot =
        crate::verification::ExactTreeVerificationSlot::acquire(&main_root, "gate-doctor-probe")
            .map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    let checkout = slot
        .materialize(repo, &head)
        .map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    let before = capture_probe_state(checkout.root())?;
    let runtime = tempfile::tempdir().map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    let mut store = crate::BrokerStore::open(&runtime.path().join("probe.db"))
        .map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    let outcomes = if let Some(name) = only {
        crate::gates::run_named(
            &mut store,
            runtime.path(),
            &checkout,
            &gates,
            &[],
            name,
            None,
            crate::CachePolicy::Bypass,
        )
    } else {
        crate::gates::run_all_with_progress(
            &mut store,
            runtime.path(),
            &checkout,
            &gates,
            None,
            crate::CachePolicy::Bypass,
            progress,
        )
    }
    .map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    let after = capture_probe_state(checkout.root())?;
    let mutations = after.difference(&before);
    add_probe_mutation_findings(&mut report.findings, &mutations);
    sort_findings(&mut report.findings);
    let outcomes = outcomes
        .into_iter()
        .map(|outcome| GateProbeOutcome {
            gate: outcome.gate,
            status: outcome.status,
            failure_class: outcome.failure_class,
            exit_code: outcome.exit_code,
            duration_ms: outcome.duration_ms,
            definition_hash: outcome.definition_hash,
            acquired_declared_resources: outcome.resource_lease.is_some(),
        })
        .collect::<Vec<_>>();
    let passed = outcomes
        .iter()
        .all(|outcome| outcome.status == crate::GateStatus::Pass);
    report.probe = Some(GateProbeReport {
        schema_version: 1,
        selected_gates,
        worktree: GateProbeWorktree {
            exact_head: head,
            detached: true,
            clean_before: before.is_clean(),
            configuration_source: "committed_head".into(),
            result_cache: "ephemeral_probe_only".into(),
            dependency_preparation: "not_run".into(),
        },
        outcomes,
        mutations,
        passed,
    });
    Ok(report)
}

#[derive(Default)]
struct ProbeState {
    tracked: Vec<String>,
    untracked: Vec<String>,
    ignored: Vec<String>,
}

impl ProbeState {
    fn is_clean(&self) -> bool {
        self.tracked.is_empty() && self.untracked.is_empty() && self.ignored.is_empty()
    }

    fn difference(&self, before: &Self) -> GateProbeMutations {
        GateProbeMutations {
            tracked: difference(&self.tracked, &before.tracked),
            untracked: difference(&self.untracked, &before.untracked),
            ignored: difference(&self.ignored, &before.ignored),
        }
    }
}

fn capture_probe_state(root: &std::path::Path) -> Result<ProbeState, GateDoctorError> {
    let output = crate::git::git_command()
        .args([
            "status",
            "--porcelain=v1",
            "--ignored=matching",
            "--untracked-files=all",
            "-z",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| GateDoctorError::Probe(error.to_string()))?;
    if !output.status.success() {
        return Err(GateDoctorError::Probe(format!(
            "cannot inspect disposable worktree state (exit {})",
            output.status.code().unwrap_or(-1)
        )));
    }
    let mut state = ProbeState::default();
    for record in output.stdout.split(|byte| *byte == 0) {
        if record.len() < 4 {
            continue;
        }
        let status = &record[..2];
        let path = String::from_utf8_lossy(&record[3..]).into_owned();
        match status {
            b"??" => state.untracked.push(path),
            b"!!" => state.ignored.push(path),
            _ => state.tracked.push(path),
        }
    }
    state.tracked.sort();
    state.tracked.dedup();
    state.untracked.sort();
    state.untracked.dedup();
    state.ignored.sort();
    state.ignored.dedup();
    Ok(state)
}

fn difference(after: &[String], before: &[String]) -> Vec<String> {
    after
        .iter()
        .filter(|path| before.binary_search(path).is_err())
        .cloned()
        .collect()
}

fn add_probe_mutation_findings(findings: &mut Vec<GateDiagnostic>, mutations: &GateProbeMutations) {
    for (id, paths, summary, remediation) in [
        (
            GateDiagnosticId::ProbeMutatesTracked,
            &mutations.tracked,
            "probe gates modified tracked files",
            "Make gate commands read-only with respect to tracked repository files.",
        ),
        (
            GateDiagnosticId::ProbeCreatesUntracked,
            &mutations.untracked,
            "probe gates created untracked files",
            "Write disposable outputs under ignored, worktree-local paths or broker-managed caches.",
        ),
        (
            GateDiagnosticId::ProbeCreatesIgnored,
            &mutations.ignored,
            "probe gates created ignored files",
            "Confirm ignored outputs are worktree-local and cannot collide across concurrent workers.",
        ),
    ] {
        if !paths.is_empty() {
            findings.push(finding(
                id,
                None,
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                summary,
                vec![format!("paths: {}", paths.join(", "))],
                remediation,
            ));
        }
    }
}

fn parse_doctor_gates(
    head: &str,
    text: &str,
) -> Result<(Vec<Gate>, Vec<GateDiagnostic>), GateDoctorError> {
    let mut value: toml::Value =
        text.parse()
            .map_err(|error: toml::de::Error| GateDoctorError::InvalidPolicy {
                head: head.into(),
                message: error.to_string(),
            })?;
    let entries = value
        .get_mut("gate")
        .and_then(toml::Value::as_array_mut)
        .ok_or_else(|| GateDoctorError::InvalidPolicy {
            head: head.into(),
            message: "expected at least one [[gate]] table".into(),
        })?;
    let mut findings = Vec::new();
    let mut invalid_names = Vec::new();
    for (index, entry) in entries.iter_mut().enumerate() {
        let Some(table) = entry.as_table_mut() else {
            continue;
        };
        let name = table
            .get("name")
            .and_then(toml::Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("gate[{index}]"));
        let invalid = table
            .get("timeout_seconds")
            .is_some_and(|value| value.as_integer().is_none_or(|seconds| seconds <= 0));
        if invalid {
            table.remove("timeout_seconds");
            invalid_names.push(name.clone());
            findings.push(finding(
                GateDiagnosticId::InvalidTimeout,
                Some(&name),
                GateDiagnosticSeverity::Warning,
                GateDiagnosticConfidence::High,
                format!("gate {name:?} has an invalid execution timeout"),
                vec!["timeout_seconds is not a positive integer".into()],
                "Set timeout_seconds to a reviewed positive integer.",
            ));
        }
    }
    let normalized = toml::to_string(&value).map_err(|error| GateDoctorError::InvalidPolicy {
        head: head.into(),
        message: error.to_string(),
    })?;
    let gates = crate::parse_gates(&normalized)?;
    let mut static_findings = static_gate_diagnostics(&gates);
    static_findings.retain(|finding| {
        finding.id != GateDiagnosticId::MissingTimeout
            || finding
                .gate
                .as_ref()
                .is_none_or(|name| !invalid_names.contains(name))
    });
    findings.extend(static_findings);
    Ok((gates, findings))
}

fn trigger_matches(trigger: &str, path: &str) -> bool {
    globset::Glob::new(trigger)
        .ok()
        .map(|glob| glob.compile_matcher().is_match(path))
        .unwrap_or(false)
}

fn is_source_path(path: &str) -> bool {
    let extension = path.rsplit_once('.').map(|(_, extension)| extension);
    matches!(
        extension,
        Some(
            "rs" | "py"
                | "js"
                | "jsx"
                | "ts"
                | "tsx"
                | "go"
                | "java"
                | "kt"
                | "kts"
                | "swift"
                | "c"
                | "cc"
                | "cpp"
                | "h"
                | "hpp"
                | "cs"
                | "rb"
                | "php"
                | "vue"
                | "svelte"
        )
    )
}

fn source_area(path: &str) -> String {
    let parts = path.split('/').collect::<Vec<_>>();
    if parts.len() <= 1 {
        return ".".into();
    }
    if matches!(
        parts[0],
        "src" | "lib" | "app" | "apps" | "packages" | "crates" | "services"
    ) && parts.len() > 2
    {
        format!("{}/{}", parts[0], parts[1])
    } else {
        parts[0].into()
    }
}

pub(crate) fn sort_findings(findings: &mut [GateDiagnostic]) {
    findings.sort_by(|left, right| {
        left.id
            .cmp(&right.id)
            .then(left.gate.cmp(&right.gate))
            .then(left.summary.cmp(&right.summary))
    });
}

fn is_repository_wide(gate: &Gate) -> bool {
    gate.triggers.is_empty()
        || gate
            .triggers
            .iter()
            .any(|trigger| matches!(trigger.as_str(), "*" | "**" | "**/*"))
}

fn command_needs_isolation(command: &str) -> bool {
    [
        "docker ",
        "docker-compose",
        "postgres",
        "pg_isready",
        "psql ",
    ]
    .iter()
    .any(|token| command.contains(token))
}

fn service_family(command: &str) -> &'static str {
    if command.contains("docker ") || command.contains("docker-compose") {
        "docker"
    } else {
        "postgresql"
    }
}

/// A hardcoded listening port in a gate command.
///
/// Parsed rather than substring-matched. The previous check looked for the
/// literal `:5432` and `=5432`, which both miss the two most common dev-server
/// ports (`:3000`, `:8080`) entirely and fire on unrelated text — a fixture
/// string, or `:15432` standing in for a non-default port.
///
/// Three shapes are recognised, all of which name one specific port:
///
/// - `:NNNN` as a URL authority or host separator (`http://localhost:3000`)
/// - `PORT=NNNN` and `--port NNNN` / `--port=NNNN`
/// - a bare `-p NNNN` is *not* matched: `-p` is too overloaded (it is
///   `cargo --package`, `docker publish`, `pgrep`) to carry this meaning
///
/// A non-default port is as much a collision as the default one, so the whole
/// digit run is read: `:15432` is port 15432, never `5432` with a stray prefix.
fn hardcoded_port(command: &str) -> Option<u16> {
    let bytes = command.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        // `:NNNN` after a host — the `localhost:3000` case is the reason this is
        // a parse and not a token list.
        if bytes[index] == b':'
            && bytes.get(index + 1).is_some_and(u8::is_ascii_digit)
            && host_before_colon(&command[..index])
            // A quoted literal names a value under test, not a listener:
            // `grep -c ':5432' expected.txt` asserts about a port without
            // binding one. Only a colon outside quotes configures anything.
            && !inside_quotes(&command[..index])
            && let Some((port, _)) = port_digits(&command[index + 1..])
        {
            return Some(port);
        }
        // `PORT=NNNN`, `PGPORT=NNNN`, `SERVER_PORT=NNNN`: an identifier that
        // names a port, assigned a number.
        if (index == 0 || !is_name_byte(bytes[index - 1]))
            && let Some(port) = port_assignment(&command[index..])
        {
            return Some(port);
        }
        // `--port NNNN` / `--port=NNNN`.
        let rest = &command[index..];
        if let Some(tail) = rest
            .strip_prefix("--port=")
            .or_else(|| rest.strip_prefix("--port "))
            && let Some((port, _)) = port_digits(tail)
        {
            return Some(port);
        }
        index += 1;
    }
    None
}

/// Whether the text before a `:NNNN` names a host, so the digits are a port.
///
/// A host is `localhost`, an IP address, a dotted name, a bare `:3000` listen
/// address, a URL authority (`postgres://db:5432`), or the published half of a
/// Docker mapping (`5432:5432`). Anything else is a name with a tag —
/// `postgres:16`, `node:20-alpine`, `ghcr.io/acme/app:1234` — which pins a
/// version, not a listener. A dotted registry path counts as an image, not a
/// host, because the segment that carries the tag follows a `/`.
fn host_before_colon(prefix: &str) -> bool {
    let start = prefix
        .rfind(|character: char| {
            character.is_ascii_whitespace()
                || matches!(character, '/' | '@' | '=' | '\'' | '"' | '(')
        })
        .map_or(0, |position| position + 1);
    let segment = &prefix[start..];
    let delimiter = prefix[..start].chars().last();
    let authority = prefix[..start].ends_with("//") || delimiter == Some('@');
    if delimiter == Some('/') && !authority {
        // `registry/name:tag` — an image path, never a host.
        return false;
    }
    segment.is_empty()
        || authority
        || segment.eq_ignore_ascii_case("localhost")
        || segment
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        || (segment.contains('.') && !segment.contains('/'))
}

/// `NAME=NNNN` where `NAME` is `PORT`, ends in `_PORT`, or is `PGPORT`, in any
/// case. A word that merely ends in those letters (`SUPPORT`, `REPORT`) is not
/// a port variable.
fn port_assignment(text: &str) -> Option<u16> {
    let end =
        text.find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))?;
    let name = &text[..end];
    let value = text[end..].strip_prefix('=')?;
    let upper = name.to_ascii_uppercase();
    if upper == "PORT" || upper == "PGPORT" || upper.ends_with("_PORT") {
        return port_digits(value).map(|(port, _)| port);
    }
    None
}

/// A fixed value assigned to a name that identifies a shared resource
/// (`COMPOSE_PROJECT_NAME=app`, `DATABASE_URL=...`).
fn fixed_shared_name(command: &str) -> bool {
    let bytes = command.as_bytes();
    (0..bytes.len()).any(|index| {
        (index == 0 || !is_name_byte(bytes[index - 1]))
            && screaming_snake_identifier(&command[index..]).is_some_and(|(identifier, end)| {
                command[index + end..].starts_with('=')
                    && SHARED_IDENTIFIER_NAMES.contains(&identifier)
            })
    })
}

/// A hardcoded port must be detected whichever shape it takes: the two dev-server
/// ports the previous substring check missed, the forms it did catch, and the
/// broker-provided spellings that must stay silent.
#[cfg(test)]
mod port_detection {
    /// Run a command through the same entry point `gates doctor` uses, so a
    /// test cannot pass on an input production never sees (the inspection
    /// lowercases nothing it must not, and these spellings reach it verbatim).
    fn detect(command: &str) -> bool {
        let gates = crate::parse_gates(&format!(
            "[[gate]]\nname = \"g\"\ncommand = '''{command}'''\ncost = 1\ntriggers = [\"**\"]\ntimeout_seconds = 60\n"
        ))
        .unwrap();
        super::static_gate_diagnostics(&gates)
            .iter()
            .any(|finding| finding.id == super::GateDiagnosticId::FixedResourceIdentifier)
    }

    /// An image tag is a version, not a port: `postgres:16` and `node:20` name
    /// what to run, and gates that start containers are exactly where this
    /// detector looks, so it must not fire on every one of them.
    #[test]
    fn an_image_tag_is_not_a_port() {
        for command in [
            r#"docker run --rm postgres:16 pg_isready"#,
            r#"docker run --rm node:20-alpine npm test"#,
            r#"docker pull ghcr.io/acme/app:1234"#,
        ] {
            assert!(!detect(command), "an image tag is not a port: {command}");
        }
    }

    /// `port=` must end an identifier that names a port, not any word that
    /// happens to end in those letters.
    #[test]
    fn a_word_ending_in_port_is_not_a_port_variable() {
        for command in [r#"SUPPORT=1234 make check"#, r#"make REPORT=2024 summary"#] {
            assert!(!detect(command), "not a port variable: {command}");
        }
    }

    #[test]
    fn common_hardcoded_ports_are_detected() {
        for command in [
            // The two the previous substring check missed entirely.
            r#"npm run dev -- --port 3000"#,
            r#"PORT=8080 npm start"#,
            r#"curl http://localhost:3000/health"#,
            r#"docker run -p 5432:5432 postgres"#,
            // Previously detected; must stay detected.
            r#"PGPORT=5432 psql"#,
            r#"postgres --port 5432"#,
            r#"POSTGRES_PORT=5432 pg_ctl start"#,
        ] {
            assert!(
                detect(command),
                "a hardcoded port must be flagged: {command}"
            );
        }
    }

    /// A port published by the broker must not be reported, even when the same
    /// command names a fixed *container-side* port alongside it: that half
    /// describes the image, not what this host binds.
    #[test]
    fn broker_provided_ports_are_not_flagged() {
        for command in [
            r#"TEST_URL=postgres://localhost:$AETHYME_RESOURCE_PGPORT/db npm test"#,
            r#"PORT=$AETHYME_RESOURCE_PGPORT npm start"#,
            r#"psql --port $AETHYME_RESOURCE_PGPORT"#,
            r#"docker run -p $AETHYME_RESOURCE_PGPORT:5432 postgres"#,
        ] {
            assert!(
                !detect(command),
                "a broker-provided port must not be flagged: {command}"
            );
        }
    }

    /// A non-default port is still a hardcoded port: `--port 15432` collides
    /// across worktrees exactly as `--port 5432` does. The old substring check
    /// did not see it at all, because it only looked for `5432`.
    #[test]
    fn a_non_default_port_is_still_a_hardcoded_port() {
        for command in [
            r#"psql --port 15432"#,
            r#"curl http://localhost:15432/health"#,
        ] {
            assert!(
                detect(command),
                "a non-default hardcoded port is the same defect: {command}"
            );
        }
    }

    /// Text that names digits without configuring a listener must stay clean. A
    /// detector that cries wolf on a healthy gate gets ignored, which is worse
    /// than not shipping it.
    #[test]
    fn text_that_merely_contains_digits_is_not_a_port() {
        for command in [
            // Fixtures and assertions, not listeners.
            r#"test -f fixtures/expected-3000.json"#,
            r#"grep -c ':5432' expected.txt"#,
            // A feature name, not a port.
            r#"cargo test --features port-3000-compat"#,
            // A bare `scheme://` colon, and a ratio in prose.
            r#"curl -sS http://localhost/health"#,
            r#"test $((3000 / 2)) -eq 1500"#,
        ] {
            assert!(
                !detect(command),
                "text naming digits is not a hardcoded port: {command}"
            );
        }
    }

    /// `-p` is deliberately not read as a port flag: `cargo -p`, `docker
    /// publish -p` and `pgrep -p` all use it for something else.
    #[test]
    fn an_overloaded_short_flag_is_not_a_port() {
        for command in [
            r#"cargo test -p aethyme-broker"#,
            r#"cargo publish -p aethyme-cli --dry-run"#,
        ] {
            assert!(
                !detect(command),
                "-p is too overloaded to carry port meaning: {command}"
            );
        }
    }

    /// The identifier forms that are not ports must still be caught, so the
    /// parse did not replace the check but widened it.
    #[test]
    fn non_port_shared_identifiers_are_still_detected() {
        for command in [
            r#"docker compose --project-name fixed-name up"#,
            r#"COMPOSE_PROJECT_NAME=myapp docker compose up"#,
            r#"DATABASE_URL=postgres://localhost/db npm test"#,
            r#"POSTGRES_DB=app docker compose up"#,
        ] {
            assert!(
                detect(command),
                "a fixed shared name must be flagged: {command}"
            );
        }
    }
}

/// Environment identifiers that name a shared resource under concurrency.
///
/// A fixed value here is the same defect as a fixed port: two worktrees resolve
/// the same database, container project, or network and overwrite each other.
/// Matched by shape (`COMPOSE_PROJECT_NAME=`) rather than by enumerating every
/// value, so a spelling that is not listed still reads as the shape it is.
const SHARED_IDENTIFIER_NAMES: &[&str] = &[
    "COMPOSE_PROJECT_NAME",
    "DATABASE_URL",
    "DATABASE_NAME",
    "POSTGRES_DB",
    "POSTGRES_USER",
    "POSTGRES_PASSWORD",
    "PGDATABASE",
    "PGUSER",
    "PGPASSWORD",
    "MYSQL_DATABASE",
    "REDIS_URL",
    "MONGODB_URI",
];

/// Prefixes the broker exports to a gate. Their presence means the repository
/// already resolved this value through a lease, so a literal in the same
/// command is describing something else — the image, the far end of a tunnel,
/// or a value asserted in a fixture.
///
/// Lowercase; compared against a lowercased command.
const BROKER_VALUE_PREFIXES: &[&str] = &[
    "aethyme_resource_",
    "aethyme_test_db_suffix",
    "aethyme_gate_worker_id",
    "aethyme_gate_cache_dir",
    "aethyme_prepare_cache_dir",
];

fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

/// Whether `offset` sits inside a single- or double-quoted run.
///
/// Both quote characters are honoured rather than only the one this repository
/// happens to use, because a gate command is a repository's own text. An
/// unterminated quote runs to the end, which is the conservative reading: it
/// suppresses a finding rather than inventing one.
fn inside_quotes(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    let mut single = false;
    let mut double = false;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            // A backslash escapes the next byte, so `\'` does not close a run.
            b'\\' => index += 1,
            b'\'' if !double => single = !single,
            b'"' if !single => double = !double,
            _ => {}
        }
        index += 1;
    }
    single || double
}

/// Read a leading SCREAMING_SNAKE identifier, if the text starts with one.
fn screaming_snake_identifier(text: &str) -> Option<(&str, usize)> {
    let end = text
        .find(|character: char| !(character.is_ascii_uppercase() || character == '_'))
        .unwrap_or(text.len());
    // A single letter is a flag or a variable, not a configuration key.
    if end < 3 || !text[..end].contains('_') {
        return None;
    }
    Some((&text[..end], end))
}

/// Read a decimal port from the front of `text`.
///
/// The whole digit run is consumed, so `:15432` is read as port 15432 and never
/// as 5432 with a stray prefix — it is a hardcoded port either way, and a
/// non-default one is exactly what collides across worktrees.
fn port_digits(text: &str) -> Option<(u16, usize)> {
    let end = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    if end == 0 {
        return None;
    }
    let value: u16 = text[..end].parse().ok()?;
    Some((value, end))
}

/// Takes the command as written: the variable names it reads are uppercase by
/// convention, so lowercasing first would hide every one of them.
fn fixed_resource_identifier(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    // A command that consumes a broker value has already resolved the shared
    // identifier through a lease, so a remaining literal is the image or the
    // far end rather than something this host binds. Matched without case
    // because the documented spelling is uppercase (`AETHYME_RESOURCE_PGPORT`).
    if BROKER_VALUE_PREFIXES
        .iter()
        .any(|prefix| lower.contains(prefix))
    {
        return false;
    }
    hardcoded_port(command).is_some()
        || fixed_shared_name(command)
        || [
            "database_name=",
            "postgres_db=",
            "compose_project_name=",
            "--project-name ",
        ]
        .iter()
        .any(|token| lower.contains(token))
}

fn shared_writable_cache(command: &str) -> bool {
    [
        "cargo_target_dir=",
        "pip_cache_dir=",
        "npm_config_cache=",
        "gradle_user_home=",
        "--cache-dir ",
        "maven.repo.local=",
    ]
    .iter()
    .any(|token| command.contains(token))
}

fn tied_to_main(command: &str) -> bool {
    [
        "checkout main",
        "switch main",
        "origin/main",
        "refs/heads/main",
        "branch -f main",
        "pull origin main",
    ]
    .iter()
    .any(|token| command.contains(token))
}

fn masks_exit_status(command: &str) -> bool {
    command.contains("|| true")
        || command.contains("; true")
        || command.contains("set +e")
        || command.trim_end().ends_with("exit 0")
}

fn contains_pipeline(command: &str) -> bool {
    command
        .as_bytes()
        .windows(3)
        .any(|window| window[1] == b'|' && window[0] != b'|' && window[2] != b'|')
}

fn equivalence_key(gate: &Gate) -> String {
    let command = gate
        .command
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut triggers = gate.triggers.clone();
    triggers.sort();
    format!("{command}\u{0}{}", triggers.join("\u{0}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_gates;

    struct SilentProgress;

    impl crate::GateProgressSink for SilentProgress {
        fn report(&self, _line: &str) {}
    }

    #[test]
    fn static_diagnostics_are_typed_redacted_and_deterministic() {
        let gates = parse_gates(
            r#"
[[gate]]
name = "service"
command = "SECRET=hidden POSTGRES_PORT=5432 COMPOSE_PROJECT_NAME=app docker compose up | tee result; true"
cost = 3
triggers = ["src/**"]

[[gate]]
name = "service-copy"
command = "SECRET=hidden   POSTGRES_PORT=5432 COMPOSE_PROJECT_NAME=app docker compose up | tee result; true"
cost = 3
triggers = ["src/**"]
"#,
        )
        .unwrap();
        let findings = static_gate_diagnostics(&gates);
        assert_eq!(findings, static_gate_diagnostics(&gates));
        for finding in &findings {
            assert!(!finding.evidence.is_empty());
        }
        let ids = findings
            .iter()
            .map(|finding| finding.id)
            .collect::<Vec<_>>();
        for expected in [
            GateDiagnosticId::MissingTimeout,
            GateDiagnosticId::NoCheapGate,
            GateDiagnosticId::NoFullGate,
            GateDiagnosticId::MissingResourceIsolation,
            GateDiagnosticId::FixedResourceIdentifier,
            GateDiagnosticId::InconsistentFailureClassification,
            GateDiagnosticId::DuplicateGate,
        ] {
            assert!(
                ids.contains(&expected),
                "missing {expected:?}: {findings:#?}"
            );
        }
        let json = serde_json::to_string(&findings).unwrap();
        assert!(!json.contains("SECRET"));
        assert!(!json.contains("hidden"));
    }

    #[test]
    fn safe_worker_and_cache_contracts_avoid_false_shared_resource_findings() {
        let gates = parse_gates(
            r#"
[[gate]]
name = "isolated"
command = "COMPOSE_PROJECT_NAME=$AETHYME_GATE_WORKER_ID docker compose up"
timeout_seconds = 60

[gate.managed_cache]
key = "build"
max_bytes = 1024

[[gate.resources]]
key = "database_port"
kind = "tcp_port"
start = 55000
end = 55999
"#,
        )
        .unwrap();
        let findings = static_gate_diagnostics(&gates);
        assert!(!findings.iter().any(|finding| matches!(
            finding.id,
            GateDiagnosticId::MissingTimeout
                | GateDiagnosticId::MissingResourceIsolation
                | GateDiagnosticId::FixedResourceIdentifier
                | GateDiagnosticId::SharedWritableCache
        )));
    }

    #[test]
    fn exact_head_coverage_finds_stale_broad_and_uncovered_triggers() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::create_dir_all(tmp.path().join("src/api")).unwrap();
        std::fs::create_dir_all(tmp.path().join("web")).unwrap();
        for index in 0..20 {
            std::fs::write(
                tmp.path().join(format!("src/api/module-{index}.rs")),
                "fn source() {}\n",
            )
            .unwrap();
        }
        for (path, contents) in [
            ("web/app.ts", "export const app = 1;\n"),
            ("README.md", "readme\n"),
        ] {
            std::fs::write(tmp.path().join(path), contents).unwrap();
        }
        std::fs::write(
            tmp.path().join(crate::GATES_CONFIG_RELPATH),
            "[[gate]]\nname='broad'\ncommand='true'\ncost=3\ntimeout_seconds=60\ntriggers=['src/**', 'missing/**']\n",
        )
        .unwrap();
        let status = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());

        let report = inspect_gate_quality(&crate::GitRepo::discover(tmp.path()).unwrap()).unwrap();
        assert_eq!(report.source_file_count, 21);
        assert_eq!(report.gates[0].matched_source_paths, 20);
        assert!(
            report
                .findings
                .iter()
                .any(|finding| { finding.id == GateDiagnosticId::TriggerMatchesNoTrackedPath })
        );
        assert!(report.findings.iter().any(|finding| {
            finding.id == GateDiagnosticId::UncoveredSourceArea && finding.summary.contains("web")
        }));
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.id == GateDiagnosticId::ExpensiveBroadTrigger)
        );
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains(tmp.path().to_string_lossy().as_ref())
        );
    }

    #[test]
    fn invalid_timeout_is_advisory_for_doctor_but_invalid_for_execution() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::write(
            tmp.path().join(crate::GATES_CONFIG_RELPATH),
            "[[gate]]\nname='bad'\ncommand='true'\ntimeout_seconds=0\n",
        )
        .unwrap();
        let status = std::process::Command::new("git")
            .args(["add", "-A"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let status = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let repo = crate::GitRepo::discover(tmp.path()).unwrap();
        let report = inspect_gate_quality(&repo).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|finding| finding.id == GateDiagnosticId::InvalidTimeout)
        );
        assert!(matches!(
            crate::load_gates(tmp.path()),
            Err(crate::GateConfigError::BadTimeout { .. })
        ));
    }

    /// #170: the fallback key is now correct, but it is still not the
    /// cross-clone identity a declared pool reads as -- and it rotates if
    /// the checkout moves. Raised only for a gate that stakes something on
    /// it, so a repository with no pools and no caches stays quiet.
    #[test]
    fn an_originless_key_is_reported_only_for_gates_that_stake_something_on_it() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::write(
            tmp.path().join(crate::GATES_CONFIG_RELPATH),
            concat!(
                "[[gate]]\nname='plain'\ncommand='true'\ntimeout_seconds=30\n\n",
                "[[gate]]\nname='pooled'\ncommand='true'\ntimeout_seconds=30\nresource_ttl_seconds=30\n",
                "[[gate.resources]]\nkey='slot'\nkind='exclusive_key'\nname='aethyme-unit-170'\n",
            ),
        )
        .unwrap();
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["add", "-A"],
            vec![
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .status()
                .unwrap();
            assert!(status.success());
        }

        let repo = crate::GitRepo::discover(tmp.path()).unwrap();
        let flagged = |report: &GateDoctorReport| {
            report
                .findings
                .iter()
                .filter(|f| f.id == GateDiagnosticId::OriginlessCoordinationKey)
                .map(|f| f.gate.clone().unwrap())
                .collect::<Vec<_>>()
        };

        // No origin: only the gate with something to lose is named.
        assert_eq!(flagged(&inspect_gate_quality(&repo).unwrap()), ["pooled"]);

        // With an origin the key is the remote's, and the notice goes away.
        let status = std::process::Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://example.invalid/org/app.git",
            ])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        assert!(flagged(&inspect_gate_quality(&repo).unwrap()).is_empty());
    }

    #[test]
    fn probe_is_disposable_detects_all_mutation_classes_and_skips_normal_cache() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::fs::create_dir_all(tmp.path().join(".aethyme")).unwrap();
        std::fs::write(
            tmp.path().join(".gitignore"),
            "ignored.txt\n.aethyme/broker.db*\n.aethyme/logs/\n.aethyme/run/\n",
        )
        .unwrap();
        std::fs::write(tmp.path().join("tracked.txt"), "before\n").unwrap();
        std::fs::write(
            tmp.path().join(crate::GATES_CONFIG_RELPATH),
            "[[gate]]\nname='mutator'\ncommand='printf after > tracked.txt; touch untracked.txt ignored.txt'\ntimeout_seconds=30\nresource_ttl_seconds=30\n\n[[gate.resources]]\nkey='probe_slot'\nkind='exclusive_key'\nname='aethyme-gate-doctor-unit-probe'\n",
        )
        .unwrap();
        for args in [
            vec!["add", "-A"],
            vec![
                "-c",
                "user.name=test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(tmp.path())
                .status()
                .unwrap();
            assert!(status.success());
        }
        let repo = crate::GitRepo::discover(tmp.path()).unwrap();
        let report = probe_gate_quality(&repo, None, &SilentProgress).unwrap();
        let probe = report.probe.unwrap();
        assert!(probe.passed);
        assert!(probe.outcomes[0].acquired_declared_resources);
        assert!(probe.worktree.clean_before);
        assert_eq!(probe.worktree.result_cache, "ephemeral_probe_only");
        assert_eq!(probe.mutations.tracked, ["tracked.txt"]);
        assert_eq!(probe.mutations.untracked, ["untracked.txt"]);
        assert_eq!(probe.mutations.ignored, ["ignored.txt"]);
        assert!(repo.worktree_paths().unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("tracked.txt")).unwrap(),
            "before\n"
        );

        let tree = repo.working_tree_hash().unwrap();
        let store = crate::BrokerStore::open_in_repo(tmp.path()).unwrap();
        assert!(
            store
                .cached_gate_result("mutator", &tree)
                .unwrap()
                .is_none()
        );
    }

    /// A placement standing in for one the environment produced.
    fn placement(
        directory: &str,
        inside_repository: bool,
        preferred: Option<&str>,
        refusals: &[&str],
    ) -> crate::verification::SlotPlacement {
        crate::verification::SlotPlacement {
            directory: directory.into(),
            inside_repository,
            preferred: preferred.map(Into::into),
            refusals: refusals.iter().map(|refusal| (*refusal).into()).collect(),
        }
    }

    #[test]
    fn a_slot_outside_the_repository_is_not_reported() {
        assert!(
            nested_verification_slot_finding(&placement(
                "/host/run/repo-abc/merge-sim",
                false,
                None,
                &[],
            ))
            .is_none()
        );
    }

    #[test]
    fn a_slot_inside_the_repository_names_the_hazard_and_what_was_refused() {
        let found = nested_verification_slot_finding(&placement(
            "/repo/.aethyme/run/merge-sim",
            true,
            Some("/host/run/repo-abc/merge-sim"),
            &["/host/run/repo-abc/merge-sim: Operation not permitted (os error 1)"],
        ))
        .expect("a nested slot is reported");
        assert_eq!(found.id, GateDiagnosticId::NestedVerificationSlot);
        assert_eq!(found.severity, GateDiagnosticSeverity::Warning);
        assert_eq!(
            found.summary,
            "verification slot is nested inside the repository"
        );
        let evidence = found.evidence.join("\n");
        assert!(
            evidence.contains("placed  /repo/.aethyme/run/merge-sim (fallback)"),
            "{evidence}"
        );
        assert!(
            evidence.contains("wanted  /host/run/repo-abc/merge-sim"),
            "{evidence}"
        );
        assert!(
            evidence.contains("because /host/run/repo-abc/merge-sim: Operation not permitted"),
            "{evidence}"
        );
        // The hazard is the point of the finding: without it a reader sees an
        // unusual path and no reason to care about it.
        assert!(
            found
                .evidence
                .iter()
                .any(|line| line.starts_with("risk") && line.contains("enclosing checkout")),
            "{evidence}"
        );
        assert!(found.remediation.contains("AETHYME_HOST_STATE_DIR"));
    }
}
