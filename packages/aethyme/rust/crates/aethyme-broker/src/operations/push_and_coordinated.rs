use super::redaction::{add_failure_stderr, redacted_failure_stderr};
use super::*;

fn add_operation_liveness(details: &mut serde_json::Value, liveness: serde_json::Value) {
    if let Some(details) = details.as_object_mut() {
        details.insert("operation_liveness".into(), liveness);
    }
}

fn with_push_planning(mut extra: serde_json::Value, planning: &PushPlanning) -> serde_json::Value {
    if let (Some(extra), Some(push)) = (extra.as_object_mut(), planning.journal_value()) {
        extra.insert("push_reconciliation".into(), push);
    }
    extra
}

/// Push sources that mean something different in each worktree.
///
/// Refs under `refs/` are shared by every worktree of a repository, so
/// `main:refs/heads/x` resolves identically wherever the command runs. `HEAD`
/// does not -- it is per-worktree state. `broker git` executes inside the
/// *session* worktree rather than the caller's, so a `HEAD:` refspec sent from
/// somewhere else silently publishes the session's commit under the caller's
/// chosen branch name, and the push reports success (#269).
pub(crate) fn worktree_relative_push_sources(args: &[String]) -> Vec<String> {
    let Some(args) = git_subcommand_args(args) else {
        return Vec::new();
    };
    if args.first().map(String::as_str) != Some("push") {
        return Vec::new();
    }
    args.iter()
        .skip(1)
        .filter(|argument| !argument.starts_with('-'))
        .filter(|argument| {
            let refspec = argument.strip_prefix('+').unwrap_or(argument);
            let source = refspec.split(':').next().unwrap_or(refspec);
            let base = source.split(['~', '^']).next().unwrap_or(source);
            base == "HEAD" || base == "@" || base.starts_with("@{")
        })
        .cloned()
        .collect()
}

/// Whether `candidate` is the session worktree or lives inside it.
///
/// Compared after canonicalization so a symlinked temporary directory -- the
/// normal shape of a scratch checkout on macOS -- is not mistaken for a
/// different tree.
pub(crate) fn is_within(candidate: &Path, root: &Path) -> bool {
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical(candidate).starts_with(canonical(root))
}

fn plan_exact_push(
    cwd: &Path,
    args: &[String],
    target: Option<&crate::ResolvedRemoteTarget>,
) -> PushPlanning {
    let Some(args) = git_subcommand_args(args) else {
        return PushPlanning::NotApplicable;
    };
    if args.first().map(String::as_str) != Some("push") {
        return PushPlanning::NotApplicable;
    }
    let Some(target) = target else {
        return PushPlanning::Unsupported {
            reason: "push_target_is_not_a_resolved_remote",
        };
    };
    if args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "--all"
                | "--delete"
                | "--dry-run"
                | "--follow-tags"
                | "--mirror"
                | "--prune"
                | "--tags"
        )
    }) {
        return PushPlanning::Unsupported {
            reason: "push_uses_implicit_or_set_expanding_options",
        };
    }
    let remote_positions = args
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, argument)| (argument == &target.remote_name).then_some(index))
        .collect::<Vec<_>>();
    let [remote_index] = remote_positions.as_slice() else {
        return PushPlanning::Unsupported {
            reason: "push_remote_position_is_not_unique",
        };
    };
    let refspecs = &args[*remote_index + 1..];
    if refspecs.is_empty()
        || refspecs
            .iter()
            .any(|refspec| refspec.starts_with('-') || refspec == "--")
    {
        return PushPlanning::Unsupported {
            reason: "push_does_not_have_only_explicit_refspecs",
        };
    }

    let Ok(repo) = crate::GitRepo::discover(cwd) else {
        return PushPlanning::Unavailable {
            reason: "local_repository_evidence_unavailable",
        };
    };
    let mut seen_destinations = BTreeSet::new();
    let mut destinations = Vec::with_capacity(refspecs.len());
    for refspec in refspecs {
        let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
        // A refspec without a colon pushes the named ref to the ref of the
        // same name on the remote, so its destination is derivable locally.
        // Resolving it here is what lets a failed `push origin <branch>`,
        // `push -u origin HEAD`, or `push origin refs/tags/<tag>` be
        // classified from remote evidence instead of being reported as an
        // unknown outcome that write-blocks the repository.
        let resolved_destination;
        let (source, destination) = match refspec.split_once(':') {
            Some(pair) => pair,
            None => {
                let Some(full) = repo.full_ref_name(refspec) else {
                    return PushPlanning::Unsupported {
                        reason: "push_refspec_does_not_resolve_to_one_local_ref",
                    };
                };
                resolved_destination = full;
                (refspec, resolved_destination.as_str())
            }
        };
        if source.is_empty()
            || destination.is_empty()
            || source.starts_with('-')
            || source.contains(':')
            || destination.contains(':')
            || !destination.starts_with("refs/")
            || !seen_destinations.insert(destination.to_string())
            || repo.validate_push_destination(destination).is_err()
        {
            return PushPlanning::Unsupported {
                reason: "push_refspec_is_not_one_unique_full_destination",
            };
        }
        let Ok(proposed_sha) = repo.resolve_push_source(source) else {
            return PushPlanning::Unavailable {
                reason: "push_source_object_is_unavailable",
            };
        };
        if proposed_sha.len() != 40 || !proposed_sha.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return PushPlanning::Unavailable {
                reason: "push_source_object_is_not_a_full_sha",
            };
        }
        destinations.push(ExactPushDestination {
            destination_ref: destination.into(),
            pre_push_sha: None,
            proposed_sha: proposed_sha.to_ascii_lowercase(),
        });
    }

    let destination_refs = destinations
        .iter()
        .map(|destination| destination.destination_ref.clone())
        .collect::<Vec<_>>();
    let Ok(pre_push) = repo.remote_ref_oids(&target.remote_name, &destination_refs) else {
        return PushPlanning::Unavailable {
            reason: "pre_push_remote_evidence_unavailable",
        };
    };
    for destination in &mut destinations {
        destination.pre_push_sha = pre_push
            .get(&destination.destination_ref)
            .cloned()
            .flatten();
    }
    PushPlanning::Planned(ExactPushPlan {
        remote: target.remote_name.clone(),
        destinations,
    })
}

fn reconcile_failed_push(
    cwd: &Path,
    planning: &PushPlanning,
    remote_contact: Option<RemoteContactEvidence>,
) -> Option<(OperationStatus, serde_json::Value)> {
    let PushPlanning::Planned(plan) = planning else {
        return planning.journal_value().map(|mut value| {
            value["evidence"] = json!({
                "classification": "unknown",
                "reason": "exact_push_plan_unavailable",
            });
            (OperationStatus::OutcomeUnknown, value)
        });
    };
    let Ok(repo) = crate::GitRepo::discover(cwd) else {
        let mut value = planning.journal_value().expect("planned push");
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "local_repository_evidence_unavailable",
        });
        return Some((OperationStatus::OutcomeUnknown, value));
    };
    let destination_refs = plan
        .destinations
        .iter()
        .map(|destination| destination.destination_ref.clone())
        .collect::<Vec<_>>();
    let Ok(observed) = repo.remote_ref_oids(&plan.remote, &destination_refs) else {
        let mut value = planning.journal_value().expect("planned push");
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "post_push_remote_evidence_unavailable",
        });
        return Some((OperationStatus::OutcomeUnknown, value));
    };

    let mut all_pre_push = true;
    let mut all_proposed = true;
    let mut every_observation_is_expected = true;
    let observations = plan
        .destinations
        .iter()
        .map(|destination| {
            let observed_sha = observed
                .get(&destination.destination_ref)
                .cloned()
                .flatten();
            all_pre_push &= observed_sha == destination.pre_push_sha;
            all_proposed &= observed_sha.as_deref() == Some(destination.proposed_sha.as_str());
            every_observation_is_expected &= observed_sha == destination.pre_push_sha
                || observed_sha.as_deref() == Some(destination.proposed_sha.as_str());
            json!({
                "destination_ref": destination.destination_ref,
                "observed_sha": observed_sha,
            })
        })
        .collect::<Vec<_>>();
    let (status, classification) = if all_proposed {
        (OperationStatus::Succeeded, "succeeded")
    } else if all_pre_push {
        (OperationStatus::Failed, "failed")
    } else if every_observation_is_expected {
        (OperationStatus::OutcomeUnknown, "partial")
    } else {
        (OperationStatus::OutcomeUnknown, "unknown")
    };
    let mut value = planning.journal_value().expect("planned push");
    value["evidence"] = json!({
        "classification": classification,
        "destinations": observations,
    });
    if let Some(remote_contact) = remote_contact {
        value["evidence"]["remote_contact"] = json!(remote_contact.remote_contact);
        value["evidence"]["remote_write_contact"] = json!(remote_contact.remote_write_contact);
        value["evidence"]["remote_not_contacted"] = json!(remote_contact.remote_not_contacted);
    }
    Some((status, value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RemoteContactEvidence {
    remote_contact: &'static str,
    remote_write_contact: &'static str,
    remote_not_contacted: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct GitTransferTrace {
    pre_push_hook: bool,
    pre_push_hook_failed: bool,
    remote_transport: bool,
}

impl GitTransferTrace {
    fn remote_contact(self) -> Option<RemoteContactEvidence> {
        if self.pre_push_hook_failed {
            Some(RemoteContactEvidence {
                remote_contact: if self.remote_transport {
                    "contacted"
                } else {
                    "not_contacted"
                },
                remote_write_contact: "not_contacted",
                remote_not_contacted: !self.remote_transport,
            })
        } else if self.remote_transport {
            Some(RemoteContactEvidence {
                remote_contact: "contacted",
                remote_write_contact: "unknown",
                remote_not_contacted: false,
            })
        } else {
            None
        }
    }
}

fn inspect_git_transfer_trace(path: &Path) -> GitTransferTrace {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return GitTransferTrace::default();
    };
    let mut pre_push_child_ids = BTreeSet::new();
    let mut trace = GitTransferTrace::default();
    for line in contents.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match event.get("event").and_then(serde_json::Value::as_str) {
            Some("child_start") => {
                let Some(arguments) = event
                    .get("argv")
                    .or_else(|| event.get("child").and_then(|child| child.get("argv")))
                else {
                    continue;
                };
                let Some(arguments) = arguments.as_array() else {
                    continue;
                };
                let command = arguments
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase();
                if command.contains("pre-push") {
                    trace.pre_push_hook = true;
                    if let Some(child_id) =
                        event.get("child_id").and_then(serde_json::Value::as_u64)
                    {
                        pre_push_child_ids.insert(child_id);
                    }
                }
                trace.remote_transport |= ["receive-pack", "upload-pack", "git-remote-", "ssh"]
                    .iter()
                    .any(|marker| command.contains(marker));
            }
            Some("child_exit")
                if event
                    .get("child_id")
                    .and_then(serde_json::Value::as_u64)
                    .is_some_and(|child_id| pre_push_child_ids.contains(&child_id))
                    && event
                        .get("code")
                        .and_then(serde_json::Value::as_i64)
                        .is_some_and(|code| code != 0) =>
            {
                trace.pre_push_hook_failed = true;
            }
            _ => {}
        }
    }
    trace
}

/// The `(command, action)` pair a `gh` invocation names.
///
/// Flags may precede the subcommand (`--repo o/n issue create`), and dropping
/// every `-` token would leave a flag's *value* looking positional. So match an
/// adjacent pair neither of whose halves is a flag, and require that the first
/// half is not itself the value of a preceding flag -- the same rule
/// [`crate::creates_pull_request`] applies to one hard-coded pair, generalised.
fn gh_subcommand(args: &[String]) -> Option<(&str, &str)> {
    args.windows(2).enumerate().find_map(|(index, pair)| {
        (!pair[0].starts_with('-')
            && !pair[1].starts_with('-')
            && (index == 0 || !args[index - 1].starts_with('-')))
        .then(|| (pair[0].as_str(), pair[1].as_str()))
    })
}

/// Every value a repeatable `gh` flag carries, in either spelling.
///
/// A spelling this does not recognise -- `pflag` also accepts the attached
/// shorthand `-lbug` -- yields no value, and every caller is written so that no
/// value means "behave as if this check did not exist". Missing a label is
/// then the status quo `gh` already reports; inventing one would refuse a
/// command that was going to work.
fn gh_flag_values<'a>(args: &'a [String], long: &str, short: Option<&str>) -> Vec<&'a str> {
    let long_assigned = format!("{long}=");
    let short_assigned = short.map(|short| format!("{short}="));
    let mut values = Vec::new();
    let mut pending = false;
    for arg in args {
        if pending {
            values.push(arg.as_str());
            pending = false;
        } else if arg == long || short.is_some_and(|short| arg == short) {
            pending = true;
        } else if let Some(value) = arg.strip_prefix(&long_assigned) {
            values.push(value);
        } else if let Some(value) = short_assigned
            .as_deref()
            .and_then(|prefix| arg.strip_prefix(prefix))
        {
            values.push(value);
        }
    }
    values
}

