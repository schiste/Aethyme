//! Build script captures source identity and build time for `aethyme --version`.
//! Best-effort by design — building without Git or `date` still succeeds with
//! empty source identity and an `unknown` build date.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ALWAYS_RECHECK: &str = ".aethyme-source-identity-always-recheck";
const SOURCE_ROOT_ENV: &str = "AETHYME_BUILD_SOURCE_ROOT";
const MAX_TRACKED_LIST_BYTES: usize = 4 * 1024 * 1024;
const MAX_TRACKED_PATHS: usize = 32_768;
const MAX_WATCH_DIRECTIVE_BYTES: usize = 8 * 1024 * 1024;
const MAX_GIT_ADMIN_ENTRIES: usize = 4_096;
const MAX_GIT_INDEX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CONFIG_LIST_BYTES: usize = 1024 * 1024;

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

fn null_device() -> &'static str {
    if cfg!(windows) { "NUL" } else { "/dev/null" }
}

fn source_git_environment(command: &mut Command) {
    clear_git_environment(command);
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", null_device())
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_NO_REPLACE_OBJECTS", "1")
        .env("GIT_NO_LAZY_FETCH", "1");
}

fn git_command(manifest_dir: &Path, args: &[&str]) -> Option<Output> {
    let mut command = Command::new("git");
    source_git_environment(&mut command);
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
    source_git_environment(&mut command);
    let attributes_file = format!("core.attributesFile={}", null_device());
    command
        .env("GIT_INDEX_FILE", index_file)
        .current_dir(manifest_dir)
        .arg("--attr-source=HEAD")
        .args(["-c", attributes_file.as_str(), "-c", "core.fsmonitor=false"])
        .args(["-c", "core.untrackedCache=false"])
        .args(args);
    command.output().ok()?.status.code()
}

fn is_full_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn source_revision(manifest_dir: Option<&Path>) -> (String, String, bool) {
    let Some(manifest_dir) = manifest_dir else {
        return (String::new(), String::new(), true);
    };
    let Some(commit) = git_output(
        manifest_dir,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )
    .filter(|value| is_full_oid(value)) else {
        return (String::new(), String::new(), true);
    };

    // Screen configuration before diff: local includes can redirect Git to
    // unbounded inputs, clean filters can execute commands, and sparse
    // checkouts omit tracked paths. These modes use conservative identity and
    // always recheck rather than running an incomplete dirty scan.
    let Some((_, sparse_checkout)) = local_git_config_state(manifest_dir) else {
        return (commit.clone(), format!("{commit}-dirty"), true);
    };
    if sparse_checkout {
        return (commit.clone(), format!("{commit}-dirty"), true);
    }

    // Compare the worktree with the exact commit, against a private index.
    // The fixed HEAD attribute source prevents untracked .gitattributes files
    // from changing this build identity. Unsupported Git versions fail closed.
    let Some(index_file) = copied_git_index_for_diff(manifest_dir) else {
        return (commit.clone(), format!("{commit}-dirty"), true);
    };
    let diff_code = git_exit_code(
        manifest_dir,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--ignore-submodules=none",
            "--quiet",
            "HEAD",
            "--",
        ],
        &index_file.path,
    );
    match diff_code {
        Some(0) => (commit.clone(), commit, false),
        Some(1) => (commit.clone(), format!("{commit}-dirty"), false),
        // Exit codes other than 0 (clean) and 1 (differences), or failure to
        // start Git, leave dirty state unknown. Report conservatively and make
        // Cargo recheck the build script on the next invocation.
        _ => (commit.clone(), format!("{commit}-dirty"), true),
    }
}

fn git_stdout_bounded(manifest_dir: &Path, args: &[&str], limit: usize) -> Option<Vec<u8>> {
    let mut command = Command::new("git");
    source_git_environment(&mut command);
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

fn parse_config_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Some(true),
        "" | "false" | "no" | "off" | "0" => Some(false),
        _ => None,
    }
}

