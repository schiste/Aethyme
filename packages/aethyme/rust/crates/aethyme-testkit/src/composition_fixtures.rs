//! Provisional composition fixtures FX01–FX07 and their behavior oracle.
//!
//! These stand in for the E1 (#650) evidence until it lands, so that L4
//! (#663–#665) can be built against something concrete. They are
//! **provisional**: E1's frozen fixtures and requirements replace them.
//! See `packages/aethyme/docs/architecture/local-v3-l4-provisional-fixtures.md`.
//!
//! A case lives in `fixtures/composition-provisional/{cases,held-out}/<case>/`:
//!
//! * `trees/<name>/`: whole source trees (the baseline and any other base).
//! * `contributions/<id>/`: each contribution's whole result tree.
//! * `case.json`: the inputs (trees, contributions, scenarios with their
//!   request and application orders) plus two expectation columns: the
//!   outcomes the plan requires, and the outcome measured under the
//!   provisional "plain three-way text merge" profile.
//! * `requirements.json`: the independently specified behaviors a candidate
//!   must show. Only [`judge`] reads it.
//!
//! **Keep composers blind.** A composer under test gets the materialized
//! repository ([`materialize`]) and a scenario's [`ScenarioInput`]. It never
//! reads `required_outcomes`, `provisional_text`, `requirements.json`, the
//! `oracle-selftest/` trees, or anything under `held-out/` while it is being
//! tuned. The oracle shares no code with any composer, and a conflict-free
//! merge is never evidence of success: only the behaviors are.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

pub const FIXTURE_SCHEMA: &str = "aethyme.composition-fixture/provisional-v0";
pub const REQUIREMENTS_SCHEMA: &str = "aethyme.composition-requirements/provisional-v0";

/// `crates/aethyme-testkit/fixtures/composition-provisional`.
pub fn fixtures_root() -> PathBuf {
    crate::rust_workspace_root().join("crates/aethyme-testkit/fixtures/composition-provisional")
}

/// FX01–FX07, in case order.
pub fn cases() -> Vec<Case> {
    load_dir(&fixtures_root().join("cases"), false)
}

/// Held-out variants. Never use them to tune a composer.
pub fn held_out_cases() -> Vec<Case> {
    load_dir(&fixtures_root().join("held-out"), true)
}

fn load_dir(dir: &Path, held_out: bool) -> Vec<Case> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|error| panic!("read {}: {error}", dir.display()))
        .map(|entry| entry.expect("dir entry").path())
        .filter(|path| path.join("case.json").is_file())
        .collect();
    dirs.sort();
    dirs.iter().map(|dir| load_case(dir, held_out)).collect()
}

/// Load and validate one case directory.
pub fn load_case(dir: &Path, held_out: bool) -> Case {
    let text = crate::repos::read(dir.join("case.json"));
    let mut case: Case = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{}/case.json: {error}", dir.display()));
    assert_eq!(case.schema, FIXTURE_SCHEMA, "{}", dir.display());
    case.dir = dir.to_path_buf();
    case.held_out = held_out;
    case.validate();
    case
}

/// The outcome vocabulary. `Candidate` is the only success-shaped outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Candidate,
    Conflict,
    Unsupported,
    ResolutionRequired,
    UnknownBase,
    DependencyCycle,
    MissingInput,
    CompetingRevisions,
    BudgetExhausted,
    InseparableSelection,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::Conflict => "conflict",
            Self::Unsupported => "unsupported",
            Self::ResolutionRequired => "resolution_required",
            Self::UnknownBase => "unknown_base",
            Self::DependencyCycle => "dependency_cycle",
            Self::MissingInput => "missing_input",
            Self::CompetingRevisions => "competing_revisions",
            Self::BudgetExhausted => "budget_exhausted",
            Self::InseparableSelection => "inseparable_selection",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub schema: String,
    /// `FX01`…`FX07`, or `HX..` for a held-out variant.
    pub case: String,
    pub title: String,
    pub plan: String,
    pub trees: BTreeMap<String, TreeSpec>,
    pub contributions: Vec<Contribution>,
    pub scenarios: Vec<Scenario>,
    #[serde(skip)]
    pub dir: PathBuf,
    #[serde(skip)]
    pub held_out: bool,
}

