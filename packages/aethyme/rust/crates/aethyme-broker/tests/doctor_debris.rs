//! `broker doctor plan` / `doctor apply` (#287): labelled gate resources are
//! listed with their owner's liveness next to every store's blockers, and only
//! those whose gate run is verifiably over are removed, through a digest.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use aethyme_broker::{
    Broker, ContainerRuntime, DebrisAction, DebrisApplyOutcome, DebrisOwner, RuntimeResource,
    RuntimeResourceKind,
};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    session_id: i64,
    key: String,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
        std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        let worktree = tmp.path().join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "work",
                worktree.to_str().unwrap(),
            ],
        );
        let mut broker = Broker::open(&repo).unwrap();
        let session_id = broker.adopt(&worktree, None).unwrap().id;
        let key = broker.gate_repository_key().unwrap();
        Self {
            _tmp: tmp,
            repo,
            session_id,
            key,
        }
    }

    fn broker(&self) -> Broker {
        Broker::open(&self.repo).unwrap()
    }

    /// A gate pidfile naming this test process, which is alive, for `run`.
    fn running_gate(&self, gate: &str, run: &str) {
        let dir = self.repo.join(".aethyme/run/gates");
        std::fs::create_dir_all(&dir).unwrap();
        let pid = std::process::id();
        std::fs::write(
            dir.join(format!("{}-{gate}.pid", self.session_id)),
            format!("{pid} tree {pid} - {run}\n"),
        )
        .unwrap();
    }

    fn labelled(
        &self,
        kind: RuntimeResourceKind,
        handle: &str,
        gate: &str,
        run: &str,
    ) -> RuntimeResource {
        let labels = BTreeMap::from([
            ("aethyme.repo".to_string(), self.key.clone()),
            ("aethyme.gate".to_string(), gate.to_string()),
            ("aethyme.session".to_string(), self.session_id.to_string()),
            ("aethyme.run".to_string(), run.to_string()),
        ]);
        RuntimeResource {
            kind,
            handle: handle.into(),
            name: handle.into(),
            labels,
        }
    }
}

#[derive(Default)]
struct FakeRuntime {
    resources: RefCell<Vec<RuntimeResource>>,
    removed: RefCell<Vec<String>>,
}

impl FakeRuntime {
    fn with(resources: Vec<RuntimeResource>) -> Self {
        Self {
            resources: RefCell::new(resources),
            removed: RefCell::default(),
        }
    }
}

impl ContainerRuntime for FakeRuntime {
    fn name(&self) -> &str {
        "fake"
    }

    fn list_labelled(&self) -> Result<Vec<RuntimeResource>, String> {
        Ok(self.resources.borrow().clone())
    }

    fn remove(&self, resource: &RuntimeResource) -> Result<(), String> {
        self.removed.borrow_mut().push(resource.handle.clone());
        self.resources
            .borrow_mut()
            .retain(|other| other.handle != resource.handle);
        Ok(())
    }
}

fn item<'a>(plan: &'a aethyme_broker::DebrisPlan, id: &str) -> &'a aethyme_broker::DebrisItem {
    plan.items
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("{id} missing from {:#?}", plan.items))
}

#[test]
fn an_orphan_of_a_finished_gate_run_is_listed_and_removed_through_the_digest() {
    let fixture = Fixture::new();
    // Listed volume first, as a runtime may: removal must still start with
    // the container that mounts it.
    let runtime = FakeRuntime::with(vec![
        fixture.labelled(RuntimeResourceKind::Volume, "quality-pg", "quality", "r1"),
        fixture.labelled(
            RuntimeResourceKind::Container,
            "c0ffee000000aaaa",
            "quality",
            "r1",
        ),
    ]);
    let mut broker = fixture.broker();
    let plan = broker.debris_plan(Some(&runtime));
    for id in ["container:c0ffee000000", "volume:quality-pg"] {
        let found = item(&plan, id);
        assert_eq!(found.owner, DebrisOwner::Dead, "{found:#?}");
        assert_eq!(found.action, DebrisAction::Remove);
        assert_eq!(found.session_id, Some(fixture.session_id));
    }
    assert_eq!(plan.removable_count, 2);

    let DebrisApplyOutcome::Applied(report) = broker
        .apply_debris_plan(Some(&runtime), &plan.digest)
        .unwrap()
    else {
        panic!("a current digest must apply");
    };
    // The container goes first: a volume is busy while it is still mounted.
    assert_eq!(
        *runtime.removed.borrow(),
        vec!["c0ffee000000aaaa".to_string(), "quality-pg".to_string()]
    );
    assert!(report.removals.iter().all(|removal| removal.removed));
    let events = broker
        .store()
        .events_after_filtered(0, 1000, Some(aethyme_broker::DOCTOR_REMOVED))
        .unwrap()
        .len();
    assert_eq!(events, 2);
}

#[test]
fn a_running_gates_resource_is_listed_and_kept_and_a_superseded_run_is_removable() {
    let fixture = Fixture::new();
    fixture.running_gate("quality", "current");
    let runtime = FakeRuntime::with(vec![
        fixture.labelled(
            RuntimeResourceKind::Container,
            "live000000000000",
            "quality",
            "current",
        ),
        fixture.labelled(
            RuntimeResourceKind::Container,
            "old0000000000000",
            "quality",
            "earlier",
        ),
    ]);
    let mut broker = fixture.broker();
    let plan = broker.debris_plan(Some(&runtime));
    let live = item(&plan, "container:live00000000");
    assert_eq!(live.owner, DebrisOwner::Live, "{live:#?}");
    assert_eq!(live.action, DebrisAction::Report);
    let superseded = item(&plan, "container:old000000000");
    assert_eq!(superseded.owner, DebrisOwner::Dead, "{superseded:#?}");

    let DebrisApplyOutcome::Applied(_) = broker
        .apply_debris_plan(Some(&runtime), &plan.digest)
        .unwrap()
    else {
        panic!("a current digest must apply");
    };
    assert_eq!(
        *runtime.removed.borrow(),
        vec!["old0000000000000".to_string()],
        "the live run's container must survive"
    );
}

