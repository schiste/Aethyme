#![cfg(unix)]

mod common;

use aethyme_broker::service::{
    job_protocol as wire,
    jobs::{JobOperation, JobState},
    protocol, transport,
};
use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn runtime_directory() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("sw")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

#[test]
fn repository_scope_preserves_legacy_frames_and_refuses_explicit_null() {
    let legacy = serde_json::json!({"protocol_version": 2, "request_id": 1,
        "method": {"kind": "lookup", "idempotency_key": "key"}});
    let decoded: wire::Request = serde_json::from_value(legacy.clone()).unwrap();
    assert!(decoded.repository_key.is_none());
    assert_eq!(serde_json::to_value(decoded).unwrap(), legacy);
    for invalid in [serde_json::Value::Null, serde_json::json!(42)] {
        let mut scoped = legacy.clone();
        scoped["repository_key"] = invalid;
        assert!(serde_json::from_value::<wire::Request>(scoped).is_err());
    }
    assert!(serde_json::from_str::<wire::Request>(
        r#"{"protocol_version":2,"request_id":1,"repository_key":"a","repository_key":"b","method":{"kind":"repository"}}"#
    ).is_err());
    let root = runtime_directory();
    assert_eq!(
        transport::scoped_job_request(root.path(), "invalid", wire::Method::Repository {})
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidInput
    );
}

