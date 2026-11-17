//! `aethyme hook <event>` — the agent-surface hook entry point.
//!
//! The inversion this implements: an agent used to learn about
//! coordination by *asking*, and every ask costs a full assistant turn.
//! Measured over 48h on this repository, 62% of those calls came from a
//! session that was the only one running at the time — coordination that
//! bought nothing. Hooks fire at turn boundaries for zero tokens, so the
//! broker can speak instead, and only when something changed.
//!
//! ## Why one entry point
//!
//! The plugin under `packages/aethyme/plugins/aethyme` ships a shim that
//! does nothing but pipe the event JSON here. Plugin and CLI are
//! installed by different commands at different times, so any rule
//! living in the shim would pin a plugin version to a CLI version. All
//! the logic is here; the shim is a pipe.
//!
//! ## Silence is the default
//!
//! Stdout is parsed as a hook envelope by the agent surface, so printing
//! anything that is not a deliberate envelope corrupts the session. Every
//! event below prints nothing unless it has something the agent did not
//! already know. That is the whole point: a turn boundary that produces
//! no output cost nothing.

use std::io::{Read, Write};

thread_local! {
    // A changed view is acknowledged only after its envelope reaches stdout.
    static PENDING_PEER_VIEWS: std::cell::RefCell<Vec<(std::path::PathBuf, serde_json::Value, crate::file_lock::ExclusiveFileLock)>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn acknowledge_peer_views() {
    PENDING_PEER_VIEWS.with(|pending| {
        for (path, value, _lock) in pending.borrow_mut().drain(..) {
            save_hook_state(Some(&path), &value);
        }
    });
}

use crate::{Broker, Session};

/// Hook events this entry point understands, in turn order.
///
/// `PermissionRequest` is deliberately absent. Nothing in the
/// coordination model needs it, and an unused hook is a process spawn
/// per permission prompt in exchange for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
}

impl HookEvent {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "SessionStart" => Some(Self::SessionStart),
            "UserPromptSubmit" => Some(Self::UserPromptSubmit),
            "PreToolUse" => Some(Self::PreToolUse),
            "PostToolUse" => Some(Self::PostToolUse),
            "Stop" => Some(Self::Stop),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
        }
    }
}

/// What one event decided to say. `Silent` is the common case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookOutcome {
    Silent,
    Context(String),
    Deny(String),
}

impl HookOutcome {
    /// Render the agent-surface envelope, or `None` when silent.
    ///
    /// The shape matches what `aethyme repo hook-envelope` already emits
    /// and what both Claude Code and Codex parse.
    pub fn envelope(&self, event: HookEvent) -> Option<String> {
        let inner = match self {
            Self::Silent => return None,
            Self::Context(text) => serde_json::json!({
                "hookEventName": event.as_str(),
                "additionalContext": text,
            }),
            Self::Deny(reason) => serde_json::json!({
                "hookEventName": event.as_str(),
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }),
        };
        Some(serde_json::json!({ "hookSpecificOutput": inner }).to_string())
    }
}

/// Run one event. Returns the process exit code.
///
/// Never fails loudly: a hook that errors is a hook that breaks the
/// agent surface it was meant to help. Anything unexpected — no broker
/// database, an unreadable repository, a malformed event — exits 0 with
/// no output, and the agent proceeds exactly as it would have without
/// the plugin installed.
pub fn run(args: &[String]) -> u8 {
    run_with_optional_build_banner(args, None)
}

/// Run one event with the build banner captured by the CLI that launched this hook.
///
/// SessionStart uses it to compare this process with the router selected by PATH.
pub fn run_with_build_banner(args: &[String], running_build_banner: &str) -> u8 {
    run_with_optional_build_banner(args, Some(running_build_banner))
}

fn run_with_optional_build_banner(args: &[String], running_build_banner: Option<&str>) -> u8 {
    // The compatibility shim probes before streaming. Older routers answer
    // unknown hook events silently, so they retain the buffered fallback.
    if args
        .first()
        .is_some_and(|arg| arg == "__stream_protocol_v1")
    {
        println!("aethyme-hook-stream-v1");
        return 0;
    }
    let Some(event) = args.first().map(String::as_str).and_then(HookEvent::parse) else {
        // Includes `--help` and every future event this binary predates.
        // An older CLI must stay silent in front of a newer plugin.
        return 0;
    };

    let repo = parse_repo_flag(&args[1..]);
    let mut payload = String::new();
    // Drain stdin before any early return: the caller writes the event
    // there and an unread pipe can surface as an error on their side.
    let _ = std::io::stdin().read_to_string(&mut payload);
    let event_json: serde_json::Value =
        serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null);

    match decide(event, repo.as_deref(), &event_json, running_build_banner) {
        Some(outcome) => {
            if let Some(envelope) = outcome.envelope(event) {
                let stdout = std::io::stdout();
                let mut output = stdout.lock();
                if writeln!(output, "{envelope}")
                    .and_then(|_| output.flush())
                    .is_ok()
                {
                    acknowledge_peer_views();
                }
            }
            0
        }
        None => 0,
    }
}

fn parse_repo_flag(rest: &[String]) -> Option<String> {
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        if arg == "--repo" {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix("--repo=") {
            return Some(value.to_string());
        }
    }
    None
}

/// Resolve the broker, dispatch the event, and swallow every failure.
///
/// `None` means "could not act" and is indistinguishable from `Silent`
/// to the caller; the distinction exists only so tests can tell a
/// deliberate silence from an unreachable broker.
fn decide(
    event: HookEvent,
    repo: Option<&str>,
    event_json: &serde_json::Value,
    running_build_banner: Option<&str>,
) -> Option<HookOutcome> {
    let cwd = match repo {
        Some(path) => std::path::PathBuf::from(path),
        None => std::env::current_dir().ok()?,
    };
    // PostToolUse is the turn boundary right after a tool call, which is
    // where a note from another session reaches an agent mid-task. It records
    // no liveness (a database write per tool call for what UserPromptSubmit
    // and Stop already carry): a read-only snapshot answers "anything unread?"
    // and the store is opened for writing only when there is a note to hand
    // over.
    if event == HookEvent::PostToolUse {
        return Some(on_post_tool_use(&cwd));
    }

    // The installation notice describes the machine, not the repository, so it
    // is computed before the broker is opened and survives an open that fails.
    // A broker that will not open is one of the things a mismatched router and
    // engine can produce, and that is the moment the notice is worth most.
    let mut installation_notice = match event {
        HookEvent::SessionStart => {
            crate::install_health::session_start_warnings_with_build(now_ms(), running_build_banner)
        }
        _ => Vec::new(),
    };
    // Managed MCP servers: repair agent configuration entries when the owner
    // turned `mcp.repair_on_session_start` on (off by default: one absent
    // settings file read). Bounded to 2 s, silent on a no-op.
    if event == HookEvent::SessionStart
        && let Some(notice) = crate::mcp::repair::session_start_notice()
    {
        installation_notice.push(notice);
    }

    // PreToolUse is the highest-frequency event that reaches the broker,
    // and it only reads. A snapshot keeps it off the write lock.
    let broker = match event {
        HookEvent::PreToolUse => Broker::open_snapshot(&cwd).ok(),
        _ => Broker::open(&cwd).ok(),
    };
    let Some(mut broker) = broker else {
        return installation_context(&installation_notice);
    };
    let canonical = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let session = current_session(&mut broker, &canonical);

    let outcome = match event {
        HookEvent::SessionStart => {
            let start = on_session_start(&mut broker, &canonical, session.as_ref());
            let scoped = session.as_ref().filter(|session| {
                !session.status.is_closed() && !matches!(start, HookOutcome::Deny(_))
            });
            let own = scoped.map(|session| {
                own_view_update(&broker, session, true).unwrap_or_else(|| {
                    format!(
                        "Aethyme session {}\nNext: aethyme broker status --session {}",
                        session.id, session.id
                    )
                })
            });
            let peers = scoped.and_then(|session| peer_view_update(&broker, session, true));
            with_installation_notice(
                append_context(append_context(start, own), peers),
                &installation_notice,
            )
        }
        HookEvent::UserPromptSubmit => on_user_prompt_submit(&mut broker, session.as_ref()),
        HookEvent::PreToolUse => on_pre_tool_use(&mut broker, session.as_ref(), event_json),
        HookEvent::Stop => {
            if let Err(error) = touch(&mut broker, session.as_ref()) {
                crate::warn_unrecorded::<(), _>("record stopped session activity", Err(error));
            }
            HookOutcome::Silent
        }
        HookEvent::PostToolUse => HookOutcome::Silent,
    };
    Some(outcome)
}