#[test]
fn a_resource_whose_owner_cannot_be_established_is_reported_only() {
    let fixture = Fixture::new();
    let mut partial = fixture.labelled(
        RuntimeResourceKind::Container,
        "partial000000000",
        "quality",
        "r1",
    );
    partial.labels.remove("aethyme.run");
    partial.labels.remove("aethyme.session");
    let mut unknown_session =
        fixture.labelled(RuntimeResourceKind::Volume, "nosession", "quality", "r1");
    unknown_session
        .labels
        .insert("aethyme.session".into(), "999999".into());
    let mut foreign = fixture.labelled(RuntimeResourceKind::Volume, "foreign", "quality", "r1");
    foreign
        .labels
        .insert("aethyme.repo".into(), "another-repository".into());
    let runtime = FakeRuntime::with(vec![partial, unknown_session, foreign]);
    let mut broker = fixture.broker();
    let plan = broker.debris_plan(Some(&runtime));
    for id in ["container:partial00000", "volume:nosession"] {
        let found = item(&plan, id);
        assert_eq!(found.owner, DebrisOwner::Unknown, "{found:#?}");
        assert_eq!(found.action, DebrisAction::Report);
        assert!(
            found.clear.is_some(),
            "an unknown owner names how to inspect it"
        );
    }
    assert!(
        plan.items.iter().all(|item| item.id != "volume:foreign"),
        "another repository's broker judges its own resources"
    );
    assert_eq!(plan.removable_count, 0);
    broker
        .apply_debris_plan(Some(&runtime), &plan.digest)
        .unwrap();
    assert!(runtime.removed.borrow().is_empty());
}

#[test]
fn a_plan_that_changed_since_review_removes_nothing() {
    let fixture = Fixture::new();
    let runtime = FakeRuntime::with(vec![fixture.labelled(
        RuntimeResourceKind::Container,
        "c0ffee000000bbbb",
        "quality",
        "r1",
    )]);
    let mut broker = fixture.broker();
    let reviewed = broker.debris_plan(Some(&runtime)).digest;
    // The gate starts again with the same run before the operator applies:
    // the container is no longer an orphan.
    fixture.running_gate("quality", "r1");
    let outcome = broker.apply_debris_plan(Some(&runtime), &reviewed).unwrap();
    assert!(
        matches!(outcome, DebrisApplyOutcome::StaleDigest { .. }),
        "{outcome:?}"
    );
    assert!(runtime.removed.borrow().is_empty());
}

#[test]
fn an_unreadable_runtime_is_reported_not_treated_as_clean() {
    struct Broken;
    impl ContainerRuntime for Broken {
        fn name(&self) -> &str {
            "broken"
        }
        fn list_labelled(&self) -> Result<Vec<RuntimeResource>, String> {
            Err("daemon not running".into())
        }
        fn remove(&self, _: &RuntimeResource) -> Result<(), String> {
            unreachable!()
        }
    }
    let fixture = Fixture::new();
    let plan = fixture.broker().debris_plan(Some(&Broken));
    assert!(
        plan.unavailable
            .iter()
            .any(|source| source.source == "container runtime"),
        "{plan:#?}"
    );
    assert_eq!(
        plan.runtime.as_ref().map(|runtime| runtime.available),
        Some(false)
    );
}

fn cli(fixture: &Fixture, runtime: &Path, args: &[&str]) -> Output {
    Command::new(CLI)
        .args(args)
        .current_dir(&fixture.repo)
        .env("AETHYME_CONTAINER_RUNTIME", runtime)
        .env("AETHYME_AGENT_PID", std::process::id().to_string())
        .env(
            "AETHYME_HOST_STATE_DIR",
            fixture._tmp.path().join("host-state"),
        )
        .env_remove("AETHYME_REPO")
        .output()
        .unwrap()
}

#[test]
#[cfg(unix)]
fn the_cli_plans_and_applies_through_a_docker_compatible_runtime() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let bin = fixture._tmp.path().join("fake-docker");
    let log = fixture._tmp.path().join("docker.log");
    let labels = format!(
        "aethyme.repo={},aethyme.gate=quality,aethyme.session={},aethyme.run=r1",
        fixture.key, fixture.session_id
    );
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{log}'\ncase \"$1 $2\" in\n  'ps -a') printf '%s\\n' '{{\"ID\":\"deadbeef00000000\",\"Names\":\"pg\",\"Labels\":\"{labels}\"}}' ;;\n  'volume ls') ;;\nesac\nexit 0\n",
            log = log.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let plan = cli(&fixture, &bin, &["status", "doctor", "plan", "--json"]);
    assert!(
        plan.status.success(),
        "{}",
        String::from_utf8_lossy(&plan.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&plan.stdout).unwrap();
    assert_eq!(plan["removable_count"], 1, "{plan:#}");
    let digest = plan["digest"].as_str().unwrap().to_string();

    let stale = cli(
        &fixture,
        &bin,
        &["status", "doctor", "apply", "--confirm", &"0".repeat(64)],
    );
    assert_eq!(stale.status.code(), Some(3), "a stale digest is refused");
    assert!(!std::fs::read_to_string(&log).unwrap().contains("rm -f"));

    let applied = cli(
        &fixture,
        &bin,
        &["status", "doctor", "apply", "--confirm", &digest, "--json"],
    );
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert!(
        std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .any(|line| line == "rm -f deadbeef00000000")
    );
}
