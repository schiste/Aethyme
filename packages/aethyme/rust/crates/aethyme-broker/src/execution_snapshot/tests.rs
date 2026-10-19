use super::*;
use crate::collaboration_archive::CommitOid;
use crate::local_target::tests::{Fixture, git};
use aethyme_contracts::experimental_v0::source_snapshot::{EntryKind, SourceEntry};

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

fn manifest(paths: impl IntoIterator<Item = String>) -> SourceSnapshot {
    SourceSnapshot::new(
        paths
            .into_iter()
            .map(|path| SourceEntry::from_content(path, EntryKind::Regular, b"")),
    )
    .unwrap()
}

#[test]
fn descriptor_reader_enforces_the_injected_byte_limit() {
    let f = Fixture::new();
    let bytes = b"12345";
    let digest = archive::put_object(&f.store, bytes).unwrap();

    assert!(matches!(
        archive::read_object_bounded(&f.store, &digest, bytes.len() - 1),
        Err(archive::ArchiveError::ObjectTooLarge { limit: 4, .. })
    ));
    assert!(matches!(
        read_descriptor_object(&f.store, &digest, bytes.len() - 1),
        Err(SnapshotError::Limit)
    ));
    assert_eq!(
        read_descriptor_object(&f.store, &digest, bytes.len()).unwrap(),
        bytes
    );
}

#[test]
fn materialization_rejects_aggregate_file_count_across_roles() {
    let source = manifest((0..4_097).map(|index| format!("source-{index:04}.txt")));
    let harness = manifest((0..4_097).map(|index| format!("harness-{index:04}.txt")));

    assert!(matches!(
        validate_materialization_bounds(&[("source", &source), ("harness", &harness)]),
        Err(SnapshotError::MaterializationLimit(
            "aggregate file count exceeds 8192"
        ))
    ));
}

#[test]
fn materialization_rejects_file_limit_plus_one() {
    let source = manifest((0..8_193).map(|index| format!("file-{index:04}.txt")));

    assert_eq!(source.entries().len(), 8_193);
    assert!(matches!(
        validate_materialization_bounds(&[("source", &source)]),
        Err(SnapshotError::MaterializationLimit(
            "aggregate file count exceeds 8192"
        ))
    ));
}

#[test]
fn materialization_rejects_excessive_path_depth() {
    let path = std::iter::repeat_n("d", 64)
        .chain(["file.txt"])
        .collect::<Vec<_>>()
        .join("/");
    assert_eq!(path.split('/').count(), 65);
    let source = manifest([path]);

    assert!(matches!(
        validate_materialization_bounds(&[("source", &source)]),
        Err(SnapshotError::MaterializationLimit(
            "path depth exceeds 64 components"
        ))
    ));
}

#[test]
fn materialization_rejects_excessive_component_length() {
    let path = format!("{}.txt", "x".repeat(252));
    assert_eq!(path.len(), 256);
    let source = manifest([path]);

    assert!(matches!(
        validate_materialization_bounds(&[("source", &source)]),
        Err(SnapshotError::MaterializationLimit(
            "path component exceeds 255 bytes"
        ))
    ));
}

#[test]
fn materialization_rejects_excessive_implicit_directory_nodes() {
    let source = manifest((0..300).map(|file| {
        std::iter::once(format!("root-{file:03}"))
            .chain((0..62).map(|depth| format!("dir-{depth:02}-{file:03}")))
            .chain([format!("file-{file:03}.txt")])
            .collect::<Vec<_>>()
            .join("/")
    }));

    assert!(matches!(
        validate_materialization_bounds(&[("source", &source)]),
        Err(SnapshotError::MaterializationLimit(
            "aggregate file and directory node count exceeds 16384"
        ))
    ));
}

