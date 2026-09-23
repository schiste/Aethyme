//! Trust on first use, and again on change, for repository-defined commands.
//!
//! `.aethyme/gates.toml` and `.aethyme/prepare.toml` travel with the
//! repository, and the broker runs their commands as the operator whenever an
//! agent submits, runs gates, commits through the installed hook, or prepares
//! a session. A freshly cloned repository could therefore make the broker run
//! arbitrary shell. Nothing repository-defined runs until a human on this
//! machine has approved the exact policy with `aethyme broker trust`, which
//! refuses without a terminal so an agent cannot approve itself.
//!
//! The record is host state, keyed by the repository's canonical Git common
//! directory, so every worktree and linked checkout of one clone shares it and
//! nothing inside the repository can forge it. It holds SHA-256 digests of the
//! policy, never the commands themselves.
//!
//! A repository whose broker database already holds gate history was already
//! running these commands before this check existed; it is trusted with its
//! current policy the first time the check sees it, and that is recorded as an
//! event rather than asked for again.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::BrokerError;
use crate::gates::{Gate, GateConfigError};
use crate::git::GitRepo;
use crate::preparation::PreparationConfig;
use crate::store::BrokerStore;

use super::BrokerOpError;

/// Test-only escape. When set to `1`, `aethyme broker trust` records trust
/// without a terminal or a prompt, and an untrusted policy is allowed to run
/// without being recorded. Test harnesses set it; nothing else may.
pub const TEST_ESCAPE_ENV: &str = "AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS";

const RECORD_SCHEMA_VERSION: u32 = 1;
const POLICY_SCHEMA_VERSION: u32 = 1;
/// Superseded approvals are kept so a session still on the previous policy
/// keeps working after a re-trust. Every entry was approved by a human or by
/// grandfathering; the bound only keeps the file small.
const MAX_TRUSTED_POLICIES: usize = 16;
const RECORD_DIR: &str = "gate-trust";

/// One repository-defined command the policy would run.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PolicyCommand {
    /// `gate`, `prepare.step`, `prepare.offline_step` or `prepare.runtime`.
    pub source: &'static str,
    pub name: String,
    pub command: String,
}

/// The executable policy of one source tree and its digest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct GatePolicy {
    pub policy_sha256: String,
    pub commands: Vec<PolicyCommand>,
}

impl GatePolicy {
    /// Digest gates by name and `definition_hash` -- which already covers the
    /// command and every field that changes how it executes -- and preparation
    /// by each step and runtime command.
    pub(crate) fn from_parts(gates: &[Gate], prepare: Option<&PreparationConfig>) -> Self {
        let mut commands = Vec::new();
        let mut gate_digests = gates
            .iter()
            .map(|gate| {
                commands.push(PolicyCommand {
                    source: "gate",
                    name: gate.name.clone(),
                    command: gate.command.clone(),
                });
                serde_json::json!({ "name": gate.name, "definition_hash": gate.definition_hash })
            })
            .collect::<Vec<_>>();
        gate_digests.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        let mut prepare_digests = Vec::new();
        if let Some(config) = prepare {
            for runtime in &config.runtimes {
                commands.push(argv_command(
                    "prepare.runtime",
                    &runtime.name,
                    &runtime.command,
                ));
                prepare_digests.push(serde_json::json!({
                    "runtime": runtime.name, "command": runtime.command,
                }));
            }
            for step in &config.steps {
                commands.push(argv_command("prepare.step", &step.name, &step.command));
                if let Some(offline) = &step.offline_command {
                    commands.push(argv_command("prepare.offline_step", &step.name, offline));
                }
                prepare_digests.push(serde_json::json!({
                    "step": step.name,
                    "command": step.command,
                    "offline_command": step.offline_command,
                }));
            }
        }
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": POLICY_SCHEMA_VERSION,
            "gates": gate_digests,
            "prepare": prepare_digests,
        }))
        .expect("policy digest input is plain JSON");
        Self {
            policy_sha256: format!("{:x}", Sha256::digest(bytes)),
            commands,
        }
    }

    /// A policy that runs nothing needs no trust.
    pub fn runs_commands(&self) -> bool {
        !self.commands.is_empty()
    }
}