/// `gh` flags whose value must already name a label in the repository.
const GH_LABEL_FLAGS: &[(&str, Option<&str>)] = &[
    ("--label", Some("-l")),
    ("--add-label", None),
    ("--remove-label", None),
];

/// How many label names a refusal spells out before summarising the rest.
const LABEL_VOCABULARY_PREVIEW: usize = 40;

/// The labels a `gh` write asks GitHub to resolve by name.
///
/// `gh` resolves these against the repository *before* it creates anything and
/// refuses the whole command when one is unknown, so the vocabulary is a
/// precondition of the write rather than a part of it that could half-apply
/// (#184). Reads are excluded by the caller for a different reason: `--label`
/// on `issue list` is a filter, where an unknown name returns nothing instead
/// of failing.
///
/// One flag may carry several names -- `gh` parses these as comma-separated
/// lists -- and may be repeated.
fn gh_requested_labels(args: &[String]) -> Vec<String> {
    if !matches!(
        gh_subcommand(args),
        Some(("issue" | "pr", "create" | "edit"))
    ) {
        return Vec::new();
    }
    let mut labels: Vec<String> = Vec::new();
    for (long, short) in GH_LABEL_FLAGS {
        for value in gh_flag_values(args, long, *short) {
            for name in value.split(',') {
                let name = name.trim();
                if !name.is_empty() && !labels.iter().any(|seen| seen == name) {
                    labels.push(name.to_string());
                }
            }
        }
    }
    labels
}

/// The repository's label vocabulary, or `None` when it cannot be read.
///
/// Unreadable is not empty, and neither is a refusal. A machine that is
/// offline, unauthenticated or rate-limited may still be one where the write
/// itself would work, and turning that into "unknown label" would refuse valid
/// commands for a reason that has nothing to do with labels. The caller
/// degrades to the behaviour that existed before this check -- `gh` reports the
/// unknown name itself -- and the reconciliation below then says whether
/// anything was created.
fn github_label_vocabulary(repository: &str, cwd: &Path) -> Option<Vec<String>> {
    let output = provider_command(OperationProvider::Github)
        .args([
            "label", "list", "--repo", repository, "--limit", "500", "--json", "name",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let parsed: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    Some(
        parsed
            .as_array()?
            .iter()
            .filter_map(|label| label["name"].as_str().map(str::to_string))
            .collect(),
    )
}

/// Refuse a labelled write naming a label the repository does not define.
///
/// Refusing is the whole point: the caller runs this before anything is
/// journaled, queued or sent, so "was the issue created?" has exactly one
/// answer -- which the message states outright rather than leaving to be
/// inferred (#184).
fn refuse_undefined_labels(
    args: &[String],
    repository: &str,
    cwd: &Path,
) -> Result<(), BrokerOpError> {
    let requested = gh_requested_labels(args);
    if requested.is_empty() {
        return Ok(());
    }
    let Some(vocabulary) = github_label_vocabulary(repository, cwd) else {
        return Ok(());
    };
    // GitHub matches label names without regard to case, so refusing on case
    // alone would reject a name the command was going to apply.
    let unknown = requested
        .iter()
        .filter(|name| {
            !vocabulary
                .iter()
                .any(|known| known.eq_ignore_ascii_case(name))
        })
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>();
    if unknown.is_empty() {
        return Ok(());
    }
    let noun = if unknown.len() == 1 {
        "label"
    } else {
        "labels"
    };
    let unknown = unknown.join(", ");
    let mut defined = vocabulary;
    defined.sort();
    let remainder = defined.len().saturating_sub(LABEL_VOCABULARY_PREVIEW);
    let defined = if defined.is_empty() {
        "it defines none".to_string()
    } else {
        let listed = defined
            .iter()
            .take(LABEL_VOCABULARY_PREVIEW)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        match remainder {
            0 => listed,
            more => format!("{listed}, and {more} more"),
        }
    };
    Err(BrokerOpError::InvalidCoordinatedOperation {
        reason: format!(
            "{repository} does not define the {noun} {unknown}, and `gh` resolves labels before it \
             creates anything -- so nothing was sent and nothing was created. Labels defined \
             there: {defined}"
        ),
    })
}

/// How many entries a post-failure listing reads back.
const CREATE_OBSERVATION_LIMIT: usize = 50;

/// A `gh` command that would have GitHub assign a new number.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CreatePlanning {
    NotApplicable,
    Unplannable { reason: &'static str },
    Planned(CreatePlan),
}

/// What makes a created resource findable after a run that exited non-zero.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct CreatePlan {
    /// The `gh` subcommand, which is also the collection to list.
    collection: String,
    /// The listing field whose value names this particular create.
    identity_field: String,
    identity: String,
    /// The highest number the repository had already assigned.
    ///
    /// A create that lands takes a number strictly greater than every number
    /// the repository had, so this one integer separates the resource this run
    /// created from one that merely looks like it. `None` means the
    /// observation failed, which is what keeps such an outcome unknown.
    watermark: Option<i64>,
}

impl CreatePlanning {
    fn journal_value(&self) -> Option<serde_json::Value> {
        match self {
            Self::NotApplicable => None,
            Self::Unplannable { reason } => Some(json!({
                "planning": "unplannable",
                "reason": reason,
            })),
            Self::Planned(plan) => Some(json!({
                "planning": "planned",
                "plan": plan,
            })),
        }
    }
}

/// Plan how a failed `gh issue create` / `gh pr create` would be recognised.
///
/// Identity is the title for an issue and the head branch for a pull request.
/// Neither is unique on its own, which is why the number watermark taken in
/// [`observe_create_watermark`] is what makes the pair conclusive.
///
/// Reading the checkout's branch is not a guess: it is the same thing `gh pr
/// create` does with an omitted `--head`. A detached HEAD has no branch to
/// read, and says so rather than proposing the literal `HEAD`.
fn plan_github_create(args: &[String], cwd: &Path) -> CreatePlanning {
    match gh_subcommand(args) {
        Some(("issue", "create")) => match gh_flag_values(args, "--title", Some("-t")).first() {
            Some(title) => CreatePlanning::Planned(CreatePlan {
                collection: "issue".into(),
                identity_field: "title".into(),
                identity: (*title).to_string(),
                watermark: None,
            }),
            None => CreatePlanning::Unplannable {
                reason: "issue_create_without_an_explicit_title",
            },
        },
        Some(("pr", "create")) => {
            let head = gh_flag_values(args, "--head", Some("-H"))
                .first()
                .map(|head| (*head).to_string())
                .or_else(|| {
                    crate::GitRepo::discover(cwd)
                        .ok()?
                        .current_branch()
                        .ok()
                        .filter(|branch| branch != "HEAD")
                });
            match head {
                Some(head) => CreatePlanning::Planned(CreatePlan {
                    collection: "pr".into(),
                    identity_field: "headRefName".into(),
                    identity: head,
                    watermark: None,
                }),
                None => CreatePlanning::Unplannable {
                    reason: "pull_request_create_without_a_resolvable_head",
                },
            }
        }
        _ => CreatePlanning::NotApplicable,
    }
}

/// One page of `gh <collection> list`, newest first.
fn github_list_within(
    collection: &str,
    repository: &str,
    fields: &[&str],
    limit: usize,
    cwd: &Path,
    deadline: AdmissionDeadline,
    stage: &str,
) -> Result<Option<Vec<serde_json::Value>>, BrokerOpError> {
    let limit = limit.to_string();
    let fields = fields.join(",");
    let mut command = provider_command(OperationProvider::Github);
    command
        .args([
            collection, "list", "--repo", repository, "--state", "all", "--limit", &limit,
            "--json", &fields,
        ])
        .current_dir(cwd);
    let output = output_within(command, deadline, repository, stage, None)?;
    if !output.status.success() {
        return Ok(None);
    }
    let parsed: serde_json::Value = match serde_json::from_slice(&output.stdout) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(None),
    };
    Ok(parsed.as_array().cloned())
}

/// Record the number the repository stands at before a create runs.
///
/// Costs one listing, and only for a create whose result could be recognised
/// at all. Taking it afterwards would be worthless: the whole question is
/// which numbers are new.
fn observe_create_watermark(
    planning: &mut CreatePlanning,
    repository: &str,
    cwd: &Path,
    deadline: AdmissionDeadline,
) -> Result<(), BrokerOpError> {
    let CreatePlanning::Planned(plan) = planning else {
        return Ok(());
    };
    plan.watermark = github_list_within(
        &plan.collection,
        repository,
        &["number"],
        1,
        cwd,
        deadline,
        "observing GitHub create watermark",
    )?
    .map(|listed| {
        // A repository with nothing in the collection yet has no number, and 0
        // is below every number GitHub assigns.
        listed
            .first()
            .and_then(|entry| entry["number"].as_i64())
            .unwrap_or(0)
    });
    Ok(())
}

/// The URL `gh` prints for a resource it just created in this repository.
///
/// Scoped to the asserted repository, so an unrelated link in the output -- one
/// quoted by an error message, one in a template -- cannot be read as proof
/// that something was created.
fn created_resource_url(stdout: &str, repository: &str) -> Option<String> {
    let mut segments = repository.rsplit('/');
    let name = segments.next()?;
    let owner = segments.next()?;
    let needle = format!("/{owner}/{name}/").to_ascii_lowercase();
    stdout
        .split_whitespace()
        .find(|token| {
            let token = token.to_ascii_lowercase();
            token.starts_with("https://")
                && token.contains(&needle)
                && ["/issues/", "/pull/"].iter().any(|collection| {
                    token.rsplit_once(collection).is_some_and(|(_, number)| {
                        !number.is_empty() && number.chars().all(|digit| digit.is_ascii_digit())
                    })
                })
        })
        .map(str::to_string)
}

/// HTTP statuses with which GitHub rejects a request outright: the request was
/// refused as invalid, unauthorized, conflicting or aimed at nothing, so it was
/// not applied. 408 and 429 are absent on purpose (the request may be retried
/// or may not have been read), as is every 5xx, which says nothing about
/// whether the write landed.
const GITHUB_REJECTION_STATUSES: &[u16] = &[400, 401, 403, 404, 409, 410, 422];

/// Refusal messages `gh` prints when GitHub declines the command's one
/// mutation, or when `gh` itself declines before sending it. Each is tied to
/// the commands whose first and only mutation it can describe.
const GITHUB_REFUSAL_MESSAGES: &[(&str, &str, &str)] = &[
    (
        "pr",
        "update-branch",
        "Cannot update PR branch due to conflicts",
    ),
    ("pr", "merge", "is not mergeable"),
];

/// The GraphQL mutations each single-mutation command sends. `gh` reports a
/// resolver's refusal as `GraphQL: <message> (<mutation>)`; an error naming
/// the command's own mutation is GitHub declining that mutation, so nothing
/// was applied (#549: `Auto merge is not allowed for this repository
/// (enablePullRequestAutoMerge)` write-blocked the repository as unknown).
const GITHUB_GRAPHQL_MUTATIONS: &[(&str, &str, &[&str])] = &[
    (
        "pr",
        "merge",
        &[
            "mergePullRequest",
            "enablePullRequestAutoMerge",
            "disablePullRequestAutoMerge",
            "enqueuePullRequest",
        ],
    ),
    ("pr", "update-branch", &["updatePullRequestBranch"]),
];

