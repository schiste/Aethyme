//! How large and how risky a change is, measured from the diff (#584).
//!
//! The review trigger can only see what an author declares (`Risk:`, `Area:`,
//! `Surface:` trailers) and path globs, so no rule can tell a twelve-line typo
//! fix from a four-thousand-line rewrite. This module measures the change and
//! names every signal it found, so a repository can flag large or risky pull
//! requests -- and, through configuration only, attach actions to them.
//!
//! Like the rest of the decision plane it is a pure function of facts the
//! caller gathers: same diff, same configuration, same answer. Nothing here
//! talks to git or the provider.
//!
//! Two rules shape it:
//!
//! - **A declaration can only raise the computed risk.** The `Risk:` trailer is
//!   author-supplied and unverified; the measured risk is the floor.
//! - **Not knowing is not the same as clean.** A signal that could not be read
//!   (the contract scan when no diff text was available) is reported as
//!   unknown, and every consumer that relaxes something on a clean signal must
//!   treat unknown as set.

use std::collections::BTreeSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::review_trigger::{path_matches, risk_rank};

/// Current shape of the `[review.classification]` table.
pub const CHANGE_CLASSIFICATION_SCHEMA_VERSION: u32 = 1;

/// Size boundaries of one tier: a change is within it when both hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizeBounds {
    pub max_files: u64,
    pub max_changed_lines: u64,
}

/// `[review] pr_size` (#239): the advisory size above which a pull request is
/// `large`, and above which `broker push --pr` warns.
pub const DEFAULT_PR_SIZE: SizeBounds = SizeBounds {
    max_files: 30,
    max_changed_lines: 800,
};

/// The largest change still called `trivial`.
pub const DEFAULT_TRIVIAL: SizeBounds = SizeBounds {
    max_files: 3,
    max_changed_lines: 40,
};

/// Lockfiles, excluded from the size by default: they are machine-written and
/// a dependency bump would otherwise read as a large change. They still raise
/// the `dependency_manifest` signal.
const DEFAULT_SIZE_EXCLUDES: &[&str] = &[
    "**/Cargo.lock",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/yarn.lock",
    "**/poetry.lock",
    "**/uv.lock",
    "**/go.sum",
    "**/Gemfile.lock",
    "**/composer.lock",
];

/// Paths that change what a build pulls in.
const DEPENDENCY_MANIFESTS: &[&str] = &[
    "**/Cargo.toml",
    "**/Cargo.lock",
    "**/package.json",
    "**/package-lock.json",
    "**/pnpm-lock.yaml",
    "**/pnpm-workspace.yaml",
    "**/yarn.lock",
    "**/pyproject.toml",
    "**/poetry.lock",
    "**/uv.lock",
    "**/requirements*.txt",
    "**/go.mod",
    "**/go.sum",
    "**/Gemfile",
    "**/Gemfile.lock",
    "**/composer.json",
    "**/composer.lock",
    "rust-toolchain.toml",
];

/// Broker policy: a change here alters what gates run or how reviews route.
const GATE_POLICY: &[&str] = &[".aethyme/gates.toml", ".aethyme/config.toml"];

const WORKFLOWS: &[&str] = &[".github/workflows/**"];

const DEFAULT_MIGRATIONS: &[&str] = &["**/migrations/**"];

/// The `[review.classification]` table, plus `[review] pr_size`.
///
/// Every list is configuration. The built-in defaults (lockfile excludes,
/// dependency manifests, gate policy, workflows, `**/migrations/**`) are
/// documented, and `sensitive_paths` is empty unless a repository names some.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeClassificationPolicy {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    /// Paths that make a change risky in this repository.
    #[serde(default)]
    pub sensitive_paths: Vec<String>,
    /// Paths that hold schema or data migrations.
    #[serde(default = "default_migrations")]
    pub migration_paths: Vec<String>,
    /// Extra paths left out of the size, on top of lockfiles and files
    /// `.gitattributes` marks `linguist-generated`.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    /// Leave lockfiles out of the size (default true).
    #[serde(default = "default_true")]
    pub exclude_lockfiles: bool,
    /// The largest change called `trivial`.
    #[serde(default = "default_trivial")]
    pub trivial: SizeBounds,
    /// Repository-relative path of the cross-process consumer inventory whose
    /// tracked symbols set `contract_surface`. A repository without one has no
    /// tracked symbols, so the signal is clear.
    #[serde(default = "default_contract_doc")]
    pub contract_doc: String,
    /// Filled from `[review] pr_size`, not from this table: the boundary above
    /// which a change is `large`. One definition, shared with the
    /// `broker push --pr` warning.
    #[serde(skip, default = "default_pr_size")]
    pub pr_size: SizeBounds,
    /// Whether `[review] pr_size` was written, as opposed to defaulted.
    /// Generated guidance and the push warning only state thresholds a
    /// repository chose.
    #[serde(skip)]
    pub pr_size_configured: bool,
}

