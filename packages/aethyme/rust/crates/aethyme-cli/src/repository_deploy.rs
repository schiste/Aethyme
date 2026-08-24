//! Canonical repository enrollment: broker scaffold plus embedded agent policy.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Deploy,
    Verify,
    Bridge,
}

struct Options {
    action: Action,
    repo: PathBuf,
    force: bool,
    local_only: bool,
}

pub fn run(args: &[String]) -> u8 {
    if args
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print_usage();
        return 0;
    }
    let options = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("aethyme deploy: {message}");
            print_usage();
            return 2;
        }
    };

    let repo = match std::fs::canonicalize(&options.repo) {
        Ok(repo) if repo.is_dir() => repo,
        Ok(_) => {
            eprintln!(
                "aethyme deploy: {} is not a directory",
                options.repo.display()
            );
            return 2;
        }
        Err(error) => {
            eprintln!(
                "aethyme deploy: resolve {}: {error}",
                options.repo.display()
            );
            return 2;
        }
    };

    if options.action == Action::Bridge {
        return install_bridge(&repo);
    }
    if options.local_only {
        return if options.action == Action::Verify {
            verify_local_repository(&repo)
        } else {
            deploy_local_repository(&repo, options.force)
        };
    }
    if options.action == Action::Verify {
        return verify_repository(&repo);
    }

    let scaffold = in_repo(&repo, || {
        aethyme_broker::cli::run(&["scaffold".to_string()])
    });
    if scaffold != 0 {
        return scaffold;
    }
    let gates = in_repo(&repo, || {
        aethyme_broker::cli::run(&["gates".to_string(), "draft".to_string()])
    });
    if gates != 0 {
        return gates;
    }

    let mut deploy_args = vec![
        "deploy".to_string(),
        "--repo".to_string(),
        repo.display().to_string(),
    ];
    if options.force {
        deploy_args.push("--force".to_string());
    }
    let deployed = aethyme_enhance::cli::run(&deploy_args);
    if deployed != 0 {
        return deployed;
    }
    if let Err(error) = crate::repository_upgrade::write_current_marker(
        &repo,
        crate::repository_upgrade::RepositoryMode::Canonical,
    ) {
        eprintln!("aethyme deploy: write repository schema marker: {error}");
        return 1;
    }

    let verified = verify_repository(&repo);
    if verified == 0 {
        print_artifact_ownership();
    }
    verified
}

fn verify_repository(repo: &Path) -> u8 {
    if let Err(error) = crate::repository_upgrade::verify_current_marker(
        repo,
        crate::repository_upgrade::RepositoryMode::Canonical,
    ) {
        eprintln!("aethyme deploy verify: {error}");
        return 1;
    }
    let verified = aethyme_enhance::cli::run(&[
        "verify".to_string(),
        "--repo".to_string(),
        repo.display().to_string(),
    ]);
    if verified != 0 {
        return verified;
    }
    in_repo(repo, || aethyme_broker::cli::run(&["certify".to_string()]))
}

fn parse_args(args: &[String]) -> Result<Options, String> {
    let mut action = Action::Deploy;
    let mut repo = PathBuf::from(".");
    let mut force = false;
    let mut local_only = false;
    let mut index = 0;
    match args.first().map(String::as_str) {
        Some("verify") => {
            action = Action::Verify;
            index += 1;
        }
        Some("bridge") => {
            action = Action::Bridge;
            index += 1;
        }
        _ => {}
    }
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                let value = args.get(index + 1).ok_or("--repo requires a directory")?;
                repo = PathBuf::from(value);
                index += 2;
            }
            "--force" if action == Action::Deploy => {
                force = true;
                index += 1;
            }
            "--local-only" if action != Action::Bridge => {
                local_only = true;
                index += 1;
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown option {option}"));
            }
            value => return Err(format!("unexpected argument {value}")),
        }
    }
    Ok(Options {
        action,
        repo,
        force,
        local_only,
    })
}

fn in_repo(repo: &Path, operation: impl FnOnce() -> u8) -> u8 {
    let original = match std::env::current_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("aethyme deploy: resolve current directory: {error}");
            return 1;
        }
    };
    if let Err(error) = std::env::set_current_dir(repo) {
        eprintln!("aethyme deploy: enter {}: {error}", repo.display());
        return 1;
    }
    let code = operation();
    if let Err(error) = std::env::set_current_dir(&original) {
        eprintln!(
            "aethyme deploy: restore current directory {}: {error}",
            original.display()
        );
        return 1;
    }
    code
}

