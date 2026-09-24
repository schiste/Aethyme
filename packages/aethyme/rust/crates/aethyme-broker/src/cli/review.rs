//! `broker review`: routing, running and recording reviews.

use super::*;

/// `broker review plan` -- evaluate the review policies against a change and
/// print what they would do.
///
/// A dry run with no exceptions: it reads git, reads `.aethyme/config.toml`,
/// and writes nothing. Every mutation it would make is printed as the exact
/// `aethyme broker gh` command that would make it, so an operator can read the
/// decision and run it themselves before ever switching the policy on.
///
/// It deliberately does not consult the provider. Review *spend* and live
/// Chau7 tabs are the two inputs it cannot get without a network, and the plan
/// says so rather than guessing: what it shows is the decision for a pull
/// request with no reviews spent yet, which is the case an operator is trying
/// to reason about when they are writing the rules.
pub(super) fn run_review_plan(parsed: Parsed) -> Result<(), UsageError> {
    let broker = open_broker(true)?;
    // Policy is repository-level and lives in the main checkout; the change
    // being planned is in whatever worktree the caller is standing in. Reading
    // both from the main root would plan the main branch against itself, which
    // is an empty diff and an answer that looks perfectly correct.
    let root = broker.main_root().to_path_buf();
    let change_root = std::env::current_dir().map_err(|error| {
        UsageError::Message(format!("cannot read the working directory: {error}"))
    })?;
    let base = parsed
        .base
        .clone()
        .unwrap_or_else(|| "aethyme/integration".to_string());
    let pull_request = parsed.pr_number.unwrap_or(0);
    // `review plan` is an offline preview and `--repo` is optional on it, but
    // the reviewer prompt it shows is the real one -- and the real one names
    // the repository in every posting command. A placeholder is the honest
    // answer: filling in a guess from the remote would print a plan that
    // differs from the prompt `review run` will actually send.
    let repository = parsed
        .repository
        .clone()
        .unwrap_or_else(|| "<owner/name>".to_string());

    let paths = git_lines(
        &change_root,
        &["diff", "--name-only", &format!("{base}...HEAD")],
    )?;
    let messages = git_output(
        &change_root,
        &["log", "--format=%B%x00", &format!("{base}..HEAD")],
    )?;
    let classification = crate::CommitClassification::merge(
        messages
            .split('\0')
            .filter(|m| !m.trim().is_empty())
            .map(crate::parse_classification),
    );

    let trigger = crate::ReviewTriggerPolicy::load(&root).map_err(to_usage)?;
    let routing = crate::ReviewRoutingPolicy::load(&root).map_err(to_usage)?;
    let reporting = crate::ReviewReportingPolicy::load(&root).map_err(to_usage)?;
    let projection_policy = crate::PrProjectionPolicy::load(&root).map_err(to_usage)?;

    // `review plan` is the offline preview: git and config, no provider call,
    // runnable while other sessions work. It therefore cannot know which
    // lifecycle transition this is, and says so in `assumptions` rather than
    // presenting a guess as a reading. `review run` derives all of this for
    // real.
    let facts = crate::ChangeFacts {
        trigger: Some(crate::ReviewTrigger::PullRequestOpened),
        paths: paths.clone(),
        authored_by_model: classification.model.clone(),
        classification: classification.clone(),
        from_fork: false,
        first_time_contributor: false,
    };
    let eligible = crate::eligible_types(&trigger, &facts);
    let head = git_output(&change_root, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();
    let decisions = crate::schedule(
        &trigger,
        &eligible,
        &std::collections::BTreeMap::new(),
        &head,
        // Freshness invalidates a review that was already requested, and the
        // empty spend above says none was. Resolving a base here would change
        // no decision; `review run` derives it for real.
        None,
        now_ms(),
    );

    let dispatch: Vec<crate::ReviewDispatchAction> = decisions
        .iter()
        .filter_map(|decision| match decision {
            crate::ReviewTriggerDecision::Request { review_type, .. } => {
                Some(crate::dispatch_review(
                    &routing,
                    &reporting,
                    &root,
                    &repository,
                    review_type,
                    pull_request,
                    &head,
                    &[],
                    &[],
                    // A projection has no ledger to read, so it plans the
                    // first-attempt route. Consistent with the `&[]` tabs and
                    // slots above, and with the comment at the top of this
                    // function: `review run` derives all of this for real.
                    None,
                ))
            }
            _ => None,
        })
        .collect();

    let reviews: Vec<crate::ProjectedReview> = decisions
        .iter()
        .map(|decision| crate::ProjectedReview {
            review_type: decision.review_type().to_string(),
            state: match decision {
                crate::ReviewTriggerDecision::Request { .. } => {
                    crate::ProjectedReviewState::Requested
                }
                crate::ReviewTriggerDecision::Defer { .. } => crate::ProjectedReviewState::Deferred,
                crate::ReviewTriggerDecision::Skip { .. } => crate::ProjectedReviewState::Skipped,
            },
            detail: None,
        })
        .collect();
    let projection = crate::ReviewProjection {
        head: Some(head.clone()),
        reviews,
        classification: classification.clone(),
        conflicts: Vec::new(),
        quality_report: None,
    };
    let projection_actions = crate::project(
        &projection_policy,
        &projection,
        &crate::PrProjectionFacts {
            pull_request,
            ..Default::default()
        },
    );

    let report = serde_json::json!({
        "policy_root": root.display().to_string(),
        "change_root": change_root.display().to_string(),
        "base": base,
        "head": head,
        "pull_request": pull_request,
        "changed_paths": paths.len(),
        "classification": classification,
        "trigger_enabled": trigger.enabled,
        "routing_enabled": routing.enabled,
        "projection_enabled": projection_policy.enabled,
        "eligible": eligible,
        "decisions": decisions,
        "dispatch": dispatch,
        "projection": projection_actions,
        "assumptions": [
            "no reviews have been spent on this pull request yet",
            "no Chau7 tabs and no reviews are in flight",
            "the pull request carries no labels and no Aethyme comment",
            "the change is a newly opened pull request, by a known contributor, \
             not from a fork",
        ],
        "performed": false,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|e| UsageError::Message(e.to_string()))?
    );
    Ok(())
}

/// Read what the provider says about a pull request right now, read-only.
///
/// `None` means the question could not be answered -- no `gh`, no auth, no such
/// pull request. Callers treat that as "no observation", which makes the tick
/// derive `PullRequestOpened` and re-ask rather than invent a transition. An
/// unnecessary review is the documented cost of an unverified signal; a
/// silently skipped one is not.
pub(super) fn read_pull_request_snapshot(
    root: &Path,
    repository: &str,
    pull_request: i64,
) -> Option<crate::ProviderPullRequest> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "view",
            &pull_request.to_string(),
            "--repo",
            repository,
            "--json",
            "headRefOid,baseRefOid,baseRefName,isDraft,state,isCrossRepository,reviews",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    // Dismissal is an event, and a snapshot can only show it as a count that
    // grew since the last look.
    let dismissed_reviews = json["reviews"]
        .as_array()
        .map(|reviews| {
            reviews
                .iter()
                .filter(|review| {
                    review["state"]
                        .as_str()
                        .is_some_and(|state| state.eq_ignore_ascii_case("dismissed"))
                })
                .count() as i64
        })
        .unwrap_or(0);
    Some(crate::ProviderPullRequest {
        head_commit: json["headRefOid"].as_str().unwrap_or_default().to_string(),
        base_ref: json["baseRefName"].as_str().unwrap_or_default().to_string(),
        // Absent rather than empty when the provider did not report one:
        // `schedule` distinguishes "unproven" from "unchanged", and an empty
        // string would compare unequal to itself across ticks anyway.
        base_commit: json["baseRefOid"]
            .as_str()
            .filter(|oid| !oid.is_empty())
            .map(String::from),
        is_draft: json["isDraft"].as_bool().unwrap_or(false),
        state: json["state"]
            .as_str()
            .unwrap_or("open")
            .to_ascii_lowercase(),
        from_fork: json["isCrossRepository"].as_bool().unwrap_or(false),
        dismissed_reviews,
        author_association: read_author_association(root, repository, pull_request),
    })
}

