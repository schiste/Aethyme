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
    NoCheapGate,
    NoFullGate,
    TriggerMatchesNoTrackedPath,
    ExpensiveBroadTrigger,
    UncoveredSourceArea,
    MissingResourceIsolation,
    FixedResourceIdentifier,
    SharedWritableCache,
    MainCheckoutAssumption,
    MissingFailureClassificationEvidence,
    InconsistentFailureClassification,
    DuplicateGate,
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
        if fixed_resource_identifier(&lower) {
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
    let output = std::process::Command::new("git")
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

fn fixed_resource_identifier(command: &str) -> bool {
    if command.contains("aethyme_gate_worker_id")
        || command.contains("aethyme_test_db_suffix")
        || command.contains("aethyme_resource_")
    {
        return false;
    }
    [
        "postgres_port=",
        "database_name=",
        "postgres_db=",
        "compose_project_name=",
        "--project-name ",
        ":5432",
        "=5432",
    ]
    .iter()
    .any(|token| command.contains(token))
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
}
