//! Transcript archiving: deterministic tests over temporary directories and
//! invented names. Nothing here reads a real agent home.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::agents::{self, AgentProbe, ClaudeProcess};
use super::archive::{self, Hooks, NoHooks};
use super::config::{self, SettingsChange, TranscriptsConfig};
use super::eligibility::{
    self, CheckoutState, DAY_MS, Facts, Linkage, LinkedSession, OpenState, SkipReason,
};
use super::index::Index;
use super::linkage;
use super::open_files::{self, OpenFile, OpenFileProbe, OpenFiles};
use super::pacing::{self, Pacer};
use super::providers::{self, Provider};
use super::run::{self, Options, Probes};
use super::safe_fs::Root;
use super::{Environment, LaneLock};

const UUID: &str = "0b9f6c1e-2d3a-4b5c-8d7e-1f2a3b4c5d6e";
const OTHER_UUID: &str = "7a1e2f3d-4c5b-4a69-9b8c-0d1e2f3a4b5c";

// ── fixtures ─────────────────────────────────────────────────────────

/// An open-file probe that reports a fixed set of open paths.
#[derive(Default)]
struct FakeProbe {
    open: Vec<PathBuf>,
    incomplete: bool,
}

impl OpenFileProbe for FakeProbe {
    fn observe(&mut self) -> Result<Vec<OpenFile>, String> {
        if self.incomplete {
            return Err("listing incomplete (test)".into());
        }
        Ok(self
            .open
            .iter()
            .map(|path| OpenFile {
                pid: 4242,
                path: path.clone(),
            })
            .collect())
    }
}

fn probe(open: Vec<PathBuf>) -> OpenFiles<FakeProbe> {
    OpenFiles::new(
        FakeProbe {
            open,
            incomplete: false,
        },
        Duration::ZERO,
    )
}

struct Host {
    _temp: tempfile::TempDir,
    root: PathBuf,
    environment: Environment,
}

impl Host {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(temp.path()).unwrap();
        let environment = Environment {
            host_dir: root.join("host-state").join(super::HOST_DIRECTORY),
            home: None,
            codex_home: Some(root.join("codex-home")),
            claude_home: Some(root.join("claude-home")),
            use_registry: false,
            free_space_override: Some(1 << 40),
        };
        Self {
            _temp: temp,
            root,
            environment,
        }
    }

    fn codex_dir(&self) -> PathBuf {
        self.root.join("codex-home")
    }

    /// A Codex transcript recording `cwd`, with compressible content.
    fn codex_transcript(&self, uuid: &str, cwd: &Path) -> PathBuf {
        let directory = self.codex_dir().join("sessions/2026/01/02");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("rollout-2026-01-02T03-04-05-{uuid}.jsonl"));
        let mut text = format!(
            "{{\"timestamp\":\"2026-01-02T03:04:05Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{uuid}\",\"cwd\":\"{}\"}}}}\n",
            cwd.display()
        );
        for line in 0..2000 {
            text.push_str(&format!(
                "{{\"type\":\"response_item\",\"payload\":{{\"line\":{line},\"text\":\"an invented transcript line\"}}}}\n"
            ));
        }
        fs::write(&path, text).unwrap();
        path
    }

    fn claude_transcript(&self, uuid: &str) -> PathBuf {
        let directory = self
            .root
            .join("claude-home/projects/-work-invented-project");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{uuid}.jsonl"));
        let mut text = String::new();
        for line in 0..1000 {
            text.push_str(&format!(
                "{{\"type\":\"user\",\"cwd\":\"/work/invented-project\",\"n\":{line}}}\n"
            ));
        }
        fs::write(&path, text).unwrap();
        path
    }
}

fn enabled(providers: Vec<Provider>) -> TranscriptsConfig {
    TranscriptsConfig {
        archive: true,
        providers,
        // Claude's own default (14 days) is covered by its own test.
        archive_after_inactive_days_claude: 3,
        // Fast enough that tests never wait on pacing.
        bytes_per_second: 1 << 40,
        ..TranscriptsConfig::default()
    }
}

fn later() -> i64 {
    super::now_ms() + 10 * DAY_MS
}

fn apply() -> Options {
    Options {
        apply: true,
        budget: None,
    }
}

fn dry() -> Options {
    Options {
        apply: false,
        budget: None,
    }
}

/// A process table with a fixed set of Claude Code and Codex processes.
#[derive(Default)]
struct FakeAgents {
    processes: Option<Vec<ClaudeProcess>>,
    codex: Option<Vec<String>>,
    calls: u64,
}

impl FakeAgents {
    fn none() -> Self {
        Self {
            processes: Some(Vec::new()),
            codex: Some(Vec::new()),
            calls: 0,
        }
    }
}

impl AgentProbe for FakeAgents {
    fn claude_processes(&mut self) -> Result<Vec<ClaudeProcess>, String> {
        self.calls += 1;
        self.processes
            .clone()
            .ok_or_else(|| "process table unavailable (test)".to_string())
    }

    fn codex_command_lines(&mut self) -> Result<Vec<String>, String> {
        self.calls += 1;
        self.codex
            .clone()
            .ok_or_else(|| "process table unavailable (test)".to_string())
    }
}

fn restore_with(
    host: &Host,
    config: &TranscriptsConfig,
    key: &str,
    open: &mut OpenFiles<FakeProbe>,
    agents: &mut FakeAgents,
) -> Result<Vec<archive::Restored>, String> {
    let mut probes = Probes {
        open_files: open,
        agents,
    };
    run::restore(&host.environment, config, key, &mut probes)
}

fn restore(
    host: &Host,
    config: &TranscriptsConfig,
    key: &str,
) -> Result<Vec<archive::Restored>, String> {
    restore_with(
        host,
        config,
        key,
        &mut probe(Vec::new()),
        &mut FakeAgents::none(),
    )
}

fn pass_at<P: OpenFileProbe>(
    host: &Host,
    config: &TranscriptsConfig,
    options: Options,
    open: &mut OpenFiles<P>,
    agents: &mut dyn AgentProbe,
    hooks: &mut dyn Hooks,
    now_ms: i64,
) -> run::RunReport {
    let mut probes = Probes {
        open_files: open,
        agents,
    };
    run::run_pass(
        &host.environment,
        config,
        options,
        &mut probes,
        hooks,
        now_ms,
    )
    .unwrap()
}

fn pass(
    host: &Host,
    config: &TranscriptsConfig,
    options: Options,
    open: &mut OpenFiles<FakeProbe>,
    hooks: &mut dyn Hooks,
) -> run::RunReport {
    pass_at(
        host,
        config,
        options,
        open,
        &mut FakeAgents::none(),
        hooks,
        later(),
    )
}

fn status(
    host: &Host,
    config: &TranscriptsConfig,
    open: &mut OpenFiles<FakeProbe>,
) -> run::StatusReport {
    let mut agents = FakeAgents::none();
    let mut probes = Probes {
        open_files: open,
        agents: &mut agents,
    };
    run::status(&host.environment, config, &mut probes).unwrap()
}

fn decompress(path: &Path) -> Vec<u8> {
    let mut decoder = zstd::stream::read::Decoder::new(fs::File::open(path).unwrap()).unwrap();
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut bytes).unwrap();
    bytes
}

fn codex_destination(host: &Host, source: &Path) -> PathBuf {
    host.codex_dir().join("archived_sessions").join(format!(
        "{}.zst",
        source.file_name().unwrap().to_str().unwrap()
    ))
}

/// A repository with a minimal broker database: one session whose holder
/// command names `uuid`.
struct Repository {
    root: PathBuf,
    db: PathBuf,
    checkout: PathBuf,
}

impl Repository {
    fn new(host: &Host, uuid: &str, cleanup_state: &str) -> Self {
        let root = host.root.join("invented-repository");
        let checkout = host.root.join("worktrees/invented-session");
        fs::create_dir_all(root.join(".aethyme")).unwrap();
        fs::create_dir_all(&checkout).unwrap();
        let db = root.join(crate::BROKER_DB_RELPATH);
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id INTEGER PRIMARY KEY, worktree_path TEXT NOT NULL,
                                    cleanup_state TEXT NOT NULL DEFAULT 'open',
                                    last_activity_at INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE events (id INTEGER PRIMARY KEY AUTOINCREMENT, schema_version INTEGER,
                                  ts INTEGER, kind TEXT NOT NULL, session_id INTEGER,
                                  payload_json TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, worktree_path, cleanup_state) VALUES (7, ?1, ?2)",
            rusqlite::params![checkout.to_str().unwrap(), cleanup_state],
        )
        .unwrap();
        let payload = serde_json::json!({
            "pid": 0,
            "started": "Thu Jan  1 00:00:00 2026",
            "command": format!("node /opt/invented/codex resume {uuid}"),
            "reason": "registered",
        });
        conn.execute(
            "INSERT INTO events (schema_version, ts, kind, session_id, payload_json)
             VALUES (1, 0, ?1, 7, ?2)",
            rusqlite::params![crate::events::SESSION_HOLDER_BOUND, payload.to_string()],
        )
        .unwrap();
        Self { root, db, checkout }
    }

    fn set_cleanup_state(&self, state: &str) {
        let conn = rusqlite::Connection::open(&self.db).unwrap();
        conn.execute(
            "UPDATE sessions SET cleanup_state = ?1 WHERE id = 7",
            [state],
        )
        .unwrap();
    }
}

// ── eligibility (pure) ───────────────────────────────────────────────

fn session(state: &str, checkout: CheckoutState) -> LinkedSession {
    LinkedSession {
        cleanup_state: Some(state.into()),
        checkout,
        last_activity_ms: None,
    }
}

/// Name, modification time, linkage, open state, expected verdict.
type Case = (
    &'static str,
    Option<i64>,
    Linkage,
    OpenState,
    Result<(), SkipReason>,
);

