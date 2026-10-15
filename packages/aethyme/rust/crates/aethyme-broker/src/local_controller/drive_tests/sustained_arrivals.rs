//! FX10 actual sustained-arrival qualification over six independent Playground
//! feature modules. The native run is deliberately ignored in ordinary CI.
use super::{
    FX17_IMAGE, RemoveGateTrust, fsync_report, git_static, git_text, hex, query_strings,
    read_commit_file, record_unwind, reservation_rows, save_event, sha256, source_digest,
    update_index,
};
use crate::Broker;
use crate::broker::gate_trust::{self, GatePolicy};
use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_state::{CollaborationRoot, CollaborationStore, ProjectKey};
use crate::execution_snapshot::{
    CommandSpec, ExecutionSpec, LinuxProfile, MATERIALIZATION_PROFILE, ResourceLimits,
};
use crate::execution_supervisor::DockerExecutor;
use crate::execution_verifier::{AssertionCase, AssertionInventory, VerifierPolicy};
use crate::local_budget::{self as budget, ProjectPolicy, Units};
use crate::local_controller::{DrivePolicy, ServiceOutcome, ServicePolicy, serve};
use crate::local_source_transport::GitCarrier;
use crate::local_target::{self as target, Phase, RequestedContribution, TargetSpec};
use crate::local_wave::{self as wave, Admission, WavePhase, WavePolicy};
use crate::resources::{
    HOST_RESOURCE_REQUEST_SCHEMA_VERSION, HostResourceCoordinator, HostResourceGrant,
    HostResourceKind, HostResourceRequest, HostResourceRequirement,
};
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FX10_MANIFEST_SHA: &str = "43a340981622f3f478aa1545b83f24bedf1acb994e95b12ce4b75760cd744066";
const FX10_CONTRACT_SHA: &str = "02ba1653f0017dabb2f92c896113d4b597af612da9c89d492e30a38a791b24c6";
const FX10_ORACLE_SHA: &str = "0caede81a00be34dc3dcb171bfbb714dca3fc0c10fa49f72c39437f728532954";
const FX10_PROXY_SHA: &str = "a8b7bd9e7269868883f679b1158c519dba44a374ca6f5f9a3c675470345b0f97";
const FX10_TRANSCRIPT_SHA: &str =
    "56d15d1c702fabd891f2764a8c68d9d96da4f99f1c039801678637151f9a503e";
const FX10_RESOURCE_POOL: &str = "fx10-sustained-arrival-v1";
const FX10_MIN_CONTENTION_MS: u64 = 250;
const FX10_OBSERVER_TIMEOUT_SECONDS: u64 = 30;
const FX10_TARGET: &str = "fx10-sustained-arrivals";
const FX10_DRIFT_TARGET: &str = "fx10-external-target-update";
const FX10_PROJECT: &str = "fx10-sustained-arrivals";
const FX10_SOURCE_FILES: [(&str, &[u8]); 8] = [
    (
        "source/app.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/app.sh"),
    ),
    (
        "source/.aethyme/gates.toml",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/.aethyme/gates.toml"),
    ),
    (
        "source/features/alpha.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/features/alpha.sh"),
    ),
    (
        "source/features/beta.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/features/beta.sh"),
    ),
    (
        "source/features/gamma.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/features/gamma.sh"),
    ),
    (
        "source/features/delta.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/features/delta.sh"),
    ),
    (
        "source/features/epsilon.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/features/epsilon.sh"),
    ),
    (
        "source/features/zeta.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/features/zeta.sh"),
    ),
];
const FX10_PUBLIC_CONTRACT: &[u8] =
    include_bytes!("../../../../../../../aethyme-eval/fx10/public-contract.md");
const FX10_MANIFEST: &[u8] =
    include_bytes!("../../../../../../../aethyme-eval/fx10/proposal-manifest.json");
const FX10_ORACLE: &[u8] =
    include_bytes!("../../../../../../../aethyme-eval/fx10/oracle/cases.json");
const FX10_PROXY: &str = include_str!("../../../../../../../aethyme-eval/fx10/oracle/proxy.sh");
const FX10_EXPECTED_TRANSCRIPT: &[u8] =
    include_bytes!("../../../../../../../aethyme-eval/fx10/oracle/expected-transcript.txt");

struct Submission {
    id: &'static str,
    path: &'static str,
    bytes: &'static [u8],
    requires: &'static [&'static str],
    atomic_group: Option<&'static str>,
}
const SUBMISSIONS: [Submission; 6] = [
    Submission {
        id: "A",
        path: "features/alpha.sh",
        bytes: include_bytes!("../../../../../../../aethyme-eval/fx10/contributions/A-alpha.sh"),
        requires: &[],
        atomic_group: Some("older-route-group"),
    },
    Submission {
        id: "B",
        path: "features/beta.sh",
        bytes: include_bytes!("../../../../../../../aethyme-eval/fx10/contributions/B-beta.sh"),
        requires: &["A"],
        atomic_group: Some("older-route-group"),
    },
    Submission {
        id: "C",
        path: "features/gamma.sh",
        bytes: include_bytes!("../../../../../../../aethyme-eval/fx10/contributions/C-gamma.sh"),
        requires: &["B"],
        atomic_group: Some("older-route-group"),
    },
    Submission {
        id: "D",
        path: "features/delta.sh",
        bytes: include_bytes!("../../../../../../../aethyme-eval/fx10/contributions/D-delta.sh"),
        requires: &[],
        atomic_group: None,
    },
    Submission {
        id: "E",
        path: "features/epsilon.sh",
        bytes: include_bytes!("../../../../../../../aethyme-eval/fx10/contributions/E-epsilon.sh"),
        requires: &[],
        atomic_group: None,
    },
    Submission {
        id: "F",
        path: "features/zeta.sh",
        bytes: include_bytes!("../../../../../../../aethyme-eval/fx10/contributions/F-zeta.sh"),
        requires: &[],
        atomic_group: None,
    },
];

fn now_ms() -> Result<i64, String> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| error.to_string())?
            .as_millis(),
    )
    .map_err(|error| error.to_string())
}

fn fixture_files() -> Vec<(&'static str, &'static [u8])> {
    let mut files = FX10_SOURCE_FILES.to_vec();
    files.extend([
        ("public-contract.md", FX10_PUBLIC_CONTRACT),
        ("contributions/A-alpha.sh", SUBMISSIONS[0].bytes),
        ("contributions/B-beta.sh", SUBMISSIONS[1].bytes),
        ("contributions/C-gamma.sh", SUBMISSIONS[2].bytes),
        ("contributions/D-delta.sh", SUBMISSIONS[3].bytes),
        ("contributions/E-epsilon.sh", SUBMISSIONS[4].bytes),
        ("contributions/F-zeta.sh", SUBMISSIONS[5].bytes),
        ("oracle/proxy.sh", FX10_PROXY.as_bytes()),
        ("oracle/cases.json", FX10_ORACLE),
        ("oracle/expected-transcript.txt", FX10_EXPECTED_TRANSCRIPT),
    ]);
    files
}