fn default_schema_version() -> u32 {
    CHANGE_CLASSIFICATION_SCHEMA_VERSION
}
fn default_true() -> bool {
    true
}
fn default_migrations() -> Vec<String> {
    DEFAULT_MIGRATIONS.iter().map(|s| s.to_string()).collect()
}
fn default_trivial() -> SizeBounds {
    DEFAULT_TRIVIAL
}
fn default_contract_doc() -> String {
    crate::contract_check::DEFAULT_CONSUMERS_DOC.to_string()
}
fn default_pr_size() -> SizeBounds {
    DEFAULT_PR_SIZE
}

impl Default for ChangeClassificationPolicy {
    fn default() -> Self {
        Self {
            schema_version: CHANGE_CLASSIFICATION_SCHEMA_VERSION,
            sensitive_paths: Vec::new(),
            migration_paths: default_migrations(),
            exclude_paths: Vec::new(),
            exclude_lockfiles: true,
            trivial: DEFAULT_TRIVIAL,
            contract_doc: default_contract_doc(),
            pr_size: DEFAULT_PR_SIZE,
            pr_size_configured: false,
        }
    }
}

/// Why `[review.classification]` or `[review] pr_size` could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ChangeClassificationError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("invalid {path}: {source}")]
    Parse {
        path: String,
        source: toml::de::Error,
    },
    #[error(
        "{path}: review.classification schema_version {found} is newer than this broker \
         understands ({supported}); upgrade aethyme or pin the policy"
    )]
    UnsupportedSchema {
        path: String,
        found: u32,
        supported: u32,
    },
    #[error(
        "{path}: {key} must have max_files and max_changed_lines above zero, and `trivial` \
         must not exceed `[review] pr_size`"
    )]
    InvalidBounds { path: String, key: &'static str },
}

impl ChangeClassificationPolicy {
    /// Load from `.aethyme/config.toml`; an absent file or table is the
    /// documented default. A newer schema refuses rather than defaulting.
    pub fn load(root: &Path) -> Result<Self, ChangeClassificationError> {
        let path = root.join(".aethyme/config.toml");
        let display = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ChangeClassificationError::Read {
                    path: display,
                    source,
                });
            }
        };
        Self::from_toml_str(&text, &display)
    }

    /// Parse the policy out of a whole `.aethyme/config.toml`.
    pub fn from_toml_str(text: &str, display: &str) -> Result<Self, ChangeClassificationError> {
        let value: toml::Value =
            text.parse()
                .map_err(|source| ChangeClassificationError::Parse {
                    path: display.to_string(),
                    source,
                })?;
        let review = value.get("review");
        let mut policy: Self = match review.and_then(|review| review.get("classification")) {
            Some(table) => {
                table
                    .clone()
                    .try_into()
                    .map_err(|source| ChangeClassificationError::Parse {
                        path: display.to_string(),
                        source,
                    })?
            }
            None => Self::default(),
        };
        if let Some(pr_size) = review.and_then(|review| review.get("pr_size")) {
            policy.pr_size =
                pr_size
                    .clone()
                    .try_into()
                    .map_err(|source| ChangeClassificationError::Parse {
                        path: display.to_string(),
                        source,
                    })?;
            policy.pr_size_configured = true;
        }
        policy.validate(display)?;
        Ok(policy)
    }

    fn validate(&self, path: &str) -> Result<(), ChangeClassificationError> {
        if self.schema_version > CHANGE_CLASSIFICATION_SCHEMA_VERSION {
            return Err(ChangeClassificationError::UnsupportedSchema {
                path: path.to_string(),
                found: self.schema_version,
                supported: CHANGE_CLASSIFICATION_SCHEMA_VERSION,
            });
        }
        let positive = |bounds: SizeBounds| bounds.max_files > 0 && bounds.max_changed_lines > 0;
        if !positive(self.pr_size) {
            return Err(ChangeClassificationError::InvalidBounds {
                path: path.to_string(),
                key: "[review] pr_size",
            });
        }
        if !positive(self.trivial)
            || self.trivial.max_files > self.pr_size.max_files
            || self.trivial.max_changed_lines > self.pr_size.max_changed_lines
        {
            return Err(ChangeClassificationError::InvalidBounds {
                path: path.to_string(),
                key: "[review.classification] trivial",
            });
        }
        Ok(())
    }

    fn excluded_from_size(&self, path: &str) -> bool {
        let lockfile = self.exclude_lockfiles
            && DEFAULT_SIZE_EXCLUDES
                .iter()
                .any(|pattern| path_matches(pattern, path));
        lockfile
            || self
                .exclude_paths
                .iter()
                .any(|pattern| path_matches(pattern, path))
    }
}