#[test]
fn eligibility_table() {
    let now = 100 * DAY_MS;
    let old = Some(now - 4 * DAY_MS);
    let closed_gone = Linkage::Linked(vec![session("closed", CheckoutState::Absent)]);
    let cases: Vec<Case> = vec![
        (
            "unlinked, old, closed",
            old,
            Linkage::Unlinked,
            OpenState::Closed,
            Ok(()),
        ),
        (
            "linked, closed, checkout gone",
            old,
            closed_gone.clone(),
            OpenState::Closed,
            Ok(()),
        ),
        (
            "cleaned counts as closed",
            old,
            Linkage::Linked(vec![session("cleaned", CheckoutState::Absent)]),
            OpenState::Closed,
            Ok(()),
        ),
        (
            "mtime unreadable",
            None,
            Linkage::Unlinked,
            OpenState::Closed,
            Err(SkipReason::MtimeUnreadable),
        ),
        (
            "exactly three days is not more than three",
            Some(now - 3 * DAY_MS),
            Linkage::Unlinked,
            OpenState::Closed,
            Err(SkipReason::TooRecent),
        ),
        (
            "modified in the future",
            Some(now + DAY_MS),
            Linkage::Unlinked,
            OpenState::Closed,
            Err(SkipReason::TooRecent),
        ),
        (
            "one open session among closed ones",
            old,
            Linkage::Linked(vec![
                session("closed", CheckoutState::Absent),
                session("open", CheckoutState::Absent),
            ]),
            OpenState::Closed,
            Err(SkipReason::SessionOpen),
        ),
        (
            "unknown cleanup state",
            old,
            Linkage::Linked(vec![LinkedSession {
                cleanup_state: None,
                checkout: CheckoutState::Absent,
                last_activity_ms: None,
            }]),
            OpenState::Closed,
            Err(SkipReason::SessionOpen),
        ),
        (
            "closed but checkout still present",
            old,
            Linkage::Linked(vec![session("closed", CheckoutState::Present)]),
            OpenState::Closed,
            Err(SkipReason::CheckoutPresent),
        ),
        (
            "checkout existence unknown",
            old,
            Linkage::Linked(vec![session("cleaned", CheckoutState::Unknown)]),
            OpenState::Closed,
            Err(SkipReason::CheckoutUnknown),
        ),
        (
            "linkage unreadable",
            old,
            Linkage::Unreadable,
            OpenState::Closed,
            Err(SkipReason::LinkageUnreadable),
        ),
        (
            "open by a process",
            old,
            closed_gone.clone(),
            OpenState::Open,
            Err(SkipReason::OpenByProcess),
        ),
        (
            "open check incomplete",
            old,
            Linkage::Unlinked,
            OpenState::Unknown,
            Err(SkipReason::OpenCheckUnavailable),
        ),
        (
            "age is judged before the open check",
            Some(now - DAY_MS),
            Linkage::Unlinked,
            OpenState::Open,
            Err(SkipReason::TooRecent),
        ),
    ];
    for (name, modified_ms, linkage, open, expected) in cases {
        let facts = Facts {
            now_ms: now,
            modified_ms,
            record_ms: None,
            inactive_days: 3,
            linkage: &linkage,
        };
        assert_eq!(eligibility::judge(&facts, open), expected, "{name}");
    }
}

#[test]
fn configured_age_is_respected() {
    let now = 100 * DAY_MS;
    assert_eq!(
        eligibility::judge_age(now, Some(now - 4 * DAY_MS), 7),
        Err(SkipReason::TooRecent)
    );
    assert_eq!(
        eligibility::judge_age(now, Some(now - 8 * DAY_MS), 7),
        Ok(())
    );
}

// ── names and parsing ────────────────────────────────────────────────

#[test]
fn codex_destination_keeps_the_name_and_adds_zst() {
    let archived = Path::new("/invented/codex/archived_sessions");
    let source = Path::new(
        "/invented/codex/sessions/2026/01/02/rollout-2026-01-02T03-04-05-0b9f6c1e-2d3a-4b5c-8d7e-1f2a3b4c5d6e.jsonl",
    );
    assert_eq!(
        providers::codex_destination(archived, source).unwrap(),
        archived.join("rollout-2026-01-02T03-04-05-0b9f6c1e-2d3a-4b5c-8d7e-1f2a3b4c5d6e.jsonl.zst")
    );
    assert_eq!(
        providers::codex_destination(archived, Path::new("/x/notes.txt")),
        None
    );
    assert_eq!(
        providers::codex_session_path_for(
            Path::new("/invented/codex/sessions"),
            "rollout-2026-01-02T03-04-05-0b9f6c1e-2d3a-4b5c-8d7e-1f2a3b4c5d6e.jsonl"
        )
        .unwrap(),
        Path::new(
            "/invented/codex/sessions/2026/01/02/rollout-2026-01-02T03-04-05-0b9f6c1e-2d3a-4b5c-8d7e-1f2a3b4c5d6e.jsonl"
        )
    );
}

#[test]
fn claude_destination_mirrors_the_project_directory() {
    let projects = Path::new("/invented/claude/projects");
    let archive = Path::new("/invented/state/transcripts/claude");
    let source = projects.join(format!("-work-invented/{UUID}.jsonl"));
    assert_eq!(
        providers::claude_destination(archive, projects, &source).unwrap(),
        archive.join(format!("-work-invented/{UUID}.jsonl.zst"))
    );
    let nested = projects.join(format!("-work-invented/sub/{UUID}.jsonl"));
    assert_eq!(
        providers::claude_destination(archive, projects, &nested),
        None
    );
}

#[test]
fn session_ids_come_from_file_names_and_commands() {
    assert_eq!(
        providers::session_id_from_file_name(
            Provider::Codex,
            &format!("rollout-2026-01-02T03-04-05-{UUID}.jsonl.zst")
        )
        .as_deref(),
        Some(UUID)
    );
    assert_eq!(
        providers::session_id_from_file_name(Provider::Claude, &format!("{UUID}.jsonl")).as_deref(),
        Some(UUID)
    );
    assert_eq!(
        providers::session_id_from_file_name(Provider::Codex, &format!("{UUID}.jsonl")),
        None
    );
    let upper = UUID.to_ascii_uppercase();
    assert_eq!(
        providers::uuids_in(&format!(
            "codex --flag resume {upper} && codex exec resume {OTHER_UUID}"
        )),
        vec![UUID.to_string(), OTHER_UUID.to_string()]
    );
    // Part of a longer hexadecimal run is not a standalone UUID.
    assert!(providers::uuids_in(&format!("{UUID}0")).is_empty());
    assert!(providers::uuids_in("no ids here, ünïcode ok").is_empty());
}

#[test]
fn settings_parse_and_refuse_unknown_keys() {
    assert_eq!(
        TranscriptsConfig::parse("").unwrap(),
        TranscriptsConfig::default()
    );
    let config = TranscriptsConfig::parse(
        "[transcripts]\narchive = true\narchive_after_days = 5\nproviders = [\"claude\", \"codex\", \"claude\"]\nrepositories = [\"/invented/repo\"]\n",
    )
    .unwrap();
    assert!(config.archive);
    assert_eq!(config.archive_after_inactive_days, 5);
    assert_eq!(config.enabled_source, config::EnabledSource::Settings);
    assert_eq!(config.providers, vec![Provider::Claude, Provider::Codex]);
    assert_eq!(config.repositories, vec![PathBuf::from("/invented/repo")]);
    assert!(TranscriptsConfig::parse("[transcripts]\narchiv = true\n").is_err());
    assert!(TranscriptsConfig::parse("[transcripts]\nproviders = [\"other\"]\n").is_err());
    assert!(TranscriptsConfig::parse("[transcripts]\narchive_after_days = 0\n").is_err());
    assert!(TranscriptsConfig::parse("[transcripts]\ncodex_home = \"relative\"\n").is_err());
    assert_eq!(
        TranscriptsConfig {
            bytes_per_second: 1,
            ..TranscriptsConfig::default()
        }
        .effective_bytes_per_second(),
        config::MIN_BYTES_PER_SECOND
    );
}

#[test]
fn enable_and_disable_write_only_the_settings_file_and_keep_other_lines() {
    let host = Host::new();
    let host_dir = &host.environment.host_dir;
    fs::create_dir_all(host_dir).unwrap();
    fs::write(
        host_dir.join(config::SETTINGS_FILE),
        "# owner note\n[transcripts]\nrepositories = [\"/invented/repo\"]\n",
    )
    .unwrap();
    let config = config::write_settings(
        host_dir,
        &SettingsChange {
            archive: true,
            archive_after_inactive_days: Some(4),
            providers: Some(vec![Provider::Codex, Provider::Claude]),
        },
    )
    .unwrap();
    assert!(config.archive);
    assert_eq!(config.archive_after_inactive_days, 4);
    assert_eq!(config.enabled_source, config::EnabledSource::Settings);
    assert_eq!(config.repositories, vec![PathBuf::from("/invented/repo")]);
    let text = fs::read_to_string(host_dir.join(config::SETTINGS_FILE)).unwrap();
    assert!(text.contains("# owner note"), "{text}");
    let disabled = config::write_settings(
        host_dir,
        &SettingsChange {
            archive: false,
            ..SettingsChange::default()
        },
    )
    .unwrap();
    assert!(!disabled.archive);
    assert_eq!(disabled.archive_after_inactive_days, 4);
    assert_eq!(disabled.enabled_source, config::EnabledSource::Settings);
    let mut names: Vec<String> = fs::read_dir(host_dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec![config::SETTINGS_FILE.to_string()]);
}

#[test]
fn pacing_floors_the_rate_and_unpaces_below_the_free_space_floor() {
    let floor = 10 * 1024 * 1024 * 1024;
    assert_eq!(
        pacing::choose_rate(1, Some(floor * 2), floor),
        Some(config::MIN_BYTES_PER_SECOND)
    );
    assert_eq!(pacing::choose_rate(64 << 20, None, floor), Some(64 << 20));
    assert_eq!(pacing::choose_rate(64 << 20, Some(floor - 1), floor), None);
    let mut pacer = Pacer::new(None);
    pacer.consume(u64::MAX / 2);
    assert_eq!(pacer.slept(), Duration::ZERO);
}

// ── open files ───────────────────────────────────────────────────────

