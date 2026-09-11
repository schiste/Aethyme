//! `aethyme plugin` — install the agent-surface plugin, and name the
//! version skew that makes an installed one do nothing.
//!
//! The plugin ships four hook registrations and a shim; the CLI ships
//! every rule those hooks apply. That split is deliberate — see
//! [`crate::agent_hook`] — and it has exactly one failure mode: a plugin
//! sitting next to a CLI that predates `aethyme hook`. The shim is
//! required to stay silent on stdout, because stdout is the envelope
//! slot, so the symptom of that skew is *nothing at all*: no error, no
//! context, no clue that anything is wrong. A user reads it as "the
//! plugin does not work" and has nowhere to look.
//!
//! Two commands answer that. `status` compares the CLI the shim will
//! actually reach — the one on `PATH`, which is not necessarily this
//! process — against the floor below, and says which side is behind.
//! `install` removes the step where a human wires two package managers
//! together by hand and then discovers the skew a day later.
//!
//! `install` plans before it runs, for the reason `broker ship` does:
//! the work is a handful of commands against tools this process does not
//! own, and showing them costs less than explaining afterwards what
//! happened to someone's global configuration.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::install_health::{self, PairState, first_line_of, resolve_on_path};

/// Oldest CLI that answers `aethyme hook`.
///
/// Below this the shim's fallback path runs and the agent surface is
/// told nothing, so an installed plugin is inert rather than broken —
/// which is worse, because inert leaves no evidence. Bump this only when
/// the plugin starts requiring a subcommand a released CLI lacks; it is
/// a floor on the *contract*, not a mirror of the current version.
pub const MIN_HOOK_CLI_VERSION: &str = "0.7.17";

/// Where the marketplace lives when the caller does not say.
pub const DEFAULT_MARKETPLACE_SOURCE: &str = "schiste/Aethyme";

/// `<plugin>@<marketplace>`, the spelling both surfaces accept.
pub const PLUGIN_ID: &str = "aethyme@aethyme";

/// The `name` field of `.claude-plugin/marketplace.json`. Marketplace
/// subcommands address a configured marketplace by this, not by URL.
pub const MARKETPLACE_NAME: &str = "aethyme";

/// An agent surface that can install plugins in this format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Surface {
    Codex,
    Claude,
}

impl Surface {
    pub const ALL: [Surface; 2] = [Surface::Codex, Surface::Claude];

    /// The executable to drive. Both ship a non-interactive `plugin`
    /// namespace; neither is required to be present.
    pub fn binary(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Codex => "Codex",
            Self::Claude => "Claude Code",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "codex" => Some(Self::Codex),
            "claude" | "claude-code" => Some(Self::Claude),
            _ => None,
        }
    }
}

/// One command in a plan, and whether the plan survives its failure.
///
/// `tolerate_failure` is not laziness. Adding a marketplace that is
/// already configured is an error on both surfaces and a no-op in
/// intent, and refusing to make `install` idempotent would mean the
/// second run of a setup script fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub surface: Surface,
    pub argv: Vec<String>,
    pub tolerate_failure: bool,
    pub purpose: &'static str,
}

impl Step {
    fn new(surface: Surface, args: &[&str], tolerate_failure: bool, purpose: &'static str) -> Self {
        let mut argv = vec![surface.binary().to_string()];
        argv.extend(args.iter().map(|arg| (*arg).to_string()));
        Self {
            surface,
            argv,
            tolerate_failure,
            purpose,
        }
    }

    pub fn rendered(&self) -> String {
        self.argv.join(" ")
    }
}

/// Register the marketplace, refresh it, then install the plugin.
///
/// The refresh between the two is what makes a re-run pick up a new
/// plugin version: `add` fails when the marketplace is already
/// configured, so without it a second `install` would install whatever
/// snapshot was cloned the first time.
pub fn plan_install(surface: Surface, source: &str) -> Vec<Step> {
    let mut steps = vec![
        Step::new(
            surface,
            &["plugin", "marketplace", "add", source],
            true,
            "register the marketplace (already registered is not an error)",
        ),
        Step::new(
            surface,
            &[
                "plugin",
                "marketplace",
                match surface {
                    Surface::Codex => "upgrade",
                    Surface::Claude => "update",
                },
                MARKETPLACE_NAME,
            ],
            true,
            "refresh the snapshot so a re-run installs the current version",
        ),
    ];
    steps.push(match surface {
        Surface::Codex => Step::new(
            surface,
            &["plugin", "add", PLUGIN_ID],
            false,
            "install the plugin",
        ),
        Surface::Claude => Step::new(
            surface,
            &["plugin", "install", PLUGIN_ID, "--scope", "user"],
            false,
            "install the plugin for every project on this machine",
        ),
    });
    steps
}