/// Append anything wrong with the local installation to a `SessionStart`
/// outcome.
///
/// This is the one event where they belong: session start is rare, it is already
/// producing a paragraph, and the process/installation diagnostics concern the
/// build the agent is about to use. On every other event it would be noise
/// repeated per turn.
///
/// Reporting only, and no network: see [`crate::install_health`]. A `Deny`
/// outcome is returned untouched, because a refusal must not be diluted by
/// housekeeping the agent cannot act on right now.
fn with_installation_notice(outcome: HookOutcome, warnings: &[String]) -> HookOutcome {
    if warnings.is_empty() {
        return outcome;
    }
    match outcome {
        HookOutcome::Deny(reason) => HookOutcome::Deny(reason),
        HookOutcome::Silent => HookOutcome::Context(warnings.join("\n\n")),
        HookOutcome::Context(text) => {
            HookOutcome::Context(format!("{text}\n\n{}", warnings.join("\n\n")))
        }
    }
}

/// The notice on its own, for the path where there is no broker outcome to
/// append it to. Empty stays `None` so an unreachable broker remains
/// indistinguishable from silence, as before.
fn installation_context(warnings: &[String]) -> Option<HookOutcome> {
    (!warnings.is_empty()).then(|| HookOutcome::Context(warnings.join("\n\n")))
}

/// The broker session owning this checkout, if any.
///
/// An agent working in the main checkout, or in a worktree the broker
/// does not know, has no session — that is a normal state, not an error,
/// and `SessionStart` turns it into a nudge to register.
///
/// An exact worktree match wins. Otherwise the deepest live session worktree
/// containing `cwd` does, so a hook invoked from a subdirectory of a session
/// worktree (no `--repo`) still finds its session instead of reporting an
/// unregistered checkout.
fn current_session(broker: &mut Broker, cwd: &std::path::Path) -> Option<Session> {
    let key = cwd.to_string_lossy().to_string();
    if let Some(found) = broker.store().session_for_worktree(&key).ok().flatten() {
        return Some(found);
    }
    broker
        .store()
        .live_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|candidate| {
            let root = std::path::Path::new(&candidate.worktree_path);
            let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
            cwd.starts_with(&root)
        })
        .max_by_key(|candidate| candidate.worktree_path.len())
}

/// Record that the agent did something, in both the places that matter.
///
/// `last_activity_at` is what every liveness and abandonment check reads; the
/// activity interval is what `broker advanced insights` reads to tell working
/// time from elapsed time. `record_session_activity` updates both in one
/// transaction, so the hook — which runs on every agent turn — pays for one
/// write, not two. If recording the interval fails, liveness is still updated
/// on its own: the telemetry tables must never be what makes a live session
/// look abandoned.
///
/// Ordinary storage failures are warned about and swallowed: activity telemetry
/// must not stop the agent from working. A missing or closed session is returned
/// to admission hooks so they can refuse a new turn on a session that retired.
fn touch(broker: &mut Broker, session: Option<&Session>) -> Result<(), crate::BrokerError> {
    let Some(session) = session else {
        return Ok(());
    };
    let now = now_ms();
    match broker.store().record_session_activity(session.id, now) {
        Ok(_) => Ok(()),
        Err(error @ crate::BrokerError::SessionNotFound(_))
        | Err(error @ crate::BrokerError::SessionNotOpen(_)) => Err(error),
        Err(error) => {
            crate::warn_unrecorded::<(), _>("record the session activity interval", Err(error));
            match broker.store().touch_session_activity(session.id, now) {
                Ok(()) => Ok(()),
                Err(error @ crate::BrokerError::SessionNotFound(_))
                | Err(error @ crate::BrokerError::SessionNotOpen(_)) => Err(error),
                Err(error) => {
                    crate::warn_unrecorded::<(), _>("record session activity", Err(error));
                    Ok(())
                }
            }
        }
    }
}

/// Register presence, tell every peer that the repository just became
/// crowded, and hand the agent its current state plus the one command to run
/// next.
///
/// The arrival note is the T1 transition (SOLO → COORDINATED), announced by
/// the session that *causes* it, at the moment it causes it. No peer has to
/// poll to discover the new arrival, and no extra state is needed to remember
/// whether the announcement was already made — the note channel itself is the
/// memory.
///
/// What the agent receives is state, not rules (recovery plan P4.7). The
/// policy lives in the generated root guidance and the skill; restating it
/// here billed it twice and still left the agent to work out which rule
/// applied. A state line and a `Next:` command are what it acts on.
fn on_session_start(
    broker: &mut Broker,
    cwd: &std::path::Path,
    session: Option<&Session>,
) -> HookOutcome {
    let Some(session) = session else {
        let tab_session = host_tab_name().and_then(|tab| {
            broker
                .store()
                .live_sessions()
                .ok()?
                .iter()
                .find(|other| {
                    !other.status.is_closed() && other.tab_name.as_deref() == Some(tab.as_str())
                })
                .map(|other| (other.id, other.worktree_path.clone()))
        });
        let information = session_coordination(broker, None).information;
        let context = HookOutcome::Context(render_unbound_start(cwd, tab_session));
        return append_context(context, render_repository_information(&information));
    };
    if session.status.is_closed() {
        return HookOutcome::Context(format!(
            "Aethyme session {} is closed.\nNext: aethyme broker start --reuse --task \"<follow-up task>\" --short-name \"<short name>\"",
            session.id
        ));
    }
    if touch(broker, Some(session)).is_err() {
        return HookOutcome::Deny(format!(
            "Aethyme: session {} was closed before this turn began. Start or adopt a fresh session before editing.",
            session.id
        ));
    }
    HookOutcome::Silent
}

/// The host tab this agent runs in, when the host exports one — the same
/// variables `broker start` records as a session's `tab_name`. It is the only
/// signal that ties an agent sitting in the wrong checkout to the session it
/// registered, so without it the hook does not guess.
fn host_tab_name() -> Option<String> {
    ["AETHYME_SESSION_TAB_NAME", "AETHYME_CHAU7_TAB_NAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .filter(|value| !value.trim().is_empty())
}

fn preview(task: &str) -> String {
    let task = task.trim();
    if task.chars().count() <= 60 {
        return task.to_string();
    }
    let cut: String = task.chars().take(59).collect();
    format!("{}…", cut.trim_end())
}

fn render_unbound_start(checkout: &std::path::Path, tab_session: Option<(i64, String)>) -> String {
    let mut text = format!(
        "Aethyme: this checkout ({}) has no broker session.",
        checkout.display()
    );
    let next = match tab_session {
        Some((id, worktree)) => {
            text.push_str(&format!(
                "\nSession {id} is registered for this tab in another worktree."
            ));
            format!("cd '{}'", worktree.replace('\'', "'\\''"))
        }
        None => "aethyme broker start --task \"<task>\" --short-name \"<short name>\"".into(),
    };
    format!("{text}\nNext: {next}")
}

/// Deliver scoped coordination changes at turn boundaries.
fn on_user_prompt_submit(broker: &mut Broker, session: Option<&Session>) -> HookOutcome {
    let Some(session) = session else {
        return HookOutcome::Silent;
    };
    if touch(broker, Some(session)).is_err() {
        return HookOutcome::Deny(format!(
            "Aethyme: session {} was closed before this turn began. Start or adopt a fresh session before editing.",
            session.id
        ));
    }
    let notes = deliver_notes(broker, session);
    let notes = append_context(notes, own_view_update(broker, session, false));
    append_context(notes, peer_view_update(broker, session, false))
}

/// Stable, bounded observations of recorded peer ownership relevant to this session.
pub(crate) fn session_peer_view(
    broker: &Broker,
    session_id: i64,
) -> Result<serde_json::Value, crate::BrokerError> {
    use sha2::{Digest, Sha256};
    let leases = broker
        .store_ref()
        .session_peer_lease_overlaps(session_id, now_ms())?;
    let scopes = broker.store_ref().session_peer_scopes(session_id)?;
    let all = serde_json::json!({"leases": leases, "scopes": scopes});
    let hash = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&all).expect("serializable peer records"))
    );
    let lease_rows = all["leases"].as_array().expect("lease records");
    let scope_rows = all["scopes"].as_array().expect("scope records");
    Ok(serde_json::json!({
        "lease_count": lease_rows.len(), "scope_count": scope_rows.len(),
        "leases": lease_rows.iter().take(10).collect::<Vec<_>>(),
        "scopes": scope_rows.iter().take(10).collect::<Vec<_>>(),
        "digest": hash, "source": "recorded",
    }))
}

