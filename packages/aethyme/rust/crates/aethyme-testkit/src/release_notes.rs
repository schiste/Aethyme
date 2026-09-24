//! The release-notes contract: `CHANGELOG.md` plus `UPGRADING.md`.
//!
//! Every release has a `## [X.Y.Z] - <date>` entry in `CHANGELOG.md`. A
//! release that needs more than installing the new binary pair (a one-way
//! schema migration, a removed or changed command, flag or exit code, a new
//! install requirement) also has a `## vX.Y.Z` section in `UPGRADING.md`, and
//! its CHANGELOG entry opens with [`BREAKING_MARKER`]. The GitHub release body
//! is the CHANGELOG entry followed by the UPGRADING section when there is one.
//!
//! This module is shared by the `release_notes` example (which the release
//! workflow runs to write the body) and the `release_contract` suite (which
//! checks the pairing for every release), so the renderer the workflow uses
//! is the one the tests exercise.

use std::collections::BTreeMap;

/// Opens the CHANGELOG entry of a release that has an `UPGRADING.md` section.
pub const BREAKING_MARKER: &str = "**Breaking:**";

/// Headings every `UPGRADING.md` section must carry.
pub const REQUIRED_UPGRADE_HEADINGS: &[&str] = &[
    "### Compatibility",
    "### Migrate and verify",
    "### Rollback",
];

/// `0.8.4` -> `v084`, the GitHub anchor of the `## v0.8.4` heading.
pub fn upgrade_anchor(version: &str) -> String {
    format!("v{}", version.replace('.', ""))
}

/// Split a document into `(key, body)` pairs at level-2 headings, where
/// `key` returns the version a heading names (or `None` for other headings,
/// which end the previous section without starting a new one).
fn sections(text: &str, key: impl Fn(&str) -> Option<String>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut current: Option<(String, Vec<&str>)> = None;
    let mut in_fence = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
        }
        if !in_fence && line.starts_with("## ") {
            if let Some((version, body)) = current.take() {
                out.insert(version, body.join("\n").trim().to_string());
            }
            current = key(line).map(|version| (version, Vec::new()));
            continue;
        }
        if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
    }
    if let Some((version, body)) = current {
        out.insert(version, body.join("\n").trim().to_string());
    }
    out
}

/// CHANGELOG entries keyed by version: `## [X.Y.Z] - <date>`. The
/// `[Unreleased]` entry is not a release and is skipped.
pub fn changelog_entries(changelog: &str) -> BTreeMap<String, String> {
    sections(changelog, |line| {
        let rest = line.strip_prefix("## [")?;
        let (version, tail) = rest.split_once(']')?;
        (tail.starts_with(" - ") && version != "Unreleased").then(|| version.to_string())
    })
}

