use super::*;
use crate::collaboration_archive::CommitOid;
use crate::local_target::tests::{git, Fixture};

fn digest(c: char) -> String {
    format!("sha256:{}", c.to_string().repeat(64))
}
fn spec(f: &Fixture) -> ExecutionSpec {
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
    ExecutionSpec {
        candidate_source: f.a.result.snapshot_id.to_string(),
        preparation: None,
        dependencies: None,
        configuration: None,
        harness: f.b.result.snapshot_id.to_string(),
        dataset: None,
        assertion_inventory: digest('1'),
        materialization_profile: MATERIALIZATION_PROFILE.into(),
        profile: LinuxProfile {
            build_image: digest('2'),
            verifier_image: digest('3'),
            architecture: "arm64".into(),
            runtime_profile: digest('4'),
            build_limits: limits,
            verifier_limits: limits,
        },
        build: command.clone(),
        verify: command,
    }
}
fn active(f: &mut Fixture) -> (ControllerOwner, RecordId) {
    let owner = target::acquire_controller(&mut f.store, "execution", "appointed").unwrap();
    let s = f.spec();
    let revision = target::propose_revision(&mut f.store, &owner, s).unwrap();
    target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
    (owner, revision.id)
}
fn capture(f: &mut Fixture, files: &[(&str, &[u8])]) -> SourceSnapshotId {
    for (path, bytes) in files {
        let full = f._repo.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, bytes).unwrap();
    }
    git(f._repo.path(), &["add", "."]);
    git(f._repo.path(), &["commit", "-qm", "input"]);
    let head = CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    archive::retain_snapshot(&mut f.store, f._repo.path(), &head)
        .unwrap()
        .snapshot_id
}
#[test]
fn canonical_descriptor_roundtrip_and_each_input_identity_changes_execution() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let base = spec(&f);
    let first = freeze(&mut f.store, &owner, &revision, base.clone()).unwrap();
    assert_eq!(load(&f.store, first.reference()).unwrap(), first);
    assert_eq!(
        freeze(&mut f.store, &owner, &revision, base.clone()).unwrap(),
        first
    );
    let mut changed = vec![];
    let mut s = base.clone();
    s.candidate_source = s.harness.clone();
    changed.push(s);
    let mut s = base.clone();
    s.harness = s.candidate_source.clone();
    changed.push(s);
    let mut s = base.clone();
    s.dependencies = Some(s.harness.clone());
    changed.push(s);
    let mut s = base.clone();
    s.configuration = Some(s.harness.clone());
    changed.push(s);
    let mut s = base.clone();
    s.dataset = Some(s.harness.clone());
    changed.push(s);
    let mut s = base.clone();
    s.assertion_inventory = digest('5');
    changed.push(s);
    let mut s = base.clone();
    s.profile.build_image = digest('5');
    changed.push(s);
    let mut s = base.clone();
    s.profile.verifier_image = digest('5');
    changed.push(s);
    let mut s = base.clone();
    s.profile.runtime_profile = digest('5');
    changed.push(s);
    let mut s = base.clone();
    s.profile.architecture = "amd64".into();
    changed.push(s);
    let mut s = base.clone();
    s.profile.build_limits.pids += 1;
    changed.push(s);
    let mut s = base.clone();
    s.profile.verifier_limits.timeout_ms += 1;
    changed.push(s);
    let mut s = base.clone();
    s.build.arguments.push("features".into());
    changed.push(s);
    let mut s = base.clone();
    s.verify.arguments.push("inventory".into());
    changed.push(s);
    let mut s = base.clone();
    s.build.environment.insert("FEATURES".into(), "x".into());
    changed.push(s);
    let mut s = base.clone();
    s.verify.environment.insert("LANG".into(), "C".into());
    changed.push(s);
    let mut s = base.clone();
    s.preparation = Some(Preparation {
        recipe: digest('6'),
        inputs: vec![s.candidate_source.clone()],
        materialized_source: s.harness.clone(),
    });
    changed.push(s);
    for s in changed {
        assert_ne!(
            freeze(&mut f.store, &owner, &revision, s).unwrap().id(),
            first.id()
        );
    }
    let mut locator = first.reference().clone();
    locator.id = RecordId::parse(&digest('f')).unwrap();
    assert!(matches!(
        load(&f.store, &locator),
        Err(SnapshotError::Tampered)
    ));
}
#[test]
fn raw_bytes_modes_and_fresh_generations_ignore_ambient_worktree_files() {
    let mut f = Fixture::new();
    let id = capture(
        &mut f,
        &[("raw.bin", b"\0\xff\r\n"), ("script", b"#!/bin/sh\ntrue\n")],
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            f._repo.path().join("script"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    git(f._repo.path(), &["add", "script"]);
    git(f._repo.path(), &["commit", "-qm", "mode"]);
    let head = CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    let mode_id = archive::retain_snapshot(&mut f.store, f._repo.path(), &head)
        .unwrap()
        .snapshot_id;
    assert_ne!(id, mode_id);
    std::fs::write(f._repo.path().join("developer.env"), b"ambient").unwrap();
    let (owner, revision) = active(&mut f);
    let mut s = spec(&f);
    s.candidate_source = mode_id.to_string();
    let snapshot = freeze(&mut f.store, &owner, &revision, s).unwrap();
    let a = materialize(&f.store, &owner, &snapshot).unwrap();
    let b = materialize(&f.store, &owner, &snapshot).unwrap();
    assert_ne!(a.root(), b.root());
    assert_eq!(
        std::fs::read(a.source_dir().join("raw.bin")).unwrap(),
        b"\0\xff\r\n"
    );
    assert!(!a.source_dir().join("developer.env").exists());
    assert!(!a.source_dir().join(".git").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(a.source_dir().join("script"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
    }
}
#[test]
fn missing_preparation_input_and_bad_profiles_fail_before_retention() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let base = spec(&f);
    let mut s = base.clone();
    s.preparation = Some(Preparation {
        recipe: digest('1'),
        inputs: vec![digest('f')],
        materialized_source: s.harness.clone(),
    });
    assert!(freeze(&mut f.store, &owner, &revision, s).is_err());
    let mut s = base.clone();
    s.profile.build_image = "rust:latest".into();
    assert!(freeze(&mut f.store, &owner, &revision, s).is_err());
    let mut s = base.clone();
    s.build
        .environment
        .insert("secret-key".into(), "bad".into());
    assert!(freeze(&mut f.store, &owner, &revision, s).is_err());
    let mut s = base.clone();
    s.profile.build_limits.pids = 0;
    assert!(freeze(&mut f.store, &owner, &revision, s).is_err());
    let mut s = base.clone();
    s.materialization_profile = "checkout-with-filters".into();
    assert!(matches!(
        freeze(&mut f.store, &owner, &revision, s),
        Err(SnapshotError::Unsupported(_))
    ));
}
#[test]
fn unresolved_lfs_pointer_is_not_materialized_as_source() {
    let mut f = Fixture::new();
    let id = capture(
        &mut f,
        &[(
            "large",
            b"version https://git-lfs.github.com/spec/v1\noid sha256:bad\nsize 1\n",
        )],
    );
    let (owner, revision) = active(&mut f);
    let mut s = spec(&f);
    s.candidate_source = id.to_string();
    assert!(matches!(
        freeze(&mut f.store, &owner, &revision, s),
        Err(SnapshotError::Unsupported(_))
    ));
}
#[test]
fn prepared_source_is_distinct_and_original_inputs_stay_retained() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let mut s = spec(&f);
    s.preparation = Some(Preparation {
        recipe: digest('6'),
        inputs: vec![s.candidate_source.clone()],
        materialized_source: s.harness.clone(),
    });
    let snapshot = freeze(&mut f.store, &owner, &revision, s).unwrap();
    let m = materialize(&f.store, &owner, &snapshot).unwrap();
    assert_eq!(
        std::fs::read(m.source_dir().join("one.txt")).unwrap(),
        b"base\n"
    );
    assert!(m.source_dir().join("two.txt").exists());
    assert_ne!(
        snapshot.spec().candidate_source,
        snapshot
            .spec()
            .preparation
            .as_ref()
            .unwrap()
            .materialized_source
    );
}
#[test]
fn descriptor_data_does_not_authorize_an_inactive_revision() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let s = spec(&f);
    let snapshot = freeze(&mut f.store, &owner, &revision, s).unwrap();
    let mut next = f.spec();
    next.objective = "changed".into();
    let next = target::propose_revision(&mut f.store, &owner, next).unwrap();
    target::select_revision(&mut f.store, &owner, &next.id).unwrap();
    assert!(load(&f.store, snapshot.reference()).is_ok());
    assert!(matches!(
        materialize(&f.store, &owner, &snapshot),
        Err(SnapshotError::Stale)
    ));
}
#[test]
fn changed_or_missing_cas_bytes_are_rejected() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let s = spec(&f);
    let snapshot = freeze(&mut f.store, &owner, &revision, s).unwrap();
    let path = archive::object_path(&f.store, &snapshot.reference().object);
    std::fs::write(&path, b"forged").unwrap();
    assert!(load(&f.store, snapshot.reference()).is_err());
    std::fs::remove_file(path).unwrap();
    assert!(materialize(&f.store, &owner, &snapshot).is_err());
}
#[cfg(unix)]
#[test]
fn symlinks_and_non_utf8_paths_are_unsupported() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let mut f = Fixture::new();
    symlink("one.txt", f._repo.path().join("alias")).unwrap();
    git(f._repo.path(), &["add", "alias"]);
    git(f._repo.path(), &["commit", "-qm", "symlink"]);
    let head = CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    let id = archive::retain_snapshot(&mut f.store, f._repo.path(), &head)
        .unwrap()
        .snapshot_id;
    let (owner, revision) = active(&mut f);
    let mut s = spec(&f);
    s.candidate_source = id.to_string();
    assert!(matches!(
        freeze(&mut f.store, &owner, &revision, s),
        Err(SnapshotError::Unsupported(_))
    ));
    std::fs::remove_file(f._repo.path().join("alias")).unwrap();
    git(f._repo.path(), &["update-index", "--remove", "alias"]);
    let blob = git(f._repo.path(), &["rev-parse", "HEAD:one.txt"]);
    let output = std::process::Command::new("git")
        .current_dir(f._repo.path())
        .args(["update-index", "--add", "--cacheinfo", "100644", &blob])
        .arg(std::ffi::OsStr::from_bytes(b"bad\xff"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    git(f._repo.path(), &["commit", "-qm", "raw path"]);
    let head = CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
    let id = archive::retain_snapshot(&mut f.store, f._repo.path(), &head)
        .unwrap()
        .snapshot_id;
    let mut s = spec(&f);
    s.candidate_source = id.to_string();
    assert!(matches!(
        freeze(&mut f.store, &owner, &revision, s),
        Err(SnapshotError::Unsupported(_))
    ));
}

#[test]
fn folded_file_and_directory_aliases_are_refused_before_checkout() {
    for names in [
        ["Lib/a", "lib/b"],
        ["Readme", "README"],
        ["X", "x/file"],
        ["caf\u{e9}/a", "cafe\u{301}/b"],
    ] {
        let mut f = Fixture::new();
        git(
            f._repo.path(),
            &["config", "core.precomposeunicode", "false"],
        );
        let oid = git(f._repo.path(), &["rev-parse", "HEAD:one.txt"]);
        for name in names {
            git(
                f._repo.path(),
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{oid},{name}"),
                ],
            );
        }
        git(f._repo.path(), &["commit", "-qm", "portable collision"]);
        let head = CommitOid::parse(&git(f._repo.path(), &["rev-parse", "HEAD"])).unwrap();
        let id = archive::retain_snapshot(&mut f.store, f._repo.path(), &head)
            .unwrap()
            .snapshot_id;
        let (owner, revision) = active(&mut f);
        let mut s = spec(&f);
        s.candidate_source = id.to_string();
        assert!(
            matches!(
                freeze(&mut f.store, &owner, &revision, s),
                Err(SnapshotError::Unsupported(_))
            ),
            "{names:?}"
        );
    }
}
#[test]
fn descriptor_requires_its_exact_retention_root() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let s = spec(&f);
    let snapshot = freeze(&mut f.store, &owner, &revision, s).unwrap();
    f.store
        .read_connection()
        .execute(
            "DELETE FROM local_target_roots WHERE kind='object' AND reference=?1",
            [snapshot.reference().object.hex()],
        )
        .unwrap();
    assert!(matches!(
        load(&f.store, snapshot.reference()),
        Err(SnapshotError::Invalid(_))
    ));
}