fn argv_command(source: &'static str, name: &str, argv: &[String]) -> PolicyCommand {
    PolicyCommand {
        source,
        name: name.to_string(),
        command: argv
            .iter()
            .map(|part| shell_word(part))
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// The policy a checkout's files declare.
pub(crate) fn policy_at_root(root: &Path) -> Result<GatePolicy, BrokerOpError> {
    let gates = match crate::gates::load_gates(root) {
        Ok(gates) => gates,
        Err(GateConfigError::Missing(_)) => Vec::new(),
        Err(error) => return Err(error.into()),
    };
    let prepare = crate::preparation::load_config(root)?;
    Ok(GatePolicy::from_parts(&gates, prepare.as_ref()))
}

/// The policy as committed at `commit`.
pub(crate) fn policy_at_commit(repo: &GitRepo, commit: &str) -> Result<GatePolicy, BrokerOpError> {
    let gates = match repo.file_at_commit(commit, crate::gates::GATES_CONFIG_RELPATH)? {
        Some(text) => crate::gates::parse_gates(&text)?,
        None => Vec::new(),
    };
    let prepare = repo
        .file_at_commit(commit, crate::preparation::PREPARATION_CONFIG_RELPATH)?
        .map(|text| crate::preparation::parse_config(&text))
        .transpose()?;
    Ok(GatePolicy::from_parts(&gates, prepare.as_ref()))
}

/// One approved policy digest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrustedPolicy {
    pub policy_sha256: String,
    pub trusted_at_ms: i64,
    /// `interactive`, `test_escape`, `grandfathered` or `quick_test`.
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct TrustRecord {
    schema_version: u32,
    repository: String,
    trusted: Vec<TrustedPolicy>,
}

/// Where one repository's trust record lives.
pub(crate) fn record_path(main_root: &Path) -> Result<PathBuf, BrokerOpError> {
    let base = crate::host_state::default_host_state_dir().ok_or_else(|| {
        BrokerOpError::Store(BrokerError::Io {
            path: PathBuf::from(RECORD_DIR),
            source: std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no host state directory: set HOME or AETHYME_HOST_STATE_DIR",
            ),
        })
    })?;
    let common = GitRepo::discover(main_root)
        .ok()
        .and_then(|repo| repo.git_common_dir().ok());
    let key = crate::host_state::repository_key(main_root, common.as_deref());
    Ok(base.join(RECORD_DIR).join(format!("{key}.json")))
}

fn io_error(path: &Path, source: std::io::Error) -> BrokerOpError {
    BrokerOpError::Store(BrokerError::Io {
        path: path.to_path_buf(),
        source: std::io::Error::new(
            source.kind(),
            crate::host_state::describe_host_state_io(path, &source),
        ),
    })
}

fn read_record(path: &Path) -> Result<Option<TrustRecord>, BrokerOpError> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(path, source)),
    };
    // A record that cannot be read proves nothing was approved: fail closed
    // rather than treat it as absent, which would re-open grandfathering.
    serde_json::from_slice(&bytes).map(Some).map_err(|error| {
        io_error(
            path,
            std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()),
        )
    })
}

fn write_record(path: &Path, record: &TrustRecord) -> Result<(), BrokerOpError> {
    let parent = path.parent().expect("record path has a parent");
    std::fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let _ = crate::host_state::protect_host_state_path(parent, true);
    let bytes = serde_json::to_vec_pretty(record).expect("trust record serializes");
    crate::atomic_file::with_synced_temporary(path, &bytes, |temporary| {
        std::fs::rename(temporary, path)
    })
    .map_err(|source| io_error(path, source))
}

