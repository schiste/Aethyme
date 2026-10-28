//! Whether this machine's Aethyme installation is coherent and current.
//!
//! Four failures motivated this module, and none announced itself.
//!
//! **The pair split.** `aethyme` and `aethyme-engine-cli` are one product in
//! two binaries and must be installed together. On a machine that develops
//! Aethyme they are installed by `cargo install --path`, once per session, from
//! whichever worktree that session happens to own — so two sessions installing
//! on the same afternoon leave a router from one branch beside an engine from
//! another. Both report a plausible version. Nothing anywhere compares them.
//!
//! **The old process.** A new installation selector affects future launches,
//! not an agent or supervisor that already loaded an older CLI. Its build is
//! compared with the router selected by this process's PATH.
//!
//! **The stale release.** `aethyme update check` has always computed the right
//! answer, and nothing ever ran it.
//!
//! **The stale Claude plugin.** Claude Code caches the plugin's hook files by
//! `plugin.json` version. When the bundled hooks change without a version bump,
//! an installed copy can keep running the old behavior after the CLI updates.
//! The CLI now compares the installed files with the copies embedded at build
//! time and names the update or reinstall command when they differ.
//!
//! So the answers are reported where they are already free: `aethyme plugin
//! status`, which exists to say whether the installation works, and the
//! `SessionStart` hook, which fires at a turn boundary that costs no tokens.
//!
//! ## Reporting is the whole contract
//!
//! Nothing here installs, upgrades, or repairs anything. Plugin freshness is
//! checked from local files only, and the session hook never waits on a network.
//! `aethyme update` keeps its stance that changing binaries is explicit and
//! confirmed; this only ever prints a sentence and names the command a person
//! may choose to run.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::update::{self, UpdateAction, UpdateChannel};
use crate::update_cache;

/// Set to `off` to silence every notice and suppress the cache refresh.
pub const DISABLE_ENVIRONMENT_VARIABLE: &str = "AETHYME_UPDATE_CHECK";
const CLAUDE_PLUGIN_ROOT_ENVIRONMENT_VARIABLE: &str = "CLAUDE_PLUGIN_ROOT";
const CLAUDE_PLUGIN_REPAIR_COMMAND: &str = "claude plugin marketplace update aethyme && \
    claude plugin update aethyme@aethyme";
const CLAUDE_PLUGIN_REINSTALL_COMMAND: &str = "claude plugin uninstall aethyme@aethyme && \
    claude plugin install aethyme@aethyme";

// The command-side hook logic is distributed as a Claude plugin, separately
// from this binary. Embed the files the hook runs so a newly installed CLI can
// recognize a cached plugin that still carries older hook behavior.
const CLAUDE_PLUGIN_HOOK_FILES: &[(&str, &[u8])] = &[
    (
        "hooks/hooks.json",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../plugins/aethyme/hooks/hooks.json"
        )),
    ),
    (
        "hooks/aethyme-hook.sh",
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../plugins/aethyme/hooks/aethyme-hook.sh"
        )),
    ),
];

pub const ROUTER_BINARY: &str = "aethyme";
pub const ENGINE_BINARY: &str = "aethyme-engine-cli";

/// How the router and engine on this machine relate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairState {
    /// Both report the same build.
    Aligned(String),
    /// Both are installed and they disagree — the failure this module exists for.
    Split { router: String, engine: String },
    /// The router is installed and the engine is not.
    EngineMissing,
    /// At least one build could not be read, so no claim is possible.
    Unknown,
}

