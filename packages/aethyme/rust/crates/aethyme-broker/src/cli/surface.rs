//! The public broker verb surface: the public verbs, `advanced`, and the deprecation
//! window for every older spelling.
//!
//! This module only translates spellings. Each public form resolves to the
//! internal subcommand that already implemented it, and that implementation
//! runs unchanged: `submit`, `promote`, gates and cleanup keep their exact
//! code paths. The internal names are what `run_inner`, `FLAG_RULES`, command
//! metrics and the router's compatibility policy continue to key on.

use super::USAGE;

/// The release that removes deprecated broker and top-level spellings.
pub const REMOVED_SPELLING_RELEASE: &str = "v0.8.8";

/// Actionable error for a spelling that was removed from the public surface.
pub fn removed_spelling_message(old: &str, new: &str) -> String {
    format!("'{old}' was removed in {REMOVED_SPELLING_RELEASE}; use '{new}'")
}

/// The verbs `aethyme broker --help` lists.
pub const PUBLIC_VERBS: &[(&str, &str)] = &[
    (
        "start",
        "create an isolated worktree + session, or adopt/reuse an existing one",
    ),
    (
        "status",
        "sessions, overlaps, queue and integration; readiness and doctor views",
    ),
    (
        "submit",
        "simulate, gate and promote a session; preparation and promotion recovery",
    ),
    (
        "push",
        "publish this session's own branch as you work; --pr opens a draft PR",
    ),
    (
        "sync",
        "catch this session up to the latest default branch when it is safe",
    ),
    (
        "finish",
        "close sessions; reclaim ignored build output or clean up retained checkouts",
    ),
    ("unblock", "list current blockers, or clear one by id"),
    (
        "gc",
        "reviewed plan/apply cleanup: retention, build output, storage, resources",
    ),
];

/// Sub-forms of a public verb that dispatch to an older implementation:
/// `(verb, word, internal command words)`.
const MERGED_FORMS: &[(&str, &str, &[&str])] = &[
    ("status", "readiness", &["readiness"]),
    ("status", "doctor", &["doctor"]),
    ("submit", "prepare", &["prepare"]),
    ("submit", "promote", &["promote"]),
    ("submit", "promotion-record", &["promotion-record"]),
    ("finish", "close", &["close"]),
    ("finish", "cleanup", &["cleanup"]),
    ("gc", "reclaim", &["reclaim"]),
    ("gc", "storage", &["storage"]),
    ("gc", "reap", &["resources", "reap"]),
];

/// The `(verb, word)` sub-forms public verbs accept, such as `("gc", "reclaim")`.
/// Exposed so surface tests enumerate the forms instead of copying them.
pub fn public_forms() -> impl Iterator<Item = (&'static str, &'static str)> {
    MERGED_FORMS.iter().map(|(verb, word, _)| (*verb, *word))
}

/// Verbs reachable as `aethyme broker advanced <verb>`, with the one-line
/// summary `broker advanced --help` prints.
pub const ADVANCED_VERBS: &[(&str, &str)] = &[
    (
        "leases",
        "inspect, claim, plan, export or release path ownership",
    ),
    (
        "ownership",
        "claim, list or release a named operation such as a release",
    ),
    ("git", "run Git through the durable operation coordinator"),
    ("gh", "run GitHub CLI through the same coordinator"),
    (
        "operations",
        "inspect or reconcile the remote-operation journal",
    ),
    (
        "exec",
        "run a command in a session worktree under the dirty-path guard",
    ),
    ("ship", "plan or execute a reviewed full-SHA publication"),
    ("review", "plan, run, record and route pull request reviews"),
    ("gates", "validate, draft, scope, run and diagnose gates"),
    ("hooks", "install, remove or inspect the managed git hooks"),
    (
        "trust",
        "approve this repository's gate and prepare commands",
    ),
    (
        "agents",
        "list live sessions with activity-derived liveness",
    ),
    ("handoff", "read a finished session's persisted handoff"),
    ("queue", "show the merge queue"),
    (
        "integration",
        "inspect, wait on or reconcile the integration branch",
    ),
    (
        "main",
        "reconcile the local default branch onto integration",
    ),
    (
        "representation",
        "prove a session's work already landed on main",
    ),
    (
        "checkpoint",
        "recover a session whose checkpoint was rewritten",
    ),
    (
        "repair",
        "apply the documented recovery for a submit conflict",
    ),
    (
        "resources",
        "plan, acquire, run and release host-wide resources",
    ),
    (
        "console",
        "run a dev server under the repository's console mode",
    ),
    (
        "advisories",
        "list, show, acknowledge or suppress advisories",
    ),
    (
        "exposures",
        "reconcile exposures against the remote default branch",
    ),
    ("note", "send and acknowledge notes between live sessions"),
    ("watch", "metadata-only pull request watches"),
    (
        "deliveries",
        "the durable delivery outbox for watch adapters",
    ),
    ("pr", "check pull request state"),
    ("report", "capture, render and file diagnostic reports"),
    ("quality-report", "publish a quality report as a PR summary"),
    (
        "external-events",
        "ingest and reconcile normalized external events",
    ),
    ("events", "show or prune the append-only event log"),
    ("metrics", "cost/benefit accounting from local telemetry"),
    (
        "insights",
        "session funnel, time-to-land and gate reliability from local history",
    ),
    ("worktree-root", "resolve the external worktree root"),
    ("worktrees", "list broker-owned worktrees"),
    ("scaffold", "deterministic only-if-missing broker setup"),
    ("quick-test", "disposable first-run smoke test"),
    (
        "verify-loop",
        "end-to-end broker verification for operators",
    ),
    (
        "check-contract",
        "cross-process contract gate (CI entry point)",
    ),
];