/// One file of the change, as `git diff --numstat` or the provider reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangedFile {
    pub path: String,
    /// `None` for a binary file, whose lines cannot be counted.
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

/// Everything [`classify`] reads, gathered by the caller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeInputs {
    pub files: Vec<ChangedFile>,
    /// Paths `.gitattributes` marks `linguist-generated`, left out of the size.
    pub generated: BTreeSet<String>,
    /// Tracked cross-process symbols the diff touches; `None` when no diff text
    /// was available to scan, which is reported as unknown.
    pub contract_symbols: Option<Vec<String>>,
    pub from_fork: bool,
    pub first_time_contributor: bool,
    pub authored_by_model: bool,
    /// The highest `Risk:` the commits declared.
    pub declared_risk: Option<String>,
}

/// How large the change is, counting only the files that count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeSize {
    pub files_changed: u64,
    pub lines_added: u64,
    pub lines_deleted: u64,
    /// `lines_added + lines_deleted`.
    pub churn: u64,
    /// Files left out of the size, and why, so the numbers can be checked.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded: Vec<ExcludedFile>,
    /// Binary files counted as changed files with no lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub binary: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExcludedFile {
    pub path: String,
    /// `generated` or `excluded_path`.
    pub why: String,
}

/// Whether the contract scan found tracked symbols, found none, or could not
/// run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ContractSurface {
    Clear,
    Touched { symbols: Vec<String> },
    Unknown,
}

impl ContractSurface {
    /// Whether this can be relied on as "no contract touched".
    pub fn is_clear(&self) -> bool {
        matches!(self, Self::Clear)
    }
}

/// Every risk signal, each with the evidence that set it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskSignals {
    pub sensitive_paths: Vec<String>,
    pub contract_surface: ContractSurface,
    pub gate_policy: Vec<String>,
    pub workflows: Vec<String>,
    pub migrations: Vec<String>,
    pub dependency_manifest: Vec<String>,
    pub from_fork: bool,
    pub first_time_contributor: bool,
    pub authored_by_model: bool,
}

/// Signal names, as rules and the guardrails spell them.
pub const SIGNAL_NAMES: &[&str] = &[
    "sensitive_paths",
    "contract_surface",
    "gate_policy",
    "workflows",
    "migrations",
    "dependency_manifest",
    "from_fork",
    "first_time_contributor",
    "authored_by_model",
];

impl RiskSignals {
    /// Whether the named signal is set. An unknown contract scan counts as set:
    /// a caller relaxing something on "no contract touched" must not do so on
    /// a scan that never ran.
    pub fn is_set(&self, name: &str) -> Option<bool> {
        Some(match name {
            "sensitive_paths" => !self.sensitive_paths.is_empty(),
            "contract_surface" => !self.contract_surface.is_clear(),
            "gate_policy" => !self.gate_policy.is_empty(),
            "workflows" => !self.workflows.is_empty(),
            "migrations" => !self.migrations.is_empty(),
            "dependency_manifest" => !self.dependency_manifest.is_empty(),
            "from_fork" => self.from_fork,
            "first_time_contributor" => self.first_time_contributor,
            "authored_by_model" => self.authored_by_model,
            _ => return None,
        })
    }

