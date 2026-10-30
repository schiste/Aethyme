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
            let server = transport::Server::bind(directory)?;
            let stop = std::sync::atomic::AtomicBool::new(false);
            match repository {
                Some(repository) => {
                    let runtime = JobRuntime::open(&server, &repository)?;
                    server.serve_jobs_until(&stop, runtime)
                }
                None => server.serve_until(&stop),
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
