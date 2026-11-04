//! The integration-test binary for this crate's suites.
//!
//! Each `tests/<suite>.rs` listed below is a module of this one binary rather
//! than a binary of its own. Every separate integration-test binary links the
//! crate and its dependencies again, which made linking most of the workspace's
//! test compile time; one binary per crate links once. Tests keep their names,
//! now prefixed by their suite: run one suite with
//! `cargo test -p aethyme-broker --test it adopt_cli::` or
//! `cargo nextest run -p aethyme-broker -E 'binary(it) & test(/^adopt_cli::/)'`.
//!
//! `#[path]` keeps each suite's file, and the files it loads with its own
//! `mod` declarations, where they were: a module loaded through `#[path]`
//! resolves its child modules from its own directory, as a crate root does.
//!
//! The suites that stay separate binaries, and why, are the other `[[test]]`
//! targets in this crate's `Cargo.toml`. `aethyme-testkit/tests/test_targets.rs`
//! fails when a `tests/*.rs` file is neither listed here nor its own target,
//! because `autotests = false` would otherwise skip it silently.

#[path = "adopt_cli.rs"]
mod adopt_cli;
#[path = "advisories_cli.rs"]
mod advisories_cli;
#[path = "artifact_claim_cli.rs"]
mod artifact_claim_cli;
#[path = "auto_cleanup.rs"]
mod auto_cleanup;
#[path = "base_incidents_cli.rs"]
mod base_incidents_cli;
#[path = "blockers_cli.rs"]
mod blockers_cli;
#[path = "broker_db_reach_cli.rs"]
mod broker_db_reach_cli;
#[path = "candidate_boundary.rs"]
mod candidate_boundary;
#[path = "capture_revision_replay.rs"]
mod capture_revision_replay;
#[path = "certify_scaffold.rs"]
mod certify_scaffold;
#[path = "cheap_plan.rs"]
mod cheap_plan;
#[path = "check_contract_cli.rs"]
mod check_contract_cli;
#[path = "cleanup_audit.rs"]
mod cleanup_audit;
#[path = "cleanup_cli.rs"]
mod cleanup_cli;
#[path = "cleanup_resolve_cli.rs"]
mod cleanup_resolve_cli;
#[path = "cli_reference_consistency.rs"]
mod cli_reference_consistency;
#[path = "closed_worktree_gc.rs"]
mod closed_worktree_gc;
#[path = "collaboration_submit_cli.rs"]
mod collaboration_submit_cli;
#[path = "command_failure_messages_cli.rs"]
mod command_failure_messages_cli;
#[path = "composer_fixtures.rs"]
mod composer_fixtures;
#[path = "config_formats.rs"]
mod config_formats;
#[path = "console_cli.rs"]
mod console_cli;
#[path = "context_selection_vectors.rs"]
mod context_selection_vectors;
#[path = "contract_v1.rs"]
mod contract_v1;
#[path = "coordination_signals_e2e.rs"]
mod coordination_signals_e2e;
#[path = "doctor_debris.rs"]
mod doctor_debris;
#[path = "e1_browser.rs"]
mod e1_browser;
#[path = "e1_composition.rs"]
mod e1_composition;
#[path = "exec_guard_cli.rs"]
mod exec_guard_cli;
#[path = "external_events.rs"]
mod external_events;
#[path = "external_events_cli.rs"]
mod external_events_cli;
#[path = "finish_cli.rs"]
mod finish_cli;
#[path = "fx08_context.rs"]
mod fx08_context;
#[path = "gate_cache_inventory.rs"]
mod gate_cache_inventory;
#[path = "gate_trust_cli.rs"]
mod gate_trust_cli;
#[path = "gates_cli.rs"]
mod gates_cli;
#[path = "gc_sweep_cli.rs"]
mod gc_sweep_cli;
#[path = "gh_create_cli.rs"]
mod gh_create_cli;
#[path = "gh_ref_delete_guard_cli.rs"]
mod gh_ref_delete_guard_cli;
#[path = "gh_refusal_cli.rs"]
mod gh_refusal_cli;
#[path = "git.rs"]
mod git;
#[path = "guarded_exec_cli.rs"]
mod guarded_exec_cli;
#[path = "handoff_cli.rs"]
mod handoff_cli;
#[path = "hooks_e2e.rs"]
mod hooks_e2e;
#[path = "hooks_path_doctor_cli.rs"]
mod hooks_path_doctor_cli;
#[path = "insights.rs"]
mod insights;
#[path = "insights_cli.rs"]
mod insights_cli;
#[path = "inspection_budget_cli.rs"]
mod inspection_budget_cli;
#[path = "install_replacement_cli.rs"]
mod install_replacement_cli;
#[path = "integration_verify_base.rs"]
mod integration_verify_base;
#[path = "lease_claim_policy_e2e.rs"]
mod lease_claim_policy_e2e;
#[path = "lease_liveness_cli.rs"]
mod lease_liveness_cli;
#[path = "lease_release_finish_cli.rs"]
mod lease_release_finish_cli;
#[path = "lease_requests_cli.rs"]
mod lease_requests_cli;
#[path = "leases_cli.rs"]
mod leases_cli;
#[path = "leases_e2e.rs"]
mod leases_e2e;
#[path = "main_reconcile.rs"]
mod main_reconcile;
#[path = "merge_chain_cli.rs"]
mod merge_chain_cli;
#[path = "merge_e2e.rs"]
mod merge_e2e;
#[path = "notes_cli.rs"]
mod notes_cli;
#[path = "outcomes_cli.rs"]
mod outcomes_cli;
#[path = "output_measurement_cli.rs"]
mod output_measurement_cli;
#[path = "overlap_scaling_e2e.rs"]
mod overlap_scaling_e2e;
#[path = "overlap_severity_e2e.rs"]
mod overlap_severity_e2e;
#[path = "owners_cli.rs"]
mod owners_cli;
#[path = "ownership_claims_e2e.rs"]
mod ownership_claims_e2e;
#[path = "plugin_floor.rs"]
mod plugin_floor;
#[path = "post_merge_cleanup_cli.rs"]
mod post_merge_cleanup_cli;
#[path = "pr_subscriptions_cli.rs"]
mod pr_subscriptions_cli;
#[path = "pr_watch.rs"]
mod pr_watch;
#[path = "preparation_cli.rs"]
mod preparation_cli;
#[path = "prepare_draft.rs"]
mod prepare_draft;
#[path = "promotion_record.rs"]
mod promotion_record;
#[path = "push_source_cli.rs"]
mod push_source_cli;
#[path = "queued_operations_cli.rs"]
mod queued_operations_cli;
#[path = "readiness_cli.rs"]
mod readiness_cli;
#[path = "reclaim_ordering.rs"]
mod reclaim_ordering;
#[path = "reconcile_tracked_elsewhere_cli.rs"]
mod reconcile_tracked_elsewhere_cli;
#[path = "reconcile_usage_cli.rs"]
mod reconcile_usage_cli;
#[path = "reconciliation_external_main.rs"]
mod reconciliation_external_main;
#[path = "recovery_archive_gc_cli.rs"]
mod recovery_archive_gc_cli;
#[path = "reftable_git.rs"]
mod reftable_git;
#[path = "remote_target.rs"]
mod remote_target;
#[path = "renamed_targets.rs"]
mod renamed_targets;
#[path = "report_cli.rs"]
mod report_cli;
#[path = "resolution_boundaries.rs"]
mod resolution_boundaries;
#[path = "resolution_fixtures.rs"]
mod resolution_fixtures;
#[path = "resources_cli.rs"]
mod resources_cli;
#[path = "review_classification_cli.rs"]
mod review_classification_cli;
#[path = "review_cli.rs"]
mod review_cli;
#[path = "review_projection_guard_cli.rs"]
mod review_projection_guard_cli;
#[path = "review_routing.rs"]
mod review_routing;
#[path = "review_rule_actions_cli.rs"]
mod review_rule_actions_cli;
#[path = "review_rule_comments_cli.rs"]
mod review_rule_comments_cli;
#[path = "review_run_on_push_cli.rs"]
mod review_run_on_push_cli;
#[path = "service_protocol.rs"]
mod service_protocol;
#[path = "service_transport.rs"]
mod service_transport;
#[path = "service_worker.rs"]
mod service_worker;
#[path = "session_abandonment.rs"]
mod session_abandonment;
#[path = "session_guidance_cli.rs"]
mod session_guidance_cli;
#[path = "session_holder_cli.rs"]
mod session_holder_cli;
#[path = "session_push_cli.rs"]
mod session_push_cli;
#[path = "session_repository_cli.rs"]
mod session_repository_cli;
#[path = "session_sync_cli.rs"]
mod session_sync_cli;
#[path = "shared_edit_advice_cli.rs"]
mod shared_edit_advice_cli;
#[path = "ship_e2e.rs"]
mod ship_e2e;
#[path = "simulated_free_space_cli.rs"]
mod simulated_free_space_cli;
#[path = "start_base_cli.rs"]
mod start_base_cli;
#[path = "start_task_overlap_cli.rs"]
mod start_task_overlap_cli;
#[path = "status_compact_cli.rs"]
mod status_compact_cli;
#[path = "status_metadata.rs"]
mod status_metadata;
#[path = "status_observation.rs"]
mod status_observation;
#[path = "status_operations_cli.rs"]
mod status_operations_cli;
#[path = "status_routine.rs"]
mod status_routine;
#[path = "storage_cli.rs"]
mod storage_cli;
#[path = "store.rs"]
mod store;
#[path = "stress.rs"]
mod stress;
#[path = "sub_issue_cli.rs"]
mod sub_issue_cli;
#[path = "submit_progress_status.rs"]
mod submit_progress_status;
#[path = "submit_retry_idempotence.rs"]
mod submit_retry_idempotence;
#[path = "unpushed_work_cli.rs"]
mod unpushed_work_cli;
#[path = "v0_scenario.rs"]
mod v0_scenario;
#[path = "verify_only_main_cli.rs"]
mod verify_only_main_cli;
#[path = "worktree_reconcile.rs"]
mod worktree_reconcile;
#[path = "worktree_root_cli.rs"]
mod worktree_root_cli;
#[path = "worktree_root_config_cli.rs"]
mod worktree_root_config_cli;
