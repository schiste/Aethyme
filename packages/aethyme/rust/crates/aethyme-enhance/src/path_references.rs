//! Repository paths named by `.aethyme/**` configuration, checked against the
//! tree (#289).
//!
//! A file move that leaves one of these references behind breaks nothing at
//! the time. A stale `prepare.toml` input fails every *new* session's
//! preparation while every running session keeps its prepared digest, so the
//! breakage surfaces later, for someone with no context for the move.
//! `aethyme deploy verify` names every unresolved reference at once instead.
//!
//! Gate triggers are not checked here: `broker certify` (gate doctor) already
//! reports a gate whose triggers match no tracked path.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

use crate::pyjson::{self, Value};

pub const PREPARATION_CONFIG_PATH: &str = ".aethyme/prepare.toml";
pub const ONBOARDING_OVERRIDE_PATH: &str = crate::onboarding::ONBOARDING_OVERRIDE_PATH;
pub const AGENTS_OVERRIDE_PATH: &str = ".aethyme/overrides/agents.json";

/// One configured path that does not resolve in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedPathReference {
    /// The configuration file, repository-relative.
    pub file: &'static str,
    /// Where in that file, e.g. `steps[0].inputs[1]`.
    pub key: String,
    /// The path as written.
    pub path: String,
    pub kind: UnresolvedKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnresolvedKind {
    /// A concrete path that names nothing in the tree. Fails verification.
    Missing,
    /// A preparation input that exists but is not a regular file, which
    /// preparation refuses. Fails verification.
    NotAFile,
    /// A glob that matches no tracked path. A glob can legitimately match
    /// nothing yet, so this only warns.
    GlobMatchesNothing,
    /// A `generated_paths` entry that is neither tracked nor on disk. Build
    /// output is normally gitignored and may not have been produced yet, so
    /// this only warns.
    GeneratedNotPresent,
    /// The file could not be read or parsed, so its references are unknown.
    /// Fails verification.
    Unreadable,
}

impl UnresolvedKind {
    /// Whether this finding fails `deploy verify`.
    pub fn fails(self) -> bool {
        !matches!(self, Self::GlobMatchesNothing | Self::GeneratedNotPresent)
    }
}

impl std::fmt::Display for UnresolvedPathReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (file, key, path) = (self.file, &self.key, &self.path);
        match self.kind {
            UnresolvedKind::Missing => {
                write!(f, "{file}: {key} {path:?} does not exist in the tree")
            }
            UnresolvedKind::NotAFile => {
                write!(f, "{file}: {key} {path:?} is not a regular file")
            }
            UnresolvedKind::GeneratedNotPresent => write!(
                f,
                "{file}: {key} {path:?} is not in the tree or on disk; generated output may not exist yet"
            ),
            UnresolvedKind::GlobMatchesNothing => {
                write!(f, "{file}: {key} glob {path:?} matches no tracked path")
            }
            UnresolvedKind::Unreadable => write!(f, "{file}: cannot check path references: {path}"),
        }
    }
}

/// Every unresolved repository path named by `.aethyme/prepare.toml`,
/// `.aethyme/overrides/onboarding.json` and `.aethyme/overrides/agents.json`.
///
/// Paths are judged against tracked files, because a fresh session worktree
/// holds exactly those; outside a Git repository, against the filesystem.
pub fn unresolved_path_references(repo: &Path) -> Vec<UnresolvedPathReference> {
    let tree = Tree::load(repo);
    let mut found = Vec::new();
    preparation_references(repo, &tree, &mut found);
    onboarding_references(repo, &tree, &mut found);
    agents_references(repo, &tree, &mut found);
    found
}

/// What a fresh worktree would contain.
struct Tree<'a> {
    repo: &'a Path,
    /// Tracked files, or `None` when the repository is not a Git checkout.
    tracked: Option<BTreeSet<String>>,
}

