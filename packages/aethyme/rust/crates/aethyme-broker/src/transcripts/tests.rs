//! Transcript archiving: deterministic tests over temporary directories and
//! invented names. Nothing here reads a real agent home.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

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
use super::run::{self, Options};
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
    fn open_under(&mut self, _root: &Path) -> Result<Vec<OpenFile>, String> {
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

fn pass(
    host: &Host,
    config: &TranscriptsConfig,
    options: Options,
    open: &mut OpenFiles<FakeProbe>,
    hooks: &mut dyn Hooks,
) -> run::RunReport {
    run::run_pass(&host.environment, config, options, open, hooks, later()).unwrap()
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
                                    cleanup_state TEXT NOT NULL DEFAULT 'open');
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
            archive_after_days: 3,
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
    assert!(!TranscriptsConfig::default().archive);
    let config = TranscriptsConfig::parse(
        "[transcripts]\narchive = true\narchive_after_days = 5\nproviders = [\"claude\", \"codex\", \"claude\"]\nrepositories = [\"/invented/repo\"]\n",
    )
    .unwrap();
    assert!(config.archive);
    assert_eq!(config.archive_after_days, 5);
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
            archive_after_days: Some(4),
            providers: Some(vec![Provider::Codex, Provider::Claude]),
        },
    )
    .unwrap();
    assert!(config.archive);
    assert_eq!(config.archive_after_days, 4);
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
    assert_eq!(disabled.archive_after_days, 4);
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
    let status = run::status(&host.environment, &config, &mut open).unwrap();
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
    let report = run::run_pass(
        &host.environment,
        &config,
        apply(),
        &mut open,
        &mut NoHooks,
        later(),
    )
    .unwrap();
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
    let report = run::run_pass(
        &host.environment,
        &config,
        apply(),
        &mut open,
        &mut NoHooks,
        super::now_ms(),
    )
    .unwrap();
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

    let restored = run::restore(&host.environment, &config, UUID).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(restored[0].hash_checked);
    assert_eq!(fs::read(&codex).unwrap(), codex_bytes);
    assert!(!codex_destination(&host, &codex).exists());

    let restored = run::restore(&host.environment, &config, claude.to_str().unwrap()).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(fs::read(&claude).unwrap(), claude_bytes);
    assert!(!claude_archive.exists());

    let index = Index::open(&host.environment.host_dir).unwrap();
    assert!(index.archives().unwrap().is_empty());
    assert_eq!(index.counters().unwrap().get("restored"), Some(&2));
    assert!(run::restore(&host.environment, &config, UUID).is_err());
}

#[test]
fn restore_refuses_to_overwrite_and_checks_the_hash() {
    let host = Host::new();
    let source = host.codex_transcript(UUID, Path::new("/work/invented"));
    let config = enabled(vec![Provider::Codex]);
    let mut open = probe(Vec::new());
    pass(&host, &config, apply(), &mut open, &mut NoHooks);
    let destination = codex_destination(&host, &source);

    fs::write(&source, b"something new").unwrap();
    assert!(run::restore(&host.environment, &config, UUID).is_err());
    assert!(destination.is_file(), "the archive is kept");
    fs::remove_file(&source).unwrap();

    let wrong = archive::RestorePlan {
        archive: destination.clone(),
        original: source.clone(),
        sha256: Some("0".repeat(64)),
    };
    assert!(archive::restore_one(&wrong, None).is_err());
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
    let restored = run::restore(&host.environment, &config, UUID).unwrap();
    assert_eq!(restored.len(), 1);
    assert!(!restored[0].hash_checked);
    assert_eq!(fs::read(&source).unwrap(), original);
}
