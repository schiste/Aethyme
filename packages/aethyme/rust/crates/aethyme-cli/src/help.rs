//! `--help` for every top-level command, answered by the router before any
//! command runs (Phase 4, P4.3).
//!
//! Several groups used to treat `--help` as an ordinary argument: `graph`,
//! `enhance`, `task` and friends errored, and `graph materialize --help`
//! defaulted `--repo` to `.` and rebuilt the derived graph store. The router
//! now intercepts `-h`/`--help` (before any `--` separator) and never
//! dispatches, so asking what a command does cannot do any of it.
//!
//! Groups whose own help is already safe keep it: the router forwards to the
//! group's help (dropping any subcommand) or, where the whole group checks for
//! `--help` before acting, passes the command line through unchanged.

/// How a top-level command answers `--help`.
pub enum HelpRoute {
    /// Print this text on stdout.
    Text(String),
    /// Run the command with exactly these arguments; it prints its own help.
    Native(Vec<String>),
    /// Hand the command line to the broker CLI, which answers help itself.
    Broker,
}

/// Commands whose group-level `--help` is safe and complete. Subcommand help
/// is answered with the group's help so no subcommand parser ever sees it.
const GROUP_NATIVE: &[&str] = &[
    "intents", "update", "plugin", "ai-ready", "quality", "autofix",
];

/// Commands that check for `--help` anywhere before doing anything, so the
/// full command line can be passed through.
const PASSTHROUGH_NATIVE: &[&str] = &["deploy", "upgrade"];

/// Commands answered by the broker CLI's help surface.
const BROKER_HELP: &[&str] = &["broker", "certify", "init", "readiness"];

/// `-h`/`--help` anywhere before a `--` command separator.
pub fn wants_help(args: &[String]) -> bool {
    args.iter()
        .take_while(|arg| *arg != "--")
        .any(|arg| arg == "-h" || arg == "--help")
}

/// How to answer `--help` for `args` (the full command line after
/// `aethyme`), or `None` when the command is unknown.
pub fn route(args: &[String]) -> Option<HelpRoute> {
    let command = args.first()?.as_str();
    if BROKER_HELP.contains(&command) {
        return Some(HelpRoute::Broker);
    }
    if PASSTHROUGH_NATIVE.contains(&command) {
        return Some(HelpRoute::Native(args.to_vec()));
    }
    if GROUP_NATIVE.contains(&command) {
        return Some(HelpRoute::Native(vec![
            command.to_string(),
            "--help".to_string(),
        ]));
    }
    let (_, text) = GROUP_TEXT.iter().find(|(name, _)| *name == command)?;
    let words: Vec<&str> = args[1..]
        .iter()
        .map(String::as_str)
        .take_while(|arg| *arg != "--")
        .filter(|arg| !arg.starts_with('-'))
        .collect();
    let subcommand = words.first().copied();
    let focused = subcommand
        .map(|sub| blocks(text, &format!("  aethyme {command} {sub}")))
        .unwrap_or_default();
    Some(HelpRoute::Text(if focused.is_empty() {
        (*text).to_string()
    } else {
        focused
    }))
}

/// The usage lines starting with `prefix` and their indented descriptions.
fn blocks(text: &str, prefix: &str) -> String {
    let mut found = String::new();
    let mut in_block = false;
    for line in text.lines() {
        if line.starts_with("  aethyme ") {
            let rest = line.strip_prefix(prefix);
            in_block = rest.is_some_and(|rest| rest.is_empty() || rest.starts_with(' '));
        } else if !line.starts_with("    ") {
            in_block = false;
        }
        if in_block {
            found.push_str(line);
            found.push('\n');
        }
    }
    found
}