/// Record `digests` as trusted. Returns the record path.
pub(crate) fn record_trust(
    main_root: &Path,
    digests: &[&str],
    source: &str,
) -> Result<PathBuf, BrokerOpError> {
    let path = record_path(main_root)?;
    let mut record = read_record(&path)?.unwrap_or_default();
    record.schema_version = RECORD_SCHEMA_VERSION;
    record.repository = main_root.to_string_lossy().into_owned();
    let now = now_ms();
    for digest in digests {
        record
            .trusted
            .retain(|entry| entry.policy_sha256 != *digest);
        record.trusted.push(TrustedPolicy {
            policy_sha256: (*digest).to_string(),
            trusted_at_ms: now,
            source: source.to_string(),
        });
    }
    let excess = record.trusted.len().saturating_sub(MAX_TRUSTED_POLICIES);
    record.trusted.drain(..excess);
    write_record(&path, &record)?;
    Ok(path)
}

/// Remove a repository's trust record. For disposable repositories that
/// trusted their own fixture policy.
pub(crate) fn forget_record(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Whether the test-only escape is set.
pub(crate) fn test_escape_enabled() -> bool {
    std::env::var(TEST_ESCAPE_ENV).is_ok_and(|value| value == "1")
}

/// How an enforcement check was satisfied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrustDecision {
    NoCommands,
    Trusted,
    Grandfathered,
    TestEscape,
}

/// Whether the repository's broker database shows gates already ran here.
fn has_gate_history(store: &BrokerStore) -> bool {
    store
        .gate_execution_totals()
        .map(|totals| !totals.is_empty())
        .unwrap_or(false)
}

/// Refuse unless `policy` is trusted for the repository at `main_root`.
///
/// `store` is the repository's broker database when one exists; it is the
/// grandfathering evidence and receives the grandfathering event.
pub(crate) fn require_trusted(
    main_root: &Path,
    policy: &GatePolicy,
    store: Option<&mut BrokerStore>,
    session_id: Option<i64>,
) -> Result<TrustDecision, BrokerOpError> {
    if !policy.runs_commands() {
        return Ok(TrustDecision::NoCommands);
    }
    let path = record_path(main_root)?;
    let record = read_record(&path)?;
    let trusted = record.as_ref().map(|r| r.trusted.as_slice()).unwrap_or(&[]);
    if trusted
        .iter()
        .any(|entry| entry.policy_sha256 == policy.policy_sha256)
    {
        return Ok(TrustDecision::Trusted);
    }
    // Before grandfathering: test fixtures build gate history in throwaway
    // repositories, and recording trust for each of them would litter the
    // host state directory.
    if test_escape_enabled() {
        return Ok(TrustDecision::TestEscape);
    }
    if trusted.is_empty()
        && let Some(store) = store
        && has_gate_history(store)
    {
        // Everything the repository visibly declares now is what it was
        // already running: the policy being enforced, the main checkout's,
        // and the integration tip's. Recording all of them keeps an existing
        // repository working whichever path reaches the check first.
        let mut digests = vec![policy.policy_sha256.clone()];
        digests.extend(current_policy_digests(main_root));
        digests.dedup();
        let refs = digests.iter().map(String::as_str).collect::<Vec<_>>();
        record_trust(main_root, &refs, "grandfathered")?;
        for digest in &digests {
            store.append_event(
                crate::events::GATE_POLICY_TRUST_GRANDFATHERED,
                session_id,
                Some(&crate::events::gate_policy_trust_payload(
                    digest,
                    "grandfathered",
                )),
            )?;
        }
        return Ok(TrustDecision::Grandfathered);
    }
    Err(BrokerOpError::GatePolicyUntrusted {
        repository: main_root.to_string_lossy().into_owned(),
        policy_sha256: policy.policy_sha256.clone(),
        state: if trusted.is_empty() {
            "has never been trusted on this machine"
        } else {
            "changed since it was last trusted on this machine"
        },
        trust_command: trust_command(main_root),
    })
}

/// [`require_trusted`] for callers without an open broker: the hook and the
/// gate doctor. Opens the repository's database only if it already exists.
pub(crate) fn require_trusted_standalone(
    main_root: &Path,
    policy: &GatePolicy,
) -> Result<TrustDecision, BrokerOpError> {
    if !policy.runs_commands() {
        return Ok(TrustDecision::NoCommands);
    }
    let mut store = BrokerStore::open_current_in_repo(main_root).ok().flatten();
    require_trusted(main_root, policy, store.as_mut(), None)
}

