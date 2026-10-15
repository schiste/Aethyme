use super::*;
use crate::collaboration_archive::{CommitOid, RetainedContribution};
use crate::collaboration_gc::{self as gc, GcOptions};
use crate::collaboration_state::{CollaborationRoot, ProjectKey};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

// Fixture setup commands need only system utilities; resolve Git itself through
// the repository's trusted program probe and keep ambient user tools off PATH.
fn minimal_git_path() -> OsString {
    #[cfg(windows)]
    let paths = {
        let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        vec![PathBuf::from(system_root).join("System32")]
    };
    #[cfg(unix)]
    let paths = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
        .into_iter()
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    std::env::join_paths(paths).expect("minimal Git test PATH")
}

/// Run fixture setup with isolated Git config, identity, hooks and attributes.
/// Managed GitCarrier dispatches do not use this helper and retain receiver hooks.
pub(crate) fn git(repo: &Path, args: &[&str]) -> String {
    #[cfg(windows)]
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let home = tempfile::tempdir().unwrap();
    let hooks = home.path().join("empty-hooks");
    let templates = home.path().join("empty-templates");
    let xdg = home.path().join("xdg");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::create_dir_all(&xdg).unwrap();
    let global_config = home.path().join("global.gitconfig");
    let system_config = home.path().join("system.gitconfig");
    let attributes = home.path().join("global.attributes");
    let excludes = home.path().join("global.excludes");
    for path in [&global_config, &system_config, &attributes, &excludes] {
        std::fs::write(path, b"").unwrap();
    }

    let mut command = crate::git::git_command();
    command
        .current_dir(repo)
        .env_clear()
        .env("PATH", minimal_git_path())
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", &xdg)
        .env("GIT_CONFIG_GLOBAL", &global_config)
        .env("GIT_CONFIG_SYSTEM", &system_config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TEMPLATE_DIR", &templates)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .arg("-c")
        .arg("commit.gpgsign=false")
        .arg("-c")
        .arg("core.autocrlf=false")
        .arg("-c")
        .arg(format!("core.hooksPath={}", hooks.display()))
        .arg("-c")
        .arg(format!("core.attributesFile={}", attributes.display()))
        .arg("-c")
        .arg(format!("core.excludesFile={}", excludes.display()))
        .arg("-c")
        .arg(format!("init.templateDir={}", templates.display()))
        .arg("-c")
        .arg("user.name=fixture")
        .arg("-c")
        .arg("user.email=fixture@example.invalid")
        .args(args);
    #[cfg(windows)]
    command
        .env("SystemRoot", &system_root)
        .env("USERPROFILE", home.path());
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn open(host: &Path) -> CollaborationStore {
    CollaborationStore::open(
        &CollaborationRoot::under_host_state(host),
        &ProjectKey::parse("local-target-test").unwrap(),
        &[],
    )
    .unwrap()
}

fn age_objects(store: &CollaborationStore) {
    for prefix in std::fs::read_dir(archive::objects_dir(store)).unwrap() {
        for object in std::fs::read_dir(prefix.unwrap().path()).unwrap() {
            std::fs::File::open(object.unwrap().path())
                .unwrap()
                .set_times(
                    std::fs::FileTimes::new()
                        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1)),
                )
                .unwrap();
        }
    }
}

pub(crate) struct Fixture {
    pub(crate) _host: tempfile::TempDir,
    pub(crate) _repo: tempfile::TempDir,
    pub(crate) store: CollaborationStore,
    pub(crate) a: RetainedContribution,
    pub(crate) b: RetainedContribution,
}

impl Fixture {
    pub(crate) fn feature(&mut self, base: &CommitOid) -> RequestedContribution {
        git(self._repo.path(), &["switch", "-qc", "C", base.as_str()]);
        std::fs::write(self._repo.path().join("three.txt"), "C\n").unwrap();
        git(self._repo.path(), &["add", "three.txt"]);
        git(self._repo.path(), &["commit", "-qm", "C"]);
        let commit = CommitOid::parse(&git(self._repo.path(), &["rev-parse", "HEAD"])).unwrap();
        let retained =
            archive::retain_contribution(&mut self.store, self._repo.path(), base, &commit)
                .unwrap();
        RequestedContribution {
            id: "C".into(),
            lineage: retained.lineage_record_id.to_string(),
            requires: vec![],
            atomic_group: None,
            revision_of: None,
            derived_from: vec![],
        }
    }

