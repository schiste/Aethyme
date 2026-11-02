//! #1237 prebuild: conditions, the decision, and whole runs against a real
//! repository, template hooks and the template cache.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::compile::{CompileContext, EnvironmentOwner, compile};
use super::lifecycle::Lifecycle;
use super::prebuild::{
    Condition, Decision, GateInputs, OutcomeReporter, PrebuildState, Status, any_pending, decide,
    gate, local_minutes, run_for_repository,
};
use super::spec::{EnvironmentsFile, HostEnvironmentsFile, PrebuildSpec};
use super::store::EnvironmentStore;
use super::{ENVIRONMENTS_FILE, Loaded, parse_loaded};
use crate::resources::HostResourceCoordinator;

fn spec(when: &[&str]) -> PrebuildSpec {
    PrebuildSpec {
        name: "db".into(),
        profile: "p".into(),
        component: "db".into(),
        reference: "default_branch".into(),
        derive_from_previous: true,
        when: when.iter().map(|w| w.to_string()).collect(),
        quiet_minutes: 15,
        keep: 2,
        timeout_seconds: 60,
        artifact: None,
        max_memory_pct: None,
        preemptible: false,
    }
}

fn inputs(local_minutes: u16, idle_minutes: Option<i64>) -> GateInputs {
    let now_ms = 10_000_000_000;
    GateInputs {
        local_minutes,
        last_activity_ms: idle_minutes.map(|m| now_ms - m * 60_000),
        now_ms,
    }
}

#[test]
fn conditions_parse_and_refuse_malformed_text() {
    assert_eq!(Condition::parse("host_idle"), Ok(Condition::HostIdle));
    assert_eq!(
        Condition::parse("outside:09:00-19:00"),
        Ok(Condition::Outside(540, 1140))
    );
    assert_eq!(
        Condition::parse("inside:22:30-06:00"),
        Ok(Condition::Inside(1350, 360))
    );
    for bad in [
        "idle",
        "inside:9:00-19:00",
        "inside:09:00-24:00",
        "outside:09:00-09:00",
        "outside:09:00",
        "between:01:00-02:00",
    ] {
        assert!(Condition::parse(bad).is_err(), "{bad}");
    }
}

/// Any condition opens the gate; none closes it with the reason; no
/// conditions means always. Windows wrap past midnight.
#[test]
fn the_gate_opens_on_any_condition() {
    assert_eq!(gate(&spec(&[]), &inputs(600, Some(0))), Ok(()));
    let idle = spec(&["host_idle"]);
    assert!(gate(&idle, &inputs(600, Some(5))).is_err());
    assert_eq!(gate(&idle, &inputs(600, Some(15))), Ok(()));
    assert_eq!(gate(&idle, &inputs(600, None)), Ok(()));
    let night = spec(&["inside:22:00-06:00"]);
    assert_eq!(gate(&night, &inputs(23 * 60, Some(0))), Ok(()));
    assert_eq!(gate(&night, &inputs(60, Some(0))), Ok(()));
    let closed = gate(&night, &inputs(12 * 60, Some(0))).unwrap_err();
    assert!(closed.contains("inside:22:00-06:00"), "{closed}");
    let either = spec(&["host_idle", "outside:09:00-19:00"]);
    assert_eq!(gate(&either, &inputs(12 * 60, Some(30))), Ok(()));
    assert_eq!(gate(&either, &inputs(20 * 60, Some(1))), Ok(()));
    assert!(gate(&either, &inputs(12 * 60, Some(1))).is_err());
}

#[test]
fn the_decision_builds_only_what_is_missing_and_holds_failures() {
    let fresh = PrebuildState::default();
    let failed = PrebuildState {
        failed_digest: Some("d1".into()),
        ..PrebuildState::default()
    };
    assert_eq!(
        decide(&fresh, "d1", true, Ok(()), true),
        Decision::Published
    );
    assert_eq!(decide(&fresh, "d1", false, Ok(()), false), Decision::Build);
    assert_eq!(
        decide(&fresh, "d1", false, Err("later".into()), false),
        Decision::Wait("later".into())
    );
    assert_eq!(decide(&failed, "d1", false, Ok(()), false), Decision::Held);
    assert_eq!(decide(&failed, "d2", false, Ok(()), false), Decision::Build);
    assert_eq!(
        decide(&failed, "d1", false, Err("later".into()), true),
        Decision::Build
    );
}

