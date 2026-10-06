//! A verified terminal finish releases the session's leases, auditably and
//! before physical cleanup; a refused finish or a running gate keeps them
//! (#358).

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output};

use aethyme_broker::BrokerStore;

mod common;

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const PATH: &str = "src/owned.rs";

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

/// A process that stays alive until dropped: a stand-in gate leader.
struct Running(Child);

impl Running {
    fn spawn() -> Self {
        Self(Command::new("sleep").arg("600").spawn().unwrap())
    }

    fn stop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    host: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join(PATH), "fn main() {}\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    let host = tmp.path().join("host");
    std::fs::create_dir_all(&host).unwrap();
    Fixture {
        repo: repo.canonicalize().unwrap(),
        host,
        _tmp: tmp,
    }
}

impl Fixture {
    fn run(&self, args: &[&str]) -> Output {
        common::broker_cli(CLI, args)
            .current_dir(&self.repo)
            .env("AETHYME_HOST_STATE_DIR", &self.host)
            .env("AETHYME_AGENT_PID", "0")
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn session_claiming(&self, task: &str, paths: &[&str]) -> i64 {
        let session = self.json(&["start", "--task", task, "--json"])["id"]
            .as_i64()
            .unwrap();
        for path in paths {
            self.json(&[
                "advanced",
                "leases",
                "claim",
                path,
                "--session",
                &session.to_string(),
                "--json",
            ]);
        }
        session
    }

    fn finish(&self, session: i64) -> serde_json::Value {
        self.finish_with(session, &[])
    }

    fn finish_with(&self, session: i64, extra: &[&str]) -> serde_json::Value {
        let session = session.to_string();
        let mut args = vec!["finish", "--session", &session, "--json"];
        args.extend_from_slice(extra);
        let output = self.run(&args);
        serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "finish --json: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            )
        })
    }

    /// Explicit lease paths `session` still holds.
    fn active_paths(&self, session: i64) -> Vec<String> {
        let listed = self.json(&["advanced", "leases", "--json"]);
        listed["leases"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|lease| lease["session_id"] == session && lease["kind"] == "explicit")
            .map(|lease| lease["path"].as_str().unwrap().to_string())
            .collect()
    }

    /// `lease.released` payloads the finish recorded for `session`.
    fn finish_releases(&self, session: i64) -> Vec<serde_json::Value> {
        let store = BrokerStore::open_in_repo(&self.repo).unwrap();
        store
            .events_after(0, i64::MAX)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == "lease.released" && event.session_id == Some(session))
            .filter_map(|event| serde_json::from_str(event.payload_json.as_deref()?).ok())
            .filter(|payload: &serde_json::Value| payload["reason"] == "finish")
            .collect()
    }

    fn worktree(&self, session: i64) -> PathBuf {
        let store = BrokerStore::open_in_repo(&self.repo).unwrap();
        PathBuf::from(store.session(session).unwrap().worktree_path)
    }
}

#[test]
fn a_verified_finish_releases_every_lease_with_its_generation_on_record() {
    let fx = fixture();
    let session = fx.session_claiming("done", &[PATH, "docs/"]);
    let store = BrokerStore::open_in_repo(&fx.repo).unwrap();
    let held: Vec<(i64, i64, String)> = store
        .active_leases()
        .unwrap()
        .into_iter()
        .filter(|lease| lease.session_id == session && lease.kind.as_str() == "explicit")
        .map(|lease| (lease.id, lease.created_at, lease.path))
        .collect();
    assert_eq!(held.len(), 2);

    // Kept, so no physical cleanup runs: the release is the close's own.
    let report = fx.finish_with(session, &["--keep-worktree"]);
    assert!(report["closed"].as_bool().unwrap(), "{report}");
    assert_eq!(report["cleanup"]["completed"], false, "{report}");
    assert!(fx.active_paths(session).is_empty());

    let released = fx.finish_releases(session);
    for (id, created_at, path) in &held {
        assert!(
            released.iter().any(|payload| payload["lease_id"] == *id
                && payload["created_at"] == *created_at
                && payload["path"] == path.as_str()),
            "no finish release recorded for lease {id} ({path}): {released:?}"
        );
    }
    for lease in report["leases_held"].as_array().unwrap() {
        if lease["kind"] == "explicit" {
            assert_eq!(lease["state"], "released", "{report}");
        }
    }
}