    pub(crate) fn new() -> Self {
        let host = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("one.txt"), "base\n").unwrap();
        git(repo.path(), &["add", "one.txt"]);
        git(repo.path(), &["commit", "-qm", "base"]);
        let base = CommitOid::parse(&git(repo.path(), &["rev-parse", "HEAD"])).unwrap();
        std::fs::write(repo.path().join("one.txt"), "A\n").unwrap();
        git(repo.path(), &["commit", "-qam", "A"]);
        let a = CommitOid::parse(&git(repo.path(), &["rev-parse", "HEAD"])).unwrap();
        // B starts from base, independently, without resetting any shared repo.
        git(repo.path(), &["switch", "-qc", "B", base.as_str()]);
        std::fs::write(repo.path().join("two.txt"), "B\n").unwrap();
        git(repo.path(), &["add", "two.txt"]);
        git(repo.path(), &["commit", "-qm", "B"]);
        let b = CommitOid::parse(&git(repo.path(), &["rev-parse", "HEAD"])).unwrap();
        let mut store = open(host.path());
        let a = archive::retain_contribution(&mut store, repo.path(), &base, &a).unwrap();
        let b = archive::retain_contribution(&mut store, repo.path(), &base, &b).unwrap();
        Self {
            _host: host,
            _repo: repo,
            store,
            a,
            b,
        }
    }

    pub(crate) fn spec(&self) -> TargetSpec {
        let requested = |id: &str, c: &RetainedContribution| RequestedContribution {
            id: id.into(),
            lineage: c.lineage_record_id.to_string(),
            requires: vec![],
            atomic_group: None,
            revision_of: None,
            derived_from: vec![],
        };
        TargetSpec {
            objective_authorization: format!("sha256:{}", "1".repeat(64)),
            objective: "retain both independent features".into(),
            baseline: self.a.base.snapshot_id.to_string(),
            contributions: vec![requested("A", &self.a), requested("B", &self.b)],
            selected: vec!["A".into()],
            requirements: vec![format!("sha256:{}", "2".repeat(64))],
            acceptance_policy: format!("sha256:{}", "3".repeat(64)),
            composer_profile: crate::composer::PROVISIONAL_TEXT_PROFILE.into(),
        }
    }
}

#[test]
fn immutable_revisions_and_arrivals_do_not_change_frozen_target() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let original = {
        let spec = f.spec();
        propose_revision(&mut f.store, &owner, spec)
    }
    .unwrap();
    select_revision(&mut f.store, &owner, &original.id).unwrap();
    let s = status(&f.store, "target").unwrap();
    let s = transition(&mut f.store, &owner, s.version, Phase::Admitting).unwrap();
    transition(&mut f.store, &owner, s.version, Phase::WaveFrozen).unwrap();
    let mut next = f.spec();
    next.selected.push("B".into());
    let next = propose_revision(&mut f.store, &owner, next).unwrap();
    assert_ne!(original.id, next.id);
    assert!(matches!(
        select_revision(&mut f.store, &owner, &next.id),
        Err(TargetError::Frozen)
    ));
    assert_eq!(
        status(&f.store, "target").unwrap().revision,
        Some(original.id.clone())
    );
    assert_eq!(load_revision(&f.store, &original.id).unwrap(), original);
    assert_eq!(
        dispositions(&f.store, &original.id)
            .unwrap()
            .iter()
            .map(|d| d.status)
            .collect::<Vec<_>>(),
        vec![Disposition::Pending, Disposition::Excluded]
    );
}

#[test]
fn independent_canonical_identity_vector() {
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("revision-vector.json")).unwrap();
    let bytes = vector["canonical"].as_str().unwrap().as_bytes();
    let record = Record::decode(bytes, &[&TARGET_REVISION_SCHEMA]).unwrap();
    assert_eq!(record.canonical_bytes(), bytes);
    assert_eq!(record.id().as_str(), vector["id"].as_str().unwrap());
    let value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
    let spec: TargetSpec = serde_json::from_value(value["spec"].clone()).unwrap();
    assert_eq!(normalize(spec.clone()).unwrap(), spec);
}