/// Uninstall, then drop the marketplace. Both tolerate absence: removing
/// what is not there is the state the caller asked for.
pub fn plan_remove(surface: Surface) -> Vec<Step> {
    vec![
        match surface {
            Surface::Codex => Step::new(
                surface,
                &["plugin", "remove", MARKETPLACE_NAME],
                true,
                "uninstall the plugin",
            ),
            Surface::Claude => Step::new(
                surface,
                &["plugin", "uninstall", PLUGIN_ID],
                true,
                "uninstall the plugin",
            ),
        },
        Step::new(
            surface,
            &["plugin", "marketplace", "remove", MARKETPLACE_NAME],
            true,
            "forget the marketplace",
        ),
    ]
}

/// What one surface currently has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceStatus {
    pub surface: Surface,
    pub cli_present: bool,
    pub plugin_version: Option<String>,
    pub enabled: Option<bool>,
}

impl SurfaceStatus {
    fn installed(&self) -> bool {
        self.plugin_version.is_some()
    }
}

/// The CLI the shim will actually reach, which is a different question
/// from the version of the process asking.
///
/// `answers_hook` is the authority here and `version` is only the
/// explanation. A version string cannot answer the question we actually
/// care about: 0.7.16 built from a checkout that already contains `hook`
/// has it, and the release tagged 0.7.16 does not. Asking the binary is
/// exact where comparing is a proxy, and the proxy is wrong for precisely
/// the people most likely to run this -- anyone with a development build
/// on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCli {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    /// `None` when there is no CLI to ask.
    pub answers_hook: Option<bool>,
}

impl ResolvedCli {
    /// What the version string alone would conclude. Reported, not obeyed.
    fn meets_floor(&self) -> Option<bool> {
        let found = parse_version(self.version.as_deref()?)?;
        let floor = parse_version(MIN_HOOK_CLI_VERSION)?;
        Some(compare_versions(&found, &floor) != Ordering::Less)
    }
}

/// Ask a CLI whether it serves `hook`, the way the shim finds out: run it.
///
/// A capable CLI exits 0 on an unrecognized event without reaching the
/// broker, so the probe writes nothing anywhere; one that predates the
/// subcommand exits 2 from the unknown-subcommand path. Stdin is closed
/// rather than inherited -- the real shim pipes an event in, and this
/// must not consume the caller's.
fn probe_hook(path: &Path) -> Option<bool> {
    let status = Command::new(path)
        .arg("hook")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(status.success())
}

