//! What a killed gate leaves outside the broker's own stores, and the reviewed
//! plan that removes it (#287).
//!
//! A gate command that starts a container keeps it running after the gate is
//! killed, and the container keeps its volume mounted, so the next run of the
//! same gate refuses with "already mounted". Nothing in `broker.db`, the host
//! ledgers or the gate pidfiles names that container, so no broker view
//! showed it. The broker cannot start containers itself -- gate commands do --
//! but it does start every gate command, so it hands each one the labels to
//! put on what it starts (`AETHYME_GATE_LABELS`, `AETHYME_GATE_RUN_ID`) and
//! records the run id in the gate's pidfile. `broker doctor` then lists the
//! labelled resources with their owner's liveness next to the blockers of
//! every other store, and `doctor apply` removes, through a digest the
//! operator reviewed, only the ones whose owning run is verifiably over.
//! Everything whose owner it cannot establish is reported and kept.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Broker, BrokerOpError, SessionStatus};

/// The run id of the gate command being executed, unique per run.
pub const GATE_RUN_ID_ENV: &str = "AETHYME_GATE_RUN_ID";
/// Space-separated `key=value` labels a gate command should put on every
/// container, volume or other runtime resource it starts.
pub const GATE_LABELS_ENV: &str = "AETHYME_GATE_LABELS";
/// The container CLI `broker doctor` asks (`docker`, `podman`, a path), or
/// `none` to skip the container runtime entirely.
pub const CONTAINER_RUNTIME_ENV: &str = "AETHYME_CONTAINER_RUNTIME";
/// Event recorded for every resource `doctor apply` removes.
pub const DOCTOR_REMOVED: &str = "broker.doctor.removed";

pub const DOCTOR_PLAN_SCHEMA_VERSION: u32 = 1;

const LABEL_GATE: &str = "aethyme.gate";
const LABEL_SESSION: &str = "aethyme.session";
const LABEL_REPO: &str = "aethyme.repo";
const LABEL_RUN: &str = "aethyme.run";

/// One container CLI call. A wedged daemon must not hang the doctor.
const RUNTIME_BUDGET: Duration = Duration::from_secs(10);

/// The labels one gate run hands to its command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GateRunLabels {
    pub gate: String,
    pub session_id: Option<i64>,
    pub repository: String,
    pub run: String,
}

impl GateRunLabels {
    /// A fresh run id: the gate's own clock and the worker's pid, hashed, so
    /// two workers that start in the same tick still differ.
    pub fn new(gate: &str, session_id: Option<i64>, repository: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        let seed = format!("{gate}\0{session_id:?}\0{nanos}\0{}", std::process::id());
        let run = hex(&Sha256::digest(seed.as_bytes()))[..16].to_string();
        Self {
            gate: gate.into(),
            session_id,
            repository: repository.into(),
            run,
        }
    }