#[test]
fn identities_pin_policy_objective_requirements_dependencies_and_order() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.selected.push("B".into());
    let original = propose_revision(&mut f.store, &owner, spec.clone()).unwrap();
    let repeated = propose_revision(&mut f.store, &owner, spec.clone()).unwrap();
    assert_eq!(original.id, repeated.id);
    let mut reordered_catalog = spec.clone();
    reordered_catalog.contributions.reverse();
    assert_eq!(
        propose_revision(&mut f.store, &owner, reordered_catalog)
            .unwrap()
            .id,
        original.id
    );
    for change in 0..6 {
        let mut changed = spec.clone();
        match change {
            0 => changed.acceptance_policy = format!("sha256:{}", "4".repeat(64)),
            1 => changed.objective_authorization = format!("sha256:{}", "5".repeat(64)),
            2 => changed
                .requirements
                .push(format!("sha256:{}", "6".repeat(64))),
            3 => changed.contributions[1].requires.push("A".into()),
            4 => changed.selected.reverse(),
            _ => changed.composer_profile = "future-structural/v1".into(),
        }
        assert_ne!(
            propose_revision(&mut f.store, &owner, changed).unwrap().id,
            original.id
        );
    }
}

#[test]
fn dependency_errors_refuse_before_creating_target_revision() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    for case in 0..6 {
        let mut spec = f.spec();
        match case {
            0 => spec.contributions[0].requires.push("B".into()),
            1 => {
                spec.contributions[0].requires.push("B".into());
                spec.contributions[1].requires.push("A".into());
                spec.selected.push("B".into());
            }
            2 => {
                spec.contributions[0].atomic_group = Some("group".into());
                spec.contributions[1].atomic_group = Some("group".into());
            }
            3 => {
                spec.contributions[1].revision_of = Some("A".into());
                spec.selected.push("B".into());
            }
            4 => spec.selected.push("missing".into()),
            _ => spec.contributions[1].lineage = spec.contributions[0].lineage.clone(),
        }
        assert!(
            matches!(
                propose_revision(&mut f.store, &owner, spec),
                Err(TargetError::Invalid(_) | TargetError::SelectionRefused { .. })
            ),
            "case {case}"
        );
    }
    let count: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_target_revisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn revision_lines_and_synthesis_cannot_double_apply_original_work() {
    use crate::composition::Refusal;
    let mut f = Fixture::new();
    let base = f.a.base.commit.clone();
    let c = f.feature(&base);
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    for case in 0..3 {
        let mut spec = f.spec();
        spec.selected.push("B".into());
        match case {
            0 => {
                spec.contributions[0].revision_of = Some("original".into());
                spec.contributions[1].revision_of = Some("original".into());
            }
            1 => {
                spec.contributions[1].revision_of = Some("A".into());
                let mut c = c.clone();
                c.revision_of = Some("B".into());
                spec.contributions.push(c);
                spec.selected = vec!["A".into(), "C".into()];
            }
            _ => spec.contributions[1].derived_from.push("A".into()),
        }
        assert!(
            matches!(
                propose_revision(&mut f.store, &owner, spec),
                Err(TargetError::SelectionRefused {
                    reason: Refusal::CompetingRevisions,
                    ..
                })
            ),
            "case {case}"
        );
    }
    let count: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_target_revisions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn atomic_replacement_covers_the_original_group_member() {
    let mut f = Fixture::new();
    let base = f.a.base.commit.clone();
    let mut c = f.feature(&base);
    c.revision_of = Some("A".into());
    c.atomic_group = Some("group".into());
    let mut spec = f.spec();
    for contribution in &mut spec.contributions {
        contribution.atomic_group = Some("group".into());
    }
    spec.contributions.push(c);
    spec.selected = vec!["C".into(), "B".into()];
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = propose_revision(&mut f.store, &owner, spec).unwrap();
    let dispositions = dispositions(&f.store, &revision.id).unwrap();
    assert_eq!(dispositions[0].status, Disposition::Excluded);
    assert_eq!(dispositions[1].status, Disposition::Pending);
    assert_eq!(dispositions[2].status, Disposition::Pending);
}

#[test]
fn retained_lineage_requires_its_selected_predecessor_and_is_part_of_cycles() {
    use crate::composition::Refusal;
    let mut f = Fixture::new();
    let inherited = f.a.result.commit.clone();
    let c = f.feature(&inherited);
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut spec = f.spec();
    spec.contributions.push(c);
    spec.selected = vec!["C".into()];
    assert!(matches!(
        propose_revision(&mut f.store, &owner, spec.clone()),
        Err(TargetError::SelectionRefused {
            reason: Refusal::MissingInput,
            ..
        })
    ));
    spec.selected.push("A".into());
    spec.contributions[0].requires.push("C".into());
    assert!(matches!(
        propose_revision(&mut f.store, &owner, spec),
        Err(TargetError::SelectionRefused {
            reason: Refusal::DependencyCycle,
            ..
        })
    ));
}

#[test]
fn self_requirement_is_a_dependency_cycle() {
    let mut f = Fixture::new();
    let mut spec = f.spec();
    spec.contributions[0].requires.push("A".into());
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    assert!(matches!(
        propose_revision(&mut f.store, &owner, spec),
        Err(TargetError::SelectionRefused {
            reason: crate::composition::Refusal::DependencyCycle,
            ..
        })
    ));
}

#[test]
fn unknown_source_base_refuses_even_when_all_bytes_are_retained() {
    let mut f = Fixture::new();
    let mut spec = f.spec();
    spec.baseline = f.b.result.snapshot_id.to_string();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    assert!(matches!(
        propose_revision(&mut f.store, &owner, spec),
        Err(TargetError::SelectionRefused {
            reason: crate::composition::Refusal::UnknownBase,
            ..
        })
    ));
}

#[test]
fn archived_target_can_be_proposed_after_the_source_repository_is_gone() {
    let mut f = Fixture::new();
    let spec = f.spec();
    std::fs::remove_dir_all(f._repo.path()).unwrap();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = propose_revision(&mut f.store, &owner, spec).unwrap();
    assert_eq!(load_revision(&f.store, &revision.id).unwrap(), revision);
}

#[test]
fn duplicate_controller_issuer_and_project_cannot_gain_authority() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut other = open(f._host.path());
    assert!(matches!(
        acquire_controller(&mut other, "target", "appointed"),
        Err(TargetError::ControllerBusy)
    ));
    let other_host = tempfile::tempdir().unwrap();
    let mut unrelated = open(other_host.path());
    assert!(matches!(
        propose_revision(&mut unrelated, &owner, f.spec()),
        Err(TargetError::Unauthorized)
    ));
    drop(owner);
    assert!(matches!(
        acquire_controller(&mut other, "target", "intruder"),
        Err(TargetError::Unauthorized)
    ));
    let owner = acquire_controller(&mut other, "target", "appointed").unwrap();
    assert!(propose_revision(&mut other, &owner, f.spec()).is_ok());
}

