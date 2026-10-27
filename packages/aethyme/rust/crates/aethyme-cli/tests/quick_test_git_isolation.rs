#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use aethyme_testkit::aethyme_bin;
use serde_json::Value;

const MAX_CHILD_OUTPUT_BYTES: u64 = 1024 * 1024;
const MAX_SENTINEL_ENTRIES: usize = 500;
const MAX_SENTINEL_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_CHANGED_SNAPSHOT_PATHS: usize = 24;
const MAX_CHANGED_SNAPSHOT_PATH_CHARS: usize = 120;

#[derive(Debug, PartialEq, Eq)]
struct SnapshotEntry {
    path: PathBuf,
    kind: u8,
    mode: u32,
    bytes: Vec<u8>,
}

fn is_git_environment_key(key: &OsStr) -> bool {
    key.to_string_lossy()
        .to_ascii_uppercase()
        .starts_with("GIT_")
}

fn clear_git_environment(command: &mut Command) {
    for (key, _) in std::env::vars_os() {
        if is_git_environment_key(&key) {
            command.env_remove(key);
        }
    }
}

fn run_fixture_git(repo: &Path, global_config: &Path, args: &[&OsStr]) {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    // Setup must not leave Git's own background writer racing the exact
    // before/after sentinel snapshot. Keep every snapshot path protected.
    command
        .args(["-c", "maintenance.auto=false", "-c", "gc.auto=0"])
        .args(args)
        .current_dir(repo)
        .env("HOME", global_config.parent().unwrap())
        .env("GIT_CONFIG_GLOBAL", global_config)
        .env("GIT_CONFIG_NOSYSTEM", "1");
    let output = command.output().unwrap();
    assert!(output.status.success(), "sentinel repository setup failed");
}

fn snapshot(root: &Path) -> Vec<SnapshotEntry> {
    fn visit(root: &Path, path: &Path, entries: &mut Vec<SnapshotEntry>) {
        assert!(entries.len() < MAX_SENTINEL_ENTRIES);
        let metadata = fs::symlink_metadata(path).unwrap();
        let mode = metadata.permissions().mode() & 0o7777;
        let relative = path.strip_prefix(root).unwrap().to_path_buf();
        let file_type = metadata.file_type();
        if file_type.is_dir() {
            entries.push(SnapshotEntry {
                path: relative,
                kind: b'd',
                mode,
                bytes: Vec::new(),
            });
            let mut children = fs::read_dir(path)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            children.sort();
            for child in children {
                visit(root, &child, entries);
            }
        } else if file_type.is_file() {
            assert!(metadata.len() <= MAX_SENTINEL_FILE_BYTES);
            entries.push(SnapshotEntry {
                path: relative,
                kind: b'f',
                mode,
                bytes: fs::read(path).unwrap(),
            });
        } else if file_type.is_symlink() {
            entries.push(SnapshotEntry {
                path: relative,
                kind: b'l',
                mode,
                bytes: fs::read_link(path)
                    .unwrap()
                    .as_os_str()
                    .to_string_lossy()
                    .as_bytes()
                    .to_vec(),
            });
        } else {
            panic!("unexpected sentinel file type");
        }
    }

    let mut entries = Vec::new();
    visit(root, root, &mut entries);
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries
}

