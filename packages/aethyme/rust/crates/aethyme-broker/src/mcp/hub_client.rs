//! `connect`'s side of the hub: find or start the hub, attach, and bridge
//! the agent's stdio to the hub socket.
//!
//! Everything that can fail before the agent's first message is read ends in
//! [`Attach::Private`], and `connect` execs a private server as before (no
//! extra process). Once the `initialize` line has been read from stdin it
//! cannot be pushed back, so a later refusal (another protocol version, an
//! upstream that cannot be shared, no answer within the budget) ends in
//! [`Attach::Replay`]: `connect` then runs the private server as a child and
//! hands it that line followed by the rest of stdin.

use std::io::{self, BufReader, Read, Stdin, Write};
use std::os::unix::net::{SocketAddr, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::hub::{HubPaths, build_identity};
use super::jsonrpc::{self, read_message};
use super::key::Plan;
use crate::file_lock::ExclusiveFileLock;
use crate::service::transport;

/// How long `connect` waits for a hub to answer before starting privately.
pub const ATTACH_BUDGET: Duration = Duration::from_secs(2);
/// How long the hub may take to answer the first `initialize` (it may start
/// the upstream server first).
pub const INITIALIZE_BUDGET: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(25);

/// How an attach ended.
pub enum Attach {
    /// The session ran through the hub and is over.
    Done(u8),
    /// Nothing was read from stdin: start a private server by exec.
    Private(String),
    /// The `initialize` line was read: run a private server as a child and
    /// replay it, then the rest of stdin.
    Replay {
        reason: String,
        consumed: Vec<u8>,
        input: BufReader<Stdin>,
    },
}

/// `<host state>/mcp/hub`, canonical.
pub fn hub_directory() -> Option<PathBuf> {
    let directory = super::host_directory()?.join("hub");
    create_private_directory(&directory).ok()?;
    directory.canonicalize().ok()
}

fn create_private_directory(directory: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))
}

fn remaining(deadline: Instant) -> Duration {
    deadline
        .saturating_duration_since(Instant::now())
        .max(Duration::from_millis(1))
}

/// The environment the hub gets: what it needs to find its state and run
/// tools, not the agent's whole environment.
pub fn hub_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    std::env::vars_os()
        .filter(|(name, _)| {
            name.to_str().is_some_and(|name| {
                matches!(
                    name,
                    "PATH"
                        | "HOME"
                        | "USER"
                        | "LOGNAME"
                        | "TMPDIR"
                        | "LANG"
                        | "LC_ALL"
                        | "AETHYME_HOST_STATE_DIR"
                        | "AETHYME_HOST_CACHE_DIR"
                ) || name.starts_with("XDG_")
                    || name.starts_with("LC_")
            })
        })
        .collect()
}