/// Split a version out of anything that contains one.
///
/// Accepts the CLI's own `aethyme 0.7.17 (v0.7.17-3-gabc1234)` as
/// readily as a bare `0.7.17`: the first whitespace-separated token that
/// starts with a digit wins, and each dot-separated component
/// contributes its leading digits. `0.7.17-rc1` and `0.7.17` compare
/// equal, which is the conservative direction for a floor — a release
/// candidate of the version that introduced a subcommand has it.
pub fn parse_version(text: &str) -> Option<Vec<u32>> {
    let token = text
        .split_whitespace()
        .find(|token| token.starts_with(|c: char| c.is_ascii_digit()))?;
    Some(
        token
            .split('.')
            .map(|part| {
                part.chars()
                    .take_while(|c| c.is_ascii_digit())
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .collect(),
    )
}

/// Compare component-wise, treating a missing component as zero so
/// `0.8` and `0.8.0` are the same version.
pub fn compare_versions(left: &[u32], right: &[u32]) -> Ordering {
    for index in 0..left.len().max(right.len()) {
        let mine = left.get(index).copied().unwrap_or(0);
        let theirs = right.get(index).copied().unwrap_or(0);
        match mine.cmp(&theirs) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    Ordering::Equal
}

/// Find the plugin in either surface's `plugin list --json`.
///
/// The two shapes differ — Codex nests under `installed` and keys the id
/// `pluginId`, Claude Code returns a bare array keyed `id` — and both
/// are free to change. Walking the tree for the id costs nothing and
/// survives a reshuffle that a typed deserialiser would not.
fn find_plugin(value: &serde_json::Value) -> Option<(Option<String>, Option<bool>)> {
    match value {
        serde_json::Value::Object(map) => {
            let id = map
                .get("pluginId")
                .or_else(|| map.get("id"))
                .and_then(serde_json::Value::as_str);
            if id == Some(PLUGIN_ID) {
                return Some((
                    map.get("version")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_string),
                    map.get("enabled").and_then(serde_json::Value::as_bool),
                ));
            }
            map.values().find_map(find_plugin)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_plugin),
        _ => None,
    }
}

/// First executable named `name` on `PATH`, resolved the way a shell
/// would — which is the way the shim will resolve it.
fn resolved_cli() -> ResolvedCli {
    let path = resolve_on_path("aethyme");
    let version = path
        .as_ref()
        .and_then(|path| first_line_of(&path.to_string_lossy(), &["--version"]));
    let answers_hook = path.as_deref().and_then(probe_hook);
    ResolvedCli {
        path,
        version,
        answers_hook,
    }
}

fn surface_status(surface: Surface) -> SurfaceStatus {
    let Some(binary) = resolve_on_path(surface.binary()) else {
        return SurfaceStatus {
            surface,
            cli_present: false,
            plugin_version: None,
            enabled: None,
        };
    };
    let listing = Command::new(&binary)
        .args(["plugin", "list", "--json"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| serde_json::from_slice::<serde_json::Value>(&output.stdout).ok());
    let found = listing.as_ref().and_then(find_plugin);
    SurfaceStatus {
        surface,
        cli_present: true,
        plugin_version: found.as_ref().and_then(|(version, _)| version.clone()),
        enabled: found.and_then(|(_, enabled)| enabled),
    }
}

fn selected_surfaces(args: &[String]) -> Result<Vec<Surface>, String> {
    let Some(value) = flag_value(args, "--surface") else {
        return Ok(Surface::ALL.to_vec());
    };
    if value == "all" {
        return Ok(Surface::ALL.to_vec());
    }
    Surface::parse(&value)
        .map(|surface| vec![surface])
        .ok_or_else(|| format!("unknown surface '{value}'; expected codex, claude, or all"))
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(&format!("{flag}=")) {
            return Some(value.to_string());
        }
    }
    None
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|arg| arg == flag)
}

/// Render the skew warning, or `None` when there is nothing to warn
/// about. Separated from printing so the wording is testable.
///
/// The verdict follows the probe; the floor supplies the remedy, since
/// "upgrade" is only actionable with a version attached to it.
fn floor_verdict(cli: &ResolvedCli, installed_anywhere: bool) -> Option<String> {
    let state = if installed_anywhere {
        "the installed plugin is inert"
    } else {
        "the plugin would be inert once installed"
    };
    match cli.answers_hook {
        Some(true) => None,
        Some(false) => {
            let found = cli.version.as_deref().unwrap_or("the `aethyme` on PATH");
            Some(format!(
                "{found} does not serve `aethyme hook`, so {state}: its hooks reach \
                 that CLI, get nothing back, and stay silent. Upgrade the `aethyme` \
                 on PATH to {MIN_HOOK_CLI_VERSION} or later."
            ))
        }
        None => Some(format!(
            "no `aethyme` on PATH, so {state}: the shim exits early when it cannot \
             find one. Install {MIN_HOOK_CLI_VERSION} or later."
        )),
    }
}

fn print_status(cli: &ResolvedCli, statuses: &[SurfaceStatus]) -> u8 {
    let pair = install_health::resolve_pair();
    println!("Hook floor:  aethyme >= {MIN_HOOK_CLI_VERSION}");
    match (&cli.path, &cli.version) {
        (Some(path), Some(version)) => println!("On PATH:     {version}  ({})", path.display()),
        (Some(path), None) => println!("On PATH:     unreadable version  ({})", path.display()),
        _ => println!("On PATH:     no `aethyme` found"),
    }
    println!(
        "             serves `hook`: {}",
        match cli.answers_hook {
            Some(true) => "yes",
            Some(false) => "no",
            None => "n/a",
        }
    );
    println!("This binary: aethyme {}", env!("CARGO_PKG_VERSION"));
    println!(
        "Engine pair: {}",
        match &pair {
            PairState::Aligned(build) => format!("matched ({build})"),
            PairState::Split { router, engine } =>
                format!("SPLIT — aethyme {router}, aethyme-engine-cli {engine}"),
            PairState::EngineMissing => "aethyme-engine-cli not on PATH".to_string(),
            PairState::Unknown => "unreadable".to_string(),
        }
    );
    println!();
    for status in statuses {
        let detail = match (&status.cli_present, &status.plugin_version) {
            (false, _) => format!("{} not installed on this machine", status.surface.binary()),
            (true, None) => "plugin not installed".to_string(),
            (true, Some(version)) => match status.enabled {
                Some(false) => format!("plugin {version} installed, DISABLED"),
                _ => format!("plugin {version} installed"),
            },
        };
        println!("{:<12} {detail}", format!("{}:", status.surface.label()));
    }

    let installed_anywhere = statuses.iter().any(SurfaceStatus::installed);
    let floor = floor_verdict(cli, installed_anywhere);
    let pair_problem = install_health::pair_warning(&pair);
    if floor.is_some() || pair_problem.is_some() {
        println!();
    }
    for warning in floor.iter().chain(pair_problem.iter()) {
        eprintln!("warning: {warning}");
    }
    // Nonzero only when something is actually installed and therefore actually
    // broken. A machine that has not installed the plugin is not in a failed
    // state -- but a split pair is broken whether or not the plugin is there,
    // because every command run against it is already running mismatched
    // binaries.
    u8::from((floor.is_some() && installed_anywhere) || pair_problem.is_some())
}

fn status_json(cli: &ResolvedCli, statuses: &[SurfaceStatus]) -> serde_json::Value {
    let pair = install_health::resolve_pair();
    serde_json::json!({
        "hook_floor": MIN_HOOK_CLI_VERSION,
        "this_binary": env!("CARGO_PKG_VERSION"),
        "engine_pair": match &pair {
            PairState::Aligned(build) => serde_json::json!({"state": "aligned", "build": build}),
            PairState::Split { router, engine } => serde_json::json!({
                "state": "split", "router": router, "engine": engine,
            }),
            PairState::EngineMissing => serde_json::json!({"state": "engine_missing"}),
            PairState::Unknown => serde_json::json!({"state": "unknown"}),
        },
        "engine_pair_warning": install_health::pair_warning(&pair),
        "path_cli": {
            "path": cli.path.as_ref().map(|path| path.display().to_string()),
            "version": cli.version,
            "answers_hook": cli.answers_hook,
            "version_meets_floor": cli.meets_floor(),
        },
        "surfaces": statuses
            .iter()
            .map(|status| serde_json::json!({
                "surface": status.surface.binary(),
                "cli_present": status.cli_present,
                "plugin_version": status.plugin_version,
                "enabled": status.enabled,
            }))
            .collect::<Vec<_>>(),
    })
}

fn run_steps(steps: &[Step], dry_run: bool) -> Result<(), String> {
    for step in steps {
        if dry_run {
            println!("would run: {}   # {}", step.rendered(), step.purpose);
            continue;
        }
        println!("+ {}", step.rendered());
        let status = Command::new(&step.argv[0]).args(&step.argv[1..]).status();
        let ok = matches!(&status, Ok(status) if status.success());
        if !ok && !step.tolerate_failure {
            return Err(format!("`{}` failed", step.rendered()));
        }
    }
    Ok(())
}

pub fn run(args: &[String]) -> u8 {
    let action = args.first().map(String::as_str).unwrap_or("");
    let rest = if args.is_empty() { &[][..] } else { &args[1..] };
    match action {
        "status" => {
            let cli = resolved_cli();
            let statuses: Vec<SurfaceStatus> =
                Surface::ALL.iter().copied().map(surface_status).collect();
            if has_flag(rest, "--json") {
                println!("{}", status_json(&cli, &statuses));
                return 0;
            }
            print_status(&cli, &statuses)
        }
        "install" | "remove" => {
            let surfaces = match selected_surfaces(rest) {
                Ok(surfaces) => surfaces,
                Err(message) => {
                    eprintln!("aethyme plugin: {message}");
                    return 2;
                }
            };
            let dry_run = has_flag(rest, "--dry-run");
            let source =
                flag_value(rest, "--source").unwrap_or_else(|| DEFAULT_MARKETPLACE_SOURCE.into());

            // A surface whose CLI is absent is skipped, not failed: the
            // default is "both", and most machines have one.
            let present: Vec<Surface> = surfaces
                .iter()
                .copied()
                .filter(|surface| dry_run || resolve_on_path(surface.binary()).is_some())
                .collect();
            if present.is_empty() {
                eprintln!(
                    "aethyme plugin: none of the selected surfaces are installed on this machine"
                );
                return 1;
            }

            for surface in present {
                let steps = if action == "install" {
                    plan_install(surface, &source)
                } else {
                    plan_remove(surface)
                };
                println!("{}:", surface.label());
                if let Err(message) = run_steps(&steps, dry_run) {
                    eprintln!("aethyme plugin: {message}");
                    return 1;
                }
                println!();
            }

            if dry_run || action == "remove" {
                return 0;
            }
            // The install itself can succeed into a state that does
            // nothing, so say so here rather than leaving it to be
            // discovered as silence.
            let cli = resolved_cli();
            if let Some(warning) = floor_verdict(&cli, true) {
                eprintln!("warning: {warning}");
            }
            0
        }
        "-h" | "--help" | "help" | "" => {
            print_help();
            0
        }
        other => {
            eprintln!("aethyme plugin: unknown action '{other}'");
            print_help();
            2
        }
    }
}

fn print_help() {
    eprintln!("aethyme plugin — install the agent-surface plugin and check the CLI behind it");
    eprintln!();
    eprintln!("Usage: aethyme plugin <action> [options]");
    eprintln!();
    eprintln!("Actions:");
    eprintln!("  install     register the marketplace and install the plugin");
    eprintln!("  remove      uninstall the plugin and forget the marketplace");
    eprintln!("  status      report the plugin, the CLI on PATH, and the hook floor");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --surface codex|claude|all   default: all surfaces present on this machine");
    eprintln!("  --source <path|owner/repo>   default: {DEFAULT_MARKETPLACE_SOURCE}");
    eprintln!("  --dry-run                    print the commands without running them");
    eprintln!("  --json                       machine-readable status");
    eprintln!();
    eprintln!("`status` exits nonzero when the plugin is installed but the `aethyme`");
    eprintln!("on PATH is older than {MIN_HOOK_CLI_VERSION}, which makes its hooks inert.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_read_out_of_the_cli_banner_it_is_printed_in() {
        assert_eq!(
            parse_version("aethyme 0.7.16 (v0.7.16-6-g080d3c8a)"),
            Some(vec![0, 7, 16])
        );
        assert_eq!(parse_version("0.7.17"), Some(vec![0, 7, 17]));
        assert_eq!(parse_version("aethyme"), None);
    }

    /// A release candidate of the version that introduced a subcommand
    /// has the subcommand, so the floor must not reject it.
    #[test]
    fn a_prerelease_suffix_does_not_drop_below_its_own_version() {
        let floor = parse_version(MIN_HOOK_CLI_VERSION).unwrap();
        let candidate = parse_version("0.7.17-rc1").unwrap();
        assert_eq!(compare_versions(&candidate, &floor), Ordering::Equal);
    }

    #[test]
    fn a_missing_component_compares_as_zero() {
        assert_eq!(compare_versions(&[0, 8], &[0, 8, 0]), Ordering::Equal);
        assert_eq!(compare_versions(&[0, 7, 16], &[0, 7, 17]), Ordering::Less);
        assert_eq!(
            compare_versions(&[0, 10, 0], &[0, 9, 99]),
            Ordering::Greater
        );
    }

    /// Both surfaces publish the listing in their own shape, and the
    /// point of walking the tree is that neither shape is load-bearing.
    #[test]
    fn the_plugin_is_found_in_either_surfaces_listing_shape() {
        let codex = serde_json::json!({
            "installed": [
                {"pluginId": "browser@openai-bundled", "version": "26.0"},
                {"pluginId": "aethyme@aethyme", "version": "0.1.1", "enabled": true}
            ]
        });
        let claude = serde_json::json!([
            {"id": "code-review@claude-plugins-official", "version": "3b60"},
            {"id": "aethyme@aethyme", "version": "0.1.1", "enabled": false}
        ]);
        assert_eq!(
            find_plugin(&codex),
            Some((Some("0.1.1".into()), Some(true)))
        );
        assert_eq!(
            find_plugin(&claude),
            Some((Some("0.1.1".into()), Some(false)))
        );
        assert_eq!(find_plugin(&serde_json::json!([])), None);
    }

    /// Registering an already-registered marketplace is an error on both
    /// surfaces, so a second `install` must not fail on it.
    #[test]
    fn install_tolerates_the_marketplace_already_being_registered() {
        let steps = plan_install(Surface::Codex, "schiste/Aethyme");
        assert!(steps[0].rendered().contains("marketplace add"));
        assert!(steps[0].tolerate_failure);
        assert!(!steps.last().unwrap().tolerate_failure);
    }

    /// Without the refresh, a re-run installs whatever snapshot the
    /// first run cloned -- which is exactly the skew this module exists
    /// to prevent.
    #[test]
    fn install_refreshes_the_snapshot_before_installing() {
        for (surface, refresh) in [
            (Surface::Codex, "marketplace upgrade"),
            (Surface::Claude, "marketplace update"),
        ] {
            let steps = plan_install(surface, "schiste/Aethyme");
            assert!(
                steps[1].rendered().contains(refresh),
                "{}",
                steps[1].rendered()
            );
        }
    }

    #[test]
    fn claude_installs_for_every_project_rather_than_the_current_one() {
        let steps = plan_install(Surface::Claude, "schiste/Aethyme");
        assert!(steps.last().unwrap().rendered().contains("--scope user"));
    }

    fn cli(version: Option<&str>, answers_hook: Option<bool>) -> ResolvedCli {
        ResolvedCli {
            path: Some(PathBuf::from("/usr/local/bin/aethyme")),
            version: version.map(str::to_string),
            answers_hook,
        }
    }

    /// Inert, not broken -- and the warning has to carry a version, because
    /// "upgrade" with nothing to upgrade to is not an instruction.
    #[test]
    fn a_cli_without_hook_is_reported_as_inert_with_a_target_version() {
        let verdict =
            floor_verdict(&cli(Some("aethyme 0.7.15"), Some(false)), true).expect("a warning");
        assert!(verdict.contains("inert"), "{verdict}");
        assert!(verdict.contains("0.7.15"), "{verdict}");
        assert!(verdict.contains(MIN_HOOK_CLI_VERSION), "{verdict}");
        assert_eq!(
            floor_verdict(&cli(Some("aethyme 0.7.17"), Some(true)), true),
            None
        );
    }

    /// The whole reason the probe outranks the version: a development
    /// build carrying the tag of the release *before* the one that
    /// introduced `hook` still has `hook`, and must not be warned about.
    #[test]
    fn a_development_build_below_the_floor_passes_on_its_capability() {
        let dev = cli(Some("aethyme 0.7.16 (v0.7.16-8-gf74bf96b)"), Some(true));
        assert_eq!(dev.meets_floor(), Some(false), "version alone says no");
        assert_eq!(floor_verdict(&dev, true), None, "the probe says yes");
    }

    /// Conversely, a version at or above the floor does not excuse a
    /// binary that will not answer.
    #[test]
    fn a_new_looking_version_that_does_not_answer_still_warns() {
        assert!(floor_verdict(&cli(Some("aethyme 9.9.9"), Some(false)), true).is_some());
    }

    /// No CLI at all is its own diagnosis: the shim exits before the
    /// version question arises.
    #[test]
    fn a_missing_cli_is_named_rather_than_reported_as_old() {
        let verdict = floor_verdict(
            &ResolvedCli {
                path: None,
                version: None,
                answers_hook: None,
            },
            false,
        )
        .expect("a warning");
        assert!(verdict.contains("no `aethyme` on PATH"), "{verdict}");
    }

    /// An unreadable version must not cost the user the verdict: the
    /// probe still decides, and the warning names the binary generically.
    #[test]
    fn an_unreadable_version_does_not_block_the_verdict() {
        assert_eq!(floor_verdict(&cli(None, Some(true)), true), None);
        let verdict = floor_verdict(&cli(None, Some(false)), true).expect("a warning");
        assert!(verdict.contains("the `aethyme` on PATH"), "{verdict}");
    }

    #[test]
    fn an_unknown_surface_is_refused_by_name() {
        let args = vec!["--surface".to_string(), "emacs".to_string()];
        let error = selected_surfaces(&args).unwrap_err();
        assert!(error.contains("emacs"), "{error}");
    }

    #[test]
    fn no_surface_flag_means_every_surface() {
        assert_eq!(selected_surfaces(&[]).unwrap(), Surface::ALL.to_vec());
    }
}
