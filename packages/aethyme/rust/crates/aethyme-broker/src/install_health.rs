//! Whether this machine's Aethyme installation is coherent and current.
//!
//! Two failures motivated this module, and neither announced itself.
//!
//! **The pair split.** `aethyme` and `aethyme-engine-cli` are one product in
//! two binaries and must be installed together. On a machine that develops
//! Aethyme they are installed by `cargo install --path`, once per session, from
//! whichever worktree that session happens to own — so two sessions installing
//! on the same afternoon leave a router from one branch beside an engine from
//! another. Both report a plausible version. Nothing anywhere compares them.
//!
//! **The stale release.** `aethyme update check` has always computed the right
//! answer, and nothing ever ran it.
//!
//! So the answers are reported where they are already free: `aethyme plugin
//! status`, which exists to say whether the installation works, and the
//! `SessionStart` hook, which fires at a turn boundary that costs no tokens.
//!
//! ## Reporting is the whole contract
//!
//! Nothing here installs, upgrades, or repairs anything, and nothing here
//! reaches the network. `aethyme update` keeps its stance that changing the
//! binaries is explicit and confirmed; this only ever prints a sentence and
//! names the command a person may choose to run.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::update::{self, UpdateAction, UpdateChannel};
use crate::update_cache;

/// Set to `off` to silence every notice and suppress the cache refresh.
pub const DISABLE_ENVIRONMENT_VARIABLE: &str = "AETHYME_UPDATE_CHECK";

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
/// A banner is `<program> <version> (<describe>)`, and the program name is the
/// one part guaranteed to differ between the two binaries. Everything from the
/// first digit-leading token onward is what must match:
/// `aethyme 0.7.17 (v0.7.17)` and `aethyme-engine-cli 0.7.17 (v0.7.17)` both
/// reduce to `0.7.17 (v0.7.17)`.
///
/// Comparing the whole thing, `describe` included, is deliberate. Two builds
/// from different commits of the same unreleased version share a `version` and
/// differ only in the `-N-g<sha>` suffix, and that suffix is precisely the
/// evidence of a split.
pub fn build_identity(banner: &str) -> Option<&str> {
    let banner = banner.trim();
    let offset = banner
        .char_indices()
        .find(|(index, character)| {
            character.is_ascii_digit()
                && (*index == 0 || banner[..*index].ends_with(char::is_whitespace))
        })
        .map(|(index, _)| index)?;
    Some(banner[offset..].trim())
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
    first_line_of(&resolve_on_path(binary)?.to_string_lossy(), &["--version"])
}

/// The pair as installed, read by asking both binaries on PATH.
///
/// The router is read from PATH rather than from this process: the shim, the
/// hooks and the next shell command all reach whatever PATH resolves, which on
/// a machine with more than one copy is not necessarily this one.
pub fn resolve_pair() -> PairState {
    let router = version_banner(ROUTER_BINARY);
    let engine = version_banner(ENGINE_BINARY);
    pair_state(
        router.as_deref().and_then(build_identity),
        engine.as_deref().and_then(build_identity),
    )
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
    if disabled() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    // The local half first: it needs no network, it is always accurate, and a
    // split pair is a worse problem than being one release behind.
    if let Some(warning) = pair_warning(&resolve_pair()) {
        warnings.push(format!("Aethyme: {warning}"));
    }
    match cached_release_warning(now_unix_ms) {
        Some(warning) => warnings.push(warning),
        None => prime_release_cache(now_unix_ms),
    }
    warnings
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
