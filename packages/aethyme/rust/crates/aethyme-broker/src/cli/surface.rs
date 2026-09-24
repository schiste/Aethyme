//! The public broker verb surface: six verbs, `advanced`, and the deprecation
//! window for every older spelling.
//!
//! This module only translates spellings. Each public form resolves to the
//! internal subcommand that already implemented it, and that implementation
//! runs unchanged: `submit`, `promote`, gates and cleanup keep their exact
//! code paths. The internal names are what `run_inner`, `FLAG_RULES`, command
//! metrics and the router's compatibility policy continue to key on.

use super::USAGE;

/// The release that removes every deprecated broker and top-level spelling.
/// The single place the deprecation window is recorded.
pub const DEPRECATED_SPELLING_REMOVAL_RELEASE: &str = "v0.8.6";

/// The one stderr line for a deprecated spelling. Never printed on stdout, so
/// `--json` output stays parseable.
pub fn deprecation_warning(old: &str, new: &str) -> String {
    format!(
        "warning: '{old}' is deprecated; use '{new}' (the old spelling is removed in {DEPRECATED_SPELLING_REMOVAL_RELEASE})"
    )
}

/// The six verbs `aethyme broker --help` lists.
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
        "finish",
        "close a completed session; state-only close and worktree cleanup",
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

/// Verbs reachable as `aethyme broker advanced <verb>`, with the one-line
/// summary `broker advanced --help` prints.
pub const ADVANCED_VERBS: &[(&str, &str)] = &[
    (
        "leases",
        "inspect, claim, plan, export or release path ownership",
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

/// A deprecated spelling and its replacement, both as full commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deprecation {
    pub old: String,
    pub new: String,
}

impl Deprecation {
    pub fn warning(&self) -> String {
        deprecation_warning(&self.old, &self.new)
    }
}

/// A broker command line translated to its internal spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The command line `run_inner` dispatches (no `advanced`, no public
    /// sub-form words).
    pub args: Vec<String>,
    /// Set when the caller used an old spelling.
    pub deprecation: Option<Deprecation>,
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

/// Translate `args` (after `broker`) to the internal spelling. Idempotent:
/// resolving an already-internal command line returns it unchanged.
pub fn resolve(args: &[String]) -> Resolution {
    let Some(first) = args.first().map(String::as_str) else {
        return Resolution {
            args: Vec::new(),
            deprecation: None,
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
                deprecation: None,
                refusal: Some(format!(
                    "`{verb}` is not an advanced verb; use 'aethyme broker {}'",
                    internal.join(" ")
                )),
            };
        }
        let deprecation = old_spelling_replacement(&internal, true);
        return Resolution {
            args: internal,
            deprecation,
            refusal: None,
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
                deprecation: None,
                refusal: None,
            };
        }
        // `unblock` with nothing to clear lists what could be cleared.
        if first == "unblock" && args[1..].iter().all(|arg| arg == "--json") {
            return Resolution {
                args: words(&["blockers"], &args[1..]),
                deprecation: None,
                refusal: None,
            };
        }
        return Resolution {
            args: args.to_vec(),
            deprecation: None,
            refusal: None,
        };
    }
    Resolution {
        args: args.to_vec(),
        deprecation: old_spelling_replacement(args, false),
        refusal: None,
    }
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == flag)
}