    /// The names of every set signal, in [`SIGNAL_NAMES`] order.
    pub fn set_names(&self) -> Vec<&'static str> {
        SIGNAL_NAMES
            .iter()
            .copied()
            .filter(|name| self.is_set(name) == Some(true))
            .collect()
    }
}

/// Size tier. `risky` is reported separately, not as a tier: a one-line change
/// to a workflow is small *and* risky.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeTier {
    Trivial,
    Normal,
    Large,
}

impl SizeTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trivial => "trivial",
            Self::Normal => "normal",
            Self::Large => "large",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "trivial" => Some(Self::Trivial),
            "normal" => Some(Self::Normal),
            "large" => Some(Self::Large),
            _ => None,
        }
    }
}

/// The measured classification of one change at one head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangeClassification {
    pub size: ChangeSize,
    pub signals: RiskSignals,
    pub tier: SizeTier,
    /// Risk measured from the signals: `none`, `low` or `high`.
    pub computed_risk: String,
    /// What the label shows: the computed risk, raised by a higher `Risk:`
    /// declaration, never lowered by one.
    pub risk: String,
    /// `risk` ranks `high` or above.
    pub risky: bool,
    /// One reason per conclusion, so an operator can see why.
    pub reasons: Vec<String>,
    /// The size thresholds this was measured against.
    pub thresholds: Thresholds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thresholds {
    pub trivial: SizeBounds,
    pub pr_size: SizeBounds,
}

fn matching(patterns: &[impl AsRef<str>], paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| {
            patterns
                .iter()
                .any(|pattern| path_matches(pattern.as_ref(), path))
        })
        .map(|path| path.to_string())
        .collect()
}

