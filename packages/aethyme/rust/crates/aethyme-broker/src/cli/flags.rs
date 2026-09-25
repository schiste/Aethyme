//! Per-subcommand flag validation: which flags each subcommand reads.

use super::*;

/// Flags every subcommand accepts. `-h`/`--help` never reach validation: the
/// parser turns them into help before a subcommand can see them.
pub(super) const GLOBAL_FLAGS: &[&str] = &["--json"];

/// Which flags each subcommand reads, keyed by its command path.
///
/// The parser fills one shared `Parsed` for every subcommand, so a flag that is
/// valid somewhere parses everywhere. Without this table a flag given to a
/// subcommand that never reads it was silently dropped -- `adopt --claim` in
/// #285 -- which reports a choice the broker never made. Each entry lists the
/// flags its handler actually reads (including flags it reads only to refuse
/// with a sharper message), so nothing that works is refused.
///
/// A path of more than one word is an action (`gates scope`, `watch pr start`)
/// and applies when the positional arguments begin with those words. A
/// one-word path applies when no action entry matches: to the bare command, and
/// to a command whose positional is a value rather than an action
/// (`cleanup <id>`). A command with action entries and no matching one (an
/// unknown action) is held to the union of its entries, and its handler names
/// the bad action.
///
/// Keep in step with `USAGE`: `every_usage_example_passes_flag_validation`
/// fails when a documented flag is missing here.
pub(super) const FLAG_RULES: &[(&str, &[&str])] = &[
    ("readiness", &["--require"]),
    ("worktree-root", &[]),
    (
        "start",
        &[
            "--task",
            "--base",
            "--pull-request",
            "--path",
            "--claim",
            "--agent",
            "--repo-name",
            "--tab-name",
            "--ai-provider",
        ],
    ),
    (
        "start-agent",
        &[
            "--task",
            "--cmd",
            "--base",
            "--pull-request",
            "--agent",
            "--repo-name",
            "--tab-name",
            "--ai-provider",
        ],
    ),
    (
        "adopt",
        &[
            "--task",
            "--path",
            "--claim",
            "--agent",
            "--repo-name",
            "--tab-name",
            "--ai-provider",
            "--reuse",
            "--sync-integration",
            "--replace-stale",
            // `start --adopt` resolves here; the flag only selects the handler.
            "--adopt",
        ],
    ),
    (
        "report capture",
        &[
            "--kind",
            "--title",
            "--session",
            "--include-task",
            "--stdout",
            "--output",
        ],
    ),
    ("report list", &[]),
    ("report show", &[]),
    ("report render", &["--form", "--output"]),
    ("report file", &["--repo", "--confirm"]),
    ("quality-report", &["--repo", "--pr", "--session"]),
    ("external-events ingest", &[]),
    ("external-events list", &["--all"]),
    ("external-events show", &[]),
    (
        "external-events reconcile",
        &["--outcome", "--reason", "--session"],
    ),
    ("reclaim", &[]),
    ("reclaim plan", &[]),
    ("reclaim apply", &["--confirm"]),
    (
        "deliveries subscribe",
        &["--watch", "--adapter", "--target", "--policy"],
    ),
    ("deliveries list", &["--adapter", "--all"]),
    ("deliveries claim", &["--adapter", "--worker", "--seconds"]),
    ("deliveries resolve-tab", &["--session", "--tabs-file"]),
    (
        "deliveries dispatch",
        &["--adapter", "--worker", "--tabs-file", "--seconds"],
    ),
    (
        "deliveries complete",
        &[
            "--id",
            "--worker",
            "--generation",
            "--outcome",
            "--error-code",
        ],
    ),
    ("review plan", &["--base", "--pr", "--repo"]),
    (
        "review run",
        &[
            "--session",
            "--repo",
            "--pr",
            "--base",
            "--tabs-file",
            "--from-provider",
            "--dry-run",
        ],
    ),
    (
        "review tick",
        &[
            "--session",
            "--repo",
            "--pr",
            "--base",
            "--limit",
            "--tabs-file",
            "--from-provider",
            "--dry-run",
        ],
    ),
    ("review ledger", &["--repo", "--pr"]),
    (
        "review state",
        &[
            "--repo",
            "--pr",
            "--type",
            "--state",
            "--head",
            "--note",
            "--completed-for-commit",
            "--verdict",
            "--reviewer-provider",
            "--reviewer-model",
        ],
    ),
    (
        "review waive",
        &["--repo", "--pr", "--type", "--head", "--reason", "--agent"],
    ),
    ("review register", &["--session", "--repo", "--pr"]),
    ("review show", &["--session"]),
    ("review request", &["--session"]),
    ("review unlock", &["--session"]),
    (
        "review reassign",
        &["--session", "--to-session", "--reason"],
    ),
    ("review abandon", &["--session", "--reason"]),
    ("prepare", &["--session", "--offline", "--wait"]),
    // `status` reads `--offline` and `--wait` only to refuse them by name.
    ("prepare status", &["--session", "--offline", "--wait"]),
    ("console", &[]),
    ("console status", &[]),
    ("console list", &[]),
    ("console plan", &["--allow-parallel"]),
    (
        "console run",
        &["--wait", "--allow-parallel", "--cleanup-command", "--"],
    ),
    ("resources plan", &["--wait", "--grant-out"]),
    ("resources explain", &["--wait", "--grant-out"]),
    ("resources acquire", &["--wait", "--grant-out"]),
    ("resources run", &["--wait", "--cleanup-command", "--"]),
    ("resources renew", &["--ttl"]),
    ("resources release", &["--ttl"]),
    ("resources list", &["--all"]),
    ("resources reap", &[]),
    ("resources reconcile", &["--confirm"]),
    ("agents", &[]),
    ("leases", &[]),
    ("leases claim", &["--session", "--ttl"]),
    ("leases plan", &["--session"]),
    ("leases export", &["--session", "--entry", "--limit"]),
    ("leases release", &["--session"]),
    ("exec", &["--session", "--"]),
    (
        "git",
        &[
            "--session",
            "--repo",
            "--scope",
            "--effect",
            "--reason",
            "--destructive",
            "--no-wait",
            "--queue-timeout",
            "--",
        ],
    ),
    (
        "gh",
        &[
            "--session",
            "--repo",
            "--scope",
            "--effect",
            "--reason",
            "--destructive",
            "--no-wait",
            "--queue-timeout",
            "--",
        ],
    ),
    (
        "operations",
        &[
            "--limit",
            "--before",
            "--session",
            "--status",
            "--repo",
            "--provider",
        ],
    ),
    (
        "operations list",
        &[
            "--limit",
            "--before",
            "--session",
            "--status",
            "--repo",
            "--provider",
        ],
    ),
    ("operations show", &[]),
    ("operations stats", &["--repo", "--limit"]),
    (
        "operations reconcile",
        &["--operation", "--outcome", "--reason"],
    ),
    ("blockers", &[]),
    ("unblock", &["--outcome", "--reason", "--confirm"]),
    ("advisories list", &["--all"]),
    ("advisories show", &[]),
    ("advisories ack", &[]),
    ("advisories suppress", &[]),
    ("advisories metrics", &[]),
    ("exposures plan", &[]),
    ("exposures apply", &["--session", "--confirm"]),
    ("note send", &["--session", "--to-session", "--message"]),
    ("note list", &["--session"]),
    ("note ack", &["--session", "--id"]),
    ("gates draft", &[]),
    ("gates validate", &[]),
    (
        "gates doctor",
        &["--probe", "--only", "--session", "--all", "--no-cache"],
    ),
    ("gates manifest", &["--head"]),
    ("gates scope", &["--base", "--head"]),
    ("gates affected", &["--session"]),
    ("gates semantic", &["--session"]),
    ("gates run", &["--session", "--all", "--only", "--no-cache"]),
    ("gates pre-push", &["--session", "--all", "--no-cache"]),
    ("hooks install", &[]),
    ("hooks uninstall", &[]),
    ("hooks status", &[]),
    ("hooks snippet", &[]),
    ("hooks pre-commit", &[]),
    ("hooks post-commit", &[]),
    ("hooks pre-push", &[]),
    ("trust", &["--repo"]),
    ("trust status", &["--repo"]),
    (
        "pr check",
        &["--target", "--pr", "--agent", "--dispatch", "--cmd"],
    ),
    ("watch pr monitoring", &["--session"]),
    (
        "watch pr start",
        &["--session", "--repo", "--pr", "--events", "--seconds"],
    ),
    ("watch pr list", &["--all"]),
    ("watch pr show", &["--id"]),
    ("watch pr poll", &["--id"]),
    ("watch pr pause", &["--id"]),
    ("watch pr resume", &["--id"]),
    ("watch pr stop", &["--id"]),
    ("watch pr tick", &["--limit"]),
    ("watch pr batches", &["--id", "--all"]),
    ("watch pr ack", &["--id", "--outcome", "--reason"]),
    ("submit", &["--session", "--no-cache", "--verify-only"]),
    ("repair", &["--session"]),
    ("checkpoint plan", &["--session"]),
    ("checkpoint apply", &["--session", "--confirm"]),
    // Bare `queue` reads `--limit` and `--before` only to refuse them by name.
    ("queue", &["--active", "--limit", "--before"]),
    ("queue history", &["--limit", "--before"]),
    ("promote", &["--entry"]),
    ("ship plan", &["--entry", "--delivery", "--detail"]),
    (
        "ship execute",
        &[
            "--entry",
            "--confirm",
            "--delivery",
            "--plan",
            "--plan-digest",
            "--sync-main",
            "--break-glass",
            "--reason",
        ],
    ),
    ("integration status", &[]),
    ("integration wait-stable", &["--seconds"]),
    (
        "integration reconcile",
        &[
            "--upstream",
            "--resolution-file",
            "--write-resolution-template",
            "--dry-run",
            "--apply",
            "--confirm",
        ],
    ),
    ("status", &["--summary"]),
    ("events", &["--since", "--kind", "--follow"]),
    ("events prune", &["--keep-days"]),
    ("metrics", &[]),
    ("doctor", &["--fix-version"]),
    ("quick-test", &["--chau7", "--with-gate"]),
    ("verify-loop", &[]),
    ("e2e", &[]),
    ("init", &[]),
    ("certify", &[]),
    ("scaffold", &[]),
    ("handoff", &["--session", "--worktree"]),
    ("finish", &["--session", "--keep-worktree"]),
    ("close", &["--session"]),
    (
        "cleanup",
        &[
            "--force",
            "--all-cleaned",
            "--apply",
            "--confirm",
            "--dry-run",
            "--detail",
        ],
    ),
    (
        "main reconcile plan",
        &[
            "--detail",
            "--resolution-file",
            "--write-resolution-template",
        ],
    ),
    (
        "main reconcile apply",
        &["--session", "--confirm", "--resolution-file"],
    ),
    ("representation scan", &["--session"]),
    ("representation status", &["--session"]),
    ("representation record", &["--session", "--confirm"]),
    ("promotion-record plan", &[]),
    ("promotion-record apply", &["--confirm"]),
    // `plan` reads `--confirm` only to refuse it by name.
    ("gc plan", &["--detail", "--confirm"]),
    ("gc apply", &["--confirm"]),
    ("worktrees", &[]),
    ("storage", &["--detail", "--confirm"]),
    ("storage plan", &["--detail", "--confirm"]),
    ("storage apply", &["--confirm"]),
];