fn local_git_config_state(manifest_dir: &Path) -> Option<(bool, bool)> {
    // Disable include expansion so include/includeIf declarations themselves
    // remain visible. A local include can add an arbitrary config source or a
    // missing include can appear later; in either case use the conservative
    // always-recheck path instead of claiming a bounded config watch.
    let bytes = git_stdout_bounded(
        manifest_dir,
        &[
            "config",
            "--null",
            "--no-includes",
            "--show-origin",
            "--list",
        ],
        MAX_CONFIG_LIST_BYTES,
    )?;
    if !bytes.is_empty() && bytes.last() != Some(&0) {
        return None;
    }
    let fields = bytes
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() % 2 != 0 {
        return None;
    }

    let mut worktree_config = false;
    let mut sparse_checkout = false;
    for pair in fields.chunks_exact(2) {
        let origin = std::str::from_utf8(pair[0]).ok()?;
        if !origin.starts_with("file:") {
            return None;
        }
        let setting = std::str::from_utf8(pair[1]).ok()?;
        let (key, value) = setting.split_once('\n')?;
        let key = key.to_ascii_lowercase();
        if key.starts_with("filter.") {
            // A configured clean filter can execute arbitrary external code
            // while Git computes the dirty bit. Its full dependency surface
            // is not a bounded build input, so use the conservative fallback.
            return None;
        }
        if key == "include.path" || (key.starts_with("includeif.") && key.ends_with(".path")) {
            return None;
        }
        if key == "extensions.worktreeconfig" {
            worktree_config = parse_config_bool(value)?;
        } else if key == "core.sparsecheckout" {
            sparse_checkout = parse_config_bool(value)?;
        }
    }
    Some((worktree_config, sparse_checkout))
}

fn add_file_watch(path: &Path, allow_missing: bool, watches: &mut BTreeSet<PathBuf>) -> Option<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {
            watches.insert(path.to_path_buf());
            Some(())
        }
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => {
            watches.insert(path.to_path_buf());
            Some(())
        }
        _ => None,
    }
}

fn add_ref_directory_watch(
    common_dir: &Path,
    desired_parent: &Path,
    watches: &mut BTreeSet<PathBuf>,
) -> Option<()> {
    let relative = desired_parent.strip_prefix(common_dir).ok()?;
    let mut cursor = common_dir.to_path_buf();
    let mut nearest = common_dir.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return None;
        };
        cursor.push(component);
        match fs::symlink_metadata(&cursor) {
            Ok(metadata) if metadata.file_type().is_dir() => nearest = cursor.clone(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            _ => return None,
        }
    }
    if nearest == common_dir {
        return None;
    }
    watches.insert(nearest);
    Some(())
}

fn valid_ref_name(name: &str) -> bool {
    name.starts_with("refs/")
        && !name.contains('\\')
        && !name.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
        && name
            .split('/')
            .all(|component| !component.is_empty() && component != "." && component != "..")
        && !Path::new(name).is_absolute()
        && Path::new(name)
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn add_current_ref_watch(
    manifest_dir: &Path,
    git_dir: &Path,
    common_dir: &Path,
    commit: &str,
    watches: &mut BTreeSet<PathBuf>,
) -> Option<()> {
    let ref_format = git_output(manifest_dir, &["rev-parse", "--show-ref-format"])?;
    match ref_format.as_str() {
        "reftable" => {
            let reftable = common_dir.join("reftable");
            let metadata = fs::symlink_metadata(&reftable).ok()?;
            if !metadata.file_type().is_dir() {
                return None;
            }
            watches.insert(reftable);
            return Some(());
        }
        "files" => {}
        _ => return None,
    }

    let head_ref = git_output(manifest_dir, &["symbolic-ref", "--quiet", "HEAD"]);
    let head_path = git_dir.join("HEAD");
    let worktree_head = fs::read_to_string(&head_path).ok()?;
    let worktree_head = worktree_head.trim_end_matches(['\r', '\n']);
    let Some(head_ref) = head_ref else {
        return (worktree_head == commit).then_some(());
    };
    if !valid_ref_name(&head_ref) || worktree_head != format!("ref: {head_ref}") {
        return None;
    }
    let resolved = git_output(
        manifest_dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{head_ref}^{{commit}}"),
        ],
    )?;
    if resolved != commit {
        return None;
    }

    let loose_ref = common_dir.join(&head_ref);
    let parent = loose_ref.parent()?;
    let packed_refs = common_dir.join("packed-refs");
    match fs::symlink_metadata(&loose_ref) {
        Ok(metadata) if metadata.file_type().is_file() => {
            watches.insert(loose_ref);
            Some(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            add_file_watch(&packed_refs, false, watches)?;
            add_ref_directory_watch(common_dir, parent, watches)
        }
        _ => None,
    }
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
            // Gitlinks, symlinks, and special files use the conservative
            // fallback instead of making Cargo recursively scan them.
            return None;
        }
        watches.insert(path);
    }
    Some(())
}

