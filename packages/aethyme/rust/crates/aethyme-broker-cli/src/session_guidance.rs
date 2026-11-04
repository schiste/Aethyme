//! The working agreement `broker start` hands an agent.
//!
//! ```toml
//! [session]
//! guidance = [
//!   "Commit each small, coherent step as you go.",
//! ]
//! ```
//!
//! `broker start` is the one moment every agent, whatever its harness, reads
//! the broker's output before it begins work, so the agreement is shown there
//! and nowhere else. Generated instructions point at it instead of repeating
//! it: every line shown costs tokens on every session start.
//!
//! The section is read under the broker's one config trust rule
//! ([`crate::merge::repository_config_text`]): as committed on the fetched
//! default branch, else the main checkout's file. Absent means the built-in
//! default, an empty list turns it off, and an invalid list warns and falls
//! back to the default -- a malformed setting must never stop a session.

use std::path::Path;

/// Most lines an agreement may have. Long instructions are skimmed.
pub const MAX_GUIDANCE_LINES: usize = 6;
/// Longest single guidance line, in characters.
pub const MAX_GUIDANCE_LINE_CHARS: usize = 240;

/// The first default line when agents may push their own branch.
const COMMIT_AND_PUSH: &str = "Commit one small, coherent step at a time: stage explicit paths only (never `git add -A` or `git add .`), check `git diff --cached` before each commit, then `aethyme broker push`; open a draft PR after the first meaningful commit.";
/// The first default line when the repository has no push lane.
const COMMIT_ONLY: &str = "Commit one small, coherent step at a time: stage explicit paths only (never `git add -A` or `git add .`) and check `git diff --cached` before each commit; only committed work can be verified and integrated.";
/// Every default line after the first, which depends on the delivery policy.
const DEFAULT_TAIL: [&str; 5] = [
    "Keep each PR to one concern that a reviewer can read in one sitting; split unrelated changes into separate sessions.",
    "Before writing new code, look for an existing shared component, helper or pattern, and reuse or extend it (`aethyme explore --request \"where is …\"`). Don't add a parallel path, and don't leave duplicated logic behind.",
    "Keep each function and module focused on one responsibility, behind a narrow interface; prefer the simple, direct version over a speculative abstraction.",
    "Every behaviour change ships with a test that fails without it.",
    "Before finishing: nothing unintended committed or pushed, tree clean, branch pushed, PR up to date, and the remaining risks stated in the PR.",
];

/// Where the agreement shown at `start` came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceSource {
    /// The repository's `[session] guidance`.
    Repository,
    /// The built-in default: the section is absent, or it was invalid.
    Default,
    /// `[session] guidance = []`.
    Disabled,
}

/// The agreement to show, and why it is this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGuidance {
    pub lines: Vec<String>,
    pub source: GuidanceSource,
    /// Why a configured list was ignored, when it was.
    pub warning: Option<String>,
}

/// The built-in agreement. Its first line follows the delivery policy: it
/// names `broker push` only where agents are authorized to push.
pub fn default_guidance(push_enabled: bool) -> Vec<String> {
    let first = if push_enabled {
        COMMIT_AND_PUSH
    } else {
        COMMIT_ONLY
    };
    std::iter::once(first)
        .chain(DEFAULT_TAIL)
        .map(str::to_owned)
        .collect()
}

/// Resolve the agreement from configuration text. `None` text means there
/// is no `.aethyme/config.toml`.
pub fn from_config_text(text: Option<&str>, push_enabled: bool) -> SessionGuidance {
    let default = |warning: Option<String>| SessionGuidance {
        lines: default_guidance(push_enabled),
        source: GuidanceSource::Default,
        warning,
    };
    let Some(text) = text else {
        return default(None);
    };
    let value = match text.parse::<toml::Value>() {
        Ok(value) => value,
        // An unreadable file is reported by the commands that own the
        // sections it breaks; the agreement just uses its default.
        Err(_) => return default(None),
    };
    let Some(guidance) = value
        .get("session")
        .and_then(|section| section.get("guidance"))
    else {
        return default(None);
    };
    match validate(guidance) {
        Ok(lines) if lines.is_empty() => SessionGuidance {
            lines,
            source: GuidanceSource::Disabled,
            warning: None,
        },
        Ok(lines) => SessionGuidance {
            lines,
            source: GuidanceSource::Repository,
            warning: None,
        },
        Err(reason) => default(Some(format!(
            "[session] guidance ignored ({reason}); showing the default working agreement"
        ))),
    }
}

fn validate(guidance: &toml::Value) -> Result<Vec<String>, String> {
    let Some(entries) = guidance.as_array() else {
        return Err("it must be a list of strings".into());
    };
    if entries.len() > MAX_GUIDANCE_LINES {
        return Err(format!(
            "{} entries; at most {MAX_GUIDANCE_LINES} are allowed",
            entries.len()
        ));
    }
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let line = entry
                .as_str()
                .ok_or_else(|| format!("entry {} is not a string", index + 1))?
                .trim();
            if line.is_empty() {
                return Err(format!("entry {} is empty", index + 1));
            }
            if line.contains(['\n', '\r']) {
                return Err(format!("entry {} spans several lines", index + 1));
            }
            let chars = line.chars().count();
            if chars > MAX_GUIDANCE_LINE_CHARS {
                return Err(format!(
                    "entry {} is {chars} characters; at most {MAX_GUIDANCE_LINE_CHARS} are allowed",
                    index + 1
                ));
            }
            Ok(line.to_owned())
        })
        .collect()
}

