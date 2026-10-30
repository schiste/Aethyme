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
    for method in [
        r#"{"kind":"lookup","idempotency_key":"x","argv":[]}"#,
        r#"{"kind":"lookup","idempotency_key":"x","idempotency_key":"y"}"#,
        r#"{"kind":"apply","job_id":1}"#,
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

#[test]
fn public_cli_plans_gc_and_recovers_a_disconnected_submission_after_restart() {
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
    let runtime = runtime_directory();
    let path: PathBuf = runtime.path().canonicalize().unwrap();
    let child = start(&fixture, &path);
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
    // Close after writing a complete frame; never consume its acknowledgement.
    let mut stream = UnixStream::connect(path.join("broker-service.sock")).unwrap();
    protocol::write_frame(
        &mut stream,
        &wire::Request {
            protocol_version: wire::VERSION,
            request_id: 37,
            method: method.clone(),
        },
    )
    .unwrap();
    drop(stream);
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
    let lookup = cli_json(&fixture, &["lookup", path_text, "lost-reply", "--json"]);
    assert_eq!(lookup["result"]["Ok"]["job"]["id"], job_id);
    assert_eq!(lookup["result"]["Ok"]["job"]["state"], "succeeded");
    assert_eq!(cli_json(&fixture, &["result", path_text, &id_text]), plan);
}