/// A named whole tree under `trees/<name>/`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TreeSpec {
    /// False for an intermediate integration no record explains: a
    /// composer must refuse a contribution based on it (`unknown_base`).
    pub recorded: bool,
    /// The tree this one descends from in Git history, if any.
    #[serde(default)]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contribution {
    pub id: String,
    /// `tree:<name>` or `contribution:<id>` (that contribution's result).
    pub base: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub atomic_group: Option<String>,
    /// Another revision of the same change; selecting both competes.
    #[serde(default)]
    pub revision_of: Option<String>,
    #[serde(default)]
    pub synthesized: bool,
    #[serde(default)]
    pub derived_from: Vec<String>,
}

/// Where a contribution's base comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseRef {
    Tree(String),
    Contribution(String),
}

impl Contribution {
    pub fn base_ref(&self) -> BaseRef {
        if let Some(name) = self.base.strip_prefix("tree:") {
            BaseRef::Tree(name.to_owned())
        } else if let Some(id) = self.base.strip_prefix("contribution:") {
            BaseRef::Contribution(id.to_owned())
        } else {
            panic!(
                "contribution {}: base {:?} is neither tree: nor contribution:",
                self.id, self.base
            )
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    #[serde(flatten)]
    pub input: ScenarioInput,
    /// Expectation: the outcomes plan v3 accepts. Not for composers.
    pub required_outcomes: Vec<Outcome>,
    /// Expectation: what plain three-way text merge produced. Not for composers.
    pub provisional_text: ProvisionalText,
}

/// Everything a composer under test may see about a scenario.
#[derive(Debug, Clone, Deserialize)]
pub struct ScenarioInput {
    pub id: String,
    /// The accepted baseline tree the candidate starts from.
    pub baseline: String,
    pub request: Request,
    /// Application orders to run. Every order of a `commutative` scenario
    /// must give the same outcome and equivalent behavior; otherwise the
    /// single order is fixed. Empty for a subtraction request.
    pub orders: Vec<Vec<String>>,
    pub commutative: bool,
    /// Contributions whose original source is NOT retained: a composer
    /// test must not capture them.
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
    /// …unless `deliveries` lists the delivery sequence, repeats included,
    /// to model duplicate delivery.
    #[serde(default)]
    pub deliveries: Vec<String>,
    /// Remove contributions from a synthesized one.
    #[serde(default)]
    pub subtract: Option<Subtract>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subtract {
    pub from: String,
    pub remove: Vec<String>,
    /// Contributions built on `from` that the request wants kept.
    pub keep: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionalText {
    /// `None` for a planning refusal, which no merge engine decides.
    pub outcome: Option<String>,
    /// For a candidate: `passes` or `fails` the requirements.
    pub behavior: Option<String>,
    /// Orders to measure when the request is not a plain composition.
    #[serde(default)]
    pub measured_orders: Vec<Vec<String>>,
    pub note: String,
}

impl Case {
    pub fn contribution(&self, id: &str) -> &Contribution {
        self.contributions
            .iter()
            .find(|contribution| contribution.id == id)
            .unwrap_or_else(|| panic!("{}: no contribution {id}", self.case))
    }

    pub fn scenario(&self, id: &str) -> &Scenario {
        self.scenarios
            .iter()
            .find(|scenario| scenario.input.id == id)
            .unwrap_or_else(|| panic!("{}: no scenario {id}", self.case))
    }

    pub fn tree_dir(&self, name: &str) -> PathBuf {
        self.dir.join("trees").join(name)
    }

    pub fn result_dir(&self, id: &str) -> PathBuf {
        self.dir.join("contributions").join(id)
    }

    /// The whole tree a contribution was made against.
    pub fn base_dir(&self, id: &str) -> PathBuf {
        match self.contribution(id).base_ref() {
            BaseRef::Tree(name) => self.tree_dir(&name),
            BaseRef::Contribution(base) => self.result_dir(&base),
        }
    }

    fn validate(&self) {
        let ids: BTreeSet<&str> = self.contributions.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids.len(),
            self.contributions.len(),
            "{}: duplicate contribution id",
            self.case
        );
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
        for contribution in &self.contributions {
            assert!(
                self.result_dir(&contribution.id).is_dir(),
                "{}: {} has no result",
                self.case,
                contribution.id
            );
            match contribution.base_ref() {
                BaseRef::Tree(name) => assert!(
                    self.trees.contains_key(&name),
                    "{}: base tree {name}",
                    self.case
                ),
                BaseRef::Contribution(id) => {
                    assert!(ids.contains(id.as_str()), "{}: base {id}", self.case)
                }
            }
            for id in contribution
                .requires
                .iter()
                .chain(&contribution.derived_from)
                .chain(&contribution.revision_of)
            {
                assert!(
                    ids.contains(id.as_str()),
                    "{}: {} names unknown {id}",
                    self.case,
                    contribution.id
                );
            }
        }
        let requirements = self.requirements();
        for scenario in &self.scenarios {
            let input = &scenario.input;
            assert!(
                self.trees.contains_key(&input.baseline),
                "{}/{}: baseline",
                self.case,
                input.id
            );
            for order in input
                .orders
                .iter()
                .chain(&scenario.provisional_text.measured_orders)
            {
                for id in order {
                    assert!(
                        ids.contains(id.as_str()),
                        "{}/{}: order names {id}",
                        self.case,
                        input.id
                    );
                }
            }
            assert!(
                !scenario.required_outcomes.is_empty(),
                "{}/{}: no required outcome",
                self.case,
                input.id
            );
            let request = &input.request;
            assert_ne!(
                request.compose.is_empty(),
                request.subtract.is_none(),
                "{}/{}: compose xor subtract",
                self.case,
                input.id
            );
            let mut named: Vec<&String> = request
                .compose
                .iter()
                .chain(&request.deliveries)
                .chain(&input.unretained)
                .collect();
            if let Some(subtract) = &request.subtract {
                named.extend(
                    std::iter::once(&subtract.from)
                        .chain(&subtract.remove)
                        .chain(&subtract.keep),
                );
            }
            for id in named {
                assert!(
                    ids.contains(id.as_str()),
                    "{}/{}: request names {id}",
                    self.case,
                    input.id
                );
            }
            let behaviors = requirements
                .get(&input.id)
                .unwrap_or_else(|| panic!("{}/{}: no requirements entry", self.case, input.id));
            assert_eq!(
                scenario.required_outcomes.contains(&Outcome::Candidate),
                !behaviors.is_empty(),
                "{}/{}: a scenario accepts a candidate exactly when it states behaviors",
                self.case,
                input.id
            );
        }
        assert_eq!(
            requirements.len(),
            self.scenarios.len(),
            "{}: stray requirements",
            self.case
        );
    }

    fn requirements(&self) -> BTreeMap<String, Vec<Behavior>> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Requirements {
            schema: String,
            case: String,
            scenarios: BTreeMap<String, Vec<Behavior>>,
        }
        let path = self.dir.join("requirements.json");
        let parsed: Requirements = serde_json::from_str(&crate::repos::read(&path))
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
        assert_eq!(parsed.schema, REQUIREMENTS_SCHEMA, "{}", path.display());
        assert_eq!(parsed.case, self.case, "{}", path.display());
        parsed.scenarios
    }
}

// ------------------------------------------------------------- materialize

/// A case written into a fresh Git repository.
#[derive(Debug, Clone)]
pub struct Materialized {
    pub repo: PathBuf,
    /// Tree name → commit (branch `fixture/tree/<name>`).
    pub trees: BTreeMap<String, String>,
    /// Contribution id → its exact base and result commits (branch
    /// `fixture/contribution/<id>` points at the result).
    pub contributions: BTreeMap<String, ContributionCommits>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionCommits {
    pub base: String,
    pub result: String,
}

/// Write `case` into a new repository at `root/<case>`: one commit per
/// tree (parented on its `parent`), and one commit per contribution result
/// parented on its exact base. Commits are deterministic.
pub fn materialize(case: &Case, root: &Path) -> Materialized {
    let repo = root.join(case.case.to_lowercase());
    std::fs::create_dir_all(&repo).expect("create fixture repo dir");
    git(&repo, &["init", "-q"]);
    let mut out = Materialized {
        repo,
        trees: BTreeMap::new(),
        contributions: BTreeMap::new(),
    };
    for name in case.trees.keys() {
        tree_commit(case, name, &mut out);
    }
    for contribution in &case.contributions {
        contribution_commit(case, &contribution.id, &mut out);
    }
    out
}

fn tree_commit(case: &Case, name: &str, out: &mut Materialized) -> String {
    if let Some(commit) = out.trees.get(name) {
        return commit.clone();
    }
    let parent = case.trees[name]
        .parent
        .as_deref()
        .map(|parent| tree_commit(case, parent, out));
    let commit = commit_dir(
        &out.repo,
        &case.tree_dir(name),
        parent.as_deref(),
        &format!("tree {name}"),
    );
    git(
        &out.repo,
        &[
            "update-ref",
            &format!("refs/heads/fixture/tree/{name}"),
            &commit,
        ],
    );
    out.trees.insert(name.to_owned(), commit.clone());
    commit
}

fn contribution_commit(case: &Case, id: &str, out: &mut Materialized) -> String {
    if let Some(commits) = out.contributions.get(id) {
        return commits.result.clone();
    }
    let base = match case.contribution(id).base_ref() {
        BaseRef::Tree(name) => tree_commit(case, &name, out),
        BaseRef::Contribution(base) => contribution_commit(case, &base, out),
    };
    let result = commit_dir(
        &out.repo,
        &case.result_dir(id),
        Some(&base),
        &format!("contribution {id}"),
    );
    git(
        &out.repo,
        &[
            "update-ref",
            &format!("refs/heads/fixture/contribution/{id}"),
            &result,
        ],
    );
    out.contributions.insert(
        id.to_owned(),
        ContributionCommits {
            base,
            result: result.clone(),
        },
    );
    result
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

// ------------------------------------------------------------------ oracle

/// What a composer produced for one scenario.
#[derive(Debug, Clone, Copy)]
pub enum Observed<'a> {
    /// A complete candidate source tree.
    Candidate(&'a Path),
    /// Any non-candidate outcome (`Outcome::Candidate` is rejected here).
    Refused(Outcome),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    pub accepted: bool,
    pub failures: Vec<String>,
}

/// Judge `observed` against the scenario's required outcomes and, for a
/// candidate, every independently specified behavior.
pub fn judge(case: &Case, scenario: &str, observed: Observed<'_>) -> Verdict {
    let required = &case.scenario(scenario).required_outcomes;
    let outcome = match observed {
        Observed::Candidate(_) => Outcome::Candidate,
        Observed::Refused(outcome) => outcome,
    };
    let mut failures = Vec::new();
    if outcome == Outcome::Candidate && matches!(observed, Observed::Refused(_)) {
        failures.push("a refusal cannot carry the candidate outcome".to_owned());
    } else if !required.contains(&outcome) {
        let names: Vec<&str> = required.iter().map(|outcome| outcome.as_str()).collect();
        failures.push(format!(
            "outcome {} is not one of {names:?}",
            outcome.as_str()
        ));
    } else if let Observed::Candidate(dir) = observed {
        failures.extend(candidate_failures(dir));
        let requirements = case.requirements();
        for behavior in &requirements[scenario] {
            if let Err(failure) = behavior.check(dir) {
                failures.push(failure);
            }
        }
    }
    Verdict {
        accepted: failures.is_empty(),
        failures,
    }
}

/// Checks every candidate must pass whatever the scenario: no conflict
/// markers, and every HTML file parses with unique ids.
fn candidate_failures(dir: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    for path in tree_files(dir) {
        let text = match std::fs::read_to_string(dir.join(&path)) {
            Ok(text) => text,
            Err(error) => {
                failures.push(format!("{path}: {error}"));
                continue;
            }
        };
        if text.lines().any(|line| {
            ["<<<<<<<", "=======", ">>>>>>>", "|||||||"]
                .iter()
                .any(|m| line.starts_with(m))
        }) {
            failures.push(format!("{path}: contains a conflict marker"));
        }
        if path.ends_with(".html") {
            match Html::parse(&text) {
                Ok(doc) => {
                    let mut seen = BTreeSet::new();
                    for node in &doc.nodes {
                        if let Some(id) = node.attr("id")
                            && !seen.insert(id.to_owned())
                        {
                            failures.push(format!("{path}: duplicate id {id:?}"));
                        }
                    }
                }
                Err(error) => failures.push(format!("{path}: does not parse: {error}")),
            }
        }
    }
    failures
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

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "check", rename_all = "snake_case", deny_unknown_fields)]
enum Behavior {
    /// Exactly one element matches, and its whitespace-normalized text equals.
    Text {
        file: String,
        selector: String,
        equals: String,
    },
    /// Exactly one element matches, and its attribute equals.
    Attr {
        file: String,
        selector: String,
        name: String,
        equals: String,
    },
    /// At least one element matches, and none has the attribute.
    AttrAbsent {
        file: String,
        selector: String,
        name: String,
    },
    Count {
        file: String,
        selector: String,
        equals: usize,
    },
    /// Exactly one element matches, and it has an ancestor matching `ancestor`.
    Inside {
        file: String,
        selector: String,
        ancestor: String,
    },
    NotInside {
        file: String,
        selector: String,
        ancestor: String,
    },
    Contains {
        file: String,
        text: String,
    },
    /// Every call of `function` in `consumers` passes as many arguments as
    /// its definition in `file` declares, and there is at least one call.
    CallArity {
        file: String,
        function: String,
        consumers: Vec<String>,
    },
    /// Every `getElementById("x")` in `file` names an id present in `html`.
    IdRefsResolve {
        file: String,
        html: String,
    },
}

impl Behavior {
    fn check(&self, dir: &Path) -> Result<(), String> {
        let read = |file: &str| {
            std::fs::read_to_string(dir.join(file)).map_err(|error| format!("{file}: {error}"))
        };
        let html = |file: &str| -> Result<Html, String> {
            Html::parse(&read(file)?).map_err(|error| format!("{file}: {error}"))
        };
        let one = |doc: &Html, file: &str, selector: &str| -> Result<usize, String> {
            match doc.select(selector).as_slice() {
                [node] => Ok(*node),
                found => Err(format!(
                    "{file}: {selector:?} matches {} elements, expected 1",
                    found.len()
                )),
            }
        };
        match self {
            Self::Text {
                file,
                selector,
                equals,
            } => {
                let doc = html(file)?;
                let text = doc.text(one(&doc, file, selector)?);
                (&text == equals).then_some(()).ok_or_else(|| {
                    format!("{file}: {selector:?} text is {text:?}, required {equals:?}")
                })
            }
            Self::Attr {
                file,
                selector,
                name,
                equals,
            } => {
                let doc = html(file)?;
                let value = doc.nodes[one(&doc, file, selector)?].attr(name);
                (value == Some(equals.as_str()))
                    .then_some(())
                    .ok_or_else(|| {
                        format!("{file}: {selector:?} {name} is {value:?}, required {equals:?}")
                    })
            }
            Self::AttrAbsent {
                file,
                selector,
                name,
            } => {
                let doc = html(file)?;
                let found = doc.select(selector);
                if found.is_empty() {
                    return Err(format!("{file}: {selector:?} matches nothing"));
                }
                match found
                    .iter()
                    .find(|node| doc.nodes[**node].attr(name).is_some())
                {
                    Some(_) => Err(format!("{file}: {selector:?} must not carry {name}")),
                    None => Ok(()),
                }
            }
            Self::Count {
                file,
                selector,
                equals,
            } => {
                let count = html(file)?.select(selector).len();
                (count == *equals).then_some(()).ok_or_else(|| {
                    format!("{file}: {selector:?} matches {count}, required {equals}")
                })
            }
            Self::Inside {
                file,
                selector,
                ancestor,
            }
            | Self::NotInside {
                file,
                selector,
                ancestor,
            } => {
                let doc = html(file)?;
                let node = one(&doc, file, selector)?;
                let ancestors: BTreeSet<usize> = doc.select(ancestor).into_iter().collect();
                let inside = doc.ancestors(node).any(|node| ancestors.contains(&node));
                let wanted = matches!(self, Self::Inside { .. });
                (inside == wanted).then_some(()).ok_or_else(|| {
                    let relation = if wanted { "inside" } else { "outside" };
                    format!("{file}: {selector:?} must be {relation} {ancestor:?}")
                })
            }
            Self::Contains { file, text } => read(file)?
                .contains(text.as_str())
                .then_some(())
                .ok_or_else(|| format!("{file}: does not contain {text:?}")),
            Self::CallArity {
                file,
                function,
                consumers,
            } => {
                let source = read(file)?;
                let declared = definition_arity(&source, function)
                    .ok_or_else(|| format!("{file}: no definition of {function}"))?;
                let mut calls = 0;
                for consumer in consumers {
                    for arity in call_arities(&read(consumer)?, function) {
                        calls += 1;
                        if arity != declared {
                            return Err(format!(
                                "{consumer}: calls {function} with {arity} arguments; {file} declares {declared}"
                            ));
                        }
                    }
                }
                (calls > 0)
                    .then_some(())
                    .ok_or_else(|| format!("no call of {function} in {consumers:?}"))
            }
            Self::IdRefsResolve { file, html: page } => {
                let doc = html(page)?;
                let ids: BTreeSet<&str> = doc
                    .nodes
                    .iter()
                    .filter_map(|node| node.attr("id"))
                    .collect();
                let refs = id_refs(&read(file)?);
                if refs.is_empty() {
                    return Err(format!("{file}: no getElementById reference"));
                }
                match refs.iter().find(|id| !ids.contains(id.as_str())) {
                    Some(id) => Err(format!(
                        "{file}: getElementById({id:?}) names no element in {page}"
                    )),
                    None => Ok(()),
                }
            }
        }
    }
}

// ---------------------------------------------------- a minimal HTML model

/// Just enough HTML for the fixtures: elements with double-quoted
/// attributes, text, comments and void elements. Anything else is an error,
/// so a mangled merge fails loudly rather than parsing leniently.
struct Html {
    /// Node 0 is a synthetic root.
    nodes: Vec<Node>,
}

struct Node {
    tag: String,
    attrs: Vec<(String, String)>,
    parent: Option<usize>,
    children: Vec<Child>,
}

enum Child {
    Element(usize),
    Text(String),
}

impl Node {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }
}

const VOID: &[&str] = &["input", "br", "img", "meta", "link", "hr"];

impl Html {
    fn parse(source: &str) -> Result<Self, String> {
        let mut nodes = vec![Node {
            tag: String::new(),
            attrs: Vec::new(),
            parent: None,
            children: Vec::new(),
        }];
        let mut stack = vec![0];
        let mut rest = source;
        while !rest.is_empty() {
            let top = *stack.last().expect("root stays on the stack");
            if let Some(after) = rest.strip_prefix("<!--") {
                let end = after.find("-->").ok_or("unterminated comment")?;
                rest = &after[end + 3..];
            } else if let Some(after) = rest.strip_prefix("</") {
                let end = after.find('>').ok_or("unterminated end tag")?;
                let name = after[..end].trim();
                if stack.len() == 1 || nodes[top].tag != name {
                    return Err(format!("unexpected </{name}> (open: <{}>)", nodes[top].tag));
                }
                stack.pop();
                rest = &after[end + 1..];
            } else if let Some(after) = rest.strip_prefix('<') {
                let (node, self_closing, remaining) = Self::start_tag(after)?;
                let index = nodes.len();
                let void = VOID.contains(&node.0.as_str());
                nodes.push(Node {
                    tag: node.0,
                    attrs: node.1,
                    parent: Some(top),
                    children: Vec::new(),
                });
                nodes[top].children.push(Child::Element(index));
                if !self_closing && !void {
                    stack.push(index);
                }
                rest = remaining;
            } else {
                let end = rest.find('<').unwrap_or(rest.len());
                nodes[top]
                    .children
                    .push(Child::Text(rest[..end].to_owned()));
                rest = &rest[end..];
            }
        }
        if stack.len() != 1 {
            return Err(format!(
                "unclosed <{}>",
                nodes[*stack.last().expect("non-empty")].tag
            ));
        }
        Ok(Self { nodes })
    }

    #[allow(clippy::type_complexity)]
    fn start_tag(source: &str) -> Result<((String, Vec<(String, String)>), bool, &str), String> {
        let name_end = source
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .ok_or("unterminated start tag")?;
        if name_end == 0 {
            return Err("empty tag name".into());
        }
        let tag = source[..name_end].to_owned();
        let mut rest = &source[name_end..];
        let mut attrs: Vec<(String, String)> = Vec::new();
        loop {
            rest = rest.trim_start();
            if let Some(after) = rest.strip_prefix("/>") {
                return Ok(((tag, attrs), true, after));
            }
            if let Some(after) = rest.strip_prefix('>') {
                return Ok(((tag, attrs), false, after));
            }
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || "-_:".contains(c)))
                .ok_or("unterminated attribute")?;
            if end == 0 {
                return Err(format!(
                    "bad attribute in <{tag}> near {:?}",
                    &rest[..rest.len().min(12)]
                ));
            }
            let name = rest[..end].to_owned();
            rest = &rest[end..];
            let value = if let Some(after) = rest.strip_prefix("=\"") {
                let close = after.find('"').ok_or("unterminated attribute value")?;
                rest = &after[close + 1..];
                after[..close].to_owned()
            } else {
                String::new()
            };
            if attrs.iter().any(|(existing, _)| *existing == name) {
                return Err(format!("<{tag}> repeats attribute {name}"));
            }
            attrs.push((name, value));
        }
    }

