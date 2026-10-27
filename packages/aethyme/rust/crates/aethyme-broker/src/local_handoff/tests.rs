use super::*;
use crate::local_generation::tests::Fixture;
#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::time::{Duration, Instant};

fn agent(f: &mut Fixture) -> Agent {
    appoint(&mut f.v.f.store, &f.v.owner, "controlled-agent").unwrap()
}
fn request(
    f: &mut Fixture,
    agent: &Agent,
    g: &PreparedGeneration,
    operation: &str,
) -> Result<PreparedHandoff, HandoffError> {
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
    )?;
    let expiry = grant.expires_ms().min(now()? + 120_000);
    prepare(
        &mut f.v.f.store,
        &f.v.owner,
        agent,
        &mut f.broker,
        &f.carrier,
        &grant,
        g.id(),
        operation,
        expiry,
    )
}
#[cfg(unix)]
fn shell(script: &str) -> CommandSpec {
    CommandSpec {
        executable: PathBuf::from("/bin/sh").canonicalize().unwrap(),
        arguments: vec!["-c".into(), script.into()],
        acknowledgment_ms: 30_000,
    }
}
#[cfg(unix)]
fn launch(
    f: &mut Fixture,
    a: &Agent,
    r: &PreparedHandoff,
    c: CommandSpec,
) -> Result<RunningCommand, HandoffError> {
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
    )?;
    dispatch(
        &mut f.v.f.store,
        &f.v.owner,
        a,
        &mut f.broker,
        &f.carrier,
        &grant,
        r,
        c,
    )
}
#[cfg(unix)]
fn finish(f: &mut Fixture, a: &Agent, c: &mut RunningCommand) -> Completion {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result = poll_command(&mut f.v.f.store, &f.v.owner, a, c).unwrap();
        if result != Completion::Running {
            return result;
        };
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
}
#[cfg(unix)]
#[test]
fn actual_controlled_command_acknowledges_new_absolute_root_and_old_open_file_stays_old() {
    let mut f = Fixture::new();
    let g = f.prepare("new-root").unwrap();
    let a = agent(&mut f);
    let old_path = f.v.f._repo.path().join("late-old.txt");
    let mut old = std::fs::File::create(&old_path).unwrap();
    old.write_all(b"before\n").unwrap();
    let r = request(&mut f, &a, &g, "switch").unwrap();
    assert_eq!(show(&f.v.f.store, r.id()).unwrap().state, "prepared");
    let mut c = launch(
        &mut f,
        &a,
        &r,
        shell("pwd -P > actual-root.txt; printf new > new-only.txt"),
    )
    .unwrap();
    old.write_all(b"after\n").unwrap();
    old.sync_all().unwrap();
    assert_eq!(finish(&mut f, &a, &mut c), Completion::Current(0));
    assert_eq!(std::fs::read(&old_path).unwrap(), b"before\nafter\n");
    assert!(!g.absolute_root().join("late-old.txt").exists());
    assert_eq!(
        std::fs::read_to_string(g.absolute_root().join("actual-root.txt"))
            .unwrap()
            .trim(),
        g.absolute_root().to_str().unwrap()
    );
    assert_eq!(
        std::fs::read(g.absolute_root().join("new-only.txt")).unwrap(),
        b"new"
    );
    assert!(!f.v.f._repo.path().join("new-only.txt").exists());
    let view = show(&f.v.f.store, r.id()).unwrap();
    assert_eq!(view.state, "adopted");
    assert!(view.receipt.is_some());
    current_agent(&mut f.v.f.store, &f.v.owner, &a, &c).unwrap();
    assert!(matches!(
        launch(&mut f, &a, &r, shell("exit 0")),
        Err(HandoffError::Unknown)
    ));
}
#[cfg(unix)]
#[test]
fn busy_dispatch_requires_explicit_old_attachment_and_old_result_is_historical() {
    let mut f = Fixture::new();
    let g1 = f.prepare("first").unwrap();
    let g2 = f.prepare("second").unwrap();
    let a = agent(&mut f);
    let r1 = request(&mut f, &a, &g1, "first").unwrap();
    let mut old = launch(&mut f, &a, &r1, shell("sleep 1; printf old > delayed.txt")).unwrap();
    assert!(matches!(
        request(&mut f, &a, &g2, "second"),
        Err(HandoffError::Busy)
    ));
    retain_old_attachment(&mut f.v.f.store, &f.v.owner, &a, &old).unwrap();
    assert!(matches!(
        current_agent(&mut f.v.f.store, &f.v.owner, &a, &old),
        Err(HandoffError::Stale)
    ));
    let r2 = request(&mut f, &a, &g2, "second").unwrap();
    let mut new = launch(&mut f, &a, &r2, shell("printf new > current.txt")).unwrap();
    assert!(matches!(
        current_agent(&mut f.v.f.store, &f.v.owner, &a, &old),
        Err(HandoffError::Stale)
    ));
    assert_eq!(finish(&mut f, &a, &mut old), Completion::Historical(0));
    current_agent(&mut f.v.f.store, &f.v.owner, &a, &new).unwrap();
    assert_eq!(finish(&mut f, &a, &mut new), Completion::Current(0));
    assert_eq!(
        std::fs::read(g1.absolute_root().join("delayed.txt")).unwrap(),
        b"old"
    );
    assert!(!g2.absolute_root().join("delayed.txt").exists());
    assert!(g1.absolute_root().exists());
}
#[cfg(unix)]
#[test]
fn damaged_generation_and_wrong_agent_cannot_enter_native_dispatch() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "launch").unwrap();
    let other = appoint(&mut f.v.f.store, &f.v.owner, "other-agent").unwrap();
    assert!(matches!(
        launch(&mut f, &other, &r, shell("touch should-not-run")),
        Err(HandoffError::Stale)
    ));
    std::fs::write(g.absolute_root().join("unexpected.txt"), b"changed").unwrap();
    assert!(matches!(
        launch(&mut f, &a, &r, shell("touch should-not-run")),
        Err(HandoffError::Generation(_))
    ));
    assert!(!g.absolute_root().join("should-not-run").exists());
    assert_eq!(show(&f.v.f.store, r.id()).unwrap().state, "prepared");
}
#[cfg(unix)]
#[test]
fn interrupted_dispatch_is_inspectable_and_cannot_relaunch_or_reappoint() {
    for at in [
        native::Stage::Intent,
        native::Stage::Spawned,
        native::Stage::Ready,
        native::Stage::Adopted,
    ] {
        let mut f = Fixture::new();
        let g = f.prepare("source").unwrap();
        let a = agent(&mut f);
        let r = request(&mut f, &a, &g, "once").unwrap();
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
        let result = native::dispatch_with(
            &mut f.v.f.store,
            &f.v.owner,
            &a,
            &mut f.broker,
            &f.carrier,
            &grant,
            &r,
            shell("touch should-not-run"),
            &mut |stage, _| {
                if stage == at {
                    Err(HandoffError::Unknown)
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(HandoffError::Unknown)));
        assert!(!g.absolute_root().join("should-not-run").exists());
        assert!(matches!(
            launch(&mut f, &a, &r, shell("touch should-not-run")),
            Err(HandoffError::Unknown)
        ));
        let view = show(&f.v.f.store, r.id()).unwrap();
        assert_eq!(
            view.state,
            if at == native::Stage::Adopted {
                "adopted"
            } else {
                "dispatching"
            }
        );
        drop(a);
        assert!(matches!(
            appoint(&mut f.v.f.store, &f.v.owner, "controlled-agent"),
            Err(HandoffError::Unknown)
        ));
        assert!(g.absolute_root().exists());
    }
}
#[cfg(unix)]
#[test]
fn acknowledgment_deadline_is_checked_after_ready_before_go() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "once").unwrap();
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
    let mut c = shell("touch should-not-run");
    c.acknowledgment_ms = 100;
    let result = native::dispatch_with(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut f.broker,
        &f.carrier,
        &grant,
        &r,
        c,
        &mut |stage, _| {
            if stage == native::Stage::Ready {
                std::thread::sleep(Duration::from_millis(120));
            }
            Ok(())
        },
    );
    assert!(matches!(result, Err(HandoffError::Deadline)));
    assert!(!g.absolute_root().join("should-not-run").exists());
    assert_eq!(show(&f.v.f.store, r.id()).unwrap().state, "dispatching");
}