fn changed_snapshot_paths(before: &[SnapshotEntry], after: &[SnapshotEntry]) -> Vec<String> {
    fn display_path(path: &Path) -> String {
        let display = path.display().to_string();
        if display.chars().count() > MAX_CHANGED_SNAPSHOT_PATH_CHARS {
            format!(
                "{}…",
                display
                    .chars()
                    .take(MAX_CHANGED_SNAPSHOT_PATH_CHARS - 1)
                    .collect::<String>()
            )
        } else {
            display
        }
    }

    fn describe(entry: &SnapshotEntry) -> String {
        format!(
            "kind={} mode={:04o} length={}",
            char::from(entry.kind),
            entry.mode,
            entry.bytes.len()
        )
    }

    fn record_change(paths: &mut Vec<String>, total: &mut usize, description: String) {
        *total += 1;
        if paths.len() < MAX_CHANGED_SNAPSHOT_PATHS {
            paths.push(description);
        }
    }

    let mut paths = Vec::new();
    let mut total = 0;
    let (mut before_index, mut after_index) = (0, 0);
    while before_index < before.len() && after_index < after.len() {
        match before[before_index].path.cmp(&after[after_index].path) {
            std::cmp::Ordering::Less => {
                record_change(
                    &mut paths,
                    &mut total,
                    format!(
                        "removed {} ({})",
                        display_path(&before[before_index].path),
                        describe(&before[before_index])
                    ),
                );
                before_index += 1;
            }
            std::cmp::Ordering::Greater => {
                record_change(
                    &mut paths,
                    &mut total,
                    format!(
                        "added {} ({})",
                        display_path(&after[after_index].path),
                        describe(&after[after_index])
                    ),
                );
                after_index += 1;
            }
            std::cmp::Ordering::Equal => {
                if before[before_index] != after[after_index] {
                    record_change(
                        &mut paths,
                        &mut total,
                        format!(
                            "modified {} ({} -> {})",
                            display_path(&before[before_index].path),
                            describe(&before[before_index]),
                            describe(&after[after_index])
                        ),
                    );
                }
                before_index += 1;
                after_index += 1;
            }
        }
    }
    for entry in &before[before_index..] {
        record_change(
            &mut paths,
            &mut total,
            format!(
                "removed {} ({})",
                display_path(&entry.path),
                describe(entry)
            ),
        );
    }
    for entry in &after[after_index..] {
        record_change(
            &mut paths,
            &mut total,
            format!("added {} ({})", display_path(&entry.path), describe(entry)),
        );
    }
    if total > paths.len() {
        paths.push(format!(
            "... {} additional changed paths",
            total - paths.len()
        ));
    }
    paths
}

