//! Autofix (fix side of the quality domain) — port of
//! `src/autofixers/`.
//!
//! Layering matches the Python package: `safety` decides what may be
//! touched and how risky it is, `patch` turns proposed content into
//! diffs and applies them, the fixers propose the content, and
//! `github` wraps the whole thing in a branch/commit/push/PR flow.
//!
//! Byte parity is the contract (retirement decision #2): produced
//! unified diffs, stdout, exit codes, and post-apply trees match the
//! Python implementation on the parity corpus. Where the Python has a
//! quirk — malformed-looking diffs from `lineterm=""`, `str.replace`
//! against a progressively-rewritten buffer, risk patterns matched
//! against absolute paths — the quirk is ported, not corrected.

pub mod difflib;
pub mod fixers;
pub mod patch;
pub mod pystr;
pub mod safety;

use std::path::Path;

use fixers::FixProposal;

/// Which fixers a run selects — the `--fix-type` choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixSelection {
    All,
    Docs,
    Links,
    Selectors,
    I18n,
    Format,
}

impl FixSelection {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "all" => FixSelection::All,
            "docs" => FixSelection::Docs,
            "links" => FixSelection::Links,
            "selectors" => FixSelection::Selectors,
            "i18n" => FixSelection::I18n,
            "format" => FixSelection::Format,
            _ => return None,
        })
    }

    fn includes(self, group: FixSelection) -> bool {
        self == FixSelection::All || self == group
    }
}

/// One scanning pass, in the order `cli.py` runs them: docs, links,
/// selectors, i18n, format. Order matters — it is patch order, hence
/// diff order.
pub fn collect_group(
    repo_path: &Path,
    selection: FixSelection,
    group: FixSelection,
) -> Option<Vec<FixProposal>> {
    if !selection.includes(group) {
        return None;
    }
    Some(match group {
        FixSelection::Docs => fixers::DocsRegenerator::new(repo_path).create_folder_docs(),
        FixSelection::Links => {
            fixers::process_directory(&fixers::LinkFixer::new(repo_path), repo_path)
        }
        FixSelection::Selectors => {
            fixers::process_directory(&fixers::SelectorInserter::new(repo_path), repo_path)
        }
        _ => Vec::new(),
    })
}
