//! Actual finite Local resource-pressure qualification. Ignored by ordinary
//! suites: run explicitly with the pinned cached Linux/Docker profile.
use super::*;
use crate::collaboration_archive as archive;
use crate::collaboration_gc as gc;
use crate::execution_snapshot as snapshot;
use crate::execution_supervisor::{BuildObservationWitnessKind, DockerExecutor, Outcome};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
fn age_objects(store: &CollaborationStore) {
    for prefix in std::fs::read_dir(archive::objects_dir(store)).unwrap() {
        for file in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            std::fs::File::open(file.unwrap().path())
                .unwrap()
                .set_modified(std::time::UNIX_EPOCH + Duration::from_secs(1))
                .unwrap();
        }
    }
}
// Qualification operator controls only exact journal-owned identities. Each
// native client has a finite deadline and bounded files; no daemon-wide sweep.
struct NativeClient {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for NativeClient {
    fn drop(&mut self) {
        if !self.reaped {
            crate::warn_unrecorded("stop exact qualification client", self.child.kill());
            crate::warn_unrecorded("reap exact qualification client", self.child.wait());
        }
    }
}
fn control(executor: &DockerExecutor, args: &[&str]) -> std::process::Output {
    use std::process::{Command, Stdio};
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let child = Command::new(std::env::var("AETHYME_DOCKER_CLI").unwrap())
        .args(["--host", executor.endpoint()])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.reopen().unwrap()))
        .stderr(Stdio::from(stderr.reopen().unwrap()))
        .spawn()
        .unwrap();
    observe_control(
        NativeClient {
            child,
            reaped: false,
        },
        stdout,
        stderr,
        Instant::now() + Duration::from_secs(5),
    )
}
fn observe_control(
    mut client: NativeClient,
    stdout: tempfile::NamedTempFile,
    stderr: tempfile::NamedTempFile,
    deadline: Instant,
) -> std::process::Output {
    use std::io::Read;

    let status = loop {
        if Instant::now() >= deadline
            || stdout.as_file().metadata().unwrap().len() > MIB
            || stderr.as_file().metadata().unwrap().len() > MIB
        {
            panic!("bounded native qualification control did not complete");
        }
        if let Some(status) = client.child.try_wait().unwrap() {
            break status;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    client.reaped = true;
    assert!(stdout.as_file().metadata().unwrap().len() <= MIB);
    assert!(stderr.as_file().metadata().unwrap().len() <= MIB);
    let bounded = |file: &tempfile::NamedTempFile| {
        let mut bytes = Vec::new();
        file.reopen()
            .unwrap()
            .take(MIB + 1)
            .read_to_end(&mut bytes)
            .unwrap();
        assert!(
            bytes.len() as u64 <= MIB,
            "native control output exceeded its bound"
        );
        bytes
    };
    std::process::Output {
        status,
        stdout: bounded(&stdout),
        stderr: bounded(&stderr),
    }
}
fn completed_control() -> (
    NativeClient,
    tempfile::NamedTempFile,
    tempfile::NamedTempFile,
) {
    use std::process::{Command, Stdio};
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let child = Command::new("/usr/bin/true")
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.reopen().unwrap()))
        .stderr(Stdio::from(stderr.reopen().unwrap()))
        .spawn()
        .unwrap();
    let mut client = NativeClient {
        child,
        reaped: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = client.child.try_wait().unwrap() {
            client.reaped = true;
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owned native true helper did not complete"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    (client, stdout, stderr)
}
#[test]
fn completed_native_control_accepts_finite_native_success() {
    let (client, stdout, stderr) = completed_control();
    let output = observe_control(
        client,
        stdout,
        stderr,
        Instant::now() + Duration::from_secs(5),
    );
    assert!(output.status.success());
    assert!(output.stdout.is_empty() && output.stderr.is_empty());
}
#[test]
fn completed_native_control_cannot_bypass_deadline() {
    let (client, stdout, stderr) = completed_control();
    let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        observe_control(client, stdout, stderr, Instant::now())
    }));
    assert!(
        rejected.is_err(),
        "cached native success must not bypass the observation deadline"
    );
}
#[test]
fn completed_native_control_cannot_bypass_output_budget() {
    for oversized_stderr in [false, true] {
        let (client, stdout, stderr) = completed_control();
        let oversized = if oversized_stderr { &stderr } else { &stdout };
        oversized.as_file().set_len(MIB + 1).unwrap();
        let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            observe_control(
                client,
                stdout,
                stderr,
                Instant::now() + Duration::from_secs(5),
            )
        }));
        assert!(
            rejected.is_err(),
            "cached native success must not bypass either output-file budget"
        );
    }
}
fn journals(host: &Path) -> Vec<Journal> {
    let root = host.join("local-execution-journal");
    if !root.exists() {
        return Vec::new();
    }
    std::fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .map(|p| serde_json::from_slice(&std::fs::read(p).unwrap()).unwrap())
        .collect()
}
fn current_builder(executor: &DockerExecutor, host: &Path, execution: &str) -> Option<Object> {
    for journal in journals(host) {
        if journal.execution != execution || journal.cleaned || journal.settled {
            continue;
        }
        assert_eq!(journal.runtime, executor.runtime().id());
        assert_eq!(journal.daemon, executor.runtime().daemon_id);
        assert_eq!(journal.endpoint, executor.endpoint());
        for object in journal.objects {
            if object.kind != ObjectKind::Container
                || !object.name.ends_with("-build")
                || object.removed
                || object.id.is_none()
            {
                continue;
            }
            let reply = control(
                executor,
                &["container", "inspect", object.id.as_deref().unwrap()],
            );
            if !reply.status.success() {
                continue;
            }
            let state: serde_json::Value = serde_json::from_slice(&reply.stdout).unwrap();
            assert_eq!(state[0]["Name"], format!("/{}", object.name));
            assert_eq!(
                state[0]["Config"]["Labels"]["aethyme.local.generation"],
                object.generation
            );
            if state[0]["State"]["Running"] == true {
                return Some(object);
            }
        }
    }
    None
}
fn wait_builder(executor: &DockerExecutor, host: &Path, execution: &str) -> Object {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(object) = current_builder(executor, host, execution) {
            return object;
        }
        assert!(
            Instant::now() < deadline,
            "exact native builder never became running"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn release(executor: &DockerExecutor, object: &Object) {
    let output = control(
        executor,
        &[
            "container",
            "exec",
            "--user",
            "65534:65534",
            object.id.as_deref().unwrap(),
            "/bin/sh",
            "-c",
            "printf release > /outputs/qa-release",
        ],
    );
    assert!(output.status.success(), "release failed: {:?}", output);
}
struct PreserveRecovery<'a> {
    executor: &'a DockerExecutor,
    host: PathBuf,
    source_roots: std::cell::RefCell<Vec<PathBuf>>,
}
impl Drop for PreserveRecovery<'_> {
    fn drop(&mut self) {
        // The directory was kept before launch. Panic or ambiguous cleanup can
        // never erase the native journal or outstanding coordinator grants.
        let recovered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            for journal in journals(&self.host) {
                crate::warn_unrecorded(
                    "recover exact qualification run",
                    self.executor.recover_resources(&journal.run),
                );
            }
        }));
        if recovered.is_err() || std::thread::panicking() {
            eprintln!(
                "Local QA recovery preserved at {}; source roots {:?}",
                self.host.display(),
                self.source_roots.borrow()
            );
        }
    }
}
fn assert_cleanup(executor: &DockerExecutor, host: &Path) {
    assert!(
        !journals(host).is_empty(),
        "cleanup evidence cannot be empty"
    );
    for entry in std::fs::read_dir(host.join("local-execution-journal")).unwrap() {
        let file = entry.unwrap().path();
        if file.extension().is_some_and(|e| e == "json") {
            let journal: Journal = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
            assert!(journal.cleaned && journal.settled);
            assert!(!journal.objects.is_empty());
            assert!(journal.objects.iter().all(|o| o.removed && o.id.is_some()));
            for object in &journal.objects {
                let kind = match object.kind {
                    ObjectKind::Container => "container",
                    ObjectKind::Volume => "volume",
                };
                let output = control(executor, &[kind, "inspect", object.id.as_deref().unwrap()]);
                assert!(
                    !output.status.success()
                        && String::from_utf8_lossy(&output.stderr)
                            .to_ascii_lowercase()
                            .contains("no such")
                        && String::from_utf8_lossy(&output.stderr)
                            .contains(object.id.as_deref().unwrap())
                        && serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout)
                            .is_ok_and(|objects| objects.is_empty()),
                    "exact owned resource remains after native confirmed cleanup: {object:?}; {output:?}"
                );
            }
            executor.recover_resources(&journal.run).unwrap();
        }
    }
    assert!(
        HostResourceCoordinator::open(&host.join("host-resources.db"))
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty()
    );
}