#[test]
fn materialization_accepts_exact_aggregate_file_limit_across_roles() {
    let source = manifest((0..4_096).map(|index| format!("source-{index:04}.txt")));
    let harness = manifest((0..4_096).map(|index| format!("harness-{index:04}.txt")));

    assert_eq!(source.entries().len() + harness.entries().len(), 8_192);
    assert!(validate_materialization_bounds(&[("source", &source), ("harness", &harness)]).is_ok());
    assert!(portable(&source).is_ok());
    assert!(portable(&harness).is_ok());
}

#[test]
fn materialization_accepts_exact_path_depth_and_component_limits() {
    let depth_path = std::iter::repeat_n("d", 63)
        .chain(["file.txt"])
        .collect::<Vec<_>>()
        .join("/");
    let component_path = format!("{}.txt", "x".repeat(251));
    assert_eq!(depth_path.split('/').count(), 64);
    assert_eq!(component_path.len(), 255);
    let depth = manifest([depth_path]);
    let component = manifest([component_path]);

    assert!(validate_materialization_bounds(&[("source", &depth)]).is_ok());
    assert!(validate_materialization_bounds(&[("source", &component)]).is_ok());
    assert!(portable(&depth).is_ok());
    assert!(portable(&component).is_ok());
}

#[test]
fn materialization_accepts_exact_aggregate_node_limit() {
    // One role root + 8,191 unique directories + 8,192 files = 16,384 nodes.
    let mut paths: Vec<_> = (0..8_191)
        .map(|directory| format!("directory-{directory:04}/a.txt"))
        .collect();
    paths.push("directory-8190/b.txt".into());
    let source = manifest(paths);

    assert_eq!(source.entries().len(), 8_192);
    assert!(validate_materialization_bounds(&[("source", &source)]).is_ok());
    assert!(portable(&source).is_ok());
}

#[test]
fn materialization_rejects_aggregate_node_limit_plus_one() {
    // One role root + 8,192 unique directories + 8,192 files = 16,385 nodes.
    let source = manifest((0..8_192).map(|directory| format!("directory-{directory:04}/a.txt")));

    assert_eq!(source.entries().len(), 8_192);
    assert!(matches!(
        validate_materialization_bounds(&[("source", &source)]),
        Err(SnapshotError::MaterializationLimit(
            "aggregate file and directory node count exceeds 16384"
        ))
    ));
}

#[test]
fn portable_accepts_shared_directory_prefixes() {
    let source = manifest(["shared/a.txt".into(), "shared/nested/b.txt".into()]);

    assert!(portable(&source).is_ok());
}

#[test]
fn portable_enforces_materialization_path_and_file_limits() {
    let too_deep_path = std::iter::repeat_n("d", 64)
        .chain(["file.txt"])
        .collect::<Vec<_>>()
        .join("/");
    assert_eq!(too_deep_path.split('/').count(), 65);
    let too_deep = manifest([too_deep_path]);
    assert!(matches!(
        portable(&too_deep),
        Err(SnapshotError::MaterializationLimit(
            "path depth exceeds 64 components"
        ))
    ));

    let too_wide_path = format!("{}.txt", "x".repeat(252));
    assert_eq!(too_wide_path.len(), 256);
    let too_wide = manifest([too_wide_path]);
    assert!(matches!(
        portable(&too_wide),
        Err(SnapshotError::MaterializationLimit(
            "path component exceeds 255 bytes"
        ))
    ));

    let too_many = manifest((0..8_193).map(|index| format!("file-{index:04}.txt")));
    assert!(matches!(
        portable(&too_many),
        Err(SnapshotError::MaterializationLimit(
            "aggregate file count exceeds 8192"
        ))
    ));
}