fn source_identity_watch_paths(manifest_dir: &Path, commit: &str) -> Option<Vec<PathBuf>> {
    let repo_root = canonical_git_directory(manifest_dir, &["rev-parse", "--show-toplevel"])?;
    let git_dir = canonical_git_directory(manifest_dir, &["rev-parse", "--absolute-git-dir"])?;
    let common_dir = canonical_git_directory(
        manifest_dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;

    let (worktree_config_enabled, sparse_checkout) = local_git_config_state(manifest_dir)?;
    if sparse_checkout {
        return None;
    }

    // Cargo compares rerun-if-changed inputs by modification time. That is
    // insufficient for switching between linked worktrees whose `.git` pointer
    // files are older than the shared target's cached build output. The project
    // Cargo config supplies an exact root value that Cargo fingerprints as an
    // environment input; missing or mismatched config takes the conservative
    // always-recheck path in the caller.
    let configured_root = std::env::var_os(SOURCE_ROOT_ENV)?;
    let configured_root = fs::canonicalize(configured_root).ok()?;
    if configured_root != repo_root {
        return None;
    }

    let mut watches = BTreeSet::new();
    if git_dir != common_dir {
        add_file_watch(&git_dir.join("commondir"), false, &mut watches)?;
    }
    let head_path = git_dir.join("HEAD");
    add_file_watch(&head_path, false, &mut watches)?;
    add_file_watch(&git_dir.join("index"), false, &mut watches)?;
    add_file_watch(&common_dir.join("config"), false, &mut watches)?;
    let worktree_config = git_dir.join("config.worktree");
    let has_worktree_config = fs::symlink_metadata(&worktree_config).is_ok();
    if worktree_config_enabled || has_worktree_config {
        add_file_watch(&worktree_config, true, &mut watches)?;
    }

    let attributes = common_dir.join("info/attributes");
    match fs::symlink_metadata(&attributes) {
        Ok(metadata) if metadata.file_type().is_file() => {
            watches.insert(attributes);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let info = common_dir.join("info");
            let metadata = fs::symlink_metadata(&info).ok()?;
            if !metadata.file_type().is_dir() {
                return None;
            }
            watches.insert(info);
        }
        _ => return None,
    }

    add_current_ref_watch(manifest_dir, &git_dir, &common_dir, commit, &mut watches)?;
    add_tracked_files(&repo_root, &mut watches)?;

    let current_commit = git_output(
        manifest_dir,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
    )?;
    if current_commit != commit {
        return None;
    }

    let mut output_bytes = "cargo:rerun-if-changed=build.rs\n".len()
        + format!("cargo:rerun-if-env-changed={SOURCE_ROOT_ENV}\n").len();
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

fn emit_source_identity_reruns(
    manifest_dir: Option<&Path>,
    commit: &str,
    identity_uncertain: bool,
) {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={SOURCE_ROOT_ENV}");
    if identity_uncertain {
        println!("cargo:rerun-if-changed={ALWAYS_RECHECK}");
        return;
    }
    match manifest_dir.and_then(|directory| source_identity_watch_paths(directory, commit)) {
        Some(paths) => {
            for path in paths {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
        None => println!("cargo:rerun-if-changed={ALWAYS_RECHECK}"),
    }
}

fn build_date() -> String {
    let output = Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output();
    match output {
        Ok(output) if output.status.success() => String::from_utf8(output.stdout)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        _ => "unknown".to_owned(),
    }
}

fn main() {
    let manifest_dir = manifest_dir();
    let (commit, revision, identity_uncertain) = source_revision(manifest_dir.as_deref());
    println!("cargo:rustc-env=AETHYME_BUILD_DATE={}", build_date());
    // Keep the old variable as a compatibility alias. It no longer claims
    // git-describe tag/ancestry semantics; consumers should use REVISION.
    println!("cargo:rustc-env=AETHYME_GIT_REVISION={revision}");
    println!("cargo:rustc-env=AETHYME_GIT_DESCRIBE={revision}");
    println!("cargo:rustc-env=AETHYME_GIT_COMMIT={commit}");
    emit_source_identity_reruns(manifest_dir.as_deref(), &commit, identity_uncertain);
}