pub(crate) struct SessionCoordination {
    pub(crate) actions: Vec<crate::Blocker>,
    pub(crate) information: serde_json::Value,
}

/// Only session-owned recovery goes in the action list. Unscoped observations
/// remain informational here; the broker checks their exact target when an
/// operation is attempted.
pub(crate) fn session_coordination(
    broker: &Broker,
    session_id: Option<i64>,
) -> SessionCoordination {
    let report = broker.collect_blockers_with_recovery(false);
    let (mut actions, other): (Vec<_>, Vec<_>) = report
        .blockers
        .into_iter()
        .partition(|blocker| session_id.is_some() && blocker.session_id == session_id);
    if let Some(session_id) = session_id
        && let Ok(Some(notice)) = broker.session_action_required_blocker(session_id)
    {
        actions.insert(0, notice);
    }
    let mut warnings_by_kind = std::collections::BTreeMap::new();
    for warning in &other {
        *warnings_by_kind.entry(warning.kind.as_str()).or_insert(0) += 1;
    }
    let stale = broker
        .store_ref()
        .recorded_stale_session_count(session_id)
        .ok();
    let information = serde_json::json!({
        "count": other.len(),
        "warnings_by_kind": warnings_by_kind,
        "recorded_stale_sessions": stale,
        "unavailable_source_count": report.unavailable.len(),
        "recovery_checks": "not_inspected",
    });
    SessionCoordination {
        actions,
        information,
    }
}

pub(crate) fn render_repository_information(view: &serde_json::Value) -> Option<String> {
    let warnings = view["count"].as_u64().unwrap_or(0);
    let unavailable = view["unavailable_source_count"].as_u64().unwrap_or(0);
    let stale = view["recorded_stale_sessions"].as_u64();
    if warnings == 0 && unavailable == 0 && stale == Some(0) {
        return None;
    }
    let stale = stale.map_or_else(|| "unknown".to_string(), |count| count.to_string());
    Some(format!(
        "Repository information: {stale} recorded stale session(s), {warnings} other coordination warning(s), {unavailable} unavailable warning source(s)."
    ))
}

fn own_view_update(broker: &Broker, session: &Session, force: bool) -> Option<String> {
    let path = hook_state_path(broker.main_root(), "hook-own-view", session.id)?;
    std::fs::create_dir_all(path.parent()?).ok()?;
    let file = crate::file_lock::open_lock_file(&path.with_extension("lock")).ok()?;
    let guard = crate::file_lock::ExclusiveFileLock::try_acquire(file).ok()??;
    let previous: serde_json::Value = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let now = now_ms();
    if !force
        && previous["checked_at_ms"]
            .as_i64()
            .is_some_and(|at| now >= at && now - at < 30_000)
    {
        return None;
    }

    let coordination = session_coordination(broker, Some(session.id));
    let view =
        crate::cli::status_extras::routine_session_block(broker, session.id, &coordination.actions)
            .ok()?;
    let brief = crate::cli::status_extras::render_routine_session(&view, false);
    let information = coordination.information;
    let mut messages = Vec::new();
    if force {
        messages.push(crate::cli::status_extras::render_routine_session(
            &view, true,
        ));
    } else if previous["brief"].as_str() != Some(&brief) {
        messages.push(brief.clone());
    }
    if previous["information"] != information {
        if let Some(summary) = render_repository_information(&information) {
            messages.push(summary);
        } else if render_repository_information(&previous["information"]).is_some()
            && !previous["information"].is_null()
        {
            messages.push("Repository information: no recorded stale sessions or other coordination warnings.".into());
        }
    }
    let record =
        serde_json::json!({"checked_at_ms": now, "brief": brief, "information": information});
    if messages.is_empty() {
        save_hook_state(Some(&path), &record);
        return None;
    }
    PENDING_PEER_VIEWS.with(|pending| pending.borrow_mut().push((path, record, guard)));
    Some(messages.join("\n"))
}

pub(crate) fn render_peer_information(view: &serde_json::Value, session_id: i64) -> Option<String> {
    use std::fmt::Write as _;
    let leases = view["lease_count"].as_u64().unwrap_or(0);
    let scopes = view["scope_count"].as_u64().unwrap_or(0);
    if leases == 0 && scopes == 0 {
        return None;
    }
    let mut text = format!(
        "Coordination: {leases} recorded path overlap(s), {scopes} matching declared scope(s)."
    );
    for entry in view["leases"].as_array().into_iter().flatten().take(3) {
        let _ = write!(
            text,
            "\n  session {} leases {}, overlapping your lease {}.",
            entry[0], entry[2], entry[1]
        );
    }
    let _ = write!(
        text,
        "\nDetails: aethyme broker status --session {session_id} --json"
    );
    Some(text)
}

/// Emit recorded peer changes once at hook boundaries. The file lock prevents
/// concurrent hooks from overwriting each other's observed revision.
fn peer_view_update(broker: &Broker, session: &Session, force: bool) -> Option<String> {
    let path = hook_state_path(broker.main_root(), "hook-peer-view", session.id)?;
    let parent = path.parent()?;
    std::fs::create_dir_all(parent).ok()?;
    let file = crate::file_lock::open_lock_file(&path.with_extension("lock")).ok()?;
    let _guard = crate::file_lock::ExclusiveFileLock::try_acquire(file).ok()??;
    let previous: Option<serde_json::Value> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let current = session_peer_view(broker, session.id).ok()?;
    let has_relevance = current["lease_count"].as_u64().unwrap_or(0) > 0
        || current["scope_count"].as_u64().unwrap_or(0) > 0;
    if previous.as_ref() == Some(&current) && !(force && has_relevance) {
        return None;
    }
    if previous.is_none() && !has_relevance {
        save_hook_state(Some(&path), &current);
        return None;
    }
    PENDING_PEER_VIEWS.with(|pending| pending.borrow_mut().push((path, current.clone(), _guard)));
    Some(
        render_peer_information(&current, session.id).unwrap_or_else(|| {
            "Coordination: no recorded path overlaps or matching declared scopes remain.".into()
        }),
    )
}

fn append_context(outcome: HookOutcome, addition: Option<String>) -> HookOutcome {
    match (outcome, addition) {
        (HookOutcome::Silent, Some(text)) => HookOutcome::Context(text),
        (HookOutcome::Context(existing), Some(text)) => {
            HookOutcome::Context(format!("{existing}\n{text}"))
        }
        (other, _) => other,
    }
}

/// Hand a note from another session to this agent at the first turn
/// boundary after a tool call, instead of waiting for its next prompt or
/// broker command. Silent, and read-only, when nothing is unread.
fn on_post_tool_use(cwd: &std::path::Path) -> HookOutcome {
    let Some(mut snapshot) = Broker::open_snapshot(cwd).ok() else {
        return HookOutcome::Silent;
    };
    let canonical = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let Some(session) = current_session(&mut snapshot, &canonical) else {
        return HookOutcome::Silent;
    };
    let own = own_view_update(&snapshot, &session, false);
    let peers = peer_view_update(&snapshot, &session, false);
    let store = snapshot.store_ref();
    let unread = !store
        .unread_session_notes(session.id)
        .unwrap_or_default()
        .is_empty()
        || !store
            .unread_automation_notes(session.id)
            .unwrap_or_default()
            .is_empty();
    if !unread {
        return append_context(append_context(HookOutcome::Silent, own), peers);
    }
    drop(snapshot);
    let Some(mut broker) = Broker::open(cwd).ok() else {
        return append_context(append_context(HookOutcome::Silent, own), peers);
    };
    append_context(
        append_context(deliver_notes(&mut broker, &session), own),
        peers,
    )
}

