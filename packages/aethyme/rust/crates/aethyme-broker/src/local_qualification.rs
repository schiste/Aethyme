//! Actual finite Local resource-pressure qualification. Ignored by ordinary
//! suites: run explicitly with the pinned cached Linux/Docker profile.
use super::*;
use crate::collaboration_archive as archive;
use crate::collaboration_gc as gc;
use crate::execution_snapshot as snapshot;
use crate::execution_supervisor::{DockerExecutor, Outcome};
use std::sync::atomic::AtomicBool;
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
fn assert_cleanup(executor: &DockerExecutor, host: &Path) {
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
                let output =
                    std::process::Command::new(std::env::var("AETHYME_DOCKER_CLI").unwrap())
                        .args([
                            "--host",
                            executor.endpoint(),
                            kind,
                            "inspect",
                            object.id.as_deref().unwrap(),
                        ])
                        .output()
                        .unwrap();
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
        HostResourceCoordinator::open(host.join("host-resources.db"))
            .unwrap()
            .list(false)
            .unwrap()
            .is_empty()
    );
}
#[test]
#[ignore = "actual bounded tmpfs ENOSPC and cgroup OOM with concurrent native build and retained-source GC; requires explicit cached Linux image"]
fn actual_pressure_preserves_retained_source_and_bounded_native_cleanup() {
    use std::os::unix::fs::PermissionsExt;
    let host = tempfile::tempdir().unwrap();
    let tools = tempfile::tempdir().unwrap();
    let cli = std::env::var("AETHYME_DOCKER_CLI").expect("explicit native Docker CLI");
    let image = std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("explicit cached image digest");
    assert_eq!(
        image,
        "sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251"
    );
    assert!(!cli.contains('\''));
    let marker = tools.path().join("build-starts");
    let transcript = tools.path().join("native-attached-stream");
    let wrapper = tools.path().join("docker-pressure-transcript");
    // Exact native CLI runs all operations. For a builder's attached stream only,
    // retain its output and exit code; this fixture never changes daemon replies,
    // workload argv, mount/resource controls or the supervisor's classification.
    std::fs::write(
        &wrapper,
        format!(
            r#"#!/bin/sh
set -eu
if test "${{3-}} ${{4-}} ${{5-}} ${{6-}}" = 'container start --attach --interactive'; then
  name=$('{}' --host "$2" container inspect --format '{{{{.Name}}}}' "$7")
  case "$name" in *-build)
    printf '%s\n' "$name" >> '{}'
    set +e
    '{}' "$@" > '{}'."$7" 2>&1
    code=$?
    cat '{}'."$7"
    exit "$code"
    ;; esac
fi
exec '{}' "$@"
"#,
            cli,
            marker.display(),
            cli,
            transcript.display(),
            transcript.display(),
            cli
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    let native = DockerExecutor::probe(wrapper.clone()).unwrap();
    let mut policy = tests::policy(host.path());
    policy.runtime = native.runtime().id();
    policy.daemon = native.runtime().daemon_id.clone();
    policy.meter_image = image.clone();
    policy.memory_mib = 4096;
    policy.disk_mib = 4096;
    policy.job_slots = 4;
    let executor = native.with_resources(policy.clone()).unwrap();
    let (mut stable, stable_owner, stable_execution, stable_wave) =
        tests::subject(executor.runtime().id(), image.clone());
    let mut stable_spec = stable_execution.spec().clone();
    stable_spec.build.arguments[1] = "sleep 20; printf actual > /outputs/artifact".into();
    stable_spec.profile.build_limits.timeout_ms = 30000;
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
    let root = crate::collaboration_state::CollaborationRoot::under_host_state(stable._host.path());
    let project = stable.store.project().clone();
    let mut collector = CollaborationStore::open(&root, &project, &[]).unwrap();
    let child_executor = DockerExecutor::probe(wrapper.clone())
        .unwrap()
        .with_resources(policy.clone())
        .unwrap();
    std::thread::scope(|scope| {
        let build = scope.spawn(|| {
            child_executor
                .build(
                    &mut stable.store,
                    &stable_owner,
                    &stable_execution,
                    &stable_attempt,
                    &AtomicBool::new(false),
                )
                .unwrap()
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while !marker.exists() {
            assert!(
                Instant::now() < deadline,
                "independent native build never started"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
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
        for (label, command, expected) in [
            (
                "bounded-tmpfs-enospc",
                "dd if=/dev/zero of=/tmp/pressure bs=1048576 count=20; code=$?; test \"$code\" -ne 0 || exit 99; exit 28",
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
            spec.profile.build_limits.timeout_ms = 10000;
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
            let result = executor
                .build(
                    &mut pressure.store,
                    &pressure_owner,
                    &execution,
                    &attempt,
                    &AtomicBool::new(false),
                )
                .unwrap();
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
        let independent = build.join().unwrap();
        assert_eq!(independent.outcome(), Outcome::Built);
        assert!(independent.artifact().is_some());
    });
    let mut observed_enospc = false;
    for entry in std::fs::read_dir(tools.path()).unwrap() {
        let entry = entry.unwrap();
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("native-attached-stream.")
        {
            let bytes = std::fs::read(entry.path()).unwrap();
            observed_enospc |= String::from_utf8_lossy(&bytes).contains("No space left on device");
            eprintln!(
                "Local QA attached stream SHA-256 {}",
                ObjectDigest::of(&bytes).hex()
            );
        }
    }
    assert!(
        observed_enospc,
        "actual kernel ENOSPC diagnostic required, not just an arbitrary nonzero exit"
    );
    let options = gc::GcOptions::default();
    let plan = gc::plan(&mut collector, &options).unwrap();
    gc::apply(&mut collector, &plan.digest, &options).unwrap();
    assert!(archive::read_object(&collector, &orphan).is_err());
    let restored = tempfile::tempdir().unwrap();
    let source = restored.path().join("source");
    archive::reconstruct(&collector, &stable.a.result.snapshot_id, &source).unwrap();
    assert_eq!(std::fs::read(source.join("one.txt")).unwrap(), b"A\n");
    assert_cleanup(&executor, host.path());
}
