//! Executable Local compatibility schedules (#681). This is a test-only module;
//! it adds no alternate provider, capture, policy or authority implementation.
use crate::collaboration_state::{
    CollaborationRoot, CollaborationStateError, CollaborationStore, ProjectKey,
};
use crate::collaboration_submit::{self as capture, CaptureStatus, CaptureSubmit, Setting};
use crate::{Broker, CachePolicy, PromotionIntent};
use std::path::{Path, PathBuf};

fn git(repo: &Path, args: &[&str]) -> String {
    crate::local_target::tests::git(repo, args)
}
struct Fixture {
    repo: tempfile::TempDir,
    host: tempfile::TempDir,
    _worktrees: tempfile::TempDir,
    _origin: tempfile::TempDir,
    broker: Broker,
    session: i64,
    worktree: PathBuf,
}
impl Fixture {
    fn new(config: Option<&str>) -> Self {
        let repo = tempfile::Builder::new()
            .prefix("Playground-compatibility-")
            .tempdir()
            .unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("README.md"), "Playground\n").unwrap();
        git(repo.path(), &["add", "README.md"]);
        git(
            repo.path(),
            &[
                "commit",
                "-qm",
                "test(compatibility): initial Playground source",
            ],
        );
        if let Some(config) = config {
            std::fs::create_dir(repo.path().join(".aethyme")).unwrap();
            std::fs::write(repo.path().join(".aethyme/config.toml"), config).unwrap();
            git(repo.path(), &["add", ".aethyme/config.toml"]);
            git(
                repo.path(),
                &[
                    "commit",
                    "-qm",
                    "test(compatibility): explicit Playground policy",
                ],
            );
        }
        let origin = tempfile::tempdir().unwrap();
        git(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
        git(
            repo.path(),
            &["remote", "add", "origin", origin.path().to_str().unwrap()],
        );
        git(repo.path(), &["push", "-q", "-u", "origin", "main"]);
        git(repo.path(), &["remote", "set-head", "origin", "main"]);
        let host = tempfile::tempdir().unwrap();
        let worktrees = tempfile::tempdir().unwrap();
        let mut broker = Broker::open(repo.path())
            .unwrap()
            .with_worktree_root(worktrees.path())
            .with_collaboration_state(host.path());
        let session = broker
            .start_worktree("compatibility", Some("disposable Playground compatibility"))
            .unwrap();
        let worktree = PathBuf::from(&session.worktree_path);
        std::fs::write(worktree.join("independent.txt"), "independent Local edit\n").unwrap();
        git(&worktree, &["add", "independent.txt"]);
        git(
            &worktree,
            &[
                "commit",
                "-qm",
                "test(compatibility): independent Local edit",
            ],
        );
        Self {
            repo,
            host,
            _worktrees: worktrees,
            _origin: origin,
            broker,
            session: session.id,
            worktree,
        }
    }
    fn submit(&mut self) -> CaptureSubmit {
        capture::submit(
            &mut self.broker,
            self.session,
            CachePolicy::Use,
            PromotionIntent::Configured,
        )
        .unwrap()
    }
}
#[test]
fn absent_or_disabled_collaboration_allocates_no_retained_state_and_keeps_legacy_verdict() {
    for config in [
        None,
        Some("[collaboration]\ncapture='off'\nproject='proj-disabled'\n"),
        Some("[collaboration]\ncapture='off'\nfuture_setting='unknown but disabled'\n"),
    ] {
        let mut f = Fixture::new(config);
        assert!(!f.host.path().join("collaboration").exists());
        let outcome = match f.submit() {
            CaptureSubmit::Submitted {
                outcome,
                capture: None,
            } => outcome,
            _ => panic!("disabled must preserve the legacy result"),
        };
        assert!(!outcome.no_changes);
        assert_eq!(
            outcome.entry.head_commit,
            git(&f.worktree, &["rev-parse", "HEAD"])
        );
        assert!(outcome.conflicts.is_empty());
        assert!(!f.host.path().join("collaboration").exists());
        assert!(outcome.gate_outcomes.is_empty());
        let bytes = serde_json::to_vec(&outcome).unwrap();
        assert!(
            !String::from_utf8(bytes)
                .unwrap()
                .contains("collaboration_capture")
        );
    }
}
#[test]
fn advisory_capture_failure_preserves_real_legacy_submission() {
    let mut f = Fixture::new(Some(
        "[collaboration]\ncapture='advisory'\nproject='proj-compat'\n",
    ));
    std::fs::write(
        f.host.path().join("collaboration"),
        "blocked Local state path",
    )
    .unwrap();
    let (outcome, report) = match f.submit() {
        CaptureSubmit::Submitted {
            outcome,
            capture: Some(step),
        } => (outcome, step.finish(&f.broker)),
        _ => panic!("advisory must preserve actual legacy submit"),
    };
    assert!(!outcome.no_changes);
    assert!(outcome.conflicts.is_empty());
    assert_eq!(report.policy, "advisory");
    assert_eq!(report.status, CaptureStatus::NotConfigured);
    assert_eq!(report.code.as_deref(), Some("io"));
    assert!(report.receipt.is_none());
    assert_eq!(
        std::fs::read(f.host.path().join("collaboration")).unwrap(),
        b"blocked Local state path"
    );
}
#[test]
fn required_capture_failure_refuses_before_queue_or_integration_movement() {
    let mut f = Fixture::new(Some(
        "[collaboration]\ncapture='required'\nproject='proj-compat'\n",
    ));
    // Native submission may create the initial integration ref at main.
    // Establish that baseline first so this schedule checks promotion refusal.
    let (_, initial) = f.broker.integration_head().unwrap();
    assert_eq!(initial, git(f.repo.path(), &["rev-parse", "main"]));
    assert_ne!(initial, git(&f.worktree, &["rev-parse", "HEAD"]));
    std::fs::write(
        f.host.path().join("collaboration"),
        "blocked Local state path",
    )
    .unwrap();
    let before = git(
        f.repo.path(),
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads/aethyme/integration",
        ],
    );
    let report = match f.submit() {
        CaptureSubmit::Refused(report) => report,
        _ => panic!("required must refuse before legacy submit"),
    };
    assert_eq!(report.policy, "required");
    assert_eq!(report.status, CaptureStatus::NotConfigured);
    assert_eq!(report.code.as_deref(), Some("io"));
    assert!(report.receipt.is_none());
    assert_eq!(
        before,
        git(
            f.repo.path(),
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads/aethyme/integration"
            ]
        )
    );
    let queue: i64 = rusqlite::Connection::open(f.repo.path().join(".aethyme/broker.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM merge_queue", [], |r| r.get(0))
        .unwrap();
    assert_eq!(queue, 0);
}
#[test]
fn unknown_policy_and_unknown_required_keys_do_not_negotiate_to_safe() {
    for config in [
        "[collaboration]\ncapture='future'\n",
        "[collaboration]\ncapture='required'\nproject='proj-compat'\nfuture_setting=true\n",
    ] {
        assert!(matches!(
            capture::setting_from_text(Some(config)),
            Setting::Unsupported { .. }
        ));
    }
    match capture::setting_from_text(Some(
        "[collaboration]\ncapture='advisory'\nproject='proj-compat'\nfuture_setting=true\n",
    )) {
        Setting::On { ignored, .. } => assert_eq!(ignored, vec!["future_setting"]),
        _ => panic!(),
    }
}
fn valid_context_record() -> serde_json::Value {
    serde_json::json!({
        "schema":crate::collaboration_context::CONTEXT_SCHEMA_NAME,
        "authority":"advisory_context", "visibility":"local_project",
        "query":{"scope":[],"analysis":[]}, "items":[],
        "limit_detail":{"matched":"0","returned":"0","brief_tokens":"0","truncated_by":[]},
        "cache":{}, "coverage":"partial", "freshness":"exact", "limits":"within_limits"
    })
}
fn decode_context(
    value: &serde_json::Value,
) -> Result<
    aethyme_contracts::experimental_v0::Record,
    aethyme_contracts::experimental_v0::RecordError,
