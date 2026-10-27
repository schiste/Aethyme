//! Private process-group supervision for noninteractive broker commands.
//!
//! This is a lifecycle guard, not a sandbox: a command that deliberately leaves
//! its process group is outside the guarantee. The caller preserves the
//! repository command's argv and inherited stdio, except that JSON-mode guarded
//! exec routes child stdout to stderr. Preparation starts the helper
//! with null stdin; generic guarded execution preserves inherited stdin. A normal
//! in-group descendant is stopped when the helper exits.

use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::fd::{AsRawFd as _, FromRawFd as _, RawFd};

const FLAG: &str = "--aethyme-inline-exec-supervisor-v4";
const FRAME_MAGIC: &[u8; 4] = b"AIX4";
const MAX_FRAME_BYTES: usize = 1024 * 1024;
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
// Polling for an idle control channel is short, but once a frame starts the
// complete frame still has a fixed assembly budget.
const FRAME_IO_TIMEOUT: std::time::Duration = READY_TIMEOUT;
const HELPER_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
const GROUP_EXIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
pub(crate) const DEFAULT_GUARDED_EXEC_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30 * 60);

#[derive(Debug)]
pub(crate) enum InlineExecError {
    Setup(io::Error),
    CommandStart(io::Error),
    CommandOutcomeUnknown(String),
    CleanupUnknown(String),
    OperationAfterDispatch(crate::BrokerOpError),
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
enum ParentFrame {
    Run {
        cwd: PathBuf,
        command: Vec<String>,
        environment: Vec<(String, String)>,
        observation: Option<crate::store::guarded_exec::HelperObservationTarget>,
    },
    Ping,
    Abort,
}

#[derive(serde::Serialize)]
enum ParentFrameRef<'a> {
    Run {
        cwd: &'a Path,
        command: &'a [String],
        environment: &'a [(String, String)],
        observation: Option<&'a crate::store::guarded_exec::HelperObservationTarget>,
    },
    Ping,
    Abort,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
enum HelperFrame {
    Ready,
    Started,
    Exited { code: Option<i32> },
    Aborted,
    CommandStartFailed { error: WireCommandStartError },
    Failed { reason: String },
    ExecutionUnknown { reason: String },
}

// Keep these tags independent of Rust's Debug formatting. If an ErrorKind
// has no stable wire tag, encode its name explicitly so the receiver can
// preserve a diagnostic while mapping it conservatively when no OS code exists.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireIoErrorKind {
    NotFound,
    PermissionDenied,
    ConnectionRefused,
    ConnectionReset,
    HostUnreachable,
    NetworkUnreachable,
    ConnectionAborted,
    NotConnected,
    AddrInUse,
    AddrNotAvailable,
    NetworkDown,
    BrokenPipe,
    AlreadyExists,
    WouldBlock,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    ReadOnlyFilesystem,
    StaleNetworkFileHandle,
    InvalidInput,
    InvalidData,
    TimedOut,
    WriteZero,
    StorageFull,
    NotSeekable,
    QuotaExceeded,
    FileTooLarge,
    ResourceBusy,
    ExecutableFileBusy,
    Deadlock,
    CrossesDevices,
    TooManyLinks,
    InvalidFilename,
    ArgumentListTooLong,
    Interrupted,
    Unsupported,
    UnexpectedEof,
    OutOfMemory,
    Other,
    Unknown { debug_name: String },
}

impl WireIoErrorKind {
    fn from_io(kind: io::ErrorKind) -> Self {
        use io::ErrorKind as K;

        match kind {
            K::NotFound => Self::NotFound,
            K::PermissionDenied => Self::PermissionDenied,
            K::ConnectionRefused => Self::ConnectionRefused,
            K::ConnectionReset => Self::ConnectionReset,
            K::HostUnreachable => Self::HostUnreachable,
            K::NetworkUnreachable => Self::NetworkUnreachable,
            K::ConnectionAborted => Self::ConnectionAborted,
            K::NotConnected => Self::NotConnected,
            K::AddrInUse => Self::AddrInUse,
            K::AddrNotAvailable => Self::AddrNotAvailable,
            K::NetworkDown => Self::NetworkDown,
            K::BrokenPipe => Self::BrokenPipe,
            K::AlreadyExists => Self::AlreadyExists,
            K::WouldBlock => Self::WouldBlock,
            K::NotADirectory => Self::NotADirectory,
            K::IsADirectory => Self::IsADirectory,
            K::DirectoryNotEmpty => Self::DirectoryNotEmpty,
            K::ReadOnlyFilesystem => Self::ReadOnlyFilesystem,
            K::StaleNetworkFileHandle => Self::StaleNetworkFileHandle,
            K::InvalidInput => Self::InvalidInput,
            K::InvalidData => Self::InvalidData,
            K::TimedOut => Self::TimedOut,
            K::WriteZero => Self::WriteZero,
            K::StorageFull => Self::StorageFull,
            K::NotSeekable => Self::NotSeekable,
            K::QuotaExceeded => Self::QuotaExceeded,
            K::FileTooLarge => Self::FileTooLarge,
            K::ResourceBusy => Self::ResourceBusy,
            K::ExecutableFileBusy => Self::ExecutableFileBusy,
            K::Deadlock => Self::Deadlock,
            K::CrossesDevices => Self::CrossesDevices,
            K::TooManyLinks => Self::TooManyLinks,
            K::InvalidFilename => Self::InvalidFilename,
            K::ArgumentListTooLong => Self::ArgumentListTooLong,
            K::Interrupted => Self::Interrupted,
            K::Unsupported => Self::Unsupported,
            K::UnexpectedEof => Self::UnexpectedEof,
            K::OutOfMemory => Self::OutOfMemory,
            K::Other => Self::Other,
            other => Self::Unknown {
                debug_name: format!("{other:?}"),
            },
        }
    }

