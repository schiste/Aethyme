//! The integration-test binary for this crate's suites.
//!
//! Each `tests/<suite>.rs` listed below is a module of this one binary rather
//! than a binary of its own. Every separate integration-test binary links the
//! crate and its dependencies again, which made linking most of the workspace's
//! test compile time; one binary per crate links once. Tests keep their names,
//! now prefixed by their suite: run one suite with
//! `cargo test -p aethyme-cli --test it ai_ready_cli::` or
//! `cargo nextest run -p aethyme-cli -E 'binary(it) & test(/^ai_ready_cli::/)'`.
//!
//! `#[path]` keeps each suite's file, and the files it loads with its own
//! `mod` declarations, where they were: a module loaded through `#[path]`
//! resolves its child modules from its own directory, as a crate root does.
//!
//! The suites that stay separate binaries, and why, are the other `[[test]]`
//! targets in this crate's `Cargo.toml`. `aethyme-testkit/tests/test_targets.rs`
//! fails when a `tests/*.rs` file is neither listed here nor its own target,
//! because `autotests = false` would otherwise skip it silently.

#[path = "ai_ready_cli.rs"]
mod ai_ready_cli;
#[path = "autofix_cli.rs"]
mod autofix_cli;
#[path = "branches_audit_cli.rs"]
mod branches_audit_cli;
#[path = "certify_git_shim.rs"]
mod certify_git_shim;
#[path = "certify_read_only.rs"]
mod certify_read_only;
#[path = "collab_cli.rs"]
mod collab_cli;
#[path = "collab_project_association.rs"]
mod collab_project_association;
#[path = "commit_hygiene_cli.rs"]
mod commit_hygiene_cli;
#[path = "deprecated_spellings.rs"]
mod deprecated_spellings;
#[path = "doc_command_drift.rs"]
mod doc_command_drift;
#[path = "enhance_cli.rs"]
mod enhance_cli;
#[path = "env_cli.rs"]
mod env_cli;
#[path = "explore_brief_cli.rs"]
mod explore_brief_cli;
#[path = "explore_stale_checkout_cli.rs"]
mod explore_stale_checkout_cli;
#[path = "explore_summary_cli.rs"]
mod explore_summary_cli;
#[path = "graph_refresh_cli.rs"]
mod graph_refresh_cli;
#[path = "guided_init_readiness.rs"]
mod guided_init_readiness;
#[path = "hook_coordination_cli.rs"]
mod hook_coordination_cli;
#[path = "hook_install_health_cli.rs"]
mod hook_install_health_cli;
#[path = "intents_cli.rs"]
mod intents_cli;
#[path = "json_shape_snapshots.rs"]
mod json_shape_snapshots;
#[path = "local_workflow.rs"]
mod local_workflow;
#[path = "operations_cli.rs"]
mod operations_cli;
#[path = "playground_hygiene.rs"]
mod playground_hygiene;
#[path = "quality_cli.rs"]
mod quality_cli;
#[path = "quick_test_git_isolation.rs"]
mod quick_test_git_isolation;
#[path = "readiness_remediation_cli.rs"]
mod readiness_remediation_cli;
#[path = "reporter_telemetry.rs"]
mod reporter_telemetry;
#[path = "repository_enrollment_cli.rs"]
mod repository_enrollment_cli;
#[path = "repository_upgrade_cli.rs"]
mod repository_upgrade_cli;
#[path = "resolution_cli.rs"]
mod resolution_cli;
#[path = "session_start_brief_cli.rs"]
mod session_start_brief_cli;
#[path = "skill_templates.rs"]
mod skill_templates;
#[path = "skills_cli.rs"]
mod skills_cli;
#[path = "update_cache_cli.rs"]
mod update_cache_cli;
#[path = "update_cli.rs"]
mod update_cli;