#[test]
fn private_appointment_is_single_owner_and_prepared_offer_is_historical() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "offer").unwrap();
    assert!(matches!(
        appoint(&mut f.v.f.store, &f.v.owner, a.session()),
        Err(HandoffError::Busy)
    ));
    let view = show(&f.v.f.store, r.id()).unwrap();
    assert_eq!(view.request.source, g.intent().source);
    assert!(view.receipt.is_none());
    drop(a);
    let fresh = agent(&mut f);
    assert_ne!(fresh.nonce, view.request.agent_nonce);
    #[cfg(unix)]
    assert!(matches!(
        launch(&mut f, &fresh, &r, shell("touch should-not-run")),
        Err(HandoffError::Stale)
    ));
}
#[test]
fn native_command_limits_are_finite_and_request_unknown_fields_are_rejected() {
    let mut command = CommandSpec {
        executable: std::env::current_exe().unwrap().canonicalize().unwrap(),
        arguments: vec![],
        acknowledgment_ms: 1000,
    };
    command.acknowledgment_ms = 0;
    assert!(command.validate().is_err());
    command.acknowledgment_ms = 30_001;
    assert!(command.validate().is_err());
    command.acknowledgment_ms = 1;
    command.arguments = vec!["a".repeat(4097)];
    assert!(command.validate().is_err());
    assert!(serde_json::from_value::<CommandSpec>(serde_json::json!({"executable":"/bin/sh","arguments":[],"acknowledgment_ms":1000,"accepted":true})).is_err());
}

