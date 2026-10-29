//! How environment commands run (#1210).
//!
//! Every command a profile declares runs as `sh -c` from the worktree root
//! with a **cleared** environment: only a short allowlist (`PATH`, `HOME`, ...)
//! is inherited, plus the names a component lists in `pass_env`, plus the
//! `AETHYME_ENV_*` variables and the component's literal `env`. Secrets in the
//! caller's shell therefore never leak into a dev server by accident.
//!
//! Hooks (`provision`, `destroy`, `stop`, `build`, ...) run to completion
//! inside a process group this call owns, under a deadline, and their output
//! is appended to the component log. A hook may hand data to later hooks by
//! printing one JSON object on a line starting with `AETHYME_RECEIPT `; the
//! last such line wins.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use super::compile::{CompiledComponent, CompiledEnvironment};
use super::logs::{LOG_CAP_BYTES, open_for_append, tail};
use super::validate::env_suffix;

pub const RECEIPT_PREFIX: &str = "AETHYME_RECEIPT ";
pub const RECEIPT_MAX_BYTES: usize = 16 * 1024;
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(600);
/// Most bytes kept from one stream of one hook run.
const HOOK_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

/// Inherited from the caller. Everything else must be named in `pass_env`.
pub const INHERITED_VARIABLES: &[&str] = &[
    "PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "LC_CTYPE", "TMPDIR", "SHELL", "TERM",
];

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("{what}: {source}")]
    Io {
        what: String,
        source: std::io::Error,
    },
    #[error("{label} exited with {exit}; last log lines:\n{tail}")]
    HookFailed {
        label: String,
        exit: String,
        tail: String,
    },
    #[error("{label} did not finish within {seconds}s")]
    Timeout { label: String, seconds: u64 },
    #[error("{label} printed an invalid receipt: {reason}")]
    InvalidReceipt { label: String, reason: String },
    #[error("{component} exited right after start; last log lines:\n{tail}")]
    StartExited { component: String, tail: String },
    #[error("{component} did not become healthy within {seconds}s: {last}")]
    Unhealthy {
        component: String,
        seconds: u64,
        last: String,
    },
    #[error(
        "{component}: the recorded process or container cannot be proven to be ours ({detail}); nothing was signalled"
    )]
    Unproven { component: String, detail: String },
    #[error("container engine: {0}")]
    Engine(String),
    #[error("{0}")]
    Unsupported(String),
}

impl DriverError {
    /// Stable code for JSON output and `last_error`.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io { .. } => "io_error",
            Self::HookFailed { .. } => "hook_failed",
            Self::Timeout { .. } => "hook_timeout",
            Self::InvalidReceipt { .. } => "invalid_receipt",
            Self::StartExited { .. } => "start_exited",
            Self::Unhealthy { .. } => "unhealthy",
            Self::Unproven { .. } => "identity_unproven",
            Self::Engine(_) => "engine_error",
            Self::Unsupported(_) => "unsupported",
        }
    }
}

pub(crate) fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> DriverError {
    let what = what.into();
    move |source| DriverError::Io { what, source }
}

/// Values only known once leases are granted and hooks have run.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    pub namespace: String,
    pub worktree: PathBuf,
    /// Host port per listening component.
    pub ports: BTreeMap<String, u16>,
    /// Receipt per component that has one.
    pub receipts: BTreeMap<String, Value>,
}

/// The `AETHYME_ENV_*` variables plus the component's own `env`, for one
/// command of one component. `pass_env` is applied by [`command`].
pub fn environment_variables(
    environment: &CompiledEnvironment,
    component: &CompiledComponent,
    bindings: &Bindings,
    attempt: u32,
) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    vars.insert("AETHYME_ENV_ID".to_string(), environment.env_id.clone());
    vars.insert("AETHYME_ENV_NAMESPACE".into(), bindings.namespace.clone());
    vars.insert("AETHYME_ENV_PROFILE".into(), environment.profile.clone());
    vars.insert(
        "AETHYME_ENV_WORKTREE".into(),
        bindings.worktree.to_string_lossy().into_owned(),
    );
    vars.insert("AETHYME_ENV_COMPONENT".into(), component.name.clone());
    vars.insert("AETHYME_ENV_HOOK_ATTEMPT".into(), attempt.max(1).to_string());
    for (name, port) in &bindings.ports {
        let suffix = env_suffix(name);
        vars.insert(format!("AETHYME_ENV_PORT_{suffix}"), port.to_string());
        vars.insert(
            format!("AETHYME_ENV_URL_{suffix}"),
            format!("http://127.0.0.1:{port}"),
        );
    }
    for (name, receipt) in &bindings.receipts {
        vars.insert(
            format!("AETHYME_ENV_RECEIPT_{}", env_suffix(name)),
            receipt.to_string(),
        );
    }
    for (key, value) in &component.env {
        vars.insert(key.clone(), value.clone());
    }
    vars
}

