//! Typed repository-readiness model.
//!
//! Classification is pure: inspection gathers facts separately, then this
//! module deterministically projects them into a stable report.

use std::io::ErrorKind;
use std::path::Path;

use aethyme_engine::store::redb::graph_store::GraphStore;
use aethyme_graph_storage::{GraphAuthorityManifest, read_engine_version};

use crate::init::{CertificationFacts, CheckStatus, certification_facts};
use crate::store::BrokerStore;
use crate::{
    BROKER_DB_RELPATH, CANONICAL_REPOSITORY_MARKER_PATH, GraphAuthority, GraphIntegrityPolicy,
    HostResourceCoordinator, LOCAL_REPOSITORY_MARKER_PATH, REPOSITORY_SCHEMA_VERSION,
    RepositoryContract, default_host_resource_db_path, load_gates,
};

pub const READINESS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessState {
    Ready,
    Limited,
    NotReady,
    Unknown,
    NotApplicable,
}

impl ReadinessState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Limited => "limited",
            Self::NotReady => "not_ready",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not_applicable",
        }
    }

    fn human_label(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Limited => "limited",
            Self::NotReady => "not ready",
            Self::Unknown => "unknown",
            Self::NotApplicable => "not applicable",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryOperatingMode {
    Undeployed,
    ConflictOnly,
    AgentReady,
    ParallelReady,
    Invalid,
}

impl RepositoryOperatingMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Undeployed => "undeployed",
            Self::ConflictOnly => "conflict_only",
            Self::AgentReady => "agent_ready",
            Self::ParallelReady => "parallel_ready",
            Self::Invalid => "invalid",
        }
    }

    pub fn parse_requirement(value: &str) -> Option<Self> {
        match value {
            "conflict-only" | "conflict_only" => Some(Self::ConflictOnly),
            "agent-ready" | "agent_ready" => Some(Self::AgentReady),
            "parallel-ready" | "parallel_ready" => Some(Self::ParallelReady),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RepositoryReadinessMode {
    Canonical,
    LocalOnly,
    Absent,
}

impl RepositoryReadinessMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Canonical => "canonical",
            Self::LocalOnly => "local_only",
            Self::Absent => "absent",
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize, serde::Serialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ReadinessDimensionId {
    RepositoryDeployment,
    Coordination,
    AgentContext,
    Validation,
    ParallelExecution,
    GraphAvailability,
    UpgradeCompatibility,
}

impl ReadinessDimensionId {
    pub const ORDERED: [Self; 7] = [
        Self::RepositoryDeployment,
        Self::Coordination,
        Self::AgentContext,
        Self::Validation,
        Self::ParallelExecution,
        Self::GraphAvailability,
        Self::UpgradeCompatibility,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RepositoryDeployment => "repository-deployment",
            Self::Coordination => "coordination",
            Self::AgentContext => "agent-context",
            Self::Validation => "validation",
            Self::ParallelExecution => "parallel-execution",
            Self::GraphAvailability => "graph-availability",
            Self::UpgradeCompatibility => "upgrade-compatibility",
        }
    }

    fn human_label(self) -> &'static str {
        match self {
            Self::RepositoryDeployment => "Repository deployment",
            Self::Coordination => "Coordination",
            Self::AgentContext => "Agent context",
            Self::Validation => "Validation",
            Self::ParallelExecution => "Parallel execution",
            Self::GraphAvailability => "Graph",
            Self::UpgradeCompatibility => "Upgrade compatibility",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ReadinessEvidence {
    pub id: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ReadinessAction {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ReadinessDimension {
    pub id: ReadinessDimensionId,
    pub state: ReadinessState,
    pub summary: String,
    pub evidence: Vec<ReadinessEvidence>,
    pub remediation: Vec<ReadinessAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ReadinessFinding {
    pub dimension: ReadinessDimensionId,
    pub state: ReadinessState,
    pub summary: String,
    pub remediation: Vec<ReadinessAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ReadinessReport {
    pub schema_version: u32,
    pub repository_mode: RepositoryReadinessMode,
    pub operating_mode: RepositoryOperatingMode,
    pub source_head: Option<String>,
    pub deployment_digest: Option<String>,
    pub dimensions: Vec<ReadinessDimension>,
    pub blockers: Vec<ReadinessFinding>,
    pub warnings: Vec<ReadinessFinding>,
}

impl ReadinessReport {
    pub fn from_dimensions(
        repository_mode: RepositoryReadinessMode,
        source_head: Option<String>,
        deployment_digest: Option<String>,
        mut dimensions: Vec<ReadinessDimension>,
    ) -> Self {
        dimensions.sort_by_key(|dimension| dimension.id);
        let blockers = dimensions
            .iter()
            .filter(|dimension| dimension.state == ReadinessState::NotReady)
            .map(ReadinessFinding::from)
            .collect::<Vec<_>>();
        let warnings = dimensions
            .iter()
            .filter(|dimension| {
                matches!(
                    dimension.state,
                    ReadinessState::Limited | ReadinessState::Unknown
                )
            })
            .map(ReadinessFinding::from)
            .collect::<Vec<_>>();
        let operating_mode = classify_operating_mode(repository_mode, &dimensions);
        Self {
            schema_version: READINESS_SCHEMA_VERSION,
            repository_mode,
            operating_mode,
            source_head,
            deployment_digest,
            dimensions,
            blockers,
            warnings,
        }
    }

    pub fn meets(&self, required: RepositoryOperatingMode) -> bool {
        operating_mode_rank(self.operating_mode) >= operating_mode_rank(required)
    }
}

/// Render the stable, agent-facing readiness summary shared by the standalone
/// inspector and setup commands. Detailed evidence remains available in JSON.
pub fn render_readiness_text(report: &ReadinessReport) -> String {
    let mut lines = vec![format!(
        "Operating mode: {}",
        report.operating_mode.as_str()
    )];
    for id in [
        ReadinessDimensionId::Coordination,
        ReadinessDimensionId::AgentContext,
        ReadinessDimensionId::Validation,
        ReadinessDimensionId::ParallelExecution,
    ] {
        if let Some(dimension) = report
            .dimensions
            .iter()
            .find(|dimension| dimension.id == id)
        {
            lines.push(format!(
                "{}: {}",
                id.human_label(),
                dimension.state.human_label()
            ));
        }
    }

    if let Some(graph) = report
        .dimensions
        .iter()
        .find(|dimension| dimension.id == ReadinessDimensionId::GraphAvailability)
    {
        if graph.state == ReadinessState::NotApplicable {
            lines.push("Graph: disabled by repository policy; no action required.".into());
        } else {
            lines.push(format!(
                "Graph: {} — {}",
                graph.state.human_label(),
                graph.summary
            ));
        }
    }

    let mandatory = [
        ReadinessDimensionId::Coordination,
        ReadinessDimensionId::AgentContext,
        ReadinessDimensionId::Validation,
        ReadinessDimensionId::ParallelExecution,
    ];
    if mandatory.iter().all(|id| {
        report
            .dimensions
            .iter()
            .find(|dimension| dimension.id == *id)
            .is_some_and(|dimension| dimension.state == ReadinessState::Ready)
    }) {
        lines.push(String::new());
        lines.push("All mandatory agent-readiness dimensions are ready.".into());
    }

    let mut actions = Vec::<String>::new();
    for dimension in &report.dimensions {
        if matches!(
            dimension.state,
            ReadinessState::Ready | ReadinessState::NotApplicable
        ) {
            continue;
        }
        for remediation in &dimension.remediation {
            let rendered = match &remediation.command {
                Some(command) => format!("{}: `{command}`", remediation.summary),
                None => remediation.summary.clone(),
            };
            if !actions.contains(&rendered) {
                actions.push(rendered);
            }
        }
    }
    if !actions.is_empty() {
        lines.push(String::new());
        lines.push("Next actions:".into());
        lines.extend(actions.into_iter().map(|action| format!("- {action}")));
    }
    lines.push(String::new());
    lines.join("\n")
}

pub fn render_readiness_json(report: &ReadinessReport) -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(report)
}

/// Inspect repository and coordination readiness without creating or
/// refreshing repository, broker, lease, graph, or host state.
pub fn inspect_repository_readiness(repo_hint: &Path) -> ReadinessReport {
    let facts = match certification_facts(repo_hint) {
        Ok(facts) => facts,
        Err(error) => {
            return undeployed_report(format!("repository facts could not be inspected: {error}"));
        }
    };
    let Some(repository) = facts.repository.as_ref() else {
        return undeployed_report(
            "no Git repository was found; readiness can only report undeployed state".into(),
        );
    };
    let root = repository.checkout_root();
    let source_head = repository.source_head().map(str::to_owned);
    let canonical_marker = root.join(CANONICAL_REPOSITORY_MARKER_PATH).is_file();
    let local_marker = root.join(LOCAL_REPOSITORY_MARKER_PATH).is_file()
        || root
            .join(aethyme_enhance::local::LOCAL_MARKER_PATH)
            .is_file();
    let repository_mode = match (canonical_marker, local_marker) {
        (true, false) => RepositoryReadinessMode::Canonical,
        (false, true) => RepositoryReadinessMode::LocalOnly,
        _ => RepositoryReadinessMode::Absent,
    };
    let contract = if canonical_marker ^ local_marker {
        RepositoryContract::capture(root, false).ok()
    } else {
        None
    };
    let deployment_digest = contract
        .as_ref()
        .map(|contract| contract.deployment_state_digest.clone());
    let broker = inspect_broker_state(repository.main_root());
    let gates = inspect_gates(root);
    let preparation = inspect_preparation(root);
    let dimensions = vec![
        deployment_dimension(
            &facts,
            repository_mode,
            canonical_marker,
            local_marker,
            &contract,
        ),
        coordination_dimension(&broker),
        agent_context_dimension(&facts, root),
        validation_dimension(&gates),
        parallel_dimension(&broker, &gates, &preparation),
        graph_dimension(root, source_head.as_deref()),
        upgrade_dimension(repository_mode, &contract),
    ];
    ReadinessReport::from_dimensions(repository_mode, source_head, deployment_digest, dimensions)
}

#[derive(Debug)]
enum BrokerStateInspection {
    Missing,
    Ready { live_sessions: usize },
    Invalid(String),
    Inaccessible(String),
}

#[derive(Debug)]
enum GateInspection {
    Missing,
    Draft { total: usize },
    Ready { total: usize, cheap: usize },
    Invalid(String),
}

#[derive(Debug)]
enum PreparationInspection {
    Missing,
    Ready { steps: usize, hook_steps: usize },
    Invalid(String),
}

fn inspect_broker_state(main_root: &Path) -> BrokerStateInspection {
    let path = main_root.join(BROKER_DB_RELPATH);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound => BrokerStateInspection::Missing,
        Err(error) => BrokerStateInspection::Inaccessible(error.to_string()),
        Ok(metadata) if !metadata.is_file() => {
            BrokerStateInspection::Invalid("broker state path is not a regular file".into())
        }
        Ok(_) => match BrokerStore::open_current_read_only_in_repo(main_root) {
            Ok(store) => match store.integrity_check() {
                Ok(integrity) if integrity == "ok" => match store.live_sessions() {
                    Ok(sessions) => BrokerStateInspection::Ready {
                        live_sessions: sessions.len(),
                    },
                    Err(error) => BrokerStateInspection::Invalid(error.to_string()),
                },
                Ok(integrity) => BrokerStateInspection::Invalid(format!(
                    "broker database integrity check returned {integrity:?}"
                )),
                Err(error) => BrokerStateInspection::Invalid(error.to_string()),
            },
            Err(error) => BrokerStateInspection::Inaccessible(error.to_string()),
        },
    }
}

fn inspect_gates(root: &Path) -> GateInspection {
    let path = root.join(crate::gates::GATES_CONFIG_RELPATH);
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == ErrorKind::NotFound => GateInspection::Missing,
        Err(error) => GateInspection::Invalid(error.to_string()),
        Ok(metadata) if !metadata.is_file() => {
            GateInspection::Invalid("gate configuration is not a regular file".into())
        }
        Ok(_) => match load_gates(root) {
            Ok(gates) => {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                let explicitly_unreviewed = text
                    .parse::<toml::Value>()
                    .ok()
                    .and_then(|value| value.get("reviewed").and_then(toml::Value::as_bool))
                    == Some(false);
                let legacy_draft = text.contains("# Draft generated by `aethyme init`")
                    && !text.contains("reviewed = true");
                if explicitly_unreviewed || legacy_draft {
                    GateInspection::Draft { total: gates.len() }
                } else {
                    GateInspection::Ready {
                        total: gates.len(),
                        cheap: gates.iter().filter(|gate| gate.cost <= 1).count(),
                    }
                }
            }
            Err(error) => GateInspection::Invalid(error.to_string()),
        },
    }
}

fn inspect_preparation(root: &Path) -> PreparationInspection {
    match crate::preparation::load_config(root) {
        Ok(None) => PreparationInspection::Missing,
        Ok(Some(config)) => PreparationInspection::Ready {
            steps: config.steps.len(),
            hook_steps: config
                .steps
                .iter()
                .filter(|step| step.required_for_hooks)
                .count(),
        },
        Err(error) => PreparationInspection::Invalid(error.to_string()),
    }
}

fn deployment_dimension(
    facts: &CertificationFacts,
    repository_mode: RepositoryReadinessMode,
    canonical_marker: bool,
    local_marker: bool,
    contract: &Option<RepositoryContract>,
) -> ReadinessDimension {
    if canonical_marker && local_marker {
        return dimension_with(
            ReadinessDimensionId::RepositoryDeployment,
            ReadinessState::NotReady,
            "canonical and local-only deployment markers are both present",
            vec![evidence(
                "deployment-markers",
                "the deployment mode is ambiguous",
            )],
            vec![action(
                "Review the deployment and retain exactly one mode",
                Some("aethyme upgrade plan --repo . --diff"),
            )],
        );
    }
    if repository_mode == RepositoryReadinessMode::Absent {
        return dimension_with(
            ReadinessDimensionId::RepositoryDeployment,
            ReadinessState::NotReady,
            "Aethyme is not deployed in this repository",
            Vec::new(),
            vec![action(
                "Deploy the generated agent protocol",
                Some("aethyme deploy --repo ."),
            )],
        );
    }
    let config = check(facts, "certify.config");
    match (contract, config.map(|check| check.status)) {
        (Some(_), Some(CheckStatus::Pass)) => dimension_with(
            ReadinessDimensionId::RepositoryDeployment,
            ReadinessState::Ready,
            "the repository deployment marker and configuration are valid",
            vec![evidence(
                "deployment-mode",
                match repository_mode {
                    RepositoryReadinessMode::Canonical => "canonical deployment",
                    RepositoryReadinessMode::LocalOnly => "local-only deployment",
                    RepositoryReadinessMode::Absent => unreachable!(),
                },
            )],
            Vec::new(),
        ),
        _ => dimension_with(
            ReadinessDimensionId::RepositoryDeployment,
            ReadinessState::NotReady,
            "the repository deployment is incomplete or invalid",
            config
                .map(|check| evidence(check.id, check.detail.clone()))
                .into_iter()
                .collect(),
            vec![action(
                "Inspect the exact migration required",
                Some("aethyme upgrade plan --repo . --diff"),
            )],
        ),
    }
}

fn coordination_dimension(state: &BrokerStateInspection) -> ReadinessDimension {
    match state {
        BrokerStateInspection::Missing => dimension_with(
            ReadinessDimensionId::Coordination,
            ReadinessState::Limited,
            "broker state is absent; no sessions have been coordinated yet",
            vec![evidence(
                "broker-state",
                "readiness did not create missing broker state",
            )],
            vec![action(
                "Create the first isolated session when work begins",
                Some("aethyme broker start --task \"<task>\""),
            )],
        ),
        BrokerStateInspection::Ready { live_sessions } => dimension_with(
            ReadinessDimensionId::Coordination,
            ReadinessState::Ready,
            "broker storage is readable and healthy",
            vec![evidence(
                "live-sessions",
                format!("{live_sessions} live session(s) observed without refreshing them"),
            )],
            Vec::new(),
        ),
        BrokerStateInspection::Invalid(reason) => dimension_with(
            ReadinessDimensionId::Coordination,
            ReadinessState::NotReady,
            "broker state is invalid",
            vec![evidence("broker-state", reason.clone())],
            vec![action(
                "Inspect broker recovery options",
                Some("aethyme broker doctor"),
            )],
        ),
        BrokerStateInspection::Inaccessible(reason) => dimension_with(
            ReadinessDimensionId::Coordination,
            ReadinessState::Unknown,
            "broker state could not be inspected without mutation",
            vec![evidence("broker-state", reason.clone())],
            vec![action(
                "Grant read access and rerun readiness",
                Some("aethyme broker readiness"),
            )],
        ),
    }
}

fn agent_context_dimension(facts: &CertificationFacts, root: &Path) -> ReadinessDimension {
    let protocol = check(facts, "certify.agents-protocol");
    let protocol_ready = protocol.is_some_and(|check| check.status == CheckStatus::Pass);
    let onboarding = match aethyme_enhance::onboarding::expected_onboarding_files(root) {
        Ok(expected) => {
            let mut missing = 0;
            let mut stale = 0;
            for (relative, content) in expected {
                match std::fs::read(root.join(relative)) {
                    Ok(actual) if actual == content.as_bytes() => {}
                    Ok(_) => stale += 1,
                    Err(error) if error.kind() == ErrorKind::NotFound => missing += 1,
                    Err(_) => stale += 1,
                }
            }
            if missing > 0 {
                (
                    ReadinessState::NotReady,
                    format!("{missing} onboarding artifact(s) are missing"),
                )
            } else if stale > 0 {
                (
                    ReadinessState::Limited,
                    format!("{stale} onboarding artifact(s) are stale"),
                )
            } else {
                (
                    ReadinessState::Ready,
                    "onboarding artifacts match tracked inputs".into(),
                )
            }
        }
        Err(error) => (
            ReadinessState::Unknown,
            format!("onboarding could not be derived: {error}"),
        ),
    };
    let state = if !protocol_ready {
        ReadinessState::NotReady
    } else {
        onboarding.0
    };
    let mut evidence_items = vec![evidence("onboarding", onboarding.1)];
    if let Some(protocol) = protocol {
        evidence_items.push(evidence(protocol.id, protocol.detail.clone()));
    }
    dimension_with(
        ReadinessDimensionId::AgentContext,
        state,
        match state {
            ReadinessState::Ready => "agent protocol and generated context are current",
            ReadinessState::Limited => "agent context exists but is stale",
            ReadinessState::NotReady => "agent protocol or generated context is missing",
            ReadinessState::Unknown => "agent context could not be fully inspected",
            ReadinessState::NotApplicable => unreachable!(),
        },
        evidence_items,
        if state == ReadinessState::Ready {
            Vec::new()
        } else {
            vec![action(
                "Regenerate agent-facing context after reviewing the deployment",
                Some("aethyme enhance deploy --repo ."),
            )]
        },
    )
}

fn validation_dimension(gates: &GateInspection) -> ReadinessDimension {
    match gates {
        GateInspection::Missing => dimension_with(
            ReadinessDimensionId::Validation,
            ReadinessState::Limited,
            "no validation gates are configured; operation is conflict-only",
            Vec::new(),
            vec![action(
                "Draft repository validation gates for review",
                Some("aethyme broker gates draft"),
            )],
        ),
        GateInspection::Draft { total } => dimension_with(
            ReadinessDimensionId::Validation,
            ReadinessState::Limited,
            "validation gates are an unreviewed generated draft",
            vec![evidence(
                "gate-count",
                format!("{total} drafted gate(s); reviewed = false"),
            )],
            vec![action(
                "Review every generated command and trigger, then set reviewed = true",
                None,
            )],
        ),
        GateInspection::Ready { total, cheap } => dimension_with(
            ReadinessDimensionId::Validation,
            if *cheap > 0 {
                ReadinessState::Ready
            } else {
                ReadinessState::Limited
            },
            if *cheap > 0 {
                "validation gates include a cheap feedback lane"
            } else {
                "validation gates exist but no cheap feedback lane was identified"
            },
            vec![
                evidence("gate-count", format!("{total} configured gate(s)")),
                evidence("cheap-gate-count", format!("{cheap} cheap gate(s)")),
            ],
            if *cheap > 0 {
                Vec::new()
            } else {
                vec![action("Add a low-cost gate for fast agent feedback", None)]
            },
        ),
        GateInspection::Invalid(reason) => dimension_with(
            ReadinessDimensionId::Validation,
            ReadinessState::NotReady,
            "validation gate configuration is invalid",
            vec![evidence("gate-configuration", reason.clone())],
            vec![action("Repair .aethyme/gates.toml", None)],
        ),
    }
}

fn parallel_dimension(
    broker: &BrokerStateInspection,
    gates: &GateInspection,
    preparation: &PreparationInspection,
) -> ReadinessDimension {
    if !matches!(broker, BrokerStateInspection::Ready { .. }) {
        return dimension_with(
            ReadinessDimensionId::ParallelExecution,
            match broker {
                BrokerStateInspection::Inaccessible(_) => ReadinessState::Unknown,
                BrokerStateInspection::Invalid(_) => ReadinessState::NotReady,
                BrokerStateInspection::Missing | BrokerStateInspection::Ready { .. } => {
                    ReadinessState::Limited
                }
            },
            "parallel execution cannot be fully established without healthy broker state",
            Vec::new(),
            Vec::new(),
        );
    }
    if !matches!(gates, GateInspection::Ready { .. }) {
        return dimension_with(
            ReadinessDimensionId::ParallelExecution,
            ReadinessState::NotReady,
            "parallel validation requires valid gate definitions",
            Vec::new(),
            Vec::new(),
        );
    }
    match preparation {
        PreparationInspection::Missing => {
            return dimension_with(
                ReadinessDimensionId::ParallelExecution,
                ReadinessState::NotReady,
                "fresh worktrees are not proven ready to execute repository gates",
                vec![evidence(
                    "dependency-preparation",
                    "validation gates exist but .aethyme/prepare.toml is not declared",
                )],
                vec![action(
                    "Declare reproducible dependency preparation for isolated worktrees",
                    Some("aethyme broker prepare status --session <id>"),
                )],
            );
        }
        PreparationInspection::Invalid(reason) => {
            return dimension_with(
                ReadinessDimensionId::ParallelExecution,
                ReadinessState::NotReady,
                "dependency preparation policy is invalid",
                vec![evidence("dependency-preparation", reason.clone())],
                vec![action("Repair .aethyme/prepare.toml", None)],
            );
        }
        PreparationInspection::Ready { .. } => {}
    }
    let host_path = match default_host_resource_db_path() {
        Ok(path) => path,
        Err(error) => {
            return dimension_with(
                ReadinessDimensionId::ParallelExecution,
                ReadinessState::Unknown,
                "host resource coordination location is unavailable",
                vec![evidence("host-state", error.to_string())],
                Vec::new(),
            );
        }
    };
    match std::fs::symlink_metadata(&host_path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let PreparationInspection::Ready { steps, hook_steps } = preparation else {
                unreachable!()
            };
            dimension_with(
                ReadinessDimensionId::ParallelExecution,
                ReadinessState::Ready,
                "isolated worktree preparation and lazy host coordination are available",
                vec![
                    evidence(
                        "dependency-preparation",
                        format!("{steps} declared step(s); {hook_steps} required by hooks"),
                    ),
                    evidence(
                        "host-state",
                        "the first resource-aware gate run will initialize host coordination",
                    ),
                ],
                Vec::new(),
            )
        }
        Err(error) => dimension_with(
            ReadinessDimensionId::ParallelExecution,
            ReadinessState::Unknown,
            "host resource state is inaccessible",
            vec![evidence("host-state", error.to_string())],
            Vec::new(),
        ),
        Ok(metadata) if !metadata.is_file() => dimension_with(
            ReadinessDimensionId::ParallelExecution,
            ReadinessState::NotReady,
            "host resource state is not a regular file",
            Vec::new(),
            Vec::new(),
        ),
        Ok(_) => match HostResourceCoordinator::open_read_only(&host_path) {
            Ok(_) => {
                let PreparationInspection::Ready { steps, hook_steps } = preparation else {
                    unreachable!()
                };
                dimension_with(
                    ReadinessDimensionId::ParallelExecution,
                    ReadinessState::Ready,
                    "isolated worktree preparation and host resource coordination are available",
                    vec![evidence(
                        "dependency-preparation",
                        format!("{steps} declared step(s); {hook_steps} required by hooks"),
                    )],
                    Vec::new(),
                )
            }
            Err(error) => dimension_with(
                ReadinessDimensionId::ParallelExecution,
                ReadinessState::Unknown,
                "host resource state could not be opened read-only",
                vec![evidence("host-state", error.to_string())],
                Vec::new(),
            ),
        },
    }
}

fn graph_dimension(root: &Path, source_head: Option<&str>) -> ReadinessDimension {
    let policy = match GraphIntegrityPolicy::load(root) {
        Ok(policy) => policy,
        Err(error) => {
            return dimension_with(
                ReadinessDimensionId::GraphAvailability,
                ReadinessState::NotReady,
                "graph policy is invalid",
                vec![evidence("graph-policy", error.to_string())],
                Vec::new(),
            );
        }
    };
    if policy.authority == GraphAuthority::Disabled {
        return dimension_with(
            ReadinessDimensionId::GraphAvailability,
            ReadinessState::NotApplicable,
            "graphing is disabled; repository readiness does not require it",
            Vec::new(),
            vec![action(
                "Enroll voluntarily when graph performance is acceptable",
                Some("aethyme deploy --repo . --with-graph"),
            )],
        );
    }
    let Some(head) = source_head else {
        return dimension_with(
            ReadinessDimensionId::GraphAvailability,
            ReadinessState::Unknown,
            "graph freshness cannot be established without a source HEAD",
            Vec::new(),
            Vec::new(),
        );
    };
    let repository = policy.repository.as_deref().unwrap_or("unconfigured");
    let pinned = match read_engine_version(root) {
        Ok(version) => version,
        Err(error) => {
            return stale_graph(format!("engine version pin is unavailable: {error}"));
        }
    };
    let manifest = match std::fs::read(root.join(aethyme_graph_storage::GRAPH_MANIFEST_RELPATH))
        .map_err(|error| error.to_string())
        .and_then(|bytes| GraphAuthorityManifest::decode(&bytes).map_err(|error| error.to_string()))
    {
        Ok(manifest) => manifest,
        Err(error) => return stale_graph(format!("graph manifest is unavailable: {error}")),
    };
    let expected = match GraphAuthorityManifest::build(root, head, repository, &pinned) {
        Ok(expected) => expected,
        Err(error) => {
            return stale_graph(format!("graph fragments could not be verified: {error}"));
        }
    };
    if manifest != expected || pinned != env!("CARGO_PKG_VERSION") {
        return stale_graph(
            "graph manifest, fragments, source tree, or engine version is stale".into(),
        );
    }
    match GraphStore::open_read_only(root).and_then(|store| store.repo_metadata()) {
        Ok(Some(metadata)) if metadata.commit_hash.as_deref() == Some(head) => dimension_with(
            ReadinessDimensionId::GraphAvailability,
            ReadinessState::Ready,
            "verified graph fragments and local query store match source HEAD",
            vec![evidence("graph-source-head", head.to_string())],
            Vec::new(),
        ),
        Ok(_) => stale_graph("the local graph query store is missing or stale".into()),
        Err(error) => stale_graph(format!(
            "the local graph query store is unavailable: {error}"
        )),
    }
}

fn stale_graph(reason: String) -> ReadinessDimension {
    dimension_with(
        ReadinessDimensionId::GraphAvailability,
        ReadinessState::Limited,
        "graphing is enabled but not currently available",
        vec![evidence("graph-status", reason)],
        vec![action(
            "Review graph refresh and materialization",
            Some("aethyme graph status --repo ."),
        )],
    )
}

fn upgrade_dimension(
    repository_mode: RepositoryReadinessMode,
    contract: &Option<RepositoryContract>,
) -> ReadinessDimension {
    if repository_mode == RepositoryReadinessMode::Absent {
        return dimension_with(
            ReadinessDimensionId::UpgradeCompatibility,
            ReadinessState::NotApplicable,
            "no deployed repository contract exists",
            Vec::new(),
            Vec::new(),
        );
    }
    match contract
        .as_ref()
        .and_then(|contract| contract.repository_schema)
    {
        Some(schema) if schema == REPOSITORY_SCHEMA_VERSION => dimension_with(
            ReadinessDimensionId::UpgradeCompatibility,
            ReadinessState::Ready,
            "repository deployment schema matches this binary",
            vec![evidence("repository-schema", format!("schema {schema}"))],
            Vec::new(),
        ),
        Some(schema) if schema < REPOSITORY_SCHEMA_VERSION => dimension_with(
            ReadinessDimensionId::UpgradeCompatibility,
            ReadinessState::NotReady,
            "repository deployment must be upgraded before new agent work",
            vec![evidence(
                "repository-schema",
                format!("schema {schema}; binary supports {REPOSITORY_SCHEMA_VERSION}"),
            )],
            vec![action(
                "Review the exact upgrade diff",
                Some("aethyme upgrade plan --repo . --diff"),
            )],
        ),
        Some(schema) => dimension_with(
            ReadinessDimensionId::UpgradeCompatibility,
            ReadinessState::NotReady,
            "repository deployment is newer than this binary",
            vec![evidence(
                "repository-schema",
                format!("schema {schema}; binary supports {REPOSITORY_SCHEMA_VERSION}"),
            )],
            vec![action(
                "Update the Aethyme binary before making changes",
                None,
            )],
        ),
        None => dimension_with(
            ReadinessDimensionId::UpgradeCompatibility,
            ReadinessState::Unknown,
            "repository deployment schema could not be read",
            Vec::new(),
            Vec::new(),
        ),
    }
}

fn undeployed_report(reason: String) -> ReadinessReport {
    let dimensions = ReadinessDimensionId::ORDERED
        .into_iter()
        .map(|id| {
            if id == ReadinessDimensionId::RepositoryDeployment {
                dimension_with(
                    id,
                    ReadinessState::NotReady,
                    reason.clone(),
                    Vec::new(),
                    Vec::new(),
                )
            } else if matches!(
                id,
                ReadinessDimensionId::GraphAvailability
                    | ReadinessDimensionId::UpgradeCompatibility
            ) {
                dimension_with(
                    id,
                    ReadinessState::NotApplicable,
                    "not applicable until the repository is deployed",
                    Vec::new(),
                    Vec::new(),
                )
            } else {
                dimension_with(
                    id,
                    ReadinessState::Unknown,
                    "cannot inspect this dimension without a deployed Git repository",
                    Vec::new(),
                    Vec::new(),
                )
            }
        })
        .collect();
    ReadinessReport::from_dimensions(RepositoryReadinessMode::Absent, None, None, dimensions)
}

fn check<'a>(facts: &'a CertificationFacts, id: &str) -> Option<&'a crate::init::Check> {
    facts.checks.iter().find(|check| check.id == id)
}

fn evidence(id: impl Into<String>, summary: impl Into<String>) -> ReadinessEvidence {
    ReadinessEvidence {
        id: id.into(),
        summary: summary.into(),
    }
}

fn action(summary: impl Into<String>, command: Option<&str>) -> ReadinessAction {
    ReadinessAction {
        summary: summary.into(),
        command: command.map(str::to_owned),
    }
}

fn dimension_with(
    id: ReadinessDimensionId,
    state: ReadinessState,
    summary: impl Into<String>,
    evidence: Vec<ReadinessEvidence>,
    remediation: Vec<ReadinessAction>,
) -> ReadinessDimension {
    ReadinessDimension {
        id,
        state,
        summary: summary.into(),
        evidence,
        remediation,
    }
}

impl From<&ReadinessDimension> for ReadinessFinding {
    fn from(dimension: &ReadinessDimension) -> Self {
        Self {
            dimension: dimension.id,
            state: dimension.state,
            summary: dimension.summary.clone(),
            remediation: dimension.remediation.clone(),
        }
    }
}

fn classify_operating_mode(
    repository_mode: RepositoryReadinessMode,
    dimensions: &[ReadinessDimension],
) -> RepositoryOperatingMode {
    let state = |id| {
        dimensions
            .iter()
            .find(|dimension| dimension.id == id)
            .map(|dimension| dimension.state)
            .unwrap_or(ReadinessState::Unknown)
    };
    if repository_mode == RepositoryReadinessMode::Absent {
        return if state(ReadinessDimensionId::Coordination) == ReadinessState::Ready {
            RepositoryOperatingMode::ConflictOnly
        } else {
            RepositoryOperatingMode::Undeployed
        };
    }
    if matches!(
        state(ReadinessDimensionId::RepositoryDeployment),
        ReadinessState::NotReady
    ) || matches!(
        state(ReadinessDimensionId::UpgradeCompatibility),
        ReadinessState::NotReady
    ) {
        return RepositoryOperatingMode::Invalid;
    }
    let agent_ready = state(ReadinessDimensionId::AgentContext) == ReadinessState::Ready
        && state(ReadinessDimensionId::Validation) == ReadinessState::Ready;
    let parallel_ready = agent_ready
        && state(ReadinessDimensionId::Coordination) == ReadinessState::Ready
        && state(ReadinessDimensionId::ParallelExecution) == ReadinessState::Ready;
    if parallel_ready {
        RepositoryOperatingMode::ParallelReady
    } else if agent_ready {
        RepositoryOperatingMode::AgentReady
    } else {
        RepositoryOperatingMode::ConflictOnly
    }
}

fn operating_mode_rank(mode: RepositoryOperatingMode) -> u8 {
    match mode {
        RepositoryOperatingMode::Undeployed | RepositoryOperatingMode::Invalid => 0,
        RepositoryOperatingMode::ConflictOnly => 1,
        RepositoryOperatingMode::AgentReady => 2,
        RepositoryOperatingMode::ParallelReady => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn repository() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Test"]);
        git(repo.path(), &["config", "user.email", "test@example.com"]);
        std::fs::write(repo.path().join("README.md"), "fixture\n").unwrap();
        git(repo.path(), &["add", "README.md"]);
        git(repo.path(), &["commit", "-qm", "test: fixture"]);
        repo
    }

    fn dimension_by_id(report: &ReadinessReport, id: ReadinessDimensionId) -> &ReadinessDimension {
        report
            .dimensions
            .iter()
            .find(|dimension| dimension.id == id)
            .unwrap()
    }

    fn dimension(id: ReadinessDimensionId, state: ReadinessState) -> ReadinessDimension {
        ReadinessDimension {
            id,
            state,
            summary: format!("{id:?}"),
            evidence: Vec::new(),
            remediation: Vec::new(),
        }
    }

    fn dimensions(default: ReadinessState) -> Vec<ReadinessDimension> {
        ReadinessDimensionId::ORDERED
            .into_iter()
            .map(|id| dimension(id, default))
            .collect()
    }

    #[test]
    fn operating_modes_follow_agent_and_parallel_requirements() {
        let absent = ReadinessReport::from_dimensions(
            RepositoryReadinessMode::Absent,
            None,
            None,
            dimensions(ReadinessState::Unknown),
        );
        assert_eq!(absent.operating_mode, RepositoryOperatingMode::Undeployed);

        let mut initialized = dimensions(ReadinessState::Unknown);
        initialized
            .iter_mut()
            .find(|item| item.id == ReadinessDimensionId::Coordination)
            .unwrap()
            .state = ReadinessState::Ready;
        let initialized = ReadinessReport::from_dimensions(
            RepositoryReadinessMode::Absent,
            None,
            None,
            initialized,
        );
        assert_eq!(
            initialized.operating_mode,
            RepositoryOperatingMode::ConflictOnly
        );

        let mut agent = dimensions(ReadinessState::Limited);
        for id in [
            ReadinessDimensionId::RepositoryDeployment,
            ReadinessDimensionId::AgentContext,
            ReadinessDimensionId::Validation,
            ReadinessDimensionId::UpgradeCompatibility,
        ] {
            agent.iter_mut().find(|item| item.id == id).unwrap().state = ReadinessState::Ready;
        }
        let agent =
            ReadinessReport::from_dimensions(RepositoryReadinessMode::Canonical, None, None, agent);
        assert_eq!(agent.operating_mode, RepositoryOperatingMode::AgentReady);
        assert!(agent.meets(RepositoryOperatingMode::ConflictOnly));
        assert!(!agent.meets(RepositoryOperatingMode::ParallelReady));

        let parallel = ReadinessReport::from_dimensions(
            RepositoryReadinessMode::Canonical,
            None,
            None,
            dimensions(ReadinessState::Ready),
        );
        assert_eq!(
            parallel.operating_mode,
            RepositoryOperatingMode::ParallelReady
        );
    }

    #[test]
    fn shared_text_renderer_is_compact_and_excludes_optional_graph_actions() {
        let report = ReadinessReport::from_dimensions(
            RepositoryReadinessMode::Canonical,
            None,
            None,
            vec![
                dimension(
                    ReadinessDimensionId::RepositoryDeployment,
                    ReadinessState::Ready,
                ),
                dimension(ReadinessDimensionId::Coordination, ReadinessState::Ready),
                ReadinessDimension {
                    id: ReadinessDimensionId::AgentContext,
                    state: ReadinessState::NotReady,
                    summary: "agent context missing".into(),
                    evidence: Vec::new(),
                    remediation: vec![action(
                        "Deploy the generated agent protocol",
                        Some("aethyme deploy --repo ."),
                    )],
                },
                dimension(ReadinessDimensionId::Validation, ReadinessState::Limited),
                dimension(
                    ReadinessDimensionId::ParallelExecution,
                    ReadinessState::NotReady,
                ),
                ReadinessDimension {
                    id: ReadinessDimensionId::GraphAvailability,
                    state: ReadinessState::NotApplicable,
                    summary: "disabled".into(),
                    evidence: Vec::new(),
                    remediation: vec![action("Optional graph action", None)],
                },
                dimension(
                    ReadinessDimensionId::UpgradeCompatibility,
                    ReadinessState::Ready,
                ),
            ],
        );
        let text = render_readiness_text(&report);
        assert!(text.starts_with("Operating mode: conflict_only\n"));
        assert!(text.contains("Agent context: not ready\n"));
        assert!(text.contains("Graph: disabled by repository policy; no action required.\n"));
        assert!(text.contains("- Deploy the generated agent protocol: `aethyme deploy --repo .`"));
        assert!(!text.contains("Optional graph action"));
        assert_eq!(
            serde_json::from_str::<ReadinessReport>(&render_readiness_json(&report).unwrap())
                .unwrap(),
            report
        );
    }

    #[test]
    fn dimensions_findings_and_json_order_are_stable() {
        let report = ReadinessReport::from_dimensions(
            RepositoryReadinessMode::LocalOnly,
            Some("a".repeat(40)),
            Some("b".repeat(64)),
            vec![
                dimension(
                    ReadinessDimensionId::UpgradeCompatibility,
                    ReadinessState::Unknown,
                ),
                dimension(
                    ReadinessDimensionId::RepositoryDeployment,
                    ReadinessState::NotReady,
                ),
                dimension(
                    ReadinessDimensionId::GraphAvailability,
                    ReadinessState::NotApplicable,
                ),
            ],
        );
        assert_eq!(report.schema_version, 1);
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            report
                .dimensions
                .iter()
                .map(|dimension| dimension.id)
                .collect::<Vec<_>>(),
            vec![
                ReadinessDimensionId::RepositoryDeployment,
                ReadinessDimensionId::GraphAvailability,
                ReadinessDimensionId::UpgradeCompatibility,
            ]
        );
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.starts_with(
            "{\"schema_version\":1,\"repository_mode\":\"local_only\",\"operating_mode\":\"invalid\""
        ));
    }

    #[test]
    fn inspection_reports_missing_state_without_creating_it() {
        let repo = repository();
        let broker_db = repo.path().join(BROKER_DB_RELPATH);
        let onboarding = repo
            .path()
            .join(aethyme_enhance::onboarding::ONBOARDING_JSON_PATH);

        let report = inspect_repository_readiness(repo.path());

        assert_eq!(report.repository_mode, RepositoryReadinessMode::Absent);
        assert_eq!(report.operating_mode, RepositoryOperatingMode::Undeployed);
        assert_eq!(
            dimension_by_id(&report, ReadinessDimensionId::Coordination).state,
            ReadinessState::Limited
        );
        assert_eq!(
            dimension_by_id(&report, ReadinessDimensionId::GraphAvailability).state,
            ReadinessState::NotApplicable
        );
        assert!(!broker_db.exists());
        assert!(!onboarding.exists());
    }

    #[test]
    fn enabled_graph_without_verified_artifacts_is_limited() {
        let repo = repository();
        std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
        std::fs::write(
            repo.path().join(CANONICAL_REPOSITORY_MARKER_PATH),
            "{\"schema_version\":1}\n",
        )
        .unwrap();
        std::fs::write(
            repo.path().join(".aethyme/config.toml"),
            "schema = 1\n[graph]\nauthority = \"committed_fragments\"\nrepository = \"example/repo\"\n",
        )
        .unwrap();

        let report = inspect_repository_readiness(repo.path());

        assert_eq!(
            dimension_by_id(&report, ReadinessDimensionId::GraphAvailability).state,
            ReadinessState::Limited
        );
        assert!(!repo.path().join(".aethyme/graph").exists());
        assert!(!repo.path().join(".aethyme/graph_store.redb").exists());
    }

    #[test]
    fn older_and_newer_repository_schemas_are_not_ready() {
        let repo = repository();
        std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
        std::fs::write(repo.path().join(".aethyme/config.toml"), "schema = 1\n").unwrap();
        let marker = repo.path().join(CANONICAL_REPOSITORY_MARKER_PATH);
        for schema in [0, REPOSITORY_SCHEMA_VERSION + 1] {
            std::fs::write(&marker, format!("{{\"schema_version\":{schema}}}\n")).unwrap();
            let report = inspect_repository_readiness(repo.path());
            assert_eq!(
                dimension_by_id(&report, ReadinessDimensionId::UpgradeCompatibility).state,
                ReadinessState::NotReady
            );
            assert_eq!(report.operating_mode, RepositoryOperatingMode::Invalid);
        }
    }
}
