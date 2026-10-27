fn gh(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

fn scope(args: &[&str]) -> Option<String> {
    resource_lock_scope(OperationProvider::Github, &gh(args))
}

fn review_thread_api(query: &str, thread_id: &str, body: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "api".to_string(),
        "--method".to_string(),
        "POST".to_string(),
        "graphql".to_string(),
        "-f".to_string(),
        format!("query={query}"),
        "-f".to_string(),
        format!("threadId={thread_id}"),
    ];
    if let Some(body) = body {
        args.extend(["-f".to_string(), format!("body={body}")]);
    }
    args
}

fn issue_plan(identity: &str, watermark: Option<i64>) -> CreatePlan {
    CreatePlan {
        collection: "issue".into(),
        identity_field: "title".into(),
        identity: identity.into(),
        watermark,
    }
}

fn listed(entries: &[(i64, &str)]) -> Vec<serde_json::Value> {
    entries
        .iter()
        .map(|(number, title)| {
            json!({
                "number": number,
                "title": title,
                "url": format!("https://github.com/o/r/issues/{number}"),
            })
        })
        .collect()
}

/// The subcommand is what decides whether a command is labelled at all, and
/// `gh` accepts flags in front of it.
#[test]
fn the_subcommand_is_found_behind_leading_flags_but_never_inside_one() {
    assert_eq!(
        gh_subcommand(&gh(&["issue", "create", "--title", "x"])),
        Some(("issue", "create"))
    );
    assert_eq!(
        gh_subcommand(&gh(&["--repo", "o/r", "issue", "create"])),
        Some(("issue", "create"))
    );
    // `issue` here is the value of `--title`, not the command.
    assert_eq!(
        gh_subcommand(&gh(&["--title", "issue", "create", "later"])),
        Some(("create", "later"))
    );
}

#[test]
fn open_pr_cache_invalidation_tracks_topology_flags_not_body_values() {
    for argv in [
        vec!["pr", "create"],
        vec!["pr", "close", "506"],
        vec!["pr", "reopen", "506"],
        vec!["pr", "merge", "506"],
        vec!["pr", "update-branch", "506"],
        vec![
            "--repo",
            "acme/project",
            "pr",
            "edit",
            "506",
            "--base",
            "main",
        ],
        vec!["pr", "edit", "506", "--base=release"],
        vec!["pr", "edit", "506", "-B", "release"],
    ] {
        assert!(gh_pr_topology_write(&gh(&argv)), "{argv:?}");
    }

    for argv in [
        vec!["pr", "list", "--state", "closed"],
        vec!["pr", "edit", "506", "--body", "--base"],
        vec!["pr", "edit", "506", "-b", "-B"],
        vec!["pr", "edit", "506", "--body=state=closed --base=release"],
        vec!["pr", "edit", "506", "--title", "close this PR"],
        vec!["issue", "edit", "506", "--body=--base=release"],
    ] {
        assert!(!gh_pr_topology_write(&gh(&argv)), "{argv:?}");
    }

    assert!(should_invalidate_open_pr_listing(
        OperationProvider::Github,
        &gh(&["pr", "update-branch", "506"]),
        OperationStatus::Succeeded,
    ));
    assert!(should_invalidate_open_pr_listing(
        OperationProvider::Github,
        &gh(&["pr", "update-branch", "506"]),
        OperationStatus::OutcomeUnknown,
    ));
    assert!(!should_invalidate_open_pr_listing(
        OperationProvider::Github,
        &gh(&["pr", "update-branch", "506"]),
        OperationStatus::Failed,
    ));
    assert!(!should_invalidate_open_pr_listing(
        OperationProvider::Git,
        &gh(&["pr", "update-branch", "506"]),
        OperationStatus::Succeeded,
    ));
}

/// Every spelling `gh` accepts for a label has to be seen, because a label
/// this misses is one the pre-flight check cannot refuse (#184).
#[test]
fn labels_are_collected_across_spellings_repetitions_and_comma_lists() {
    assert_eq!(
        gh_requested_labels(&gh(&[
            "issue",
            "create",
            "--title",
            "t",
            "--label",
            "bug,dette",
            "-l",
            "area:broker",
            "--label=chore",
        ])),
        vec!["bug", "dette", "area:broker", "chore"]
    );
    assert_eq!(
        gh_requested_labels(&gh(&[
            "issue",
            "edit",
            "7",
            "--add-label",
            "bug",
            "--remove-label",
            "stale",
        ])),
        vec!["bug", "stale"]
    );
}

/// `--label` on a listing is a filter: an unknown name returns nothing
/// rather than failing, so refusing it would break a working read.
#[test]
fn a_label_filter_on_a_listing_is_not_a_label_to_resolve() {
    assert!(gh_requested_labels(&gh(&["issue", "list", "--label", "dette"])).is_empty());
    assert!(gh_requested_labels(&gh(&["label", "create", "dette"])).is_empty());
}

/// The identity has to come from the command line, and a create whose
/// result could not be named afterwards must say so rather than be planned
/// against something that does not identify it.
#[test]
fn a_create_is_planned_from_its_identity_or_declared_unplannable() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        plan_github_create(&gh(&["issue", "create", "--title", "a bug"]), tmp.path()),
        CreatePlanning::Planned(issue_plan("a bug", None))
    );
    assert_eq!(
        plan_github_create(&gh(&["issue", "create", "--body", "b"]), tmp.path()),
        CreatePlanning::Unplannable {
            reason: "issue_create_without_an_explicit_title"
        }
    );
    assert_eq!(
        plan_github_create(
            &gh(&["pr", "create", "--title", "t", "--head", "topic"]),
            tmp.path()
        ),
        CreatePlanning::Planned(CreatePlan {
            collection: "pr".into(),
            identity_field: "headRefName".into(),
            identity: "topic".into(),
            watermark: None,
        })
    );
    assert_eq!(
        plan_github_create(&gh(&["issue", "comment", "7", "--body", "b"]), tmp.path()),
        CreatePlanning::NotApplicable
    );
}

/// `gh` prints the new resource's URL once the API call has returned, so a
/// URL on stdout survives a later non-zero exit as proof. A URL for some
/// other repository proves nothing about this one.
#[test]
fn only_a_url_under_the_asserted_repository_counts_as_a_created_resource() {
    assert_eq!(
        created_resource_url(
            "https://github.com/Schiste/Aethyme/issues/184\n",
            "github.com/schiste/aethyme"
        )
        .as_deref(),
        Some("https://github.com/Schiste/Aethyme/issues/184")
    );
    assert_eq!(
        created_resource_url(
            "https://github.com/other/repo/issues/9\n",
            "schiste/aethyme"
        ),
        None
    );
    assert_eq!(
        created_resource_url("could not add label: dette not found\n", "schiste/aethyme"),
        None
    );
    assert_eq!(
        created_resource_url_if_complete(
            "https://github.com/schiste/aethyme/issues/184\n",
            "schiste/aethyme",
            true,
        ),
        None,
        "a URL in a truncated prefix is not complete create evidence"
    );
}

#[test]
fn truncated_github_listing_is_not_parsed_even_when_the_prefix_is_valid_json() {
    let prefix = br#"[{"number":1}]"#;
    assert!(parse_github_listing(prefix, true).is_none());
    assert_eq!(parse_github_listing(prefix, false).unwrap().len(), 1);
}

/// The watermark is what separates the resource this run created from one
/// that merely carries the same title.
#[test]
fn only_a_number_above_the_watermark_proves_this_run_created_it() {
    let plan = issue_plan("a bug", Some(10));
    let (status, evidence) =
        classify_create_observation(&plan, 10, &listed(&[(11, "a bug"), (9, "older")]));
    assert_eq!(status, OperationStatus::Succeeded);
    assert_eq!(evidence["classification"], "succeeded");
    assert_eq!(evidence["number"], 11);

    // Same title, but it predates the command: this run created nothing.
    let (status, evidence) =
        classify_create_observation(&plan, 10, &listed(&[(9, "a bug"), (8, "older")]));
    assert_eq!(status, OperationStatus::Failed);
    assert_eq!(evidence["classification"], "failed");
}

/// A page that never reached back to the watermark leaves a gap the create
/// could be hiding in, and a wrong "failed" is what makes a blind retry
/// look safe (#184).
#[test]
fn a_listing_that_stops_above_the_watermark_stays_unknown() {
    let plan = issue_plan("a bug", Some(10));
    let full: Vec<(i64, &str)> = (0..CREATE_OBSERVATION_LIMIT)
        .map(|index| (200 - index as i64, "unrelated"))
        .collect();
    let (status, evidence) = classify_create_observation(&plan, 10, &listed(&full));
    assert_eq!(status, OperationStatus::OutcomeUnknown);
    assert_eq!(
        evidence["reason"],
        "post_create_listing_did_not_reach_the_watermark"
    );

    // One entry short of a full page is a listing that ran out, not one
    // that was truncated, so it does prove absence.
    let (status, _) = classify_create_observation(&plan, 10, &listed(&full[1..]));
    assert_eq!(status, OperationStatus::Failed);
}

fn gh_args(args: &[&str]) -> Vec<String> {
    args.iter().map(|arg| (*arg).to_string()).collect()
}