/// GraphQL error text that says the server failed rather than refused: the
/// mutation may still have run, so it never counts as a refusal.
const GITHUB_GRAPHQL_AMBIGUOUS: &[&str] = &["Something went wrong", "timeout", "timed out"];

/// The refused mutation and `gh`'s line for it, when stderr carries a GraphQL
/// error naming one of `command action`'s own mutations.
fn github_graphql_refusal<'a>(
    command: &str,
    action: Option<&str>,
    stderr: &'a str,
) -> Option<(&'static str, &'a str)> {
    let (_, _, mutations) =
        GITHUB_GRAPHQL_MUTATIONS
            .iter()
            .find(|(refused_command, refused_action, _)| {
                command == *refused_command && action == Some(*refused_action)
            })?;
    stderr.lines().find_map(|line| {
        let line = line.trim();
        let message = line.split_once("GraphQL: ")?.1;
        if GITHUB_GRAPHQL_AMBIGUOUS.iter().any(|ambiguous| {
            message
                .to_ascii_lowercase()
                .contains(&ambiguous.to_ascii_lowercase())
        }) {
            return None;
        }
        mutations
            .iter()
            .find(|mutation| message.contains(&format!("({mutation})")))
            .map(|mutation| (*mutation, line))
    })
}

/// Every `HTTP <status>` token `gh` printed, in its two spellings:
/// `HTTP 422: Validation Failed (...)` and `gh: Not Found (HTTP 404)`.
fn github_http_statuses(stderr: &str) -> Vec<u16> {
    stderr
        .match_indices("HTTP ")
        .filter_map(|(index, _)| {
            let rest = &stderr[index + "HTTP ".len()..];
            let digits = rest.get(..3)?;
            let terminator = rest[3..].chars().next()?;
            (digits.bytes().all(|byte| byte.is_ascii_digit()) && matches!(terminator, ':' | ')'))
                .then(|| digits.parse().ok())
                .flatten()
        })
        .collect()
}

/// Evidence that GitHub definitively refused a coordinated write, so that it
/// had no effect and is safely `failed` rather than `outcome_unknown`.
///
/// Deliberately narrow. A failed write stays unknown unless all of these hold:
/// `gh` exited on its own with its generic error status (not a signal, a
/// crash, or a timeout, which the caller handles before this); the command is
/// one whose refusal can only describe its single mutation, so nothing was
/// applied before the refusal; and stderr carries an explicit server-side or
/// pre-flight rejection. A transport error, a 5xx, or any message not listed
/// here keeps the operation unknown, because a wrong "failed" is what makes a
/// blind retry look safe.
fn classify_github_refusal(
    args: &[String],
    exit_code: Option<i32>,
    stderr: &[u8],
) -> Option<serde_json::Value> {
    if exit_code != Some(1) {
        return None;
    }
    let stderr = String::from_utf8_lossy(stderr);
    let statuses = github_http_statuses(&stderr);
    if statuses.iter().any(|status| *status >= 500) {
        return None;
    }
    let command = args.first()?.as_str();
    let action = args.get(1).map(String::as_str);
    // `gh api` sends exactly one request unless it paginates; the other
    // commands here send one mutation.
    let single_request = match (command, action) {
        ("api", _) => !has_any(args, &["--paginate"]),
        ("pr", Some("update-branch" | "merge")) => true,
        _ => false,
    };
    if !single_request {
        return None;
    }
    if let Some(status) = statuses
        .iter()
        .find(|status| GITHUB_REJECTION_STATUSES.contains(status))
    {
        return Some(json!({
            "failure_class": "github_refused",
            "remote_outcome": "rejected",
            "evidence": { "http_status": status },
        }));
    }
    if let Some((mutation, line)) = github_graphql_refusal(command, action, &stderr) {
        return Some(json!({
            "failure_class": "github_refused",
            "remote_outcome": "rejected",
            "evidence": { "graphql_mutation": mutation, "message": line },
        }));
    }
    GITHUB_REFUSAL_MESSAGES
        .iter()
        .find(|(refused_command, refused_action, message)| {
            command == *refused_command
                && action == Some(*refused_action)
                && stderr.contains(message)
        })
        .map(|(_, _, message)| {
            json!({
                "failure_class": "github_refused",
                "remote_outcome": "rejected",
                "evidence": { "message": message },
            })
        })
}

/// Decide whether a failed `gh` create nevertheless created something.
///
/// The same shape as [`reconcile_failed_push`]: observe external state, then
/// classify, and record what was observed. Two independent kinds of evidence
/// answer it and the stronger one wins. `gh` prints the new resource's URL once
/// the API call has returned, so a URL on stdout is proof even when the process
/// then exits non-zero. When stdout is silent the repository itself is asked,
/// and the watermark taken before the run is what separates what this run
/// created from what was already there.
///
/// Every path that cannot see far enough records a named reason and stays
/// unknown rather than guessing, because it is a wrong "failed" that makes a
/// blind retry look safe (#184).
#[cfg(test)]
fn reconcile_failed_github_create(
    cwd: &Path,
    repository: &str,
    planning: &CreatePlanning,
    stdout: &[u8],
) -> Option<(OperationStatus, serde_json::Value)> {
    reconcile_failed_github_create_with_deadline(
        cwd,
        repository,
        planning,
        stdout,
        AdmissionDeadline::start(QueueWait::Forever),
    )
    .ok()
    .flatten()
}

fn reconcile_failed_github_create_with_deadline(
    cwd: &Path,
    repository: &str,
    planning: &CreatePlanning,
    stdout: &[u8],
    deadline: AdmissionDeadline,
) -> Result<Option<(OperationStatus, serde_json::Value)>, BrokerOpError> {
    let CreatePlanning::Planned(plan) = planning else {
        return Ok(planning.journal_value().map(|mut value| {
            value["evidence"] = json!({
                "classification": "unknown",
                "reason": "create_plan_unavailable",
            });
            (OperationStatus::OutcomeUnknown, value)
        }));
    };
    let mut value = planning.journal_value().expect("planned create");
    if let Some(created) = created_resource_url(&String::from_utf8_lossy(stdout), repository) {
        value["evidence"] = json!({
            "classification": "succeeded",
            "source": "command_output",
            "created": created,
        });
        return Ok(Some((OperationStatus::Succeeded, value)));
    }
    let Some(watermark) = plan.watermark else {
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "pre_create_number_watermark_unavailable",
        });
        return Ok(Some((OperationStatus::OutcomeUnknown, value)));
    };
    let Some(listed) = github_list_within(
        &plan.collection,
        repository,
        &["number", "url", plan.identity_field.as_str()],
        CREATE_OBSERVATION_LIMIT,
        cwd,
        deadline,
        "observing GitHub create after failure",
    )?
    else {
        value["evidence"] = json!({
            "classification": "unknown",
            "reason": "post_create_repository_evidence_unavailable",
        });
        return Ok(Some((OperationStatus::OutcomeUnknown, value)));
    };
    let (status, evidence) = classify_create_observation(plan, watermark, &listed);
    value["evidence"] = evidence;
    Ok(Some((status, value)))
}

/// Read a listing of the collection, newest first, for the planned create.
///
/// Kept apart from the `gh` call so the question it answers -- what does this
/// listing prove? -- can be asked of any listing, including the ones a test
/// writes by hand.
fn classify_create_observation(
    plan: &CreatePlan,
    watermark: i64,
    listed: &[serde_json::Value],
) -> (OperationStatus, serde_json::Value) {
    let identity_field = plan.identity_field.as_str();
    if let Some(created) = listed.iter().find(|entry| {
        entry["number"]
            .as_i64()
            .is_some_and(|number| number > watermark)
            && entry[identity_field].as_str() == Some(plan.identity.as_str())
    }) {
        return (
            OperationStatus::Succeeded,
            json!({
                "classification": "succeeded",
                "source": "post_create_observation",
                "created": created["url"],
                "number": created["number"],
            }),
        );
    }
    // The listing is newest first, so its last entry is the oldest it reached.
    // A full page that never got back to the watermark leaves a gap the create
    // could be hiding in, and "absent from the page" is then not "not created".
    let reached_watermark = listed.len() < CREATE_OBSERVATION_LIMIT
        || listed
            .last()
            .and_then(|entry| entry["number"].as_i64())
            .is_some_and(|oldest| oldest <= watermark);
    if !reached_watermark {
        return (
            OperationStatus::OutcomeUnknown,
            json!({
                "classification": "unknown",
                "reason": "post_create_listing_did_not_reach_the_watermark",
                "watermark": watermark,
            }),
        );
    }
    (
        OperationStatus::Failed,
        json!({
            "classification": "failed",
            "source": "post_create_observation",
            "watermark": watermark,
        }),
    )
}

/// How a provider's CLI is spelled on disk. Not `OperationProvider::as_str`,
/// which is the wire spelling stored in the journal: that says `github` where
/// the binary is `gh`.
fn provider_executable(provider: OperationProvider) -> &'static str {
    match provider {
        OperationProvider::Git => "git",
        OperationProvider::Github => "gh",
    }
}

/// The binary that performs a coordinated operation.
///
/// The git arm is [`crate::git::git_command`] -- the probed binary -- and
/// never `Command::new("git")`. Until #179's review this was the one spawn in
/// the crate that still resolved its own executable through PATH, and it was
/// invisible to any audit grepping for `Command::new("git")` because the
/// literal had been factored into a `match` on the provider. Extracting it
/// here is half the fix: the choice now has a name, a doc comment, and a test.
///
/// Sharing the *name* with the pre-push dry run is not enough. On a machine
/// whose wrapper the probe rejects, the check would run the trusted binary and
/// the mutation a different one -- verifying one command and performing
/// another, which is the shape of every defect #176 and #178 were about -- and
/// the `--no-verify` this function's caller appends then removes the pre-push
/// hook that was the last thing able to notice.
pub(super) fn provider_command(provider: OperationProvider) -> Command {
    match provider {
        OperationProvider::Git => crate::git::git_command(),
        OperationProvider::Github => github_command(),
    }
}

/// Variables a coordinated `gh` (and any `git` it starts) may inherit: auth,
/// locale, proxies and certificates. Everything else is dropped, because gh
/// and git turn variables into commands (`GH_BROWSER`, `EDITOR`, `PAGER`,
/// `GIT_SSH_COMMAND`, `GIT_CONFIG_*` ...) or into another target (`GH_HOST`,
/// `GH_CONFIG_DIR`, `GIT_DIR` ...), either of which bypasses the branch guard
/// (#393). `PATH` and `HOME` are set, not inherited. `AETHYME_*` is the
/// broker's own namespace.
const GH_INHERITED_ENV: &[&str] = &[
    "USER",
    "LOGNAME",
    "TMPDIR",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "HTTPS_PROXY",
    "https_proxy",
    "HTTP_PROXY",
    "http_proxy",
    "NO_PROXY",
    "no_proxy",
    "ALL_PROXY",
    "all_proxy",
];

/// The only directories gh, and the git gh starts, are taken from. The
/// caller's `PATH` never chooses either binary (#393).
const TRUSTED_TOOL_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin", "/bin"];

static GH_PROGRAM: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Where gh is looked up. Release builds use only [`TRUSTED_TOOL_DIRS`].
/// Debug builds, which only the test suite runs, also search `PATH` first,
/// so a fixture can stand in a fake gh; installed binaries are release builds.
fn gh_search_dirs(include_path: bool) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if include_path && let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.extend(TRUSTED_TOOL_DIRS.iter().map(PathBuf::from));
    dirs
}