/// Measure one change.
pub fn classify(
    policy: &ChangeClassificationPolicy,
    inputs: &ChangeInputs,
) -> ChangeClassification {
    let mut files: Vec<&ChangedFile> = inputs.files.iter().collect();
    files.sort_by(|a, b| a.path.cmp(&b.path));
    files.dedup_by(|a, b| a.path == b.path);
    let paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();

    let mut size = ChangeSize::default();
    for file in &files {
        if inputs.generated.contains(&file.path) {
            size.excluded.push(ExcludedFile {
                path: file.path.clone(),
                why: "generated".into(),
            });
            continue;
        }
        if policy.excluded_from_size(&file.path) {
            size.excluded.push(ExcludedFile {
                path: file.path.clone(),
                why: "excluded_path".into(),
            });
            continue;
        }
        size.files_changed += 1;
        match (file.added, file.deleted) {
            (Some(added), Some(deleted)) => {
                size.lines_added += added;
                size.lines_deleted += deleted;
            }
            _ => size.binary.push(file.path.clone()),
        }
    }
    size.churn = size.lines_added + size.lines_deleted;

    let contract_surface = match &inputs.contract_symbols {
        None => ContractSurface::Unknown,
        Some(symbols) if symbols.is_empty() => ContractSurface::Clear,
        Some(symbols) => {
            let mut symbols = symbols.clone();
            symbols.sort();
            symbols.dedup();
            ContractSurface::Touched { symbols }
        }
    };
    let signals = RiskSignals {
        sensitive_paths: matching(&policy.sensitive_paths, &paths),
        contract_surface,
        gate_policy: matching(GATE_POLICY, &paths),
        workflows: matching(WORKFLOWS, &paths),
        migrations: matching(&policy.migration_paths, &paths),
        dependency_manifest: matching(DEPENDENCY_MANIFESTS, &paths),
        from_fork: inputs.from_fork,
        first_time_contributor: inputs.first_time_contributor,
        authored_by_model: inputs.authored_by_model,
    };

    let mut reasons = Vec::new();
    let within = |bounds: SizeBounds| {
        size.files_changed <= bounds.max_files && size.churn <= bounds.max_changed_lines
    };
    let tier = if !within(policy.pr_size) {
        reasons.push(format!(
            "large: {} files and {} changed lines exceed [review] pr_size ({} files, {} lines)",
            size.files_changed,
            size.churn,
            policy.pr_size.max_files,
            policy.pr_size.max_changed_lines
        ));
        SizeTier::Large
    } else if within(policy.trivial) {
        reasons.push(format!(
            "trivial: {} files and {} changed lines are within trivial ({} files, {} lines)",
            size.files_changed,
            size.churn,
            policy.trivial.max_files,
            policy.trivial.max_changed_lines
        ));
        SizeTier::Trivial
    } else {
        reasons.push(format!(
            "normal: {} files and {} changed lines",
            size.files_changed, size.churn
        ));
        SizeTier::Normal
    };

    // High: anything that changes what others rely on, or arrives untrusted.
    // Low: dependencies, a first-time author, or sheer size. An unknown
    // contract scan does not raise the label -- that would flag every pull
    // request whenever the diff text is unavailable -- but it never counts as
    // clear for anything that relaxes review.
    let mut high = Vec::new();
    for (name, set) in [
        ("sensitive_paths", !signals.sensitive_paths.is_empty()),
        (
            "contract_surface",
            matches!(signals.contract_surface, ContractSurface::Touched { .. }),
        ),
        ("gate_policy", !signals.gate_policy.is_empty()),
        ("workflows", !signals.workflows.is_empty()),
        ("migrations", !signals.migrations.is_empty()),
        ("from_fork", signals.from_fork),
    ] {
        if set {
            high.push(name);
        }
    }
    let mut low = Vec::new();
    for (name, set) in [
        (
            "dependency_manifest",
            !signals.dependency_manifest.is_empty(),
        ),
        ("first_time_contributor", signals.first_time_contributor),
        ("large", tier == SizeTier::Large),
    ] {
        if set {
            low.push(name);
        }
    }
    let computed_risk = if !high.is_empty() {
        reasons.push(format!("risk high: {}", high.join(", ")));
        "high"
    } else if !low.is_empty() {
        reasons.push(format!("risk low: {}", low.join(", ")));
        "low"
    } else {
        "none"
    }
    .to_string();
    let risk = match &inputs.declared_risk {
        Some(declared) if risk_rank(declared) > risk_rank(&computed_risk) => {
            reasons.push(format!(
                "risk raised to `{}` by a `Risk:` declaration",
                declared.trim().to_ascii_lowercase()
            ));
            declared.trim().to_ascii_lowercase()
        }
        _ => computed_risk.clone(),
    };
    let risky = risk_rank(&risk) >= risk_rank("high");

    ChangeClassification {
        size,
        signals,
        tier,
        computed_risk,
        risk,
        risky,
        reasons,
        thresholds: Thresholds {
            trivial: policy.trivial,
            pr_size: policy.pr_size,
        },
    }
}

/// One line for the owned comment.
pub fn summary_line(classification: &ChangeClassification) -> String {
    let signals = classification.signals.set_names();
    let mut line = format!(
        "Size **{}** ({} files, +{}/−{}); risk **{}**",
        classification.tier.as_str(),
        classification.size.files_changed,
        classification.size.lines_added,
        classification.size.lines_deleted,
        classification.risk
    );
    if !signals.is_empty() {
        line.push_str(&format!(" — signals: {}", signals.join(", ")));
    }
    if matches!(
        classification.signals.contract_surface,
        ContractSurface::Unknown
    ) {
        line.push_str(" (contract scan unavailable)");
    }
    line
}

/// The `broker push --pr` view of `[review] pr_size` (#239). Advisory only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSizeReport {
    pub max_files: u64,
    pub max_changed_lines: u64,
    pub files: u64,
    pub changed_lines: u64,
    pub over_threshold: bool,
    /// Whether the repository wrote `[review] pr_size` or the default applied.
    pub configured: bool,
}