/// The 2026-10-03 report: GitHub refused `pr update-branch` for a
/// conflict, the PR head never moved, and the repository was write-blocked
/// anyway. A definitive refusal of the command's only mutation is failed.
#[test]
fn a_github_refusal_of_the_only_mutation_is_failed() {
    let refusal = classify_github_refusal(
        &gh_args(&["pr", "update-branch", "506"]),
        Some(1),
        b"X Cannot update PR branch due to conflicts\n",
    )
    .expect("a conflict refusal is definitive");
    assert_eq!(refusal["failure_class"], "github_refused");
    assert_eq!(
        refusal["evidence"]["message"],
        "Cannot update PR branch due to conflicts"
    );

    let refusal = classify_github_refusal(
        &gh_args(&["api", "-X", "PATCH", "repos/o/r/pulls/1", "-f", "base=x"]),
        Some(1),
        b"gh: Validation Failed (HTTP 422)\n",
    )
    .expect("a 422 on a single request is definitive");
    assert_eq!(refusal["evidence"]["http_status"], 422);

    assert!(
        classify_github_refusal(
            &gh_args(&["pr", "merge", "7", "--squash"]),
            Some(1),
            b"X Pull request #7 is not mergeable: the merge commit cannot be cleanly created.\n",
        )
        .is_some()
    );
    assert!(
        classify_github_refusal(
            &gh_args(&["pr", "update-branch", "7"]),
            Some(1),
            b"HTTP 422: Validation Failed (https://api.github.com/graphql)\n",
        )
        .is_some()
    );
}

/// #549: GitHub refused to enable auto-merge on a repository where it is
/// disabled. The PR never changed, yet the write was recorded as unknown.
#[test]
fn a_graphql_refusal_of_the_commands_own_mutation_is_failed() {
    let refusal = classify_github_refusal(
        &gh_args(&["pr", "merge", "12", "--squash", "--auto"]),
        Some(1),
        b"GraphQL: Auto merge is not allowed for this repository (enablePullRequestAutoMerge)\n",
    )
    .expect("a GraphQL refusal of pr merge's own mutation is definitive");
    assert_eq!(refusal["failure_class"], "github_refused");
    assert_eq!(
        refusal["evidence"]["graphql_mutation"],
        "enablePullRequestAutoMerge"
    );
    assert!(
        refusal["evidence"]["message"]
            .as_str()
            .unwrap()
            .contains("Auto merge is not allowed")
    );
    assert!(
        classify_github_refusal(
            &gh_args(&["pr", "merge", "12", "--merge"]),
            Some(1),
            b"GraphQL: Merge commits are not allowed on this repository. (mergePullRequest)\n",
        )
        .is_some()
    );

    // A server failure reported through GraphQL may still have run.
    assert!(
            classify_github_refusal(
                &gh_args(&["pr", "merge", "12", "--squash"]),
                Some(1),
                b"GraphQL: Something went wrong while executing your query. This may be the result of a timeout (mergePullRequest)\n",
            )
            .is_none()
        );
    // An error naming some other mutation is not this command's refusal.
    assert!(
            classify_github_refusal(
                &gh_args(&["pr", "update-branch", "12"]),
                Some(1),
                b"GraphQL: Auto merge is not allowed for this repository (enablePullRequestAutoMerge)\n",
            )
            .is_none()
        );
    // A command not known to send a single mutation stays unknown.
    assert!(
        classify_github_refusal(
            &gh_args(&["pr", "edit", "12", "--add-label", "x"]),
            Some(1),
            b"GraphQL: Could not resolve to a node (addLabelsToLabelable)\n",
        )
        .is_none()
    );
}

/// Ambiguity stays unknown: a transport error, a 5xx, a killed process, a
/// paginated or multi-step command, or a message nobody listed could all
/// sit next to a write that landed.
#[test]
fn an_ambiguous_github_failure_is_not_a_refusal() {
    let update = gh_args(&["pr", "update-branch", "506"]);
    let conflict: &[u8] = b"X Cannot update PR branch due to conflicts\n";
    // Killed by a signal: no exit code at all.
    assert!(classify_github_refusal(&update, None, conflict).is_none());
    // Any other exit status is not gh's ordinary refusal.
    assert!(classify_github_refusal(&update, Some(2), conflict).is_none());
    // Network trouble says nothing about the server's answer.
    assert!(
        classify_github_refusal(
            &update,
            Some(1),
            b"Post \"https://api.github.com/graphql\": read: connection reset by peer\n",
        )
        .is_none()
    );
    // A 5xx is not a rejection, even next to a refusal-looking message.
    assert!(
        classify_github_refusal(
            &update,
            Some(1),
            b"HTTP 502: Bad Gateway (https://api.github.com/graphql)\nHTTP 422: x (y)\n",
        )
        .is_none()
    );
    // A refusal message only counts for the command it describes.
    assert!(classify_github_refusal(&gh_args(&["pr", "edit", "506"]), Some(1), conflict).is_none());
    // A multi-step command may have applied an earlier step.
    assert!(
        classify_github_refusal(
            &gh_args(&["pr", "create", "--title", "t"]),
            Some(1),
            b"HTTP 422: Validation Failed (https://api.github.com/graphql)\n",
        )
        .is_none()
    );
    assert!(
        classify_github_refusal(
            &gh_args(&["api", "--paginate", "-X", "POST", "repos/o/r/x"]),
            Some(1),
            b"gh: Validation Failed (HTTP 422)\n",
        )
        .is_none()
    );
    // Rate limiting and request timeouts are not listed rejections.
    assert!(
        classify_github_refusal(
            &gh_args(&["api", "-X", "POST", "repos/o/r/x"]),
            Some(1),
            b"gh: Too Many Requests (HTTP 429)\n",
        )
        .is_none()
    );
}

#[test]
fn truncated_github_stderr_cannot_prove_a_refusal() {
    let args = gh_args(&["pr", "update-branch", "506"]);
    let refusal = b"X Cannot update PR branch due to conflicts\n";
    assert!(classify_github_refusal(&args, Some(1), refusal).is_some());
    assert!(classify_github_refusal_if_complete(&args, Some(1), refusal, true).is_none());
}

/// A create nobody could recognise afterwards is unknown, not failed.
#[test]
fn an_unplannable_create_and_a_missing_watermark_both_stay_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let (status, value) = reconcile_failed_github_create(
        tmp.path(),
        "o/r",
        &CreatePlanning::Unplannable {
            reason: "issue_create_without_an_explicit_title",
        },
        b"",
    )
    .unwrap();
    assert_eq!(status, OperationStatus::OutcomeUnknown);
    assert_eq!(value["evidence"]["reason"], "create_plan_unavailable");

    let (status, value) = reconcile_failed_github_create(
        tmp.path(),
        "o/r",
        &CreatePlanning::Planned(issue_plan("a bug", None)),
        b"",
    )
    .unwrap();
    assert_eq!(status, OperationStatus::OutcomeUnknown);
    assert_eq!(
        value["evidence"]["reason"],
        "pre_create_number_watermark_unavailable"
    );
}

/// Nothing to reconcile for a command that creates nothing: the caller's
/// existing conservative fallback still applies.
#[test]
fn a_command_that_creates_nothing_is_left_to_the_conservative_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        reconcile_failed_github_create(
            tmp.path(),
            "o/r",
            &CreatePlanning::NotApplicable,
            b"whatever",
        )
        .is_none()
    );
}

/// The scope must come from the command line alone: it decides which lock
/// to queue for, so it has to be known before the operation queues (#181).
#[test]
fn comment_and_review_writes_resolve_to_a_per_resource_scope() {
    assert_eq!(
        scope(&["pr", "comment", "7", "--body", "hi"]).as_deref(),
        Some("pull/7/comments")
    );
    assert_eq!(
        scope(&["issue", "comment", "12", "--body", "hi"]).as_deref(),
        Some("issue/12/comments")
    );
    assert_eq!(
        scope(&["pr", "review", "7", "--approve"]).as_deref(),
        Some("pull/7/reviews")
    );
    assert_eq!(
        scope(&["api", "repos/o/r/pulls/7/reviews", "--method", "POST"]).as_deref(),
        Some("pull/7/reviews")
    );
    assert_eq!(
        scope(&["api", "repos/o/r/issues/12/comments", "-f", "body=hi"]).as_deref(),
        Some("issue/12/comments")
    );

    let reply = review_thread_api(
        crate::gh_ref_guard::REVIEW_THREAD_REPLY_QUERY,
        "PRRT_kwDOabc",
        Some("Thanks"),
    );
    let resolve = review_thread_api(
        crate::gh_ref_guard::REVIEW_THREAD_RESOLVE_QUERY,
        "PRRT_kwDOabc",
        None,
    );
    let other_thread = review_thread_api(
        crate::gh_ref_guard::REVIEW_THREAD_RESOLVE_QUERY,
        "PRRT_kwDOdef",
        None,
    );
    let lookup = review_thread_api(
        crate::gh_ref_guard::REVIEW_THREAD_LOOKUP_QUERY,
        "PRRT_kwDOabc",
        None,
    );
    let reply_scope = resource_lock_scope(OperationProvider::Github, &reply);
    assert!(
        reply_scope
            .as_deref()
            .is_some_and(|scope| scope.starts_with("pull/review-thread/")),
        "review-thread GraphQL writes need a thread-specific lock"
    );
    assert_eq!(
        resource_lock_scope(OperationProvider::Github, &resolve),
        reply_scope,
        "reply and resolve on one thread must serialize together"
    );
    assert_ne!(
        resource_lock_scope(OperationProvider::Github, &other_thread),
        resource_lock_scope(OperationProvider::Github, &reply),
        "different threads may proceed independently"
    );
    assert_eq!(
        resource_lock_scope(
            OperationProvider::Github,
            &review_thread_api(
                "mutation { createRef(input: {}) { ref { id } } }",
                "PRRT_kwDOabc",
                None
            )
        ),
        None,
        "unknown GraphQL mutations must keep the repository-wide lock"
    );
    assert_eq!(
        resource_lock_scope(OperationProvider::Github, &lookup),
        None,
        "the verified target lookup is read-only and does not need a write lock"
    );
}