#[test]
fn proc_listing_reads_descriptors_beneath_the_root() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let watched = root.join("watched");
    fs::create_dir_all(&watched).unwrap();
    let transcript = watched.join("a.jsonl");
    fs::write(&transcript, b"x").unwrap();
    let proc_root = root.join("proc");
    fs::create_dir_all(proc_root.join("123/fd")).unwrap();
    fs::create_dir_all(proc_root.join("self-not-a-pid/fd")).unwrap();
    fs::create_dir_all(proc_root.join("456/fd")).unwrap();
    std::os::unix::fs::symlink(&transcript, proc_root.join("123/fd/3")).unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), proc_root.join("456/fd/3")).unwrap();
    let open = open_files::proc_open_under(&proc_root, &watched).unwrap();
    assert_eq!(
        open,
        vec![OpenFile {
            pid: 123,
            path: transcript.clone()
        }]
    );
    assert_eq!(
        open_files::pid_open_under(&proc_root, 123, &watched),
        vec![transcript]
    );
    assert!(open_files::pid_open_under(&proc_root, 456, &watched).is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn a_file_this_process_holds_open_is_seen_in_proc() {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    let transcript = root.join("held.jsonl");
    fs::write(&transcript, b"x").unwrap();
    let _held = fs::File::open(&transcript).unwrap();
    let open = open_files::proc_open_under(Path::new("/proc"), &root).unwrap();
    assert!(
        open.iter()
            .any(|file| file.pid == std::process::id() && file.path == transcript),
        "{open:?}"
    );
}

// ── whole passes ─────────────────────────────────────────────────────

#[test]
fn disabled_settings_do_nothing_at_all() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = TranscriptsConfig {
        archive: false,
        enabled_source: config::EnabledSource::Settings,
        providers: vec![Provider::Codex, Provider::Claude],
        ..TranscriptsConfig::default()
    };
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(!report.enabled);
    assert_eq!(report.examined, 0);
    assert_eq!(report.lock, "not_taken");
    assert_eq!(open.probes, 0, "no open-file listing was taken");
    assert!(source.is_file());
    assert!(!host.codex_dir().join("archived_sessions").exists());
    assert!(
        !host.environment.host_dir.exists(),
        "no state directory, lock or index is created"
    );
    let status = status(&host, &config, &mut open);
    assert!(!status.enabled);
    assert_eq!(status.reclaimable_now_bytes, 0);
    assert!(!host.environment.host_dir.exists());
}

#[test]
fn an_unlinked_old_codex_transcript_is_archived_where_codex_reads_it() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let original = fs::read(&source).unwrap();
    let config = enabled(vec![Provider::Codex]);

    let mut open = probe(Vec::new());
    let report = pass(&host, &config, dry(), &mut open, &mut NoHooks);
    assert_eq!(report.eligible.len(), 1);
    assert!(report.archived.is_empty());
    assert!(source.is_file(), "a dry run moves nothing");

    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.lock, "acquired");
    assert_eq!(report.archived.len(), 1, "{report:?}");
    let destination = codex_destination(&host, &source);
    assert_eq!(report.archived[0].destination, destination);
    assert!(!source.exists());
    assert_eq!(decompress(&destination), original);
    assert!(report.bytes_out < report.bytes_in);
    let index = Index::open(&host.environment.host_dir).unwrap();
    let rows = index.find_archives(UUID).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].original_path, source);
    let counters = index.counters().unwrap();
    assert_eq!(counters.get("archived"), Some(&1));
    assert_eq!(counters.get("bytes_in"), Some(&(original.len() as u64)));
}

#[test]
fn an_open_transcript_is_skipped() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(vec![source.clone()]);
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.archived.is_empty());
    assert_eq!(report.skipped.get("open_by_process"), Some(&1));
    assert!(source.is_file());

    let mut incomplete = OpenFiles::new(
        FakeProbe {
            open: Vec::new(),
            incomplete: true,
        },
        Duration::ZERO,
    );
    let report = pass(&host, &config, apply(), &mut incomplete, &mut NoHooks);
    assert!(report.archived.is_empty());
    assert_eq!(report.skipped.get("open_check_unavailable"), Some(&1));
    assert!(source.is_file());
}

#[cfg(target_os = "linux")]
#[test]
fn a_transcript_held_open_by_a_real_process_is_skipped() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let _held = fs::File::open(&source).unwrap();
    let mut open = OpenFiles::new(super::open_files::SystemProbe, Duration::ZERO);
    let report = pass_at(
        &host,
        &config,
        apply(),
        &mut open,
        &mut FakeAgents::none(),
        &mut NoHooks,
        later(),
    );
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("open_by_process"), Some(&1));
    assert!(source.is_file());
}

#[test]
fn a_recent_transcript_is_left_alone() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass_at(
        &host,
        &config,
        apply(),
        &mut open,
        &mut FakeAgents::none(),
        &mut NoHooks,
        super::now_ms(),
    );
    assert!(report.archived.is_empty());
    assert_eq!(report.skipped.get("too_recent"), Some(&1));
    assert!(source.is_file());
}

#[test]
fn linked_open_sessions_and_present_checkouts_never_archive() {
    let host = Host::new();
    let repository = Repository::new(&host, UUID, "open");
    // The transcript's cwd names the main repository, which locates the
    // broker database; the holder command names the session.
    let source = host.codex_transcript(UUID, &repository.root);
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());

    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.broker_databases, 1, "{report:?}");
    assert!(report.links_added >= 1);
    assert_eq!(report.skipped.get("session_open"), Some(&1), "{report:?}");
    assert!(source.is_file());

    repository.set_cleanup_state("closed");
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(
        report.skipped.get("checkout_present"),
        Some(&1),
        "{report:?}"
    );
    assert!(source.is_file());

    fs::remove_dir_all(&repository.checkout).unwrap();
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(!source.exists());
}

#[test]
fn backfill_reads_holder_commands_once() {
    let host = Host::new();
    let repository = Repository::new(&host, OTHER_UUID, "closed");
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert_eq!(linkage::backfill(&index, &repository.db, 1).unwrap(), 1);
    assert_eq!(linkage::backfill(&index, &repository.db, 2).unwrap(), 0);
    let links = index
        .links_for(Some(OTHER_UUID), Path::new("/nowhere"))
        .unwrap();
    assert_eq!(links, vec![(repository.db.clone(), 7)]);
}

/// Appends to the source after it was compressed.
struct AppendAfterCompress;

impl Hooks for AppendAfterCompress {
    fn after_compress(&mut self, source: &Path) {
        let mut file = fs::OpenOptions::new().append(true).open(source).unwrap();
        file.write_all(b"{\"late\":true}\n").unwrap();
    }
}

#[test]
fn a_transcript_modified_during_compression_is_skipped_and_kept() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut AppendAfterCompress);
    assert!(report.archived.is_empty());
    assert_eq!(report.skipped.get("modified_during_compression"), Some(&1));
    assert!(source.is_file());
    assert!(
        fs::read_to_string(&source)
            .unwrap()
            .ends_with("{\"late\":true}\n")
    );
    assert!(!codex_destination(&host, &source).exists());
    let leftovers: Vec<_> = fs::read_dir(host.codex_dir().join("archived_sessions"))
        .unwrap()
        .collect();
    assert!(leftovers.is_empty(), "the discarded copy is removed");
}

/// Replaces the compressed copy with bytes that do not decode.
struct CorruptBeforeVerify;

impl Hooks for CorruptBeforeVerify {
    fn before_verify(&mut self, temporary: &Path) {
        fs::write(temporary, b"definitely not a zstd frame").unwrap();
    }
}

#[test]
fn a_verify_failure_keeps_the_source() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let original = fs::read(&source).unwrap();
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut CorruptBeforeVerify);
    assert!(report.archived.is_empty());
    assert_eq!(report.verify_failures, 1);
    assert_eq!(report.skipped.get("verify_failed"), Some(&1));
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(!codex_destination(&host, &source).exists());
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());
    assert_eq!(index.counters().unwrap().get("verify_failures"), Some(&1));
}

/// Stops after publishing, as a crash before the source removal would.
struct CrashAfterPublish;

impl Hooks for CrashAfterPublish {
    fn after_publish(&mut self, _source: &Path) -> bool {
        false
    }
}

#[test]
fn a_crash_between_publish_and_removal_is_finished_by_the_next_run() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let original = fs::read(&source).unwrap();
    let destination = codex_destination(&host, &source);
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());

    let report = pass(&host, &config, apply(), &mut open, &mut CrashAfterPublish);
    assert!(report.archived.is_empty());
    assert!(
        source.is_file(),
        "the source is never removed before it is complete"
    );
    assert_eq!(decompress(&destination), original, "the copy is complete");

    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(report.archived[0].resumed);
    assert_eq!(report.resumed, 1);
    assert!(!source.exists());
    assert_eq!(decompress(&destination), original);
}

#[test]
fn a_leftover_temporary_file_is_removed_by_the_next_run() {
    let host = Host::new();
    host.codex_transcript(UUID, Path::new("/work/invented"));
    let archived = host.codex_dir().join("archived_sessions");
    fs::create_dir_all(&archived).unwrap();
    let leftover = archived.join(format!("{}crashed.part", providers::TEMP_PREFIX));
    fs::write(&leftover, b"partial").unwrap();
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());

    let report = pass(&host, &config, dry(), &mut open, &mut NoHooks);
    assert_eq!(report.leftover_temps_removed, 0);
    assert!(leftover.exists(), "a dry run removes nothing");

    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.leftover_temps_removed, 1);
    assert!(!leftover.exists());
}

#[test]
fn a_held_lane_lock_makes_a_pass_do_nothing() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let _held = LaneLock::try_acquire(&host.environment.host_dir)
        .unwrap()
        .unwrap();
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.lock, "busy");
    assert_eq!(report.examined, 0);
    assert!(source.is_file());
    let holder = LaneLock::recorded_holder(&host.environment.host_dir).unwrap();
    assert!(
        holder.contains(&format!("pid={}", std::process::id())),
        "{holder}"
    );
}

#[test]
fn largest_transcripts_are_archived_first() {
    let host = Host::new();
    let small = host.codex_transcript(UUID, Path::new("/work/invented"));
    let large = host.codex_transcript(OTHER_UUID, Path::new("/work/invented"));
    let mut file = fs::OpenOptions::new().append(true).open(&large).unwrap();
    file.write_all(&[b'\n'; 64 * 1024]).unwrap();
    drop(file);
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, dry(), &mut open, &mut NoHooks);
    let order: Vec<&Path> = report
        .eligible
        .iter()
        .map(|view| view.path.as_path())
        .collect();
    assert_eq!(order, vec![large.as_path(), small.as_path()]);
}

