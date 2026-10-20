//! Real Cargo builds of tiny synthetic packages using the production scripts.
//! No product binary or real-repository mutation is needed for this regression.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, UNIX_EPOCH};

const SOURCE_ROOT_ENV: &str = "AETHYME_BUILD_SOURCE_ROOT";
const FIXTURE_CARGO_CONFIG: &str =
    "[env]\nAETHYME_BUILD_SOURCE_ROOT = { value = \".\", relative = true, force = true }\n";
fn checked(command: &mut Command) -> String {
    let output: Output = command.output().expect("spawn fixture command");
    assert!(
        output.status.success(),
        "command {command:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("fixture command UTF-8")
}

fn clear_git_environment(command: &mut Command) {
    const BUILD_IDENTITY_VARIABLES: [&str; 6] = [
        "AETHYME_GIT_COMMIT",
        "AETHYME_GIT_DESCRIBE",
        "AETHYME_GIT_REVISION",
        "AETHYME_BROKER_GIT_COMMIT",
        "AETHYME_BROKER_GIT_DESCRIBE",
        "AETHYME_BROKER_GIT_REVISION",
    ];
    let inherited_fixture_variables = std::env::vars_os()
        .filter_map(|(name, _)| {
            let normalized = name.to_string_lossy().to_ascii_uppercase();
            (normalized.starts_with("GIT_")
                || BUILD_IDENTITY_VARIABLES.contains(&normalized.as_str()))
            .then_some(name)
        })
        .collect::<Vec<_>>();
    for name in inherited_fixture_variables {
        command.env_remove(name);
    }
    // Match the build-script's isolated Git identity profile.
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1");
}

fn git_with_environment(root: &Path, args: &[&str], environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    command
        .current_dir(root)
        .args(["-c", "commit.gpgsign=false", "-c", "core.hooksPath="])
        .args(args);
    for (name, value) in environment {
        command.env(name, value);
    }
    command.output().expect("spawn fixture Git")
}

fn git_with_copied_index(root: &Path, args: &[&str], environment: &[(&str, &str)]) -> Output {
    // Git diff can refresh index stat data even with GIT_OPTIONAL_LOCKS=0. Run
    // the oracle against a copy so it cannot mutate watched inputs.
    let index = PathBuf::from(git(
        root,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    ));
    let temp = aethyme_testkit::tmp_dir();
    let copied_index = temp.path().join("index");
    fs::copy(&index, &copied_index).expect("copy fixture Git index");
    let index_dir = index.parent().expect("fixture Git index parent");
    for entry in fs::read_dir(index_dir).expect("read fixture Git index directory") {
        let entry = entry.expect("read fixture Git index entry");
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("sharedindex.")
        {
            fs::copy(entry.path(), temp.path().join(entry.file_name()))
                .expect("copy fixture split index");
        }
    }
    let copied_index = copied_index.to_str().expect("copied Git index UTF-8 path");
    let mut isolated_environment = environment.to_vec();
    isolated_environment.push(("GIT_INDEX_FILE", copied_index));
    git_with_environment(root, args, &isolated_environment)
}

fn git_revision(root: &Path) -> String {
    let commit = git(root, &["rev-parse", "--verify", "HEAD^{commit}"]);
    let attributes_file = format!(
        "core.attributesFile={}",
        if cfg!(windows) { "NUL" } else { "/dev/null" }
    );
    let output = git_with_copied_index(
        root,
        &[
            "--attr-source=HEAD",
            "-c",
            attributes_file.as_str(),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "--quiet",
            "HEAD",
            "--",
        ],
        &[],
    );
    match output.status.code() {
        Some(0) => commit,
        Some(1) => format!("{commit}-dirty"),
        code => panic!(
            "Git tracked-dirty check failed with {code:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = git_with_environment(root, args, &[]);
    assert!(
        output.status.success(),
        "Git command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture Git output UTF-8")
        .trim()
        .to_owned()
}

struct ProbeRun {
    identity: String,
    build_script_ran: bool,
    diagnostics: String,
}

fn repository_root_for_manifest(manifest: &Path) -> PathBuf {
    let mut directory = manifest.parent().expect("fixture manifest parent");
    loop {
        if directory.join(".git").exists() {
            return directory.to_path_buf();
        }
        directory = directory
            .parent()
            .expect("fixture manifest must be inside a Git worktree");
    }
}

fn run_probe(manifest: &Path, target: &Path, common_cwd: &Path) -> ProbeRun {
    run_probe_with_environment(manifest, target, common_cwd, &[])
}

fn run_probe_with_environment(
    manifest: &Path,
    target: &Path,
    common_cwd: &Path,
    environment: &[(&str, &str)],
) -> ProbeRun {
    let source_root = repository_root_for_manifest(manifest);
    let config = source_root.join(".cargo/config.toml");
    run_probe_using(
        manifest,
        target,
        common_cwd,
        common_cwd,
        Some(&config),
        environment,
    )
}

fn run_probe_discovered_config(
    manifest: &Path,
    target: &Path,
    source_root: &Path,
    fixed_pwd: &Path,
) -> ProbeRun {
    run_probe_using(manifest, target, source_root, fixed_pwd, None, &[])
}

fn run_probe_without_config(manifest: &Path, target: &Path, common_cwd: &Path) -> ProbeRun {
    run_probe_using(manifest, target, common_cwd, common_cwd, None, &[])
}

fn run_probe_using(
    manifest: &Path,
    target: &Path,
    current_dir: &Path,
    fixed_pwd: &Path,
    config: Option<&Path>,
    environment: &[(&str, &str)],
) -> ProbeRun {
    let cargo_home = target
        .parent()
        .expect("probe target parent")
        .join("isolated-cargo-home");
    fs::create_dir_all(&cargo_home).expect("create isolated Cargo home");
    let mut command = Command::new(env!("CARGO"));
    clear_git_environment(&mut command);
    command
        .current_dir(current_dir)
        .env("PWD", fixed_pwd)
        .env_remove(SOURCE_ROOT_ENV)
        .env("CARGO_HOME", cargo_home)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("RUSTC_WRAPPER", "");
    if let Some(config) = config {
        command.arg("--config").arg(config);
    }
    command
        .args([
            "run",
            "--offline",
            "--locked",
            "--verbose",
            "--verbose",
            "-j",
            "1",
        ])
        .arg("--manifest-path")
        .arg(manifest);
    for (name, value) in environment {
        command.env(name, value);
    }
    let output = command.output().expect("spawn fixture Cargo");
    assert!(
        output.status.success(),
        "command {command:?} failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).expect("fixture output UTF-8");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let identities = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("AETHYME_PROBE_IDENTITY:"))
        .collect::<Vec<_>>();
    assert_eq!(
        identities.len(),
        1,
        "expected exactly one identity record: {stdout}"
    );
    let identity = identities[0].to_owned();
    let diagnostics = format!("{stdout}\n{stderr}");
    let build_script_ran = stderr
        .lines()
        .any(|line| line.contains("Running") && line.contains("build-script-build"));
    ProbeRun {
        identity,
        build_script_ran,
        diagnostics,
    }
}

fn assert_identity(run: &ProbeRun, commit: &str, revision: &str, context: &str) {
    assert_eq!(
        run.identity,
        format!("{commit}|{revision}|{revision}"),
        "{context}; Cargo diagnostics:\n{}",
        run.diagnostics
    );
}

fn assert_script_ran(run: &ProbeRun, context: &str) {
    assert!(
        run.build_script_ran,
        "build script did not rerun for {context}; Cargo diagnostics:\n{}",
        run.diagnostics
    );
}

fn assert_script_fresh(run: &ProbeRun, context: &str) {
    assert!(
        !run.build_script_ran,
        "build script reran for unchanged inputs ({context}); Cargo diagnostics:\n{}",
        run.diagnostics
    );
}

fn assert_source_root_fallback(run: &ProbeRun, context: &str) {
    assert!(
        run.diagnostics
            .contains(&format!("cargo:rerun-if-env-changed={SOURCE_ROOT_ENV}")),
        "source-root environment input was not tracked ({context}); Cargo diagnostics:\n{}",
        run.diagnostics
    );
    assert!(
        run.diagnostics
            .contains("cargo:rerun-if-changed=.aethyme-source-identity-always-recheck"),
        "missing or mismatched source-root config did not use conservative fallback ({context}); Cargo diagnostics:\n{}",
        run.diagnostics
    );
}

fn make_git_pointer_older_than_any_probe_output(path: &Path) {
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open linked-worktree pointer without truncating it");
    file.set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(1)))
        .expect("make linked-worktree pointer older than probe outputs");
}

fn assert_git_pointer_older_than_probe_output(
    pointer: &Path,
    target: &Path,
    commit_variable: &str,
    context: &str,
) {
    let build_dir = target.join("debug/build");
    let needle = format!("cargo:rustc-env={commit_variable}=");
    let output_time = fs::read_dir(&build_dir)
        .expect("read probe build outputs")
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("output"))
        .find(|output| fs::read_to_string(output).is_ok_and(|contents| contents.contains(&needle)))
        .and_then(|output| fs::metadata(output).ok()?.modified().ok())
        .expect("find source-identity build-script output time");
    let pointer_time = fs::metadata(pointer)
        .expect("stat linked-worktree pointer")
        .modified()
        .expect("read linked-worktree pointer time");
    assert!(
        pointer_time < output_time,
        "linked-worktree pointer must predate the build output ({context}): pointer={pointer_time:?}, output={output_time:?}"
    );
}

fn assert_nonregular_entry_falls_back(run: &ProbeRun, entry: &Path, context: &str) {
    assert!(
        run.diagnostics
            .contains("cargo:rerun-if-changed=.aethyme-source-identity-always-recheck"),
        "nonregular tracked entry did not select conservative fallback ({context}); Cargo diagnostics:\n{}",
        run.diagnostics
    );
    let recursive_entry = format!("cargo:rerun-if-changed={}", entry.display());
    assert!(
        !run.diagnostics.contains(&recursive_entry),
        "nonregular tracked entry was emitted as a recursive Cargo input ({context}); Cargo diagnostics:\n{}",
        run.diagnostics
    );
}

#[derive(Debug, PartialEq, Eq)]
struct IndexSnapshot {
    bytes: Vec<u8>,
    len: u64,
    modified: std::time::SystemTime,
    readonly: bool,
    #[cfg(unix)]
    identity: (u64, u64, u32),
}

fn snapshot_index(path: &Path) -> IndexSnapshot {
    let bytes = fs::read(path).expect("read fixture Git index snapshot");
    let metadata = fs::metadata(path).expect("stat fixture Git index snapshot");
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (metadata.dev(), metadata.ino(), metadata.mode())
    };
    IndexSnapshot {
        bytes,
        len: metadata.len(),
        modified: metadata
            .modified()
            .expect("fixture index modification time"),
        readonly: metadata.permissions().readonly(),
        #[cfg(unix)]
        identity,
    }
}