/// The first `name` in `dirs` that canonicalizes to a regular executable
/// file owned by root or the current user and writable by neither group nor
/// others.
fn trusted_tool(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
    // SAFETY: getuid has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    dirs.iter().find_map(|dir| {
        let resolved = dir.join(name).canonicalize().ok()?;
        let metadata = std::fs::metadata(&resolved).ok()?;
        let mode = metadata.permissions().mode();
        (metadata.is_file()
            && mode & 0o111 != 0
            && mode & 0o022 == 0
            && (metadata.uid() == 0 || metadata.uid() == uid))
            .then_some(resolved)
    })
}

/// The gh binary, resolved once from [`gh_search_dirs`].
fn gh_program() -> Option<&'static PathBuf> {
    GH_PROGRAM
        .get_or_init(|| trusted_tool("gh", &gh_search_dirs(cfg!(debug_assertions))))
        .as_ref()
}

/// The invoking user's home directory from the passwd database, not `HOME`,
/// so a relocated `HOME` cannot supply gh or git configuration.
fn passwd_home() -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: `passwd` is a plain C struct of pointers and integers, for which
    // all-zero bytes are a valid (empty) value; getpwuid_r overwrites it.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `getpwuid_r` writes only into `entry` and `buffer`, both owned
    // here and sized as passed; `result` is null or points at `entry`.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &mut result,
        )
    };
    if status != 0 || result.is_null() || entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: `pw_dir` points into `buffer`, NUL-terminated by getpwuid_r.
    let dir = unsafe { std::ffi::CStr::from_ptr(entry.pw_dir) };
    Some(PathBuf::from(std::ffi::OsStr::from_bytes(dir.to_bytes())))
}

pub(crate) fn github_command() -> Command {
    let program = gh_program()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("/nonexistent/gh"));
    let mut command = Command::new(&program);
    command.env_clear();
    for (key, value) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if GH_INHERITED_ENV.contains(&name.as_ref()) || name.starts_with("AETHYME_") {
            command.env(&key, &value);
        }
    }
    // gh finds git, and anything else it starts, only in the trusted
    // directories.
    command.env("PATH", TRUSTED_TOOL_DIRS.join(":"));
    if let Some(home) = passwd_home() {
        command.env("HOME", home);
    }
    // gh's settings may name a browser, editor or pager command; the
    // environment overrides them. The git gh starts reads no system or
    // global configuration, and the repository's own cannot name a hook
    // directory, an ssh command or an fsmonitor to run.
    command
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_PAGER", "cat")
        .env("GH_BROWSER", "false")
        .env("GH_EDITOR", "false")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        // An empty `credential.helper` resets the helper list, so a helper
        // the repository's config names never runs. `include.path` cannot be
        // switched off this way; it is a known limit.
        .env("GIT_CONFIG_COUNT", "5")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", "/dev/null")
        .env("GIT_CONFIG_KEY_1", "core.sshCommand")
        .env("GIT_CONFIG_VALUE_1", "ssh")
        .env("GIT_CONFIG_KEY_2", "core.fsmonitor")
        .env("GIT_CONFIG_VALUE_2", "false")
        .env("GIT_CONFIG_KEY_3", "credential.helper")
        .env("GIT_CONFIG_VALUE_3", "")
        .env("GIT_CONFIG_KEY_4", "protocol.file.allow")
        .env("GIT_CONFIG_VALUE_4", "never");
    command
}

/// Do not let inherited command-scope Git config change what a coordinated
/// pre-push hook check verifies relative to the real push.
fn remove_inherited_git_config_overrides(command: &mut Command) {
    command
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT");
}

fn redacted_command(provider: OperationProvider, args: &[String]) -> Result<String, BrokerOpError> {
    let sensitive_flags = [
        "-m",
        "--message",
        "--body",
        "--body-file",
        "--title",
        "--notes",
        "--notes-file",
        "--description",
        "--token",
        "--password",
        "--client-secret",
        "--value",
        "-f",
        "-F",
        "--field",
        "--raw-field",
        "--input",
    ];
    let mut redacted = vec![provider_executable(provider).to_string()];
    let mut hide_next = false;
    let mut pending: Option<fn(&str) -> String> = None;
    for arg in args {
        if hide_next {
            redacted.push("[REDACTED]".into());
            hide_next = false;
            continue;
        }
        if let Some(redact) = pending.take() {
            redacted.push(redact(arg));
            continue;
        }
        if provider == OperationProvider::Git && (arg == "-c" || arg == "--config-env") {
            redacted.push(arg.clone());
            pending = Some(redact_config_assignment);
        } else if provider == OperationProvider::Git && arg.starts_with("--config-env=") {
            let assignment = &arg["--config-env=".len()..];
            redacted.push(format!(
                "--config-env={}",
                redact_config_assignment(assignment)
            ));
        } else if provider == OperationProvider::Git && arg.starts_with("-c") {
            redacted.push(format!("-c{}", redact_config_assignment(&arg[2..])));
        } else if provider == OperationProvider::Github && (arg == "-H" || arg == "--header") {
            redacted.push(arg.clone());
            pending = Some(redact_header);
        } else if provider == OperationProvider::Github && arg.starts_with("--header=") {
            redacted.push(format!(
                "--header={}",
                redact_header(&arg["--header=".len()..])
            ));
        } else if provider == OperationProvider::Github && arg.starts_with("-H") {
            redacted.push(format!("-H{}", redact_header(&arg[2..])));
        } else if sensitive_flags.contains(&arg.as_str()) {
            redacted.push(arg.clone());
            hide_next = true;
        } else if sensitive_flags
            .iter()
            .any(|flag| arg.starts_with(&format!("{flag}=")))
        {
            let flag = arg.split('=').next().unwrap_or(arg);
            redacted.push(format!("{flag}=[REDACTED]"));
        } else if arg.contains("://") && arg.contains('@') {
            redacted.push("[REDACTED_URL]".into());
        } else {
            redacted.push(arg.clone());
        }
    }
    Ok(serde_json::to_string(&redacted)?)
}

/// Redact the value of a Git `key=value` configuration assignment whose key
/// can carry a credential: any `*extraheader` (`http.extraHeader`,
/// `http.<url>.extraHeader`), or a key naming an authorization, token, or
/// password. The key survives so the journal still shows which setting the
/// command overrode.
fn redact_config_assignment(assignment: &str) -> String {
    let Some((key, _value)) = assignment.split_once('=') else {
        return assignment.to_string();
    };
    let lowered = key.to_ascii_lowercase();
    let sensitive = ["extraheader", "authorization", "token", "password"]
        .iter()
        .any(|needle| lowered.contains(needle));
    if sensitive {
        format!("{key}=[REDACTED]")
    } else {
        assignment.to_string()
    }
}

/// Redact the value of a `gh api -H "Name: value"` header that can carry a
/// credential: `Authorization`, `Proxy-Authorization`, or any header whose
/// name contains `token`. The header name survives.
fn redact_header(header: &str) -> String {
    let Some((name, _value)) = header.split_once(':') else {
        return header.to_string();
    };
    let lowered = name.trim().to_ascii_lowercase();
    let sensitive =
        lowered == "authorization" || lowered == "proxy-authorization" || lowered.contains("token");
    if sensitive {
        format!("{name}: [REDACTED]")
    } else {
        header.to_string()
    }
}

pub(super) fn is_github_pull_request_merge(args: &[String]) -> bool {
    args.first().map(String::as_str) == Some("pr")
        && args.get(1).map(String::as_str) == Some("merge")
}

fn tracking_upstream_parts(upstream: &str) -> Option<(&str, &str)> {
    upstream
        .strip_prefix("refs/remotes/")
        .unwrap_or(upstream)
        .split_once('/')
        .filter(|(remote, branch)| !remote.is_empty() && !branch.is_empty())
}

fn deferred_post_merge_cleanup(
    upstream_ref: Option<String>,
    fetch_operation_id: Option<i64>,
    explanation: &str,
    next_action: Option<String>,
) -> PostMergeCleanupReport {
    PostMergeCleanupReport {
        state: PostMergeCleanupState::Deferred,
        upstream_ref,
        fetch_operation_id,
        cleanup: None,
        explanation: explanation.into(),
        next_action,
    }
}

impl Broker {
    /// Return a bounded, read-only coordination measurement snapshot. The
    /// lock policy intentionally stays unchanged until this evidence shows
    /// that repository-wide contention remains material after local hooks are
    /// moved outside the lock.
    pub fn coordinated_operation_stats(
        &mut self,
        repository: Option<&str>,
        limit: u32,
    ) -> Result<crate::OperationStats, BrokerOpError> {
        Ok(crate::operation_stats::from_store(
            self.store(),
            repository,
            limit,
        )?)
    }

    pub fn show_coordinated_operation(
        &mut self,
        operation_id: i64,
    ) -> Result<OperationShowReport, BrokerOpError> {
        let operation = self.store().coordinated_operation(operation_id)?.ok_or(
            crate::BrokerError::CoordinatedOperationNotFound(operation_id),
        )?;
        Ok(OperationShowReport::from_operation(operation))
    }

    pub fn run_coordinated_operation(
        &mut self,
        request: CoordinatedCommand,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        self.run_coordinated_operation_with_wait(request, QueueWait::Forever)
    }

    /// As [`Self::run_coordinated_operation`], but bounding how long the caller
    /// is willing to queue for the repository write lock.
    pub fn run_coordinated_operation_with_wait(
        &mut self,
        request: CoordinatedCommand,
        queue_wait: QueueWait,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        let session = self.store().session(request.session_id)?;
        if session.status.is_closed() {
            return Err(BrokerOpError::ClosedSessionOperation {
                session_id: session.id,
                repository_root: self.main_root().display().to_string(),
            });
        }
        if let Some(requested) = request.repository.as_deref() {
            refuse_session_repository_mismatch(
                session.id,
                Path::new(&session.worktree_path),
                requested,
            )?;
        }
        let should_cleanup_after_merge = request.provider == OperationProvider::Github
            && is_github_pull_request_merge(&request.args);
        let session_id = request.session_id;
        let repository = request.repository.clone();
        let mut report = self.run_coordinated_operation_at_with_hooks(
            request,
            Path::new(&session.worktree_path),
            queue_wait,
            || Ok(()),
            |_, _| Ok(None),
        )?;
        if report.ok() && should_cleanup_after_merge {
            report.post_merge_cleanup = Some(
                self.cleanup_after_github_pull_request_merge(session_id, repository.as_deref()),
            );
            // The cleanup above fetches the upstream first, so the default
            // branch now carries the merge and the landing is findable. A
            // squash rewrites the SHA, so without this the session's own work
            // would look unsubmitted forever (#152).
            report.representing_commit = self.note_merge_time_representation(session_id, None);
        }
        Ok(report)
    }