/// Internal entry points invoked by installed hook shims, CI, and other
/// binaries (`update` runs a staged binary's `broker quick-test`, which may be
/// older or newer than this one). Their spelling lives outside this binary's
/// control, so it is permanent: they never warn and are never removed.
fn is_machine_entry_point(args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("check-contract" | "quick-test") => true,
        Some("hooks") => matches!(
            args.get(1).map(String::as_str),
            Some("pre-commit" | "post-commit" | "pre-push")
        ),
        _ => false,
    }
}

/// A broker command line translated to its internal spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The command line `run_inner` dispatches (no `advanced`, no public
    /// sub-form words).
    pub args: Vec<String>,
    /// Set when the command line must not run at all: a public verb spelled
    /// under `advanced`. Refusing it (rather than stripping `advanced`)
    /// keeps resolution a single step, so the router's compatibility
    /// classification and the broker's dispatch see the same command.
    pub refusal: Option<String>,
}

fn words(prefix: &[&str], rest: &[String]) -> Vec<String> {
    prefix
        .iter()
        .map(|word| (*word).to_string())
        .chain(rest.iter().cloned())
        .collect()
}

/// Resolve a public broker command line to its internal spelling. Removed
/// spellings are refused with their current replacement.
pub fn resolve(args: &[String]) -> Resolution {
    let Some(first) = args.first().map(String::as_str) else {
        return Resolution {
            args: Vec::new(),
            refusal: None,
        };
    };
    if first == "advanced" {
        let internal = args[1..].to_vec();
        if let Some(verb) = internal.first().map(String::as_str)
            && (verb == "advanced" || PUBLIC_VERBS.iter().any(|(public, _)| *public == verb))
        {
            return Resolution {
                args: internal.clone(),
                refusal: Some(format!(
                    "`{verb}` is not an advanced verb; use 'aethyme broker {}'",
                    internal.join(" ")
                )),
            };
        }
        let refusal = removed_spelling_replacement(&internal, true)
            .map(|(old, new)| removed_spelling_message(&old, &new));
        return Resolution {
            args: internal,
            refusal,
        };
    }
    if PUBLIC_VERBS.iter().any(|(verb, _)| *verb == first) {
        if let Some(second) = args.get(1).map(String::as_str)
            && let Some((_, _, internal)) = MERGED_FORMS
                .iter()
                .find(|(verb, word, _)| *verb == first && *word == second)
        {
            return Resolution {
                args: words(internal, &args[2..]),
                refusal: None,
            };
        }
        // `unblock` with nothing to clear lists what could be cleared.
        if first == "unblock" && args[1..].iter().all(|arg| arg == "--json") {
            return Resolution {
                args: words(&["blockers"], &args[1..]),
                refusal: None,
            };
        }
        return Resolution {
            args: args.to_vec(),
            refusal: None,
        };
    }
    let refusal = removed_spelling_replacement(args, false)
        .map(|(old, new)| removed_spelling_message(&old, &new));
    Resolution {
        args: args.to_vec(),
        refusal,
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == flag)
}

