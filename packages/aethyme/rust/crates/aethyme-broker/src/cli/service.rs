//! Explicit opt-in service commands; ordinary broker requests remain local.

use super::*;
use crate::service::{job_protocol as wire, jobs::JobOperation};

pub(super) const SERVICE_USAGE: &str = "  aethyme broker advanced service ensure <private-service-root> --jobs-for <repository> [--json]\n  aethyme broker advanced service directory <private-service-root> --jobs-for <repository> [--json]\n  aethyme broker advanced service serve <private-runtime-directory> [--jobs-for <repository>]\n  aethyme broker advanced service ping <private-runtime-directory> [--json]\n  aethyme broker advanced service repository <private-runtime-directory> [--json]\n  aethyme broker advanced service submit <private-runtime-directory> --request-key <key> --deadline-ms <utc-ms> [--full-scan] [--include-active-gate-cache] [--json]\n  aethyme broker advanced service lookup <private-runtime-directory> <request-key> [--json]\n  aethyme broker advanced service status <private-runtime-directory> <job-id> [--json]\n  aethyme broker advanced service cancel <private-runtime-directory> <job-id> [--json]\n  aethyme broker advanced service result <private-runtime-directory> <job-id> [--json]\n      Explicit foreground service; existing broker operations stay local.\n      Create an absolute canonical directory owned by this user, mode 0700.\n      Jobs require an initialized repository and only plan GC; they never apply it.\n      Keep the request key and exact deadline for durable retry or lookup.\n      Stop with Ctrl-C or your process supervisor; no implicit activation.\n";

enum Command {
    Serve { repository: Option<PathBuf> },
    Directory { repository: PathBuf },
    Ensure { repository: PathBuf },
    Ping,
    Job(wire::Method),
    Result { job_id: i64 },
}

// Validate the complete command before opening a socket or creating files.
fn parse(args: &[String]) -> Option<Command> {
    let (command, rest) = args.split_first()?;
    let (_, options) = rest.split_first()?;
    let options = options
        .strip_suffix(&["--json".to_owned()])
        .unwrap_or(options);
    match (command.as_str(), options) {
        ("directory" | "ensure", [flag, repository])
            if flag == "--jobs-for" && !repository.is_empty() =>
        {
            let repository = repository.into();
            Some(if command == "directory" {
                Command::Directory { repository }
            } else {
                Command::Ensure { repository }
            })
        }
        ("serve", []) if args.len() == 2 => Some(Command::Serve { repository: None }),
        ("serve", [flag, repository]) if flag == "--jobs-for" && args.len() == 4 => {
            Some(Command::Serve {
                repository: Some(repository.into()),
            })
        }
        ("ping", []) => Some(Command::Ping),
        ("repository", []) => Some(Command::Job(wire::Method::Repository {})),
        ("lookup", [key]) if !key.is_empty() => Some(Command::Job(wire::Method::Lookup {
            idempotency_key: key.clone(),
        })),
        ("status" | "cancel" | "result", [id]) => {
            let job_id: i64 = id.parse().ok().filter(|id| *id > 0)?;
            Some(match command.as_str() {
                "status" => Command::Job(wire::Method::Status { job_id }),
                "cancel" => Command::Job(wire::Method::Cancel { job_id }),
                _ => Command::Result { job_id },
            })
        }
        ("submit", options) => {
            let (mut key, mut deadline, mut full_scan, mut include_active_gate_cache) =
                (None, None, false, false);
            let mut options = options.iter();
            while let Some(flag) = options.next() {
                match flag.as_str() {
                    "--request-key" if key.is_none() => key = Some(options.next()?.clone()),
                    "--deadline-ms" if deadline.is_none() => {
                        deadline = Some(options.next()?.parse::<i64>().ok()?)
                    }
                    "--full-scan" if !full_scan => full_scan = true,
                    "--include-active-gate-cache" if !include_active_gate_cache => {
                        include_active_gate_cache = true
                    }
                    _ => return None,
                }
            }
            let idempotency_key = key.filter(|key| !key.is_empty())?;
            Some(Command::Job(wire::Method::Submit {
                idempotency_key,
                deadline_ms: deadline?,
                operation: JobOperation::GcPlan {
                    include_active_gate_cache,
                    full_scan,
                },
            }))
        }
        _ => None,
    }
}

pub(super) fn run_service(args: &[String]) -> u8 {
    let Some(command) = parse(args) else {
        return report_dispatch_error(args, crate::exit_status::USAGE, SERVICE_USAGE);
    };
    #[cfg(unix)]
    {
        match execute(Path::new(&args[1]), command) {
            Ok(()) => crate::exit_status::SUCCESS,
            Err(error) => {
                eprintln!("broker service: {error}");
                crate::exit_status::ENVIRONMENT
            }
        }
    }
    #[cfg(not(unix))]
    {
        match command {
            Command::Serve { repository } => drop(repository),
            Command::Directory { repository } => drop(repository),
            Command::Ensure { repository } => drop(repository),
            Command::Job(method) => drop(method),
            Command::Result { job_id } => {
                let _id = job_id;
            }
            Command::Ping => {}
        }
        report_dispatch_error(
            args,
            crate::exit_status::ENVIRONMENT,
            "broker service requires Unix sockets",
        )
    }
}

