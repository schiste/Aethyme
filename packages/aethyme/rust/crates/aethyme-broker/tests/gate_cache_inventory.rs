//! `gc` must see this repository's gate cache and reclaim only what no gate
//! uses, and must sweep build output only from finished sessions (#295).
//!
//! The gate cache lives under the per-user cache directory rather than beside
//! any worktree, so every byte total `gc plan` produced was rooted somewhere
//! that could not see it. On the host that motivated this, `gc plan` reported
//! `0 build caches, 209.3 KiB reclaimable` while 7.7 GiB sat in the gate cache
//! -- and the gate's own low-headroom refusal pointed the operator at `gc plan`.
//!
//! One fixture holds a fake gate cache tree (this repository's entries plus
//! another repository's), a finished session and a live session, each with
//! build output. Every case runs the CLI in its own process with private host
//! cache and state directories, so nothing here reads or writes the machine's.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, SystemTime};

use aethyme_broker::{Broker, FinishOptions};

const CLI: &str = env!("CARGO_BIN_EXE_broker-cli-shim");
const DAY: Duration = Duration::from_secs(86_400);

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

/// A directory holding one file of exactly `bytes`, so sizes are predictable.
fn filled(dir: &Path, bytes: usize) {
    std::fs::create_dir_all(dir.join("debug")).unwrap();
    std::fs::write(dir.join("debug/blob"), vec![b'x'; bytes]).unwrap();
}

/// A cargo-shaped build directory: the `CACHEDIR.TAG` witness plus `bytes`.
fn build_dir(dir: &Path, bytes: usize) {
    filled(dir, bytes);
    std::fs::write(dir.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
}

/// Backdate a directory and the two levels beneath it, deepest first, which
/// is everything the inventory reads to decide when an entry was last used.
fn age(dir: &Path, ago: Duration) {
    let when = SystemTime::now() - ago;
    let mut paths = vec![dir.to_path_buf()];
    for child in std::fs::read_dir(dir).unwrap().flatten() {
        if child.path().is_dir() {
            for grandchild in std::fs::read_dir(child.path()).unwrap().flatten() {
                paths.push(grandchild.path());
            }
        }
        paths.push(child.path());
    }
    for path in paths.iter().rev() {
        std::fs::File::open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }
}

fn size(path: &Path) -> u64 {
    let metadata = std::fs::symlink_metadata(path).unwrap();
    if metadata.is_file() {
        return metadata.len();
    }
    std::fs::read_dir(path)
        .unwrap()
        .flatten()
        .map(|entry| size(&entry.path()))
        .sum()
}

/// A pid that belonged to a process which has exited.
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap();
    pid
}

struct World {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    cache: PathBuf,
    state: PathBuf,
    finished: PathBuf,
    live: PathBuf,
    /// `<cache>/gates/<this repository's key>`.
    gates: PathBuf,
    repository_key: String,
    other_repository: PathBuf,
    retired: String,
}

impl World {
    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.repo, args)
    }

    fn run_in(&self, cwd: &Path, args: &[&str]) -> Output {
        Command::new(CLI)
            .args(args)
            .current_dir(cwd)
            .env("AETHYME_HOST_CACHE_DIR", &self.cache)
            .env("AETHYME_HOST_STATE_DIR", &self.state)
            .output()
            .unwrap()
    }

    fn plan(&self) -> serde_json::Value {
        self.plan_with(&[])
    }

    fn plan_with(&self, extra: &[&str]) -> serde_json::Value {
        let mut args = vec!["gc", "plan", "--json"];
        args.extend_from_slice(extra);
        let output = self.run(&args);
        assert!(
            output.status.success(),
            "gc plan: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn apply(&self, digest: &str) -> Output {
        self.run(&["gc", "apply", "--confirm", digest, "--json"])
    }

    fn apply_with(&self, digest: &str, extra: &[&str]) -> Output {
        let mut args = vec!["gc", "apply", "--confirm", digest, "--json"];
        args.extend_from_slice(extra);
        self.run(&args)
    }

    fn registry(&self) -> aethyme_broker::HostResourceCoordinator {
        aethyme_broker::HostResourceCoordinator::open(&self.state.join("host-resources.db"))
            .unwrap()
    }
}

fn candidate_entries(plan: &serde_json::Value) -> Vec<String> {
    plan["gate_caches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|candidate| candidate["entry"].as_str().unwrap().to_string())
        .collect()
}

fn entry<'a>(plan: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    plan["gate_cache"]["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["entry"] == name)
        .unwrap_or_else(|| panic!("no gate cache entry {name}: {:#}", plan["gate_cache"]))
}