    fn as_io_kind(&self) -> Option<io::ErrorKind> {
        use io::ErrorKind as K;

        Some(match self {
            Self::NotFound => K::NotFound,
            Self::PermissionDenied => K::PermissionDenied,
            Self::ConnectionRefused => K::ConnectionRefused,
            Self::ConnectionReset => K::ConnectionReset,
            Self::HostUnreachable => K::HostUnreachable,
            Self::NetworkUnreachable => K::NetworkUnreachable,
            Self::ConnectionAborted => K::ConnectionAborted,
            Self::NotConnected => K::NotConnected,
            Self::AddrInUse => K::AddrInUse,
            Self::AddrNotAvailable => K::AddrNotAvailable,
            Self::NetworkDown => K::NetworkDown,
            Self::BrokenPipe => K::BrokenPipe,
            Self::AlreadyExists => K::AlreadyExists,
            Self::WouldBlock => K::WouldBlock,
            Self::NotADirectory => K::NotADirectory,
            Self::IsADirectory => K::IsADirectory,
            Self::DirectoryNotEmpty => K::DirectoryNotEmpty,
            Self::ReadOnlyFilesystem => K::ReadOnlyFilesystem,
            Self::StaleNetworkFileHandle => K::StaleNetworkFileHandle,
            Self::InvalidInput => K::InvalidInput,
            Self::InvalidData => K::InvalidData,
            Self::TimedOut => K::TimedOut,
            Self::WriteZero => K::WriteZero,
            Self::StorageFull => K::StorageFull,
            Self::NotSeekable => K::NotSeekable,
            Self::QuotaExceeded => K::QuotaExceeded,
            Self::FileTooLarge => K::FileTooLarge,
            Self::ResourceBusy => K::ResourceBusy,
            Self::ExecutableFileBusy => K::ExecutableFileBusy,
            Self::Deadlock => K::Deadlock,
            Self::CrossesDevices => K::CrossesDevices,
            Self::TooManyLinks => K::TooManyLinks,
            Self::InvalidFilename => K::InvalidFilename,
            Self::ArgumentListTooLong => K::ArgumentListTooLong,
            Self::Interrupted => K::Interrupted,
            Self::Unsupported => K::Unsupported,
            Self::UnexpectedEof => K::UnexpectedEof,
            Self::OutOfMemory => K::OutOfMemory,
            Self::Other => K::Other,
            Self::Unknown { .. } => return None,
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCommandStartError {
    kind: WireIoErrorKind,
    raw_os_error: Option<i32>,
    message: String,
}

impl WireCommandStartError {
    fn capture(error: io::Error) -> Self {
        Self {
            kind: WireIoErrorKind::from_io(error.kind()),
            raw_os_error: error.raw_os_error(),
            message: error.to_string(),
        }
    }

    fn into_io_error(self) -> io::Result<io::Error> {
        if let Some(raw_os_error) = self.raw_os_error {
            let error = io::Error::from_raw_os_error(raw_os_error);
            if let Some(expected_kind) = self.kind.as_io_kind()
                && error.kind() != expected_kind
            {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "helper spawn error kind {:?} disagrees with raw OS error {raw_os_error} ({:?})",
                        expected_kind,
                        error.kind()
                    ),
                ));
            }
            if let WireIoErrorKind::Unknown { debug_name } = self.kind {
                crate::warn_unrecorded(
                    "preserve raw spawn error with unknown remote io::ErrorKind",
                    Err::<(), _>(io::Error::other(format!(
                        "unmapped remote io::ErrorKind {debug_name}; raw OS error {raw_os_error}"
                    ))),
                );
            }
            return Ok(error);
        }

        match self.kind {
            WireIoErrorKind::Unknown { debug_name } => Ok(io::Error::other(format!(
                "unmapped remote std::io::ErrorKind {debug_name}: {}",
                self.message
            ))),
            kind => match kind.as_io_kind() {
                Some(kind) => Ok(io::Error::new(kind, self.message)),
                None => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "known wire io::ErrorKind did not map to std::io::ErrorKind",
                )),
            },
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct CommandResult {
    pub exit_code: Option<i32>,
    pub succeeded: bool,
    pub termination: CommandTermination,
    pub command_started: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandTermination {
    Exited,
    TimedOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandDispatchState {
    Started,
    NotStarted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GenericExecMode {
    Supervised,
    Legacy(String),
}

/// Select a deadline only for supervised execution. Legacy mode keeps its
/// existing behavior unless the caller explicitly asked for a timeout, in
/// which case refusing is safer than promising a bound it cannot enforce.
pub(crate) fn select_guarded_exec_timeout(
    mode: &GenericExecMode,
    requested: Option<std::time::Duration>,
) -> Result<Option<std::time::Duration>, String> {
    match (mode, requested) {
        (GenericExecMode::Supervised, Some(timeout)) => Ok(Some(timeout)),
        (GenericExecMode::Supervised, None) => Ok(Some(DEFAULT_GUARDED_EXEC_TIMEOUT)),
        (GenericExecMode::Legacy(_), None) => Ok(None),
        (GenericExecMode::Legacy(reason), Some(_)) => Err(reason.clone()),
    }
}

fn select_generic_exec_mode(
    supported_platform: bool,
    controlling_terminal: Result<bool, String>,
) -> GenericExecMode {
    if !supported_platform {
        return GenericExecMode::Legacy(
            "process-group supervision supports only macOS and Linux".into(),
        );
    }
    match controlling_terminal {
        Ok(false) => GenericExecMode::Supervised,
        Ok(true) => GenericExecMode::Legacy("a controlling terminal is available".into()),
        Err(reason) => GenericExecMode::Legacy(format!(
            "could not determine whether a controlling terminal is available: {reason}"
        )),
    }
}

#[cfg(unix)]
pub(crate) fn generic_exec_mode() -> GenericExecMode {
    let supported_platform = cfg!(any(target_os = "linux", target_os = "macos"));
    if !supported_platform {
        return select_generic_exec_mode(false, Ok(false));
    }
    let controlling_terminal = match std::fs::File::open("/dev/tty") {
        Ok(_) => Ok(true),
        Err(error) if error.raw_os_error() == Some(libc::ENXIO) => Ok(false),
        Err(error) => Err(error.to_string()),
    };
    select_generic_exec_mode(supported_platform, controlling_terminal)
}

#[cfg(not(unix))]
pub(crate) fn generic_exec_mode() -> GenericExecMode {
    select_generic_exec_mode(
        false,
        Err("controlling-terminal probe is unavailable".into()),
    )
}

/// An exact helper that is ready to run one command. Preparation starts one
/// before opening its shared resource grant or writing cache/state paths; both
/// preparation and generic guarded execution create an instance immediately
/// before each command.
#[cfg(unix)]
pub(crate) struct InlineExecSupervisor {
    child: Option<std::process::Child>,
    control: Option<std::os::unix::net::UnixStream>,
    process_group: i32,
    terminalized: bool,
    observation: Option<crate::store::guarded_exec::HelperObservationTarget>,
    artifact_use: Option<crate::file_lock::ArtifactUseProducer>,
}

#[cfg(not(unix))]
pub(crate) struct InlineExecSupervisor;

impl InlineExecSupervisor {
    pub(crate) fn with_observation(
        self,
        target: Option<crate::store::guarded_exec::HelperObservationTarget>,
    ) -> Self {
        #[cfg(unix)]
        {
            let mut supervisor = self;
            supervisor.observation = target;
            supervisor
        }
        #[cfg(not(unix))]
        {
            std::mem::drop(target);
            self
        }
    }

    pub(crate) fn with_artifact_use(
        self,
        producer: Option<crate::file_lock::ArtifactUseProducer>,
    ) -> Self {
        #[cfg(unix)]
        {
            let mut supervisor = self;
            supervisor.artifact_use = producer;
            supervisor
        }
        #[cfg(not(unix))]
        {
            std::mem::drop(producer);
            self
        }
    }

    #[cfg(unix)]
    pub(crate) fn start(executable: &Path) -> Result<Self, InlineExecError> {
        Self::start_with_stdin(
            executable,
            std::process::Stdio::null(),
            std::process::Stdio::inherit(),
            false,
            false,
        )
    }

    #[cfg(unix)]
    pub(crate) fn start_inheriting_stdin(
        executable: &Path,
        stdout: std::process::Stdio,
    ) -> Result<Self, InlineExecError> {
        Self::start_with_stdin(
            executable,
            std::process::Stdio::inherit(),
            stdout,
            false,
            false,
        )
    }

    #[cfg(all(unix, test))]
    fn start_with_watcher_fault(
        executable: &Path,
        fail_cleanup_watcher: bool,
        fail_after_launch: bool,
    ) -> Result<Self, InlineExecError> {
        Self::start_with_stdin(
            executable,
            std::process::Stdio::null(),
            std::process::Stdio::inherit(),
            fail_cleanup_watcher,
            fail_after_launch,
        )
    }

    #[cfg(unix)]
    fn start_with_stdin(
        executable: &Path,
        stdin: std::process::Stdio,
        stdout: std::process::Stdio,
        fail_cleanup_watcher: bool,
        _fail_after_launch: bool,
    ) -> Result<Self, InlineExecError> {
        use std::os::unix::process::CommandExt as _;
        use std::process::{Command, Stdio};
        use std::time::Instant;

        if !cfg!(any(target_os = "linux", target_os = "macos")) {
            return Err(InlineExecError::Setup(io::Error::new(
                io::ErrorKind::Unsupported,
                "process supervision supports only macOS and Linux",
            )));
        }

        let (parent, child_socket) =
            std::os::unix::net::UnixStream::pair().map_err(InlineExecError::Setup)?;
        // Command's stdin/stdout/stderr setup runs before `pre_exec`; move both
        // endpoints above fd 3 so an initially closed standard descriptor
        // cannot be replaced with /dev/null or inherited stdio before dup2.
        let parent = move_control_fd_above_stdio(parent).map_err(InlineExecError::Setup)?;
        let child_socket =
            move_control_fd_above_stdio(child_socket).map_err(InlineExecError::Setup)?;
        parent
            .set_nonblocking(true)
            .map_err(InlineExecError::Setup)?;
        let child_fd = child_socket.as_raw_fd();
        let mut command = Command::new(executable);
        #[cfg(test)]
        {
            let selected = executable.canonicalize().map_err(InlineExecError::Setup)?;
            let test_binary = std::env::current_exe()
                .and_then(|path| path.canonicalize())
                .ok();
            if test_binary.as_deref() == Some(selected.as_path()) {
                command
                    .args([
                        "--exact",
                        "inline_exec_supervisor::tests::inline_helper_child_fixture",
                        "--nocapture",
                        "--quiet",
                    ])
                    .env("AETHYME_TEST_INLINE_EXEC_SUPERVISOR", "1");
                if fail_cleanup_watcher {
                    command.env("AETHYME_TEST_INLINE_FAIL_CLEANUP_WATCHER", "1");
                }
                if _fail_after_launch {
                    command.env("AETHYME_TEST_INLINE_FAIL_AFTER_LAUNCH", "1");
                }
            } else {
                if fail_cleanup_watcher {
                    return Err(InlineExecError::Setup(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "watcher failure injection requires the unit-test helper binary",
                    )));
                }
                command.arg(FLAG);
            }
        }
        #[cfg(not(test))]
        {
            if fail_cleanup_watcher {
                return Err(InlineExecError::Setup(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "watcher failure injection is test-only",
                )));
            }
            command.arg(FLAG);
        }
        command
            .stdin(stdin)
            .stdout(stdout)
            .stderr(Stdio::inherit())
            .process_group(0);
        // SAFETY: the child-side stream is created before spawn. The hook uses
        // only dup2/fcntl syscalls, and retains no Rust state or locks.
        unsafe {
            command.pre_exec(move || {
                if child_fd != 3 && libc::dup2(child_fd, 3) < 0 {
                    return Err(io::Error::last_os_error());
                }
                let flags = libc::fcntl(3, libc::F_GETFD);
                if flags < 0 || libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(InlineExecError::Setup)?;
        drop(child_socket);
        let process_group = match i32::try_from(child.id()) {
            Ok(process_group) => process_group,
            Err(_) => {
                let mut child = child;
                if let Err(error) = child.kill() {
                    crate::warn_unrecorded(
                        "stop helper with an invalid process id",
                        Err::<(), _>(error),
                    );
                }
                if let Err(error) = child.wait() {
                    crate::warn_unrecorded(
                        "reap helper with an invalid process id",
                        Err::<(), _>(error),
                    );
                }
                return Err(InlineExecError::Setup(io::Error::other(
                    "helper process id is out of range",
                )));
            }
        };
        let mut supervisor = Self {
            child: Some(child),
            control: Some(parent),
            process_group,
            terminalized: false,
            observation: None,
            artifact_use: None,
        };
        let ready = read_frame_until(
            supervisor
                .control
                .as_mut()
                .expect("control stream is present"),
            Instant::now() + READY_TIMEOUT,
        );
        match ready {
            Ok(HelperFrame::Ready) => Ok(supervisor),
            Ok(frame) => {
                let detail = format!("unexpected helper readiness frame: {frame:?}");
                supervisor
                    .terminate_and_verify()
                    .map_err(InlineExecError::CleanupUnknown)?;
                Err(InlineExecError::Setup(io::Error::other(detail)))
            }
            Err(error) => {
                let detail = format!("helper did not become ready: {error}");
                supervisor
                    .terminate_and_verify()
                    .map_err(InlineExecError::CleanupUnknown)?;
                Err(InlineExecError::Setup(io::Error::other(detail)))
            }
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn start(_: &Path) -> Result<Self, InlineExecError> {
        Err(InlineExecError::Setup(io::Error::new(
            io::ErrorKind::Unsupported,
            "process-group supervision requires a supported Unix platform",
        )))
    }

    #[cfg(not(unix))]
    pub(crate) fn start_inheriting_stdin(
        executable: &Path,
        _: std::process::Stdio,
    ) -> Result<Self, InlineExecError> {
        Self::start(executable)
    }

    #[cfg(unix)]
    pub(crate) fn run<F>(
        self,
        cwd: &Path,
        command: &[String],
        environment: &[(String, String)],
        heartbeat: F,
    ) -> Result<CommandResult, InlineExecError>
    where
        F: FnMut() -> Result<(), crate::BrokerOpError>,
    {
        self.run_with_timeout(cwd, command, environment, None, heartbeat)
    }

    #[cfg(unix)]
    pub(crate) fn run_with_timeout<F>(
        self,
        cwd: &Path,
        command: &[String],
        environment: &[(String, String)],
        command_timeout: Option<std::time::Duration>,
        heartbeat: F,
    ) -> Result<CommandResult, InlineExecError>
    where
        F: FnMut() -> Result<(), crate::BrokerOpError>,
    {
        self.run_with_timeout_and_dispatch(
            cwd,
            command,
            environment,
            command_timeout,
            heartbeat,
            |_| {},
        )
    }

    #[cfg(unix)]
    pub(crate) fn run_with_timeout_and_dispatch<F, D>(
        mut self,
        cwd: &Path,
        command: &[String],
        environment: &[(String, String)],
        command_timeout: Option<std::time::Duration>,
        mut heartbeat: F,
        mut dispatch: D,
    ) -> Result<CommandResult, InlineExecError>
    where
        F: FnMut() -> Result<(), crate::BrokerOpError>,
        D: FnMut(CommandDispatchState),
    {
        use std::time::{Duration, Instant};

        if command.is_empty() || command[0].is_empty() {
            let cleanup = self.terminate_not_started_and_verify();
            if cleanup.is_ok() {
                dispatch(CommandDispatchState::NotStarted);
            }
            return match cleanup {
                Ok(()) => Err(InlineExecError::Setup(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "supervised command argv is empty",
                ))),
                Err(reason) => Err(InlineExecError::CleanupUnknown(reason)),
            };
        }
        validate_environment(environment).map_err(|error| {
            let cleanup = self.terminate_not_started_and_verify();
            if cleanup.is_ok() {
                dispatch(CommandDispatchState::NotStarted);
            }
            match cleanup {
                Ok(()) => InlineExecError::Setup(error),
                Err(reason) => InlineExecError::CleanupUnknown(reason),
            }
        })?;
        let cwd = cwd.canonicalize().map_err(|error| {
            let cleanup = self.terminate_not_started_and_verify();
            if cleanup.is_ok() {
                dispatch(CommandDispatchState::NotStarted);
            }
            match cleanup {
                Ok(()) => InlineExecError::Setup(error),
                Err(reason) => InlineExecError::CleanupUnknown(reason),
            }
        })?;
        let request = ParentFrameRef::Run {
            cwd: &cwd,
            command,
            environment,
            observation: self.observation.as_ref(),
        };
        if let Some(producer) = self.artifact_use.as_mut() {
            let child = self.child.as_ref().ok_or_else(|| {
                InlineExecError::CleanupUnknown(
                    "helper child handle disappeared before command admission".into(),
                )
            })?;
            if let Err(error) = producer.mark_process_started(child, self.process_group) {
                let cleanup = self.terminate_and_verify();
                return match cleanup {
                    Ok(()) => Err(InlineExecError::Setup(error)),
                    Err(reason) => Err(InlineExecError::CleanupUnknown(reason)),
                };
            }
        }
        if let Err(error) = write_frame_until(
            self.control.as_mut().expect("control stream is present"),
            &request,
            Instant::now() + READY_TIMEOUT,
        ) {
            // The helper may have received a complete request before the
            // control channel failed. Preserve outcome-unknown semantics;
            // terminate_and_verify settles the durable marker only after the
            // owned helper exits and its original process group is absent.
            let cleanup = self.terminate_and_verify();
            return match cleanup {
                Ok(()) => Err(InlineExecError::CommandOutcomeUnknown(format!(
                    "command admission channel failed after dispatch may have occurred: {error}"
                ))),
                Err(reason) => Err(InlineExecError::CleanupUnknown(reason)),
            };
        }

        let command_deadline = command_timeout.map(|timeout| Instant::now() + timeout);
        let mut next_ping = Instant::now() + Duration::from_secs(1);
        let mut command_started = false;
        loop {
            if command_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return self.timed_out_result(command_started);
            }
            if let Err(error) = heartbeat() {
                if command_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    return self.timed_out_result(command_started);
                }
                let cleanup = self.abort_and_verify();
                return match cleanup {
                    Ok(()) => Err(InlineExecError::OperationAfterDispatch(error)),
                    Err(reason) => Err(InlineExecError::CleanupUnknown(reason)),
                };
            }
            if command_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return self.timed_out_result(command_started);
            }
            let now = Instant::now();
            let read_timeout = command_deadline.map_or(Duration::from_millis(200), |deadline| {
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(200))
            });
            let frame = match read_frame_timeout(
                self.control.as_mut().expect("control stream is present"),
                read_timeout,
                command_deadline,
            ) {
                Ok(Some(frame)) => frame,
                Ok(None) => {
                    if command_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return self.timed_out_result(command_started);
                    }
                    if Instant::now() >= next_ping {
                        let ping_deadline = Instant::now() + Duration::from_secs(1);
                        let write_deadline = command_deadline
                            .map_or(ping_deadline, |deadline| deadline.min(ping_deadline));
                        if let Err(error) = write_frame_until(
                            self.control.as_mut().expect("control stream is present"),
                            &ParentFrameRef::Ping,
                            write_deadline,
                        ) {
                            if command_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                                return self.timed_out_result(command_started);
                            }
                            let cleanup = self.terminate_and_verify();
                            return match cleanup {
                                Ok(()) => Err(InlineExecError::CommandOutcomeUnknown(format!(
                                    "could not keep the execution control channel alive: {error}"
                                ))),
                                Err(reason) => Err(InlineExecError::CleanupUnknown(reason)),
                            };
                        }
                        next_ping = Instant::now() + Duration::from_secs(1);
                    }
                    continue;
                }
                Err(error) => {
                    if command_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return self.timed_out_result(command_started);
                    }
                    let cleanup = self.terminate_and_verify();
                    return match cleanup {
                        Ok(()) => Err(InlineExecError::CommandOutcomeUnknown(format!(
                            "could not read the command result after dispatch: {error}"
                        ))),
                        Err(reason) => Err(InlineExecError::CleanupUnknown(reason)),
                    };
                }
            };
            return match frame {
                HelperFrame::Started if !command_started => {
                    command_started = true;
                    dispatch(CommandDispatchState::Started);
                    continue;
                }
                HelperFrame::Started => {
                    self.terminate_and_verify()
                        .map_err(InlineExecError::CleanupUnknown)?;
                    Err(InlineExecError::CommandOutcomeUnknown(
                        "helper reported command start more than once".into(),
                    ))
                }
                HelperFrame::Exited { code } => {
                    self.terminate_and_verify()
                        .map_err(InlineExecError::CleanupUnknown)?;
                    if command_started {
                        Ok(CommandResult {
                            exit_code: code,
                            succeeded: code == Some(0),
                            termination: CommandTermination::Exited,
                            command_started: true,
                        })
                    } else {
                        Err(InlineExecError::CommandOutcomeUnknown(
                            "helper reported command exit without a start confirmation".into(),
                        ))
                    }
                }
                HelperFrame::CommandStartFailed { error } => {
                    self.terminate_and_verify()
                        .map_err(InlineExecError::CleanupUnknown)?;
                    if command_started {
                        return Err(InlineExecError::CommandOutcomeUnknown(
                            "helper reported command-start failure after start confirmation".into(),
                        ));
                    }
                    dispatch(CommandDispatchState::NotStarted);
                    match error.into_io_error() {
                        Ok(error) => Err(InlineExecError::CommandStart(error)),
                        Err(error) => Err(InlineExecError::CommandOutcomeUnknown(format!(
                            "helper returned invalid command-start error details: {error}"
                        ))),
                    }
                }
                HelperFrame::Failed { reason } => {
                    self.terminate_not_started_and_verify()
                        .map_err(InlineExecError::CleanupUnknown)?;
                    if command_started {
                        return Err(InlineExecError::CommandOutcomeUnknown(format!(
                            "helper reported a setup failure after start confirmation: {reason}"
                        )));
                    }
                    dispatch(CommandDispatchState::NotStarted);
                    Err(InlineExecError::Setup(io::Error::other(reason)))
                }
                HelperFrame::ExecutionUnknown { reason } => {
                    self.terminate_and_verify()
                        .map_err(InlineExecError::CleanupUnknown)?;
                    Err(InlineExecError::CommandOutcomeUnknown(reason))
                }
                HelperFrame::Aborted | HelperFrame::Ready => {
                    self.terminate_and_verify()
                        .map_err(InlineExecError::CleanupUnknown)?;
                    Err(InlineExecError::CommandOutcomeUnknown(
                        "helper returned an invalid command response after dispatch".into(),
                    ))
                }
            };
        }
    }