/// A flag's value is not an operand. `--method POST` in front of the path
/// must not make `POST` the resource.
#[test]
fn a_flag_value_is_never_read_as_the_resource() {
    assert_eq!(
        scope(&["api", "--method", "POST", "repos/o/r/pulls/7/reviews"]).as_deref(),
        Some("pull/7/reviews")
    );
    assert_eq!(
        scope(&["pr", "comment", "--body", "9", "7"]).as_deref(),
        Some("pull/7/comments")
    );
}

/// Everything not provably ref-free keeps the repository lock. These are
/// the cases that must NOT narrow: a merge moves a ref, a protection
/// change is a repository setting, and a selector this cannot resolve
/// without asking the provider is not a resource it may assume.
#[test]
fn anything_not_provably_resource_scoped_stays_repository_wide() {
    assert_eq!(scope(&["pr", "merge", "7"]), None);
    assert_eq!(scope(&["pr", "create", "--title", "x"]), None);
    assert_eq!(scope(&["issue", "create"]), None);
    assert_eq!(
        scope(&[
            "api",
            "repos/o/r/branches/main/protection",
            "--method",
            "PUT"
        ]),
        None
    );
    // A URL or branch selector, not a number.
    assert_eq!(
        scope(&["pr", "comment", "https://github.com/o/r/pull/7"]),
        None
    );
    // Deeper than the collection endpoint: a reaction, not the thread.
    assert_eq!(
        scope(&[
            "api",
            "repos/o/r/issues/comments/99/reactions",
            "--method",
            "POST"
        ]),
        None
    );
    // A different provider never narrows.
    assert_eq!(
        resource_lock_scope(OperationProvider::Git, &gh(&["push", "origin", "main"])),
        None
    );
}

/// Repository-wide operations keep the bare canonical key, so #166's
/// "one repository, one lock" is untouched for anything touching a ref.
#[test]
fn lock_keys_separate_resources_without_fragmenting_the_repository() {
    let repo = "github.com/o/r";
    assert_eq!(coordination_lock_key(repo, None), repo);
    assert_ne!(
        coordination_lock_key(repo, Some("pull/7/comments")),
        coordination_lock_key(repo, None)
    );
    assert_ne!(
        coordination_lock_key(repo, Some("pull/7/comments")),
        coordination_lock_key(repo, Some("pull/9/comments")),
    );
    assert_eq!(
        coordination_lock_key(repo, Some("pull/7/comments")),
        coordination_lock_key(repo, Some("pull/7/comments")),
    );
    // The host key validator rejects credential and URL syntax; the
    // separator must survive it.
    assert!(!coordination_lock_key(repo, Some("pull/7/comments")).contains('#'));
}
use super::*;

/// #179's security finding. The coordinated write performs the remote
/// mutation; the pre-push dry run beside it decides whether that mutation
/// is allowed. They must be the same binary, and comparing the *program*
/// rather than the name is the whole assertion -- both spell "git", and
/// only one of them is the one the probe accepted.
#[test]
fn a_coordinated_git_operation_spawns_the_probed_binary() {
    assert_eq!(
        provider_command(OperationProvider::Git).get_program(),
        crate::git::git_command().get_program(),
        "the coordinated write must spawn the binary the dry run verified"
    );

    // The equality above holds trivially if both sides are the bare name
    // `git`, which is exactly what `git_command` falls back to when no
    // candidate probes clean. Where a candidate did, the probe resolved an
    // absolute path, and a regression to `Command::new("git")` becomes
    // visible rather than equal by coincidence.
    if matches!(
        crate::git::git_output_trust(),
        crate::git::GitOutputTrust::Undecorated { .. }
    ) {
        let program = provider_command(OperationProvider::Git);
        assert!(
            Path::new(program.get_program()).is_absolute(),
            "an accepted git resolves to a path, not to whatever PATH offers next: {:?}",
            program.get_program()
        );
    } else {
        eprintln!("no git on PATH probed clean; only the equality was checked");
    }

    // `gh` is resolved once to an absolute path, so the PATH a caller
    // hands the child never chooses the binary (#393).
    assert!(
        Path::new(provider_command(OperationProvider::Github).get_program()).is_absolute(),
        "gh runs by absolute path: {:?}",
        provider_command(OperationProvider::Github).get_program()
    );
}

#[test]
fn gh_is_trusted_only_from_fixed_directories_and_safe_files() {
    use std::os::unix::fs::PermissionsExt as _;
    // Release resolution never consults PATH.
    assert_eq!(
        gh_search_dirs(false),
        TRUSTED_TOOL_DIRS
            .iter()
            .map(PathBuf::from)
            .collect::<Vec<_>>()
    );
    let dir = tempfile::tempdir().unwrap();
    let gh = dir.path().join("gh");
    std::fs::write(&gh, "#!/bin/sh\n").unwrap();
    for (mode, trusted) in [
        (0o755, true),
        (0o775, false),
        (0o757, false),
        (0o644, false),
    ] {
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(mode)).unwrap();
        assert_eq!(
            trusted_tool("gh", &[dir.path().to_path_buf()]).is_some(),
            trusted,
            "mode {mode:o}"
        );
    }
}

#[test]
fn wait_durations_read_naturally() {
    assert_eq!(humanize_duration(9), "9s");
    assert_eq!(humanize_duration(59), "59s");
    assert_eq!(humanize_duration(1_688), "28m 8s");
}

fn liveness_operation(
    status: OperationStatus,
    heartbeat_age_ms: i64,
    progress_age_ms: i64,
) -> CoordinatedOperation {
    let now = unix_now_ms();
    CoordinatedOperation {
        id: 1,
        session_id: 2,
        provider: OperationProvider::Git,
        repository: "owner/repo".into(),
        scope: "repository".into(),
        effect: OperationEffect::Write,
        status,
        authorization_reason: Some("test".into()),
        command_json: "[\"git\",\"push\"]".into(),
        pid: 3,
        exit_code: None,
        details_json: Some(
            json!({
                "operation_liveness": {
                    "phase": "quality",
                    "progress": "gate 3 of 7",
                    "heartbeat_at": now - heartbeat_age_ms,
                    "progress_at": now - progress_age_ms,
                }
            })
            .to_string(),
        ),
        created_at: now,
        updated_at: now,
        finished_at: None,
        host_operation_id: None,
        identity_provenance: OperationIdentityProvenance::VerifiedCanonical,
        agent_provenance: None,
    }
}

#[test]
fn operation_liveness_distinguishes_active_progress_and_dead_heartbeat() {
    assert_eq!(
        operation_liveness_view(&liveness_operation(OperationStatus::Running, 5_000, 5_000,)).state,
        "active"
    );
    assert_eq!(
        operation_liveness_view(&liveness_operation(OperationStatus::Running, 5_000, 61_000,))
            .state,
        "progress_stale"
    );
    assert_eq!(
        operation_liveness_view(&liveness_operation(OperationStatus::Running, 31_000, 5_000,))
            .state,
        "heartbeat_stale"
    );
}

#[test]
fn progress_file_accepts_json_and_plain_text_events() {
    let file = tempfile::NamedTempFile::new().unwrap();
    append_progress_event(file.path(), "gate 2 of 7", Some("quality"));
    std::fs::OpenOptions::new()
        .append(true)
        .open(file.path())
        .unwrap()
        .write_all(b"gate 3 of 7\n")
        .unwrap();
    let state = Arc::new(Mutex::new(OperationHeartbeatState {
        phase: "starting".into(),
        progress: "none".into(),
        last_progress_at: 0,
        output_bytes: 0,
    }));
    let mut consumed = 0;
    consume_progress_file(file.path(), &mut consumed, &state);
    let state = state.lock().unwrap();
    assert_eq!(state.phase, "quality");
    assert_eq!(state.progress, "gate 3 of 7");
    assert!(state.last_progress_at > 0);
}

/// Issue #138: a blocked caller saw nothing at all, so a long hold was
/// indistinguishable from a dead command and got re-issued.
#[test]
fn a_contended_lock_names_the_operation_holding_it() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("broker.db");
    // Open once so the schema exists, then seed the owning session directly.
    drop(crate::BrokerStore::open(&db).unwrap());
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "INSERT INTO sessions (
                 id, worktree_path, branch, origin, status,
                 created_at, updated_at, last_activity_at
             ) VALUES (37, '/repo/one', 'agent/one', 'adopted', 'active', 1, 1, 1);",
    )
    .unwrap();
    drop(conn);
    let mut store = crate::BrokerStore::open(&db).unwrap();
    let created = store
        .create_coordinated_operation(&crate::NewCoordinatedOperation {
            session_id: 37,
            provider: OperationProvider::Git,
            repository: "owner/repo".into(),
            scope: "refs/heads/feature".into(),
            effect: OperationEffect::Write,
            authorization_reason: Some("test".into()),
            command_json: "[\"push\"]".into(),
            pid: std::process::id() as i64,
            host_operation_id: None,
            identity_provenance: crate::OperationIdentityProvenance::VerifiedCanonical,
        })
        .unwrap();
    store
        .transition_coordinated_operation(created.id, OperationStatus::Running, None, None)
        .unwrap();

    let described = lock_holder_info(&mut store, "owner/repo").description;
    assert!(
        described.contains(&format!("operation {}", created.id))
            && described.contains("session 37")
            && described.contains("refs/heads/feature"),
        "the notice must identify the holder: {described}"
    );

    // A repository with nothing running must not claim a phantom holder.
    let other = lock_holder_info(&mut store, "owner/elsewhere").description;
    assert!(
        other.contains("has not recorded itself"),
        "an unregistered holder must be reported as such: {other}"
    );
}

