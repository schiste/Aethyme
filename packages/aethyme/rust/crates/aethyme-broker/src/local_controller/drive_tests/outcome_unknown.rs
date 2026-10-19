use crate::collaboration_archive::{self as archive, CommitOid};
use crate::execution_resources::ResourcePolicy;
use crate::execution_supervisor::DockerExecutor;
use crate::execution_verifier::{
    self as verifier, AssertionCase, AssertionInventory, VerifierPolicy,
};
use crate::local_budget as budget;
use crate::local_controller::ControllerError;
use crate::local_controller::drive as driver;
use crate::local_controller::service::{self as controller_service, ServiceOutcome, ServicePolicy};
use crate::local_source_transport::{self as transport, GitCarrier};
use crate::local_target::tests::{Fixture, git};
use crate::local_target::{self as target, Phase};
use crate::local_wave::{self as wave, WavePhase};
use crate::native_gates;
use crate::{
    Broker, CoordinatedOperation, OperationHistoryQuery, OperationProvider, OperationStatus,
};
use aethyme_contracts::experimental_v0::RecordId;
use rusqlite::params;
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const PINNED_IMAGE: &str =
    "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251";

fn publication_operations(
    broker: &Broker,
    session_id: i64,
    candidate: &str,
) -> Vec<CoordinatedOperation> {
    let page = broker
        .store_ref()
        .operation_history(&OperationHistoryQuery {
            limit: 100,
            session_id: Some(session_id),
            provider: Some(OperationProvider::Git),
            ..Default::default()
        })
        .unwrap();
    let pushed_ref = format!("{candidate}:refs/heads/accepted");
    page.operations
        .into_iter()
        .filter(|operation| {
            let args: Vec<String> =
                serde_json::from_str(&operation.command_json).unwrap_or_default();
            args.get(1).is_some_and(|arg| arg == "push")
                && args.iter().any(|arg| arg == &pushed_ref)
        })
        .collect()
}