    fn cleanup_after_github_pull_request_merge(
        &mut self,
        session_id: i64,
        repository: Option<&str>,
    ) -> PostMergeCleanupReport {
        let Some(repository) = repository else {
            return deferred_post_merge_cleanup(
                None,
                None,
                "the successful GitHub merge had no canonical repository assertion",
                None,
            );
        };
        let Some((upstream_ref, _)) = self.repo_handle().tracking_upstream() else {
            return deferred_post_merge_cleanup(
                None,
                None,
                "the primary branch has no configured fetched upstream",
                None,
            );
        };
        let Some((remote, branch)) = tracking_upstream_parts(&upstream_ref) else {
            return deferred_post_merge_cleanup(
                Some(upstream_ref.clone()),
                None,
                "the configured upstream is not a remote-tracking branch",
                Some(format!(
                    "aethyme broker advanced integration reconcile --upstream {upstream_ref} --dry-run"
                )),
            );
        };
        let destination = format!("refs/remotes/{remote}/{branch}");
        let source = format!("refs/heads/{branch}");
        let fetch = self.run_coordinated_operation(CoordinatedCommand {
            session_id,
            provider: OperationProvider::Git,
            repository: Some(repository.to_string()),
            resolved_target: None,
            scope: Some(format!("ref:{destination}")),
            declared_effect: None,
            destructive_confirmed: false,
            cross_session: None,
            ref_write_acknowledged: false,
            authorization_reason: Some(
                "refresh the tracked target after an authorized pull-request merge".into(),
            ),
            args: vec![
                "fetch".into(),
                remote.to_string(),
                format!("{source}:{destination}"),
            ],
        });
        let fetch = match fetch {
            Ok(fetch) if fetch.ok() => fetch,
            Ok(fetch) => {
                return deferred_post_merge_cleanup(
                    Some(upstream_ref),
                    Some(fetch.operation.id),
                    "the pull request merged, but refreshing its target branch did not complete successfully",
                    Some(format!(
                        "aethyme broker advanced operations show {}",
                        fetch.operation.id
                    )),
                );
            }
            Err(error) => {
                return deferred_post_merge_cleanup(
                    Some(upstream_ref.clone()),
                    None,
                    &format!(
                        "the pull request merged, but the tracked target could not be refreshed: {error}"
                    ),
                    Some(format!(
                        "aethyme broker advanced integration reconcile --upstream {upstream_ref} --dry-run"
                    )),
                );
            }
        };

        match self.auto_cleanup_landed_integration(&upstream_ref) {
            Ok(cleanup) => {
                let state = match cleanup.state {
                    crate::AutomaticIntegrationCleanupState::Cleaned => {
                        PostMergeCleanupState::Cleaned
                    }
                    crate::AutomaticIntegrationCleanupState::NotNeeded => {
                        PostMergeCleanupState::NotNeeded
                    }
                    crate::AutomaticIntegrationCleanupState::Deferred => {
                        PostMergeCleanupState::Deferred
                    }
                };
                PostMergeCleanupReport {
                    state,
                    upstream_ref: Some(upstream_ref),
                    fetch_operation_id: Some(fetch.operation.id),
                    explanation: cleanup.explanation.clone(),
                    next_action: cleanup.next_action.clone(),
                    cleanup: Some(cleanup),
                }
            }
            Err(error) => deferred_post_merge_cleanup(
                Some(upstream_ref.clone()),
                Some(fetch.operation.id),
                &format!(
                    "the pull request merged and upstream refreshed, but automatic cleanup was refused: {error}"
                ),
                Some(format!(
                    "aethyme broker advanced integration reconcile --upstream {upstream_ref} --dry-run"
                )),
            ),
        }
    }

    /// A record created before queueing must not linger as prepared when the
    /// operation never started. Best-effort: the caller is already returning the
    /// real failure, and the liveness-aware sweep is the backstop.
    fn resolve_unstarted_operation(&mut self, id: i64, reason: &str) {
        self.resolve_unstarted_operation_with_details(id, json!({ "reason": reason }));
    }

    fn resolve_unstarted_operation_with_details(&mut self, id: i64, details: serde_json::Value) {
        let details = details.to_string();
        crate::warn_unrecorded(
            "mark an unstarted coordinated operation failed",
            self.store().transition_coordinated_operation(
                id,
                OperationStatus::Failed,
                None,
                Some(&details),
            ),
        );
    }

    /// Reap prepared write rows whose client process is gone. This runs from
    /// broker open so a dead client is resolved by an independent invocation,
    /// not only when another write happens to reach the same lock.
    pub(crate) fn reap_abandoned_prepared_operations(&mut self) -> Result<usize, BrokerOpError> {
        let pending = self.store().pending_coordinated_operations()?;
        let mut reaped = 0;
        for operation in pending {
            if operation.status != OperationStatus::Prepared || !process_is_gone(operation.pid) {
                continue;
            }
            let mut details = operation
                .details_json
                .as_deref()
                .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
                .filter(serde_json::Value::is_object)
                .unwrap_or_else(|| json!({}));
            details["reason"] = json!("abandoned_before_start");
            details["reaped_by"] = json!("broker_open");
            self.store().transition_coordinated_operation(
                operation.id,
                OperationStatus::Failed,
                None,
                Some(&details.to_string()),
            )?;
            reaped += 1;
        }
        Ok(reaped)
    }

    pub(crate) fn run_coordinated_operation_at(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        self.run_coordinated_operation_at_with_wait(request, cwd, QueueWait::Forever)
    }

    /// As [`Self::run_coordinated_operation_at`], but bound the complete
    /// operation admission and child process to the caller's wait budget.
    ///
    /// The original helper intentionally keeps the compatibility API's
    /// unbounded behavior. Delivery callers use this form because a remote
    /// fetch, push, or provider query must not leave the broker holding a
    /// repository lane forever when the network stops responding.
    pub(crate) fn run_coordinated_operation_at_with_wait(
        &mut self,
        request: CoordinatedCommand,
        cwd: &Path,
        queue_wait: QueueWait,
    ) -> Result<CoordinatedOperationReport, BrokerOpError> {
        self.run_coordinated_operation_at_with_hooks(
            request,
            cwd,
            queue_wait,
            || Ok(()),
            |_, _| Ok(None),
        )
    }