/// Compare a change against `[review] pr_size`.
pub fn pr_size_report(
    policy: &ChangeClassificationPolicy,
    classification: &ChangeClassification,
) -> PrSizeReport {
    PrSizeReport {
        max_files: policy.pr_size.max_files,
        max_changed_lines: policy.pr_size.max_changed_lines,
        files: classification.size.files_changed,
        changed_lines: classification.size.churn,
        over_threshold: classification.tier == SizeTier::Large,
        configured: policy.pr_size_configured,
    }
}

/// Parse `git diff --numstat -z` output.
///
/// With `-z` each record is `added\tdeleted\tpath\0`, and a rename is
/// `added\tdeleted\t\0old\0new\0`; the new path is the one that counts. A
/// binary file reports `-` for both counts.
pub fn parse_numstat_z(output: &str) -> Vec<ChangedFile> {
    let mut files = Vec::new();
    let mut fields = output.split('\0');
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(deleted), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let path = if path.is_empty() {
            // Rename: the old path, then the new one.
            let _old = fields.next();
            match fields.next() {
                Some(new) => new.to_string(),
                None => continue,
            }
        } else {
            path.to_string()
        };
        files.push(ChangedFile {
            path,
            added: added.parse().ok(),
            deleted: deleted.parse().ok(),
        });
    }
    files
}

