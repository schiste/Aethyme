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

    // PreToolUse is the highest-frequency event that reaches the broker,
    // and it only reads. A snapshot keeps it off the write lock.
    let mut broker = match event {
        HookEvent::PreToolUse => Broker::open_snapshot(&cwd).ok()?,
        _ => Broker::open(&cwd).ok()?,
    };
    let canonical = std::fs::canonicalize(&cwd).unwrap_or(cwd);
    let session = current_session(&mut broker, &canonical);

    let outcome = match event {
        HookEvent::SessionStart => on_session_start(&mut broker, session.as_ref()),
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

/// The broker session owning this checkout, if any.
///
/// An agent working in the main checkout, or in a worktree the broker
/// does not know, has no session — that is a normal state, not an error,
/// and `SessionStart` turns it into a nudge to register.
fn current_session(broker: &mut Broker, cwd: &std::path::Path) -> Option<Session> {
    let key = cwd.to_string_lossy().to_string();
    broker.store().session_for_worktree(&key).ok().flatten()
}

fn touch(broker: &mut Broker, session: Option<&Session>) {
    if let Some(session) = session {
        let now = now_ms();
        let _ = broker.store().touch_session_activity(session.id, now);
    }
}

/// Register presence, and tell every peer that the repository just
/// became crowded.
///
/// This is the T1 transition (SOLO → COORDINATED) and it is announced by
/// the session that *causes* it, at the moment it causes it. No peer has
/// to poll to discover the new arrival, and no extra state is needed to
/// remember whether the announcement was already made — the note channel
/// itself is the memory.
fn on_session_start(broker: &mut Broker, session: Option<&Session>) -> HookOutcome {
    let Some(session) = session else {
        return HookOutcome::Context(
            "This repository coordinates concurrent agents through the Aethyme broker, and \
             this session is not registered. Before editing, run `aethyme broker status --json` \
             and then `aethyme broker start --task \"<task>\"`, and work in the worktree it \
             reports."
                .into(),
        );
    };
    touch(broker, Some(session));

    let peers = live_peers(broker, session.id);
    if peers.is_empty() {
        return HookOutcome::Context(
            "Aethyme: you are the only live session on this repository. Nothing is contended, \
             so you do not need to poll `aethyme broker status` — you will be told at your next \
             turn boundary if a second session appears."
                .into(),
        );
    }

    let joined = format!(
        "Aethyme: session {} joined this repository. You are no longer the only live session — \
         claim paths before editing shared files (`aethyme broker leases claim <path> --session \
         <id>`) and integrate through `aethyme broker submit`.",
        session.id
    );
    for peer in &peers {
        // SessionStart fires again whenever the agent's TUI restarts on
        // the same worktree. Announce a given pairing once, or a peer
        // collects one identical note per restart.
        if already_announced(broker, session.id, peer.id) {
            continue;
        }
        let _ = broker
            .store()
            .record_session_note(session.id, peer.id, &joined);
    }

    HookOutcome::Context(format!(
        "Aethyme: {} other live session(s) on this repository ({}). Work only in your own \
         worktree, claim shared paths before editing, and integrate through `aethyme broker \
         submit`.",
        peers.len(),
        describe(&peers)
    ))
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
        let _ = broker.store().acknowledge_session_note(note.id);
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

fn describe(peers: &[Session]) -> String {
    peers
        .iter()
        .map(|peer| match peer.task.as_deref() {
            Some(task) => format!("{}: {task}", peer.id),
            None => peer.id.to_string(),
        })
        .collect::<Vec<_>>()
        .join("; ")
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
