//! What a routed reviewer is told to produce, and where to put it.
//!
//! [`crate::review_trigger`] decides a review is owed and
//! [`crate::review_backend`] decides who performs it. Neither says what a
//! finding looks like, and until this module existed neither did anything
//! else: the generated prompt asked for "findings, most severe first" and left
//! the reviewer to invent a severity vocabulary, a layout, and a posting
//! command. Two agents reviewing the same pull request then produced two
//! documents that could not be compared, and a third produced a terminal
//! transcript nobody would ever read.
//!
//! Three things are worth stating outright, because each was learned the
//! expensive way:
//!
//! * **The review on the pull request is the record.** A finding that exists
//!   only in the reviewer's terminal is not tracked. So is one in a tab that
//!   was closed. The prompt therefore asks for a posted review even when the
//!   reviewer found nothing -- an unposted review is indistinguishable from a
//!   review that never ran, and the ledger row says `running` for both.
//! * **A severity ladder only works if everyone uses the same rungs.** They
//!   are configured here rather than baked in, because "P1" means something
//!   different to a repository shipping firmware than to one shipping a CLI,
//!   but within one repository they must mean one thing.
//! * **A finding without a location is not actionable.** The reviewer has the
//!   diff open and the reader does not.
//!
//! As everywhere else in this area, nothing here performs anything. The output
//! is text that goes into a prompt.

use std::path::Path;

use serde::{Deserialize, Serialize};

/// Current shape of the `[review.reporting]` table.
pub const REVIEW_REPORTING_SCHEMA_VERSION: u32 = 1;

/// One rung of a repository's severity ladder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSeverity {
    /// What the reviewer writes in the finding's heading, e.g. `P1`.
    pub label: String,
    /// What that rung means here. This reaches the reviewer verbatim, so it is
    /// the entire definition -- a label with a vague `means` produces findings
    /// sorted by vibe.
    pub means: String,
}

/// The `[review.reporting]` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReportingPolicy {
    #[serde(default = "default_reporting_schema_version")]
    pub schema_version: u32,
    /// The severity ladder, most severe first. Order is the definition of
    /// "most severe first" in every other instruction here.
    #[serde(default = "default_severity")]
    pub severity: Vec<ReviewSeverity>,
    /// Severity at or above which the reviewer submits `--request-changes`
    /// rather than `--comment`. `None` means always comment.
    ///
    /// Unset by default, and the default is not timidity. GitHub refuses
    /// `--request-changes` and `--approve` on a pull request the caller
    /// authored, so a repository whose reviewers run under the author's own
    /// account gets a failed post -- a review that was written, refused at the
    /// last step, and then exists nowhere. Set this only once the reviewers
    /// have an identity of their own.
    #[serde(default)]
    pub request_changes_at: Option<String>,
    /// Post through `aethyme broker gh` rather than bare `gh`.
    ///
    /// On by default: a reviewer is an agent doing work in a shared
    /// repository, and posting a review is a GitHub mutation like any other.
    /// "It is only a comment" is how writes end up outside the journal.
    #[serde(default = "default_true")]
    pub coordinated: bool,
    /// Require every finding to name `path:line`.
    #[serde(default = "default_true")]
    pub require_location: bool,
    /// Most findings one review may carry. `0` means unbounded.
    ///
    /// A cap is a forcing function, not a quota: a reviewer that must choose
    /// ten ranks its findings, and one that need not choose reports forty
    /// undifferentiated observations that nobody reads to the end of.
    #[serde(default = "default_max_findings")]
    pub max_findings: u32,
}

fn default_reporting_schema_version() -> u32 {
    REVIEW_REPORTING_SCHEMA_VERSION
}

fn default_true() -> bool {
    true
}

fn default_max_findings() -> u32 {
    10
}

/// The ladder a repository gets when it configures none.
///
/// Deliberately opinionated rather than empty. An unconfigured repository that
/// turned routing on is the case this whole module exists for, and handing it
/// no vocabulary would leave it exactly where it started.
fn default_severity() -> Vec<ReviewSeverity> {
    [
        (
            "P0",
            "ship-blocking: data loss, a security hole, a broken build, or a \
             correctness bug on a path users reach",
        ),
        (
            "P1",
            "must fix before merge: a real defect with a narrower trigger, or a \
             missing check the change itself depends on",
        ),
        (
            "P2",
            "worth fixing: maintainability, a gap in test coverage, a comment \
             that will mislead the next reader",
        ),
        ("P3", "optional: naming, style, preference"),
    ]
    .into_iter()
    .map(|(label, means)| ReviewSeverity {
        label: label.into(),
        means: means.into(),
    })
    .collect()
}