    #[cfg(not(unix))]
    pub(crate) fn run<F>(
        self,
        cwd: &Path,
        command: &[String],
        environment: &[(String, String)],
        heartbeat: F,
    ) -> Result<CommandResult, InlineExecError>
    where
        F: FnMut() -> Result<(), crate::BrokerOpError>,
    {
        self.run_with_timeout(cwd, command, environment, None, heartbeat)
    }

    #[cfg(not(unix))]
    pub(crate) fn run_with_timeout<F>(
        self,
        _: &Path,
        _: &[String],
        _: &[(String, String)],
        _: Option<std::time::Duration>,
        _: F,
    ) -> Result<CommandResult, InlineExecError>
    where
        F: FnMut() -> Result<(), crate::BrokerOpError>,
    {
        Err(InlineExecError::Setup(io::Error::new(
            io::ErrorKind::Unsupported,
            "process-group supervision requires a supported Unix platform",
        )))
    }

    #[cfg(not(unix))]
    pub(crate) fn run_with_timeout_and_dispatch<F, D>(
        self,
        cwd: &Path,
        command: &[String],
        environment: &[(String, String)],
        command_timeout: Option<std::time::Duration>,
        heartbeat: F,
        mut dispatch: D,
    ) -> Result<CommandResult, InlineExecError>
    where
        F: FnMut() -> Result<(), crate::BrokerOpError>,
        D: FnMut(CommandDispatchState),
    {
        dispatch(CommandDispatchState::NotStarted);
        self.run_with_timeout(cwd, command, environment, command_timeout, heartbeat)
    }

    #[cfg(unix)]
    fn timed_out_result(
        &mut self,
        command_started: bool,
    ) -> Result<CommandResult, InlineExecError> {
        self.abort_and_verify().map_err(|reason| {
            InlineExecError::CleanupUnknown(format!(
                "the execution deadline elapsed after command dispatch; its cleanup could not be verified: {reason}"
            ))
        })?;
        Ok(CommandResult {
            exit_code: None,
            succeeded: false,
            termination: CommandTermination::TimedOut,
            command_started,
        })
    }

    #[cfg(unix)]
    fn abort_and_verify(&mut self) -> Result<(), String> {
        if let Some(control) = self.control.as_mut()
            && let Err(error) = write_frame(control, &ParentFrameRef::Abort)
        {
            crate::warn_unrecorded("send process supervisor abort", Err::<(), _>(error));
        }
        self.terminate_and_verify()
    }