    /// The `AETHYME_GATE_LABELS` value. A run without a session carries an
    /// empty session label, which the doctor reads as an unknown owner.
    pub fn env_value(&self) -> String {
        format!(
            "{LABEL_GATE}={} {LABEL_SESSION}={} {LABEL_REPO}={} {LABEL_RUN}={}",
            self.gate,
            self.session_id.map(|id| id.to_string()).unwrap_or_default(),
            self.repository,
            self.run
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeResourceKind {
    Container,
    Volume,
}

/// One resource the container runtime reports, with its labels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeResource {
    pub kind: RuntimeResourceKind,
    /// The handle the runtime removes it by: a container id or a volume name.
    pub handle: String,
    pub name: String,
    pub labels: BTreeMap<String, String>,
}

/// The container runtime as the doctor uses it. A trait so tests (and CI
/// without Docker) never touch a real daemon.
pub trait ContainerRuntime {
    fn name(&self) -> &str;
    /// Every container and volume carrying an `aethyme.repo` label.
    fn list_labelled(&self) -> Result<Vec<RuntimeResource>, String>;
    fn remove(&self, resource: &RuntimeResource) -> Result<(), String>;
}

/// A Docker-compatible CLI (`docker`, `podman`).
#[derive(Debug, Clone)]
pub struct CliContainerRuntime {
    program: PathBuf,
    name: String,
}

impl CliContainerRuntime {
    /// The runtime named by [`CONTAINER_RUNTIME_ENV`], else `docker` or
    /// `podman` from `PATH`. `Ok(None)` means there is none to ask -- not an
    /// error: most repositories' gates start no containers.
    pub fn discover() -> Option<Self> {
        let requested = std::env::var(CONTAINER_RUNTIME_ENV).ok();
        match requested.as_deref().map(str::trim) {
            Some("none") => None,
            Some(program) if !program.is_empty() => {
                let path = PathBuf::from(program);
                let resolved = if path.components().count() > 1 {
                    path.is_file().then_some(path)
                } else {
                    find_on_path(program)
                };
                resolved.map(Self::with_program)
            }
            _ => ["docker", "podman"]
                .into_iter()
                .find_map(find_on_path)
                .map(Self::with_program),
        }
    }

    fn with_program(program: PathBuf) -> Self {
        let name = program
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| program.to_string_lossy().into_owned());
        Self { program, name }
    }

    fn run(&self, args: &[&str]) -> Result<String, String> {
        let mut command = Command::new(&self.program);
        command.args(args);
        let output = crate::bounded_output::output_within(&mut command, RUNTIME_BUDGET)
            .map_err(|error| format!("{} {}: {error}", self.name, args.join(" ")))?
            .ok_or_else(|| {
                format!(
                    "{} {} did not answer within {}s",
                    self.name,
                    args.join(" "),
                    RUNTIME_BUDGET.as_secs()
                )
            })?;
        if !output.status.success() {
            return Err(format!(
                "{} {} failed: {}",
                self.name,
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }
}

impl ContainerRuntime for CliContainerRuntime {
    fn name(&self) -> &str {
        &self.name
    }

    fn list_labelled(&self) -> Result<Vec<RuntimeResource>, String> {
        let filter = format!("label={LABEL_REPO}");
        let containers = self.run(&[
            "ps",
            "-a",
            "--no-trunc",
            "--filter",
            &filter,
            "--format",
            "{{json .}}",
        ])?;
        let volumes = self.run(&[
            "volume",
            "ls",
            "--filter",
            &filter,
            "--format",
            "{{json .}}",
        ])?;
        let mut resources = parse_runtime_lines(RuntimeResourceKind::Container, &containers)?;
        resources.extend(parse_runtime_lines(RuntimeResourceKind::Volume, &volumes)?);
        Ok(resources)
    }

    fn remove(&self, resource: &RuntimeResource) -> Result<(), String> {
        match resource.kind {
            RuntimeResourceKind::Container => self.run(&["rm", "-f", &resource.handle]),
            RuntimeResourceKind::Volume => self.run(&["volume", "rm", &resource.handle]),
        }
        .map(|_| ())
    }
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

/// One JSON object per line, as `--format '{{json .}}'` prints them. Labels
/// arrive as `k=v,k=v` from Docker and as an object from Podman.
fn parse_runtime_lines(
    kind: RuntimeResourceKind,
    text: &str,
) -> Result<Vec<RuntimeResource>, String> {
    let mut resources = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let value: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("unreadable container runtime line {line:?}: {error}"))?;
        let field = |names: &[&str]| {
            names
                .iter()
                .find_map(|name| value.get(*name).and_then(|field| field.as_str()))
                .map(str::to_string)
        };
        let (handle, name) = match kind {
            RuntimeResourceKind::Container => {
                let id = field(&["ID", "Id"]).ok_or("container line has no ID")?;
                let name = field(&["Names", "Name"]).unwrap_or_else(|| id.clone());
                (id, name)
            }
            RuntimeResourceKind::Volume => {
                let name = field(&["Name"]).ok_or("volume line has no Name")?;
                (name.clone(), name)
            }
        };
        let labels = match value.get("Labels") {
            Some(serde_json::Value::String(text)) => text
                .split(',')
                .filter_map(|pair| pair.split_once('='))
                .map(|(key, value)| (key.trim().to_string(), value.trim().to_string()))
                .collect(),
            Some(serde_json::Value::Object(map)) => map
                .iter()
                .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                .collect(),
            _ => BTreeMap::new(),
        };
        resources.push(RuntimeResource {
            kind,
            handle,
            name,
            labels,
        });
    }
    Ok(resources)
}

/// Whether the run that owns a resource is over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebrisOwner {
    /// The owning gate run is verifiably over: removable.
    Dead,
    /// The owning gate run is still going.
    Live,
    /// The doctor cannot tell; never removed.
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebrisAction {
    /// `doctor apply` removes it.
    Remove,
    /// Listed only; the command in `clear` (if any) is the operator's call.
    Report,
}

/// One item of debris, from any store.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebrisItem {
    pub id: String,
    /// `container`, `volume`, `stale_worktree`, or a blocker kind
    /// (`operation`, `resource_lease`, ...).
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<String>,
    pub owner: DebrisOwner,
    pub action: DebrisAction,
    pub reason: String,
    /// The command that clears it by hand, when the doctor does not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub clear: Option<String>,
}

/// The container runtime the plan asked, if any.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebrisRuntime {
    pub name: String,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `broker doctor plan`: every debris item across stores, and the digest
/// `doctor apply --confirm` must repeat.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebrisPlan {
    pub schema_version: u32,
    pub digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<DebrisRuntime>,
    pub items: Vec<DebrisItem>,
    pub removable_count: usize,
    /// Stores the plan could not read. Never empty-means-clean.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable: Vec<crate::BlockerSourceError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paired_recovery: Option<crate::PairedRecovery>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebrisRemoval {
    pub id: String,
    pub removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DebrisApplyReport {
    pub digest: String,
    pub removals: Vec<DebrisRemoval>,
    /// Items left in place: live or unknown owners and report-only kinds.
    pub kept: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DebrisApplyOutcome {
    Applied(DebrisApplyReport),
    /// The plan changed since it was reviewed; nothing was removed.
    StaleDigest {
        expected: String,
        current: String,
    },
}

impl Broker {
    /// Every debris item this repository has across its stores and the
    /// container runtime, read-only.
    pub fn debris_plan(&mut self, runtime: Option<&dyn ContainerRuntime>) -> DebrisPlan {
        let blockers = self.blockers();
        let mut items: Vec<DebrisItem> = blockers
            .blockers
            .iter()
            .map(|blocker| DebrisItem {
                id: blocker.id.clone(),
                kind: blocker.kind.as_str().into(),
                name: blocker.cause.clone(),
                session_id: blocker.session_id,
                gate: None,
                run: None,
                owner: DebrisOwner::Unknown,
                action: DebrisAction::Report,
                reason: "a blocker needs the recovery its store owns; the doctor never clears it"
                    .into(),
                clear: Some(blocker.clear.clone()),
            })
            .collect();
        let mut unavailable = blockers.unavailable;
        match self.stale_worktree_items() {
            Ok(found) => items.extend(found),
            Err(error) => unavailable.push(crate::BlockerSourceError {
                source: "session worktrees",
                error: error.to_string(),
            }),
        }
        let runtime_report = runtime.map(|runtime| match runtime.list_labelled() {
            Ok(resources) => {
                items.extend(self.runtime_items(resources));
                DebrisRuntime {
                    name: runtime.name().into(),
                    available: true,
                    error: None,
                }
            }
            Err(error) => {
                unavailable.push(crate::BlockerSourceError {
                    source: "container runtime",
                    error: error.clone(),
                });
                DebrisRuntime {
                    name: runtime.name().into(),
                    available: false,
                    error: Some(error),
                }
            }
        });
        let removable_count = items
            .iter()
            .filter(|item| item.action == DebrisAction::Remove)
            .count();
        DebrisPlan {
            schema_version: DOCTOR_PLAN_SCHEMA_VERSION,
            digest: plan_digest(&items),
            runtime: runtime_report,
            items,
            removable_count,
            unavailable,
            paired_recovery: blockers.paired_recovery,
        }
    }

    /// Remove what a reviewed plan marked removable. The plan is rebuilt
    /// here, so every owner is judged again at apply time; a plan that no
    /// longer matches the reviewed digest removes nothing.
    pub fn apply_debris_plan(
        &mut self,
        runtime: Option<&dyn ContainerRuntime>,
        confirm: &str,
    ) -> Result<DebrisApplyOutcome, BrokerOpError> {
        let plan = self.debris_plan(runtime);
        if plan.digest != confirm {
            return Ok(DebrisApplyOutcome::StaleDigest {
                expected: confirm.into(),
                current: plan.digest,
            });
        }
        let mut removals = Vec::new();
        let mut kept = 0;
        // Plan items are sorted by id, which puts every `container:` before
        // any `volume:`: a volume is in use while a container still mounts it.
        let mut removable: Vec<&DebrisItem> = Vec::new();
        for item in &plan.items {
            if item.action == DebrisAction::Remove {
                removable.push(item);
            } else {
                kept += 1;
            }
        }
        if !removable.is_empty() {
            let runtime = runtime.ok_or_else(|| BrokerOpError::InvalidCoordinatedOperation {
                reason: "the plan removes runtime resources but no container runtime is available"
                    .into(),
            })?;
            let listed = runtime
                .list_labelled()
                .map_err(|reason| BrokerOpError::InvalidCoordinatedOperation { reason })?;
            for item in removable {
                let Some(resource) = listed
                    .iter()
                    .find(|resource| runtime_id(resource) == item.id)
                else {
                    removals.push(DebrisRemoval {
                        id: item.id.clone(),
                        removed: false,
                        error: Some("no longer reported by the container runtime".into()),
                    });
                    continue;
                };
                let result = runtime.remove(resource);
                if result.is_ok() {
                    let payload = serde_json::json!({
                        "id": item.id,
                        "kind": item.kind,
                        "name": item.name,
                        "gate": item.gate,
                        "run": item.run,
                        "reason": item.reason,
                        "digest": plan.digest,
                    });
                    self.store().append_event(
                        DOCTOR_REMOVED,
                        item.session_id,
                        Some(&payload.to_string()),
                    )?;
                }
                removals.push(DebrisRemoval {
                    id: item.id.clone(),
                    removed: result.is_ok(),
                    error: result.err(),
                });
            }
        }
        Ok(DebrisApplyOutcome::Applied(DebrisApplyReport {
            digest: plan.digest,
            removals,
            kept,
        }))
    }

    /// Session worktrees left by sessions that went stale. Reported only:
    /// `finish`/`gc` own their removal and its safety checks.
    fn stale_worktree_items(&mut self) -> Result<Vec<DebrisItem>, BrokerOpError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis() as i64)
            .unwrap_or_default();
        Ok(self
            .agents(now)?
            .into_iter()
            .filter(|agent| agent.derived_status == SessionStatus::Stale)
            .filter(|agent| Path::new(&agent.session.worktree_path).is_dir())
            .map(|agent| DebrisItem {
                id: format!("worktree:{}", agent.session.id),
                kind: "stale_worktree".into(),
                name: agent.session.worktree_path.clone(),
                session_id: Some(agent.session.id),
                gate: None,
                run: None,
                owner: DebrisOwner::Unknown,
                action: DebrisAction::Report,
                reason: "the session went stale with its worktree still on disk".into(),
                clear: Some(format!(
                    "aethyme broker finish --session {}",
                    agent.session.id
                )),
            })
            .collect())
    }

    /// The `aethyme.repo` value gate commands in this repository receive.
    pub fn gate_repository_key(&self) -> Option<String> {
        crate::GitRepo::discover(self.main_root())
            .map(|repo| crate::gates::repository_key(&repo).0)
            .ok()
    }

    fn runtime_items(&mut self, resources: Vec<RuntimeResource>) -> Vec<DebrisItem> {
        let repository = self.gate_repository_key();
        let run_dir = crate::gates::running_dir(self.main_root());
        let mut items = Vec::new();
        for resource in resources {
            // Another repository's broker judges its own resources.
            if repository.as_deref() != resource.labels.get(LABEL_REPO).map(String::as_str) {
                continue;
            }
            let (owner, reason) = self.runtime_owner(&resource, &run_dir);
            let session_id = resource
                .labels
                .get(LABEL_SESSION)
                .and_then(|value| value.parse().ok());
            let clear = match resource.kind {
                RuntimeResourceKind::Container => {
                    format!(
                        "docker inspect {}",
                        crate::broker::shell_quote(&resource.handle)
                    )
                }
                RuntimeResourceKind::Volume => format!(
                    "docker volume inspect {}",
                    crate::broker::shell_quote(&resource.handle)
                ),
            };
            items.push(DebrisItem {
                id: runtime_id(&resource),
                kind: match resource.kind {
                    RuntimeResourceKind::Container => "container",
                    RuntimeResourceKind::Volume => "volume",
                }
                .into(),
                name: resource.name.clone(),
                session_id,
                gate: resource.labels.get(LABEL_GATE).cloned(),
                run: resource.labels.get(LABEL_RUN).cloned(),
                owner,
                action: if owner == DebrisOwner::Dead {
                    DebrisAction::Remove
                } else {
                    DebrisAction::Report
                },
                reason,
                clear: (owner != DebrisOwner::Dead).then_some(clear),
            });
        }
        // Sorted by id: the digest must not depend on the runtime's listing
        // order, and `doctor apply` removes containers before volumes.
        items.sort_by(|left, right| left.id.cmp(&right.id));
        items
    }

    /// Who owns a labelled resource, and whether its run is over. Anything
    /// short of proof is [`DebrisOwner::Unknown`].
    fn runtime_owner(
        &mut self,
        resource: &RuntimeResource,
        run_dir: &Path,
    ) -> (DebrisOwner, String) {
        let label = |key: &str| {
            resource
                .labels
                .get(key)
                .map(String::as_str)
                .filter(|value| !value.is_empty())
        };
        let (Some(gate), Some(session), Some(run)) =
            (label(LABEL_GATE), label(LABEL_SESSION), label(LABEL_RUN))
        else {
            return (
                DebrisOwner::Unknown,
                format!(
                    "labelled for this repository but missing {LABEL_GATE}, {LABEL_SESSION} or {LABEL_RUN}; its owner cannot be established"
                ),
            );
        };
        let Some(session_id) = session.parse::<i64>().ok().filter(|id| *id > 0) else {
            return (
                DebrisOwner::Unknown,
                format!("{LABEL_SESSION}={session:?} is not a session id"),
            );
        };
        if !valid_gate_name(gate) {
            return (
                DebrisOwner::Unknown,
                format!("{LABEL_GATE}={gate:?} is not a gate name"),
            );
        }
        if self.store().session(session_id).is_err() {
            return (
                DebrisOwner::Unknown,
                format!("session {session_id} is not recorded in this repository"),
            );
        }
        let pidfile = run_dir.join(format!("{session_id}-{gate}.pid"));
        let content = match std::fs::read_to_string(&pidfile) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (
                    DebrisOwner::Dead,
                    format!("no run of gate {gate} is active for session {session_id}"),
                );
            }
            Err(error) => {
                return (
                    DebrisOwner::Unknown,
                    format!("gate pidfile {} is unreadable: {error}", pidfile.display()),
                );
            }
        };
        let Some(record) = crate::gates::GatePidRecord::parse(&content) else {
            return (
                DebrisOwner::Unknown,
                format!("gate pidfile {} is unreadable", pidfile.display()),
            );
        };
        if !record.names_running_process() {
            return (
                DebrisOwner::Dead,
                format!("the gate {gate} run recorded for session {session_id} has exited"),
            );
        }
        match record.run.as_deref() {
            Some(current) if current == run => (
                DebrisOwner::Live,
                format!("run {run} of gate {gate} is still going"),
            ),
            Some(current) => (
                DebrisOwner::Dead,
                format!("run {run} is over; gate {gate} is now running as run {current}"),
            ),
            None => (
                DebrisOwner::Unknown,
                format!(
                    "a run of gate {gate} is going for session {session_id} but recorded no run id"
                ),
            ),
        }
    }
}