#[cfg(unix)]
fn execute(directory: &Path, command: Command) -> std::io::Result<()> {
    use crate::service::{job_runtime::JobRuntime, transport};
    match command {
        Command::Ensure { repository } => {
            let report =
                crate::service::startup::ensure(directory, &repository, &std::env::current_exe()?)?;
            out!("{}", serde_json::to_string(&report)?);
            Ok(())
        }
        Command::Directory { repository } => {
            let location = crate::service::discovery::locate(directory, &repository)?;
            out!("{}", serde_json::to_string(&location)?);
            Ok(())
        }
        Command::Serve { repository } => {
            let _signals = ServiceSignals::install()?;
            let executable = std::env::current_exe()?;
            // The service must pin the exact managed router/engine pair for
            // its whole lifetime so an update cannot retire its engine.
            let _runtime_pin = crate::pin_runtime_pair_for_build(
                &executable,
                &aethyme_engine::daemon::runtime_build_identity(),
            )
            .map_err(std::io::Error::other)?;
            use std::sync::atomic::Ordering;
            if SERVICE_STOP.load(Ordering::Acquire) {
                return Ok(());
            }
            let server = transport::Server::bind(directory)?;
            if SERVICE_STOP.load(Ordering::Acquire) {
                return Ok(());
            }
            match repository {
                Some(repository) => {
                    let runtime = JobRuntime::open(&server, &repository)?;
                    server.serve_jobs_until(&SERVICE_STOP, runtime)
                }
                None => server.serve_until(&SERVICE_STOP),
            }
        }
        Command::Ping => {
            let response = transport::ping(directory)?;
            out!("{}", serde_json::to_string(&response)?);
            if response.result.is_err() {
                return Err(std::io::Error::other(
                    "broker service refused the health request",
                ));
            }
            Ok(())
        }
        Command::Job(method) => {
            let response = transport::job_request(directory, method.clone()).map_err(|error| {
                if matches!(method, wire::Method::Submit { .. }) {
                    std::io::Error::new(error.kind(), format!("{error}; submission outcome may be unknown: lookup the same request key before creating new work"))
                } else { error }
            })?;
            out!("{}", serde_json::to_string(&response)?);
            if response.result.is_err() {
                return Err(std::io::Error::other(
                    "broker service refused the job request",
                ));
            }
            Ok(())
        }
        Command::Result { job_id } => {
            let value = crate::service::client::read_result(
                |method| {
                    transport::job_request(directory, method)?
                        .result
                        .map_err(|error| {
                            std::io::Error::other(format!("{:?}: {}", error.code, error.message))
                        })
                },
                job_id,
            )?;
            out!("{}", serde_json::to_string(&value)?);
            Ok(())
        }
    }
}

#[cfg(unix)]
static SERVICE_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn stop_service_on_signal(_signal: libc::c_int) {
    // `AtomicBool` is lock-free. This handler only sets static process state;
    // it does not touch a stack pointer, allocate, or invoke non-signal-safe IO.
    SERVICE_STOP.store(true, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(unix)]
struct ServiceSignals {
    previous_int: libc::sigaction,
    previous_term: libc::sigaction,
    previous_mask: libc::sigset_t,
}

#[cfg(unix)]
fn restore_service_signal_mask(mask: &libc::sigset_t) -> std::io::Result<()> {
    // SAFETY: `mask` is the live signal set captured by `pthread_sigmask` in
    // `ServiceSignals::install`; a null old-set pointer is allowed here.
    let result = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, mask, std::ptr::null_mut()) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(result))
    }
}