/// Gate cache (budget for older entries 2500 bytes):
///
/// | entry | bytes | last used | expected |
/// |---|---|---|---|
/// | `rust-workspace-v3` | 2000 | 1 day | kept (active): newest of its kind |
/// | `rust-workspace-v2` | 3000 | 10 days | older generation beyond budget |
/// | `rust-workspace-v1` | 1000 | 20 days | older, strict LRU behind v2 |
/// | `py-tools-v1` | 800 | 5 days | kept (active): newest of its kind |
/// | `.rust-workspace-v3.retired-…-<dead pid>` | 500 | now | dead rotation |
///
/// plus another repository's 3000-byte entry.
///
/// Sessions: a finished session whose checkout holds an ignored `target/`, a
/// `target/` a nested `.gitignore` un-ignores, and an untracked file; and a
/// live session with its own ignored `target/`.
fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "fixture\n").unwrap();
    std::fs::write(repo.join(".gitignore"), "/.aethyme/\ntarget/\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-qm", "init"]);
    std::fs::create_dir_all(repo.join(".aethyme")).unwrap();
    // A dirty finished checkout is held from whole-worktree removal by age,
    // which is the state whose build output only the build cache lane frees.
    // The autonomous sweep is off so the reviewed plan is the only remover.
    std::fs::write(
        repo.join(".aethyme/broker.toml"),
        "[retention]\nclosed_worktrees_days = 30\nartifact_reclaim_days = 0\n\
         artifact_sweep_budget_ms = 0\ngate_cache_bytes_budget = 2500\n",
    )
    .unwrap();

    let mut broker = Broker::open(&repo).unwrap();
    let finished = broker.start_worktree("finished work", None).unwrap();
    let finished_path = PathBuf::from(&finished.worktree_path);
    std::fs::write(finished_path.join("done.txt"), "done\n").unwrap();
    git(&finished_path, &["add", "done.txt"]);
    git(&finished_path, &["commit", "-qm", "done"]);
    assert!(broker.submit(finished.id).unwrap().promoted);
    assert!(
        broker
            .finish_with_options(
                finished.id,
                FinishOptions {
                    keep_worktree: true
                }
            )
            .unwrap()
            .closed
    );
    build_dir(&finished_path.join("target"), 4096);
    // Named like build output and witnessed like it, but not ignored.
    std::fs::create_dir_all(finished_path.join("keep")).unwrap();
    std::fs::write(finished_path.join("keep/.gitignore"), "!target/\n").unwrap();
    build_dir(&finished_path.join("keep/target"), 1024);
    std::fs::write(finished_path.join("notes.txt"), "not ignored\n").unwrap();

    let live = broker.start_worktree("live work", None).unwrap();
    let live_path = PathBuf::from(&live.worktree_path);
    build_dir(&live_path.join("target"), 2048);
    drop(broker);

    let cache = tmp.path().join("cache");
    let state = tmp.path().join("state");
    let mut world = World {
        _tmp: tmp,
        repo,
        cache,
        state,
        finished: finished_path,
        live: live_path,
        gates: PathBuf::new(),
        repository_key: String::new(),
        other_repository: PathBuf::new(),
        retired: String::new(),
    };
    // The plan says where this repository's gate cache is, which is the same
    // derivation a gate uses; the fixture builds its tree there.
    let empty = world.plan();
    world.gates = PathBuf::from(empty["gate_cache"]["root"].as_str().unwrap());
    world.repository_key = empty["gate_cache"]["repository_key"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(empty["gate_cache"]["total_bytes"], 0);
    assert!(
        world.gates.starts_with(&world.cache),
        "{}",
        world.gates.display()
    );

    for (name, bytes, days) in [
        ("rust-workspace-v3", 2000, 1),
        ("rust-workspace-v2", 3000, 10),
        ("rust-workspace-v1", 1000, 20),
        ("py-tools-v1", 800, 5),
    ] {
        filled(&world.gates.join(name), bytes);
        age(&world.gates.join(name), DAY * days);
    }
    world.retired = format!(".rust-workspace-v3.retired-1700000000000-{}", dead_pid());
    filled(&world.gates.join(&world.retired), 500);
    world.other_repository = world.cache.join("gates/another-repository/cold");
    filled(&world.other_repository, 3000);
    age(&world.other_repository, DAY * 30);
    world
}