    #[cfg(unix)]
    fn terminate_and_verify(&mut self) -> Result<(), String> {
        if self.terminalized {
            return Ok(());
        }
        drop(self.control.take()); // EOF makes the helper abort any active child.
        let child = self.child.as_mut().ok_or_else(|| {
            "helper child handle disappeared before cleanup was verified".to_string()
        })?;
        let deadline = std::time::Instant::now() + HELPER_EXIT_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(None) => {
                    // The helper is still our unreaped direct child, so this
                    // signal cannot target a recycled pid. Its in-group watcher
                    // performs descendant cleanup after helper exit.
                    child
                        .kill()
                        .map_err(|error| format!("could not stop owned helper: {error}"))?;
                    let reap_deadline = std::time::Instant::now() + HELPER_EXIT_TIMEOUT;
                    loop {
                        match child.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) if std::time::Instant::now() < reap_deadline => {
                                std::thread::sleep(std::time::Duration::from_millis(10));
                            }
                            Ok(None) => {
                                return Err("owned helper did not reap after stop signal".into());
                            }
                            Err(error) => {
                                return Err(format!("could not reap owned helper: {error}"));
                            }
                        }
                    }
                    break;
                }
                Err(error) => return Err(format!("could not inspect owned helper: {error}")),
            }
        }
        wait_process_group_absent(self.process_group)?;
        if let Some(producer) = self.artifact_use.as_mut()
            && producer.is_unsettled_running()
        {
            let child = self.child.as_mut().ok_or_else(|| {
                "helper child handle disappeared before artifact-use settlement".to_string()
            })?;
            producer
                .finish_verified_quiescence(child)
                .map_err(|error| format!("could not settle artifact-use marker: {error}"))?;
        }
        drop(self.child.take());
        self.terminalized = true;
        Ok(())
    }

    #[cfg(unix)]
    fn terminate_not_started_and_verify(&mut self) -> Result<(), String> {
        self.terminate_and_verify()?;
        if let Some(producer) = self.artifact_use.as_mut()
            && producer.is_pre_dispatch()
        {
            producer.finish_not_started().map_err(|error| {
                format!("could not clear pre-dispatch artifact-use marker: {error}")
            })?;
        }
        Ok(())
    }
}

#[cfg(unix)]
impl Drop for InlineExecSupervisor {
    fn drop(&mut self) {
        if !self.terminalized
            && let Err(reason) = self.terminate_and_verify()
        {
            crate::warn_unrecorded(
                "clean up process supervisor during drop",
                Err::<(), _>(io::Error::other(reason)),
            );
        }
    }
}

/// Reserved protocol dispatch runs before ordinary CLI routing and before any
/// broker open. The exact version and argument shape are required.
pub(crate) fn dispatch(args: &[String]) -> Option<u8> {
    let first = args.first()?;
    if !first.starts_with("--aethyme-inline-exec-supervisor") {
        return None;
    }
    if args.len() != 1 || first != FLAG {
        return Some(125);
    }
    #[cfg(unix)]
    {
        Some(run_helper().unwrap_or(125))
    }
    #[cfg(not(unix))]
    Some(125)
}

#[cfg(unix)]
fn run_helper() -> io::Result<u8> {
    use std::os::fd::AsRawFd as _;
    use std::process::{Command, Stdio};

    close_unrelated_descriptors(3)?;
    let raw_control = 3;
    validate_control_socket(raw_control)?;
    // SAFETY: fd 3 was validated as a connected Unix stream socket, and
    // F_GETFD only reads its descriptor flags.
    let flags = unsafe { libc::fcntl(raw_control, libc::F_GETFD) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd 3 is still the validated, owned control socket; F_SETFD
    // updates only its close-on-exec flag.
    if unsafe { libc::fcntl(raw_control, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fd 3 is installed by the parent pre-exec hook as the unique
    // control endpoint. Ownership moves to this UnixStream exactly once.
    let mut control = unsafe { std::os::unix::net::UnixStream::from_raw_fd(raw_control) };
    // The helper never passes its parent control channel to a command or watcher.
    let mut watcher = crate::agent_supervisor::start_cleanup_watcher_for_inline()?;
    #[cfg(test)]
    if std::env::var_os("AETHYME_TEST_INLINE_FAIL_CLEANUP_WATCHER").is_some() {
        if let Err(error) = watcher.kill() {
            crate::warn_unrecorded("stop injected process cleanup watcher", Err::<(), _>(error));
        }
        if let Err(error) = watcher.wait() {
            crate::warn_unrecorded("reap injected process cleanup watcher", Err::<(), _>(error));
        }
    }
    write_frame(&mut control, &HelperFrame::Ready)?;
    let request: ParentFrame = loop {
        if !matches!(watcher.try_wait(), Ok(None)) {
            write_frame(
                &mut control,
                &HelperFrame::Failed {
                    reason: "cleanup watcher exited while awaiting command admission".into(),
                },
            )?;
            return Ok(125);
        }
        match poll_control(control.as_raw_fd(), 100)? {
            ControlPoll::Readable => {
                break read_frame_until(&mut control, std::time::Instant::now() + READY_TIMEOUT)?;
            }
            ControlPoll::Closed => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "parent closed the command channel before admission",
                ));
            }
            ControlPoll::Pending => {}
            ControlPoll::Writable => {
                return Err(io::Error::other(
                    "command-channel read poll returned writable readiness",
                ));
            }
        }
    };
    let ParentFrame::Run {
        cwd,
        command,
        environment,
        observation,
    } = request
    else {
        write_frame(
            &mut control,
            &HelperFrame::Failed {
                reason: "expected a single Run request after readiness".into(),
            },
        )?;
        return Ok(125);
    };
    if command.is_empty()
        || command[0].is_empty()
        || !cwd.is_absolute()
        || validate_environment(&environment).is_err()
    {
        write_frame(
            &mut control,
            &HelperFrame::Failed {
                reason: "invalid bounded command request".into(),
            },
        )?;
        return Ok(125);
    }
    let cwd = match cwd.canonicalize() {
        Ok(path) if path.is_dir() => path,
        Ok(_) => {
            write_frame(
                &mut control,
                &HelperFrame::Failed {
                    reason: "command working directory is not a directory".into(),
                },
            )?;
            return Ok(125);
        }
        Err(error) => {
            write_frame(
                &mut control,
                &HelperFrame::Failed {
                    reason: format!("cannot validate command working directory: {error}"),
                },
            )?;
            return Ok(125);
        }
    };
    let mut child_command = Command::new(&command[0]);
    child_command
        .args(&command[1..])
        .current_dir(cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .envs(
            environment
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
    if !matches!(watcher.try_wait(), Ok(None)) {
        write_frame(
            &mut control,
            &HelperFrame::Failed {
                reason: "cleanup watcher exited before command launch".into(),
            },
        )?;
        return Ok(125);
    }
    // Commands inherit this helper's process group. The cleanup watcher remains
    // in that same group and removes ordinary descendants after helper exit.
    let mut child = match child_command.spawn() {
        Ok(child) => child,
        Err(error) => {
            write_frame(
                &mut control,
                &HelperFrame::CommandStartFailed {
                    error: WireCommandStartError::capture(error),
                },
            )?;
            return Ok(125);
        }
    };
    if let Err(error) = write_frame(&mut control, &HelperFrame::Started) {
        stop_owned_command(&mut child);
        observe_direct_child(observation.as_ref(), &mut child, true);
        if let Err(cleanup_error) = terminate_owned_group_from_live_helper() {
            crate::warn_unrecorded(
                "stop command after start confirmation could not be delivered",
                Err::<(), _>(cleanup_error),
            );
        }
        return Err(error);
    }
    #[cfg(test)]
    if std::env::var_os("AETHYME_TEST_INLINE_FAIL_AFTER_LAUNCH").is_some() {
        std::thread::sleep(std::time::Duration::from_millis(100));
        stop_owned_command(&mut child);
        observe_direct_child(observation.as_ref(), &mut child, true);
        write_frame(
            &mut control,
            &HelperFrame::ExecutionUnknown {
                reason: "injected post-launch result loss".into(),
            },
        )?;
        return Ok(125);
    }
    loop {
        if !matches!(watcher.try_wait(), Ok(None)) {
            stop_owned_command(&mut child);
            observe_direct_child(observation.as_ref(), &mut child, true);
            if let Err(error) = write_frame(
                &mut control,
                &HelperFrame::ExecutionUnknown {
                    reason: "cleanup watcher exited before command cleanup was verified".into(),
                },
            ) {
                crate::warn_unrecorded(
                    "report process cleanup watcher failure",
                    Err::<(), _>(error),
                );
            }
            terminate_owned_group_from_live_helper()?;
            return Ok(125);
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                observe_direct_child(observation.as_ref(), &mut child, false);
                write_frame(
                    &mut control,
                    &HelperFrame::Exited {
                        code: status.code(),
                    },
                )?;
                return Ok(0);
            }
            Ok(None) => {}
            Err(error) => {
                stop_owned_command(&mut child);
                observe_direct_child(observation.as_ref(), &mut child, true);
                write_frame(
                    &mut control,
                    &HelperFrame::ExecutionUnknown {
                        reason: format!("cannot inspect command status: {error}"),
                    },
                )?;
                return Ok(125);
            }
        }
        let parent_frame = match read_frame_timeout::<ParentFrame>(
            &mut control,
            std::time::Duration::from_millis(100),
            None,
        ) {
            Ok(frame) => frame,
            Err(error) => {
                stop_owned_command(&mut child);
                observe_direct_child(observation.as_ref(), &mut child, true);
                return Err(error);
            }
        };
        if let Some(parent_frame) = parent_frame {
            match parent_frame {
                ParentFrame::Ping => {}
                ParentFrame::Abort => {
                    stop_owned_command(&mut child);
                    observe_direct_child(observation.as_ref(), &mut child, true);
                    write_frame(&mut control, &HelperFrame::Aborted)?;
                    return Ok(125);
                }
                ParentFrame::Run { .. } => {
                    stop_owned_command(&mut child);
                    observe_direct_child(observation.as_ref(), &mut child, true);
                    write_frame(
                        &mut control,
                        &HelperFrame::ExecutionUnknown {
                            reason: "received a second Run request".into(),
                        },
                    )?;
                    return Ok(125);
                }
            }
        }
    }
}

#[cfg(unix)]
fn observe_direct_child(
    target: Option<&crate::store::guarded_exec::HelperObservationTarget>,
    child: &mut std::process::Child,
    cancelled: bool,
) {
    let Some(target) = target else {
        return;
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            let outcome = if cancelled {
                "cancelled"
            } else if status.success() {
                "succeeded"
            } else {
                "failed"
            };
            crate::warn_unrecorded(
                "persist direct-child helper observation",
                target.record(outcome, status.code()),
            );
        }
        Ok(None) => {}
        Err(error) => crate::warn_unrecorded(
            "inspect direct child for helper observation",
            Err::<(), _>(error),
        ),
    }
}

