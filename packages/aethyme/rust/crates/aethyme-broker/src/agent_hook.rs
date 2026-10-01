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
        overlap: lease_overlap_summary(broker, session),
    };
    HookOutcome::Context(render_start(&facts))
}

/// "Sessions 209 (pr-1122) and 220 are changing 3 files you lease: a, b, c
/// — you'll get details when you edit them." `None` when no live session
/// overlaps this session's leases, which keeps the brief to its usual size.
fn lease_overlap_summary(broker: &Broker, session: &Session) -> Option<String> {
    let leases = broker.store_ref().active_leases().ok()?;
    let mine: Vec<&str> = leases
        .iter()
        .filter(|lease| lease.session_id == session.id)
        .map(|lease| lease.path.as_str())
        .collect();
    if mine.is_empty() {
        return None;
    }
    let mut by_session: std::collections::BTreeMap<i64, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for lease in leases.iter().filter(|lease| lease.session_id != session.id) {
        for path in &mine {
            if crate::leases::paths_overlap(&lease.path, path) {
                by_session
                    .entry(lease.session_id)
                    .or_default()
                    .insert((*path).to_string());
            }
        }
    }
    let mut names = Vec::new();
    let mut files = std::collections::BTreeSet::new();
    for (id, paths) in by_session {
        let Ok(other) = broker.store_ref().session(id) else {
            continue;
        };
        if !matches!(
            other.status,
            crate::SessionStatus::Active | crate::SessionStatus::Idle
        ) {
            continue;
        }
        names.push(match other.short_name.as_deref() {
            Some(name) => format!("{id} ({name})"),
            None => id.to_string(),
        });
        files.extend(paths);
    }
    if names.is_empty() {
        return None;
    }
    let who = match names.len() {
        1 => format!("Session {}", names[0]),
        _ => {
            let last = names.pop().unwrap_or_default();
            format!("Sessions {} and {last}", names.join(", "))
        }
    };
    let verb = if who.starts_with("Sessions") {
        "are"
    } else {
        "is"
    };
    let count = files.len();
    let sample: Vec<&str> = files.iter().take(3).map(String::as_str).collect();
    let more = count.saturating_sub(sample.len());
    let more = if more > 0 {
        format!(" (+{more} more)")
    } else {
        String::new()
    };
    let noun = if count == 1 { "file" } else { "files" };
    Some(format!(
        "Overlap: {who} {verb} changing {count} {noun} you lease: {}{more}. You'll get details when you edit them.",
        sample.join(", ")
    ))
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
    /// One line naming the live sessions changing files this session
    /// leases, when there are any.
    overlap: Option<String>,
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
                    "Next: aethyme broker start --task \"<task>\" --short-name \"<short name>\" (then work only in the worktree it reports)"
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
        lines.push("Next: aethyme broker start --reuse --task \"<follow-up task>\" --short-name \"<short name>\"".to_string());
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
    if let Some(overlap) = &facts.overlap {
        lines.push(overlap.clone());
    }
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
    deliver_notes(broker, session)
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
    let unread = snapshot
        .store_ref()
        .unread_session_notes(session.id)
        .unwrap_or_default();
    if unread.is_empty() {
        return HookOutcome::Silent;
    }
    drop(snapshot);
    let Some(mut broker) = Broker::open(cwd).ok() else {
        return HookOutcome::Silent;
    };
    deliver_notes(&mut broker, &session)
}

/// Render every unread note for `session`, each with the command that
/// answers its sender, and acknowledge them so the next turn does not
/// repeat them. `broker advanced note list --session <id>` still shows them.
fn deliver_notes(broker: &mut Broker, session: &Session) -> HookOutcome {
    let unread = broker
        .store()
        .unread_session_notes(session.id)
        .unwrap_or_default();
    if unread.is_empty() {
        return HookOutcome::Silent;
    }
    let mut lines = Vec::with_capacity(unread.len() * 2);
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
    HookOutcome::Context(lines.join("\n"))
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
    seen.save();
    cache.save();
    if notes.is_empty() {
        return HookOutcome::Silent;
    }
    HookOutcome::Context(notes.join("\n"))
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
struct SeenState {
    path: Option<std::path::PathBuf>,
    keys: Vec<String>,
    dirty: bool,
}

/// Remembered notes per session; the oldest are forgotten first.
const SEEN_CAPACITY: usize = 256;

impl SeenState {
    fn load(main_root: &std::path::Path, session_id: i64) -> Self {
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

    fn first_time(&mut self, key: &str) -> bool {
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

    fn save(&self) {
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
            next_line(&text).starts_with("aethyme broker start --task \"<task>\" --short-name"),
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
            next_line(&text).starts_with("aethyme broker start --reuse --task")
                && next_line(&text).contains("--short-name"),
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