#[test]
fn restore_round_trips_codex_and_claude_byte_identically() {
    let host = Host::new();
    let codex = host.codex_transcript(UUID, Path::new("/work/invented"));
    let claude = host.claude_transcript(OTHER_UUID);
    let codex_bytes = fs::read(&codex).unwrap();
    let claude_bytes = fs::read(&claude).unwrap();
    let config = enabled(vec![Provider::Codex, Provider::Claude]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 2, "{report:?}");
    let claude_archive = host
        .environment
        .host_dir
        .join("claude/-work-invented-project")
        .join(format!("{OTHER_UUID}.jsonl.zst"));
    assert!(claude_archive.is_file());
    assert!(!claude.exists() && !codex.exists());

    let restored = restore(&host, &config, UUID).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].hash_checked);
    assert_eq!(fs::read(&codex).unwrap(), codex_bytes);
    assert!(!codex_destination(&host, &codex).exists());

    let restored = restore(&host, &config, claude.to_str().unwrap()).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(fs::read(&claude).unwrap(), claude_bytes);
    assert!(!claude_archive.exists());

    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());
    assert_eq!(index.counters().unwrap().get("restored"), Some(&2));
    assert!(restore(&host, &config, UUID).is_err());
}

#[test]
fn restore_checks_the_hash_before_publishing() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    pass(&host, &config, apply(), &mut open, &mut NoHooks);
    let destination = codex_destination(&host, &source);

    let wrong = archive::RestorePlan {
        archive: destination.clone(),
        original: source.clone(),
        sha256: Some("0".repeat(64)),
    };
    let root = Root::open(&host.codex_dir()).unwrap();
    assert!(archive::restore_one(&wrong, &root, &root, None, later()).is_err());
    assert!(!source.exists(), "a mismatched restore publishes nothing");
    assert!(destination.is_file());
}

#[test]
fn an_unindexed_codex_archive_restores_to_its_dated_directory() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let original = fs::read(&source).unwrap();
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    pass(&host, &config, apply(), &mut open, &mut NoHooks);
    // The index is lost; the archive alone must still restore.
    fs::remove_file(host.environment.host_dir.join(super::index::INDEX_FILE)).unwrap();
    let restored = restore(&host, &config, UUID).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(!restored[0].hash_checked);
    assert_eq!(fs::read(&source).unwrap(), original);
}

// ── confinement, privacy and reconciliation ──────────────────────────

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn archives_the_index_and_their_directories_are_private() {
    let host = Host::new();
    let codex = host.codex_transcript(UUID, Path::new("/work/invented"));
    let claude = host.claude_transcript(OTHER_UUID);
    let config = enabled(vec![Provider::Codex, Provider::Claude]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 2, "{report:?}");
    let codex_archive = codex_destination(&host, &codex);
    assert_eq!(mode(&codex_archive), 0o600);
    assert_eq!(mode(codex_archive.parent().unwrap()), 0o700);
    let host_dir = &host.environment.host_dir;
    let claude_archive = host_dir
        .join("claude/-work-invented-project")
        .join(format!("{OTHER_UUID}.jsonl.zst"));
    assert_eq!(mode(&claude_archive), 0o600);
    assert_eq!(mode(claude_archive.parent().unwrap()), 0o700);
    assert_eq!(mode(&host_dir.join("claude")), 0o700);
    assert_eq!(mode(host_dir), 0o700);
    assert_eq!(mode(&host_dir.join(super::index::INDEX_FILE)), 0o600);
    assert_eq!(mode(&host_dir.join(run::RUN_LOG)), 0o600);
    assert_eq!(mode(&host_dir.join(super::LOCK_FILE)), 0o600);
    assert!(!claude.exists());
}

#[test]
fn an_existing_archive_directory_is_narrowed_to_owner_only() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let archived = host.codex_dir().join("archived_sessions");
    fs::create_dir_all(&archived).unwrap();
    fs::set_permissions(&archived, fs::Permissions::from_mode(0o755)).unwrap();
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert_eq!(mode(&archived), 0o700);
    assert!(!source.exists());
}

#[test]
fn run_logs_and_warnings_carry_no_content_or_outside_paths() {
    let host = Host::new();
    let repository = Repository::new(&host, UUID, "closed");
    fs::remove_dir_all(&repository.checkout).unwrap();
    let source = host.codex_transcript(UUID, &repository.root);
    let missing_repository = host.root.join("repository-without-broker");
    fs::create_dir_all(&missing_repository).unwrap();
    let config = TranscriptsConfig {
        repositories: vec![missing_repository.clone()],
        ..enabled(vec![Provider::Codex])
    };
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    let log = fs::read_to_string(host.environment.host_dir.join(run::RUN_LOG)).unwrap();
    let warnings = report.warnings.join("\n");
    for text in [&log, &warnings] {
        assert!(!text.contains("invented transcript line"), "{text}");
        assert!(!text.contains(source.to_str().unwrap()), "{text}");
        assert!(!text.contains(repository.root.to_str().unwrap()), "{text}");
        assert!(
            !text.contains(missing_repository.to_str().unwrap()),
            "{text}"
        );
    }
    assert!(warnings.contains("configured repository"), "{warnings}");
}

#[test]
fn a_symlinked_provider_root_is_refused() {
    let host = Host::new();
    let real = host.root.join("real-codex-home");
    fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, host.codex_dir()).unwrap();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    assert!(real.join("sessions").is_dir());
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("unsafe_path"), Some(&1), "{report:?}");
    assert!(source.is_file());
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.contains("Codex home")),
        "{report:?}"
    );
}

#[test]
fn confined_operations_never_follow_links_or_replace_entries() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let root_path = base.join("root");
    let outside = base.join("outside");
    fs::create_dir_all(root_path.join("real")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.jsonl"), b"outside").unwrap();
    std::os::unix::fs::symlink(&outside, root_path.join("linked")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("secret.jsonl"),
        root_path.join("real/alias.jsonl"),
    )
    .unwrap();
    let root = Root::open(&root_path).unwrap();

    // A symbolic link component is never followed.
    assert!(
        root.parent_of(&root_path.join("linked/secret.jsonl"), false)
            .is_err()
    );
    assert!(
        root.parent_of(&root_path.join("linked/new/x"), true)
            .is_err()
    );
    assert!(!outside.join("new").exists());
    // A symbolic link entry is never treated as a file.
    let (directory, name) = root
        .parent_of(&root_path.join("real/alias.jsonl"), false)
        .unwrap();
    assert!(directory.stamp(&name).is_err());
    assert!(directory.open_read(&name).is_err());
    // Paths outside the root, or with `..`, are refused.
    assert!(
        root.parent_of(&outside.join("secret.jsonl"), false)
            .is_err()
    );
    assert!(
        root.parent_of(&root_path.join("real/../linked/secret.jsonl"), false)
            .is_err()
    );

    // New files are private and never replace an entry.
    let (directory, _) = root.parent_of(&root_path.join("real/x"), false).unwrap();
    let temporary = directory.create_temp(".t-", ".part").unwrap();
    let temporary_path = root_path
        .join("real")
        .join(temporary.name.to_str().unwrap());
    assert_eq!(mode(&temporary_path), 0o600);
    let existing = std::ffi::CString::new("alias.jsonl").unwrap();
    assert_eq!(
        temporary.publish(&existing).unwrap_err().kind(),
        std::io::ErrorKind::AlreadyExists
    );
    assert_eq!(fs::read(outside.join("secret.jsonl")).unwrap(), b"outside");
    assert!(
        !temporary_path.exists(),
        "an unpublished temporary file is removed"
    );

    // The root itself must not be a symbolic link.
    let link_root = base.join("link-root");
    std::os::unix::fs::symlink(&root_path, &link_root).unwrap();
    assert!(Root::open(&link_root).is_err());
}

/// Moves the Codex archive back the way `codex unarchive` does.
fn codex_unarchive(host: &Host, source: &Path) {
    let archive = codex_destination(host, source);
    let back = source.with_file_name(format!(
        "{}.zst",
        source.file_name().unwrap().to_str().unwrap()
    ));
    fs::rename(archive, back).unwrap();
}

#[test]
fn an_archive_moved_away_is_reconciled_and_never_overcounted() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    pass(&host, &config, apply(), &mut open, &mut NoHooks);
    codex_unarchive(&host, &source);

    let status = status(&host, &config, &mut open);
    assert_eq!(status.archived_files, 0, "{status:?}");
    assert_eq!(status.archived_bytes_after, 0);

    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.reconciled_missing, 1, "{report:?}");
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());
    assert!(
        report.archived.is_empty(),
        "an unarchived .jsonl.zst is left to Codex"
    );
}

// ── review fixes: linkage, live Claude sessions, space, durability ───

#[test]
fn an_unreadable_broker_database_skips_unlinked_transcripts() {
    let host = Host::new();
    let repository = Repository::new(&host, OTHER_UUID, "closed");
    // The transcript's cwd finds this database, which cannot be read.
    fs::write(&repository.db, b"not a database at all").unwrap();
    let source = host.codex_transcript(UUID, &repository.root);
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.linkage_degraded, "{report:?}");
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("linkage_unreadable"), Some(&1));
    assert!(source.is_file());
}

#[test]
fn claude_names_and_project_directories() {
    assert!(agents::is_claude("claude --resume x"));
    assert!(agents::is_claude("/opt/bin/claude"));
    assert!(agents::is_claude(
        "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"
    ));
    assert!(!agents::is_claude("codex resume x"));
    assert!(!agents::is_claude("vim claude.md"));
    assert_eq!(
        agents::project_dir_name(Path::new("/work/invented.project_x")),
        "-work-invented-project-x"
    );
    let live = |cwd: &str| ClaudeProcess {
        cwd: Some(PathBuf::from(cwd)),
        args: "claude".into(),
    };
    let project = "-work-invented-project";
    assert!(agents::claude_session_live(
        &[live("/work/invented/project")],
        project,
        None
    ));
    assert!(agents::claude_session_live(
        &[live("/work/invented-project/sub")],
        project,
        None
    ));
    assert!(!agents::claude_session_live(
        &[live("/work/other")],
        project,
        None
    ));
    let unknown_cwd = ClaudeProcess {
        cwd: None,
        args: "claude".into(),
    };
    assert!(agents::claude_session_live(&[unknown_cwd], project, None));
    let named = ClaudeProcess {
        cwd: Some(PathBuf::from("/elsewhere")),
        args: format!("claude --resume {OTHER_UUID}"),
    };
    assert!(agents::claude_session_live(
        &[named],
        project,
        Some(OTHER_UUID)
    ));
}

