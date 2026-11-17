//! Standing authorizations a repository grants its agents.
//!
//! ```toml
//! [authorization]
//! prepare_session_worktrees = true     # default
//! disposable_test_environments = true  # default
//! ```
//!
//! Two routine, bounded actions agents otherwise stop to ask about: running
//! the repository's declared dependency preparation in their own session
//! worktree, and creating and dropping disposable test environments. Both are
//! granted when absent, so a repository opts out rather than in; `false`
//! tells agents to ask the owner first. A grant is standing: it covers every
//! session, so an agent never re-asks per session id. Neither key covers
//! anything else, such as dispatching CI workflows.
//!
//! The section is its own table rather than more `[delivery]` keys: the
//! broker parses `[delivery]` with unknown keys refused, so an older runtime
//! would reject a delivery policy carrying them and stop pushing.
//!
//! One module owns the parse and the wording, so the generated agent files
//! (`aethyme deploy`) and the working agreement `broker start` prints cannot
//! drift apart.

use std::path::Path;

/// The configuration section that holds the standing authorizations.
pub const SECTION: &str = "authorization";
/// Key for running declared preparation without asking.
pub const PREPARE_SESSION_WORKTREES: &str = "prepare_session_worktrees";
/// Key for creating and dropping disposable test environments without asking.
pub const DISPOSABLE_TEST_ENVIRONMENTS: &str = "disposable_test_environments";

/// Column the generated paragraph wraps at, like the hand-wrapped text
/// around it.
const WRAP_COLUMN: usize = 80;

const PREPARE_GRANTED: &str =
    "`aethyme broker submit prepare --session <id>` in your own worktree \
as soon as the broker reports it required (the declared frozen, pinned installs; no dependency or \
lockfile change)";
const PREPARE_WITHHELD: &str = "running `aethyme broker submit prepare`";
const ENVIRONMENTS_GRANTED: &str = "creating, using and dropping local test databases and \
environments named for the session (`$AETHYME_TEST_DB_SUFFIX` or the script's own scratch name), \
broker-leased (`aethyme env`) or made by the repository's declared test or scratch commands, then \
dropped (never shared, staging or production data)";
const ENVIRONMENTS_WITHHELD: &str = "creating test environments or databases";

/// `broker start` working-agreement lines: one per key, in full.
const PREPARE_GRANTED_LINE: &str = "Preparation is a standing authorization for every session (no \
need to ask again per session id): when the broker reports preparation required, run `aethyme \
broker submit prepare --session <id>` at once, in your own session worktree only. It installs the \
repository's declared, frozen and pinned dependency set with lockfiles unchanged: no dependency \
upgrades, additions or lockfile edits. Anything else still needs the owner.";
const PREPARE_WITHHELD_LINE: &str = "Ask the owner before running `aethyme broker submit prepare \
--session <id>` or installing dependencies in a session worktree \
([authorization] prepare_session_worktrees = false).";
const ENVIRONMENTS_GRANTED_LINE: &str = "Disposable test state is a standing authorization for every \
session (no need to ask again per session id): create, use and drop local test databases and \
environments, whether broker-leased (`aethyme env up`, `aethyme env exec`, `aethyme env down <id>`) \
or made by the repository's own declared test or scratch commands, when each is named for this \
session (`$AETHYME_TEST_DB_SUFFIX` or the script's own scratch naming) and dropped afterwards. \
Never a shared, staging or production database, service or data (not `aethyme env shared`), never \
data that outlives the session; nothing else is implied.";
const ENVIRONMENTS_WITHHELD_LINE: &str = "Ask the owner before creating any test environment or \
database, including the environments the repository declares \
([authorization] disposable_test_environments = false).";

/// What the repository's `[authorization]` section grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct StandingAuthorizations {
    /// Run `aethyme broker submit prepare --session <id>` without asking.
    pub prepare_session_worktrees: bool,
    /// Create, use and drop declared environments and suffixed test
    /// databases without asking.
    pub disposable_test_environments: bool,
}

impl Default for StandingAuthorizations {
    /// Both granted: what an absent section means.
    fn default() -> Self {
        Self {
            prepare_session_worktrees: true,
            disposable_test_environments: true,
        }
    }
}

impl StandingAuthorizations {
    /// Nothing granted: what an unreadable configuration means.
    pub const NONE: Self = Self {
        prepare_session_worktrees: false,
        disposable_test_environments: false,
    };