/// The author's relationship to the repository, from the provider.
///
/// A separate call because `gh pr view --json` does not expose
/// `authorAssociation`; the REST representation does. Read-only, and a failure
/// answers `None`, which [`crate::first_time_contributor`] treats as "not new"
/// rather than as "new".
pub(super) fn read_author_association(
    root: &Path,
    repository: &str,
    pull_request: i64,
) -> Option<String> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "api",
            &format!("repos/{repository}/pulls/{pull_request}"),
            "--jq",
            ".author_association",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() || value == "null" {
        return None;
    }
    Some(value.to_ascii_lowercase())
}

/// The change itself -- paths, declarations, head -- from the provider.
///
/// `review run` normally describes the working directory it was invoked in,
/// which is right for an agent reviewing its own branch and impossible for a
/// sweep: a tick visits every open pull request and is standing in none of
/// them. Asking the provider needs no fetch, no checkout, and no local ref, so
/// one tick can route a repository it has never cloned.
pub(super) fn read_change_from_provider(
    root: &Path,
    repository: &str,
    pull_request: i64,
) -> Option<(Vec<String>, crate::CommitClassification, String)> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "view",
            &pull_request.to_string(),
            "--repo",
            repository,
            "--json",
            "files,commits,headRefOid",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    let paths = json["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file["path"].as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    // Trailers live in the body, and `merge` is oldest-first, so the order the
    // provider returns commits in is the order declarations must be read in.
    let classification = crate::CommitClassification::merge(
        json["commits"]
            .as_array()
            .map(|commits| {
                commits
                    .iter()
                    .map(|commit| {
                        let headline = commit["messageHeadline"].as_str().unwrap_or_default();
                        let body = commit["messageBody"].as_str().unwrap_or_default();
                        crate::parse_classification(&format!("{headline}\n{body}"))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    );
    let head = json["headRefOid"].as_str()?.to_string();
    Some((paths, classification, head))
}

/// Whether `head` has `previous` in its history.
///
/// This is what separates a commit added on top from a head that replaced the
/// old one, and only a repository can answer it. An unknown answer is `false`,
/// which reports `ReplacementCommit`: treating a rewrite as an append would
/// tell a rule that history it already reviewed is still intact when it may not
/// be, and that is the direction that loses a review.
pub(super) fn head_descends_from(root: &Path, previous: &str, head: &str) -> bool {
    if previous.is_empty() || head.is_empty() {
        return false;
    }
    crate::git::git_command()
        .current_dir(root)
        .args(["merge-base", "--is-ancestor", previous, head])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// Everything the decision plane reads about a change, gathered for real.
///
/// The trigger and the provider-supplied facts used to be hardcoded here, which
/// meant every rule keyed on `on`, `from_fork` or `first_time_contributor`
/// parsed, loaded, and never matched. Returns the snapshot alongside the facts
/// so the caller can record the observation once the tick has acted on it.
pub(super) fn gather_change_facts(
    store_root: &Path,
    repository: &str,
    pull_request: i64,
    paths: Vec<String>,
    classification: crate::CommitClassification,
    previous: Option<&crate::PullRequestObservation>,
) -> (crate::ChangeFacts, Option<crate::ProviderPullRequest>) {
    let snapshot = read_pull_request_snapshot(store_root, repository, pull_request);
    let Some(snapshot) = snapshot else {
        return (
            crate::ChangeFacts {
                trigger: Some(crate::ReviewTrigger::PullRequestOpened),
                authored_by_model: classification.model.clone(),
                paths,
                classification,
                from_fork: false,
                first_time_contributor: false,
            },
            None,
        );
    };
    let descends = previous.is_some_and(|previous| {
        head_descends_from(store_root, &previous.head_commit, &snapshot.head_commit)
    });
    let facts = crate::ChangeFacts {
        trigger: Some(crate::derive_trigger(previous, &snapshot, descends)),
        paths,
        authored_by_model: classification.model.clone(),
        classification,
        from_fork: snapshot.from_fork,
        first_time_contributor: crate::first_time_contributor(
            snapshot.author_association.as_deref(),
        ),
    };
    (facts, Some(snapshot))
}

/// Aethyme's own comment on a pull request, from the `comments` array of
/// `gh pr view --json comments`.
///
/// The id is read from each comment's `url` and never from its `id`: `gh`
/// reports the latter as a GraphQL node id, while the endpoint that edits a
/// comment takes the REST integer. See [`crate::rest_comment_id`].
///
/// A comment whose url yields no id is skipped when it is someone else's --
/// it was never a candidate, so nothing is lost -- and is an error when it
/// carries [`crate::COMMENT_MARKER`]. That asymmetry is the whole point of
/// this function. Elsewhere in `read_pull_request_facts`, missing data means
/// the pull request really might be untouched, and planning to create is the
/// right guess. Here the pull request demonstrably is not untouched: Aethyme's
/// own comment is sitting in the payload. Reporting it absent is not a guess
/// but a known-false statement, and the write it licenses posts a second
/// comment beside the first. Refusing costs one sweep; guessing costs a comment
/// per sweep, forever, and the duplicates cannot be told apart afterwards.
pub(super) fn owned_comment_from_view(
    repository: &str,
    pull_request: i64,
    comments: &[serde_json::Value],
) -> Result<Option<crate::OwnedComment>, UsageError> {
    let mut readable: Vec<(i64, String)> = Vec::new();
    for comment in comments {
        let Some(body) = comment["body"].as_str() else {
            continue;
        };
        match comment["url"].as_str().and_then(crate::rest_comment_id) {
            Some(id) => readable.push((id, body.to_string())),
            None if body.contains(crate::COMMENT_MARKER) => {
                return Err(UsageError::Message(format!(
                    "{repository}#{pull_request} already carries an Aethyme review comment, but \
                     its REST id could not be read from its url ({}); refusing to project rather \
                     than post a second comment beside it. `gh pr view --json comments` must \
                     report a url ending in `#issuecomment-<id>`.",
                    comment["url"].as_str().unwrap_or("<no url>")
                )));
            }
            None => {}
        }
    }
    Ok(crate::find_owned_comment(
        readable.iter().map(|(id, body)| (*id, body.as_str())),
    ))
}

/// Read the pull request facts the projection needs, with read-only `gh`.
///
/// Read-only GitHub inspection runs directly; only writes go through the
/// coordinated lane. A repository whose `gh` is unauthenticated, or a pull
/// request that does not exist, is not a hard failure here: the projection then
/// sees an empty pull request and plans to create its comment and labels, which
/// the coordinated write refuses loudly if it was wrong. Guessing quietly is
/// what this avoids.
///
/// One observation *is* a hard failure, and `owned_comment_from_view` says why.
pub(super) fn read_pull_request_facts(
    root: &Path,
    repository: &str,
    pull_request: i64,
) -> Result<crate::PrProjectionFacts, UsageError> {
    let mut facts = crate::PrProjectionFacts {
        pull_request,
        ..Default::default()
    };
    let view = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "view",
            &pull_request.to_string(),
            "--repo",
            repository,
            "--json",
            "labels,comments",
        ])
        .output();
    if let Ok(output) = view
        && output.status.success()
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
    {
        facts.current_labels = json["labels"]
            .as_array()
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(|label| label["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        facts.owned_comment = owned_comment_from_view(
            repository,
            pull_request,
            json["comments"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        )?;
    }
    let labels = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "label", "list", "--repo", repository, "--limit", "200", "--json", "name",
        ])
        .output();
    if let Ok(output) = labels
        && output.status.success()
        && let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        && let Some(array) = json.as_array()
    {
        facts.repository_labels = array
            .iter()
            .filter_map(|label| label["name"].as_str().map(String::from))
            .collect();
    }
    Ok(facts)
}

/// Perform one tick of the review router for one pull request.
///
/// This is `review plan` with the assumptions replaced by facts -- the ledger
/// for what has been spent, a Chau7 snapshot for what is in flight, and a
/// read-only `gh` for what the pull request already says -- followed by the
/// effects. `--dry-run` stops after the plan, which is what makes the first
/// run against a real repository safe to look at.
pub(super) fn run_review_run(parsed: Parsed) -> Result<serde_json::Value, UsageError> {
    // A dry run performs nothing, so it needs neither a session nor a write
    // lock -- which is what makes it runnable while other sessions are working.
    let session_id = match parsed.session {
        Some(id) => Some(id),
        None if parsed.dry_run => None,
        None => {
            return Err(UsageError::Message(
                "review run requires --session <id>".into(),
            ));
        }
    };
    let repository = parsed
        .repository
        .clone()
        .ok_or_else(|| UsageError::Message("review run requires --repo <owner/name>".into()))?;
    let pull_request = parsed
        .pr_number
        .ok_or_else(|| UsageError::Message("review run requires --pr <number>".into()))?;

    let mut broker = open_broker(parsed.dry_run)?;
    let root = broker.main_root().to_path_buf();
    let change_root = std::env::current_dir().map_err(|error| {
        UsageError::Message(format!("cannot read the working directory: {error}"))
    })?;
    let base = parsed
        .base
        .clone()
        .unwrap_or_else(|| "aethyme/integration".to_string());

    let trigger = crate::ReviewTriggerPolicy::load(&root).map_err(to_usage)?;
    let routing = crate::ReviewRoutingPolicy::load(&root).map_err(to_usage)?;
    let reporting = crate::ReviewReportingPolicy::load(&root).map_err(to_usage)?;
    let projection_policy = crate::PrProjectionPolicy::load(&root).map_err(to_usage)?;

    // Where the change is read from. The working directory is right for an
    // agent routing its own branch and impossible for a sweep, which visits
    // every open pull request and is standing in none of them; the git reads
    // below would fail on the first one. So the source is chosen before either
    // is attempted, never after.
    let (paths, classification, head) = if parsed.from_provider {
        read_change_from_provider(&root, &repository, pull_request).ok_or_else(|| {
            UsageError::Message(format!(
                "cannot read pull request {pull_request} from {repository}; \
                 --from-provider needs an authenticated gh"
            ))
        })?
    } else {
        let paths = git_lines(
            &change_root,
            &["diff", "--name-only", &format!("{base}...HEAD")],
        )?;
        let messages = git_output(
            &change_root,
            &["log", "--format=%B%x00", &format!("{base}..HEAD")],
        )?;
        let classification = crate::CommitClassification::merge(
            messages
                .split('\0')
                .filter(|m| !m.trim().is_empty())
                .map(crate::parse_classification),
        );
        let head = git_output(&change_root, &["rev-parse", "HEAD"])?
            .trim()
            .to_string();
        (paths, classification, head)
    };

    // Before reading anything, stop waiting on reviews nobody is coming back
    // for. This has to happen first: an expired row becomes `abandoned`, which
    // is neither spend nor in flight, so a tick that read either before
    // expiring would plan against a repository whose slots are still held by
    // reviewers that died days ago.
    let open = broker
        .store()
        .review_requests_in_flight(&repository)
        .map_err(to_usage)?;
    let expired = crate::expired(&routing, &open, now_ms());
    if !parsed.dry_run {
        for review in &expired {
            broker
                .store()
                .set_review_request_state(
                    review.id,
                    crate::ReviewRequestState::Abandoned,
                    Some(&review.why),
                    now_ms(),
                )
                .map_err(to_usage)?;
        }
    }
    let expired_ids: std::collections::BTreeSet<i64> =
        expired.iter().map(|review| review.id).collect();

    // The three facts `review plan` has to assume. Spend is this pull request's
    // history; concurrency is the whole repository's, because `max_concurrent`
    // is a per-repository budget. Reading the slot count from this pull
    // request's rows would quietly multiply the cap by the number of open pull
    // requests, which is the opposite of what a cap is for.
    let recorded = broker
        .store()
        .review_requests_for_pr(&repository, pull_request)
        .map_err(to_usage)?;
    let spend = crate::spend_by_type(
        &recorded
            .iter()
            .filter(|row| !expired_ids.contains(&row.id))
            .cloned()
            .collect::<Vec<_>>(),
    );
    // A dry run performs nothing, so the rows it just decided to expire are
    // still open in the database. Dropping them here is what makes the plan it
    // prints the plan a real run would follow.
    let still_open: Vec<crate::ReviewRequest> = open
        .into_iter()
        .filter(|row| !expired_ids.contains(&row.id))
        .collect();
    let in_flight = crate::in_flight(&still_open);
    let tabs = read_tab_snapshot(&parsed)?;

    // Reviewer workspaces nobody is using any more. Every row this pull
    // request ever had, with this tick's expiries applied so a dry run plans
    // what a real run would do, and the tab list as the other half: a
    // dimension whose rows have all settled but whose tab is still standing in
    // the workspace is a reviewer that finished and was never released.
    //
    // Derived here rather than inside `dispatch_review` because it is not a
    // per-requested-review decision. A dimension can need reclaiming on a tick
    // that requests nothing at all -- which is the common case, since the
    // reviewer usually posts long after the push that asked for it.
    let reconciled: Vec<crate::ReviewRequest> = recorded
        .iter()
        .cloned()
        .map(|mut row| {
            if expired_ids.contains(&row.id) {
                row.state = crate::ReviewRequestState::Abandoned;
            }
            row
        })
        .collect();
    let teardown = crate::finished_workspaces(&routing, &root, pull_request, &reconciled, &tabs);
    let pr_facts = read_pull_request_facts(&change_root, &repository, pull_request)?;

    let previous = broker
        .store()
        .pull_request_observation(&repository, pull_request)
        .map_err(to_usage)?;
    let (facts, snapshot) = gather_change_facts(
        &root,
        &repository,
        pull_request,
        paths.clone(),
        classification.clone(),
        previous.as_ref(),
    );
    let eligible = crate::eligible_types(&trigger, &facts);
    // The provider's answer first: on a sweep there is no local checkout of
    // this pull request's base to resolve. Falling back to the local ref keeps
    // an agent routing its own branch working without `gh`, and `None` --
    // neither available -- makes a `head_and_base` dimension re-review rather
    // than trust a base it cannot name (#172).
    let base_commit = snapshot
        .as_ref()
        .and_then(|snapshot| snapshot.base_commit.clone())
        .or_else(|| {
            git_output(&change_root, &["rev-parse", &base])
                .ok()
                .map(|oid| oid.trim().to_string())
                .filter(|oid| !oid.is_empty())
        });
    let decisions = crate::schedule(
        &trigger,
        &eligible,
        &spend,
        &head,
        base_commit.as_deref(),
        now_ms(),
    );
    let dispatch: Vec<crate::ReviewDispatchAction> = decisions
        .iter()
        .filter_map(|decision| match decision {
            crate::ReviewTriggerDecision::Request { review_type, .. } => {
                Some(crate::dispatch_review(
                    &routing,
                    &reporting,
                    &root,
                    &repository,
                    review_type,
                    pull_request,
                    &head,
                    &tabs,
                    &in_flight,
                    // From `reconciled` rather than `recorded`: this tick's
                    // expiries are already applied there, so a review the
                    // router just gave up on is read as the abandoned row it
                    // has become.
                    crate::last_refusal(&reconciled, review_type),
                ))
            }
            _ => None,
        })
        .collect();

    let reviews: Vec<crate::ProjectedReview> = decisions
        .iter()
        .map(|decision| {
            // A waived dimension settles, so `schedule` reports it exactly as
            // it reports one already reviewed: `Skip`. Rendering that as
            // "skipped" would put a waiver and a completed review under the
            // same word on the pull request, which is the confusion #172 is
            // about. The ledger is consulted only for the `Skip` case and only
            // at this head, so nothing else can be relabelled by a waiver.
            let waiver = match decision {
                crate::ReviewTriggerDecision::Skip { .. } => {
                    crate::waiver_for(&reconciled, decision.review_type(), &head)
                }
                _ => None,
            };
            crate::ProjectedReview {
                review_type: decision.review_type().to_string(),
                state: match decision {
                    crate::ReviewTriggerDecision::Request { .. } => {
                        crate::ProjectedReviewState::Requested
                    }
                    crate::ReviewTriggerDecision::Defer { .. } => {
                        crate::ProjectedReviewState::Deferred
                    }
                    crate::ReviewTriggerDecision::Skip { .. } if waiver.is_some() => {
                        crate::ProjectedReviewState::Waived
                    }
                    crate::ReviewTriggerDecision::Skip { .. } => {
                        crate::ProjectedReviewState::Skipped
                    }
                },
                detail: waiver.map(|waiver| format!("{}: {}", waiver.who, waiver.reason)),
            }
        })
        .collect();
    let projection_actions = crate::project(
        &projection_policy,
        &crate::ReviewProjection {
            head: Some(head.clone()),
            reviews,
            classification: classification.clone(),
            conflicts: Vec::new(),
            quality_report: None,
        },
        &pr_facts,
    );

    let plan = crate::plan_execution(&dispatch, &projection_actions, &teardown, pull_request);

    if parsed.dry_run {
        return Ok(build_review_run_report(
            &base,
            &head,
            pull_request,
            &plan,
            &[],
            &[],
            &expired,
            facts.trigger,
            false,
        ));
    }

    // Record before performing. See `review_execution`'s module comment: a
    // crash after this point costs a missed review, and a crash before it would
    // cost a duplicated one.
    let mut recorded_now = Vec::new();
    let mut skipped = Vec::new();
    // Which row answers for which dimension, so a `gh` call that fails can
    // reopen exactly the review it failed to ask for.
    let mut rows_by_type: std::collections::BTreeMap<String, i64> =
        std::collections::BTreeMap::new();
    for write in &plan.ledger {
        let (request, created) = broker
            .store()
            .record_review_request_with_trigger(
                &repository,
                pull_request,
                &write.review_type,
                &head,
                base_commit.as_deref(),
                write.backend,
                facts.trigger,
                now_ms(),
            )
            .map_err(to_usage)?;
        if !created {
            skipped.push(serde_json::json!({
                "review_type": write.review_type,
                "why": "already recorded for this head",
                "state": request.state,
            }));
            continue;
        }
        if write.state != crate::ReviewRequestState::Requested {
            broker
                .store()
                .set_review_request_state(
                    request.id,
                    write.state,
                    write.detail.as_deref(),
                    now_ms(),
                )
                .map_err(to_usage)?;
        }
        rows_by_type.insert(write.review_type.clone(), request.id);
        recorded_now.push(write.review_type.clone());
    }

    // Every GitHub write goes through the coordinated lane, which takes the
    // repository write lock and records the operation. A failed call stops the
    // tick: the rest of the projection describes a pull request state this one
    // was supposed to establish.
    //
    // Before it stops, the review that call was asking for goes back to
    // `abandoned`. The ledger's unique index makes a row permanent for its
    // head, so leaving it at `requested` would mean one failed `gh` call
    // settles that dimension forever -- the next tick would read the row,
    // count it as spend, and skip. `abandoned` is the one state the router may
    // ask about again, which is what turns a transient GitHub failure into a
    // retry instead of a silently missing review.
    let mut performed = Vec::new();
    for call in &plan.gh {
        let report = broker
            .run_coordinated_operation(crate::CoordinatedCommand {
                session_id: session_id.expect("a non-dry run requires a session"),
                provider: crate::OperationProvider::Github,
                repository: Some(repository.clone()),
                resolved_target: None,
                scope: Some(format!("pr/{pull_request}")),
                declared_effect: Some(crate::OperationEffect::Write),
                destructive_confirmed: false,
                authorization_reason: Some(call.purpose.clone()),
                args: call.args.clone(),
            })
            .map_err(to_usage)?;
        // What the provider said, classified. Built for every call because the
        // failing branch below needs it in two places and the report needs it
        // whether or not a review row is attached to this call.
        let refusal = crate::ReviewRefusal::from_provider_output(
            &report.stdout,
            &report.stderr,
            &format!("the coordinated GitHub write failed: {}", call.purpose),
        );
        performed.push(serde_json::json!({
            "purpose": call.purpose,
            "operation_id": report.operation.id,
            "success": report.command_success,
            // Why it failed, in the provider's own words and as a class. #173:
            // this was dropped, so a spent quota reached the operator as a row
            // indistinguishable from "not requested yet".
            "refusal": (!report.command_success).then(|| refusal.clone()),
        }));
        if !report.command_success {
            if let Some(id) = call
                .review_type
                .as_deref()
                .and_then(|review_type| rows_by_type.get(review_type))
            {
                broker
                    .store()
                    .set_review_request_state(
                        *id,
                        crate::ReviewRequestState::Abandoned,
                        // The classification and the provider's words, not the
                        // fact that something failed -- which the state
                        // already said. `abandoned` is where a refused review
                        // lands, and until now the row could not say whether
                        // waiting would help (#173).
                        Some(&refusal.detail()),
                        now_ms(),
                    )
                    .map_err(to_usage)?;
            }
            // The report still goes out: it names the operation that failed
            // and the row that went back to `abandoned`, which is what a
            // caller needs to decide whether to retry.
            print_json(&build_review_run_report(
                &base,
                &head,
                pull_request,
                &plan,
                &performed,
                &skipped,
                &expired,
                facts.trigger,
                true,
            ))?;
            // The classification rides on the error too. A caller that only
            // reads exit status and stderr is the common case, and it is the
            // one that spent 48 hours not knowing in #173.
            return Err(UsageError::Message(format!(
                "coordinated GitHub write failed: {} [{}]",
                call.purpose,
                refusal.detail()
            )));
        }
    }

    // The tick acted, so this look becomes the one the next tick compares
    // against. Deliberately last: recording it earlier would mean a tick that
    // failed part-way had already declared the transition handled, and the
    // next tick would see `Scheduled` and never retry what it missed.
    if let Some(snapshot) = snapshot.as_ref() {
        broker
            .store()
            .record_pull_request_observation(&crate::PullRequestObservation {
                repository: repository.clone(),
                pr_number: pull_request,
                head_commit: snapshot.head_commit.clone(),
                base_ref: snapshot.base_ref.clone(),
                base_commit: snapshot.base_commit.clone(),
                is_draft: snapshot.is_draft,
                state: snapshot.state.clone(),
                dismissed_reviews: snapshot.dismissed_reviews,
                observed_at: now_ms(),
            })
            .map_err(to_usage)?;
    }

    Ok(build_review_run_report(
        &base,
        &head,
        pull_request,
        &plan,
        &performed,
        &skipped,
        &expired,
        facts.trigger,
        true,
    ))
}

/// Route every open pull request in a repository, once.
///
/// One bounded foreground pass, in the same shape as `watch pr tick` and for
/// the same reason: **the broker never starts a background poller.** A daemon
/// inside a coordination tool is a second thing to supervise, it holds the
/// machine-wide database open for its whole life, and it fails silently by
/// construction because nobody is watching the thing that watches. A command
/// that does one pass and exits can be run by cron, by a CI schedule, by a
/// hook, or by a person, and each of those already has a way to tell you it
/// stopped.
///
/// The bound matters as much as the pass. `--limit` caps how many pull requests
/// one invocation touches, so a repository with sixty open pull requests costs
/// a predictable number of provider calls rather than however many there happen
/// to be.
///
/// A pull request that fails is recorded and skipped, not fatal: one
/// unreachable pull request must not stop the other fifty-nine from being
/// routed.
pub(super) fn run_review_tick(parsed: Parsed) -> Result<(), UsageError> {
    let repository = parsed
        .repository
        .clone()
        .ok_or_else(|| UsageError::Message("review tick requires --repo <owner/name>".into()))?;
    if parsed.session.is_none() && !parsed.dry_run {
        return Err(UsageError::Message(
            "review tick requires --session <id>, or --dry-run to plan only".into(),
        ));
    }
    let limit = parsed.limit.unwrap_or(20).clamp(1, 100);

    let root = {
        let broker = open_broker(true)?;
        broker.main_root().to_path_buf()
    };
    let open = list_open_pull_requests(&root, &repository, limit)?;

    let mut visited = Vec::new();
    for pull_request in &open {
        let mut one = parsed.clone();
        one.pr_number = Some(*pull_request);
        // A sweep stands in no pull request's checkout, so it always reads the
        // change from the provider.
        one.from_provider = true;
        match run_review_run(one) {
            Ok(report) => visited.push(serde_json::json!({
                "pull_request": pull_request,
                "trigger": report.get("trigger").cloned(),
                // The head every handoff below was decided against. An adapter
                // needs it to check out the right commit and to close the row
                // it was actually handed, rather than whatever the most recent
                // push made current in between.
                "head": report.get("head").cloned(),
                "requested": report
                    .get("plan")
                    .and_then(|plan| plan.get("ledger"))
                    .cloned(),
                // Carried whole, not counted: a sweep exists so that one
                // adapter invocation can start every review it produced, and a
                // count would force the adapter to re-run `review run` per
                // pull request to learn what it was handed.
                "chau7_handoff": report.get("chau7_handoff").cloned(),
                // Carried for the same reason as the handoffs: the adapter
                // performs both, and a sweep that reported only what to start
                // would leak a tab per finished review across the repository.
                "chau7_teardown": report.get("chau7_teardown").cloned(),
                "expired": report.get("expired").cloned(),
                "ok": true,
            })),
            // A pull request that cannot be routed is reported and left
            // behind. Stopping here would let one unreachable pull request
            // decide that none of the others get reviewed.
            Err(error) => visited.push(serde_json::json!({
                "pull_request": pull_request,
                "ok": false,
                "error": match error {
                    UsageError::Message(message) => message,
                    UsageError::Exit { message, .. } => message,
                    UsageError::Help => "usage".to_string(),
                    UsageError::SilentExit(code) => format!("exited with code {code}"),
                },
            })),
        }
    }

    print_json(&serde_json::json!({
        "repository": repository,
        "limit": limit,
        "open_pull_requests": open.len(),
        "performed": !parsed.dry_run,
        "visited": visited,
    }))
}

/// Open pull request numbers, oldest first, capped.
///
/// Oldest first so a repository with more open pull requests than `--limit`
/// makes progress on a fixed set rather than re-routing whatever happens to be
/// newest every pass and never reaching the rest.
pub(super) fn list_open_pull_requests(
    root: &Path,
    repository: &str,
    limit: u32,
) -> Result<Vec<i64>, UsageError> {
    let output = std::process::Command::new("gh")
        .current_dir(root)
        .args([
            "pr",
            "list",
            "--repo",
            repository,
            "--state",
            "open",
            "--limit",
            &limit.to_string(),
            "--json",
            "number",
        ])
        .output()
        .map_err(|error| UsageError::Message(format!("gh pr list: {error}")))?;
    if !output.status.success() {
        return Err(UsageError::Message(format!(
            "cannot list open pull requests in {repository}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| UsageError::Message(format!("gh pr list returned no JSON: {error}")))?;
    let mut numbers: Vec<i64> = json
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["number"].as_i64())
                .collect()
        })
        .unwrap_or_default();
    numbers.sort_unstable();
    Ok(numbers)
}

/// The tab snapshot, or none.
///
/// An absent snapshot is not an error: it means "no tabs", and routing then
/// defers every Chau7 review rather than spawning into a workspace it cannot
/// see. That is the safe reading, and it is what makes `review run` usable from
/// a cron that has no Chau7 access at all -- it still records, mentions bots,
/// and projects.
pub(super) fn read_tab_snapshot(parsed: &Parsed) -> Result<Vec<crate::Chau7Tab>, UsageError> {
    let Some(path) = parsed.tabs_file.as_deref() else {
        return Ok(Vec::new());
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|error| UsageError::Message(format!("cannot read {}: {error}", path.display())))?;
    serde_json::from_str(&raw).map_err(|error| {
        UsageError::Message(format!(
            "tab snapshot is not a Chau7 tab_list array: {error}"
        ))
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn build_review_run_report(
    base: &str,
    head: &str,
    pull_request: i64,
    plan: &crate::ReviewExecutionPlan,
    performed: &[serde_json::Value],
    skipped: &[serde_json::Value],
    expired: &[crate::ExpiredReview],
    trigger: Option<crate::ReviewTrigger>,
    executed: bool,
) -> serde_json::Value {
    serde_json::json!({
        "base": base,
        "head": head,
        "pull_request": pull_request,
        // Which transition this tick decided had happened. Absent when the
        // provider could not be reached, which is itself worth seeing: the
        // whole rule set then ran against a change nobody could describe.
        "trigger": trigger,
        "plan": plan,
        "performed": executed,
        "github_operations": performed,
        "already_recorded": skipped,
        // Reviews this tick stopped waiting for, and therefore may re-ask. An
        // entry here every tick means something starts reviews and never
        // reports back, which is worth more attention than the retry it causes.
        "expired": expired,
        // The one thing the broker cannot do itself. An adapter with Chau7
        // access starts these, then closes each row with
        // `aethyme broker review state --repo <r> --pr <n> --type <t> --state <s>`.
        "chau7_handoff": plan.chau7,
        // Tabs to close, performed before `chau7_handoff` is started: a tick
        // may reclaim a dimension's workspace and dispatch a new review of
        // that same dimension into it, and the other order spawns into an
        // occupied directory.
        "chau7_teardown": plan.chau7_close,
    })
}

/// Print the review ledger for a repository, or for one pull request in it.
///
/// The executor writes this table and nothing reads it back, which is the
/// difference between a review that is missing and a review that is missing
/// silently. A row carries who was asked, for which head, and how it ended, so
/// "why was there no security review on #412" is answered by one command
/// rather than by reading the router's source.
pub(super) fn run_review_ledger(parsed: Parsed) -> Result<(), UsageError> {
    let repository = parsed
        .repository
        .clone()
        .ok_or_else(|| UsageError::Message("review ledger requires --repo <owner/name>".into()))?;
    let mut broker = open_broker(true)?;
    let rows = match parsed.pr_number {
        Some(pull_request) => broker
            .store()
            .review_requests_for_pr(&repository, pull_request),
        None => broker.store().review_requests_for_repository(&repository),
    }
    .map_err(to_usage)?;

    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        out!("No reviews recorded for {repository}.");
        return Ok(());
    }
    for row in &rows {
        let head = &row.head_commit[..12.min(row.head_commit.len())];
        out!(
            "#{:<5} {:<10} {:<10} {:<16} {head}",
            row.pr_number,
            row.review_type,
            row.state.label(),
            row.backend
        );
        if let Some(detail) = &row.detail {
            out!("        {detail}");
        }
    }
    Ok(())
}

/// Report what became of one review, or record a provider completion that was
/// never requested by Aethyme.
///
/// This is the other half of `review run`'s handoff. The broker decides and
/// records; a Chau7 adapter or a provider bot performs, and closes the row
/// here. Without it the ledger only ever says `requested`, and the router's
/// concurrency slots fill up and never drain.
///
/// `--head` names the request being reported. For a completion without
/// `--head`, an exact completion-head match is used; if none exists, the
/// result is recorded as unsolicited. Naming a head is therefore required for
/// a late result whose completion commit differs from the request it belongs
/// to.
pub(super) fn run_review_state(parsed: Parsed) -> Result<(), UsageError> {
    let repository = parsed.repository.clone().ok_or_else(|| {
        UsageError::Message(
            "review state requires --repo <owner/name> --pr <number> --type <review-type> --state <state>"
                .into(),
        )
    })?;
    let pull_request = parsed
        .pr_number
        .ok_or_else(|| UsageError::Message("review state requires --pr <number>".into()))?;
    let review_type = parsed
        .review_type
        .clone()
        .ok_or_else(|| UsageError::Message("review state requires --type <review-type>".into()))?;
    let label = parsed
        .review_state
        .clone()
        .ok_or_else(|| UsageError::Message("review state requires --state <state>".into()))?;
    let state = crate::ReviewRequestState::parse(&label).ok_or_else(|| {
        UsageError::Message(format!(
            "unknown review state {label:?}; expected requested, running, satisfied, failed, recorded, or abandoned"
        ))
    })?;
    // `waived` is reachable through `review waive` and nowhere else. This
    // command takes no reason and no author, so accepting the label here would
    // reintroduce the unattributed override in the very place that is supposed
    // to have stopped being one.
    if state == crate::ReviewRequestState::Waived {
        return Err(UsageError::Message(
            "`review state --state waived` is not a thing; use `aethyme broker advanced review waive \
             --repo <owner/name> --pr <number> --type <review-type> --head <sha> --reason <text>`, \
             which records who waived it and why"
                .into(),
        ));
    }

    let has_completion_argument = parsed.verdict.is_some()
        || parsed.completed_for_commit.is_some()
        || parsed.reviewer_provider.is_some()
        || parsed.reviewer_model.is_some();
    let completion = if state == crate::ReviewRequestState::Satisfied {
        let verdict_label = parsed.verdict.as_deref().ok_or_else(|| {
            UsageError::Message(
                "review state --state satisfied requires --verdict <pass|fail|changes_requested|commented>"
                    .into(),
            )
        })?;
        let verdict = crate::ReviewVerdict::parse(verdict_label).ok_or_else(|| {
            UsageError::Message(format!(
                "unknown review verdict {verdict_label:?}; expected pass, fail, changes_requested, or commented"
            ))
        })?;
        let completed_for_commit = parsed
            .completed_for_commit
            .clone()
            .map(|commit| commit.trim().to_string())
            .filter(|commit| !commit.is_empty())
            .ok_or_else(|| {
                UsageError::Message(
                    "review state --state satisfied requires --completed-for-commit <sha>".into(),
                )
            })?;
        let reviewer_provider = parsed
            .reviewer_provider
            .clone()
            .map(|provider| provider.trim().to_string())
            .filter(|provider| !provider.is_empty())
            .ok_or_else(|| {
                UsageError::Message(
                    "review state --state satisfied requires --reviewer-provider <provider>".into(),
                )
            })?;
        let reviewer_model = parsed
            .reviewer_model
            .clone()
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        Some((
            completed_for_commit,
            verdict,
            reviewer_provider,
            reviewer_model,
        ))
    } else {
        if has_completion_argument {
            return Err(UsageError::Message(
                "--verdict, --completed-for-commit, and reviewer identity are only valid when --state satisfied"
                    .into(),
            ));
        }
        None
    };

    let mut broker = open_broker(parsed.read_only_snapshot)?;

    // `review ledger` prints a 12-character head while this command matched on
    // the full forty, so the one value an operator has in front of them was the
    // one value this refused -- and the refusal below names `review ledger` as
    // where to look, which closed the loop. Resolving a prefix opens it.
    //
    // A prefix that resolves to nothing keeps the operator's own spelling, so
    // the "no code review is recorded" refusal below names what they typed.
    // A prefix that is not a commit at all is refused here instead: it is a
    // usage error, and reporting it as an absent review would be this command's
    // original defect wearing the other face.
    let head = match parsed.head.as_deref() {
        Some(prefix) if prefix.len() < 40 => {
            let recorded: Vec<String> = broker
                .store()
                .review_requests_for_pr(&repository, pull_request)
                .map_err(to_usage)?
                .into_iter()
                .filter(|row| row.review_type == review_type)
                .map(|row| row.head_commit)
                .collect();
            match crate::review::resolve_review_head_prefix(
                recorded.iter().map(String::as_str),
                prefix,
            ) {
                Ok(Some(head)) => Some(head),
                Ok(None) => Some(prefix.to_string()),
                Err(crate::review::HeadPrefixError::Malformed(message)) => {
                    return Err(UsageError::Message(message));
                }
                Err(crate::review::HeadPrefixError::Ambiguous(candidates)) => {
                    return Err(UsageError::Message(format!(
                        "head {prefix:?} matches {} recorded {review_type} commits on \
                         {repository}#{pull_request} ({}); name the full commit",
                        candidates.len(),
                        candidates.join(", ")
                    )));
                }
            }
        }
        other => other.map(str::to_string),
    };

    // A completion without --head is unambiguous only when its completion
    // commit already identifies a row. If it names a different commit from
    // the latest request, attach it as unsolicited rather than silently
    // rewriting an older request; the reviewer can pass --head to report a
    // late result for that request explicitly.
    let existing_head = match (head.as_deref(), completion.as_ref()) {
        (Some(head), _) => Some(head),
        (None, Some((completed_for_commit, ..))) => Some(completed_for_commit.as_str()),
        (None, None) => None,
    };
    let existing = broker
        .store()
        .latest_review_request(&repository, pull_request, &review_type, existing_head)
        .map_err(to_usage)?;
    let updated = if let Some((completed_for_commit, verdict, reviewer_provider, reviewer_model)) =
        completion
    {
        match existing {
            Some(existing) => broker
                .store()
                .complete_review_request(
                    existing.id,
                    &completed_for_commit,
                    verdict,
                    &reviewer_provider,
                    reviewer_model.as_deref(),
                    parsed.note.as_deref(),
                    now_ms(),
                )
                .map_err(to_usage)?,
            None if parsed.head.is_none() => {
                broker
                    .store()
                    .record_unsolicited_review_completion(
                        &repository,
                        pull_request,
                        &review_type,
                        &completed_for_commit,
                        verdict,
                        &reviewer_provider,
                        reviewer_model.as_deref(),
                        parsed.note.as_deref(),
                        now_ms(),
                    )
                    .map_err(to_usage)?
                    .0
            }
            None => {
                return Err(UsageError::Message(format!(
                    "no {review_type} review is recorded for {repository}#{pull_request}{}; `aethyme broker advanced review ledger --repo {repository} --pr {pull_request}` lists what is, and omit --head only for an unsolicited completion",
                    match parsed.head.as_deref() {
                        Some(head) => format!(" at {head}"),
                        None => String::new(),
                    }
                )));
            }
        }
    } else {
        let existing = existing.ok_or_else(|| {
            UsageError::Message(format!(
                "no {review_type} review is recorded for {repository}#{pull_request}{}; `aethyme broker advanced review ledger --repo {repository} --pr {pull_request}` lists what is",
                match parsed.head.as_deref() {
                    Some(head) => format!(" at {head}"),
                    None => String::new(),
                }
            ))
        })?;
        broker
            .store()
            .set_review_request_state(existing.id, state, parsed.note.as_deref(), now_ms())
            .map_err(to_usage)?
    };

    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&updated)?);
    } else {
        out!(
            "{} review on {}#{} at {} is now {}.",
            updated.review_type,
            updated.repository,
            updated.pr_number,
            &updated.head_commit[..12.min(updated.head_commit.len())],
            updated.state.label()
        );
    }
    Ok(())
}

/// Excuse one dimension at one head, with an author and a reason.
///
/// Repository-scoped rather than session-scoped, like `ledger` and `state`: a
/// waiver is a statement about a pull request, and binding it to a session
/// would make it expire with a worktree instead of with a commit.
pub(super) fn run_review_waive(parsed: Parsed) -> Result<(), UsageError> {
    let repository = parsed.repository.clone().ok_or_else(|| {
        UsageError::Message(
            "review waive requires --repo <owner/name> --pr <number> --type <review-type> --head <sha> --reason <text>"
                .into(),
        )
    })?;
    let pull_request = parsed
        .pr_number
        .ok_or_else(|| UsageError::Message("review waive requires --pr <number>".into()))?;
    let review_type = parsed
        .review_type
        .clone()
        .ok_or_else(|| UsageError::Message("review waive requires --type <review-type>".into()))?;
    // Required, unlike `review state`, and deliberately not defaulted to the
    // pull request's current head. The head *is* the waiver's scope, so a
    // default would have the operator excuse a dimension at a commit they never
    // named -- and for the dimension most worth waiving, the one no review was
    // ever requested for, there is no latest request to default to.
    let head_argument = parsed.head.clone().ok_or_else(|| {
        UsageError::Message(
            "review waive requires --head <sha>: a waiver applies to one commit, and \
             defaulting it would excuse a dimension at a commit nobody named"
                .into(),
        )
    })?;
    // Text, not a digest. Every other `--reason` in this CLI authorizes a
    // coordinated operation and is retained only as a SHA-256 so the
    // authorization can be proven without being kept; this one is the
    // explanation a reviewer reads months later, and a digest tells them
    // nothing.
    let reason = parsed
        .reason
        .clone()
        .map(|reason| reason.trim().to_string())
        .filter(|reason| !reason.is_empty())
        .ok_or_else(|| {
            UsageError::Message(
                "review waive requires --reason <text>: a waiver with no reason is the \
                 unexplained override this command exists to replace"
                    .into(),
            )
        })?;

    let mut broker = open_broker(parsed.read_only_snapshot)?;

    // Resolved against every recorded head on this pull request, not just this
    // dimension's: the dimension being waived is frequently the one with no row
    // at all, so filtering by type would refuse the prefix exactly when the
    // operator most needs it.
    let head = if head_argument.len() < 40 {
        let recorded: Vec<String> = broker
            .store()
            .review_requests_for_pr(&repository, pull_request)
            .map_err(to_usage)?
            .into_iter()
            .map(|row| row.head_commit)
            .collect();
        match crate::review::resolve_review_head_prefix(
            recorded.iter().map(String::as_str),
            &head_argument,
        ) {
            Ok(Some(head)) => head,
            // Nothing recorded matches, which is ordinary here -- a dimension
            // nobody ever requested leaves no head to match against. Refusing
            // is still right: an abbreviation cannot be expanded from nothing,
            // and waiving a commit this command guessed at would be worse.
            Ok(None) => {
                return Err(UsageError::Message(format!(
                    "head {head_argument:?} matches no commit recorded for \
                     {repository}#{pull_request}; give the full 40-character commit"
                )));
            }
            Err(crate::review::HeadPrefixError::Malformed(message)) => {
                return Err(UsageError::Message(message));
            }
            Err(crate::review::HeadPrefixError::Ambiguous(candidates)) => {
                return Err(UsageError::Message(format!(
                    "head {head_argument:?} matches {} recorded commits on \
                     {repository}#{pull_request} ({}); name the full commit",
                    candidates.len(),
                    candidates.join(", ")
                )));
            }
        }
    } else {
        head_argument
    };

    // A satisfied row is the one thing a waiver must never overwrite. The row
    // records that a review happened; replacing it with "excused" destroys the
    // only evidence that it did, and the operator asking for this has almost
    // certainly named the wrong dimension -- a satisfied review is not blocking
    // anything.
    if let Some(existing) = broker
        .store()
        .latest_review_request(&repository, pull_request, &review_type, Some(&head))
        .map_err(to_usage)?
        && existing.state == crate::ReviewRequestState::Satisfied
    {
        return Err(UsageError::Message(format!(
            "the {review_type} review on {repository}#{pull_request} at {} is already \
             satisfied; waiving it would replace a recorded verdict with an excuse",
            &head[..12.min(head.len())]
        )));
    }

    let waiver = crate::ReviewWaiver::new(
        session_agent_identity(parsed.agent.as_deref()).as_deref(),
        &reason,
    );
    let waived = broker
        .store()
        .waive_review_request(
            &repository,
            pull_request,
            &review_type,
            &head,
            &waiver.detail(),
            now_ms(),
        )
        .map_err(to_usage)?;

    if parsed.json {
        out!("{}", serde_json::to_string_pretty(&waived)?);
    } else {
        out!(
            "{} review on {}#{} at {} is waived by {}: {}",
            waived.review_type,
            waived.repository,
            waived.pr_number,
            &waived.head_commit[..12.min(waived.head_commit.len())],
            waiver.who,
            waiver.reason
        );
        out!("Only this dimension, only this head. A new head is unwaived.");
    }
    Ok(())
}

pub(super) fn run_review(parsed: Parsed) -> Result<(), UsageError> {
    let action = parsed
        .positional
        .first()
        .map(String::as_str)
        .ok_or_else(|| {
            UsageError::Message(
                "review requires plan, run, ledger, state, waive, register, show, request, unlock, reassign, or abandon"
                    .into(),
            )
        })?;
    if parsed.positional.len() != 1 {
        return Err(UsageError::Message(format!(
            "review {action} accepts no positional arguments"
        )));
    }
    // `plan` is the only review action that is about a change rather than a
    // session: it answers "what would this repository do about this diff", and
    // it performs nothing, so it neither needs nor should require a session.
    if action == "plan" {
        return run_review_plan(parsed);
    }
    if action == "run" {
        let report = run_review_run(parsed)?;
        return print_json(&report);
    }
    // A sweep over every open pull request, which is `run` repeated. It takes
    // its own `--session` check rather than the shared one below because
    // `--dry-run` must stay usable without one, exactly as it is for `run`.
    if action == "tick" {
        return run_review_tick(parsed);
    }
    // `ledger` and `state` are about the router's ledger rather than a
    // session's review lifecycle, so they take a repository instead of the
    // `--session` every action below requires. `ledger` reads and `state` is
    // written by whoever performed the review, which is never this broker.
    if action == "ledger" {
        return run_review_ledger(parsed);
    }
    if action == "state" {
        return run_review_state(parsed);
    }
    // Beside `state` rather than below: a waiver is about a pull request, not
    // about a session, so requiring `--session` would tie a decision about a
    // commit to the lifetime of a worktree.
    if action == "waive" {
        return run_review_waive(parsed);
    }
    let session_id = parsed
        .session
        .ok_or_else(|| UsageError::Message(format!("review {action} requires --session <id>")))?;
    match action {
        "register" => {
            let repository = parsed.repository.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "review register requires --session <id> --repo <owner/name> --pr <number>"
                        .into(),
                )
            })?;
            let pr_number = parsed.pr_number.ok_or_else(|| {
                UsageError::Message(
                    "review register requires --session <id> --repo <owner/name> --pr <number>"
                        .into(),
                )
            })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let policy = crate::ReviewPolicy::load(broker.main_root())?;
            let session = broker.store().session(session_id)?;
            let snapshot = crate::load_review_provider_snapshot(
                Path::new(&session.worktree_path),
                repository,
                pr_number,
                &policy,
            )?;
            let report =
                broker.register_review_lifecycle(session_id, repository, &snapshot, now_ms())?;
            render_review_report(&report, parsed.json)?;
        }
        "show" => {
            let mut broker = open_broker(true)?;
            let lifecycle = broker
                .store()
                .review_lifecycle_for_session(session_id)?
                .ok_or(crate::BrokerError::ReviewLifecycleNotFound(session_id))?;
            let policy = crate::ReviewPolicy::load(broker.main_root())?;
            let report = crate::ReviewLifecycleReport {
                next_action: review_next_action(&lifecycle),
                policy,
                lifecycle,
                changed: false,
                operation_id: None,
                non_blocking_feedback: true,
            };
            render_review_report(&report, parsed.json)?;
        }
        "request" | "unlock" => {
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let lifecycle = broker
                .store()
                .review_lifecycle_for_session(session_id)?
                .ok_or(crate::BrokerError::ReviewLifecycleNotFound(session_id))?;
            let session = broker.store().session(session_id)?;
            if session.status.is_closed() {
                return Err(UsageError::Message(format!(
                    "session {session_id} is closed; `review show` remains available for diagnostics, but review mutations require `aethyme broker advanced review reassign --session {session_id} --to-session <live-id> --reason <text>` or `aethyme broker advanced review abandon --session {session_id} --reason <text>`"
                )));
            }
            let policy = crate::ReviewPolicy::load(broker.main_root())?;
            let repository = lifecycle
                .repository
                .strip_prefix("github.com/")
                .unwrap_or(&lifecycle.repository);
            let snapshot = crate::load_review_provider_snapshot(
                Path::new(&session.worktree_path),
                repository,
                lifecycle.pr_number,
                &policy,
            )?;
            let report = if action == "request" {
                broker.request_review(session_id, &snapshot, now_ms())?
            } else {
                broker.unlock_review_validation(session_id, &snapshot, now_ms())?
            };
            render_review_report(&report, parsed.json)?;
        }
        "reassign" => {
            let to_session_id = parsed.to_session.ok_or_else(|| {
                UsageError::Message(
                    "review reassign requires --session <closed-id> --to-session <live-id> --reason <text>"
                        .into(),
                )
            })?;
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message(
                    "review reassign requires --session <closed-id> --to-session <live-id> --reason <text>"
                        .into(),
                )
            })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report =
                broker.reassign_review_lifecycle(session_id, to_session_id, reason, now_ms())?;
            render_review_report(&report, parsed.json)?;
        }
        "abandon" => {
            let reason = parsed.reason.as_deref().ok_or_else(|| {
                UsageError::Message("review abandon requires --session <id> --reason <text>".into())
            })?;
            let mut broker = open_broker(parsed.read_only_snapshot)?;
            let report = broker.abandon_review_lifecycle(session_id, reason, now_ms())?;
            if parsed.json {
                out!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                out!(
                    "Review lifecycle {} abandoned; {}",
                    report.lifecycle.id,
                    report.next_action
                );
            }
        }
        other => {
            return Err(UsageError::Message(format!(
                "unknown review action {other:?}; expected plan, run, ledger, state, register, show, request, unlock, reassign, or abandon"
            )));
        }
    }
    Ok(())
}

