//! Build script captures source identity so broker diagnostics can identify
//! the exact checkout used to build the library.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ALWAYS_RECHECK: &str = ".aethyme-source-identity-always-recheck";
const MAX_TRACKED_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_TRACKED_PATHS: usize = 32_768;
const MAX_WATCH_DIRECTIVE_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIT_ADMIN_ENTRIES: usize = 4_096;
const MAX_GIT_INDEX_BYTES: u64 = 16 * 1024 * 1024;

fn manifest_dir() -> Option<PathBuf> {
    let path = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR")?);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    fs::canonicalize(absolute).ok()
}

fn clear_git_environment(command: &mut Command) {
    // Git environment variables can redirect repository discovery, refs, or
    // configuration. Remove every GIT_* override while preserving PATH and
    // the rest of Cargo's normal build environment.
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
}

fn git_command(manifest_dir: &Path, args: &[&str]) -> Option<Output> {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    command.current_dir(manifest_dir).args(args);
    let output = command.output().ok()?;
    output.status.success().then_some(output)
}

fn git_output(manifest_dir: &Path, args: &[&str]) -> Option<String> {
    let output = git_command(manifest_dir, args)?;
    let text = String::from_utf8(output.stdout).ok()?;
    let trimmed = text.trim_end_matches(['\r', '\n']);
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

struct TemporaryGitIndex {
    path: PathBuf,
}

impl Drop for TemporaryGitIndex {
    fn drop(&mut self) {
        let mut lock_path = self.path.as_os_str().to_os_string();
        lock_path.push(".lock");
        let _remove_lock_result = fs::remove_file(PathBuf::from(lock_path));
        let _remove_index_result = fs::remove_file(&self.path);
    }
}

fn copied_git_index_for_diff(manifest_dir: &Path) -> Option<TemporaryGitIndex> {
    // A split index links to sharedindex.* in the real Git directory. Do not
    // ask Git to resolve that link: reading it can refresh watched metadata.
    // Rebuild conservatively until a non-split index is available.
    let git_dir = canonical_git_directory(manifest_dir, &["rev-parse", "--absolute-git-dir"])?;
    let mut git_admin_entries = 0usize;
    for entry in fs::read_dir(&git_dir).ok()? {
        let entry = entry.ok()?;
        git_admin_entries = git_admin_entries.checked_add(1)?;
        if git_admin_entries > MAX_GIT_ADMIN_ENTRIES {
            return None;
        }
        if entry
            .file_name()
            .to_string_lossy()
            .starts_with("sharedindex.")
        {
            return None;
        }
    }

    let source_index_path = git_dir.join("index");
    if !fs::symlink_metadata(&source_index_path)
        .ok()?
        .file_type()
        .is_file()
    {
        return None;
    }
    let source = fs::File::open(source_index_path).ok()?;
    let source_metadata = source.metadata().ok()?;
    if !source_metadata.is_file() || source_metadata.len() > MAX_GIT_INDEX_BYTES {
        return None;
    }

    let out_dir = fs::canonicalize(PathBuf::from(std::env::var_os("OUT_DIR")?)).ok()?;
    let path = out_dir.join(format!(
        ".aethyme-source-identity-index-{}",
        std::process::id()
    ));
    let mut copied_index = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    let temporary_index = TemporaryGitIndex { path };
    let mut limited_source = source.take(MAX_GIT_INDEX_BYTES + 1);
    let copy_result = std::io::copy(&mut limited_source, &mut copied_index);
    drop(copied_index);
    let copied_bytes = copy_result.ok()?;
    if copied_bytes != source_metadata.len() || copied_bytes > MAX_GIT_INDEX_BYTES {
        return None;
    }
    Some(temporary_index)
}

fn git_exit_code(manifest_dir: &Path, args: &[&str], index_file: &Path) -> Option<i32> {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_INDEX_FILE", index_file)
        .current_dir(manifest_dir)
        .args(args);
    command.output().ok()?.status.code()
}

fn git_describe(manifest_dir: Option<&Path>) -> (String, bool) {
    let Some(manifest_dir) = manifest_dir else {
        return (String::new(), true);
    };
    let Some(describe) = git_output(manifest_dir, &["describe", "--tags", "--always"]) else {
        return (String::new(), true);
    };

    // Keep Git's exact diff semantics without allowing its stat-only index
    // refresh to mutate the real checkout watched by Cargo. Fail closed for
    // split indexes and any copy or Git error.
    let Some(index_file) = copied_git_index_for_diff(manifest_dir) else {
        return (format!("{describe}-dirty"), true);
    };
    match git_exit_code(
        manifest_dir,
        &[
            "-c",
            "core.splitIndex=false",
            "diff",
            "--no-ext-diff",
            "--quiet",
            "HEAD",
            "--",
        ],
        &index_file.path,
    ) {
        Some(0) => (describe, false),
        Some(1) => (format!("{describe}-dirty"), false),
        // Exit codes other than 0 (clean) and 1 (differences), or failure to
        // start Git, leave dirty state unknown. Report conservatively and make
        // Cargo recheck the build script on the next invocation.
        _ => (format!("{describe}-dirty"), true),
    }
}

fn git_stdout_bounded(manifest_dir: &Path, args: &[&str], limit: usize) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    clear_git_environment(&mut command);
    command
        .current_dir(manifest_dir)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let mut bytes = Vec::new();
    if stdout
        .by_ref()
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .is_err()
    {
        let _kill_result = child.kill();
        let _wait_result = child.wait();
        return None;
    }
    if bytes.len() > limit {
        let _kill_result = child.kill();
        let _wait_result = child.wait();
        return None;
    }
    child.wait().ok()?.success().then_some(bytes)
}