fn child_output_size(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn stop_owned_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn run_cli_bounded(mut command: Command, output_dir: &Path) -> Output {
    let stdout_path = output_dir.join("quick-test.stdout");
    let stderr_path = output_dir.join("quick-test.stderr");
    command
        .stdin(Stdio::null())
        .stdout(Stdio::from(File::create(&stdout_path).unwrap()))
        .stderr(Stdio::from(File::create(&stderr_path).unwrap()));
    let mut child = command.spawn().expect("spawn quick-test CLI");
    let deadline = Instant::now() + Duration::from_secs(180);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(_) => {
                stop_owned_child(&mut child);
                panic!("quick-test CLI process status could not be read");
            }
        }
        if Instant::now() >= deadline {
            stop_owned_child(&mut child);
            panic!("quick-test CLI exceeded its test deadline");
        }
        if child_output_size(&stdout_path) > MAX_CHILD_OUTPUT_BYTES
            || child_output_size(&stderr_path) > MAX_CHILD_OUTPUT_BYTES
        {
            stop_owned_child(&mut child);
            panic!("quick-test CLI exceeded its output limit");
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert!(
        child_output_size(&stdout_path) <= MAX_CHILD_OUTPUT_BYTES
            && child_output_size(&stderr_path) <= MAX_CHILD_OUTPUT_BYTES,
        "quick-test CLI exceeded its output limit"
    );
    Output {
        status,
        stdout: fs::read(stdout_path).unwrap(),
        stderr: fs::read(stderr_path).unwrap(),
    }
}

fn git_environment_snapshot() -> Vec<(OsString, OsString)> {
    let mut values = std::env::vars_os()
        .filter(|(key, _)| is_git_environment_key(key))
        .collect::<Vec<_>>();
    values.sort();
    values
}

#[test]
fn quick_test_cli_keeps_host_git_context_out_of_its_disposable_repository() {
    let owner = tempfile::tempdir().unwrap();
    let sentinel = owner.path().join("sentinel");
    fs::create_dir(&sentinel).unwrap();
    let empty_config = owner.path().join("empty-gitconfig");
    fs::write(&empty_config, []).unwrap();

    run_fixture_git(
        &sentinel,
        &empty_config,
        &[
            OsStr::new("init"),
            OsStr::new("--quiet"),
            OsStr::new("--template="),
            OsStr::new("-b"),
            OsStr::new("main"),
        ],
    );
    fs::write(sentinel.join("tracked.txt"), b"committed sentinel\n").unwrap();
    run_fixture_git(
        &sentinel,
        &empty_config,
        &[OsStr::new("add"), OsStr::new("--all")],
    );
    run_fixture_git(
        &sentinel,
        &empty_config,
        &[
            OsStr::new("-c"),
            OsStr::new("user.name=Quick Test Sentinel"),
            OsStr::new("-c"),
            OsStr::new("user.email=quick-test-sentinel@example.invalid"),
            OsStr::new("commit"),
            OsStr::new("--quiet"),
            OsStr::new("-m"),
            OsStr::new("sentinel commit"),
        ],
    );
    fs::write(
        sentinel.join("tracked.txt"),
        b"uncommitted sentinel bytes\n",
    )
    .unwrap();

    let hooks = sentinel.join(".git/hooks");
    fs::create_dir_all(&hooks).unwrap();
    let hook_marker = owner.path().join("sentinel-hook-ran");
    let post_commit = hooks.join("post-commit");
    fs::write(
        &post_commit,
        "#!/bin/sh\nprintf 'invoked\\n' > \"$AETHYME_QUICK_TEST_HOOK_MARKER\"\n",
    )
    .unwrap();
    fs::set_permissions(&post_commit, fs::Permissions::from_mode(0o700)).unwrap();

    let hostile_config = owner.path().join("hostile-gitconfig");
    fs::write(&hostile_config, []).unwrap();
    run_fixture_git(
        &sentinel,
        &empty_config,
        &[
            OsStr::new("config"),
            OsStr::new("--file"),
            hostile_config.as_os_str(),
            OsStr::new("core.hooksPath"),
            hooks.as_os_str(),
        ],
    );

    let git_dir = sentinel.join(".git");
    let index = git_dir.join("index");
    let objects = git_dir.join("objects");
    let mut positive_control = Command::new("git");
    clear_git_environment(&mut positive_control);
    // Keep this control commit from leaving Git's own maintenance writer
    // racing the exact before/after sentinel snapshot below.
    positive_control
        .current_dir(&sentinel)
        .args([
            "-c",
            "maintenance.auto=false",
            "-c",
            "gc.auto=0",
            "-c",
            "user.name=Quick Test Sentinel",
            "-c",
            "user.email=quick-test-sentinel@example.invalid",
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "hook positive control",
        ])
        .env("GIT_DIR", &git_dir)
        .env("GIT_COMMON_DIR", &git_dir)
        .env("GIT_WORK_TREE", &sentinel)
        .env("GIT_INDEX_FILE", &index)
        .env("GIT_OBJECT_DIRECTORY", &objects)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects)
        .env("GIT_CONFIG_GLOBAL", &hostile_config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", &hooks)
        .env("AETHYME_QUICK_TEST_HOOK_MARKER", &hook_marker);
    let positive_control_output = positive_control.output().unwrap();
    assert!(
        positive_control_output.status.success() && hook_marker.is_file(),
        "the sentinel hook positive control failed"
    );
    fs::remove_file(&hook_marker).unwrap();

    let before = snapshot(&sentinel);
    let parent_git_environment = git_environment_snapshot();
    let mut command = Command::new(aethyme_bin());
    command
        .args(["broker", "advanced", "quick-test", "--json"])
        .current_dir(&sentinel)
        .env_remove("AETHYME_ROOT")
        .env_remove("AETHYME_REPO")
        .env("AETHYME_CHAU7_MCP_BRIDGE", "/__aethyme_no_chau7_bridge__")
        .env("AETHYME_QUICK_TEST_HOOK_MARKER", &hook_marker)
        .env("AETHYME_GIT_TIMEOUT_SECS", "120")
        .env("AETHYME_MEASURE_OUTPUT", "1");
    clear_git_environment(&mut command);
    command
        .env("GIT_DIR", &git_dir)
        .env("GIT_COMMON_DIR", &git_dir)
        .env("GIT_WORK_TREE", &sentinel)
        .env("GIT_INDEX_FILE", &index)
        .env("GIT_OBJECT_DIRECTORY", &objects)
        .env("GIT_ALTERNATE_OBJECT_DIRECTORIES", &objects)
        .env("GIT_CONFIG_GLOBAL", &hostile_config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", &hooks);
    let output = run_cli_bounded(command, owner.path());

    assert!(output.status.success(), "quick-test CLI did not succeed");
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("quick-test CLI emitted a JSON report");
    assert_eq!(report["ok"], true, "quick-test report did not pass");
    assert_eq!(
        report["temp_repo_removed"], true,
        "quick-test did not remove its disposable repository"
    );
    assert!(
        parent_git_environment == git_environment_snapshot(),
        "quick-test CLI changed the parent process Git environment"
    );
    let after = snapshot(&sentinel);
    assert!(
        before == after,
        "quick-test CLI changed the unrelated sentinel repository; changed paths: {:?}",
        changed_snapshot_paths(&before, &after)
    );
    assert!(
        !hook_marker.exists(),
        "quick-test CLI ran a hook from the unrelated repository"
    );
}