#[cfg(unix)]
#[test]
#[ignore = "owned child helper invoked by surviving_grandchild test"]
fn executor_parent_restart_helper() {
    let Some(checkpoint) = std::env::var_os("AETHYME_HANDOFF_HELPER_CHECKPOINT") else {
        return;
    };
    let mut f = Fixture::new();
    let g1 = f.prepare("old").unwrap();
    let g2 = f.prepare("new").unwrap();
    let a = agent(&mut f);
    let r1 = request(&mut f, &a, &g1, "old-command").unwrap();
    let mut old=launch(&mut f,&a,&r1,shell("exec 3>open-child.txt; (sleep 2; printf late >&3; cat .git/HEAD > watcher-read.tmp && mv watcher-read.tmp watcher-read.txt) >/dev/null 2>&1 & printf launched > child-launched.txt")).unwrap();
    assert_eq!(finish(&mut f, &a, &mut old), Completion::Current(0));
    assert!(!g1.absolute_root().join("watcher-read.txt").exists());
    drop(a);
    let fresh = agent(&mut f);
    let r2 = request(&mut f, &fresh, &g2, "new-command").unwrap();
    let mut new = launch(&mut f, &fresh, &r2, shell("printf new > new-command.txt")).unwrap();
    assert_eq!(finish(&mut f, &fresh, &mut new), Completion::Current(0));
    assert!(matches!(
        current_agent(&mut f.v.f.store, &f.v.owner, &fresh, &old),
        Err(HandoffError::Stale)
    ));
    std::fs::write(checkpoint,serde_json::to_vec(&serde_json::json!({"old":g1.absolute_root(),"new":g2.absolute_root(),"host":f.v.f._host.path()})).unwrap()).unwrap();
    // Actual parent exit: bypass Drop so its native descendant survives. Every
    // temporary fixture is inside the outer test's private TMPDIR, retained until
    // the late writer and watcher complete; no shared resource is touched.
    std::process::exit(0);
}
#[cfg(unix)]
#[test]
fn surviving_grandchild_keeps_old_descriptor_and_watcher_after_new_adoption_and_parent_exit() {
    let isolated = tempfile::tempdir().unwrap();
    let isolated_root = isolated.path().canonicalize().unwrap();
    let checkpoint = isolated_root.join("checkpoint.json");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "local_handoff::tests::executor_parent_restart_helper",
            "--exact",
            "--ignored",
            "--nocapture",
        ])
        .env("TMPDIR", &isolated_root)
        .env("TMP", &isolated_root)
        .env("TEMP", &isolated_root)
        .env("AETHYME_HANDOFF_HELPER_CHECKPOINT", &checkpoint)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(checkpoint).unwrap()).unwrap();
    let old = PathBuf::from(value["old"].as_str().unwrap());
    let new = PathBuf::from(value["new"].as_str().unwrap());
    assert!(old.starts_with(&isolated_root));
    assert!(new.starts_with(&isolated_root));
    assert_eq!(std::fs::read(new.join("new-command.txt")).unwrap(), b"new");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !old.join("watcher-read.txt").exists() {
        assert!(Instant::now() < deadline, "old watcher did not complete");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(std::fs::read(old.join("open-child.txt")).unwrap(), b"late");
    assert_eq!(
        std::fs::read(old.join("watcher-read.txt")).unwrap(),
        std::fs::read(old.join(".git/HEAD")).unwrap()
    );
    assert!(!new.join("open-child.txt").exists());
    assert!(!new.join("watcher-read.txt").exists());
    assert!(old.exists());
}
#[cfg(unix)]
#[test]
fn current_result_guard_rejects_changed_target_and_local_freeze() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "run").unwrap();
    let mut c = launch(&mut f, &a, &r, shell("true")).unwrap();
    assert_eq!(finish(&mut f, &a, &mut c), Completion::Current(0));
    current_agent(&mut f.v.f.store, &f.v.owner, &a, &c)
        .unwrap()
        .check_clock()
        .unwrap();
    f.v.f
        .store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='stop_requested' WHERE target=?1",
            [a.target.as_str()],
        )
        .unwrap();
    assert!(matches!(
        current_agent(&mut f.v.f.store, &f.v.owner, &a, &c),
        Err(HandoffError::Stale)
    ));
    f.v.f
        .store
        .connection()
        .execute(
            "UPDATE local_targets SET phase='verifying' WHERE target=?1",
            [a.target.as_str()],
        )
        .unwrap();
    current_agent(&mut f.v.f.store, &f.v.owner, &a, &c).unwrap();
    crate::local_authority::freeze(
        &mut f.v.f.store,
        f.v.owner.authority_epoch(),
        "handoff controlled restore fixture",
    )
    .unwrap();
    assert!(current_agent(&mut f.v.f.store, &f.v.owner, &a, &c).is_err());
}