/// Digests of the policies the repository declares right now: the main
/// checkout's files and the integration tip. Best effort; a source that
/// cannot be read contributes nothing.
fn current_policy_digests(main_root: &Path) -> Vec<String> {
    let mut digests = Vec::new();
    if let Ok(policy) = policy_at_root(main_root) {
        digests.push(policy.policy_sha256);
    }
    if let Some(policy) = integration_tip_policy(main_root) {
        digests.push(policy.policy_sha256);
    }
    digests
}

/// The policy committed at the integration branch tip, when there is one.
pub(crate) fn integration_tip_policy(main_root: &Path) -> Option<GatePolicy> {
    let repo = GitRepo::discover(main_root).ok()?;
    let branch = crate::PromoteConfig::load(main_root).branch;
    let tip = repo.resolve_ref(&format!("refs/heads/{branch}"))?;
    policy_at_commit(&repo, &tip).ok()
}

/// One policy source a human is asked to approve.
#[derive(Debug, Clone, Serialize)]
pub struct PolicySource {
    /// `checkout` (the files at `--repo`) or `integration_tip`.
    pub source: &'static str,
    pub trusted: bool,
    #[serde(flatten)]
    pub policy: GatePolicy,
}

/// `aethyme broker trust status`.
#[derive(Debug, Clone, Serialize)]
pub struct GateTrustStatus {
    pub schema_version: u32,
    pub repository: String,
    pub record_path: String,
    /// Every source that runs commands is trusted.
    pub trusted: bool,
    pub sources: Vec<PolicySource>,
    pub trusted_policies: Vec<TrustedPolicy>,
    pub next_action: Option<String>,
}

/// `aethyme broker trust`.
#[derive(Debug, Clone, Serialize)]
pub struct GateTrustReport {
    pub schema_version: u32,
    pub repository: String,
    pub record_path: String,
    pub source: String,
    /// Sources recorded by this call; empty when everything was already
    /// trusted or nothing runs commands.
    pub recorded: Vec<String>,
    pub sources: Vec<PolicySource>,
}

/// The main checkout of the repository containing `dir`, and the root of the
/// checkout `dir` is in.
fn locate(dir: &Path) -> Result<(PathBuf, PathBuf), BrokerOpError> {
    let checkout = GitRepo::discover(dir)?;
    let main_root = checkout.main_root()?;
    Ok((main_root, checkout.root().to_path_buf()))
}

/// The distinct policies that would run for this repository: the checkout's
/// files and, when it differs, what is committed at the integration tip --
/// the base that judges the next submission.
fn policy_sources(
    main_root: &Path,
    checkout_root: &Path,
    trusted: &[TrustedPolicy],
) -> Result<Vec<PolicySource>, BrokerOpError> {
    let mut sources = vec![("checkout", policy_at_root(checkout_root)?)];
    if let Some(tip) = integration_tip_policy(main_root)
        && tip.policy_sha256 != sources[0].1.policy_sha256
    {
        sources.push(("integration_tip", tip));
    }
    Ok(sources
        .into_iter()
        .filter(|(_, policy)| policy.runs_commands())
        .map(|(source, policy)| PolicySource {
            source,
            trusted: trusted
                .iter()
                .any(|entry| entry.policy_sha256 == policy.policy_sha256),
            policy,
        })
        .collect())
}

/// Read-only trust state for the repository containing `dir`.
pub(crate) fn status(dir: &Path) -> Result<GateTrustStatus, BrokerOpError> {
    let (main_root, checkout_root) = locate(dir)?;
    let path = record_path(&main_root)?;
    let trusted_policies = read_record(&path)?
        .map(|record| record.trusted)
        .unwrap_or_default();
    let sources = policy_sources(&main_root, &checkout_root, &trusted_policies)?;
    let trusted = sources.iter().all(|source| source.trusted);
    Ok(GateTrustStatus {
        schema_version: RECORD_SCHEMA_VERSION,
        repository: main_root.to_string_lossy().into_owned(),
        record_path: path.to_string_lossy().into_owned(),
        trusted,
        next_action: (!trusted).then(|| trust_command(&main_root)),
        sources,
        trusted_policies,
    })
}

