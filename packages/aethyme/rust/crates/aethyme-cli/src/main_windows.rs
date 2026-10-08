// Phase 1 Windows CLI. This deliberately links no broker modules: the broker
// remains unavailable until its Windows coordination and IPC design lands.
use std::env;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        print_help();
        return ExitCode::from(2);
    };
    if matches!(command, "-h" | "--help") {
        print_help();
        return ExitCode::SUCCESS;
    }
    if matches!(command, "-V" | "--version") {
        print_version();
        return ExitCode::SUCCESS;
    }

    match command {
        "broker" | "certify" | "init" | "hook" | "plugin" | "update" | "self-update"
        | "upgrade" => broker_unavailable(),
        "explore" => run_explore(&args[1..]),
        "graph" => run_graph(&args[1..]),
        "deploy" => run_deploy(&args[1..]),
        other => {
            eprintln!(
                "Error: unsupported command {other:?} on Windows; supported commands are explore, graph navigation, and generated-only deploy"
            );
            ExitCode::from(2)
        }
    }
}

fn print_help() {
    println!("aethyme — repository navigation and generated policy deployment");
    println!();
    println!("Windows x64 phase 1:");
    println!("  aethyme explore --request <text> [--repo <path>] [--format brief|answer-json]");
    println!("  aethyme graph <navigation-command> ...");
    println!("  aethyme deploy [--generated-only] [--repo <path>] [--force]");
    println!();
    println!("Broker commands are not yet supported on Windows.");
    println!("Run aethyme graph --help for the native graph navigation commands.");
}

fn print_version() {
    let describe = env!("AETHYME_GIT_DESCRIBE");
    let commit = {
        let value = env!("AETHYME_GIT_COMMIT");
        if value.is_empty() { "unknown" } else { value }
    };
    let build_date = env!("AETHYME_BUILD_DATE");
    let stable = if describe.is_empty() {
        format!("build_commit={commit}")
    } else {
        format!("{describe} build_commit={commit}")
    };
    println!(
        "aethyme {} ({stable}) build_date={build_date}",
        env!("CARGO_PKG_VERSION")
    );
}

fn broker_unavailable() -> ExitCode {
    eprintln!("aethyme: the broker is not yet supported on Windows");
    ExitCode::from(1)
}

fn run_explore(args: &[String]) -> ExitCode {
    use aethyme_engine::explore_cli::{ExploreCliOutcome, run};
    match run(args) {
        ExploreCliOutcome::Done => ExitCode::SUCCESS,
        ExploreCliOutcome::BadUsage(message) => {
            eprintln!("{message}");
            ExitCode::from(2)
        }
        ExploreCliOutcome::Failed(message) => {
            eprintln!("{message}");
            ExitCode::from(1)
        }
        ExploreCliOutcome::DaemonNotRunning { repo } => {
            eprintln!(
                "explore: engine daemon is not yet supported on Windows for {}",
                repo.display()
            );
            ExitCode::from(1)
        }
    }
}

fn run_graph(args: &[String]) -> ExitCode {
    if matches!(
        args.first().map(String::as_str),
        Some("status" | "units" | "materialize" | "refresh" | "impact")
    ) {
        eprintln!(
            "aethyme graph {} is not yet supported on Windows; native graph navigation is available",
            args[0]
        );
        return ExitCode::from(1);
    }
    match aethyme_engine::graph_cli::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

fn run_deploy(args: &[String]) -> ExitCode {
    if matches!(
        args.first().map(String::as_str),
        Some("plan" | "execute" | "bridge")
    ) {
        return broker_unavailable();
    }
    let mut action = "deploy";
    let mut repo = ".".to_string();
    let mut generated_only = false;
    let mut force = false;
    let mut index = 0;
    if args.first().is_some_and(|arg| arg == "verify") {
        action = "verify";
        index = 1;
    }
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("Usage:");
        println!("  aethyme deploy --generated-only [--repo <path>] [--force]");
        println!("  aethyme deploy verify --generated-only [--repo <path>]");
        println!(
            "Generated-only deployment writes embedded repository guidance without broker enrollment."
        );
        return ExitCode::SUCCESS;
    }
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("aethyme deploy: --repo requires a directory");
                    return ExitCode::from(2);
                };
                repo = value.clone();
                index += 2;
            }
            "--generated-only" => {
                generated_only = true;
                index += 1;
            }
            "--force" if action == "deploy" => {
                force = true;
                index += 1;
            }
            "--local-only" | "--with-graph" | "--graph-repository" => {
                return broker_unavailable();
            }
            option => {
                eprintln!("aethyme deploy: unsupported Windows option or argument {option:?}");
                return ExitCode::from(2);
            }
        }
    }
    if !generated_only {
        return broker_unavailable();
    }

    let mut enhance_args = vec![action.to_string(), "--repo".to_string(), repo];
    if force {
        enhance_args.push("--force".to_string());
    }
    ExitCode::from(aethyme_enhance::cli::run(&enhance_args))
}