#[cfg(unix)]
#[test]
fn external_freeze_waits_through_ready_adoption_and_go() {
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "run").unwrap();
    let host = f.v.f._host.path().to_owned();
    let epoch = f.v.owner.authority_epoch().clone();
    let (begun_tx, begun_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    let mut freeze_thread = None;
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
    let running = native::dispatch_with(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut f.broker,
        &f.carrier,
        &grant,
        &r,
        shell("true"),
        &mut |stage, _| {
            if stage == native::Stage::Ready {
                let host = host.clone();
                let epoch = epoch.clone();
                let begun = begun_tx.clone();
                let done = done_tx.clone();
                freeze_thread = Some(std::thread::spawn(move || {
                    let mut other = CollaborationStore::open(
                        &CollaborationRoot::under_host_state(&host),
                        &ProjectKey::parse("local-target-test").unwrap(),
                        &[],
                    )
                    .unwrap();
                    begun.send(()).unwrap();
                    let result = crate::local_authority::freeze(
                        &mut other,
                        &epoch,
                        "controlled handoff freeze race",
                    );
                    done.send(result.map(|_| ())).unwrap();
                }));
                begun_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                assert!(matches!(
                    done_rx.recv_timeout(Duration::from_millis(50)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ));
            }
            Ok(())
        },
    )
    .unwrap();
    done_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    freeze_thread.unwrap().join().unwrap();
    assert_eq!(show(&f.v.f.store, r.id()).unwrap().state, "adopted");
    assert!(current_agent(&mut f.v.f.store, &f.v.owner, &a, &running).is_err());
}

#[cfg(unix)]
#[test]
fn independent_adoption_vector_is_historical_and_binds_each_exact_subject() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("adoption-vector.json")).unwrap();
    let spec: AdoptionSpec = serde_json::from_value(vector["spec"].clone()).unwrap();
    let receipt = record(&ADOPTION, &spec).unwrap();
    assert_eq!(
        receipt.canonical_bytes(),
        vector["canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(receipt.id().as_str(), vector["id"].as_str().unwrap());
    for (key, changed) in [
        ("handoff", "sha256:".to_owned() + &"f".repeat(64)),
        ("dispatch", "sha256:".to_owned() + &"f".repeat(64)),
        ("generation", "sha256:".to_owned() + &"f".repeat(64)),
        ("source", "sha256:".to_owned() + &"f".repeat(64)),
        ("session", "different-agent".into()),
        ("agent_nonce", "different-incarnation".into()),
        ("epoch", "sha256:".to_owned() + &"f".repeat(64)),
        ("absolute_root", "/playground/other/source".into()),
    ] {
        let mut altered = vector["spec"].clone();
        altered[key] = serde_json::Value::String(changed);
        let altered: AdoptionSpec = serde_json::from_value(altered).unwrap();
        assert_ne!(record(&ADOPTION, &altered).unwrap().id(), receipt.id());
    }
    let mut altered = spec;
    altered.native_pid += 1;
    assert_ne!(record(&ADOPTION, &altered).unwrap().id(), receipt.id());
    // Decoding this fixture produces data only: dispatch requires a native
    // Agent, fresh QualifiedAcceptance and private PreparedHandoff.
}

#[cfg(unix)]
#[test]
fn explicit_retention_of_unknown_dispatch_allows_fresh_request_without_source_change() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "interrupted").unwrap();
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
    let result = native::dispatch_with(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut f.broker,
        &f.carrier,
        &grant,
        &r,
        shell("touch should-not-run"),
        &mut |stage, _| {
            if stage == native::Stage::Ready {
                Err(HandoffError::Unknown)
            } else {
                Ok(())
            }
        },
    );
    assert!(matches!(result, Err(HandoffError::Unknown)));
    assert!(matches!(
        retain_unknown_dispatch(
            &mut f.v.f.store,
            &f.v.owner,
            a.session(),
            r.id(),
            "keep old attachment unknown"
        ),
        Err(HandoffError::Busy)
    ));
    drop(a);
    assert!(matches!(
        appoint(&mut f.v.f.store, &f.v.owner, "controlled-agent"),
        Err(HandoffError::Unknown)
    ));
    let retained = retain_unknown_dispatch(
        &mut f.v.f.store,
        &f.v.owner,
        "controlled-agent",
        r.id(),
        "operator explicitly retains old attachment; no process death claimed",
    )
    .unwrap();
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM local_agent_reconciliations WHERE id=?1",
                [retained.as_str()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    let fresh = agent(&mut f);
    assert_ne!(fresh.nonce, r.spec.agent_nonce);
    assert!(matches!(
        launch(&mut f, &fresh, &r, shell("touch should-not-run")),
        Err(HandoffError::Stale)
    ));
    let next = request(&mut f, &fresh, &g, "fresh-operation").unwrap();
    assert_eq!(next.spec.source, r.spec.source);
    assert_eq!(next.spec.absolute_root, r.spec.absolute_root);
    let mut c = launch(
        &mut f,
        &fresh,
        &next,
        shell("printf recovered > recovered.txt"),
    )
    .unwrap();
    assert_eq!(finish(&mut f, &fresh, &mut c), Completion::Current(0));
    assert_eq!(
        std::fs::read(g.absolute_root().join("recovered.txt")).unwrap(),
        b"recovered"
    );
    assert_eq!(show(&f.v.f.store, r.id()).unwrap().state, "dispatching");
    assert!(g.absolute_root().exists());
}