fn claude_enabled() -> TranscriptsConfig {
    TranscriptsConfig {
        archive_after_inactive_days_claude: 3,
        ..enabled(vec![Provider::Claude])
    }
}

#[test]
fn a_live_or_unverifiable_claude_session_is_never_archived() {
    let host = Host::new();
    let source = host.claude_transcript(OTHER_UUID);
    let config = claude_enabled();
    let mut open = probe(Vec::new());

    let mut running = FakeAgents {
        processes: Some(vec![ClaudeProcess {
            cwd: Some(PathBuf::from("/work/invented-project")),
            args: "claude".into(),
        }]),
        codex: Some(Vec::new()),
        calls: 0,
    };
    let report = pass_at(
        &host,
        &config,
        apply(),
        &mut open,
        &mut running,
        &mut NoHooks,
        later(),
    );
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("agent_running"), Some(&1));

    let mut unavailable = FakeAgents::default();
    let report = pass_at(
        &host,
        &config,
        apply(),
        &mut open,
        &mut unavailable,
        &mut NoHooks,
        later(),
    );
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("process_check_unavailable"), Some(&1));
    assert!(source.is_file());

    let mut elsewhere = FakeAgents {
        processes: Some(vec![ClaudeProcess {
            cwd: Some(PathBuf::from("/work/unrelated")),
            args: "claude".into(),
        }]),
        codex: Some(Vec::new()),
        calls: 0,
    };
    let report = pass_at(
        &host,
        &config,
        apply(),
        &mut open,
        &mut elsewhere,
        &mut NoHooks,
        later(),
    );
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(
        elsewhere.calls >= 2,
        "checked at judging and again before archiving"
    );
}

#[test]
fn claude_transcripts_wait_fourteen_days_by_default() {
    let host = Host::new();
    let claude = host.claude_transcript(OTHER_UUID);
    let codex = host.codex_transcript(UUID, Path::new("/work/invented"));
    assert_eq!(
        TranscriptsConfig::default().archive_after_inactive_days_claude,
        14
    );
    let config = TranscriptsConfig {
        archive_after_inactive_days_claude: config::DEFAULT_ARCHIVE_AFTER_INACTIVE_DAYS_CLAUDE,
        ..enabled(vec![Provider::Codex, Provider::Claude])
    };
    let mut open = probe(Vec::new());
    // `later()` is ten days on.
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(!codex.exists());
    assert!(claude.is_file());
    assert_eq!(report.skipped.get("too_recent"), Some(&1));
    assert!(
        TranscriptsConfig::parse("[transcripts]\narchive_after_days_claude = 30\n")
            .unwrap()
            .archive_after_inactive_days_claude
            == 30
    );
}

#[test]
fn restore_merges_a_newer_fragment_after_the_archived_content() {
    let host = Host::new();
    let source = host.claude_transcript(OTHER_UUID);
    let archived = fs::read(&source).unwrap();
    let config = claude_enabled();
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    // The idle session wrote again: a fragment at the same path.
    let fragment = b"{\"type\":\"user\",\"n\":\"after the archive\"}\n";
    fs::write(&source, fragment).unwrap();

    let restored = restore(&host, &config, OTHER_UUID).unwrap();
    assert_eq!(restored[0].merged_fragment_bytes, fragment.len() as u64);
    let mut expected = archived.clone();
    expected.extend_from_slice(fragment);
    assert_eq!(fs::read(&source).unwrap(), expected);
    assert!(!report.archived[0].destination.exists());
    let leftovers: Vec<String> = fs::read_dir(source.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(providers::TEMP_PREFIX))
        .collect();
    assert!(
        leftovers.is_empty(),
        "the fragment backup is removed: {leftovers:?}"
    );
}

#[test]
fn a_destination_conflict_is_recorded_once_then_backed_off() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let destination = codex_destination(&host, &source);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    fs::write(&destination, b"not this transcript").unwrap();
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.skipped.get("destination_conflict"), Some(&1));
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.skipped.get("destination_conflict"), None);
    assert_eq!(report.skipped.get("destination_conflict_backoff"), Some(&1));
    assert!(report.skipped_examples.is_empty(), "{report:?}");
    // Due again after the backoff.
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(!index.conflict_due(&source, later()).unwrap());
    assert!(index.conflict_due(&source, later() + 2 * DAY_MS).unwrap());
    assert!(source.is_file());
}

/// Reports `path` open from the `open_from`-th listing on.
struct OpensLater {
    path: PathBuf,
    open_from: u64,
    calls: u64,
}

impl OpenFileProbe for OpensLater {
    fn observe(&mut self) -> Result<Vec<OpenFile>, String> {
        self.calls += 1;
        Ok(if self.calls >= self.open_from {
            vec![OpenFile {
                pid: 77,
                path: self.path.clone(),
            }]
        } else {
            Vec::new()
        })
    }
}

#[test]
fn a_transcript_opened_after_the_shared_observation_is_kept() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    // The shared observation says "closed" and would serve for an hour; the
    // batch observation, taken after the copy was verified, sees the
    // process that opened the transcript meanwhile.
    let mut open = OpenFiles::new(
        OpensLater {
            path: source.clone(),
            open_from: 2,
            calls: 0,
        },
        Duration::from_secs(3600),
    );
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("open_by_process"), Some(&1));
    assert_eq!(report.removals_refused_open, 1, "{report:?}");
    assert_eq!(report.removal_batches, 1);
    assert_eq!(report.open_listings, 2, "{report:?}");
    assert!(source.is_file());
    assert!(
        !codex_destination(&host, &source).exists(),
        "the copy is discarded"
    );
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());
}

/// Complete for the first observation, incomplete from then on.
struct IncompleteAfterFirst {
    calls: u64,
}

impl OpenFileProbe for IncompleteAfterFirst {
    fn observe(&mut self) -> Result<Vec<OpenFile>, String> {
        self.calls += 1;
        if self.calls == 1 {
            Ok(Vec::new())
        } else {
            Err("listing incomplete (test)".into())
        }
    }
}

#[test]
fn an_incomplete_batch_observation_removes_nothing() {
    let host = Host::new();
    let sources = codex_transcripts(&host, 3);
    let config = enabled(vec![Provider::Codex]);
    let mut open = OpenFiles::new(IncompleteAfterFirst { calls: 0 }, Duration::from_secs(3600));
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.removals_refused_unobserved, 3, "{report:?}");
    assert_eq!(report.skipped.get("open_check_unavailable"), Some(&3));
    for source in &sources {
        assert!(source.is_file());
        assert!(
            !codex_destination(&host, source).exists(),
            "every copy of the batch is discarded"
        );
    }
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());
}

/// Makes each archive take longer than the pass budget.
struct SlowPublish;

impl Hooks for SlowPublish {
    fn after_publish(&mut self, _source: &Path) -> bool {
        std::thread::sleep(Duration::from_millis(300));
        true
    }
}

#[test]
fn an_exhausted_budget_still_removes_the_published_batch() {
    let host = Host::new();
    let sources = codex_transcripts(&host, 3);
    let config = enabled(vec![Provider::Codex]);
    let mut open = OpenFiles::new(FakeProbe::default(), Duration::from_secs(3600));
    let options = Options {
        apply: true,
        budget: Some(Duration::from_millis(200)),
    };
    let report = pass(&host, &config, options, &mut open, &mut SlowPublish);
    assert!(report.budget_exhausted, "{report:?}");
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert_eq!(report.removal_batches, 1);
    let removed: Vec<_> = sources.iter().filter(|source| !source.exists()).collect();
    assert_eq!(removed.len(), 1, "the published copy's original is removed");
    assert!(codex_destination(&host, removed[0]).is_file());
    for source in sources.iter().filter(|source| source.exists()) {
        assert!(
            !codex_destination(&host, source).exists(),
            "nothing else was published"
        );
    }
}

#[test]
fn a_crash_leftover_whose_source_changed_loses_its_stale_copy() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let destination = codex_destination(&host, &source);
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());

    let report = pass(&host, &config, apply(), &mut open, &mut CrashAfterPublish);
    assert!(report.archived.is_empty());
    assert!(source.is_file() && destination.is_file(), "both remain");

    // The session resumed and appended before the next pass.
    let mut file = fs::OpenOptions::new().append(true).open(&source).unwrap();
    file.write_all(b"{\"resumed\":true}\n").unwrap();
    drop(file);
    let grown = fs::read(&source).unwrap();

    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("modified_during_compression"), Some(&1));
    assert!(!destination.exists(), "the stale copy is discarded");
    assert_eq!(fs::read(&source).unwrap(), grown);
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());

    // The next pass archives the current content.
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(!source.exists());
    assert_eq!(decompress(&destination), grown);
}

/// `count` Codex transcripts with distinct session ids.
fn codex_transcripts(host: &Host, count: usize) -> Vec<PathBuf> {
    (0..count)
        .map(|n| {
            let uuid = format!("0b9f6c1e-2d3a-4b5c-8d7e-1f2a3b4c5d{n:02x}");
            host.codex_transcript(&uuid, Path::new("/work/invented"))
        })
        .collect()
}

#[test]
fn one_observation_serves_every_transcript_while_it_is_young() {
    let host = Host::new();
    let sources = codex_transcripts(&host, 5);
    let config = enabled(vec![Provider::Codex]);
    let mut open = OpenFiles::new(FakeProbe::default(), Duration::from_secs(3600));
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 5, "{report:?}");
    assert_eq!(
        report.open_listings, 2,
        "one shared observation for judging and one for the removal batch"
    );
    assert_eq!(report.open_observation_refreshes, 0);
    assert_eq!(report.removal_batches, 1);
    // Judged once when eligibility is decided and once when archiving
    // starts against the shared observation, then once each at removal.
    assert_eq!(report.open_files_judged, 15, "{report:?}");
    assert_eq!(report.open_files_judged_max_per_observation, 10);
    assert_eq!(report.removals_refused_changed, 0);
    for source in &sources {
        assert!(!source.exists());
        assert!(codex_destination(&host, source).is_file());
    }
}

