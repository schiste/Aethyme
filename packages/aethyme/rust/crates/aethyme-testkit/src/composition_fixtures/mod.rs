//! Provisional composition fixtures FX01–FX07 and their behavior oracle.
//!
//! These stand in for the E1 (#650) evidence until it lands, so that L4
//! (#663–#665) can be built against something concrete. They are
//! **provisional**: E1's frozen fixtures and requirements replace them.
//! See `packages/aethyme/docs/architecture/local-v3-l4-provisional-fixtures.md`.
//!
//! The data is split in two:
//!
//! * `fixtures/composition-provisional/inputs/{cases,held-out}/<case>/`:
//!   what a composer may know. Neutral tree ids (`t0`…), contribution ids
//!   (`c1`…) and scenario ids (`s1`…), whole `trees/` and `contributions/`,
//!   and `inputs.json`.
//! * `expectations/` and `oracle-selftest/`: the answer key, read only by
//!   [`judge`] and [`judge_scenario`] inside this module.
//!
//! **The composer-facing API is everything public here.** A composer test
//! loads a [`CaseInput`], calls [`CaseInput::materialize`] for one scenario,
//! composes from the Git repository and the scenario's [`ScenarioInput`],
//! and hands the result to [`judge_scenario`]. Nothing public exposes a
//! required outcome, a behavior, a measured column or a fixture path. The
//! oracle shares no code with any composer, and a conflict-free merge is
//! never evidence of success: only the behaviors and preservation checks
//! are.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

mod html;
mod oracle;
#[cfg(test)]
mod tests;

pub use oracle::{Observed, Outcome, Verdict, judge, judge_scenario};

const INPUTS_SCHEMA: &str = "aethyme.composition-inputs/provisional-v0";

fn fixtures_root() -> PathBuf {
    crate::rust_workspace_root().join("crates/aethyme-testkit/fixtures/composition-provisional")
}

/// FX01–FX07, in case order.
pub fn cases() -> Vec<CaseInput> {
    load_kind("cases")
}

/// Held-out variants. Run them only to report how a frozen composer
/// generalizes; never use them to tune one.
pub fn held_out_cases() -> Vec<CaseInput> {
    load_kind("held-out")
}

fn load_kind(kind: &str) -> Vec<CaseInput> {
    let dir = fixtures_root().join("inputs").join(kind);
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.join("inputs.json").is_file())
        .collect();
    dirs.sort();
    dirs.iter().map(|dir| CaseInput::load(dir, kind)).collect()
}

/// One case, as a composer may see it.
#[derive(Debug, Clone)]
pub struct CaseInput {
    /// `FX01`…`FX07`, or `HX..` for a held-out variant.
    pub case: String,
    pub contributions: Vec<ContributionInput>,
    pub scenarios: Vec<ScenarioInput>,
    trees: BTreeMap<String, TreeSpec>,
    dir: PathBuf,
    kind: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct TreeSpec {
    /// Part of accepted history. False for an intermediate integration no
    /// record explains.
    accepted: bool,
    #[serde(default)]
    parent: Option<String>,
}

/// A contribution's metadata. Its source is in the materialized repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionInput {
    pub id: String,
    pub base: BaseRef,
    /// Exact prerequisite contributions (a pin on that revision).
    pub requires: Vec<String>,
    /// Contributions that are accepted together or not at all.
    pub atomic_group: Option<String>,
    /// Another revision of the same change: it fills the same place in its
    /// atomic group, and selecting both revisions competes.
    pub revision_of: Option<String>,
    pub synthesized: bool,
    pub derived_from: Vec<String>,
}

/// Where a contribution's base comes from. Tree names are neutral ids;
/// only [`Materialized`] says which commits are accepted history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseRef {
    Tree(String),
    Contribution(String),
}