/// UPGRADING sections keyed by version: `## vX.Y.Z`.
pub fn upgrading_sections(upgrading: &str) -> BTreeMap<String, String> {
    sections(upgrading, |line| {
        let version = line.strip_prefix("## v")?.trim();
        (!version.is_empty()
            && version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        .then(|| version.to_string())
    })
}

/// Whether a CHANGELOG entry declares itself breaking.
pub fn is_marked_breaking(entry: &str) -> bool {
    entry.trim_start().starts_with(BREAKING_MARKER)
}

/// Every violation of the CHANGELOG/UPGRADING pairing, one message each.
pub fn contract_violations(changelog: &str, upgrading: &str) -> Vec<String> {
    let entries = changelog_entries(changelog);
    let upgrades = upgrading_sections(upgrading);
    let mut problems = Vec::new();
    for (version, entry) in &entries {
        let marked = is_marked_breaking(entry);
        match (marked, upgrades.contains_key(version)) {
            (true, false) => problems.push(format!(
                "CHANGELOG {version} is marked {BREAKING_MARKER} but UPGRADING.md has no `## v{version}` section"
            )),
            (false, true) => problems.push(format!(
                "UPGRADING.md has `## v{version}` but its CHANGELOG entry does not open with {BREAKING_MARKER}"
            )),
            _ => {}
        }
        let anchor = format!("UPGRADING.md#{}", upgrade_anchor(version));
        if marked && !entry.lines().next().unwrap_or("").contains(&anchor) {
            problems.push(format!(
                "CHANGELOG {version}: the {BREAKING_MARKER} line must link {anchor}"
            ));
        }
    }
    for (version, section) in &upgrades {
        if !entries.contains_key(version) {
            problems.push(format!(
                "UPGRADING.md has `## v{version}` but CHANGELOG.md has no `## [{version}] - <date>` entry"
            ));
        }
        for heading in REQUIRED_UPGRADE_HEADINGS {
            if !section.lines().any(|line| line.trim_end() == *heading) {
                problems.push(format!("UPGRADING.md v{version} is missing `{heading}`"));
            }
        }
    }
    problems
}

/// Render the GitHub release body for `tag` (`vX.Y.Z`).
pub fn render(tag: &str, changelog: &str, upgrading: &str) -> Result<String, String> {
    let version = tag
        .strip_prefix('v')
        .ok_or_else(|| format!("release tag {tag:?} must start with 'v'"))?;
    let problems = contract_violations(changelog, upgrading);
    if !problems.is_empty() {
        return Err(problems.join("\n"));
    }
    let entry = changelog_entries(changelog)
        .remove(version)
        .ok_or_else(|| format!("CHANGELOG.md has no `## [{version}] - <date>` entry for {tag}"))?;
    if entry.is_empty() {
        return Err(format!("CHANGELOG.md entry for {version} is empty"));
    }
    let Some(section) = upgrading_sections(upgrading).remove(version) else {
        return Ok(format!("{entry}\n"));
    };
    // The marker line links `UPGRADING.md#vXYZ`, which is relative to the
    // repository and dangles on a release page; the section follows instead.
    let rest = entry
        .split_once('\n')
        .map_or("", |(_, rest)| rest)
        .trim_start();
    Ok(format!(
        "{BREAKING_MARKER} read Upgrading below before installing.\n\n{rest}\n\n## Upgrading\n\n{section}\n"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n## [0.2.0] - 2026-01-02\n\n\
        **Breaking:** see [UPGRADING.md](UPGRADING.md#v020).\n\n### Changed\n\n- removed x\n\n\
        ## [0.1.1] - 2026-01-01\n\n### Fixed\n\n- y\n";
    const UPGRADING: &str = "# Upgrading\n\n## Installing\n\nshared\n\n## v0.2.0\n\nintro\n\n\
        ### Compatibility\n\nschema 2\n\n```bash\n## not a heading\n```\n\n\
        ### Migrate and verify\n\nrun it\n\n### Rollback\n\nnone\n";

    #[test]
    fn a_breaking_release_body_carries_its_upgrade_section() {
        let body = render("v0.2.0", CHANGELOG, UPGRADING).unwrap();
        assert!(body.starts_with("**Breaking:** read Upgrading below"));
        assert!(
            !body.contains("UPGRADING.md#"),
            "repository-relative link on a release page"
        );
        assert!(body.contains("- removed x"));
        assert!(body.contains("## Upgrading\n\nintro"));
        assert!(
            body.contains("## not a heading"),
            "fenced lines stay in the section"
        );
        assert!(!body.contains("shared"));
    }

    #[test]
    fn a_non_breaking_release_body_is_only_its_changelog_entry() {
        let body = render("v0.1.1", CHANGELOG, UPGRADING).unwrap();
        assert_eq!(body, "### Fixed\n\n- y\n");
    }

    #[test]
    fn a_release_without_a_changelog_entry_is_refused() {
        let error = render("v0.3.0", CHANGELOG, UPGRADING).unwrap_err();
        assert!(error.contains("no `## [0.3.0] - <date>` entry"), "{error}");
        assert!(render("0.2.0", CHANGELOG, UPGRADING).is_err());
    }

    #[test]
    fn the_breaking_marker_and_the_upgrade_section_must_agree() {
        let unmarked = CHANGELOG.replace(
            "**Breaking:** see [UPGRADING.md](UPGRADING.md#v020).\n\n",
            "",
        );
        let problems = contract_violations(&unmarked, UPGRADING);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("does not open with"));

        let orphan_marker = CHANGELOG.replace(
            "## [0.1.1] - 2026-01-01\n\n",
            "## [0.1.1] - 2026-01-01\n\n**Breaking:** see [UPGRADING.md](UPGRADING.md#v011).\n\n",
        );
        let problems = contract_violations(&orphan_marker, UPGRADING);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("has no `## v0.1.1` section"));

        let wrong_anchor = CHANGELOG.replace("#v020", "#v02");
        assert!(
            contract_violations(&wrong_anchor, UPGRADING)[0]
                .contains("must link UPGRADING.md#v020")
        );

        let no_rollback = UPGRADING.replace("### Rollback", "### Undo");
        assert!(contract_violations(CHANGELOG, &no_rollback)[0].contains("missing `### Rollback`"));

        let stray = format!(
            "{UPGRADING}\n## v9.9.9\n\n### Compatibility\n### Migrate and verify\n### Rollback\n"
        );
        assert!(
            contract_violations(CHANGELOG, &stray)[0].contains("no `## [9.9.9] - <date>` entry")
        );
    }
}
