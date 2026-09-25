//! Shared command-line front end for `explore`.
//!
//! Both binaries expose `explore` — `aethyme-engine-cli` (the engine's
//! operational binary) and the top-level `aethyme` router — and both must
//! parse the same flags and produce the same answer-json. This module owns
//! that parsing and dispatch so the two front ends cannot drift; the
//! binaries translate [`ExploreCliOutcome`] into their own exit-code
//! contracts. The non-usage-boundary native explore path reads redb directly;
//! the daemon outcome remains only for older callers that still surface that
//! process-management distinction.

use std::path::PathBuf;

use crate::explore;

/// Result of one explore CLI invocation. No process exits happen inside
/// this module — the caller owns exit codes and fallback policy.
pub enum ExploreCliOutcome {
    /// Answer printed to stdout.
    Done,
    /// Legacy process-management outcome for callers that still distinguish
    /// daemon startup from ordinary query failures.
    DaemonNotRunning { repo: PathBuf },
    /// Caller error (bad flags / bad params) — exit-2 semantics.
    BadUsage(String),
    /// Execution failure — exit-1 semantics.
    Failed(String),
}

/// Output formats `explore` prints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// The `aethyme-explore-v1` document. The default and the machine
    /// surface: cross-process consumers and the eval runner parse it.
    AnswerJson,
    /// A text summary plus the top verified source spans, for an agent to
    /// read in one call (see [`crate::explore_brief`]).
    Brief,
}

fn parse_format(args: &[String]) -> Result<Format, String> {
    match read_option(args, "--format").as_deref() {
        Err(_) | Ok("answer-json") => Ok(Format::AnswerJson),
        Ok("brief") => Ok(Format::Brief),
        Ok(other) => Err(format!(
            "explore: unknown --format {other:?}; expected answer-json or brief"
        )),
    }
}

/// `--repo`, defaulting to the current directory. The default is what lets
/// the guidance prescribe one short command from inside a checkout.
fn read_repo(args: &[String]) -> String {
    read_option(args, "--repo").unwrap_or_else(|_| ".".to_string())
}

pub fn run(args: &[String]) -> ExploreCliOutcome {
    let repo_str = read_repo(args);
    let request = match read_option(args, "--request") {
        Ok(v) => v,
        Err(e) => return ExploreCliOutcome::BadUsage(e),
    };
    let format = match parse_format(args) {
        Ok(format) => format,
        Err(message) => return ExploreCliOutcome::BadUsage(message),
    };
    let detail = read_option(args, "--detail").unwrap_or_else(|_| "compact".to_string());
    let detail_enum = match detail.as_str() {
        "compact" => explore::Detail::Compact,
        "standard" => explore::Detail::Standard,
        "full" => explore::Detail::Full,
        other => {
            return ExploreCliOutcome::BadUsage(format!("explore: unknown --detail {other:?}"));
        }
    };

    let max_answer_items: usize = read_option(args, "--max-answer-items")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    // --depth N (0..=3) selects a rung on the progressive-disclosure
    // ladder defined in `DISCLOSURE_LEVELS`. Overrides --detail and
    // --max-answer-items when set: the depth table is the budget,
    // not the per-knob defaults. When --depth is omitted, behavior is
    // pre-2026-05-09 (legacy detail-based path).
    let depth: Option<u8> = match read_option(args, "--depth") {
        Ok(s) => match s.parse::<u8>() {
            Ok(n) if (n as usize) < explore::DISCLOSURE_LEVELS.len() => Some(n),
            Ok(n) => {
                return ExploreCliOutcome::BadUsage(format!(
                    "explore: --depth {n} out of range (0..={})",
                    explore::DISCLOSURE_LEVELS.len() - 1,
                ));
            }
            Err(e) => {
                return ExploreCliOutcome::BadUsage(format!("explore: --depth must be 0..=3: {e}"));
            }
        },
        // The brief is the one-call discovery surface, so it starts on the
        // discovery rung unless the caller chose a budget.
        Err(_) if format == Format::Brief && !has_flag_value(args, "--detail") => Some(0),
        Err(_) => None,
    };
    // The brief reports readiness, which lives in the observability block.
    let show_observability = has_flag(args, "--show-observability") || format == Format::Brief;

    // --intent picks the orchestration shape. The default when no
    // --intent is passed is `auto`: scan the first ~10 tokens of the
    // request for change-verbs and pick behavior_localization when
    // we see one, else task_localization.
    let intent_str = read_option(args, "--intent").unwrap_or_else(|_| "auto".to_string());
    let (intent, intent_source) = match intent_str.as_str() {
        "task_localization_query" | "default" => (
            explore::Intent::TaskLocalization,
            explore::IntentSource::Explicit,
        ),
        "behavior_localization_query" | "behavior" => (
            explore::Intent::BehaviorLocalization,
            explore::IntentSource::Explicit,
        ),
        "auto" | "" => (
            explore::Intent::auto_select(&request),
            explore::IntentSource::Auto,
        ),
        "usage_boundary_query" => {
            // Different orchestrator: hybrid redb seed discovery plus
            // source-text evidence scanning.
            return run_usage_boundary(args, &repo_str, &request, format);
        }
        other => {
            return ExploreCliOutcome::BadUsage(format!("explore: unknown --intent {other:?}"));
        }
    };

    let mut params = explore::ExploreParams {
        max_answer_items,
        detail: detail_enum,
        depth,
        show_observability,
        ..explore::ExploreParams::default()
    };
    // When --depth is set, the disclosure-level table overrides the
    // per-knob defaults. Apply AFTER struct construction so the
    // table values land on the right fields without callers having
    // to remember the order.
    params.apply_disclosure_level();

    let repo = PathBuf::from(&repo_str);
    if !repo.is_dir() {
        return ExploreCliOutcome::BadUsage(format!("--repo path is not a directory: {repo_str}"));
    }

    match explore::explore_with_intent(&repo, &request, intent, intent_source, &params) {
        Ok(response) => emit(&repo, format, &response),
        Err(explore::ExploreError::DaemonNotRunning) => {
            ExploreCliOutcome::DaemonNotRunning { repo }
        }
        Err(explore::ExploreError::GraphUnavailable { status, reason }) => print_graph_unavailable(
            &repo,
            &request,
            (intent.as_str(), intent_source.as_str()),
            (status, reason),
            detail_enum,
            show_observability,
            format,
        ),
        Err(other) => ExploreCliOutcome::Failed(format!("explore: {other}")),
    }
}