/// Record every untrusted source of the repository containing `dir`.
/// The caller has already shown them to a human and obtained approval.
pub(crate) fn trust(dir: &Path, source: &str) -> Result<GateTrustReport, BrokerOpError> {
    let current = status(dir)?;
    let main_root = PathBuf::from(&current.repository);
    let recorded = current
        .sources
        .iter()
        .filter(|entry| !entry.trusted)
        .map(|entry| entry.policy.policy_sha256.clone())
        .collect::<Vec<_>>();
    if !recorded.is_empty() {
        let refs = recorded.iter().map(String::as_str).collect::<Vec<_>>();
        record_trust(&main_root, &refs, source)?;
        if let Ok(Some(mut store)) = BrokerStore::open_current_in_repo(&main_root) {
            for digest in &recorded {
                let _ = store.append_event(
                    crate::events::GATE_POLICY_TRUSTED,
                    None,
                    Some(&crate::events::gate_policy_trust_payload(digest, source)),
                );
            }
        }
    }
    let after = status(dir)?;
    Ok(GateTrustReport {
        schema_version: RECORD_SCHEMA_VERSION,
        repository: after.repository,
        record_path: after.record_path,
        source: source.to_string(),
        recorded,
        sources: after.sources,
    })
}

/// The exact command a human runs to approve this repository's policy.
pub(crate) fn trust_command(main_root: &Path) -> String {
    format!(
        "aethyme broker trust --repo {}",
        shell_word(&main_root.to_string_lossy())
    )
}

fn shell_word(value: &str) -> String {
    let safe = !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@%".contains(c));
    if safe {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', r"'\''"))
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_follows_commands_and_ignores_gate_order() {
        let a = crate::gates::parse_gates(
            "[[gate]]\nname = \"a\"\ncommand = \"true\"\ntriggers = [\"**\"]\n\
             [[gate]]\nname = \"b\"\ncommand = \"false\"\ntriggers = [\"**\"]\n",
        )
        .unwrap();
        let reordered = crate::gates::parse_gates(
            "[[gate]]\nname = \"b\"\ncommand = \"false\"\ntriggers = [\"**\"]\n\
             [[gate]]\nname = \"a\"\ncommand = \"true\"\ntriggers = [\"**\"]\n",
        )
        .unwrap();
        let changed = crate::gates::parse_gates(
            "[[gate]]\nname = \"a\"\ncommand = \"touch pwned\"\ntriggers = [\"**\"]\n\
             [[gate]]\nname = \"b\"\ncommand = \"false\"\ntriggers = [\"**\"]\n",
        )
        .unwrap();
        let base = GatePolicy::from_parts(&a, None);
        assert_eq!(
            base.policy_sha256,
            GatePolicy::from_parts(&reordered, None).policy_sha256
        );
        assert_ne!(
            base.policy_sha256,
            GatePolicy::from_parts(&changed, None).policy_sha256
        );
        assert!(!GatePolicy::from_parts(&[], None).runs_commands());
    }

    #[test]
    fn a_prepare_command_is_part_of_the_policy() {
        let config = |command: &str| {
            crate::preparation::parse_config(&format!(
                "schema_version = 1\n[[steps]]\nname = \"deps\"\ncommand = [\"{command}\"]\n\
                 outputs = [\"out\"]\n"
            ))
            .unwrap()
        };
        let one = GatePolicy::from_parts(&[], Some(&config("install")));
        let two = GatePolicy::from_parts(&[], Some(&config("curl")));
        assert!(one.runs_commands());
        assert_ne!(one.policy_sha256, two.policy_sha256);
    }

    #[test]
    fn a_path_with_spaces_is_quoted_in_the_hint() {
        assert_eq!(
            trust_command(Path::new("/tmp/My Repo")),
            "aethyme broker trust --repo '/tmp/My Repo'"
        );
        assert_eq!(
            trust_command(Path::new("/tmp/repo")),
            "aethyme broker trust --repo /tmp/repo"
        );
    }
}