#[test]
fn a_refused_finish_keeps_every_lease() {
    let fx = fixture();
    let session = fx.session_claiming("unfinished", &[PATH]);
    std::fs::write(fx.worktree(session).join(PATH), "fn main() { todo!() }\n").unwrap();

    let report = fx.finish(session);
    assert_eq!(report["closed"], false, "{report}");
    assert_eq!(fx.active_paths(session), [PATH]);
    assert!(fx.finish_releases(session).is_empty());
}

#[test]
fn a_running_gate_keeps_the_leases_until_it_ends() {
    let fx = fixture();
    let session = fx.session_claiming("gated", &[PATH]);
    let mut gate = Running::spawn();
    let run_dir = fx.repo.join(".aethyme/run/gates");
    std::fs::create_dir_all(&run_dir).unwrap();
    let pid = gate.0.id();
    let pidfile = run_dir.join(format!("{session}-tests.pid"));
    std::fs::write(&pidfile, format!("{pid} deadbeef {pid}\n")).unwrap();

    let refused = fx.finish(session);
    assert_eq!(refused["closed"], false, "{refused}");
    assert!(
        refused["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning
                .as_str()
                .unwrap()
                .contains("gate tests is still running")),
        "{refused}"
    );
    assert_eq!(fx.active_paths(session), [PATH]);
    assert!(fx.finish_releases(session).is_empty());

    gate.stop();
    let finished = fx.finish(session);
    assert!(finished["closed"].as_bool().unwrap(), "{finished}");
    assert!(fx.active_paths(session).is_empty());
    assert_eq!(fx.finish_releases(session).len(), 1);
}

#[test]
fn finishing_an_older_holder_never_releases_a_newer_holders_lease() {
    let fx = fixture();
    // Verify-only lets two sessions hold the same path, so each holds its own
    // generation of the lease.
    std::fs::create_dir_all(fx.repo.join(".aethyme")).unwrap();
    std::fs::write(
        fx.repo.join(".aethyme/config.toml"),
        "schema = 1\n[promote]\nmode = \"verify-only\"\n",
    )
    .unwrap();
    let older = fx.session_claiming("older", &[PATH]);
    let newer = fx.session_claiming("newer", &[PATH]);
    let store = BrokerStore::open_in_repo(&fx.repo).unwrap();
    let newer_lease = store
        .active_leases()
        .unwrap()
        .into_iter()
        .find(|lease| lease.session_id == newer && lease.path == PATH)
        .unwrap();

    let report = fx.finish(older);
    assert!(report["closed"].as_bool().unwrap(), "{report}");
    assert_eq!(fx.active_paths(newer), [PATH]);
    assert!(
        fx.finish_releases(older)
            .iter()
            .all(|payload| payload["lease_id"] != newer_lease.id),
        "the older session's finish released the newer generation"
    );
    assert!(fx.finish_releases(newer).is_empty());
}

#[test]
fn an_unidentified_holder_keeps_its_lease_until_a_verified_finish() {
    // `run` passes AETHYME_AGENT_PID=0: no holder is ever recorded, as for a
    // caller with no agent ancestor in CI.
    let fx = fixture();
    let session = fx.session_claiming("unbound", &[PATH]);
    let explained = fx.json(&["advanced", "leases", "explain", PATH, "--json"]);
    let lease = &explained["leases"][0];
    assert_eq!(lease["liveness"], "unknown", "{explained}");
    assert_eq!(
        lease["liveness_evidence"]["holds"], true,
        "unknown is never dead"
    );

    std::fs::write(fx.worktree(session).join(PATH), "fn main() { todo!() }\n").unwrap();
    assert_eq!(fx.finish(session)["closed"], false);
    assert_eq!(fx.active_paths(session), [PATH]);

    std::fs::write(fx.worktree(session).join(PATH), "fn main() {}\n").unwrap();
    let report = fx.finish(session);
    assert!(report["closed"].as_bool().unwrap(), "{report}");
    assert!(fx.active_paths(session).is_empty());
    // The edit also left an implicit lease on the path; both are released.
    let released = fx.finish_releases(session);
    assert!(
        !released.is_empty() && released.iter().all(|payload| payload["path"] == PATH),
        "{released:?}"
    );
}