#[test]
fn stale_transition_duplicate_listener_and_hold_cannot_skip_a_phase() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = {
        let spec = f.spec();
        propose_revision(&mut f.store, &owner, spec)
    }
    .unwrap();
    select_revision(&mut f.store, &owner, &revision.id).unwrap();
    let s = status(&f.store, "target").unwrap();
    assert!(matches!(
        transition(&mut f.store, &owner, s.version, Phase::Composing),
        Err(TargetError::Transition { .. })
    ));
    let s2 = transition(&mut f.store, &owner, s.version, Phase::Admitting).unwrap();
    assert!(matches!(
        transition(&mut f.store, &owner, s.version, Phase::WaveFrozen),
        Err(TargetError::StaleVersion)
    ));
    let s3 = transition(&mut f.store, &owner, s2.version, Phase::WaitingForInput).unwrap();
    assert_eq!(s3.resume_phase, Some(Phase::Admitting));
    assert!(matches!(
        transition(&mut f.store, &owner, s3.version, Phase::WaveFrozen),
        Err(TargetError::Transition { .. })
    ));
    let s = resume(&mut f.store, &owner, s3.version).unwrap();
    assert_eq!(s.phase, Phase::Admitting);
    let s = transition(&mut f.store, &owner, s.version, Phase::StopRequested).unwrap();
    transition(&mut f.store, &owner, s.version, Phase::Stopped).unwrap();
    assert!(matches!(
        release_revision(&mut f.store, &owner, &revision.id),
        Err(TargetError::Frozen)
    ));
}