/// The build identity inside a `--version` banner.
///
/// A banner is `<program> <version> (build_revision=<oid>[-dirty] ...)`, and the program name is the
/// one part guaranteed to differ between the two binaries. Everything from the
/// first digit-leading token onward is what must match:
/// `aethyme 0.7.17 (v0.7.17)` and `aethyme-engine-cli 0.7.17 (v0.7.17)` both
/// reduce to `0.7.17 (v0.7.17)`. Newer banners append a build date after that
/// stable identity; it is deliberately omitted so separately installed
/// binaries built from the same source commit do not look split merely because
/// they were built seconds apart.
///
/// Comparing the whole thing, including the exact revision and dirty marker, is deliberate. Two builds
/// from different commits of the same unreleased version share a `version` and
/// differ in the full revision OID, which is precisely the evidence of a split.
pub fn build_identity(banner: &str) -> Option<&str> {
    let banner = banner.trim();
    let offset = banner
        .char_indices()
        .find(|(index, character)| {
            character.is_ascii_digit()
                && (*index == 0 || banner[..*index].ends_with(char::is_whitespace))
        })
        .map(|(index, _)| index)?;
    let identity = banner[offset..].trim();
    Some(
        identity
            .split_once(" build_date=")
            .map_or(identity, |(stable, _)| stable.trim_end()),
    )
}

/// Compare two build identities. Pure, so the interesting cases are testable
/// without installing anything.
pub fn pair_state(router: Option<&str>, engine: Option<&str>) -> PairState {
    match (router, engine) {
        (Some(router), Some(engine)) if router == engine => PairState::Aligned(router.to_string()),
        (Some(router), Some(engine)) => PairState::Split {
            router: router.to_string(),
            engine: engine.to_string(),
        },
        (Some(_), None) => PairState::EngineMissing,
        _ => PairState::Unknown,
    }
}

/// What to say about a pair, or `None` when it is fine.
///
/// `Unknown` is silent on purpose. A machine where a version banner cannot be
/// read has some other problem, and guessing at it here would put a warning in
/// front of every agent on a machine that may be perfectly healthy.
pub fn pair_warning(state: &PairState) -> Option<String> {
    match state {
        PairState::Aligned(_) | PairState::Unknown => None,
        PairState::EngineMissing => Some(format!(
            "`{ENGINE_BINARY}` is not on PATH. It ships with `{ROUTER_BINARY}` and is not \
             optional; install both: cargo install --locked --path \
             packages/aethyme/rust/crates/aethyme-cli && cargo install --locked --path \
             packages/aethyme/rust/crates/aethyme-engine"
        )),
        PairState::Split { router, engine } => Some(format!(
            "router and engine are from different builds ({ROUTER_BINARY} {router}, \
             {ENGINE_BINARY} {engine}). They are one product in two binaries and a split \
             pair can fail in ways neither version explains. Reinstall both from the same \
             tree, in one go: cargo install --locked --path \
             packages/aethyme/rust/crates/aethyme-cli && cargo install --locked --path \
             packages/aethyme/rust/crates/aethyme-engine"
        )),
    }
}

/// The first line of `<binary> --version`, when the binary is on PATH.
pub(crate) fn version_banner(binary: &str) -> Option<String> {
    version_banner_at(&resolve_on_path(binary)?)
}

fn version_banner_at(path: &Path) -> Option<String> {
    first_line_of(&path.to_string_lossy(), &["--version"])
}

/// The pair as installed, read by asking both binaries on PATH.
///
/// The router is read from PATH rather than from this process: the shim, the
/// hooks and the next shell command all reach whatever PATH resolves, which on
/// a machine with more than one copy is not necessarily this one.
fn resolve_pair_with_router_identity() -> (PairState, Option<String>, Option<PathBuf>) {
    let router_path = resolve_on_path(ROUTER_BINARY);
    let router = router_path
        .as_deref()
        .and_then(version_banner_at)
        .and_then(|banner| build_identity(&banner).map(str::to_owned));
    let engine = version_banner(ENGINE_BINARY);
    let engine = engine.as_deref().and_then(build_identity);
    (pair_state(router.as_deref(), engine), router, router_path)
}

pub fn resolve_pair() -> PairState {
    resolve_pair_with_router_identity().0
}

const MAX_KNOWN_INSTALL_ROOTS: usize = 5;
const MAX_ROOT_DISPLAY_CHARS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
struct KnownInstallRoot {
    path: PathBuf,
    selected_by_path: bool,
    has_router_entry: bool,
    has_engine_entry: bool,
}