#[test]
fn v2_nullable_fields_require_keys_and_accept_explicit_nulls() {
    let status = serde_json::json!({
        "id": 1,
        "operation": {"kind": "gc_plan", "include_active_gate_cache": false, "full_scan": false},
        "state": "queued", "deadline_ms": 100, "created_at_ms": 1, "updated_at_ms": 1,
        "cancellation_requested": false, "progress": null, "completion": null
    });
    for field in ["progress", "completion"] {
        let mut missing = status.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<wire::Status>(missing).is_err(),
            "accepted missing {field}"
        );
    }
    assert!(serde_json::from_value::<wire::Status>(status.clone()).is_ok());
    for total in [serde_json::Value::Null, serde_json::json!(10)] {
        let mut populated = status.clone();
        populated["progress"] =
            serde_json::json!({"completed": 0, "total": total, "message": "planning"});
        populated["completion"] = serde_json::json!({"kind": "succeeded"});
        let decoded: wire::Status = serde_json::from_value(populated.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), populated);
        populated["progress"]
            .as_object_mut()
            .unwrap()
            .remove("total");
        assert!(serde_json::from_value::<wire::Status>(populated).is_err());
    }
    assert!(serde_json::from_str::<wire::Reply>(r#"{"kind":"lookup"}"#).is_err());
    assert_eq!(
        serde_json::from_str::<wire::Reply>(r#"{"kind":"lookup","job":null}"#).unwrap(),
        wire::Reply::Lookup { job: None }
    );
    assert!(
        serde_json::from_value::<wire::Reply>(serde_json::json!({"kind":"lookup", "job":status}))
            .is_ok()
    );
}

#[test]
fn empty_v2_variants_refuse_extra_fields_without_changing_valid_json() {
    let repository = serde_json::json!({"kind": "repository"});
    let method: wire::Method = serde_json::from_value(repository.clone()).unwrap();
    assert_eq!(serde_json::to_value(method).unwrap(), repository);
    assert!(
        serde_json::from_value::<wire::Method>(
            serde_json::json!({"kind": "repository", "extra": null})
        )
        .is_err()
    );
    for kind in ["succeeded", "cancelled"] {
        let value = serde_json::json!({"kind": kind});
        let completion: wire::Completion = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(serde_json::to_value(completion).unwrap(), value);
        assert!(
            serde_json::from_value::<wire::Completion>(
                serde_json::json!({"kind": kind, "extra": null})
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<wire::Completion>(&format!(
                r#"{{"kind":"{kind}","extra":1,"extra":2}}"#
            ))
            .is_err()
        );
    }
}

#[test]
fn result_chunks_preserve_utf8_and_bound_even_maximally_escaped_frames() {
    for encoded in ["é🎉".repeat(10000), "\u{0000}".repeat(10000)] {
        let mut reconstructed = String::new();
        loop {
            let reply = wire::result_chunk(7, &encoded, reconstructed.len()).unwrap();
            let mut frame = Vec::new();
            protocol::write_frame(
                &mut frame,
                &wire::Response {
                    protocol_version: wire::VERSION,
                    request_id: 11,
                    result: Ok(reply.clone()),
                },
            )
            .unwrap();
            assert!(frame.len() <= protocol::MAX_FRAME_BYTES);
            let wire::Reply::ResultChunk {
                offset,
                next_offset,
                data,
                complete,
                ..
            } = reply
            else {
                panic!("wrong chunk reply");
            };
            assert_eq!(offset, reconstructed.len());
            reconstructed.push_str(&data);
            assert_eq!(next_offset, reconstructed.len());
            if complete {
                break;
            }
        }
        assert_eq!(reconstructed, encoded);
    }
    assert!(wire::result_chunk(1, "é", 1).is_err());
    assert!(wire::result_chunk(1, "é", 3).is_err());
}

#[test]
fn health_only_service_refuses_jobs_and_strictly_decodes_v2_requests() {
    let root = runtime_directory();
    let path = root.path().canonicalize().unwrap();
    let server = transport::Server::bind(&path).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let worker = thread::spawn(move || server.serve_until(&thread_stop).unwrap());
    // Always stop the server even if an assertion fails.
    struct Stop(Arc<AtomicBool>, Option<thread::JoinHandle<()>>);
    impl Drop for Stop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
            self.1.take().unwrap().join().unwrap();
        }
    }
    let _running = Stop(stop, Some(worker));
    let reply = transport::job_request(
        &path,
        wire::Method::Lookup {
            idempotency_key: "absent".into(),
        },
    )
    .unwrap();
    assert_eq!(
        reply.result.unwrap_err().code,
        wire::ErrorCode::JobsDisabled
    );
    assert_eq!(
        transport::job_request(&path, wire::Method::Repository {})
            .unwrap()
            .result
            .unwrap_err()
            .code,
        wire::ErrorCode::JobsDisabled
    );
    for method in [
        r#"{"kind":"lookup","idempotency_key":"x","argv":[]}"#,
        r#"{"kind":"lookup","idempotency_key":"x","idempotency_key":"y"}"#,
        r#"{"kind":"apply","job_id":1}"#,
        r#"{"kind":"repository","argv":[]}"#,
    ] {
        let mut stream = UnixStream::connect(path.join("broker-service.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        writeln!(
            stream,
            "{{\"protocol_version\":2,\"request_id\":12,\"method\":{method}}}"
        )
        .unwrap();
        let reply: wire::Response = protocol::read_frame(&mut BufReader::new(stream)).unwrap();
        assert_eq!(reply.request_id, 12);
        assert_eq!(
            reply.result.unwrap_err().code,
            wire::ErrorCode::InvalidRequest
        );
    }
    assert!(transport::ping(&path).unwrap().result.is_ok());
    assert!(!path.join("broker-service-jobs.sqlite").exists());
    assert!(!path.join(".aethyme").exists());
}

#[test]
fn job_client_refuses_mismatched_reply_identity() {
    for (version, request_id) in [(3, 1), (2, 99)] {
        let root = runtime_directory();
        let path = root.path().canonicalize().unwrap();
        let socket = path.join("broker-service.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let worker = thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error)
                        if error.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("fake service accept: {error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let _: wire::Request = protocol::read_frame(&mut BufReader::new(&mut stream)).unwrap();
            protocol::write_frame(
                &mut stream,
                &wire::Response {
                    protocol_version: version,
                    request_id,
                    result: Ok(wire::Reply::Lookup { job: None }),
                },
            )
            .unwrap();
        });
        let result = transport::job_request(
            &path,
            wire::Method::Lookup {
                idempotency_key: "x".into(),
            },
        );
        worker.join().unwrap();
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }
}

fn isolated(command: &mut Command, fixture: &common::FixtureRepository) {
    common::isolate_host_paths(command, fixture.path());
    for (name, _) in
        std::env::vars_os().filter(|(name, _)| name.to_string_lossy().starts_with("GIT_"))
    {
        command.env_remove(name);
    }
    command
        .current_dir(fixture.path())
        .env("HOME", fixture.owner_path())
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "AETHYME_WORKTREE_ROOT",
            fixture.owner_path().join("worktrees"),
        );
}
fn shim(fixture: &common::FixtureRepository) -> Command {
    let mut command = common::broker_cli_for_fixture(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["advanced", "service"],
        fixture.path(),
    );
    isolated(&mut command, fixture);
    command
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Err(error) = self.0.kill() {
            eprintln!("service test kill: {error}");
        }
        if let Err(error) = self.0.wait() {
            eprintln!("service test reap: {error}");
        }
    }
}
fn start(fixture: &common::FixtureRepository, runtime: &Path) -> OwnedChild {
    let mut child = OwnedChild(
        shim(fixture)
            .arg("serve")
            .arg(runtime)
            .arg("--jobs-for")
            .arg(fixture.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if transport::ping(runtime).is_ok() {
            return child;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "service exited before readiness"
        );
        assert!(Instant::now() < deadline, "service readiness expired");
        thread::sleep(Duration::from_millis(20));
    }
}
fn cli_json(fixture: &common::FixtureRepository, args: &[&str]) -> serde_json::Value {
    let output = shim(fixture).args(args).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn planning_fixture() -> common::FixtureRepository {
    let fixture = common::FixtureRepository::new();
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "commit",
            "--allow-empty",
            "-qm",
            "fixture",
        ],
    ] {
        let mut git = Command::new("git");
        isolated(&mut git, &fixture);
        let output = git.args(args).output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let mut broker = common::open_fixture_repository_broker(&fixture).unwrap();
    broker.start_worktree("service fixture", None).unwrap();
    drop(broker);
    fixture
}

fn discover(
    fixture: &common::FixtureRepository,
    root: &Path,
    repository: &Path,
) -> std::process::Output {
    shim(fixture)
        .arg("directory")
        .arg(root)
        .arg("--jobs-for")
        .arg(repository)
        .arg("--json")
        .output()
        .unwrap()
}

#[test]
fn discovery_unifies_linked_worktrees_and_separates_repositories_without_creating_state() {
    let fixture = planning_fixture();
    let other = planning_fixture();
    let root = runtime_directory();
    let root = root.path().canonicalize().unwrap();
    fs::write(root.join("sentinel"), "preserve").unwrap();
    let linked = fixture.owner_path().join("discovery-linked");
    let mut git = Command::new("git");
    isolated(&mut git, &fixture);
    let output = git
        .args(["worktree", "add", "--detach", "-q"])
        .arg(&linked)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let resolve = |repository: &Path| {
        let output = discover(&fixture, &root, repository);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    let main = resolve(fixture.path());
    assert_eq!(resolve(&linked), main);
    let different = resolve(other.path());
    assert_ne!(different["repository"]["key"], main["repository"]["key"]);
    assert_ne!(different["directory"], main["directory"]);
    assert_eq!(main["schema_version"], 1);
    assert_eq!(main["repository"]["key"].as_str().unwrap().len(), 64);
    let directory = Path::new(main["directory"].as_str().unwrap());
    assert_eq!(directory.parent(), Some(root.as_path()));
    assert_eq!(directory.file_name().unwrap().to_str().unwrap().len(), 32);
    assert!(!directory.exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);
    assert_eq!(fs::read(root.join("sentinel")).unwrap(), b"preserve");
}

#[test]
fn discovery_refuses_unsafe_roots_and_leaves_an_uninitialized_repository_untouched() {
    let fixture = planning_fixture();
    let root = runtime_directory();
    let root = root.path().canonicalize().unwrap();
    let missing = root.join("missing");
    assert!(
        !discover(&fixture, &missing, fixture.path())
            .status
            .success()
    );
    assert!(!missing.exists());
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    assert!(!discover(&fixture, &alias, fixture.path()).status.success());
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!discover(&fixture, &root, fixture.path()).status.success());
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o755
    );
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

    let untouched = common::FixtureRepository::new();
    let mut git = Command::new("git");
    isolated(&mut git, &untouched);
    assert!(git.args(["init", "-q"]).status().unwrap().success());
    assert!(
        !discover(&untouched, &root, untouched.path())
            .status
            .success()
    );
    assert!(!untouched.path().join(".aethyme").exists());
}