    /// Read from a parsed configuration; `None` means there is no
    /// configuration file.
    ///
    /// An absent section or key grants; an explicit boolean is taken as
    /// written; anything else (a string, a section that is not a table)
    /// withholds, so a malformed declaration never grants more than a
    /// readable one could.
    pub fn from_config(config: Option<&toml::Value>) -> Self {
        let Some(section) = config.and_then(|config| config.get(SECTION)) else {
            return Self::default();
        };
        let Some(table) = section.as_table() else {
            return Self::NONE;
        };
        let granted = |key: &str| {
            table
                .get(key)
                .is_none_or(|value| value.as_bool() == Some(true))
        };
        Self {
            prepare_session_worktrees: granted(PREPARE_SESSION_WORKTREES),
            disposable_test_environments: granted(DISPOSABLE_TEST_ENVIRONMENTS),
        }
    }

    /// Read from configuration text as TOML; `None` means there is no file.
    /// Text that does not parse grants nothing.
    pub fn from_config_text(text: Option<&str>) -> Self {
        match text.map(str::parse::<toml::Value>) {
            None => Self::default(),
            Some(Ok(value)) => Self::from_config(Some(&value)),
            Some(Err(_)) => Self::NONE,
        }
    }

    /// Read the repository's working-copy `.aethyme/config` (any supported
    /// format), as `aethyme deploy` renders from it.
    pub fn load(repo: &Path) -> Self {
        match aethyme_config::load_config(&repo.join(aethyme_config::CONFIG_DIR), "config") {
            Ok(None) => Self::default(),
            Ok(Some(document)) => Self::from_config(Some(&document.value)),
            Err(_) => Self::NONE,
        }
    }

    /// The paragraph the generated agent instructions carry: what is granted
    /// without asking, then what still needs the owner.
    pub fn agents_paragraph(&self) -> String {
        let mut granted = Vec::new();
        let mut withheld = Vec::new();
        if self.prepare_session_worktrees {
            granted.push(PREPARE_GRANTED);
        } else {
            withheld.push(PREPARE_WITHHELD);
        }
        if self.disposable_test_environments {
            granted.push(ENVIRONMENTS_GRANTED);
        } else {
            withheld.push(ENVIRONMENTS_WITHHELD);
        }
        let mut sentences = Vec::new();
        if !granted.is_empty() {
            sentences.push(format!(
                "Standing authorizations (`[{SECTION}]`), for every session without asking \
                 again: {}. Nothing else is implied.",
                granted.join("; ")
            ));
        }
        if !withheld.is_empty() {
            sentences.push(format!(
                "Ask the owner before {} (`[{SECTION}]`).",
                withheld.join(" or ")
            ));
        }
        wrap(&sentences.join(" "), WRAP_COLUMN)
    }

    /// The lines `broker start` prints under its working agreement.
    pub fn agreement_lines(&self) -> Vec<&'static str> {
        vec![
            if self.prepare_session_worktrees {
                PREPARE_GRANTED_LINE
            } else {
                PREPARE_WITHHELD_LINE
            },
            if self.disposable_test_environments {
                ENVIRONMENTS_GRANTED_LINE
            } else {
                ENVIRONMENTS_WITHHELD_LINE
            },
        ]
    }
}