pub(super) fn render_review_report(
    report: &crate::ReviewLifecycleReport,
    json: bool,
) -> Result<(), UsageError> {
    if json {
        out!("{}", serde_json::to_string_pretty(report)?);
    } else {
        out!(
            "Review lifecycle {}: {}{}",
            report.lifecycle.id,
            report.lifecycle.state.as_str(),
            if report.changed { " (advanced)" } else { "" }
        );
        out!(
            "  session/queue: {} / {}",
            report.lifecycle.session_id,
            report
                .lifecycle
                .queue_entry_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "not yet verified".into())
        );
        out!(
            "  repository/PR: {} / #{}",
            report.lifecycle.repository,
            report.lifecycle.pr_number
        );
        out!("  commit: {}", report.lifecycle.commit_sha);
        if let Some(operation_id) = report.operation_id {
            out!("  coordinated operation: {operation_id}");
        }
        out!("  next: {}", report.next_action);
    }
    Ok(())
}

pub(super) fn review_next_action(lifecycle: &crate::ReviewLifecycle) -> String {
    match lifecycle.state {
        crate::ReviewLifecycleState::DraftOpened => {
            format!("aethyme broker submit --session {}", lifecycle.session_id)
        }
        crate::ReviewLifecycleState::LocalSubmissionVerified
        | crate::ReviewLifecycleState::ReplacementCommitSubmitted => {
            format!(
                "aethyme broker advanced review request --session {}",
                lifecycle.session_id
            )
        }
        crate::ReviewLifecycleState::ReviewRequested => {
            format!(
                "aethyme broker advanced review show --session {}",
                lifecycle.session_id
            )
        }
        crate::ReviewLifecycleState::ChangesRequested => {
            "commit the replacement through the accepted session, then submit it".into()
        }
        crate::ReviewLifecycleState::ReviewSatisfied => {
            format!(
                "aethyme broker advanced review unlock --session {}",
                lifecycle.session_id
            )
        }
        crate::ReviewLifecycleState::ValidationUnlocked => {
            "validation is explicitly unlocked".into()
        }
    }
}