/// The old and replacement command lines, or `None` for a current or unknown
/// spelling. These names only produce refusal guidance; they never dispatch.
fn removed_spelling_replacement(args: &[String], via_advanced: bool) -> Option<(String, String)> {
    let first = args.first()?.as_str();
    if is_machine_entry_point(args) {
        return None;
    }
    let broker = |spelling: &str| format!("aethyme broker {spelling}");
    let old_prefix = if via_advanced {
        "aethyme broker advanced"
    } else {
        "aethyme broker"
    };
    let old = |spelling: &str| format!("{old_prefix} {spelling}");
    let (old, new) = match first {
        "adopt" if has_flag(args, "--reuse") => (old("adopt --reuse"), broker("start --reuse")),
        "adopt" if has_flag(args, "--replace-stale") => (
            old("adopt --replace-stale"),
            broker("start --replace-stale"),
        ),
        "adopt" => (old("adopt"), broker("start --adopt")),
        "start-agent" => (old("start-agent"), broker("start --cmd <command>")),
        "blockers" => (old("blockers"), broker("unblock")),
        "resources" if args.get(1).map(String::as_str) == Some("reap") => {
            (old("resources reap"), broker("gc reap"))
        }
        "init" | "certify" => (old(first), format!("aethyme {first}")),
        "e2e" => (old("e2e"), broker("advanced verify-loop")),
        _ => {
            if let Some((verb, word, _)) = MERGED_FORMS
                .iter()
                .find(|(_, _, internal)| internal.len() == 1 && internal[0] == first)
            {
                (old(first), broker(&format!("{verb} {word}")))
            } else if !via_advanced && ADVANCED_VERBS.iter().any(|(verb, _)| *verb == first) {
                (old(first), broker(&format!("advanced {first}")))
            } else {
                return None;
            }
        }
    };
    Some((old, new))
}

/// `-h`/`--help` anywhere before a `--` command separator.
pub fn wants_help(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "-h" || arg == "--help")
}

/// Help text for a broker command line, or `None` when the command is not
/// one this surface knows (the dispatcher then reports it as unknown).
pub fn help_text(args: &[String]) -> Option<String> {
    if resolve(args).refusal.is_some() {
        return None;
    }
    help_text_for_internal_command(args)
}

/// Help renderer for trusted internal callers that already resolved the
/// public spelling before dispatch.
pub(super) fn help_text_for_internal_command(args: &[String]) -> Option<String> {
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .take_while(|arg| *arg != "--")
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let Some(&first) = words.first() else {
        return Some(public_help());
    };
    if first == "advanced" {
        return match words.get(1) {
            None => Some(advanced_help()),
            Some(&verb) => internal_help(verb, words.get(2).copied()),
        };
    }
    if PUBLIC_VERBS.iter().any(|(verb, _)| *verb == first) {
        if let Some(&second) = words.get(1)
            && let Some((verb, word, internal)) = MERGED_FORMS
                .iter()
                .find(|(verb, word, _)| *verb == first && *word == second)
        {
            let public = format!("aethyme broker {verb} {word}");
            return Some(blocks(&internal.join(" "), &public));
        }
        return Some(public_verb_help(first));
    }
    internal_help(first, words.get(1).copied())
}

pub(super) fn public_help() -> String {
    let mut text = String::from(
        "aethyme broker — coordinate concurrent AI agent sessions on this repository\n\n\
         Usage: aethyme broker <verb> [args...]\n\n\
         Verbs:\n",
    );
    for (verb, summary) in PUBLIC_VERBS {
        text.push_str(&format!("  {verb:<9} {summary}\n"));
    }
    text.push_str(
        "\nEverything else (leases, git, gh, ship, review, operations, exec, ...):\n\
         \x20 aethyme broker advanced --help\n\n\
         Run `aethyme broker <verb> --help` for a verb's forms and flags.\n\
         Overlaps warn — they never block (v0 policy).\n",
    );
    text
}

pub(super) fn advanced_help() -> String {
    let mut text = String::from(
        "aethyme broker advanced — the full broker surface beyond the public verbs\n\n\
         Usage: aethyme broker advanced <verb> [args...]\n\n\
         Verbs:\n",
    );
    for (verb, summary) in ADVANCED_VERBS {
        text.push_str(&format!("  {verb:<16} {summary}\n"));
    }
    text.push_str("\nRun aethyme broker advanced <verb> --help for command forms and flags.\n");
    text
}