fn args(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).into()).collect()
}

/// A repository with one commit on `main`, a tag, and a GitHub remote.
fn push_fixture() -> (tempfile::TempDir, crate::ResolvedRemoteTarget) {
    let tmp = tempfile::tempdir().unwrap();
    let run = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(tmp.path())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    };
    run(&["init", "-q", "-b", "main"]);
    std::fs::write(tmp.path().join("a.txt"), "a\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-qm", "init"]);
    run(&["tag", "v1.0.0"]);
    // A real local remote: planning queries pre-push SHAs, so the remote
    // must be reachable for the plan to resolve.
    let bare = tmp.path().join("remote.git");
    let init = std::process::Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&bare)
        .output()
        .unwrap();
    assert!(init.status.success());
    run(&["remote", "add", "origin", bare.to_str().unwrap()]);
    let repo = crate::GitRepo::discover(tmp.path()).unwrap();
    let target = repo.resolve_remote_target("origin", None).unwrap();
    (tmp, target)
}

#[test]
fn only_head_relative_push_sources_are_worktree_relative() {
    let argv = |items: &[&str]| {
        items
            .iter()
            .map(|item| item.to_string())
            .collect::<Vec<_>>()
    };

    // `HEAD` and its relatives mean something different in each worktree.
    for source in [
        "HEAD:refs/heads/x",
        "HEAD",
        "+HEAD:refs/heads/x",
        "HEAD~1:refs/heads/x",
        "@",
        "@{u}",
    ] {
        let args = argv(&["push", "origin", source]);
        assert_eq!(
            worktree_relative_push_sources(&args),
            vec![source.to_string()],
            "{source} should be recognised as worktree-relative"
        );
    }

    // Refs under `refs/` are shared by every worktree, so a branch name
    // resolves identically wherever the command runs. Refusing these would
    // block safe pushes without catching anything.
    for source in [
        "main:refs/heads/x",
        "refs/heads/main:refs/heads/x",
        "deadbeef:refs/heads/x",
    ] {
        let args = argv(&["push", "origin", source]);
        assert!(
            worktree_relative_push_sources(&args).is_empty(),
            "{source} is shared across worktrees and must be allowed"
        );
    }

    // Options are not refspecs.
    let args = argv(&[
        "push",
        "--force-with-lease=refs/heads/x:abc",
        "origin",
        "abc:refs/heads/x",
    ]);
    assert!(worktree_relative_push_sources(&args).is_empty());

    // Anything that is not a push is none of this function's business.
    let args = argv(&["log", "HEAD"]);
    assert!(worktree_relative_push_sources(&args).is_empty());
}

#[test]
fn containment_survives_a_symlinked_temporary_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("worktree");
    std::fs::create_dir_all(root.join("nested/deeper")).unwrap();

    assert!(is_within(&root, &root), "a worktree contains itself");
    assert!(is_within(&root.join("nested/deeper"), &root));
    assert!(
        !is_within(tmp.path(), &root),
        "the parent is not inside the worktree"
    );

    let sibling = tmp.path().join("elsewhere");
    std::fs::create_dir_all(&sibling).unwrap();
    assert!(!is_within(&sibling, &root), "a sibling checkout is outside");
}

fn planned_destinations(
    tmp: &tempfile::TempDir,
    target: &crate::ResolvedRemoteTarget,
    argv: &[&str],
) -> Vec<String> {
    match plan_exact_push(tmp.path(), &args(argv), Some(target)) {
        PushPlanning::Planned(plan) => plan
            .destinations
            .iter()
            .map(|d| d.destination_ref.clone())
            .collect(),
        other => panic!("expected a plan for {argv:?}, got {other:?}"),
    }
}

/// A push written without an explicit `src:dst` must still be planable:
/// an unplanned push cannot be classified from remote evidence, so any
/// failure of it write-blocks the repository (issue #131).
#[test]
fn implicit_push_refspecs_resolve_to_their_destination_ref() {
    let (tmp, target) = push_fixture();
    assert_eq!(
        planned_destinations(&tmp, &target, &["push", "-u", "origin", "HEAD"]),
        vec!["refs/heads/main"],
        "`push -u origin HEAD` pushes the current branch"
    );
    assert_eq!(
        planned_destinations(&tmp, &target, &["push", "origin", "main"]),
        vec!["refs/heads/main"],
        "a bare branch name pushes to the same branch"
    );
    assert_eq!(
        planned_destinations(&tmp, &target, &["push", "origin", "refs/tags/v1.0.0"]),
        vec!["refs/tags/v1.0.0"],
        "a fully qualified tag ref pushes to itself"
    );
    // An explicit refspec keeps working unchanged.
    assert_eq!(
        planned_destinations(&tmp, &target, &["push", "origin", "HEAD:refs/heads/main"]),
        vec!["refs/heads/main"]
    );
}

/// The case from issue #131: a local pre-push hook rejects the push, so no
/// ref moved. With the refspec planable, remote evidence proves the write
/// failed, which must classify as `failed` — not as an unknown outcome that
/// write-blocks the repository.
#[test]
fn a_push_that_moved_no_ref_classifies_as_failed() {
    let (tmp, target) = push_fixture();
    for argv in [
        vec!["push", "-u", "origin", "HEAD"],
        vec!["push", "origin", "main"],
        vec!["push", "origin", "refs/tags/v1.0.0"],
    ] {
        let planning = plan_exact_push(tmp.path(), &args(&argv), Some(&target));
        let (status, value) =
            reconcile_failed_push(tmp.path(), &planning, None).expect("a planned push reconciles");
        assert_eq!(
            status,
            OperationStatus::Failed,
            "{argv:?} moved no ref and must classify as failed, got {value}"
        );
        assert_eq!(value["evidence"]["classification"], "failed", "{argv:?}");
    }
}

/// Resolution must stay conservative: anything that is not exactly one
/// local ref still refuses to plan rather than inventing a destination.
#[test]
fn unresolvable_push_refspecs_still_refuse_to_plan() {
    let (tmp, target) = push_fixture();
    let repo = crate::GitRepo::discover(tmp.path()).unwrap();
    let sha = repo.resolve_push_source("HEAD").unwrap();
    for argv in [
        vec!["push", "origin", "no-such-branch"],
        vec!["push", "origin", sha.as_str()],
    ] {
        match plan_exact_push(tmp.path(), &args(&argv), Some(&target)) {
            PushPlanning::Unsupported { reason } => assert_eq!(
                reason, "push_refspec_does_not_resolve_to_one_local_ref",
                "{argv:?}"
            ),
            other => panic!("expected refusal for {argv:?}, got {other:?}"),
        }
    }
}

#[test]
fn classifiers_fail_closed_and_detect_destructive_operations() {
    assert_eq!(
        classify_git(&args(&["status"])),
        Some(OperationEffect::Read)
    );
    assert_eq!(classify_git(&args(&["push"])), Some(OperationEffect::Write));
    assert_eq!(
        classify_git(&args(&["push", "--force-with-lease"])),
        Some(OperationEffect::Destructive)
    );
    assert_eq!(classify_git(&args(&["unknown-extension"])), None);
    assert_eq!(
        classify_git(&args(&[
            "-C",
            "/tmp/linked-worktree",
            "merge",
            "--ff-only",
            "abc123"
        ])),
        Some(OperationEffect::Write)
    );
    assert_eq!(classify_git(&args(&["-C"])), None);

    assert_eq!(
        classify_gh(&args(&["pr", "view", "12"])),
        Some(OperationEffect::Read)
    );
    assert_eq!(
        classify_gh(&args(&["pr", "merge", "12"])),
        Some(OperationEffect::Write)
    );
    assert_eq!(
        classify_gh(&args(&["api", "repos/o/r", "--method", "DELETE"])),
        Some(OperationEffect::Destructive)
    );
    assert_eq!(classify_gh(&args(&["extension", "exec", "x"])), None);
}

/// Every classification bypass found in the 2026-09-23 audit, one row
/// each. A row that stops holding reopens a way to hide a destructive or
/// remote write behind a milder label.
#[test]
fn classification_closes_the_audited_bypasses() {
    use OperationEffect::{Destructive, Read, Write};
    let git_rows: &[(&[&str], Option<OperationEffect>)] = &[
        (&["push", "origin", "+main:main"], Some(Destructive)),
        (&["push", "-uf", "origin", "main"], Some(Destructive)),
        (&["push", "-d", "origin", "topic"], Some(Destructive)),
        (&["push", "-u", "origin", "topic"], Some(Write)),
        (&["clean", "-fdx"], Some(Destructive)),
        (&["clean", "-xdf"], Some(Destructive)),
        (&["branch", "-Df", "topic"], Some(Destructive)),
        (&["branch", "-f", "topic", "HEAD~1"], Some(Destructive)),
        (&["branch", "-vv"], Some(Read)),
        (&["tag", "-fa", "v1", "-m", "release"], Some(Destructive)),
        (&["reset", "--hard", "HEAD~1"], Some(Destructive)),
        (&["reset", "HEAD~1"], Some(Write)),
        (&["update-ref", "-d", "refs/heads/main"], Some(Destructive)),
        (&["send-pack", "origin", "+main:main"], Some(Destructive)),
        (&["rev-list", "--count", "HEAD"], Some(Read)),
        (&["config", "--get", "user.name"], Some(Read)),
        (&["config", "user.name", "x"], Some(Write)),
    ];
    for (row, expected) in git_rows {
        assert_eq!(classify_git(&args(row)), *expected, "git {row:?}");
    }
    assert_eq!(
        classify_gh(&args(&["api", "-XDELETE", "repos/o/r/git/refs/heads/x"])),
        Some(Destructive)
    );
    assert_eq!(
        classify_gh(&args(&["api", "-Xget", "repos/o/r"])),
        Some(Read)
    );
}

