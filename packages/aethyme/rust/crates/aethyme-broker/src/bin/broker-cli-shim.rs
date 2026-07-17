//! Router stand-in for the hook end-to-end tests (`tests/hooks_e2e.rs`).
//!
//! Installed hooks invoke `<binary> broker hooks <hook>`; in production
//! that binary is the `aethyme` router (aethyme-engine), which this crate
//! cannot depend on. This shim mimics the router's broker dispatch so the
//! tests can exercise real `git commit` runs end to end. Not a
//! user-facing entrypoint.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = match args.first().map(String::as_str) {
        Some("broker") => &args[1..],
        _ => &args[..],
    };
    ExitCode::from(aethyme_broker::cli::run(rest))
}
