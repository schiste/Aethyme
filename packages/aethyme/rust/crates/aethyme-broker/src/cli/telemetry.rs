//! Command telemetry: the allowlisted command label and the outcome and timing records.

use std::cell::RefCell;

thread_local! {
    static COMMAND_METRIC_DETAILS: RefCell<Option<serde_json::Map<String, serde_json::Value>>> = const {
        RefCell::new(None)
    };
}

/// Attach command-specific fields to the metric written after this command
/// returns. The command runner takes and clears them exactly once.
pub(super) fn set_command_metric_details(details: serde_json::Value) {
    let Some(details) = details.as_object().cloned() else {
        return;
    };
    COMMAND_METRIC_DETAILS.with(|slot| *slot.borrow_mut() = Some(details));
}

fn take_command_metric_details() -> Option<serde_json::Map<String, serde_json::Value>> {
    COMMAND_METRIC_DETAILS.with(|slot| slot.borrow_mut().take())
}

/// Safe-by-construction command telemetry: the label is built ONLY from
/// an allowlist of known subcommand words, so positional values (paths,
/// session ids, task text) can never leak into the metrics file. Best
/// effort — any failure is silently ignored.
pub(super) const KNOWN_COMMAND_WORDS: &[&str] = &[
    "adopt",
    "commit",
    "start",
    "start-agent",
    "worktree-root",
    "exec",
    "exec-status",
    "git",
    "gh",
    "merge-chain",
    "operations",
    "stats",
    "blockers",
    "unblock",
    "advisories",
    "exposures",
    "note",
    "send",
    "list",
    "ack",
    "reconcile",
    "agents",
    "owners",
    "leases",
    "ownership",
    "export",
    "resources",
    "console",
    "prepare",
    "claim",
    "release",
    "gates",
    "draft",
    "validate",
    "manifest",
    "scope",
    "affected",
    "semantic",
    "run",
    "pre-push",
    "hooks",
    "install",
    "uninstall",
    "pre-commit",
    "post-commit",
    "pr",
    "check",
    "submit",
    "advance",
    "push",
    "sync",
    "repair",
    "checkpoint",
    "representation",
    "scan",
    "record",
    "apply",
    "queue",
    "plan",
    "sync-integration",
    "no-cache",
    "integration",
    "status",
    "events",
    "prune",
    "metrics",
    "insights",
    "doctor",
    "quick-test",
    "trust",
    "verify-loop",
    "e2e",
    "finish",
    // `finish close` resolves to `close` before a label is built. Without
    // the word here every state-only close went unrecorded, which hid the
    // lifecycle that leaves a whole checkout behind per task.
    "close",
    "handoff",
    "report",
    "external-events",
    "deliveries",
    "subscribe",
    "complete",
    "review",
    "register",
    "request",
    "unlock",
    "reassign",
    "abandon",
    "ingest",
    "capture",
    "cleanup",
    "gc",
    "storage",
    // `gc reclaim` resolves to `reclaim`. A plan attaches its phase timings
    // and counts, which is what records it; see `command_records_metric`.
    "reclaim",
    "certify",
    "readiness",
    "scaffold",
    "init",
];

fn known_command_word(word: &str) -> bool {
    KNOWN_COMMAND_WORDS.contains(&word)
        || super::surface::PUBLIC_VERBS
            .iter()
            .chain(super::surface::ADVANCED_VERBS)
            .any(|(name, _)| *name == word)
}

pub(super) fn safe_command_surface(args: &[String]) -> Option<String> {
    let first = args.first()?.as_str();
    if !known_command_word(first) {
        return None;
    }
    let mut words = vec![first];
    if let Some(second) = args.get(1).map(String::as_str)
        && known_command_word(second)
    {
        words.push(second);
    }
    Some(words.join("."))
}