impl<'a> Tree<'a> {
    fn load(repo: &'a Path) -> Self {
        let tracked = Command::new("git")
            .args(["ls-files", "-z", "--cached"])
            .current_dir(repo)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                output
                    .stdout
                    .split(|byte| *byte == 0)
                    .filter(|entry| !entry.is_empty())
                    .map(|entry| String::from_utf8_lossy(entry).into_owned())
                    .collect()
            });
        Self { repo, tracked }
    }

    fn is_file(&self, path: &str) -> bool {
        match &self.tracked {
            Some(tracked) => tracked.contains(path),
            None => self.repo.join(path).is_file(),
        }
    }

    /// A tracked file, or a directory holding one.
    fn exists(&self, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        if path.is_empty() || path == "." {
            return true;
        }
        match &self.tracked {
            Some(tracked) => {
                let prefix = format!("{path}/");
                tracked.contains(path)
                    || tracked
                        .range(prefix.clone()..)
                        .next()
                        .is_some_and(|first| first.starts_with(&prefix))
            }
            None => self.repo.join(path).exists(),
        }
    }

    fn glob_matches_anything(&self, pattern: &str) -> bool {
        let pattern = pattern.trim_end_matches('/');
        match &self.tracked {
            Some(tracked) => tracked.iter().any(|file| {
                // A pattern naming a directory matches any file beneath it.
                let mut candidate = file.as_str();
                loop {
                    if glob_match(pattern, candidate) {
                        return true;
                    }
                    match candidate.rsplit_once('/') {
                        Some((parent, _)) => candidate = parent,
                        None => return false,
                    }
                }
            }),
            // Without an index there is no cheap listing; do not guess.
            None => true,
        }
    }
}

fn is_glob(path: &str) -> bool {
    path.contains(['*', '?', '['])
}

/// `*` and `?` match within one path segment; `**` matches any number of
/// segments. Enough for the patterns these files hold; `[...]` classes are
/// treated literally.
fn glob_match(pattern: &str, path: &str) -> bool {
    fn segments(pattern: &[&str], path: &[&str]) -> bool {
        match pattern.split_first() {
            None => path.is_empty(),
            Some((&"**", rest)) => (0..=path.len()).any(|skip| segments(rest, &path[skip..])),
            Some((head, rest)) => path.split_first().is_some_and(|(segment, tail)| {
                segment_match(head, segment) && segments(rest, tail)
            }),
        }
    }
    fn segment_match(pattern: &str, text: &str) -> bool {
        let pattern: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = text.chars().collect();
        let (mut p, mut t, mut star, mut mark) = (0, 0, None, 0);
        while t < text.len() {
            if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
                p += 1;
                t += 1;
            } else if p < pattern.len() && pattern[p] == '*' {
                star = Some(p);
                mark = t;
                p += 1;
            } else if let Some(position) = star {
                p = position + 1;
                mark += 1;
                t = mark;
            } else {
                return false;
            }
        }
        pattern[p..].iter().all(|c| *c == '*')
    }
    let pattern: Vec<&str> = pattern.split('/').collect();
    let path: Vec<&str> = path.split('/').collect();
    segments(&pattern, &path)
}

/// Judge one configured path that should name something in the tree.
fn check(
    tree: &Tree<'_>,
    file: &'static str,
    key: String,
    path: &str,
    found: &mut Vec<UnresolvedPathReference>,
) {
    let path = path.trim();
    if path.is_empty() || Path::new(path).is_absolute() {
        return;
    }
    let path = path.trim_start_matches("./");
    let kind = if is_glob(path) {
        (!tree.glob_matches_anything(path)).then_some(UnresolvedKind::GlobMatchesNothing)
    } else {
        (!tree.exists(path)).then_some(UnresolvedKind::Missing)
    };
    if let Some(kind) = kind {
        found.push(UnresolvedPathReference {
            file,
            key,
            path: path.to_string(),
            kind,
        });
    }
}

fn unreadable(file: &'static str, reason: String) -> UnresolvedPathReference {
    UnresolvedPathReference {
        file,
        key: String::new(),
        path: reason,
        kind: UnresolvedKind::Unreadable,
    }
}

/// `steps[].inputs`: preparation hashes each one before any step runs, so
/// each must be a tracked regular file. Outputs are produced by the steps and
/// are not checked.
fn preparation_references(repo: &Path, tree: &Tree<'_>, found: &mut Vec<UnresolvedPathReference>) {
    let file = PREPARATION_CONFIG_PATH;
    let Ok(text) = std::fs::read_to_string(repo.join(file)) else {
        return;
    };
    let config = match text.parse::<toml::Table>() {
        Ok(config) => config,
        Err(error) => {
            found.push(unreadable(
                file,
                format!("invalid TOML: {}", error.message()),
            ));
            return;
        }
    };
    let Some(steps) = config.get("steps").and_then(toml::Value::as_array) else {
        return;
    };
    for (step_index, step) in steps.iter().enumerate() {
        let Some(inputs) = step.get("inputs").and_then(toml::Value::as_array) else {
            continue;
        };
        for (input_index, input) in inputs.iter().enumerate() {
            let Some(input) = input.as_str() else {
                continue;
            };
            let key = format!("steps[{step_index}].inputs[{input_index}]");
            let input = input.trim_start_matches("./");
            if tree.is_file(input) {
                continue;
            }
            let kind = if tree.exists(input) {
                UnresolvedKind::NotAFile
            } else {
                UnresolvedKind::Missing
            };
            found.push(UnresolvedPathReference {
                file,
                key,
                path: input.to_string(),
                kind,
            });
        }
    }
}