fn snapshot_index_family(git_dir: &Path, index: &Path) -> Vec<(PathBuf, IndexSnapshot)> {
    let mut paths = vec![index.to_path_buf()];
    for entry in fs::read_dir(git_dir).expect("read fixture Git admin directory") {
        let entry = entry.expect("read fixture Git admin entry");
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("sharedindex.")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let snapshot = snapshot_index(&path);
            (path, snapshot)
        })
        .collect()
}

fn assert_conservative_index_fallback(run: &ProbeRun, context: &str) {
    let rerun_directives = run
        .diagnostics
        .lines()
        .filter(|line| line.contains("cargo:rerun-if-changed="))
        .collect::<Vec<_>>();
    assert!(
        rerun_directives.iter().any(|line| {
            line.contains("cargo:rerun-if-changed=.aethyme-source-identity-always-recheck")
        }),
        "index uncertainty did not select conservative fallback ({context}); Cargo diagnostics:\n{}",
        run.diagnostics
    );
    assert!(
        !rerun_directives
            .iter()
            .any(|line| line.contains("/.git") || line.contains("sharedindex.")),
        "uncertain index emitted a recursive Git-admin watch ({context}); Cargo diagnostics:\n{}",
        run.diagnostics
    );
}

fn assert_no_private_index_copies(target: &Path, context: &str) {
    let build_dir = target.join("debug/build");
    for package in fs::read_dir(&build_dir).expect("read fixture Cargo build directory") {
        let out_dir = package
            .expect("read fixture Cargo package output")
            .path()
            .join("out");
        if !out_dir.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&out_dir).expect("read fixture OUT_DIR") {
            let entry = entry.expect("read fixture OUT_DIR entry");
            let name = entry.file_name().to_string_lossy().into_owned();
            assert!(
                !name.starts_with(".aethyme-source-identity-index-"),
                "private index copy was left in OUT_DIR for {context}: {}",
                entry.path().display()
            );
        }
    }
}

