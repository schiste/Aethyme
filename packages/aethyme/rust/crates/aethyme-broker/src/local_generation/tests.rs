use super::*;

fn git(repo: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}
struct Fixture {
    v: crate::execution_verifier::tests::Subject,
    broker: Broker,
    carrier: GitCarrier,
    binding: RecordId,
    evidence: crate::execution_verifier::TrustedEvidence,
    _remote: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let mut v = crate::execution_verifier::tests::Subject::acceptance_fixture();
        v.f.store
            .connection()
            .execute(
                "UPDATE local_targets SET phase='idle' WHERE target=?1",
                [v.owner.target()],
            )
            .unwrap();
        crate::local_wave::available(&mut v.f.store, &v.owner, "A", 0).unwrap();
        let wave = match crate::local_wave::admit(
            &mut v.f.store,
            &v.owner,
            crate::local_wave::WavePolicy {
                debounce_ms: 0,
                ..Default::default()
            },
            1,
        )
        .unwrap()
        {
            crate::local_wave::Admission::Frozen(w) => w,
            _ => panic!(),
        };
        let wave = crate::local_wave::compose(&mut v.f.store, &v.owner, v.f._repo.path(), &wave.id)
            .unwrap();
        let status = target::status(&v.f.store, v.owner.target()).unwrap();
        target::resume(&mut v.f.store, &v.owner, status.version).unwrap();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        git(
            v.f._repo.path(),
            &[
                "remote",
                "add",
                "publication",
                remote.path().to_str().unwrap(),
            ],
        );
        let mut broker = Broker::open(v.f._repo.path())
            .unwrap()
            .with_host_operation_database(v.f._host.path().join("host-operations.db"))
            .with_worktree_root(v.f._host.path().join("worktrees"));
        let session = broker
            .adopt(
                v.f._repo.path(),
                Some("disposable Playground generation fixture"),
            )
            .unwrap();
        let carrier = GitCarrier::new(
            &mut broker,
            session.id,
            "publication",
            "refs/heads/accepted",
            30,
            "operator authorizes disposable Playground generation",
            true,
        )
        .unwrap();
        let base = v.f.a.base.commit.clone();
        git(
            v.f._repo.path(),
            &[
                "push",
                "-q",
                "publication",
                &format!("{}:refs/heads/accepted", base.as_str()),
            ],
        );
        let intent = crate::local_source_transport::prepare(
            &mut v.f.store,
            &v.owner,
            &carrier,
            &wave.id,
            &"a".repeat(64),
            Some(&base),
        )
        .unwrap();
        let binding = acceptance::appoint(
            &mut v.f.store,
            &v.owner,
            &mut broker,
            &carrier,
            &intent.id,
            acceptance::ProviderRoute::GenericGit,
            "refs/heads/accepted",
            64,
        )
        .unwrap();
        crate::local_source_transport::publish(
            &mut v.f.store,
            &v.owner,
            &carrier,
            &mut broker,
            &intent.id,
        )
        .unwrap();
        let evidence = v.evidence();
        Self {
            v,
            evidence,
            broker,
            carrier,
            binding: binding.id().clone(),
            _remote: remote,
        }
    }
    fn old(&self) -> OldWorkspace {
        OldWorkspace {
            session: "original-agent".into(),
            absolute_root: self.v.f._repo.path().canonicalize().unwrap(),
            generation: None,
            pending_contributions: vec![self.v.f.b.lineage_record_id.to_string()],
        }
    }
    fn prepare(&mut self, operation: &str) -> Result<PreparedGeneration, GenerationError> {
        let old = self.old();
        let evidence = &self.evidence;
        let grant = acceptance::qualify(
            &mut self.v.f.store,
            &self.v.owner,
            &mut self.broker,
            &self.carrier,
            &self.binding,
            &self.v.execution,
            &self.v.artifact,
            &self.v.permit,
            evidence,
        )?;
        prepare(
            &mut self.v.f.store,
            &self.v.owner,
            &mut self.broker,
            &self.carrier,
            &grant,
            operation,
            old,
        )
    }
}
#[test]
fn independent_generation_is_editable_standalone_and_leaves_old_edits_and_open_files() {
    let mut f = Fixture::new();
    let old = f.old();
    let head = git(&old.absolute_root, &["rev-parse", "HEAD"]);
    let path = old.absolute_root.join("late-dirty.txt");
    let mut writer = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .unwrap();
    writer.write_all(b"before\n").unwrap();
    writer.sync_all().unwrap();
    let generated = f.prepare("first-generation").unwrap();
    writer.write_all(b"after\n").unwrap();
    writer.sync_all().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"before\nafter\n");
    assert_eq!(git(&old.absolute_root, &["rev-parse", "HEAD"]), head);
    assert!(!generated.absolute_root().join("late-dirty.txt").exists());
    assert!(generated.absolute_root().is_absolute());
    assert_ne!(generated.absolute_root(), old.absolute_root);
    assert_eq!(
        git(generated.absolute_root(), &["rev-parse", "--show-toplevel"]),
        generated.absolute_root().to_str().unwrap()
    );
    assert!(git(generated.absolute_root(), &["remote"]).is_empty());
    assert!(
        !generated
            .absolute_root()
            .join(".git/objects/info/alternates")
            .exists()
    );
    assert!(!generated.absolute_root().join(".git/hooks").exists());
    assert_eq!(
        git(generated.absolute_root(), &["rev-list", "--count", "HEAD"]),
        "1"
    );
    assert_eq!(
        git(generated.absolute_root(), &["status", "--porcelain"]),
        ""
    );
    assert_eq!(
        generated.mapping().provider_commit,
        git(f._remote.path(), &["rev-parse", "refs/heads/accepted"])
    );
    assert_ne!(
        generated.mapping().local_commit,
        generated.mapping().provider_commit
    );
    assert_eq!(
        target::status(&f.v.f.store, f.v.owner.target())
            .unwrap()
            .phase,
        target::Phase::Verifying
    );
    let source = manifest_entry_path(&f.v.f.store, &generated);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_ne!(
            std::fs::metadata(&source).unwrap().permissions().mode() & 0o200,
            0
        );
    }
    std::fs::write(source, b"agent's next edit\n").unwrap();
    assert!(load_prepared(&f.v.f.store, generated.id()).is_err());
}
fn manifest_entry_path(store: &CollaborationStore, g: &PreparedGeneration) -> PathBuf {
    let manifest = snapshot(store, &g.intent.source).unwrap();
    g.absolute_root()
        .join(std::str::from_utf8(manifest.entries()[0].path()).unwrap())
}
#[test]
fn interrupted_source_is_preserved_and_an_event_cannot_adopt() {
    let mut f = Fixture::new();
    let old = f.old();
    let evidence = &f.evidence;
    let grant = acceptance::qualify(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &f.binding,
        &f.v.execution,
        &f.v.artifact,
        &f.v.permit,
        evidence,
    )
    .unwrap();
    let result = prepare_with(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &grant,
        "crashed",
        old.clone(),
        &mut |stage, _| {
            if stage == Stage::Materialized {
                Err(GenerationError::Incomplete)
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(result, Err(GenerationError::Incomplete)));
    let generation: String =
        f.v.f
            .store
            .read_connection()
            .query_row(
                "SELECT id FROM local_generations WHERE operation='crashed'",
                [],
                |r| r.get(0),
            )
            .unwrap();
    let path = directory(&f.v.f.store, &id(&generation).unwrap()).join("source");
    std::fs::write(path.join("concurrent.txt"), b"retained partial evidence").unwrap();
    assert!(matches!(
        prepare(
            &mut f.v.f.store,
            &f.v.owner,
            &mut f.broker,
            &f.carrier,
            &grant,
            "crashed",
            old
        ),
        Err(GenerationError::Incomplete)
    ));
    assert_eq!(
        std::fs::read(path.join("concurrent.txt")).unwrap(),
        b"retained partial evidence"
    );
    assert!(load_prepared(&f.v.f.store, &id(&generation).unwrap()).is_err());
    assert_eq!(
        target::status(&f.v.f.store, f.v.owner.target())
            .unwrap()
            .phase,
        target::Phase::Verifying
    );
}
#[test]
fn changed_target_at_final_commit_refuses_a_prepared_receipt() {
    let mut f = Fixture::new();
    let old = f.old();
    let evidence = &f.evidence;
    let grant = acceptance::qualify(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &f.binding,
        &f.v.execution,
        &f.v.artifact,
        &f.v.permit,
        evidence,
    )
    .unwrap();
    let target_name = f.v.owner.target().to_string();
    let result = prepare_with(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &grant,
        "stop-during-copy",
        old,
        &mut |stage, store| {
            if stage == Stage::Durable {
                store.connection().execute(
                    "UPDATE local_targets SET phase='stop_requested' WHERE target=?1",
                    [&target_name],
                )?;
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row("SELECT COUNT(*) FROM local_generation_receipts", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}
#[test]
fn provider_movement_during_materialization_refuses_the_generation() {
    let mut f = Fixture::new();
    let old = f.old();
    let evidence = &f.evidence;
    let repo = f.v.f._repo.path().to_path_buf();
    let wrong = f.v.f.b.result.commit.as_str().to_string();
    let grant = acceptance::qualify(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &f.binding,
        &f.v.execution,
        &f.v.artifact,
        &f.v.permit,
        evidence,
    )
    .unwrap();
    let result = prepare_with(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &grant,
        "provider-moved",
        old,
        &mut |stage, _| {
            if stage == Stage::Materialized {
                git(
                    &repo,
                    &[
                        "push",
                        "-q",
                        "--force",
                        "publication",
                        &format!("{wrong}:refs/heads/accepted"),
                    ],
                );
            }
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(GenerationError::Acceptance(
            acceptance::AcceptanceError::NotAccepted
        ))
    ));
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row("SELECT COUNT(*) FROM local_generation_receipts", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}
#[test]
fn complete_source_and_private_git_metadata_are_checked_on_load() {
    let mut f = Fixture::new();
    let g = f.prepare("metadata").unwrap();
    assert_eq!(load_prepared(&f.v.f.store, g.id()).unwrap(), g);
    std::fs::write(g.absolute_root().join("unexpected"), b"new file").unwrap();
    assert!(load_prepared(&f.v.f.store, g.id()).is_err());
    std::fs::remove_file(g.absolute_root().join("unexpected")).unwrap();
    assert!(load_prepared(&f.v.f.store, g.id()).is_ok());
    std::fs::write(
        g.absolute_root().join(".git/config"),
        b"[core]\n hooksPath=/tmp/untrusted\n",
    )
    .unwrap();
    assert!(load_prepared(&f.v.f.store, g.id()).is_err());
}

#[test]
fn generation_roots_survive_target_release_and_a_preexisting_gc_plan() {
    let mut f = Fixture::new();
    // Plan while inputs are target-rooted; then generation creation changes the
    // root set, and target retirement cannot revoke the prepared source promise.
    let options = collaboration_gc::GcOptions {
        grace_ms: collaboration_gc::PLAN_TTL_MS,
    };
    let garbage = archive::put_object(&f.v.f.store, b"generation-gc-unreferenced-before").unwrap();
    age_objects(&f.v.f.store);
    let plan = collaboration_gc::plan(&mut f.v.f.store, &options).unwrap();
    assert!(!plan.reclaimable.is_empty());
    let g = f.prepare("roots").unwrap();
    f.v.f
        .store
        .connection()
        .execute("DELETE FROM local_target_roots", [])
        .unwrap();
    let _report = collaboration_gc::apply(&mut f.v.f.store, &plan.digest, &options).unwrap();
    assert_eq!(load_prepared(&f.v.f.store, g.id()).unwrap(), g);
    for reference in &g.intent.old.pending_contributions {
        assert!(
            archive::retained_contribution(&f.v.f.store, &id(reference).unwrap())
                .unwrap()
                .is_some()
        );
    }
    assert!(archive::read_object(&f.v.f.store, &garbage).is_err());
    archive::put_object(&f.v.f.store, b"generation-gc-unreferenced-after").unwrap();
    age_objects(&f.v.f.store);
    let fresh = collaboration_gc::plan(&mut f.v.f.store, &options).unwrap();
    assert!(fresh.blockers.is_empty(), "{:?}", fresh.blockers);
    let _report = collaboration_gc::apply(&mut f.v.f.store, &fresh.digest, &options).unwrap();
    assert!(
        archive::retained(
            &f.v.f.store,
            &SourceSnapshotId::parse(&g.intent.source).unwrap()
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(load_prepared(&f.v.f.store, g.id()).unwrap(), g);
}
#[test]
fn producer_revocation_during_copy_prevents_prepared_receipt() {
    let mut f = Fixture::new();
    let old = f.old();
    let evidence = &f.evidence;
    let grant = acceptance::qualify(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &f.binding,
        &f.v.execution,
        &f.v.artifact,
        &f.v.permit,
        evidence,
    )
    .unwrap();
    let owner = &f.v.owner;
    let producer = f.v.permit.producer().to_string();
    let generation = f.v.permit.generation();
    let result = prepare_with(
        &mut f.v.f.store,
        owner,
        &mut f.broker,
        &f.carrier,
        &grant,
        "revoked-during-copy",
        old,
        &mut |stage, store| {
            if stage == Stage::Materialized {
                crate::execution_verifier::revoke(store, owner, &producer, generation)
                    .map_err(acceptance::AcceptanceError::from)?;
            }
            Ok(())
        },
    );
    assert!(matches!(
        result,
        Err(GenerationError::Acceptance(
            acceptance::AcceptanceError::Verification(
                crate::execution_verifier::VerificationError::Producer
            )
        ))
    ));
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row("SELECT COUNT(*) FROM local_generation_receipts", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap(),
        0
    );
}
#[test]
fn fresh_qualification_replays_the_same_operation_but_changed_observation_does_not() {
    let mut f = Fixture::new();
    let g = f.prepare("stable-operation").unwrap();
    assert_eq!(f.prepare("stable-operation").unwrap(), g);
    let mut old = f.old();
    old.session = "different-agent".into();
    let evidence = &f.evidence;
    let grant = acceptance::qualify(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &f.binding,
        &f.v.execution,
        &f.v.artifact,
        &f.v.permit,
        evidence,
    )
    .unwrap();
    assert!(matches!(
        prepare(
            &mut f.v.f.store,
            &f.v.owner,
            &mut f.broker,
            &f.carrier,
            &grant,
            "stable-operation",
            old
        ),
        Err(GenerationError::Invalid(_))
    ));
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row("SELECT COUNT(*) FROM local_generations", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}
#[test]
fn raw_checkout_rebuilds_from_cas_after_original_git_repository_disappears() {
    let mut f = Fixture::new();
    let source = f.v.f.a.result.snapshot_id.clone();
    let manifest = snapshot(&f.v.f.store, source.as_str()).unwrap();
    std::fs::remove_dir_all(f.v.f._repo.path().join(".git")).unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let source_dir = workspace.path().join("source");
    archive::reconstruct(&f.v.f.store, &source, &source_dir).unwrap();
    let scratch = tempfile::tempdir_in(workspace.path()).unwrap();
    let (tree, commit) =
        crate::composer::generation_checkout(&f.v.f.store, &source_dir, scratch.path(), &manifest)
            .unwrap();
    crate::composer::verify_generation_checkout(&source_dir, &manifest, &tree, commit.as_str())
        .unwrap();
    assert_eq!(git(&source_dir, &["status", "--porcelain"]), "");
    // This qualifies raw reconstruction only: destroyed carrier state cannot
    // supply a new native acceptance observation and is not silently bypassed.
    let _ = &mut f;
}
#[test]
fn an_existing_symlink_destination_is_never_followed_or_overwritten() {
    #[cfg(unix)]
    {
        let mut f = Fixture::new();
        let old = f.old();
        let evidence = &f.evidence;
        let grant = acceptance::qualify(
            &mut f.v.f.store,
            &f.v.owner,
            &mut f.broker,
            &f.carrier,
            &f.binding,
            &f.v.execution,
            &f.v.artifact,
            &f.v.permit,
            evidence,
        )
        .unwrap();
        let foreign = tempfile::tempdir().unwrap();
        let marker = foreign.path().join("keep");
        std::fs::write(&marker, b"unrelated").unwrap();
        let result = prepare_with(
            &mut f.v.f.store,
            &f.v.owner,
            &mut f.broker,
            &f.carrier,
            &grant,
            "symlink",
            old,
            &mut |stage, store| {
                if stage == Stage::Intent {
                    let generation: String = store.read_connection().query_row(
                        "SELECT id FROM local_generations WHERE operation='symlink'",
                        [],
                        |r| r.get(0),
                    )?;
                    let parent = store.project_dir().join("generations");
                    private_directory(&parent)?;
                    std::os::unix::fs::symlink(
                        foreign.path(),
                        directory(store, &id(&generation)?),
                    )?;
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(GenerationError::Incomplete)));
        assert_eq!(std::fs::read(marker).unwrap(), b"unrelated");
        assert_eq!(std::fs::read_dir(foreign.path()).unwrap().count(), 1);
    }
}
fn age_objects(store: &CollaborationStore) {
    let root = archive::objects_dir(store);
    for prefix in std::fs::read_dir(root).unwrap() {
        for object in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            File::open(object.unwrap().path())
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH),
                )
                .unwrap();
        }
    }
}
#[test]
fn a_prepared_generation_can_be_the_old_dirty_workspace_of_the_next() {
    let mut f = Fixture::new();
    let g1 = f.prepare("g1").unwrap();
    let dirty = g1.absolute_root().join("next-unsubmitted.txt");
    std::fs::write(&dirty, b"agent's ongoing work").unwrap();
    let old = OldWorkspace {
        session: "agent-g1".into(),
        absolute_root: g1.absolute_root().to_path_buf(),
        generation: Some(g1.id().to_string()),
        pending_contributions: g1.intent.old.pending_contributions.clone(),
    };
    let grant = acceptance::qualify(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &f.binding,
        &f.v.execution,
        &f.v.artifact,
        &f.v.permit,
        &f.evidence,
    )
    .unwrap();
    let g2 = prepare(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &grant,
        "g2",
        old.clone(),
    )
    .unwrap();
    assert_ne!(g1.absolute_root(), g2.absolute_root());
    assert_eq!(g2.intent.old, old);
    assert_eq!(std::fs::read(&dirty).unwrap(), b"agent's ongoing work");
    assert!(!g2.absolute_root().join("next-unsubmitted.txt").exists());
    assert_eq!(load_prepared(&f.v.f.store, g2.id()).unwrap(), g2);
}
#[test]
fn a_symlinked_generation_ancestor_cannot_reuse_a_prepared_receipt() {
    #[cfg(unix)]
    {
        let mut f = Fixture::new();
        let g = f.prepare("ancestor").unwrap();
        let original = g.absolute_root().parent().unwrap();
        let moved = original.with_extension("moved");
        std::fs::rename(original, &moved).unwrap();
        std::os::unix::fs::symlink(&moved, original).unwrap();
        assert!(matches!(
            load_prepared(&f.v.f.store, g.id()),
            Err(GenerationError::Incomplete)
        ));
        assert!(moved.join("source/.git").exists());
    }
}

#[test]
fn independent_prepared_vector_binds_source_and_both_git_representations() {
    let v: serde_json::Value = serde_json::from_str(include_str!("prepared-vector.json")).unwrap();
    let bytes = v["canonical"].as_str().unwrap().as_bytes();
    let expected = id(v["id"].as_str().unwrap()).unwrap();
    let spec: PreparedSpec = decode(&PREPARED, &expected, bytes).unwrap();
    assert_eq!(record(&PREPARED, &spec).unwrap().canonical_bytes(), bytes);
    let raw: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    for key in [
        "profile",
        "generation",
        "source",
        "provider_commit",
        "tree",
        "local_commit",
        "git_metadata",
    ] {
        let mut changed = raw.clone();
        changed["spec"][key] = "changed".into();
        assert_ne!(
            Record::decode(&serde_json::to_vec(&changed).unwrap(), &[&PREPARED])
                .unwrap()
                .id(),
            expected,
            "{key}"
        );
    }
}

#[test]
fn crash_before_source_and_after_durable_files_never_implies_preparation_or_adoption() {
    let mut f = Fixture::new();
    for (stage, operation) in [
        (Stage::Intent, "crash-intent"),
        (Stage::Durable, "crash-durable"),
    ] {
        let old = f.old();
        let grant = acceptance::qualify(
            &mut f.v.f.store,
            &f.v.owner,
            &mut f.broker,
            &f.carrier,
            &f.binding,
            &f.v.execution,
            &f.v.artifact,
            &f.v.permit,
            &f.evidence,
        )
        .unwrap();
        let result = prepare_with(
            &mut f.v.f.store,
            &f.v.owner,
            &mut f.broker,
            &f.carrier,
            &grant,
            operation,
            old.clone(),
            &mut |at, _| {
                if at == stage {
                    Err(GenerationError::Incomplete)
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(GenerationError::Incomplete)));
        let (generation, state): (String, String) =
            f.v.f
                .store
                .read_connection()
                .query_row(
                    "SELECT id,state FROM local_generations WHERE operation=?1",
                    [operation],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
        assert_eq!(state, "intent");
        let generation = id(&generation).unwrap();
        assert!(load_prepared(&f.v.f.store, &generation).is_err());
        let dir = directory(&f.v.f.store, &generation);
        if stage == Stage::Intent {
            assert!(!dir.exists());
            // An INTENT without any source writes can resume the exact request.
            let g = prepare(
                &mut f.v.f.store,
                &f.v.owner,
                &mut f.broker,
                &f.carrier,
                &grant,
                operation,
                old,
            )
            .unwrap();
            assert_eq!(g.id(), &generation);
        } else {
            assert!(dir.join("prepared.json").is_file());
            let marker = dir.join("source/partial-evidence");
            std::fs::write(&marker, b"preserve").unwrap();
            assert!(matches!(
                prepare(
                    &mut f.v.f.store,
                    &f.v.owner,
                    &mut f.broker,
                    &f.carrier,
                    &grant,
                    operation,
                    old
                ),
                Err(GenerationError::Incomplete)
            ));
            assert_eq!(std::fs::read(marker).unwrap(), b"preserve");
        }
        assert_eq!(
            target::status(&f.v.f.store, f.v.owner.target())
                .unwrap()
                .phase,
            target::Phase::Verifying
        );
    }
}

#[test]
fn actual_late_child_and_watcher_remain_attached_to_old_absolute_workspace() {
    use std::io::{BufRead, BufReader};
    use std::process::{Child, Command, Stdio};
    struct OwnedChild(Option<Child>);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                if let Err(e) = child.kill() {
                    eprintln!("child cleanup: {e}");
                }
                if let Err(e) = child.wait() {
                    eprintln!("child reap: {e}");
                }
            }
        }
    }
    let mut f = Fixture::new();
    let old = f.old();
    std::fs::write(old.absolute_root.join("watch.txt"), b"watch-v1\n").unwrap();
    let mut child=OwnedChild(Some(Command::new("/bin/sh").arg("-c").arg("printf 'ready\\n'; IFS= read -r command; printf 'late child edit\\n' >> child.txt; cat watch.txt").current_dir(&old.absolute_root).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap()));
    let mut line = String::new();
    BufReader::new(child.0.as_mut().unwrap().stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "ready\n");
    let grant = acceptance::qualify(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &f.binding,
        &f.v.execution,
        &f.v.artifact,
        &f.v.permit,
        &f.evidence,
    )
    .unwrap();
    let old_root = old.absolute_root.clone();
    let g = prepare_with(
        &mut f.v.f.store,
        &f.v.owner,
        &mut f.broker,
        &f.carrier,
        &grant,
        "child-watcher",
        old,
        &mut |stage, _| {
            if stage == Stage::Materialized {
                std::fs::write(old_root.join("watch.txt"), b"watch-v2\n")?;
                let mut running = child.0.take().unwrap();
                running.stdin.as_mut().unwrap().write_all(b"continue\n")?;
                let output = running.wait_with_output()?;
                assert!(output.status.success());
                assert_eq!(output.stdout, b"watch-v2\n");
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(
        std::fs::read(old_root.join("child.txt")).unwrap(),
        b"late child edit\n"
    );
    assert!(!g.absolute_root().join("child.txt").exists());
    assert!(!g.absolute_root().join("watch.txt").exists());
    assert_eq!(load_prepared(&f.v.f.store, g.id()).unwrap(), g);
}