#[cfg(unix)]
#[test]
fn current_result_guard_serializes_session_and_target_changes_until_consumed() {
    let mut f = Fixture::new();
    let g1 = f.prepare("first").unwrap();
    let g2 = f.prepare("second").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g1, "first").unwrap();
    let old = launch(&mut f, &a, &r, shell("true")).unwrap();
    let other = rusqlite::Connection::open(f.v.f.store.project_dir().join("state.db")).unwrap();
    other.busy_timeout(Duration::from_millis(10)).unwrap();
    let guard = current_agent(&mut f.v.f.store, &f.v.owner, &a, &old).unwrap();
    for sql in [
        "UPDATE local_agent_sessions SET phase='paused'",
        "UPDATE local_targets SET phase='stop_requested',version=version+1",
    ] {
        let error = other.execute(sql, []).unwrap_err();
        assert!(
            matches!(error,rusqlite::Error::SqliteFailure(ref e,_) if e.code==rusqlite::ErrorCode::DatabaseBusy)
        );
    }
    guard.check_clock().unwrap();
    drop(guard);
    retain_old_attachment(&mut f.v.f.store, &f.v.owner, &a, &old).unwrap();
    let next = request(&mut f, &a, &g2, "second").unwrap();
    let new = launch(&mut f, &a, &next, shell("true")).unwrap();
    assert!(matches!(
        current_agent(&mut f.v.f.store, &f.v.owner, &a, &old),
        Err(HandoffError::Stale)
    ));
    current_agent(&mut f.v.f.store, &f.v.owner, &a, &new)
        .unwrap()
        .check_clock()
        .unwrap();
    let version = target::status(&f.v.f.store, a.target.as_str())
        .unwrap()
        .version;
    target::transition(
        &mut f.v.f.store,
        &f.v.owner,
        version,
        target::Phase::StopRequested,
    )
    .unwrap();
    assert!(matches!(
        current_agent(&mut f.v.f.store, &f.v.owner, &a, &new),
        Err(HandoffError::Stale)
    ));
}