#[cfg(unix)]
fn validate_control_socket(fd: RawFd) -> io::Result<()> {
    let mut socket_type: libc::c_int = 0;
    let mut type_length = std::mem::size_of_val(&socket_type) as libc::socklen_t;
    // SAFETY: these pointers reference initialized, writable stack values and
    // type_length gives the exact socket_type buffer size for getsockopt.
    if unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_TYPE,
            (&mut socket_type as *mut libc::c_int).cast(),
            &mut type_length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if socket_type != libc::SOCK_STREAM {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inline helper control descriptor is not a stream socket",
        ));
    }
    // SAFETY: sockaddr_storage contains only integer fields and padding, for
    // which the all-zero bit pattern is valid. Initialize it before the call.
    let mut peer: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut peer_length = std::mem::size_of_val(&peer) as libc::socklen_t;
    // SAFETY: peer and peer_length are writable stack values; peer_length
    // supplies the complete buffer capacity. getpeername also checks connection.
    if unsafe {
        libc::getpeername(
            fd,
            (&mut peer as *mut libc::sockaddr_storage).cast(),
            &mut peer_length,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if peer.ss_family as libc::c_int != libc::AF_UNIX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inline helper control descriptor is not a connected Unix socket",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn terminate_owned_group_from_live_helper() -> io::Result<()> {
    // SAFETY: the helper created this process group with its own pid as leader
    // and has not reaped itself. It therefore still pins the numeric group id.
    let (pid, group) = unsafe { (libc::getpid(), libc::getpgrp()) };
    if pid <= 1 || group != pid {
        return Err(io::Error::other(
            "cannot prove the inline helper still pins its process group",
        ));
    }
    // SAFETY: the helper is a live member and leader of this exact owned group;
    // SIGKILL stops remaining in-group writers without a recycled-id window.
    if unsafe { libc::killpg(group, libc::SIGKILL) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn stop_owned_command(child: &mut std::process::Child) -> bool {
    use std::time::{Duration, Instant};
    match child.try_wait() {
        Ok(Some(_)) => return true,
        Ok(None) => {}
        Err(error) => {
            crate::warn_unrecorded(
                "inspect supervised command before stop",
                Err::<(), _>(error),
            );
            return false;
        }
    }
    if let Err(error) = child.kill() {
        crate::warn_unrecorded("stop supervised command", Err::<(), _>(error));
        return false;
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                crate::warn_unrecorded(
                    "supervised command reap remains incomplete",
                    Err::<(), _>(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "owned command did not reap before cleanup deadline",
                    )),
                );
                return false;
            }
            Err(error) => {
                crate::warn_unrecorded("poll supervised command reap", Err::<(), _>(error));
                return false;
            }
        }
    }
}

#[cfg(unix)]
enum ControlPoll {
    Readable,
    Writable,
    Closed,
    Pending,
}

#[cfg(unix)]
fn poll_control(fd: RawFd, timeout_ms: i32) -> io::Result<ControlPoll> {
    poll_socket(fd, libc::POLLIN, timeout_ms)
}

#[cfg(unix)]
fn poll_writable(fd: RawFd, timeout_ms: i32) -> io::Result<ControlPoll> {
    poll_socket(fd, libc::POLLOUT, timeout_ms)
}

#[cfg(unix)]
fn poll_socket(fd: RawFd, events: libc::c_short, timeout_ms: i32) -> io::Result<ControlPoll> {
    let mut descriptor = libc::pollfd {
        fd,
        events,
        revents: 0,
    };
    // SAFETY: `descriptor` is a valid single-element array for the duration of
    // poll, and the fd is owned by the helper's control UnixStream.
    let result = unsafe { libc::poll(&mut descriptor, 1, timeout_ms) };
    if result < 0 {
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            return Ok(ControlPoll::Pending);
        }
        return Err(error);
    }
    if result == 0 {
        return Ok(ControlPoll::Pending);
    }
    if descriptor.revents & events != 0 {
        return Ok(if events == libc::POLLIN {
            ControlPoll::Readable
        } else {
            ControlPoll::Writable
        });
    }
    if descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
        return Ok(ControlPoll::Closed);
    }
    Ok(ControlPoll::Pending)
}

#[cfg(unix)]
fn close_unrelated_descriptors(control_fd: RawFd) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    let directory = "/proc/self/fd";
    #[cfg(target_os = "macos")]
    let directory = "/dev/fd";
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "inline helper supports only macOS and Linux descriptor enumeration",
    ));

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut entries = std::fs::read_dir(directory)?;
        let mut descriptors = Vec::new();
        for entry in entries.by_ref() {
            let entry = entry?;
            if let Some(number) = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse::<RawFd>().ok())
                && number > 2
                && number != control_fd
            {
                descriptors.push(number);
            }
        }
        // Drop the directory stream before closing its descriptor numbers; its
        // own number may have appeared in the enumeration and is now harmless.
        drop(entries);
        descriptors.sort_unstable();
        descriptors.dedup();
        for descriptor in descriptors {
            // SAFETY: the enumerated descriptor was open in this single-threaded
            // helper. No other code opens descriptors during this close loop.
            if unsafe { libc::close(descriptor) } != 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EBADF) {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

#[cfg(unix)]
fn wait_process_group_absent(process_group: i32) -> Result<(), String> {
    wait_process_group_absent_with_probe(GROUP_EXIT_TIMEOUT, || {
        // SAFETY: signal zero only probes existence and permission. The parent
        // never sends a signal to the group after waiting for its helper.
        if unsafe { libc::killpg(process_group, 0) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    })
}

#[cfg(unix)]
fn wait_process_group_absent_with_probe(
    timeout: std::time::Duration,
    mut probe: impl FnMut() -> io::Result<()>,
) -> Result<(), String> {
    use std::time::{Duration, Instant};
    let deadline = Instant::now() + timeout;
    loop {
        let pending_error = match probe() {
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => return Ok(()),
            // Darwin can report EPERM while an existing group contains only
            // exiting/zombie members. Retry within the same cleanup deadline;
            // permission denial is never itself evidence that the group is gone.
            Err(error) if error.raw_os_error() == Some(libc::EPERM) => Some(error),
            Err(error) => {
                return Err(format!(
                    "cannot verify helper process-group cleanup: {error}"
                ));
            }
            Ok(()) => None,
        };
        let now = Instant::now();
        if now >= deadline {
            return Err(match pending_error {
                Some(error) => {
                    format!("cannot verify helper process-group cleanup before deadline: {error}")
                }
                None => "helper process group remained present after cleanup deadline".into(),
            });
        }
        std::thread::sleep(Duration::from_millis(20).min(deadline - now));
    }
}

#[cfg(unix)]
fn validate_environment(environment: &[(String, String)]) -> io::Result<()> {
    for (name, value) in environment {
        if name.is_empty() || name.contains('=') || name.contains('\0') || value.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "supervisor environment contains an invalid variable",
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn move_control_fd_above_stdio(
    stream: std::os::unix::net::UnixStream,
) -> io::Result<std::os::unix::net::UnixStream> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    // SAFETY: F_DUPFD_CLOEXEC duplicates the owned socket descriptor into a
    // new descriptor at or above 4 and marks that duplicate close-on-exec.
    let duplicate = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 4) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: fcntl returned a new, uniquely owned descriptor for this socket.
    Ok(unsafe { std::os::unix::net::UnixStream::from_raw_fd(duplicate) })
}

#[cfg(unix)]
fn write_frame<T: serde::Serialize>(
    stream: &mut std::os::unix::net::UnixStream,
    value: &T,
) -> io::Result<()> {
    write_frame_until(stream, value, std::time::Instant::now() + READY_TIMEOUT)
}

#[cfg(unix)]
fn write_frame_until<T: serde::Serialize>(
    stream: &mut std::os::unix::net::UnixStream,
    value: &T,
    deadline: std::time::Instant,
) -> io::Result<()> {
    let payload = serialize_frame_payload(value)?;
    let length = frame_length(&payload)?;
    with_nonblocking(stream, |stream| {
        write_all_until(stream, FRAME_MAGIC, deadline)?;
        write_all_until(stream, &length.to_be_bytes(), deadline)?;
        write_all_until(stream, &payload, deadline)
    })
}

#[cfg(unix)]
fn serialize_frame_payload<T: serde::Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let mut payload = BoundedPayload {
        bytes: Vec::with_capacity(8 * 1024),
        limit: MAX_FRAME_BYTES,
    };
    serde_json::to_writer(&mut payload, value).map_err(io::Error::other)?;
    if payload.bytes.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "inline helper frame is outside its size limit",
        ));
    }
    Ok(payload.bytes)
}

#[cfg(unix)]
fn frame_length(payload: &[u8]) -> io::Result<u32> {
    u32::try_from(payload.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "inline helper frame length does not fit its header",
        )
    })
}

#[cfg(unix)]
struct BoundedPayload {
    bytes: Vec<u8>,
    limit: usize,
}

#[cfg(unix)]
impl io::Write for BoundedPayload {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let Some(next_length) = self.bytes.len().checked_add(bytes.len()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "inline helper frame length overflowed",
            ));
        };
        if next_length > self.limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "inline helper frame exceeds its size limit",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(unix)]
fn read_frame_until<T: serde::de::DeserializeOwned>(
    stream: &mut std::os::unix::net::UnixStream,
    deadline: std::time::Instant,
) -> io::Result<T> {
    with_nonblocking(stream, |stream| {
        let mut received_any = false;
        let mut magic = [0u8; 4];
        read_exact_until(stream, &mut magic, deadline, &mut received_any)?;
        read_frame_after_magic(stream, magic, deadline, &mut received_any)
    })
}