#[test]
fn local_minutes_are_a_time_of_day() {
    assert!(local_minutes(0) < 24 * 60);
    assert!(local_minutes(1_793_621_308_654) < 24 * 60);
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {output:?}");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Records every outcome report and answers that it was reported.
#[derive(Default)]
struct Reports(Vec<crate::OutcomeReportRequest>);

impl OutcomeReporter for Reports {
    fn report(&mut self, request: &crate::OutcomeReportRequest) -> serde_json::Value {
        self.0.push(request.clone());
        serde_json::json!({ "status": "reported" })
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    repo: PathBuf,
    host: PathBuf,
    lifecycle: Lifecycle,
    log: PathBuf,
}

impl Fixture {
    /// A repository whose `db` template is built by a hook that appends its
    /// template id, derived-from id and the commit's migration to `log`, and
    /// fails while `db/fail` exists.
    fn new(when: &str, keep: usize) -> Self {
        Self::custom(|log, destroyed| {
            format!(
                r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"
template = 'test ! -e db/fail && echo "$AETHYME_ENV_TEMPLATE_ID from=${{AETHYME_PREBUILD_PREVIOUS_ID:-none}} $(cat db/schema.sql)" >> "{log}"'
template_destroy = 'echo "$AETHYME_ENV_TEMPLATE_ID" >> "{destroyed}"'
template_inputs = ["db/**"]

[[prebuild]]
name = "db"
profile = "p"
component = "db"
derive_from_previous = true
when = [{when}]
keep = {keep}
"#,
                log = log.display(),
                destroyed = destroyed.display(),
            )
        })
    }

    /// A repository with the environments file `text(log, destroyed)`, a
    /// `db/schema.sql` and a `web/app.txt`.
    fn custom(text: impl FnOnce(&Path, &Path) -> String) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let host = temp.path().join("host");
        std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
        std::fs::create_dir_all(repo.join("db")).unwrap();
        std::fs::create_dir_all(repo.join("web")).unwrap();
        let log = temp.path().join("templates.log");
        let destroyed = temp.path().join("destroyed.log");
        std::fs::write(repo.join(ENVIRONMENTS_FILE), text(&log, &destroyed)).unwrap();
        std::fs::write(repo.join("web/app.txt"), "app v1").unwrap();
        std::fs::write(repo.join(".gitignore"), "dist/\n").unwrap();
        std::fs::write(repo.join("db/schema.sql"), "v1").unwrap();
        std::fs::write(repo.join("README.md"), "r").unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-qm", "init"]);
        let lifecycle = Lifecycle {
            store: EnvironmentStore::open(&host.join("environments.db")).unwrap(),
            coordinator: HostResourceCoordinator::open(&host.join("host.db")).unwrap(),
            root: host.join("environments"),
            holder_pid: None,
            hook_timeout: Duration::from_secs(20),
            actor: "test".into(),
        };
        Self {
            _temp: temp,
            repo,
            host,
            lifecycle,
            log,
        }
    }

    fn commit(&self, path: &str, text: Option<&str>) {
        let full = self.repo.join(path);
        match text {
            Some(text) => std::fs::write(full, text).unwrap(),
            None => std::fs::remove_file(full).unwrap(),
        }
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-qm", path]);
    }

    fn run(&mut self, force: bool, reports: &mut Reports) -> super::prebuild::PrebuildReport {
        let mut all = run_for_repository(
            &mut self.lifecycle,
            &self.repo,
            &HostEnvironmentsFile::default(),
            force,
            None,
            reports,
        )
        .unwrap();
        assert_eq!(all.len(), 1);
        all.remove(0)
    }

    fn built(&self) -> Vec<String> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn repository(&self) -> String {
        crate::gates::git_origin_fingerprint(&crate::GitRepo::discover(&self.repo).unwrap())
    }

    /// The digest `env up` computes in the repository's own checkout.
    fn worktree_digest(&self) -> String {
        let text = std::fs::read_to_string(self.repo.join(ENVIRONMENTS_FILE)).unwrap();
        let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", &text);
        let file = loaded.value.unwrap();
        let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
        assert!(found.is_empty(), "{found:#?}");
        let fingerprint = crate::console::worktree_fingerprint(&self.repo);
        let compiled = compile(
            &file,
            &HostEnvironmentsFile::default(),
            "p",
            "t",
            &CompileContext {
                repository: self.repository(),
                owner: EnvironmentOwner::Worktree {
                    fingerprint: fingerprint.clone(),
                },
                worktree_fingerprint: fingerprint,
                source_commit: None,
                source_dirty: false,
                source_digest: String::new(),
            },
        )
        .unwrap();
        super::cache::template_digest(&self.repo, &compiled.components[0])
            .unwrap()
            .unwrap()
    }
}

/// #1237 acceptance: the first look builds the tip's template (the very one
/// `env up` would look for); a commit outside `template_inputs` builds
/// nothing; one inside builds the next template from the previous one, and
/// `keep` evicts the oldest through its `template_destroy`. A tip already
/// looked at costs nothing.
#[test]
fn a_moved_ref_builds_only_a_changed_key_from_the_previous_template() {
    let mut f = Fixture::new("", 1);
    let mut reports = Reports::default();

    let first = f.run(false, &mut reports);
    assert_eq!(first.result, "built", "{first:?}");
    assert_eq!(f.built().len(), 1);
    assert!(f.built()[0].ends_with("from=none v1"), "{:?}", f.built());
    let digest = f.worktree_digest();
    assert_eq!(first.digest.as_deref(), Some(digest.as_str()));
    assert!(f.lifecycle.template_exists(&f.repository(), "db", &digest));

    let again = f.run(false, &mut reports);
    assert_eq!(again.result, "unchanged");

    f.commit("README.md", Some("changed"));
    let outside = f.run(false, &mut reports);
    assert_eq!(outside.result, "published", "{outside:?}");
    assert_eq!(f.built().len(), 1);

    f.commit("db/schema.sql", Some("v2"));
    let inside = f.run(false, &mut reports);
    assert_eq!(inside.result, "built", "{inside:?}");
    let first_id = &digest[..16];
    assert_eq!(
        f.built()[1],
        format!(
            "{} from={first_id} v2",
            &inside.digest.as_deref().unwrap()[..16]
        )
    );
    assert_eq!(
        inside.run.as_ref().unwrap().derived_from.as_deref(),
        Some(first_id)
    );
    assert_eq!(inside.pruned.len(), 1, "{inside:?}");
    let destroyed =
        std::fs::read_to_string(f.host.parent().unwrap().join("destroyed.log")).unwrap();
    assert_eq!(destroyed.trim(), first_id);
    assert!(!f.lifecycle.template_exists(&f.repository(), "db", &digest));
    assert!(reports.0.is_empty());
    let timings = f.lifecycle.store.timings(&f.repository()).unwrap();
    assert!(!timings.is_empty());
}

/// A failed build keeps the previous template, reports the failure with the
/// commits since the last good build, and is not retried on the same digest
/// until `--run`; the next good build reports the recovery.
#[test]
fn a_failure_is_held_reported_and_recovered() {
    let mut f = Fixture::new("", 2);
    let mut reports = Reports::default();
    let good = f.run(false, &mut reports);
    assert_eq!(good.result, "built");
    let good_commit = good.commit.clone().unwrap();

    f.commit("db/fail", Some("x"));
    let failed = f.run(false, &mut reports);
    assert_eq!(failed.result, "failed", "{failed:?}");
    let tip = failed.commit.clone().unwrap();
    assert_eq!(reports.0.len(), 1);
    assert!(reports.0[0].failed);
    assert_eq!(reports.0[0].source, "prebuild:db");
    assert_eq!(reports.0[0].key, None);
    assert_eq!(
        reports.0[0].commits.as_deref(),
        Some(format!("{good_commit}..{tip}").as_str())
    );
    let old = good.digest.unwrap();
    assert!(f.lifecycle.template_exists(&f.repository(), "db", &old));

    assert_eq!(f.run(false, &mut reports).result, "unchanged");
    let forced = f.run(true, &mut reports);
    assert_eq!(forced.result, "failed");
    assert_eq!(reports.0.len(), 2);

    f.commit("db/fail", None);
    let recovered = f.run(false, &mut reports);
    // Removing the failing file returns to the first digest: published.
    assert_eq!(recovered.result, "published", "{recovered:?}");
    assert_eq!(reports.0.len(), 3);
    assert!(!reports.0[2].failed);
    let after = f.run(false, &mut reports);
    assert_eq!(after.result, "unchanged");
    assert_eq!(reports.0.len(), 3);
}

/// A closed gate waits, keeps the supervisor up, and builds nothing until
/// `--run`.
#[test]
fn a_closed_window_waits_and_keeps_the_supervisor_up() {
    let now = super::activity::now_ms();
    let start = (local_minutes(now) + 120) % (24 * 60);
    let end = (start + 60) % (24 * 60);
    let when = format!(
        "\"inside:{:02}:{:02}-{:02}:{:02}\"",
        start / 60,
        start % 60,
        end / 60,
        end % 60
    );
    let mut f = Fixture::new(&when, 2);
    let mut reports = Reports::default();
    let waiting = f.run(false, &mut reports);
    assert_eq!(waiting.result, "waiting", "{waiting:?}");
    assert!(f.built().is_empty());
    assert!(any_pending(&f.lifecycle));
    let still = f.run(false, &mut reports);
    assert_eq!(still.result, "waiting");
    let forced = f.run(true, &mut reports);
    assert_eq!(forced.result, "built");
    assert!(!any_pending(&f.lifecycle));
    let states = f.lifecycle.store.prebuild_states().unwrap();
    let state: PrebuildState = serde_json::from_str(&states[0].3).unwrap();
    assert_eq!(state.status, Status::Ready);
    assert_eq!(state.runs.len(), 1);
}

/// Declarations are validated: an unknown component, one without a
/// template, a bad condition, `keep = 0` and a duplicate name.
#[test]
fn prebuild_entries_are_validated() {
    let text = r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "web"
runtime = "hooks"
provision = "true"
destroy = "true"

[[prebuild]]
name = "a"
profile = "p"
component = "web"
when = ["sometimes"]
keep = 0

[[prebuild]]
name = "a"
profile = "p"
component = "nope"
"#;
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
    let file = loaded.value.unwrap();
    let found = super::validate(&file, &HostEnvironmentsFile::default(), "t", "h");
    let codes: Vec<&str> = found.iter().map(|d| d.code).collect();
    for code in [
        "prebuild_without_artifact",
        "invalid_prebuild",
        "duplicate_prebuild",
        "unknown_component",
    ] {
        assert!(codes.contains(&code), "{code} missing from {codes:?}");
    }
    assert_eq!(
        codes.iter().filter(|c| **c == "invalid_prebuild").count(),
        2,
        "{codes:?}"
    );
}

#[test]
fn the_memory_gate_waits_only_on_a_reading_over_the_cap() {
    let mut capped = spec(&[]);
    capped.max_memory_pct = Some(70);
    assert_eq!(super::prebuild::memory_gate(&capped, Some(70)), Ok(()));
    let closed = super::prebuild::memory_gate(&capped, Some(71)).unwrap_err();
    assert!(closed.contains("70%") && closed.contains("71%"), "{closed}");
    assert_eq!(super::prebuild::memory_gate(&capped, None), Ok(()));
    assert_eq!(super::prebuild::memory_gate(&spec(&[]), Some(99)), Ok(()));
    if let Some(used) = super::prebuild::host_memory_used_pct() {
        assert!(used <= 100);
    }
}

/// #1366: a cacheable build prebuilds like a template. The output published
/// from the prebuild's checkout is the entry `env up` would restore, and a
/// commit outside `cache_inputs` builds nothing.
#[test]
fn a_cacheable_build_is_prebuilt_from_the_checkout() {
    let mut f = Fixture::custom(|log, _| {
        format!(
            r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "site"
runtime = "static"
serve_dir = "dist"
build = 'mkdir -p dist && cp web/app.txt dist/index.html && echo built >> "{log}"'
cache_inputs = ["web/**"]

[[prebuild]]
name = "site"
profile = "p"
component = "site"
"#,
            log = log.display()
        )
    });
    let mut reports = Reports::default();
    let first = f.run(false, &mut reports);
    assert_eq!(first.result, "built", "{first:?}");
    assert_eq!(f.built().len(), 1);
    let digest = first.digest.clone().unwrap();
    assert!(f.lifecycle.build_exists(&f.repository(), "site", &digest));
    let text = std::fs::read_to_string(f.repo.join(ENVIRONMENTS_FILE)).unwrap();
    let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", &text);
    let file = loaded.value.unwrap();
    let fingerprint = crate::console::worktree_fingerprint(&f.repo);
    let compiled = compile(
        &file,
        &HostEnvironmentsFile::default(),
        "p",
        "t",
        &CompileContext {
            repository: f.repository(),
            owner: EnvironmentOwner::Worktree {
                fingerprint: fingerprint.clone(),
            },
            worktree_fingerprint: fingerprint,
            source_commit: None,
            source_dirty: false,
            source_digest: String::new(),
        },
    )
    .unwrap();
    assert_eq!(
        super::cache::build_digest(&f.repo, &compiled.components[0])
            .unwrap()
            .as_deref(),
        Some(digest.as_str())
    );
    f.commit("README.md", Some("moved"));
    assert_eq!(f.run(false, &mut reports).result, "published");
    f.commit("web/app.txt", Some("app v2"));
    assert_eq!(f.run(false, &mut reports).result, "built");
    assert_eq!(f.built().len(), 2);
}

/// #1366: a preemptible prebuild that waited for host_idle stops as soon as
/// an environment becomes active, records no failure and waits for the next
/// window; nothing is published.
#[test]
fn a_preemptible_prebuild_yields_to_an_environment_becoming_active() {
    let mut f = Fixture::custom(|log, destroyed| {
        format!(
            r#"
schema = 1
[[profiles]]
name = "p"
[[profiles.components]]
name = "db"
runtime = "hooks"
provision = "true"
destroy = "true"
template = 'echo started >> "{log}"; sleep 30'
template_destroy = 'echo "$AETHYME_ENV_TEMPLATE_ID" >> "{destroyed}"'
template_inputs = ["db/**"]

[[prebuild]]
name = "db"
profile = "p"
component = "db"
when = ["host_idle"]
preemptible = true
"#,
            log = log.display(),
            destroyed = destroyed.display(),
        )
    });
    let database = f.host.join("environments.db");
    let log = f.log.clone();
    let toucher = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !log.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the build never started"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut store = EnvironmentStore::open(&database).unwrap();
        let text = "schema = 1\n[[profiles]]\nname = \"x\"\n[[profiles.components]]\nname = \"c\"\nruntime = \"hooks\"\nprovision = \"true\"\ndestroy = \"true\"\n";
        let loaded: Loaded<EnvironmentsFile> = parse_loaded(PathBuf::from("t"), "t", text);
        let file = loaded.value.unwrap();
        let other = compile(
            &file,
            &HostEnvironmentsFile::default(),
            "x",
            "t",
            &CompileContext {
                repository: "elsewhere".into(),
                owner: EnvironmentOwner::Worktree {
                    fingerprint: "other".into(),
                },
                worktree_fingerprint: "other".into(),
                source_commit: None,
                source_dirty: false,
                source_digest: String::new(),
            },
        )
        .unwrap();
        let record = store.register(&other, None, None).unwrap();
        store
            .record_activity(&record.env_id, super::activity::now_ms() + 1_000)
            .unwrap();
    });
    let mut reports = Reports::default();
    let clock = std::time::Instant::now();
    let report = f.run(false, &mut reports);
    toucher.join().unwrap();
    assert_eq!(report.result, "preempted", "{report:?}");
    assert!(
        clock.elapsed() < Duration::from_secs(20),
        "the hook was not stopped"
    );
    assert!(reports.0.is_empty());
    let states = f.lifecycle.store.prebuild_states().unwrap();
    let state: PrebuildState = serde_json::from_str(&states[0].3).unwrap();
    assert_eq!(state.status, Status::Waiting);
    assert_eq!(state.failed_digest, None);
    assert!(
        !f.lifecycle
            .template_exists(&f.repository(), "db", report.digest.as_deref().unwrap())
    );
}

/// `run_hook_until` stops the hook's process group when asked and says why.
#[test]
fn a_stopped_hook_is_preempted_not_failed() {
    let temp = tempfile::tempdir().unwrap();
    let mut polls = 0;
    let clock = std::time::Instant::now();
    let result = super::exec::run_hook_until(
        "t: template",
        "sleep 30",
        temp.path(),
        &[],
        &Default::default(),
        &temp.path().join("log"),
        Duration::from_secs(60),
        &mut || {
            polls += 1;
            (polls >= 2).then(|| "yield".to_string())
        },
    );
    match result {
        Err(super::exec::DriverError::Preempted { reason, .. }) => assert_eq!(reason, "yield"),
        other => panic!("expected a preempted hook, got {other:?}"),
    }
    assert!(clock.elapsed() < Duration::from_secs(10));
}
