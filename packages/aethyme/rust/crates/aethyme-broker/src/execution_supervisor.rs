//! One concrete disposable Linux Docker profile (#671). The daemon and host
//! supervisor are trusted; the candidate receives no daemon socket/credentials.
//! Build exits are observations, never independent behavioral evidence.
use crate::collaboration_archive::{self as archive, ObjectDigest};
use crate::collaboration_state::CollaborationStore;
use crate::execution_snapshot::{self as snapshot, ExecutionSnapshot, MaterializedInputs};
use crate::local_target::{self as target, ControllerOwner};
use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshot,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
pub(crate) mod artifact;
mod docker;

pub const PROFILE: &str = "aethyme.docker-linux/offline-v0";
pub const ARTIFACT_SCHEMA: &str = "aethyme.build-artifact/provisional-v0";
static SCHEMA: RecordSchema = RecordSchema {
    name: ARTIFACT_SCHEMA,
    fields: &[
        FieldSpec {
            name: "execution",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "target_revision",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "output",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "packaging",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "runtime",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
        FieldSpec {
            name: "generation",
            required: true,
            kind: FieldKind::String,
            capability: None,
        },
    ],
    capabilities: &[],
};
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("unsupported executor: {0}")]
    Unsupported(String),
    #[error("infrastructure deferred: {0}")]
    Infrastructure(String),
    #[error("inconclusive execution: {0}")]
    Inconclusive(String),
    #[error("tampering detected: {0}")]
    Tampered(String),
    #[error("execution resource limit exceeded")]
    Limit,
    #[error("execution cancelled")]
    Cancelled,
    #[error("execution timed out")]
    TimedOut,
    #[error(transparent)]
    Resources(#[from] crate::execution_resources::ResourceError),
    #[error(transparent)]
    Budget(#[from] crate::local_budget::BudgetError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Snapshot(#[from] snapshot::SnapshotError),
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Sensitive(#[from] crate::collaboration_sensitive::SensitiveError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Built,
    CodeFailed,
    InfrastructureDeferred,
    Cancelled,
    TimedOut,
    Inconclusive,
    TamperingDetected,
}
/// Exact configured daemon/runtime identity observed outside the workload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeDescription {
    pub native: String,
    pub client: String,
    pub profile: String,
    pub endpoint: String,
    pub daemon_id: String,
    pub engine: String,
    pub api: String,
    pub kernel: String,
    pub architecture: String,
    pub components: serde_json::Value,
    pub policy: String,
}
impl RuntimeDescription {
    pub fn id(&self) -> String {
        format!(
            "sha256:{}",
            ObjectDigest::of(&serde_json::to_vec(self).expect("runtime serialization")).hex()
        )
    }
}
#[derive(Debug)]
pub struct DockerExecutor {
    cli: docker::Connection,
    runtime: RuntimeDescription,
    resources: Option<crate::execution_resources::ResourcePolicy>,
}
impl DockerExecutor {
    /// Capture a runtime descriptor for operator approval and the frozen spec.
    /// Every build probes again; this does not silently follow runtime changes.
    pub fn probe(cli: PathBuf) -> Result<Self, SupervisorError> {
        if !cli.is_absolute() {
            return Err(SupervisorError::Unsupported(
                "absolute Docker CLI required".into(),
            ));
        }
        let cli = docker::Connection::local(cli)?;
        let runtime = docker::probe(&cli)?;
        Ok(Self {
            cli,
            runtime,
            resources: None,
        })
    }
    /// Capture a runtime descriptor from one operator-pinned Unix endpoint.
    /// Unlike [`Self::probe`], this path never consults `DOCKER_HOST` or a CLI
    /// context, and the endpoint remains part of the observed runtime identity.
    pub fn probe_at(cli: PathBuf, endpoint: &str) -> Result<Self, SupervisorError> {
        if !cli.is_absolute() {
            return Err(SupervisorError::Unsupported(
                "absolute Docker CLI required".into(),
            ));
        }
        let cli = docker::Connection::pinned(cli, endpoint)?;
        let runtime = docker::probe(&cli)?;
        Ok(Self {
            cli,
            runtime,
            resources: None,
        })
    }
    /// Reclaim only the named journal's exact resources after its reader lock
    /// is available. Unacknowledged creation stays held for explicit review.
    pub fn recover_resources(&self, run: &str) -> Result<(), SupervisorError> {
        let policy = self.resource_policy()?;
        if docker::probe(&self.cli)? != self.runtime {
            return Err(SupervisorError::Unsupported(
                "recovery runtime drift".into(),
            ));
        }
        let mut run = crate::execution_resources::Run::recover(policy, run)?;
        docker::recover(&self.cli, &mut run)
    }
    pub fn with_resources(
        mut self,
        policy: crate::execution_resources::ResourcePolicy,
    ) -> Result<Self, SupervisorError> {
        if policy.runtime != self.runtime.id() || policy.daemon != self.runtime.daemon_id {
            return Err(SupervisorError::Unsupported(
                "resource policy/runtime mismatch".into(),
            ));
        }
        docker::image(&self.cli, &policy.meter_image, &self.runtime.architecture)?;
        self.resources = Some(policy);
        Ok(self)
    }
    pub(crate) fn optional_temporary_root(&self) -> Option<&Path> {
        self.resources.as_ref().map(|p| p.temporary_root.as_path())
    }
    pub(crate) fn endpoint(&self) -> &str {
        &self.cli.endpoint
    }
    pub(crate) fn resource_policy(
        &self,
    ) -> Result<&crate::execution_resources::ResourcePolicy, SupervisorError> {
        self.resources.as_ref().ok_or_else(|| {
            SupervisorError::Infrastructure("explicit host resource policy required".into())
        })
    }
    pub fn runtime(&self) -> &RuntimeDescription {
        &self.runtime
    }
}
/// Observations of daemon-attached streams, never candidate result files.
pub(crate) struct ObservedCommand {
    pub exit: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn reconstruct_artifact_output(
    store: &CollaborationStore,
    target_revision: &RecordId,
    record: &ObjectDigest,
    dest: &Path,
    expected: &SourceSnapshot,
    max_output_bytes: u64,
) -> Result<SourceSnapshot, SupervisorError> {
    let max_output_bytes = usize::try_from(max_output_bytes).map_err(|_| SupervisorError::Limit)?;
    let limits = snapshot::RECONSTRUCTION_LIMITS.with_max_output_bytes(max_output_bytes);
    let archive_guard = crate::collaboration_gc::archive_use(store)?;
    let id = expected.id();
    let mut objects = Vec::with_capacity(expected.entries().len() + 2);
    objects.push(*record);
    objects.push(ObjectDigest::of_snapshot(&id));
    objects.extend(
        expected
            .entries()
            .iter()
            .map(|entry| ObjectDigest::from_bytes(*entry.content_sha256())),
    );
    if !target::artifact_objects_are_retained(store, target_revision, &objects, &archive_guard)? {
        return Err(SupervisorError::Tampered(
            "artifact objects are not retained by the target revision".into(),
        ));
    }
    let manifest = archive::reconstruct_artifact_bounded_under_guard(
        store,
        expected,
        dest,
        limits,
        &archive_guard,
    )?;
    if manifest != *expected {
        return Err(SupervisorError::Tampered("sealed artifact mismatch".into()));
    }
    Ok(manifest)
}

impl DockerExecutor {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn observe_cli<F, G>(
        &self,
        store: &CollaborationStore,
        execution: &ExecutionSnapshot,
        artifact: &SealedArtifact,
        generation: &str,
        arguments: &[String],
        stdin: &[u8],
        admission: crate::execution_resources::Admission<'_>,
        cancel: &AtomicBool,
        authorize: F,
    ) -> Result<ObservedCommand, SupervisorError>
    where
        F: FnMut() -> Result<G, SupervisorError>,
    {
        let spec = execution.spec();
        if spec.profile.runtime_profile != self.runtime.id()
            || docker::probe(&self.cli)? != self.runtime
        {
            return Err(SupervisorError::Unsupported("runtime/profile drift".into()));
        }
        docker::image(
            &self.cli,
            &spec.profile.verifier_image,
            &spec.profile.architecture,
        )?;
        let _archive = crate::collaboration_gc::archive_use(store)?;
        let directory = if let Some(policy) = &self.resources {
            tempfile::TempDir::new_in(&policy.temporary_root)?
        } else {
            tempfile::TempDir::new()?
        };
        let materialized = directory.path().join("artifact");
        std::fs::create_dir(&materialized)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&materialized, std::fs::Permissions::from_mode(0o755))?;
        }

        let manifest = reconstruct_artifact_output(
            store,
            execution.target_revision(),
            &artifact.record,
            &materialized,
            &artifact.output.manifest,
            spec.profile.build_limits.output_bytes,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for entry in manifest.entries() {
                let relative = std::str::from_utf8(entry.path())
                    .map_err(|_| SupervisorError::Tampered("artifact path".into()))?;
                let mut parent = std::path::Path::new(relative).parent();
                while let Some(path) = parent {
                    if path.as_os_str().is_empty() {
                        break;
                    }
                    std::fs::set_permissions(
                        materialized.join(path),
                        std::fs::Permissions::from_mode(0o755),
                    )?;
                    parent = path.parent();
                }
            }
        }
        let mut job = docker::Job::new(&self.cli, generation, admission);
        let mut authorize = authorize;
        if self.resources.is_some() {
            job.capacity(self.resource_policy()?, cancel, &mut authorize)?;
        }
        let result = job.cli_trial(
            spec,
            &materialized,
            artifact.output_id().as_str(),
            arguments,
            stdin,
            cancel,
            authorize,
        );
        job.cleanup()?;
        drop(job);
        if cancel.load(Ordering::SeqCst) {
            return Err(SupervisorError::Cancelled);
        }
        result
    }
}
/// Not deserializable or cloneable. Only a successfully observed, stopped build
/// can construct this capability; an artifact descriptor alone cannot do so.
#[derive(Debug)]
pub struct SealedArtifact {
    pub(crate) id: RecordId,
    pub(crate) record: ObjectDigest,
    pub(crate) output: artifact::Output,
    pub(crate) execution: RecordId,
    pub(crate) generation: String,
    pub(crate) runtime: String,
    pub(crate) epoch: crate::local_authority::Epoch,
}
impl SealedArtifact {
    pub fn id(&self) -> &RecordId {
        &self.id
    }
    pub fn output_id(&self) -> aethyme_contracts::experimental_v0::SourceSnapshotId {
        self.output.manifest.id()
    }
    pub fn execution(&self) -> &RecordId {
        &self.execution
    }
    pub fn record_object(&self) -> ObjectDigest {
        self.record
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub fn runtime(&self) -> &str {
        &self.runtime
    }
    pub(crate) fn verify_record(
        &self,
        store: &CollaborationStore,
        execution: &ExecutionSnapshot,
    ) -> Result<(), SupervisorError> {
        let packaging = format!(
            "sha256:{}",
            ObjectDigest::of(&serde_json::to_vec(&execution.spec().build)?).hex()
        );
        let record=Record::decode(&serde_json::to_vec(&serde_json::json!({"schema":ARTIFACT_SCHEMA,"execution":execution.id().as_str(),"target_revision":execution.target_revision().as_str(),"output":self.output_id().as_str(),"packaging":packaging,"runtime":self.runtime,"generation":self.generation}))?,&[&SCHEMA]).map_err(|e|SupervisorError::Tampered(e.to_string()))?;
        if record.id() != self.id
            || archive::read_object(store, &self.record)? != record.canonical_bytes()
        {
            return Err(SupervisorError::Tampered("artifact record mismatch".into()));
        }
        Ok(())
    }
    pub fn authority_epoch(&self) -> &crate::local_authority::Epoch {
        &self.epoch
    }
}
#[derive(Debug)]
pub struct BuildObservation {
    outcome: Outcome,
    artifact: Option<SealedArtifact>,
    generation: String,
    exit: Option<i32>,
    detail: Option<String>,
}
impl BuildObservation {
    pub fn outcome(&self) -> Outcome {
        self.outcome
    }
    pub fn artifact(&self) -> Option<&SealedArtifact> {
        self.artifact.as_ref()
    }
    pub fn into_artifact(self) -> Option<SealedArtifact> {
        self.artifact
    }
    pub fn generation(&self) -> &str {
        &self.generation
    }
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }
    pub fn exit_code(&self) -> Option<i32> {
        self.exit
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct BuildObservationWitness {
    pub(crate) kind: BuildObservationWitnessKind,
    pub(crate) run_id: String,
    pub(crate) outcome: Option<Outcome>,
    pub(crate) exit_code: Option<i32>,
    pub(crate) detail: Option<String>,
    pub(crate) artifact_present: bool,
}

#[cfg(test)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum BuildObservationWitnessKind {
    Observation,
    Error,
}

#[cfg(test)]
impl BuildObservationWitness {
    fn capture(result: &Result<BuildObservation, SupervisorError>, run_id: &str) -> Self {
        let bounded = |text: &str| text.chars().take(512).collect();
        match result {
            Ok(observation) => Self {
                kind: BuildObservationWitnessKind::Observation,
                run_id: run_id.to_owned(),
                outcome: Some(observation.outcome()),
                exit_code: observation.exit_code(),
                detail: observation.detail().map(bounded),
                artifact_present: observation.artifact().is_some(),
            },
            Err(error) => Self {
                kind: BuildObservationWitnessKind::Error,
                run_id: run_id.to_owned(),
                outcome: None,
                exit_code: None,
                detail: Some(bounded(&error.to_string())),
                artifact_present: false,
            },
        }
    }
}

fn active(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
) -> Result<crate::local_authority::Guard, SupervisorError> {
    let guard = target::authorize(store, owner)?;
    require_active(store, owner, execution, &guard)?;
    Ok(guard)
}

fn require_active(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    execution: &ExecutionSnapshot,
    _authority: &crate::local_authority::Guard,
) -> Result<(), SupervisorError> {
    let status = target::status(store, owner.target())?;
    if status.revision.as_ref() != Some(execution.target_revision())
        || status.phase != target::Phase::Verifying
    {
        return Err(SupervisorError::Cancelled);
    }
    Ok(())
}
impl DockerExecutor {
    /// Execute once under a current finite budget attempt and host admission.
    pub fn build(
        &self,
        store: &mut CollaborationStore,
        owner: &ControllerOwner,
        execution: &ExecutionSnapshot,
        attempt: &crate::local_budget::Attempt,
        cancel: &AtomicBool,
    ) -> Result<BuildObservation, SupervisorError> {
        self.build_with_pre_finish_observer(store, owner, execution, attempt, cancel, |_, _| {})
    }