fn canonical_git_directory(manifest_dir: &Path, args: &[&str]) -> Option<PathBuf> {
    let reported = PathBuf::from(git_output(manifest_dir, args)?);
    let absolute = if reported.is_absolute() {
        reported
    } else {
        manifest_dir.join(reported)
    };
    let canonical = fs::canonicalize(absolute).ok()?;
    let metadata = fs::symlink_metadata(&canonical).ok()?;
    metadata.file_type().is_dir().then_some(canonical)
}

fn add_tracked_files(repo_root: &Path, watches: &mut BTreeSet<PathBuf>) -> Option<()> {
    let output = git_stdout_bounded(
        repo_root,
        &["ls-files", "--cached", "--full-name", "-z"],
        MAX_TRACKED_LIST_BYTES,
    )?;
    if !output.is_empty() && output.last() != Some(&0) {
        return None;
    }

    let mut count = 0usize;
    for name in output
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
    {
        count += 1;
        if count > MAX_TRACKED_PATHS {
            return None;
        }
        let name = std::str::from_utf8(name).ok()?;
        if name.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
            return None;
        }
        let relative = Path::new(name);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return None;
        }

        let components: Vec<_> = relative.components().collect();
        if components.is_empty() {
            return None;
        }
        let mut parent = repo_root.to_path_buf();
        for component in components.iter().take(components.len() - 1) {
            parent.push(component.as_os_str());
            let metadata = fs::symlink_metadata(&parent).ok()?;
            if !metadata.file_type().is_dir() {
                return None;
            }
        }

        let path = repo_root.join(relative);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.file_type().is_file() {
            // Cargo recursively scans directory inputs. Gitlinks, symlinks,
            // and special files use the conservative fallback instead.
            return None;
        }
        watches.insert(path);
    }
    Some(())
}

fn source_identity_watch_paths(manifest_dir: &Path) -> Option<Vec<PathBuf>> {
    let repo_root = canonical_git_directory(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let git_dir = canonical_git_directory(manifest_dir, &["rev-parse", "--absolute-git-dir"])?;
    let common_dir = canonical_git_directory(
        manifest_dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;

    let mut watches = BTreeSet::new();
    let git_marker = repo_root.join(".git");
    match fs::symlink_metadata(&git_marker) {
        Ok(metadata) if metadata.file_type().is_file() => {
            // A linked-worktree marker distinguishes checkouts sharing one
            // Cargo target directory. Its contents are not followed as a path.
            watches.insert(git_marker);
        }
        Ok(metadata) if metadata.file_type().is_dir() => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Ok(_) | Err(_) => return None,
    }

    // Watching the validated real Git administrative directories covers files
    // that do not exist yet (including shallow, packed-refs, refs/reftable,
    // and per-worktree metadata), without maintaining a fragile optional-file
    // inventory. Cargo recursively scans these directories. This deliberately
    // includes objects, logs, hooks, and other worktree metadata; that cost is
    // broad and is not bounded by the inventory limits below.
    watches.insert(git_dir);
    watches.insert(common_dir);
    add_tracked_files(&repo_root, &mut watches)?;

    let mut output_bytes = "cargo:rerun-if-changed=build.rs\n".len();
    for path in &watches {
        let text = path.to_str()?;
        if text.bytes().any(|byte| byte < 0x20 || byte == 0x7f) {
            return None;
        }
        output_bytes = output_bytes.checked_add(
            "cargo:rerun-if-changed="
                .len()
                .checked_add(text.len())?
                .checked_add(1)?,
        )?;
        if output_bytes > MAX_WATCH_DIRECTIVE_BYTES {
            return None;
        }
    }
    Some(watches.into_iter().collect())
}

fn emit_source_identity_reruns(manifest_dir: Option<&Path>, identity_uncertain: bool) {
    println!("cargo:rerun-if-changed=build.rs");
    if identity_uncertain {
        println!("cargo:rerun-if-changed={ALWAYS_RECHECK}");
        return;
    }
    match manifest_dir.and_then(source_identity_watch_paths) {
        Some(paths) => {
            for path in paths {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
        None => println!("cargo:rerun-if-changed={ALWAYS_RECHECK}"),
    }
}

fn main() {
    let manifest_dir = manifest_dir();
    let (describe, identity_uncertain) = git_describe(manifest_dir.as_deref());
    let commit = manifest_dir
        .as_deref()
        .and_then(|directory| git_output(directory, &["rev-parse", "HEAD"]));
    let identity_uncertain = identity_uncertain || commit.is_none();
    let commit = commit.unwrap_or_default();
    println!("cargo:rustc-env=AETHYME_BROKER_GIT_DESCRIBE={describe}");
    println!("cargo:rustc-env=AETHYME_BROKER_GIT_COMMIT={commit}");
    emit_source_identity_reruns(manifest_dir.as_deref(), identity_uncertain);
}