/// The onboarding override fields that name repository paths.
fn onboarding_references(repo: &Path, tree: &Tree<'_>, found: &mut Vec<UnresolvedPathReference>) {
    let file = ONBOARDING_OVERRIDE_PATH;
    let Some(payload) = read_json(repo, file, found) else {
        return;
    };
    if let Some(repo_section) = payload.get("repo") {
        if let Some(Value::Str(root)) = repo_section.get("root") {
            check(tree, file, "$.repo.root".into(), root, found);
        }
        for (index, manifest) in strings(repo_section.get("manifests")) {
            check(
                tree,
                file,
                format!("$.repo.manifests[{index}]"),
                manifest,
                found,
            );
        }
    }
    let workspace = |prefix: String, workspace: &Value, found: &mut Vec<_>| {
        if let Some(Value::Str(root)) = workspace.get("root") {
            check(tree, file, format!("{prefix}.root"), root, found);
        }
        if let Some(Value::Str(path)) = workspace.get("manifest").and_then(|m| m.get("path")) {
            check(tree, file, format!("{prefix}.manifest.path"), path, found);
        }
        for (index, member) in strings(workspace.get("members")) {
            check(
                tree,
                file,
                format!("{prefix}.members[{index}]"),
                member,
                found,
            );
        }
    };
    if let Some(Value::Array(workspaces)) = payload.get("workspaces") {
        for (index, item) in workspaces.iter().enumerate() {
            workspace(format!("$.workspaces[{index}]"), item, found);
        }
    }
    if let Some(primary) = payload.get("primary_workspace") {
        workspace("$.primary_workspace".into(), primary, found);
    }
    for key in [
        "areas",
        "entrypoints",
        "caution_zones",
        "generated_paths",
        "dangerous_paths",
    ] {
        let Some(Value::Array(items)) = payload.get(key) else {
            continue;
        };
        let item_is_generated = key == "generated_paths";
        for (index, item) in items.iter().enumerate() {
            let (key, path) = match item {
                Value::Str(path) => (format!("$.{key}[{index}]"), path),
                _ => match item.get("path") {
                    Some(Value::Str(path)) => (format!("$.{key}[{index}].path"), path),
                    _ => continue,
                },
            };
            let start = found.len();
            check(tree, file, key, path, found);
            // Generated output is normally gitignored, so a generated path
            // absent from the tracked tree is fine when it is on disk, and
            // only worth a warning when it is not there yet either.
            if item_is_generated && found.len() > start {
                let finding = found.pop().expect("check pushed one");
                if finding.kind == UnresolvedKind::Missing && !repo.join(&finding.path).exists() {
                    found.push(UnresolvedPathReference {
                        kind: UnresolvedKind::GeneratedNotPresent,
                        ..finding
                    });
                } else if finding.kind != UnresolvedKind::Missing {
                    found.push(finding);
                }
            }
        }
    }
}

/// Relative link targets in `maintainer_markdown`, which is rendered into the
/// root `CLAUDE.md` and so resolves from the repository root.
fn agents_references(repo: &Path, tree: &Tree<'_>, found: &mut Vec<UnresolvedPathReference>) {
    let file = AGENTS_OVERRIDE_PATH;
    let Some(payload) = read_json(repo, file, found) else {
        return;
    };
    let Some(Value::Str(markdown)) = payload.get("maintainer_markdown") else {
        return;
    };
    for (index, target) in markdown_link_targets(markdown).into_iter().enumerate() {
        check(
            tree,
            file,
            format!("$.maintainer_markdown link {}", index + 1),
            &target,
            found,
        );
    }
}