fn check_frozen_inputs() -> Result<(Vec<Value>, String, Value), String> {
    if sha256(FX10_MANIFEST) != FX10_MANIFEST_SHA
        || sha256(FX10_PUBLIC_CONTRACT) != FX10_CONTRACT_SHA
        || sha256(FX10_ORACLE) != FX10_ORACLE_SHA
        || sha256(FX10_PROXY.as_bytes()) != FX10_PROXY_SHA
        || sha256(FX10_EXPECTED_TRANSCRIPT) != FX10_TRANSCRIPT_SHA
    {
        return Err(
            "frozen FX10 contract, manifest, oracle, proxy or transcript digest mismatch".into(),
        );
    }
    let manifest: Value =
        serde_json::from_slice(FX10_MANIFEST).map_err(|error| error.to_string())?;
    if manifest["profile"] != "aethyme.fx10-sustained-arrivals/actual-v1"
        || manifest["oracle"]["case_count"] != 16
        || manifest["arrival_schedule"]["new_contributions_per_wave_max"] != 3
        || manifest["arrival_schedule"]["cumulative_selected_max"] != 6
        || manifest["arrival_schedule"]["ordinary_arrivals_may_cancel_active_wave"] != false
        || manifest["wave_policy"]["batch_width"] != 3
        || manifest["wave_policy"]["max_group"] != 6
        || manifest["resource_contention"]["pool"] != FX10_RESOURCE_POOL
        || manifest["resource_contention"]["units_held"] != 1
        || manifest["resource_contention"]["limit"] != 1
        || manifest["resource_contention"]["gate_wait_seconds"] != 5
        || manifest["resource_contention"]["holder_ttl_seconds"] != 60
        || manifest["resource_contention"]["minimum_wait_while_native_dispatching_ms"]
            != FX10_MIN_CONTENTION_MS
        || manifest["resource_contention"]["observer_timeout_seconds"]
            != FX10_OBSERVER_TIMEOUT_SECONDS
        || manifest["resource_contention"]["second_admission_required"] != true
        || manifest["target_update_schedule"]["target"] != FX10_DRIFT_TARGET
        || manifest["target_update_schedule"]["separate_target_and_bare_remote"] != true
        || manifest["target_update_schedule"]["external_update_count"] != 1
        || manifest["target_update_schedule"]["external_update_keeps_tree_unchanged"] != true
        || manifest["target_update_schedule"]["minimum_resource_contention_ms"]
            != FX10_MIN_CONTENTION_MS
        || manifest["target_update_schedule"]["observer_timeout_seconds"]
            != FX10_OBSERVER_TIMEOUT_SECONDS
        || manifest["target_update_schedule"]["private_completions"] != 0
        || manifest["target_update_schedule"]["automatic_retries"] != 0
        || manifest["target_update_schedule"]["service"]["max_ticks"] != 1
        || manifest["target_update_schedule"]["service"]["max_completions"] != 1
        || manifest["target_update_schedule"]["service"]["max_elapsed_ms"] != 180_000
        || manifest["service_allowance"]["max_ticks"] != 8
        || manifest["service_allowance"]["max_completions"] != 2
        || manifest["service_allowance"]["max_elapsed_ms"] != 180_000
        || manifest["service_allowance"]["max_drive_wall_ms"] != 120_000
    {
        return Err("FX10 frozen manifest does not describe the reviewed finite schedule".into());
    }
    for (path, bytes) in fixture_files() {
        if manifest["files"][path] != sha256(bytes) {
            return Err(format!("FX10 source digest mismatch for {path}"));
        }
    }
    let oracle: Vec<Value> =
        serde_json::from_slice(FX10_ORACLE).map_err(|error| error.to_string())?;
    if oracle.len() != 16 {
        return Err(format!(
            "expected 16 independent FX10 vectors, found {}",
            oracle.len()
        ));
    }
    let mut ids = BTreeSet::new();
    let mut transcript = String::from("FX10V1\n");
    for case in &oracle {
        let id = case["id"].as_str().ok_or("FX10 oracle case has no id")?;
        let exit = case["exit_code"]
            .as_u64()
            .ok_or("FX10 oracle case has no exit code")?;
        let stdout = case["stdout"]
            .as_str()
            .ok_or("FX10 oracle case has no stdout")?;
        let stderr = case["stderr"]
            .as_str()
            .ok_or("FX10 oracle case has no stderr")?;
        if !ids.insert(id.to_owned()) || case["stdin_hex"] != "" {
            return Err(format!(
                "FX10 oracle case {id} is repeated or has unsupported stdin"
            ));
        }
        let stdout_hex = if stdout.is_empty() {
            "-".into()
        } else {
            hex(stdout.as_bytes())
        };
        let stderr_hex = if stderr.is_empty() {
            "-".into()
        } else {
            hex(stderr.as_bytes())
        };
        transcript.push_str(&format!("{id}\t{exit}\t{stdout_hex}\t{stderr_hex}\n"));
    }
    if transcript.as_bytes() != FX10_EXPECTED_TRANSCRIPT {
        return Err("FX10 expected transcript is not the frozen-oracle encoding".into());
    }
    Ok((oracle, transcript, manifest))
}

fn write_playground(repo: &Path) -> Result<String, String> {
    for (fixture_path, bytes) in FX10_SOURCE_FILES {
        let relative = Path::new(fixture_path)
            .strip_prefix("source")
            .map_err(|error| error.to_string())?;
        let path = repo.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(path, bytes).map_err(|error| error.to_string())?;
    }
    git_static(repo, &["init", "-q", "-b", "main"])?;
    git_static(repo, &["add", "-A"])?;
    git_static(
        repo,
        &["commit", "-qm", "FX10 frozen route contract baseline"],
    )?;
    git_static(repo, &["rev-parse", "HEAD"])
}

fn retain_submissions(
    store: &mut CollaborationStore,
    repo: &Path,
    baseline: &CommitOid,
    baseline_snapshot: &str,
    index: &Path,
) -> Result<Vec<(String, String, String)>, String> {
    let mut retained = Vec::with_capacity(SUBMISSIONS.len());
    for submission in &SUBMISSIONS {
        let commit = update_index(
            repo,
            index,
            baseline.as_str(),
            &[(submission.path, submission.bytes)],
            &format!("FX10 {} independent route feature update", submission.id),
        )?;
        let changed = git_text(
            repo,
            &[
                "diff-tree".into(),
                "--no-commit-id".into(),
                "--name-only".into(),
                "-r".into(),
                baseline.as_str().into(),
                commit.clone(),
            ],
            None,
            None,
        )?;
        if changed.lines().collect::<Vec<_>>() != [submission.path] {
            return Err(format!(
                "FX10 {} must change only {}; got {:?}",
                submission.id,
                submission.path,
                changed.lines().collect::<Vec<_>>()
            ));
        }
        let commit_oid = CommitOid::parse(&commit).map_err(|error| error.to_string())?;
        let contribution = archive::retain_contribution(store, repo, baseline, &commit_oid)
            .map_err(|error| error.to_string())?;
        if contribution.base.snapshot_id.as_str() != baseline_snapshot {
            return Err(format!(
                "FX10 {} retained a different baseline",
                submission.id
            ));
        }
        retained.push((
            submission.id.into(),
            contribution.lineage_record_id.to_string(),
            commit,
        ));
    }
    Ok(retained)
}

fn requested_contributions(retained: &[(String, String, String)]) -> Vec<RequestedContribution> {
    retained
        .iter()
        .zip(SUBMISSIONS.iter())
        .map(|((id, lineage, _), submission)| RequestedContribution {
            id: id.clone(),
            lineage: lineage.clone(),
            requires: submission.requires.iter().map(|id| (*id).into()).collect(),
            atomic_group: submission.atomic_group.map(str::to_owned),
            revision_of: None,
            derived_from: Vec::new(),
        })
        .collect()
}

fn make_inventory(oracle: &[Value], transcript: &str) -> Result<AssertionInventory, String> {
    if oracle.len() != 16 {
        return Err("FX10 verifier inventory requires the complete frozen vector set".into());
    }
    AssertionInventory::new(vec![AssertionCase {
        id: "fx10-independent-route-contract".into(),
        arguments: Vec::new(),
        stdin_hex: String::new(),
        exit_code: 0,
        stdout: source_digest(transcript.as_bytes()),
        stderr: source_digest(b""),
    }])
    .map_err(|error| error.to_string())
}

fn hold_gate_resource(host: &Path, run_label: &str) -> Result<ContendedResource, String> {
    let digest = ObjectDigest::of(host.as_os_str().as_encoded_bytes()).hex();
    let suffix = &digest[..24];
    let request = HostResourceRequest {
        schema_version: HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!("fx10-test-holder-{run_label}-{suffix}"),
        repository: FX10_PROJECT.into(),
        worktree_fingerprint: digest.clone(),
        run_id: format!("fx10-holder-{run_label}-{suffix}"),
        ttl_seconds: 60,
        holder_pid: Some(std::process::id()),
        resources: vec![HostResourceRequirement {
            key: "fx10_sustained_arrival_slot".into(),
            resource: HostResourceKind::Capacity {
                pool: FX10_RESOURCE_POOL.into(),
                units: 1,
                limit: 1,
            },
        }],
    };
    let mut coordinator =
        HostResourceCoordinator::open_default().map_err(|error| error.to_string())?;
    let grant = coordinator
        .acquire(&request)
        .map_err(|error| error.to_string())?;
    Ok(ContendedResource {
        coordinator,
        request,
        grant,
        released: false,
    })
}

struct ContendedResource {
    coordinator: HostResourceCoordinator,
    request: HostResourceRequest,
    grant: HostResourceGrant,
    released: bool,
}
impl ContendedResource {
    fn is_active(&self) -> Result<bool, String> {
        self.coordinator
            .list(false)
            .map(|leases| {
                leases.iter().any(|lease| {
                    lease.lease_id == self.grant.lease.lease_id && lease.state.as_str() == "active"
                })
            })
            .map_err(|error| error.to_string())
    }
    fn release(&mut self) -> Result<crate::resources::HostResourceLease, String> {
        let lease = self
            .coordinator
            .release(
                &self.grant.lease.lease_id,
                self.grant.lease.generation,
                &self.grant.ownership_token,
            )
            .map_err(|error| error.to_string())?;
        self.released = true;
        if lease.state.as_str() != "released" {
            return Err("FX10 exact capacity lease did not reach released state".into());
        }
        Ok(lease)
    }

    fn typed_hold(&self) -> Value {
        json!({
            "request": self.request,
            "lease": self.grant.lease,
            "resource": {
                "kind": "capacity",
                "key": "fx10_sustained_arrival_slot",
                "pool": FX10_RESOURCE_POOL,
                "units": 1,
                "limit": 1,
                "ttl_seconds": 60
            }
        })
    }
}
impl Drop for ContendedResource {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.coordinator.release(
                &self.grant.lease.lease_id,
                self.grant.lease.generation,
                &self.grant.ownership_token,
            );
        }
    }
}

