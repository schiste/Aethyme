//! Host admission and durable exact-resource inventory for Local execution.
//! Capacity uses the existing host coordinator. Unknown daemon work keeps its
//! reservation; the host process disappearing is not proof of Docker teardown.
use crate::collaboration_archive::ObjectDigest;
use crate::collaboration_state::CollaborationStore;
use crate::execution_snapshot::ExecutionSnapshot;
use crate::local_budget::{self as budget, Attempt, Units};
use crate::local_target::{self as target, ControllerOwner};
use crate::resources::{
    HOST_RESOURCE_REQUEST_SCHEMA_VERSION, HostResourceCoordinator, HostResourceGrant,
    HostResourceKind, HostResourceRequest, HostResourceRequirement,
};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
const MIB: u64 = 1024 * 1024;
#[derive(Debug, thiserror::Error)]
pub enum ResourceError {
    #[error("infrastructure deferred: {0}")]
    Deferred(String),
    #[error("execution resource authority is stale or mismatched")]
    Stale,
    #[error("cross-candidate result reuse is disabled")]
    CacheDisabled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Host(#[from] crate::resources::HostResourceError),
    #[error(transparent)]
    Budget(#[from] budget::BudgetError),
    #[error(transparent)]
    Target(#[from] target::TargetError),
}
/// Appointed by the trusted host operator. Both paths must be absolute/private;
/// journals live beside host state, outside collaboration backup scope.
#[derive(Debug, Clone)]
pub struct ResourcePolicy {
    pub host_state: PathBuf,
    pub temporary_root: PathBuf,
    pub runtime: String,
    pub daemon: String,
    pub meter_image: String,
    pub memory_mib: u32,
    pub disk_mib: u32,
    pub process_slots: u32,
    pub job_slots: u32,
    pub host_floor_bytes: u64,
    pub daemon_floor_bytes: u64,
}
/// Conservative aggregate limits, including helpers and artifact/tar copies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Envelope {
    pub memory: u64,
    pub disk: u64,
    pub pids: u32,
    pub jobs: u32,
    pub time_ms: u64,
}
pub(crate) fn build_envelope(execution: &ExecutionSnapshot) -> Result<Envelope, ResourceError> {
    let limits = execution.spec().profile.build_limits;
    Ok(Envelope {
        memory: limits
            .memory_bytes
            .checked_add(576 * MIB + 4 * limits.output_bytes)
            .ok_or(ResourceError::Stale)?,
        disk: 1024 * MIB + 4 * limits.output_bytes,
        pids: limits.pids.checked_add(32).ok_or(ResourceError::Stale)?,
        jobs: 2,
        time_ms: limits
            .timeout_ms
            .checked_add(600_000)
            .ok_or(ResourceError::Stale)?,
    })
}
pub(crate) fn verify_envelope(execution: &ExecutionSnapshot) -> Result<Envelope, ResourceError> {
    let limits = execution.spec().profile.verifier_limits;
    // The retained inventory is materialized only after admission. Reserve the
    // profile's maximum 32 sequential mandatory trials, not guessed task size.
    Ok(Envelope {
        memory: limits
            .memory_bytes
            .checked_add(576 * MIB + 4 * limits.output_bytes)
            .ok_or(ResourceError::Stale)?,
        disk: 1024 * MIB + 4 * limits.output_bytes,
        pids: limits.pids.checked_add(32).ok_or(ResourceError::Stale)?,
        jobs: 2,
        time_ms: limits
            .timeout_ms
            .checked_add(240_000)
            .and_then(|n| n.checked_mul(32))
            .and_then(|n| n.checked_add(600_000))
            .ok_or(ResourceError::Stale)?,
    })
}
impl Envelope {
    pub(crate) fn units(self) -> Result<Units, ResourceError> {
        Ok(Units {
            time_ms: i64::try_from(self.time_ms).map_err(|_| ResourceError::Stale)?,
            memory_bytes: i64::try_from(self.memory).map_err(|_| ResourceError::Stale)?,
            disk_bytes: i64::try_from(self.disk).map_err(|_| ResourceError::Stale)?,
            slots: i64::from(self.jobs),
            ..Units::default()
        })
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ObjectKind {
    Container,
    Volume,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Object {
    pub kind: ObjectKind,
    pub name: String,
    pub id: Option<String>,
    pub generation: String,
    pub removed: bool,
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Journal {
    pub schema: u32,
    pub run: String,
    pub runtime: String,
    pub daemon: String,
    pub endpoint: String,
    pub execution: String,
    pub reservation: String,
    pub attempt: u32,
    pub epoch: String,
    pub envelope: Envelope,
    pub request: HostResourceRequest,
    pub grant: Option<HostResourceGrant>,
    pub objects: Vec<Object>,
    pub cleaned: bool,
    pub settled: bool,
}
fn private_directory(path: &Path) -> Result<(), ResourceError> {
    if !path.is_absolute() {
        return Err(ResourceError::Stale);
    }
    std::fs::create_dir_all(path)?;
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(ResourceError::Stale);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(ResourceError::Deferred(
                "resource directory must be private".into(),
            ));
        }
    }
    Ok(())
}
fn create_private_directory(path: &Path) -> Result<(), ResourceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let mut b = std::fs::DirBuilder::new();
        b.mode(0o700);
        match b.create(path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        };
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(path)?;
    private_directory(path)
}
fn safe_run(run: &str) -> bool {
    run.len() == 64
        && run
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
fn lock(root: &Path, run: &str) -> Result<File, ResourceError> {
    if !safe_run(run) {
        return Err(ResourceError::Stale);
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(root.join(format!("{run}.lock")))?;
    file.try_lock().map_err(|_| {
        ResourceError::Deferred("execution reader or cleanup owner is active".into())
    })?;
    Ok(file)
}
fn save(root: &Path, journal: &Journal) -> Result<(), ResourceError> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(&serde_json::to_vec(journal)?)?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(root.join(format!("{}.json", journal.run)))
        .map_err(|e| e.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}
fn host_floor(
    policy: &ResourcePolicy,
    store: &CollaborationStore,
    envelope: Envelope,
) -> Result<(), ResourceError> {
    let required = policy
        .host_floor_bytes
        .checked_add(u64::from(policy.disk_mib) * MIB)
        .ok_or(ResourceError::Stale)?;
    if envelope.disk > u64::from(policy.disk_mib) * MIB {
        return Err(ResourceError::Deferred(
            "declared disk pool is smaller than execution envelope".into(),
        ));
    }
    for path in [
        &policy.temporary_root,
        std::env::temp_dir().as_path(),
        store.project_dir(),
    ] {
        let available = crate::disk_headroom::available_bytes(path).ok_or_else(|| {
            ResourceError::Deferred("host disk capacity cannot be witnessed".into())
        })?;
        if available < required {
            return Err(ResourceError::Deferred("host disk floor".into()));
        }
    }
    Ok(())
}
fn request(
    policy: &ResourcePolicy,
    store: &CollaborationStore,
    run: &str,
    envelope: Envelope,
) -> Result<HostResourceRequest, ResourceError> {
    let pool = format!(
        "local-exec:{}",
        ObjectDigest::of(policy.daemon.as_bytes()).hex()
    );
    let capacity = |key: &str, units: u32, limit: u32| HostResourceRequirement {
        key: key.into(),
        resource: HostResourceKind::Capacity {
            pool: format!("{pool}:{key}"),
            units,
            limit,
        },
    };
    Ok(HostResourceRequest {
        schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!("local-exec:{run}"),
        repository: store.project().as_str().into(),
        worktree_fingerprint: ObjectDigest::of(store.project_dir().as_os_str().as_encoded_bytes())
            .hex(),
        run_id: run.into(),
        ttl_seconds: 3600,
        holder_pid: None,
        resources: vec![
            HostResourceRequirement {
                key: "native_memory".into(),
                resource: HostResourceKind::Capacity {
                    pool: "aethyme-local-native-memory".into(),
                    units: u32::try_from(envelope.memory.div_ceil(MIB))
                        .map_err(|_| ResourceError::Stale)?,
                    limit: policy.memory_mib,
                },
            },
            HostResourceRequirement {
                key: "native_disk".into(),
                resource: HostResourceKind::Capacity {
                    pool: "aethyme-local-native-disk".into(),
                    units: u32::try_from(envelope.disk.div_ceil(MIB))
                        .map_err(|_| ResourceError::Stale)?,
                    limit: policy.disk_mib,
                },
            },
            capacity(
                "memory",
                u32::try_from(envelope.memory.div_ceil(MIB)).map_err(|_| ResourceError::Stale)?,
                policy.memory_mib,
            ),
            capacity(
                "disk",
                u32::try_from(envelope.disk.div_ceil(MIB)).map_err(|_| ResourceError::Stale)?,
                policy.disk_mib,
            ),
            capacity("pids", envelope.pids, policy.process_slots),
            capacity("jobs", envelope.jobs, policy.job_slots),
        ],
    })
}
/// Non-deserializable live ownership. The lock protects cleanup against active
/// readers; only its daemon inventory is removable, never CAS/retained roots.
pub(crate) struct Run {
    root: PathBuf,
    _lock: File,
    coordinator: HostResourceCoordinator,
    pub journal: Journal,
    units: Units,
}
impl Run {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admit(
        policy: &ResourcePolicy,
        store: &CollaborationStore,
        owner: &ControllerOwner,
        execution: &ExecutionSnapshot,
        attempt: &Attempt,
        run: String,
        endpoint: String,
        envelope: Envelope,
    ) -> Result<Self, ResourceError> {
        let _authority = target::authorize(store, owner)?;
        if policy.runtime != execution.spec().profile.runtime_profile
            || !safe_run(&run)
            || policy.host_floor_bytes == 0
            || policy.daemon_floor_bytes == 0
            || !policy.host_state.is_absolute()
        {
            return Err(ResourceError::Stale);
        }
        private_directory(&policy.temporary_root)?;
        let parent = policy.host_state.parent().ok_or(ResourceError::Stale)?;
        private_directory(parent)?;
        let parent = parent.canonicalize()?;
        let project = store.project_dir().canonicalize()?;
        if parent.starts_with(&project)
            || policy.temporary_root.canonicalize()?.starts_with(&project)
        {
            return Err(ResourceError::Stale);
        }
        if let Ok(meta) = std::fs::symlink_metadata(&policy.host_state) {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return Err(ResourceError::Stale);
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if meta.nlink() != 1 {
                    return Err(ResourceError::Stale);
                }
            }
        }
        let root = parent.join("local-execution-journal");
        create_private_directory(&root)?;
        let units = envelope.units()?;
        budget::authorize_execution(store, owner, attempt, execution, units)?;
        host_floor(policy, store, envelope)?;
        let claim = ObjectDigest::of(
            format!(
                "{}:{}:{}",
                owner.authority_epoch().as_str(),
                attempt.reservation(),
                attempt.number()
            )
            .as_bytes(),
        )
        .hex();
        let claim_lock = lock(&root, &claim)?;
        let claim_path = root.join(format!("{claim}.claim"));
        if claim_path.exists() {
            return Err(ResourceError::Stale);
        }
        let lock = lock(&root, &run)?;
        if root.join(format!("{run}.json")).exists() {
            return Err(ResourceError::Stale);
        }
        let mut coordinator = HostResourceCoordinator::open(&policy.host_state)?;
        let request = request(policy, store, &run, envelope)?;
        let mut journal = Journal {
            schema: 1,
            run,
            runtime: policy.runtime.clone(),
            daemon: policy.daemon.clone(),
            endpoint,
            execution: execution.id().to_string(),
            reservation: attempt.reservation().to_string(),
            attempt: attempt.number(),
            epoch: owner.authority_epoch().as_str().into(),
            envelope,
            request,
            grant: None,
            objects: vec![],
            cleaned: false,
            settled: false,
        };
        // Intent precedes the atomic coordinator acquisition. A crash before
        // saving the grant is recoverable by the same exact request, not retry
        // of a Docker side effect.
        save(&root, &journal)?;
        let mut claim_file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&claim_path)?;
        claim_file.write_all(journal.run.as_bytes())?;
        claim_file.sync_all()?;
        File::open(&root)?.sync_all()?;
        drop(claim_lock);
        let grant = coordinator.acquire(&journal.request)?;
        journal.grant = Some(grant);
        save(&root, &journal)?;
        Ok(Self {
            root,
            _lock: lock,
            coordinator,
            journal,
            units,
        })
    }
    pub(crate) fn recover(policy: &ResourcePolicy, run: &str) -> Result<Self, ResourceError> {
        let root = policy
            .host_state
            .parent()
            .ok_or(ResourceError::Stale)?
            .join("local-execution-journal");
        private_directory(&root)?;
        let lock = lock(&root, run)?;
        let path = root.join(format!("{run}.json"));
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW);
        }
        let file = options.open(path)?;
        if file.metadata()?.len() > 1024 * 1024 {
            return Err(ResourceError::Stale);
        }
        let journal: Journal = serde_json::from_reader(file)?;
        if journal.schema != 1
            || journal.run != run
            || journal.daemon != policy.daemon
            || journal.request.run_id != run
            || journal.request.request_id != format!("local-exec:{run}")
            || journal.request.holder_pid.is_some()
        {
            return Err(ResourceError::Stale);
        }
        for object in &journal.objects {
            if !object.name.starts_with(&format!("aethyme-{run}-"))
                || !(object.generation == run || object.generation.starts_with(&format!("{run}-")))
            {
                return Err(ResourceError::Stale);
            }
        }
        let coordinator = HostResourceCoordinator::open(&policy.host_state)?;
        Ok(Self {
            root,
            _lock: lock,
            coordinator,
            units: journal.envelope.units()?,
            journal,
        })
    }
    /// Resource recovery does not reconcile financial/budget consumption. That
    /// remains pending until the appointed owner supplies a trusted receipt.
    pub(crate) fn release_clean(&mut self) -> Result<(), ResourceError> {
        if !self.journal.cleaned {
            return Err(ResourceError::Stale);
        }
        let grant = self.journal.grant.as_ref().ok_or(ResourceError::Deferred(
            "host acquisition outcome requires reconciliation".into(),
        ))?;
        self.coordinator.release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )?;
        Ok(())
    }
    pub(crate) fn intent(
        &mut self,
        kind: ObjectKind,
        name: String,
        generation: String,
    ) -> Result<(), ResourceError> {
        if !name.starts_with(&format!("aethyme-{}-", self.journal.run))
            || self.journal.objects.iter().any(|o| o.name == name)
        {
            return Err(ResourceError::Stale);
        }
        self.journal.objects.push(Object {
            kind,
            name,
            generation,
            id: None,
            removed: false,
        });
        save(&self.root, &self.journal)
    }
    pub(crate) fn created(&mut self, name: &str, id: String) -> Result<(), ResourceError> {
        let object = self
            .journal
            .objects
            .iter_mut()
            .find(|o| o.name == name && o.id.is_none())
            .ok_or(ResourceError::Stale)?;
        object.id = Some(id);
        save(&self.root, &self.journal)
    }
    pub(crate) fn removed(&mut self, kind: ObjectKind, id: &str) -> Result<(), ResourceError> {
        let object = self
            .journal
            .objects
            .iter_mut()
            .find(|o| o.kind == kind && o.id.as_deref() == Some(id))
            .ok_or(ResourceError::Stale)?;
        object.removed = true;
        save(&self.root, &self.journal)
    }
    pub(crate) fn cleaned(&mut self) -> Result<(), ResourceError> {
        if self
            .journal
            .objects
            .iter()
            .any(|o| !o.removed || o.id.is_none())
        {
            return Err(ResourceError::Deferred(
                "unknown daemon resource remains reserved".into(),
            ));
        }
        self.journal.cleaned = true;
        save(&self.root, &self.journal)
    }
    pub(crate) fn finish(
        &mut self,
        store: &mut CollaborationStore,
        owner: &ControllerOwner,
        attempt: &Attempt,
        result: &str,
    ) -> Result<(), ResourceError> {
        if !self.journal.cleaned || self.journal.settled {
            return Err(ResourceError::Stale);
        }
        use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec, Record, RecordSchema};
        static SCHEMA: RecordSchema = RecordSchema {
            name: "aethyme.execution-reservation-receipt/provisional-v0",
            fields: &[FieldSpec {
                name: "observation",
                required: true,
                kind: FieldKind::Opaque,
                capability: None,
            }],
            capabilities: &[],
        };
        let _authority = target::authorize(store, owner)?;
        let _archive = crate::collaboration_gc::archive_use(store)
            .map_err(|e| ResourceError::Deferred(e.to_string()))?;
        let record=Record::decode(&serde_json::to_vec(&serde_json::json!({"schema":SCHEMA.name,"observation":{"run":self.journal.run,"execution":self.journal.execution,"reservation":self.journal.reservation,"attempt":self.journal.attempt,"epoch":self.journal.epoch,"envelope":self.journal.envelope,"result":result,"accounting":"conservative-reserved-envelope"}}))?,&[&SCHEMA]).map_err(|e|ResourceError::Deferred(e.to_string()))?;
        let object = crate::collaboration_archive::put_object(store, &record.canonical_bytes())
            .map_err(|e| ResourceError::Deferred(e.to_string()))?;
        let revision = target::status(store, owner.target())?
            .revision
            .ok_or(ResourceError::Stale)?;
        target::retain_artifacts(store, owner, &revision, &[], &[object])?;
        self.settle(store, owner, attempt, &record.id())
    }
    pub(crate) fn settle(
        &mut self,
        store: &mut CollaborationStore,
        owner: &ControllerOwner,
        attempt: &Attempt,
        receipt: &aethyme_contracts::experimental_v0::RecordId,
    ) -> Result<(), ResourceError> {
        if !self.journal.cleaned || self.journal.settled {
            return Err(ResourceError::Stale);
        }
        budget::settle(store, owner, attempt, self.units, receipt)?;
        self.journal.settled = true;
        save(&self.root, &self.journal)?;
        let grant = self.journal.grant.as_ref().ok_or(ResourceError::Stale)?;
        self.coordinator.release(
            &grant.lease.lease_id,
            grant.lease.generation,
            &grant.ownership_token,
        )?;
        Ok(())
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        if !self.journal.cleaned
            && let Some(grant) = &self.journal.grant
        {
            crate::warn_unrecorded(
                "quarantine unknown Local execution capacity",
                self.coordinator.quarantine(
                    &grant.lease.lease_id,
                    grant.lease.generation,
                    &grant.ownership_token,
                ),
            );
        }
    }
}
/// Complete logical identity for a future native cache. It is diagnostic only:
/// the first authoritative profile always performs a new check.
#[derive(Debug, Serialize)]
pub struct CacheSubject<'a> {
    pub execution: &'a str,
    pub artifact: &'a str,
    pub producer: &'a str,
    pub producer_generation: u64,
    pub policy: &'a str,
    pub harness: &'a str,
    pub collection: &'a str,
    pub authority_epoch: &'a str,
    pub target_revision: &'a str,
    pub result_class: &'a str,
}
impl CacheSubject<'_> {
    pub fn key(&self) -> Result<String, ResourceError> {
        Ok(format!(
            "sha256:{}",
            ObjectDigest::of(&serde_json::to_vec(&serde_json::to_value(self)?)?).hex()
        ))
    }
    pub fn reuse(&self) -> Result<(), ResourceError> {
        Err(ResourceError::CacheDisabled)
    }
}
/// Production callers can only carry an admitted live run. Profile component
/// tests retain their older isolated schedules without claiming budget evidence.
pub(crate) enum Admission<'a> {
    Bound(&'a mut Run),
    #[cfg(test)]
    ProfileOnly,
}
impl Admission<'_> {
    pub(crate) fn reborrow(&mut self) -> Admission<'_> {
        match self {
            Self::Bound(run) => Admission::Bound(run),
            #[cfg(test)]
            Self::ProfileOnly => Admission::ProfileOnly,
        }
    }
    pub(crate) fn journal(&self) -> Option<&Journal> {
        match self {
            Self::Bound(run) => Some(&run.journal),
            #[cfg(test)]
            Self::ProfileOnly => None,
        }
    }
    pub(crate) fn run(&mut self) -> Option<&mut Run> {
        match self {
            Self::Bound(run) => Some(*run),
            #[cfg(test)]
            Self::ProfileOnly => None,
        }
    }
}
#[cfg(test)]
pub(crate) mod tests;
