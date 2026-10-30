#![cfg(unix)]

use aethyme_broker::service::{protocol::*, transport::*};
use std::fs;
use std::io::{BufReader, Write};
use std::os::unix::fs::{PermissionsExt, symlink};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};

fn directory() -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix("bs")
        .tempdir_in("/tmp")
        .unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    root
}

struct Running {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
impl Running {
    fn start(path: &Path) -> Self {
        let server = Server::bind(path).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = thread::spawn(move || server.serve_until(&worker_stop).unwrap());
        Self {
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[test]
fn health_roundtrip_and_orderly_socket_cleanup() {
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    let running = Running::start(&path);
    let response = ping(&path).unwrap();
    assert_eq!(response.result.unwrap().pid, std::process::id());
    assert!(!path.join(".aethyme").exists());
    drop(running);
    assert!(!path.join("broker-service.sock").exists());
    assert!(path.join("broker-service.lock").exists());
    assert!(ping(&path).is_err());
}

#[test]
fn singleton_claim_refuses_second_server() {
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    let first = Server::bind(&path).unwrap();
    assert!(Server::bind(&path).is_err());
    drop(first);
    Server::bind(&path).unwrap();
}

#[test]
fn stale_socket_recovers_but_regular_file_is_preserved() {
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    let socket = path.join("broker-service.sock");
    drop(UnixListener::bind(&socket).unwrap());
    drop(Server::bind(&path).unwrap());
    fs::write(&socket, "keep").unwrap();
    assert!(Server::bind(&path).is_err());
    assert_eq!(fs::read_to_string(&socket).unwrap(), "keep");
}

#[test]
fn permissive_directory_and_symlinks_are_refused() {
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Server::bind(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
    let target = path.join("keep");
    fs::write(&target, "keep").unwrap();
    symlink(&target, path.join("broker-service.lock")).unwrap();
    assert!(Server::bind(&path).is_err());
    assert_eq!(fs::read_to_string(target).unwrap(), "keep");
}

#[test]
fn drop_preserves_replaced_socket_path() {
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    let server = Server::bind(&path).unwrap();
    let socket = path.join("broker-service.sock");
    fs::remove_file(&socket).unwrap();
    fs::write(&socket, "replacement").unwrap();
    drop(server);
    assert_eq!(fs::read_to_string(socket).unwrap(), "replacement");
}

#[test]
fn incomplete_client_does_not_block_health_or_incompatible_version_reply() {
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    let _running = Running::start(&path);
    let socket = path.join("broker-service.sock");
    let mut slow = UnixStream::connect(&socket).unwrap();
    slow.write_all(b"{").unwrap();
    assert!(ping(&path).unwrap().result.is_ok());
    let mut client = UnixStream::connect(&socket).unwrap();
    client
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    write_frame(
        &mut client,
        &Request {
            protocol_version: 2,
            ..Request::ping(9)
        },
    )
    .unwrap();
    let response: Response = read_frame(&mut BufReader::new(client)).unwrap();
    assert_eq!(response.request_id, 9);
    assert_eq!(
        response.result.unwrap_err().code,
        ErrorCode::UnsupportedProtocol
    );
}

fn shim(cwd: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_broker-cli-shim"));
    command
        .current_dir(cwd)
        .env("HOME", cwd)
        .env("AETHYME_HOST_STATE_DIR", cwd.join("state"));
    command.args(["broker", "advanced", "service"]);
    command
}

#[test]
fn public_cli_help_and_missing_service_do_not_initialize_repository() {
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    let help = shim(&path).arg("--help").output().unwrap();
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(String::from_utf8_lossy(&help.stdout).contains("serve <private-runtime-directory>"));
    let missing = shim(&path)
        .args(["ping", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(!path.join(".aethyme").exists());
    assert!(!path.join("state").exists());
    let invalid = shim(&path)
        .args(["serve", path.to_str().unwrap(), "--unknown"])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    assert!(!path.join("broker-service.sock").exists());
}

#[test]
fn public_cli_ping_reports_the_running_service() {
    let root = directory();
    let path: PathBuf = root.path().canonicalize().unwrap();
    let _running = Running::start(&path);
    let output = shim(&path)
        .args(["ping", path.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: Response = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response.result.unwrap().pid, std::process::id());
    assert!(!path.join(".aethyme").exists());
}

#[test]
fn public_cli_serve_runs_until_killed_and_restart_recovers_its_socket() {
    struct OwnedChild(std::process::Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let root = directory();
    let path = root.path().canonicalize().unwrap();
    let mut child = OwnedChild(
        shim(&path)
            .args(["serve", path.to_str().unwrap()])
            .spawn()
            .unwrap(),
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if ping(&path).is_ok() {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "service exited before readiness"
        );
        assert!(
            std::time::Instant::now() < deadline,
            "service did not become ready"
        );
        thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!path.join(".aethyme").exists());
    drop(child);
    let _replacement = Server::bind(&path).unwrap();
}
