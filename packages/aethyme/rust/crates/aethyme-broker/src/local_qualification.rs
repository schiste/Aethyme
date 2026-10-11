//! Actual finite Local resource-pressure qualification. Ignored by ordinary
//! suites: run explicitly with the pinned cached Linux/Docker profile.
use super::*;
use crate::collaboration_archive as archive;
use crate::collaboration_gc as gc;
use crate::execution_snapshot as snapshot;
use crate::execution_supervisor::{DockerExecutor, Outcome};
use std::sync::atomic::{AtomicBool, Ordering};
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
    use std::io::Read;
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
    let mut client = NativeClient {
        child,
        reaped: false,
    };
    let deadline = Instant::now() + Duration::from_secs(5);
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
                        && output.stdout.is_empty(),
                    "exact owned resource remains after native confirmed cleanup"
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
#[test]
#[ignore = "actual bounded tmpfs ENOSPC and cgroup OOM with concurrent native build and retained-source GC; requires explicit cached Linux image"]
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
    policy.memory_mib = 4096;
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
                    "dd if=/dev/zero of=/tmp/pressure bs=1048576 count=20 2>/outputs/pressure-proof; code=$?; test \"$code\" -ne 0 || exit 99; while ! test -f /outputs/qa-release; do sleep 0.05; done; exit 28",
                    Outcome::CodeFailed,
                ),
                (
                    "bounded-cgroup-oom",
                    "awk 'BEGIN {s=\"x\";for(i=0;i<28;i++)s=s s;print length(s)}'",
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
                let result = if label == "bounded-tmpfs-enospc" {
                    std::thread::scope(|pressure_scope| {
                        let job = pressure_scope.spawn(|| {
                            executor.build(
                                &mut pressure.store,
                                &pressure_owner,
                                &execution,
                                &attempt,
                                &cancel,
                            )
                        });
                        // The trusted recipe waits after the actual dd failure. Observe
                        // its kernel diagnostic via exact native exec before releasing it.
                        let observed =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                let object =
                                    wait_builder(&executor, &host, execution.id().as_str());
                                let deadline = Instant::now() + Duration::from_secs(15);
                                loop {
                                    let proof = control(
                                        &executor,
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
                                        eprintln!(
                                            "Local QA kernel ENOSPC diagnostic SHA-256 {}",
                                            ObjectDigest::of(&proof.stdout).hex()
                                        );
                                        break;
                                    }
                                    assert!(
                                        Instant::now() < deadline,
                                        "actual kernel ENOSPC diagnostic required"
                                    );
                                    std::thread::sleep(Duration::from_millis(25));
                                }
                                release(&executor, &object);
                            }));
                        if observed.is_err() {
                            cancel.store(true, Ordering::SeqCst);
                        }
                        let outcome = job.join().unwrap();
                        if let Err(error) = observed {
                            std::panic::resume_unwind(error);
                        }
                        outcome.unwrap()
                    })
                } else {
                    executor
                        .build(
                            &mut pressure.store,
                            &pressure_owner,
                            &execution,
                            &attempt,
                            &cancel,
                        )
                        .unwrap()
                };
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
                    serde_json::json!({"trial":label,"runtime":executor.runtime().id(),"daemon":executor.runtime().daemon_id,"image":image,"execution":execution.id().as_str(),"outcome":format!("{:?}",result.outcome()),"exit":result.exit_code(),"artifact":false})
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
    gc::apply(&mut collector, &plan.digest, &options).unwrap();
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