#[test]
fn provider_read_budgets_keep_github_bounded_and_git_unchanged() {
    assert_eq!(GITHUB_READ_OPERATION_BUDGET, Duration::from_secs(30));
    assert_eq!(GIT_READ_OPERATION_BUDGET, Duration::from_secs(90));
    assert_eq!(
        read_operation_budget_from(None, GITHUB_READ_OPERATION_BUDGET),
        Duration::from_secs(30)
    );
    assert_eq!(
        read_operation_budget_from(None, GIT_READ_OPERATION_BUDGET),
        Duration::from_secs(90)
    );
    assert_eq!(
        read_operation_budget_from(Some("7"), GITHUB_READ_OPERATION_BUDGET),
        Duration::from_secs(7),
        "a configured budget must remain available"
    );
    assert_eq!(
        read_operation_budget_from(Some("0"), GITHUB_READ_OPERATION_BUDGET),
        Duration::from_secs(30),
        "zero is not a usable operation budget"
    );
    assert_eq!(
        read_operation_budget_from(Some("invalid"), GIT_READ_OPERATION_BUDGET),
        Duration::from_secs(90),
        "invalid configuration must fall back to the provider default"
    );
}

#[test]
fn an_unbounded_read_gets_the_read_budget_and_writes_keep_theirs() {
    // #555/#706: an ordinary GitHub read gets a provider deadline by default.
    let issue_list = gh(&["issue", "list"]);
    let issue_list_effect = classify_gh(&issue_list).expect("issue list is classified");
    assert_eq!(issue_list_effect, OperationEffect::Read);
    let issue_list_long_running = is_long_running_read(OperationProvider::Github, &issue_list);
    assert!(
        !issue_list_long_running,
        "an ordinary issue list should get the default read bound"
    );
    let read = AdmissionDeadline::start(QueueWait::Forever).bounded_for(
        issue_list_effect,
        issue_list_long_running,
        OperationProvider::Github,
    );
    let configured = std::env::var("AETHYME_BROKER_READ_BUDGET_SECS").ok();
    assert_eq!(
        read.budget,
        Some(read_operation_budget_from(
            configured.as_deref(),
            GITHUB_READ_OPERATION_BUDGET,
        ))
    );
    assert!(read.at.is_some());

    // Git keeps its longer default; a caller's own bound still wins.
    let git_status = args(&["status"]);
    let git_status_effect = classify_git(&git_status).expect("git status is classified");
    assert_eq!(git_status_effect, OperationEffect::Read);
    let git_read = AdmissionDeadline::start(QueueWait::Forever).bounded_for(
        git_status_effect,
        false,
        OperationProvider::Git,
    );
    assert_eq!(
        git_read.budget,
        Some(read_operation_budget_from(
            configured.as_deref(),
            GIT_READ_OPERATION_BUDGET,
        ))
    );

    // A caller's own bound wins, and writes keep the wait they asked for.
    let explicit = AdmissionDeadline::start(QueueWait::Seconds(5)).bounded_for(
        OperationEffect::Read,
        false,
        OperationProvider::Github,
    );
    assert_eq!(explicit.budget, Some(Duration::from_secs(5)));
    for effect in [OperationEffect::Write, OperationEffect::Destructive] {
        let write = AdmissionDeadline::start(QueueWait::Forever).bounded_for(
            effect,
            false,
            OperationProvider::Github,
        );
        assert_eq!(write.budget, None, "{effect:?} must stay unbounded");
        assert!(write.at.is_none());
    }
}

/// #555 review: a read that is long by design skips the automatic default
/// budget; caller-selected `--queue-timeout` and `--no-wait` bounds still apply.
fn assert_long_running_read_is_exempt(args: &[&str]) {
    let args = gh(args);
    assert_eq!(classify_gh(&args), Some(OperationEffect::Read), "{args:?}");
    assert!(
        is_long_running_read(OperationProvider::Github, &args),
        "{args:?}"
    );
    let long_running = is_long_running_read(OperationProvider::Github, &args);
    let admission = AdmissionDeadline::start(QueueWait::Forever).bounded_for(
        OperationEffect::Read,
        long_running,
        OperationProvider::Github,
    );
    assert_eq!(admission.budget, None, "{args:?} must stay unbounded");

    let no_wait = AdmissionDeadline::start(QueueWait::Refuse).bounded_for(
        OperationEffect::Read,
        long_running,
        OperationProvider::Github,
    );
    assert_eq!(
        no_wait.budget,
        Some(NO_WAIT_ADMISSION_BUDGET),
        "`--no-wait` remains bounded for {args:?}"
    );

    let explicit = AdmissionDeadline::start(QueueWait::Seconds(5)).bounded_for(
        OperationEffect::Read,
        long_running,
        OperationProvider::Github,
    );
    assert_eq!(
        explicit.budget,
        Some(Duration::from_secs(5)),
        "an explicit caller bound still applies to {args:?}"
    );
}

#[test]
fn run_watch_is_exempt_from_the_read_budget() {
    assert_long_running_read_is_exempt(&["run", "watch", "123"]);
}

#[test]
fn pr_checks_watch_is_exempt_from_the_read_budget() {
    assert_long_running_read_is_exempt(&["pr", "checks", "12", "--watch"]);
}

#[test]
fn run_view_log_is_exempt_from_the_read_budget() {
    assert_long_running_read_is_exempt(&["run", "view", "123", "--log"]);
    assert_long_running_read_is_exempt(&["run", "view", "123", "--log-failed"]);
}

#[test]
fn downloads_are_exempt_from_the_read_budget() {
    assert_long_running_read_is_exempt(&["run", "download", "123"]);
    assert_long_running_read_is_exempt(&["release", "download", "v1.0.0"]);
}

#[test]
fn paginated_api_reads_are_exempt_from_the_read_budget() {
    assert_long_running_read_is_exempt(&["api", "repos/o/r/issues", "--paginate"]);
}

#[test]
fn short_reads_and_git_keep_the_read_budget() {
    for args in [
        gh(&["api", "repos/o/r/issues/230/comments?per_page=100&page=1"]),
        gh(&["pr", "view", "12"]),
        gh(&["pr", "checks", "12"]),
        gh(&["run", "view", "123"]),
    ] {
        assert!(
            !is_long_running_read(OperationProvider::Github, &args),
            "{args:?}"
        );
    }
    assert!(!is_long_running_read(
        OperationProvider::Git,
        &gh(&["log", "--watch"])
    ));
}

#[test]
fn humanize_ms_reads_at_every_scale() {
    assert_eq!(humanize_ms(850), "850ms");
    assert_eq!(humanize_ms(4_200), "4.2s");
    assert_eq!(humanize_ms(91_000), "1m 31s");
}

#[test]
fn a_repeated_program_name_is_refused_before_classification() {
    assert!(
        refuse_repeated_program_name(OperationProvider::Git, &args(&["git", "add", "x"])).is_err()
    );
    assert!(
        refuse_repeated_program_name(
            OperationProvider::Git,
            &args(&["-C", "/tmp/r", "git", "status"])
        )
        .is_err()
    );
    assert!(
        refuse_repeated_program_name(OperationProvider::Github, &args(&["gh", "pr", "merge"]))
            .is_err()
    );
    assert!(
        refuse_repeated_program_name(OperationProvider::Git, &args(&["push", "origin", "main"]))
            .is_ok()
    );
    assert!(
        refuse_repeated_program_name(OperationProvider::Github, &args(&["pr", "view", "1"]))
            .is_ok()
    );
}

#[test]
fn an_unrecognized_command_cannot_be_declared_a_read() {
    assert!(resolve_effect(None, Some(OperationEffect::Read)).is_err());
    assert!(resolve_effect(None, Some(OperationEffect::Write)).is_ok());
    assert!(resolve_effect(Some(OperationEffect::Read), Some(OperationEffect::Read)).is_ok());
}

#[test]
fn code_executing_git_config_is_refused_before_the_subcommand() {
    for row in [
        &["-c", "alias.p=push", "p", "origin", "HEAD:main"][..],
        &["-calias.p=push", "p"][..],
        &["-c", "Core.HooksPath=/tmp/h", "commit", "-m", "x"][..],
        &["--config-env=core.sshCommand=SSH", "fetch"][..],
        &["--exec-path=/tmp/x", "status"][..],
    ] {
        assert!(
            refuse_code_executing_git_options(&args(row)).is_err(),
            "{row:?} must be refused"
        );
    }
    for row in [
        &["-c", "user.name=x", "commit", "-m", "x"][..],
        &["-C", "/tmp/repo", "status"][..],
        // Subcommand options are not global config: `commit -c` reuses a message.
        &["commit", "-c", "HEAD"][..],
    ] {
        assert!(
            refuse_code_executing_git_options(&args(row)).is_ok(),
            "{row:?} must be allowed"
        );
    }
}