#[cfg(unix)]
fn read_frame_timeout<T: serde::de::DeserializeOwned>(
    stream: &mut std::os::unix::net::UnixStream,
    idle_timeout: std::time::Duration,
    command_deadline: Option<std::time::Instant>,
) -> io::Result<Option<T>> {
    with_nonblocking(stream, |stream| {
        let mut received_any = false;
        let mut magic = [0u8; 4];
        let idle_deadline = std::time::Instant::now() + idle_timeout;
        if let Err(error) =
            read_exact_until(stream, &mut magic[..1], idle_deadline, &mut received_any)
        {
            if !received_any && error.kind() == io::ErrorKind::TimedOut {
                return Ok(None);
            }
            return Err(error);
        }

        // The short timeout above only answers whether a new frame has begun.
        // Once its first byte arrives, keep one absolute deadline for the rest
        // of the frame so a stalled or dripping peer cannot extend it forever.
        let frame_deadline = command_deadline.map_or_else(
            || std::time::Instant::now() + FRAME_IO_TIMEOUT,
            |deadline| (std::time::Instant::now() + FRAME_IO_TIMEOUT).min(deadline),
        );
        read_exact_until(stream, &mut magic[1..], frame_deadline, &mut received_any)?;
        read_frame_after_magic(stream, magic, frame_deadline, &mut received_any).map(Some)
    })
}

#[cfg(unix)]
fn read_frame_after_magic<T: serde::de::DeserializeOwned>(
    stream: &mut std::os::unix::net::UnixStream,
    magic: [u8; 4],
    deadline: std::time::Instant,
    received_any: &mut bool,
) -> io::Result<T> {
    if &magic != FRAME_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inline helper frame has invalid version marker",
        ));
    }
    let mut length = [0u8; 4];
    read_exact_until(stream, &mut length, deadline, received_any)?;
    let length = u32::from_be_bytes(length) as usize;
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "inline helper frame exceeds its size limit",
        ));
    }
    let mut payload = vec![0; length];
    read_exact_until(stream, &mut payload, deadline, received_any)?;
    serde_json::from_slice(&payload).map_err(io::Error::other)
}

#[cfg(unix)]
fn read_exact_until(
    stream: &mut std::os::unix::net::UnixStream,
    mut remaining: &mut [u8],
    deadline: std::time::Instant,
    received_any: &mut bool,
) -> io::Result<()> {
    use std::io::Read as _;
    while !remaining.is_empty() {
        let timeout_ms = deadline_timeout_ms(deadline).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "inline helper frame deadline elapsed",
            )
        })?;
        match poll_control(stream.as_raw_fd(), timeout_ms)? {
            ControlPoll::Readable if std::time::Instant::now() < deadline => {
                match stream.read(remaining) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "inline helper closed its control channel mid-frame",
                        ));
                    }
                    Ok(count) => {
                        *received_any = true;
                        remaining = &mut remaining[count..];
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                }
            }
            ControlPoll::Readable => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "inline helper frame deadline elapsed",
                ));
            }
            ControlPoll::Closed => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "inline helper closed its control channel mid-frame",
                ));
            }
            ControlPoll::Writable => {
                return Err(io::Error::other("read poll returned writable readiness"));
            }
            ControlPoll::Pending => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_all_until(
    stream: &mut std::os::unix::net::UnixStream,
    mut remaining: &[u8],
    deadline: std::time::Instant,
) -> io::Result<()> {
    use std::io::Write as _;
    while !remaining.is_empty() {
        let timeout_ms = deadline_timeout_ms(deadline).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "inline helper write deadline elapsed",
            )
        })?;
        match poll_writable(stream.as_raw_fd(), timeout_ms)? {
            ControlPoll::Writable if std::time::Instant::now() < deadline => {
                match stream.write(remaining) {
                    Ok(0) => {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "inline helper control channel stopped accepting bytes",
                        ));
                    }
                    Ok(count) => remaining = &remaining[count..],
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => return Err(error),
                }
            }
            ControlPoll::Writable => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "inline helper write deadline elapsed",
                ));
            }
            ControlPoll::Closed => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "inline helper closed its control channel during a write",
                ));
            }
            ControlPoll::Readable => {
                return Err(io::Error::other("write poll returned readable readiness"));
            }
            ControlPoll::Pending => {}
        }
    }
    Ok(())
}

#[cfg(unix)]
fn deadline_timeout_ms(deadline: std::time::Instant) -> Option<i32> {
    let remaining = deadline.checked_duration_since(std::time::Instant::now())?;
    if remaining.is_zero() {
        return None;
    }
    let milliseconds = remaining.as_millis();
    let rounded = milliseconds
        + if remaining.subsec_nanos() % 1_000_000 != 0 {
            1
        } else {
            0
        };
    Some(rounded.clamp(1, i32::MAX as u128) as i32)
}