/// Extra guidance for a refusal whose plausible cause the generic list does not
/// explain, keyed by the one-word subcommand and the flag.
pub(super) const FLAG_REFUSAL_HINTS: &[(&str, &str, &str)] = &[(
    "adopt",
    "--base",
    "--base does not apply to broker adopt: adopting registers an existing worktree, whose branch already has its own \
     history. Use broker start --base <ref> to cut a new branch from a chosen base.",
)];

/// The `FLAG_RULES` entry that governs `subcommand` with these positionals, as
/// its path and flags. `None` for a subcommand the table does not know, which
/// the dispatcher then reports as unknown.
pub(super) fn flag_rule(
    subcommand: &str,
    positional: &[String],
) -> Option<(String, Vec<&'static str>)> {
    let entries: Vec<(Vec<&str>, &'static [&'static str])> = FLAG_RULES
        .iter()
        .filter_map(|(path, flags)| {
            let words: Vec<&str> = path.split(' ').collect();
            (words[0] == subcommand).then_some((words, *flags))
        })
        .collect();
    if entries.is_empty() {
        return None;
    }
    let action_match = entries
        .iter()
        .filter(|(words, _)| {
            words.len() > 1
                && words.len() - 1 <= positional.len()
                && words[1..]
                    .iter()
                    .zip(positional)
                    .all(|(word, given)| word == given)
        })
        .max_by_key(|(words, _)| words.len());
    if let Some((words, flags)) = action_match {
        return Some((words.join(" "), flags.to_vec()));
    }
    let has_actions = entries.iter().any(|(words, _)| words.len() > 1);
    if let Some((_, flags)) = entries.iter().find(|(words, _)| words.len() == 1)
        && (positional.is_empty() || !has_actions)
    {
        return Some((subcommand.to_string(), flags.to_vec()));
    }
    let mut union: Vec<&'static str> = Vec::new();
    for (_, flags) in &entries {
        for flag in *flags {
            if !union.contains(flag) {
                union.push(flag);
            }
        }
    }
    Some((subcommand.to_string(), union))
}

