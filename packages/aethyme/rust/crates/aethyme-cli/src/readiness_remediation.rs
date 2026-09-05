//! Reviewed, digest-confirmed repository-readiness remediation.

use std::path::PathBuf;

use crate::repository_upgrade::{
    ReadinessRemediationPlan, RepositoryMode, apply_readiness_remediation,
    build_readiness_remediation_plan, recover_readiness_remediation,
};

pub fn is_command(args: &[String]) -> bool {
    // The subcommand has to match too. Testing only the verb captured every
    // other `broker <group> plan|apply`, so `gc plan`, `ship plan` and
    // `promotion-record plan` all returned a readiness plan -- and returned its
    // digest, which `apply --confirm` would then have accepted.
    args.first().map(String::as_str) == Some("readiness")
        && matches!(
            args.get(1).map(String::as_str),
            Some("plan" | "apply" | "recover")
        )
}

pub fn run(args: &[String]) -> u8 {
    match run_inner(args) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("aethyme broker readiness: {error}");
            1
        }
    }
}

fn run_inner(args: &[String]) -> Result<(), String> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "-h" | "--help"))
    {
        print_usage();
        return Ok(());
    }
    let action = args
        .first()
        .map(String::as_str)
        .ok_or("expected plan, apply, or recover")?;
    let mut repo = PathBuf::from(".");
    let mut mode = None;
    let mut resolution_file = None;
    let mut confirmation = None;
    let mut json = false;
    let mut diff = false;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--repo" => {
                repo = PathBuf::from(args.get(index + 1).ok_or("--repo requires a path")?);
                index += 2;
            }
            "--local-only" => {
                mode = Some(RepositoryMode::LocalOnly);
                index += 1;
            }
            "--resolution-file" => {
                resolution_file = Some(PathBuf::from(
                    args.get(index + 1)
                        .ok_or("--resolution-file requires a path")?,
                ));
                index += 2;
            }
            "--confirm" => {
                confirmation = Some(
                    args.get(index + 1)
                        .ok_or("--confirm requires a digest")?
                        .clone(),
                );
                index += 2;
            }
            "--plan" if action == "recover" => {
                confirmation = Some(
                    args.get(index + 1)
                        .ok_or("--plan requires a digest")?
                        .clone(),
                );
                index += 2;
            }
            "--json" => {
                json = true;
                index += 1;
            }
            "--diff" => {
                diff = true;
                index += 1;
            }
            option => return Err(format!("unknown option {option}")),
        }
    }
    if json && diff {
        return Err("--diff and --json are separate review formats; choose one".into());
    }
    match action {
        "plan" => {
            let built = build_readiness_remediation_plan(&repo, mode, resolution_file.as_deref())?;
            render_plan(&built.report, json)?;
            if diff {
                println!("Remediation diff:");
                if built.remediation_diff.is_empty() {
                    println!("(no changes)");
                } else {
                    print!("{}", built.remediation_diff);
                }
            }
        }
        "apply" => {
            if diff {
                return Err("--diff is available only for readiness plan".into());
            }
            let report = apply_readiness_remediation(
                &repo,
                mode,
                confirmation
                    .as_deref()
                    .ok_or("apply requires --confirm <plan-sha256>")?,
                resolution_file.as_deref(),
            )?;
            render_plan(&report, json)?;
        }
        "recover" => {
            if diff || resolution_file.is_some() || mode.is_some() {
                return Err("recover accepts only --repo, --plan, and --json".into());
            }
            let recovered = recover_readiness_remediation(
                &repo,
                confirmation
                    .as_deref()
                    .ok_or("recover requires --plan <plan-sha256>")?,
            )?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&recovered).map_err(|error| error.to_string())?
                );
            } else {
                println!("Recovered plan: {}", recovered.plan_digest);
                for path in &recovered.restored_paths {
                    println!("  restored: {path}");
                }
                println!("Next: {}", recovered.next_action);
            }
        }
        other => {
            return Err(format!(
                "unknown action {other}; expected plan, apply, or recover"
            ));
        }
    }
    Ok(())
}

fn render_plan(report: &ReadinessRemediationPlan, json: bool) -> Result<(), String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(report).map_err(|error| error.to_string())?
        );
        return Ok(());
    }
    println!("Readiness remediation plan: {}", report.plan_sha256);
    println!("Source HEAD: {}", report.source_head);
    println!("Repository mode: {:?}", report.repository_mode);
    println!(
        "Repository schema: {} -> {}",
        report.repository_schema, report.target_schema
    );
    println!("Managed state: {}", report.managed_state_digest);
    println!("Safe to apply: {}", report.safe);
    println!("Applied: {}", report.applied);
    for change in &report.changes {
        println!(
            "  {:?}: {} {}@{} -> {}@{} ({:?})",
            change.action,
            change.path,
            optional(&change.before_sha256),
            optional(&change.before_mode),
            optional(&change.after_sha256),
            optional(&change.after_mode),
            change.ownership,
        );
    }
    for action in &report.actions {
        println!(
            "  action: {} — {}{}",
            action.id,
            action.summary,
            if action.review_required {
                " [review required]"
            } else {
                ""
            }
        );
    }
    for path in &report.dirty_overlapping_paths {
        println!("  overlapping dirty path: {path}");
    }
    for path in &report.dirty_disjoint_paths {
        println!("  disjoint dirty path: {path}");
    }
    for blocker in &report.blockers {
        println!("  blocker: {blocker}");
    }
    for warning in &report.warnings {
        println!("  warning: {warning}");
    }
    println!("Next: {}", report.next_action);
    Ok(())
}

fn optional(value: &Option<String>) -> &str {
    value.as_deref().unwrap_or("absent")
}

fn print_usage() {
    println!("Usage:");
    println!(
        "  aethyme broker readiness plan [--repo <path>] [--local-only] [--resolution-file <path>] [--diff|--json]"
    );
    println!(
        "  aethyme broker readiness apply [--repo <path>] [--local-only] [--resolution-file <path>] --confirm <plan-sha256> [--json]"
    );
    println!("  aethyme broker readiness recover [--repo <path>] --plan <plan-sha256> [--json]");
}

#[cfg(test)]
mod dispatch_tests {
    use super::is_command;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).into()).collect()
    }

    #[test]
    fn only_readiness_plan_apply_and_recover_route_here() {
        assert!(is_command(&args(&["readiness", "plan"])));
        assert!(is_command(&args(&["readiness", "apply"])));
        assert!(is_command(&args(&["readiness", "recover"])));
    }

    /// Every other digest-bound plan/apply pair must reach its own command. A
    /// readiness digest returned for `gc plan` would be accepted by `gc apply
    /// --confirm`, applying something the caller never reviewed.
    #[test]
    fn other_plan_and_apply_commands_are_not_captured() {
        for group in ["gc", "ship", "checkpoint", "promotion-record", "resources"] {
            assert!(
                !is_command(&args(&[group, "plan"])),
                "{group} plan must not route to readiness remediation"
            );
            assert!(
                !is_command(&args(&[group, "apply"])),
                "{group} apply must not route to readiness remediation"
            );
        }
    }
}