/// Greedy word wrap on single spaces: a word that would pass `column` starts
/// the next line, and a word longer than `column` stands alone.
fn wrap(text: &str, column: usize) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for word in text.split(' ') {
        if line.is_empty() {
            line.push_str(word);
        } else if line.chars().count() + 1 + word.chars().count() <= column {
            line.push(' ');
            line.push_str(word);
        } else {
            lines.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    lines.push(line);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> StandingAuthorizations {
        StandingAuthorizations::from_config_text(Some(text))
    }

    #[test]
    fn both_are_granted_when_absent() {
        let both = StandingAuthorizations::default();
        assert!(both.prepare_session_worktrees && both.disposable_test_environments);
        assert_eq!(StandingAuthorizations::from_config_text(None), both);
        assert_eq!(parsed("schema = 1\n"), both);
        assert_eq!(parsed("[authorization]\n"), both);
        assert_eq!(parsed("[delivery]\npush_session_branches = true\n"), both);
    }

    #[test]
    fn an_explicit_false_withholds_only_its_own_key() {
        assert_eq!(
            parsed("[authorization]\nprepare_session_worktrees = false\n"),
            StandingAuthorizations {
                prepare_session_worktrees: false,
                disposable_test_environments: true,
            }
        );
        assert_eq!(
            parsed("[authorization]\ndisposable_test_environments = false\n"),
            StandingAuthorizations {
                prepare_session_worktrees: true,
                disposable_test_environments: false,
            }
        );
        assert_eq!(
            parsed(
                "[authorization]\nprepare_session_worktrees = false\n\
                 disposable_test_environments = false\n"
            ),
            StandingAuthorizations::NONE
        );
    }

    #[test]
    fn an_explicit_true_grants() {
        assert_eq!(
            parsed(
                "[authorization]\nprepare_session_worktrees = true\n\
                 disposable_test_environments = true\n"
            ),
            StandingAuthorizations::default()
        );
    }

    #[test]
    fn a_malformed_declaration_grants_nothing_it_names() {
        assert!(
            !parsed("[authorization]\nprepare_session_worktrees = \"yes\"\n")
                .prepare_session_worktrees
        );
        assert_eq!(
            parsed("authorization = true\n"),
            StandingAuthorizations::NONE
        );
        assert_eq!(
            parsed("[authorization\nprepare_session_worktrees = true\n"),
            StandingAuthorizations::NONE
        );
    }

    #[test]
    fn the_granted_paragraph_states_the_bounds() {
        let text = StandingAuthorizations::default()
            .agents_paragraph()
            .replace('\n', " ");
        for needle in [
            "Standing authorizations (`[authorization]`), for every session without asking again:",
            "`aethyme broker submit prepare --session <id>` in your own worktree",
            "as soon as the broker reports it required",
            "frozen, pinned installs; no dependency or lockfile change",
            "local test databases and environments named for the session",
            "`$AETHYME_TEST_DB_SUFFIX` or the script's own scratch name",
            "broker-leased (`aethyme env`) or made by the repository's declared test or scratch",
            "then dropped (never shared, staging or production data)",
            "Nothing else is implied.",
        ] {
            assert!(text.contains(needle), "missing {needle:?}: {text}");
        }
        assert!(!text.contains("Ask the owner"), "{text}");
    }

    #[test]
    fn the_withheld_paragraph_says_to_ask_the_owner() {
        let text = StandingAuthorizations::NONE
            .agents_paragraph()
            .replace('\n', " ");
        assert_eq!(
            text,
            "Ask the owner before running `aethyme broker submit prepare` or creating test \
             environments or databases (`[authorization]`)."
        );
        let mixed = StandingAuthorizations {
            prepare_session_worktrees: true,
            disposable_test_environments: false,
        }
        .agents_paragraph()
        .replace('\n', " ");
        assert!(
            mixed.contains("again: `aethyme broker submit prepare"),
            "{mixed}"
        );
        assert!(
            mixed.contains("Ask the owner before creating test environments or databases"),
            "{mixed}"
        );
    }

    #[test]
    fn the_paragraph_wraps_at_eighty_columns() {
        for authorizations in [
            StandingAuthorizations::default(),
            StandingAuthorizations::NONE,
        ] {
            let paragraph = authorizations.agents_paragraph();
            for line in paragraph.lines() {
                assert!(line.chars().count() <= WRAP_COLUMN, "{line:?}");
            }
        }
        assert_eq!(wrap("aa bb cc", 5), "aa bb\ncc");
        assert_eq!(wrap("aaaaaaa b", 3), "aaaaaaa\nb");
    }

    #[test]
    fn agreement_lines_follow_each_key() {
        let granted = StandingAuthorizations::default().agreement_lines();
        for line in &granted {
            assert!(
                line.contains("standing authorization for every session"),
                "{line}"
            );
            assert!(
                line.contains("no need to ask again per session id"),
                "{line}"
            );
        }
        assert!(granted[0].contains("run `aethyme broker submit prepare --session <id>` at once"));
        assert!(granted[0].contains("in your own session worktree only"));
        assert!(granted[0].contains("frozen and pinned dependency set with lockfiles unchanged"));
        assert!(granted[1].contains("`aethyme env up`"));
        assert!(granted[1].contains("repository's own declared test or scratch commands"));
        assert!(granted[1].contains("$AETHYME_TEST_DB_SUFFIX"));
        assert!(granted[1].contains("dropped afterwards"));
        assert!(granted[1].contains("nothing else is implied"));
        assert!(!granted.iter().any(|line| line.contains("workflow")));
        let withheld = StandingAuthorizations::NONE.agreement_lines();
        assert!(withheld
            .iter()
            .all(|line| line.starts_with("Ask the owner")));
        assert!(withheld[0].contains("prepare_session_worktrees = false"));
        assert!(withheld[1].contains("disposable_test_environments = false"));
    }
}