#[test]
fn discovery_validates_existing_leaf_and_socket_path_without_repairing_them() {
    let fixture = planning_fixture();
    let root = runtime_directory();
    let root = root.path().canonicalize().unwrap();
    let output = discover(&fixture, &root, fixture.path());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let leaf = Path::new(value["directory"].as_str().unwrap());
    fs::write(leaf, "preserve").unwrap();
    assert!(!discover(&fixture, &root, fixture.path()).status.success());
    assert_eq!(fs::read(leaf).unwrap(), b"preserve");
    fs::remove_file(leaf).unwrap();
    std::os::unix::fs::symlink(&root, leaf).unwrap();
    assert!(!discover(&fixture, &root, fixture.path()).status.success());
    assert_eq!(fs::read_link(leaf).unwrap(), root);
    fs::remove_file(leaf).unwrap();
    fs::create_dir(leaf).unwrap();
    fs::set_permissions(leaf, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(!discover(&fixture, &root, fixture.path()).status.success());
    fs::set_permissions(leaf, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(discover(&fixture, &root, fixture.path()).status.success());
    assert_eq!(fs::read_dir(leaf).unwrap().count(), 0);

    let too_long = root.join("x".repeat(100));
    fs::create_dir(&too_long).unwrap();
    fs::set_permissions(&too_long, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        !discover(&fixture, &too_long, fixture.path())
            .status
            .success()
    );
    assert_eq!(fs::read_dir(&too_long).unwrap().count(), 0);
}

fn ensure_command(fixture: &common::FixtureRepository, root: &Path) -> Command {
    let mut command = shim(fixture);
    command
        .arg("ensure")
        .arg(root)
        .arg("--jobs-for")
        .arg(fixture.path())
        .arg("--json");
    command
}

struct OwnedEnsuredService(aethyme_broker::environments::identity::ProcessIdentity);

impl Drop for OwnedEnsuredService {
    fn drop(&mut self) {
        use aethyme_broker::environments::identity::Observation;
        if self.0.observe() == Observation::Alive {
            // SAFETY: signal only the exact process identity created by this test.
            assert_eq!(unsafe { libc::kill(self.0.pid, libc::SIGTERM) }, 0);
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.0.observe() == Observation::Alive && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            self.0.observe(),
            Observation::Gone,
            "owned service exit could not be proved"
        );
    }
}

#[test]
fn concurrent_ensure_starts_one_service_and_reuses_its_repository_identity() {
    let fixture = planning_fixture();
    let root = runtime_directory();
    let root = root.path().canonicalize().unwrap();
    let children: Vec<_> = (0..4)
        .map(|_| {
            ensure_command(&fixture, &root)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let outputs: Vec<_> = children
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    let successful: Vec<serde_json::Value> = outputs
        .iter()
        .filter(|o| o.status.success())
        .map(|o| serde_json::from_slice(&o.stdout).unwrap())
        .collect();
    let owned = successful.first().map(|value| {
        OwnedEnsuredService(
            aethyme_broker::environments::identity::ProcessIdentity::capture(
                value["pid"].as_i64().unwrap() as i32,
            )
            .expect("capture our newly started service identity"),
        )
    });
    for output in &outputs {
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(
        successful.iter().filter(|v| v["started"] == true).count(),
        1
    );
    let first = &successful[0];
    for value in &successful {
        assert_eq!(value["pid"], first["pid"]);
        assert_eq!(value["repository"], first["repository"]);
        assert_eq!(value["directory"], first["directory"]);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(value.as_object().unwrap().len(), 6);
    }
    let repeated = ensure_command(&fixture, &root).output().unwrap();
    assert!(
        repeated.status.success(),
        "{}",
        String::from_utf8_lossy(&repeated.stderr)
    );
    let repeated: serde_json::Value = serde_json::from_slice(&repeated.stdout).unwrap();
    assert_eq!(repeated["pid"], first["pid"]);
    assert_eq!(repeated["started"], false);
    let directory = Path::new(first["directory"].as_str().unwrap());
    assert!(directory.join("broker-service.lock").exists());
    assert!(directory.join("broker-service-startup.lock").exists());
    let deadline_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 120000;
    let plan = gc_plan_command(&fixture)
        .arg("--service-directory")
        .arg(directory)
        .args([
            "--request-key",
            "ensured-plan",
            "--deadline-ms",
            &deadline_ms.to_string(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&plan.stdout)
            .unwrap()
            .is_object()
    );
    let before = cli_json(
        &fixture,
        &["lookup", directory.to_str().unwrap(), "ensured-plan"],
    );
    drop(owned);
    let restarted = ensure_command(&fixture, &root).output().unwrap();
    assert!(
        restarted.status.success(),
        "{}",
        String::from_utf8_lossy(&restarted.stderr)
    );
    let restarted: serde_json::Value = serde_json::from_slice(&restarted.stdout).unwrap();
    let _restarted = OwnedEnsuredService(
        aethyme_broker::environments::identity::ProcessIdentity::capture(
            restarted["pid"].as_i64().unwrap() as i32,
        )
        .expect("capture our restarted service identity"),
    );
    assert_eq!(restarted["started"], true);
    assert_eq!(
        cli_json(
            &fixture,
            &["lookup", directory.to_str().unwrap(), "ensured-plan"]
        ),
        before
    );
}

#[test]
fn ensure_preserves_a_live_service_for_another_repository() {
    let fixture = planning_fixture();
    let other = planning_fixture();
    let root = runtime_directory();
    let root = root.path().canonicalize().unwrap();
    let discovered = discover(&fixture, &root, fixture.path());
    assert!(discovered.status.success());
    let discovered: serde_json::Value = serde_json::from_slice(&discovered.stdout).unwrap();
    let directory = Path::new(discovered["directory"].as_str().unwrap());
    fs::create_dir(directory).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    let _other_service = start(&other, directory);
    let before = transport::ping(directory).unwrap().result.unwrap();
    let output = ensure_command(&fixture, &root).output().unwrap();
    assert!(!output.status.success());
    assert_eq!(transport::ping(directory).unwrap().result.unwrap(), before);
    assert!(!directory.join("broker-service-startup.log").exists());
}

#[test]
fn ensure_refuses_unsafe_coordination_files_without_spawning_or_repair() {
    let fixture = planning_fixture();
    let root = runtime_directory();
    let root = root.path().canonicalize().unwrap();
    let discovered = discover(&fixture, &root, fixture.path());
    assert!(discovered.status.success());
    let discovered: serde_json::Value = serde_json::from_slice(&discovered.stdout).unwrap();
    let directory = Path::new(discovered["directory"].as_str().unwrap());
    fs::create_dir(directory).unwrap();
    fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).unwrap();
    let sentinel = root.join("sentinel");
    fs::write(&sentinel, "preserve").unwrap();
    let startup_lock = directory.join("broker-service-startup.lock");
    std::os::unix::fs::symlink(&sentinel, &startup_lock).unwrap();
    assert!(
        !ensure_command(&fixture, &root)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
    assert!(startup_lock.is_symlink());
    fs::remove_file(&startup_lock).unwrap();
    fs::write(&startup_lock, "private lock").unwrap();
    fs::set_permissions(&startup_lock, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        !ensure_command(&fixture, &root)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::metadata(&startup_lock).unwrap().permissions().mode() & 0o777,
        0o644
    );
    fs::set_permissions(&startup_lock, fs::Permissions::from_mode(0o600)).unwrap();
    let startup_log = directory.join("broker-service-startup.log");
    std::os::unix::fs::symlink(&sentinel, &startup_log).unwrap();
    assert!(
        !ensure_command(&fixture, &root)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(startup_log.is_symlink());
    assert_eq!(fs::read(&sentinel).unwrap(), b"preserve");
    assert!(!directory.join("broker-service.sock").exists());
    assert!(!directory.join("broker-service-jobs.sqlite").exists());
}

#[test]
fn ensure_invalid_arguments_and_uninitialized_owner_create_no_service_state() {
    let fixture = common::FixtureRepository::new();
    let root = runtime_directory();
    let root = root.path().canonicalize().unwrap();
    let mut git = Command::new("git");
    isolated(&mut git, &fixture);
    assert!(git.args(["init", "-q"]).status().unwrap().success());
    for extra in [
        vec![],
        vec!["--jobs-for"],
        vec!["--jobs-for", ".", "--json", "--json"],
        vec!["--jobs-for", ".", "--unknown"],
    ] {
        let output = shim(&fixture)
            .arg("ensure")
            .arg(&root)
            .args(extra)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(!fixture.path().join(".aethyme").exists());
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    }
    let output = ensure_command(&fixture, &root).output().unwrap();
    assert!(!output.status.success());
    assert!(!fixture.path().join(".aethyme").exists());
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
}

fn gc_plan_command(fixture: &common::FixtureRepository) -> Command {
    let mut command = common::broker_cli_for_fixture(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["gc", "plan"],
        fixture.path(),
    );
    isolated(&mut command, fixture);
    command
}

#[test]
fn opted_in_gc_plan_uses_the_verified_service_and_replays_the_same_json() {
    let fixture = planning_fixture();
    let runtime = runtime_directory();
    let path = runtime.path().canonicalize().unwrap();
    let _child = start(&fixture, &path);
    let deadline = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 120_000)
        .to_string();
    let invoke = || {
        gc_plan_command(&fixture)
            .args([
                "--service-directory",
                path.to_str().unwrap(),
                "--request-key",
                "gc-plan-cli",
                "--deadline-ms",
                &deadline,
                "--json",
            ])
            .output()
            .unwrap()
    };
    let first = invoke();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert!(plan["schema_version"].is_number());
    assert!(plan["digest"].is_string());
    assert!(plan.get("result").is_none());
    let lookup = cli_json(
        &fixture,
        &["lookup", path.to_str().unwrap(), "gc-plan-cli", "--json"],
    );
    let job_id = lookup["result"]["Ok"]["job"]["id"]
        .as_i64()
        .unwrap()
        .to_string();
    let stored = cli_json(
        &fixture,
        &["result", path.to_str().unwrap(), &job_id, "--json"],
    );
    assert_eq!(plan, stored);
    let second = invoke();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&second.stdout).unwrap(),
        plan
    );
    assert_eq!(
        cli_json(
            &fixture,
            &["lookup", path.to_str().unwrap(), "gc-plan-cli", "--json"]
        ),
        lookup
    );
}

#[test]
fn opted_in_gc_plan_refuses_another_repository_without_accepting_work() {
    let owner = planning_fixture();
    let other = planning_fixture();
    let runtime = runtime_directory();
    let path = runtime.path().canonicalize().unwrap();
    let _child = start(&owner, &path);
    let deadline = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 120_000)
        .to_string();
    let output = gc_plan_command(&other)
        .args([
            "--service-directory",
            path.to_str().unwrap(),
            "--request-key",
            "wrong-repository-sentinel",
            "--deadline-ms",
            &deadline,
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        transport::job_request(
            &path,
            wire::Method::Lookup {
                idempotency_key: "wrong-repository-sentinel".into(),
            }
        )
        .unwrap()
        .result
        .unwrap(),
        wire::Reply::Lookup { job: None }
    );
    assert!(!output.status.success());
    let failure: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        failure.to_string().contains("RepositoryMismatch"),
        "{failure}"
    );
}

#[test]
fn invalid_or_unavailable_shared_gc_never_initializes_a_local_broker() {
    let fixture = common::FixtureRepository::new();
    let runtime = runtime_directory();
    let missing = runtime.path().canonicalize().unwrap().join("missing");
    let deadline = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        + 120_000)
        .to_string();
    for action in [
        "apply",
        "abandon",
        "forget-abandoned",
        "sweep",
        "__continue-artifacts",
        "storage",
    ] {
        for (flag, value) in [
            ("--service-directory", missing.to_str().unwrap()),
            ("--request-key", "key"),
            ("--deadline-ms", deadline.as_str()),
        ] {
            let mut command = common::broker_cli_for_fixture(
                env!("CARGO_BIN_EXE_broker-cli-shim"),
                &["gc", action],
                fixture.path(),
            );
            isolated(&mut command, &fixture);
            let output = command.args([flag, value]).output().unwrap();
            assert!(!output.status.success(), "gc {action} accepted {flag}");
            let error = String::from_utf8_lossy(&output.stderr);
            assert!(
                error.contains("is not valid"),
                "gc {action} {flag}: {error}"
            );
            assert!(!fixture.path().join(".aethyme/broker.db").exists());
            assert!(!missing.exists());
        }
    }
    // Unknown GC actions reach the handler rather than an action-specific flag rule.
    let mut unknown = common::broker_cli_for_fixture(
        env!("CARGO_BIN_EXE_broker-cli-shim"),
        &["gc", "not-an-action"],
        fixture.path(),
    );
    isolated(&mut unknown, &fixture);
    let output = unknown.args(["--request-key", "key"]).output().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("require gc plan"), "{error}");
    assert!(!fixture.path().join(".aethyme/broker.db").exists());
    assert!(!missing.exists());
    for args in [
        vec!["--service-directory", missing.to_str().unwrap(), "--json"],
        vec!["--request-key", "key", "--deadline-ms", &deadline, "--json"],
        vec![
            "--service-directory",
            missing.to_str().unwrap(),
            "--request-key",
            "key",
            "--deadline-ms",
            "0",
            "--json",
        ],
        vec![
            "--service-directory",
            missing.to_str().unwrap(),
            "--request-key",
            "key",
            "--deadline-ms",
            &deadline,
            "--json",
        ],
    ] {
        let output = gc_plan_command(&fixture).args(args).output().unwrap();
        assert!(!output.status.success());
        assert!(!fixture.path().join(".aethyme/broker.db").exists());
        assert!(!missing.exists());
    }
    let initialized = planning_fixture();
    let output = gc_plan_command(&initialized)
        .args([
            "--service-directory",
            missing.to_str().unwrap(),
            "--request-key",
            "unavailable",
            "--deadline-ms",
            &deadline,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "unavailable service must not fall back to a successful local plan"
    );
    assert!(!missing.exists());
}

#[test]
fn public_cli_plans_gc_and_recovers_a_disconnected_submission_after_restart() {
    let fixture = planning_fixture();
    let runtime = runtime_directory();
    let path: PathBuf = runtime.path().canonicalize().unwrap();
    let child = start(&fixture, &path);
    let repository: wire::Response = serde_json::from_value(cli_json(
        &fixture,
        &["repository", path.to_str().unwrap(), "--json"],
    ))
    .unwrap();
    let wire::Reply::Repository { identity } = repository.result.unwrap() else {
        panic!("expected repository identity reply");
    };
    assert_eq!(identity.root, fixture.path().canonicalize().unwrap());
    assert_eq!(
        identity.common_dir,
        fixture.path().join(".git").canonicalize().unwrap()
    );
    assert!(!identity.repository_uuid.is_empty());
    assert_eq!(identity.key.len(), 64);
    let mut wrong_key = identity.key.clone();
    wrong_key.replace_range(
        ..1,
        if identity.key.starts_with('0') {
            "1"
        } else {
            "0"
        },
    );
    let deadline = i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
        + 120000;
    let method = wire::Method::Submit {
        idempotency_key: "lost-reply".into(),
        deadline_ms: deadline,
        operation: JobOperation::GcPlan {
            include_active_gate_cache: false,
            full_scan: false,
        },
    };
    // The real transport must reject the wrong repository before even a
    // submit is accepted. Exercise every operation through the same fence.
    let mut refusals = Vec::new();
    for operation in [
        method.clone(),
        wire::Method::Repository {},
        wire::Method::Lookup {
            idempotency_key: "lost-reply".into(),
        },
        wire::Method::Status { job_id: 1 },
        wire::Method::Cancel { job_id: 1 },
        wire::Method::ResultChunk {
            job_id: 1,
            offset: 0,
        },
    ] {
        let context = format!("wrong-owner {operation:?}");
        refusals.push(
            transport::scoped_job_request(&path, &wrong_key, operation)
                .unwrap_or_else(|error| panic!("{context}: {error:?}"))
                .result,
        );
    }
    assert_eq!(
        transport::scoped_job_request(
            &path,
            &identity.key,
            wire::Method::Lookup {
                idempotency_key: "lost-reply".into()
            }
        )
        .unwrap()
        .result
        .unwrap(),
        wire::Reply::Lookup { job: None }
    );
    for refusal in refusals {
        assert_eq!(
            refusal.unwrap_err().code,
            wire::ErrorCode::RepositoryMismatch
        );
    }
    // Never consume the acknowledgement. Keep the peer open until lookup
    // proves durable acceptance: writing bytes alone does not prove acceptance,
    // and an already disconnected peer may be refused before frame dispatch.
    let mut stream = UnixStream::connect(path.join("broker-service.sock")).unwrap();
    protocol::write_frame(
        &mut stream,
        &wire::Request {
            protocol_version: wire::VERSION,
            request_id: 37,
            repository_key: Some(identity.key.clone()),
            method: method.clone(),
        },
    )
    .unwrap();
    let wait_deadline = Instant::now() + Duration::from_secs(20);
    let job_id = loop {
        let reply = transport::job_request(
            &path,
            wire::Method::Lookup {
                idempotency_key: "lost-reply".into(),
            },
        )
        .unwrap()
        .result
        .unwrap();
        if let wire::Reply::Lookup { job: Some(job) } = reply {
            break job.id;
        }
        assert!(
            Instant::now() < wait_deadline,
            "accepted request was not discoverable"
        );
        thread::sleep(Duration::from_millis(20));
    };
    drop(stream);
    loop {
        let reply = transport::job_request(&path, wire::Method::Status { job_id })
            .unwrap()
            .result
            .unwrap();
        if let wire::Reply::Status { job } = reply {
            if job.state == JobState::Succeeded {
                break;
            }
            assert!(
                matches!(job.state, JobState::Queued | JobState::Running),
                "{job:?}"
            );
        }
        assert!(Instant::now() < wait_deadline, "GC plan did not finish");
        thread::sleep(Duration::from_millis(20));
    }
    let path_text = path.to_str().unwrap();
    let id_text = job_id.to_string();
    let plan = cli_json(&fixture, &["result", path_text, &id_text, "--json"]);
    assert!(
        plan["worktrees"].is_array(),
        "expected complete GC plan: {plan}"
    );
    let retried = cli_json(
        &fixture,
        &[
            "submit",
            path_text,
            "--request-key",
            "lost-reply",
            "--deadline-ms",
            &deadline.to_string(),
            "--json",
        ],
    );
    assert_eq!(retried["result"]["Ok"]["job"]["id"], job_id);
    assert_eq!(retried["result"]["Ok"]["joined"], true);
    drop(child);
    let _restarted = start(&fixture, &path);
    assert_eq!(
        transport::scoped_job_request(&path, &identity.key, wire::Method::Repository {})
            .unwrap()
            .result
            .unwrap(),
        wire::Reply::Repository { identity }
    );
    let lookup = cli_json(&fixture, &["lookup", path_text, "lost-reply", "--json"]);
    assert_eq!(lookup["result"]["Ok"]["job"]["id"], job_id);
    assert_eq!(lookup["result"]["Ok"]["job"]["state"], "succeeded");
    assert_eq!(cli_json(&fixture, &["result", path_text, &id_text]), plan);
}