#[cfg(unix)]
#[test]
fn ready_or_adopted_target_stop_drift_and_resume_aba_never_send_go() {
    for (stage, next, aba) in [
        (native::Stage::Ready, target::Phase::StopRequested, false),
        (
            native::Stage::Ready,
            target::Phase::BlockedTargetDrift,
            false,
        ),
        (native::Stage::Ready, target::Phase::StopRequested, true),
        (native::Stage::Adopted, target::Phase::StopRequested, false),
    ] {
        let mut f = Fixture::new();
        let g = f.prepare("source").unwrap();
        let a = agent(&mut f);
        let r = request(&mut f, &a, &g, "run").unwrap();
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
        let result = native::dispatch_with(
            &mut f.v.f.store,
            &f.v.owner,
            &a,
            &mut f.broker,
            &f.carrier,
            &grant,
            &r,
            shell("touch should-not-run"),
            &mut |seen, store| {
                if seen == stage {
                    let version = target::status(store, a.target.as_str())?.version;
                    target::transition(store, &f.v.owner, version, next)?;
                    if aba {
                        store.connection().execute("UPDATE local_targets SET phase='verifying',version=version+1 WHERE target=?1",[a.target.as_str()])?;
                    }
                }
                Ok(())
            },
        );
        assert!(
            matches!(result, Err(HandoffError::Stale)),
            "stop/drift must refuse GO"
        );
        assert!(!g.absolute_root().join("should-not-run").exists());
        assert_eq!(
            show(&f.v.f.store, r.id()).unwrap().state,
            if stage == native::Stage::Adopted {
                "adopted"
            } else {
                "dispatching"
            }
        );
    }
}

#[cfg(unix)]
#[test]
fn expired_completed_leader_is_historical_but_does_not_strand_new_appointment() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
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
    let expiry = now().unwrap() + 5_000;
    let r = prepare(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut f.broker,
        &f.carrier,
        &grant,
        g.id(),
        "expires",
        expiry,
    )
    .unwrap();
    let mut old = launch(&mut f, &a, &r, shell("true")).unwrap();
    let delay = expiry - now().unwrap() + 1;
    if delay > 0 {
        std::thread::sleep(Duration::from_millis(delay as u64));
    }
    assert_eq!(finish(&mut f, &a, &mut old), Completion::Historical(0));
    assert!(matches!(
        current_agent(&mut f.v.f.store, &f.v.owner, &a, &old),
        Err(HandoffError::Stale)
    ));
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row(
                "SELECT state FROM local_agent_dispatches WHERE id=?1",
                [old.dispatch.as_str()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "leader_completed_old"
    );
    drop(a);
    let fresh = agent(&mut f);
    let next = request(&mut f, &fresh, &g, "fresh-after-expiry").unwrap();
    let mut new = launch(&mut f, &fresh, &next, shell("true")).unwrap();
    assert_eq!(finish(&mut f, &fresh, &mut new), Completion::Current(0));
    assert!(g.absolute_root().exists());
}