/// Render every unread note for `session`, each with the command that
/// answers its sender, and acknowledge them so the next turn does not
/// repeat them. `broker advanced note list --session <id>` still shows them.
fn deliver_notes(broker: &mut Broker, session: &Session) -> HookOutcome {
    let unread = broker
        .store()
        .unread_session_notes(session.id)
        .unwrap_or_default();
    let automation = broker
        .store()
        .unread_automation_notes(session.id)
        .unwrap_or_default();
    if unread.is_empty() && automation.is_empty() {
        return HookOutcome::Silent;
    }
    let mut lines = Vec::with_capacity((unread.len() + automation.len()) * 2);
    for note in &automation {
        if note.sender == "automation:lease-release" {
            let request_id = note
                .fingerprint
                .as_deref()
                .and_then(|value| value.strip_prefix("lease-release-request:"))
                .and_then(|value| value.parse::<i64>().ok());
            let Some(request_id) = request_id else {
                continue;
            };
            match broker.lease_release_request(request_id) {
                Ok(request)
                    if request.holder_session_id == session.id
                        && request.state == crate::lease_requests::RequestState::Pending => {}
                Ok(_) => {
                    crate::warn_unrecorded(
                        "acknowledge a resolved release request note",
                        broker.store().acknowledge_automation_note(note.id),
                    );
                    continue;
                }
                // An unavailable or pruned request cannot be safely presented
                // as actionable; retain the note for later inspection.
                Err(_) => continue,
            }
        }
        // An automation cannot be replied to; the note itself says what
        // failed and where it is tracked.
        lines.push(format!("Note from {}: {}", note.sender, note.message));
        crate::warn_unrecorded(
            "acknowledge a delivered automation note",
            broker.store().acknowledge_automation_note(note.id),
        );
    }
    for note in &unread {
        lines.push(format!(
            "Note from session {}: {}",
            note.sender_session_id, note.message
        ));
        lines.push(format!(
            "  reply: aethyme broker advanced note send --session {} --to-session {} --message \"…\"",
            session.id, note.sender_session_id
        ));
        // Delivered is acknowledged: the agent has it in context now, and
        // re-delivering it every turn would read as a new message.
        crate::warn_unrecorded(
            "acknowledge a delivered session note",
            broker.store().acknowledge_session_note(note.id),
        );
    }
    if lines.is_empty() {
        HookOutcome::Silent
    } else {
        HookOutcome::Context(lines.join("\n"))
    }
}

/// Tell the agent, before it edits a file, that another live session is
/// changing the same file: who, what, where, whether Git would conflict, and
/// the command that reaches that agent.
///
/// Informative only, in every promote mode. Leases are a coordination
/// channel, not a lock: an agent that is told about the other session can
/// decide to coordinate, land the shared edit first, or carry on, and a
/// refused edit would only stall it. The hook never claims a lease either:
/// the point is to surface what the agent did not anticipate.
///
/// "Changing" means now: each other live session's worktree is compared with
/// its base for the path at the moment of the edit, so a change committed
/// since the last lease refresh is reported and a reverted one is not. An
/// explicit claim is reported either way. This session's own leases are not
/// refreshed: the note is about other sessions, and the next broker command
/// records this edit for them.
///
/// Fires once per (path, other session, other session's change to the
/// path), so an agent editing the same file repeatedly hears it once, and
/// again only when the other session's change moves.
fn on_pre_tool_use(
    broker: &mut Broker,
    session: Option<&Session>,
    event_json: &serde_json::Value,
) -> HookOutcome {
    let Some(session) = session else {
        return HookOutcome::Silent;
    };
    let targets = write_targets(event_json);
    if targets.is_empty() {
        return HookOutcome::Silent;
    }
    let mut relatives: Vec<String> = targets
        .iter()
        .filter_map(|target| repo_relative(target, &session.worktree_path, broker.main_root()))
        .collect();
    relatives.sort();
    relatives.dedup();
    if relatives.is_empty() {
        // Outside every checkout we know: nothing to coordinate on.
        return HookOutcome::Silent;
    }

    // The other sessions an edit can collide with. Read from the session
    // table, not from the lease table: implicit leases are only as fresh as
    // the last broker command that refreshed them, and an agent that commits
    // and pushes without running one would otherwise be invisible here.
    let peers: Vec<Session> = broker
        .store_ref()
        .live_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|other| {
            other.id != session.id
                && matches!(
                    other.status,
                    crate::SessionStatus::Active | crate::SessionStatus::Idle
                )
        })
        .collect();
    if peers.is_empty() {
        return HookOutcome::Silent;
    }
    let leases = broker.store_ref().active_leases().unwrap_or_default();
    let mut seen = SeenState::load(broker.main_root(), session.id);
    let mut cache = ChangeCache::load(broker.main_root(), session.id);
    let mut notes = Vec::new();
    let mut diff_requests = Vec::new();
    for relative in &relatives {
        if notes.len() >= MAX_PEER_NOTES {
            break;
        }
        let mut candidates: Vec<Candidate<'_>> = peers
            .iter()
            .map(|other| Candidate::new(other, relative, &leases))
            .collect();
        // Decide "already told?" from the other worktree's copy of the file
        // (modification time and size, no process), and run Git only for a
        // state this agent has not heard about. A file the other session
        // merely touched is asked about again; that errs towards telling,
        // never towards silence.
        candidates.retain(|candidate| !seen.contains(&candidate.seen_key(relative)));
        resolve_current_changes(&mut candidates, relative, &mut cache);
        for candidate in candidates {
            if notes.len() >= MAX_PEER_NOTES {
                break;
            }
            if !candidate.announce() {
                continue;
            }
            if !seen.first_time(&candidate.seen_key(relative)) {
                continue;
            }
            let other = candidate.session;
            diff_requests.push(crate::overlap_diff::HolderRequest::new(
                other,
                candidate.claimed.is_some(),
                vec![relative.clone()],
            ));
            let change = describe_change(
                other,
                relative,
                candidate.claimed.as_deref(),
                candidate.diff.as_deref(),
            );
            let verdict = crate::overlap_pairs::cached_conflicting_paths(
                broker.store_ref(),
                session.id,
                other.id,
            );
            let peer = PeerSession {
                id: other.id,
                short_name: other.short_name.as_deref(),
                task: other.task.as_deref(),
            };
            notes.push(render_peer_note(
                session.id,
                &peer,
                relative,
                &change,
                verdict.as_deref(),
            ));
        }
    }
    let impact = peer_impact_notes(broker, session, &relatives, &peers, &leases, &mut seen);
    if !notes.is_empty() && crate::overlap_diff::enabled(broker.main_root()) {
        // One budget for the whole tool call, read together: an edit is waiting.
        let reports = crate::overlap_diff::read_all(
            broker.main_root(),
            diff_requests,
            crate::overlap_diff::HOOK_BUDGET,
        );
        for (note, mut report) in notes.iter_mut().zip(reports) {
            crate::overlap_diff::mark_shown(&mut report, &mut seen);
            note.push('\n');
            note.push_str(&crate::overlap_diff::render(&report, "  "));
        }
    }
    notes.extend(impact);
    seen.save();
    cache.save();
    if notes.is_empty() {
        return HookOutcome::Silent;
    }
    HookOutcome::Context(notes.join("\n"))
}

/// Files these edits depend on that another live session is changing, from the
/// code graph. Free on a repository with no graph, and silent on one it cannot
/// trust: see [`crate::peer_impact`]. Once per change, like the notes above.
fn peer_impact_notes(
    broker: &Broker,
    session: &Session,
    relatives: &[String],
    peers: &[Session],
    leases: &[crate::Lease],
    seen: &mut SeenState,
) -> Vec<String> {
    use crate::{overlap_diff, peer_impact};
    let main_root = broker.main_root();
    if !peer_impact::enabled(main_root) {
        return Vec::new();
    }
    let changed: Vec<peer_impact::Peer> = peers
        .iter()
        .map(|peer| {
            let files = leases
                .iter()
                .filter(|lease| {
                    lease.session_id == peer.id && lease.kind == crate::LeaseKind::Implicit
                })
                .map(|lease| lease.path.clone())
                .collect();
            peer_impact::Peer::new(peer, files)
        })
        .collect();
    let my_base = session
        .diff_base
        .as_deref()
        .or(session.adoption_base.as_deref());
    let mut notes = Vec::new();
    for relative in relatives {
        let deadline = std::time::Instant::now() + overlap_diff::HOOK_BUDGET;
        for finding in peer_impact::find(main_root, relative, my_base, &changed, deadline) {
            let Some(peer) = peers.iter().find(|peer| peer.id == finding.session_id) else {
                continue;
            };
            let mut hunks = overlap_diff::read_all(
                main_root,
                vec![peer_impact::hunk_request(peer, &finding)],
                overlap_diff::HOOK_BUDGET,
            );
            let mut hunk = hunks.pop();
            let digest_key = format!(
                "peer-impact|{}|{}|{}",
                finding.session_id, finding.changed_file, finding.path
            );
            if let Some(hunk) = hunk.as_mut() {
                overlap_diff::mark_shown(hunk, seen);
                if hunk.state == overlap_diff::OverlapChangesState::AlreadyShown {
                    continue;
                }
            } else if !seen.first_time(&digest_key) {
                continue;
            }
            notes.push(peer_impact::render(&finding, hunk.as_ref()));
        }
    }
    notes
}