#[cfg(unix)]
fn with_nonblocking<T>(
    stream: &mut std::os::unix::net::UnixStream,
    operation: impl FnOnce(&mut std::os::unix::net::UnixStream) -> io::Result<T>,
) -> io::Result<T> {
    // SAFETY: F_GETFL reads the status flags of the owned UnixStream descriptor.
    let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let was_nonblocking = flags & libc::O_NONBLOCK != 0;
    if !was_nonblocking {
        stream.set_nonblocking(true)?;
    }
    let result = operation(stream);
    let restore = if was_nonblocking {
        Ok(())
    } else {
        stream.set_nonblocking(false)
    };
    match (result, restore) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(error), Err(restore_error)) => {
            crate::warn_unrecorded(
                "restore inline helper socket mode",
                Err::<(), _>(restore_error),
            );
            Err(error)
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn dispatch(args: &[String]) -> Option<u8> {
    let first = args.first()?;
    first
        .starts_with("--aethyme-inline-exec-supervisor")
        .then_some(125)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn generic_mode_supervises_only_supported_hosts_without_a_controlling_terminal() {
        assert_eq!(
            select_generic_exec_mode(true, Ok(false)),
            GenericExecMode::Supervised
        );
        assert_eq!(
            select_generic_exec_mode(false, Ok(false)),
            GenericExecMode::Legacy(
                "process-group supervision supports only macOS and Linux".into()
            )
        );
        assert_eq!(
            select_generic_exec_mode(true, Ok(true)),
            GenericExecMode::Legacy("a controlling terminal is available".into())
        );
        assert_eq!(
            select_generic_exec_mode(true, Err("permission denied".into())),
            GenericExecMode::Legacy(
                "could not determine whether a controlling terminal is available: permission denied".into()
            )
        );
    }

    #[test]
    fn guarded_exec_timeout_is_defaulted_only_for_supervised_mode() {
        use std::time::Duration;

        assert_eq!(
            select_guarded_exec_timeout(&GenericExecMode::Supervised, None).unwrap(),
            Some(Duration::from_secs(30 * 60))
        );
        assert_eq!(
            select_guarded_exec_timeout(
                &GenericExecMode::Supervised,
                Some(Duration::from_secs(17))
            )
            .unwrap(),
            Some(Duration::from_secs(17))
        );
        assert_eq!(
            select_guarded_exec_timeout(&GenericExecMode::Legacy("tty".into()), None).unwrap(),
            None
        );
        assert!(
            select_guarded_exec_timeout(
                &GenericExecMode::Legacy("tty".into()),
                Some(Duration::from_secs(17))
            )
            .unwrap_err()
            .contains("tty")
        );
    }

    #[test]
    fn inline_helper_child_fixture() {
        if std::env::var_os("AETHYME_TEST_INLINE_EXEC_SUPERVISOR").is_some() {
            std::process::exit(super::run_helper().unwrap_or(125).into());
        }
    }

    fn start_helper() -> InlineExecSupervisor {
        InlineExecSupervisor::start(&std::env::current_exe().unwrap()).unwrap()
    }

    fn start_helper_with_watcher_failure() -> InlineExecSupervisor {
        InlineExecSupervisor::start_with_watcher_fault(
            &std::env::current_exe().unwrap(),
            true,
            false,
        )
        .unwrap()
    }

    fn start_helper_with_post_launch_failure() -> InlineExecSupervisor {
        InlineExecSupervisor::start_with_watcher_fault(
            &std::env::current_exe().unwrap(),
            false,
            true,
        )
        .unwrap()
    }

    #[test]
    fn artifact_use_marker_settles_after_owned_helper_group_quiescence() {
        let primary = tempfile::tempdir().unwrap();
        let checkout = primary.path().join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let producer = match crate::file_lock::begin_artifact_use(
            primary.path(),
            &checkout,
            41,
            "supervised-test:41:child-settlement",
        )
        .unwrap()
        {
            crate::file_lock::ArtifactUseAdmission::Admitted(producer) => producer,
            crate::file_lock::ArtifactUseAdmission::ProtocolUnavailable => {
                panic!("Unix test filesystem should support directory sync")
            }
        };

        let result = start_helper()
            .with_artifact_use(Some(producer))
            .run(
                &checkout,
                &["/bin/sh".into(), "-c".into(), "touch produced".into()],
                &[],
                || Ok(()),
            )
            .unwrap();

        assert!(result.succeeded);
        assert!(checkout.join("produced").exists());
        assert!(matches!(
            crate::file_lock::try_artifact_use_reclaimer(primary.path(), &checkout).unwrap(),
            crate::file_lock::ArtifactUseReclaimTry::Acquired(_)
        ));
    }

    #[test]
    fn group_cleanup_retries_permission_denial_until_absence_is_observed() {
        let mut probes = 0;
        wait_process_group_absent_with_probe(std::time::Duration::from_secs(1), || {
            probes += 1;
            Err(io::Error::from_raw_os_error(if probes == 1 {
                libc::EPERM
            } else {
                libc::ESRCH
            }))
        })
        .unwrap();
        assert_eq!(probes, 2, "only ESRCH can establish group absence");
    }

    #[test]
    fn group_cleanup_permission_denial_at_deadline_remains_unknown() {
        let mut probes = 0;
        let error = wait_process_group_absent_with_probe(std::time::Duration::ZERO, || {
            probes += 1;
            Err(io::Error::from_raw_os_error(libc::EPERM))
        })
        .unwrap_err();
        assert_eq!(probes, 1);
        assert!(error.contains("cannot verify helper process-group cleanup before deadline"));
        assert!(error.contains(&io::Error::from_raw_os_error(libc::EPERM).to_string()));
    }

    #[test]
    fn group_cleanup_other_probe_errors_remain_immediately_unknown() {
        let mut probes = 0;
        let error = wait_process_group_absent_with_probe(std::time::Duration::from_secs(1), || {
            probes += 1;
            Err(io::Error::from_raw_os_error(libc::EIO))
        })
        .unwrap_err();
        assert_eq!(probes, 1);
        assert!(error.contains("cannot verify helper process-group cleanup:"));
        assert!(error.contains(&io::Error::from_raw_os_error(libc::EIO).to_string()));
    }

    #[test]
    fn helper_preserves_argv_environment_stdio_and_exit_status() {
        let owner = tempfile::tempdir().unwrap();
        let output = owner.path().join("argv.txt");
        let supervisor = start_helper();
        let result = supervisor
            .run(
                owner.path(),
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "printf '%s:%s:%s:%s' \"$FIRST_ARG\" \"$SECOND_ARG\" \"$1\" \"$2\" > \"$OUTPUT_PATH\"; exit 7".into(),
                    "shell-name".into(),
                    "".into(),
                    "literal\nargument".into(),
                ],
                &[
                    ("FIRST_ARG".into(), "one two".into()),
                    ("SECOND_ARG".into(), "three".into()),
                    (
                        "OUTPUT_PATH".into(),
                        output.to_string_lossy().into_owned(),
                    ),
                ],
                || Ok(()),
            )
            .unwrap();

        assert_eq!(result.exit_code, Some(7));
        assert!(!result.succeeded);
        assert_eq!(result.termination, CommandTermination::Exited);
        assert_eq!(
            std::fs::read_to_string(output).unwrap(),
            "one two:three::literal\nargument"
        );
    }

    #[test]
    fn command_deadline_includes_time_spent_in_heartbeat() {
        use std::time::Duration;

        let owner = tempfile::tempdir().unwrap();
        let result = start_helper()
            .run_with_timeout(
                owner.path(),
                &["/bin/sh".into(), "-c".into(), "sleep 30".into()],
                &[],
                Some(Duration::from_millis(100)),
                || {
                    std::thread::sleep(Duration::from_millis(200));
                    Ok(())
                },
            )
            .unwrap();

        assert_eq!(result.termination, CommandTermination::TimedOut);
        assert_eq!(result.exit_code, None);
        assert!(!result.succeeded);
        assert!(
            !result.command_started,
            "deadline expiry before the parent reads Started must remain unconfirmed"
        );
    }

    #[test]
    fn inherited_pipe_stdin_reaches_the_supervised_command() {
        use std::io::Write as _;
        use std::os::fd::FromRawFd as _;
        use std::process::Stdio;

        let owner = tempfile::tempdir().unwrap();
        let output = owner.path().join("stdin.txt");
        let mut fds = [-1; 2];
        // SAFETY: `fds` points to two writable `c_int` slots for `pipe` to fill.
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        // SAFETY: successful `pipe` initialized `fds[0]` as an owned read fd;
        // `File` takes responsibility for closing it exactly once.
        let reader = unsafe { std::fs::File::from_raw_fd(fds[0]) };
        // SAFETY: successful `pipe` initialized `fds[1]` as an owned write fd;
        // `File` takes responsibility for closing it exactly once.
        let mut writer = unsafe { std::fs::File::from_raw_fd(fds[1]) };
        writer.write_all(b"pipe payload\n").unwrap();
        drop(writer);

        let supervisor = InlineExecSupervisor::start_with_stdin(
            &std::env::current_exe().unwrap(),
            Stdio::from(reader),
            Stdio::inherit(),
            false,
            false,
        )
        .unwrap();
        let result = supervisor
            .run(
                owner.path(),
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    r#"IFS= read -r value; printf '%s' "$value" > "$OUTPUT_PATH""#.into(),
                ],
                &[("OUTPUT_PATH".into(), output.to_string_lossy().into_owned())],
                || Ok(()),
            )
            .unwrap();

        assert!(result.succeeded);
        assert_eq!(std::fs::read_to_string(output).unwrap(), "pipe payload");
    }

    #[test]
    fn preparation_start_keeps_child_stdin_null() {
        let owner = tempfile::tempdir().unwrap();
        let output = owner.path().join("stdin.txt");
        let result = start_helper()
            .run(
                owner.path(),
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    r#"IFS= read -r value; printf 'eof:%s' "$?" > "$OUTPUT_PATH""#.into(),
                ],
                &[("OUTPUT_PATH".into(), output.to_string_lossy().into_owned())],
                || Ok(()),
            )
            .unwrap();

        assert!(result.succeeded);
        assert_eq!(std::fs::read_to_string(output).unwrap(), "eof:1");
    }

    #[test]
    fn helper_removes_in_group_descendant_before_returning() {
        use std::time::Duration;

        let owner = tempfile::tempdir().unwrap();
        let output = owner.path().join("descendant.txt");
        let ready = owner.path().join("descendant-ready");
        let result = start_helper()
            .run(
                owner.path(),
                &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "(printf x >> \"$OUTPUT_PATH\"; : > \"$READY_PATH\"; i=0; while [ \"$i\" -lt 500 ]; do printf x >> \"$OUTPUT_PATH\"; sleep 0.02; i=$((i + 1)); done) & i=0; while [ ! -e \"$READY_PATH\" ] && [ \"$i\" -lt 100 ]; do sleep 0.01; i=$((i + 1)); done; exit 0".into(),
                ],
                &[
                    ("OUTPUT_PATH".into(), output.to_string_lossy().into_owned()),
                    ("READY_PATH".into(), ready.to_string_lossy().into_owned()),
                ],
                || Ok(()),
            )
            .unwrap();

        assert!(result.succeeded);
        let bytes_after_return = std::fs::metadata(&output).unwrap().len();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            std::fs::metadata(output).unwrap().len(),
            bytes_after_return,
            "an in-group descendant kept writing after the helper returned"
        );
    }

    #[test]
    fn failed_cleanup_watcher_prevents_command_launch() {
        let owner = tempfile::tempdir().unwrap();
        let marker = owner.path().join("must-not-exist");
        let result = start_helper_with_watcher_failure().run(
            owner.path(),
            &[
                "/bin/sh".into(),
                "-c".into(),
                "printf launched > \"$MARKER_PATH\"".into(),
            ],
            &[("MARKER_PATH".into(), marker.to_string_lossy().into_owned())],
            || Ok(()),
        );

        assert!(matches!(result, Err(InlineExecError::Setup(_))));
        assert!(
            !marker.exists(),
            "command launched without a cleanup watcher"
        );
    }

    #[test]
    fn post_launch_result_loss_is_not_reported_as_setup_failure() {
        let result = start_helper_with_post_launch_failure().run(
            Path::new("/"),
            &["/bin/sleep".into(), "30".into()],
            &[],
            || Ok(()),
        );

        assert!(matches!(
            result,
            Err(InlineExecError::CommandOutcomeUnknown(_))
        ));
    }

    #[test]
    fn ready_helper_waits_past_startup_timeout_for_admission() {
        use std::time::Duration;

        let supervisor = start_helper();
        std::thread::sleep(READY_TIMEOUT + Duration::from_millis(100));
        let result = supervisor
            .run(
                Path::new("/"),
                &["/bin/sh".into(), "-c".into(), "exit 0".into()],
                &[],
                || Ok(()),
            )
            .unwrap();

        assert!(result.succeeded);
    }

    #[test]
    fn idle_parent_disconnect_exits_helper_and_reaps_watcher() {
        use std::time::{Duration, Instant};

        let mut supervisor = start_helper();
        drop(supervisor.control.take());
        let child = supervisor.child.as_mut().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => panic!("idle helper did not exit after parent disconnect"),
                Err(error) => panic!("could not reap idle helper: {error}"),
            }
        }
        supervisor.child.take();
        wait_process_group_absent(supervisor.process_group).unwrap();
        supervisor.terminalized = true;
    }

    #[test]
    fn unavailable_observation_write_does_not_replace_the_command_result() {
        let (_owner, store, receipt, target) =
            crate::store::guarded_exec::helper_observation_fixture();
        store.connection().execute_batch("BEGIN IMMEDIATE").unwrap();
        let result = start_helper()
            .with_observation(Some(target))
            .run(
                Path::new("/"),
                &["/bin/sh".into(), "-c".into(), "exit 7".into()],
                &[],
                || Ok(()),
            )
            .unwrap();
        store.connection().execute_batch("ROLLBACK").unwrap();
        assert_eq!(result.exit_code, Some(7));
        assert!(
            store
                .guarded_exec_receipt(1, &receipt)
                .unwrap()
                .unwrap()
                .helper_observation
                .is_none()
        );
    }

    #[test]
    fn direct_child_exit_is_persisted_before_parent_completion() {
        let (_owner, store, receipt, target) =
            crate::store::guarded_exec::helper_observation_fixture();
        let result = start_helper()
            .with_observation(Some(target))
            .run(
                Path::new("/"),
                &["/bin/sh".into(), "-c".into(), "exit 7".into()],
                &[],
                || Ok(()),
            )
            .unwrap();
        assert_eq!(result.exit_code, Some(7));
        let row = store.guarded_exec_receipt(1, &receipt).unwrap().unwrap();
        assert_eq!(row.phase, "running");
        let observed = row.helper_observation.unwrap();
        assert_eq!(observed.direct_child_outcome, "failed");
        assert_eq!(observed.exit_code, Some(7));
        assert_eq!(observed.process_group_cleanup, "unknown");
    }

    #[test]
    fn control_disconnect_stops_command_and_waits_for_group_cleanup() {
        use std::time::{Duration, Instant};

        let owner = tempfile::tempdir().unwrap();
        let output = owner.path().join("disconnected-descendant.txt");
        let (_db_owner, store, receipt, observation) =
            crate::store::guarded_exec::helper_observation_fixture();
        let mut supervisor = start_helper();
        write_frame_until(
            supervisor.control.as_mut().unwrap(),
            &ParentFrameRef::Run {
                cwd: &owner.path().canonicalize().unwrap(),
                command: &[
                    "/bin/sh".into(),
                    "-c".into(),
                    "(i=0; while [ \"$i\" -lt 500 ]; do printf x >> \"$OUTPUT_PATH\"; sleep 0.02; i=$((i + 1)); done) & wait".into(),
                ],
                environment: &[("OUTPUT_PATH".into(), output.to_string_lossy().into_owned())],
                observation: Some(&observation),
            },
            Instant::now() + READY_TIMEOUT,
        )
        .unwrap();
        let start_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::metadata(&output) {
                Ok(metadata) if metadata.len() > 0 => break,
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => panic!("cannot inspect descendant output: {error}"),
            }
            assert!(
                Instant::now() < start_deadline,
                "descendant writer did not start before control disconnect"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(supervisor.control.take());

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match supervisor.child.as_mut().unwrap().try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => panic!("helper did not exit after control disconnect"),
                Err(error) => panic!("could not reap disconnected helper: {error}"),
            }
        }
        supervisor.child.take();
        wait_process_group_absent(supervisor.process_group).unwrap();
        supervisor.terminalized = true;

        let bytes_after_cleanup = std::fs::metadata(&output).unwrap().len();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            std::fs::metadata(output).unwrap().len(),
            bytes_after_cleanup,
            "the cleanup watcher left an in-group writer alive"
        );
        let row = store.guarded_exec_receipt(1, &receipt).unwrap().unwrap();
        assert_eq!(
            row.phase, "running",
            "the helper cannot complete the parent audit"
        );
        assert_eq!(row.command_outcome, "unknown");
        let observed = row
            .helper_observation
            .expect("caller loss must retain the direct-child cancellation observation");
        assert_eq!(observed.direct_child_outcome, "cancelled");
        assert_eq!(observed.process_group_cleanup, "unknown");
        assert_eq!(observed.ownership_audit, "not_run_by_helper");
    }

    #[test]
    fn malformed_reserved_protocol_fails_closed_before_routing() {
        assert_eq!(
            dispatch(&["--aethyme-inline-exec-supervisor-v1".into()]),
            Some(125)
        );
        assert_eq!(
            dispatch(&["--aethyme-inline-exec-supervisor-v2".into()]),
            Some(125)
        );
        assert_eq!(
            dispatch(&["--aethyme-inline-exec-supervisor-v3".into()]),
            Some(125)
        );
        assert_eq!(dispatch(&[FLAG.into(), "extra".into()]), Some(125));
        assert_eq!(dispatch(&["status".into()]), None);
    }

    #[test]
    fn command_start_wire_error_preserves_raw_os_and_non_os_details() {
        let raw = WireCommandStartError::capture(io::Error::from_raw_os_error(libc::ENOENT));
        let raw =
            serde_json::from_slice::<WireCommandStartError>(&serde_json::to_vec(&raw).unwrap())
                .unwrap()
                .into_io_error()
                .unwrap();
        assert_eq!(raw.raw_os_error(), Some(libc::ENOENT));
        assert_eq!(raw.kind(), io::ErrorKind::NotFound);

        let non_os = WireCommandStartError::capture(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argument contains NUL",
        ));
        let non_os =
            serde_json::from_slice::<WireCommandStartError>(&serde_json::to_vec(&non_os).unwrap())
                .unwrap()
                .into_io_error()
                .unwrap();
        assert_eq!(non_os.raw_os_error(), None);
        assert_eq!(non_os.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(non_os.to_string(), "argument contains NUL");

        assert!(
            serde_json::from_str::<WireIoErrorKind>(r#"{"future_wire_tag":{}}"#).is_err(),
            "unknown wire tags must fail closed"
        );
    }

    #[test]
    fn idle_poll_timeout_does_not_limit_partial_frame_assembly() {
        use std::io::Write as _;
        use std::os::fd::AsRawFd as _;
        use std::time::{Duration, Instant};

        let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let payload = serialize_frame_payload(&HelperFrame::Ready).unwrap();
        let length = frame_length(&payload).unwrap();
        let mut encoded = Vec::with_capacity(8 + payload.len());
        encoded.extend_from_slice(FRAME_MAGIC);
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&payload);

        // Queue the first byte before starting the reader, then wait until the
        // reader consumed it. The remaining bytes arrive after its 100 ms idle
        // poll interval but well within the fixed full-frame budget.
        writer.write_all(&encoded[..1]).unwrap();
        let remainder = encoded[1..].to_vec();
        let reader_fd = reader.as_raw_fd();
        let reader_thread = std::thread::spawn(move || {
            read_frame_timeout::<HelperFrame>(&mut reader, Duration::from_millis(100), None)
        });

        let consume_deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match poll_control(reader_fd, 0).unwrap() {
                ControlPoll::Pending => break,
                ControlPoll::Readable if Instant::now() < consume_deadline => {
                    std::thread::sleep(Duration::from_millis(1));
                }
                ControlPoll::Readable => panic!("reader did not consume the first frame byte"),
                ControlPoll::Writable | ControlPoll::Closed => {
                    panic!("unexpected reader socket state while consuming frame")
                }
            }
        }
        std::thread::sleep(Duration::from_millis(150));
        let delivery = writer.write_all(&remainder);
        let frame = reader_thread.join().unwrap();
        assert!(
            matches!(frame.as_ref(), Ok(Some(HelperFrame::Ready))),
            "reader rejected a delayed but valid frame: {frame:?}"
        );
        assert!(
            delivery.is_ok(),
            "writer could not deliver the delayed frame remainder: {delivery:?}"
        );
    }

    #[test]
    fn incomplete_partial_frame_hits_fixed_assembly_deadline() {
        use std::io::Write as _;
        use std::time::{Duration, Instant};

        let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let payload = serialize_frame_payload(&HelperFrame::Ready).unwrap();
        let length = frame_length(&payload).unwrap();
        let mut encoded = Vec::with_capacity(8 + payload.len());
        encoded.extend_from_slice(FRAME_MAGIC);
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&payload);
        writer.write_all(&encoded[..1]).unwrap();

        let started = Instant::now();
        let error =
            read_frame_timeout::<HelperFrame>(&mut reader, Duration::from_millis(100), None)
                .unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(
            elapsed >= FRAME_IO_TIMEOUT,
            "partial-frame deadline ended too early: {elapsed:?}"
        );
        assert!(
            elapsed < FRAME_IO_TIMEOUT + Duration::from_secs(5),
            "partial-frame deadline was not enforced promptly: {elapsed:?}"
        );
        // Keep the writer alive through the timeout so this exercises an
        // incomplete frame, not a peer disconnect.
        drop(writer);
    }

    #[test]
    fn frame_deadlines_cover_partial_reads_and_blocked_writes() {
        use std::io::Write as _;
        use std::os::fd::AsRawFd as _;
        use std::time::{Duration, Instant};

        let (mut reader, mut writer) = std::os::unix::net::UnixStream::pair().unwrap();
        assert!(
            read_frame_timeout::<HelperFrame>(&mut reader, Duration::from_millis(10), None)
                .unwrap()
                .is_none()
        );
        let payload = serialize_frame_payload(&HelperFrame::Ready).unwrap();
        let length = frame_length(&payload).unwrap();
        let mut encoded = Vec::with_capacity(8 + payload.len());
        encoded.extend_from_slice(FRAME_MAGIC);
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(&payload);
        let dripper = std::thread::spawn(move || {
            for byte in encoded {
                match writer.write_all(&[byte]) {
                    Ok(()) => std::thread::sleep(Duration::from_millis(15)),
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::BrokenPipe | io::ErrorKind::ConnectionReset
                        ) =>
                    {
                        return;
                    }
                    Err(error) => panic!("drip writer failed unexpectedly: {error}"),
                }
            }
        });
        let started = Instant::now();
        let error =
            read_frame_until::<HelperFrame>(&mut reader, started + Duration::from_millis(60))
                .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(200));
        drop(reader);
        dripper.join().unwrap();

        let (mut blocked_writer, _receiver) = std::os::unix::net::UnixStream::pair().unwrap();
        let send_buffer: libc::c_int = 4096;
        // SAFETY: setsockopt reads the initialized integer and mutates only the
        // sender socket's bounded send-buffer option.
        let result = unsafe {
            libc::setsockopt(
                blocked_writer.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&send_buffer as *const libc::c_int).cast(),
                std::mem::size_of_val(&send_buffer) as libc::socklen_t,
            )
        };
        assert_eq!(result, 0, "could not set deterministic socket send buffer");
        let large_argument = "x".repeat(MAX_FRAME_BYTES - 256);
        let command = ["/bin/true".to_owned(), large_argument];
        let request = ParentFrameRef::Run {
            cwd: Path::new("/"),
            command: &command,
            environment: &[],
            observation: None,
        };
        let started = Instant::now();
        let error = write_frame_until(
            &mut blocked_writer,
            &request,
            started + Duration::from_millis(60),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(200));

        let (mut slow_writer, mut slow_reader) = std::os::unix::net::UnixStream::pair().unwrap();
        // SAFETY: setsockopt reads the initialized integer and mutates only the
        // sender socket's bounded send-buffer option.
        let result = unsafe {
            libc::setsockopt(
                slow_writer.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                (&send_buffer as *const libc::c_int).cast(),
                std::mem::size_of_val(&send_buffer) as libc::socklen_t,
            )
        };
        assert_eq!(result, 0, "could not set slow-reader send buffer");
        let slow_drain = std::thread::spawn(move || {
            use std::io::Read as _;
            let mut chunk = [0u8; 1024];
            loop {
                match slow_reader.read(&mut chunk) {
                    Ok(0) => return,
                    Ok(_) => std::thread::sleep(Duration::from_millis(15)),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => panic!("slow frame reader failed unexpectedly: {error}"),
                }
            }
        });
        let started = Instant::now();
        let error = write_frame_until(
            &mut slow_writer,
            &request,
            started + Duration::from_millis(60),
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(200));
        drop(slow_writer);
        slow_drain.join().unwrap();
    }

    #[test]
    fn heartbeat_failure_stops_descendant_writers_before_returning() {
        use std::time::{Duration, Instant};

        let owner = tempfile::tempdir().unwrap();
        let output = owner.path().join("heartbeat-descendant.txt");
        let mut ticks = 0;
        let result = start_helper().run(
            owner.path(),
            &[
                "/bin/sh".into(),
                "-c".into(),
                    "(i=0; while [ \"$i\" -lt 500 ]; do printf x >> \"$OUTPUT_PATH\"; sleep 0.02; i=$((i + 1)); done) & wait".into(),
            ],
            &[("OUTPUT_PATH".into(), output.to_string_lossy().into_owned())],
            || {
                ticks += 1;
                if ticks == 1 {
                    Ok(())
                } else {
                    let deadline = Instant::now() + Duration::from_secs(2);
                    loop {
                        match std::fs::metadata(&output) {
                            Ok(metadata) if metadata.len() > 0 => break,
                            Ok(_) => {}
                            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                            Err(error) => panic!("cannot inspect heartbeat output: {error}"),
                        }
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(crate::PreparationError::LivenessDeadlineElapsed.into())
                }
            },
        );

        assert!(matches!(
            result,
            Err(InlineExecError::OperationAfterDispatch(_))
        ));
        let bytes_after_cleanup = std::fs::metadata(&output).unwrap().len();
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(
            std::fs::metadata(output).unwrap().len(),
            bytes_after_cleanup,
            "a descendant writer survived heartbeat-triggered cleanup"
        );
    }
}