#[cfg(unix)]
#[test]
fn completed_leader_rechecks_real_target_stop_at_classification() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "run").unwrap();
    let mut c = launch(&mut f, &a, &r, shell("true")).unwrap();
    c.child.wait().unwrap();
    let result = poll_command_with(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut c,
        &mut |store| {
            let version = target::status(store, a.target.as_str())?.version;
            target::transition(store, &f.v.owner, version, target::Phase::StopRequested)?;
            Ok(())
        },
        &mut || Ok(()),
    )
    .unwrap();
    assert_eq!(result, Completion::Historical(0));
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row(
                "SELECT phase FROM local_agent_sessions WHERE target=?1 AND session=?2",
                params![a.target, a.session],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "paused"
    );
    assert!(g.absolute_root().exists());
}

#[cfg(unix)]
#[test]
fn expiry_while_completion_guard_is_held_cannot_commit_current_classification() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
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
    let expiry = now().unwrap() + 5_000;
    let r = prepare(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut f.broker,
        &f.carrier,
        &grant,
        g.id(),
        "expires-inside-completion",
        expiry,
    )
    .unwrap();
    let mut c = launch(&mut f, &a, &r, shell("true")).unwrap();
    c.child.wait().unwrap();
    let mut held = false;
    let result = poll_command_with(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut c,
        &mut |_| Ok(()),
        &mut || {
            held = true;
            let delay = expiry - now()? + 1;
            if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay as u64));
            }
            Ok(())
        },
    )
    .unwrap();
    assert!(held, "must acquire current guard before the injected delay");
    assert_eq!(result, Completion::Historical(0));
    assert_eq!(
        f.v.f
            .store
            .read_connection()
            .query_row(
                "SELECT state FROM local_agent_dispatches WHERE id=?1",
                [c.dispatch.as_str()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "leader_completed_old"
    );
    drop(a);
    agent(&mut f);
    assert!(g.absolute_root().exists());
}

#[cfg(unix)]
#[test]
fn go_deadline_is_rechecked_after_waiting_for_database_writer() {
    let mut f = Fixture::new();
    let g = f.prepare("source").unwrap();
    let a = agent(&mut f);
    let r = request(&mut f, &a, &g, "run").unwrap();
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
    let database = f.v.f.store.project_dir().join("state.db");
    let (locked_tx, locked_rx) = std::sync::mpsc::sync_channel(1);
    let mut writer = None;
    let mut command = shell("touch should-not-run");
    command.acknowledgment_ms = 1_000;
    let result = native::dispatch_with(
        &mut f.v.f.store,
        &f.v.owner,
        &a,
        &mut f.broker,
        &f.carrier,
        &grant,
        &r,
        command,
        &mut |stage, _| {
            if stage == native::Stage::Adopted {
                let database = database.clone();
                let locked = locked_tx.clone();
                writer = Some(std::thread::spawn(move || {
                    let conn = rusqlite::Connection::open(database).unwrap();
                    conn.execute_batch("BEGIN IMMEDIATE").unwrap();
                    locked.send(()).unwrap();
                    std::thread::sleep(Duration::from_millis(1_100));
                    conn.execute_batch("ROLLBACK").unwrap();
                }));
                locked_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            Ok(())
        },
    );
    writer
        .expect("must reach adopted journal before delaying final GO lock")
        .join()
        .unwrap();
    assert!(matches!(result, Err(HandoffError::Deadline)));
    assert!(!g.absolute_root().join("should-not-run").exists());
    assert_eq!(show(&f.v.f.store, r.id()).unwrap().state, "adopted");
}