#[test]
fn an_incomplete_observation_archives_nothing() {
    let host = Host::new();
    let sources = codex_transcripts(&host, 3);
    let config = enabled(vec![Provider::Codex]);
    let mut open = OpenFiles::new(
        FakeProbe {
            open: Vec::new(),
            incomplete: true,
        },
        Duration::from_secs(3600),
    );
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.archived.is_empty(), "{report:?}");
    assert!(report.eligible.is_empty(), "{report:?}");
    assert_eq!(report.skipped.get("open_check_unavailable"), Some(&3));
    assert_eq!(report.open_listings, 1);
    for source in &sources {
        assert!(source.is_file());
        assert!(!codex_destination(&host, source).exists());
    }
}

/// Appends to the source after its copy is published: a writer that opened
/// it after the open-file observation.
struct WriteAfterPublish;

impl Hooks for WriteAfterPublish {
    fn after_publish(&mut self, source: &Path) -> bool {
        let mut file = fs::OpenOptions::new().append(true).open(source).unwrap();
        file.write_all(b"{\"late\":true}\n").unwrap();
        true
    }
}

#[test]
fn a_transcript_written_between_observation_and_unlink_is_kept() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = OpenFiles::new(FakeProbe::default(), Duration::from_secs(3600));
    let report = pass(&host, &config, apply(), &mut open, &mut WriteAfterPublish);
    assert!(report.archived.is_empty(), "{report:?}");
    assert_eq!(report.removals_refused_changed, 1, "{report:?}");
    assert_eq!(report.skipped.get("modified_during_compression"), Some(&1));
    assert_eq!(report.removal_batches, 1);
    assert!(
        fs::read_to_string(&source)
            .unwrap()
            .ends_with("{\"late\":true}\n")
    );
    assert!(
        !codex_destination(&host, &source).exists(),
        "the copy is discarded"
    );
    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());
    assert_eq!(
        index.counters().unwrap().get("removals_refused_changed"),
        Some(&1)
    );
}

#[test]
fn below_the_free_space_floor_only_what_fits_is_archived_unpaced() {
    assert!(pacing::fits(256 * 1024 * 1024 + 100, 300));
    assert!(!pacing::fits(256 * 1024 * 1024, 3 * 1024 * 1024));

    let mut host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());

    host.environment.free_space_override = Some(100 * 1024 * 1024);
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(report.archived.is_empty(), "{report:?}");
    assert!(report.insufficient_space);
    assert_eq!(report.skipped.get("insufficient_space"), Some(&1));
    assert!(
        report
            .warnings
            .iter()
            .any(|warning| warning.starts_with("insufficient_space")),
        "{report:?}"
    );
    assert!(source.is_file());

    host.environment.free_space_override = Some(300 * 1024 * 1024);
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert_eq!(
        report.paced_bytes_per_second, None,
        "unpaced below the floor"
    );
}

#[test]
fn full_sync_flushes_a_file_and_a_directory() {
    use std::os::fd::AsRawFd as _;
    let temp = tempfile::tempdir().unwrap();
    let file = fs::File::create(temp.path().join("f")).unwrap();
    super::safe_fs::full_sync(file.as_raw_fd()).unwrap();
    let directory = fs::File::open(temp.path()).unwrap();
    super::safe_fs::full_sync(directory.as_raw_fd()).unwrap();
}

#[test]
fn disabled_reads_no_provider_root_and_no_state() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    // An index from an earlier, enabled time.
    let enabled_config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    pass(&host, &enabled_config, dry(), &mut open, &mut NoHooks);
    fs::set_permissions(host.codex_dir(), fs::Permissions::from_mode(0o000)).unwrap();
    let disabled = TranscriptsConfig {
        archive: false,
        ..enabled_config
    };
    let mut open = probe(Vec::new());
    let mut agents = FakeAgents::none();
    let report = pass_at(
        &host,
        &disabled,
        apply(),
        &mut open,
        &mut agents,
        &mut NoHooks,
        later(),
    );
    let status = status(&host, &disabled, &mut open);
    let rows = run::archived_rows(&host.environment, &disabled).unwrap();
    fs::set_permissions(host.codex_dir(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(report.examined, 0);
    assert!(report.warnings.is_empty(), "{report:?}");
    assert_eq!(open.probes, 0);
    assert_eq!(agents.calls, 0);
    assert!(status.last_run.is_none() && status.counters.is_empty());
    assert!(rows.is_empty());
    assert!(source.is_file());
}

#[test]
fn the_run_log_keeps_session_ids_and_reasons_but_no_paths() {
    let host = Host::new();
    let source = host.claude_transcript(OTHER_UUID);
    let config = claude_enabled();
    let mut open = probe(Vec::new());
    let mut running = FakeAgents {
        processes: Some(vec![ClaudeProcess {
            cwd: Some(PathBuf::from("/work/invented-project")),
            args: "claude".into(),
        }]),
        codex: Some(Vec::new()),
        calls: 0,
    };
    pass_at(
        &host,
        &config,
        apply(),
        &mut open,
        &mut running,
        &mut NoHooks,
        later(),
    );
    let log = fs::read_to_string(host.environment.host_dir.join(run::RUN_LOG)).unwrap();
    assert!(log.contains(OTHER_UUID), "{log}");
    assert!(log.contains("agent_running"), "{log}");
    assert!(!log.contains("-work-invented-project"), "{log}");
    assert!(!log.contains(source.to_str().unwrap()), "{log}");
}

// ── second review: restore idempotence, live writers, names, health ──

#[test]
fn an_interrupted_merge_is_finished_without_duplicating_the_archive() {
    let host = Host::new();
    let source = host.claude_transcript(OTHER_UUID);
    let archived = fs::read(&source).unwrap();
    let config = claude_enabled();
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    // The state a crash after publishing leaves: the merged file, the
    // archive, its row and the restore intent.
    let fragment = b"{\"n\":\"newer\"}\n";
    let mut merged = archived.clone();
    merged.extend_from_slice(fragment);
    fs::write(&source, &merged).unwrap();
    let index = Index::open(&host.environment.host_dir).unwrap();
    index
        .record_restore_intent(&source, &report.archived[0].destination, later())
        .unwrap();
    assert_eq!(
        status(&host, &config, &mut open)
            .durability
            .restore_intents_pending,
        1
    );

    let restored = restore(&host, &config, OTHER_UUID).unwrap();
    assert!(restored[0].already_restored);
    assert_eq!(fs::read(&source).unwrap(), merged, "nothing merged twice");
    assert!(!report.archived[0].destination.exists());
    assert!(index.archives().unwrap().is_empty());
    assert_eq!(index.pending_restore_intents().unwrap(), 0);
}

#[test]
fn a_merge_is_refused_while_its_session_may_be_writing() {
    let host = Host::new();
    let claude = host.claude_transcript(OTHER_UUID);
    let codex = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex, Provider::Claude]);
    let mut open = probe(Vec::new());
    let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert_eq!(report.archived.len(), 2, "{report:?}");
    fs::write(&claude, b"{\"n\":1}\n").unwrap();
    fs::write(&codex, b"{\"n\":1}\n").unwrap();

    let mut running = FakeAgents {
        processes: Some(vec![ClaudeProcess {
            cwd: Some(PathBuf::from("/work/invented-project")),
            args: "claude".into(),
        }]),
        codex: Some(Vec::new()),
        calls: 0,
    };
    let error = restore_with(
        &host,
        &config,
        OTHER_UUID,
        &mut probe(Vec::new()),
        &mut running,
    )
    .unwrap_err();
    assert!(error.contains("running"), "{error}");
    let error = restore_with(
        &host,
        &config,
        OTHER_UUID,
        &mut probe(Vec::new()),
        &mut FakeAgents::default(),
    )
    .unwrap_err();
    assert!(error.contains("cannot be ruled out"), "{error}");

    let error = restore_with(
        &host,
        &config,
        UUID,
        &mut probe(vec![codex.clone()]),
        &mut FakeAgents::none(),
    )
    .unwrap_err();
    assert!(error.contains("open"), "{error}");
    let mut naming = FakeAgents {
        codex: Some(vec![format!("codex resume {UUID}")]),
        ..FakeAgents::none()
    };
    let error =
        restore_with(&host, &config, UUID, &mut probe(Vec::new()), &mut naming).unwrap_err();
    assert!(error.contains("Codex process"), "{error}");
    // Nothing was changed by any refusal.
    assert_eq!(fs::read(&claude).unwrap(), b"{\"n\":1}\n");
    assert_eq!(fs::read(&codex).unwrap(), b"{\"n\":1}\n");
}

#[test]
fn bytes_written_to_the_old_fragment_after_the_merge_are_carried_over() {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    let root = Root::open(&base).unwrap();
    let original = base.join("t.jsonl");
    fs::write(&original, b"fragment\n").unwrap();
    let (directory, name) = root.parent_of(&original, false).unwrap();
    let before = directory.stamp(&name).unwrap();
    let backup = directory
        .link_unique(&name, providers::TEMP_PREFIX, ".fragment")
        .unwrap();
    let backup_path = base.join(backup.to_str().unwrap());
    // Publication replaces the original with the merged file...
    let merged = base.join("merged");
    fs::write(&merged, b"archived\nfragment\n").unwrap();
    fs::rename(&merged, &original).unwrap();
    // ...while a writer still appends to the old fragment.
    let mut writer = fs::OpenOptions::new()
        .append(true)
        .open(&backup_path)
        .unwrap();
    writer.write_all(b"late\n").unwrap();
    drop(writer);
    let kept = archive::reconcile_backup(&directory, &name, &backup, before, true);
    assert_eq!(kept, None);
    assert_eq!(fs::read(&original).unwrap(), b"archived\nfragment\nlate\n");
    assert!(!backup_path.exists());

    // A backup that shrank cannot be reconciled and is kept.
    fs::write(&original, b"fragment two\n").unwrap();
    let before = directory.stamp(&name).unwrap();
    let backup = directory
        .link_unique(&name, providers::TEMP_PREFIX, ".fragment")
        .unwrap();
    fs::write(base.join("merged"), b"x").unwrap();
    fs::rename(base.join("merged"), &original).unwrap();
    fs::write(base.join(backup.to_str().unwrap()), b"short").unwrap();
    let kept = archive::reconcile_backup(&directory, &name, &backup, before, true);
    assert_eq!(kept, Some(backup.clone()));
    assert!(base.join(backup.to_str().unwrap()).exists());
}