#[test]
fn shared_cache_tracks_stable_revision_without_unrelated_git_metadata_rebuilds() {
    let scripts = [
        (
            "cli",
            include_str!("../../aethyme-cli/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "engine",
            include_str!("../../aethyme-engine/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "broker",
            include_str!("../../aethyme-broker/build.rs"),
            "AETHYME_BROKER_GIT_COMMIT",
            "AETHYME_BROKER_GIT_DESCRIBE",
        ),
    ];
    // Serial nested Cargo invocations have an independent tiny target and no
    // dependencies. A common cwd also prevents PWD from hiding the regression.
    for (name, script, commit_variable, legacy_alias_variable) in scripts {
        let temp = aethyme_testkit::tmp_dir();
        let repo = temp.path().join("newer");
        let older = temp.path().join("older");
        let package = Path::new("packages/probe");
        let package_root = repo.join(package);
        let target = temp.path().join("shared-target");
        fs::create_dir_all(package_root.join("src")).unwrap();
        fs::create_dir_all(repo.join(".cargo")).unwrap();
        fs::write(repo.join(".cargo/config.toml"), FIXTURE_CARGO_CONFIG).unwrap();
        fs::write(
            package_root.join("Cargo.toml"),
            "[package]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
        )
        .unwrap();
        fs::write(
            package_root.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(package_root.join("build.rs"), script).unwrap();
        fs::write(
            package_root.join("src/main.rs"),
            {
                let revision_variable = commit_variable.replace("_COMMIT", "_REVISION");
                format!(
                    "fn main() {{ println!(\"AETHYME_PROBE_IDENTITY:{{}}|{{}}|{{}}\", env!(\"{commit_variable}\"), env!(\"{revision_variable}\"), env!(\"{legacy_alias_variable}\")); }}\n"
                )
            },
        )
        .unwrap();
        git(&repo, &["init", "--initial-branch=probe"]);
        git(&repo, &["config", "user.name", "Build identity fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        git(
            &repo,
            &[
                "add",
                "packages/probe/Cargo.toml",
                "packages/probe/Cargo.lock",
                ".cargo/config.toml",
                "packages/probe/build.rs",
                "packages/probe/src/main.rs",
            ],
        );
        git(&repo, &["commit", "-m", "initial source"]);
        let first = git(&repo, &["rev-parse", "HEAD"]);
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                older.to_str().unwrap(),
                &first,
            ],
        );
        fs::write(repo.join("tracked-marker"), "new source\n").unwrap();
        git(&repo, &["add", "tracked-marker"]);
        git(&repo, &["commit", "-m", "advance newer source"]);
        let second = git(&repo, &["rev-parse", "HEAD"]);
        assert_ne!(first, second);
        let sibling = temp.path().join("sibling-only");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                sibling.to_str().unwrap(),
                &second,
            ],
        );
        make_git_pointer_older_than_any_probe_output(&older.join(".git"));
        make_git_pointer_older_than_any_probe_output(&sibling.join(".git"));

        // Explicit `--config` keeps the actual CWD and PWD fixed while the
        // config file path supplies the active worktree root fingerprint.
        let first_main = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&first_main, &second, &second, "initial main worktree");
        assert_script_ran(&first_main, "initial main worktree");
        assert_git_pointer_older_than_probe_output(
            &older.join(".git"),
            &target,
            commit_variable,
            "primary-to-linked pointer precondition",
        );
        assert_git_pointer_older_than_probe_output(
            &sibling.join(".git"),
            &target,
            commit_variable,
            "linked-to-linked pointer precondition",
        );

        let linked_old = run_probe(
            &older.join(package).join("Cargo.toml"),
            &target,
            temp.path(),
        );
        assert_identity(&linked_old, &first, &first, "main to older linked worktree");
        assert_script_ran(&linked_old, "main to older linked worktree");
        let linked_git_dir = git(&older, &["rev-parse", "--absolute-git-dir"]);
        let commondir_watch = format!(
            "cargo:rerun-if-changed={}",
            Path::new(&linked_git_dir).join("commondir").display()
        );
        let cargo_prefixed_commondir_watch =
            format!("[source-identity-probe 0.1.0] {commondir_watch}");
        assert!(
            linked_old
                .diagnostics
                .lines()
                .any(|line| { line == commondir_watch || line == cargo_prefixed_commondir_watch }),
            "linked worktree source identity omitted its commondir input: {}",
            linked_old.diagnostics
        );

        let main_again = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &main_again,
            &second,
            &second,
            "older linked worktree to main",
        );
        assert_script_ran(&main_again, "older linked worktree to main");
        let main_warm = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&main_warm, &second, &git_revision(&repo), "main warm build");
        assert_script_fresh(&main_warm, "same main worktree and source state");

        let linked_old_again = run_probe(
            &older.join(package).join("Cargo.toml"),
            &target,
            temp.path(),
        );
        assert_identity(
            &linked_old_again,
            &first,
            &first,
            "main to linked worktree before linked switch",
        );
        assert_script_ran(
            &linked_old_again,
            "main to linked worktree before linked switch",
        );
        assert_git_pointer_older_than_probe_output(
            &sibling.join(".git"),
            &target,
            commit_variable,
            "linked-to-linked older-pointer precondition",
        );
        let linked_latest = run_probe(
            &sibling.join(package).join("Cargo.toml"),
            &target,
            temp.path(),
        );
        assert_identity(
            &linked_latest,
            &second,
            &second,
            "linked to linked worktree",
        );
        assert_script_ran(
            &linked_latest,
            "linked to linked worktree with older pointer",
        );
        let linked_warm = run_probe(
            &sibling.join(package).join("Cargo.toml"),
            &target,
            temp.path(),
        );
        assert_identity(
            &linked_warm,
            &second,
            &second,
            "same linked worktree warm build",
        );
        assert_script_fresh(&linked_warm, "same linked worktree warm build");

        // Cargo's normal config discovery follows the active CWD. PWD remains
        // fixed, so these root changes exercise the config env value directly.
        let discovered_target = temp.path().join("discovered-target");
        let discovered_main = run_probe_discovered_config(
            &repo.join(package).join("Cargo.toml"),
            &discovered_target,
            &repo,
            temp.path(),
        );
        assert_identity(
            &discovered_main,
            &second,
            &second,
            "discovered config main worktree",
        );
        assert_script_ran(&discovered_main, "discovered config main worktree");
        assert_git_pointer_older_than_probe_output(
            &older.join(".git"),
            &discovered_target,
            commit_variable,
            "discovered primary-to-linked pointer precondition",
        );
        let discovered_old = run_probe_discovered_config(
            &older.join(package).join("Cargo.toml"),
            &discovered_target,
            &older,
            temp.path(),
        );
        assert_identity(&discovered_old, &first, &first, "discovered main to linked");
        assert_script_ran(&discovered_old, "discovered main to linked");
        assert_git_pointer_older_than_probe_output(
            &sibling.join(".git"),
            &discovered_target,
            commit_variable,
            "discovered linked-to-linked older-pointer precondition",
        );
        let discovered_sibling = run_probe_discovered_config(
            &sibling.join(package).join("Cargo.toml"),
            &discovered_target,
            &sibling,
            temp.path(),
        );
        assert_identity(
            &discovered_sibling,
            &second,
            &second,
            "discovered linked to linked",
        );
        assert_script_ran(
            &discovered_sibling,
            "discovered linked to linked with older pointer",
        );
        let discovered_sibling_warm = run_probe_discovered_config(
            &sibling.join(package).join("Cargo.toml"),
            &discovered_target,
            &sibling,
            temp.path(),
        );
        assert_identity(
            &discovered_sibling_warm,
            &second,
            &second,
            "discovered sibling warm build",
        );
        assert_script_fresh(&discovered_sibling_warm, "discovered sibling warm build");
        let discovered_main_again = run_probe_discovered_config(
            &repo.join(package).join("Cargo.toml"),
            &discovered_target,
            &repo,
            temp.path(),
        );
        assert_identity(
            &discovered_main_again,
            &second,
            &second,
            "discovered linked to main",
        );
        assert_script_ran(&discovered_main_again, "discovered linked to main");
        let discovered_main_warm = run_probe_discovered_config(
            &repo.join(package).join("Cargo.toml"),
            &discovered_target,
            &repo,
            temp.path(),
        );
        assert_identity(
            &discovered_main_warm,
            &second,
            &second,
            "discovered main warm build",
        );
        assert_script_fresh(&discovered_main_warm, "discovered main warm build");

        // A sibling's creation and later commit do not invalidate the active
        // root's stable source identity.
        let main_before_sibling_change =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &main_before_sibling_change,
            &second,
            &second,
            "return from sibling to main before sibling change",
        );
        assert_script_ran(
            &main_before_sibling_change,
            "return from sibling to main before sibling change",
        );
        let late_sibling = temp.path().join("late-sibling");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "--detach",
                late_sibling.to_str().unwrap(),
                &second,
            ],
        );
        let sibling_added = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&sibling_added, &second, &second, "sibling worktree added");
        assert_script_fresh(&sibling_added, "sibling worktree added");
        fs::write(late_sibling.join("sibling-only.txt"), "sibling-only\n").unwrap();
        git(
            &repo,
            &[
                "-C",
                late_sibling.to_str().unwrap(),
                "add",
                "sibling-only.txt",
            ],
        );
        git(
            &repo,
            &[
                "-C",
                late_sibling.to_str().unwrap(),
                "commit",
                "-m",
                "sibling-only commit",
            ],
        );
        let sibling_committed =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&sibling_committed, &second, &second, "sibling-only commit");
        assert_script_fresh(&sibling_committed, "sibling-only commit");
        git(
            &repo,
            &[
                "worktree",
                "remove",
                "--force",
                late_sibling.to_str().unwrap(),
            ],
        );

        // Without the project config, each invocation remains correct by
        // taking the conservative always-recheck path.
        let no_config_target = temp.path().join("external-no-config-target");
        let no_config_main = run_probe_without_config(
            &repo.join(package).join("Cargo.toml"),
            &no_config_target,
            temp.path(),
        );
        assert_identity(
            &no_config_main,
            &second,
            &second,
            "external cwd without config",
        );
        assert_source_root_fallback(&no_config_main, "external cwd without config");
        assert_script_ran(&no_config_main, "first external cwd without config");
        let no_config_warm = run_probe_without_config(
            &repo.join(package).join("Cargo.toml"),
            &no_config_target,
            temp.path(),
        );
        assert_identity(
            &no_config_warm,
            &second,
            &second,
            "repeated external cwd without config",
        );
        assert_source_root_fallback(&no_config_warm, "repeated external cwd without config");
        assert_script_ran(&no_config_warm, "missing config always-recheck fallback");
        let no_config_linked = run_probe_without_config(
            &older.join(package).join("Cargo.toml"),
            &no_config_target,
            temp.path(),
        );
        assert_identity(
            &no_config_linked,
            &first,
            &first,
            "external cwd without config linked",
        );
        assert_source_root_fallback(&no_config_linked, "external cwd without config linked");
        assert_script_ran(&no_config_linked, "missing config linked-worktree fallback");

        let mismatch_root = temp.path().join("not-source-root");
        fs::create_dir_all(&mismatch_root).unwrap();
        let forced_config_target = temp.path().join("forced-config-target");
        let poisoned_environment = [(SOURCE_ROOT_ENV, mismatch_root.to_str().unwrap())];
        let forced_config = run_probe_with_environment(
            &repo.join(package).join("Cargo.toml"),
            &forced_config_target,
            temp.path(),
            &poisoned_environment,
        );
        assert_identity(
            &forced_config,
            &second,
            &second,
            "forced source-root config",
        );
        assert_script_ran(&forced_config, "forced source-root config");
        assert!(
            !forced_config
                .diagnostics
                .contains("cargo:rerun-if-changed=.aethyme-source-identity-always-recheck"),
            "project config did not override ambient source-root spoof: {}",
            forced_config.diagnostics
        );
        let forced_config_warm = run_probe_with_environment(
            &repo.join(package).join("Cargo.toml"),
            &forced_config_target,
            temp.path(),
            &poisoned_environment,
        );
        assert_identity(
            &forced_config_warm,
            &second,
            &second,
            "repeated forced source-root config",
        );
        assert_script_fresh(&forced_config_warm, "repeated forced source-root config");

        let mismatch_config = temp.path().join("mismatched-source-root.toml");
        fs::write(
            &mismatch_config,
            format!(
                "[env]\n{SOURCE_ROOT_ENV} = {{ value = {:?}, force = true }}\n",
                mismatch_root.to_string_lossy()
            ),
        )
        .unwrap();
        let mismatch_target = temp.path().join("mismatched-config-target");
        let mismatched = run_probe_using(
            &repo.join(package).join("Cargo.toml"),
            &mismatch_target,
            temp.path(),
            temp.path(),
            Some(&mismatch_config),
            &[],
        );
        assert_identity(
            &mismatched,
            &second,
            &second,
            "mismatched source-root config",
        );
        assert_source_root_fallback(&mismatched, "mismatched source-root config");
        assert_script_ran(&mismatched, "mismatched source-root config");
        let mismatched_again = run_probe_using(
            &repo.join(package).join("Cargo.toml"),
            &mismatch_target,
            temp.path(),
            temp.path(),
            Some(&mismatch_config),
            &[],
        );
        assert_identity(
            &mismatched_again,
            &second,
            &second,
            "repeated mismatched source-root config",
        );
        assert_source_root_fallback(&mismatched_again, "repeated mismatched source-root config");
        assert_script_ran(
            &mismatched_again,
            "mismatched source-root always-recheck fallback",
        );

        // `--dirty` changes for tracked worktree edits even without an index or
        // HEAD update. Both entering and leaving dirty state must be observed.
        let marker = repo.join("tracked-marker");
        let original = fs::read(&marker).unwrap();
        fs::write(&marker, "tracked edit\n").unwrap();
        let dirty = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let dirty_revision = git_revision(&repo);
        assert!(dirty_revision.ends_with("-dirty"));
        assert_identity(&dirty, &second, &dirty_revision, "tracked edit");
        assert_script_ran(&dirty, "tracked worktree edit");
        fs::write(&marker, original).unwrap();
        let clean = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let clean_revision = git_revision(&repo);
        assert_identity(&clean, &second, &clean_revision, "tracked edit restored");
        assert_script_ran(&clean, "tracked edit restoration");

        // Untracked files do not affect exact commit + tracked-dirty identity.
        let untracked = repo.join("untracked-marker");
        fs::write(&untracked, "untracked\n").unwrap();
        let untracked_run = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&untracked_run, &second, &second, "untracked file");
        assert_script_fresh(&untracked_run, "untracked file");
        fs::remove_file(untracked).unwrap();

        // An untracked text attribute cannot normalize a tracked CRLF-only edit
        // out of the dirty bit because Git reads attributes from HEAD.
        let attributes = repo.join(".gitattributes");
        fs::write(&attributes, "tracked-marker text\n").unwrap();
        let marker_bytes = fs::read(&marker).unwrap();
        let attributes_only =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &attributes_only,
            &second,
            &second,
            "untracked attributes file",
        );
        assert_script_fresh(&attributes_only, "untracked attributes file");
        let mut crlf_marker = Vec::with_capacity(marker_bytes.len() + 1);
        for byte in &marker_bytes {
            if *byte == b'\n' {
                crlf_marker.push(b'\r');
            }
            crlf_marker.push(*byte);
        }
        fs::write(&marker, crlf_marker).unwrap();
        let attribute_probe =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &attribute_probe,
            &second,
            &format!("{second}-dirty"),
            "untracked text attribute",
        );
        assert_script_ran(&attribute_probe, "tracked edit under untracked attributes");
        fs::write(&marker, marker_bytes).unwrap();
        let restored_after_attributes =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &restored_after_attributes,
            &second,
            &second,
            "attributes test restored",
        );
        assert_script_ran(
            &restored_after_attributes,
            "tracked edit restoration under untracked attributes",
        );
        fs::remove_file(attributes).unwrap();

        // Tag creation and tag packing leave the exact source revision intact.
        // The loose current branch ref stays present until the final prune case.
        let tag = format!("{name}-release");
        git(&repo, &["tag", "-a", &tag, "-m", "fixture release"]);
        let tagged = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&tagged, &second, &second, "annotated tag added");
        assert_script_fresh(&tagged, "tag added");
        git(&repo, &["pack-refs", "--all", "--no-prune"]);
        let packed_tags = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &packed_tags,
            &second,
            &second,
            "tags packed with loose branch ref",
        );
        assert_script_fresh(&packed_tags, "packed tags with loose branch ref");

        // Git does not prune a checked-out branch. Detach first, pack its ref,
        // then attach HEAD to the packed-only branch and verify absent-to-present
        // transition handling when a commit recreates the loose ref.
        git(&repo, &["checkout", "--detach", &second]);
        let detached_for_pack =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &detached_for_pack,
            &second,
            &second,
            "detach before branch packing",
        );
        assert_script_ran(&detached_for_pack, "detach before branch packing");
        git(&repo, &["pack-refs", "--all", "--prune"]);
        let packed_detached =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &packed_detached,
            &second,
            &second,
            "unrelated packed branch while detached",
        );
        assert_script_fresh(&packed_detached, "unrelated packed branch while detached");

        let git_dir = PathBuf::from(git(&repo, &["rev-parse", "--absolute-git-dir"]));
        let loose_branch = git_dir.join("refs/heads/probe");
        assert!(!loose_branch.exists(), "fixture branch must be packed only");
        git(&repo, &["symbolic-ref", "HEAD", "refs/heads/probe"]);
        let packed_only = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &packed_only,
            &second,
            &second,
            "HEAD attached to packed-only branch",
        );
        assert_script_ran(&packed_only, "HEAD attached to packed-only branch");
        assert!(
            !loose_branch.exists(),
            "symbolic-ref unexpectedly recreated the loose branch"
        );
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
        let advanced = git(
            &repo,
            &[
                "commit-tree",
                &tree,
                "-p",
                &second,
                "-m",
                "advance packed branch",
            ],
        );
        git(&repo, &["update-ref", "refs/heads/probe", &advanced]);
        let packed_to_loose =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &packed_to_loose,
            &advanced,
            &advanced,
            "packed ref became loose",
        );
        assert_script_ran(&packed_to_loose, "packed ref became loose");
        assert!(
            loose_branch.is_file(),
            "updated packed branch should have a loose ref"
        );

        // Detached HEAD identity changes live in the per-worktree HEAD file.
        git(&repo, &["checkout", "--detach", &first]);
        let detached = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let detached_revision = git_revision(&repo);
        assert_identity(&detached, &first, &detached_revision, "detached HEAD");
        assert_script_ran(&detached, "detached HEAD changed");
        let detached_warm = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &detached_warm,
            &first,
            &detached_revision,
            "detached HEAD stable",
        );
        assert_script_fresh(&detached_warm, "detached HEAD stable");

        // Replacement refs and inherited Git/build identity variables cannot
        // rewrite this profile's exact commit + tracked-dirty identity.
        git(&repo, &["checkout", "--detach", &second]);
        git(
            &repo,
            &["commit", "--allow-empty", "-m", "replacement fixture child"],
        );
        let replacement_head = git(&repo, &["rev-parse", "HEAD"]);
        let expected_revision = git_revision(&repo);
        let replacement_base =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &replacement_base,
            &replacement_head,
            &expected_revision,
            "replacement fixture commit",
        );
        assert_script_ran(&replacement_base, "replacement fixture commit");
        git(&repo, &["replace", "--graft", &replacement_head]);
        let replacement_probe =
            run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &replacement_probe,
            &replacement_head,
            &expected_revision,
            "replacement ref added",
        );
        assert_script_fresh(&replacement_probe, "replacement ref added");
        let poisoned_identity = run_probe_with_environment(
            &repo.join(package).join("Cargo.toml"),
            &temp.path().join("poisoned-git-env-target"),
            temp.path(),
            &[
                ("GIT_NO_REPLACE_OBJECTS", "0"),
                ("GIT_REPLACE_REF_BASE", "refs/poison/replace/"),
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "core.abbrev"),
                ("GIT_CONFIG_VALUE_0", "4"),
            ],
        );
        assert_script_ran(&poisoned_identity, "poisoned Git configuration environment");
        assert_identity(
            &poisoned_identity,
            &replacement_head,
            &expected_revision,
            "poisoned Git configuration environment",
        );
        git(&repo, &["checkout", "--detach", &first]);

        // Gitlinks appear as directories in the working tree. They are not
        // regular tracked files and must select the conservative fallback
        // instead of a recursive watch of the submodule directory.
        let gitlink = repo.join("nested-gitlink");
        fs::create_dir(&gitlink).unwrap();
        let gitlink_cache_info = format!("160000,{first},nested-gitlink");
        git(
            &repo,
            &["update-index", "--add", "--cacheinfo", &gitlink_cache_info],
        );
        let gitlink_run = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let gitlink_revision = git_revision(&repo);
        assert_identity(
            &gitlink_run,
            &first,
            &gitlink_revision,
            "tracked gitlink directory",
        );
        assert_script_ran(&gitlink_run, "tracked gitlink directory");
        assert_nonregular_entry_falls_back(&gitlink_run, &gitlink, "tracked gitlink directory");
        git(&repo, &["update-index", "--force-remove", "nested-gitlink"]);
        fs::remove_dir(gitlink).unwrap();

        // A tracked symlink may point to a directory outside its package. Its
        // symlink metadata is nonregular, so it also selects fallback without
        // making Cargo recursively scan the link target.
        #[cfg(unix)]
        {
            let target_dir = repo.join("symlink-target");
            fs::create_dir(&target_dir).unwrap();
            fs::write(target_dir.join("nested-file"), b"target").unwrap();
            let symlink = repo.join("tracked-directory-symlink");
            std::os::unix::fs::symlink(&target_dir, &symlink).unwrap();
            git(&repo, &["add", "tracked-directory-symlink"]);
            let symlink_run =
                run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
            let symlink_revision = git_revision(&repo);
            assert_identity(
                &symlink_run,
                &first,
                &symlink_revision,
                "tracked directory symlink",
            );
            assert_script_ran(&symlink_run, "tracked directory symlink");
            assert_nonregular_entry_falls_back(&symlink_run, &symlink, "tracked directory symlink");
        }

        // Exercise both private-index-copy gates through each actual production
        // build script from `scripts`. Reset the fixture so the safety cases
        // start from a clean worktree and do not inherit the symlink fallback.
        git(&repo, &["reset", "--hard", &first]);
        let symlink = repo.join("tracked-directory-symlink");
        if fs::symlink_metadata(&symlink).is_ok() {
            fs::remove_file(&symlink).unwrap();
        }
        let symlink_target = repo.join("symlink-target");
        if symlink_target.exists() {
            fs::remove_dir_all(&symlink_target).unwrap();
        }
        let filter_attributes = repo.join(".gitattributes");
        fs::write(
            &filter_attributes,
            "packages/probe/src/main.rs filter=probe\n",
        )
        .unwrap();
        git(&repo, &["add", ".gitattributes"]);
        git(&repo, &["commit", "-m", "tracked clean filter attribute"]);
        let filter_commit = git(&repo, &["rev-parse", "--verify", "HEAD^{commit}"]);
        let filter_marker = temp.path().join("clean-filter-executed");
        let filter_command = format!("touch '{}'", filter_marker.display());
        git(&repo, &["config", "filter.probe.clean", &filter_command]);
        let source_path = repo.join(package).join("src/main.rs");
        let mut changed_source = fs::read(&source_path).unwrap();
        changed_source.extend_from_slice(b"\n// force clean-filter evaluation\n");
        fs::write(&source_path, changed_source).unwrap();
        let filter_run = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &filter_run,
            &filter_commit,
            &format!("{filter_commit}-dirty"),
            "configured clean filter fallback",
        );
        assert_conservative_index_fallback(&filter_run, "configured clean filter");
        assert!(
            !filter_marker.exists(),
            "source identity diff executed a configured external clean filter"
        );
        git(&repo, &["config", "--unset-all", "filter.probe.clean"]);
        git(&repo, &["reset", "--hard", &first]);

        let index = PathBuf::from(git(
            &repo,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        ));
        let git_dir = PathBuf::from(git(&repo, &["rev-parse", "--absolute-git-dir"]));
        let fallback_revision = git(&repo, &["rev-parse", "--verify", "HEAD^{commit}"]);

        // A split index is detected through its sharedindex entry before the
        // primary index is opened for a private copy.
        git(&repo, &["update-index", "--split-index"]);
        let split_before = snapshot_index_family(&git_dir, &index);
        assert!(
            split_before.len() > 1,
            "fixture did not create a split index"
        );
        let split_run = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_script_ran(&split_run, "split-index fallback");
        assert_identity(
            &split_run,
            &first,
            &format!("{fallback_revision}-dirty"),
            "split-index conservative identity",
        );
        assert_conservative_index_fallback(&split_run, "split index");
        assert_eq!(snapshot_index_family(&git_dir, &index), split_before);
        assert_no_private_index_copies(&target, "split index");

        // Return to a full index, remove obsolete shared-index files, and make
        // the primary index just over the 16 MiB copy cap. The guard must
        // refuse before creating an OUT_DIR copy or changing the source file.
        git(&repo, &["update-index", "--no-split-index"]);
        for entry in fs::read_dir(&git_dir).expect("read fixture Git admin directory") {
            let entry = entry.expect("read fixture Git admin entry");
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with("sharedindex.")
            {
                fs::remove_file(entry.path()).expect("remove unused shared index");
            }
        }
        const INDEX_COPY_LIMIT: u64 = 16 * 1024 * 1024;
        fs::OpenOptions::new()
            .write(true)
            .open(&index)
            .expect("open fixture Git index for oversize boundary")
            .set_len(INDEX_COPY_LIMIT + 1)
            .expect("extend fixture index past private-copy cap");
        let oversized_before = snapshot_index_family(&git_dir, &index);
        assert_eq!(oversized_before.len(), 1, "fixture still has a split index");
        assert_eq!(oversized_before[0].1.len, INDEX_COPY_LIMIT + 1);
        let oversized_run = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_script_ran(&oversized_run, "oversized-index fallback");
        assert_identity(
            &oversized_run,
            &first,
            &format!("{fallback_revision}-dirty"),
            "oversized-index conservative identity",
        );
        assert_conservative_index_fallback(&oversized_run, "oversized index");
        assert_eq!(snapshot_index_family(&git_dir, &index), oversized_before);
        assert_no_private_index_copies(&target, "oversized index");
    }
}

