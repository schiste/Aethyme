//! Real Cargo builds of tiny synthetic packages using the production scripts.
//! No product binary or real-repository mutation is needed for this regression.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
    let inherited_git_variables = std::env::vars_os()
        .filter_map(|(name, _)| {
            name.to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
                .then_some(name)
        })
        .collect::<Vec<_>>();
    for name in inherited_git_variables {
        command.env_remove(name);
    }
    // Match build-script Git calls while suppressing optional index refreshes.
    // `describe --dirty` still refreshes, so its oracle uses a copied index below.
    command.env("GIT_OPTIONAL_LOCKS", "0");
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
    // `git describe --dirty` refreshes index stat data even with
    // GIT_OPTIONAL_LOCKS=0. Run it against a copy so the oracle cannot change
    // the fixture's watched `.git` directory before the Cargo probe.
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

fn git_describe_dirty(root: &Path) -> String {
    let output = git_with_copied_index(root, &["describe", "--tags", "--always", "--dirty"], &[]);
    assert!(
        output.status.success(),
        "Git describe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("fixture Git describe UTF-8")
        .trim()
        .to_owned()
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

fn run_probe(manifest: &Path, target: &Path, common_cwd: &Path) -> ProbeRun {
    run_probe_with_environment(manifest, target, common_cwd, &[])
}

fn run_probe_with_environment(
    manifest: &Path,
    target: &Path,
    common_cwd: &Path,
    environment: &[(&str, &str)],
) -> ProbeRun {
    let mut command = Command::new(env!("CARGO"));
    clear_git_environment(&mut command);
    command
        .current_dir(common_cwd)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "0")
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("RUSTC_WRAPPER", "")
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

fn assert_identity(run: &ProbeRun, commit: &str, describe: &str, context: &str) {
    assert_eq!(
        run.identity,
        format!("{commit}|{describe}"),
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
fn shared_cache_tracks_each_worktrees_source_identity_without_rebuilding_warm_tree() {
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
    for (name, script, commit_variable, describe_variable) in scripts {
        let temp = aethyme_testkit::tmp_dir();
        let repo = temp.path().join("newer");
        let older = temp.path().join("older");
        let package = Path::new("packages/probe");
        let package_root = repo.join(package);
        let target = temp.path().join("shared-target");
        fs::create_dir_all(package_root.join("src")).unwrap();
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
            format!(
                "fn main() {{ println!(\"AETHYME_PROBE_IDENTITY:{{}}|{{}}\", env!(\"{commit_variable}\"), env!(\"{describe_variable}\")); }}\n"
            ),
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
        // Start with packed refs already present so an absent packed-refs path
        // cannot conceal an invalidation bug.
        git(&repo, &["pack-refs", "--all", "--no-prune"]);

        for (tree, expected) in [(&repo, &second), (&older, &first), (&repo, &second)] {
            let run = run_probe(&tree.join(package).join("Cargo.toml"), &target, temp.path());
            let describe = git_describe_dirty(tree);
            assert_identity(&run, expected, &describe, name);
            assert_script_ran(&run, "switching source worktree");
        }
        let warm = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let stable_describe = git_describe_dirty(&repo);
        assert_identity(&warm, &second, &stable_describe, name);
        assert_script_fresh(&warm, "same linked worktree and source state");

        // `--dirty` changes for tracked worktree edits even without an index or
        // HEAD update. Both entering and leaving dirty state must be observed.
        let marker = repo.join("tracked-marker");
        let original = fs::read(&marker).unwrap();
        fs::write(&marker, "tracked edit\n").unwrap();
        let dirty = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let dirty_describe = git_describe_dirty(&repo);
        assert!(dirty_describe.ends_with("-dirty"));
        assert_identity(&dirty, &second, &dirty_describe, "tracked edit");
        assert_script_ran(&dirty, "tracked worktree edit");
        fs::write(&marker, original).unwrap();
        let clean = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let clean_describe = git_describe_dirty(&repo);
        assert_identity(&clean, &second, &clean_describe, "tracked edit restored");
        assert_script_ran(&clean, "tracked edit restoration");

        // Untracked files do not affect `git describe --dirty`; they should not
        // invalidate the build script or make a warm package rebuild.
        let untracked = repo.join("untracked-marker");
        fs::write(&untracked, "untracked\n").unwrap();
        let untracked_run = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let untracked_describe = git_describe_dirty(&repo);
        assert_eq!(untracked_describe, clean_describe);
        assert_identity(&untracked_run, &second, &clean_describe, "untracked file");
        assert_script_fresh(&untracked_run, "untracked file");
        fs::remove_file(untracked).unwrap();

        // Tag creation and packing both change `git describe` inputs. The refs
        // directory and packed file are watched without naming missing files.
        let tag = format!("{name}-release");
        git(&repo, &["tag", "-a", &tag, "-m", "fixture release"]);
        let tagged = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&tagged, &second, &tag, "annotated tag added");
        assert_script_ran(&tagged, "tag added");
        git(&repo, &["pack-refs", "--all", "--prune"]);
        let packed = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&packed, &second, &tag, "tag packed");
        assert_script_ran(&packed, "refs packed");
        let packed_warm = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(&packed_warm, &second, &tag, "packed tag stable");
        assert_script_fresh(&packed_warm, "packed refs stable");

        // Detached HEAD identity changes live in the per-worktree HEAD file.
        git(&repo, &["checkout", "--detach", &first]);
        let detached = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        let detached_describe = git_describe_dirty(&repo);
        assert_identity(&detached, &first, &detached_describe, "detached HEAD");
        assert_script_ran(&detached, "detached HEAD changed");
        let detached_warm = run_probe(&repo.join(package).join("Cargo.toml"), &target, temp.path());
        assert_identity(
            &detached_warm,
            &first,
            &detached_describe,
            "detached HEAD stable",
        );
        assert_script_fresh(&detached_warm, "detached HEAD stable");

        // Git replacement and config variables can alter `git describe`, which
        // is part of the exported build identity. Verify each poison changes
        // raw Git output, then verify Cargo's build script still sees the real
        // checkout after clearing all inherited GIT_* variables.
        git(&repo, &["checkout", "--detach", &second]);
        // An exact tag on HEAD makes parent traversal irrelevant to describe.
        // Put the tagged commit behind a clean child so the replacement must
        // change Git's independently derived description.
        git(
            &repo,
            &["commit", "--allow-empty", "-m", "replacement fixture child"],
        );
        let replacement_head = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["replace", "--graft", &replacement_head]);
        let expected_describe = git_describe_dirty(&repo);
        let no_replace = git_with_copied_index(
            &repo,
            &["describe", "--tags", "--always", "--dirty"],
            &[("GIT_NO_REPLACE_OBJECTS", "1")],
        );
        assert!(no_replace.status.success());
        assert_ne!(
            String::from_utf8(no_replace.stdout).unwrap().trim(),
            expected_describe,
            "GIT_NO_REPLACE_OBJECTS must alter describe in the replacement fixture"
        );
        let short_abbrev = git_with_copied_index(
            &repo,
            &["describe", "--tags", "--always", "--dirty"],
            &[
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "core.abbrev"),
                ("GIT_CONFIG_VALUE_0", "4"),
            ],
        );
        assert!(short_abbrev.status.success());
        assert_ne!(
            String::from_utf8(short_abbrev.stdout).unwrap().trim(),
            expected_describe,
            "GIT_CONFIG_COUNT must alter describe abbreviation in the fixture"
        );
        let poisoned_identity = run_probe_with_environment(
            &repo.join(package).join("Cargo.toml"),
            &temp.path().join("poisoned-git-env-target"),
            temp.path(),
            &[
                ("GIT_NO_REPLACE_OBJECTS", "1"),
                ("GIT_REPLACE_REF_BASE", "refs/poison/replace/"),
                ("GIT_CONFIG_COUNT", "1"),
                ("GIT_CONFIG_KEY_0", "core.abbrev"),
                ("GIT_CONFIG_VALUE_0", "4"),
            ],
        );
        assert_script_ran(
            &poisoned_identity,
            "poisoned Git config and replacement environment",
        );
        assert_identity(
            &poisoned_identity,
            &replacement_head,
            &expected_describe,
            "poisoned Git config and replacement environment",
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
        let gitlink_describe = git_describe_dirty(&repo);
        assert_identity(
            &gitlink_run,
            &first,
            &gitlink_describe,
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
            let symlink_describe = git_describe_dirty(&repo);
            assert_identity(
                &symlink_run,
                &first,
                &symlink_describe,
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
        let index = PathBuf::from(git(
            &repo,
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
        ));
        let git_dir = PathBuf::from(git(&repo, &["rev-parse", "--absolute-git-dir"]));
        let fallback_describe = git(&repo, &["describe", "--tags", "--always"]);

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
            &format!("{fallback_describe}-dirty"),
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
            &format!("{fallback_describe}-dirty"),
            "oversized-index conservative identity",
        );
        assert_conservative_index_fallback(&oversized_run, "oversized index");
        assert_eq!(snapshot_index_family(&git_dir, &index), oversized_before);
        assert_no_private_index_copies(&target, "oversized index");
    }
}

#[test]
fn absent_shallow_metadata_appearing_invalidates_the_source_identity() {
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

    for (name, script, commit_variable, describe_variable) in scripts {
        let temp = aethyme_testkit::tmp_dir();
        let repo = temp.path().join("repo");
        let package_root = repo.join("packages/probe");
        let target = temp.path().join("shared-target");
        fs::create_dir_all(package_root.join("src")).unwrap();
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
            format!(
                "fn main() {{ println!(\"AETHYME_PROBE_IDENTITY:{{}}|{{}}\", env!(\"{commit_variable}\"), env!(\"{describe_variable}\")); }}\n"
            ),
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
        let expected_before = git_describe_dirty(&repo);
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
        let expected_after = git_describe_dirty(&repo);
        assert_ne!(
            expected_before, expected_after,
            "shallow boundary must change describe"
        );

        assert_identity(
            &shallow_run,
            &head,
            &expected_after,
            "shallow metadata appeared",
        );
        assert_script_ran(
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

    for (name, script, commit_variable, describe_variable) in scripts {
        let temp = aethyme_testkit::tmp_dir();
        let repo = temp.path().join("repo");
        let package_root = repo.join("packages/probe");
        let target = temp.path().join("shared-target");
        fs::create_dir_all(package_root.join("src")).unwrap();
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
            format!(
                "fn main() {{ println!(\"AETHYME_PROBE_IDENTITY:{{}}|{{}}\", env!(\"{commit_variable}\"), env!(\"{describe_variable}\")); }}\n"
            ),
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
        let describe = git_describe_dirty(&repo);
        assert_identity(&run, &commit, &describe, name);
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
    for (name, script, commit_variable, describe_variable) in scripts {
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
        assert!(
            stdout
                .lines()
                .any(|line| line == format!("cargo:rustc-env={describe_variable}=")),
            "missing best-effort empty describe export for {name}: {stdout}"
        );
        assert!(
            stdout.lines().any(|line| {
                line == "cargo:rerun-if-changed=.aethyme-source-identity-always-recheck"
            }),
            "no-Git build did not use conservative fallback for {name}: {stdout}"
        );
    }
}