#[test]
fn repository_lock_reports_progress_while_waiting() {
    let root = tempfile::tempdir().unwrap();
    let held = RepositoryWriteLock::acquire(
        root.path(),
        "owner/repo",
        1,
        None,
        || Ok("session 1".into()),
        QueueWait::Refuse,
        |_, _| {},
    )
    .unwrap();
    let root_path = root.path().to_path_buf();
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        RepositoryWriteLock::acquire_with_progress_interval(
            &root_path,
            "owner/repo",
            (2, Some(9)),
            || Ok("session 1".into()),
            QueueWait::Seconds(3),
            Duration::from_millis(50),
            move |holder, elapsed| {
                sender.send((holder.to_owned(), elapsed)).unwrap();
            },
        )
    });
    let progress = receiver.recv_timeout(Duration::from_secs(2));
    // Another terminal's `broker status` sees who waits, on what, behind whom.
    let waiting = crate::waiters::current_waiters(root.path(), crate::clock::epoch_ms());
    drop(held);
    let acquired = waiter.join().unwrap().unwrap();
    let (holder, elapsed) = progress.expect("a waiting writer should report progress");
    assert_eq!(holder, "session 1");
    assert_eq!(waiting.len(), 1, "{waiting:?}");
    assert_eq!(waiting[0].session_id, Some(9));
    assert_eq!(waiting[0].kind, crate::waiters::WAIT_COORDINATED_WRITE_LOCK);
    assert_eq!(waiting[0].resource, "owner/repo");
    assert_eq!(waiting[0].holder, "session 1");
    assert!(
        crate::waiters::current_waiters(root.path(), crate::clock::epoch_ms()).is_empty(),
        "an acquired lock is no longer waited for"
    );
    assert!(elapsed >= Duration::from_millis(50), "{elapsed:?}");
    assert!(acquired.queue_wait_ms > 0);
}

#[test]
fn a_failing_holder_lookup_does_not_abort_a_progress_reporting_wait() {
    let root = tempfile::tempdir().unwrap();
    let held = RepositoryWriteLock::acquire(
        root.path(),
        "owner/repo",
        1,
        None,
        || Ok("session 1".into()),
        QueueWait::Refuse,
        |_, _| {},
    )
    .unwrap();
    let root_path = root.path().to_path_buf();
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let mut lookups = 0;
        RepositoryWriteLock::acquire_with_progress_interval(
            &root_path,
            "owner/repo",
            (2, Some(9)),
            move || {
                lookups += 1;
                if lookups == 1 {
                    Ok("session 1".into())
                } else {
                    Err(BrokerOpError::RepresentationUnavailable {
                        reason: "database is locked".into(),
                    })
                }
            },
            QueueWait::Seconds(5),
            Duration::from_millis(50),
            move |holder, _| {
                let _ = sender.send(holder.to_owned());
            },
        )
    });
    let progress = receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("the wait must keep reporting after a failed holder lookup");
    drop(held);
    waiter
        .join()
        .unwrap()
        .expect("a failed progress lookup must not abort the wait");
    assert_eq!(progress, "holder unavailable");
}

/// `--no-wait` must still bound its own preparation. Without a budget it
/// inherits the unbounded wait it exists to avoid (#219).
#[test]
fn no_wait_admission_is_bounded_and_forever_is_not() {
    assert!(AdmissionDeadline::start(QueueWait::Refuse).at.is_some());
    assert!(
        AdmissionDeadline::start(QueueWait::Seconds(60))
            .at
            .is_some()
    );
    assert!(AdmissionDeadline::start(QueueWait::Forever).at.is_none());
    assert_eq!(
        AdmissionDeadline::start(QueueWait::Forever).budget_label(),
        "unbounded"
    );
}

/// The budget is shared between preparation and the lock, so a bounded
/// request cannot spend its timeout twice.
#[test]
fn the_lock_only_gets_what_preparation_left() {
    let admission = AdmissionDeadline::start(QueueWait::Seconds(60));
    match admission.remaining_queue_wait(QueueWait::Seconds(60)) {
        QueueWait::Seconds(left) => assert!(left <= 60, "{left}"),
        other => panic!("expected a narrowed bound, got {other:?}"),
    }

    // A caller who asked to refuse still refuses; one who asked to wait
    // forever still waits.
    assert_eq!(
        admission.remaining_queue_wait(QueueWait::Refuse),
        QueueWait::Refuse
    );
    assert_eq!(
        AdmissionDeadline::start(QueueWait::Forever).remaining_queue_wait(QueueWait::Forever),
        QueueWait::Forever
    );
}

/// A provider can exit while its descendant still holds the captured pipes.
#[cfg(unix)]
#[test]
fn provider_exit_does_not_outwait_deadline_for_inherited_output_pipes() {
    let tmp = tempfile::tempdir().unwrap();
    let ready = tmp.path().join("descendant-ready");
    let release = tmp.path().join("release-descendant");
    let done = tmp.path().join("descendant-done");
    let ready_for_thread = ready.clone();
    let release_for_thread = release.clone();
    let done_for_thread = done.clone();
    let releaser = std::thread::spawn(move || {
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(8);
        while !ready_for_thread.exists() && std::time::Instant::now() < ready_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let ready_seen = ready_for_thread.exists();
        if ready_seen {
            // Keep both inherited output descriptors open beyond the provider's
            // deadline, then release the finite child so the test leaves no
            // process behind.
            std::thread::sleep(Duration::from_secs(4));
        }
        std::fs::write(release_for_thread, b"release").unwrap();
        let done_deadline = std::time::Instant::now() + Duration::from_secs(3);
        while ready_seen && !done_for_thread.exists() && std::time::Instant::now() < done_deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        (ready_seen, done_for_thread.exists())
    });

    let mut command = Command::new("sh");
    command.args([
        "-c",
        "(i=0; while [ ! -e \"$1\" ] && [ \"$i\" -lt 120 ]; do sleep 0.05; i=$((i + 1)); done; : > \"$3\") & printf child; : > \"$2\"; exit 0",
        "sh",
        release.to_str().unwrap(),
        ready.to_str().unwrap(),
        done.to_str().unwrap(),
    ]);
    let started = std::time::Instant::now();
    let budget = Duration::from_secs(2);
    let error = output_within(
        command,
        AdmissionDeadline {
            started,
            at: Some(started + budget),
            budget: Some(budget),
        },
        "owner/repo",
        "capturing provider output",
        None,
    );
    let provider_wait = started.elapsed();
    let (ready_seen, descendant_done) = releaser.join().unwrap();

    assert!(ready_seen, "the provider leader must exit before pipe EOF");
    assert!(
        descendant_done,
        "the finite descendant must exit after release"
    );
    assert!(
        provider_wait < Duration::from_secs(3),
        "reader drain exceeded the 2s operation deadline: {provider_wait:?}"
    );
    assert!(
        matches!(&error, Err(BrokerOpError::AdmissionTimedOut { .. })),
        "an inherited pipe held past the deadline must time out, got {error:?}"
    );
}

#[test]
fn capture_limit_boundary_flags_only_omitted_bytes() {
    let mut capture = OutputCaptureState::new(4);
    capture.append(b"1234");
    assert_eq!(capture.bytes, b"1234");
    assert!(!capture.truncated, "exactly-at-limit output is complete");

    capture.append(b"5");
    assert_eq!(capture.bytes, b"1234");
    assert!(capture.truncated, "the first omitted byte marks truncation");
}

#[test]
fn truncated_failure_stderr_is_not_persisted_as_a_tail() {
    let mut details = json!({"output_truncated": {"stdout": false, "stderr": true}});
    add_failure_stderr_if_complete(&mut details, b"captured prefix", true);

    assert_eq!(details["output_truncated"]["stderr"], true);
    assert!(details.get("failure_output").is_none());

    let mut complete_details = json!({});
    add_failure_stderr_if_complete(&mut complete_details, b"complete diagnostic", false);
    assert_eq!(
        complete_details["failure_output"]["stderr_tail"],
        json!(["complete diagnostic"])
    );
}

#[cfg(unix)]
#[test]
fn capture_limit_keeps_draining_and_preserves_provider_exit_status() {
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "head -c 1048576 /dev/zero; head -c 1048576 /dev/zero >&2",
    ]);
    let output = output_within_capture_limit(
        command,
        AdmissionDeadline::start(QueueWait::Seconds(10)),
        "owner/repo",
        "testing bounded output capture",
        None,
        8,
    )
    .expect("the readers keep draining after the capture limit");

    assert!(output.status.success());
    assert_eq!(output.stdout, [0; 8]);
    assert_eq!(output.stderr, [0; 8]);
    assert!(output.stdout_truncated);
    assert!(output.stderr_truncated);
}

#[test]
fn discarded_output_reader_drains_without_retaining_after_timeout() {
    struct GatedReader {
        started: Option<std::sync::mpsc::Sender<()>>,
        release: std::sync::mpsc::Receiver<()>,
        sent: bool,
    }

    impl std::io::Read for GatedReader {
        fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
            if self.sent {
                return Ok(0);
            }
            self.sent = true;
            let _notified = self.started.take().expect("one read start").send(());
            self.release
                .recv()
                .map_err(|error| std::io::Error::other(error.to_string()))?;
            let payload = b"arrived after timeout";
            buffer[..payload.len()].copy_from_slice(payload);
            Ok(payload.len())
        }
    }

    let capture = Arc::new(Mutex::new(OutputCaptureState::new(64)));
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (done_tx, _done_rx) = std::sync::mpsc::channel();
    let reader = spawn_output_reader(
        GatedReader {
            started: Some(started_tx),
            release: release_rx,
            sent: false,
        },
        Arc::clone(&capture),
        None,
        "provider stdout",
        done_tx,
    );

    started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("the detached reader reached its blocking read");
    discard_output_capture(&capture);
    release_tx.send(()).unwrap();
    join_output_reader(reader).unwrap();

    let (bytes, truncated) = finish_output_capture(&capture);
    assert!(bytes.is_empty());
    assert!(!truncated);
    let capture = capture.lock().unwrap();
    assert_eq!(
        capture.bytes.capacity(),
        0,
        "timeout releases the buffer allocation"
    );
}