/// Run the `usage_boundary_query` intent path. This remains hybrid: redb
/// chooses candidates and source text supplies evidence.
fn run_usage_boundary(
    args: &[String],
    repo_str: &str,
    request: &str,
    format: Format,
) -> ExploreCliOutcome {
    let scope = match read_option(args, "--scope") {
        Ok(v) => v,
        Err(_) => {
            return ExploreCliOutcome::BadUsage(
                "usage_boundary_query requires --scope <repo-relative-path>".to_string(),
            );
        }
    };
    let search_roots: Vec<String> = read_options(args, "--search-root");
    let include_methods = !has_flag(args, "--no-methods");
    let budget_ms: u64 = read_option(args, "--budget-ms")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10_000);
    let max_evidence: usize = read_option(args, "--max-evidence-per-symbol")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(5);
    let max_answer_items: usize = read_option(args, "--max-answer-items")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25);

    let params = explore::UsageBoundaryParams {
        scope,
        search_roots,
        include_methods,
        budget_ms,
        max_evidence_per_symbol: max_evidence,
        max_answer_items,
    };

    let repo = PathBuf::from(repo_str);
    if !repo.is_dir() {
        return ExploreCliOutcome::BadUsage(format!("--repo path is not a directory: {repo_str}"));
    }

    match explore::explore_usage_boundary(&repo, request, &params) {
        Ok(response) => emit(&repo, format, &response),
        Err(explore::ExploreError::BadParams(msg)) => {
            ExploreCliOutcome::BadUsage(format!("explore (usage_boundary_query): {msg}"))
        }
        Err(explore::ExploreError::GraphUnavailable { status, reason }) => {
            let detail = match read_option(args, "--detail").as_deref() {
                Ok("full") => explore::Detail::Full,
                Ok("standard") => explore::Detail::Standard,
                _ => explore::Detail::Compact,
            };
            print_graph_unavailable(
                &repo,
                request,
                ("usage_boundary_query", "explicit"),
                (status, reason),
                detail,
                has_flag(args, "--show-observability") || format == Format::Brief,
                format,
            )
        }
        Err(err) => ExploreCliOutcome::Failed(format!("explore (usage_boundary_query): {err}")),
    }
}

/// Print the graph-free envelope shaped for the caller's output profile
/// (see [`explore::project_graph_free_output`]).
fn print_graph_unavailable(
    repo: &std::path::Path,
    request: &str,
    (intent, intent_source): (&'static str, &'static str),
    (status, reason): (&'static str, String),
    detail: explore::Detail,
    show_observability: bool,
    format: Format,
) -> ExploreCliOutcome {
    let response = explore::project_graph_free_output(
        explore::graph_unavailable_response(repo, request, intent, intent_source, status, reason),
        detail,
        show_observability,
    );
    emit(repo, format, &response)
}

/// Print one answer document in the requested format.
fn emit<T: serde::Serialize>(
    repo: &std::path::Path,
    format: Format,
    response: &T,
) -> ExploreCliOutcome {
    match format {
        Format::AnswerJson => match serde_json::to_string_pretty(response) {
            Ok(json) => {
                println!("{json}");
                ExploreCliOutcome::Done
            }
            Err(error) => ExploreCliOutcome::Failed(format!("serialize response: {error}")),
        },
        Format::Brief => match serde_json::to_value(response) {
            Ok(answer) => {
                let root = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
                print!("{}", crate::explore_brief::render(&root, &answer));
                ExploreCliOutcome::Done
            }
            Err(error) => ExploreCliOutcome::Failed(format!("serialize response: {error}")),
        },
    }
}

// Minimal flag helpers, local to this module so both binaries share one
// parsing behavior for explore without depending on bin-side helpers.

fn read_option(args: &[String], flag: &str) -> Result<String, String> {
    args.windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| format!("missing required option: {flag}"))
}

fn read_options(args: &[String], flag: &str) -> Vec<String> {
    args.windows(2)
        .filter(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .collect()
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

fn has_flag_value(args: &[String], flag: &str) -> bool {
    read_option(args, flag).is_ok()
}