fn hold_lease(world: &World, cache_key: &str) -> aethyme_broker::HostResourceCoordinator {
    let mut registry = world.registry();
    registry
        .acquire(&aethyme_broker::HostResourceRequest {
            schema_version: aethyme_broker::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
            request_id: format!("running-gate-{cache_key}"),
            repository: world.repository_key.clone(),
            worktree_fingerprint: "wt".into(),
            run_id: "gate".into(),
            ttl_seconds: 600,
            holder_pid: Some(std::process::id()),
            resources: vec![aethyme_broker::HostResourceRequirement {
                key: "managed_cache".into(),
                resource: aethyme_broker::HostResourceKind::ExclusiveKey {
                    name: format!("aethyme-gate-cache:{}:{cache_key}", world.repository_key),
                },
            }],
        })
        .unwrap();
    registry
}

/// The defect itself: the bytes were absent, not merely unreclaimable. The
/// plan reports every entry with its size and age, keeps the newest entry of
/// each kind whatever the budget, and applies the budget to older entries,
/// least recently used first.
#[test]
fn plan_keeps_the_active_cache_of_each_kind_and_budgets_older_entries() {
    let world = world();
    let plan = world.plan();
    let cache = &plan["gate_cache"];

    assert_eq!(cache["total_bytes"], 7300, "{cache:#}");
    assert_eq!(cache["budget_bytes"], 2500);
    assert_eq!(cache["entries"].as_array().unwrap().len(), 5, "{cache:#}");
    assert_eq!(entry(&plan, "rust-workspace-v3")["disposition"], "active");
    assert_eq!(entry(&plan, "py-tools-v1")["disposition"], "active");
    assert_eq!(cache["active_bytes"], 2800);
    assert_eq!(
        entry(&plan, "rust-workspace-v2")["disposition"],
        "reclaimable"
    );
    assert_eq!(entry(&plan, "rust-workspace-v2")["age_days"], 10);
    assert_eq!(entry(&plan, "rust-workspace-v2")["estimated_bytes"], 3000);
    assert_eq!(
        entry(&plan, "rust-workspace-v1")["disposition"],
        "reclaimable"
    );
    assert_eq!(entry(&plan, &world.retired)["disposition"], "reclaimable");

    // Dead rotations first, then least recently used first.
    assert_eq!(
        candidate_entries(&plan),
        vec![
            world.retired.clone(),
            "rust-workspace-v1".to_string(),
            "rust-workspace-v2".to_string()
        ]
    );
    assert_eq!(cache["reclaimable_bytes"], 4500);
    let artifact_bytes: u64 = plan["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|artifact| artifact["estimated_bytes"].as_u64().unwrap())
        .sum();
    assert_eq!(
        plan["estimated_build_output_reclaimable_bytes"]
            .as_u64()
            .unwrap(),
        artifact_bytes + 4500,
        "the figure a refused gate needs must add both lanes"
    );

    // The operator sees where the space is, and what taking it would cost.
    let text = world.run(&["gc", "plan"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("kept (active) rust-workspace-v3"), "{text}");
    assert!(text.contains("--include-active-gate-cache"), "{text}");
    assert!(text.contains("rebuild from scratch"), "{text}");
}

/// Only an explicit opt-in proposes the active caches, it says what that
/// costs, and its digest cannot be applied without the same opt-in.
#[test]
fn the_active_cache_is_proposed_only_with_the_explicit_opt_in() {
    let world = world();
    let plan = world.plan_with(&["--include-active-gate-cache"]);
    assert_eq!(plan["gate_cache"]["include_active"], true);
    let v3 = entry(&plan, "rust-workspace-v3");
    assert_eq!(v3["disposition"], "reclaimable", "{v3:#}");
    assert!(
        v3["reason"]
            .as_str()
            .unwrap()
            .contains("rebuild it from scratch"),
        "{v3:#}"
    );
    assert_eq!(candidate_entries(&plan).len(), 5);
    let text = world.run(&["gc", "plan", "--include-active-gate-cache"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(
        text.contains("next gate will rebuild from scratch"),
        "{text}"
    );
    assert!(
        text.contains("apply --confirm") && text.contains(" --include-active-gate-cache"),
        "{text}"
    );

    // Without the opt-in the same digest is refused, and nothing moves.
    let digest = plan["digest"].as_str().unwrap();
    let refused = world.apply(digest);
    assert!(!refused.status.success());
    assert_eq!(size(&world.gates.join("rust-workspace-v3")), 2000);

    let output = world.apply_with(digest, &["--include-active-gate-cache"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(std::fs::read_dir(&world.gates).unwrap().count(), 0);
}

/// (b) Another repository's gate cache belongs to its own broker: it is not
/// inventoried, not proposed, and survives an apply.
#[test]
fn another_repositorys_gate_cache_is_never_seen_or_proposed() {
    let world = world();
    let plan = world.plan_with(&["--include-active-gate-cache"]);
    let serialized = plan["gate_cache"].to_string() + &plan["gate_caches"].to_string();
    assert!(
        !serialized.contains("another-repository"),
        "another repository's cache leaked into this plan: {serialized}"
    );
    assert!(
        plan["gate_cache"]["root"]
            .as_str()
            .unwrap()
            .ends_with(&world.repository_key)
    );

    let output = world.apply_with(
        plan["digest"].as_str().unwrap(),
        &["--include-active-gate-cache"],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(size(&world.other_repository), 3000);
}

/// (a) A running gate holds its cache's lease. That entry is reported as held
/// with the lease named and never proposed -- not even with the opt-in.
///
/// Fairness: held bytes sit outside the budget. Under a rule that charged
/// them to it, the held 3000-byte `-v2` would exhaust the 2500-byte budget
/// and push the idle 1000-byte `-v1` out; here `-v1` is kept, exactly as if
/// the gate were not running and `-v2` had been removed.
#[test]
fn an_entry_whose_lease_a_live_gate_holds_is_never_proposed_nor_charged_to_the_budget() {
    let world = world();
    let _gate = hold_lease(&world, "rust-workspace-v2");

    let plan = world.plan_with(&["--include-active-gate-cache"]);
    let held = entry(&plan, "rust-workspace-v2");
    assert_eq!(held["disposition"], "held", "{held:#}");
    assert!(
        held["reason"].as_str().unwrap().contains("lease"),
        "{held:#}"
    );
    assert!(!candidate_entries(&plan).contains(&"rust-workspace-v2".to_string()));

    let plan = world.plan();
    assert_eq!(plan["gate_cache"]["held_bytes"], 3000);
    assert_eq!(
        entry(&plan, "rust-workspace-v1")["disposition"],
        "within_budget"
    );

    // Applying what *is* proposed leaves the held entry exactly as it was.
    let output = world.apply(plan["digest"].as_str().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(size(&world.gates.join("rust-workspace-v2")), 3000);
}

/// (a) A live gate pidfile or a held gate owner lock means some gate of this
/// repository is running, and nothing says which cache it uses: every entry
/// is held until it finishes.
#[test]
fn a_live_gate_pidfile_or_owner_lock_holds_every_entry() {
    let world = world();
    let run_dir = world.repo.join(".aethyme/run/gates");
    std::fs::create_dir_all(run_dir.join("owners")).unwrap();

    let pid = std::process::id();
    let pidfile = run_dir.join("42-rust.pid");
    std::fs::write(&pidfile, format!("{pid} tree {pid} -")).unwrap();
    let plan = world.plan_with(&["--include-active-gate-cache"]);
    assert!(
        candidate_entries(&plan).is_empty(),
        "{:#}",
        plan["gate_caches"]
    );
    assert!(
        plan["gate_cache"]["holders"][0]
            .as_str()
            .unwrap()
            .contains("42-rust.pid"),
        "{:#}",
        plan["gate_cache"]
    );
    std::fs::remove_file(&pidfile).unwrap();

    let lock = std::fs::File::create(run_dir.join("owners/rust-all-0000.lock")).unwrap();
    lock.lock().unwrap();
    let plan = world.plan_with(&["--include-active-gate-cache"]);
    assert!(
        candidate_entries(&plan).is_empty(),
        "{:#}",
        plan["gate_caches"]
    );
    assert_eq!(entry(&plan, "rust-workspace-v2")["disposition"], "held");
    lock.unlock().unwrap();

    // Once the gate is gone the same entries are proposed again.
    assert_eq!(candidate_entries(&world.plan()).len(), 3);
}

/// (c) and (d): only the finished session's ignored, witnessed build output
/// is proposed. The live session's `target/`, a `target/` that is not
/// ignored, and an untracked file are never candidates -- and survive apply.
#[test]
fn only_a_finished_sessions_ignored_build_output_is_proposed() {
    let world = world();
    let plan = world.plan();
    let artifacts = plan["artifacts"].as_array().unwrap();
    assert_eq!(artifacts.len(), 1, "{artifacts:#?}");
    assert_eq!(artifacts[0]["relative_dir"], "target");
    assert_eq!(
        Path::new(artifacts[0]["worktree_path"].as_str().unwrap())
            .canonicalize()
            .unwrap(),
        world.finished.canonicalize().unwrap()
    );
    assert_eq!(
        artifacts[0]["estimated_bytes"],
        size(&world.finished.join("target"))
    );

    let output = world.apply(plan["digest"].as_str().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!world.finished.join("target").exists());
    assert!(world.live.join("target/CACHEDIR.TAG").is_file());
    assert!(world.finished.join("keep/target/CACHEDIR.TAG").is_file());
    assert!(world.finished.join("notes.txt").is_file());
    assert!(world.finished.join("done.txt").is_file());
}

/// (c) for a reused directory: a new session adopting the finished session's
/// checkout makes its build output live again. The finished row still names
/// the directory, but neither plan nor apply may touch it.
#[test]
fn a_finished_checkout_a_live_session_adopted_is_never_swept() {
    let world = world();
    let stale = world.plan();
    assert_eq!(stale["artifacts"].as_array().unwrap().len(), 1);

    let adopted = world.run_in(&world.finished, &["adopt", "--task", "resume the work"]);
    assert!(
        adopted.status.success(),
        "adopt: {}",
        String::from_utf8_lossy(&adopted.stderr)
    );

    let plan = world.plan();
    assert!(
        plan["artifacts"].as_array().unwrap().is_empty(),
        "an adopted checkout's build output is live: {:#}",
        plan["artifacts"]
    );
    // Nor may the plan reviewed before the adoption reach it.
    let output = world.apply(stale["digest"].as_str().unwrap());
    assert!(!output.status.success(), "the stale digest must be refused");
    assert!(world.finished.join("target/CACHEDIR.TAG").is_file());
    let output = world.apply(plan["digest"].as_str().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(world.finished.join("target/CACHEDIR.TAG").is_file());
}

/// (e) A digest is an authorization for one exact set. A cache that was used
/// after the plan invalidates it, and the refusal removes nothing.
#[test]
fn apply_refuses_a_stale_digest_and_removes_nothing() {
    let world = world();
    let stale = world.plan();
    // A gate ran: the older entry grew and is recent now.
    std::fs::write(
        world.gates.join("rust-workspace-v2/debug/new"),
        vec![b'y'; 100],
    )
    .unwrap();

    let output = world.apply(stale["digest"].as_str().unwrap());
    assert!(!output.status.success(), "a stale digest must be refused");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no longer matches"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(size(&world.gates.join("rust-workspace-v2")), 3100);
    assert!(world.gates.join("rust-workspace-v1").is_dir());
    assert!(world.gates.join(&world.retired).is_dir());
    assert!(world.finished.join("target/CACHEDIR.TAG").is_file());
}

/// (f) What the plan reports is what the apply removes: per candidate, in
/// total, and as the reclaimed figure the apply reports.
#[test]
fn reported_sizes_match_what_apply_deletes() {
    let world = world();
    let plan = world.plan();
    let mut expected = 0_u64;
    for candidate in plan["gate_caches"].as_array().unwrap() {
        let path = PathBuf::from(candidate["path"].as_str().unwrap());
        assert_eq!(candidate["estimated_bytes"].as_u64().unwrap(), size(&path));
        expected += size(&path);
    }
    for artifact in plan["artifacts"].as_array().unwrap() {
        let path = Path::new(artifact["worktree_path"].as_str().unwrap())
            .join(artifact["relative_dir"].as_str().unwrap());
        assert_eq!(artifact["estimated_bytes"].as_u64().unwrap(), size(&path));
        expected += size(&path);
    }
    assert_eq!(
        plan["estimated_build_output_reclaimable_bytes"]
            .as_u64()
            .unwrap(),
        expected
    );

    let output = world.apply(plan["digest"].as_str().unwrap());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(report["complete"].as_bool().unwrap(), "{report:#}");
    assert!(
        report["failures"].as_array().unwrap().is_empty(),
        "{report:#}"
    );
    // Rows and gate logs are not build output; this fixture has none of age.
    assert_eq!(report["rows_removed"], 0, "{report:#}");
    assert_eq!(
        report["reclaimed_bytes"].as_u64().unwrap(),
        expected,
        "{report:#}"
    );
    for path in report["gate_caches_reclaimed"].as_array().unwrap() {
        assert!(!Path::new(path.as_str().unwrap()).exists());
    }
    assert_eq!(report["gate_caches_reclaimed"].as_array().unwrap().len(), 3);
    // The active caches are kept, and nothing is left renamed aside.
    assert_eq!(size(&world.gates.join("rust-workspace-v3")), 2000);
    assert_eq!(size(&world.gates.join("py-tools-v1")), 800);
    assert_eq!(std::fs::read_dir(&world.gates).unwrap().count(), 2);
}