fn known_install_root_paths(
    cargo_install_root: Option<&Path>,
    cargo_home: Option<&Path>,
    home: Option<&Path>,
    path_router: Option<&Path>,
) -> Vec<PathBuf> {
    let candidates = [
        cargo_install_root.map(|root| root.join("bin")),
        cargo_home.map(|root| root.join("bin")),
        home.map(|root| root.join(".cargo/bin")),
        home.map(|root| root.join(".local/bin")),
        path_router.and_then(|router| router.parent().map(Path::to_path_buf)),
    ];
    let mut roots = Vec::with_capacity(MAX_KNOWN_INSTALL_ROOTS);
    for path in candidates.into_iter().flatten() {
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}

fn direct_binary_entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.is_file() || metadata.file_type().is_symlink())
}

fn known_install_roots(path_router: Option<&Path>) -> Vec<KnownInstallRoot> {
    let cargo_install_root = std::env::var_os("CARGO_INSTALL_ROOT").map(PathBuf::from);
    let cargo_home = std::env::var_os("CARGO_HOME").map(PathBuf::from);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path_root = path_router.and_then(Path::parent);
    known_install_root_paths(
        cargo_install_root.as_deref(),
        cargo_home.as_deref(),
        home.as_deref(),
        path_router,
    )
    .into_iter()
    .filter_map(|path| {
        let has_router_entry = direct_binary_entry_exists(&path.join(ROUTER_BINARY));
        let has_engine_entry = direct_binary_entry_exists(&path.join(ENGINE_BINARY));
        (has_router_entry || has_engine_entry).then_some(KnownInstallRoot {
            selected_by_path: path_root == Some(path.as_path()),
            path,
            has_router_entry,
            has_engine_entry,
        })
    })
    .collect()
}