/// Everything a composer may see about a scenario.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScenarioInput {
    pub id: String,
    /// The accepted tree the candidate starts from.
    pub baseline: String,
    pub request: Request,
    /// Application orders to run, each at least twice (see
    /// [`judge_scenario`]). Every order of a `commutative` scenario must
    /// give the same outcome and a byte-identical tree. Empty for a
    /// subtraction, which the composer orders itself.
    pub orders: Vec<Vec<String>>,
    pub commutative: bool,
    /// Contributions whose original source is not retained:
    /// [`CaseInput::materialize`] leaves them out.
    #[serde(default)]
    pub unretained: Vec<String>,
}

/// Either `compose` (optionally with repeated `deliveries`) or `subtract`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    /// Compose these contributions, each delivered once…
    #[serde(default)]
    pub compose: Vec<String>,
    /// …unless `deliveries` lists the delivery sequence, repeats included.
    #[serde(default)]
    pub deliveries: Vec<String>,
    #[serde(default)]
    pub subtract: Option<Subtract>,
}

/// Remove `remove` from the synthesized contribution `from`, keeping `keep`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subtract {
    pub from: String,
    pub remove: Vec<String>,
    pub keep: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawInputs {
    schema: String,
    case: String,
    trees: BTreeMap<String, TreeSpec>,
    contributions: Vec<RawContribution>,
    scenarios: Vec<ScenarioInput>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawContribution {
    id: String,
    base: String,
    #[serde(default)]
    requires: Vec<String>,
    #[serde(default)]
    atomic_group: Option<String>,
    #[serde(default)]
    revision_of: Option<String>,
    #[serde(default)]
    synthesized: bool,
    #[serde(default)]
    derived_from: Vec<String>,
}

impl CaseInput {
    fn load(dir: &Path, kind: &str) -> Self {
        let path = dir.join("inputs.json");
        let raw: RawInputs = serde_json::from_str(&crate::repos::read(&path))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(raw.schema, INPUTS_SCHEMA, "{}", path.display());
        let contributions = raw
            .contributions
            .into_iter()
            .map(|raw| ContributionInput {
                base: if let Some(name) = raw.base.strip_prefix("tree:") {
                    BaseRef::Tree(name.to_owned())
                } else if let Some(id) = raw.base.strip_prefix("contribution:") {
                    BaseRef::Contribution(id.to_owned())
                } else {
                    panic!("{}: {}: base {:?}", path.display(), raw.id, raw.base)
                },
                id: raw.id,
                requires: raw.requires,
                atomic_group: raw.atomic_group,
                revision_of: raw.revision_of,
                synthesized: raw.synthesized,
                derived_from: raw.derived_from,
            })
            .collect();
        let case = Self {
            case: raw.case,
            contributions,
            scenarios: raw.scenarios,
            trees: raw.trees,
            dir: dir.to_path_buf(),
            kind: kind.to_owned(),
        };
        case.validate();
        case
    }

    pub fn is_held_out(&self) -> bool {
        self.kind == "held-out"
    }

    pub fn contribution(&self, id: &str) -> &ContributionInput {
        self.contributions
            .iter()
            .find(|contribution| contribution.id == id)
            .unwrap_or_else(|| panic!("{}: no contribution {id}", self.case))
    }

    pub fn scenario(&self, id: &str) -> &ScenarioInput {
        self.scenarios
            .iter()
            .find(|scenario| scenario.id == id)
            .unwrap_or_else(|| panic!("{}: no scenario {id}", self.case))
    }

    fn tree_dir(&self, name: &str) -> PathBuf {
        self.dir.join("trees").join(name)
    }

    fn result_dir(&self, id: &str) -> PathBuf {
        self.dir.join("contributions").join(id)
    }

    fn base_dir(&self, id: &str) -> PathBuf {
        match &self.contribution(id).base {
            BaseRef::Tree(name) => self.tree_dir(name),
            BaseRef::Contribution(base) => self.result_dir(base),
        }
    }

    fn validate(&self) {
        let known = |id: &str| self.contributions.iter().any(|c| c.id == id);
        for (name, tree) in &self.trees {
            assert!(
                self.tree_dir(name).is_dir(),
                "{}: tree {name} has no directory",
                self.case
            );
            if let Some(parent) = &tree.parent {
                assert!(
                    self.trees.contains_key(parent),
                    "{}: tree {name} parent {parent}",
                    self.case
                );
            }
        }
        for (index, contribution) in self.contributions.iter().enumerate() {
            assert!(
                !self.contributions[..index]
                    .iter()
                    .any(|c| c.id == contribution.id),
                "{}: duplicate contribution {}",
                self.case,
                contribution.id
            );
            assert!(
                self.result_dir(&contribution.id).is_dir(),
                "{}: {} has no result",
                self.case,
                contribution.id
            );
            match &contribution.base {
                BaseRef::Tree(name) => {
                    assert!(self.trees.contains_key(name), "{}: base {name}", self.case)
                }
                BaseRef::Contribution(id) => assert!(known(id), "{}: base {id}", self.case),
            }
            for id in contribution
                .requires
                .iter()
                .chain(&contribution.derived_from)
                .chain(&contribution.revision_of)
            {
                assert!(
                    known(id),
                    "{}: {} names unknown {id}",
                    self.case,
                    contribution.id
                );
            }
        }
        for scenario in &self.scenarios {
            let label = format!("{}/{}", self.case, scenario.id);
            assert!(
                self.trees
                    .get(&scenario.baseline)
                    .is_some_and(|tree| tree.accepted),
                "{label}: baseline"
            );
            let request = &scenario.request;
            assert_ne!(
                request.compose.is_empty(),
                request.subtract.is_none(),
                "{label}: compose xor subtract"
            );
            assert_eq!(
                scenario.orders.is_empty(),
                request.subtract.is_some(),
                "{label}: orders iff compose"
            );
            let mut named: Vec<&String> = request
                .compose
                .iter()
                .chain(&request.deliveries)
                .chain(&scenario.unretained)
                .chain(scenario.orders.iter().flatten())
                .collect();
            if let Some(subtract) = &request.subtract {
                named.extend(
                    std::iter::once(&subtract.from)
                        .chain(&subtract.remove)
                        .chain(&subtract.keep),
                );
            }
            for id in named {
                assert!(known(id), "{label}: names unknown {id}");
            }
        }
    }

    /// Write this case into a new Git repository at `root/<case>-<scenario>`
    /// for one scenario. Every tree the scenario's history and bases need,
    /// and every retained contribution, is one commit; each contribution's
    /// result is parented on its exact base. Contributions in the
    /// scenario's `unretained` list are left out. Ids, refs
    /// (`refs/heads/fixture/<id>`) and commit messages are the neutral ids,
    /// and commits are deterministic.
    pub fn materialize(&self, scenario: &str, root: &Path) -> Materialized {
        let scenario = self.scenario(scenario);
        let repo = root.join(format!("{}-{}", self.case.to_lowercase(), scenario.id));
        std::fs::create_dir_all(&repo).expect("create fixture repo dir");
        git(&repo, &["init", "-q"]);
        let mut writer = Writer {
            case: self,
            repo: &repo,
            trees: BTreeMap::new(),
            contributions: BTreeMap::new(),
        };
        let baseline = writer.tree(&scenario.baseline);
        for contribution in &self.contributions {
            if !scenario.unretained.contains(&contribution.id) {
                writer.contribution(&contribution.id, &scenario.unretained);
            }
        }
        let mut accepted = Vec::new();
        let mut next = Some(scenario.baseline.as_str());
        while let Some(name) = next {
            let tree = &self.trees[name];
            if tree.accepted {
                accepted.push(writer.tree(name));
            }
            next = tree.parent.as_deref();
        }
        let contributions = writer.contributions;
        Materialized {
            repo,
            baseline,
            accepted,
            contributions,
        }
    }
}

/// One scenario's case written into a fresh Git repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Materialized {
    pub repo: PathBuf,
    /// The scenario's baseline commit.
    pub baseline: String,
    /// Accepted history: the baseline and its accepted ancestors, newest
    /// first. A contribution whose base is neither one of these nor the
    /// result of a retained contribution has an unknown base.
    pub accepted: Vec<String>,
    /// Retained contribution id → its exact base and result commits.
    pub contributions: BTreeMap<String, ContributionCommits>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionCommits {
    pub base: String,
    pub result: String,
}

struct Writer<'a> {
    case: &'a CaseInput,
    repo: &'a Path,
    trees: BTreeMap<String, String>,
    contributions: BTreeMap<String, ContributionCommits>,
}