/// `sh -c <text>` from `cwd` with a cleared environment: the inherited
/// allowlist, then `pass_env`, then `vars`.
pub fn command(text: &str, cwd: &Path, pass_env: &[String], vars: &BTreeMap<String, String>) -> Command {
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(text).current_dir(cwd).env_clear();
    for name in INHERITED_VARIABLES
        .iter()
        .copied()
        .chain(pass_env.iter().map(String::as_str))
    {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command.envs(vars);
    command
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookOutcome {
    pub receipt: Option<Value>,
}

/// Run one hook to completion. Output is appended to `log`; a non-zero exit,
/// a timeout or an invalid receipt is an error carrying the log tail.
pub fn run_hook(
    label: &str,
    text: &str,
    cwd: &Path,
    pass_env: &[String],
    vars: &BTreeMap<String, String>,
    log: &Path,
    timeout: Duration,
) -> Result<HookOutcome, DriverError> {
    let mut command = command(text, cwd, pass_env, vars);
    let outcome =
        crate::bounded_output::output_limited_in_owned_group(&mut command, timeout, HOOK_OUTPUT_LIMIT)
            .map_err(io(format!("run {label}")))?;
    let output = match outcome {
        crate::bounded_output::LimitedOutput::Completed(output) => output,
        crate::bounded_output::LimitedOutput::Deadline => {
            append_log(log, &format!("[aethyme] {label}: timed out after {}s\n", timeout.as_secs()));
            return Err(DriverError::Timeout {
                label: label.into(),
                seconds: timeout.as_secs(),
            });
        }
        crate::bounded_output::LimitedOutput::OutputLimit => {
            append_log(log, &format!("[aethyme] {label}: output exceeded {HOOK_OUTPUT_LIMIT} bytes\n"));
            return Err(DriverError::HookFailed {
                label: label.into(),
                exit: "output limit".into(),
                tail: String::new(),
            });
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    append_log(
        log,
        &format!("[aethyme] {label}: {}\n{stdout}{stderr}", output.status),
    );
    if !output.status.success() {
        return Err(DriverError::HookFailed {
            label: label.into(),
            exit: output.status.to_string(),
            tail: log_tail(log),
        });
    }
    let receipt = parse_receipt(&stdout).map_err(|reason| DriverError::InvalidReceipt {
        label: label.into(),
        reason,
    })?;
    Ok(HookOutcome { receipt })
}

/// The last `AETHYME_RECEIPT {json}` line, which must be a JSON object of at
/// most [`RECEIPT_MAX_BYTES`].
pub fn parse_receipt(stdout: &str) -> Result<Option<Value>, String> {
    let Some(line) = stdout
        .lines()
        .rev()
        .find_map(|line| line.trim_end().strip_prefix(RECEIPT_PREFIX))
    else {
        return Ok(None);
    };
    if line.len() > RECEIPT_MAX_BYTES {
        return Err(format!("{} bytes exceeds {RECEIPT_MAX_BYTES}", line.len()));
    }
    let value: Value = serde_json::from_str(line).map_err(|error| error.to_string())?;
    if !value.is_object() {
        return Err("a receipt must be a JSON object".into());
    }
    Ok(Some(value))
}

pub(crate) fn append_log(log: &Path, text: &str) {
    if let Ok(mut file) = open_for_append(log, LOG_CAP_BYTES) {
        let _ = file.write_all(text.as_bytes());
    }
}

pub(crate) fn log_tail(log: &Path) -> String {
    tail(log, 20).unwrap_or_default().join("\n")
}