fn public_verb_help(verb: &str) -> String {
    let own = blocks(verb, &format!("aethyme broker {verb}"));
    let mut text = own;
    let extra: Vec<(String, String)> = match verb {
        "start" => vec![
            ("start-agent".into(), "aethyme broker start".into()),
            ("adopt".into(), "aethyme broker start --adopt".into()),
        ],
        "unblock" => vec![("blockers".into(), "aethyme broker unblock".into())],
        _ => MERGED_FORMS
            .iter()
            .filter(|(owner, _, _)| *owner == verb)
            .map(|(owner, word, internal)| {
                (internal.join(" "), format!("aethyme broker {owner} {word}"))
            })
            .collect(),
    };
    for (internal, public) in extra {
        text.push_str(&blocks(&internal, &public));
    }
    if verb == "start" {
        text.push_str(
            "\n  `start --reuse`, `start --replace-stale` and `start --adopt` register an\n\
             \x20 existing worktree (formerly `adopt`); `start --cmd` also spawns a process\n\
             \x20 (formerly `start-agent`). Each dispatches to the same implementation.\n",
        );
    }
    text
}

fn internal_help(verb: &str, action: Option<&str>) -> Option<String> {
    if verb == "init" || verb == "certify" {
        return Some(blocks_with_prefix(
            &format!("  aethyme {verb}"),
            &format!("aethyme {verb}"),
            &format!("aethyme {verb}"),
        ));
    }
    if verb == "e2e" {
        return internal_help("verify-loop", None);
    }
    if let Some((owner, word, internal)) = MERGED_FORMS.iter().find(|(_, _, internal)| {
        internal[0] == verb && internal.get(1).is_none_or(|second| Some(*second) == action)
    }) {
        return Some(blocks(
            &internal.join(" "),
            &format!("aethyme broker {owner} {word}"),
        ));
    }
    if verb == "adopt" || verb == "start-agent" || verb == "blockers" {
        return Some(public_verb_help(if verb == "blockers" {
            "unblock"
        } else {
            "start"
        }));
    }
    let (_, summary) = ADVANCED_VERBS.iter().find(|(name, _)| *name == verb)?;
    let public = format!("aethyme broker advanced {verb}");
    let text = blocks(verb, &public);
    if text.trim().is_empty() {
        return Some(format!("  {public} [--json]\n      {summary}\n"));
    }
    Some(text)
}

/// The `USAGE` blocks for internal command `internal`, with the command
/// spelling on each usage line rewritten to `public`.
fn blocks(internal: &str, public: &str) -> String {
    blocks_with_prefix(
        &format!("  aethyme broker {internal}"),
        &format!("aethyme broker {internal}"),
        public,
    )
}