impl Writer<'_> {
    fn tree(&mut self, name: &str) -> String {
        if let Some(commit) = self.trees.get(name) {
            return commit.clone();
        }
        let parent = self.case.trees[name]
            .parent
            .clone()
            .map(|parent| self.tree(&parent));
        let commit = commit_dir(
            self.repo,
            &self.case.tree_dir(name),
            parent.as_deref(),
            name,
        );
        git(
            self.repo,
            &["update-ref", &format!("refs/heads/fixture/{name}"), &commit],
        );
        self.trees.insert(name.to_owned(), commit.clone());
        commit
    }

    fn contribution(&mut self, id: &str, unretained: &[String]) -> String {
        if let Some(commits) = self.contributions.get(id) {
            return commits.result.clone();
        }
        let base = match &self.case.contribution(id).base {
            BaseRef::Tree(name) => self.tree(name),
            BaseRef::Contribution(base) => {
                assert!(
                    !unretained.contains(base),
                    "{}: {id} is based on unretained {base}",
                    self.case.case
                );
                self.contribution(base, unretained)
            }
        };
        let result = commit_dir(self.repo, &self.case.result_dir(id), Some(&base), id);
        git(
            self.repo,
            &["update-ref", &format!("refs/heads/fixture/{id}"), &result],
        );
        self.contributions.insert(
            id.to_owned(),
            ContributionCommits {
                base,
                result: result.clone(),
            },
        );
        result
    }
}