> {
    aethyme_contracts::experimental_v0::Record::decode(
        &serde_json::to_vec(value).unwrap(),
        &[&crate::collaboration_context::CONTEXT_SCHEMA],
    )
}
#[test]
fn unknown_record_schema_refuses_an_otherwise_valid_context() {
    let mut raw = valid_context_record();
    decode_context(&raw).unwrap();
    raw["schema"] = "aethyme.contribution-context/v999".into();
    assert!(matches!(
        decode_context(&raw),
        Err(aethyme_contracts::experimental_v0::RecordError::UnsupportedSchema { .. })
    ));
}
#[test]
fn required_unknown_capability_refuses_an_otherwise_valid_context() {
    let mut raw = valid_context_record();
    decode_context(&raw).unwrap();
    raw["requires"] = serde_json::json!(["new-authority-meaning"]);
    assert!(matches!(
        decode_context(&raw),
        Err(aethyme_contracts::experimental_v0::RecordError::UnsupportedCapability { .. })
    ));
}
/// Logical SQLite image includes every user table, trigger and index; CAS bytes
/// are independently hashed. WAL placement and connection bookkeeping are not
/// product data and are deliberately excluded from this comparison.
type CompatibilityStateImage = (
    Vec<Vec<String>>,
    Vec<(String, Vec<Vec<String>>)>,
    Vec<(PathBuf, String)>,
);
fn logical_state(conn: &rusqlite::Connection, objects: &Path) -> CompatibilityStateImage {
    let schema = conn
        .prepare("SELECT type,name,tbl_name,coalesce(sql,'') FROM sqlite_schema ORDER BY type,name")
        .unwrap()
        .query_map([], |r| {
            Ok((0..4)
                .map(|i| r.get::<_, String>(i).unwrap())
                .collect::<Vec<_>>())
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let names: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let mut data = vec![];
    for name in names {
        let mut statement = conn
            .prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))
            .unwrap();
        let columns = statement.column_count();
        let mut rows = statement
            .query_map([], |r| {
                Ok((0..columns)
                    .map(|i| format!("{:?}", r.get_ref(i).unwrap()))
                    .collect::<Vec<_>>())
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        rows.sort();
        data.push((name, rows));
    }
    let mut hashes = vec![];
    for prefix in std::fs::read_dir(objects).unwrap() {
        for entry in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            let entry = entry.unwrap().path();
            hashes.push((
                entry.strip_prefix(objects).unwrap().to_owned(),
                crate::collaboration_archive::ObjectDigest::of(&std::fs::read(&entry).unwrap())
                    .hex(),
            ));
        }
    }
    hashes.sort();
    (schema, data, hashes)
}
#[test]
fn newer_incompatible_store_refuses_without_migration_or_data_mutation() {
    let mut f = crate::local_target::tests::Fixture::new();
    let brief = aethyme_contracts::experimental_v0::brief::Brief::from_decision_file(
        br#"{"intent":"Preserve independently retained history."}"#,
    )
    .unwrap();
    crate::collaboration_context::attach_brief(&mut f.store, &f.a.lineage_record_id, &brief)
        .unwrap();
    let root = CollaborationRoot::under_host_state(f._host.path());
    let project = f.store.project().clone();
    let db = f.store.project_dir().join("state.db");
    let objects = crate::collaboration_archive::objects_dir(&f.store);
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute(
        "UPDATE meta SET value='999' WHERE key IN ('schema_version','min_compatible_schema')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO meta(key,value) VALUES('compatibility-evidence','unchanged')",
        [],
    )
    .unwrap();
    let before = logical_state(&conn, &objects);
    assert!(!before.2.is_empty());
    assert!(
        before
            .1
            .iter()
            .any(|(name, rows)| name == "retained_contributions" && !rows.is_empty())
    );
    assert!(
        before
            .1
            .iter()
            .any(|(name, rows)| name == "contribution_briefs" && !rows.is_empty())
    );
    assert!(matches!(
        CollaborationStore::open(&root, &project, &[]),
        Err(CollaborationStateError::SchemaTooNew { .. })
    ));
    assert_eq!(before, logical_state(&conn, &objects));
}
#[test]
fn overlapping_shared_state_root_refuses_with_a_separate_location_action() {
    let host = tempfile::tempdir().unwrap();
    let root = CollaborationRoot::under_host_state(host.path());
    let blocked = crate::collaboration_state::ForbiddenRoot {
        path: host.path().to_owned(),
        kind: crate::collaboration_state::ForbiddenKind::WorktreeContainer,
    };
    let result = CollaborationStore::open(
        &root,
        &ProjectKey::parse("unsafe-shared").unwrap(),
        &[blocked],
    );
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("unsafe cleanup overlap must refuse"),
    };
    assert!(error.to_string().contains("move"));
    assert!(!root.path().exists());
}
#[cfg(target_os = "macos")]
mod old_binary;

mod independence;

#[cfg(target_os = "macos")]
mod cli_parity;