fn wait_output_marker(executor: &DockerExecutor, object: &Object, path: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = control(
            executor,
            &[
                "container",
                "exec",
                "--user",
                "65534:65534",
                object.id.as_deref().unwrap(),
                "/bin/cat",
                path,
            ],
        );
        if output.status.success() && output.stdout == b"ready" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "exact builder did not reach {path}"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn signal_pressure(executor: &DockerExecutor, object: &Object) {
    let output = control(
        executor,
        &[
            "container",
            "exec",
            "--user",
            "65534:65534",
            object.id.as_deref().unwrap(),
            "/bin/sh",
            "-c",
            "printf ready > /outputs/qa-pressure",
        ],
    );
    assert!(
        output.status.success(),
        "exact builder pressure signal failed: {output:?}"
    );
}

struct ResumeGc(Option<mpsc::Sender<()>>);

fn assert_archive_exclusive_observed(lock_path: &std::path::Path, label: &str, stage: &str) {
    let lock = std::fs::File::open(lock_path)
        .unwrap_or_else(|error| panic!("{label}: open canonical archive lock for probe: {error}"));
    assert!(
        matches!(
            lock.try_lock_shared(),
            Err(std::fs::TryLockError::WouldBlock)
        ),
        "{label}: canonical archive shared try-lock must report WouldBlock {stage}; GC must still hold archive EX"
    );
}

impl ResumeGc {
    fn release(&mut self) {
        if let Some(sender) = self.0.take() {
            // A closed receiver means the GC worker exited; no pause remains.
            sender.send(()).unwrap_or(());
        }
    }
}

impl Drop for ResumeGc {
    fn drop(&mut self) {
        self.release();
    }
}
#[test]
#[ignore = "actual bounded tmpfs ENOSPC and owned-cgroup OOM while same-store GC holds EX; requires explicit cached Linux image"]
fn actual_pressure_preserves_retained_source_and_bounded_native_cleanup() {
    let host = tempfile::tempdir().unwrap().keep();
    eprintln!("Local QA durable host journal {}", host.display());
    let cli = std::env::var("AETHYME_DOCKER_CLI").expect("explicit native Docker CLI");
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("explicit cached image digest");
    assert_eq!(
        image,
        "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251"
    );
    let native = DockerExecutor::probe(cli.into()).unwrap();
    let mut policy = tests::policy(&host);
    policy.runtime = native.runtime().id();
    policy.daemon = native.runtime().daemon_id.clone();
    policy.meter_image = image.clone();
    policy.memory_mib = 3072;
    policy.disk_mib = 4096;
    policy.job_slots = 4;
    let executor = native.with_resources(policy.clone()).unwrap();
    let recovery = PreserveRecovery {
        executor: &executor,
        host: host.clone(),
        source_roots: Default::default(),
    };
    let (mut stable, stable_owner, stable_execution, stable_wave) =
        tests::subject(executor.runtime().id(), image.clone());
    let mut stable_spec = stable_execution.spec().clone();
    stable_spec.build.arguments[1] =
        "while ! test -f /outputs/qa-release; do sleep 0.05; done; printf actual > /outputs/artifact"
            .into();
    stable_spec.profile.build_limits.timeout_ms = 300000;
    let stable_execution = snapshot::freeze(
        &mut stable.store,
        &stable_owner,
        stable_execution.target_revision(),
        stable_spec,
    )
    .unwrap();
    let stable_attempt = tests::attempt(
        &mut stable,
        &stable_owner,
        &stable_execution,
        &stable_wave,
        build_envelope(&stable_execution).unwrap(),
        b"QA concurrent independent build",
    );
    let orphan =
        archive::put_object(&stable.store, b"QA orphan must be actually collected").unwrap();
    age_objects(&stable.store);
    let retained_host = std::mem::replace(&mut stable._host, tempfile::tempdir().unwrap()).keep();
    recovery
        .source_roots
        .borrow_mut()
        .push(retained_host.clone());
    eprintln!(
        "Local QA retained stable source root {}",
        retained_host.display()
    );
    let root = crate::collaboration_state::CollaborationRoot::under_host_state(&retained_host);

    let project = stable.store.project().clone();
    let mut collector = CollaborationStore::open(&root, &project, &[]).unwrap();
    let child_executor = DockerExecutor::probe(std::env::var("AETHYME_DOCKER_CLI").unwrap().into())
        .unwrap()
        .with_resources(policy.clone())
        .unwrap();
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let build = scope.spawn(|| {
            child_executor.build(
                &mut stable.store,
                &stable_owner,
                &stable_execution,
                &stable_attempt,
                &cancel,
            )
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let stable_id = stable_execution.id().as_str();
            let stable_object = wait_builder(&executor, &host, stable_id);
            let options = gc::GcOptions::default();
            let plan = gc::plan(&mut collector, &options).unwrap();
            match gc::apply(&mut collector, &plan.digest, &options) {
                Ok(report) => assert!(!report.reclaimed.is_empty()),
                Err(gc::GcError::ArchiveInUse) => {}
                other => panic!(
                    "concurrent GC must reclaim only eligible source or explicitly refuse the live reader: {other:?}"
                ),
            }
            let (mut pressure, pressure_owner, base_execution, wave) =
                tests::subject(executor.runtime().id(), image.clone());
            let pressure_host =
                std::mem::replace(&mut pressure._host, tempfile::tempdir().unwrap()).keep();
            recovery
                .source_roots
                .borrow_mut()
                .push(pressure_host.clone());
            eprintln!(
                "Local QA retained pressure source root {}",
                pressure_host.display()
            );
            for (label, command, expected) in [
                (
                    "bounded-tmpfs-enospc",
                    "printf ready > /outputs/qa-start-pressure; while ! test -f /outputs/qa-pressure; do sleep 0.05; done; dd if=/dev/zero of=/tmp/pressure bs=1048576 count=20 2>/outputs/pressure-proof; code=$?; test \"$code\" -ne 0 || exit 99; while ! test -f /outputs/qa-release; do sleep 0.05; done; exit 28",
                    Outcome::CodeFailed,
                ),
                (
                    "bounded-cgroup-oom",
                    "printf ready > /outputs/qa-start-pressure; while ! test -f /outputs/qa-pressure; do sleep 0.05; done; awk 'BEGIN {s=\"x\";for(i=0;i<28;i++)s=s s;print length(s)}'",
                    Outcome::InfrastructureDeferred,
                ),
            ] {
                let mut spec = base_execution.spec().clone();
                spec.build.arguments[1] = command.into();
                spec.profile.build_limits.timeout_ms = 60000;
                let execution = snapshot::freeze(
                    &mut pressure.store,
                    &pressure_owner,
                    base_execution.target_revision(),
                    spec,
                )
                .unwrap();
                let attempt = tests::attempt(
                    &mut pressure,
                    &pressure_owner,
                    &execution,
                    &wave,
                    build_envelope(&execution).unwrap(),
                    label.as_bytes(),
                );
                assert_eq!(
                    current_builder(&executor, &host, stable_id).unwrap().id,
                    stable_object.id,
                    "stable build must still be actually running before each pressure trial"
                );
                let pressure_orphan = archive::put_object(
                    &pressure.store,
                    format!("T45 {label} independently reclaimable source").as_bytes(),
                )
                .unwrap();
                age_objects(&pressure.store);
                let pressure_root =
                    crate::collaboration_state::CollaborationRoot::under_host_state(&pressure_host);
                let pressure_project = pressure.store.project().clone();
                let mut pressure_collector =
                    CollaborationStore::open(&pressure_root, &pressure_project, &[]).unwrap();
                let archive_lock_path = gc::lock_path(&pressure_collector);
                let gc_options = gc::GcOptions::default();
                let gc_plan = gc::plan(&mut pressure_collector, &gc_options).unwrap();
                let orphan_path = archive::object_path(&pressure_collector, &pressure_orphan);
                let move_ordinal = gc_plan
                    .reclaimable
                    .iter()
                    .position(|entry| {
                        pressure_collector.project_dir().join(&entry.relpath) == orphan_path
                    })
                    .map(|index| index + 1)
                    .expect("the same-store plan must contain the named pressure orphan");
                let plan_digest = gc_plan.digest.clone();
                let executor_ref = &executor;
                let host_ref = &host;
                let execution_ref = &execution;
                let attempt_ref = &attempt;
                let pressure_owner_ref = &pressure_owner;
                let (witness_tx, witness_rx) = mpsc::sync_channel(1);
                let (result, gc_report, pressure_collector) = std::thread::scope(
                    |pressure_scope| {
                        let job = pressure_scope.spawn(|| {
                            executor_ref.test_build_with_pre_finish_observer(
                                &mut pressure.store,
                                pressure_owner_ref,
                                execution_ref,
                                attempt_ref,
                                &cancel,
                                witness_tx.clone(),
                            )
                        });
                        let trial = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let object =
                                wait_builder(executor_ref, host_ref, execution_ref.id().as_str());
                            wait_output_marker(executor_ref, &object, "/outputs/qa-start-pressure");

                            let (entered_tx, entered_rx) = mpsc::channel();
                            let (resume_tx, resume_rx) = mpsc::channel();
                            let resume_rx = Arc::new(Mutex::new(resume_rx));
                            let mut resume_gc = ResumeGc(Some(resume_tx));
                            let moved_path = orphan_path.clone();
                            let gc_plan_for_worker = plan_digest.clone();
                            let gc_callback = Box::new(move || {
                                let moved = !moved_path.exists();
                                entered_tx.send(moved).unwrap();
                                resume_rx.lock().unwrap().recv().unwrap();
                            });
                            let hooks = gc::GcHooks {
                                after_moves: Some((move_ordinal, gc_callback)),
                                ..gc::GcHooks::default()
                            };
                            let gc_worker = pressure_scope.spawn(move || {
                                let result = gc::apply_with(
                                    &mut pressure_collector,
                                    &gc_plan_for_worker,
                                    &gc_options,
                                    &hooks,
                                );
                                (result, pressure_collector)
                            });
                            assert!(
                                entered_rx.recv_timeout(Duration::from_secs(60)).unwrap(),
                                "GC hook must acknowledge after moving the exact named orphan"
                            );
                            assert_archive_exclusive_observed(
                                &archive_lock_path,
                                label,
                                "after the named move and before the pressure signal",
                            );
                            signal_pressure(executor_ref, &object);

                            if label == "bounded-tmpfs-enospc" {
                                let deadline = Instant::now() + Duration::from_secs(15);
                                loop {
                                    let proof = control(
                                        executor_ref,
                                        &[
                                            "container",
                                            "exec",
                                            "--user",
                                            "65534:65534",
                                            object.id.as_deref().unwrap(),
                                            "/bin/cat",
                                            "/outputs/pressure-proof",
                                        ],
                                    );
                                    if proof.status.success()
                                        && String::from_utf8_lossy(&proof.stdout)
                                            .contains("No space left on device")
                                    {
                                        assert_archive_exclusive_observed(
                                            &archive_lock_path,
                                            label,
                                            "after observing the actual kernel ENOSPC diagnostic",
                                        );
                                        eprintln!(
                                            "Local QA kernel ENOSPC diagnostic SHA-256 {} while GC holds archive EX",
                                            ObjectDigest::of(&proof.stdout).hex()
                                        );
                                        break;
                                    }
                                    assert!(
                                        Instant::now() < deadline,
                                        "actual kernel ENOSPC diagnostic required while GC is paused"
                                    );
                                    std::thread::sleep(Duration::from_millis(25));
                                }
                                release(executor_ref, &object);
                            }

                            let witness = witness_rx
                                .recv_timeout(Duration::from_secs(75))
                                .unwrap_or_else(|error| {
                                    panic!("{label}: bounded pre-finish observation unavailable: {error}")
                                });
                            assert_eq!(
                                witness.kind,
                                BuildObservationWitnessKind::Observation,
                                "{label}: unexpected build error before observation: {:?}",
                                witness.detail
                            );
                            assert_eq!(witness.run_id, object.generation);
                            assert_eq!(
                                witness.outcome,
                                Some(expected),
                                "{label}: {:?}",
                                witness.detail
                            );
                            assert!(
                                !witness.artifact_present,
                                "{label}: pressure cannot mint an artifact/pass"
                            );
                            if label == "bounded-tmpfs-enospc" {
                                assert_eq!(witness.exit_code, Some(28));
                                assert_eq!(witness.detail, None);
                            } else {
                                assert_eq!(witness.exit_code, None);
                                assert_eq!(
                                    witness.detail.as_deref(),
                                    Some("infrastructure deferred: workload OOM")
                                );
                            }
                            assert_archive_exclusive_observed(
                                &archive_lock_path,
                                label,
                                "after the exact cleaned build observation and before join",
                            );
                            resume_gc.release();
                            let observation = job.join().unwrap().unwrap();
                            assert_eq!(
                                observation.outcome(),
                                expected,
                                "{label}: {:?}",
                                observation.detail()
                            );
                            assert!(
                                observation.artifact().is_none(),
                                "pressure cannot mint an artifact/pass"
                            );
                            if label == "bounded-tmpfs-enospc" {
                                assert_eq!(observation.exit_code(), Some(28));
                            } else {
                                assert!(
                                    observation
                                        .detail()
                                        .is_some_and(|detail| detail.contains("OOM"))
                                );
                            }
                            let (report, collector) = gc_worker.join().unwrap();
                            (observation, report.unwrap(), collector)
                        }));
                        if trial.is_err() {
                            cancel.store(true, Ordering::SeqCst);
                        }
                        match trial {
                            Ok(result) => result,
                            Err(panic) => std::panic::resume_unwind(panic),
                        }
                    },
                );
                assert!(
                    gc_report.reclaimed.iter().any(|entry| pressure_collector
                        .project_dir()
                        .join(&entry.relpath)
                        == orphan_path),
                    "same-store GC must complete the exact named move after pressure"
                );
                assert!(!orphan_path.exists());
                assert!(archive::read_object(&pressure_collector, &pressure_orphan).is_err());
                let restored = tempfile::tempdir().unwrap();
                let restored_source = restored.path().join("retained-pressure-source");
                assert_eq!(
                    archive::reconstruct(
                        &pressure_collector,
                        &pressure.a.result.snapshot_id,
                        &restored_source
                    )
                    .unwrap()
                    .id(),
                    pressure.a.result.snapshot_id,
                    "same-store GC must preserve the exact retained build source"
                );
                assert_eq!(
                    std::fs::read(restored_source.join("one.txt")).unwrap(),
                    b"A\n"
                );
                assert_eq!(
                    current_builder(&executor, &host, stable_id).unwrap().id,
                    stable_object.id,
                    "stable build must remain running through each pressure trial"
                );
                assert_eq!(result.outcome(), expected, "{label}: {:?}", result.detail());
                assert!(
                    result.artifact().is_none(),
                    "pressure must not mint an artifact/pass"
                );
                if label == "bounded-tmpfs-enospc" {
                    assert_eq!(result.exit_code(), Some(28));
                } else {
                    assert!(result.detail().is_some_and(|s| s.contains("OOM")));
                }
                assert_eq!(
                    budget::load(&pressure.store, attempt.reservation())
                        .unwrap()
                        .state,
                    budget::State::PartlyConsumed
                );
                eprintln!(
                    "Local QA pressure transcript {}",
                    serde_json::json!({"trial":label,"runtime":executor.runtime().id(),"daemon":executor.runtime().daemon_id,"image":image,"execution":execution.id().as_str(),"outcome":format!("{:?}",result.outcome()),"exit":result.exit_code(),"artifact":false,"same_store_gc":"held_archive_ex_during_pressure_then_reclaimed_named_orphan","retained_source_reconstructed":true})
                );
            }
            release(&executor, &stable_object);
        }));
        if result.is_err() {
            cancel.store(true, Ordering::SeqCst);
        }
        let independent = build.join().unwrap();
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
        let independent = independent.unwrap();
        assert_eq!(independent.outcome(), Outcome::Built);
        assert!(independent.artifact().is_some());
    });
    let options = gc::GcOptions::default();
    let plan = gc::plan(&mut collector, &options).unwrap();
    assert!(plan.blockers.is_empty(), "post-build GC must be unblocked");
    // The concurrent collector may already have reclaimed the exact orphan.
    // Empty surveys are no-op plans and are deliberately not persisted for apply.
    if !plan.reclaimable.is_empty() || !plan.entities.is_empty() {
        gc::apply(&mut collector, &plan.digest, &options).unwrap();
    }
    assert!(archive::read_object(&collector, &orphan).is_err());
    let restored = tempfile::tempdir().unwrap();
    let source = restored.path().join("source");
    archive::reconstruct(&collector, &stable.a.result.snapshot_id, &source).unwrap();
    assert_eq!(std::fs::read(source.join("one.txt")).unwrap(), b"A\n");
    assert_cleanup(&executor, &host);
    let source_roots = recovery.source_roots.borrow().clone();
    drop(recovery);
    drop(collector);
    drop(stable);
    for root in source_roots {
        std::fs::remove_dir_all(root).unwrap();
    }
    std::fs::remove_dir_all(&host).unwrap();
}

#[cfg(unix)]
#[path = "local_qualification/native_flow.rs"]
mod native_flow;

#[cfg(target_os = "macos")]
#[path = "local_qualification/native_filesystem.rs"]
mod native_filesystem;