fn restore_guard_counts(
    store: &crate::collaboration_state::CollaborationStore,
) -> (i64, i64, i64, i64, i64, i64, i64, i64, i64, i64) {
    store
        .read_connection()
        .query_row(
            "SELECT (SELECT count(*) FROM local_controller_services),(SELECT count(*) FROM local_controller_completions),(SELECT count(*) FROM local_controller_completion_roots),(SELECT count(*) FROM local_controller_runs),(SELECT count(*) FROM local_acceptance_receipts),(SELECT count(*) FROM local_acceptance_qualifications),(SELECT count(*) FROM local_budget_reservations),(SELECT coalesce(sum(attempts),0) FROM local_budget_reservations),(SELECT count(*) FROM local_budget_receipts),(SELECT count(*) FROM local_source_transports)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )
        .unwrap()
}

fn shell_quote(path: &Path) -> String {
    let path = path.to_str().expect("temporary path is UTF-8");
    shell_quote_text(path)
}

fn shell_quote_text(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ReceiverHookPids {
    hook: libc::pid_t,
    delay: libc::pid_t,
}

struct ReceiverHookProcess {
    started: PathBuf,
    finished: PathBuf,
    release: PathBuf,
    marker: String,
    armed: bool,
    confirmed_exit: bool,
}

impl ReceiverHookProcess {
    fn new(started: PathBuf, finished: PathBuf, release: PathBuf, marker: String) -> Self {
        Self {
            started,
            finished,
            release,
            marker,
            armed: false,
            confirmed_exit: false,
        }
    }

    fn arm(&mut self) {
        self.armed = true;
    }

    fn wait_until_live(&self) -> Result<ReceiverHookPids, String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut last_problem = "receiver hook has not recorded its process identities".to_owned();
        while Instant::now() < deadline {
            if let Ok(record) = std::fs::read_to_string(&self.started)
                && let Ok(pids) = parse_hook_pids(&record, &self.marker)
            {
                if self.finished.exists() {
                    return Err("receiver hook finished before the restore barrier".into());
                }
                if process_is_alive(pids.hook) && process_is_alive(pids.delay) {
                    return Ok(pids);
                }
                last_problem = format!(
                    "receiver hook process IDs {} and {} did not both remain alive",
                    pids.hook, pids.delay
                );
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(format!(
            "could not prove receiver hook {:?} remained alive at the restore barrier: {last_problem}",
            self.marker
        ))
    }

    fn release(&self) -> Result<(), String> {
        std::fs::write(&self.release, b"release\n")
            .map_err(|error| format!("could not release receiver hook barrier: {error}"))
    }

    fn wait_for_exit(&mut self) -> Result<ReceiverHookPids, String> {
        if self.confirmed_exit {
            return Err("receiver hook exit was already confirmed".into());
        }
        if !self.armed {
            return Err("receiver hook cleanup guard was not armed".into());
        }
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut last_problem = "receiver hook has not recorded its process identities".to_owned();
        while Instant::now() < deadline {
            let start_record = std::fs::read_to_string(&self.started)
                .ok()
                .and_then(|record| parse_hook_pids(&record, &self.marker).ok());
            let finish_record = std::fs::read_to_string(&self.finished)
                .ok()
                .and_then(|record| parse_hook_pids(&record, &self.marker).ok());
            match (start_record, finish_record) {
                (Some(start), Some(finish)) if start == finish => {
                    if !process_is_alive(start.hook) && !process_is_alive(start.delay) {
                        self.confirmed_exit = true;
                        return Ok(start);
                    }
                    last_problem = format!(
                        "receiver hook recorded completion, but process IDs {} and {} remain present",
                        start.hook, start.delay
                    );
                }
                (Some(start), Some(finish)) => {
                    last_problem = format!(
                        "receiver hook start/completion identities differ: {start:?} vs {finish:?}"
                    );
                }
                (Some(start), None) => {
                    last_problem = format!(
                        "receiver hook process IDs {} and {} have no matching completion record",
                        start.hook, start.delay
                    );
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        Err(format!(
            "could not prove receiver hook {:?} exited within its 15-second cleanup bound: {last_problem}",
            self.marker
        ))
    }
}

impl Drop for ReceiverHookProcess {
    fn drop(&mut self) {
        if self.armed && !self.confirmed_exit {
            if let Err(error) = self.release() {
                eprintln!("receiver hook release was not proven: {error}");
            }
            if let Err(error) = self.wait_for_exit() {
                eprintln!("receiver hook cleanup was not proven: {error}");
            }
        }
    }
}

struct AuthorityWorkerProcess {
    child: std::process::Child,
    host: PathBuf,
    finished: bool,
}

impl AuthorityWorkerProcess {
    fn spawn(host: &Path) -> Self {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "local_controller::drive::tests::outcome_unknown::actual_fx13_stale_worker_child",
                "--exact",
                "--ignored",
                "--nocapture",
            ])
            .env("AETHYME_FX13_AUTHORITY_CHILD_HOST", host)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .expect("spawn the stale FX13 controller worker child");
        Self {
            child,
            host: host.to_owned(),
            finished: false,
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn wait_until_ready(&mut self, expected_epoch: &str) -> Result<String, String> {
        let ready = self.host.join("fx13-worker-ready");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match read_worker_ready_file(&ready, expected_epoch) {
                Ok(bytes) => {
                    if worker_ready_file_state(&bytes, expected_epoch)? {
                        if self
                            .child
                            .try_wait()
                            .map_err(|error| {
                                format!("could not inspect FX13 worker child: {error}")
                            })?
                            .is_some()
                        {
                            self.finished = true;
                            return Err("FX13 worker child exited after recording readiness".into());
                        }
                        return Ok(expected_epoch.to_owned());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                    return Err("FX13 worker readiness file exceeds its expected length".into());
                }
                Err(error) => {
                    return Err(format!("could not read FX13 worker readiness: {error}"));
                }
            }
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|error| format!("could not inspect FX13 worker child: {error}"))?
            {
                self.finished = true;
                return Err(format!(
                    "FX13 worker child exited before readiness with {status}"
                ));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("FX13 worker child did not become ready within 10 seconds".into())
    }

    fn is_alive(&mut self) -> Result<bool, String> {
        match self.child.try_wait() {
            Ok(None) => Ok(true),
            Ok(Some(_)) => {
                self.finished = true;
                Ok(false)
            }
            Err(error) => Err(format!("could not inspect FX13 worker child: {error}")),
        }
    }

    fn release_and_wait(&mut self) -> Result<i32, String> {
        std::fs::write(
            self.host.join("fx13-worker-go"),
            b"restore finished frozen\n",
        )
        .map_err(|error| format!("could not release FX13 worker child: {error}"))?;
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|error| format!("could not reap FX13 worker child: {error}"))?
            {
                self.finished = true;
                if !status.success() {
                    return Err(format!("FX13 worker child exited unsuccessfully: {status}"));
                }
                return Ok(status.code().unwrap_or_default());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Err("FX13 worker child did not exit within 15 seconds after release".into())
    }
}

fn worker_ready_file_state(contents: &[u8], expected_epoch: &str) -> Result<bool, String> {
    let expected = format!("{expected_epoch}\n");
    if contents.len() > expected.len() {
        return Err("FX13 worker readiness exceeded its expected length".into());
    }
    if contents == expected.as_bytes() {
        return Ok(true);
    }
    if !contents.ends_with(b"\n") {
        return Ok(false);
    }
    Err("FX13 worker published an unexpected complete readiness identity".into())
}

fn read_worker_ready_file(path: &Path, expected_epoch: &str) -> std::io::Result<Vec<u8>> {
    let record_limit = expected_epoch
        .len()
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("FX13 readiness length overflow"))?;
    let read_limit = record_limit
        .checked_add(1)
        .ok_or_else(|| std::io::Error::other("FX13 readiness length overflow"))?;
    let read_limit_u64 = u64::try_from(read_limit)
        .map_err(|_| std::io::Error::other("FX13 readiness length overflow"))?;
    let mut contents = Vec::with_capacity(read_limit);
    std::fs::File::open(path)?
        .take(read_limit_u64)
        .read_to_end(&mut contents)?;
    if contents.len() > record_limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "FX13 worker readiness exceeded its expected length",
        ));
    }
    Ok(contents)
}

fn publish_worker_ready(host: &Path, epoch: &str) -> std::io::Result<()> {
    let destination = host.join("fx13-worker-ready");
    let mut temporary = tempfile::NamedTempFile::new_in(host)?;
    temporary.write_all(format!("{epoch}\n").as_bytes())?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist_noclobber(&destination)
        .map_err(|error| error.error)?;
    Ok(())
}

impl Drop for AuthorityWorkerProcess {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let _ = std::fs::write(self.host.join("fx13-worker-go"), b"test cleanup release\n");
        let deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.finished = true;
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    eprintln!("could not inspect FX13 worker during cleanup: {error}");
                    break;
                }
            }
        }
        let child_pid = self.child.id();
        if let Err(error) = self.child.kill() {
            eprintln!("could not stop FX13 worker child during cleanup: {error}");
        }
        let reap_deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < reap_deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    self.finished = true;
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(error) => {
                    eprintln!("could not inspect FX13 worker child during bounded reap: {error}");
                    break;
                }
            }
        }
        eprintln!(
            "FX13 worker child cleanup remains unproven; process {child_pid} may still be running"
        );
    }
}