/// The replacement for an old internal spelling, or `None` when the spelling
/// is current (or unknown, which the dispatcher reports).
fn old_spelling_replacement(args: &[String], via_advanced: bool) -> Option<Deprecation> {
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
    Some(Deprecation { old, new })
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
        "aethyme broker advanced — the full broker surface beyond the six public verbs\n\n\
         Usage: aethyme broker advanced <verb> [args...]\n\n\
         Verbs:\n",
    );
    for (verb, summary) in ADVANCED_VERBS {
        text.push_str(&format!("  {verb:<16} {summary}\n"));
    }
    text.push_str(
        "\nRun `aethyme broker advanced <verb> --help` for a verb's forms and flags.\n\
         Until the deprecation window closes, `aethyme broker <verb>` still works\n\
         for each of these and prints a warning naming the `advanced` spelling.\n",
    );
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
    fn public_sub_forms_resolve_to_the_old_implementation_without_warning() {
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
            assert_eq!(resolved.deprecation, None, "{public}");
        }
    }

    #[test]
    fn resolution_is_idempotent_on_internal_spellings() {
        for line in [
            "readiness plan",
            "adopt --reuse",
            "leases claim a --session 1",
            "resources reap",
            "blockers --json",
        ] {
            let once = resolve(&args(line)).args;
            assert_eq!(resolve(&once).args, once, "{line}");
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

    /// The router resolves once and hands the result to the broker, which
    /// resolves again: both must see the identical command, or the router's
    /// compatibility classification describes a different command than the
    /// one that runs.
    #[test]
    fn resolution_reaches_its_fixpoint_in_one_step() {
        let mut corpus: Vec<String> = Vec::new();
        for (verb, _) in PUBLIC_VERBS.iter().chain(ADVANCED_VERBS) {
            corpus.push(format!("{verb} --json"));
            corpus.push(format!("advanced {verb} --json"));
            for (_, word, _) in MERGED_FORMS {
                corpus.push(format!("{verb} {word} plan"));
                corpus.push(format!("advanced {verb} {word} plan"));
            }
        }
        for (_, _, internal) in MERGED_FORMS {
            corpus.push(internal.join(" "));
            corpus.push(format!("advanced {}", internal.join(" ")));
        }
        for extra in ["adopt --reuse", "start-agent", "blockers", "init", "e2e"] {
            corpus.push(extra.to_string());
            corpus.push(format!("advanced {extra}"));
        }
        for line in corpus {
            let once = resolve(&args(&line));
            if once.refusal.is_some() {
                continue;
            }
            let twice = resolve(&once.args);
            assert_eq!(twice.args, once.args, "{line}");
            assert_eq!(twice.refusal, None, "{line}");
        }
    }

    #[test]
    fn old_spellings_warn_with_the_new_spelling() {
        for (old, new) in [
            ("adopt --reuse --task t", "aethyme broker start --reuse"),
            (
                "adopt --replace-stale",
                "aethyme broker start --replace-stale",
            ),
            ("adopt --task t", "aethyme broker start --adopt"),
            (
                "start-agent --task t --cmd c",
                "aethyme broker start --cmd <command>",
            ),
            ("readiness", "aethyme broker status readiness"),
            ("doctor", "aethyme broker status doctor"),
            ("prepare --session 1", "aethyme broker submit prepare"),
            ("promote --entry 1", "aethyme broker submit promote"),
            (
                "promotion-record plan",
                "aethyme broker submit promotion-record",
            ),
            ("close --session 1", "aethyme broker finish close"),
            ("cleanup 1", "aethyme broker finish cleanup"),
            ("blockers", "aethyme broker unblock"),
            ("reclaim plan", "aethyme broker gc reclaim"),
            ("storage", "aethyme broker gc storage"),
            ("resources reap", "aethyme broker gc reap"),
            ("init", "aethyme init"),
            ("certify --json", "aethyme certify"),
            (
                "leases claim a --session 1",
                "aethyme broker advanced leases",
            ),
            ("git --session 1 -- status", "aethyme broker advanced git"),
            ("e2e", "aethyme broker advanced verify-loop"),
        ] {
            let deprecation = resolve(&args(old))
                .deprecation
                .unwrap_or_else(|| panic!("{old} should be deprecated"));
            assert_eq!(deprecation.new, new, "{old}");
            let warning = deprecation.warning();
            assert!(
                warning.starts_with("warning: 'aethyme broker "),
                "{warning}"
            );
            assert!(warning.contains(DEPRECATED_SPELLING_REMOVAL_RELEASE));
            assert_eq!(warning.lines().count(), 1);
        }
    }

    #[test]
    fn current_spellings_and_machine_entry_points_do_not_warn() {
        for line in [
            "start --task t",
            "status --json",
            "submit --session 1",
            "finish --session 1",
            "gc plan",
            "unblock op:1",
            "advanced leases",
            "hooks pre-commit",
            "hooks post-commit",
            "hooks pre-push",
            "quick-test",
            "check-contract --base main",
            "no-such-verb",
        ] {
            assert_eq!(resolve(&args(line)).deprecation, None, "{line}");
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
    }

    #[test]
    fn help_is_detected_only_before_the_command_separator() {
        assert!(wants_help(&args("start --help")));
        assert!(wants_help(&args("gc -h")));
        assert!(!wants_help(&args("exec --session 1 -- ls --help")));
        assert!(!wants_help(&args("git --session 1 -- log -h")));
    }
}