    fn ancestors(&self, node: usize) -> impl Iterator<Item = usize> + '_ {
        std::iter::successors(self.nodes[node].parent, |node| self.nodes[*node].parent)
            .filter(|node| *node != 0)
    }

    /// Descendant text, whitespace-normalized.
    fn text(&self, node: usize) -> String {
        fn collect(doc: &Html, node: usize, out: &mut String) {
            for child in &doc.nodes[node].children {
                match child {
                    Child::Text(text) => {
                        out.push(' ');
                        out.push_str(text);
                    }
                    Child::Element(child) => collect(doc, *child, out),
                }
            }
        }
        let mut out = String::new();
        collect(self, node, &mut out);
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// Elements matching a descendant selector of compounds such as
    /// `tag#id.class[attr=value]`, in document order.
    fn select(&self, selector: &str) -> Vec<usize> {
        let compounds: Vec<Compound> = selector.split_whitespace().map(Compound::parse).collect();
        let Some((last, outer)) = compounds.split_last() else {
            return Vec::new();
        };
        (1..self.nodes.len())
            .filter(|node| last.matches(&self.nodes[*node]) && self.ancestors_match(*node, outer))
            .collect()
    }

    fn ancestors_match(&self, node: usize, outer: &[Compound]) -> bool {
        let mut remaining = outer;
        for ancestor in self.ancestors(node) {
            let Some((last, rest)) = remaining.split_last() else {
                break;
            };
            if last.matches(&self.nodes[ancestor]) {
                remaining = rest;
            }
        }
        remaining.is_empty()
    }
}