#[test]
fn unverified_candidate_never_becomes_accepted_or_complete() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = {
        let spec = f.spec();
        propose_revision(&mut f.store, &owner, spec)
    }
    .unwrap();
    select_revision(&mut f.store, &owner, &revision.id).unwrap();
    for next in [
        Phase::Admitting,
        Phase::WaveFrozen,
        Phase::Composing,
        Phase::Verifying,
    ] {
        let s = status(&f.store, "target").unwrap();
        transition(&mut f.store, &owner, s.version, next).unwrap();
    }
    let s = status(&f.store, "target").unwrap();
    assert!(matches!(
        transition(&mut f.store, &owner, s.version, Phase::ReadyForAcceptance),
        Err(TargetError::VerificationUnavailable)
    ));
    assert_eq!(status(&f.store, "target").unwrap().phase, Phase::Verifying);
    assert!(
        dispositions(&f.store, &revision.id)
            .unwrap()
            .iter()
            .all(|d| matches!(d.status, Disposition::Pending | Disposition::Excluded))
    );
}

#[test]
fn target_retention_outlives_controller_and_old_gc_plan() {
    let mut f = Fixture::new();
    age_objects(&f.store);
    let options = GcOptions::default();
    let obsolete_plan = gc::plan(&mut f.store, &options).unwrap();
    assert!(!obsolete_plan.reclaimable.is_empty());
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = {
        let spec = f.spec();
        propose_revision(&mut f.store, &owner, spec)
    }
    .unwrap();
    drop(owner);
    gc::apply(&mut f.store, &obsolete_plan.digest, &options).unwrap();
    assert!(
        archive::retained_contribution(&f.store, &f.a.lineage_record_id)
            .unwrap()
            .is_some()
    );
    assert!(
        archive::retained_contribution(&f.store, &f.b.lineage_record_id)
            .unwrap()
            .is_some()
    );
    assert!(
        gc::plan(&mut f.store, &options)
            .unwrap()
            .reclaimable
            .is_empty()
    );
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    release_revision(&mut f.store, &owner, &revision.id).unwrap();
    assert!(matches!(
        select_revision(&mut f.store, &owner, &revision.id),
        Err(TargetError::Released)
    ));
    let reclaim = gc::plan(&mut f.store, &options).unwrap();
    assert!(!reclaim.reclaimable.is_empty());
    gc::apply(&mut f.store, &reclaim.digest, &options).unwrap();
    assert!(
        archive::retained_contribution(&f.store, &f.a.lineage_record_id)
            .unwrap()
            .is_none()
    );
    assert_eq!(load_revision(&f.store, &revision.id).unwrap(), revision);
}

#[test]
fn corrupt_record_and_missing_source_refuse_without_partial_roots() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let mut missing = f.spec();
    missing.baseline = format!("sha256:{}", "a".repeat(64));
    assert!(matches!(
        propose_revision(&mut f.store, &owner, missing),
        Err(TargetError::Archive(_))
    ));
    let count: i64 = f
        .store
        .read_connection()
        .query_row("SELECT count(*) FROM local_target_roots", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
    let revision = {
        let spec = f.spec();
        propose_revision(&mut f.store, &owner, spec)
    }
    .unwrap();
    f.store
        .connection()
        .execute(
            "UPDATE local_target_revisions SET body = ?1 WHERE revision = ?2",
            params![b"{}".to_vec(), revision.id.as_str()],
        )
        .unwrap();
    assert!(matches!(
        load_revision(&f.store, &revision.id),
        Err(TargetError::Corrupt(_))
    ));
}

#[test]
fn candidate_and_request_objects_are_pinned_atomically_with_their_source() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = {
        let spec = f.spec();
        propose_revision(&mut f.store, &owner, spec)
    }
    .unwrap();
    let artifact = archive::put_object(&f.store, b"exact candidate manifest or request").unwrap();
    let missing = ObjectDigest::of(b"never written");
    assert!(
        retain_artifacts(
            &mut f.store,
            &owner,
            &revision.id,
            &[],
            &[artifact, missing]
        )
        .is_err()
    );
    let count: i64 = f
        .store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM local_target_roots WHERE kind = 'object'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
    retain_artifacts(
        &mut f.store,
        &owner,
        &revision.id,
        &[f.a.result.snapshot_id.clone()],
        &[artifact],
    )
    .unwrap();
    age_objects(&f.store);
    let options = GcOptions::default();
    assert!(
        gc::plan(&mut f.store, &options)
            .unwrap()
            .reclaimable
            .is_empty()
    );
    release_revision(&mut f.store, &owner, &revision.id).unwrap();
    assert!(
        !gc::plan(&mut f.store, &options)
            .unwrap()
            .reclaimable
            .is_empty()
    );
}