/// The agreement for the repository at `main_root`.
pub fn load(main_root: &Path) -> SessionGuidance {
    let push_enabled = crate::GitRepo::discover(main_root)
        .ok()
        .is_some_and(|repo| crate::session_push::session_push_enabled(&repo));
    from_config_text(
        crate::merge::repository_config_text(main_root).as_deref(),
        push_enabled,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_absent_section_shows_the_default() {
        let guidance = from_config_text(Some("schema = 1\n"), false);
        assert_eq!(guidance.source, GuidanceSource::Default);
        assert_eq!(guidance.lines, default_guidance(false));
        assert_eq!(guidance.lines.len(), MAX_GUIDANCE_LINES);
        assert!(guidance.warning.is_none());
    }

    #[test]
    fn no_config_file_shows_the_default() {
        let guidance = from_config_text(None, true);
        assert_eq!(guidance.source, GuidanceSource::Default);
        assert_eq!(guidance.lines, default_guidance(true));
    }

    #[test]
    fn the_first_default_line_follows_the_push_policy() {
        assert!(default_guidance(true)[0].contains("aethyme broker push"));
        assert!(default_guidance(true)[0].contains("draft PR"));
        assert!(!default_guidance(false)[0].contains("broker push"));
        assert!(!default_guidance(false)[0].contains("draft PR"));
        assert_eq!(default_guidance(true)[1..], default_guidance(false)[1..]);
    }

    /// The first line asks for targeted commits: explicit paths only, and a
    /// look at the staged diff, so nothing unintended is committed or pushed.
    #[test]
    fn the_default_asks_for_targeted_commits_and_a_final_check() {
        for push in [true, false] {
            let lines = default_guidance(push);
            assert!(
                lines[0].contains("stage explicit paths only (never `git add -A` or `git add .`)"),
                "{lines:?}"
            );
            assert!(lines[0].contains("`git diff --cached`"), "{lines:?}");
            assert!(
                lines[5].starts_with("Before finishing: nothing unintended committed or pushed"),
                "{lines:?}"
            );
        }
    }

    #[test]
    fn every_default_line_fits_the_configured_limits() {
        for push in [true, false] {
            let lines = default_guidance(push);
            assert!(lines.len() <= MAX_GUIDANCE_LINES);
            for line in &lines {
                assert!(
                    line.chars().count() <= MAX_GUIDANCE_LINE_CHARS,
                    "{} chars: {line}",
                    line.chars().count()
                );
            }
        }
    }

    #[test]
    fn the_default_asks_agents_to_reuse_shared_components() {
        assert!(
            default_guidance(false)
                .iter()
                .any(|line| line.contains("shared component") && line.contains("aethyme explore"))
        );
    }

    #[test]
    fn repository_guidance_replaces_the_default() {
        let guidance = from_config_text(
            Some(
                "[session]\nguidance = [\"Run the linter first.\", \"  Ask before migrations.  \"]\n",
            ),
            true,
        );
        assert_eq!(guidance.source, GuidanceSource::Repository);
        assert_eq!(
            guidance.lines,
            ["Run the linter first.", "Ask before migrations."]
        );
    }

    #[test]
    fn an_empty_list_disables_the_agreement() {
        let guidance = from_config_text(Some("[session]\nguidance = []\n"), true);
        assert_eq!(guidance.source, GuidanceSource::Disabled);
        assert!(guidance.lines.is_empty());
        assert!(guidance.warning.is_none());
    }

    #[test]
    fn too_many_lines_fall_back_to_the_default_with_a_warning() {
        let seven = (1..=7).map(|n| format!("\"line {n}\"")).collect::<Vec<_>>();
        let text = format!("[session]\nguidance = [{}]\n", seven.join(", "));
        let guidance = from_config_text(Some(&text), false);
        assert_eq!(guidance.source, GuidanceSource::Default);
        assert_eq!(guidance.lines, default_guidance(false));
        assert!(guidance.warning.unwrap().contains("at most 6"));
    }

    #[test]
    fn empty_multiline_overlong_or_non_string_entries_are_rejected() {
        let long = "x".repeat(MAX_GUIDANCE_LINE_CHARS + 1);
        for (text, needle) in [
            (
                "[session]\nguidance = [\"ok\", \"  \"]\n".to_owned(),
                "entry 2 is empty",
            ),
            (
                "[session]\nguidance = [\"a\\nb\"]\n".to_owned(),
                "several lines",
            ),
            (
                format!("[session]\nguidance = [\"{long}\"]\n"),
                "characters",
            ),
            ("[session]\nguidance = [1]\n".to_owned(), "not a string"),
            (
                "[session]\nguidance = \"one\"\n".to_owned(),
                "list of strings",
            ),
        ] {
            let guidance = from_config_text(Some(&text), false);
            assert_eq!(guidance.source, GuidanceSource::Default, "{text}");
            let warning = guidance.warning.expect("a warning");
            assert!(
                warning.contains(needle),
                "{warning} should mention {needle}"
            );
        }
    }

    #[test]
    fn an_exactly_full_agreement_is_accepted() {
        let max = "y".repeat(MAX_GUIDANCE_LINE_CHARS);
        let six = vec![format!("\"{max}\""); MAX_GUIDANCE_LINES];
        let text = format!("[session]\nguidance = [{}]\n", six.join(", "));
        let guidance = from_config_text(Some(&text), false);
        assert_eq!(guidance.source, GuidanceSource::Repository);
        assert_eq!(guidance.lines.len(), MAX_GUIDANCE_LINES);
    }
}
