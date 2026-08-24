//! Canonical repository enrollment: broker scaffold plus embedded agent policy.

use std::path::{Path, PathBuf};

pub fn run(args: &[String]) -> u8 {
    if args
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print_usage();
        return 0;
    }
    let (verify_only, repo, force) = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("aethyme deploy: {message}");
            print_usage();
            return 2;
        }
    };

    let repo = match std::fs::canonicalize(&repo) {
        Ok(repo) if repo.is_dir() => repo,
        Ok(_) => {
            eprintln!("aethyme deploy: {} is not a directory", repo.display());
            return 2;
        }
        Err(error) => {
            eprintln!("aethyme deploy: resolve {}: {error}", repo.display());
            return 2;
        }
    };

    if verify_only {
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
    if force {
        deploy_args.push("--force".to_string());
    }
    let deployed = aethyme_enhance::cli::run(&deploy_args);
    if deployed != 0 {
        return deployed;
    }

    let verified = verify_repository(&repo);
    if verified == 0 {
        print_artifact_ownership();
    }
    verified
}

fn verify_repository(repo: &Path) -> u8 {
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

fn parse_args(args: &[String]) -> Result<(bool, PathBuf, bool), String> {
    let mut verify_only = false;
    let mut repo = PathBuf::from(".");
    let mut force = false;
    let mut index = 0;
    if args.first().map(String::as_str) == Some("verify") {
        verify_only = true;
        index += 1;
    }
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                let value = args.get(index + 1).ok_or("--repo requires a directory")?;
                repo = PathBuf::from(value);
                index += 2;
            }
            "--force" if !verify_only => {
                force = true;
                index += 1;
            }
            option if option.starts_with('-') => {
                return Err(format!("unknown option {option}"));
            }
            value => return Err(format!("unexpected argument {value}")),
        }
    }
    Ok((verify_only, repo, force))
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
    println!("  aethyme deploy verify [--repo <path>]");
}

fn print_artifact_ownership() {
    println!();
    println!("Repository deployment verified.");
    println!("Review and commit repository policy:");
    for path in [
        ".gitignore",
        ".aethyme/config.toml",
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
    println!("Next: review the generated policy, commit it, and retain `aethyme deploy verify --repo .` in CI.");
}