fn runtime_id(resource: &RuntimeResource) -> String {
    match resource.kind {
        RuntimeResourceKind::Container => {
            let short: String = resource.handle.chars().take(12).collect();
            format!("container:{short}")
        }
        RuntimeResourceKind::Volume => format!("volume:{}", resource.handle),
    }
}

fn valid_gate_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && !name.starts_with('.')
}

/// The digest binds what the operator reviewed: each item, its owner verdict
/// and its action. Free text (reasons, names) is left out, so rewording a
/// message does not invalidate a plan.
fn plan_digest(items: &[DebrisItem]) -> String {
    let canonical: Vec<serde_json::Value> = items
        .iter()
        .map(|item| {
            serde_json::json!({
                "id": item.id,
                "kind": item.kind,
                "owner": item.owner,
                "action": item.action,
                "run": item.run,
            })
        })
        .collect();
    let text = serde_json::to_string(&canonical).unwrap_or_default();
    hex(&Sha256::digest(text.as_bytes()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_and_podman_label_shapes_both_parse() {
        let docker =
            r#"{"ID":"abcdef0123456789","Names":"pg","Labels":"aethyme.repo=k,aethyme.run=r1"}"#;
        let podman = r#"{"Name":"vol","Labels":{"aethyme.repo":"k","aethyme.gate":"g"}}"#;
        let containers = parse_runtime_lines(RuntimeResourceKind::Container, docker).unwrap();
        assert_eq!(containers[0].labels["aethyme.run"], "r1");
        assert_eq!(runtime_id(&containers[0]), "container:abcdef012345");
        let volumes = parse_runtime_lines(RuntimeResourceKind::Volume, podman).unwrap();
        assert_eq!(volumes[0].labels["aethyme.gate"], "g");
        assert_eq!(runtime_id(&volumes[0]), "volume:vol");
    }

    #[test]
    fn run_labels_name_every_owner_field() {
        let labels = GateRunLabels::new("quality", Some(7), "repokey");
        let value = labels.env_value();
        for expected in [
            "aethyme.gate=quality",
            "aethyme.session=7",
            "aethyme.repo=repokey",
            &format!("aethyme.run={}", labels.run),
        ] {
            assert!(value.split(' ').any(|pair| pair == expected), "{value}");
        }
        assert_ne!(
            labels.run,
            GateRunLabels::new("quality", Some(7), "repokey").run
        );
    }

    #[test]
    fn the_digest_ignores_wording_but_not_verdicts() {
        let item = DebrisItem {
            id: "container:abc".into(),
            kind: "container".into(),
            name: "pg".into(),
            session_id: Some(1),
            gate: Some("g".into()),
            run: Some("r".into()),
            owner: DebrisOwner::Dead,
            action: DebrisAction::Remove,
            reason: "one wording".into(),
            clear: None,
        };
        let reworded = DebrisItem {
            reason: "another wording".into(),
            ..item.clone()
        };
        assert_eq!(
            plan_digest(std::slice::from_ref(&item)),
            plan_digest(&[reworded])
        );
        let live = DebrisItem {
            owner: DebrisOwner::Live,
            action: DebrisAction::Report,
            ..item.clone()
        };
        assert_ne!(plan_digest(&[item]), plan_digest(&[live]));
    }
}