#[test]
fn portable_rejects_node_overflow_before_prefix_allocation() {
    // The first raw-sorted path contains a backslash, so portable() returns
    // immediately if it reaches path checks before the node preflight. This
    // compact synthetic manifest avoids creating a large filesystem tree
    // while checking the common boundary used by execution, generation, and
    // artifact callers.
    let paths = std::iter::once("!bad\\dir/file.txt".to_owned()).chain((0..256).map(|file| {
        std::iter::once(format!("root-{file:03}"))
            .chain((0..62).map(|depth| format!("dir-{depth:02}-{file:03}")))
            .chain([format!("file-{file:03}.txt")])
            .collect::<Vec<_>>()
            .join("/")
    }));
    let source = manifest(paths);

    assert!(matches!(
        portable(&source),
        Err(SnapshotError::MaterializationLimit(
            "aggregate file and directory node count exceeds 16384"
        ))
    ));
}

#[test]
fn materialization_plan_refusal_does_not_create_a_workspace() {
    let source = manifest((0..4_097).map(|index| format!("source-{index:04}.txt")));
    let harness = manifest((0..4_097).map(|index| format!("harness-{index:04}.txt")));
    let temporary_root = tempfile::tempdir().unwrap();
    let mut writer_ran = false;

    let result = with_materialization_plan(
        || {
            validate_materialization_bounds(&[("source", &source), ("harness", &harness)])?;
            Ok(MaterializationPlan { roles: Vec::new() })
        },
        |_| {
            writer_ran = true;
            std::fs::create_dir(temporary_root.path().join("workspace"))?;
            Ok(())
        },
    );

    assert!(matches!(
        result,
        Err(SnapshotError::MaterializationLimit(
            "aggregate file count exceeds 8192"
        ))
    ));
    assert!(!writer_ran);
    assert_eq!(std::fs::read_dir(temporary_root.path()).unwrap().count(), 0);
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
fn load_refuses_snapshot_after_sensitive_quarantine() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let execution_spec = spec(&f);
    let snapshot = freeze(&mut f.store, &owner, &revision, execution_spec).unwrap();
    assert_eq!(load(&f.store, snapshot.reference()).unwrap(), snapshot);

    let epoch = owner.authority_epoch().clone();
    drop(owner);
    let frozen = crate::local_authority::freeze(&mut f.store, &epoch, "test quarantine").unwrap();
    let incident = format!("sha256:{}", "a".repeat(64));
    crate::collaboration_sensitive::quarantine(
        &mut f.store,
        &incident,
        "test-operator",
        frozen.epoch.as_str(),
        crate::collaboration_sensitive::ReasonCode::OperatorRequestedPurge,
    )
    .unwrap();

    assert!(matches!(
        load(&f.store, snapshot.reference()),
        Err(SnapshotError::Sensitive(
            crate::collaboration_sensitive::SensitiveError::Quarantined
        ))
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

#[test]
fn freeze_checks_each_materialized_role_even_when_retention_is_deduplicated() {
    let mut f = Fixture::new();
    let (owner, revision) = active(&mut f);
    let mut s = spec(&f);
    s.harness = s.candidate_source.clone(); // Each contains exactly two bytes.
    assert!(matches!(
        freeze_bounded(&mut f.store, &owner, &revision, s.clone(), 3),
        Err(SnapshotError::Limit)
    ));
    let snapshot = freeze_bounded(&mut f.store, &owner, &revision, s, 4).unwrap();
    assert!(materialize(&f.store, &owner, &snapshot).is_ok());
}
#[test]
fn noncanonical_lfs_pointer_variants_are_refused() {
    for version in [
        "https://git-lfs.github.com/spec/v1",
        "https://hawser.github.com/spec/v1",
        "http://git-media.io/v/2",
    ] {
        let mut f = Fixture::new();
        let pointer = format!(
            " \t\r\nversion {version}\r\noid sha256:{}\r\nsize 12\r\n \t",
            "a".repeat(64)
        );
        let id = capture(&mut f, &[("pointer", pointer.as_bytes())]);
        let (owner, revision) = active(&mut f);
        let mut s = spec(&f);
        s.candidate_source = id.to_string();
        assert!(matches!(
            freeze(&mut f.store, &owner, &revision, s),
            Err(SnapshotError::Unsupported(_))
        ));
    }
}