/// The defect itself: preparation that outruns the budget must be killed
/// and reported, not waited on. A wedged remote is what this stands in for.
#[test]
fn preparation_that_outruns_the_budget_is_killed_and_reported() {
    let mut sleeper = Command::new("sleep");
    sleeper.arg("30");
    let started = std::time::Instant::now();
    let error = output_within(
        sleeper,
        AdmissionDeadline::start(QueueWait::Seconds(1)),
        "owner/repo",
        "running the pre-push dry run",
        None,
    )
    .expect_err("a child outliving the budget must not be waited on");

    match error {
        BrokerOpError::AdmissionTimedOut {
            repository,
            stage,
            budget,
        } => {
            assert_eq!(repository, "owner/repo");
            assert_eq!(stage, "running the pre-push dry run");
            assert_eq!(budget, humanize_duration(1));
        }
        other => panic!("expected AdmissionTimedOut, got {other:?}"),
    }
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "returned after {:?}, so the child was waited on rather than killed",
        started.elapsed()
    );
}

#[test]
fn failed_pre_push_hook_marks_remote_write_as_not_contacted() {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
            tmp.path(),
            r#"{"event":"child_start","child_id":1,"child_class":"hook","hook_name":"pre-push","argv":[".git/hooks/pre-push","origin"]}
{"event":"child_exit","child_id":1,"code":1}
"#,
        )
        .unwrap();
    let trace = inspect_git_transfer_trace(tmp.path());
    assert_eq!(
        trace.remote_contact(),
        Some(RemoteContactEvidence {
            remote_contact: "not_contacted",
            remote_write_contact: "not_contacted",
            remote_not_contacted: true,
        })
    );

    std::fs::write(
        tmp.path(),
        r#"{"event":"child_start","child_id":1,"argv":[".git/hooks/pre-push","origin"]}
{"event":"child_exit","child_id":1,"code":1}
{"event":"child_start","child_id":2,"argv":["git-receive-pack","repo.git"]}
"#,
    )
    .unwrap();
    let trace = inspect_git_transfer_trace(tmp.path());
    assert_eq!(
        trace.remote_contact(),
        Some(RemoteContactEvidence {
            remote_contact: "contacted",
            remote_write_contact: "not_contacted",
            remote_not_contacted: false,
        })
    );
}

#[test]
fn remote_git_detection_ignores_global_checkout_options() {
    assert_eq!(
        git_operation_kind(&args(&["-C", "/tmp/checkout", "push", "origin", "main"])),
        GitOperationKind::Remote
    );
    assert_eq!(
        git_operation_kind(&args(&["-C", "/tmp/checkout", "rebase", "main"])),
        GitOperationKind::Local
    );
}

#[test]
fn git_global_options_are_skipped_before_the_subcommand() {
    for command in [
        args(&["-c", "core.fsmonitor=true", "push"]),
        args(&["-c", "core.fsmonitor=true", "-C", "/tmp/checkout", "push"]),
        args(&["--git-dir", "/tmp/checkout/.git", "push"]),
        args(&["--git-dir=/tmp/checkout/.git", "push"]),
        args(&["--work-tree", "/tmp/checkout", "push"]),
        args(&["--namespace", "namespace", "push"]),
        args(&["--super-prefix", "prefix", "push"]),
        args(&["--config-env", "http.proxy=HTTPS_PROXY", "push"]),
    ] {
        assert_eq!(classify_git(&command), Some(OperationEffect::Write));
        assert_eq!(git_operation_kind(&command), GitOperationKind::Remote);
    }
    assert_eq!(
        classify_git(&args(&["--git-dir"])),
        None,
        "a missing global-option value must not be mistaken for a subcommand"
    );
    assert_eq!(
        git_operation_kind(&args(&["--unknown-global-option", "push"])),
        GitOperationKind::Unknown
    );
}

#[test]
fn redaction_keeps_audit_shape_without_secret_values() {
    let value = redacted_command(
        OperationProvider::Github,
        &args(&["secret", "set", "TOKEN", "--body", "super-secret"]),
    )
    .unwrap();
    assert!(value.contains("[REDACTED]"));
    assert!(!value.contains("super-secret"));
}

#[test]
fn review_thread_journal_retains_scope_inputs_and_redacts_the_reply_body() {
    let args = review_thread_api(
        crate::gh_ref_guard::REVIEW_THREAD_REPLY_QUERY,
        "PRRT_kwDOabc",
        Some("private inline reply"),
    );
    let value = redacted_command(OperationProvider::Github, &args).unwrap();
    assert!(value.contains(crate::gh_ref_guard::REVIEW_THREAD_REPLY_QUERY));
    assert!(value.contains("threadId=PRRT_kwDOabc"));
    assert!(!value.contains("private inline reply"));

    let stored: Vec<String> = serde_json::from_str(&value).unwrap();
    let expected_scope = crate::gh_ref_guard::review_thread_resource_scope(&args).unwrap();
    assert_eq!(
        resource_lock_scope(OperationProvider::Github, &stored[1..]).as_deref(),
        Some(expected_scope.as_str()),
        "a journaled operation must recompute its exact thread scope after restart"
    );
}

fn assert_redacted(provider: OperationProvider, argv: &[&str], kept: &[&str], secret: &str) {
    let value = redacted_command(provider, &args(argv)).unwrap();
    assert!(!value.contains(secret), "{value} leaks {secret}");
    assert!(value.contains("[REDACTED]"), "{value}");
    for fragment in kept {
        assert!(value.contains(fragment), "{value} lost {fragment}");
    }
}

#[test]
fn redaction_hides_git_extraheader_config_values() {
    // Separated `-c key=value`, mixed-case key.
    assert_redacted(
        OperationProvider::Git,
        &[
            "-c",
            "http.extraHeader=AUTHORIZATION: bearer s3cr3t",
            "push",
            "origin",
            "main",
        ],
        &["\"-c\"", "http.extraHeader=[REDACTED]", "push", "origin"],
        "s3cr3t",
    );
    // URL-scoped key.
    assert_redacted(
        OperationProvider::Git,
        &[
            "-c",
            "http.https://github.com/.extraheader=Basic s3cr3t",
            "fetch",
        ],
        &["http.https://github.com/.extraheader=[REDACTED]", "fetch"],
        "s3cr3t",
    );
    // Joined `-ckey=value`.
    assert_redacted(
        OperationProvider::Git,
        &["-chttp.extraheader=Authorization: token s3cr3t", "push"],
        &["-chttp.extraheader=[REDACTED]"],
        "s3cr3t",
    );
}

#[test]
fn redaction_hides_credential_named_config_values() {
    assert_redacted(
        OperationProvider::Git,
        &["-c", "credential.helper.token=s3cr3t", "push"],
        &["credential.helper.token=[REDACTED]"],
        "s3cr3t",
    );
    assert_redacted(
        OperationProvider::Git,
        &["-c", "remote.origin.password=s3cr3t", "push"],
        &["remote.origin.password=[REDACTED]"],
        "s3cr3t",
    );
    // `--config-env`, separated and joined.
    assert_redacted(
        OperationProvider::Git,
        &["--config-env", "http.extraheader=S3CR3T_ENV", "push"],
        &["\"--config-env\"", "http.extraheader=[REDACTED]"],
        "S3CR3T_ENV",
    );
    assert_redacted(
        OperationProvider::Git,
        &["--config-env=http.authorization=S3CR3T_ENV", "push"],
        &["--config-env=http.authorization=[REDACTED]"],
        "S3CR3T_ENV",
    );
    // Harmless configuration stays readable.
    let value = redacted_command(
        OperationProvider::Git,
        &args(&["-c", "core.quotepath=false", "push"]),
    )
    .unwrap();
    assert!(value.contains("core.quotepath=false"), "{value}");
}

#[test]
fn redaction_hides_github_credential_header_values() {
    // Separated `-H`.
    assert_redacted(
        OperationProvider::Github,
        &["api", "-H", "Authorization: token s3cr3t", "repos/o/r"],
        &["\"-H\"", "Authorization: [REDACTED]", "repos/o/r"],
        "s3cr3t",
    );
    // Separated `--header`, Proxy-Authorization.
    assert_redacted(
        OperationProvider::Github,
        &[
            "api",
            "--header",
            "Proxy-Authorization: Basic s3cr3t",
            "user",
        ],
        &["\"--header\"", "Proxy-Authorization: [REDACTED]"],
        "s3cr3t",
    );
    // Joined `--header=` with a token-named header.
    assert_redacted(
        OperationProvider::Github,
        &["api", "--header=X-Api-Token: s3cr3t", "user"],
        &["--header=X-Api-Token: [REDACTED]"],
        "s3cr3t",
    );
    // Joined `-H`.
    assert_redacted(
        OperationProvider::Github,
        &["api", "-Hauthorization: bearer s3cr3t", "user"],
        &["-Hauthorization: [REDACTED]"],
        "s3cr3t",
    );
    // Non-credential headers stay readable.
    let value = redacted_command(
        OperationProvider::Github,
        &args(&["api", "-H", "Accept: application/vnd.github+json", "user"]),
    )
    .unwrap();
    assert!(
        value.contains("Accept: application/vnd.github+json"),
        "{value}"
    );
}

#[test]
fn github_target_cannot_be_overridden_after_the_broker_boundary() {
    let err = crate::resolve_github_target(
        "owner/repo",
        &args(&["pr", "merge", "12", "--repo", "other/repo"]),
    )
    .unwrap_err();
    assert!(err.to_string().contains("second repository target"));
}