/// The command paths at which `flag` is valid, in table order.
pub(super) fn flag_valid_at(flag: &str) -> Vec<&'static str> {
    FLAG_RULES
        .iter()
        .filter(|(_, flags)| flags.contains(&flag))
        .map(|(path, _)| *path)
        .collect()
}

/// Why `flags` cannot be given to `subcommand` with these positionals, or
/// `None` when every flag is one it reads.
pub(super) fn flag_refusal(
    subcommand: &str,
    positional: &[String],
    flags: &[String],
) -> Option<String> {
    let (path, allowed) = flag_rule(subcommand, positional)?;
    let flag = flags
        .iter()
        .find(|flag| !GLOBAL_FLAGS.contains(&flag.as_str()) && !allowed.contains(&flag.as_str()))?;
    let flag_label = if flag == "--" {
        "`--` (the command separator)".to_string()
    } else {
        format!("`{flag}`")
    };
    let valid_at = flag_valid_at(flag);
    let mut message = if valid_at.is_empty() {
        format!("{flag_label} is not valid for `broker {path}`")
    } else {
        format!(
            "{flag_label} is not valid for `broker {path}`; it applies to: {}",
            valid_at.join(", ")
        )
    };
    if let Some((_, _, hint)) = FLAG_REFUSAL_HINTS
        .iter()
        .find(|(command, hinted, _)| *command == subcommand && hinted == flag)
    {
        message.push_str(". ");
        message.push_str(hint);
    }
    Some(message)
}

/// Refuse any flag the subcommand does not read, as a usage error.
pub(super) fn validate_flags(subcommand: &str, parsed: &Parsed) -> Result<(), UsageError> {
    match flag_refusal(subcommand, &parsed.positional, &parsed.given_flags) {
        Some(message) => Err(UsageError::Exit {
            message,
            code: crate::exit_status::USAGE,
        }),
        None => Ok(()),
    }
}