/// `](target)` destinations that point into the repository: no scheme, no
/// in-page anchor, fragment and title stripped.
fn markdown_link_targets(markdown: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = markdown;
    while let Some(start) = rest.find("](") {
        rest = &rest[start + 2..];
        let Some(end) = rest.find(')') else {
            break;
        };
        let target = rest[..end].split_whitespace().next().unwrap_or("");
        rest = &rest[end + 1..];
        let target = target.trim_matches(['<', '>']);
        if target.is_empty()
            || target.starts_with('#')
            || target.contains("://")
            || target.starts_with("mailto:")
        {
            continue;
        }
        let target = target.split('#').next().unwrap_or(target);
        if !target.is_empty() {
            targets.push(target.to_string());
        }
    }
    targets
}

fn read_json(
    repo: &Path,
    file: &'static str,
    found: &mut Vec<UnresolvedPathReference>,
) -> Option<Value> {
    let text = std::fs::read_to_string(repo.join(file)).ok()?;
    match pyjson::loads(&text) {
        Ok(payload) => Some(payload),
        Err(_) => {
            found.push(unreadable(file, "invalid JSON".into()));
            None
        }
    }
}

fn strings(value: Option<&Value>) -> Vec<(usize, &str)> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| match item {
                Value::Str(text) => Some((index, text.as_str())),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway Git repository holding `files`, removed on drop.
    struct Fixture(std::path::PathBuf);

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    fn fixture(files: &[(&str, &str)]) -> Fixture {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "aethyme-path-references-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (path, content) in files {
            let path = root.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-qm", "init"]);
        Fixture(root)
    }

    const PREPARE: &str = "schema_version = 1\n\n[[steps]]\nname = \"deps\"\ncommand = [\"true\"]\ninputs = [\"web/package.json\", \"web/pnpm-lock.yaml\"]\noutputs = [\"web/node_modules/\"]\n";

    const ONBOARDING: &str = r#"{
  "repo": {"manifests": ["web/package.json"]},
  "entrypoints": [{"path": "web/src/main.ts"}],
  "dangerous_paths": [{"path": "infra/"}],
  "caution_zones": [{"path": "**/migrations"}]
}"#;

    const AGENTS: &str = r#"{"maintainer_markdown": "Read [the guide](docs/guide.md#start) and [site](https://example.com)."}"#;

    fn complete() -> Vec<(&'static str, &'static str)> {
        vec![
            (".aethyme/prepare.toml", PREPARE),
            (".aethyme/overrides/onboarding.json", ONBOARDING),
            (".aethyme/overrides/agents.json", AGENTS),
            ("web/package.json", "{}"),
            ("web/pnpm-lock.yaml", ""),
            ("web/src/main.ts", ""),
            ("infra/main.tf", ""),
            ("db/migrations/001.sql", ""),
            ("docs/guide.md", ""),
        ]
    }

    #[test]
    fn a_tree_that_holds_every_reference_passes() {
        let repo = fixture(&complete());
        assert_eq!(unresolved_path_references(&repo.0), []);
    }

    /// The #289 incident: a workspace package was deleted and `prepare.toml`
    /// still listed its manifest. Every reference it left behind is named at
    /// once, with the file and key to fix.
    #[test]
    fn a_moved_package_names_every_reference_it_left_behind() {
        let files: Vec<_> = complete()
            .into_iter()
            .filter(|(path, _)| !path.starts_with("web/") && *path != "docs/guide.md")
            .collect();
        let repo = fixture(&files);
        let found = unresolved_path_references(&repo.0);
        let named: Vec<(&str, &str, &str, UnresolvedKind)> = found
            .iter()
            .map(|r| (r.file, r.key.as_str(), r.path.as_str(), r.kind))
            .collect();
        assert_eq!(
            named,
            [
                (
                    PREPARATION_CONFIG_PATH,
                    "steps[0].inputs[0]",
                    "web/package.json",
                    UnresolvedKind::Missing
                ),
                (
                    PREPARATION_CONFIG_PATH,
                    "steps[0].inputs[1]",
                    "web/pnpm-lock.yaml",
                    UnresolvedKind::Missing
                ),
                (
                    ONBOARDING_OVERRIDE_PATH,
                    "$.repo.manifests[0]",
                    "web/package.json",
                    UnresolvedKind::Missing
                ),
                (
                    ONBOARDING_OVERRIDE_PATH,
                    "$.entrypoints[0].path",
                    "web/src/main.ts",
                    UnresolvedKind::Missing
                ),
                (
                    AGENTS_OVERRIDE_PATH,
                    "$.maintainer_markdown link 1",
                    "docs/guide.md",
                    UnresolvedKind::Missing
                ),
            ]
        );
        assert!(found.iter().all(|r| r.kind.fails()));
        assert_eq!(
            found[0].to_string(),
            ".aethyme/prepare.toml: steps[0].inputs[0] \"web/package.json\" does not exist in the tree"
        );
    }

    /// A glob may match nothing yet; that warns without failing.
    #[test]
    fn a_glob_that_matches_nothing_warns_without_failing() {
        let files: Vec<_> = complete()
            .into_iter()
            .filter(|(path, _)| !path.starts_with("db/"))
            .collect();
        let repo = fixture(&files);
        let found = unresolved_path_references(&repo.0);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, UnresolvedKind::GlobMatchesNothing);
        assert_eq!(found[0].key, "$.caution_zones[0].path");
        assert!(!found[0].kind.fails());
    }

    /// Preparation hashes inputs as files and refuses a directory, and judges
    /// tracked content, which is what a fresh worktree holds.
    #[test]
    fn a_preparation_input_must_be_a_tracked_file() {
        let mut files = complete();
        files.retain(|(path, _)| *path != "web/pnpm-lock.yaml");
        files.push(("web/pnpm-lock.yaml/inner", ""));
        let repo = fixture(&files);
        std::fs::write(repo.0.join("untracked.txt"), "").unwrap();
        let found = unresolved_path_references(&repo.0);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, UnresolvedKind::NotAFile);
        assert_eq!(found[0].path, "web/pnpm-lock.yaml");
    }

    /// Build output named in `generated_paths` is normally gitignored: an
    /// untracked one on disk passes, a missing one only warns, while a
    /// missing entrypoint still fails.
    #[test]
    fn generated_paths_need_not_be_tracked() {
        let mut files = complete();
        files.retain(|(path, _)| {
            *path != ".aethyme/overrides/onboarding.json" && *path != "web/src/main.ts"
        });
        files.push((
            ".aethyme/overrides/onboarding.json",
            r#"{"entrypoints": [{"path": "web/src/main.ts"}],
                "generated_paths": [{"path": "dist/"}, {"path": "target"}]}"#,
        ));
        let repo = fixture(&files);
        std::fs::create_dir_all(repo.0.join("target/debug")).unwrap();
        let found = unresolved_path_references(&repo.0);
        let named: Vec<(&str, &str, UnresolvedKind)> = found
            .iter()
            .map(|r| (r.key.as_str(), r.path.as_str(), r.kind))
            .collect();
        assert_eq!(
            named,
            [
                (
                    "$.entrypoints[0].path",
                    "web/src/main.ts",
                    UnresolvedKind::Missing
                ),
                (
                    "$.generated_paths[0].path",
                    "dist/",
                    UnresolvedKind::GeneratedNotPresent
                ),
            ]
        );
        assert!(found[0].kind.fails());
        assert!(!found[1].kind.fails());
        assert!(
            found[1].to_string().contains("may not exist yet"),
            "{}",
            found[1]
        );
    }

    #[test]
    fn an_unparseable_file_fails_rather_than_passing_silently() {
        let mut files = complete();
        files.retain(|(path, _)| *path != ".aethyme/prepare.toml");
        files.push((".aethyme/prepare.toml", "steps = ["));
        let repo = fixture(&files);
        let found = unresolved_path_references(&repo.0);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, UnresolvedKind::Unreadable);
        assert!(found[0].kind.fails());
    }

    #[test]
    fn globs_match_by_segment() {
        assert!(glob_match("packages/**", "packages/a/b.rs"));
        assert!(glob_match("**/migrations", "db/x/migrations"));
        assert!(glob_match("scripts/pilot-*.jq", "scripts/pilot-report.jq"));
        assert!(!glob_match("scripts/*.jq", "scripts/a/b.jq"));
        assert!(glob_match("src/?.rs", "src/a.rs"));
        assert!(!glob_match("src/?.rs", "src/ab.rs"));
    }

    #[test]
    fn link_targets_skip_urls_and_anchors() {
        let markdown = "See [a](docs/a.md#part), [b](https://x.y), [c](#top), \
                        [d](<docs/d.md> \"title\") and [e](mailto:x@y).";
        assert_eq!(markdown_link_targets(markdown), ["docs/a.md", "docs/d.md"]);
    }
}