    /// Decide a gh command's branch-ref write before anything is journaled
    /// (#393): the branches it deletes, and the exact argv to run when the
    /// check binds it (`--match-head-commit` injected for `pr merge -d`).
    /// Every doubt refuses; only an operator's `--destructive
    /// --ref-write-acknowledged` lets an unverifiable command through.
    fn check_gh_ref_write(
        &mut self,
        request: &CoordinatedCommand,
        cwd: &Path,
        github_target: Option<&crate::ResolvedGithubTarget>,
    ) -> Result<(GhRefTargets, Option<Vec<String>>), BrokerOpError> {
        use crate::gh_ref_guard::Verdict;
        let refuse = |why: String| {
            Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "refusing a gh command that may write a branch ref: {why}. Nothing was run. \
                     Use the exact allowlisted `pr merge` or `api -X DELETE` form. An \
                     unverifiable write can be acknowledged with --destructive \
                     --ref-write-acknowledged only when no shared branch can be involved and the \
                     operator confirmed it touches no other live session's branch"
                ),
            })
        };
        let slug = github_target.map_or("", |target| target.display_slug.as_str());
        let verdict = crate::gh_ref_guard::assess(&request.args, slug);
        let (number, match_head_commit) = match verdict {
            Verdict::NoRefWrite => return Ok((GhRefTargets::default(), None)),
            Verdict::Unverifiable(why)
                if request.ref_write_acknowledged && request.destructive_confirmed =>
            {
                let shared = self.shared_branches();
                if shared.is_empty() {
                    return Ok((GhRefTargets::default(), None));
                }
                return refuse(format!(
                    "{why}; the broker cannot establish that this write avoids shared branch(es): {}",
                    shared.join(", ")
                ));
            }
            Verdict::Unverifiable(why) => return refuse(why),
            // Files land in the working directory, which must be the session
            // worktree's root, never inside a `.git` directory.
            Verdict::DownloadHere => {
                return match self.store().session(request.session_id) {
                    Ok(session) if same_directory(cwd, Path::new(&session.worktree_path)) => {
                        Ok((GhRefTargets::default(), None))
                    }
                    _ => refuse(format!(
                        "a download must run from session {}'s worktree root, not {}",
                        request.session_id,
                        cwd.display()
                    )),
                };
            }
            // It cannot touch a ref, but only in the session's own repository.
            Verdict::SafeWrite => {
                return match verify_github_origin(cwd, github_target) {
                    Ok(_) => Ok((GhRefTargets::default(), None)),
                    Err(why) => refuse(why),
                };
            }
            Verdict::DeleteBranch(branch) => {
                return match verify_github_origin(cwd, github_target) {
                    Ok(_) => Ok((GhRefTargets::rewrite(vec![branch]), None)),
                    Err(why) => refuse(why),
                };
            }
            // Setting a base advances nothing yet, but names the branch a
            // later merge will advance.
            Verdict::PrBase(branch) => {
                return match verify_github_origin(cwd, github_target) {
                    Ok(_) => Ok((GhRefTargets::advance(vec![branch]), None)),
                    Err(why) => refuse(why),
                };
            }
            // Merging the base into the head writes the head branch.
            Verdict::PrUpdateBranch(number) => {
                let target = match verify_github_origin(cwd, github_target) {
                    Ok(target) => target,
                    Err(why) => return refuse(why),
                };
                return match gh_pr_refs(&number, cwd, target) {
                    Ok(refs) => Ok((GhRefTargets::rewrite(vec![refs.head]), None)),
                    Err(why) => refuse(why),
                };
            }
            // Every merge advances its base and may delete its head, with
            // `-d` or the repository's delete-on-merge setting, so both are
            // checked and the head is bound to the commit read here.
            Verdict::PrMerge {
                number,
                match_head_commit,
                ..
            } => (number, match_head_commit),
        };
        let target = match verify_github_origin(cwd, github_target) {
            Ok(target) => target,
            Err(why) => return refuse(why),
        };
        let refs = match gh_pr_refs(&number, cwd, target) {
            Ok(refs) => refs,
            Err(why) => return refuse(why),
        };
        self.advise_pr_merge_graph_integrity(request.session_id, cwd, &number, &refs.head_oid);
        // The head may be deleted (rewrite); the base is advanced.
        let targets = GhRefTargets {
            rewrite: vec![refs.head],
            advance: vec![refs.base],
        };
        match match_head_commit {
            Some(sha) if sha != refs.head_oid => refuse(format!(
                "--match-head-commit {sha} is not pull request #{number}'s head {}",
                refs.head_oid
            )),
            Some(_) => Ok((targets, None)),
            None => {
                let mut args = request.args.clone();
                args.extend(["--match-head-commit".to_string(), refs.head_oid]);
                Ok((targets, Some(args)))
            }
        }
    }

    /// Graph integrity is advice on the pull-request lane too (#280, #292):
    /// check the head tree the merge lands, record the verdict, and print
    /// its advice. Nothing here can refuse or delay the merge beyond the check
    /// itself; the head is already bound by `--match-head-commit`.
    fn advise_pr_merge_graph_integrity(
        &mut self,
        session_id: i64,
        cwd: &Path,
        number: &str,
        head_oid: &str,
    ) {
        let Ok(checkout) = crate::GitRepo::discover(cwd) else {
            return;
        };
        let main_root = self.main_root_path();
        let Some(outcome) =
            crate::graph_integrity::verify_commit_for_advice(&main_root, &checkout, head_oid)
        else {
            return;
        };
        let recorded = self.store().append_event(
            crate::events::GRAPH_INTEGRITY_CHECKED,
            Some(session_id),
            Some(&crate::events::graph_integrity_checked_payload(&outcome)),
        );
        if let Err(error) = recorded {
            eprintln!("[graph-integrity] could not record the verdict: {error}");
        }
        match outcome.advice() {
            Some(advice) => eprintln!("[graph-integrity] pull request #{number}: {advice}"),
            None => eprintln!(
                "[graph-integrity] pull request #{number}: committed graph is fresh for head {}",
                head_oid.get(..12).unwrap_or(head_oid)
            ),
        }
    }

    /// Branches no session can own and no broker operation may delete or
    /// rewrite: the integration branch, and the default branch as
    /// `origin/HEAD` (or the main checkout's upstream) names it. Exact names
    /// only, resolved from the repository, never from a session record.
    fn shared_branches(&self) -> Vec<String> {
        let mut shared = vec![crate::merge::PromoteConfig::load(&self.main_root_path()).branch];
        match self
            .repo_handle()
            .upstream_default()
            .and_then(|(upstream, _)| {
                upstream
                    .split_once('/')
                    .map(|(_, branch)| branch.to_string())
            }) {
            Some(branch) => shared.push(branch),
            // Unknown default branch: protect both conventional names rather
            // than none. This only ever refuses more; it lets nothing through.
            None => shared.extend(["main".to_string(), "master".to_string()]),
        }
        shared
    }

    /// Refuse a destructive operation on a branch that belongs to another
    /// live session, unless `cross_session` names exactly that session.
    /// Returns the session the caller was allowed to cross into.
    fn refuse_foreign_session_branches(
        &mut self,
        request: &CoordinatedCommand,
        effect: OperationEffect,
        gh_targets: GhRefTargets,
    ) -> Result<Option<i64>, BrokerOpError> {
        let parsed_push = if request.provider == OperationProvider::Git {
            match git_push_branch_targets(&request.args) {
                Ok(parsed) => parsed,
                Err(why) => {
                    return Err(BrokerOpError::InvalidCoordinatedOperation {
                        reason: format!(
                            "cannot safely classify this git push's ref targets ({why}); it may \
                             rewrite a shared or session-owned branch. Nothing was run."
                        ),
                    });
                }
            }
        } else {
            None
        };
        let has_git_branch_targets = parsed_push
            .as_ref()
            .is_some_and(|targets| !targets.all().is_empty());
        if effect != OperationEffect::Destructive
            && gh_targets.is_empty()
            && !has_git_branch_targets
        {
            return match request.cross_session {
                Some(_) => Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "--cross-session applies only to a destructive operation".into(),
                }),
                None => Ok(None),
            };
        }
        // Branches this command deletes or rewrites, and branches it only
        // advances (a merge's base).
        // Git: every destination is attributed, but only a deleted or forced
        // one is a rewrite; a plain fast-forward push to main is an advance.
        let git_targets = match request.provider {
            OperationProvider::Git => parsed_push.as_ref().map_or_else(
                || destructive_branch_targets(request.provider, &request.args),
                GitPushBranchTargets::all,
            ),
            OperationProvider::Github => Vec::new(),
        };
        let mut rewrites = match request.provider {
            OperationProvider::Git => parsed_push.as_ref().map_or_else(
                || git_rewritten_branches(&request.args),
                |targets| targets.rewrite.clone(),
            ),
            OperationProvider::Github => Vec::new(),
        };
        let mut advances: Vec<String> = git_targets
            .into_iter()
            .filter(|target| !rewrites.contains(target))
            .collect();
        rewrites.extend(gh_targets.rewrite);
        advances.extend(gh_targets.advance);
        // The default and integration branches are shared: never a session's
        // own, and never deleted or rewritten through the broker. Matching is
        // by exact branch name against the repository's own resolution, never
        // against a session record. Unresolvable means no exemption.
        let shared = self.shared_branches();
        if let Some(protected) = rewrites.iter().find(|target| shared.contains(*target)) {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "refusing to delete or rewrite {protected}: it is the repository's shared \
                     default or integration branch, which no broker operation may delete, \
                     force-update or rebase. Nothing was run."
                ),
            });
        }
        // Advancing a shared branch (merging into main) is ordinary; it is
        // only never attributed to a session that happens to record it.
        let targets: Vec<String> = rewrites
            .into_iter()
            .chain(
                advances
                    .into_iter()
                    .filter(|target| !shared.contains(target)),
            )
            .collect();
        let owners: Vec<crate::Session> = if targets.is_empty() {
            Vec::new()
        } else {
            self.store()
                .live_sessions()?
                .into_iter()
                .filter(|session| {
                    session.id != request.session_id
                        && !session.status.is_closed()
                        && targets.contains(&session.branch)
                })
                .collect()
        };
        match (owners.as_slice(), request.cross_session) {
            ([], None) => Ok(None),
            ([], Some(id)) => Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "--cross-session {id} names no live session whose branch this command deletes or rewrites"
                ),
            }),
            ([owner], Some(id)) if owner.id == id => Ok(Some(id)),
            (owners, _) => {
                let owner = owners
                    .iter()
                    .find(|owner| Some(owner.id) != request.cross_session)
                    .unwrap_or(&owners[0]);
                Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: format!(
                        "refusing a destructive operation from session {}: branch {} belongs to live session {} ({:?}). \
                         Run it from session {}, or, with the operator's confirmation, add --cross-session {}",
                        request.session_id, owner.branch, owner.id, owner.task, owner.id, owner.id
                    ),
                })
            }
        }
    }

    /// Execute through the normal coordinated-operation state machine while
    /// allowing a caller to revalidate local state under the repository lock,
    /// then durably journal structured successful stdout before success.
    pub(crate) fn run_coordinated_operation_at_with_hooks<P, F>(
        &mut self,
        mut request: CoordinatedCommand,
        cwd: &Path,
        queue_wait: QueueWait,
        pre_execute: P,
        on_success: F,
    ) -> Result<CoordinatedOperationReport, BrokerOpError>
    where
        P: FnOnce() -> Result<(), String>,
        F: FnOnce(&[u8], i64) -> Result<Option<serde_json::Value>, String>,
    {
        // Starts before any preparation, so what preparation spends is taken
        // out of what the lock may wait (#219).
        let admission = AdmissionDeadline::start(queue_wait);
        if request.args.is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "broker {} requires arguments after --",
                    request.provider.as_str()
                ),
            });
        }
        refuse_repeated_program_name(request.provider, &request.args)?;
        if request.provider == OperationProvider::Git {
            refuse_code_executing_git_options(&request.args)?;
        }
        if request.provider == OperationProvider::Git
            && let Some(directory) = git_explicit_directory(&request.args, cwd)?
        {
            let selected = crate::GitRepo::discover(&directory)?;
            if selected.git_common_dir()? != self.repo_handle().git_common_dir()? {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: format!(
                        "git -C target {:?} is outside this broker repository; run the operation through that repository's broker",
                        directory
                    ),
                });
            }
        }
        let github_target = if request.provider == OperationProvider::Github {
            request
                .repository
                .as_deref()
                .map(|repository| crate::resolve_github_target(repository, &request.args))
                .transpose()?
        } else {
            None
        };
        let inferred = match request.provider {
            OperationProvider::Git => classify_git(&request.args),
            OperationProvider::Github => classify_gh(&request.args),
        };
        let (effect, classification) = resolve_effect(inferred, request.declared_effect)?;
        let admission = admission.bounded_for(
            effect,
            is_long_running_read(request.provider, &request.args),
        );
        if effect == OperationEffect::Destructive && !request.destructive_confirmed {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: DESTRUCTIVE_FLAG_REQUIRED.into(),
            });
        }
        let mut authorization_reason =
            validate_authorization_reason(request.authorization_reason.as_deref())?;
        if effect != OperationEffect::Read && authorization_reason.is_none() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "coordinated writes require --reason identifying their authorization"
                    .into(),
            });
        }
        // A session id is all a caller needs to name a session, so it proves
        // nothing about whose branch is being deleted or rewritten. Before
        // anything is journaled: the refusal leaves no operation behind (#393).
        if request.ref_write_acknowledged {
            authorization_reason =
                authorization_reason.map(|reason| format!("{reason} [ref-write-acknowledged]"));
        }
        let gh_targets = if request.provider == OperationProvider::Github {
            let (targets, bound_args) =
                self.check_gh_ref_write(&request, cwd, github_target.as_ref())?;
            if let Some(args) = bound_args {
                // The argv journaled and run is the one just checked.
                request.args = args;
            }
            targets
        } else {
            GhRefTargets::default()
        };
        if let Some(owner) = self.refuse_foreign_session_branches(&request, effect, gh_targets)? {
            // Recorded with the authorization, so the journal says which
            // session's branch this operation was allowed to touch.
            authorization_reason =
                authorization_reason.map(|reason| format!("{reason} [cross-session {owner}]"));
        }

        let git_operation =
            (request.provider == OperationProvider::Git).then(|| git_operation_kind(&request.args));
        let is_remote_git = git_operation == Some(GitOperationKind::Remote);
        let resolved_target = match (
            &request.resolved_target,
            &request.repository,
            request.provider,
        ) {
            (Some(expected), None, OperationProvider::Git) if is_remote_git => {
                let repo = crate::GitRepo::discover(cwd)?;
                let actual = repo.resolve_remote_command_target(
                    git_subcommand_args(&request.args).expect("remote Git command was parsed"),
                    None,
                )?;
                if actual.remote_name != expected.remote_name
                    || actual.coordination_key != expected.coordination_key
                {
                    return Err(BrokerOpError::InvalidCoordinatedOperation {
                        reason: format!(
                            "Git command resolved to {} via remote {:?}, but the internal workflow authorized {} via remote {:?}",
                            actual.coordination_key,
                            actual.remote_name,
                            expected.coordination_key,
                            expected.remote_name
                        ),
                    });
                }
                Some(actual)
            }
            (Some(_), _, _) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: "a resolved remote target is only valid for internal Git operations without a second repository assertion".into(),
                });
            }
            (None, Some(repository), OperationProvider::Git) if is_remote_git => {
                validate_repository(repository)?;
                let repo = crate::GitRepo::discover(cwd)?;
                Some(repo.resolve_remote_command_target(
                    git_subcommand_args(&request.args).expect("remote Git command was parsed"),
                    Some(repository),
                )?)
            }
            (None, Some(_), OperationProvider::Github) => {
                debug_assert!(github_target.is_some());
                None
            }
            (None, Some(repository), OperationProvider::Git) => {
                validate_repository(repository)?;
                None
            }
            (None, None, OperationProvider::Github) => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: GH_REPO_REQUIRED.into(),
                });
            }
            (None, None, OperationProvider::Git) if is_remote_git => {
                return Err(BrokerOpError::InvalidCoordinatedOperation {
                    reason: REMOTE_GIT_REPO_REQUIRED.into(),
                });
            }
            (None, None, OperationProvider::Git) => None,
        };
        // Before anything is journaled, queued or sent, because that is the
        // whole value of the check: a refusal leaves no operation to reconcile
        // and no question about what reached GitHub (#184).
        if let Some(target) = &github_target
            && effect != OperationEffect::Read
        {
            refuse_undefined_labels(&request.args, &target.display_slug, cwd)?;
        }
        // One repository has exactly one coordination key. The head-of-line
        // check below matches `repository` exactly, so a second spelling is not
        // cosmetic: a wedged operation journaled as `owner/Name` would not block
        // a later push journaled as `github.com/owner/name`, and the check that
        // exists to fail closed would fail open instead (issue #166).
        let (repository, canonical_identity) =
            match (&resolved_target, &request.repository, request.provider) {
                (Some(target), _, OperationProvider::Git) => {
                    (target.coordination_key.clone(), true)
                }
                (None, Some(_), OperationProvider::Github) => (
                    github_target
                        .as_ref()
                        .expect("resolved GitHub target")
                        .coordination_key
                        .clone(),
                    true,
                ),
                // A local Git command selects no remote, so it cannot resolve its
                // own identity the way `push` does. Anchoring it to `origin` --
                // the same anchor gates already use -- gives `tag -a` and the
                // `push` that publishes that tag one key instead of two.
                (None, assertion, OperationProvider::Git) => {
                    match canonical_local_repository(cwd, assertion.as_deref())? {
                        Some(key) => (key, true),
                        None => (
                            assertion
                                .clone()
                                .unwrap_or_else(|| format!("local:{}", self.main_root().display())),
                            false,
                        ),
                    }
                }
                (Some(_), _, OperationProvider::Github) => unreachable!("validated above"),
                (None, None, OperationProvider::Github) => unreachable!("validated above"),
            };
        // Derived before anything queues, from the command line alone. A
        // recognized comment or review write coordinates per resource; every
        // other command stays repository-wide (#181).
        let resource_scope = if effect == OperationEffect::Read {
            None
        } else {
            resource_lock_scope(request.provider, &request.args)
        };
        let lock_key = coordination_lock_key(&repository, resource_scope.as_deref());

        let scope_was_declared = request.scope.is_some();
        // The derived scope is also the audit scope, so the journal names the
        // resource the lock actually protects. A caller-declared scope still
        // wins for the record, but never changes which lock is taken.
        let scope = request
            .scope
            .or_else(|| resource_scope.clone())
            .unwrap_or_else(|| "repository".into());
        validate_scope(&scope)?;
        if inferred.is_none() && !scope_was_declared {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "ambiguous operation requires an explicit --scope as well as --effect"
                    .into(),
            });
        }

        let is_remote_write = effect != OperationEffect::Read
            && (resolved_target.is_some() || github_target.is_some());

        // Register before queueing, not after acquiring. An operation that only
        // existed once it held the lock was invisible for the whole wait, so a
        // caller could not tell a queued command from one that never started and
        // re-issued it (issue #138).
        let command_json = redacted_command(request.provider, &request.args)?;

        // What the push's hooks are told it is (#264). A delete-only push sends
        // nothing a pre-push gate could verify, so the broker runs no dry run
        // of its own for it; the repository's hook still runs, once, inside the
        // push, and can short-circuit on `AETHYME_PUSH_KIND=delete-only`.
        let push_shape = (request.provider == OperationProvider::Git)
            .then(|| classify_push(&request.args))
            .flatten();
        let hooks_ran_outside_lock = effect != OperationEffect::Read
            && request.provider == OperationProvider::Git
            && is_push(&request.args)
            && push_shape
                .as_ref()
                .is_none_or(|shape| shape.kind != PushKind::DeleteOnly)
            && hooks_outside_lock_enabled(self.main_root());

        // Two identical commands from one session cannot both be intended: the
        // second would fire against state the first already changed. Now that a
        // queued operation is recorded, refusing the duplicate is possible before
        // it is queued rather than after both have run (issue #138).
        if effect != OperationEffect::Read
            && let Some(pending) = self
                .store()
                .unresolved_coordinated_operations(&repository)?
                .into_iter()
                .find(|pending| {
                    pending.session_id == request.session_id
                        && pending.command_json == command_json
                        && matches!(
                            pending.status,
                            OperationStatus::Prepared | OperationStatus::Running
                        )
                        && !process_is_gone(pending.pid)
                })
        {
            return Err(BrokerOpError::DuplicatePendingOperation {
                operation_id: pending.id,
                status: pending.status.as_str(),
                liveness: operation_liveness_summary(&pending),
            });
        }

        let operation = self
            .store()
            .create_coordinated_operation(&NewCoordinatedOperation {
                session_id: request.session_id,
                provider: request.provider,
                repository: repository.clone(),
                scope,
                effect,
                authorization_reason,
                command_json,
                pid: i64::from(std::process::id()),
                // Not known until the lock is held and the host guard begins.
                host_operation_id: None,
                identity_provenance: if canonical_identity {
                    OperationIdentityProvenance::VerifiedCanonical
                } else {
                    OperationIdentityProvenance::LocalRepository
                },
            })?;

        let queued_operation_id = operation.id;

        let ref_determination = measure_pr_merge_ref_determination(
            self.main_root(),
            cwd,
            &request.args,
            github_target.as_ref(),
        );

        // Run the push's local hooks before queueing for the lock, when the
        // repository opts in. A dry run executes `pre-push` against exactly the
        // commits the real push will send, so the expensive part happens outside
        // the lock and the fleet no longer serialises on the slowest gate
        // (issues #138, #146).
        let prechecked_plan = if hooks_ran_outside_lock {
            let mut dry_run = crate::git::git_command();
            remove_inherited_git_config_overrides(&mut dry_run);
            let command_index =
                git_subcommand_index(&request.args).expect("push command was parsed");
            dry_run.args(&request.args[..command_index]);
            dry_run.arg("push").arg("--dry-run");
            dry_run.args(&request.args[command_index + 1..]);
            dry_run.current_dir(cwd);
            export_push_shape(&mut dry_run, push_shape.as_ref());
            match output_within(
                dry_run,
                admission,
                &repository,
                "running the pre-push dry run",
                None,
            ) {
                Ok(output) if output.status.success() => {}
                Ok(output) => {
                    // Failing here is the point: nothing is queued, and no other
                    // session waited on a gate that was going to refuse anyway.
                    let mut details = json!({ "reason": "pre_push_refused" });
                    add_failure_stderr(&mut details, &output.stderr);
                    self.resolve_unstarted_operation_with_details(queued_operation_id, details);
                    let safe_stderr = redacted_failure_stderr(&output.stderr);
                    return Err(BrokerOpError::InvalidCoordinatedOperation {
                        reason: format!(
                            "the repository's pre-push hook refused this push before the lock was taken: {}",
                            safe_stderr.trim()
                        ),
                    });
                }
                Err(error) => {
                    let reason = if matches!(&error, BrokerOpError::AdmissionTimedOut { .. }) {
                        "admission_timed_out"
                    } else {
                        "pre_push_unavailable"
                    };
                    self.resolve_unstarted_operation(queued_operation_id, reason);
                    return Err(error);
                }
            }
            Some(plan_exact_push(
                cwd,
                &request.args,
                resolved_target.as_ref(),
            ))
        } else {
            None
        };

        let lock_wait_started_at = (effect != OperationEffect::Read).then(unix_now_ms);
        let waiting_started_at = lock_wait_started_at.unwrap_or_else(unix_now_ms);
        let enqueued_at = operation.created_at;
        let lock = if effect == OperationEffect::Read {
            None
        } else {
            let main_root = self.main_root().to_path_buf();
            if let Err(error) = admission.check(&repository, "preparing the operation") {
                self.resolve_unstarted_operation(queued_operation_id, "admission_timed_out");
                return Err(error);
            }
            match RepositoryWriteLock::acquire(
                &main_root,
                &lock_key,
                queued_operation_id,
                || {
                    let holder = lock_holder_info(self.store(), &repository);
                    let details =
                        coordination_wait_details(&holder, enqueued_at, waiting_started_at);
                    self.store()
                        .annotate_prepared_operation(queued_operation_id, &details)?;
                    Ok(holder.description)
                },
                admission.remaining_queue_wait(queue_wait),
            ) {
                Ok(lock) => Some(lock),
                Err(error) => {
                    // Nothing ran, so the record must not linger as queued.
                    self.resolve_unstarted_operation(queued_operation_id, "lock_unavailable");
                    return Err(error);
                }
            }
        };
        if effect != OperationEffect::Read {
            let unresolved = self
                .store()
                .unresolved_coordinated_operations(&repository)?;
            for operation in unresolved {
                // Two operations block each other only where they would have
                // serialised. An unknown-outcome comment write leaves no ref
                // ambiguous, so it must not write-block a push -- and a stalled
                // push says nothing about a comment thread (#181).
                if stored_resource_scope(&operation) != resource_scope {
                    continue;
                }
                match operation.status {
                    // A prepared record now also covers an operation queued for
                    // this lock, so only a record whose owner is gone is abandoned.
                    // Resolving a live one would fail an operation that is merely
                    // waiting its turn (issue #138).
                    OperationStatus::Prepared if process_is_gone(operation.pid) => {
                        self.store().transition_coordinated_operation(
                            operation.id,
                            OperationStatus::Failed,
                            None,
                            Some(r#"{"reason":"abandoned_before_start"}"#),
                        )?;
                    }
                    OperationStatus::Prepared => {}
                    OperationStatus::Running => {
                        let blocking = self.store().transition_coordinated_operation(
                            operation.id,
                            OperationStatus::OutcomeUnknown,
                            operation.exit_code,
                            Some(r#"{"reason":"process_ended_without_outcome"}"#),
                        )?;
                        self.resolve_unstarted_operation(queued_operation_id, "repository_blocked");
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: blocking.id,
                            recovery: UnknownOutcomeRecovery::from_operation(&blocking),
                        });
                    }
                    OperationStatus::OutcomeUnknown => {
                        let recovery = UnknownOutcomeRecovery::from_operation(&operation);
                        self.resolve_unstarted_operation(queued_operation_id, "repository_blocked");
                        return Err(BrokerOpError::CoordinatedOperationBlocked {
                            repository,
                            operation_id: operation.id,
                            recovery,
                        });
                    }
                    _ => {}
                }
            }
        }
        let mut host_guard = if is_remote_write {
            Some(crate::HostOperationGuard::begin(
                &self.host_operation_database_path()?,
                &lock_key,
                request.provider,
                effect,
            )?)
        } else {
            None
        };
        if let Err(reason) = pre_execute() {
            self.resolve_unstarted_operation(queued_operation_id, "revalidation_failed");
            return Err(BrokerOpError::InvalidCoordinatedOperation { reason });
        }
        let push_planning = if request.provider == OperationProvider::Git {
            plan_exact_push(cwd, &request.args, resolved_target.as_ref())
        } else {
            PushPlanning::NotApplicable
        };
        // The counterpart for GitHub: what a create would produce, and the
        // number the repository already stands at. Taken here and not earlier
        // so the watermark is as close to the spawn as the lock allows --
        // every number assigned after it is a candidate for "this run did
        // that" (#184).
        let mut create_planning =
            if request.provider == OperationProvider::Github && effect != OperationEffect::Read {
                plan_github_create(&request.args, cwd)
            } else {
                CreatePlanning::NotApplicable
            };
        if let Some(target) = &github_target
            && let Err(error) =
                observe_create_watermark(&mut create_planning, &target.display_slug, cwd, admission)
        {
            self.resolve_unstarted_operation(queued_operation_id, "create_observation_failed");
            return Err(error);
        }

        // The hook verified specific commits. If any local ref moved while this
        // operation waited for the lock, that verification no longer describes
        // what would be sent, and the push must not proceed with hooks skipped.
        if let Some(prechecked) = &prechecked_plan
            && !push_planning.matches_prechecked(prechecked)
        {
            self.resolve_unstarted_operation(queued_operation_id, "refs_moved_after_pre_push");
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "a local ref moved between the pre-push hook and acquiring the lock, so \
                         the hook no longer describes what would be pushed; re-run the command"
                    .into(),
            });
        }

        // A bounded request may spend its entire budget in revalidation or
        // push planning. It has not started the provider command in this
        // case, so leave the prepared journal row failed rather than claiming
        // that a remote write has an unknown outcome.
        if let Err(error) = admission.check(&repository, "preparing the operation") {
            self.resolve_unstarted_operation(queued_operation_id, "admission_timed_out");
            return Err(error);
        }

        if let Some(host_operation_id) = host_guard
            .as_ref()
            .map(|guard| guard.operation().operation_id.clone())
        {
            self.store()
                .attach_host_operation(operation.id, &host_operation_id)?;
        }
        if let Some(guard) = &mut host_guard {
            guard.mark_running()?;
        }
        let operation_db_path = crate::broker_db_path(self.main_root())?;
        let mut operation_heartbeat = OperationHeartbeat::start(
            &operation_db_path,
            operation.id,
            "executing coordinated operation",
        );
        let mut running_details = journal_details(
            classification,
            resolved_target.as_ref(),
            github_target.as_ref(),
            with_push_planning(json!({}), &push_planning),
        );
        if let Some(heartbeat) = operation_heartbeat.as_ref() {
            add_operation_liveness(&mut running_details, heartbeat.liveness(unix_now_ms()));
        }
        self.store().transition_coordinated_operation(
            operation.id,
            OperationStatus::Running,
            None,
            Some(&running_details.to_string()),
        )?;

        // This is the spawn that performs the remote mutation, so the binary
        // it names is the whole point -- see `provider_command`.
        let executable = provider_executable(request.provider);
        let mut command = provider_command(request.provider);
        command.args(&request.args);
        // Appended after the subcommand, where `git push` accepts it. The
        // repository opted in, the same hook already ran against these exact
        // commits in the dry run above, and the plan was re-proven unchanged
        // under the lock. Re-running it here would double the cost the opt-in
        // exists to avoid (issues #138, #146).
        if hooks_ran_outside_lock {
            command.arg("--no-verify");
        }
        if request.provider == OperationProvider::Git {
            export_push_shape(&mut command, push_shape.as_ref());
        }
        // Trace2 gives us process-level evidence for the one useful
        // pre-transfer distinction Git itself does not expose in its exit
        // status: a local pre-push hook can reject the command before any
        // transport child starts. Keep the trace private and use it only for
        // the journal; it must not alter the command's user-facing output.
        let git_trace = (request.provider == OperationProvider::Git
            && is_remote_git
            && effect != OperationEffect::Read)
            .then(|| tempfile::NamedTempFile::new().ok())
            .flatten();
        if let Some(trace) = &git_trace {
            command.env("GIT_TRACE2_EVENT", trace.path());
        }
        // gh's child gets its own command-scope git config from
        // `github_command`; stripping it here would undo those overrides.
        if request.provider == OperationProvider::Git {
            remove_inherited_git_config_overrides(&mut command);
        }
        command
            .current_dir(cwd)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("AETHYME_BROKER_SESSION_ID", request.session_id.to_string());
        // The pre-push hook treats the operation id as coordination for
        // protected branches, so a read never carries one.
        if effect != OperationEffect::Read {
            command.env("AETHYME_BROKER_OPERATION_ID", operation.id.to_string());
        }
        if let Some(heartbeat) = operation_heartbeat.as_ref() {
            command.env(BROKER_OPERATION_PROGRESS_ENV, heartbeat.path());
        }
        if request.provider == OperationProvider::Github {
            command.env(
                "GH_REPO",
                &github_target
                    .as_ref()
                    .expect("resolved GitHub target")
                    .display_slug,
            );
        }
        // Split the budget a read spent by phase, so a timeout says whether
        // the broker or the provider used it (#555).
        let preparation_ms = admission.elapsed_ms();
        let output = match output_within(
            command,
            admission,
            &repository,
            "executing coordinated operation",
            operation_heartbeat.as_ref(),
        ) {
            Ok(output) => output,
            Err(BrokerOpError::AdmissionTimedOut {
                repository,
                stage,
                budget,
            }) => {
                let operation_liveness =
                    operation_heartbeat.as_mut().map(OperationHeartbeat::finish);
                let status = if is_remote_write {
                    OperationStatus::OutcomeUnknown
                } else {
                    OperationStatus::Failed
                };
                let mut details = journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    with_push_planning(
                        json!({
                            "failure_class": "coordinated_operation_timeout",
                            "stage": stage,
                            "budget": budget,
                            "remote_outcome": if is_remote_write {
                                "unknown"
                            } else {
                                "not_applicable"
                            },
                            "preparation_ms": preparation_ms,
                            "provider_wait_ms": admission.elapsed_ms().saturating_sub(preparation_ms),
                        }),
                        &push_planning,
                    ),
                );
                if let Some(liveness) = operation_liveness {
                    add_operation_liveness(&mut details, liveness);
                }
                add_coordination_timing(
                    &mut details,
                    &lock_key,
                    lock_wait_started_at,
                    lock.as_ref(),
                    hooks_ran_outside_lock,
                    ref_determination,
                );
                let operation = self.store().transition_coordinated_operation(
                    operation.id,
                    status,
                    None,
                    Some(&details.to_string()),
                )?;
                if let Some(guard) = &mut host_guard {
                    guard.finish(operation.status)?;
                }
                if status == OperationStatus::OutcomeUnknown {
                    return Err(BrokerOpError::CoordinatedOperationBlocked {
                        repository,
                        operation_id: operation.id,
                        recovery: UnknownOutcomeRecovery::from_operation(&operation),
                    });
                }
                if effect == OperationEffect::Read {
                    return Err(BrokerOpError::ReadOperationTimedOut {
                        provider: provider_executable(request.provider),
                        operation_id: operation.id,
                        repository,
                        budget,
                        preparation_ms,
                        provider_wait_ms: admission.elapsed_ms().saturating_sub(preparation_ms),
                    });
                }
                return Err(BrokerOpError::CoordinatedOperationTimedOut {
                    provider: request.provider.as_str(),
                    operation_id: operation.id,
                    repository,
                    stage,
                    budget,
                });
            }
            Err(BrokerOpError::OperationIo { source, .. }) => {
                let operation_liveness =
                    operation_heartbeat.as_mut().map(OperationHeartbeat::finish);
                let mut details = journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    with_push_planning(
                        json!({
                            "reason": "spawn_failed",
                            "remote_contact": "not_contacted",
                            "remote_not_contacted": true,
                        }),
                        &push_planning,
                    ),
                );
                if let Some(liveness) = operation_liveness {
                    add_operation_liveness(&mut details, liveness);
                }
                add_coordination_timing(
                    &mut details,
                    &lock_key,
                    lock_wait_started_at,
                    lock.as_ref(),
                    hooks_ran_outside_lock,
                    ref_determination,
                );
                let operation = self.store().transition_coordinated_operation(
                    operation.id,
                    OperationStatus::Failed,
                    None,
                    Some(&details.to_string()),
                )?;
                if let Some(guard) = &mut host_guard {
                    guard.finish(operation.status)?;
                }
                return Err(BrokerOpError::OperationSpawn {
                    executable: executable.into(),
                    source,
                });
            }
            Err(error) => {
                let _ = operation_heartbeat.as_mut().map(OperationHeartbeat::finish);
                return Err(error);
            }
        };
        // The heartbeat deliberately outlives the child. Everything below --
        // `on_success`, and the push and GitHub reconciles -- runs while the
        // row is still `running`, and one of those reconciles is an unbounded
        // `git ls-remote`. Stopping the heartbeat here left a healthy
        // operation looking `heartbeat_stale` after 30s, so every blocked
        // caller was told the holder may have died: the #138 failure this
        // machinery exists to prevent, restated more confidently.
        let remote_contact = git_trace
            .as_ref()
            .map(|trace| inspect_git_transfer_trace(trace.path()))
            .and_then(GitTransferTrace::remote_contact);
        let exit_code = output.status.code().map(i64::from);
        let (status, mut details) = if output.status.success() {
            match on_success(&output.stdout, operation.id) {
                Ok(Some(result)) => (
                    OperationStatus::Succeeded,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(json!({ "result": result }), &push_planning),
                    ),
                ),
                Ok(None) => (
                    OperationStatus::Succeeded,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(json!({}), &push_planning),
                    ),
                ),
                Err(reason) => (
                    OperationStatus::OutcomeUnknown,
                    journal_details(
                        classification,
                        resolved_target.as_ref(),
                        github_target.as_ref(),
                        with_push_planning(
                            json!({
                                "reason": "success_result_not_recorded",
                                "diagnosis": reason,
                            }),
                            &push_planning,
                        ),
                    ),
                ),
            }
        } else if effect == OperationEffect::Read {
            (
                OperationStatus::Failed,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({}),
                ),
            )
        } else if request.provider == OperationProvider::Git
            && git_operation == Some(GitOperationKind::Local)
        {
            // A local Git command can leave the worktree or index in a
            // conflict state, but it cannot have an uncertain remote effect.
            // Keeping it as `outcome_unknown` write-blocked the canonical
            // repository and told the operator to inspect remote state that
            // the command could never have touched (#185).
            (
                OperationStatus::Failed,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({
                        "failure_class": "local_git_command_failed",
                        "remote_contact": "not_applicable",
                        "recovery": "inspect_or_abort_local_worktree_state",
                    }),
                ),
            )
        } else if let Some((status, push_reconciliation)) =
            reconcile_failed_push(cwd, &push_planning, remote_contact)
        {
            (
                status,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({ "push_reconciliation": push_reconciliation }),
                ),
            )
        } else if let Some((status, create_reconciliation)) = match github_target.as_ref() {
            Some(target) => reconcile_failed_github_create_with_deadline(
                cwd,
                &target.display_slug,
                &create_planning,
                &output.stdout,
                admission,
            )
            .ok()
            .flatten(),
            None => None,
        } {
            (
                status,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    json!({ "create_reconciliation": create_reconciliation }),
                ),
            )
        } else if let Some(refusal) = (request.provider == OperationProvider::Github)
            .then(|| classify_github_refusal(&request.args, output.status.code(), &output.stderr))
            .flatten()
        {
            // GitHub said no to the command's only mutation, so nothing was
            // applied. Recording that as unknown write-blocked the repository
            // over a refusal the PR head itself proved (2026-10-03).
            (
                OperationStatus::Failed,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    refusal,
                ),
            )
        } else {
            // A mutating command may have applied a subset of its effects
            // before returning non-zero. Treating that as safely failed would
            // make a blind retry possible, so require external inspection.
            (
                OperationStatus::OutcomeUnknown,
                journal_details(
                    classification,
                    resolved_target.as_ref(),
                    github_target.as_ref(),
                    remote_contact.map_or_else(
                        || json!({}),
                        |remote_contact| {
                            json!({
                                "remote_contact": remote_contact.remote_contact,
                                "remote_write_contact": remote_contact.remote_write_contact,
                                "remote_not_contacted": remote_contact.remote_not_contacted,
                            })
                        },
                    ),
                ),
            )
        };
        // Every failed Git write keeps its stderr tail: a failed local
        // `merge --ff-only` (main reconcile) is as undiagnosable without it as
        // a refused push (#415).
        if request.provider == OperationProvider::Git && !output.status.success() {
            add_failure_stderr(&mut details, &output.stderr);
        }
        // The row leaves `running` on the next statement, after which liveness
        // is no longer consulted, so this is the first moment the heartbeat is
        // redundant rather than load-bearing.
        if let Some(liveness) = operation_heartbeat.as_mut().map(OperationHeartbeat::finish) {
            add_operation_liveness(&mut details, liveness);
        }
        add_coordination_timing(
            &mut details,
            &lock_key,
            lock_wait_started_at,
            lock.as_ref(),
            hooks_ran_outside_lock,
            ref_determination,
        );
        let operation = self.store().transition_coordinated_operation(
            operation.id,
            status,
            exit_code,
            Some(&details.to_string()),
        )?;
        if let Some(guard) = &mut host_guard {
            guard.finish(operation.status)?;
        }
        let created_pull_request_number = (output.status.success()
            && request.provider == OperationProvider::Github
            && crate::creates_pull_request(&request.args))
        .then(|| crate::pull_request_number_from_output(&String::from_utf8_lossy(&output.stdout)))
        .flatten();
        Ok(CoordinatedOperationReport {
            operation,
            classification,
            resolved_target,
            github_target,
            command_success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            post_merge_cleanup: None,
            representing_commit: None,
            created_pull_request: created_pull_request_number,
            pushed_refs: if output.status.success() {
                match &push_planning {
                    PushPlanning::Planned(plan) => plan
                        .destinations
                        .iter()
                        .map(|destination| PushedRef {
                            destination_ref: destination.destination_ref.clone(),
                            proposed_sha: destination.proposed_sha.clone(),
                        })
                        .collect(),
                    _ => Vec::new(),
                }
            } else {
                Vec::new()
            },
        })
    }

    pub fn reconcile_coordinated_operation(
        &mut self,
        operation_id: i64,
        succeeded: bool,
        reason: &str,
    ) -> Result<OperationReconcileReport, BrokerOpError> {
        if reason.trim().is_empty() {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: "operation reconciliation requires a non-empty --reason".into(),
            });
        }
        let operation = self.store().coordinated_operation(operation_id)?.ok_or(
            crate::BrokerError::CoordinatedOperationNotFound(operation_id),
        )?;
        if operation.host_operation_id.is_none()
            && operation.status != OperationStatus::OutcomeUnknown
        {
            return Err(BrokerOpError::InvalidCoordinatedOperation {
                reason: format!(
                    "operation {} is {}, not outcome_unknown",
                    operation_id,
                    operation.status.as_str()
                ),
            });
        }
        let status = if succeeded {
            OperationStatus::ReconciledSucceeded
        } else {
            OperationStatus::ReconciledFailed
        };
        if let Some(host_operation_id) = &operation.host_operation_id {
            crate::reconcile_host_operation(
                &self.host_operation_database_path()?,
                host_operation_id,
                succeeded,
            )?;
        }
        let mut details = operation
            .details_json
            .as_deref()
            .and_then(|details| serde_json::from_str::<serde_json::Value>(details).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| json!({}));
        details["reconciliation"] = json!({
            "operator_reason": reason,
            "outcome": status.as_str(),
        });
        let operation = self.store().transition_coordinated_operation(
            operation_id,
            status,
            operation.exit_code,
            Some(&details.to_string()),
        )?;
        Ok(OperationReconcileReport {
            operation,
            reason: reason.into(),
        })
    }
}

#[cfg(test)]
mod tests;