#[test]
fn destructive_branch_targets_name_session_branches() {
    let git = |args: &[&str]| {
        destructive_branch_targets(
            OperationProvider::Git,
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
        )
    };
    assert_eq!(git(&["push", "origin", ":agent/a"]), ["agent/a"]);
    assert_eq!(
        git(&["push", "-o", "ci.skip", "origin", "--delete", "agent/a"]),
        ["agent/a"]
    );
    assert_eq!(
        git(&["push", "--force", "origin", "+HEAD:refs/heads/agent/a"]),
        ["agent/a"]
    );
    assert_eq!(
        git(&["-C", "x", "branch", "-D", "agent/a", "agent/b"]),
        ["agent/a", "agent/b"]
    );
    assert_eq!(git(&["branch", "-dr", "origin/agent/a"]), ["agent/a"]);
    assert_eq!(
        git(&["update-ref", "-d", "refs/remotes/origin/agent/a"]),
        ["agent/a"]
    );
    assert!(git(&["reset", "--hard", "HEAD~1"]).is_empty());
    // gh commands are resolved by `gh_ref_guard`, not by argument matching.
    assert!(
        destructive_branch_targets(
            OperationProvider::Github,
            &["api", "-X", "DELETE", "repos/o/n/git/refs/heads/agent/a"]
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
        )
        .is_empty()
    );
}

#[test]
fn only_deleted_or_forced_push_destinations_are_rewrites() {
    let rewritten = |args: &[&str]| {
        git_rewritten_branches(
            &args
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
        )
    };
    // A plain fast-forward to main is an advance, the deletion a rewrite.
    assert_eq!(
        rewritten(&["push", "origin", "HEAD:refs/heads/main", ":refs/heads/old"]),
        ["old"]
    );
    assert_eq!(
        rewritten(&["push", "origin", "+HEAD:refs/heads/main"]),
        ["main"]
    );
    for force in [
        "--force",
        "-f",
        "--force-with-lease",
        "--force-with-lease=main",
    ] {
        assert_eq!(
            rewritten(&["push", force, "origin", "HEAD:refs/heads/main"]),
            ["main"],
            "{force}"
        );
    }
    assert_eq!(rewritten(&["push", "origin", "--delete", "main"]), ["main"]);
    assert!(rewritten(&["update-ref", "refs/heads/main", "abc"]).contains(&"main".to_string()));
    assert_eq!(rewritten(&["branch", "-D", "main"]), ["main"]);
}

#[test]
fn push_targets_parse_remote_aliases_urls_and_namespace_destinations() {
    let targets = |argv: &[&str]| {
        git_push_branch_targets(&args(argv))
            .unwrap()
            .unwrap_or_default()
    };
    for argv in [
        &["push", "origin", "+HEAD:refs/heads/main"][..],
        &["push", "ssh://git@example.test/repo.git", "+HEAD:main"][..],
        &[
            "push",
            "--repo",
            "ssh://git@example.test/repo.git",
            "origin",
            "+HEAD:main",
        ][..],
        &[
            "push",
            "--repo=ssh://git@example.test/repo.git",
            "origin",
            "+HEAD:main",
        ][..],
    ] {
        assert_eq!(targets(argv).rewrite, ["main"], "{argv:?}");
    }
    assert!(
        destructive_branch_targets(
            OperationProvider::Git,
            &args(&["push", "origin", "+HEAD:refs/remotes/origin/main"]),
        )
        .is_empty(),
        "remote-tracking refs are not remote branch heads"
    );
    assert!(
        destructive_branch_targets(
            OperationProvider::Git,
            &args(&["push", "origin", "+HEAD:refs/tags/main"]),
        )
        .is_empty(),
        "tags are not branch heads"
    );
}

#[test]
fn push_targets_reject_implicit_and_set_expanding_ref_selection() {
    for argv in [
        &["push", "origin"][..],
        &["push"][..],
        &["push", "--all", "origin"][..],
        &["push", "--branches", "origin"][..],
        &["push", "--mirror", "origin"][..],
        &["push", "--prune", "origin", "main"][..],
        &["push", "origin", "refs/heads/*:refs/heads/*"][..],
        &["push", "origin", "+HEAD:"][..],
        &["push", "origin", "HEAD"][..],
        &["push", "--unknown-option", "origin", "main"][..],
    ] {
        assert!(
            git_push_branch_targets(&args(argv)).is_err(),
            "{argv:?} must fail closed"
        );
    }
}

fn push_shape(args: &[&str]) -> Option<PushShape> {
    classify_push(
        &args
            .iter()
            .map(|arg| (*arg).to_string())
            .collect::<Vec<_>>(),
    )
}

fn push_kind(args: &[&str]) -> Option<PushKind> {
    push_shape(args).map(|shape| shape.kind)
}

#[test]
fn push_kind_recognises_deletions_by_flag_and_by_empty_source() {
    assert_eq!(
        push_kind(&["push", "origin", "--delete", "feature"]),
        Some(PushKind::DeleteOnly)
    );
    assert_eq!(
        push_kind(&["push", "-d", "origin", "a", "b"]),
        Some(PushKind::DeleteOnly)
    );
    assert_eq!(
        push_kind(&["push", "origin", ":refs/heads/feature", "+:old"]),
        Some(PushKind::DeleteOnly)
    );
    assert_eq!(
        push_kind(&["-C", "/tmp/x", "push", "origin", ":feature"]),
        Some(PushKind::DeleteOnly)
    );
    assert_eq!(
        push_shape(&["push", "origin", "--delete", "a", "b"])
            .unwrap()
            .refs,
        vec!["a".to_string(), "b".to_string()]
    );
}

#[test]
fn push_kind_classifies_updates_and_mixed_pushes() {
    assert_eq!(
        push_kind(&["push", "origin", "HEAD:refs/heads/main"]),
        Some(PushKind::Update)
    );
    assert_eq!(
        push_kind(&["push", "--force-with-lease", "-u", "origin", "+src:dst"]),
        Some(PushKind::Update)
    );
    assert_eq!(
        push_kind(&["push", "origin", "main", ":old"]),
        Some(PushKind::Mixed)
    );
}

#[test]
fn push_kind_falls_back_to_mixed_when_the_shape_is_uncertain() {
    for args in [
        &["push", "origin"][..],
        &["push"],
        &["push", "--all", "origin"],
        &["push", "--mirror", "origin"],
        &["push", "--prune", "origin", ":x"],
        &["push", "--unknown-option", "origin", ":x"],
        &["push", "-dq", "origin", "x"],
        &["push", "origin", ":"],
        &["push", "--delete", "origin", "a:b"],
        &["push", "-o"],
    ] {
        assert_eq!(push_kind(args), Some(PushKind::Mixed), "{args:?}");
    }
    assert_eq!(push_kind(&["fetch", "origin"]), None);
}

#[test]
fn fetch_completion_journal_survives_minimal_unknown_transition() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("broker.db");
    drop(crate::BrokerStore::open(&database).unwrap());
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("INSERT INTO sessions (id, worktree_path, branch, origin, status, created_at, updated_at, last_activity_at) VALUES (37, '/repo/one', 'agent/one', 'adopted', 'active', 1, 1, 1);").unwrap();
    drop(connection);
    let mut store = crate::BrokerStore::open(&database).unwrap();
    let operation = store
        .create_coordinated_operation(&crate::NewCoordinatedOperation {
            session_id: 37,
            provider: OperationProvider::Git,
            repository: "owner/repo".into(),
            scope: "fetch".into(),
            effect: OperationEffect::Write,
            authorization_reason: Some("test".into()),
            command_json: "[\"git\",\"fetch\"]".into(),
            pid: std::process::id() as i64,
            host_operation_id: None,
            identity_provenance: crate::OperationIdentityProvenance::VerifiedCanonical,
        })
        .unwrap();
    let proof = json!({"schema_version": 1, "operation_id": operation.id,
        "completion": {"pid": 123, "success": true, "exit_code": 0}});
    store
        .transition_coordinated_operation(
            operation.id,
            OperationStatus::Running,
            None,
            Some(&json!({"fetch_reconciliation": proof, "stderr": "old output"}).to_string()),
        )
        .unwrap();
    let unknown = store
        .transition_coordinated_operation(
            operation.id,
            OperationStatus::OutcomeUnknown,
            None,
            Some(r#"{"reason":"process_ended_without_outcome"}"#),
        )
        .unwrap();
    let details: serde_json::Value =
        serde_json::from_str(unknown.details_json.as_deref().unwrap()).unwrap();
    assert_eq!(details["fetch_reconciliation"], proof);
    assert_eq!(details["reason"], "process_ended_without_outcome");
    assert!(details.get("stderr").is_none());
}

#[test]
fn exact_fetch_profile_validates_git_config_and_observes_exact_refs() {
    let (root, target) = push_fixture();
    let args = git_scopes::canonical_fetch_args("origin", "main").unwrap();
    let deadline = AdmissionDeadline::start(QueueWait::Seconds(10));
    git_scopes::validate_repository(root.path(), &args, deadline, &target.coordination_key)
        .unwrap();
    let oid = crate::GitRepo::discover(root.path())
        .unwrap()
        .resolve_ref("HEAD")
        .unwrap();
    let output = Command::new("git")
        .current_dir(root.path())
        .args(["update-ref", "refs/remotes/origin/main", &oid])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        git_scopes::observe_ref(
            root.path(),
            "refs/remotes/origin/main",
            deadline,
            &target.coordination_key
        )
        .unwrap(),
        Some(oid)
    );
    assert_eq!(
        git_scopes::observe_ref(
            root.path(),
            "refs/remotes/origin/absent",
            deadline,
            &target.coordination_key
        )
        .unwrap(),
        None
    );
    let output = Command::new("git")
        .current_dir(root.path())
        .args([
            "config",
            "--add",
            "remote.origin.fetch",
            "+refs/heads/*:refs/heads/*",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(
        git_scopes::validate_repository(root.path(), &args, deadline, &target.coordination_key)
            .is_err(),
        "a second expanding refmap must force the wide lane"
    );
}