#[cfg(unix)]
fn restore_service_signal_handler(
    signal: libc::c_int,
    handler: &libc::sigaction,
) -> std::io::Result<()> {
    // SAFETY: `handler` is a saved disposition returned by `sigaction` and the
    // null old-action pointer is allowed when restoring it.
    let result = unsafe { libc::sigaction(signal, handler, std::ptr::null_mut()) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
fn set_service_signal_set(
    set: &mut libc::sigset_t,
    signals: &[libc::c_int],
) -> std::io::Result<()> {
    // SAFETY: `set` is a live, aligned `sigset_t`; the function only fills it.
    if unsafe { libc::sigemptyset(set) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    for signal in signals {
        // SAFETY: `set` is live and each signal is one of SIGINT/SIGTERM.
        if unsafe { libc::sigaddset(set, *signal) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(unix)]
impl ServiceSignals {
    fn install() -> std::io::Result<Self> {
        use std::sync::atomic::Ordering;

        // This is the CLI process's only service instance. Install process-wide
        // handlers before pinning or binding; they cover any existing threads,
        // while newly spawned service threads inherit the unblocked mask.
        SERVICE_STOP.store(false, Ordering::Release);
        // SAFETY: `sigaction` is a plain C struct whose zeroed form is valid;
        // `sigemptyset` initializes its signal mask before installation.
        let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
        let mut previous_int = unsafe { std::mem::zeroed::<libc::sigaction>() };
        let mut previous_term = unsafe { std::mem::zeroed::<libc::sigaction>() };
        let mut watched = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        set_service_signal_set(&mut action.sa_mask, &[])?;
        set_service_signal_set(&mut watched, &[libc::SIGINT, libc::SIGTERM])?;
        action.sa_sigaction = stop_service_on_signal as libc::sighandler_t;
        action.sa_flags = 0;
        // SAFETY: the action and output disposition are live, correctly aligned
        // `sigaction` structs for the duration of the call.
        if unsafe { libc::sigaction(libc::SIGINT, &action, &mut previous_int) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: as above; both structs remain live until `sigaction` returns.
        if unsafe { libc::sigaction(libc::SIGTERM, &action, &mut previous_term) } != 0 {
            let error = std::io::Error::last_os_error();
            crate::warn_unrecorded(
                "restore SIGINT handler after SIGTERM install failure",
                restore_service_signal_handler(libc::SIGINT, &previous_int),
            );
            return Err(error);
        }
        // Make the main thread eligible to receive process-directed signals;
        // newly created service threads inherit this unblocked mask. Other
        // pre-existing threads keep their own masks and use the same handler.
        // SAFETY: both signal sets are live and aligned; `previous_mask` is a
        // writable output for the prior mask of the current thread.
        let mut previous_mask = unsafe { std::mem::zeroed::<libc::sigset_t>() };
        let result =
            unsafe { libc::pthread_sigmask(libc::SIG_UNBLOCK, &watched, &mut previous_mask) };
        if result != 0 {
            let error = std::io::Error::from_raw_os_error(result);
            crate::warn_unrecorded(
                "restore SIGTERM handler after signal unblocking failure",
                restore_service_signal_handler(libc::SIGTERM, &previous_term),
            );
            crate::warn_unrecorded(
                "restore SIGINT handler after signal unblocking failure",
                restore_service_signal_handler(libc::SIGINT, &previous_int),
            );
            return Err(error);
        }
        Ok(Self {
            previous_int,
            previous_term,
            previous_mask,
        })
    }
}

#[cfg(unix)]
impl Drop for ServiceSignals {
    fn drop(&mut self) {
        crate::warn_unrecorded(
            "restore service signal mask",
            restore_service_signal_mask(&self.previous_mask),
        );
        crate::warn_unrecorded(
            "restore SIGTERM handler",
            restore_service_signal_handler(libc::SIGTERM, &self.previous_term),
        );
        crate::warn_unrecorded(
            "restore SIGINT handler",
            restore_service_signal_handler(libc::SIGINT, &self.previous_int),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_every_argument_before_service_side_effects() {
        for words in [
            "",
            "serve",
            "serve /tmp --json",
            "serve /tmp --jobs-for",
            "serve /tmp --jobs-for repo --unknown",
            "directory /tmp",
            "directory /tmp --jobs-for",
            "directory /tmp --jobs-for repo --json --json",
            "directory /tmp --jobs-for repo --unknown",
            "ensure /tmp",
            "ensure /tmp --jobs-for",
            "ensure /tmp --jobs-for repo --json --json",
            "ensure /tmp --jobs-for repo --unknown",
            "ping /tmp --unknown",
            "repository /tmp --unknown",
            "repository /tmp extra",
            "status /tmp 0",
            "cancel /tmp -1",
            "result /tmp invalid",
            "lookup /tmp",
            "submit /tmp --request-key key",
            "submit /tmp --request-key key --deadline-ms 1 --full-scan --full-scan",
            "submit /tmp --request-key key --deadline-ms 1 --deadline-ms 2",
        ] {
            let args: Vec<String> = words.split_whitespace().map(str::to_owned).collect();
            assert!(parse(&args).is_none(), "accepted invalid command {words}");
        }
        for words in [
            "serve /tmp",
            "serve /tmp --jobs-for repo",
            "directory /tmp --jobs-for repo",
            "directory /tmp --jobs-for repo --json",
            "ensure /tmp --jobs-for repo",
            "ensure /tmp --jobs-for repo --json",
            "ping /tmp --json",
            "repository /tmp --json",
            "status /tmp 1",
            "lookup /tmp key --json",
            "submit /tmp --request-key key --deadline-ms 100 --full-scan --include-active-gate-cache --json",
        ] {
            let args: Vec<String> = words.split_whitespace().map(str::to_owned).collect();
            assert!(parse(&args).is_some(), "refused valid command {words}");
        }
    }
}