fn print_usage() {
    println!("Usage:");
    println!("  aethyme deploy [--repo <path>] [--force]");
    println!("  aethyme deploy bridge [--repo <path>]");
    println!("  aethyme deploy --local-only [--repo <path>] [--force]");
    println!("  aethyme deploy verify [--repo <path>] [--local-only]");
}

fn install_bridge(repo: &Path) -> u8 {
    match aethyme_enhance::local::install_bridge(repo) {
        Ok(actions) => {
            for action in actions {
                println!("{:<9} {}", action.action, action.path);
            }
            println!(
                "Review and commit AGENTS.md and CLAUDE.md; without local activation the bridge performs only one file-existence check."
            );
            0
        }
        Err(error) => {
            eprintln!("aethyme deploy bridge: {error}");
            1
        }
    }
}

fn deploy_local_repository(repo: &Path, force: bool) -> u8 {
    if !aethyme_enhance::local::bridge_installed(repo) {
        eprintln!(
            "aethyme deploy: local-only activation requires the committed inert bridge; run `aethyme deploy bridge --repo .`, review it, and commit it first"
        );
        return 1;
    }
    if let Err(error) = aethyme_enhance::local::prepare(repo) {
        eprintln!("aethyme deploy: {error}");
        return 1;
    }
    let scaffold = match aethyme_broker::init::scaffold_local(repo) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("aethyme deploy: local broker scaffold: {error}");
            return 1;
        }
    };
    print_checks(&scaffold);
    let gates = match aethyme_broker::init::draft_gates(repo) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("aethyme deploy: local gate draft: {error}");
            return 1;
        }
    };
    print_checks(&gates);
    match aethyme_enhance::local::deploy(repo, force) {
        Ok(actions) => {
            for action in actions {
                println!("{:<9} {}", action.action, action.path);
            }
        }
        Err(error) => {
            eprintln!("aethyme deploy: {error}");
            return 1;
        }
    }
    if let Err(error) = crate::repository_upgrade::write_current_marker(
        repo,
        crate::repository_upgrade::RepositoryMode::LocalOnly,
    ) {
        eprintln!("aethyme deploy: write local repository schema marker: {error}");
        return 1;
    }
    let verified = verify_local_repository(repo);
    if verified == 0 {
        println!("Local-only Aethyme activation verified; Git tracks no activation artifacts.");
        println!("Other clones remain inactive unless they create .aethyme/local/enabled.");
    }
    verified
}

fn verify_local_repository(repo: &Path) -> u8 {
    if let Err(error) = crate::repository_upgrade::verify_current_marker(
        repo,
        crate::repository_upgrade::RepositoryMode::LocalOnly,
    ) {
        eprintln!("aethyme deploy verify: {error}");
        return 1;
    }
    match aethyme_enhance::local::verify(repo) {
        Ok(problems) if problems.is_empty() => {}
        Ok(problems) => {
            for problem in problems {
                eprintln!("fail      {problem}");
            }
            return 1;
        }
        Err(error) => {
            eprintln!("aethyme deploy verify: {error}");
            return 1;
        }
    }
    in_repo(repo, || aethyme_broker::cli::run(&["certify".to_string()]))
}

fn print_checks(report: &aethyme_broker::init::InitReport) {
    for check in &report.checks {
        println!(
            "{:<9} {:<32} {}",
            format!("{:?}", check.status).to_lowercase(),
            check.id,
            check.detail
        );
    }
}

fn print_artifact_ownership() {
    println!();
    println!("Repository deployment verified.");
    println!("Review and commit repository policy:");
    for path in [
        ".gitignore",
        ".aethyme/config.toml",
        ".aethyme/repository.json",
        ".aethyme/gates.toml (when generated)",
        ".aethyme/overrides/ (when present)",
        ".aethyme/generated/onboarding.json",
        ".aethyme/generated/act-starter.json",
        "AGENTS.md and CLAUDE.md",
        ".codex/skills/",
        ".claude/skills/",
        ".claude/hooks/aethyme-load-context.sh",
        ".claude/settings.local.json",
    ] {
        println!("  {path}");
    }
    println!("Ignored machine-local runtime state:");
    for path in [
        ".aethyme/broker.db*",
        ".aethyme/logs/",
        ".aethyme/reports/",
        ".aethyme/run/",
        ".aethyme/worktrees/",
        ".aethyme/broker-action-required.md",
        ".aethyme/generated/experience-telemetry.jsonl",
        ".aethyme/generated/experience-status.json",
        ".aethyme/generated/experience-status.md",
    ] {
        println!("  {path}");
    }
    println!(
        "Next: review the generated policy, commit it, and retain `aethyme deploy verify --repo .` in CI."
    );
}