fn parse_hook_pids(record: &str, expected_marker: &str) -> Result<ReceiverHookPids, String> {
    let fields: Vec<_> = record.split_whitespace().collect();
    if fields.len() != 3 || fields[2] != expected_marker {
        return Err("receiver hook marker record is malformed or belongs to another hook".into());
    }
    let hook = fields[0]
        .parse::<libc::pid_t>()
        .map_err(|error| format!("receiver hook pid is invalid: {error}"))?;
    let delay = fields[1]
        .parse::<libc::pid_t>()
        .map_err(|error| format!("receiver hook delay pid is invalid: {error}"))?;
    if hook <= 1 || delay <= 1 || hook == delay {
        return Err("receiver hook process IDs are not distinct child process IDs".into());
    }
    Ok(ReceiverHookPids { hook, delay })
}

fn process_is_alive(pid: libc::pid_t) -> bool {
    // Observe only the two PIDs recorded by the unique receiver hook. Never
    // send a signal: signal 0 probes existence, and PID reuse fails closed.
    // SAFETY: `pid` is a parsed positive PID and signal 0 has no process effect.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[test]
#[ignore = "spawned by actual_fx13_publication_survives_restore_and_old_worker_is_fenced"]
fn actual_fx13_stale_worker_child() {
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};

    let host = PathBuf::from(
        std::env::var_os("AETHYME_FX13_AUTHORITY_CHILD_HOST")
            .expect("appointed FX13 authority child host"),
    );
    let mut store = crate::collaboration_state::CollaborationStore::open(
        &CollaborationRoot::under_host_state(&host),
        &ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap();
    let owner = target::acquire_controller(&mut store, "target", "appointed").unwrap();
    let epoch = owner.authority_epoch().as_str().to_owned();
    drop(store);
    publish_worker_ready(&host, &epoch).expect("publish the complete FX13 worker epoch atomically");
    for _ in 0..12000 {
        if host.join("fx13-worker-go").exists() {
            let mut restored = crate::collaboration_state::CollaborationStore::open(
                &CollaborationRoot::under_host_state(&host),
                &ProjectKey::parse("local-target-test").unwrap(),
                &[],
            )
            .unwrap();
            let witness = crate::local_authority::status(&restored).unwrap();
            if witness.status != crate::local_authority::Status::Frozen {
                std::fs::write(
                    host.join("fx13-worker-not-frozen"),
                    b"child released before freeze; mutation skipped\n",
                )
                .unwrap();
                return;
            }
            assert!(matches!(
                wave::available(&mut restored, &owner, "A", 2),
                Err(crate::local_wave::WaveError::Target(
                    crate::local_target::TargetError::Authority(
                        crate::local_authority::AuthorityError::Frozen
                    )
                ))
            ));
            std::fs::write(host.join("fx13-worker-refused"), b"frozen authority\n").unwrap();
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("parent did not release the FX13 stale-worker child within 120 seconds");
}

#[test]
fn worker_readiness_waits_for_complete_identity_and_never_overwrites() {
    let epoch = "sha256:9390a2d6456e445a0d1fb7df90f55ac9009531edf8766dd9718235546dfa4189";
    let complete = format!("{epoch}\n");
    assert_eq!(worker_ready_file_state(b"", epoch), Ok(false));
    assert_eq!(
        worker_ready_file_state(&complete.as_bytes()[..complete.len() - 1], epoch),
        Ok(false)
    );
    assert_eq!(
        worker_ready_file_state(complete.as_bytes(), epoch),
        Ok(true)
    );
    assert_eq!(
        worker_ready_file_state(b"sha256:wrong-epoch\n", epoch).unwrap_err(),
        "FX13 worker published an unexpected complete readiness identity"
    );
    assert_eq!(
        worker_ready_file_state(format!("{complete}x").as_bytes(), epoch).unwrap_err(),
        "FX13 worker readiness exceeded its expected length"
    );

    let host = tempfile::tempdir().expect("private readiness host");
    let ready = host.path().join("fx13-worker-ready");
    publish_worker_ready(host.path(), epoch).expect("publish readiness file");
    assert_eq!(std::fs::read(&ready).unwrap(), complete.as_bytes());
    assert_eq!(
        worker_ready_file_state(&std::fs::read(&ready).unwrap(), epoch),
        Ok(true)
    );

    let existing = b"existing readiness\n";
    std::fs::write(&ready, existing).unwrap();
    let error = publish_worker_ready(host.path(), epoch).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(&ready).unwrap(), existing);
    std::fs::write(&ready, vec![b'x'; 4096]).unwrap();
    let error = read_worker_ready_file(&ready, epoch).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert_eq!(
        error.to_string(),
        "FX13 worker readiness exceeded its expected length"
    );
    let entries: Vec<_> = std::fs::read_dir(host.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, vec![std::ffi::OsString::from("fx13-worker-ready")]);
}

#[test]
#[ignore = "actual appointed Docker build, independent verifier, native gates, and timed disposable bare-Git publication"]
fn actual_fx13_publication_survives_restore_and_old_worker_is_fenced() {
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};
    use std::os::unix::fs::PermissionsExt;

    let host = tempfile::tempdir().unwrap().keep();
    eprintln!(
        "Controller actual unknown-publication evidence root {}",
        host.display()
    );
    std::fs::set_permissions(&host, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cli = std::env::var("AETHYME_DOCKER_CLI").expect("appointed actual Docker client");
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("appointed cached image");
    assert_eq!(image, PINNED_IMAGE, "use the reviewed image digest");
    let native = DockerExecutor::probe(cli.into()).unwrap();
    let runtime = native.runtime().id();
    let daemon = native.runtime().daemon_id.clone();
    let executor = native
        .with_resources(ResourcePolicy {
            host_state: host.join("resources.db"),
            temporary_root: host.clone(),
            runtime,
            daemon,
            meter_image: image.clone(),
            memory_mib: 2048,
            disk_mib: 2048,
            process_slots: 1024,
            job_slots: 2,
            host_floor_bytes: 1,
            daemon_floor_bytes: 1,
        })
        .unwrap();

    let mut f = Fixture::new();
    let source_host = std::mem::replace(&mut f._host, tempfile::tempdir().unwrap()).keep();
    let repo = std::mem::replace(&mut f._repo, tempfile::tempdir().unwrap()).keep();
    eprintln!(
        "Controller unknown-publication source host {}, repository {}",
        source_host.display(),
        repo.display()
    );
    let owner = target::acquire_controller(&mut f.store, "target", "appointed").unwrap();

    let inventory = AssertionInventory::new(vec![AssertionCase {
        id: "retained-one".into(),
        arguments: vec![],
        stdin_hex: String::new(),
        exit_code: 0,
        stdout: super::digest(b"A\n"),
        stderr: super::digest(b""),
    }])
    .unwrap();
    std::fs::create_dir_all(repo.join("harness")).unwrap();
    std::fs::write(
        repo.join("harness/inventory.json"),
        inventory.canonical_bytes(),
    )
    .unwrap();
    git(&repo, &["add", "harness/inventory.json"]);
    git(
        &repo,
        &["commit", "-qm", "independent controller behavior inventory"],
    );
    let harness_commit = CommitOid::parse(&git(&repo, &["rev-parse", "HEAD"])).unwrap();
    let harness = archive::retain_snapshot(&mut f.store, &repo, &harness_commit).unwrap();

    let mut drive = super::policy();
    drive.execution.harness = harness.snapshot_id.to_string();
    drive.execution.assertion_inventory = inventory.id().to_string();
    drive.execution.profile.build_image = image.clone();
    drive.execution.profile.verifier_image = image.clone();
    drive.execution.profile.runtime_profile = executor.runtime().id();
    drive.execution.profile.architecture = executor.runtime().architecture.clone();
    drive.wave_policy.batch_width = 1;
    let verifier_policy = VerifierPolicy {
        profile: verifier::PROFILE.into(),
        runtime: executor.runtime().id(),
        image: image.clone(),
        harness: harness.snapshot_id.to_string(),
        inventory: inventory.id().to_string(),
        command: drive.execution.verify.clone(),
        max_age_ms: 300000,
    };
    let mut selected = f.spec();
    selected.acceptance_policy = verifier_policy.record().unwrap().id().to_string();
    selected.selected = vec!["A".into()];
    let revision = target::propose_revision(&mut f.store, &owner, selected).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let permit = verifier::configure(
        &mut f.store,
        &owner,
        "controller-native",
        None,
        verifier_policy,
    )
    .unwrap();

    let cap = budget::Units {
        time_ms: 1000000000,
        memory_bytes: 64 * 1024 * 1024 * 1024,
        disk_bytes: 64 * 1024 * 1024 * 1024,
        slots: 100,
        ..Default::default()
    };
    budget::configure_project(
        &mut f.store,
        &owner,
        budget::ProjectPolicy {
            authorization: super::digest(b"controller authorization"),
            price_profile: super::digest(b"unpriced native execution"),
            cap,
            recovery_headroom: Default::default(),
            mandatory_headroom: Default::default(),
            max_root_reservations: 32,
        },
    )
    .unwrap();
    budget::authorize_objective(&mut f.store, &owner, cap).unwrap();

    let remote = tempfile::tempdir().unwrap().keep();
    git(&remote, &["init", "--bare", "-q"]);
    // Empty fixture templates do not create the receiver-owned hooks directory.
    std::fs::create_dir_all(remote.join("hooks")).unwrap();
    git(
        &repo,
        &[
            "remote",
            "add",
            "controller-accepted",
            remote.to_str().unwrap(),
        ],
    );
    let baseline = f.a.base.commit.as_str().to_owned();
    git(
        &repo,
        &[
            "push",
            "-q",
            "controller-accepted",
            &format!("{baseline}:refs/heads/accepted"),
        ],
    );
    let hook_record = remote.join("hooks/post-receive-record");
    let hook_started = remote.join("hooks/post-receive-started");
    let hook_finished = remote.join("hooks/post-receive-finished");
    let hook_release = remote.join("hooks/post-receive-release");
    let hook = remote.join("hooks/post-receive");
    let hook_marker = format!(
        "receiver-{}",
        remote
            .file_name()
            .expect("temporary receiver has a directory name")
            .to_string_lossy()
    );
    std::fs::write(
        &hook,
        format!(
            "#!/bin/sh\nset -eu\n/bin/cat > {}\nhook_pid=$$\n/bin/sh -c 'i=0; while [ ! -e \"$1\" ] && [ \"$i\" -lt 120 ]; do /bin/sleep 1; i=$((i + 1)); done' receiver-delay {} &\ndelay_pid=$!\nprintf '%s %s %s\\n' \"$hook_pid\" \"$delay_pid\" {} > {}\nwait \"$delay_pid\"\nprintf '%s %s %s\\n' \"$hook_pid\" \"$delay_pid\" {} > {}\n",
            shell_quote(&hook_record),
            shell_quote(&hook_release),
            shell_quote_text(&hook_marker),
            shell_quote(&hook_started),
            shell_quote_text(&hook_marker),
            shell_quote(&hook_finished),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut hook_process = ReceiverHookProcess::new(
        hook_started.clone(),
        hook_finished.clone(),
        hook_release.clone(),
        hook_marker.clone(),
    );

    let mut broker = Broker::open(&repo)
        .unwrap()
        .with_host_operation_database(host.join("operations.db"))
        .with_worktree_root(host.join("worktrees"));
    let session = broker
        .adopt(
            &repo,
            Some("actual receiver response timeout after ref update"),
        )
        .unwrap();
    let carrier = GitCarrier::new(
        &mut broker,
        session.id,
        "controller-accepted",
        "refs/heads/accepted",
        2,
        "Operator authorizes exact disposable local bare Git controller target",
        true,
    )
    .unwrap();

    let pre_effect_database = host.join("before-publication.db");
    f.store
        .connection()
        .execute("VACUUM INTO ?1", [pre_effect_database.to_str().unwrap()])
        .unwrap();
    let pre_effect_database_sha256 = super::digest(&std::fs::read(&pre_effect_database).unwrap());

    wave::available(&mut f.store, &owner, "A", 0).unwrap();
    let appointment = ServicePolicy {
        nonce: "post-receive-timeout-1".into(),
        drive: drive.clone(),
        max_ticks: 32,
        max_completions: 1,
        poll_ms: 20,
        max_elapsed_ms: 600000,
    };
    hook_process.arm();
    let service = controller_service::serve(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        &executor,
        &permit,
        &appointment,
        &AtomicBool::new(false),
    )
    .unwrap();
    let live_hook_pids = hook_process
        .wait_until_live()
        .expect("receiver hook must remain alive at the restore barrier");
    assert!(
        !hook_finished.exists(),
        "receiver hook must not finish before the restore/refusal checks"
    );

    assert_eq!(service.outcome, ServiceOutcome::Held);
    assert!(service.completions.is_empty());
    assert_eq!(service.ticks, 1);
    let intent_id: String = f
        .store
        .read_connection()
        .query_row(
            "SELECT intent FROM local_source_transports WHERE target=?1 AND revision=?2",
            params![owner.target(), revision.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let intent = transport::load(&f.store, &RecordId::parse(&intent_id).unwrap()).unwrap();
    let receiver_receipt_bytes = std::fs::read(&hook_record)
        .expect("receiver-owned post-receive hook must record the moved ref before delaying");
    let receiver_record = String::from_utf8(receiver_receipt_bytes.clone())
        .expect("receiver hook record must be UTF-8");
    let receiver_update: Vec<&str> = receiver_record.split_whitespace().collect();
    assert_eq!(
        receiver_update,
        vec![
            baseline.as_str(),
            intent.spec.commit.as_str(),
            "refs/heads/accepted"
        ],
        "Git invokes post-receive after applying this exact update"
    );
    let remote_commit = git(&remote, &["rev-parse", "refs/heads/accepted"]);
    assert_eq!(remote_commit, intent.spec.commit);

    let publication = publication_operations(&broker, session.id, &intent.spec.commit);
    assert_eq!(
        publication.len(),
        1,
        "one actual managed push was dispatched"
    );
    let operation = &publication[0];
    assert_eq!(operation.status, OperationStatus::OutcomeUnknown);
    assert!(
        operation.exit_code.is_none(),
        "the bounded caller timed out"
    );
    let operation_details: Value =
        serde_json::from_str(operation.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(
        operation_details["failure_class"],
        "coordinated_operation_timeout"
    );
    assert_eq!(
        operation_details["stage"],
        "executing coordinated operation"
    );
    assert_eq!(operation_details["remote_outcome"], "unknown");

    let (dispatched, observation, receipt): (i64, String, Option<Vec<u8>>) = f
        .store
        .read_connection()
        .query_row(
            "SELECT dispatched,observation,receipt FROM local_source_transports WHERE intent=?1",
            [&intent_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(dispatched, 1, "transport intent remains durably dispatched");
    assert_eq!(
        observation, "unobserved",
        "unknown write was never inspected"
    );
    assert!(
        receipt.is_none(),
        "no read receipt or private grant was manufactured"
    );
    let transport_receipts: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_source_transport_receipts WHERE intent=?1",
            [&intent_id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(transport_receipts, 0);

    let (wave_id, run_state, run_observation): (String, String, Option<Vec<u8>>) = f
        .store
        .read_connection()
        .query_row(
            "SELECT wave,state,observation FROM local_controller_runs WHERE wave IN (SELECT wave FROM local_waves WHERE target=?1)",
            [owner.target()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        run_state, "unknown",
        "the actual native dispatch history is retained"
    );
    assert!(run_observation.is_some());
    let wave_id = RecordId::parse(&wave_id).unwrap();
    assert_eq!(
        wave::load(&f.store, &wave_id).unwrap().phase,
        WavePhase::Verifying
    );
    assert_eq!(
        target::status(&f.store, owner.target()).unwrap().phase,
        Phase::Verifying
    );

    let native_id: String = f
        .store
        .read_connection()
        .query_row(
            "SELECT id FROM local_native_gate_jobs WHERE wave=?1",
            [wave_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let native_job = native_gates::load(&f.store, &RecordId::parse(&native_id).unwrap()).unwrap();
    assert_eq!(
        native_job.state, "completed",
        "real native gate job remains historical"
    );
    assert_eq!(
        native_job.observation.as_ref().unwrap()["current_authority"],
        true
    );
    let native_roots: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_native_gate_roots WHERE job=?1",
            [&native_id],
            |row| row.get(0),
        )
        .unwrap();
    assert!(native_roots > 0, "native job roots remain retained");
    let (target_roots, object_roots): (i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*),coalesce(sum(CASE WHEN kind='object' THEN 1 ELSE 0 END),0) FROM local_target_roots WHERE revision=?1",
            [revision.id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(
        target_roots > 0 && object_roots > 0,
        "candidate and run evidence remain rooted"
    );

    let (budget_rows, consumed_rows, attempts, budget_receipts): (i64, i64, i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*),sum(CASE WHEN phase='consumed' THEN 1 ELSE 0 END),coalesce(sum(attempts),0),(SELECT count(*) FROM local_budget_receipts WHERE reservation IN (SELECT reservation FROM local_budget_reservations WHERE wave=?1)) FROM local_budget_reservations WHERE wave=?1",
            [wave_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (budget_rows, consumed_rows, attempts, budget_receipts),
        (3, 3, 3, 3)
    );
    let pending_reservations: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_budget_reservations WHERE wave=?1 AND phase IN ('reserved','partly_consumed','completion_pending','consumption_unknown')",
            [wave_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending_reservations, 0);

    let (completion_rows, qualification_rows, acceptance_receipts): (i64, i64, i64) = f
        .store
        .read_connection()
        .query_row(
            "SELECT (SELECT count(*) FROM local_controller_completions WHERE wave=?1),(SELECT count(*) FROM local_acceptance_qualifications),(SELECT count(*) FROM local_acceptance_receipts)",
            [wave_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (completion_rows, qualification_rows, acceptance_receipts),
        (0, 0, 0)
    );
    let (service_state, service_report): (String, Vec<u8>) = f
        .store
        .read_connection()
        .query_row(
            "SELECT state,report FROM local_controller_services WHERE id=?1",
            [&service.appointment],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(service_state, "held");
    assert_eq!(
        serde_json::from_slice::<Value>(&service_report).unwrap(),
        serde_json::to_value(&service).unwrap()
    );

    let replay = driver::drive(
        &mut f.store,
        &owner,
        &mut broker,
        &carrier,
        &executor,
        &permit,
        &drive,
        &AtomicBool::new(false),
        super::clock().unwrap(),
    );
    assert!(
        matches!(&replay, Err(ControllerError::DispatchHeld(state)) if state == "unknown"),
        "a second native drive must stop at the retained unknown run: {replay:?}"
    );
    assert_eq!(
        publication_operations(&broker, session.id, &intent.spec.commit).len(),
        1
    );
    assert_eq!(
        f.store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_controller_runs WHERE wave=?1",
                [wave_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
        1,
        "replay cannot create another native controller run"
    );
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/accepted"]),
        intent.spec.commit,
        "receiver ref remains the exact candidate after replay refusal"
    );

    let receipt_archive = host.join("receiver-post-receive.receipt");
    std::fs::write(&receipt_archive, &receiver_receipt_bytes).unwrap();
    std::fs::File::open(&receipt_archive)
        .unwrap()
        .sync_all()
        .unwrap();
    assert_eq!(
        std::fs::read(&receipt_archive).unwrap(),
        receiver_receipt_bytes,
        "the exact receiver-owned update record is archived outside restored project data"
    );
    let receiver_receipt_sha256 = super::digest(&receiver_receipt_bytes);

    let unknown_database_archive = host.join("unknown-publication-before-freeze.db");
    f.store
        .connection()
        .execute(
            "VACUUM INTO ?1",
            [unknown_database_archive.to_str().unwrap()],
        )
        .unwrap();
    let unknown_database_sha256 = super::digest(&std::fs::read(&unknown_database_archive).unwrap());

    let prior_epoch = owner.authority_epoch().clone();
    let old_epoch = prior_epoch.as_str().to_owned();
    drop(owner);
    let mut old_worker = AuthorityWorkerProcess::spawn(&source_host);
    let worker_epoch = old_worker
        .wait_until_ready(&old_epoch)
        .expect("stale worker must acquire its old epoch before the announced restore");
    assert_eq!(worker_epoch, old_epoch);
    let old_worker_pid = old_worker.pid();
    let frozen = crate::local_authority::freeze(
        &mut f.store,
        &prior_epoch,
        "FX13 announced restore after an unknown disposable Git publication",
    )
    .unwrap();
    assert_ne!(frozen.epoch.as_str(), old_epoch);
    assert_eq!(
        crate::local_authority::status(&f.store).unwrap(),
        frozen,
        "announced restore rotates the external authority witness before database replacement"
    );
    assert!(
        process_is_alive(live_hook_pids.hook) && process_is_alive(live_hook_pids.delay),
        "the receiver process stays alive through authority rotation"
    );
    assert!(
        old_worker
            .is_alive()
            .expect("inspect the stale controller worker process"),
        "the stale controller process stays alive through authority rotation"
    );

    let frozen_database_archive = host.join("frozen-unknown-publication.db");
    f.store
        .connection()
        .execute(
            "VACUUM INTO ?1",
            [frozen_database_archive.to_str().unwrap()],
        )
        .unwrap();
    let frozen_database_sha256 = super::digest(&std::fs::read(&frozen_database_archive).unwrap());
    let archived_frozen_state = rusqlite::Connection::open(&frozen_database_archive).unwrap();
    let archived_wave_phase: String = archived_frozen_state
        .query_row(
            "SELECT phase FROM local_waves WHERE wave=?1",
            [wave_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let archived_run_state: String = archived_frozen_state
        .query_row(
            "SELECT state FROM local_controller_runs WHERE wave=?1",
            [wave_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let archived_budget: (i64, i64, i64, i64) = archived_frozen_state
        .query_row(
            "SELECT count(*),sum(CASE WHEN phase='consumed' THEN 1 ELSE 0 END),coalesce(sum(attempts),0),(SELECT count(*) FROM local_budget_receipts WHERE reservation IN (SELECT reservation FROM local_budget_reservations WHERE wave=?1)) FROM local_budget_reservations WHERE wave=?1",
            [wave_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    let archived_completions: i64 = archived_frozen_state
        .query_row(
            "SELECT count(*) FROM local_controller_completions WHERE wave=?1",
            [wave_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(archived_wave_phase, "outcome_unknown");
    assert_eq!(archived_run_state, "unknown");
    assert_eq!(archived_budget, (3, 3, 3, 3));
    assert_eq!(archived_completions, 0);

    let project_database = f.store.project_dir().join("state.db");
    drop(f.store);
    std::fs::copy(&pre_effect_database, &project_database).unwrap();
    assert_eq!(
        super::digest(&std::fs::read(&project_database).unwrap()),
        pre_effect_database_sha256,
        "restored project database must byte-match the exact archived pre-publication snapshot"
    );
    f.store = crate::collaboration_state::CollaborationStore::open(
        &CollaborationRoot::under_host_state(&source_host),
        &ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap();
    assert_eq!(
        crate::local_authority::status(&f.store).unwrap(),
        frozen,
        "the external frozen witness survives restoration of the older database"
    );
    assert!(matches!(
        crate::local_authority::resume(&mut f.store, &frozen.epoch),
        Err(crate::local_authority::AuthorityError::Stale)
    ));
    assert_eq!(
        crate::local_authority::status(&f.store).unwrap(),
        frozen,
        "the archived effect cannot authorize resuming a database that lost its ledger history"
    );
    assert!(
        old_worker
            .is_alive()
            .expect("inspect the stale controller worker after database restoration"),
        "the stale controller process remains alive after restoring the older database"
    );

    let restored_counts_before = restore_guard_counts(&f.store);
    let worker_exit_code = old_worker
        .release_and_wait()
        .expect("stale worker must be reaped after its post-restore mutation is refused");
    assert_eq!(worker_exit_code, 0);
    assert_eq!(
        std::fs::read_to_string(source_host.join("fx13-worker-refused"))
            .unwrap()
            .trim(),
        "frozen authority",
        "the stale worker records exact frozen-authority refusal"
    );
    assert_eq!(
        restore_guard_counts(&f.store),
        restored_counts_before,
        "the old controller worker cannot create a completion, run, budget attempt, or transport"
    );
    assert_eq!(
        publication_operations(&broker, session.id, &intent.spec.commit).len(),
        1,
        "the restored old worker cannot cause a second external publication"
    );
    assert_eq!(
        git(&remote, &["rev-parse", "refs/heads/accepted"]),
        intent.spec.commit,
        "the exact published ref remains unchanged after restore refusal"
    );
    assert!(
        process_is_alive(live_hook_pids.hook) && process_is_alive(live_hook_pids.delay),
        "the receiver process remains alive until the stale worker is refused"
    );
    assert!(
        !hook_finished.exists(),
        "the receiver hook is held until after restore and refusal checks"
    );

    hook_process
        .release()
        .expect("release the exact owned receiver hook after restore checks");
    let hook_pids = hook_process
        .wait_for_exit()
        .expect("receiver hook completion and both owned process exits are proven");
    assert_eq!(hook_pids, live_hook_pids);

    let evidence = json!({
        "profile": "aethyme.local-controller/actual-publication-outcome-unknown-v1",
        "service": service,
        "operation": operation,
        "native_job": {
            "id": native_job.id.to_string(),
            "spec": native_job.spec,
            "state": native_job.state,
            "observation": native_job.observation,
        },
        "source_transport": {
            "intent": intent_id,
            "dispatched": dispatched,
            "observation": observation,
            "receipt_present": receipt.is_some(),
            "candidate": intent.spec.commit,
        },
        "receiver_update": receiver_record.trim(),
        "receiver_receipt_archive": {
            "path": receipt_archive,
            "sha256": receiver_receipt_sha256,
            "bytes": receiver_receipt_bytes.len(),
        },
        "receiver_process_cleanup": {
            "marker": hook_marker,
            "hook_pid": hook_pids.hook,
            "delay_pid": hook_pids.delay,
            "alive_through_restore": true,
            "start_record": hook_started,
            "finish_record": hook_finished,
            "exited": true,
        },
        "stale_worker_process": {
            "pid": old_worker_pid,
            "old_epoch": worker_epoch,
            "alive_through_authority_rotation": true,
            "alive_through_database_restore": true,
            "post_restore_mutation_refused": true,
            "exit_code": worker_exit_code,
        },
        "remote_ref": remote_commit,
        "restore_fence": {
            "old_epoch": old_epoch,
            "frozen_epoch": frozen.epoch.as_str(),
            "pre_effect_database_sha256": pre_effect_database_sha256,
            "unknown_database_archive_sha256": unknown_database_sha256,
            "frozen_database_archive_sha256": frozen_database_sha256,
            "restored_database_resume_refused": true,
            "old_worker_mutation_refused": true,
            "restored_counts_unchanged": true,
            "remaining_frozen": true,
        },
        "run_state": run_state,
        "native_roots": native_roots,
        "target_roots": target_roots,
        "object_roots": object_roots,
        "budget": {
            "reservations": budget_rows,
            "consumed": consumed_rows,
            "attempts": attempts,
            "receipts": budget_receipts,
            "pending": pending_reservations,
        },
        "private_acceptance": {
            "completions": completion_rows,
            "qualifications": qualification_rows,
            "receipts": acceptance_receipts,
        },
        "replay_refused": true,
        "source_host": source_host,
        "repository": repo,
        "receiver": remote,
        "evidence_root": host,
        "runtime": executor.runtime(),
    });
    std::fs::write(
        host.join("report.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    eprintln!(
        "Controller actual unknown-publication report {}",
        host.join("report.json").display()
    );
}