struct ArrivalObservation {
    native_job: String,
    dispatch_seen_unix_ms: i64,
    arrivals: Vec<(String, i64)>,
    observer_polls: u64,
    polls_while_resource_held: u64,
    contention_window_ms: u128,
    typed_holder: Value,
    released_holder: crate::resources::HostResourceLease,
    holder_release_unix_ms: i64,
    target_phase_before: String,
    target_phase_after: String,
    active_wave_phase_after: String,
    active_wave_selection_after: Vec<String>,
    completions_before_release: i64,
}

fn dispatching_native_job(
    store: &CollaborationStore,
    owner: &target::ControllerOwner,
    wave_id: &aethyme_contracts::experimental_v0::RecordId,
) -> Result<(String, u64), String> {
    let deadline = Instant::now() + Duration::from_secs(FX10_OBSERVER_TIMEOUT_SECONDS);
    let mut polls = 0_u64;
    loop {
        polls = polls.saturating_add(1);
        let job: Option<String> = store
            .read_connection()
            .query_row(
                "SELECT id FROM local_native_gate_jobs WHERE target=?1 AND wave=?2 AND state='dispatching' LIMIT 1",
                (owner.target(), wave_id.as_str()),
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(job) = job {
            return Ok((job, polls));
        }
        if Instant::now() >= deadline {
            return Err("bounded observer did not see the actual native gate dispatch".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn await_minimum_contention_window(
    store: &CollaborationStore,
    owner: &target::ControllerOwner,
    wave_id: &aethyme_contracts::experimental_v0::RecordId,
    job_id: &str,
    hold: &ContendedResource,
    window: Duration,
) -> Result<u64, String> {
    let deadline = Instant::now() + Duration::from_secs(FX10_OBSERVER_TIMEOUT_SECONDS);
    let until = Instant::now() + window;
    let mut polls = 0_u64;
    while Instant::now() < until {
        polls = polls.saturating_add(1);
        let job = crate::native_gates::load(
            store,
            &aethyme_contracts::experimental_v0::RecordId::parse(job_id)
                .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        if job.spec.wave != wave_id.as_str() || job.state != "dispatching" {
            return Err(
                "FX10 native gate left dispatching before the bounded contention window".into(),
            );
        }
        if !hold.is_active()? {
            return Err("FX10 capacity holder was not active during native dispatch".into());
        }
        if Instant::now() >= deadline {
            return Err("bounded resource contention window expired".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(polls)
}

fn observe_sustained_arrivals(
    host: PathBuf,
    owner: &target::ControllerOwner,
    target: String,
    active_wave: aethyme_contracts::experimental_v0::RecordId,
    mut hold: ContendedResource,
) -> Result<ArrivalObservation, String> {
    let root = CollaborationRoot::under_host_state(&host);
    let project = ProjectKey::parse(FX10_PROJECT).map_err(|error| error.to_string())?;
    let mut observer =
        CollaborationStore::open(&root, &project, &[]).map_err(|error| error.to_string())?;
    let (job_id, mut observer_polls) = dispatching_native_job(&observer, owner, &active_wave)?;
    let job = crate::native_gates::load(
        &observer,
        &aethyme_contracts::experimental_v0::RecordId::parse(&job_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if job.spec.wave != active_wave.as_str() || job.state != "dispatching" {
        return Err("FX10 observer matched a different or stale native job".into());
    }
    let before_wave = wave::load(&observer, &active_wave).map_err(|error| error.to_string())?;
    let before_target = target::status(&observer, &target).map_err(|error| error.to_string())?;
    let completions_before: i64 = observer
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_completions WHERE target=?1",
            [&target],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if before_wave.phase != WavePhase::Verifying
        || sorted(&before_wave.spec.selected) != ["A", "B", "C"]
        || before_target.phase != Phase::Verifying
        || completions_before != 0
        || !hold.is_active()?
    {
        return Err("FX10 older wave or real host resource contention was not active".into());
    }
    let dispatch_seen_unix_ms = now_ms()?;
    let mut arrivals = Vec::with_capacity(3);
    for id in ["D", "E", "F"] {
        let at = now_ms()?;
        wave::available(&mut observer, owner, id, at).map_err(|error| error.to_string())?;
        arrivals.push((id.into(), at));
        observer_polls = observer_polls.saturating_add(1);
        let current = wave::load(&observer, &active_wave).map_err(|error| error.to_string())?;
        let target_now = target::status(&observer, &target).map_err(|error| error.to_string())?;
        if current.phase != WavePhase::Verifying
            || sorted(&current.spec.selected) != ["A", "B", "C"]
            || target_now.phase != Phase::Verifying
        {
            return Err(
                "ordinary later arrivals changed or cancelled the active older wave".into(),
            );
        }
    }
    let contention_started = Instant::now();
    let polls_while_resource_held = await_minimum_contention_window(
        &observer,
        owner,
        &active_wave,
        &job_id,
        &hold,
        Duration::from_millis(FX10_MIN_CONTENTION_MS),
    )?;
    let active = wave::load(&observer, &active_wave).map_err(|error| error.to_string())?;
    let target_after = target::status(&observer, &target).map_err(|error| error.to_string())?;
    if active.phase != WavePhase::Verifying
        || sorted(&active.spec.selected) != ["A", "B", "C"]
        || target_after.phase != Phase::Verifying
    {
        return Err("bounded continued arrivals did not preserve the active wave".into());
    }
    let typed_holder = hold.typed_hold();
    let released_holder = hold.release()?;
    let holder_release_unix_ms = released_holder
        .released_at
        .ok_or_else(|| "FX10 resource holder release lacks a durable timestamp".to_string())?;
    Ok(ArrivalObservation {
        native_job: job_id,
        dispatch_seen_unix_ms,
        arrivals,
        observer_polls: observer_polls.saturating_add(polls_while_resource_held),
        polls_while_resource_held,
        contention_window_ms: contention_started.elapsed().as_millis(),
        typed_holder,
        released_holder,
        holder_release_unix_ms,
        target_phase_before: format!("{:?}", before_target.phase),
        target_phase_after: format!("{:?}", target_after.phase),
        active_wave_phase_after: format!("{:?}", active.phase),
        active_wave_selection_after: active.spec.selected,
        completions_before_release: completions_before,
    })
}

struct DriftObservation {
    native_job: String,
    dispatch_seen_unix_ms: i64,
    observer_polls: u64,
    polls_while_resource_held: u64,
    contention_window_ms: u128,
    typed_holder: Value,
    released_holder: crate::resources::HostResourceLease,
    holder_release_unix_ms: i64,
    remote_tip_before: String,
    remote_tip_after: String,
    remote_tree_before: String,
    remote_tree_after: String,
    target_advanced_unix_ms: i64,
    target_phase_after: String,
    active_wave_phase_after: String,
    completions_before_update: i64,
}

fn observe_external_ref_advance(
    host: PathBuf,
    repo: PathBuf,
    remote: PathBuf,
    owner: &target::ControllerOwner,
    active_wave: aethyme_contracts::experimental_v0::RecordId,
    baseline_commit: String,
    advanced_commit: String,
    mut hold: ContendedResource,
) -> Result<DriftObservation, String> {
    let root = CollaborationRoot::under_host_state(&host);
    let project = ProjectKey::parse(FX10_PROJECT).map_err(|error| error.to_string())?;
    let observer =
        CollaborationStore::open(&root, &project, &[]).map_err(|error| error.to_string())?;
    let (job_id, observer_polls) = dispatching_native_job(&observer, owner, &active_wave)?;
    let job = crate::native_gates::load(
        &observer,
        &aethyme_contracts::experimental_v0::RecordId::parse(&job_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if job.spec.wave != active_wave.as_str() || job.state != "dispatching" {
        return Err("FX10 target-update observer matched a stale native job".into());
    }
    let before_wave = wave::load(&observer, &active_wave).map_err(|error| error.to_string())?;
    let before = target::status(&observer, owner.target()).map_err(|error| error.to_string())?;
    if before_wave.phase != WavePhase::Verifying
        || before.phase != Phase::Verifying
        || sorted(&before_wave.spec.selected) != ["A", "B", "C"]
        || !hold.is_active()?
    {
        return Err(
            "FX10 external ref update did not begin during the exact older dispatch".into(),
        );
    }
    let completions_before_update: i64 = observer
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_completions WHERE target=?1",
            [owner.target()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if completions_before_update != 0 {
        return Err("FX10 drift target had a completion before the controlled update".into());
    }
    let remote_tip_before = git_static(&remote, &["rev-parse", "refs/heads/accepted"])?;
    if remote_tip_before != baseline_commit {
        return Err("FX10 drift remote changed before the controlled observer window".into());
    }
    let dispatch_seen_unix_ms = now_ms()?;
    let contention_started = Instant::now();
    let polls_while_resource_held = await_minimum_contention_window(
        &observer,
        owner,
        &active_wave,
        &job_id,
        &hold,
        Duration::from_millis(FX10_MIN_CONTENTION_MS),
    )?;
    let target_advanced_unix_ms = now_ms()?;
    git_static(
        &remote,
        &[
            "update-ref",
            "-m",
            "FX10 controlled external target advance",
            "refs/heads/accepted",
            &advanced_commit,
            &baseline_commit,
        ],
    )?;
    let remote_tip_after = git_static(&remote, &["rev-parse", "refs/heads/accepted"])?;
    if remote_tip_after != advanced_commit {
        return Err("FX10 controlled target update did not advance the exact bare ref".into());
    }
    let remote_tree_before = git_static(
        &remote,
        &["rev-parse", &format!("{remote_tip_before}^{{tree}}")],
    )?;
    let remote_tree_after = git_static(
        &remote,
        &["rev-parse", &format!("{remote_tip_after}^{{tree}}")],
    )?;
    if remote_tree_before != remote_tree_after
        || git_static(
            &repo,
            &[
                "merge-base",
                "--is-ancestor",
                &remote_tip_before,
                &remote_tip_after,
            ],
        )
        .is_err()
    {
        return Err("FX10 external ref movement was not a same-tree fast-forward".into());
    }
    let active_after = wave::load(&observer, &active_wave).map_err(|error| error.to_string())?;
    let target_after =
        target::status(&observer, owner.target()).map_err(|error| error.to_string())?;
    if active_after.phase != WavePhase::Verifying
        || sorted(&active_after.spec.selected) != ["A", "B", "C"]
        || target_after.phase != Phase::Verifying
    {
        return Err(
            "FX10 external ref movement changed local target authority during the gate".into(),
        );
    }
    let typed_holder = hold.typed_hold();
    let released_holder = hold.release()?;
    let holder_release_unix_ms = released_holder
        .released_at
        .ok_or_else(|| "FX10 resource holder release lacks a durable timestamp".to_string())?;
    Ok(DriftObservation {
        native_job: job_id,
        dispatch_seen_unix_ms,
        observer_polls: observer_polls.saturating_add(polls_while_resource_held),
        polls_while_resource_held,
        contention_window_ms: contention_started.elapsed().as_millis(),
        typed_holder,
        released_holder,
        holder_release_unix_ms,
        remote_tip_before,
        remote_tip_after,
        remote_tree_before,
        remote_tree_after,
        target_advanced_unix_ms,
        target_phase_after: format!("{:?}", target_after.phase),
        active_wave_phase_after: format!("{:?}", active_after.phase),
        completions_before_update,
    })
}

fn sorted(values: &[String]) -> Vec<String> {
    let mut values = values.to_vec();
    values.sort();
    values
}

fn actual_gate_resource_admission(
    observation: &Value,
    holder: &crate::resources::HostResourceLease,
    holder_release_unix_ms: i64,
) -> Result<Value, String> {
    let gate = observation["gates"]
        .as_array()
        .and_then(|gates| gates.first())
        .ok_or_else(|| "FX10 completed native job lacks its gate record".to_string())?;
    let resource = &gate["resource_lease"];
    let gate_lease_id = resource["lease_id"]
        .as_str()
        .ok_or_else(|| "FX10 native gate lacks its actual resource grant".to_string())?;
    if gate_lease_id == holder.lease_id {
        return Err("FX10 native gate reused the external holder lease".into());
    }
    let gate_lease = HostResourceCoordinator::open_default()
        .map_err(|error| error.to_string())?
        .list(true)
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|lease| lease.lease_id == gate_lease_id)
        .ok_or_else(|| "FX10 actual gate admission lease was not retained".to_string())?;
    if gate_lease.state.as_str() != "released"
        || gate_lease.created_at < holder_release_unix_ms
        || !gate_lease.allocations.iter().any(|allocation| {
            allocation.key == "fx10_sustained_arrival_slot"
                && allocation.kind == "capacity"
                && allocation.value == FX10_RESOURCE_POOL
                && allocation.units == Some(1)
                && allocation.capacity_limit == Some(1)
        })
    {
        return Err("FX10 actual native admission did not follow the finite typed hold".into());
    }
    let wait_duration_ms = gate["wait_duration_ms"]
        .as_i64()
        .ok_or_else(|| "FX10 native gate did not retain its measured wait".to_string())?;
    if wait_duration_ms <= 0 {
        return Err("FX10 second resource admission recorded no wait".into());
    }
    let grant_created_after_holder_release_ms =
        gate_lease.created_at.saturating_sub(holder_release_unix_ms);
    Ok(json!({
        "admission": "actual-native-gate",
        "lease": gate_lease,
        "typed_request": {
            "kind": "capacity",
            "key": "fx10_sustained_arrival_slot",
            "pool": FX10_RESOURCE_POOL,
            "units": 1,
            "limit": 1
        },
        "wait_duration_ms": wait_duration_ms,
        "grant_created_after_holder_release_ms": grant_created_after_holder_release_ms
    }))
}

#[allow(clippy::too_many_arguments)]
fn run_external_target_update(
    store: &mut CollaborationStore,
    broker: &mut Broker,
    session_id: i64,
    host: &Path,
    repo: &Path,
    remote: &Path,
    baseline_commit: &str,
    baseline_snapshot: &str,
    retained: &[(String, String, String)],
    executor: &DockerExecutor,
    verifier_policy: &VerifierPolicy,
    drive_policy: &DrivePolicy,
    cap: Units,
    report: &mut Value,
    report_path: &Path,
    started: Instant,
) -> Result<Value, String> {
    let owner = target::acquire_controller(store, FX10_DRIFT_TARGET, "appointed-fx10-operator")
        .map_err(|error| error.to_string())?;
    let acceptance_policy = verifier_policy
        .record()
        .map_err(|error| error.to_string())?
        .id()
        .to_string();
    let revision = target::propose_revision(
        store,
        &owner,
        TargetSpec {
            objective_authorization: source_digest(
                b"FX10 one bounded external target-ref update while the older wave is in flight",
            ),
            objective: "Verify the retained older route group while the disposable target ref advances once".into(),
            baseline: baseline_snapshot.into(),
            contributions: requested_contributions(&retained[..3]),
            selected: vec!["A".into(), "B".into(), "C".into()],
            requirements: vec![source_digest(FX10_PUBLIC_CONTRACT)],
            acceptance_policy,
            composer_profile: crate::composer::PROVISIONAL_TEXT_PROFILE.into(),
        },
    )
    .map_err(|error| error.to_string())?;
    target::select_revision(store, &owner, &revision.id).map_err(|error| error.to_string())?;
    budget::authorize_objective(store, &owner, cap).map_err(|error| error.to_string())?;
    let permit = crate::execution_verifier::configure(
        store,
        &owner,
        "fx10-native",
        None,
        verifier_policy.clone(),
    )
    .map_err(|error| error.to_string())?;

    git_static(
        remote,
        &["init", "--bare", "-q", "--initial-branch=accepted"],
    )?;
    git_static(
        repo,
        &[
            "remote",
            "add",
            "fx10-drift",
            remote
                .to_str()
                .ok_or_else(|| "FX10 drift remote path is not UTF-8".to_string())?,
        ],
    )?;
    git_static(
        repo,
        &[
            "push",
            "-q",
            "fx10-drift",
            &format!("{baseline_commit}:refs/heads/accepted"),
        ],
    )?;
    let baseline_tree = git_static(repo, &["rev-parse", &format!("{baseline_commit}^{{tree}}")])?;
    let advanced_commit = git_text(
        repo,
        &[
            "commit-tree".into(),
            baseline_tree,
            "-p".into(),
            baseline_commit.into(),
            "-m".into(),
            "FX10 controlled external target-ref advance".into(),
        ],
        None,
        None,
    )?;
    git_static(
        repo,
        &[
            "push",
            "-q",
            "fx10-drift",
            &format!("{advanced_commit}:refs/heads/fx10-ref-advance"),
        ],
    )?;
    if git_static(remote, &["rev-parse", "refs/heads/accepted"])? != baseline_commit {
        return Err("FX10 drift target did not start at the frozen baseline ref".into());
    }
    let carrier = GitCarrier::new(
        broker,
        session_id,
        "fx10-drift",
        "refs/heads/accepted",
        30,
        "Operator authorizes only the disposable FX10 drift-control bare Git target",
        true,
    )
    .map_err(|error| error.to_string())?;

    let wave_policy = drive_policy.wave_policy;
    let Admission::Frozen(old_wave) =
        wave::admit(store, &owner, wave_policy, now_ms()?).map_err(|error| error.to_string())?
    else {
        return Err("FX10 drift control did not freeze its exact three-member older group".into());
    };
    if sorted(&old_wave.spec.selected) != ["A", "B", "C"] {
        return Err("FX10 external-ref schedule did not admit exactly A-C".into());
    }
    let hold = hold_gate_resource(host, "external-ref-update")?;
    let appointment = ServicePolicy {
        nonce: "fx10-external-target-update".into(),
        drive: drive_policy.clone(),
        max_ticks: 1,
        max_completions: 1,
        poll_ms: 20,
        max_elapsed_ms: 180_000,
    };
    let observer_host = host.to_path_buf();
    let observer_repo = repo.to_path_buf();
    let observer_remote = remote.to_path_buf();
    let observer_owner = &owner;
    let wave_id = old_wave.id.clone();
    let baseline = baseline_commit.to_owned();
    let advanced = advanced_commit.clone();
    let (service, drift) = std::thread::scope(|scope| {
        let observer = scope.spawn(move || {
            observe_external_ref_advance(
                observer_host,
                observer_repo,
                observer_remote,
                observer_owner,
                wave_id,
                baseline,
                advanced,
                hold,
            )
        });
        let service = serve(
            store,
            &owner,
            broker,
            &carrier,
            executor,
            &permit,
            &appointment,
            &AtomicBool::new(false),
        );
        let drift = observer
            .join()
            .map_err(|_| "FX10 external-ref observer panicked".to_string())??;
        let service = service.map_err(|error| error.to_string())?;
        Ok::<_, String>((service, drift))
    })?;
    let service_finished_unix_ms = now_ms()?;
    if service.outcome != ServiceOutcome::Held
        || !service.completions.is_empty()
        || service.ticks != 1
    {
        return Err(format!(
            "FX10 external target update did not hold exactly one finite attempt: {service:?}"
        ));
    }
    let remote_tip = git_static(remote, &["rev-parse", "refs/heads/accepted"])?;
    if remote_tip != advanced_commit {
        return Err("FX10 failed candidate publication moved the external target ref".into());
    }
    let job = crate::native_gates::load(
        store,
        &aethyme_contracts::experimental_v0::RecordId::parse(&drift.native_job)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let gate_observation = job
        .observation
        .as_ref()
        .ok_or_else(|| "FX10 drift-control native result is missing".to_string())?;
    if job.state != "completed"
        || job.spec.wave != old_wave.id.as_str()
        || gate_observation["status"] != "passed"
        || gate_observation["current_authority"] != true
        || gate_observation["selected_gates"] != 1
        || drift.target_advanced_unix_ms > drift.holder_release_unix_ms
    {
        return Err("FX10 target-ref update did not preserve one fresh native pass".into());
    }
    let gate_admission = actual_gate_resource_admission(
        gate_observation,
        &drift.released_holder,
        drift.holder_release_unix_ms,
    )?;

    let (issued_ms, evidence_bytes): (i64, Vec<u8>) = store
        .read_connection()
        .query_row(
            "SELECT issued_ms,body FROM local_verification_evidence WHERE revision=?1 AND producer='fx10-native'",
            [revision.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| error.to_string())?;
    let evidence: Value =
        serde_json::from_slice(&evidence_bytes).map_err(|error| error.to_string())?;
    if evidence["observation"]["result"] != "passed" || issued_ms <= drift.target_advanced_unix_ms {
        return Err(
            "FX10 stale verifier evidence was not a real passing result issued after ref movement"
                .into(),
        );
    }
    let completion_count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_completions WHERE target=?1 AND revision=?2",
            (FX10_DRIFT_TARGET, revision.id.as_str()),
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if completion_count != 0 {
        return Err("FX10 stale verification produced a private completion".into());
    }
    let unknown_run_count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_runs WHERE wave=?1 AND state='unknown'",
            [old_wave.id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if unknown_run_count != 0 {
        return Err("FX10 external-ref schedule created an uncertain run".into());
    }
    let (dispatched, operation, write_outcome, observation): (i64, Option<i64>, Option<String>, String) = store
        .read_connection()
        .query_row(
            "SELECT dispatched,operation,write_outcome,observation FROM local_source_transports WHERE target=?1 AND revision=?2",
            (FX10_DRIFT_TARGET, revision.id.as_str()),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|error| error.to_string())?;
    if dispatched != 1 || operation.is_none() || write_outcome.as_deref() != Some("failed") {
        return Err(format!(
            "FX10 controlled ref advance did not produce one known failed publication: dispatched={dispatched}, operation={operation:?}, outcome={write_outcome:?}, observation={observation}"
        ));
    }
    let transport_count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_source_transports WHERE target=?1 AND revision=?2",
            (FX10_DRIFT_TARGET, revision.id.as_str()),
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if transport_count != 1 {
        return Err("FX10 drift-control service created more than one publication attempt".into());
    }
    let status = target::status(store, FX10_DRIFT_TARGET).map_err(|error| error.to_string())?;
    let wave = wave::load(store, &old_wave.id).map_err(|error| error.to_string())?;
    if status.phase != Phase::Verifying || wave.phase != WavePhase::Verifying {
        return Err("FX10 finite stale publication changed target or wave authority".into());
    }
    let update_to_verify_ms = issued_ms.saturating_sub(drift.target_advanced_unix_ms);
    let update_to_hold_ms = service_finished_unix_ms.saturating_sub(drift.target_advanced_unix_ms);
    if update_to_verify_ms <= 0 || update_to_hold_ms <= 0 {
        return Err("FX10 drift-control stale verification interval was not measurable".into());
    }
    let result = json!({
        "target": FX10_DRIFT_TARGET,
        "revision": revision.id.as_str(),
        "wave": old_wave.id.as_str(),
        "selected_older_group": ["A","B","C"],
        "target_update": {
            "kind": "external_fast_forward_of_disposable_bare_ref",
            "remote_tip_before": drift.remote_tip_before,
            "remote_tip_after": drift.remote_tip_after,
            "remote_tree_before": drift.remote_tree_before,
            "remote_tree_after": drift.remote_tree_after,
            "same_tree_fast_forward": true,
            "external_ref_updates": 1,
            "ref_advanced_at_unix_ms": drift.target_advanced_unix_ms,
            "native_job_dispatch_seen_unix_ms": drift.dispatch_seen_unix_ms,
            "native_job": drift.native_job,
            "native_job_state_during_update": "dispatching",
            "local_target_phase_after": drift.target_phase_after,
            "local_wave_phase_after": drift.active_wave_phase_after,
            "completions_before_update": drift.completions_before_update,
            "final_target_phase": format!("{:?}", status.phase),
            "final_wave_phase": format!("{:?}", wave.phase)
        },
        "resource_contention": {
            "typed_holder": drift.typed_holder,
            "holder_lease_released": drift.released_holder,
            "holder_release_unix_ms": drift.holder_release_unix_ms,
            "holder_to_update_elapsed_ms": drift.contention_window_ms,
            "observer_polls": drift.observer_polls,
            "polls_while_resource_held": drift.polls_while_resource_held,
            "second_admission_blocked_until_holder_release": true,
            "actual_second_admission": gate_admission
        },
        "stale_verification": {
            "result": evidence["observation"]["result"],
            "candidate": evidence["observation"]["candidate"],
            "issued_at_unix_ms": issued_ms,
            "target_advance_to_passing_verifier_receipt_ms": update_to_verify_ms,
            "ref_advance_to_held_service_result_ms": update_to_hold_ms,
            "stale_candidate_verification_attempts": 1,
            "stale_candidate_elapsed_to_receipt_ms": update_to_verify_ms
        },
        "publication": {
            "dispatched_once": true,
            "operation": operation,
            "write_outcome": write_outcome,
            "observation": observation,
            "replays": 0,
            "private_completion_count": completion_count,
            "unknown_run_count": unknown_run_count,
            "service_outcome": service.outcome,
            "service_ticks": service.ticks,
            "detail": service.detail,
            "final_remote_tip": remote_tip
        },
        "finite_allowance": {
            "new_wave_width": 3,
            "cumulative_selected_max": 6,
            "service_ticks": 1,
            "service_completions": 1,
            "gate_resource_wait_seconds": 5,
            "minimum_contention_ms": FX10_MIN_CONTENTION_MS,
            "holder_units": 1,
            "holder_limit": 1,
            "holder_ttl_seconds": 60,
            "observer_timeout_seconds": FX10_OBSERVER_TIMEOUT_SECONDS,
            "max_elapsed_seconds": 180,
            "max_drive_wall_seconds": 120
        },
        "retained_roots": {
            "host_state": host.display().to_string(),
            "project": store.project_dir().display().to_string(),
            "source_repository": repo.display().to_string(),
            "bare_remote": remote.display().to_string()
        },
        "cumulative_elapsed_ms": started.elapsed().as_millis()
    });
    save_event(
        report_path,
        report,
        json!({"at":"external-target-update","result":"one controlled bare-ref advance produced a measured stale verifier and a held known-failed publication"}),
    )?;
    Ok(result)
}

fn run_fx10(
    report: &mut Value,
    report_path: &Path,
    started: Instant,
    host: &Path,
    repo: &Path,
    harness_repo: &Path,
    remote: &Path,
    drift_remote: &Path,
) -> Result<(), String> {
    let (oracle, expected_transcript, manifest) = check_frozen_inputs()?;
    report["frozen_inputs"] = json!({
        "manifest_sha256": FX10_MANIFEST_SHA,
        "public_contract_sha256": FX10_CONTRACT_SHA,
        "oracle_sha256": FX10_ORACLE_SHA,
        "proxy_sha256": FX10_PROXY_SHA,
        "expected_transcript_sha256": FX10_TRANSCRIPT_SHA,
        "oracle_vector_count": oracle.len(),
        "contribution_count": SUBMISSIONS.len(),
    });
    save_event(
        report_path,
        report,
        json!({"at":"preflight","result":"frozen FX10 inputs and oracle-derived transcript verified"}),
    )?;

    let baseline_commit = write_playground(repo)?;
    let baseline_oid = CommitOid::parse(&baseline_commit).map_err(|error| error.to_string())?;
    let mut store = CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse(FX10_PROJECT).map_err(|error| error.to_string())?,
        &[],
    )
    .map_err(|error| error.to_string())?;
    let baseline_snapshot = archive::retain_snapshot(&mut store, repo, &baseline_oid)
        .map_err(|error| error.to_string())?;

    fs::write(harness_repo.join("proxy.sh"), FX10_PROXY.as_bytes())
        .map_err(|error| error.to_string())?;
    let inventory = make_inventory(&oracle, &expected_transcript)?;
    fs::write(
        harness_repo.join("inventory.json"),
        inventory.canonical_bytes(),
    )
    .map_err(|error| error.to_string())?;
    git_static(harness_repo, &["init", "-q", "-b", "main"])?;
    git_static(harness_repo, &["add", "proxy.sh", "inventory.json"])?;
    git_static(
        harness_repo,
        &["commit", "-qm", "FX10 independent route oracle"],
    )?;
    let harness_commit = CommitOid::parse(&git_static(harness_repo, &["rev-parse", "HEAD"])?)
        .map_err(|error| error.to_string())?;
    let harness_snapshot = archive::retain_snapshot(&mut store, harness_repo, &harness_commit)
        .map_err(|error| error.to_string())?;

    let cli = std::env::var("AETHYME_DOCKER_CLI")
        .map_err(|_| "AETHYME_DOCKER_CLI must name the appointed Docker client".to_string())?;
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").map_err(|_| {
        "AETHYME_DOCKER_TEST_IMAGE must name the appointed cached image".to_string()
    })?;
    if image != FX17_IMAGE {
        return Err(format!(
            "Docker image must equal pinned FX10 image {FX17_IMAGE}"
        ));
    }
    let native = DockerExecutor::probe(PathBuf::from(&cli)).map_err(|error| error.to_string())?;
    let execution_resources = crate::execution_resources::ResourcePolicy {
        host_state: host.join("execution-resources.db"),
        temporary_root: host.to_path_buf(),
        runtime: native.runtime().id(),
        daemon: native.runtime().daemon_id.clone(),
        meter_image: image.clone(),
        memory_mib: 2048,
        disk_mib: 2048,
        process_slots: 1024,
        job_slots: 2,
        host_floor_bytes: 1,
        daemon_floor_bytes: 1,
    };
    let executor = native
        .with_resources(execution_resources)
        .map_err(|error| error.to_string())?;
    let limits = ResourceLimits {
        timeout_ms: 60_000,
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 1024 * 1024,
        pids: 32,
    };
    let verify = CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), FX10_PROXY.into()],
        environment: Default::default(),
    };
    let execution = ExecutionSpec {
        candidate_source: baseline_snapshot.snapshot_id.to_string(),
        preparation: None,
        dependencies: None,
        configuration: None,
        harness: harness_snapshot.snapshot_id.to_string(),
        dataset: None,
        assertion_inventory: inventory.id().to_string(),
        materialization_profile: MATERIALIZATION_PROFILE.into(),
        profile: LinuxProfile {
            build_image: image.clone(),
            verifier_image: image.clone(),
            architecture: executor.runtime().architecture.clone(),
            runtime_profile: executor.runtime().id(),
            build_limits: limits,
            verifier_limits: limits,
        },
        build: CommandSpec {
            program: "/bin/sh".into(),
            arguments: vec!["-c".into(), "set -eu; cp -R /source/. /outputs/".into()],
            environment: Default::default(),
        },
        verify: verify.clone(),
    };
    let verifier_policy = VerifierPolicy {
        profile: crate::execution_verifier::PROFILE.into(),
        runtime: executor.runtime().id(),
        image: image.clone(),
        harness: harness_snapshot.snapshot_id.to_string(),
        inventory: inventory.id().to_string(),
        command: verify,
        max_age_ms: 900_000,
    };
    let acceptance_policy = verifier_policy
        .record()
        .map_err(|error| error.to_string())?
        .id()
        .to_string();

    let retained = retain_submissions(
        &mut store,
        repo,
        &baseline_oid,
        baseline_snapshot.snapshot_id.as_str(),
        &host.join("source.index"),
    )?;
    let owner = target::acquire_controller(&mut store, FX10_TARGET, "appointed-fx10-operator")
        .map_err(|error| error.to_string())?;
    let objective_authorization =
        source_digest(b"FX10 finite six-submission sustained-arrival objective");
    let selected = TargetSpec {
        objective_authorization: objective_authorization.clone(),
        objective:
            "Preserve six independent route behaviors while accepting the bounded arrival schedule"
                .into(),
        baseline: baseline_snapshot.snapshot_id.to_string(),
        contributions: requested_contributions(&retained),
        selected: SUBMISSIONS
            .iter()
            .map(|submission| submission.id.into())
            .collect(),
        requirements: vec![source_digest(FX10_PUBLIC_CONTRACT)],
        acceptance_policy: acceptance_policy.clone(),
        composer_profile: crate::composer::PROVISIONAL_TEXT_PROFILE.into(),
    };
    let revision = target::propose_revision(&mut store, &owner, selected)
        .map_err(|error| error.to_string())?;
    target::select_revision(&mut store, &owner, &revision.id).map_err(|error| error.to_string())?;
    let cap = Units {
        time_ms: 1_000_000_000,
        memory_bytes: 64 * 1024 * 1024 * 1024,
        disk_bytes: 128 * 1024 * 1024 * 1024,
        slots: 200,
        ..Default::default()
    };
    budget::configure_project(
        &mut store,
        &owner,
        ProjectPolicy {
            authorization: source_digest(b"FX10 bounded Docker, gate and provider budget"),
            price_profile: source_digest(b"FX10 declared unpriced local execution profile"),
            cap,
            recovery_headroom: Units::default(),
            mandatory_headroom: Units::default(),
            max_root_reservations: 32,
        },
    )
    .map_err(|error| error.to_string())?;
    budget::authorize_objective(&mut store, &owner, cap).map_err(|error| error.to_string())?;
    let permit = crate::execution_verifier::configure(
        &mut store,
        &owner,
        "fx10-native",
        None,
        verifier_policy,
    )
    .map_err(|error| error.to_string())?;

    let wave_policy = WavePolicy {
        batch_width: 3,
        max_group: 6,
        debounce_ms: 0,
        age_promotion_ms: 60_000,
        drift_rechecks: 1,
    };
    let drive_policy = DrivePolicy {
        execution,
        wave_policy,
        max_wall_ms: 120_000,
        allow_empty_native_gates: false,
    };
    let old_arrival_ms = now_ms()?;
    for id in ["A", "B", "C"] {
        wave::available(&mut store, &owner, id, old_arrival_ms)
            .map_err(|error| error.to_string())?;
    }
    let oldest_arrival_ms: i64 = store
        .read_connection()
        .query_row(
            "SELECT min(first_seen_ms) FROM local_wave_arrivals WHERE revision=?1 AND contribution IN ('A','B','C')",
            [revision.id.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;

    let gates = crate::gates::parse_gates(
        std::str::from_utf8(FX10_SOURCE_FILES[1].1).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if gates.len() != 1 || gates[0].resource_wait_seconds != 5 {
        return Err(
            "FX10 requires its one fixed safe native gate and five-second resource wait".into(),
        );
    }
    let trusted_policy = GatePolicy::from_parts(&gates, None);
    let trust_path = gate_trust::record_trust(
        repo,
        &[trusted_policy.policy_sha256.as_str()],
        "FX10 fixed route-source integrity gate in disposable Playground",
    )
    .map_err(|error| error.to_string())?;
    let _remove_trust = RemoveGateTrust(trust_path);

    let mut broker = Broker::open(repo)
        .map_err(|error| error.to_string())?
        .with_host_operation_database(host.join("operations.db"))
        .with_worktree_root(host.join("worktrees"));
    let session = broker
        .adopt(repo, Some("FX10 disposable sustained-arrival Playground"))
        .map_err(|error| error.to_string())?;
    git_static(
        remote,
        &["init", "--bare", "-q", "--initial-branch=accepted"],
    )?;
    git_static(
        repo,
        &[
            "remote",
            "add",
            "fx10-accepted",
            remote
                .to_str()
                .ok_or_else(|| "FX10 bare remote path is not UTF-8".to_string())?,
        ],
    )?;
    git_static(
        repo,
        &[
            "push",
            "-q",
            "fx10-accepted",
            &format!("{baseline_commit}:refs/heads/accepted"),
        ],
    )?;
    let original_head = git_static(repo, &["rev-parse", "HEAD"])?;
    let carrier = GitCarrier::new(
        &mut broker,
        session.id,
        "fx10-accepted",
        "refs/heads/accepted",
        30,
        "Operator authorizes only the disposable FX10 bare Git target",
        true,
    )
    .map_err(|error| error.to_string())?;

    let admission_ms = now_ms()?;
    let Admission::Frozen(old_wave) = wave::admit(&mut store, &owner, wave_policy, admission_ms)
        .map_err(|error| error.to_string())?
    else {
        return Err("FX10 older atomic group did not freeze for its first actual wave".into());
    };
    let group_ready_at = oldest_arrival_ms;
    if group_ready_at > admission_ms {
        return Err("FX10 older group's ready timestamp is later than its admission".into());
    }
    let oldest_ready_age_ms = admission_ms.saturating_sub(group_ready_at);
    if sorted(&old_wave.spec.selected) != ["A", "B", "C"] {
        return Err(format!(
            "FX10 first wave was not exact old group A-C: {:?}",
            old_wave.spec.selected
        ));
    }
    let ready_sequence = old_wave.spec.sequence;
    let hold = hold_gate_resource(host, "positive")?;
    let appointment = ServicePolicy {
        nonce: "fx10-sustained-arrivals".into(),
        drive: drive_policy.clone(),
        max_ticks: 8,
        max_completions: 2,
        poll_ms: 20,
        max_elapsed_ms: 180_000,
    };
    let observer_host = host.to_path_buf();
    let target_owner = &owner;
    let wave_id = old_wave.id.clone();
    let (service, arrivals) = std::thread::scope(|scope| {
        let sender = scope.spawn(move || {
            observe_sustained_arrivals(
                observer_host,
                target_owner,
                FX10_TARGET.into(),
                wave_id,
                hold,
            )
        });
        let service = serve(
            &mut store,
            &owner,
            &mut broker,
            &carrier,
            &executor,
            &permit,
            &appointment,
            &AtomicBool::new(false),
        );
        let arrivals = sender
            .join()
            .map_err(|_| "FX10 arrival observer panicked".to_string())?;
        let arrivals = arrivals?;
        let service = service.map_err(|error| error.to_string())?;
        Ok::<_, String>((service, arrivals))
    })?;
    if service.outcome != ServiceOutcome::CompletionAllowance
        || service.completions.len() != 2
        || service.ticks != 2
    {
        return Err(format!(
            "FX10 finite service did not complete exactly two waves: {service:?}"
        ));
    }
    let first = service.completions[0].clone();
    let second = service.completions[1].clone();
    let first_wave = wave::load(
        &store,
        &aethyme_contracts::experimental_v0::RecordId::parse(&first.wave)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let second_wave = wave::load(
        &store,
        &aethyme_contracts::experimental_v0::RecordId::parse(&second.wave)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if first.wave != old_wave.id.as_str()
        || first_wave.phase != WavePhase::Accepted
        || sorted(&first_wave.spec.selected) != ["A", "B", "C"]
        || sorted(&second_wave.spec.selected) != ["A", "B", "C", "D", "E", "F"]
        || second_wave.spec.sequence != ready_sequence + 1
        || second_wave.spec.prior_completion.as_deref()
            != Some(
                first
                    .record()
                    .map_err(|error| error.to_string())?
                    .id()
                    .as_str(),
            )
        || second_wave.spec.baseline != first.source
    {
        return Err(
            "FX10 accepted waves do not preserve the exact older-to-newer obligation order".into(),
        );
    }
    if arrivals
        .arrivals
        .iter()
        .map(|(id, _)| id.as_str())
        .collect::<Vec<_>>()
        != ["D", "E", "F"]
        || arrivals.completions_before_release != 0
        || arrivals.active_wave_selection_after != ["A", "B", "C"]
        || arrivals.target_phase_before != "Verifying"
        || arrivals.target_phase_after != "Verifying"
        || arrivals.active_wave_phase_after != "Verifying"
    {
        return Err(
            "FX10 bounded arrival observation did not witness the intended active-wave schedule"
                .into(),
        );
    }
    let status = target::status(&store, FX10_TARGET).map_err(|error| error.to_string())?;
    if status.phase != Phase::PublicationConfirmed {
        return Err("FX10 private acceptance did not reach publication confirmation".into());
    }
    let dispositions =
        target::dispositions(&store, &revision.id).map_err(|error| error.to_string())?;
    if dispositions.len() != 6
        || dispositions
            .iter()
            .any(|row| row.status != target::Disposition::Delivered)
    {
        return Err(
            "FX10 final target does not retain all six delivered feature obligations".into(),
        );
    }
    let first_accepted_parent = git_text(
        repo,
        &["rev-parse".into(), format!("{}^", first.accepted_commit)],
        None,
        None,
    )?;
    let second_accepted_parent = git_text(
        repo,
        &["rev-parse".into(), format!("{}^", second.accepted_commit)],
        None,
        None,
    )?;
    if first_accepted_parent != baseline_commit || second_accepted_parent != first.accepted_commit {
        return Err(
            "FX10 private accepted commits do not form the exact retained parent chain".into(),
        );
    }
    let remote_tip = git_static(remote, &["rev-parse", "refs/heads/accepted"])?;
    if remote_tip != second.accepted_commit {
        return Err(
            "FX10 private completion does not match the exact disposable Git target".into(),
        );
    }
    for submission in &SUBMISSIONS {
        let expected = if ["A", "B", "C"].contains(&submission.id) {
            submission.bytes
        } else {
            FX10_SOURCE_FILES
                .iter()
                .find(|(path, _)| path.ends_with(submission.path))
                .map(|(_, bytes)| *bytes)
                .ok_or_else(|| format!("missing baseline for {}", submission.id))?
        };
        if read_commit_file(repo, &first.accepted_commit, submission.path)? != expected {
            return Err(format!(
                "FX10 first candidate unexpectedly changed feature {}",
                submission.id
            ));
        }
        if read_commit_file(repo, &second.accepted_commit, submission.path)? != submission.bytes {
            return Err(format!(
                "FX10 final candidate failed to preserve feature {}",
                submission.id
            ));
        }
    }
    if read_commit_file(repo, &second.accepted_commit, "app.sh")? != FX10_SOURCE_FILES[0].1
        || read_commit_file(repo, &second.accepted_commit, ".aethyme/gates.toml")?
            != FX10_SOURCE_FILES[1].1
        || git_static(repo, &["rev-parse", "HEAD"])? != original_head
    {
        return Err(
            "FX10 candidate changed the public router, gate policy or source checkout ref".into(),
        );
    }

    let mut gate_observations = Vec::new();
    for completion in [&first, &second] {
        let ids = query_strings(
            &store,
            "SELECT id FROM local_native_gate_jobs WHERE wave=?1",
            &completion.wave,
        )?;
        if ids.len() != 1 {
            return Err(format!(
                "FX10 wave {} lacks exactly one actual native gate job",
                completion.wave
            ));
        }
        let key = aethyme_contracts::experimental_v0::RecordId::parse(&ids[0])
            .map_err(|error| error.to_string())?;
        let job = crate::native_gates::load(&store, &key).map_err(|error| error.to_string())?;
        let observation = job
            .observation
            .ok_or_else(|| "FX10 native observation missing".to_string())?;
        if job.state != "completed"
            || observation["status"] != "passed"
            || observation["current_authority"] != true
            || observation["selected_gates"] != 1
        {
            return Err(format!(
                "FX10 native gate was not a fresh current pass: {observation}"
            ));
        }
        gate_observations.push(observation);
    }
    let first_gate_wait_ms = gate_observations[0]["gates"][0]["wait_duration_ms"]
        .as_i64()
        .ok_or_else(|| "FX10 native gate did not retain a measured resource wait".to_string())?;
    if first_gate_wait_ms <= 0 {
        return Err("FX10 real gate resource contention was not measured".into());
    }
    let first_gate_admission = actual_gate_resource_admission(
        &gate_observations[0],
        &arrivals.released_holder,
        arrivals.holder_release_unix_ms,
    )?;
    let evidence = query_strings(
        &store,
        "SELECT id FROM local_verification_evidence WHERE revision=?1 AND producer='fx10-native' ORDER BY issued_ms",
        revision.id.as_str(),
    )?;
    if evidence.len() != 2 {
        return Err(format!(
            "FX10 expected two independent verifier receipts, got {}",
            evidence.len()
        ));
    }
    let completion_count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_completions WHERE target=?1 AND revision=?2",
            (FX10_TARGET, revision.id.as_str()),
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let unknown_runs: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_runs WHERE state='unknown'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    let blocked_waves: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_waves WHERE phase IN ('blocked','unsupported','target_drift','outcome_unknown','stopped')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if completion_count != 2 || unknown_runs != 0 || blocked_waves != 0 {
        return Err(
            "FX10 positive schedule contains missing completions, uncertain runs or blocked waves"
                .into(),
        );
    }
    let (reservation_records, allocated, spent) = reservation_rows(&store, &first.wave)?;
    let (second_reservations, second_allocated, second_spent) =
        reservation_rows(&store, &second.wave)?;
    let pending_reservations: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_budget_reservations WHERE phase IN ('reserved','partly_consumed','completion_pending','consumption_unknown')",
            [],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if pending_reservations != 0 {
        return Err("FX10 positive service left unconfirmed budget reservations".into());
    }
    let roots = super::completion_roots(
        &store,
        &first
            .record()
            .map_err(|error| error.to_string())?
            .id()
            .to_string(),
    )?;
    let second_roots = super::completion_roots(
        &store,
        &second
            .record()
            .map_err(|error| error.to_string())?
            .id()
            .to_string(),
    )?;
    report["retained_roots"] = json!({
        "host_state": host.display().to_string(),
        "project": store.project_dir().display().to_string(),
        "baseline_commit": baseline_commit,
        "baseline_snapshot": baseline_snapshot.snapshot_id.as_str(),
        "harness_commit": harness_commit.as_str(),
        "harness_snapshot": harness_snapshot.snapshot_id.as_str(),
        "source_repository": repo.display().to_string(),
        "bare_remote": remote.display().to_string(),
        "drift_bare_remote": drift_remote.display().to_string(),
    });
    report["actual_profile"] = json!({
        "runtime": executor.runtime(),
        "image": image,
        "network": "none",
        "build": "copy the exact retained source tree into isolated Docker outputs",
        "verifier_vectors": oracle.len(),
        "native_gate": {"policy_sha256":trusted_policy.policy_sha256,"resource_wait_seconds":5,"measured_wait_ms":first_gate_wait_ms},
        "resource_policy": {"memory_mib":2048,"disk_mib":2048,"process_slots":1024,"job_slots":2},
        "drive_max_wall_ms":120000,
        "service": appointment,
        "wave_policy": wave_policy,
        "budget_cap": cap,
    });
    report["sustained_arrivals"] = json!({
        "old_group": ["A","B","C"],
        "old_wave": first.wave,
        "old_native_job_dispatch_seen_unix_ms": arrivals.dispatch_seen_unix_ms,
        "oldest_ready_age_ms_at_freeze": oldest_ready_age_ms,
        "old_group_ready_at_ms": group_ready_at,
        "wave_admitted_at_ms": admission_ms,
        "later_arrivals": arrivals.arrivals,
        "arrival_wave_waited": 1,
        "observer_polls": arrivals.observer_polls,
        "polls_while_resource_held": arrivals.polls_while_resource_held,
        "bounded_contention_window_ms": arrivals.contention_window_ms,
        "resource_contention": {
            "typed_holder": arrivals.typed_holder,
            "holder_lease_released": arrivals.released_holder,
            "holder_release_unix_ms": arrivals.holder_release_unix_ms,
            "actual_second_admission": first_gate_admission
        },
        "first_gate_reported_wait_ms": first_gate_wait_ms,
        "active_wave_phase_before_release": arrivals.active_wave_phase_after,
        "active_wave_selection_after_arrivals": arrivals.active_wave_selection_after,
        "target_phase_before_release": arrivals.target_phase_before,
        "target_phase_after_release": arrivals.target_phase_after,
        "completions_before_release": arrivals.completions_before_release,
        "service_rescans_ticks": service.ticks,
        "completions": [first, second],
        "all_six_final_obligations": true,
        "invalidated_verification_attempts": 0,
        "invalidated_verification_time_ms": 0,
        "blocked_waves": blocked_waves,
        "unknown_runs": unknown_runs,
        "automatic_uncertain_retries": 0,
        "finite_allowance": {"batch_width_new":3,"cumulative_max_group":6,"service_ticks":8,"service_completions":2,"resource_wait_seconds":5,"observer_timeout_seconds":FX10_OBSERVER_TIMEOUT_SECONDS,"minimum_contention_ms":FX10_MIN_CONTENTION_MS},
    });
    report["accounting"] = json!({
        "first_wave_reservations":reservation_records,
        "second_wave_reservations":second_reservations,
        "first_wave_allocated":allocated,
        "first_wave_spent":spent,
        "second_wave_allocated":second_allocated,
        "second_wave_spent":second_spent,
        "pending_reservations":pending_reservations,
        "first_completion_roots":roots,
        "second_completion_roots":second_roots,
    });
    save_event(
        report_path,
        report,
        json!({"at":"sustained-arrivals","result":"the older A-C wave completed before the later D-F wave with all obligations retained"}),
    )?;
    let external_target_update = run_external_target_update(
        &mut store,
        &mut broker,
        session.id,
        host,
        repo,
        drift_remote,
        &baseline_commit,
        baseline_snapshot.snapshot_id.as_str(),
        &retained,
        &executor,
        permit.policy(),
        &drive_policy,
        cap,
        report,
        report_path,
        started,
    )?;
    report["external_target_update"] = external_target_update;
    report["completed_at_unix_ms"] = json!(now_ms()?);
    report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
    let _ = manifest;
    Ok(())
}

#[test]
fn fx10_frozen_inputs_bind_the_sustained_arrival_contract_and_oracle() {
    check_frozen_inputs().expect("FX10 frozen inputs and independent oracle transcript");
}

struct PrivateHostStateEnvironment(Option<OsString>);

impl PrivateHostStateEnvironment {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("AETHYME_HOST_STATE_DIR");
        // SAFETY: the ignored actual FX10 test runs as the sole exact test
        // filter, and this guard restores the process value when it exits.
        unsafe {
            std::env::set_var("AETHYME_HOST_STATE_DIR", path);
        }
        Self(previous)
    }
}

impl Drop for PrivateHostStateEnvironment {
    fn drop(&mut self) {
        // SAFETY: paired with the sole-test scoped mutation above.
        unsafe {
            if let Some(previous) = self.0.take() {
                std::env::set_var("AETHYME_HOST_STATE_DIR", previous);
            } else {
                std::env::remove_var("AETHYME_HOST_STATE_DIR");
            }
        }
    }
}

#[test]
#[ignore = "actual FX10 Docker build, resource-contended native gate, external verifier and disposable GenericGit sustained arrivals"]
fn actual_fx10_sustained_arrivals_serve_old_group_before_later_features() {
    let started = Instant::now();
    let host = tempfile::tempdir().expect("FX10 private host root").keep();
    let repo = tempfile::tempdir()
        .expect("FX10 source repository root")
        .keep();
    let harness_repo = tempfile::tempdir()
        .expect("FX10 harness repository root")
        .keep();
    let remote = tempfile::tempdir().expect("FX10 bare remote root").keep();
    let drift_remote = tempfile::tempdir()
        .expect("FX10 drift-control bare remote root")
        .keep();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _private_host_state = PrivateHostStateEnvironment::set(&host);
    let report_path = host.join("fx10-report.json");
    let mut report = json!({
        "profile":"aethyme.fx10-sustained-arrivals-actual/v1",
        "status":"running",
        "started_unix_ms":now_ms().expect("system clock"),
        "retained_roots":{"host_state":host.display().to_string(),"source_repository":repo.display().to_string(),"harness_repository":harness_repo.display().to_string(),"bare_remote":remote.display().to_string(),"drift_bare_remote":drift_remote.display().to_string(),"report":report_path.display().to_string()},
        "events":[],
        "failures":[],
        "model_requests":0,
        "sealed_task_executions":0,
    });
    fsync_report(&report_path, &report).expect("write initial FX10 failure-retaining report");
    eprintln!(
        "FX10 actual trial report {}, host {}, source {}, remote {}, harness {}",
        report_path.display(),
        host.display(),
        repo.display(),
        remote.display(),
        harness_repo.display()
    );
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_fx10(
            &mut report,
            &report_path,
            started,
            &host,
            &repo,
            &harness_repo,
            &remote,
            &drift_remote,
        )
    }));
    match outcome {
        Ok(Ok(())) => {
            report["status"] = json!("completed");
            fsync_report(&report_path, &report).expect("sync completed FX10 report");
        }
        Ok(Err(error)) => {
            report["status"] = json!("failed");
            report["failures"]
                .as_array_mut()
                .expect("FX10 failures array")
                .push(json!({"error":error}));
            report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
            fsync_report(&report_path, &report).expect("sync failed FX10 report");
            panic!(
                "FX10 actual qualification failed; retained report: {}",
                report_path.display()
            );
        }
        Err(payload) => {
            let message = record_unwind(&mut report, payload);
            report["status"] = json!("panicked");
            report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
            fsync_report(&report_path, &report).expect("sync panicked FX10 report");
            panic!(
                "FX10 actual qualification panicked ({message}); retained report: {}",
                report_path.display()
            );
        }
    }
}
