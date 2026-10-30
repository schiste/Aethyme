//! Explicit, repository-independent opt-in service commands.

use super::*;

pub(super) const SERVICE_USAGE: &str = "  aethyme broker advanced service serve <private-runtime-directory>\n  aethyme broker advanced service ping <private-runtime-directory> [--json]\n      Opt-in foreground health service; existing broker operations stay local.\n      Create an absolute canonical directory owned by this user, mode 0700.\n      Stop with Ctrl-C or your process supervisor; no implicit activation.\n";

pub(super) fn run_service(args: &[String]) -> u8 {
    let valid = args.len() == 2 || (args.len() == 3 && args[0] == "ping" && args[2] == "--json");
    if !valid || !matches!(args[0].as_str(), "serve" | "ping") {
        return report_dispatch_error(args, crate::exit_status::USAGE, SERVICE_USAGE);
    }
    #[cfg(unix)]
    {
        let directory = Path::new(&args[1]);
        let result = if args[0] == "serve" {
            crate::service::transport::Server::bind(directory)
                .and_then(|server| server.serve_until(&std::sync::atomic::AtomicBool::new(false)))
        } else {
            crate::service::transport::ping(directory).and_then(|response| {
                let failed = response.result.is_err();
                out!("{}", serde_json::to_string(&response)?);
                if failed {
                    Err(std::io::Error::other(
                        "broker service refused the health request",
                    ))
                } else {
                    Ok(())
                }
            })
        };
        match result {
            Ok(()) => crate::exit_status::SUCCESS,
            Err(error) => {
                eprintln!("broker service: {error}");
                crate::exit_status::ENVIRONMENT
            }
        }
    }
    #[cfg(not(unix))]
    report_dispatch_error(
        args,
        crate::exit_status::ENVIRONMENT,
        "broker service requires Unix sockets",
    )
}