/// One other live session, considered for one path the agent is about to
/// write.
struct Candidate<'a> {
    session: &'a Session,
    /// The explicit claim covering the path, when the session made one. A
    /// claim announces what a session is about to do, so it is reported
    /// whether or not the file has changed yet.
    claimed: Option<String>,
    /// Whether the last lease refresh recorded the path as changed. Only
    /// consulted when the current state cannot be read.
    implicitly_leased: bool,
    /// The base the session's change is measured from, when known.
    base: Option<String>,
    /// [`file_stamp`] of the path in the session's worktree.
    stamp: String,
    /// Whether the session's working tree differs from its base at the path
    /// right now; `None` when that could not be determined.
    changed: Option<bool>,
    /// The `-U0` diff read while deciding `changed`, reused to describe it.
    diff: Option<String>,
}

impl<'a> Candidate<'a> {
    fn new(session: &'a Session, path: &str, leases: &[crate::Lease]) -> Self {
        let mut claimed = None;
        let mut implicitly_leased = false;
        for lease in leases.iter().filter(|lease| {
            lease.session_id == session.id && crate::leases::paths_overlap(&lease.path, path)
        }) {
            if lease.kind == crate::LeaseKind::Explicit {
                claimed.get_or_insert_with(|| lease.path.clone());
            } else {
                implicitly_leased = true;
            }
        }
        Self {
            session,
            claimed,
            implicitly_leased,
            base: session
                .diff_base
                .clone()
                .or_else(|| session.adoption_base.clone()),
            stamp: file_stamp(&session.worktree_path, path),
            changed: None,
            diff: None,
        }
    }

    /// The once-per-change memory key. A claim keys on the claimed path, a
    /// change on the file itself, as before the change check existed, so an
    /// upgrade does not repeat notes already delivered.
    fn seen_key(&self, path: &str) -> String {
        format!(
            "{path}|{}|{}|{}",
            self.session.id,
            self.claimed.as_deref().unwrap_or(path),
            self.stamp
        )
    }

    fn cache_key(&self, path: &str) -> Option<String> {
        let base = self.base.as_deref()?;
        Some(format!("{}|{base}|{path}|{}", self.session.id, self.stamp))
    }

    /// Whether this session belongs in a note: it claimed the path, or it is
    /// changing the file now. A change that was reverted stays silent even
    /// while an old implicit lease still names the file.
    fn announce(&self) -> bool {
        self.claimed.is_some() || self.changed.unwrap_or(self.implicitly_leased)
    }
}

/// Fill in [`Candidate::changed`] for every candidate whose answer is not
/// cached, reading each other worktree's current state in parallel.
///
/// One Git process per uncached session (two for a file Git does not
/// track), all at once, so the wall-clock cost is about one process
/// however many sessions are live. The answer is cached by the file's
/// stamp and the session's base, which together determine it: a commit
/// does not change a working-tree diff against the base, and a write to
/// the file changes the stamp.
fn resolve_current_changes(candidates: &mut [Candidate<'_>], path: &str, cache: &mut ChangeCache) {
    let mut pending: Vec<&mut Candidate<'_>> = Vec::new();
    for candidate in candidates.iter_mut() {
        if candidate.claimed.is_some() {
            // Announced either way; the diff is read only if it is described.
            continue;
        }
        match candidate.cache_key(path).and_then(|key| cache.get(&key)) {
            Some(changed) => candidate.changed = Some(changed),
            None if candidate.base.is_some() => pending.push(candidate),
            None => {}
        }
    }
    std::thread::scope(|scope| {
        for candidate in pending.iter_mut() {
            scope.spawn(move || {
                let base = candidate.base.as_deref().unwrap_or("HEAD");
                let (changed, diff) = current_change(&candidate.session.worktree_path, base, path);
                candidate.changed = changed;
                candidate.diff = diff;
            });
        }
    });
    for candidate in pending {
        if let (Some(changed), Some(key)) = (candidate.changed, candidate.cache_key(path)) {
            cache.insert(key, changed);
        }
    }
}

/// Whether `path` in `worktree` differs from `base` right now, committed or
/// not, and the zero-context diff when it is a tracked change.
fn current_change(worktree: &str, base: &str, path: &str) -> (Option<bool>, Option<String>) {
    let repo = crate::GitRepo::at_known_root(std::path::Path::new(worktree));
    let Ok(diff) = repo.working_zero_context_diff(base, path) else {
        return (None, None);
    };
    if !diff.trim().is_empty() {
        return (Some(true), Some(diff));
    }
    if !std::path::Path::new(worktree).join(path).exists() {
        return (Some(false), None);
    }
    (repo.path_is_untracked(path).ok(), None)
}

/// Remembered "is this file changed in that session" answers, keyed so a
/// stale answer cannot be read back (see [`resolve_current_changes`]). Kept
/// beside [`SeenState`] and, like it, losing the file only costs a Git call.
struct ChangeCache {
    path: Option<std::path::PathBuf>,
    entries: Vec<(String, bool)>,
    dirty: bool,
}

/// Remembered answers per session; the oldest are forgotten first. Larger
/// than [`SEEN_CAPACITY`] because most answers are "not changed", one per
/// live session per file edited.
const CHANGE_CACHE_CAPACITY: usize = 2048;

impl ChangeCache {
    fn load(main_root: &std::path::Path, session_id: i64) -> Self {
        let path = hook_state_path(main_root, "hook-changes", session_id);
        let entries = path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            entries,
            dirty: false,
        }
    }

    fn get(&self, key: &str) -> Option<bool> {
        self.entries
            .iter()
            .rev()
            .find(|(seen, _)| seen == key)
            .map(|(_, changed)| *changed)
    }

    fn insert(&mut self, key: String, changed: bool) {
        self.entries.retain(|(seen, _)| *seen != key);
        self.entries.push((key, changed));
        if self.entries.len() > CHANGE_CACHE_CAPACITY {
            let excess = self.entries.len() - CHANGE_CACHE_CAPACITY;
            self.entries.drain(..excess);
        }
        self.dirty = true;
    }

    fn save(&self) {
        if self.dirty {
            save_hook_state(self.path.as_deref(), &self.entries);
        }
    }
}

/// At most this many other sessions are described per tool call. A file
/// four sessions are editing at once is already the message.
const MAX_PEER_NOTES: usize = 2;

/// Where another session is changing a file, summarised for one note.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PeerChange {
    /// "lines 40–88, 120" or a lease-only description.
    where_: String,
    /// Function or section names Git reported for the changed hunks.
    symbols: Vec<String>,
}

/// `claimed` is the explicit claim that put the session in the note, if any;
/// `diff` is the zero-context diff already read for `path`, if any.
fn describe_change(
    other: &Session,
    path: &str,
    claimed: Option<&str>,
    diff: Option<&str>,
) -> PeerChange {
    let ranges = match diff {
        Some(diff) => parse_hunks(diff),
        None => other
            .diff_base
            .as_deref()
            .or(other.adoption_base.as_deref())
            .and_then(|base| {
                crate::GitRepo::at_known_root(std::path::Path::new(&other.worktree_path))
                    .working_zero_context_diff(base, path)
                    .ok()
            })
            .map(|diff| parse_hunks(&diff))
            .unwrap_or_default(),
    };
    if ranges.0.is_empty() {
        let where_ = match claimed {
            Some(claim) if claim != path => format!("`{claim}` (claimed)"),
            _ => "this file".to_string(),
        };
        return PeerChange {
            where_,
            symbols: Vec::new(),
        };
    }
    let (spans, symbols) = ranges;
    let text = format_spans(&spans);
    let noun = if spans.len() == 1 && spans[0].0 == spans[0].1 {
        "line"
    } else {
        "lines"
    };
    PeerChange {
        where_: format!("{noun} {text}"),
        // Git's default hunk context is "the previous line that starts with a
        // letter", which names a function in code and an arbitrary line in
        // prose or data. Only code gets symbol names.
        symbols: if is_code_path(path) {
            symbols
        } else {
            Vec::new()
        },
    }
}