#[test]
fn migration_adopts_charged_resolution_attempts_and_fences_schema_six() {
    let f = Fixture::new();
    let host = f._host.path().to_path_buf();
    let db = f.store.project_dir().join("state.db");
    drop(f.store);
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("UPDATE meta SET value = '6' WHERE key IN ('schema_version', 'min_compatible_schema');
        DROP TABLE local_adoption_receipts; DROP TABLE local_agent_dispatches; DROP TABLE local_agent_reconciliations; DROP TABLE local_handoffs; DROP TABLE local_agent_sessions; DROP TABLE local_import_rehearsals; DROP TABLE local_imported_records; DROP TABLE local_imported_sources; DROP TABLE local_imports; DROP TABLE local_exchange_messages; DROP TABLE local_exchange_deliveries; DROP TABLE local_exchange_streams; DROP TABLE local_exchange_events; DROP TABLE local_exchange_projection; DROP TABLE local_exchange_observations; DROP TABLE local_exchange_bindings; DROP TRIGGER outbox_capture_insert; DROP TRIGGER outbox_capture_update; DROP TRIGGER outbox_capture_delete; DROP TABLE local_generation_roots; DROP TABLE local_generation_receipts; DROP TABLE local_generations; DROP TABLE local_acceptance_qualifications; DROP TABLE local_acceptance_receipts; DROP TABLE local_acceptance_bindings; DROP TABLE local_source_transport_receipts; DROP TABLE local_source_transports; DROP TABLE local_verification_evidence; DROP TABLE local_verifier_producers; DROP TABLE local_authority_state; DROP TABLE local_waves; DROP TABLE local_wave_arrivals;
        DROP TABLE local_target_roots; DROP TABLE local_target_dispositions; DROP TABLE local_target_revisions; DROP TABLE local_targets;
        INSERT INTO resolution_attempts VALUES (1, 'decision', 'conflict', 'A', 'one.txt', 'original-request', 'resolver', 'failed', 1);").unwrap();
    drop(c);
    let store = open(&host);
    assert_eq!(
        store.schema_version(),
        crate::collaboration_state::COLLABORATION_STATE_SCHEMA_VERSION
    );
    let count: i64 = store
        .read_connection()
        .query_row(
            "SELECT count(*) FROM resolution_attempts WHERE decision_key = 'decision'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    let floor: String = store
        .read_connection()
        .query_row(
            "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        floor,
        crate::collaboration_state::COLLABORATION_STATE_SCHEMA_VERSION.to_string()
    );
}

// Run in a real child process with a dedicated host root. The parent kills
// only its own child, testing OS lock release rather than a simulated lease.
#[test]
fn controller_child() {
    let Some(host) = std::env::var_os("AETHYME_TARGET_TEST_HOST") else {
        return;
    };
    let host = PathBuf::from(host);
    let mut store = open(&host);
    let _owner = acquire_controller(&mut store, "target", "appointed").unwrap();
    std::fs::write(host.join("child-ready"), "ready").unwrap();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();
}

#[test]
fn controller_process_death_releases_lock_but_preserves_revision() {
    let mut f = Fixture::new();
    let owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    let revision = {
        let spec = f.spec();
        propose_revision(&mut f.store, &owner, spec)
    }
    .unwrap();
    select_revision(&mut f.store, &owner, &revision.id).unwrap();
    drop(owner);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "local_target::tests::controller_child",
            "--nocapture",
        ])
        .env("AETHYME_TARGET_TEST_HOST", f._host.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !f._host.path().join("child-ready").exists() {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("child did not acquire its lock");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(matches!(
        acquire_controller(&mut f.store, "target", "appointed"),
        Err(TargetError::ControllerBusy)
    ));
    child.kill().unwrap();
    child.wait().unwrap();
    let _owner = acquire_controller(&mut f.store, "target", "appointed").unwrap();
    assert_eq!(
        status(&f.store, "target").unwrap().revision,
        Some(revision.id.clone())
    );
    assert_eq!(load_revision(&f.store, &revision.id).unwrap(), revision);
}