/// Set when this invocation was a delivery poll that found nothing to claim
/// (#417). The PR monitor polls on a timer whether or not anything happened,
/// so these empty polls were most of the command history -- 79% of all
/// commands, about 130 an hour overnight with no session working -- and buried
/// the failures the history exists to show. A poll that claims, or fails, is
/// still recorded.
static IDLE_DELIVERY_POLL: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub(super) fn mark_idle_delivery_poll() {
    IDLE_DELIVERY_POLL.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether a successful invocation should leave no outcome event and no
/// metric line because it was an empty delivery poll.
pub(super) fn idle_delivery_poll(exit: u8) -> bool {
    exit == 0 && IDLE_DELIVERY_POLL.load(std::sync::atomic::Ordering::Relaxed)
}

thread_local! {
    static METRIC_SESSION_ID: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
    static WRAPPED_COMMAND_FAILURE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the current invocation's failure as the wrapped command's own exit,
/// not a broker failure. `exec` calls this only when the command ran, was not
/// timed out, and the ownership audit passed.
pub(super) fn mark_wrapped_command_failure() {
    WRAPPED_COMMAND_FAILURE.with(|flag| flag.set(true));
}

pub(super) fn set_metric_session(id: i64) {
    METRIC_SESSION_ID.with(|slot| slot.set(Some(id)));
}

fn metric_session() -> Option<i64> {
    METRIC_SESSION_ID.with(std::cell::Cell::get)
}

fn take_wrapped_command_failure() -> bool {
    WRAPPED_COMMAND_FAILURE.with(|flag| flag.replace(false))
}

/// Longest failure message kept with a `broker.command.failed` event. Enough
/// for an error and its first cause; the full text went to stderr.
pub(super) const FAILURE_MESSAGE_MAX_CHARS: usize = 500;

/// `error` is the text the command printed as `Error: …`, when it printed
/// one. It is stored redacted and capped so a failure can be explained after
/// the terminal that showed it is gone.
pub(super) fn record_command_outcome(args: &[String], exit: u8, error: Option<&str>) {
    let wrapped_command_failure = take_wrapped_command_failure();
    if !command_records_outcome(args) {
        return;
    }
    let Some(surface) = safe_command_surface(args) else {
        return;
    };
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(repo) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = repo.main_root() else {
        return;
    };
    // `open_current_in_repo`, not `open_in_repo`: this is a metric, and a
    // metric may not create a repository's broker state or migrate it. The
    // repository here came from the process working directory, which for a
    // spawned test binary is a checkout nobody asked this command to touch
    // (#163).
    let Ok(Some(mut store)) = crate::BrokerStore::open_current_in_repo(&main_root) else {
        return;
    };
    let session_id = metric_session().or_else(|| session_for_metric(args, &store, &repo));
    if let Some(id) = session_id {
        set_metric_session(id);
    }
    let command_surface = format!("broker.{surface}");
    let failure_class = (exit != 0).then_some(match args.first().map(String::as_str) {
        Some("submit") => "submission_failed",
        Some("repair") => "recovery_failed",
        Some("git" | "gh") => "coordinated_operation_failed",
        Some("exec") if wrapped_command_failure => crate::scoreboard::WRAPPED_COMMAND_FAILED,
        _ => "command_failed",
    });
    let message = (exit != 0)
        .then_some(error)
        .flatten()
        .and_then(|error| crate::text_redaction::failure_message(error, FAILURE_MESSAGE_MAX_CHARS));
    let payload = crate::events::broker_command_outcome_payload(
        &command_surface,
        exit,
        failure_class,
        None,
        None,
        message.as_deref(),
    );
    let kind = if exit == 0 {
        crate::events::BROKER_COMMAND_SUCCEEDED
    } else {
        crate::events::BROKER_COMMAND_FAILED
    };
    crate::warn_unrecorded(
        "record the command outcome event",
        store.append_event(kind, session_id, Some(&payload)),
    );
}

/// The name of the process that ran this command: its parent's executable
/// name, so a caller that polls an expensive form can be identified. Only the
/// final path component is kept -- never arguments, never a directory -- and
/// it is cut to 64 characters of `[A-Za-z0-9._+-]`. `None` when unknown.
pub(super) fn caller_hint() -> Option<String> {
    #[cfg(unix)]
    {
        let raw = parent_process_name(std::os::unix::process::parent_id())?;
        sanitize_process_name(&raw)
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
fn parent_process_name(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()
}

/// `ps -o comm=` prints the executable (on macOS its full path) without
/// arguments; only its last component survives `sanitize_process_name`.
#[cfg(all(unix, not(target_os = "linux")))]
fn parent_process_name(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-o", "comm=", "-p", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

fn sanitize_process_name(raw: &str) -> Option<String> {
    let name = raw.trim().rsplit(['/', '\\']).next().unwrap_or_default();
    // A login shell is reported as `-zsh`.
    let name = name.trim_start_matches('-');
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
        .take(64)
        .collect();
    (!name.is_empty()).then_some(name)
}

fn session_for_metric(
    args: &[String],
    store: &crate::BrokerStore,
    repo: &crate::GitRepo,
) -> Option<i64> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--session" {
            return args
                .next()?
                .parse::<i64>()
                .ok()
                .filter(|id| store.session(*id).is_ok());
        }
        if super::VALUE_FLAGS.contains(&arg.as_str()) {
            args.next();
        }
    }
    store
        .session_for_worktree(repo.root().to_string_lossy().as_ref())
        .ok()
        .flatten()
        .map(|session| session.id)
}

/// Record one command's cost. `output_bytes` counts stdout emitted through
/// `out!`; stderr is excluded because it carries diagnostics rather than the
/// payload an agent pays to read.
pub(super) fn record_command_metric(
    args: &[String],
    exit: u8,
    started: std::time::Instant,
    ended: std::time::Instant,
    output_bytes: u64,
    spans: &crate::phase_telemetry::Recorder,
) {
    let session_id = METRIC_SESSION_ID.with(|slot| slot.take());
    let duration_ms = (ended - started).as_millis() as i64;
    let details = take_command_metric_details();
    // Inspection commands are contractually side-effect free: the CLI documents
    // "never writes broker state or command telemetry" and `external_events_cli`
    // asserts the metrics file is byte-identical across them. That invariant
    // also hides the commands that dominate agent token cost, because reads are
    // the frequent, expensive ones. Measuring them is therefore opt-in: unset,
    // nothing changes; set, the operator has accepted that inspection now
    // writes one telemetry line.
    if !command_metric_enabled(args, output_measurement_opted_in(), details.is_some()) {
        return;
    }
    let Some(label) = safe_command_surface(args) else {
        return;
    };
    // Spans never decide whether a line is written: a read-only command stays
    // telemetry-free however much of its work was timed.
    let mut details = with_spans(details.unwrap_or_default(), spans);
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let Ok(repo) = crate::GitRepo::discover(&cwd) else {
        return;
    };
    let Ok(main_root) = repo.main_root() else {
        return;
    };
    let session_id = session_id.or_else(|| {
        let store = crate::BrokerStore::open_current_in_repo(&main_root)
            .ok()
            .flatten()?;
        session_for_metric(args, &store, &repo)
    });
    details.insert("session_id".into(), serde_json::json!(session_id));
    details.insert("attribution".into(), spans.attribution(started, ended));
    details.insert(
        "outcome".into(),
        serde_json::json!(match exit {
            crate::exit_status::SUCCESS => "succeeded",
            crate::exit_status::OUTCOME_UNKNOWN => "outcome_unknown",
            crate::exit_status::REFUSED => "refused",
            crate::exit_status::ENVIRONMENT => "environment_failure",
            _ => "failed",
        }),
    );
    // Opt-in latency telemetry, not broker state: a command must never fail
    // or grow stderr noise because its own timing line could not be written.
    let dir = main_root.join(".aethyme/logs");
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let extra_fields = serde_json::to_string(&serde_json::Value::Object(details))
        .ok()
        .map(|json| json[1..json.len().saturating_sub(1)].to_string())
        .map(|fields| format!(",{fields}"))
        .unwrap_or_default();
    let line = metric_line(ts, &label, duration_ms, exit, output_bytes, &extra_fields);
    // Under the shared append lock, so a GC trim replacing this log can never
    // drop the row with the inode it replaces.
    crate::gc::metrics_trim::append_command_metric_line(
        &main_root,
        &dir.join("command-metrics.jsonl"),
        line.as_bytes(),
    );
}

/// Fold the spans recorded anywhere during the command into its metric
/// fields. A phase or count the command named itself wins over a span of the
/// same name; `phase_counts` says how many times each span ran, beside any
/// counts the command attached.
fn with_spans(
    mut fields: serde_json::Map<String, serde_json::Value>,
    spans: &crate::phase_telemetry::Recorder,
) -> serde_json::Map<String, serde_json::Value> {
    // Attached facts (a lock's queue position, who held it) are top-level
    // fields; one the command attached itself under the same name wins.
    for (name, value) in spans.attachments() {
        fields.entry(name).or_insert(value);
    }
    let (millis, counts) = spans.totals();
    if millis.is_empty() {
        return fields;
    }
    let phases = object_field(&mut fields, "phase_timings_ms");
    for (name, elapsed) in millis {
        phases.entry(name).or_insert(elapsed.into());
    }
    let named = object_field(&mut fields, "phase_counts");
    for (name, count) in counts {
        named.entry(name).or_insert(count.into());
    }
    fields
}

/// The object at `key`, created when absent. Every reader of these rows
/// expects an object there, so a command that attached anything else is a
/// bug: the value is replaced (and the spans recorded) rather than the spans
/// being dropped without a trace.
fn object_field<'a>(
    fields: &'a mut serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> &'a mut serde_json::Map<String, serde_json::Value> {
    let value = fields
        .entry(key)
        .or_insert_with(|| serde_json::Value::Object(Default::default()));
    if !value.is_object() {
        *value = serde_json::Value::Object(Default::default());
    }
    match value {
        serde_json::Value::Object(object) => object,
        _ => unreachable!("replaced by an object above"),
    }
}

/// Whether this invocation should contribute command-latency telemetry.
/// Report-only commands stay telemetry-free; variants that mutate broker,
/// repository, or installation state remain observable.
/// Whether the operator opted into measuring read-only command output.
///
/// Off by default so inspection stays side-effect free. `AETHYME_MEASURE_OUTPUT`
/// is read per invocation rather than cached, so enabling it needs no restart of
/// anything and a wrapper can scope it to a single command.
pub(super) fn output_measurement_opted_in() -> bool {
    std::env::var_os("AETHYME_MEASURE_OUTPUT")
        .map(|value| {
            let value = value.to_string_lossy().to_ascii_lowercase();
            !matches!(value.as_str(), "" | "0" | "false" | "no" | "off")
        })
        .unwrap_or(false)
}

pub(super) fn command_records_metric(args: &[String]) -> bool {
    if super::routine_status(args) {
        return false;
    }
    match args.first().map(String::as_str) {
        // Quick-test owns a temporary repository that is removed before the
        // dispatcher records telemetry; there is no durable repository target.
        Some("quick-test") => false,
        // `insights` reads the same history `metrics` accounts for. Polling it
        // from cron must not write a telemetry line, or the report would be one
        // of the things it reports on.
        Some(
            "certify" | "readiness" | "queue" | "metrics" | "insights" | "handoff" | "exec-status"
            | "worktree-root" | "owners",
        ) => false,
        Some("advisories") => matches!(args.get(1).map(String::as_str), Some("ack" | "suppress")),
        Some("exposures") => args.get(1).map(String::as_str) == Some("apply"),
        Some("report") => args.get(1).map(String::as_str) == Some("file"),
        Some("quality-report") => args.get(1).map(String::as_str) != Some("plan"),
        Some("issues" | "outcomes") => true,
        Some("external-events") => matches!(
            args.get(1).map(String::as_str),
            Some("ingest" | "reconcile")
        ),
        Some("review") => !matches!(args.get(1).map(String::as_str), Some("show")),
        Some("checkpoint") => args.get(1).map(String::as_str) == Some("apply"),
        // The detached artifact worker writes one phase-timing line per run,
        // so its removal slices leave timing evidence.
        Some("gc") => matches!(
            args.get(1).map(String::as_str),
            Some("apply" | "sweep" | "__continue-artifacts")
        ),
        // A reclaim plan is an observation and writes no outcome event; it
        // records its timing line by attaching phase details once it has run,
        // so a refused selector still leaves no telemetry behind. An apply
        // removes directories, so like `gc apply` it writes an outcome event
        // and a `reclaim.apply` row on every run, refusals included; once it
        // has scanned, the row carries its phase timings and counts.
        Some("reclaim") => args.get(1).map(String::as_str) == Some("apply"),
        Some("storage") => match args.get(1).map(String::as_str) {
            Some("apply") => true,
            Some("attribute") => args.iter().any(|arg| arg == "--apply"),
            Some("identity") => args.get(2).map(String::as_str) == Some("apply"),
            _ => false,
        },
        Some("representation") => args.get(1).map(String::as_str) == Some("record"),
        Some("operations") => args.get(1).map(String::as_str) == Some("reconcile"),
        Some("merge-chain") => !args.iter().any(|arg| arg == "--dry-run"),
        Some("git" | "gh") => {
            let command = args
                .iter()
                .position(|arg| arg == "--")
                .map(|index| &args[index + 1..])
                .unwrap_or(&[]);
            let effect = if args.first().map(String::as_str) == Some("git") {
                crate::classify_git(command)
            } else {
                crate::classify_gh(command)
            };
            effect != Some(crate::OperationEffect::Read)
        }
        Some("hooks") => !matches!(args.get(1).map(String::as_str), Some("status" | "snippet")),
        Some("leases") => !matches!(
            args.get(1).map(String::as_str),
            Some("plan" | "export" | "explain")
        ),
        Some("ownership") => matches!(args.get(1).map(String::as_str), Some("claim" | "release")),
        Some("console") => args.get(1).map(String::as_str) == Some("run"),
        Some("resources") => !matches!(
            args.get(1).map(String::as_str),
            Some("plan" | "explain" | "list")
        ),
        Some("events") => args.get(1).map(String::as_str) == Some("prune"),
        Some("gates") => match args.get(1).map(String::as_str) {
            Some("affected") => true,
            Some("validate" | "manifest" | "scope" | "semantic") => false,
            Some("doctor") => args.iter().any(|arg| arg == "--probe"),
            _ => true,
        },
        Some("doctor") => {
            args.get(1).map(String::as_str) == Some("apply")
                || args.iter().any(|arg| arg == "--fix-version")
        }
        Some("trust") => args.get(1).map(String::as_str) != Some("status"),
        Some("blockers") => false,
        // Listing the outbox is a read; claims, dispatches and completions
        // change it.
        Some("deliveries") => args.get(1).map(String::as_str) != Some("list"),
        // A push plan reads the remote and changes nothing.
        Some("push") => !args.iter().any(|arg| arg == "--plan"),
        _ => true,
    }
}

/// Decide whether this invocation writes a timing line. A GC plan is a
/// special case: its candidate set may rewrite this same metrics file, so
/// recording the plan afterward would invalidate the digest it just produced.
fn command_metric_enabled(args: &[String], opted_in: bool, has_details: bool) -> bool {
    // Explicit output measurement cannot make this temporary smoke command
    // write telemetry into the unrelated repository that invoked it.
    if args.first().map(String::as_str) == Some("quick-test") {
        return false;
    }
    if matches!(
        (
            args.first().map(String::as_str),
            args.get(1).map(String::as_str)
        ),
        (Some("gc"), Some("plan"))
    ) {
        return false;
    }
    // Every `status` call appends its row, routine compact status included:
    // identifying who polls status, and in which form, is what the row is
    // for. It is a log append, so routine status still writes no broker
    // state and no outcome event (`command_records_outcome` stays false).
    command_records_metric(args)
        || opted_in
        || has_details
        || args.first().map(String::as_str) == Some("status")
}

/// `gates affected` opts into a timing line while preserving its read-only
/// event-store contract. Other outcome events follow the command metric policy.
fn command_records_outcome(args: &[String]) -> bool {
    !matches!(
        (
            args.first().map(String::as_str),
            args.get(1).map(String::as_str)
        ),
        (Some("gates"), Some("affected")) | (Some("gc"), Some("__continue-artifacts"))
    ) && command_records_metric(args)
}

/// One `command-metrics.jsonl` row.
///
/// `aethyme_version` is what makes two releases comparable: impact is measured
/// by deploying a release and reading the rows it wrote, and a row that does
/// not say which binary produced it can only be attributed by guessing from
/// its timestamp. `extra_fields` is either empty or begins with a comma.
fn metric_line(
    ts: i64,
    label: &str,
    duration_ms: i64,
    exit: u8,
    output_bytes: u64,
    extra_fields: &str,
) -> String {
    format!(
        "{{\"ts\":{ts},\"aethyme_version\":\"{}\",\"command\":\"{label}\",\"duration_ms\":{duration_ms},\"exit\":{exit},\"output_bytes\":{output_bytes}{extra_fields}}}\n",
        env!("CARGO_PKG_VERSION")
    )
}

#[cfg(test)]
mod tests {
    use super::{
        command_metric_enabled, command_records_metric, command_records_outcome, metric_line,
        safe_command_surface, with_spans,
    };

    #[test]
    fn caller_hints_keep_only_the_executable_name() {
        use super::sanitize_process_name;
        assert_eq!(sanitize_process_name("zsh\n").as_deref(), Some("zsh"));
        assert_eq!(
            sanitize_process_name("/usr/local/bin/node").as_deref(),
            Some("node")
        );
        assert_eq!(sanitize_process_name("-zsh").as_deref(), Some("zsh"));
        assert_eq!(
            sanitize_process_name("/opt/My Tools/poll er").as_deref(),
            Some("poller")
        );
        assert_eq!(sanitize_process_name("  \n"), None);
        assert_eq!(sanitize_process_name(&"x".repeat(200)).unwrap().len(), 64);
    }

    #[test]
    fn this_process_reports_a_bare_parent_name() {
        if let Some(name) = super::caller_hint() {
            assert!(!name.is_empty() && name.len() <= 64);
            assert!(!name.contains('/') && !name.contains(' '));
        }
    }

    #[test]
    fn every_metric_row_names_the_release_that_wrote_it() {
        let line = metric_line(
            7,
            "submit",
            1200,
            3,
            42,
            ",\"phase_counts\":{\"broker_open\":1}",
        );
        assert!(line.ends_with('\n'));
        let row: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(row["aethyme_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(row["ts"], 7);
        assert_eq!(row["command"], "submit");
        assert_eq!(row["duration_ms"], 1200);
        assert_eq!(row["exit"], 3);
        assert_eq!(row["output_bytes"], 42);
        assert_eq!(row["phase_counts"]["broker_open"], 1);

        let bare: serde_json::Value =
            serde_json::from_str(&metric_line(7, "status", 1, 0, 0, "")).unwrap();
        assert_eq!(bare["aethyme_version"], env!("CARGO_PKG_VERSION"));
    }

    /// Labels are built from the resolved internal spelling, which is what
    /// the dispatcher records.
    fn label(public: &str) -> Option<String> {
        let args = public
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        safe_command_surface(&super::super::surface::resolve(&args).args)
    }

    #[test]
    fn the_three_ways_to_end_a_session_record_distinct_labels() {
        assert_eq!(label("finish --session 3").as_deref(), Some("finish"));
        assert_eq!(label("finish close --session 3").as_deref(), Some("close"));
        assert_eq!(label("finish cleanup 3").as_deref(), Some("cleanup"));
        // The pre-six-verb spelling is the same command, so the same label.
        assert_eq!(label("close --session 3").as_deref(), Some("close"));
    }

    #[test]
    fn resolved_public_and_advanced_verbs_have_command_labels() {
        use super::super::surface::{ADVANCED_VERBS, PUBLIC_VERBS, public_forms};
        for &(verb, _) in PUBLIC_VERBS {
            assert!(label(verb).is_some(), "{verb}");
        }
        for &(verb, _) in ADVANCED_VERBS {
            assert!(label(&format!("advanced {verb}")).is_some(), "{verb}");
        }
        for (verb, form) in public_forms() {
            assert!(label(&format!("{verb} {form}")).is_some(), "{verb} {form}");
        }
    }

    #[test]
    fn positional_values_never_reach_a_label() {
        assert_eq!(
            label("finish close --session 3 --json").as_deref(),
            Some("close")
        );
        assert_eq!(
            label("finish cleanup 42 --force").as_deref(),
            Some("cleanup")
        );
    }

    #[test]
    fn affected_gates_records_phase_metrics_without_a_broker_outcome_event() {
        let affected = ["gates", "affected", "--session", "7"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(command_records_metric(&affected));
        assert!(!command_records_outcome(&affected));

        let run = ["gates", "run", "--session", "7"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        assert!(command_records_outcome(&run));
    }

    /// The detached artifact worker leaves a timing line per run (its removal
    /// slices were otherwise invisible) but no broker outcome event, which
    /// would add a database row every minute.
    #[test]
    fn the_artifact_worker_records_phase_metrics_without_an_outcome_event() {
        let worker = ["gc", "__continue-artifacts", "/repository"].map(str::to_string);
        assert!(command_records_metric(&worker));
        assert!(!command_records_outcome(&worker));
    }

    #[test]
    fn a_push_plan_is_an_observation_and_a_push_is_not() {
        let plan = ["push", "--session", "7", "--plan"].map(str::to_string);
        assert!(!command_records_metric(&plan));
        assert!(!command_records_outcome(&plan));
        let push = ["push", "--session", "7"].map(str::to_string);
        assert!(command_records_metric(&push));
    }

    #[test]
    fn compact_status_is_routine_and_full_status_is_recorded() {
        let compact = ["status", "--json"].map(str::to_string);
        assert!(!command_records_metric(&compact));
        assert!(!command_records_outcome(&compact));
        assert!(command_metric_enabled(&compact, false, false));
        let focused = ["status", "--session", "7", "--json"].map(str::to_string);
        assert!(command_metric_enabled(&focused, false, false));
        let full = ["status", "--full", "--json"].map(str::to_string);
        assert!(command_records_metric(&full));
    }

    #[test]
    fn exec_status_is_read_only_but_can_be_measured_when_opted_in() {
        let args = ["exec-status", "--session", "7"].map(str::to_string);
        assert_eq!(safe_command_surface(&args).as_deref(), Some("exec-status"));
        assert!(!command_records_metric(&args));
        assert!(!command_records_outcome(&args));
        assert!(command_metric_enabled(&args, true, false));
    }

    #[test]
    fn quick_test_does_not_write_invoking_repository_telemetry() {
        let quick_test = ["quick-test"].map(str::to_string);
        assert!(!command_records_metric(&quick_test));
        assert!(!command_records_outcome(&quick_test));
        assert!(!command_metric_enabled(&quick_test, true, true));
    }

    /// A whole-root reclaim plan ran for minutes and left no row at all: its
    /// label was unknown. It now records once it has run, and only then.
    #[test]
    fn a_reclaim_plan_records_its_metric_only_through_attached_details() {
        let plan = ["reclaim", "plan", "--json"].map(str::to_string);
        assert_eq!(label("gc reclaim plan").as_deref(), Some("reclaim.plan"));
        assert!(!command_records_metric(&plan));
        assert!(!command_records_outcome(&plan));
        assert!(
            !command_metric_enabled(&plan, false, false),
            "a refused selector attaches nothing and writes nothing"
        );
        assert!(command_metric_enabled(&plan, false, true));
    }

    /// Apply removes directories and used to leave neither a row nor an
    /// outcome event: `reclaim` was excluded wholesale with the plan.
    #[test]
    fn a_reclaim_apply_records_its_metric_and_outcome() {
        let digest = "a".repeat(64);
        let apply = ["reclaim", "apply", "--confirm", digest.as_str()].map(str::to_string);
        assert_eq!(
            label(&format!("gc reclaim apply --confirm {digest}")).as_deref(),
            Some("reclaim.apply")
        );
        assert!(command_records_metric(&apply));
        assert!(command_records_outcome(&apply));
        assert!(command_metric_enabled(&apply, false, false));
    }

    /// A command that attaches a malformed `phase_counts` must not make the
    /// spans vanish without a trace.
    #[test]
    fn spans_survive_a_non_object_phase_field() {
        let spans = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(spans.clone());
            crate::phase_telemetry::add("subprocess_git", std::time::Duration::from_millis(2));
        }
        let attached = serde_json::json!({"phase_timings_ms": 7, "phase_counts": [1, 2]});

        let fields = with_spans(attached.as_object().unwrap().clone(), &spans);

        assert_eq!(fields["phase_counts"]["subprocess_git"], 1);
        assert_eq!(fields["phase_timings_ms"]["subprocess_git"], 2);
    }

    #[test]
    fn spans_never_replace_counts_or_phases_the_command_attached() {
        let spans = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(spans.clone());
            crate::phase_telemetry::add("subprocess_git", std::time::Duration::from_millis(2));
            crate::phase_telemetry::add("measured", std::time::Duration::from_millis(9));
        }
        let attached = serde_json::json!({
            "phase_timings_ms": {"measured": 1},
            "phase_counts": {"measured": 40, "unmeasured": 3},
        });

        let fields = with_spans(attached.as_object().unwrap().clone(), &spans);

        assert_eq!(fields["phase_counts"]["measured"], 40);
        assert_eq!(fields["phase_counts"]["unmeasured"], 3);
        assert_eq!(fields["phase_counts"]["subprocess_git"], 1);
        assert_eq!(fields["phase_timings_ms"]["measured"], 1);
        assert_eq!(fields["phase_timings_ms"]["subprocess_git"], 2);
    }

    #[test]
    fn attached_facts_reach_the_metric_line_without_replacing_the_commands_own() {
        let spans = crate::phase_telemetry::Recorder::default();
        {
            let _installed = crate::phase_telemetry::install(spans.clone());
            crate::phase_telemetry::attach(
                "lifecycle_lock",
                serde_json::json!({"queue_position": 2, "outcome": "timeout"}),
            );
            crate::phase_telemetry::attach("owned", serde_json::json!("from a span"));
        }
        let attached = serde_json::json!({"owned": "by the command"});

        let fields = with_spans(attached.as_object().unwrap().clone(), &spans);

        assert_eq!(fields["lifecycle_lock"]["queue_position"], 2);
        assert_eq!(fields["lifecycle_lock"]["outcome"], "timeout");
        assert_eq!(fields["owned"], "by the command");
    }

    #[test]
    fn opted_in_measurement_does_not_make_gc_plan_rewrite_its_own_input() {
        let gc_plan = ["gc", "plan"].map(str::to_string);
        assert!(!command_metric_enabled(&gc_plan, true, false));
        assert!(!command_metric_enabled(&gc_plan, true, true));

        let status = ["status"].map(str::to_string);
        assert!(command_metric_enabled(&status, true, false));

        let gc_apply = ["gc", "apply"].map(str::to_string);
        assert!(command_metric_enabled(&gc_apply, false, false));
    }
}
