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
use crate::local_wave::{self as wave, WavePhase, WavePolicy};
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
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const FX10_MANIFEST_SHA: &str = "01aa466182a9054f5f4f0bd1f71fa1a3f2869ea731cee0f69ed0098a2b679ae1";
const FX10_CONTRACT_SHA: &str = "0baf3b085357267128927d85658af9abd759e420795008b83209b683f0855339";
const FX10_ORACLE_SHA: &str = "0caede81a00be34dc3dcb171bfbb714dca3fc0c10fa49f72c39437f728532954";
const FX10_PROXY_SHA: &str = "a8b7bd9e7269868883f679b1158c519dba44a374ca6f5f9a3c675470345b0f97";
const FX10_TRANSCRIPT_SHA: &str =
    "56d15d1c702fabd891f2764a8c68d9d96da4f99f1c039801678637151f9a503e";
const FX10_RESOURCE_POOL: &str = "fx10-sustained-arrival-v1";
const FX10_MIN_CONTENTION_MS: u64 = 250;
const FX10_MIN_GATE_CHILD_RUNNING_MS: u64 = 250;
const FX10_GATE_HANDSHAKE_ENV: &str = "AETHYME_FX10_GATE_HANDSHAKE_DIR";
const FX10_OBSERVER_TIMEOUT_SECONDS: u64 = 30;
const FX10_TARGET: &str = "fx10-sustained-arrivals";
const FX10_DRIFT_TARGET: &str = "fx10-external-target-update";
const FX10_PROJECT: &str = "fx10-sustained-arrivals";
const FX10_SOURCE_FILES: [(&str, &[u8]); 9] = [
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
    (
        "source/.aethyme/fx10-gate-child.sh",
        include_bytes!("../../../../../../../aethyme-eval/fx10/source/.aethyme/fx10-gate-child.sh"),
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
        || manifest["arrival_schedule"]["while_old_native_gate_child_is_running_after_resource_admission"]
            != json!(["D", "E", "F"])
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
        || manifest["resource_contention"]["minimum_gate_child_running_ms"]
            != FX10_MIN_GATE_CHILD_RUNNING_MS
        || manifest["resource_contention"]["gate_child_observer_timeout_seconds"]
            != FX10_OBSERVER_TIMEOUT_SECONDS
        || manifest["resource_contention"]["required_gate_child_heartbeat"] != true
        || manifest["gate_child_handshake"]["script"] != "source/.aethyme/fx10-gate-child.sh"
        || manifest["gate_child_handshake"]["environment"] != FX10_GATE_HANDSHAKE_ENV
        || manifest["gate_child_handshake"]["started_signal"] != "started"
        || manifest["gate_child_handshake"]["heartbeat_signal"] != "heartbeat"
        || manifest["gate_child_handshake"]["minimum_heartbeat_updates"] != 1
        || manifest["gate_child_handshake"]["release_signal"] != "release"
        || manifest["gate_child_handshake"]["completed_signal"] != "completed"
        || manifest["gate_child_handshake"]["minimum_live_after_later_arrivals_ms"]
            != FX10_MIN_GATE_CHILD_RUNNING_MS
        || manifest["gate_child_handshake"]["observer_timeout_seconds"]
            != FX10_OBSERVER_TIMEOUT_SECONDS
        || manifest["gate_child_handshake"]["gate_timeout_seconds"] != 20
        || manifest["gate_child_handshake"]["integrity_check_after_release"]
            != "test -f app.sh && git diff --check HEAD^ HEAD"
        || manifest["gate_child_handshake"]["native_gate_must_have_active_typed_resource_lease_before_started_signal"]
            != true
        || manifest["target_update_schedule"]["native_gate_child_running_required"] != true
        || manifest["target_update_schedule"]["update_occurs_before_gate_child_release"] != true
        || manifest["target_update_schedule"]["target"] != FX10_DRIFT_TARGET
        || manifest["target_update_schedule"]["separate_target_and_bare_remote"] != true
        || manifest["target_update_schedule"]["external_update_count"] != 1
        || manifest["target_update_schedule"]["external_update_keeps_tree_unchanged"] != true
        || manifest["target_update_schedule"]["trigger"]
            != "exact A-C native gate child has started after the finite capacity hold released"
        || manifest["target_update_schedule"]["minimum_resource_contention_ms"]
            != FX10_MIN_CONTENTION_MS
        || manifest["target_update_schedule"]["minimum_gate_child_running_before_ref_advance_ms"]
            != FX10_MIN_GATE_CHILD_RUNNING_MS
        || manifest["target_update_schedule"]["minimum_gate_child_running_after_ref_advance_ms"]
            != FX10_MIN_GATE_CHILD_RUNNING_MS
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
            crate::warn_unrecorded(
                "release abandoned FX10 capacity holder",
                self.coordinator.release(
                    &self.grant.lease.lease_id,
                    self.grant.lease.generation,
                    &self.grant.ownership_token,
                ),
            );
        }
    }
}

struct ArrivalObservation {
    native_job: String,
    native_wave: aethyme_contracts::experimental_v0::RecordId,
    native_wave_sequence: i64,
    dispatch_seen_unix_ms: i64,
    oldest_member_first_seen_unix_ms: i64,
    group_ready_unix_ms: i64,
    oldest_member_age_at_dispatch_ms: i64,
    ready_group_age_at_dispatch_ms: i64,
    arrival_sequences: Vec<(String, i64, i64)>,
    arrivals: Vec<ArrivalEvent>,
    observer_polls: u64,
    polls_while_resource_held: u64,
    polls_while_gate_child_running: u64,
    contention_window_ms: u128,
    gate_child_pid: i32,
    gate_child_started_unix_ms: i64,
    gate_child_running_window_ms: u128,
    gate_child_heartbeat_updates: u64,
    gate_child_release_unix_ms: i64,
    gate_child_completed_unix_ms: i64,
    gate_resource_lease: crate::resources::HostResourceLease,
    typed_holder: Value,
    released_holder: crate::resources::HostResourceLease,
    holder_release_unix_ms: i64,
    target_phase_before: String,
    target_phase_after: String,
    active_wave_phase_after: String,
    active_wave_selection_after: Vec<String>,
    completions_before_release: i64,
}

struct PersistedOldGroupArrivals {
    arrival_sequences: Vec<(String, i64, i64)>,
    oldest_member_first_seen_unix_ms: i64,
    group_ready_unix_ms: i64,
}

struct ActiveNativeWave<'a> {
    owner: &'a target::ControllerOwner,
    revision: &'a str,
    wave_id: &'a aethyme_contracts::experimental_v0::RecordId,
    job_id: &'a str,
}

impl ActiveNativeWave<'_> {
    fn assert_active(&self, store: &CollaborationStore) -> Result<(String, i64), String> {
        assert_active_older_dispatch(store, self.owner, self.revision, self.wave_id, self.job_id)
    }
}

struct ArrivalEvent {
    id: String,
    sequence: i64,
    first_seen_unix_ms: i64,
    observed_while_gate_child_running_unix_ms: i64,
    heartbeat_count: u64,
}

struct GateChildHandshakeEnvironment(Option<OsString>);
impl GateChildHandshakeEnvironment {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os(FX10_GATE_HANDSHAKE_ENV);
        // SAFETY: the ignored FX10 test is a sole exact test filter, and this
        // guard restores the process environment after both service schedules.
        unsafe { std::env::set_var(FX10_GATE_HANDSHAKE_ENV, path) };
        Self(previous)
    }
}
impl Drop for GateChildHandshakeEnvironment {
    fn drop(&mut self) {
        // SAFETY: paired with the scoped sole-test mutation above.
        unsafe {
            if let Some(previous) = self.0.take() {
                std::env::set_var(FX10_GATE_HANDSHAKE_ENV, previous);
            } else {
                std::env::remove_var(FX10_GATE_HANDSHAKE_ENV);
            }
        }
    }
}

struct GateChildHandshake {
    directory: PathBuf,
    released: bool,
}
impl GateChildHandshake {
    fn prepare(directory: PathBuf) -> Result<Self, String> {
        fs::create_dir(&directory).map_err(|error| error.to_string())?;
        Ok(Self {
            directory,
            released: false,
        })
    }

    fn started_pid(&self) -> Result<Option<i32>, String> {
        let path = self.directory.join("started");
        if !path.exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let pid = text
            .trim()
            .parse::<i32>()
            .map_err(|error| format!("invalid FX10 gate-child PID marker: {error}"))?;
        if pid <= 0 {
            return Err("FX10 gate-child PID marker is not positive".into());
        }
        Ok(Some(pid))
    }

    fn is_complete(&self) -> bool {
        self.directory.join("completed").is_file()
    }

    fn heartbeat_count(&self) -> Result<Option<u64>, String> {
        let path = self.directory.join("heartbeat");
        if !path.is_file() {
            return Ok(None);
        }
        let text = fs::read_to_string(path).map_err(|error| error.to_string())?;
        let count = text
            .trim()
            .parse::<u64>()
            .map_err(|error| format!("invalid FX10 gate-child heartbeat: {error}"))?;
        Ok(Some(count))
    }

