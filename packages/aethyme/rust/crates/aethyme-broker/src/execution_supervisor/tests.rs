use super::*;
use artifact::Purpose;
use std::io::Write;
fn archive(entries: &[(&[u8], tar::EntryType, u32, &[u8])]) -> tempfile::NamedTempFile {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut builder = tar::Builder::new(file.reopen().unwrap());
    for (name, kind, mode, bytes) in entries {
        let mut header = tar::Header::new_ustar();
        header.set_entry_type(*kind);
        header.set_mode(*mode);
        header.set_size(bytes.len() as u64);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name);
        header.set_cksum();
        builder.append(&header, *bytes).unwrap();
    }
    builder.finish().unwrap();
    file
}
#[test]
fn output_bytes_and_modes_have_an_independent_manifest_identity() {
    let file = archive(&[
        (b"./asset.txt", tar::EntryType::Regular, 0o644, b"hello\r\n"),
        (b"./bin", tar::EntryType::Regular, 0o755, b"\0\xff"),
    ]);
    let output = artifact::read(file.path(), 64, Purpose::Output).unwrap();
    assert_eq!(output.files[b"asset.txt".as_slice()], b"hello\r\n");
    // Computed independently from the public snapshot encoding using Ruby
    // Digest::SHA256; no production snapshot code produced this expectation.
    assert_eq!(
        output.manifest.id().as_str(),
        "sha256:67f2281012764bd9bb0967cf06bb83f43aef8760b678e4a1df5b8d96f8f82008"
    );
}
#[test]
fn hostile_output_names_and_kinds_never_extract() {
    for name in [
        b"../outside".as_slice(),
        b"/absolute",
        b".git/config",
        b"bad\\path",
        b"bad\xff",
    ] {
        let file = archive(&[(name, tar::EntryType::Regular, 0o644, b"x")]);
        assert!(artifact::read(file.path(), 64, Purpose::Output).is_err());
    }
    for kind in [
        tar::EntryType::Symlink,
        tar::EntryType::Link,
        tar::EntryType::Fifo,
        tar::EntryType::Char,
        tar::EntryType::Block,
    ] {
        let file = archive(&[(b"entry", kind, 0o644, b"")]);
        assert!(matches!(
            artifact::read(file.path(), 64, Purpose::Output),
            Err(SupervisorError::Tampered(_))
        ));
    }
    let file = archive(&[(b"special", tar::EntryType::Regular, 0o4755, b"x")]);
    assert!(artifact::read(file.path(), 64, Purpose::Output).is_err());
}
#[test]
fn duplicate_folded_paths_and_directory_aliases_are_rejected() {
    for names in [
        ["dup", "dup"],
        ["Lib/a", "lib/b"],
        ["X", "x/file"],
        ["caf\u{e9}/a", "cafe\u{301}/b"],
    ] {
        let file = archive(&[
            (names[0].as_bytes(), tar::EntryType::Regular, 0o644, b"x"),
            (names[1].as_bytes(), tar::EntryType::Regular, 0o644, b"y"),
        ]);
        assert!(
            artifact::read(file.path(), 64, Purpose::Output).is_err(),
            "{names:?}"
        );
    }
}
#[test]
fn byte_limits_empty_artifacts_and_oversized_headers_are_not_passes() {
    let file = archive(&[(b"asset", tar::EntryType::Regular, 0o644, b"1234")]);
    assert!(matches!(
        artifact::read(file.path(), 3, Purpose::Output),
        Err(SupervisorError::Limit)
    ));
    let empty = archive(&[]);
    assert!(matches!(
        artifact::read(empty.path(), 64, Purpose::Output),
        Err(SupervisorError::Inconclusive(_))
    ));
    assert!(artifact::read(empty.path(), 64, Purpose::Input).is_ok());
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let mut header = tar::Header::new_ustar();
    header.set_path("oversized").unwrap();
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(64 * 1024 * 1024 + 1);
    header.set_cksum();
    file.write_all(header.as_bytes()).unwrap();
    file.write_all(&[0; 1024]).unwrap();
    assert!(matches!(
        artifact::read(file.path(), 256 * 1024 * 1024, Purpose::Output),
        Err(SupervisorError::Limit)
    ));
}
#[cfg(unix)]
#[test]
#[ignore = "requires operator-approved Linux Docker daemon and cached pinned Debian image"]
fn real_linux_build_observes_readonly_inputs_stopped_writers_and_failures() {
    use crate::execution_snapshot::{CommandSpec, ExecutionSpec, LinuxProfile, ResourceLimits};
    use crate::local_target::tests::Fixture;
    use std::collections::BTreeMap;
    let cli = std::env::var_os("AETHYME_DOCKER_CLI")
        .map(PathBuf::from)
        .expect("explicit trusted Docker CLI");
    let image =
        std::env::var("AETHYME_DOCKER_TEST_IMAGE").expect("explicit pinned public test image");
    let executor = DockerExecutor::probe(cli).unwrap();
    let mut f = Fixture::new();
    let owner = target::acquire_controller(&mut f.store, "supervisor", "appointed").unwrap();
    let target_spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, target_spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    for phase in [
        target::Phase::Admitting,
        target::Phase::WaveFrozen,
        target::Phase::Composing,
        target::Phase::Verifying,
    ] {
        let state = target::status(&f.store, owner.target()).unwrap();
        target::transition(&mut f.store, &owner, state.version, phase).unwrap();
    }
    let limits = ResourceLimits {
        timeout_ms: 5000,
        memory_bytes: 64 * 1024 * 1024,
        output_bytes: 1024 * 1024,
        pids: 32,
    };
    let command = CommandSpec {
        program: "/bin/sh".into(),
        arguments: vec!["-c".into(), "true".into()],
        environment: BTreeMap::new(),
    };
    let mut spec = ExecutionSpec {
        candidate_source: f.a.result.snapshot_id.to_string(),
        preparation: None,
        dependencies: None,
        configuration: None,
        harness: f.b.result.snapshot_id.to_string(),
        dataset: None,
        assertion_inventory: format!("sha256:{}", "a".repeat(64)),
        materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
        profile: LinuxProfile {
            build_image: image.clone(),
            verifier_image: image,
            architecture: executor.runtime().architecture.clone(),
            runtime_profile: executor.runtime().id(),
            build_limits: limits,
            verifier_limits: limits,
        },
        build: command.clone(),
        verify: command,
    };
    // Explicitly wait for the safety checks before a detached writer is started.
    spec.build.arguments[1]="set -e; test ! -e /var/run/docker.sock; test ! -e /harness; test ! -e /dataset; test -z \"$HOME\"; if echo bad > /source/one.txt; then exit 99; fi; printf sealed > /outputs/asset; (sleep 1; echo late > /outputs/late) >/dev/null 2>&1 & exit 0".into();
    let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec.clone()).unwrap();
    let result = executor
        .build(&mut f.store, &owner, &execution, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(result.outcome(), Outcome::Built);
    let sealed = result.into_artifact().unwrap();
    assert_eq!(sealed.output.files[b"asset".as_slice()], b"sealed");
    assert!(!sealed.output.files.contains_key(b"late".as_slice()));
    spec.build.arguments[1] = "exit 42".into();
    let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec.clone()).unwrap();
    let failed = executor
        .build(&mut f.store, &owner, &execution, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(failed.outcome(), Outcome::CodeFailed);
    assert_eq!(failed.exit_code(), Some(42));
    assert!(failed.artifact().is_none());
    spec.build.arguments[1] = "sleep 10".into();
    spec.profile.build_limits.timeout_ms = 200;
    let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec.clone()).unwrap();
    let timed = executor
        .build(&mut f.store, &owner, &execution, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(timed.outcome(), Outcome::TimedOut);
    assert!(timed.artifact().is_none());
    let stopped = executor
        .build(&mut f.store, &owner, &execution, &AtomicBool::new(true))
        .unwrap();
    assert_eq!(stopped.outcome(), Outcome::Cancelled);
    assert!(stopped.artifact().is_none());
    spec.profile.build_limits.timeout_ms = 5000;
    spec.build.arguments[1] = "ln -s /etc/passwd /outputs/asset".into();
    let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec.clone()).unwrap();
    let forged = executor
        .build(&mut f.store, &owner, &execution, &AtomicBool::new(false))
        .unwrap();
    assert_eq!(forged.outcome(), Outcome::TamperingDetected);
    assert!(forged.artifact().is_none());
    // Block the real output export, latch cancellation, then let it finish.
    // A completed build cannot survive that cancellation as a capability.
    {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        let temp = tempfile::tempdir().unwrap();
        let marker = temp.path().join("exporting");
        let release = temp.path().join("release");
        let wrapper = temp.path().join("docker-export-control");
        let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
        std::fs::write(&wrapper,format!("#!/bin/sh\nset -eu\nif test \"$3\" = cp && test \"$5\" = -; then\n case \"$4\" in *:/outputs/.) printf exporting > {}; while test ! -f {}; do sleep 0.01; done;; esac\nfi\nexec /opt/homebrew/bin/docker \"$@\"\n",quote(&marker),quote(&release))).unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let wrapped = DockerExecutor::probe(wrapper).unwrap();
        spec.profile.runtime_profile = wrapped.runtime().id();
        let cancel = Arc::new(AtomicBool::new(false));
        let trigger = cancel.clone();
        let cancelling = std::thread::spawn(move || {
            let started = Instant::now();
            while !marker.is_file() {
                assert!(started.elapsed() < Duration::from_secs(10));
                std::thread::sleep(Duration::from_millis(5));
            }
            trigger.store(true, Ordering::SeqCst);
            std::fs::write(release, b"cancel latched").unwrap();
        });
        spec.build.arguments[1] = "printf sealed > /outputs/asset".into();
        let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec.clone()).unwrap();
        let observation = wrapped
            .build(&mut f.store, &owner, &execution, &cancel)
            .unwrap();
        cancelling.join().unwrap();
        assert_eq!(observation.outcome(), Outcome::Cancelled);
        assert!(observation.artifact().is_none());
    }
    // Delayed real start acknowledgement means the first status can already
    // be stopped. An exit-zero result observed past the deadline is not Built.
    {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let wrapper = temp.path().join("docker-delay-control");
        std::fs::write(&wrapper,"#!/bin/sh\nset -eu\nif test \"$3 $4\" = 'container start'; then\n name=$(/opt/homebrew/bin/docker --host \"$2\" container inspect --format '{{.Name}}' \"$5\")\n case \"$name\" in *-build) /opt/homebrew/bin/docker \"$@\"; sleep 0.4; exit 0;; esac\nfi\nexec /opt/homebrew/bin/docker \"$@\"\n").unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
        let wrapped = DockerExecutor::probe(wrapper).unwrap();
        spec.profile.runtime_profile = wrapped.runtime().id();
        spec.profile.build_limits.timeout_ms = 200;
        spec.build.arguments[1] = "printf too-late > /outputs/asset".into();
        let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec.clone()).unwrap();
        let observation = wrapped
            .build(&mut f.store, &owner, &execution, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(observation.outcome(), Outcome::TimedOut);
        assert!(observation.artifact().is_none());
    }
}
#[test]
fn item_limits_and_truncation_are_not_successful_artifacts() {
    let names: Vec<String> = (0..8193).map(|i| format!("file{i}")).collect();
    let entries: Vec<_> = names
        .iter()
        .map(|n| (n.as_bytes(), tar::EntryType::Regular, 0o644, b"".as_slice()))
        .collect();
    let file = archive(&entries);
    assert!(matches!(
        artifact::read(file.path(), 64, Purpose::Output),
        Err(SupervisorError::Limit)
    ));
    let mut file = tempfile::NamedTempFile::new().unwrap();
    let mut header = tar::Header::new_ustar();
    header.set_path("truncated").unwrap();
    header.set_size(64);
    header.set_mode(0o644);
    header.set_cksum();
    file.write_all(header.as_bytes()).unwrap();
    file.write_all(b"short").unwrap();
    assert!(artifact::read(file.path(), 64, Purpose::Output).is_err());
}
#[test]
fn only_the_current_verifying_revision_and_epoch_authorize_execution() {
    use crate::execution_snapshot::{CommandSpec, ExecutionSpec, LinuxProfile, ResourceLimits};
    use crate::local_target::tests::Fixture;
    let mut f = Fixture::new();
    let owner =
        target::acquire_controller(&mut f.store, "executor-authority", "appointed").unwrap();
    let spec = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, spec).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let limits = ResourceLimits {
        timeout_ms: 1000,
        memory_bytes: 67108864,
        output_bytes: 1048576,
        pids: 32,
    };
    let command = CommandSpec {
        program: "/bin/true".into(),
        arguments: vec![],
        environment: Default::default(),
    };
    let digest = format!("sha256:{}", "a".repeat(64));
    let spec = ExecutionSpec {
        candidate_source: f.a.result.snapshot_id.to_string(),
        preparation: None,
        dependencies: None,
        configuration: None,
        harness: f.b.result.snapshot_id.to_string(),
        dataset: None,
        assertion_inventory: digest.clone(),
        materialization_profile: snapshot::MATERIALIZATION_PROFILE.into(),
        profile: LinuxProfile {
            build_image: digest.clone(),
            verifier_image: digest.clone(),
            architecture: "arm64".into(),
            runtime_profile: digest,
            build_limits: limits,
            verifier_limits: limits,
        },
        build: command.clone(),
        verify: command,
    };
    let execution = snapshot::freeze(&mut f.store, &owner, &revision.id, spec).unwrap();
    assert!(matches!(
        active(&f.store, &owner, &execution),
        Err(SupervisorError::Cancelled)
    ));
    for phase in [
        target::Phase::Admitting,
        target::Phase::WaveFrozen,
        target::Phase::Composing,
        target::Phase::Verifying,
    ] {
        let state = target::status(&f.store, owner.target()).unwrap();
        target::transition(&mut f.store, &owner, state.version, phase).unwrap();
    }
    active(&f.store, &owner, &execution).unwrap();
    // Corrupt the selected subject while retaining a valid owner/phase: refuse.
    f.store
        .connection()
        .execute(
            "UPDATE local_targets SET active_revision=?1 WHERE target=?2",
            (format!("sha256:{}", "b".repeat(64)), owner.target()),
        )
        .unwrap();
    assert!(active(&f.store, &owner, &execution).is_err());
    f.store
        .connection()
        .execute(
            "UPDATE local_targets SET active_revision=?1 WHERE target=?2",
            (revision.id.as_str(), owner.target()),
        )
        .unwrap();
    let state = target::status(&f.store, owner.target()).unwrap();
    target::transition(
        &mut f.store,
        &owner,
        state.version,
        target::Phase::StopRequested,
    )
    .unwrap();
    assert!(matches!(
        active(&f.store, &owner, &execution),
        Err(SupervisorError::Cancelled)
    ));
    crate::local_authority::freeze(
        &mut f.store,
        owner.authority_epoch(),
        "explicit test-only freeze",
    )
    .unwrap();
    assert!(active(&f.store, &owner, &execution).is_err());
}