#[test]
fn project_names_map_utf16_units_and_tolerate_truncation() {
    assert_eq!(agents::project_dir_name(Path::new("/a/\u{e9}")), "-a--");
    assert_eq!(agents::project_dir_name(Path::new("/a/\u{1F600}")), "-a---");
    let process = |cwd: &str| ClaudeProcess {
        cwd: Some(PathBuf::from(cwd)),
        args: "claude".into(),
    };
    let long_cwd = format!("/work/{}", "x".repeat(200));
    let full = agents::project_dir_name(Path::new(&long_cwd));
    // Cut short, and cut short with a suffix added after the cut.
    let truncated = full[..150].to_string();
    let suffixed = format!("{}-1a2b3c", &full[..140]);
    assert!(agents::claude_session_live(
        &[process(&long_cwd)],
        &truncated,
        None
    ));
    assert!(agents::claude_session_live(
        &[process(&long_cwd)],
        &suffixed,
        None
    ));
    assert!(!agents::claude_session_live(
        &[process("/work/short")],
        &truncated,
        None
    ));
}

#[test]
fn status_shows_linkage_degradation_and_broker_status_advises_after_three_passes() {
    let host = Host::new();
    let repository = Repository::new(&host, OTHER_UUID, "closed");
    fs::write(&repository.db, b"not a database at all").unwrap();
    let source = host.codex_transcript(UUID, &repository.root);
    // Old enough for `status`, which judges at the real current time.
    fs::File::options()
        .write(true)
        .open(&source)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(10 * 86_400))
        .unwrap();
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let index = || Index::open(&host.environment.host_dir).unwrap();
    for pass_number in 1..=3 {
        let report = pass(&host, &config, apply(), &mut open, &mut NoHooks);
        assert_eq!(report.linkage_issues.unreadable_databases, 1);
        let advice = run::linkage_advice_from(&index());
        assert_eq!(advice.is_some(), pass_number >= 3, "pass {pass_number}");
    }
    let advice = run::linkage_advice_from(&index()).unwrap();
    assert_eq!(advice.id, "transcripts.linkage-degraded");
    assert!(
        advice.summary.contains("1 unreadable broker database"),
        "{}",
        advice.summary
    );
    assert!(!advice.summary.contains(repository.root.to_str().unwrap()));

    let shown = status(&host, &config, &mut open);
    assert!(shown.linkage.degraded_now);
    assert_eq!(shown.linkage.issues_now.unreadable_databases, 1);
    assert_eq!(shown.linkage.consecutive_degraded_passes, 3);
    index()
        .add_counters(
            &[("skipped_sync_failed".to_string(), 2)]
                .into_iter()
                .collect(),
        )
        .unwrap();
    assert_eq!(
        status(&host, &config, &mut open)
            .durability
            .sources_kept_sync_failed,
        2
    );

    // A pass that can read everything resets the streak.
    fs::remove_dir_all(&repository.root).unwrap();
    pass(&host, &config, apply(), &mut open, &mut NoHooks);
    assert!(run::linkage_advice_from(&index()).is_none());
}

// ── on by default, inactivity, first-run notice ──────────────────────

/// Set a file's modification time to `ms` since the epoch.
fn set_modified_ms(path: &Path, ms: i64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + Duration::from_millis(ms as u64))
        .unwrap();
}

fn worker(host: &Host, config: &TranscriptsConfig, now_ms: i64) -> run::RunReport {
    let mut open = probe(Vec::new());
    let mut agents = FakeAgents::none();
    let mut probes = Probes {
        open_files: &mut open,
        agents: &mut agents,
    };
    run::worker_pass(&host.environment, config, &mut probes, &mut NoHooks, now_ms).unwrap()
}

#[test]
fn archiving_is_on_by_default_without_a_settings_file() {
    let host = Host::new();
    let config = TranscriptsConfig::load(&host.environment.host_dir).unwrap();
    assert!(config.archive);
    assert_eq!(config.enabled_source, config::EnabledSource::Default);
    assert_eq!(config.providers, vec![Provider::Codex, Provider::Claude]);
    assert_eq!(config.archive_after_inactive_days, 3);
    assert_eq!(config.archive_after_inactive_days_claude, 14);
    // A file that omits `archive` is still the default.
    let omitted = TranscriptsConfig::parse("[transcripts]\nbytes_per_second = 1048576\n").unwrap();
    assert!(omitted.archive);
    assert_eq!(omitted.enabled_source, config::EnabledSource::Default);

    let mut open = probe(Vec::new());
    let shown = serde_json::to_value(status(&host, &config, &mut open)).unwrap();
    assert_eq!(shown["enabled"], true);
    assert_eq!(shown["enabled_source"], "default");
    assert_eq!(shown["config"]["enabled_source"], "default");
    assert_eq!(shown["first_run"]["gate"], "notice_pending");
}

#[test]
fn an_explicit_archive_false_is_respected() {
    let host = Host::new();
    let host_dir = &host.environment.host_dir;
    fs::create_dir_all(host_dir).unwrap();
    fs::write(
        host_dir.join(config::SETTINGS_FILE),
        "[transcripts]\narchive = false\n",
    )
    .unwrap();
    let config = TranscriptsConfig::load(host_dir).unwrap();
    assert!(!config.archive);
    assert_eq!(config.enabled_source, config::EnabledSource::Settings);
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let report = worker(&host, &config, later());
    assert_eq!(report.examined, 0);
    assert!(source.is_file());
    let mut open = probe(Vec::new());
    let shown = serde_json::to_value(status(&host, &config, &mut open)).unwrap();
    assert_eq!(shown["enabled"], false);
    assert_eq!(shown["enabled_source"], "settings");
    // `disable` on a host with no file writes the same explicit setting.
    let other = Host::new();
    let disabled = config::write_settings(
        &other.environment.host_dir,
        &SettingsChange {
            archive: false,
            ..SettingsChange::default()
        },
    )
    .unwrap();
    assert!(!disabled.archive);
    assert_eq!(disabled.enabled_source, config::EnabledSource::Settings);
}

#[test]
fn old_setting_names_are_still_honoured_and_new_names_win() {
    let old = TranscriptsConfig::parse(
        "[transcripts]\narchive_after_days = 5\narchive_after_days_claude = 20\n",
    )
    .unwrap();
    assert_eq!(old.archive_after_inactive_days, 5);
    assert_eq!(old.archive_after_inactive_days_claude, 20);
    for text in [
        "[transcripts]\narchive_after_days = 5\narchive_after_inactive_days = 7\n\
         archive_after_days_claude = 20\narchive_after_inactive_days_claude = 21\n",
        "[transcripts]\narchive_after_inactive_days = 7\narchive_after_days = 5\n\
         archive_after_inactive_days_claude = 21\narchive_after_days_claude = 20\n",
    ] {
        let config = TranscriptsConfig::parse(text).unwrap();
        assert_eq!(config.archive_after_inactive_days, 7, "{text}");
        assert_eq!(config.archive_after_inactive_days_claude, 21, "{text}");
    }
    assert!(TranscriptsConfig::parse("[transcripts]\narchive_after_inactive_days = 0\n").is_err());

    // `enable --after-days` and `--after-inactive-days` write the new name
    // and drop the old one.
    let host = Host::new();
    let host_dir = &host.environment.host_dir;
    fs::create_dir_all(host_dir).unwrap();
    fs::write(
        host_dir.join(config::SETTINGS_FILE),
        "[transcripts]\narchive_after_days = 5\n",
    )
    .unwrap();
    for flag in ["--after-days", "--after-inactive-days"] {
        let change =
            super::cli::parse_settings_flags(&[flag.to_string(), "9".to_string()], true).unwrap();
        assert_eq!(change.archive_after_inactive_days, Some(9), "{flag}");
        let config = config::write_settings(host_dir, &change).unwrap();
        assert_eq!(config.archive_after_inactive_days, 9);
    }
    let text = fs::read_to_string(host_dir.join(config::SETTINGS_FILE)).unwrap();
    assert!(text.contains("archive_after_inactive_days = 9"), "{text}");
    assert!(!text.contains("archive_after_days ="), "{text}");
}

/// Name, modification time, last record time, linkage, expected verdict.
type InactivityCase = (
    &'static str,
    Option<i64>,
    Option<i64>,
    Linkage,
    Result<(), SkipReason>,
);

#[test]
fn inactivity_is_the_newest_of_mtime_last_record_and_linked_session() {
    let now = 100 * DAY_MS;
    let old = Some(now - 10 * DAY_MS);
    let recent = Some(now - DAY_MS);
    let closed = |activity: Option<i64>| {
        Linkage::Linked(vec![LinkedSession {
            cleanup_state: Some("closed".into()),
            checkout: CheckoutState::Absent,
            last_activity_ms: activity,
        }])
    };
    let cases: Vec<InactivityCase> = vec![
        ("all three old", old, old, closed(old), Ok(())),
        (
            "recent mtime",
            recent,
            old,
            closed(old),
            Err(SkipReason::TooRecent),
        ),
        (
            "recent last record",
            old,
            recent,
            closed(old),
            Err(SkipReason::RecentRecord),
        ),
        (
            "recent linked session",
            old,
            old,
            closed(recent),
            Err(SkipReason::RecentSessionActivity),
        ),
        ("no record, unlinked", old, None, Linkage::Unlinked, Ok(())),
        (
            "a record in the future counts as recent",
            old,
            Some(now + DAY_MS),
            Linkage::Unlinked,
            Err(SkipReason::RecentRecord),
        ),
    ];
    for (name, modified_ms, record_ms, linkage, expected) in cases {
        let facts = Facts {
            now_ms: now,
            modified_ms,
            record_ms,
            inactive_days: 3,
            linkage: &linkage,
        };
        assert_eq!(
            eligibility::judge(&facts, OpenState::Closed),
            expected,
            "{name}"
        );
    }

    // The last record comes from the tail only, ignoring a partial first
    // line and records without a timestamp.
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("tail.jsonl");
    let mut text = String::from("{\"timestamp\":\"2026-12-31T00:00:00Z\"}\n");
    while text.len() < providers::RECORD_TAIL_BYTES as usize + 1024 {
        text.push_str("{\"type\":\"filler\",\"text\":\"an invented line\"}\n");
    }
    text.push_str("{\"timestamp\":\"2026-03-01T00:00:00.250Z\",\"type\":\"a\"}\n");
    text.push_str("{\"timestamp\":\"2026-03-02T00:00:00Z\",\"type\":\"b\"}\n");
    text.push_str("{\"type\":\"no timestamp\"}\n");
    fs::write(&path, &text).unwrap();
    assert_eq!(
        providers::last_record_ms(&path),
        crate::pr_watch::parse_iso8601_utc_ms("2026-03-02T00:00:00Z"),
        "the head's later timestamp is outside the tail"
    );
    fs::write(&path, "{\"type\":\"no timestamp\"}\n").unwrap();
    assert_eq!(providers::last_record_ms(&path), None);
}