    fn release(&mut self) -> Result<i64, String> {
        let path = self.directory.join("release");
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| error.to_string())?;
        std::io::Write::write_all(&mut file, b"release\n").map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        self.released = true;
        now_ms()
    }

    fn wait_completed(&self) -> Result<i64, String> {
        let deadline = Instant::now() + Duration::from_secs(FX10_OBSERVER_TIMEOUT_SECONDS);
        let path = self.directory.join("completed");
        loop {
            if path.is_file() {
                let result = fs::read_to_string(path).map_err(|error| error.to_string())?;
                if result.trim() != "passed" {
                    return Err("FX10 gate-child completion marker is not a passing check".into());
                }
                return now_ms();
            }
            if Instant::now() >= deadline {
                return Err(
                    "FX10 real gate child did not complete within its bounded observer window"
                        .into(),
                );
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for GateChildHandshake {
    fn drop(&mut self) {
        if !self.released {
            crate::warn_unrecorded(
                "release abandoned FX10 gate child",
                fs::write(self.directory.join("release"), b"release\n"),
            );
        }
    }
}

fn active_gate_capacity_lease(
    holder_lease_id: &str,
) -> Result<Option<crate::resources::HostResourceLease>, String> {
    HostResourceCoordinator::open_default()
        .map_err(|error| error.to_string())?
        .list(false)
        .map_err(|error| error.to_string())
        .map(|leases| {
            leases.into_iter().find(|lease| {
                lease.lease_id != holder_lease_id
                    && lease.state.as_str() == "active"
                    && lease.allocations.iter().any(|allocation| {
                        allocation.key == "fx10_sustained_arrival_slot"
                            && allocation.kind == "capacity"
                            && allocation.value == FX10_RESOURCE_POOL
                            && allocation.units == Some(1)
                            && allocation.capacity_limit == Some(1)
                    })
            })
        })
}

fn dispatching_native_job(
    store: &CollaborationStore,
    owner: &target::ControllerOwner,
    revision: &str,
) -> Result<(String, aethyme_contracts::experimental_v0::RecordId, u64), String> {
    let deadline = Instant::now() + Duration::from_secs(FX10_OBSERVER_TIMEOUT_SECONDS);
    let epoch = owner.authority_epoch().as_str();
    let mut polls = 0_u64;
    loop {
        polls = polls.saturating_add(1);
        let found: Option<(String, String)> = store
            .read_connection()
            .query_row(
                "SELECT id,wave FROM local_native_gate_jobs WHERE target=?1 AND revision=?2 AND epoch=?3 AND state='dispatching' LIMIT 1",
                (owner.target(), revision, epoch),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some((job_id, wave_text)) = found {
            let wave_id = aethyme_contracts::experimental_v0::RecordId::parse(&wave_text)
                .map_err(|error| error.to_string())?;
            let job_key = aethyme_contracts::experimental_v0::RecordId::parse(&job_id)
                .map_err(|error| error.to_string())?;
            let job =
                crate::native_gates::load(store, &job_key).map_err(|error| error.to_string())?;
            let wave = wave::load(store, &wave_id).map_err(|error| error.to_string())?;
            if job.spec.target != owner.target()
                || job.spec.revision != revision
                || job.spec.epoch != epoch
                || job.spec.wave != wave_id.as_str()
                || job.state != "dispatching"
                || wave.spec.target != owner.target()
                || wave.spec.revision != revision
                || sorted(&wave.spec.selected) != ["A", "B", "C"]
            {
                return Err("FX10 controller-created dispatch did not match the exact target, revision, epoch and older group".into());
            }
            return Ok((job_id, wave_id, polls));
        }
        if Instant::now() >= deadline {
            return Err(
                "bounded observer did not discover the controller-created native gate dispatch"
                    .into(),
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn assert_active_older_dispatch(
    store: &CollaborationStore,
    owner: &target::ControllerOwner,
    revision: &str,
    wave_id: &aethyme_contracts::experimental_v0::RecordId,
    job_id: &str,
) -> Result<(String, i64), String> {
    let job = crate::native_gates::load(
        store,
        &aethyme_contracts::experimental_v0::RecordId::parse(job_id)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    let active = wave::load(store, wave_id).map_err(|error| error.to_string())?;
    let target_status = target::status(store, owner.target()).map_err(|error| error.to_string())?;
    let completions: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_controller_completions WHERE target=?1",
            [owner.target()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if job.state != "dispatching"
        || job.spec.target != owner.target()
        || job.spec.revision != revision
        || job.spec.epoch != owner.authority_epoch().as_str()
        || job.spec.wave != wave_id.as_str()
        || active.phase != WavePhase::Verifying
        || active.spec.target != owner.target()
        || active.spec.revision != revision
        || sorted(&active.spec.selected) != ["A", "B", "C"]
        || target_status.phase != Phase::Verifying
    {
        return Err(
            "FX10 older controller-created native wave left its exact Verifying state".into(),
        );
    }
    Ok((format!("{:?}", target_status.phase), completions))
}

fn persisted_old_group_arrivals(
    store: &CollaborationStore,
    revision: &str,
) -> Result<PersistedOldGroupArrivals, String> {
    let connection = store.read_connection();
    let mut statement = connection
        .prepare(
            "SELECT contribution,sequence,first_seen_ms FROM local_wave_arrivals WHERE revision=?1 AND contribution IN ('A','B','C') ORDER BY sequence",
        )
        .map_err(|error| error.to_string())?;
    let arrivals = statement
        .query_map([revision], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if arrivals
        .iter()
        .map(|(id, _, _)| id.as_str())
        .collect::<Vec<_>>()
        != ["A", "B", "C"]
    {
        return Err(
            "FX10 persisted older-group arrival rows are incomplete or out of submission order"
                .into(),
        );
    }
    let oldest_member = arrivals
        .iter()
        .map(|(_, _, at)| *at)
        .min()
        .ok_or_else(|| "FX10 older group has no persisted arrival time".to_string())?;
    let group_ready = arrivals
        .iter()
        .map(|(_, _, at)| *at)
        .max()
        .ok_or_else(|| "FX10 older group has no persisted readiness time".to_string())?;
    Ok(PersistedOldGroupArrivals {
        arrival_sequences: arrivals,
        oldest_member_first_seen_unix_ms: oldest_member,
        group_ready_unix_ms: group_ready,
    })
}

fn persisted_arrival(
    store: &CollaborationStore,
    revision: &str,
    contribution: &str,
) -> Result<(i64, i64), String> {
    store
        .read_connection()
        .query_row(
            "SELECT sequence,first_seen_ms FROM local_wave_arrivals WHERE revision=?1 AND contribution=?2",
            (revision, contribution),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("FX10 persisted arrival row for {contribution} is missing"))
}

fn await_minimum_contention_window(
    store: &CollaborationStore,
    active_wave: &ActiveNativeWave<'_>,
    hold: &ContendedResource,
    handshake: &GateChildHandshake,
    window: Duration,
) -> Result<u64, String> {
    let deadline = Instant::now() + Duration::from_secs(FX10_OBSERVER_TIMEOUT_SECONDS);
    let until = Instant::now() + window;
    let mut polls = 0_u64;
    while Instant::now() < until {
        polls = polls.saturating_add(1);
        let (_, completions) = active_wave.assert_active(store)?;
        if completions != 0
            || !hold.is_active()?
            || handshake.started_pid()?.is_some()
            || handshake.is_complete()
            || active_gate_capacity_lease(&hold.grant.lease.lease_id)?.is_some()
        {
            return Err("FX10 native dispatch acquired capacity or started its child before the finite holder released".into());
        }
        if Instant::now() >= deadline {
            return Err("bounded resource contention window expired".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(polls)
}

fn await_gate_child_started(
    store: &CollaborationStore,
    active_wave: &ActiveNativeWave<'_>,
    holder: &crate::resources::HostResourceLease,
    holder_release_unix_ms: i64,
    handshake: &GateChildHandshake,
) -> Result<(i32, crate::resources::HostResourceLease, u64, i64), String> {
    let deadline = Instant::now() + Duration::from_secs(FX10_OBSERVER_TIMEOUT_SECONDS);
    let mut polls = 0_u64;
    loop {
        polls = polls.saturating_add(1);
        active_wave.assert_active(store)?;
        let child_pid = handshake.started_pid()?;
        let gate_lease = active_gate_capacity_lease(&holder.lease_id)?;
        if let (Some(pid), Some(lease)) = (child_pid, gate_lease) {
            if lease.created_at < holder_release_unix_ms {
                return Err(format!(
                    "FX10 gate-child resource lease predates the finite holder release: lease_created_at={}, holder_release_unix_ms={holder_release_unix_ms}",
                    lease.created_at
                ));
            }
            await_gate_child_heartbeat(
                || handshake.heartbeat_count(),
                || handshake.is_complete(),
                deadline,
            )?;
            if handshake.is_complete() {
                return Err(
                    "FX10 gate child completed before its live admission could be recorded".into(),
                );
            }
            return Ok((pid, lease, polls, now_ms()?));
        }
        if Instant::now() >= deadline {
            return Err("bounded observer did not see an admitted live FX10 gate child".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn await_gate_child_heartbeat(
    mut heartbeat_count: impl FnMut() -> Result<Option<u64>, String>,
    mut is_complete: impl FnMut() -> bool,
    deadline: Instant,
) -> Result<u64, String> {
    let mut last_heartbeat = None;
    loop {
        if is_complete() {
            return Err(format!(
                "FX10 gate child completed before a live heartbeat was observed; last heartbeat marker={last_heartbeat:?}"
            ));
        }
        last_heartbeat = heartbeat_count()?;
        if let Some(heartbeat) = last_heartbeat.filter(|heartbeat| *heartbeat > 0) {
            return Ok(heartbeat);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "bounded observer did not see the FX10 gate-child heartbeat; last marker={last_heartbeat:?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn fx10_gate_child_start_waits_for_first_heartbeat() {
    let mut heartbeat_markers = [None, Some(1)].into_iter();
    let mut polls = 0;
    let heartbeat = await_gate_child_heartbeat(
        || {
            polls += 1;
            Ok(heartbeat_markers.next().flatten())
        },
        || false,
        Instant::now() + Duration::from_secs(1),
    )
    .expect("started marker may precede the first heartbeat");

    assert_eq!(heartbeat, 1);
    assert_eq!(polls, 2);
}

fn await_live_gate_child_window(
    store: &CollaborationStore,
    active_wave: &ActiveNativeWave<'_>,
    holder: &ContendedResource,
    gate_lease: &crate::resources::HostResourceLease,
    child_pid: i32,
    handshake: &GateChildHandshake,
    window: Duration,
) -> Result<(u64, u64), String> {
    let deadline = Instant::now() + Duration::from_secs(FX10_OBSERVER_TIMEOUT_SECONDS);
    let until = Instant::now() + window;
    let initial_heartbeat = handshake
        .heartbeat_count()?
        .ok_or_else(|| "FX10 gate child has not emitted its live heartbeat".to_string())?;
    let mut polls = 0_u64;
    while Instant::now() < until {
        polls = polls.saturating_add(1);
        assert_live_gate_child(store, active_wave, holder, gate_lease, child_pid, handshake)?;
        if Instant::now() >= deadline {
            return Err("bounded live gate-child window expired".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let final_heartbeat = handshake.heartbeat_count()?.ok_or_else(|| {
        "FX10 gate child stopped emitting heartbeats during its live window".to_string()
    })?;
    let heartbeat_updates = final_heartbeat.saturating_sub(initial_heartbeat);
    if heartbeat_updates == 0 {
        return Err(
            "FX10 real gate child did not advance its heartbeat during the bounded live window"
                .into(),
        );
    }
    Ok((polls, heartbeat_updates))
}

fn assert_live_gate_child(
    store: &CollaborationStore,
    active_wave: &ActiveNativeWave<'_>,
    holder: &ContendedResource,
    gate_lease: &crate::resources::HostResourceLease,
    child_pid: i32,
    handshake: &GateChildHandshake,
) -> Result<u64, String> {
    let (_, completions) = active_wave.assert_active(store)?;
    let actual_lease = active_gate_capacity_lease(&holder.grant.lease.lease_id)?;
    let heartbeat = handshake
        .heartbeat_count()?
        .filter(|count| *count > 0)
        .ok_or_else(|| "FX10 actual gate child has not emitted a live heartbeat".to_string())?;
    if completions != 0
        || holder.is_active()?
        || actual_lease.as_ref().map(|lease| lease.lease_id.as_str())
            != Some(gate_lease.lease_id.as_str())
        || handshake.started_pid()? != Some(child_pid)
        || handshake.is_complete()
    {
        return Err(
            "FX10 older wave or real gate child did not remain active during later arrivals".into(),
        );
    }
    Ok(heartbeat)
}

fn observe_sustained_arrivals(
    host: PathBuf,
    owner: &target::ControllerOwner,
    revision: String,
    mut hold: ContendedResource,
    mut handshake: GateChildHandshake,
) -> Result<ArrivalObservation, String> {
    let root = CollaborationRoot::under_host_state(&host);
    let project = ProjectKey::parse(FX10_PROJECT).map_err(|error| error.to_string())?;
    let mut observer =
        CollaborationStore::open(&root, &project, &[]).map_err(|error| error.to_string())?;
    let (job_id, active_wave, mut observer_polls) =
        dispatching_native_job(&observer, owner, &revision)?;
    let active_dispatch = ActiveNativeWave {
        owner,
        revision: &revision,
        wave_id: &active_wave,
        job_id: &job_id,
    };
    let PersistedOldGroupArrivals {
        arrival_sequences,
        oldest_member_first_seen_unix_ms,
        group_ready_unix_ms,
    } = persisted_old_group_arrivals(&observer, &revision)?;
    let dispatch_seen_unix_ms = now_ms()?;
    let oldest_member_age_at_dispatch_ms =
        dispatch_seen_unix_ms.saturating_sub(oldest_member_first_seen_unix_ms);
    let ready_group_age_at_dispatch_ms = dispatch_seen_unix_ms.saturating_sub(group_ready_unix_ms);
    let (target_phase_before, completions_before) = active_dispatch.assert_active(&observer)?;
    if completions_before != 0 || !hold.is_active()? {
        return Err("FX10 controller-created older wave or real capacity hold was not active before arrivals".into());
    }

    let contention_started = Instant::now();
    let polls_while_resource_held = await_minimum_contention_window(
        &observer,
        &active_dispatch,
        &hold,
        &handshake,
        Duration::from_millis(FX10_MIN_CONTENTION_MS),
    )?;
    let typed_holder = hold.typed_hold();
    let released_holder = hold.release()?;
    let holder_release_unix_ms = released_holder
        .released_at
        .ok_or_else(|| "FX10 resource holder release lacks a durable timestamp".to_string())?;
    let contention_window_ms = contention_started.elapsed().as_millis();
    let (gate_child_pid, gate_resource_lease, child_polls, gate_child_started_unix_ms) =
        await_gate_child_started(
            &observer,
            &active_dispatch,
            &released_holder,
            holder_release_unix_ms,
            &handshake,
        )?;
    observer_polls = observer_polls.saturating_add(child_polls);

    let mut arrivals = Vec::with_capacity(3);
    for id in ["D", "E", "F"] {
        let heartbeat_before_arrival = assert_live_gate_child(
            &observer,
            &active_dispatch,
            &hold,
            &gate_resource_lease,
            gate_child_pid,
            &handshake,
        )?;
        let at = now_ms()?;
        wave::available(&mut observer, owner, id, at).map_err(|error| error.to_string())?;
        let (sequence, first_seen_unix_ms) = persisted_arrival(&observer, &revision, id)?;
        let observed_while_gate_child_running_unix_ms = now_ms()?;
        let heartbeat_count = assert_live_gate_child(
            &observer,
            &active_dispatch,
            &hold,
            &gate_resource_lease,
            gate_child_pid,
            &handshake,
        )?;
        arrivals.push(ArrivalEvent {
            id: id.into(),
            sequence,
            first_seen_unix_ms,
            observed_while_gate_child_running_unix_ms,
            heartbeat_count: heartbeat_count.max(heartbeat_before_arrival),
        });
        observer_polls = observer_polls.saturating_add(1);
    }

    let gate_child_running_started = Instant::now();
    let (polls_while_gate_child_running, gate_child_heartbeat_updates) =
        await_live_gate_child_window(
            &observer,
            &active_dispatch,
            &hold,
            &gate_resource_lease,
            gate_child_pid,
            &handshake,
            Duration::from_millis(FX10_MIN_GATE_CHILD_RUNNING_MS),
        )?;
    observer_polls = observer_polls.saturating_add(polls_while_gate_child_running);
    let (target_phase_after, completions_before_release) =
        active_dispatch.assert_active(&observer)?;
    if completions_before_release != 0 {
        return Err("FX10 active A-C wave completed before the gate-child release".into());
    }
    let active_after = wave::load(&observer, &active_wave).map_err(|error| error.to_string())?;
    let gate_child_running_window_ms = gate_child_running_started.elapsed().as_millis();
    let gate_child_release_unix_ms = handshake.release()?;
    let gate_child_completed_unix_ms = handshake.wait_completed()?;

    Ok(ArrivalObservation {
        native_job: job_id,
        native_wave_sequence: active_after.spec.sequence,
        native_wave: active_wave,
        dispatch_seen_unix_ms,
        oldest_member_first_seen_unix_ms,
        group_ready_unix_ms,
        oldest_member_age_at_dispatch_ms,
        ready_group_age_at_dispatch_ms,
        arrival_sequences,
        arrivals,
        observer_polls,
        polls_while_resource_held,
        polls_while_gate_child_running,
        contention_window_ms,
        gate_child_pid,
        gate_child_started_unix_ms,
        gate_child_running_window_ms,
        gate_child_heartbeat_updates,
        gate_child_release_unix_ms,
        gate_child_completed_unix_ms,
        gate_resource_lease,
        typed_holder,
        released_holder,
        holder_release_unix_ms,
        target_phase_before,
        target_phase_after,
        active_wave_phase_after: format!("{:?}", active_after.phase),
        active_wave_selection_after: sorted(&active_after.spec.selected),
        completions_before_release,
    })
}

fn persist_external_event(
    progress: &Mutex<Value>,
    report_path: &Path,
    stage: &str,
    facts: Value,
) -> Result<(), String> {
    let mut report = progress
        .lock()
        .map_err(|_| "FX10 progress report mutex was poisoned".to_string())?;
    save_event(
        report_path,
        &mut report,
        json!({"at":format!("external-target-update.{stage}"),"facts":facts}),
    )
}

struct DriftObservation {
    native_job: String,
    native_wave: aethyme_contracts::experimental_v0::RecordId,
    dispatch_seen_unix_ms: i64,
    oldest_member_first_seen_unix_ms: i64,
    group_ready_unix_ms: i64,
    ready_group_age_at_dispatch_ms: i64,
    arrival_sequences: Vec<(String, i64, i64)>,
    observer_polls: u64,
    polls_while_resource_held: u64,
    contention_window_ms: u128,
    gate_child_pid: i32,
    gate_child_started_unix_ms: i64,
    gate_resource_lease: crate::resources::HostResourceLease,
    gate_child_running_before_ref_update_ms: u128,
    gate_child_heartbeat_updates_before_ref_update: u64,
    gate_child_running_after_ref_update_ms: u128,
    gate_child_heartbeat_updates_after_ref_update: u64,
    gate_child_release_unix_ms: i64,
    gate_child_completed_unix_ms: i64,
    typed_holder: Value,
    released_holder: crate::resources::HostResourceLease,
    holder_release_unix_ms: i64,
    remote_tip_before: String,
    remote_tip_after: String,
    remote_tree_before: String,
    remote_tree_after: String,
    ref_update_attempt_started_unix_ms: i64,
    target_advanced_unix_ms: i64,
    target_phase_after: String,
    active_wave_phase_after: String,
    completions_before_update: i64,
    completions_after_update: i64,
}

struct ExternalRefAdvanceContext<'a> {
    host: PathBuf,
    repo: PathBuf,
    remote: PathBuf,
    owner: &'a target::ControllerOwner,
    revision: String,
    baseline_commit: String,
    advanced_commit: String,
    hold: ContendedResource,
    handshake: GateChildHandshake,
    progress: &'a Mutex<Value>,
    report_path: &'a Path,
}

fn observe_external_ref_advance(
    context: ExternalRefAdvanceContext<'_>,
) -> Result<DriftObservation, String> {
    let ExternalRefAdvanceContext {
        host,
        repo,
        remote,
        owner,
        revision,
        baseline_commit,
        advanced_commit,
        mut hold,
        mut handshake,
        progress,
        report_path,
    } = context;
    let root = CollaborationRoot::under_host_state(&host);
    let project = ProjectKey::parse(FX10_PROJECT).map_err(|error| error.to_string())?;
    let observer =
        CollaborationStore::open(&root, &project, &[]).map_err(|error| error.to_string())?;
    let (job_id, active_wave, mut observer_polls) =
        dispatching_native_job(&observer, owner, &revision)?;
    let active_dispatch = ActiveNativeWave {
        owner,
        revision: &revision,
        wave_id: &active_wave,
        job_id: &job_id,
    };
    let PersistedOldGroupArrivals {
        arrival_sequences,
        oldest_member_first_seen_unix_ms,
        group_ready_unix_ms,
    } = persisted_old_group_arrivals(&observer, &revision)?;
    let dispatch_seen_unix_ms = now_ms()?;
    let ready_group_age_at_dispatch_ms = dispatch_seen_unix_ms.saturating_sub(group_ready_unix_ms);
    let (target_phase_before, completions_before_update) =
        active_dispatch.assert_active(&observer)?;
    if completions_before_update != 0 || !hold.is_active()? {
        return Err("FX10 drift observer did not begin on the controller-created older wave behind a live typed hold".into());
    }
    let typed_holder = hold.typed_hold();
    persist_external_event(
        progress,
        report_path,
        "controller-created-dispatch",
        json!({
            "target":owner.target(),
            "revision":&revision,
            "epoch":owner.authority_epoch().as_str(),
            "wave":active_wave.as_str(),
            "native_job":&job_id,
            "native_job_state":"dispatching",
            "selected":["A","B","C"],
            "target_phase":target_phase_before,
            "completions_before_update":completions_before_update,
            "arrival_sequences":&arrival_sequences,
            "oldest_member_first_seen_unix_ms":oldest_member_first_seen_unix_ms,
            "group_ready_unix_ms":group_ready_unix_ms,
            "ready_group_age_at_dispatch_ms":ready_group_age_at_dispatch_ms,
            "dispatch_seen_unix_ms":dispatch_seen_unix_ms,
            "typed_holder":&typed_holder,
        }),
    )?;

    let contention_started = Instant::now();
    let polls_while_resource_held = await_minimum_contention_window(
        &observer,
        &active_dispatch,
        &hold,
        &handshake,
        Duration::from_millis(FX10_MIN_CONTENTION_MS),
    )?;
    let released_holder = hold.release()?;
    let holder_release_unix_ms = released_holder
        .released_at
        .ok_or_else(|| "FX10 resource holder release lacks a durable timestamp".to_string())?;
    let contention_window_ms = contention_started.elapsed().as_millis();
    persist_external_event(
        progress,
        report_path,
        "capacity-holder-released",
        json!({
            "holder":&released_holder,
            "holder_release_unix_ms":holder_release_unix_ms,
            "minimum_contention_ms":FX10_MIN_CONTENTION_MS,
            "measured_contention_ms":contention_window_ms,
            "native_job":&job_id,
            "wave":active_wave.as_str(),
            "polls_while_resource_held":polls_while_resource_held,
        }),
    )?;
    let admission = await_gate_child_started(
        &observer,
        &active_dispatch,
        &released_holder,
        holder_release_unix_ms,
        &handshake,
    );
    let (gate_child_pid, gate_resource_lease, child_polls, gate_child_started_unix_ms) =
        match admission {
            Ok(admission) => admission,
            Err(error) => {
                let started_pid_before_cleanup = handshake
                    .started_pid()
                    .map(|pid| json!(pid))
                    .unwrap_or_else(
                        |observation_error| json!({"observation_error":observation_error}),
                    );
                let heartbeat_before_cleanup = handshake
                    .heartbeat_count()
                    .map(|count| json!(count))
                    .unwrap_or_else(
                        |observation_error| json!({"observation_error":observation_error}),
                    );
                let active_gate_lease_before_cleanup =
                    active_gate_capacity_lease(&released_holder.lease_id)
                        .map(|lease| json!(lease))
                        .unwrap_or_else(
                            |observation_error| json!({"observation_error":observation_error}),
                        );
                let evidence = persist_external_event(
                    progress,
                    report_path,
                    "native-gate-child-admission-failed",
                    json!({
                        "error":&error,
                        "native_job":&job_id,
                        "wave":active_wave.as_str(),
                        "holder":&released_holder,
                        "holder_release_unix_ms":holder_release_unix_ms,
                        "gate_child_started_pid_before_cleanup":started_pid_before_cleanup,
                        "gate_child_heartbeat_before_cleanup":heartbeat_before_cleanup,
                        "gate_child_completed_before_cleanup":handshake.is_complete(),
                        "active_gate_resource_lease_before_cleanup":active_gate_lease_before_cleanup,
                        "expected_remote_ref_before_update":&baseline_commit,
                        "proposed_remote_ref_after_update":&advanced_commit,
                        "remote_ref_before_update_observed":null,
                        "remote_ref_advance_attempted":false,
                        "external_verifier_invoked":false,
                        "publication_transport_invoked":false,
                        "observed_at_unix_ms":now_ms().ok(),
                    }),
                );
                if let Err(persist_error) = evidence {
                    return Err(format!(
                        "{error}; failed to persist pre-cleanup gate admission evidence: {persist_error}"
                    ));
                }
                return Err(error);
            }
        };
    observer_polls = observer_polls.saturating_add(child_polls);
    persist_external_event(
        progress,
        report_path,
        "native-gate-child-running",
        json!({
            "native_job":&job_id,
            "wave":active_wave.as_str(),
            "native_job_state":"dispatching",
            "gate_child_pid":gate_child_pid,
            "gate_child_started_unix_ms":gate_child_started_unix_ms,
            "resource_lease":&gate_resource_lease,
            "holder_release_unix_ms":holder_release_unix_ms,
        }),
    )?;

    let remote_tip_before = git_static(&remote, &["rev-parse", "refs/heads/accepted"])?;
    let remote_tip_matches_baseline = remote_tip_before == baseline_commit;
    persist_external_event(
        progress,
        report_path,
        "ref-state-before-update",
        json!({
            "remote_tip_before":&remote_tip_before,
            "expected_baseline_tip":&baseline_commit,
            "matches_expected_baseline":remote_tip_matches_baseline,
            "ref":"refs/heads/accepted",
        }),
    )?;
    if !remote_tip_matches_baseline {
        return Err("FX10 drift remote changed before the controlled in-flight ref update".into());
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
    let live_before_ref_update_started_unix_ms = now_ms()?;
    persist_external_event(
        progress,
        report_path,
        "native-gate-child-running-before-ref-advance-started",
        json!({
            "native_job":&job_id,
            "wave":active_wave.as_str(),
            "gate_child_pid":gate_child_pid,
            "started_unix_ms":live_before_ref_update_started_unix_ms,
            "minimum_running_window_ms":FX10_MIN_GATE_CHILD_RUNNING_MS,
            "confirmed_remote_tip":&remote_tip_before,
        }),
    )?;
    let live_before_ref_update_started = Instant::now();
    let (polls_before_ref_update, gate_child_heartbeat_updates_before_ref_update) =
        await_live_gate_child_window(
            &observer,
            &active_dispatch,
            &hold,
            &gate_resource_lease,
            gate_child_pid,
            &handshake,
            Duration::from_millis(FX10_MIN_GATE_CHILD_RUNNING_MS),
        )?;
    let gate_child_running_before_ref_update_ms =
        live_before_ref_update_started.elapsed().as_millis();
    observer_polls = observer_polls.saturating_add(polls_before_ref_update);
    persist_external_event(
        progress,
        report_path,
        "native-gate-child-running-before-ref-advance",
        json!({
            "native_job":&job_id,
            "wave":active_wave.as_str(),
            "gate_child_pid":gate_child_pid,
            "running_window_ms":gate_child_running_before_ref_update_ms,
            "minimum_running_window_ms":FX10_MIN_GATE_CHILD_RUNNING_MS,
            "heartbeat_updates":gate_child_heartbeat_updates_before_ref_update,
            "polls":polls_before_ref_update,
        }),
    )?;
    let gate_child_live_before_update = handshake
        .heartbeat_count()?
        .is_some_and(|heartbeat| heartbeat > 0)
        && !handshake.is_complete();
    let ref_update_attempt_started_unix_ms = now_ms()?;
    persist_external_event(
        progress,
        report_path,
        "ref-advance-attempt-started",
        json!({
            "remote_tip_before":&remote_tip_before,
            "proposed_remote_tip":&advanced_commit,
            "ref":"refs/heads/accepted",
            "compare_and_swap_expected":&baseline_commit,
            "attempt_started_unix_ms":ref_update_attempt_started_unix_ms,
            "gate_child_pid":gate_child_pid,
            "gate_child_running":gate_child_live_before_update,
        }),
    )?;
    if !gate_child_live_before_update {
        return Err(
            "FX10 controlled ref advance did not begin while the real gate child was live".into(),
        );
    }
    let ref_update_result = git_static(
        &remote,
        &[
            "update-ref",
            "-m",
            "FX10 controlled external target advance",
            "refs/heads/accepted",
            &advanced_commit,
            &baseline_commit,
        ],
    );
    let remote_tip_observation = git_static(&remote, &["rev-parse", "refs/heads/accepted"]);
    let target_advanced_unix_ms = match &remote_tip_observation {
        Ok(observed_tip) if observed_tip == &advanced_commit => Some(now_ms()?),
        _ => None,
    };
    persist_external_event(
        progress,
        report_path,
        "ref-advance-observed-after-command",
        json!({
            "remote_tip_before":&remote_tip_before,
            "proposed_remote_tip":&advanced_commit,
            "ref_update_command_succeeded":ref_update_result.is_ok(),
            "ref_update_command_error":ref_update_result.as_ref().err(),
            "remote_tip_after":remote_tip_observation.as_ref().ok(),
            "remote_tip_read_error":remote_tip_observation.as_ref().err(),
            "confirmed_ref_advance_unix_ms":target_advanced_unix_ms,
            "update_count":1,
        }),
    )?;
    ref_update_result
        .map_err(|error| format!("FX10 controlled update-ref command failed: {error}"))?;
    let remote_tip_after = remote_tip_observation.map_err(|error| error.to_string())?;
    if remote_tip_after != advanced_commit {
        return Err("FX10 controlled target update did not advance the exact bare ref".into());
    }
    let target_advanced_unix_ms = target_advanced_unix_ms.ok_or_else(|| {
        "FX10 confirmed ref update is missing its post-read timestamp".to_string()
    })?;
    let remote_tree_before = git_static(
        &remote,
        &["rev-parse", &format!("{remote_tip_before}^{{tree}}")],
    )?;
    let remote_tree_after = git_static(
        &remote,
        &["rev-parse", &format!("{remote_tip_after}^{{tree}}")],
    )?;
    let same_tree_fast_forward = remote_tree_before == remote_tree_after
        && git_static(
            &repo,
            &[
                "merge-base",
                "--is-ancestor",
                &remote_tip_before,
                &remote_tip_after,
            ],
        )
        .is_ok();
    persist_external_event(
        progress,
        report_path,
        "same-tree-fast-forward-verified",
        json!({
            "remote_tip_before":&remote_tip_before,
            "remote_tip_after":&remote_tip_after,
            "remote_tree_before":&remote_tree_before,
            "remote_tree_after":&remote_tree_after,
            "same_tree_fast_forward":same_tree_fast_forward,
            "confirmed_ref_advance_unix_ms":target_advanced_unix_ms,
        }),
    )?;
    let live_after_ref_update_started = Instant::now();
    let (polls_after_ref_update, gate_child_heartbeat_updates_after_ref_update) =
        await_live_gate_child_window(
            &observer,
            &active_dispatch,
            &hold,
            &gate_resource_lease,
            gate_child_pid,
            &handshake,
            Duration::from_millis(FX10_MIN_GATE_CHILD_RUNNING_MS),
        )?;
    let gate_child_running_after_ref_update_ms =
        live_after_ref_update_started.elapsed().as_millis();
    observer_polls = observer_polls.saturating_add(polls_after_ref_update);
    persist_external_event(
        progress,
        report_path,
        "native-gate-child-running-after-ref-advance",
        json!({
            "native_job":&job_id,
            "wave":active_wave.as_str(),
            "confirmed_ref_advance_unix_ms":target_advanced_unix_ms,
            "gate_child_pid":gate_child_pid,
            "running_window_ms":gate_child_running_after_ref_update_ms,
            "minimum_running_window_ms":FX10_MIN_GATE_CHILD_RUNNING_MS,
            "heartbeat_updates":gate_child_heartbeat_updates_after_ref_update,
            "polls":polls_after_ref_update,
        }),
    )?;
    let in_flight_check = (|| {
        if !same_tree_fast_forward {
            return Err("FX10 external ref movement was not a same-tree fast-forward".to_string());
        }
        let (target_phase, completions) = active_dispatch.assert_active(&observer)?;
        if completions != 0 {
            return Err(format!(
                "FX10 external ref update coincided with {completions} older private completion(s)"
            ));
        }
        let active = wave::load(&observer, &active_wave).map_err(|error| error.to_string())?;
        Ok::<_, String>((target_phase, completions, active))
    })();
    let gate_child_release_result = handshake.release();
    match &gate_child_release_result {
        Ok(gate_child_release_unix_ms) => persist_external_event(
            progress,
            report_path,
            "gate-child-released-after-ref-update",
            json!({
                "native_job":&job_id,
                "wave":active_wave.as_str(),
                "gate_child_pid":gate_child_pid,
                "gate_child_release_unix_ms":gate_child_release_unix_ms,
                "confirmed_ref_advance_unix_ms":target_advanced_unix_ms,
                "in_flight_check":match &in_flight_check {
                    Ok((phase, completions, active)) => json!({
                        "result":"active",
                        "target_phase":phase,
                        "wave_phase":format!("{:?}",active.phase),
                        "completions":completions,
                    }),
                    Err(error) => json!({"result":"failed","error":error}),
                },
            }),
        )?,
        Err(error) => persist_external_event(
            progress,
            report_path,
            "gate-child-release-failed-after-ref-update",
            json!({"native_job":&job_id,"gate_child_pid":gate_child_pid,"error":error}),
        )?,
    }
    let gate_child_completed_result = match &gate_child_release_result {
        Ok(_) => handshake.wait_completed(),
        Err(error) => Err(error.clone()),
    };
    match &gate_child_completed_result {
        Ok(gate_child_completed_unix_ms) => persist_external_event(
            progress,
            report_path,
            "gate-child-completed",
            json!({
                "native_job":&job_id,
                "gate_child_pid":gate_child_pid,
                "gate_child_completed_unix_ms":gate_child_completed_unix_ms,
                "integrity_check":"passed",
            }),
        )?,
        Err(error) => persist_external_event(
            progress,
            report_path,
            "gate-child-completion-failed",
            json!({"native_job":&job_id,"gate_child_pid":gate_child_pid,"error":error}),
        )?,
    }
    let (target_phase_after, completions_after_update, active_after) = in_flight_check?;
    let gate_child_release_unix_ms = gate_child_release_result?;
    let gate_child_completed_unix_ms = gate_child_completed_result?;

    Ok(DriftObservation {
        native_job: job_id,
        native_wave: active_wave,
        dispatch_seen_unix_ms,
        oldest_member_first_seen_unix_ms,
        group_ready_unix_ms,
        ready_group_age_at_dispatch_ms,
        arrival_sequences,
        observer_polls: observer_polls.saturating_add(polls_while_resource_held),
        polls_while_resource_held,
        contention_window_ms,
        gate_child_pid,
        gate_child_started_unix_ms,
        gate_resource_lease,
        gate_child_running_before_ref_update_ms,
        gate_child_heartbeat_updates_before_ref_update,
        gate_child_running_after_ref_update_ms,
        gate_child_heartbeat_updates_after_ref_update,
        gate_child_release_unix_ms,
        gate_child_completed_unix_ms,
        typed_holder,
        released_holder,
        holder_release_unix_ms,
        remote_tip_before,
        remote_tip_after,
        remote_tree_before,
        remote_tree_after,
        ref_update_attempt_started_unix_ms,
        target_advanced_unix_ms,
        target_phase_after,
        active_wave_phase_after: format!("{:?}", active_after.phase),
        completions_before_update,
        completions_after_update,
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

struct ExternalFailureEvidenceContext<'a> {
    target: &'a str,
    revision: &'a str,
    epoch: &'a str,
    remote: &'a Path,
    expected_remote_tip: &'a str,
}

struct PublicationTransportRow {
    dispatched: i64,
    operation: Option<i64>,
    write_outcome: Option<String>,
    observation: String,
}

fn persist_external_failure_evidence(
    store: &CollaborationStore,
    report: &mut Value,
    report_path: &Path,
    context: ExternalFailureEvidenceContext<'_>,
) -> Result<(), String> {
    let confirmed_ref_advance_unix_ms = report["events"]
        .as_array()
        .and_then(|events| {
            events.iter().rev().find(|event| {
                event["at"] == "external-target-update.ref-advance-observed-after-command"
            })
        })
        .and_then(|event| event["facts"]["confirmed_ref_advance_unix_ms"].as_i64());
    let native_job_id = store.read_connection().query_row(
        "SELECT id FROM local_native_gate_jobs WHERE target=?1 AND revision=?2 AND epoch=?3 ORDER BY rowid DESC LIMIT 1",
        (context.target, context.revision, context.epoch),
        |row| row.get::<_, String>(0),
    ).optional();
    match native_job_id {
        Ok(Some(id)) => {
            let job_key = aethyme_contracts::experimental_v0::RecordId::parse(&id)
                .map_err(|error| error.to_string())?;
            match crate::native_gates::load(store, &job_key) {
                Ok(job) => save_event(
                    report_path,
                    report,
                    json!({
                        "at":"external-target-update.native-result",
                        "facts":{
                            "native_job":job.id.as_str(),
                            "target":&job.spec.target,
                            "revision":&job.spec.revision,
                            "epoch":&job.spec.epoch,
                            "wave":&job.spec.wave,
                            "state":&job.state,
                            "observation":&job.observation,
                        }
                    }),
                )?,
                Err(error) => save_event(
                    report_path,
                    report,
                    json!({"at":"external-target-update.native-result-read-failed","facts":{"native_job":id,"error":error.to_string()}}),
                )?,
            }
        }
        Ok(None) => save_event(
            report_path,
            report,
            json!({"at":"external-target-update.native-result-unavailable","facts":{"target":context.target,"revision":context.revision,"epoch":context.epoch}}),
        )?,
        Err(error) => save_event(
            report_path,
            report,
            json!({"at":"external-target-update.native-result-query-failed","error":error.to_string()}),
        )?,
    }

    let evidence_row: Result<Option<(i64, Vec<u8>)>, _> = store
        .read_connection()
        .query_row(
            "SELECT issued_ms,body FROM local_verification_evidence WHERE revision=?1 AND producer='fx10-native' ORDER BY issued_ms DESC LIMIT 1",
            [context.revision],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional();
    match evidence_row {
        Ok(Some((issued_ms, bytes))) => {
            let body = serde_json::from_slice::<Value>(&bytes);
            match body {
                Ok(body) => save_event(
                    report_path,
                    report,
                    json!({
                        "at":"external-target-update.verifier-receipt",
                        "facts":{
                            "receipt_count_at_least":1,
                            "issued_unix_ms":issued_ms,
                            "confirmed_ref_advance_unix_ms":confirmed_ref_advance_unix_ms,
                            "issued_after_confirmed_ref_advance":confirmed_ref_advance_unix_ms
                                .is_some_and(|advanced_at| issued_ms > advanced_at),
                            "advance_to_receipt_ms":confirmed_ref_advance_unix_ms
                                .map(|advanced_at| issued_ms.saturating_sub(advanced_at)),
                            "observation":&body["observation"],
                        }
                    }),
                )?,
                Err(error) => save_event(
                    report_path,
                    report,
                    json!({"at":"external-target-update.verifier-receipt-read-failed","facts":{"issued_unix_ms":issued_ms,"error":error.to_string()}}),
                )?,
            }
        }
        Ok(None) => save_event(
            report_path,
            report,
            json!({"at":"external-target-update.verifier-receipt-unavailable","facts":{"revision":context.revision}}),
        )?,
        Err(error) => save_event(
            report_path,
            report,
            json!({"at":"external-target-update.verifier-receipt-query-failed","error":error.to_string()}),
        )?,
    }

    let transport_rows = (|| -> Result<Vec<PublicationTransportRow>, rusqlite::Error> {
        let connection = store.read_connection();
        let mut statement = connection.prepare(
            "SELECT dispatched,operation,write_outcome,observation FROM local_source_transports WHERE target=?1 AND revision=?2 ORDER BY rowid",
        )?;
        let rows = statement.query_map((context.target, context.revision), |row| {
            Ok(PublicationTransportRow {
                dispatched: row.get(0)?,
                operation: row.get(1)?,
                write_outcome: row.get(2)?,
                observation: row.get(3)?,
            })
        })?;
        rows.collect()
    })();
    match transport_rows {
        Ok(rows) => {
            let transport_count = rows.len();
            let serialized_rows = rows
                .iter()
                .map(|row| {
                    json!({
                        "dispatched":row.dispatched,
                        "operation":row.operation,
                        "write_outcome":row.write_outcome,
                        "observation":row.observation,
                    })
                })
                .collect::<Vec<_>>();
            save_event(
                report_path,
                report,
                json!({"at":"external-target-update.publication-transport","facts":{"transport_count":transport_count,"rows":serialized_rows}}),
            )?
        }
        Err(error) => save_event(
            report_path,
            report,
            json!({"at":"external-target-update.publication-transport-query-failed","error":error.to_string()}),
        )?,
    }

    match git_static(context.remote, &["rev-parse", "refs/heads/accepted"]) {
        Ok(observed_tip) => save_event(
            report_path,
            report,
            json!({
                "at":"external-target-update.final-remote-ref",
                "facts":{"expected_competing_tip":context.expected_remote_tip,"observed_tip":observed_tip},
            }),
        )?,
        Err(error) => save_event(
            report_path,
            report,
            json!({"at":"external-target-update.final-remote-ref-read-failed","error":error}),
        )?,
    }
    Ok(())
}

struct ExternalTargetUpdateContext<'a> {
    store: &'a mut CollaborationStore,
    broker: &'a mut Broker,
    session_id: i64,
    host: &'a Path,
    repo: &'a Path,
    remote: &'a Path,
    baseline_commit: &'a str,
    baseline_snapshot: &'a str,
    retained: &'a [(String, String, String)],
    executor: &'a DockerExecutor,
    verifier_policy: &'a VerifierPolicy,
    drive_policy: &'a DrivePolicy,
    cap: Units,
    report: &'a mut Value,
    report_path: &'a Path,
    started: Instant,
}

fn run_external_target_update(context: ExternalTargetUpdateContext<'_>) -> Result<Value, String> {
    let ExternalTargetUpdateContext {
        store,
        broker,
        session_id,
        host,
        repo,
        remote,
        baseline_commit,
        baseline_snapshot,
        retained,
        executor,
        verifier_policy,
        drive_policy,
        cap,
        report,
        report_path,
        started,
    } = context;
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

    let mut availability_events = Vec::with_capacity(3);
    for id in ["A", "B", "C"] {
        let at = now_ms()?;
        wave::available(store, &owner, id, at).map_err(|error| error.to_string())?;
        availability_events.push((id.to_owned(), at));
    }
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
            baseline_tree.clone(),
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
    let remote_tip_before_service = git_static(remote, &["rev-parse", "refs/heads/accepted"])?;
    if remote_tip_before_service != baseline_commit {
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
    save_event(
        report_path,
        report,
        json!({
            "at":"external-target-update.prepared",
            "target":FX10_DRIFT_TARGET,
            "revision":revision.id.as_str(),
            "epoch":owner.authority_epoch().as_str(),
            "availability_events":availability_events,
            "remote_tip_before_service":remote_tip_before_service,
            "proposed_ref_advance":advanced_commit,
            "baseline_tree":baseline_tree,
            "target_ref":"refs/heads/accepted",
            "external_update_count":1,
        }),
    )?;

    let hold = hold_gate_resource(host, "external-ref-update")?;
    let appointment = ServicePolicy {
        nonce: "fx10-external-target-update".into(),
        drive: drive_policy.clone(),
        max_ticks: 1,
        max_completions: 1,
        poll_ms: 20,
        max_elapsed_ms: 180_000,
    };
    let handshake_directory = host.join("fx10-gate-child-drift");
    let handshake = GateChildHandshake::prepare(handshake_directory.clone())?;
    let _handshake_environment = GateChildHandshakeEnvironment::set(&handshake_directory);
    let progress = Mutex::new(report.clone());
    let observer_progress = &progress;
    let observer_host = host.to_path_buf();
    let observer_repo = repo.to_path_buf();
    let observer_remote = remote.to_path_buf();
    let observer_owner = &owner;
    let observer_revision = revision.id.to_string();
    let baseline = baseline_commit.to_owned();
    let advanced = advanced_commit.clone();
    let (service_result, drift_result) = std::thread::scope(|scope| {
        let observer = scope.spawn(move || {
            observe_external_ref_advance(ExternalRefAdvanceContext {
                host: observer_host,
                repo: observer_repo,
                remote: observer_remote,
                owner: observer_owner,
                revision: observer_revision,
                baseline_commit: baseline,
                advanced_commit: advanced,
                hold,
                handshake,
                progress: observer_progress,
                report_path,
            })
        });
        let service_result = serve(
            store,
            &owner,
            broker,
            &carrier,
            executor,
            &permit,
            &appointment,
            &AtomicBool::new(false),
        );
        let drift_result = observer
            .join()
            .unwrap_or_else(|_| Err("FX10 external-ref observer panicked".to_string()));
        (service_result, drift_result)
    });
    let progress_snapshot = progress
        .into_inner()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *report = progress_snapshot;
    let service_finished_unix_ms = now_ms()?;
    let service_summary = match &service_result {
        Ok(service) => json!({
            "result":"returned",
            "outcome":format!("{:?}",service.outcome),
            "ticks":service.ticks,
            "completions":service.completions.len(),
            "detail":service.detail.as_deref(),
            "finished_unix_ms":service_finished_unix_ms,
        }),
        Err(error) => json!({
            "result":"error",
            "error":error.to_string(),
            "finished_unix_ms":service_finished_unix_ms,
        }),
    };
    save_event(
        report_path,
        report,
        json!({"at":"external-target-update.service-returned","facts":service_summary}),
    )?;

    let drift = match drift_result {
        Ok(drift) => drift,
        Err(error) => {
            persist_external_failure_evidence(
                store,
                report,
                report_path,
                ExternalFailureEvidenceContext {
                    target: FX10_DRIFT_TARGET,
                    revision: revision.id.as_str(),
                    epoch: owner.authority_epoch().as_str(),
                    remote,
                    expected_remote_tip: &advanced_commit,
                },
            )?;
            return Err(error);
        }
    };

    let gate_job = match crate::native_gates::load(
        store,
        &aethyme_contracts::experimental_v0::RecordId::parse(&drift.native_job)
            .map_err(|error| error.to_string())?,
    ) {
        Ok(job) => {
            save_event(
                report_path,
                report,
                json!({
                    "at":"external-target-update.native-result",
                    "facts":{
                        "native_job":job.id.as_str(),
                        "target":job.spec.target,
                        "revision":job.spec.revision,
                        "epoch":job.spec.epoch,
                        "wave":job.spec.wave,
                    "state":&job.state,
                    "observation":&job.observation,
                    }
                }),
            )?;
            job
        }
        Err(error) => {
            save_event(
                report_path,
                report,
                json!({"at":"external-target-update.native-result-read-failed","error":error.to_string()}),
            )?;
            return Err(error.to_string());
        }
    };
    let gate_observation = gate_job
        .observation
        .as_ref()
        .ok_or_else(|| "FX10 drift-control native result is missing".to_string())?;
    let gate_admission = actual_gate_resource_admission(
        gate_observation,
        &drift.released_holder,
        drift.holder_release_unix_ms,
    )?;

    let evidence_row: Option<(i64, Vec<u8>)> = store
        .read_connection()
        .query_row(
            "SELECT issued_ms,body FROM local_verification_evidence WHERE revision=?1 AND producer='fx10-native'",
            [revision.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let evidence = evidence_row
        .as_ref()
        .map(|(_, bytes)| serde_json::from_slice::<Value>(bytes).map_err(|error| error.to_string()))
        .transpose()?;
    let issued_ms = evidence_row.as_ref().map(|(issued_ms, _)| *issued_ms);
    save_event(
        report_path,
        report,
        json!({
            "at":"external-target-update.verifier-receipt",
            "facts":{
                "receipt_count":i64::from(evidence_row.is_some()),
                "issued_unix_ms":issued_ms,
                "observation":evidence.as_ref().map(|body| &body["observation"]),
                "issued_after_confirmed_ref_advance":issued_ms.is_some_and(|issued| issued > drift.target_advanced_unix_ms),
            }
        }),
    )?;

    let transport_row: Option<PublicationTransportRow> = store
        .read_connection()
        .query_row(
            "SELECT dispatched,operation,write_outcome,observation FROM local_source_transports WHERE target=?1 AND revision=?2",
            (FX10_DRIFT_TARGET, revision.id.as_str()),
            |row| {
                Ok(PublicationTransportRow {
                    dispatched: row.get(0)?,
                    operation: row.get(1)?,
                    write_outcome: row.get(2)?,
                    observation: row.get(3)?,
                })
            },
        )
        .optional()
        .map_err(|error| error.to_string())?;
    let transport_count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_source_transports WHERE target=?1 AND revision=?2",
            (FX10_DRIFT_TARGET, revision.id.as_str()),
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    save_event(
        report_path,
        report,
        json!({
            "at":"external-target-update.publication-transport",
            "facts":{
                "transport_count":transport_count,
                "dispatched":transport_row.as_ref().map(|row| row.dispatched),
                "operation":transport_row.as_ref().and_then(|row| row.operation),
                "write_outcome":transport_row.as_ref().and_then(|row| row.write_outcome.as_deref()),
                "observation":transport_row.as_ref().map(|row| row.observation.as_str()),
            }
        }),
    )?;
    let remote_tip = git_static(remote, &["rev-parse", "refs/heads/accepted"])?;
    save_event(
        report_path,
        report,
        json!({
            "at":"external-target-update.final-remote-ref",
            "facts":{"expected_competing_tip":advanced_commit,"observed_tip":remote_tip}
        }),
    )?;

    let service = service_result.map_err(|error| error.to_string())?;
    if service.outcome != ServiceOutcome::Held
        || !service.completions.is_empty()
        || service.ticks != 1
    {
        return Err(format!(
            "FX10 external target update did not hold exactly one finite attempt: {service:?}"
        ));
    }
    if remote_tip != advanced_commit {
        return Err("FX10 failed candidate publication moved the external target ref".into());
    }
    if gate_job.state != "completed"
        || gate_job.spec.wave != drift.native_wave.as_str()
        || gate_observation["status"] != "passed"
        || gate_observation["current_authority"] != true
        || gate_observation["selected_gates"] != 1
        || drift.target_advanced_unix_ms > drift.gate_child_release_unix_ms
        || drift.gate_child_running_before_ref_update_ms
            < u128::from(FX10_MIN_GATE_CHILD_RUNNING_MS)
        || drift.gate_child_heartbeat_updates_before_ref_update == 0
        || drift.gate_child_running_after_ref_update_ms < u128::from(FX10_MIN_GATE_CHILD_RUNNING_MS)
        || drift.gate_child_heartbeat_updates_after_ref_update == 0
        || drift.completions_after_update != 0
    {
        return Err("FX10 target-ref update did not preserve one fresh real native pass after the confirmed ref advance".into());
    }
    let issued_ms =
        issued_ms.ok_or_else(|| "FX10 stale verifier receipt is missing".to_string())?;
    let evidence =
        evidence.ok_or_else(|| "FX10 stale verifier receipt body is missing".to_string())?;
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
            [drift.native_wave.as_str()],
            |row| row.get(0),
        )
        .map_err(|error| error.to_string())?;
    if unknown_run_count != 0 {
        return Err("FX10 external-ref schedule created an uncertain run".into());
    }
    let transport = transport_row
        .ok_or_else(|| "FX10 controlled ref advance has no publication transport".to_string())?;
    if transport.dispatched != 1
        || transport.operation.is_none()
        || transport.write_outcome.as_deref() != Some("failed")
    {
        return Err(format!(
            "FX10 controlled ref advance did not produce one known failed publication: dispatched={}, operation={:?}, outcome={:?}, observation={}",
            transport.dispatched,
            transport.operation,
            transport.write_outcome,
            transport.observation
        ));
    }
    if transport_count != 1 {
        return Err("FX10 drift-control service created more than one publication attempt".into());
    }
    let status = target::status(store, FX10_DRIFT_TARGET).map_err(|error| error.to_string())?;
    let wave = wave::load(store, &drift.native_wave).map_err(|error| error.to_string())?;
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
        "epoch": owner.authority_epoch().as_str(),
        "wave": drift.native_wave.as_str(),
        "native_job": drift.native_job,
        "selected_older_group": ["A","B","C"],
        "arrival_sequences": drift.arrival_sequences,
        "oldest_member_first_seen_unix_ms": drift.oldest_member_first_seen_unix_ms,
        "group_ready_unix_ms": drift.group_ready_unix_ms,
        "ready_group_age_at_native_dispatch_ms": drift.ready_group_age_at_dispatch_ms,
        "target_update": {
            "kind": "external_fast_forward_of_disposable_bare_ref",
            "remote_tip_before": drift.remote_tip_before,
            "remote_tip_after": drift.remote_tip_after,
            "remote_tree_before": drift.remote_tree_before,
            "remote_tree_after": drift.remote_tree_after,
            "same_tree_fast_forward": true,
            "external_ref_updates": 1,
            "ref_update_attempt_started_unix_ms": drift.ref_update_attempt_started_unix_ms,
            "ref_advanced_at_unix_ms": drift.target_advanced_unix_ms,
            "native_job_dispatch_seen_unix_ms": drift.dispatch_seen_unix_ms,
            "native_job_state_during_update": "dispatching",
            "gate_child_pid":drift.gate_child_pid,
            "gate_child_started_unix_ms":drift.gate_child_started_unix_ms,
            "gate_child_running_before_ref_update_ms":drift.gate_child_running_before_ref_update_ms,
            "gate_child_heartbeat_updates_before_ref_update":drift.gate_child_heartbeat_updates_before_ref_update,
            "gate_child_running_after_ref_update_ms":drift.gate_child_running_after_ref_update_ms,
            "gate_child_heartbeat_updates_after_ref_update":drift.gate_child_heartbeat_updates_after_ref_update,
            "gate_child_release_unix_ms":drift.gate_child_release_unix_ms,
            "gate_child_completed_unix_ms":drift.gate_child_completed_unix_ms,
            "gate_child_resource_lease":drift.gate_resource_lease,
            "local_target_phase_after": drift.target_phase_after,
            "local_wave_phase_after": drift.active_wave_phase_after,
            "completions_before_update": drift.completions_before_update,
            "completions_after_update_before_release": drift.completions_after_update,
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
            "operation": transport.operation,
            "write_outcome": transport.write_outcome,
            "observation": transport.observation,
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
            "minimum_gate_child_running_ms":FX10_MIN_GATE_CHILD_RUNNING_MS,
            "minimum_gate_child_running_before_ref_advance_ms":FX10_MIN_GATE_CHILD_RUNNING_MS,
            "minimum_gate_child_running_after_ref_advance_ms":FX10_MIN_GATE_CHILD_RUNNING_MS,
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
        json!({"at":"external-target-update","result":"one controlled ref advance during a real gate child produced a measured stale verifier and held known-failed publication"}),
    )?;
    Ok(result)
}

struct Fx10RunContext<'a> {
    report: &'a mut Value,
    report_path: &'a Path,
    started: Instant,
    host: &'a Path,
    repo: &'a Path,
    harness_repo: &'a Path,
    remote: &'a Path,
    drift_remote: &'a Path,
}

fn run_fx10(context: Fx10RunContext<'_>) -> Result<(), String> {
    let Fx10RunContext {
        report,
        report_path,
        started,
        host,
        repo,
        harness_repo,
        remote,
        drift_remote,
    } = context;
    let (oracle, expected_transcript, _manifest) = check_frozen_inputs()?;
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
    for id in ["A", "B", "C"] {
        wave::available(&mut store, &owner, id, now_ms()?).map_err(|error| error.to_string())?;
    }

    let gates = crate::gates::parse_gates(
        std::str::from_utf8(FX10_SOURCE_FILES[1].1).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if gates.len() != 1
        || gates[0].resource_wait_seconds != 5
        || gates[0].timeout_seconds != Some(20)
        || gates[0].command != "sh .aethyme/fx10-gate-child.sh"
    {
        return Err(
            "FX10 requires its frozen gate-child handshake and five-second resource wait".into(),
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

    let hold = hold_gate_resource(host, "positive")?;
    let handshake_directory = host.join("fx10-gate-child-positive");
    let handshake = GateChildHandshake::prepare(handshake_directory.clone())?;
    let _handshake_environment = GateChildHandshakeEnvironment::set(&handshake_directory);
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
    let target_revision = revision.id.to_string();
    let (service, arrivals) = std::thread::scope(|scope| {
        let sender = scope.spawn(move || {
            observe_sustained_arrivals(
                observer_host,
                target_owner,
                target_revision,
                hold,
                handshake,
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
    if first.wave != arrivals.native_wave.as_str()
        || first_wave.phase != WavePhase::Accepted
        || sorted(&first_wave.spec.selected) != ["A", "B", "C"]
        || sorted(&second_wave.spec.selected) != ["A", "B", "C", "D", "E", "F"]
        || first_wave.spec.sequence != arrivals.native_wave_sequence
        || second_wave.spec.sequence != arrivals.native_wave_sequence + 1
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
    let observed_old_job = crate::native_gates::load(
        &store,
        &aethyme_contracts::experimental_v0::RecordId::parse(&arrivals.native_job)
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if observed_old_job.state != "completed" || observed_old_job.spec.wave != first.wave {
        return Err(
            "FX10 controller-created first native dispatch did not become the first completed wave"
                .into(),
        );
    }
    if arrivals
        .arrivals
        .iter()
        .map(|arrival| arrival.id.as_str())
        .collect::<Vec<_>>()
        != ["D", "E", "F"]
        || arrivals
            .arrivals
            .windows(2)
            .any(|pair| pair[0].sequence >= pair[1].sequence)
        || arrivals
            .arrival_sequences
            .last()
            .is_some_and(|(_, old_sequence, _)| {
                arrivals
                    .arrivals
                    .first()
                    .is_some_and(|first_later| *old_sequence >= first_later.sequence)
            })
        || arrivals
            .arrivals
            .iter()
            .any(|arrival| arrival.first_seen_unix_ms < arrivals.gate_child_started_unix_ms)
        || arrivals.arrivals.iter().any(|arrival| {
            arrival.observed_while_gate_child_running_unix_ms < arrivals.gate_child_started_unix_ms
                || arrival.observed_while_gate_child_running_unix_ms
                    >= arrivals.gate_child_release_unix_ms
        })
        || arrivals.completions_before_release != 0
        || arrivals.gate_child_running_window_ms < u128::from(FX10_MIN_GATE_CHILD_RUNNING_MS)
        || arrivals.gate_child_heartbeat_updates == 0
        || arrivals.gate_child_completed_unix_ms < arrivals.gate_child_release_unix_ms
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
        || read_commit_file(repo, &second.accepted_commit, ".aethyme/fx10-gate-child.sh")?
            != FX10_SOURCE_FILES[8].1
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
        "old_wave_sequence": first_wave.spec.sequence,
        "old_native_job": arrivals.native_job,
        "old_native_job_dispatch_seen_unix_ms": arrivals.dispatch_seen_unix_ms,
        "oldest_member_first_seen_unix_ms": arrivals.oldest_member_first_seen_unix_ms,
        "old_group_ready_at_ms": arrivals.group_ready_unix_ms,
        "oldest_member_age_ms_at_dispatch": arrivals.oldest_member_age_at_dispatch_ms,
        "ready_group_age_ms_at_dispatch": arrivals.ready_group_age_at_dispatch_ms,
        "later_arrivals": arrivals.arrivals.iter().map(|arrival| json!({
            "contribution":arrival.id,
            "persisted_sequence":arrival.sequence,
            "persisted_first_seen_unix_ms":arrival.first_seen_unix_ms,
            "observed_while_gate_child_running_unix_ms":arrival.observed_while_gate_child_running_unix_ms,
            "gate_child_heartbeat_count":arrival.heartbeat_count,
        })).collect::<Vec<_>>(),
        "arrival_wave_waited": second_wave.spec.sequence - first_wave.spec.sequence,
        "observer_polls": arrivals.observer_polls,
        "polls_while_resource_held": arrivals.polls_while_resource_held,
        "polls_while_gate_child_running": arrivals.polls_while_gate_child_running,
        "bounded_contention_window_ms": arrivals.contention_window_ms,
        "gate_child": {
            "pid":arrivals.gate_child_pid,
            "started_unix_ms":arrivals.gate_child_started_unix_ms,
            "running_window_ms":arrivals.gate_child_running_window_ms,
            "heartbeat_updates_during_window":arrivals.gate_child_heartbeat_updates,
            "minimum_running_window_ms":FX10_MIN_GATE_CHILD_RUNNING_MS,
            "release_unix_ms":arrivals.gate_child_release_unix_ms,
            "completed_unix_ms":arrivals.gate_child_completed_unix_ms,
            "integrity_check":"passed",
        },
        "resource_contention": {
            "typed_holder": arrivals.typed_holder,
            "holder_lease_released": arrivals.released_holder,
            "holder_release_unix_ms": arrivals.holder_release_unix_ms,
            "active_gate_child_lease": arrivals.gate_resource_lease,
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
        "finite_allowance": {"batch_width_new":3,"cumulative_max_group":6,"service_ticks":8,"service_completions":2,"resource_wait_seconds":5,"observer_timeout_seconds":FX10_OBSERVER_TIMEOUT_SECONDS,"minimum_contention_ms":FX10_MIN_CONTENTION_MS,"minimum_gate_child_running_ms":FX10_MIN_GATE_CHILD_RUNNING_MS},
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
    let external_target_update = run_external_target_update(ExternalTargetUpdateContext {
        store: &mut store,
        broker: &mut broker,
        session_id: session.id,
        host,
        repo,
        remote: drift_remote,
        baseline_commit: &baseline_commit,
        baseline_snapshot: baseline_snapshot.snapshot_id.as_str(),
        retained: &retained,
        executor: &executor,
        verifier_policy: permit.policy(),
        drive_policy: &drive_policy,
        cap,
        report,
        report_path,
        started,
    })?;
    report["external_target_update"] = external_target_update;
    report["completed_at_unix_ms"] = json!(now_ms()?);
    report["cumulative_elapsed_ms"] = json!(started.elapsed().as_millis());
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
        run_fx10(Fx10RunContext {
            report: &mut report,
            report_path: &report_path,
            started,
            host: &host,
            repo: &repo,
            harness_repo: &harness_repo,
            remote: &remote,
            drift_remote: &drift_remote,
        })
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