/// Runs in the hub between fork and exec: a new session, detached from the
/// agent's terminal and process group.
fn new_session() -> io::Result<()> {
    // SAFETY: called in the forked child before exec; setsid is
    // async-signal-safe and takes no pointers.
    if unsafe { libc::setsid() } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// `aethyme mcp hub launch <directory>`: start `hub serve` in its own
/// session and return without waiting for it.
pub fn launch(directory: &Path) -> io::Result<()> {
    let paths = HubPaths::new(directory);
    let executable = std::env::current_exe()?.canonicalize()?;
    let log = transport::private_coordination_file(&paths.log)?;
    let mut command = Command::new(executable);
    command
        .args(["mcp", "hub", "serve"])
        .arg(directory)
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // SAFETY: `new_session` only calls setsid, which is async-signal-safe.
    unsafe {
        command.pre_exec(new_session);
    }
    command.spawn().map(drop)
}

/// Connect to this build's hub, starting it when nobody answers.
fn connect_or_start(paths: &HubPaths, deadline: Instant) -> Result<UnixStream, String> {
    match UnixStream::connect(&paths.socket) {
        Ok(stream) => return Ok(stream),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) => {}
        Err(error) => return Err(format!("hub_unreachable:{:?}", error.kind())),
    }
    let lock_file = transport::private_coordination_file(&paths.startup_lock)
        .map_err(|_| "hub_start_lock_unusable".to_string())?;
    let _claim = loop {
        if let Ok(stream) = UnixStream::connect(&paths.socket) {
            return Ok(stream);
        }
        let clone = lock_file
            .try_clone()
            .map_err(|_| "hub_start_lock_unusable".to_string())?;
        if let Some(claim) = ExclusiveFileLock::try_acquire(clone)
            .map_err(|_| "hub_start_lock_unusable".to_string())?
        {
            break claim;
        }
        if Instant::now() >= deadline {
            return Err("hub_start_busy".into());
        }
        thread::sleep(POLL);
    };
    // Someone may have started it while we waited for the claim.
    if let Ok(stream) = UnixStream::connect(&paths.socket) {
        return Ok(stream);
    }
    let executable = std::env::current_exe()
        .and_then(|path| path.canonicalize())
        .map_err(|_| "hub_executable_unknown".to_string())?;
    // `hub launch` starts the hub in its own session and exits at once, so
    // the hub is never a child of this process (which may exec a server).
    let mut launcher = Command::new(&executable);
    launcher
        .args(["mcp", "hub", "launch"])
        .arg(&paths.directory)
        .current_dir(&paths.directory)
        .env_clear()
        .envs(hub_environment())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let launched = launcher
        .status()
        .map_err(|_| "hub_spawn_failed".to_string())?;
    if !launched.success() {
        return Err("hub_launch_failed".into());
    }
    loop {
        match UnixStream::connect(&paths.socket) {
            Ok(stream) => return Ok(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(POLL),
            Err(_) => return Err("hub_start_timeout".into()),
        }
    }
}

/// Send one line and read one line back, within `timeout`.
fn exchange(stream: &mut UnixStream, line: &Value, timeout: Duration) -> io::Result<Option<Value>> {
    stream.set_read_timeout(Some(timeout))?;
    stream.write_all(&jsonrpc::encode(line))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let Some(reply) = read_message(&mut reader)? else {
        return Ok(None);
    };
    Ok(serde_json::from_slice(&reply).ok())
}

/// The hub's status, when one of this build is running. Never starts one.
pub fn query_status() -> Option<Value> {
    let directory = hub_directory()?;
    let paths = HubPaths::new(&directory);
    let mut stream = UnixStream::connect(&paths.socket).ok()?;
    exchange(
        &mut stream,
        &json!({"hub": 1, "op": "status"}),
        Duration::from_secs(2),
    )
    .ok()?
}

/// Attach this agent to the hub for `plan` and bridge until it leaves.
pub fn attach(plan: &Plan, budget: Duration) -> Attach {
    let deadline = Instant::now() + budget;
    let Some(directory) = hub_directory() else {
        return Attach::Private("hub_directory_unusable".into());
    };
    let paths = HubPaths::new(&directory);
    if SocketAddr::from_pathname(&paths.socket).is_err() {
        return Attach::Private("socket_path_too_long".into());
    }
    // Marks any hub (of any build) recorded, even if that hub has exited.
    if let Some(reason) = super::hub::unhealthy_reason(&directory, &plan.key) {
        return Attach::Private(format!("unhealthy:{reason}"));
    }
    let mut environment = Vec::new();
    for (name, value) in &plan.environment.0 {
        match (name.to_str(), value.to_str()) {
            (Some(name), Some(value)) => environment.push(json!([name, value])),
            _ => return Attach::Private("non_utf8_environment".into()),
        }
    }
    let mut stream = match connect_or_start(&paths, deadline) {
        Ok(stream) => stream,
        Err(reason) => return Attach::Private(reason),
    };
    // Argv and environment go only to a hub run by this user.
    if super::hub::peer_uid(&stream).ok() != Some(transport::owner()) {
        return Attach::Private("hub_peer_mismatch".into());
    }
    let hello = json!({
        "hub": 1,
        "op": "attach",
        "build": build_identity(),
        "key": plan.key,
        "name": plan.name,
        "argv": plan.resolution.argv,
        "cwd": plan.cwd,
        "env": environment,
    });
    let mut hub_reader = match stream.try_clone() {
        Ok(read_half) => BufReader::new(read_half),
        Err(_) => return Attach::Private("hub_socket_unusable".into()),
    };
    let accepted = stream
        .set_read_timeout(Some(remaining(deadline)))
        .and_then(|()| stream.write_all(&jsonrpc::encode(&hello)))
        .and_then(|()| read_message(&mut hub_reader));
    let reply: Option<Value> = match accepted {
        Ok(Some(line)) => serde_json::from_slice(&line).ok(),
        _ => None,
    };
    match reply {
        Some(reply) if reply.get("ok") == Some(&Value::Bool(true)) => {}
        Some(reply) => {
            let reason = reply
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("refused")
                .to_string();
            return Attach::Private(reason);
        }
        None => return Attach::Private("hub_no_answer".into()),
    }

    // The agent's first message is its initialize.
    let mut input = BufReader::new(io::stdin());
    let first = match read_message(&mut input) {
        Ok(Some(first)) => first,
        Ok(None) | Err(_) => return Attach::Done(0),
    };
    let mut line = first.clone();
    line.push(b'\n');
    let answered = stream
        .set_read_timeout(Some(INITIALIZE_BUDGET))
        .and_then(|()| stream.write_all(&line))
        .and_then(|()| read_message(&mut hub_reader));
    let reply = match answered {
        Ok(Some(reply)) => reply,
        _ => {
            return Attach::Replay {
                reason: "no_initialize_answer".into(),
                consumed: first,
                input,
            };
        }
    };
    if let Ok(control) = serde_json::from_slice::<Value>(&reply)
        && let Some(kind) = control.get("aethyme_hub").and_then(Value::as_str)
    {
        let reason = control
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or(kind)
            .to_string();
        return Attach::Replay {
            reason,
            consumed: first,
            input,
        };
    }
    if stream.set_read_timeout(None).is_err() {
        return Attach::Done(1);
    }
    let mut stdout = io::stdout();
    if stdout
        .write_all(&reply)
        .and_then(|()| stdout.write_all(b"\n"))
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return Attach::Done(1);
    }
    // Bridge: stdin -> hub on a thread, hub -> stdout here. When the agent
    // closes stdin the hub sees the end and closes; when the hub closes, the
    // session is over.
    let Ok(mut to_hub) = stream.try_clone() else {
        return Attach::Done(1);
    };
    let pump = thread::Builder::new()
        .name("mcp-connect-stdin".into())
        .spawn(move || {
            let _copied = io::copy(&mut input, &mut to_hub);
            let _closed = to_hub.shutdown(std::net::Shutdown::Write);
        });
    if pump.is_err() {
        return Attach::Done(1);
    }
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        match hub_reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                if stdout
                    .write_all(&buffer[..read])
                    .and_then(|()| stdout.flush())
                    .is_err()
                {
                    break;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
    }
    Attach::Done(0)
}

/// Run the private server as a child, replaying the consumed `initialize`
/// line, then the rest of stdin. Its stdout and stderr are this process's.
pub fn replay_private(plan: &Plan, consumed: &[u8], mut input: BufReader<Stdin>) -> u8 {
    let mut command = super::connect::server_command(plan);
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "aethyme mcp: {}: could not start {}: {error}",
                plan.name, plan.resolution.argv[0]
            );
            return crate::exit_status::ENVIRONMENT;
        }
    };
    let Some(mut stdin) = child.stdin.take() else {
        return crate::exit_status::ENVIRONMENT;
    };
    let mut first = consumed.to_vec();
    first.push(b'\n');
    let pump = thread::Builder::new()
        .name("mcp-connect-replay".into())
        .spawn(move || {
            if stdin.write_all(&first).is_ok() {
                let _copied = io::copy(&mut input, &mut stdin);
            }
        });
    crate::warn_unrecorded("feed the private MCP server", pump);
    match child.wait() {
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .unwrap_or(crate::exit_status::FAILED),
        Err(_) => crate::exit_status::FAILED,
    }
}
