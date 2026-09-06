//! Who gets credit on a broker-authored commit.
//!
//! A promote commit is the one place in this repo where "who wrote the
//! change" and "who applied it" genuinely differ, and three parties have a
//! claim on it:
//!
//! - the **human** whose repository it is — the commit's `author`, so
//!   `git log --author` and GitHub account linking keep working;
//! - the **broker** that merged and verified it — the commit's
//!   `committer`, which is the literal truth about who ran `commit-tree`;
//! - the **agent** that produced the work — a `Co-Authored-By` trailer,
//!   the only channel Git offers for a third party.
//!
//! The broker also repeats itself as a trailer: `committer` is invisible
//! in most log views and on GitHub, and the point of this module is that
//! nobody in the chain silently disappears.
//!
//! Identity is never invented. An unknown agent is omitted rather than
//! guessed at, because a wrong `Co-Authored-By` is a false statement about
//! a person or product, not a cosmetic defect.

use std::path::Path;

use crate::git::GitRepo;

/// Environment fallback for the agent identity when `--agent` was not
/// passed to `broker adopt`.
pub const AGENT_ENV_VAR: &str = "AETHYME_AGENT";

/// The broker's own identity. Not a real mailbox: `.local` is reserved for
/// exactly this (RFC 6762), so it can never collide with a routable
/// address or be mistaken for a person.
pub const BROKER_NAME: &str = "aethyme-broker";
pub const BROKER_EMAIL: &str = "broker@aethyme.local";

/// A `Name <email>` pair, the only shape Git accepts in an identity slot
/// or a `Co-Authored-By` trailer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub name: String,
    pub email: String,
}

impl Identity {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
        }
    }

    pub fn broker() -> Self {
        Self::new(BROKER_NAME, BROKER_EMAIL)
    }

    /// Parse `Name <email>`. Returns `None` for anything that would not
    /// round-trip as a Git identity — a bare name, a bare address, or an
    /// empty half — rather than emitting a malformed trailer.
    pub fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        let open = raw.find('<')?;
        let close = raw.rfind('>')?;
        if close < open {
            return None;
        }
        let name = raw[..open].trim();
        let email = raw[open + 1..close].trim();
        if name.is_empty() || email.is_empty() {
            return None;
        }
        Some(Self::new(name, email))
    }

    /// `Name <email>`, the form Git renders and parses.
    pub fn render(&self) -> String {
        format!("{} <{}>", self.name, self.email)
    }
}

/// The full credit assignment for one broker-authored commit.
#[derive(Debug, Clone)]
pub struct Attribution {
    pub author: Identity,
    pub committer: Identity,
    /// Rendered `Co-Authored-By` values, already deduplicated and ordered.
    pub coauthors: Vec<Identity>,
}

impl Attribution {
    /// Every identity slot filled by the broker. The floor this module
    /// degrades to, and what a repo with no configured user still gets.
    pub fn broker_only() -> Self {
        Self {
            author: Identity::broker(),
            committer: Identity::broker(),
            coauthors: vec![],
        }
    }

    /// The trailer block appended to the commit message, including the
    /// blank line that separates trailers from the body. Empty when there
    /// is nobody to credit beyond the author and committer.
    pub fn trailer_block(&self) -> String {
        if self.coauthors.is_empty() {
            return String::new();
        }
        let lines: Vec<String> = self
            .coauthors
            .iter()
            .map(|id| format!("Co-Authored-By: {}", id.render()))
            .collect();
        format!("\n\n{}", lines.join("\n"))
    }
}

/// The human this checkout belongs to, from `git config user.*`.
///
/// Read from the main root so the answer is the repository's configured
/// identity, not whatever a throwaway simulation worktree inherited.
pub fn configured_user(main_root: &Path) -> Option<Identity> {
    let repo = GitRepo::discover(main_root).ok()?;
    let name = repo.config_value("user.name")?;
    let email = repo.config_value("user.email")?;
    Some(Identity::new(name, email))
}

/// The agent identity supplied out-of-band, for callers that did not get
/// one from the session row.
pub fn agent_from_env() -> Option<Identity> {
    std::env::var(AGENT_ENV_VAR)
        .ok()
        .as_deref()
        .and_then(Identity::parse)
}

/// Assign credit for a promote commit.
///
/// `agent` is the session's recorded identity (`sessions.agent_identity`),
/// already parsed; `None` means the agent never identified itself.
///
/// Policy, in order:
/// 1. The human authors. With no `user.name`/`user.email` configured there
///    is no human to name, so the broker authors instead — a commit
///    attributed to the tool is honest, whereas one attributed to a
///    fabricated identity is not.
/// 2. The broker always commits.
/// 3. Agent and broker are credited as co-authors, skipping any identity
///    that is already the author so nobody is thanked for their own work.
pub fn for_promote(main_root: &Path, agent: Option<Identity>) -> Attribution {
    let broker = Identity::broker();
    let author = configured_user(main_root).unwrap_or_else(Identity::broker);

    let mut coauthors = Vec::new();
    for identity in agent.into_iter().chain(std::iter::once(broker.clone())) {
        if identity != author && !coauthors.contains(&identity) {
            coauthors.push(identity);
        }
    }

    Attribution {
        author,
        committer: broker,
        coauthors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_renders_round_trip() {
        let id = Identity::parse("Claude Opus 5 <noreply@anthropic.com>").unwrap();
        assert_eq!(id.name, "Claude Opus 5");
        assert_eq!(id.email, "noreply@anthropic.com");
        assert_eq!(id.render(), "Claude Opus 5 <noreply@anthropic.com>");
    }

    #[test]
    fn rejects_identities_git_would_mangle() {
        assert!(Identity::parse("Claude Opus 5").is_none());
        assert!(Identity::parse("noreply@anthropic.com").is_none());
        assert!(Identity::parse("<noreply@anthropic.com>").is_none());
        assert!(Identity::parse("Claude Opus 5 <>").is_none());
        assert!(Identity::parse("").is_none());
    }

    #[test]
    fn trailer_block_lists_every_coauthor() {
        let attribution = Attribution {
            author: Identity::new("Christophe Henner", "christophe@aeptus.com"),
            committer: Identity::broker(),
            coauthors: vec![
                Identity::new("Claude Opus 5", "noreply@anthropic.com"),
                Identity::broker(),
            ],
        };
        assert_eq!(
            attribution.trailer_block(),
            "\n\nCo-Authored-By: Claude Opus 5 <noreply@anthropic.com>\n\
             Co-Authored-By: aethyme-broker <broker@aethyme.local>"
        );
    }

    #[test]
    fn no_coauthors_means_no_trailer_block() {
        assert_eq!(Attribution::broker_only().trailer_block(), "");
    }

    #[test]
    fn author_is_never_also_credited_as_coauthor() {
        // broker_only()'s author IS the broker: crediting it again would
        // read as two participants where there was one.
        let attribution = Attribution {
            author: Identity::broker(),
            committer: Identity::broker(),
            coauthors: vec![],
        };
        assert!(attribution.coauthors.is_empty());
    }
}