/// "<mtime-ns>:<len>" of `path` in another session's worktree, or "absent".
/// Changes whenever that session writes the file, which is what makes a
/// moved change announce again.
fn file_stamp(worktree: &str, path: &str) -> String {
    std::fs::metadata(std::path::Path::new(worktree).join(path))
        .ok()
        .map(|metadata| {
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|since| since.as_nanos())
                .unwrap_or(0);
            format!("{modified}:{}", metadata.len())
        })
        .unwrap_or_else(|| "absent".to_string())
}

/// Whether Git's hunk context for `path` is likely a function or section
/// name rather than an arbitrary preceding line.
fn is_code_path(path: &str) -> bool {
    let extension = std::path::Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    !matches!(
        extension.as_str(),
        "" | "txt"
            | "md"
            | "json"
            | "yml"
            | "yaml"
            | "toml"
            | "lock"
            | "csv"
            | "snap"
            | "html"
            | "svg"
    )
}

/// New-side line spans and hunk function context from a `-U0` patch.
fn parse_hunks(diff: &str) -> (Vec<(u32, u32)>, Vec<String>) {
    let mut spans = Vec::new();
    let mut symbols: Vec<String> = Vec::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(end) = rest.find(" @@") else {
            continue;
        };
        let header = &rest[..end];
        let context = rest[end + 3..].trim();
        if let Some(new) = header
            .split_whitespace()
            .find_map(|part| part.strip_prefix('+'))
        {
            let mut fields = new.splitn(2, ',');
            let start: u32 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
            let count: u32 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(1);
            let start = start.max(1);
            let last = if count == 0 { start } else { start + count - 1 };
            spans.push((start, last));
        }
        if !context.is_empty() {
            let symbol: String = context.chars().take(48).collect();
            if !symbols.contains(&symbol) {
                symbols.push(symbol);
            }
        }
    }
    (spans, symbols)
}

