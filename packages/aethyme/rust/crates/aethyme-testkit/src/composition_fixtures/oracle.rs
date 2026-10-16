//! The answer key and the judge. Nothing here is visible to a composer
//! except [`judge`], [`judge_scenario`] and the outcome types.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::html::Html;
use super::{CaseInput, fixtures_root, tree_files};

const EXPECTATIONS_SCHEMA: &str = "aethyme.composition-expectations/provisional-v0";

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
    pub const ALL: [Outcome; 10] = [
        Self::Candidate,
        Self::Conflict,
        Self::Unsupported,
        Self::ResolutionRequired,
        Self::UnknownBase,
        Self::DependencyCycle,
        Self::MissingInput,
        Self::CompetingRevisions,
        Self::BudgetExhausted,
        Self::InseparableSelection,
    ];

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

/// What a composer produced for one run of a scenario.
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

impl Verdict {
    fn of(failures: Vec<String>) -> Self {
        Self {
            accepted: failures.is_empty(),
            failures,
        }
    }
}

// ------------------------------------------------------------ answer key

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
// Titles, plan references and the measured column are for readers and the
// measuring unit test; the judge itself never reads them.
#[allow(dead_code)]
pub(super) struct Expectations {
    schema: String,
    case: String,
    pub(super) title: String,
    pub(super) plan: String,
    pub(super) scenarios: BTreeMap<String, ScenarioExpectation>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
// Titles, plan references and the measured column are for readers and the
// measuring unit test; the judge itself never reads them.
#[allow(dead_code)]
pub(super) struct ScenarioExpectation {
    pub(super) title: String,
    pub(super) required_outcomes: Vec<Outcome>,
    /// Contributions whose changes a candidate may carry; by default the
    /// composed ones. Everything else in the baseline must survive.
    #[serde(default)]
    allowed_changes: Option<Vec<String>>,
    behaviors: Vec<Behavior>,
    pub(super) provisional_text: ProvisionalText,
}

/// What plain sequential three-way text merge produced, measured.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
// Titles, plan references and the measured column are for readers and the
// measuring unit test; the judge itself never reads them.
#[allow(dead_code)]
pub(super) struct ProvisionalText {
    /// `None` for a planning refusal, which no merge engine decides.
    pub(super) outcome: Option<String>,
    /// For a candidate: `passes` or `fails` judgment.
    pub(super) behavior: Option<String>,
    #[serde(default)]
    pub(super) measured_orders: Vec<Vec<String>>,
    pub(super) note: String,
}

pub(super) fn expectations(case: &CaseInput) -> Expectations {
    let path = fixtures_root()
        .join("expectations")
        .join(&case.kind)
        .join(format!("{}.json", case.case));
    let parsed: Expectations = serde_json::from_str(&crate::repos::read(&path))
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    assert_eq!(parsed.schema, EXPECTATIONS_SCHEMA, "{}", path.display());
    assert_eq!(parsed.case, case.case, "{}", path.display());
    let ids: BTreeSet<&String> = case.scenarios.iter().map(|scenario| &scenario.id).collect();
    assert_eq!(
        ids,
        parsed.scenarios.keys().collect(),
        "{}: scenario ids",
        path.display()
    );
    for (id, scenario) in &parsed.scenarios {
        assert!(
            !scenario.required_outcomes.is_empty(),
            "{}/{id}: no outcome",
            case.case
        );
        assert_eq!(
            scenario.required_outcomes.contains(&Outcome::Candidate),
            !scenario.behaviors.is_empty(),
            "{}/{id}: a scenario accepts a candidate exactly when it states behaviors",
            case.case
        );
        if scenario.required_outcomes.contains(&Outcome::Candidate) {
            assert!(
                !allowed(case, id, scenario).is_empty(),
                "{}/{id}: no allowed changes",
                case.case
            );
        }
    }
    parsed
}

fn allowed(case: &CaseInput, scenario: &str, expectation: &ScenarioExpectation) -> Vec<String> {
    let mut ids = expectation
        .allowed_changes
        .clone()
        .unwrap_or_else(|| case.scenario(scenario).request.compose.clone());
    ids.sort();
    ids.dedup();
    ids
}

// ------------------------------------------------------------------ judge

/// Judge one run: the outcome must be one the plan accepts and, for a
/// candidate, every generic check and stated behavior must hold.
pub fn judge(case: &CaseInput, scenario: &str, observed: Observed<'_>) -> Verdict {
    let expectations = expectations(case);
    let expectation = &expectations.scenarios[scenario];
    Verdict::of(run_failures(case, scenario, expectation, observed))
}

fn run_failures(
    case: &CaseInput,
    scenario: &str,
    expectation: &ScenarioExpectation,
    observed: Observed<'_>,
) -> Vec<String> {
    let outcome = match observed {
        Observed::Candidate(_) => Outcome::Candidate,
        Observed::Refused(outcome) => outcome,
    };
    if outcome == Outcome::Candidate && matches!(observed, Observed::Refused(_)) {
        return vec!["a refusal cannot carry the candidate outcome".to_owned()];
    }
    if !expectation.required_outcomes.contains(&outcome) {
        let names: Vec<&str> = expectation
            .required_outcomes
            .iter()
            .map(|outcome| outcome.as_str())
            .collect();
        return vec![format!(
            "outcome {} is not one of {names:?}",
            outcome.as_str()
        )];
    }
    let Observed::Candidate(dir) = observed else {
        return Vec::new();
    };
    let mut failures = candidate_failures(dir);
    failures.extend(preservation_failures(
        case,
        scenario,
        &allowed(case, scenario, expectation),
        dir,
    ));
    for behavior in &expectation.behaviors {
        if let Err(failure) = behavior.check(dir) {
            failures.push(failure);
        }
    }
    failures
}

/// Judge a whole scenario from every run of it, as `(order, observed)`.
///
/// * Each run passes [`judge`].
/// * Every declared order is run at least twice (a subtraction, which has
///   no declared orders, at least twice with an empty order), and no
///   undeclared order is run.
/// * Every run has the same outcome.
/// * Candidates are byte-identical across all runs when the scenario is
///   commutative, and across repeats of the same order otherwise.
pub fn judge_scenario(
    case: &CaseInput,
    scenario: &str,
    runs: &[(Vec<String>, Observed<'_>)],
) -> Verdict {
    let expectations = expectations(case);
    let expectation = &expectations.scenarios[scenario];
    let input = case.scenario(scenario);
    let mut failures = Vec::new();
    let declared: Vec<Vec<String>> = if input.orders.is_empty() {
        vec![Vec::new()]
    } else {
        input.orders.clone()
    };
    for order in &declared {
        let count = runs.iter().filter(|(run, _)| run == order).count();
        if count < 2 {
            failures.push(format!(
                "order {order:?} ran {count} time(s); run every order at least twice"
            ));
        }
    }
    for (order, _) in runs {
        if !declared.contains(order) {
            failures.push(format!(
                "order {order:?} is not one of the scenario's orders"
            ));
        }
    }
    for (index, (order, observed)) in runs.iter().enumerate() {
        for failure in run_failures(case, scenario, expectation, *observed) {
            failures.push(format!("run {index} {order:?}: {failure}"));
        }
    }
    let outcome = |observed: &Observed<'_>| match observed {
        Observed::Candidate(_) => Outcome::Candidate,
        Observed::Refused(outcome) => *outcome,
    };
    if let Some((_, first)) = runs.first() {
        let distinct: BTreeSet<&str> = runs
            .iter()
            .map(|(_, observed)| outcome(observed).as_str())
            .collect();
        if distinct.len() > 1 {
            failures.push(format!("runs disagree on the outcome: {distinct:?}"));
        } else if let Observed::Candidate(first_dir) = first {
            for (order, observed) in runs {
                let Observed::Candidate(dir) = observed else {
                    continue;
                };
                let reference = if input.commutative {
                    *first_dir
                } else {
                    runs.iter()
                        .find_map(|(other, observed)| match observed {
                            Observed::Candidate(other_dir) if other == order => Some(*other_dir),
                            _ => None,
                        })
                        .expect("this run itself")
                };
                if let Some(difference) = tree_difference(reference, dir) {
                    let scope = if input.commutative {
                        "across orders"
                    } else {
                        "across repeats"
                    };
                    failures.push(format!(
                        "order {order:?}: candidate differs {scope}: {difference}"
                    ));
                }
            }
        }
    } else {
        failures.push("no runs".to_owned());
    }
    failures.dedup();
    Verdict::of(failures)
}

fn tree_difference(a: &Path, b: &Path) -> Option<String> {
    let (files_a, files_b) = (tree_files(a), tree_files(b));
    if files_a != files_b {
        return Some(format!("file sets {files_a:?} and {files_b:?}"));
    }
    files_a
        .into_iter()
        .find(|path| std::fs::read(a.join(path)).ok() != std::fs::read(b.join(path)).ok())
        .map(|path| format!("{path} differs"))
}

// ----------------------------------------------------- generic candidate checks

const MARKERS: [&str; 4] = ["<<<<<<<", "=======", ">>>>>>>", "|||||||"];

/// Every candidate: no conflict markers (indented or not), and every HTML
/// file parses with unique ids.
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
            MARKERS
                .iter()
                .any(|marker| line.trim_start().starts_with(marker))
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

/// Nothing outside the allowed contributions' changes may move:
///
/// * the candidate's file set is the baseline's, plus files the allowed
///   contributions add, minus files they delete;
/// * a file no allowed contribution changes is byte-identical to the
///   baseline's;
/// * in an HTML file, every baseline element with an id that no allowed
///   contribution changes (tag, attributes, or descendant text) is present
///   with the same tag, attributes and descendant text.
fn preservation_failures(
    case: &CaseInput,
    scenario: &str,
    allowed: &[String],
    dir: &Path,
) -> Vec<String> {
    let baseline = case.tree_dir(&case.scenario(scenario).baseline);
    let pairs: Vec<(PathBuf, PathBuf)> = allowed
        .iter()
        .map(|id| (case.base_dir(id), case.result_dir(id)))
        .collect();
    let read = |dir: &Path, path: &str| std::fs::read(dir.join(path)).ok();
    let mut expected: BTreeSet<String> = tree_files(&baseline).into_iter().collect();
    for (base, result) in &pairs {
        let (before, after): (BTreeSet<String>, BTreeSet<String>) = (
            tree_files(base).into_iter().collect(),
            tree_files(result).into_iter().collect(),
        );
        expected.extend(after.difference(&before).cloned());
        for deleted in before.difference(&after) {
            expected.remove(deleted);
        }
    }
    let actual: BTreeSet<String> = tree_files(dir).into_iter().collect();
    let mut failures = Vec::new();
    for missing in expected.difference(&actual) {
        failures.push(format!("{missing}: missing from the candidate"));
    }
    for extra in actual.difference(&expected) {
        failures.push(format!("{extra}: no allowed contribution adds it"));
    }
    for path in tree_files(&baseline) {
        let Some(candidate) = read(dir, &path) else {
            continue;
        };
        let changers: Vec<&(PathBuf, PathBuf)> = pairs
            .iter()
            .filter(|(base, result)| read(base, &path) != read(result, &path))
            .collect();
        let original = read(&baseline, &path).expect("baseline file");
        if changers.is_empty() {
            if candidate != original {
                failures.push(format!(
                    "{path}: no allowed contribution changes it, but the candidate does"
                ));
            }
            continue;
        }
        if !path.ends_with(".html") {
            continue;
        }
        let parse = |bytes: &[u8]| {
            Html::parse(&String::from_utf8_lossy(bytes))
                .ok()
                .map(|doc| signatures(&doc))
        };
        let (Some(before), Some(after)) = (parse(&original), parse(&candidate)) else {
            continue; // a parse failure is already reported
        };
        let touched = |id: &str| {
            changers.iter().any(|(base, result)| {
                let side = |dir: &Path| {
                    read(dir, &path)
                        .and_then(|bytes| parse(&bytes))
                        .and_then(|sigs| sigs.get(id).cloned())
                };
                side(base) != side(result)
            })
        };
        for (id, signature) in &before {
            if touched(id) {
                continue;
            }
            match after.get(id) {
                None => failures.push(format!(
                    "{path}: #{id} was dropped, though no allowed contribution changes it"
                )),
                Some(now) if now != signature => failures.push(format!(
                    "{path}: #{id} changed, though no allowed contribution changes it"
                )),
                Some(_) => {}
            }
        }
    }
    failures
}

/// Element id → its tag, sorted attributes and descendant text.
fn signatures(doc: &Html) -> BTreeMap<String, String> {
    (1..doc.nodes.len())
        .filter_map(|index| {
            let node = &doc.nodes[index];
            let id = node.attr("id")?;
            let mut attrs = node.attrs.clone();
            attrs.sort();
            Some((
                id.to_owned(),
                format!("{}{attrs:?}|{}", node.tag, doc.text(index)),
            ))
        })
        .collect()
}

// ------------------------------------------------------------- behaviors

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
    /// The comment-stripped source contains `text`.
    Contains { file: String, text: String },
    /// Every call of `function` in `consumers` passes as many arguments as
    /// its definition in `file` declares, and there is at least one call.
    CallArity {
        file: String,
        function: String,
        consumers: Vec<String>,
    },
    /// Every `getElementById("x")` and `querySelector("#x")` in `file`
    /// names an id present in `html`, and there is at least one.
    IdRefsResolve { file: String, html: String },
    /// Some `addEventListener("<event>", …)` call's arguments look up `id`,
    /// directly or through a `const` bound to it.
    ListenerReferences {
        file: String,
        event: String,
        id: String,
    },
}

impl Behavior {
    fn check(&self, dir: &Path) -> Result<(), String> {
        let read = |file: &str| {
            std::fs::read_to_string(dir.join(file)).map_err(|error| format!("{file}: {error}"))
        };
        let script = |file: &str| read(file).map(|source| strip_comments(&source));
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
            Self::Contains { file, text } => script(file)?
                .contains(text.as_str())
                .then_some(())
                .ok_or_else(|| format!("{file}: does not contain {text:?}")),
            Self::CallArity {
                file,
                function,
                consumers,
            } => {
                let source = script(file)?;
                let declared = definition_arity(&source, function)
                    .ok_or_else(|| format!("{file}: no definition of {function}"))?;
                let mut calls = 0;
                for consumer in consumers {
                    for arity in call_arities(&script(consumer)?, function) {
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
                let refs = id_refs(&script(file)?);
                if refs.is_empty() {
                    return Err(format!("{file}: no element lookup"));
                }
                match refs.iter().find(|id| !ids.contains(id.as_str())) {
                    Some(id) => Err(format!("{file}: looks up #{id}, which is not in {page}")),
                    None => Ok(()),
                }
            }
            Self::ListenerReferences { file, event, id } => {
                let source = script(file)?;
                let bound = bindings(&source, id);
                let found = listener_arguments(&source, event).iter().any(|arguments| {
                    id_refs(arguments).iter().any(|found| found == id)
                        || bound.iter().any(|name| contains_word(arguments, name))
                });
                found
                    .then_some(())
                    .ok_or_else(|| format!("{file}: no {event:?} listener looks up #{id}"))
            }
        }
    }
}

// --------------------------------------------- a minimal TypeScript surface

/// `source` with `//` and `/* */` comments blanked, string literals kept.
pub(super) fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(open) = quote {
            out.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            } else if c == open {
                quote = None;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('/', Some('/')) => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut previous = ' ';
                for c in chars.by_ref() {
                    if c == '\n' {
                        out.push('\n');
                    }
                    if previous == '*' && c == '/' {
                        break;
                    }
                    previous = c;
                }
            }
            ('"' | '\'' | '`', _) => {
                quote = Some(c);
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// The parameter count of `function <name>(…)` in `source`.
pub(super) fn definition_arity(source: &str, name: &str) -> Option<usize> {
    let needle = format!("function {name}(");
    let start = source.find(&needle)? + needle.len();
    Some(argument_count(&source[start..]))
}

/// The argument count of every call `<name>(…)` that is not its definition
/// or a member call.
pub(super) fn call_arities(source: &str, name: &str) -> Vec<usize> {
    let needle = format!("{name}(");
    source
        .match_indices(&needle)
        .filter(|(at, _)| {
            let before = &source[..*at];
            before
                .chars()
                .last()
                .is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '.'))
                && !before.ends_with("function ")
        })
        .map(|(at, _)| argument_count(&source[at + needle.len()..]))
        .collect()
}

/// Top-level comma-separated items before the `)` that closes an
/// already-consumed `(`. `<…>` nests only as a generic (directly after an
/// identifier), so `=>` and spaced comparisons do not miscount.
pub(super) fn argument_count(after_open: &str) -> usize {
    arguments_text(after_open).map_or(0, |text| {
        let mut depth = 0usize;
        let mut angle = 0usize;
        let mut quote: Option<char> = None;
        let mut commas = 0;
        let mut previous = ' ';
        for c in text.chars() {
            if let Some(open) = quote {
                if c == open {
                    quote = None;
                }
                previous = c;
                continue;
            }
            match c {
                '"' | '\'' | '`' => quote = Some(c),
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = depth.saturating_sub(1),
                '<' if previous.is_alphanumeric() || previous == '_' => angle += 1,
                '>' if angle > 0 && previous != '=' => angle -= 1,
                ',' if depth == 0 && angle == 0 => commas += 1,
                _ => {}
            }
            previous = c;
        }
        if text.trim().is_empty() {
            0
        } else {
            commas + 1
        }
    })
}

/// The text between an already-consumed `(` and its matching `)`.
fn arguments_text(after_open: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    for (index, c) in after_open.char_indices() {
        if let Some(open) = quote {
            if c == open {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => quote = Some(c),
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => return Some(&after_open[..index]),
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

/// Ids looked up with `getElementById("x")` or `querySelector[All][<T>]("#x")`.
pub(super) fn id_refs(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (at, needle) in source.match_indices("getElementById(\"") {
        let rest = &source[at + needle.len()..];
        if let Some(end) = rest.find('"') {
            out.push(rest[..end].to_owned());
        }
    }
    for (at, needle) in source.match_indices("querySelector") {
        let mut rest = &source[at + needle.len()..];
        rest = rest.strip_prefix("All").unwrap_or(rest);
        if let Some(generic) = rest.strip_prefix('<') {
            rest = generic.find('>').map_or("", |end| &generic[end + 1..]);
        }
        let Some(literal) = rest.strip_prefix("(\"").or_else(|| rest.strip_prefix("('")) else {
            continue;
        };
        if let Some(id) = literal.strip_prefix('#') {
            let end = id
                .find(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_'))
                .unwrap_or(id.len());
            out.push(id[..end].to_owned());
        }
    }
    out
}

/// Names of `const`/`let` bindings initialized by looking up `id`.
fn bindings(source: &str, id: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line
                .strip_prefix("const ")
                .or_else(|| line.strip_prefix("let "))?;
            let (name, value) = rest.split_once('=')?;
            id_refs(value)
                .iter()
                .any(|found| found == id)
                .then(|| name.trim().to_owned())
        })
        .collect()
}

/// The argument text of every `addEventListener("<event>", …)` call.
fn listener_arguments(source: &str, event: &str) -> Vec<String> {
    let needle = format!("addEventListener(\"{event}\"");
    source
        .match_indices(&needle)
        .filter_map(|(at, _)| {
            arguments_text(&source[at + "addEventListener(".len()..]).map(str::to_owned)
        })
        .collect()
}

fn contains_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().last();
        let after = text[at + word.len()..].chars().next();
        let boundary = |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
        boundary(before) && boundary(after)
    })
}