#[test]
fn shallow_metadata_does_not_change_the_stable_source_identity() {
    let scripts = [
        (
            "cli-shallow",
            include_str!("../../aethyme-cli/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "engine-shallow",
            include_str!("../../aethyme-engine/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "broker-shallow",
            include_str!("../../aethyme-broker/build.rs"),
            "AETHYME_BROKER_GIT_COMMIT",
            "AETHYME_BROKER_GIT_DESCRIBE",
        ),
    ];

    for (name, script, commit_variable, legacy_alias_variable) in scripts {
        let temp = aethyme_testkit::tmp_dir();
        let repo = temp.path().join("repo");
        let package_root = repo.join("packages/probe");
        let target = temp.path().join("shared-target");
        fs::create_dir_all(package_root.join("src")).unwrap();
        fs::create_dir_all(repo.join(".cargo")).unwrap();
        fs::write(repo.join(".cargo/config.toml"), FIXTURE_CARGO_CONFIG).unwrap();
        fs::write(
            package_root.join("Cargo.toml"),
            "[package]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
        )
        .unwrap();
        fs::write(
            package_root.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(package_root.join("build.rs"), script).unwrap();
        fs::write(
            package_root.join("src/main.rs"),
            {
                let revision_variable = commit_variable.replace("_COMMIT", "_REVISION");
                format!(
                    "fn main() {{ println!(\"AETHYME_PROBE_IDENTITY:{{}}|{{}}|{{}}\", env!(\"{commit_variable}\"), env!(\"{revision_variable}\"), env!(\"{legacy_alias_variable}\")); }}\n"
                )
            },
        )
        .unwrap();
        git(&repo, &["init", "--initial-branch=probe"]);
        git(&repo, &["config", "user.name", "Build identity fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        fs::write(repo.join("tracked-marker"), "base\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "tagged base"]);
        git(&repo, &["tag", "-a", "base-tag", "-m", "base"]);
        fs::write(repo.join("tracked-marker"), "descendant\n").unwrap();
        git(&repo, &["add", "tracked-marker"]);
        git(&repo, &["commit", "-m", "descendant"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let common_dir_text = git(
            &repo,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        );
        let common_dir = Path::new(&common_dir_text);
        let shallow = common_dir.join("shallow");
        assert!(
            !shallow.exists(),
            "fixture must begin without shallow metadata"
        );

        let initial = run_probe(&package_root.join("Cargo.toml"), &target, temp.path());
        let expected_before = git_revision(&repo);
        assert_identity(&initial, &head, &expected_before, name);
        assert_script_ran(&initial, "initial shallow fixture build");

        let git_dir_text = git(&repo, &["rev-parse", "--absolute-git-dir"]);
        let git_dir = Path::new(&git_dir_text);
        let head_file = fs::read(git_dir.join("HEAD")).unwrap();
        let index_file = fs::read(git_dir.join("index")).unwrap();
        let refs = git(
            &repo,
            &["for-each-ref", "--format=%(refname) %(objectname)"],
        );

        fs::write(&shallow, format!("{head}\n")).unwrap();
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), head);
        assert_eq!(fs::read(git_dir.join("HEAD")).unwrap(), head_file);
        assert_eq!(fs::read(git_dir.join("index")).unwrap(), index_file);
        assert_eq!(
            git(
                &repo,
                &["for-each-ref", "--format=%(refname) %(objectname)"],
            ),
            refs
        );
        let shallow_run = run_probe(&package_root.join("Cargo.toml"), &target, temp.path());
        let expected_after = git_revision(&repo);
        assert_eq!(
            expected_before, expected_after,
            "shallow metadata does not alter commit or tracked dirtiness"
        );

        assert_identity(
            &shallow_run,
            &head,
            &expected_after,
            "shallow metadata appeared",
        );
        assert_script_fresh(
            &shallow_run,
            "shallow metadata appeared without HEAD/index/ref changes",
        );
    }
}

#[cfg(unix)]
#[test]
fn symlinked_tracked_ancestor_uses_fallback_without_watching_its_target() {
    let scripts = [
        (
            "cli-symlink-ancestor",
            include_str!("../../aethyme-cli/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "engine-symlink-ancestor",
            include_str!("../../aethyme-engine/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "broker-symlink-ancestor",
            include_str!("../../aethyme-broker/build.rs"),
            "AETHYME_BROKER_GIT_COMMIT",
            "AETHYME_BROKER_GIT_DESCRIBE",
        ),
    ];

    for (name, script, commit_variable, legacy_alias_variable) in scripts {
        let temp = aethyme_testkit::tmp_dir();
        let repo = temp.path().join("repo");
        let package_root = repo.join("packages/probe");
        let target = temp.path().join("shared-target");
        fs::create_dir_all(package_root.join("src")).unwrap();
        fs::create_dir_all(repo.join(".cargo")).unwrap();
        fs::write(repo.join(".cargo/config.toml"), FIXTURE_CARGO_CONFIG).unwrap();
        fs::create_dir_all(repo.join("tracked-parent")).unwrap();
        fs::write(
            package_root.join("Cargo.toml"),
            "[package]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n",
        )
        .unwrap();
        fs::write(
            package_root.join("Cargo.lock"),
            "version = 4\n[[package]]\nname = \"source-identity-probe\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(package_root.join("build.rs"), script).unwrap();
        fs::write(
            package_root.join("src/main.rs"),
            {
                let revision_variable = commit_variable.replace("_COMMIT", "_REVISION");
                format!(
                    "fn main() {{ println!(\"AETHYME_PROBE_IDENTITY:{{}}|{{}}|{{}}\", env!(\"{commit_variable}\"), env!(\"{revision_variable}\"), env!(\"{legacy_alias_variable}\")); }}\n"
                )
            },
        )
        .unwrap();
        let tracked = repo.join("tracked-parent/source.txt");
        fs::write(&tracked, "tracked source\n").unwrap();
        git(&repo, &["init", "--initial-branch=probe"]);
        git(&repo, &["config", "user.name", "Build identity fixture"]);
        git(&repo, &["config", "user.email", "fixture@example.invalid"]);
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-m", "tracked ancestor"]);
        let commit = git(&repo, &["rev-parse", "HEAD"]);

        let external = temp.path().join("external-target");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("source.txt"), "outside tracked source\n").unwrap();
        fs::remove_file(&tracked).unwrap();
        fs::remove_dir(repo.join("tracked-parent")).unwrap();
        std::os::unix::fs::symlink(&external, repo.join("tracked-parent")).unwrap();

        let run = run_probe(&package_root.join("Cargo.toml"), &target, temp.path());
        let revision = git_revision(&repo);
        assert_identity(&run, &commit, &revision, name);
        assert_script_ran(&run, "tracked path gained a symlinked ancestor");
        assert_nonregular_entry_falls_back(
            &run,
            &repo.join("tracked-parent/source.txt"),
            "tracked path gained a symlinked ancestor",
        );
        assert!(
            !run.diagnostics.contains(&format!(
                "cargo:rerun-if-changed={}",
                external.join("source.txt").display()
            )),
            "tracked path through symlink ancestor must not watch outside target: {}",
            run.diagnostics
        );
    }
}

#[test]
fn build_scripts_keep_empty_identity_exports_when_git_is_unavailable() {
    let scripts = [
        (
            "cli-no-git",
            include_str!("../../aethyme-cli/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "engine-no-git",
            include_str!("../../aethyme-engine/build.rs"),
            "AETHYME_GIT_COMMIT",
            "AETHYME_GIT_DESCRIBE",
        ),
        (
            "broker-no-git",
            include_str!("../../aethyme-broker/build.rs"),
            "AETHYME_BROKER_GIT_COMMIT",
            "AETHYME_BROKER_GIT_DESCRIBE",
        ),
    ];

    let temp = aethyme_testkit::tmp_dir();
    let empty_path = temp.path().join("empty-path");
    fs::create_dir(&empty_path).unwrap();
    for (name, script, commit_variable, legacy_alias_variable) in scripts {
        let source = temp.path().join(format!("{name}.rs"));
        fs::write(&source, script).unwrap();
        let mut executable = temp.path().join(name);
        if cfg!(windows) {
            executable.set_extension("exe");
        }
        let mut compile = Command::new("rustc");
        compile
            .args(["--edition=2024"])
            .arg(&source)
            .arg("-o")
            .arg(&executable);
        checked(&mut compile);

        let manifest = temp.path().join(format!("{name}-package"));
        fs::create_dir(&manifest).unwrap();
        let mut command = Command::new(&executable);
        command
            .env("CARGO_MANIFEST_DIR", &manifest)
            .env("PATH", &empty_path)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_COMMON_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_OBJECT_DIRECTORY")
            .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
            .env_remove("GIT_CEILING_DIRECTORIES");
        let output = command.output().expect("run no-Git build script");
        assert!(
            output.status.success(),
            "build script {name} failed without Git: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).expect("build script output UTF-8");
        if name != "broker-no-git" {
            assert!(
                stdout
                    .lines()
                    .any(|line| line == "cargo:rustc-env=AETHYME_BUILD_DATE=unknown"),
                "router and engine must retain their build-date export even without date or Git ({name}): {stdout}"
            );
        }
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("cargo:rustc-env={commit_variable}=")),
            "missing best-effort empty commit export for {name}: {stdout}"
        );
        let revision_variable = commit_variable.replace("_COMMIT", "_REVISION");
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("cargo:rustc-env={revision_variable}=")),
            "missing best-effort empty revision export for {name}: {stdout}"
        );
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("cargo:rustc-env={legacy_alias_variable}=")),
            "missing best-effort empty compatibility alias for {name}: {stdout}"
        );
        assert!(
            stdout.lines().any(|line| {
                line == "cargo:rerun-if-changed=.aethyme-source-identity-always-recheck"
            }),
            "no-Git build did not use conservative fallback for {name}: {stdout}"
        );
    }
}