fn commit_dir(repo: &Path, dir: &Path, parent: Option<&str>, message: &str) -> String {
    let git_dir = repo.join(".git");
    let index = git_dir.join("fixture-index");
    let _ = std::fs::remove_file(&index);
    let run = |args: &[&str]| -> String {
        let output = Command::new("git")
            .arg("--git-dir")
            .arg(&git_dir)
            .arg("--work-tree")
            .arg(dir)
            .args(["-c", "commit.gpgsign=false", "-c", "core.autocrlf=false"])
            .args(args)
            .current_dir(dir)
            .env("GIT_INDEX_FILE", &index)
            .envs(fixed_identity())
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout)
            .expect("utf-8")
            .trim()
            .to_owned()
    };
    run(&["add", "-A", "."]);
    let tree = run(&["write-tree"]);
    let mut args = vec!["commit-tree", tree.as_str(), "-m", message];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    let commit = run(&args);
    let _ = std::fs::remove_file(&index);
    commit
}

fn fixed_identity() -> [(&'static str, &'static str); 6] {
    [
        ("GIT_AUTHOR_NAME", "Composition Fixture"),
        ("GIT_AUTHOR_EMAIL", "fixture@example.invalid"),
        ("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z"),
        ("GIT_COMMITTER_NAME", "Composition Fixture"),
        ("GIT_COMMITTER_EMAIL", "fixture@example.invalid"),
        ("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z"),
    ]
}

fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("utf-8")
        .trim()
        .to_owned()
}

/// Every regular file under `dir`, as sorted `/`-separated relative paths.
pub fn tree_files(dir: &Path) -> Vec<String> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in
            std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let relative = path.strip_prefix(root).expect("under root");
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}