/// Help text for commands whose parsers do not answer `--help` themselves.
/// Each usage line starts with two spaces and `aethyme <group> <sub>`;
/// description lines are indented further.
const GROUP_TEXT: &[(&str, &str)] = &[
    (
        "explore",
        r#"aethyme explore — bounded Explore query; prints answer-json

Usage:
  aethyme explore --repo <path> --request <text> [--format answer-json]
                  [--intent auto|default|behavior|usage_boundary_query]
                  [--detail compact|standard|full] [--depth 0-3]
                  [--max-answer-items <n>] [--show-observability]
      --repo <path>           Repository directory (required).
      --request <text>        Task or question to localize (required).
      --format answer-json    Only answer-json is supported (default).
      --intent <name>         auto (default) picks from the request's verbs;
                              default|task_localization_query,
                              behavior|behavior_localization_query,
                              usage_boundary_query (needs --scope).
      --detail <level>        compact (default), standard, or full.
      --depth <0-3>           Progressive-disclosure rung; overrides --detail
                              and --max-answer-items when set.
      --max-answer-items <n>  Answer items to return (default 5; 25 for
                              usage_boundary_query).
      --show-observability    Include the observability block.
    usage_boundary_query only:
      --scope <path>          Repo-relative boundary path (required).
      --search-root <path>    Restrict evidence search; repeatable.
      --no-methods            Exclude methods from the symbol set.
      --budget-ms <n>         Time budget in ms (default 10000).
      --max-evidence-per-symbol <n>  Evidence items per symbol (default 5).
    Starts the engine daemon automatically when it is not running.
"#,
    ),
    (
        "verify-targets",
        r#"aethyme verify-targets — bounded source spans for Explore targets

Usage:
  aethyme verify-targets --from <file|-> [--repo <path>] [--max-targets <n>] [--max-lines <n>]
      --from <file|->     Saved `explore --format answer-json` output; `-` reads stdin (required).
      --repo <path>       Repository the spans are read from (default: current directory).
      --max-targets <n>   Verification targets to print, > 0 (default 2).
      --max-lines <n>     Maximum source lines per target, > 0 (default 80).
"#,
    ),
    (
        "explore-summary",
        r#"aethyme explore-summary — compact decision surface from a saved answer-json

Usage:
  aethyme explore-summary --from <file|->
      --from <file|->     Saved `explore --format answer-json` output; `-` reads stdin (required).
                          Prints the trust policy, subsystems, verification targets and steps,
                          and readiness as JSON.
"#,
    ),
    (
        "graph",
        r#"aethyme graph — repository graph lifecycle and navigation

Usage:
  aethyme graph status [--repo <path>] [--json]
      Report fragment, derived-store, and compatibility health (default repo: .).
  aethyme graph units [--repo <path>] [--revision <sha>] [--cursor <token>] [--limit <n>] [--json]
      Page through committed graph units; --limit 1-1000 (default 100).
  aethyme graph materialize [--repo <path>] [--json]
      Build or refresh the derived graph store from committed fragments (writes the store).
  aethyme graph refresh plan [--repo <path>] [--json | --diff]
      Regenerate fragments from committed HEAD in a disposable clone and print the plan.
  aethyme graph refresh execute --confirm <plan-sha256> [--repo <path>] [--json]
      Apply a reviewed refresh plan; refuses if state changed since the plan.
  aethyme graph refresh recover --plan <plan-sha256> [--repo <path>]
      Recover an interrupted refresh from its journal.
  aethyme graph impact --repo <path> --revision <rev> --diff <file|text>
                       [--mode calls|imports] [--budget <n>] [--json]
      Impact report for a diff: callers/importers, tests, configs, risk hints (budget default 128).
  aethyme graph node <repo_path> <target> [--json-output]
      Show one node. <target>: node id, file path, area, or symbol (path::name or name).
  aethyme graph children <repo_path> <target> [--json-output]
      Contained nodes.
  aethyme graph parents <repo_path> <target> [--json-output]
      Containing nodes.
  aethyme graph callers <repo_path> <target> [--json-output]
      Call-graph callers.
  aethyme graph callees <repo_path> <target> [--json-output]
      Call-graph callees.
  aethyme graph docs <repo_path> <target> [--json-output]
      Documentation linked to the target.
  aethyme graph configs <repo_path> <target> [--json-output]
      Configuration linked to the target.
  aethyme graph expand <repo_path> <target> [--json-output]
      The target with its immediate neighborhood.
  aethyme graph overview <repo_path> [--json-output]
      Repository overview from the graph store.
"#,
    ),
    (
        "analyze",
        r#"aethyme analyze — repository analyzers

Usage:
  aethyme analyze dead-code --repo <path> --scope <prefix> [--roots <a,b,...>]
                            [--include-methods] [--boundary outside-directory]
                            [--format summary|full-json|eval-json] [--json-output]
                            [--show-observability]
      Find functions under <prefix> with no callers outside it.
      --roots: comma-separated entry roots; --json-output implies --format full-json;
      --show-observability adds observability to eval-json.
"#,
    ),
    (
        "facts",
        r#"aethyme facts — derived function facts

Usage:
  aethyme facts public-functions --repo <path> --scope <prefix> [--include-methods] [--json-output]
      List public functions defined under <prefix> and how each is exposed.
  aethyme facts function-usage --repo <path> --target <fn> --boundary <prefix>
                               [--roots <a,b,...>] [--json-output]
      Internal and external callers of one function (<fn>: id, name, or qualified name).
"#,
    ),
    (
        "task",
        r#"aethyme task — task-scoped context from the graph store

Usage:
  aethyme task pack --repo <path> --task <text> [--json-output]
      Build a context pack for the task.
  aethyme task context --repo <path> --task <text> [--content-budget <n>] [--json-output]
      Context pack with file content inlined (budget default 80000).
  aethyme task anchors --repo <path> --task <text> [--json-output]
      Anchor nodes for the task, with reasons.
  aethyme task scope --repo <path> --task <text> [--json-output]
      Suggested edit scope for the task.
  aethyme task next --repo <path> --task <text> [--json-output]
      Next nodes to inspect.
  aethyme task expand --repo <path> --node <target> [--json-output]
      Expand one node in task context.
  aethyme task explain --repo <path> [--task <text>]
      Plain-text explanation (default task: "Explain this repo").
"#,
    ),
    (
        "query",
        r#"aethyme query — direct graph-store lookups

Usage:
  aethyme query symbol <repo_path> <query> [--json-output]
      Search symbols by name (top 20 hits).
  aethyme query deps <repo_path> <node-id>
      Files the node depends on (outgoing edges), e.g. file:<repo>:<path>.
  aethyme query impact <repo_path> <target>
      Impact frontier for the target.
"#,
    ),
    (
        "repo",
        r#"aethyme repo — repository utilities, skills, overrides, telemetry, commit hygiene

Usage:
  aethyme repo ingest <repo_path>
      Build the repository map; report file count and graph-store presence.
  aethyme repo inspect <repo_path> [--mode brief|structure|full] [--json-output]
      Show the repository map (default mode: full).
  aethyme repo warm <repo_path>
      Build the repository map once.
  aethyme repo clear-cache <repo_path>
      Delete the legacy output cache root ($AETHYME_CACHE_DIR, default /tmp/aethyme-cache).
  aethyme repo engine-info [--repo <path>] [--check] [--json-output]
      Binary, version, graph store, and daemon status; --check exits 1 when not ready.
  aethyme repo deploy-skills <repo_path> [--force | --remove]
      Compatibility path; prefer `aethyme deploy`.
  aethyme repo compile-skills <repo_path> [--skill repo-onboarding]
      Regenerate the repo-onboarding skill files.
  aethyme repo init-onboarding-overrides <repo_path> [--force]
      Write the onboarding override template.
  aethyme repo validate-onboarding-overrides <repo_path>
      Validate the onboarding override file.
  aethyme repo init-agents-overrides <repo_path> [--force]
      Write the agents-policy override template.
  aethyme repo validate-agents-overrides <repo_path>
      Validate the agents-policy override file.
  aethyme repo experience-telemetry <repo_path> [--check] [--json-output]
      Report experience telemetry; --check exits 1 on attention signals.
  aethyme repo experience-status <repo_path> [--json-output]
      Write and print the experience-status artifacts.
  aethyme repo commit-message-template [--type <type>] [--scope <scope>]
      Print a typed commit template (defaults: fix, scope).
  aethyme repo lint-commit-message [<message_path> | --message <text>] [--json-output]
      Lint a commit message from a file, --message, or stdin.
  aethyme repo hook-envelope [--event <name>]
      Wrap stdin as a hook additionalContext envelope (default event: SessionStart).
  aethyme repo record-wrapper-invocation <repo_path> --wrapper <name> [--detail key=value ...]
      Record a wrapper invocation in the telemetry ledger (used by hooks).
"#,
    ),
    (
        "root",
        r#"aethyme root — locate the Aethyme package root (legacy compatibility)

Usage:
  aethyme root show
      Print the resolved root and how it was found (the default action).
  aethyme root set <path>
      Save the package or monorepo path to ~/.config/aethyme/root.
"#,
    ),
    (
        "hook",
        r#"aethyme hook — agent-surface hook entry point (called by the Aethyme plugin)

Usage:
  aethyme hook <SessionStart|UserPromptSubmit|PreToolUse|PostToolUse|Stop> [--repo <path>]
      Reads the event JSON on stdin and prints a hook envelope only when it has
      something new to say. Always exits 0; any failure is silent.
      --repo <path>   Checkout to act for (default: current directory).
"#,
    ),
    (
        "enhance",
        r#"aethyme enhance — deprecated spelling of `aethyme deploy`

Usage:
  aethyme enhance deploy --repo <path> [--force]
      Write or refresh the generated root files and skills; --force rewrites unchanged files.
      Prefer `aethyme deploy --repo <path>`, which also scaffolds the broker and
      drafts gates only when they are missing.
  aethyme enhance verify --repo <path>
      Check the generated files for missing files, placeholders, and drift; exits 1 on failure.
      Prefer `aethyme deploy verify --repo <path>`.
"#,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn subcommand_help_is_focused_and_unknown_subcommands_get_the_group() {
        let Some(HelpRoute::Text(text)) = route(&args("graph materialize --help")) else {
            panic!("graph help is router text");
        };
        assert!(text.starts_with("  aethyme graph materialize"), "{text}");
        assert!(!text.contains("graph status"), "{text}");
        let Some(HelpRoute::Text(text)) = route(&args("graph nonsense --help")) else {
            panic!("graph help is router text");
        };
        assert!(text.starts_with("aethyme graph —"), "{text}");
    }

    #[test]
    fn group_native_help_drops_the_subcommand() {
        let Some(HelpRoute::Native(forwarded)) = route(&args("update check --help")) else {
            panic!("update is group-native");
        };
        assert_eq!(forwarded, args("update --help"));
    }

    #[test]
    fn help_stops_at_the_command_separator() {
        assert!(wants_help(&args("graph --help")));
        assert!(!wants_help(&args("broker exec --session 1 -- ls --help")));
    }
}