#[test]
fn independently_encoded_execution_identity_vector() {
    // Oracle: Ruby JSON with recursive UTF-16 key sorting and Digest::SHA256,
    // plus the public record domain prefix. No production canonicalizer used.
    let f = Fixture::new();
    let mut s = spec(&f);
    s.candidate_source = digest('a');
    s.harness = digest('b');
    let revision = RecordId::parse(&digest('c')).unwrap();
    assert_eq!(
        record(&revision, &s).unwrap().id().as_str(),
        "sha256:3fc0f12ef73c9ecf3d4997dcacca855fa2246b7a79b7348671d7a60b913a4951"
    );
}
#[test]
fn a_new_target_revision_changes_execution_even_when_source_matches() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let s = spec(&f);
    let original = freeze(&mut f.store, &owner, &revision, s.clone()).unwrap();
    let mut next = f.spec();
    next.objective = "new obligation".into();
    let next = target::propose_revision(&mut f.store, &owner, next).unwrap();
    target::select_revision(&mut f.store, &owner, &next.id).unwrap();
    let changed = freeze(&mut f.store, &owner, &next.id, s).unwrap();
    assert_eq!(
        original.spec().candidate_source,
        changed.spec().candidate_source
    );
    assert_ne!(original.id(), changed.id());
}