#[derive(Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
    attrs: Vec<(String, String)>,
}

impl Compound {
    fn parse(text: &str) -> Self {
        let mut compound = Self::default();
        let mut rest = text;
        let token_end = |rest: &str| rest.find(['#', '.', '[']).unwrap_or(rest.len());
        let end = token_end(rest);
        if end > 0 {
            compound.tag = Some(rest[..end].to_owned());
        }
        rest = &rest[end..];
        while !rest.is_empty() {
            if let Some(after) = rest.strip_prefix('#') {
                let end = token_end(after);
                compound.id = Some(after[..end].to_owned());
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix('.') {
                let end = token_end(after);
                compound.classes.push(after[..end].to_owned());
                rest = &after[end..];
            } else if let Some(after) = rest.strip_prefix('[') {
                let end = after
                    .find(']')
                    .unwrap_or_else(|| panic!("selector {text:?}: unterminated ["));
                let (name, value) = after[..end]
                    .split_once('=')
                    .unwrap_or_else(|| panic!("selector {text:?}: [name=value]"));
                compound
                    .attrs
                    .push((name.to_owned(), value.trim_matches('"').to_owned()));
                rest = &after[end + 1..];
            } else {
                panic!("selector {text:?}: unexpected {rest:?}");
            }
        }
        compound
    }

    fn matches(&self, node: &Node) -> bool {
        self.tag.as_ref().is_none_or(|tag| *tag == node.tag)
            && self
                .id
                .as_ref()
                .is_none_or(|id| node.attr("id") == Some(id))
            && self.classes.iter().all(|class| {
                node.attr("class")
                    .is_some_and(|classes| classes.split_whitespace().any(|c| c == class))
            })
            && self
                .attrs
                .iter()
                .all(|(name, value)| node.attr(name) == Some(value))
    }
}

// --------------------------------------------- a minimal TypeScript surface

/// The parameter count of `function <name>(…)` in `source`.
fn definition_arity(source: &str, name: &str) -> Option<usize> {
    let needle = format!("function {name}(");
    let start = source.find(&needle)? + needle.len();
    Some(argument_count(&source[start..]))
}

/// The argument count of every call `<name>(…)` that is not its definition
/// or an import.
fn call_arities(source: &str, name: &str) -> Vec<usize> {
    let needle = format!("{name}(");
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = source[from..].find(&needle) {
        let at = from + found;
        let before = &source[..at];
        let word_start = before
            .chars()
            .last()
            .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'));
        if word_start && !before.ends_with("function ") {
            out.push(argument_count(&source[at + needle.len()..]));
        }
        from = at + needle.len();
    }
    out
}

/// Top-level comma-separated items before the closing parenthesis that
/// matches an already-consumed `(`.
fn argument_count(after_open: &str) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut commas = 0;
    let mut any = false;
    for c in after_open.chars() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => {
                quote = Some(c);
                any = true;
            }
            '(' | '[' | '{' | '<' => depth += 1,
            ')' if depth == 0 => break,
            ')' | ']' | '}' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => commas += 1,
            c if !c.is_whitespace() => any = true,
            _ => {}
        }
    }
    if any { commas + 1 } else { 0 }
}