fn display_path_bounded(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    let mut characters = rendered.chars();
    let prefix = characters
        .by_ref()
        .take(MAX_ROOT_DISPLAY_CHARS)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn known_install_roots_summary(path_router: Option<&Path>) -> String {
    let roots = known_install_roots(path_router);
    if roots.is_empty() {
        return "none of the known roots has an Aethyme binary entry".into();
    }
    roots
        .iter()
        .map(|root| {
            let mut binaries = Vec::with_capacity(2);
            if root.has_router_entry {
                binaries.push(ROUTER_BINARY);
            }
            if root.has_engine_entry {
                binaries.push(ENGINE_BINARY);
            }
            let selection = if root.selected_by_path {
                "PATH-selected"
            } else {
                "not selected by this PATH"
            };
            format!(
                "{} [{}; {selection}]",
                display_path_bounded(&root.path),
                binaries.join(", ")
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn running_process_warning(
    running_build_banner: Option<&str>,
    path_router_identity: Option<&str>,
    path_router: Option<&Path>,
) -> Option<String> {
    let running_identity = build_identity(running_build_banner?)?;
    let path_identity = path_router_identity?;
    if running_identity == path_identity {
        return None;
    }
    let running_executable = std::env::current_exe()
        .ok()
        .map(|path| display_path_bounded(&path))
        .unwrap_or_else(|| "unavailable".into());
    let roots = known_install_roots_summary(path_router);
    let path_router_display = path_router
        .map(display_path_bounded)
        .unwrap_or_else(|| "unavailable".into());
    Some(format!(
        "the running Aethyme process uses build {running_identity} at {running_executable}, \
         while PATH selects build {path_identity} at {path_router_display}. A running process keeps \
         its loaded build when PATH or install selectors change; restart the \
         owning agent or supervisor after choosing the intended runtime. Known \
         install roots (direct entries only; \
         alternate binaries are not run): {}",
        roots
    ))
}

/// What to say about a newer release, from the cache only.
///
/// `None` covers every uninteresting case: up to date, no cached answer yet,
/// a downgrade, and an installation whose update path this cannot name.
pub fn cached_release_warning(now_unix_ms: i64) -> Option<String> {
    let (plan, fetch) = update::cached_update_plan(UpdateChannel::Stable, now_unix_ms)?;
    if matches!(
        plan.action,
        UpdateAction::UpToDate | UpdateAction::RefuseDowngrade
    ) {
        return None;
    }
    let command = plan.recommended_command.as_deref()?;
    let age = update_cache::describe_age(fetch.age_seconds);
    Some(format!(
        "Aethyme {} is available; this machine runs {}. Nothing has been changed — run it \
         when you want it: {command} (release information {age}; `aethyme update check \
         --refresh` re-asks)",
        plan.target_version, plan.current_version
    ))
}

/// Everything worth saying at the start of a session, in order of urgency.
///
/// Empty is the expected result on a healthy machine, and empty means the hook
/// prints nothing, which is what a turn boundary should normally cost.
pub fn session_start_warnings(now_unix_ms: i64) -> Vec<String> {
    session_start_warnings_with_build(now_unix_ms, None)
}

pub(crate) fn session_start_warnings_with_build(
    now_unix_ms: i64,
    running_build_banner: Option<&str>,
) -> Vec<String> {
    if disabled() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    // The local half first: it needs no network, it is always accurate, and a
    // split pair is a worse problem than being one release behind.
    let (pair, path_router_identity, path_router) = resolve_pair_with_router_identity();
    if let Some(warning) = pair_warning(&pair) {
        warnings.push(format!("Aethyme: {warning}"));
    }
    if let Some(warning) = running_process_warning(
        running_build_banner,
        path_router_identity.as_deref(),
        path_router.as_deref(),
    ) {
        warnings.push(format!("Aethyme: {warning}"));
    }
    if let Some(warning) =
        claude_plugin_warning_from_root(std::env::var_os(CLAUDE_PLUGIN_ROOT_ENVIRONMENT_VARIABLE))
    {
        warnings.push(format!("Aethyme: {warning}"));
    }
    match cached_release_warning(now_unix_ms) {
        Some(warning) => warnings.push(warning),
        None => prime_release_cache(now_unix_ms),
    }
    warnings
}

/// What to say when Claude Code's cached plugin cannot run the hooks this
/// binary was built to expect. No plugin root means another agent surface (or
/// a direct CLI invocation), so it is intentionally silent.
fn claude_plugin_warning(plugin_root: Option<&std::path::Path>) -> Option<String> {
    let plugin_root = plugin_root?;
    let current = CLAUDE_PLUGIN_HOOK_FILES
        .iter()
        .all(|(relative_path, bundled)| {
            installed_plugin_file_matches(&plugin_root.join(relative_path), bundled)
        });
    if current {
        None
    } else {
        Some(format!(
            "Claude Code's installed Aethyme plugin is missing or has stale hook files. \
             Update it with `{CLAUDE_PLUGIN_REPAIR_COMMAND}`. If Claude reports it is \
             already current, reinstall it with `{CLAUDE_PLUGIN_REINSTALL_COMMAND}`."
        ))
    }
}

// A hook notice must stay cheap even if the plugin installation is damaged.
// Bound reads by the embedded file and never block opening a Unix FIFO.
fn installed_plugin_file_matches(path: &std::path::Path, expected: &[u8]) -> bool {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let Ok(file) = options.open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    if !metadata.is_file() || metadata.len() != expected.len() as u64 {
        return false;
    }
    let mut installed = Vec::with_capacity(expected.len());
    file.take(expected.len() as u64 + 1)
        .read_to_end(&mut installed)
        .is_ok()
        && installed == expected
}

fn claude_plugin_warning_from_root(plugin_root: Option<std::ffi::OsString>) -> Option<String> {
    let plugin_root = plugin_root.map(PathBuf::from);
    claude_plugin_warning(plugin_root.as_deref())
}

fn disabled() -> bool {
    disabled_by(std::env::var(DISABLE_ENVIRONMENT_VARIABLE).ok().as_deref())
}

/// Split out from the environment read so the policy is testable: setting a
/// process-wide variable inside a test races every other test in the binary.
fn disabled_by(value: Option<&str>) -> bool {
    value.is_some_and(|value| matches!(value.trim(), "0" | "off" | "false" | "no"))
}

/// Warm the manifest cache for the *next* session, in the background.
///
/// The alternative was fetching here, and a `SessionStart` hook that waits on
/// GitHub is a hook that stalls the agent on a slow network and fails on no
/// network. So a cold cache stays silent this time and asks off to the side;
/// by the next session the answer is on disk and free to read.
///
/// This does not weaken `aethyme update`'s rule that nothing installs in the
/// background. What runs detached is `update check`, which downloads a manifest
/// and prints a verdict. Nothing it does can change a binary.
fn prime_release_cache(now_unix_ms: i64) {
    let Ok(url) = update::manifest_discovery_url(UpdateChannel::Stable) else {
        return;
    };
    if !update_cache::needs_refresh(&url, now_unix_ms) {
        return;
    }
    // Without this, a machine that is offline retries at every single session
    // start forever, because a failed fetch writes no entry to expire.
    if !update_cache::claim_refresh_attempt(&url, now_unix_ms) {
        return;
    }
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    let _ = Command::new(executable)
        .args(["update", "check", "--refresh", "--json"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
}

pub(crate) fn resolve_on_path(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate)
                .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}

pub(crate) fn first_line_of(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_install_root_candidates_are_fixed_and_deduplicated() {
        let home = PathBuf::from("/home/example");
        let cargo_home = home.join(".cargo");
        let cargo_install_root = PathBuf::from("/opt/aethyme-cargo-root");
        let path_router = PathBuf::from("/opt/selected-bin/aethyme");

        let roots = known_install_root_paths(
            Some(&cargo_install_root),
            Some(&cargo_home),
            Some(&home),
            Some(&path_router),
        );

        assert_eq!(
            roots,
            vec![
                PathBuf::from("/opt/aethyme-cargo-root/bin"),
                PathBuf::from("/home/example/.cargo/bin"),
                PathBuf::from("/home/example/.local/bin"),
                PathBuf::from("/opt/selected-bin"),
            ]
        );
        assert!(roots.len() <= MAX_KNOWN_INSTALL_ROOTS);
    }

    fn write_bundled_plugin_hooks(root: &std::path::Path) {
        for (relative_path, bundled) in CLAUDE_PLUGIN_HOOK_FILES {
            let path = root.join(relative_path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bundled).unwrap();
        }
    }

    #[test]
    fn plugin_hook_comparison_refuses_oversized_or_nonregular_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("hook");
        std::fs::write(&path, "larger than the embedded hook").unwrap();
        assert!(!installed_plugin_file_matches(&path, b"small"));
        assert!(!installed_plugin_file_matches(root.path(), b""));
        std::fs::write(&path, b"small").unwrap();
        assert!(installed_plugin_file_matches(&path, b"small"));
        std::fs::write(&path, b"other").unwrap();
        assert!(!installed_plugin_file_matches(&path, b"small"));
    }

    #[test]
    fn a_current_claude_plugin_is_silent() {
        let root = tempfile::tempdir().unwrap();
        write_bundled_plugin_hooks(root.path());

        assert_eq!(
            claude_plugin_warning_from_root(Some(root.path().as_os_str().to_os_string())),
            None
        );
    }

    #[test]
    fn a_missing_claude_plugin_root_is_silent() {
        assert_eq!(claude_plugin_warning_from_root(None), None);
    }

    #[test]
    fn stale_or_incomplete_claude_hooks_name_update_and_reinstall_commands() {
        let root = tempfile::tempdir().unwrap();
        write_bundled_plugin_hooks(root.path());
        std::fs::write(root.path().join("hooks/hooks.json"), b"stale hooks").unwrap();

        let plugin_root = Some(root.path().as_os_str().to_os_string());
        let warning =
            claude_plugin_warning_from_root(plugin_root.clone()).expect("stale plugin warning");
        assert!(warning.contains(CLAUDE_PLUGIN_REPAIR_COMMAND));
        assert!(warning.contains(CLAUDE_PLUGIN_REINSTALL_COMMAND));

        std::fs::remove_file(root.path().join("hooks/aethyme-hook.sh")).unwrap();
        let warning =
            claude_plugin_warning_from_root(plugin_root).expect("incomplete plugin warning");
        assert!(warning.contains("missing or has stale hook files"));
    }

    #[test]
    fn the_program_name_is_dropped_and_the_build_kept() {
        assert_eq!(
            build_identity("aethyme 0.7.17 (v0.7.17)"),
            Some("0.7.17 (v0.7.17)")
        );
        assert_eq!(
            build_identity("aethyme-engine-cli 0.7.17 (v0.7.17)"),
            Some("0.7.17 (v0.7.17)")
        );
    }

    #[test]
    fn build_date_is_not_part_of_the_pair_identity() {
        let router = build_identity(
            "aethyme 0.7.18 (build_revision=abcdef0123456789 build_commit=abcdef0123456789) build_date=2026-09-14T10:00:00Z",
        );
        let engine = build_identity(
            "aethyme-engine-cli 0.7.18 (build_revision=abcdef0123456789 build_commit=abcdef0123456789) build_date=2026-09-14T10:00:01Z",
        );
        assert_eq!(router, engine);
        assert_eq!(
            router,
            Some("0.7.18 (build_revision=abcdef0123456789 build_commit=abcdef0123456789)")
        );
    }

    /// A banner that is only a version is still a banner.
    #[test]
    fn a_bare_version_is_its_own_identity() {
        assert_eq!(build_identity("0.7.17"), Some("0.7.17"));
        assert_eq!(build_identity("  0.7.17  "), Some("0.7.17"));
    }

    /// A digit inside the program name must not be mistaken for the version.
    #[test]
    fn a_digit_inside_a_word_does_not_start_the_version() {
        assert_eq!(
            build_identity("aethyme2 0.7.17 (v0.7.17)"),
            Some("0.7.17 (v0.7.17)")
        );
    }

    #[test]
    fn a_banner_without_a_version_yields_nothing() {
        assert_eq!(build_identity("aethyme unknown"), None);
        assert_eq!(build_identity(""), None);
    }

    /// The exact skew observed on the maintainer's machine: one version, two
    /// commits. A check that compared only `version` would have called this
    /// aligned.
    #[test]
    fn two_builds_of_one_version_from_different_commits_are_a_split() {
        let state = pair_state(
            Some("0.7.16 (v0.7.16-8-gf74bf96b)"),
            Some("0.7.16 (v0.7.16-6-g080d3c8a)"),
        );
        assert!(matches!(state, PairState::Split { .. }), "{state:?}");
        let warning = pair_warning(&state).expect("a split pair must be reported");
        assert!(warning.contains("v0.7.16-8-gf74bf96b"), "{warning}");
        assert!(warning.contains("v0.7.16-6-g080d3c8a"), "{warning}");
        assert!(warning.contains("aethyme-cli"), "{warning}");
        assert!(warning.contains("aethyme-engine"), "{warning}");
    }

    #[test]
    fn a_matched_pair_says_nothing() {
        let state = pair_state(Some("0.7.17 (v0.7.17)"), Some("0.7.17 (v0.7.17)"));
        assert_eq!(state, PairState::Aligned("0.7.17 (v0.7.17)".into()));
        assert_eq!(pair_warning(&state), None);
    }

    #[test]
    fn a_missing_engine_is_named_rather_than_called_a_mismatch() {
        let state = pair_state(Some("0.7.17 (v0.7.17)"), None);
        assert_eq!(state, PairState::EngineMissing);
        let warning = pair_warning(&state).expect("a missing engine must be reported");
        assert!(warning.contains(ENGINE_BINARY), "{warning}");
        assert!(warning.contains("not optional"), "{warning}");
    }

    /// Nothing readable means no claim, not a guess.
    #[test]
    fn an_unreadable_pair_is_silent() {
        assert_eq!(pair_state(None, None), PairState::Unknown);
        assert_eq!(pair_state(None, Some("0.7.17")), PairState::Unknown);
        assert_eq!(pair_warning(&PairState::Unknown), None);
    }

    /// Only an explicit off value opts out. `1` reads as "on" to most people,
    /// and silently disabling on it would be the worst possible reading.
    #[test]
    fn only_an_explicit_off_value_opts_out() {
        for value in ["0", "off", "false", "no", " off "] {
            assert!(disabled_by(Some(value)), "{value:?} must opt out");
        }
        for value in ["1", "on", "true", "yes", ""] {
            assert!(!disabled_by(Some(value)), "{value:?} must not opt out");
        }
        assert!(!disabled_by(None), "unset must not opt out");
    }
}