    #[cfg(test)]
    pub(crate) fn test_build_with_pre_finish_observer(
        &self,
        store: &mut CollaborationStore,
        owner: &ControllerOwner,
        execution: &ExecutionSnapshot,
        attempt: &crate::local_budget::Attempt,
        cancel: &AtomicBool,
        witness: std::sync::mpsc::SyncSender<BuildObservationWitness>,
    ) -> Result<BuildObservation, SupervisorError> {
        self.build_with_pre_finish_observer(
            store,
            owner,
            execution,
            attempt,
            cancel,
            move |result, run_id| {
                witness
                    .send(BuildObservationWitness::capture(result, run_id))
                    .unwrap_or(());
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_with_pre_finish_observer<F>(
        &self,
        store: &mut CollaborationStore,
        owner: &ControllerOwner,
        execution: &ExecutionSnapshot,
        attempt: &crate::local_budget::Attempt,
        cancel: &AtomicBool,
        before_finish: F,
    ) -> Result<BuildObservation, SupervisorError>
    where
        F: FnOnce(&Result<BuildObservation, SupervisorError>, &str),
    {
        active(store, owner, execution)?;
        let policy = self.resource_policy()?;
        let generation: String = store
            .read_connection()
            .query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))
            .map_err(target::TargetError::from)?;
        let envelope = crate::execution_resources::build_envelope(execution)?;
        let mut run = crate::execution_resources::Run::admit(
            policy,
            store,
            owner,
            execution,
            attempt,
            generation.clone(),
            self.cli.endpoint.clone(),
            envelope,
        )?;
        let result = self.build_inner(
            store,
            owner,
            execution,
            cancel,
            generation,
            crate::execution_resources::Admission::Bound(&mut run),
            Some(attempt),
        );
        if run
            .journal
            .objects
            .iter()
            .all(|object| object.removed && object.id.is_some())
        {
            run.cleaned()?;
            before_finish(&result, &run.journal.run);
            run.finish(
                store,
                owner,
                attempt,
                &format!("{:?}", result.as_ref().map(|v| v.outcome())),
            )?;
        } else {
            crate::local_budget::execution_unknown(
                store,
                owner,
                attempt,
                "unconfirmed exact execution teardown",
            )?;
        }
        result
    }
    #[cfg(test)]
    pub(crate) fn test_build(
        &self,
        store: &mut CollaborationStore,
        owner: &ControllerOwner,
        execution: &ExecutionSnapshot,
        cancel: &AtomicBool,
    ) -> Result<BuildObservation, SupervisorError> {
        let generation: String = store
            .read_connection()
            .query_row("SELECT lower(hex(randomblob(32)))", [], |r| r.get(0))
            .map_err(target::TargetError::from)?;
        self.build_inner(
            store,
            owner,
            execution,
            cancel,
            generation,
            crate::execution_resources::Admission::ProfileOnly,
            None,
        )
    }
    #[allow(clippy::too_many_arguments)]
    fn build_inner(
        &self,
        store: &mut CollaborationStore,
        owner: &ControllerOwner,
        execution: &ExecutionSnapshot,
        cancel: &AtomicBool,
        generation: String,
        admission: crate::execution_resources::Admission<'_>,
        attempt: Option<&crate::local_budget::Attempt>,
    ) -> Result<BuildObservation, SupervisorError> {
        active(store, owner, execution)?;
        let spec = execution.spec();
        // First profile performs only raw materialization. Prepared output is
        // data until an independently selected preparation recipe is observed.
        if spec.preparation.is_some() {
            return Err(SupervisorError::Unsupported(
                "unwitnessed preparation".into(),
            ));
        }
        if spec.profile.runtime_profile != self.runtime.id()
            || spec.profile.architecture != self.runtime.architecture
            || docker::probe(&self.cli)? != self.runtime
        {
            return Err(SupervisorError::Unsupported("runtime/profile drift".into()));
        }
        docker::image(
            &self.cli,
            &spec.profile.build_image,
            &spec.profile.architecture,
        )?;
        let inputs = snapshot::materialize_in(
            store,
            owner,
            execution,
            self.resources.as_ref().map(|p| p.temporary_root.as_path()),
        )?;
        let mut job = docker::Job::new(&self.cli, &generation, admission);
        let mut authorize = || {
            let authority = active(store, owner, execution)?;
            let budget = if let Some(attempt) = attempt {
                Some(crate::local_budget::execution_guard(
                    store,
                    owner,
                    attempt,
                    execution,
                    crate::execution_resources::build_envelope(execution)?.units()?,
                )?)
            } else {
                None
            };
            Ok((authority, budget))
        };
        if self.resources.is_some() {
            job.capacity(self.resource_policy()?, cancel, &mut authorize)?;
        }
        let result = job.build(&inputs, cancel, &mut authorize);
        // Teardown must succeed; cancellation latched during export/cleanup
        // invalidates the result before a capability can be constructed.
        job.cleanup()?;
        drop(job);
        let result = if cancel.load(Ordering::SeqCst) {
            Err(SupervisorError::Cancelled)
        } else {
            result
        };
        let detail = result.as_ref().err().map(ToString::to_string);
        let (outcome, exit, output) = match result {
            Ok((exit, output)) if exit == 0 => (Outcome::Built, Some(exit), Some(output)),
            Ok((exit, _)) => (Outcome::CodeFailed, Some(exit), None),
            Err(SupervisorError::Cancelled) => (Outcome::Cancelled, None, None),
            Err(SupervisorError::TimedOut) => (Outcome::TimedOut, None, None),
            Err(SupervisorError::Tampered(_)) => (Outcome::TamperingDetected, None, None),
            Err(SupervisorError::Inconclusive(_)) => (Outcome::Inconclusive, None, None),
            Err(SupervisorError::Infrastructure(_) | SupervisorError::Limit) => {
                (Outcome::InfrastructureDeferred, None, None)
            }
            Err(error) => return Err(error),
        };
        let artifact = if let Some(output) = output {
            let archive_guard = crate::collaboration_gc::archive_use(store)?;
            let response_guard = crate::collaboration_sensitive::response_use(store)?;
            let authority_guard = target::authorize(store, owner)?;
            let retention = target::ArtifactRetentionPermit::new(
                store,
                owner,
                &archive_guard,
                &response_guard,
                &authority_guard,
            )?;
            require_active(store, owner, execution, &authority_guard)?;
            let packaging = format!(
                "sha256:{}",
                ObjectDigest::of(&serde_json::to_vec(&spec.build)?).hex()
            );
            let record=Record::decode(&serde_json::to_vec(&serde_json::json!({"schema":ARTIFACT_SCHEMA,"execution":execution.id().as_str(),"target_revision":execution.target_revision().as_str(),"output":output.manifest.id().as_str(),"packaging":packaging,"runtime":self.runtime.id(),"generation":generation}))?,&[&SCHEMA]).map_err(|e|SupervisorError::Tampered(e.to_string()))?;
            let mut objects = Vec::new();
            for bytes in output.files.values() {
                objects.push(archive::put_object(store, bytes)?);
            }
            objects.push(archive::put_object(
                store,
                &output.manifest.manifest_bytes(),
            )?);
            let physical = archive::put_object(store, &record.canonical_bytes())?;
            objects.push(physical);
            target::retain_artifacts_under_guards(
                store,
                execution.target_revision(),
                &[],
                &objects,
                &retention,
            )?;
            if cancel.load(Ordering::SeqCst) {
                return Ok(BuildObservation {
                    outcome: Outcome::Cancelled,
                    artifact: None,
                    generation,
                    exit: None,
                    detail: Some("cancelled before sealing".into()),
                });
            }
            Some(SealedArtifact {
                id: record.id(),
                record: physical,
                output,
                execution: execution.id().clone(),
                generation: generation.clone(),
                runtime: self.runtime.id(),
                epoch: owner.authority_epoch().clone(),
            })
        } else {
            None
        };
        Ok(BuildObservation {
            outcome,
            detail,
            artifact,
            generation,
            exit,
        })
    }
}
#[cfg(test)]
mod tests;