fn id_refs(source: &str) -> Vec<String> {
    let needle = "getElementById(\"";
    source
        .match_indices(needle)
        .filter_map(|(at, _)| {
            let rest = &source[at + needle.len()..];
            rest.find('"').map(|end| rest[..end].to_owned())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_match_descendants_and_compounds() {
        let doc = Html::parse(
            r#"<main id="app"><aside id="s"><article class="card x"><button type="b">A</button></article></aside><p>t</p></main>"#,
        )
        .unwrap();
        assert_eq!(doc.select("#s button").len(), 1);
        assert_eq!(doc.select("#app article.card button[type=b]").len(), 1);
        assert_eq!(doc.select("p button").len(), 0);
        assert_eq!(doc.text(doc.select("#app")[0]), "A t");
    }

    #[test]
    fn malformed_markup_is_an_error() {
        assert!(Html::parse("<a><b></a></b>").is_err());
        assert!(Html::parse("<a x=\"1\" x=\"2\"></a>").is_err());
        assert!(Html::parse("<a>").is_err());
    }

    #[test]
    fn arities_count_top_level_arguments() {
        let source = "export function f(a: number, b: Map<string, number>): string {}\nf(1, g(2, 3)); f(); x.f(9);";
        assert_eq!(definition_arity(source, "f"), Some(2));
        assert_eq!(call_arities(source, "f"), vec![2, 0]);
    }
}
