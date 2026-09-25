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

use std::io::Read;

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

    match decide(event, repo.as_deref(), &event_json) {
        Some(outcome) => {
            if let Some(envelope) = outcome.envelope(event) {
                println!("{envelope}");
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
) -> Option<HookOutcome> {
    let cwd = match repo {
        Some(path) => std::path::PathBuf::from(path),
        None => std::env::current_dir().ok()?,
    };
    // PostToolUse is accepted and does nothing. A turn is already bounded
    // by UserPromptSubmit and Stop, so per-tool-call liveness would be a
    // database write per tool call for information the turn boundaries
    // already carry. It stays in the enum so a plugin that wires it is a
    // no-op rather than an error.
    if event == HookEvent::PostToolUse {
        return Some(HookOutcome::Silent);
    }

    // The installation notice describes the machine, not the repository, so it
    // is computed before the broker is opened and survives an open that fails.
    // A broker that will not open is one of the things a mismatched router and
    // engine can produce, and that is the moment the notice is worth most.
    let installation_notice = match event {
        HookEvent::SessionStart => crate::install_health::session_start_warnings(now_ms()),
        _ => Vec::new(),
    };

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
        HookEvent::SessionStart => with_installation_notice(
            on_session_start(&mut broker, &canonical, session.as_ref()),
            &installation_notice,
        ),
        HookEvent::UserPromptSubmit => on_user_prompt_submit(&mut broker, session.as_ref()),
        HookEvent::PreToolUse => on_pre_tool_use(&mut broker, session.as_ref(), event_json),
        HookEvent::Stop => {
            touch(&mut broker, session.as_ref());
            HookOutcome::Silent
        }
        HookEvent::PostToolUse => HookOutcome::Silent,
    };
    Some(outcome)
}

/// Append anything wrong with the local installation to a `SessionStart`
/// outcome.
///
/// This is the one event where it belongs. A session start is rare, it is
/// already producing a paragraph, and the two things it reports — a router and
/// engine from different builds, and a release newer than the one installed —
/// are both things the agent is about to act on and cannot otherwise discover.
/// On every other event it would be noise repeated per turn.
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

fn touch(broker: &mut Broker, session: Option<&Session>) {
    if let Some(session) = session {
        let now = now_ms();
        crate::warn_unrecorded(
            "record session activity",
            broker.store().touch_session_activity(session.id, now),
        );
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
        let live = broker.store().live_sessions().unwrap_or_default();
        let tab_session = host_tab_name().and_then(|tab| {
            live.iter()
                .find(|other| {
                    !other.status.is_closed() && other.tab_name.as_deref() == Some(tab.as_str())
                })
                .map(|other| (other.id, other.worktree_path.clone()))
        });
        let facts = StartFacts {
            checkout: cwd.display().to_string(),
            tab_session,
            peers: live
                .iter()
                .filter(|other| !other.status.is_closed())
                .map(|other| (other.id, other.task.clone()))
                .collect(),
            ..StartFacts::default()
        };
        return HookOutcome::Context(render_start(&facts));
    };
    touch(broker, Some(session));

    if session.status.is_closed() {
        let facts = StartFacts {
            checkout: cwd.display().to_string(),
            session: Some(SessionFacts {
                id: session.id,
                worktree: session.worktree_path.clone(),
                task: session.task.clone(),
                closed: true,
            }),
            ..StartFacts::default()
        };
        return HookOutcome::Context(render_start(&facts));
    }

    let peers = live_peers(broker, session.id);
    if !peers.is_empty() {
        let joined = format!(
            "Aethyme: session {} joined this repository. You are no longer the only live \
             session — claim paths before editing shared files (`aethyme broker advanced \
             leases claim <path> --session <id>`) and integrate through `aethyme broker submit`.",
            session.id
        );
        for peer in &peers {
            // SessionStart fires again whenever the agent's TUI restarts on
            // the same worktree. Announce a given pairing once, or a peer
            // collects one identical note per restart.
            if already_announced(broker, session.id, peer.id) {
                continue;
            }
            crate::warn_unrecorded(
                "record the arrival note for a peer session",
                broker
                    .store()
                    .record_session_note(session.id, peer.id, &joined),
            );
        }
    }

    let blockers: Vec<crate::Blocker> = broker
        .blockers()
        .blockers
        .into_iter()
        .filter(|blocker| blocker.session_id.is_none_or(|owner| owner == session.id))
        .collect();
    let advisories = broker
        .store()
        .outstanding_advisories_for_session(session.id)
        .map(|found| found.len())
        .unwrap_or(0);
    let facts = StartFacts {
        checkout: cwd.display().to_string(),
        session: Some(SessionFacts {
            id: session.id,
            worktree: session.worktree_path.clone(),
            task: session.task.clone(),
            closed: false,
        }),
        tab_session: None,
        peers: peers
            .iter()
            .map(|peer| (peer.id, peer.task.clone()))
            .collect(),
        first_blocker: blockers.first().map(|blocker| blocker.clear.clone()),
        blocker_count: blockers.len(),
        advisories,
        unintegrated_commits: unintegrated_commits(broker, session),
    };
    HookOutcome::Context(render_start(&facts))
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

/// Commits in the session worktree that the integration branch does not hold
/// yet. Two local `git` reads; zero on any failure, which only ever costs the
/// agent a `submit` suggestion, never a wrong one.
fn unintegrated_commits(broker: &Broker, session: &Session) -> usize {
    let branch = crate::merge::PromoteConfig::load(&broker.main_root_path()).branch;
    let Ok(worktree) = crate::GitRepo::discover(std::path::Path::new(&session.worktree_path))
    else {
        return 0;
    };
    let base = if worktree.resolve_ref(&branch).is_some() {
        branch
    } else if let Some(diff_base) = session.diff_base.clone() {
        diff_base
    } else {
        return 0;
    };
    worktree
        .commit_count_between(&base, "HEAD")
        .map(|count| count as usize)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SessionFacts {
    id: i64,
    worktree: String,
    task: Option<String>,
    closed: bool,
}

/// Everything the `SessionStart` brief reports, gathered once so the
/// rendering — the part agents read — is a pure function under test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct StartFacts {
    checkout: String,
    session: Option<SessionFacts>,
    /// A live session registered for this agent's host tab, when the agent
    /// is sitting in a checkout that is not that session's worktree.
    tab_session: Option<(i64, String)>,
    peers: Vec<(i64, Option<String>)>,
    /// The clear command of the first blocker that is this session's (or the
    /// whole repository's) to clear.
    first_blocker: Option<String>,
    blocker_count: usize,
    advisories: usize,
    unintegrated_commits: usize,
}

/// Longest task text quoted in the brief. A task is a label here, not a
/// specification; the full text is one `broker status` away.
const TASK_PREVIEW_CHARS: usize = 60;

fn preview(task: &str) -> String {
    let task = task.trim();
    if task.chars().count() <= TASK_PREVIEW_CHARS {
        return task.to_string();
    }
    let cut: String = task.chars().take(TASK_PREVIEW_CHARS - 1).collect();
    format!("{}…", cut.trim_end())
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn describe_peers(peers: &[(i64, Option<String>)], other: bool) -> String {
    let other = if other { "other " } else { "" };
    if peers.is_empty() {
        return format!("no {other}live session");
    }
    let listed = peers
        .iter()
        .take(3)
        .map(|(id, task)| match task.as_deref() {
            Some(task) => format!("{id}: {}", preview(task)),
            None => id.to_string(),
        })
        .collect::<Vec<_>>()
        .join("; ");
    let more = peers.len().saturating_sub(3);
    let suffix = if more > 0 {
        format!("; +{more} more")
    } else {
        String::new()
    };
    let noun = if peers.len() == 1 {
        "session"
    } else {
        "sessions"
    };
    format!("{} {other}live {noun} ({listed}{suffix})", peers.len())
}

/// The brief: at most five lines in the normal case, always ending in one
/// `Next:` command.
fn render_start(facts: &StartFacts) -> String {
    let mut lines = Vec::new();
    let Some(session) = &facts.session else {
        lines.push(format!(
            "Aethyme: this checkout ({}) is not a broker session; {}.",
            facts.checkout,
            describe_peers(&facts.peers, false)
        ));
        match &facts.tab_session {
            Some((id, worktree)) => {
                lines.push(format!(
                    "Session {id} is registered for this tab in another worktree."
                ));
                lines.push(format!("Next: cd {}", shell_quote(worktree)));
            }
            None => {
                lines.push(
                    "Next: aethyme broker start --task \"<task>\" (then work only in the worktree it reports)"
                        .to_string(),
                );
            }
        }
        return lines.join("\n");
    };

    let task = session
        .task
        .as_deref()
        .map(|task| format!(": {}", preview(task)))
        .unwrap_or_default();
    if session.closed {
        lines.push(format!(
            "Aethyme: session {}{task} is finished; this worktree is no longer registered for new work.",
            session.id
        ));
        lines.push("Next: aethyme broker start --reuse --task \"<follow-up task>\"".to_string());
        return lines.join("\n");
    }

    lines.push(format!("Aethyme: session {}{task}", session.id));
    lines.push(format!("Worktree: {}", session.worktree));
    let blockers = match facts.blocker_count {
        0 => "no blockers".to_string(),
        1 => "1 blocker".to_string(),
        n => format!("{n} blockers"),
    };
    let advisories = match facts.advisories {
        0 => "no advisories".to_string(),
        1 => "1 outstanding advisory".to_string(),
        n => format!("{n} outstanding advisories"),
    };
    let commits = match facts.unintegrated_commits {
        0 => "nothing committed to integrate".to_string(),
        1 => "1 commit to integrate".to_string(),
        n => format!("{n} commits to integrate"),
    };
    lines.push(format!(
        "State: {}; {blockers}; {advisories}; {commits}.",
        describe_peers(&facts.peers, true)
    ));
    let next = if let Some(clear) = &facts.first_blocker {
        clear.clone()
    } else if facts.unintegrated_commits > 0 {
        format!("aethyme broker submit --session {}", session.id)
    } else if facts.advisories > 0 {
        "aethyme broker status --json (read the advisories before editing the named paths)"
            .to_string()
    } else {
        format!(
            "edit in this worktree and commit, then aethyme broker submit --session {}",
            session.id
        )
    };
    lines.push(format!("Next: {next}"));
    lines.join("\n")
}

/// Deliver what changed since the last turn, and nothing else.
///
/// The note channel is drained and acknowledged here, which is what
/// makes the SOLO claim safe to keep: an agent told it is alone will be
/// told otherwise before its next prompt, so it never has to check.
fn on_user_prompt_submit(broker: &mut Broker, session: Option<&Session>) -> HookOutcome {
    let Some(session) = session else {
        return HookOutcome::Silent;
    };
    touch(broker, Some(session));

    let unread = broker
        .store()
        .unread_session_notes(session.id)
        .unwrap_or_default();
    if unread.is_empty() {
        return HookOutcome::Silent;
    }

    let mut lines = Vec::with_capacity(unread.len());
    for note in &unread {
        lines.push(note.message.clone());
        // An unacknowledged note is delivered again next turn; say so rather
        // than let the repeat look like a new event.
        crate::warn_unrecorded(
            "acknowledge a delivered session note",
            broker.store().acknowledge_session_note(note.id),
        );
    }
    HookOutcome::Context(lines.join("\n"))
}

/// Refuse a write into a path another live session holds.
///
/// Deny only. This hook never claims a lease on the agent's behalf: the
/// whole purpose of the broker is to surface conflicts the agent did not
/// anticipate, and a hook that quietly claimed whatever was about to be
/// touched would convert every such conflict into a silent success.
fn on_pre_tool_use(
    broker: &mut Broker,
    session: Option<&Session>,
    event_json: &serde_json::Value,
) -> HookOutcome {
    let Some(session) = session else {
        return HookOutcome::Silent;
    };
    let Some(target) = write_target(event_json) else {
        return HookOutcome::Silent;
    };
    // Leases are repo-relative, but the agent is editing inside its own
    // worktree, so the session's checkout is the prefix that matters. The
    // main checkout is the fallback for an agent working there directly.
    let Some(relative) = repo_relative(&target, &session.worktree_path, broker.main_root()) else {
        // Outside every checkout we know: not ours to police.
        return HookOutcome::Silent;
    };

    let leases = broker.store().active_leases().unwrap_or_default();
    let held = leases.iter().find(|lease| {
        lease.session_id != session.id && crate::leases::paths_overlap(&lease.path, &relative)
    });
    let Some(held) = held else {
        return HookOutcome::Silent;
    };

    HookOutcome::Deny(format!(
        "Aethyme: `{}` is leased by session {}. Editing it here would conflict with work \
         already in flight. Coordinate with that session, or claim a path you own instead.",
        held.path, held.session_id
    ))
}

/// The absolute path a tool call is about to write, when the event names
/// one. Read-only tools and tools with no path produce `None`, which is
/// how the common case stays free.
fn write_target(event_json: &serde_json::Value) -> Option<std::path::PathBuf> {
    let tool = event_json.get("tool_name")?.as_str()?;
    if !matches!(tool, "Edit" | "Write" | "NotebookEdit" | "MultiEdit") {
        return None;
    }
    let input = event_json.get("tool_input")?;
    let path = input
        .get("file_path")
        .or_else(|| input.get("notebook_path"))?
        .as_str()?;
    Some(std::path::PathBuf::from(path))
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

/// True when `sender` has already put a note in `recipient`'s queue,
/// read or not. One announcement per pairing, for the life of the pair.
fn already_announced(broker: &mut Broker, sender: i64, recipient: i64) -> bool {
    broker
        .store()
        .session_notes(recipient)
        .unwrap_or_default()
        .iter()
        .any(|note| note.sender_session_id == sender)
}

fn live_peers(broker: &mut Broker, self_id: i64) -> Vec<Session> {
    broker
        .store()
        .live_sessions()
        .unwrap_or_default()
        .into_iter()
        .filter(|other| other.id != self_id && !other.status.is_closed())
        .collect()
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

    fn open_session(id: i64) -> SessionFacts {
        SessionFacts {
            id,
            worktree: "/wt/s7".into(),
            task: Some("fix the parser".into()),
            closed: false,
        }
    }

    fn next_line(text: &str) -> &str {
        text.lines()
            .last()
            .and_then(|line| line.strip_prefix("Next: "))
            .unwrap_or_else(|| panic!("the brief must end in one Next: line: {text}"))
    }

    /// P4.7: the brief is state plus one command, never a paragraph of rules.
    #[test]
    fn unregistered_checkout_is_told_to_start() {
        let text = render_start(&StartFacts {
            checkout: "/repo".into(),
            peers: vec![(3, Some("other work".into()))],
            ..StartFacts::default()
        });
        assert!(text.lines().count() <= 5, "{text}");
        assert!(text.contains("not a broker session"), "{text}");
        assert!(text.contains("1 live session (3: other work)"), "{text}");
        assert!(
            next_line(&text).starts_with("aethyme broker start --task \"<task>\""),
            "{text}"
        );
    }

    #[test]
    fn a_checkout_other_than_the_tab_session_is_told_to_cd() {
        let text = render_start(&StartFacts {
            checkout: "/repo".into(),
            tab_session: Some((7, "/wt/it's here".into())),
            ..StartFacts::default()
        });
        assert_eq!(next_line(&text), "cd '/wt/it'\\''s here'", "{text}");
    }

    #[test]
    fn a_finished_session_is_told_to_reuse() {
        let text = render_start(&StartFacts {
            checkout: "/wt/s7".into(),
            session: Some(SessionFacts {
                closed: true,
                ..open_session(7)
            }),
            ..StartFacts::default()
        });
        assert!(
            next_line(&text).starts_with("aethyme broker start --reuse --task"),
            "{text}"
        );
    }

    #[test]
    fn next_action_prefers_blocker_then_commits_then_advisories() {
        let base = StartFacts {
            checkout: "/wt/s7".into(),
            session: Some(open_session(7)),
            ..StartFacts::default()
        };
        let idle = render_start(&base);
        assert_eq!(idle.lines().count(), 4, "{idle}");
        assert!(idle.contains("Worktree: /wt/s7"), "{idle}");
        assert!(
            idle.contains("no other live session; no blockers; no advisories"),
            "{idle}"
        );
        assert!(
            next_line(&idle).ends_with("aethyme broker submit --session 7"),
            "{idle}"
        );

        let advised = render_start(&StartFacts {
            advisories: 2,
            ..base.clone()
        });
        assert!(
            next_line(&advised).starts_with("aethyme broker status --json"),
            "{advised}"
        );

        let committed = render_start(&StartFacts {
            advisories: 2,
            unintegrated_commits: 3,
            ..base.clone()
        });
        assert!(committed.contains("3 commits to integrate"), "{committed}");
        assert_eq!(next_line(&committed), "aethyme broker submit --session 7");

        let blocked = render_start(&StartFacts {
            advisories: 2,
            unintegrated_commits: 3,
            blocker_count: 1,
            first_blocker: Some("aethyme broker unblock op:4 --outcome succeeded".into()),
            ..base
        });
        assert!(blocked.contains("1 blocker"), "{blocked}");
        assert_eq!(
            next_line(&blocked),
            "aethyme broker unblock op:4 --outcome succeeded"
        );
        assert!(blocked.lines().count() <= 5, "{blocked}");
    }

    #[test]
    fn long_task_text_is_previewed_not_dumped() {
        let long = "x".repeat(200);
        let text = render_start(&StartFacts {
            checkout: "/wt/s7".into(),
            session: Some(SessionFacts {
                task: Some(long),
                ..open_session(7)
            }),
            ..StartFacts::default()
        });
        assert!(text.lines().next().unwrap().chars().count() < 100, "{text}");
    }

    /// Read-only tools cost nothing: no lease lookup happens at all.
    #[test]
    fn only_writing_tools_name_a_target() {
        let write = serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {"file_path": "/wt/s1/a.rs"},
        });
        assert_eq!(
            write_target(&write),
            Some(std::path::PathBuf::from("/wt/s1/a.rs"))
        );

        let read = serde_json::json!({
            "tool_name": "Read",
            "tool_input": {"file_path": "/wt/s1/a.rs"},
        });
        assert_eq!(write_target(&read), None);

        let notebook = serde_json::json!({
            "tool_name": "NotebookEdit",
            "tool_input": {"notebook_path": "/wt/s1/a.ipynb"},
        });
        assert_eq!(
            write_target(&notebook),
            Some(std::path::PathBuf::from("/wt/s1/a.ipynb"))
        );

        assert_eq!(write_target(&serde_json::Value::Null), None);
        assert_eq!(
            write_target(&serde_json::json!({"tool_name": "Edit"})),
            None
        );
    }
}