/// "40–88, 120, 131–140" with adjacent spans merged and at most four shown.
fn format_spans(spans: &[(u32, u32)]) -> String {
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for &(start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 + 1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    let shown: Vec<String> = merged
        .iter()
        .take(4)
        .map(|(start, end)| {
            if start == end {
                start.to_string()
            } else {
                format!("{start}–{end}")
            }
        })
        .collect();
    let more = merged.len().saturating_sub(4);
    if more > 0 {
        format!("{} (+{more} more)", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// The other session, as much of it as a note shows.
struct PeerSession<'a> {
    id: i64,
    short_name: Option<&'a str>,
    task: Option<&'a str>,
}

/// At most three lines: who and where, the conflict verdict, and the
/// command that reaches the other agent.
fn render_peer_note(
    me: i64,
    other: &PeerSession<'_>,
    path: &str,
    change: &PeerChange,
    conflicting: Option<&[String]>,
) -> String {
    let name = other
        .short_name
        .map(|name| format!(" ({name})"))
        .unwrap_or_default();
    let task = other
        .task
        .map(|task| format!(" working on \"{}\"", preview(task)))
        .unwrap_or_default();
    let symbols = if change.symbols.is_empty() {
        String::new()
    } else {
        format!(
            " in {}",
            change
                .symbols
                .iter()
                .take(2)
                .map(|symbol| format!("`{symbol}`"))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let verdict = match conflicting {
        Some(paths) if paths.iter().any(|conflict| conflict == path) => {
            "At the last check Git would conflict with your change to this file."
        }
        Some(_) => "At the last check your changes to this file merged cleanly.",
        None => "Not compared yet: edits to the same lines will conflict.",
    };
    format!(
        "Aethyme: session {}{name}{task} is also changing `{path}` ({}{symbols}). {verdict}\n  \
         coordinate: aethyme broker advanced note send --session {me} --to-session {} --message \"…\"",
        other.id, change.where_, other.id
    )
}

/// The once-per-change memory of what this session's agent was already
/// told. A small file in the main checkout's Git directory: never tracked,
/// never shared, and a PreToolUse hook (read-only on the broker) can write
/// it. Losing it only repeats a note.
pub(crate) struct SeenState {
    path: Option<std::path::PathBuf>,
    keys: Vec<String>,
    dirty: bool,
}

/// Remembered notes per session; the oldest are forgotten first.
const SEEN_CAPACITY: usize = 256;

impl SeenState {
    pub(crate) fn load(main_root: &std::path::Path, session_id: i64) -> Self {
        let path = hook_state_path(main_root, "hook-seen", session_id);
        let keys = path
            .as_deref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path,
            keys,
            dirty: false,
        }
    }

    fn contains(&self, key: &str) -> bool {
        self.keys.iter().any(|seen| seen == key)
    }

    pub(crate) fn first_time(&mut self, key: &str) -> bool {
        if self.contains(key) {
            return false;
        }
        self.keys.push(key.to_string());
        if self.keys.len() > SEEN_CAPACITY {
            let excess = self.keys.len() - SEEN_CAPACITY;
            self.keys.drain(..excess);
        }
        self.dirty = true;
        true
    }

    pub(crate) fn save(&self) {
        if self.dirty {
            save_hook_state(self.path.as_deref(), &self.keys);
        }
    }
}

/// `<main checkout>/.git/aethyme/<stem>-<session>.json`, when the main
/// checkout has a `.git` directory.
fn hook_state_path(
    main_root: &std::path::Path,
    stem: &str,
    session_id: i64,
) -> Option<std::path::PathBuf> {
    let git_dir = main_root.join(".git");
    git_dir.is_dir().then(|| {
        git_dir
            .join("aethyme")
            .join(format!("{stem}-{session_id}.json"))
    })
}

fn save_hook_state(path: Option<&std::path::Path>, value: &impl serde::Serialize) {
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        crate::warn_unrecorded(
            "create the hook state directory",
            std::fs::create_dir_all(parent),
        );
    }
    if let Ok(bytes) = serde_json::to_vec(value) {
        crate::warn_unrecorded(
            "record the agent hook's coordination state",
            crate::atomic_file::with_synced_temporary(path, &bytes, |temporary| {
                std::fs::rename(temporary, path)
            }),
        );
    }
}

/// The absolute paths a tool call is about to write, when the event names
/// any. Read-only tools and tools with no path produce nothing, which is
/// how the common case stays free.
///
/// Claude Code's edit tools carry one `file_path` (or `notebook_path`).
/// Codex's `apply_patch` carries the patch text, whose `*** Update File:`,
/// `*** Add File:` and `*** Move to:` headers name every file it writes;
/// relative paths there are relative to the event's `cwd`.
fn write_targets(event_json: &serde_json::Value) -> Vec<std::path::PathBuf> {
    let Some(tool) = event_json.get("tool_name").and_then(|tool| tool.as_str()) else {
        return Vec::new();
    };
    let Some(input) = event_json.get("tool_input") else {
        return Vec::new();
    };
    match tool {
        "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => input
            .get("file_path")
            .or_else(|| input.get("notebook_path"))
            .and_then(|path| path.as_str())
            .map(|path| vec![std::path::PathBuf::from(path)])
            .unwrap_or_default(),
        "apply_patch" | "ApplyPatch" => {
            let cwd = event_json
                .get("cwd")
                .and_then(|cwd| cwd.as_str())
                .map(std::path::PathBuf::from);
            let patch = ["input", "patch", "command"]
                .iter()
                .find_map(|key| match input.get(*key) {
                    Some(serde_json::Value::String(text)) => Some(text.clone()),
                    Some(serde_json::Value::Array(parts)) => Some(
                        parts
                            .iter()
                            .filter_map(|part| part.as_str())
                            .collect::<Vec<_>>()
                            .join("\n"),
                    ),
                    _ => None,
                })
                .or_else(|| input.as_str().map(str::to_string))
                .unwrap_or_default();
            patch_targets(&patch)
                .into_iter()
                .map(|path| {
                    let path = std::path::PathBuf::from(path);
                    match (&cwd, path.is_absolute()) {
                        (Some(cwd), false) => cwd.join(path),
                        _ => path,
                    }
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

/// Files an `apply_patch` envelope writes, in order, without duplicates.
fn patch_targets(patch: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for line in patch.lines() {
        let line = line.trim();
        let path = ["*** Update File:", "*** Add File:", "*** Move to:"]
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .map(str::trim);
        if let Some(path) = path.filter(|path| !path.is_empty())
            && !found.iter().any(|seen| seen == path)
        {
            found.push(path.to_string());
        }
    }
    found
}

/// Make an absolute tool-call path repo-relative against the session's own
/// worktree first, then the main checkout.
///
/// Both are tried because a lease names one repo-relative path that must
/// mean the same file no matter which checkout an agent edits it from.
fn repo_relative(
    target: &std::path::Path,
    worktree_path: &str,
    main_root: &std::path::Path,
) -> Option<String> {
    let worktree = std::path::Path::new(worktree_path);
    let relative = target
        .strip_prefix(worktree)
        .or_else(|_| target.strip_prefix(main_root))
        .ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_request_delivers_once_and_resolved_request_stays_silent() {
        use crate::main_drift::tests::git;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("file.rs"), "base\n").unwrap();
        git(&root, &["add", "file.rs"]);
        git(&root, &["commit", "-qm", "base"]);
        let worktree = |name: &str| {
            let path = temp.path().join(name);
            git(
                &root,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    name,
                    path.to_str().unwrap(),
                    "main",
                ],
            );
            path
        };
        let mut broker = Broker::open(&root).unwrap();
        let holder = broker.adopt(&worktree("holder"), Some("holder")).unwrap();
        let requester = broker
            .adopt(&worktree("requester"), Some("requester"))
            .unwrap();
        let other = broker.adopt(&worktree("other"), Some("other")).unwrap();
        // Establish the same delivered baseline as SessionStart for both hosts.
        for session in [&holder, &other] {
            let _ = own_view_update(&broker, session, true);
            let _ = peer_view_update(&broker, session, true);
            acknowledge_peer_views();
        }
        broker.claim_lease(holder.id, "file.rs", None).unwrap();
        broker.claim_lease(requester.id, "file.rs", None).unwrap();
        let requests = broker
            .request_lease_release(requester.id, "file.rs", "need file")
            .unwrap();
        assert_eq!(requests.len(), 1);
        for id in [holder.id, requester.id, other.id] {
            let expected: Vec<_> = broker
                .pending_lease_release_requests()
                .unwrap()
                .into_iter()
                .filter(|request| {
                    request.requester_session_id == id || request.holder_session_id == id
                })
                .collect();
            assert_eq!(
                broker.session_pending_lease_release_requests(id).unwrap(),
                expected
            );
        }
        assert_eq!(
            requests[0].state,
            crate::lease_requests::RequestState::Pending
        );
        assert_eq!(
            on_post_tool_use(std::path::Path::new(&other.worktree_path)),
            HookOutcome::Silent
        );
        let delivered = on_post_tool_use(std::path::Path::new(&holder.worktree_path));
        let HookOutcome::Context(text) = delivered else {
            panic!("expected request note")
        };
        assert!(text.contains("recorded path overlap"));
        acknowledge_peer_views();
        assert!(text.contains(&format!(
            "leases ack {} --session {}",
            requests[0].request_id, holder.id
        )));
        assert_eq!(
            on_post_tool_use(std::path::Path::new(&holder.worktree_path)),
            HookOutcome::Silent
        );
        let repeated = broker
            .request_lease_release(requester.id, "file.rs", "need file")
            .unwrap();
        assert_eq!(repeated[0].request_id, requests[0].request_id);
        assert_eq!(
            on_post_tool_use(std::path::Path::new(&holder.worktree_path)),
            HookOutcome::Silent
        );
        broker
            .decline_lease_release(holder.id, requests[0].request_id, "still editing")
            .unwrap();
        assert!(
            broker
                .session_pending_lease_release_requests(requester.id)
                .unwrap()
                .is_empty()
        );
        let pending = broker
            .request_lease_release(requester.id, "file.rs", "need again")
            .unwrap();
        broker
            .decline_lease_release(holder.id, pending[0].request_id, "still editing")
            .unwrap();
        assert_eq!(
            on_post_tool_use(std::path::Path::new(&holder.worktree_path)),
            HookOutcome::Silent
        );
        assert!(
            broker
                .store()
                .unread_automation_notes(holder.id)
                .unwrap()
                .is_empty()
        );
        let repository = format!("local:{}", broker.main_root().display());
        let operation = broker
            .store()
            .create_coordinated_operation(&crate::NewCoordinatedOperation {
                session_id: requester.id,
                provider: crate::OperationProvider::Git,
                repository,
                scope: "repository".into(),
                effect: crate::OperationEffect::Write,
                authorization_reason: Some("test crash".into()),
                command_json: r#"["git","push"]"#.into(),
                pid: 999_999,
                host_operation_id: None,
                identity_provenance: crate::OperationIdentityProvenance::LocalRepository,
            })
            .unwrap();
        broker
            .store()
            .transition_coordinated_operation(
                operation.id,
                crate::OperationStatus::OutcomeUnknown,
                None,
                None,
            )
            .unwrap();
        let observed = session_coordination(&broker, Some(holder.id));
        assert_eq!(observed.information["warnings_by_kind"]["operation"], 1);
        assert!(observed.actions.is_empty());
        assert!(
            !observed
                .information
                .to_string()
                .contains(&format!("op:{}", operation.id))
        );
        let update = own_view_update(&broker, &requester, true).expect("owned blocker");
        assert!(
            update.contains("Action required: A write from this session has an unknown outcome")
        );
        assert!(update.contains("Next: aethyme broker unblock"));
        assert!(!update.contains("--outcome"));
        acknowledge_peer_views();
        assert!(own_view_update(&broker, &requester, false).is_none());
    }

    /// Through the hook's own path: opted in, a session changing a file this
    /// edit depends on is named with its diff, once; not opted in, it is silent.
    #[test]
    fn a_dependency_another_session_is_changing_is_flagged_only_when_opted_in() {
        use crate::main_drift::tests::{git, graph};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join(".gitignore"), ".aethyme/\n").unwrap();
        for path in ["a.rs", "b.rs"] {
            std::fs::write(root.join(path), "base\n").unwrap();
        }
        git(&root, &["add", "."]);
        git(&root, &["commit", "-qm", "base"]);
        let head = git(&root, &["rev-parse", "HEAD"]);
        graph(&root, &head, true);

        let mut broker = Broker::open(&root).unwrap();
        let worktree = |name: &str| {
            let path = temp.path().join(name);
            git(
                &root,
                &[
                    "worktree",
                    "add",
                    "-q",
                    "-b",
                    &format!("agent/{name}"),
                    path.to_str().unwrap(),
                    "main",
                ],
            );
            path
        };
        let other_tree = worktree("other");
        let other = broker.adopt(&other_tree, Some("rework a")).unwrap();
        let me = broker.adopt(&worktree("me"), Some("use a")).unwrap();
        std::fs::write(other_tree.join("a.rs"), "changed by the other session\n").unwrap();
        broker.refresh_leases().unwrap();
        let leases = broker.store_ref().active_leases().unwrap();
        let peers = vec![broker.store_ref().session(other.id).unwrap()];
        let notes = |broker: &Broker, seen: &mut SeenState| {
            peer_impact_notes(broker, &me, &["b.rs".to_string()], &peers, &leases, seen)
        };
        let mut seen = SeenState::load(&root, me.id);

        assert!(notes(&broker, &mut seen).is_empty(), "off by default");

        std::fs::create_dir_all(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[leases]\noverlap_impact = true\n",
        )
        .unwrap();
        let found = notes(&broker, &mut seen);
        assert_eq!(found.len(), 1, "{found:?}");
        let note = &found[0];
        assert!(
            note.contains(&format!("session {} is changing `a.rs`", other.id)),
            "{note}"
        );
        assert!(note.contains("Advisory, not a conflict"), "{note}");
        assert!(note.contains("| +changed by the other session"), "{note}");

        assert!(
            notes(&broker, &mut seen).is_empty(),
            "shown once per change"
        );
    }

    #[test]
    fn silence_renders_no_envelope() {
        assert!(
            HookOutcome::Silent
                .envelope(HookEvent::UserPromptSubmit)
                .is_none()
        );
    }

    #[test]
    fn context_and_deny_use_the_shapes_the_surfaces_parse() {
        let context = HookOutcome::Context("hello".into())
            .envelope(HookEvent::SessionStart)
            .expect("context renders");
        let parsed: serde_json::Value = serde_json::from_str(&context).unwrap();
        let inner = &parsed["hookSpecificOutput"];
        assert_eq!(inner["hookEventName"], "SessionStart");
        assert_eq!(inner["additionalContext"], "hello");

        let deny = HookOutcome::Deny("contended".into())
            .envelope(HookEvent::PreToolUse)
            .expect("deny renders");
        let parsed: serde_json::Value = serde_json::from_str(&deny).unwrap();
        let inner = &parsed["hookSpecificOutput"];
        assert_eq!(inner["permissionDecision"], "deny");
        assert_eq!(inner["permissionDecisionReason"], "contended");
    }

    /// An older CLI must stay silent in front of a newer plugin rather
    /// than printing an error into the envelope slot.
    #[test]
    fn unknown_events_are_not_parsed() {
        assert_eq!(
            HookEvent::parse("SessionStart"),
            Some(HookEvent::SessionStart)
        );
        assert_eq!(HookEvent::parse("PermissionRequest"), None);
        assert_eq!(HookEvent::parse("SomethingInventedLater"), None);
        assert_eq!(HookEvent::parse("--help"), None);
    }

    #[test]
    fn repo_flag_accepts_both_spellings() {
        let split = vec!["--repo".to_string(), "/a/b".to_string()];
        assert_eq!(parse_repo_flag(&split), Some("/a/b".into()));
        let joined = vec!["--repo=/c/d".to_string()];
        assert_eq!(parse_repo_flag(&joined), Some("/c/d".into()));
        assert_eq!(parse_repo_flag(&[]), None);
    }

    /// The worktree wins over the main checkout, which is what makes one
    /// repo-relative lease mean the same file from every checkout.
    #[test]
    fn paths_resolve_against_the_session_worktree_first() {
        let main = std::path::Path::new("/repo");
        assert_eq!(
            repo_relative(std::path::Path::new("/wt/s1/src/a.rs"), "/wt/s1", main),
            Some("src/a.rs".into())
        );
        assert_eq!(
            repo_relative(std::path::Path::new("/repo/src/a.rs"), "/wt/s1", main),
            Some("src/a.rs".into())
        );
        assert_eq!(
            repo_relative(std::path::Path::new("/elsewhere/a.rs"), "/wt/s1", main),
            None
        );
    }

    #[test]
    fn unbound_start_names_only_the_registered_tab_session() {
        let root = std::path::Path::new("/repo");
        let text = render_unbound_start(root, None);
        assert!(text.contains("has no broker session"), "{text}");
        assert!(text.contains("Next: aethyme broker start --task"), "{text}");
        let text = render_unbound_start(root, Some((7, "/wt/it's here".into())));
        assert!(
            text.contains("Session 7 is registered for this tab"),
            "{text}"
        );
        assert!(text.ends_with("Next: cd '/wt/it'\\''s here'"), "{text}");
    }

    /// Read-only tools cost nothing: no lease lookup happens at all.
    #[test]
    fn only_writing_tools_name_a_target() {
        let write = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": "/wt/s1/a.rs"},
        });
        assert_eq!(
            write_targets(&write),
            vec![std::path::PathBuf::from("/wt/s1/a.rs")]
        );

        let read = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/wt/s1/a.rs"},
        });
        assert!(write_targets(&read).is_empty());

        let notebook = serde_json::json!({
            "tool_name": "NotebookEdit",
            "tool_input": {"notebook_path": "/wt/s1/a.ipynb"},
        });
        assert_eq!(
            write_targets(&notebook),
            vec![std::path::PathBuf::from("/wt/s1/a.ipynb")]
        );

        assert!(write_targets(&serde_json::Value::Null).is_empty());
        assert!(write_targets(&serde_json::json!({"tool_name": "Edit"})).is_empty());
    }

    /// Codex writes through `apply_patch`; every file its headers name is a
    /// target, relative paths resolved against the event's `cwd`.
    #[test]
    fn codex_apply_patch_names_every_file_it_writes() {
        let event = serde_json::json!({
            "tool_name": "apply_patch",
            "cwd": "/wt/s1",
            "tool_input": {"command": ["apply_patch", "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n*** Add File: /abs/b.rs\n+z\n*** Update File: src/a.rs\n*** End Patch"]},
        });
        assert_eq!(
            write_targets(&event),
            vec![
                std::path::PathBuf::from("/wt/s1/src/a.rs"),
                std::path::PathBuf::from("/abs/b.rs"),
            ]
        );
    }

    /// The new-side spans and Git's function context are what the note
    /// shows; a pure deletion still points at a line.
    #[test]
    fn hunk_headers_become_spans_and_symbols() {
        let diff = "diff --git a/x b/x\n@@ -10,2 +40,49 @@ fn run_focused(\n@@ -100 +120 @@ fn helper()\n@@ -130,3 +131,0 @@\n";
        let (spans, symbols) = parse_hunks(diff);
        assert_eq!(spans, vec![(40, 88), (120, 120), (131, 131)]);
        assert_eq!(
            symbols,
            vec!["fn run_focused(".to_string(), "fn helper()".to_string()]
        );
        assert_eq!(format_spans(&spans), "40–88, 120, 131");
        assert_eq!(format_spans(&[(1, 2), (3, 4), (10, 10)]), "1–4, 10");
    }

    /// Symbol names come from Git's hunk context, which only means something
    /// in code.
    #[test]
    fn only_code_files_get_symbol_names() {
        assert!(is_code_path("src/parser.rs"));
        assert!(is_code_path("scripts/ci/run.mjs"));
        assert!(!is_code_path("notes.txt"));
        assert!(!is_code_path("config/manifest.json"));
        assert!(!is_code_path("Makefile"));
    }

    /// The note names the other session, where it is changing the file, the
    /// conflict verdict, and the command that reaches it.
    #[test]
    fn a_peer_note_names_the_session_the_lines_the_verdict_and_the_command() {
        let other = PeerSession {
            id: 209,
            short_name: Some("pr-1122"),
            task: Some("Repair and merge PR1122"),
        };
        let change = PeerChange {
            where_: "lines 40–88".into(),
            symbols: vec!["fn run_focused(".into()],
        };
        let conflicting = vec!["scripts/ci/x.mjs".to_string()];
        let text = render_peer_note(5, &other, "scripts/ci/x.mjs", &change, Some(&conflicting));
        assert!(text.contains("session 209 (pr-1122)"), "{text}");
        assert!(
            text.contains("`scripts/ci/x.mjs` (lines 40–88 in `fn run_focused(`)"),
            "{text}"
        );
        assert!(text.contains("Git would conflict"), "{text}");
        assert!(
            text.contains("note send --session 5 --to-session 209"),
            "{text}"
        );
        assert!(text.lines().count() <= 3, "{text}");

        let clean = render_peer_note(5, &other, "scripts/ci/x.mjs", &change, Some(&[]));
        assert!(clean.contains("merged cleanly"), "{clean}");
        let unknown = render_peer_note(5, &other, "scripts/ci/x.mjs", &change, None);
        assert!(unknown.contains("Not compared yet"), "{unknown}");
    }

    /// Once per change: the same key is new once, then remembered across a
    /// reload, and a moved change is a new key.
    #[test]
    fn seen_state_delivers_each_change_once() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".git")).unwrap();
        let mut seen = SeenState::load(tmp.path(), 5);
        assert!(seen.first_time("a.rs|209|40–88"));
        assert!(!seen.first_time("a.rs|209|40–88"));
        seen.save();
        let mut reloaded = SeenState::load(tmp.path(), 5);
        assert!(!reloaded.first_time("a.rs|209|40–88"));
        assert!(reloaded.first_time("a.rs|209|40–90"));
    }
}