impl Default for ReviewReportingPolicy {
    fn default() -> Self {
        Self {
            schema_version: REVIEW_REPORTING_SCHEMA_VERSION,
            severity: default_severity(),
            request_changes_at: None,
            coordinated: default_true(),
            require_location: default_true(),
            max_findings: default_max_findings(),
        }
    }
}

/// Why a reporting policy could not be used.
#[derive(Debug, thiserror::Error)]
pub enum ReviewReportingError {
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
        "{path}: review.reporting schema_version {found} is newer than this broker understands \
         ({supported}); upgrade aethyme or pin the policy"
    )]
    UnsupportedSchema {
        path: String,
        found: u32,
        supported: u32,
    },
    #[error("{path}: review.reporting declares no severity levels; findings would be untagged")]
    NoSeverities { path: String },
    #[error("{path}: review.reporting severity label {label:?} appears twice")]
    DuplicateSeverity { path: String, label: String },
    #[error(
        "{path}: review.reporting request_changes_at = {label:?} names no declared severity \
         (have: {declared})"
    )]
    UnknownThreshold {
        path: String,
        label: String,
        declared: String,
    },
}

impl ReviewReportingPolicy {
    /// Load from `.aethyme/config.toml`, defaulting to the built-in ladder when
    /// the file or the table is absent.
    ///
    /// Absent means default, not disabled: there is no "report nothing" mode,
    /// because a review that reports nothing anywhere was not worth
    /// dispatching. A repository that wants no reviewers turns off
    /// `[review.routing]`.
    pub fn load(root: &Path) -> Result<Self, ReviewReportingError> {
        let path = root.join(".aethyme/config.toml");
        let display = path.display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(ReviewReportingError::Read {
                    path: display,
                    source,
                });
            }
        };
        let value: toml::Value = text.parse().map_err(|source| ReviewReportingError::Parse {
            path: display.clone(),
            source,
        })?;
        let Some(table) = value
            .get("review")
            .and_then(|review| review.get("reporting"))
        else {
            return Ok(Self::default());
        };
        let policy: Self =
            table
                .clone()
                .try_into()
                .map_err(|source| ReviewReportingError::Parse {
                    path: display.clone(),
                    source,
                })?;
        policy.validate(&display)?;
        Ok(policy)
    }

    fn validate(&self, path: &str) -> Result<(), ReviewReportingError> {
        if self.schema_version > REVIEW_REPORTING_SCHEMA_VERSION {
            return Err(ReviewReportingError::UnsupportedSchema {
                path: path.to_string(),
                found: self.schema_version,
                supported: REVIEW_REPORTING_SCHEMA_VERSION,
            });
        }
        if self.severity.is_empty() {
            return Err(ReviewReportingError::NoSeverities {
                path: path.to_string(),
            });
        }
        // Two rungs with one label make "most severe first" unorderable, and
        // the reviewer cannot be told which of them it meant.
        for (index, level) in self.severity.iter().enumerate() {
            if self.severity[..index]
                .iter()
                .any(|earlier| earlier.label == level.label)
            {
                return Err(ReviewReportingError::DuplicateSeverity {
                    path: path.to_string(),
                    label: level.label.clone(),
                });
            }
        }
        // A threshold naming an undeclared rung never fires, so every review
        // would quietly be a comment -- the failure is invisible at exactly the
        // moment the repository believed it had started blocking merges.
        if let Some(threshold) = &self.request_changes_at
            && self.index_of(threshold).is_none()
        {
            return Err(ReviewReportingError::UnknownThreshold {
                path: path.to_string(),
                label: threshold.clone(),
                declared: self
                    .severity
                    .iter()
                    .map(|level| level.label.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
        Ok(())
    }

    fn index_of(&self, label: &str) -> Option<usize> {
        self.severity
            .iter()
            .position(|level| level.label == label)
    }

    /// The labels that make a review `--request-changes`, most severe first.
    ///
    /// Empty when no threshold is set, which is also the answer the prompt
    /// needs: nothing blocks, so every review is a comment.
    pub fn blocking_labels(&self) -> Vec<&str> {
        let Some(threshold) = self.request_changes_at.as_deref() else {
            return Vec::new();
        };
        let Some(last) = self.index_of(threshold) else {
            return Vec::new();
        };
        self.severity[..=last]
            .iter()
            .map(|level| level.label.as_str())
            .collect()
    }

    /// The reporting half of a reviewer's prompt.
    ///
    /// Rendered per review rather than stored, because every line of it names
    /// the pull request, the dimension, or the repository. A reviewer handed a
    /// generic version would have to fill in the blanks, and the one that
    /// matters most -- the pull request number -- is the one it cannot guess.
    pub fn instructions(&self, review_type: &str, repository: &str, pull_request: i64) -> String {
        let mut out = String::new();
        out.push_str(
            "## Reporting\n\n\
             Post the result as a review on the pull request. That review is the record: a \
             finding that lives only in this terminal is not tracked, and an unposted review \
             cannot be told apart from one that never ran. Post even when you find nothing.\n\n",
        );

        if self.coordinated {
            out.push_str(&format!(
                "Posting is a coordinated write, and this workspace is a worktree with no \
                 session of its own. Take one, post, close it:\n\n    \
                 aethyme broker adopt --task \"{review_type} review of #{pull_request}\"\n    \
                 aethyme broker gh --session <id> --repo {repository} \\\n        \
                 --reason \"post the {review_type} review on #{pull_request}\" -- \\\n        \
                 pr review {pull_request} --comment --body-file <file>\n    \
                 aethyme broker close --session <id>\n\n\
                 Running `gh` directly puts a shared-state write outside the operations \
                 journal.\n\n"
            ));
        } else {
            out.push_str(&format!(
                "Post it with:\n\n    \
                 gh pr review {pull_request} --repo {repository} --comment --body-file <file>\n\n"
            ));
        }

        let blocking = self.blocking_labels();
        if !blocking.is_empty() {
            out.push_str(&format!(
                "Submit `--request-changes` instead of `--comment` when any finding is {}.\n\n",
                join_with_or(&blocking)
            ));
        }

        let top = self.severity[0].label.as_str();
        let location = if self.require_location {
            "\n    `path/to/file.rs:123`\n"
        } else {
            ""
        };
        out.push_str(&format!(
            "Body format -- one `###` section per finding, most severe first:\n\n    \
             ## {heading}\n\n    \
             ### [{top}] One line naming the defect, not the file\n{location}\n    \
             What is wrong, the state or input that reaches it, and what it costs.\n    \
             Name the fix if you have one.\n\n",
            heading = review_heading(review_type),
        ));

        out.push_str("Severity -- exactly one per finding:\n\n");
        for level in &self.severity {
            out.push_str(&format!("    {} -- {}\n", level.label, level.means));
        }
        out.push('\n');

        out.push_str("Rules:\n");
        out.push_str(
            "- Tag every finding with exactly one label from that list, in its heading: \
             `### [LABEL] summary`.\n",
        );
        if self.require_location {
            out.push_str(
                "- Anchor every finding to `path:line`. A finding the reader cannot locate is \
                 not actionable.\n",
            );
        }
        if self.max_findings > 0 {
            out.push_str(&format!(
                "- At most {} findings. If you have more, report the {} most severe and say how \
                 many you left out.\n",
                self.max_findings, self.max_findings
            ));
        }
        out.push_str(&format!(
            "- Stay inside {review_type}; another reviewer covers the rest.\n\
             - Found nothing? Post `No {review_type} findings.` and nothing else.\n"
        ));
        out
    }
}

/// `security` -> `Security review`, for the body's own heading.
fn review_heading(review_type: &str) -> String {
    let mut chars = review_type.chars();
    match chars.next() {
        Some(first) => format!(
            "{}{} review",
            first.to_uppercase(),
            chars.as_str().replace(['-', '_'], " ")
        ),
        None => "Review".to_string(),
    }
}

/// `["P0", "P1"]` -> `P0 or P1`, so the sentence reads as a sentence.
fn join_with_or(labels: &[&str]) -> String {
    match labels {
        [] => String::new(),
        [only] => (*only).to_string(),
        [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(body: &str) -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(temp.path().join(".aethyme")).unwrap();
        std::fs::write(temp.path().join(".aethyme/config.toml"), body).unwrap();
        temp
    }

    #[test]
    fn a_repository_with_no_config_still_gets_a_ladder() {
        let temp = tempfile::tempdir().unwrap();
        let policy = ReviewReportingPolicy::load(temp.path()).unwrap();
        assert_eq!(policy.severity.len(), 4);
        assert_eq!(policy.severity[0].label, "P0");
        // Nothing blocks until a repository says so, because requesting
        // changes on your own pull request is refused by GitHub.
        assert!(policy.blocking_labels().is_empty());
    }

    #[test]
    fn a_configured_ladder_replaces_the_default_entirely() {
        let temp = write_config(
            r#"
[review.reporting]
request_changes_at = "major"
max_findings = 3
[[review.reporting.severity]]
label = "major"
means = "breaks a user"
[[review.reporting.severity]]
label = "minor"
means = "annoys a maintainer"
"#,
        );
        let policy = ReviewReportingPolicy::load(temp.path()).unwrap();
        assert_eq!(policy.blocking_labels(), vec!["major"]);
        let text = policy.instructions("security", "o/r", 7);
        assert!(text.contains("breaks a user"), "{text}");
        assert!(!text.contains("P0"), "the default ladder is gone: {text}");
        assert!(text.contains("At most 3 findings"), "{text}");
    }

    /// A threshold naming no declared rung never fires, and the repository
    /// finds out only by noticing that nothing has ever blocked.
    #[test]
    fn a_threshold_that_names_no_declared_severity_is_refused() {
        let temp = write_config(
            r#"
[review.reporting]
request_changes_at = "P9"
"#,
        );
        let error = ReviewReportingPolicy::load(temp.path()).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("P9"), "{message}");
        assert!(message.contains("P0, P1, P2, P3"), "{message}");
    }

    #[test]
    fn a_duplicated_severity_label_is_refused() {
        let temp = write_config(
            r#"
[review.reporting]
[[review.reporting.severity]]
label = "P1"
means = "one"
[[review.reporting.severity]]
label = "P1"
means = "the other"
"#,
        );
        assert!(
            ReviewReportingPolicy::load(temp.path())
                .unwrap_err()
                .to_string()
                .contains("appears twice")
        );
    }

    #[test]
    fn an_empty_ladder_is_refused_rather_than_producing_untagged_findings() {
        let temp = write_config(
            r#"
[review.reporting]
severity = []
"#,
        );
        assert!(
            ReviewReportingPolicy::load(temp.path())
                .unwrap_err()
                .to_string()
                .contains("no severity levels")
        );
    }

    #[test]
    fn the_instructions_name_the_pull_request_the_repository_and_the_dimension() {
        let policy = ReviewReportingPolicy::default();
        let text = policy.instructions("security", "schiste/Aethyme", 179);
        assert!(text.contains("#179"), "{text}");
        assert!(text.contains("schiste/Aethyme"), "{text}");
        assert!(text.contains("## Security review"), "{text}");
        assert!(text.contains("Stay inside security"), "{text}");
        assert!(text.contains("No security findings."), "{text}");
    }

    #[test]
    fn coordinated_posting_takes_a_session_and_bare_posting_does_not() {
        let coordinated = ReviewReportingPolicy::default().instructions("code", "o/r", 1);
        assert!(coordinated.contains("aethyme broker adopt"), "{coordinated}");
        assert!(coordinated.contains("aethyme broker gh"), "{coordinated}");

        let direct = ReviewReportingPolicy {
            coordinated: false,
            ..Default::default()
        }
        .instructions("code", "o/r", 1);
        assert!(!direct.contains("broker"), "{direct}");
        assert!(direct.contains("gh pr review 1 --repo o/r"), "{direct}");
    }

    #[test]
    fn the_location_rule_disappears_when_a_repository_turns_it_off() {
        let text = ReviewReportingPolicy {
            require_location: false,
            ..Default::default()
        }
        .instructions("code", "o/r", 1);
        assert!(!text.contains("path/to/file.rs"), "{text}");
        assert!(!text.contains("path:line"), "{text}");
    }

    #[test]
    fn a_multi_rung_threshold_reads_as_a_sentence() {
        let policy = ReviewReportingPolicy {
            request_changes_at: Some("P1".into()),
            ..Default::default()
        };
        assert_eq!(policy.blocking_labels(), vec!["P0", "P1"]);
        assert!(
            policy
                .instructions("code", "o/r", 1)
                .contains("any finding is P0 or P1")
        );
    }

    #[test]
    fn a_newer_schema_is_refused_by_name() {
        let temp = write_config(
            r#"
[review.reporting]
schema_version = 99
"#,
        );
        assert!(
            ReviewReportingPolicy::load(temp.path())
                .unwrap_err()
                .to_string()
                .contains("newer than this broker understands")
        );
    }
}