fn blocks_with_prefix(prefix: &str, spelling: &str, public: &str) -> String {
    let mut text = String::new();
    let mut in_block = false;
    for line in USAGE.lines() {
        let is_usage = line.starts_with("  aethyme ");
        if is_usage {
            let rest = line.strip_prefix(prefix);
            in_block = rest.is_some_and(|rest| rest.is_empty() || rest.starts_with(' '));
            if in_block {
                text.push_str(&line.replacen(spelling, public, 1));
                text.push('\n');
            }
            continue;
        }
        if in_block && line.starts_with("      ") {
            text.push_str(line);
            text.push('\n');
        } else {
            in_block = false;
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn public_sub_forms_resolve_to_the_internal_implementation() {
        for (public, internal) in [
            ("status readiness plan --json", "readiness plan --json"),
            ("status doctor", "doctor"),
            ("submit prepare --session 3", "prepare --session 3"),
            ("submit promote --entry 4", "promote --entry 4"),
            ("submit promotion-record plan", "promotion-record plan"),
            ("finish close --session 3", "close --session 3"),
            ("finish cleanup 3 --force", "cleanup 3 --force"),
            ("gc reclaim plan", "reclaim plan"),
            ("gc storage apply --confirm x", "storage apply --confirm x"),
            ("gc reap --json", "resources reap --json"),
            ("unblock", "blockers"),
            ("unblock --json", "blockers --json"),
            ("unblock op:3 --reason r", "unblock op:3 --reason r"),
            ("gc plan", "gc plan"),
            (
                "advanced leases claim src/ --session 1",
                "leases claim src/ --session 1",
            ),
        ] {
            let resolved = resolve(&args(public));
            assert_eq!(resolved.args, args(internal), "{public}");
            assert_eq!(resolved.refusal, None, "{public}");
        }
    }

    #[test]
    fn removed_spellings_are_refused_with_their_replacement() {
        for (line, replacement) in [
            ("adopt --reuse --task t", "aethyme broker start --reuse"),
            (
                "start-agent --task t --cmd c",
                "aethyme broker start --cmd <command>",
            ),
            ("readiness", "aethyme broker status readiness"),
            ("doctor", "aethyme broker status doctor"),
            ("prepare --session 1", "aethyme broker submit prepare"),
            ("close --session 1", "aethyme broker finish close"),
            ("blockers", "aethyme broker unblock"),
            ("resources reap", "aethyme broker gc reap"),
            ("leases claim src/", "aethyme broker advanced leases"),
        ] {
            let refusal = resolve(&args(line))
                .refusal
                .unwrap_or_else(|| panic!("{line} should be refused"));
            assert!(refusal.contains(REMOVED_SPELLING_RELEASE), "{refusal}");
            assert!(refusal.contains(replacement), "{refusal}");
        }
    }

    #[test]
    fn public_verbs_under_advanced_are_refused_not_stripped() {
        for (line, public) in [
            ("advanced status doctor --json", "status doctor --json"),
            (
                "advanced status readiness recover",
                "status readiness recover",
            ),
            (
                "advanced submit promote --entry 1",
                "submit promote --entry 1",
            ),
            (
                "advanced submit prepare --session 1",
                "submit prepare --session 1",
            ),
            ("advanced gc reap", "gc reap"),
            ("advanced unblock", "unblock"),
            ("advanced advanced leases", "advanced leases"),
        ] {
            let resolved = resolve(&args(line));
            let refusal = resolved
                .refusal
                .unwrap_or_else(|| panic!("{line} should be refused"));
            assert!(
                refusal.contains(&format!("use 'aethyme broker {public}'")),
                "{line}: {refusal}"
            );
        }
    }

    /// Public spellings used by the router and in-process broker entry point.
    #[test]
    fn canonical_spellings_resolve_without_refusal() {
        let mut corpus: Vec<String> = Vec::new();
        for (verb, _) in PUBLIC_VERBS {
            corpus.push(format!("{verb} --json"));
            for (_, word, _) in MERGED_FORMS.iter().filter(|(owner, _, _)| *owner == *verb) {
                corpus.push(format!("{verb} {word} plan"));
            }
        }
        for (verb, _) in ADVANCED_VERBS {
            corpus.push(format!("advanced {verb} --json"));
        }
        for extra in ["start --reuse", "advanced leases claim", "unblock"] {
            corpus.push(extra.to_string());
        }
        for line in corpus {
            let once = resolve(&args(&line));
            assert_eq!(once.refusal, None, "{line}");
        }
    }

    #[test]
    fn current_spellings_and_machine_entry_points_are_not_refused() {
        for line in [
            "start --task t",
            "status --json",
            "submit --session 1",
            "finish --session 1",
            "gc plan",
            "unblock op:1",
            "advanced leases",
            "advanced hooks pre-commit",
            "advanced hooks post-commit",
            "advanced hooks pre-push",
            "quick-test",
            "check-contract --base main",
            "no-such-verb",
        ] {
            assert_eq!(resolve(&args(line)).refusal, None, "{line}");
        }
    }

    #[test]
    fn every_public_and_advanced_verb_has_help() {
        for (verb, _) in PUBLIC_VERBS {
            let text = help_text(&args(&format!("{verb} --help"))).unwrap();
            assert!(text.contains(&format!("aethyme broker {verb}")), "{verb}");
        }
        for (verb, _) in ADVANCED_VERBS {
            let text = help_text(&args(&format!("advanced {verb} --help"))).unwrap();
            assert!(
                text.contains(&format!("aethyme broker advanced {verb}")),
                "{verb}: {text}"
            );
        }
        let top = help_text(&args("--help")).unwrap();
        for (verb, _) in PUBLIC_VERBS {
            assert!(top.contains(&format!("  {verb} ")), "{verb}");
        }
        assert!(help_text(&args("no-such-verb --help")).is_none());
        assert!(help_text(&args("readiness --help")).is_none());
    }

    #[test]
    fn help_is_detected_only_before_the_command_separator() {
        assert!(wants_help(&args("start --help")));
        assert!(wants_help(&args("gc -h")));
        assert!(!wants_help(&args("exec --session 1 -- ls --help")));
        assert!(!wants_help(&args("git --session 1 -- log -h")));
    }
}