/// Paths `.gitattributes` marks `linguist-generated`, as `root` resolves them.
///
/// The same rule `quality inspect` uses (#565). A failed lookup returns no
/// paths, which counts generated files toward the size: overstating a change
/// is the safe direction, since nothing relaxes review on a large one.
pub fn linguist_generated_paths(
    root: &Path,
    paths: &[String],
) -> std::collections::BTreeSet<String> {
    use std::io::Write;
    let mut found = std::collections::BTreeSet::new();
    if paths.is_empty() {
        return found;
    }
    let Ok(mut child) = crate::git::git_command()
        .current_dir(root)
        .args(["check-attr", "-z", "--stdin", "linguist-generated"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return found;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let mut input = Vec::new();
        for path in paths {
            input.extend_from_slice(path.as_bytes());
            input.push(0);
        }
        if stdin.write_all(&input).is_err() {
            return found;
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return found;
    };
    if !output.status.success() {
        return found;
    }
    // `-z` output is `path\0attribute\0value\0` per path.
    let text = String::from_utf8_lossy(&output.stdout);
    let fields: Vec<&str> = text.split('\0').collect();
    for record in fields.chunks(3) {
        if let [path, _attribute, value] = record
            && matches!(*value, "set" | "true")
        {
            found.insert((*path).to_string());
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, added: u64, deleted: u64) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            added: Some(added),
            deleted: Some(deleted),
        }
    }

    fn inputs(files: Vec<ChangedFile>) -> ChangeInputs {
        ChangeInputs {
            files,
            contract_symbols: Some(Vec::new()),
            ..Default::default()
        }
    }

    #[test]
    fn a_small_change_is_trivial_with_no_risk() {
        let c = classify(
            &ChangeClassificationPolicy::default(),
            &inputs(vec![file("docs/a.md", 3, 1)]),
        );
        assert_eq!(c.tier, SizeTier::Trivial);
        assert_eq!(c.risk, "none");
        assert!(!c.risky);
        assert_eq!(c.size.churn, 4);
    }

    #[test]
    fn exceeding_pr_size_is_large_and_low_risk() {
        let c = classify(
            &ChangeClassificationPolicy::default(),
            &inputs(vec![file("src/a.rs", 900, 0)]),
        );
        assert_eq!(c.tier, SizeTier::Large);
        assert_eq!(c.computed_risk, "low");
    }

    #[test]
    fn lockfiles_and_generated_files_do_not_count_toward_size() {
        let mut i = inputs(vec![
            file("Cargo.lock", 5000, 4000),
            file("gen/out.rs", 3000, 0),
            file("src/a.rs", 2, 0),
        ]);
        i.generated.insert("gen/out.rs".into());
        let c = classify(&ChangeClassificationPolicy::default(), &i);
        assert_eq!(c.size.files_changed, 1);
        assert_eq!(c.size.churn, 2);
        assert_eq!(c.tier, SizeTier::Trivial);
        // The lockfile still raises the dependency signal.
        assert_eq!(c.signals.dependency_manifest, vec!["Cargo.lock"]);
        assert_eq!(c.computed_risk, "low");
    }

    #[test]
    fn workflows_gate_policy_and_sensitive_paths_are_high_risk() {
        let policy = ChangeClassificationPolicy {
            sensitive_paths: vec!["src/auth/**".into()],
            ..Default::default()
        };
        for path in [
            ".github/workflows/ci.yml",
            ".aethyme/gates.toml",
            "src/auth/token.rs",
            "db/migrations/001.sql",
        ] {
            let c = classify(&policy, &inputs(vec![file(path, 1, 0)]));
            assert_eq!(c.computed_risk, "high", "{path}");
            assert!(c.risky, "{path}");
            assert_eq!(
                c.tier,
                SizeTier::Trivial,
                "{path}: size and risk are separate"
            );
        }
    }

    #[test]
    fn a_touched_contract_symbol_is_high_and_an_unknown_scan_is_not_clear() {
        let mut touched = inputs(vec![file("src/a.rs", 1, 1)]);
        touched.contract_symbols = Some(vec!["check-contract".into()]);
        let c = classify(&ChangeClassificationPolicy::default(), &touched);
        assert_eq!(c.computed_risk, "high");

        let mut unknown = inputs(vec![file("src/a.rs", 1, 1)]);
        unknown.contract_symbols = None;
        let c = classify(&ChangeClassificationPolicy::default(), &unknown);
        assert_eq!(c.signals.contract_surface, ContractSurface::Unknown);
        assert_eq!(
            c.computed_risk, "none",
            "an unknown scan does not raise the label"
        );
        assert_eq!(c.signals.is_set("contract_surface"), Some(true));
    }

    #[test]
    fn a_declaration_raises_risk_but_never_lowers_it() {
        let mut raised = inputs(vec![file("docs/a.md", 1, 0)]);
        raised.declared_risk = Some("High".into());
        let c = classify(&ChangeClassificationPolicy::default(), &raised);
        assert_eq!(
            (c.computed_risk.as_str(), c.risk.as_str()),
            ("none", "high")
        );
        assert!(c.risky);

        let mut lowered = inputs(vec![file(".github/workflows/ci.yml", 1, 0)]);
        lowered.declared_risk = Some("none".into());
        let c = classify(&ChangeClassificationPolicy::default(), &lowered);
        assert_eq!(c.risk, "high");
    }

    #[test]
    fn classification_is_deterministic_whatever_the_input_order() {
        let a = inputs(vec![file("b.rs", 1, 0), file("a.rs", 2, 0)]);
        let b = inputs(vec![file("a.rs", 2, 0), file("b.rs", 1, 0)]);
        let policy = ChangeClassificationPolicy::default();
        assert_eq!(classify(&policy, &a), classify(&policy, &b));
    }

    #[test]
    fn pr_size_comes_from_the_review_table_and_bounds_are_validated() {
        let policy = ChangeClassificationPolicy::from_toml_str(
            "[review]\npr_size = { max_files = 5, max_changed_lines = 50 }\n",
            "config.toml",
        )
        .unwrap();
        assert!(policy.pr_size_configured);
        assert_eq!(policy.pr_size.max_files, 5);
        // The default trivial bound (40 lines) fits under 50.
        let c = classify(&policy, &inputs(vec![file("a.rs", 60, 0)]));
        assert_eq!(c.tier, SizeTier::Large);
        assert!(pr_size_report(&policy, &c).over_threshold);

        let error = ChangeClassificationPolicy::from_toml_str(
            "[review]\npr_size = { max_files = 2, max_changed_lines = 10 }\n",
            "config.toml",
        )
        .unwrap_err();
        assert!(error.to_string().contains("trivial"), "{error}");
    }

    #[test]
    fn numstat_z_reads_renames_and_binary_files() {
        let out = "3\t1\tsrc/a.rs\0-\t-\timg.png\x002\t0\t\0old.rs\0new.rs\0";
        let files = parse_numstat_z(out);
        assert_eq!(
            files,
            vec![
                file("src/a.rs", 3, 1),
                ChangedFile {
                    path: "img.png".into(),
                    added: None,
                    deleted: None
                },
                file("new.rs", 2, 0),
            ]
        );
    }
}