#[test]
fn a_recent_last_record_or_session_keeps_an_old_transcript() {
    let host = Host::new();
    let repository = Repository::new(&host, UUID, "closed");
    fs::remove_dir_all(&repository.checkout).unwrap();
    let source = host.codex_transcript(UUID, &repository.root);
    let now = crate::pr_watch::parse_iso8601_utc_ms("2026-03-10T00:00:00Z").unwrap();
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    let run_at = |open: &mut OpenFiles<FakeProbe>| {
        pass_at(
            &host,
            &config,
            apply(),
            open,
            &mut FakeAgents::none(),
            &mut NoHooks,
            now,
        )
    };

    // Modified long ago, but its last record is a day old.
    fs::OpenOptions::new()
        .append(true)
        .open(&source)
        .unwrap()
        .write_all(b"{\"timestamp\":\"2026-03-09T00:00:00Z\",\"type\":\"event_msg\"}\n")
        .unwrap();
    set_modified_ms(&source, now - 10 * DAY_MS);
    let report = run_at(&mut open);
    assert_eq!(report.skipped.get("recent_record"), Some(&1), "{report:?}");
    assert_eq!(report.record_tails_read, 1);
    assert!(source.is_file());

    // An old last record, but the linked broker session was active a day
    // ago.
    let text = fs::read_to_string(&source)
        .unwrap()
        .replace("2026-03-09T00:00:00Z", "2026-02-01T00:00:00Z");
    fs::write(&source, text).unwrap();
    set_modified_ms(&source, now - 10 * DAY_MS);
    let conn = rusqlite::Connection::open(&repository.db).unwrap();
    conn.execute(
        "UPDATE sessions SET last_activity_at = ?1 WHERE id = 7",
        [now - DAY_MS],
    )
    .unwrap();
    let report = run_at(&mut open);
    assert_eq!(
        report.skipped.get("recent_session_activity"),
        Some(&1),
        "{report:?}"
    );
    assert!(source.is_file());

    // Everything older than three days: archived.
    conn.execute(
        "UPDATE sessions SET last_activity_at = ?1 WHERE id = 7",
        [now - 10 * DAY_MS],
    )
    .unwrap();
    let report = run_at(&mut open);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(!source.exists());
}

#[test]
fn the_first_run_notice_holds_the_worker_for_24_hours() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    assert_eq!(config.enabled_source, config::EnabledSource::Default);
    let host_dir = &host.environment.host_dir;
    let hour = 60 * 60 * 1000;
    let t0 = later();

    // Never shown: the worker archives nothing.
    let report = worker(&host, &config, t0);
    assert_eq!(report.first_run_gate, super::Gate::NoticePending);
    assert_eq!(report.examined, 0);
    assert!(source.is_file());

    // `broker status` shows the notice once and records when.
    let advice = super::first_run::notice_advice_at(host_dir, &config, t0).unwrap();
    assert_eq!(advice.id, "transcripts.archiving-default-on");
    assert!(
        advice.summary.contains("3 inactive days"),
        "{}",
        advice.summary
    );
    assert!(advice.summary.contains("aethyme transcripts disable"));
    assert!(!advice.summary.contains('\n'));
    assert_eq!(
        super::first_run::read(host_dir).noticed_at_ms,
        Some(t0),
        "the notice time is recorded"
    );
    // Showing it again does not restart the wait.
    super::first_run::notice_advice_at(host_dir, &config, t0 + hour).unwrap();
    assert_eq!(super::first_run::read(host_dir).noticed_at_ms, Some(t0));

    let report = worker(&host, &config, t0 + 23 * hour);
    assert_eq!(report.first_run_gate, super::Gate::Waiting);
    assert_eq!(report.examined, 0);
    assert!(source.is_file());

    let report = worker(&host, &config, t0 + 25 * hour);
    assert_eq!(report.first_run_gate, super::Gate::Open);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(!source.exists());
    assert!(super::first_run::notice_advice_at(host_dir, &config, t0 + 25 * hour).is_none());
}

#[test]
fn enable_or_a_manual_apply_skips_the_first_run_wait() {
    // Explicitly enabled: no notice, no wait.
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let explicit = TranscriptsConfig {
        enabled_source: config::EnabledSource::Settings,
        ..enabled(vec![Provider::Codex])
    };
    assert!(
        super::first_run::notice_advice_at(&host.environment.host_dir, &explicit, later())
            .is_none()
    );
    let report = worker(&host, &explicit, later());
    assert_eq!(report.first_run_gate, super::Gate::Open);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(!source.exists());

    // On by default, then archived by hand: the worker no longer waits.
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let default_on = enabled(vec![Provider::Codex]);
    assert_eq!(
        run::acknowledge_manual_apply(&host.environment, later()),
        None
    );
    let state = super::first_run::read(&host.environment.host_dir);
    assert_eq!(state.acknowledged_by.as_deref(), Some("archive_apply"));
    assert!(
        super::first_run::notice_advice_at(&host.environment.host_dir, &default_on, later())
            .is_none()
    );
    let report = worker(&host, &default_on, later());
    assert_eq!(report.first_run_gate, super::Gate::Open);
    assert_eq!(report.archived.len(), 1, "{report:?}");
    assert!(!source.exists());
}

#[test]
fn transcripts_status_shows_the_first_run_wait_in_text_and_json() {
    let host = Host::new();
    let config = TranscriptsConfig::default();
    let host_dir = &host.environment.host_dir;
    let mut open = probe(Vec::new());

    // Not shown yet: status says so, and reading does not start the wait.
    let pending = status(&host, &config, &mut open);
    assert_eq!(pending.first_run.gate, super::Gate::NoticePending);
    assert!(super::first_run::read(host_dir).noticed_at_ms.is_none());
    let lines = super::first_run::status_lines(&pending.first_run).join("\n");
    assert!(lines.contains("notice_pending"), "{lines}");
    assert!(
        lines.contains("run: aethyme transcripts disable"),
        "{lines}"
    );

    // Shown by `broker status`: the start time, the notice and the commands.
    let noticed = super::now_ms();
    super::first_run::notice_advice_at(host_dir, &config, noticed).unwrap();
    let starts = noticed + super::first_run::NOTICE_WAIT_MS;
    let starts_text = crate::cli::insights::timestamp_millis(starts);
    let waiting = status(&host, &config, &mut open);
    let value = serde_json::to_value(&waiting).unwrap();
    assert_eq!(value["first_run"]["gate"], "waiting");
    assert_eq!(value["first_run"]["noticed_at_ms"], noticed);
    assert_eq!(value["first_run"]["automatic_archiving_from_ms"], starts);
    assert_eq!(
        value["first_run"]["automatic_archiving_from"],
        starts_text.as_str()
    );
    let notice = value["first_run"]["notice"].as_str().unwrap();
    assert!(notice.contains(&starts_text), "{notice}");
    assert!(notice.contains("--after-inactive-days"), "{notice}");
    assert!(notice.contains("aethyme transcripts disable"), "{notice}");
    assert!(
        value["first_run"]["commands"]
            .to_string()
            .contains("aethyme transcripts disable")
    );
    let lines = super::first_run::status_lines(&waiting.first_run).join("\n");
    assert!(
        lines.contains(&format!("automatic archiving starts at {starts_text}")),
        "{lines}"
    );
    assert!(lines.contains("Notice: transcript archiving is on by default"));
    assert!(lines.contains("run: aethyme transcripts enable --after-inactive-days <n>"));

    // Open (explicitly enabled): nothing to say.
    let explicit = TranscriptsConfig {
        enabled_source: config::EnabledSource::Settings,
        ..TranscriptsConfig::default()
    };
    let open_status = status(&host, &explicit, &mut open);
    assert_eq!(open_status.first_run.gate, super::Gate::Open);
    assert!(open_status.first_run.notice.is_none());
    assert!(super::first_run::status_lines(&open_status.first_run).is_empty());
}

#[test]
fn broker_start_prints_the_notice_while_the_wait_runs() {
    let host = Host::new();
    let config = TranscriptsConfig::default();
    let now = later();
    let advice = super::default_on_advice_at(&host.environment.host_dir, &config, now).unwrap();
    let lines = super::default_on_start_lines(&advice);
    let starts_text =
        crate::cli::insights::timestamp_millis(now + super::first_run::NOTICE_WAIT_MS);
    assert!(
        lines[0].starts_with("Notice (transcripts.archiving-default-on): "),
        "{lines:?}"
    );
    assert!(lines[0].contains(&starts_text), "{lines:?}");
    assert!(lines.contains(&"  run: aethyme transcripts disable".to_string()));
    assert!(
        lines.contains(&"  run: aethyme transcripts enable --after-inactive-days <n>".to_string())
    );
    // The compact JSON keeps the start time in the summary; the full form
    // also has it as evidence.
    assert!(advice.evidence.contains(&format!(
        "automatic_archiving_from_ms={}",
        now + super::first_run::NOTICE_WAIT_MS
    )));
    // Once the wait is over there is nothing to print.
    assert!(
        super::default_on_advice_at(
            &host.environment.host_dir,
            &config,
            now + super::first_run::NOTICE_WAIT_MS
        )
        .is_none()
    );
}
