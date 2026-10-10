//! The provisional composition fixtures (L4 #664, standing in for E1 #650)
//! are internally consistent, their oracle accepts correct compositions
//! and rejects wrong ones, and their "provisional text profile" column is
//! measured rather than guessed.
use std::path::{Path, PathBuf};
use std::process::Command;

use aethyme_testkit::composition_fixtures::{
    self as fixtures, Case, Observed, Outcome, judge, materialize, tree_files,
};

fn all_cases() -> Vec<Case> {
    let mut cases = fixtures::cases();
    cases.extend(fixtures::held_out_cases());
    cases
}

#[test]
fn every_plan_case_is_present() {
    let ids: Vec<String> = fixtures::cases()
        .into_iter()
        .map(|case| case.case)
        .collect();
    assert_eq!(
        ids,
        ["FX01", "FX02", "FX03", "FX04", "FX05", "FX06", "FX07"]
    );
    assert!(fixtures::held_out_cases().len() >= 3);
}

/// Sequential three-way text merge per plan §7.3 step 5: each contribution's
/// `(base, accumulator, result)` merged file by file with `git merge-file`.
/// This is a measuring instrument for the fixture column, not a composer.
fn text_compose(case: &Case, baseline: &str, order: &[String], scratch: &Path) -> Option<PathBuf> {
    let acc = scratch.join("acc");
    copy_tree(&case.tree_dir(baseline), &acc);
    for id in order {
        let base = case.base_dir(id);
        let result = case.result_dir(id);
        let mut paths = tree_files(&base);
        paths.extend(tree_files(&result));
        paths.extend(tree_files(&acc));
        paths.sort();
        paths.dedup();
        for path in paths {
            let b = std::fs::read(base.join(&path)).ok();
            let a = std::fs::read(acc.join(&path)).ok();
            let r = std::fs::read(result.join(&path)).ok();
            let merged = if r == b || a == r {
                a
            } else if a == b {
                r
            } else {
                Some(merge_file(
                    scratch,
                    b.as_deref(),
                    a.as_deref(),
                    r.as_deref(),
                )?)
            };
            match merged {
                Some(bytes) => {
                    std::fs::create_dir_all(acc.join(&path).parent().unwrap()).unwrap();
                    std::fs::write(acc.join(&path), bytes).unwrap();
                }
                None => {
                    let _ = std::fs::remove_file(acc.join(&path));
                }
            }
        }
    }
    Some(acc)
}

fn merge_file(
    scratch: &Path,
    base: Option<&[u8]>,
    ours: Option<&[u8]>,
    theirs: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let side = |name: &str, bytes: Option<&[u8]>| {
        let path = scratch.join(name);
        std::fs::write(&path, bytes.unwrap_or_default()).unwrap();
        path
    };
    let (b, o, t) = (
        side("m.base", base),
        side("m.ours", ours),
        side("m.theirs", theirs),
    );
    let output = Command::new("git")
        .args(["merge-file", "-p"])
        .args([&o, &b, &t])
        .output()
        .expect("git merge-file");
    match output.status.code() {
        Some(0) => Some(output.stdout),
        Some(code) if code > 0 => None,
        _ => panic!(
            "git merge-file failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    for path in tree_files(from) {
        let dest = to.join(&path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(from.join(&path), dest).unwrap();
    }
}

#[test]
fn the_provisional_text_column_is_measured() {
    let mut report = Vec::new();
    let mut mismatches = Vec::new();
    for case in all_cases() {
        for scenario in &case.scenarios {
            let column = &scenario.provisional_text;
            let Some(expected) = column.outcome.as_deref() else {
                continue;
            };
            let orders = if column.measured_orders.is_empty() {
                &scenario.input.orders
            } else {
                &column.measured_orders
            };
            assert!(
                !orders.is_empty(),
                "{}/{}: nothing to measure",
                case.case,
                scenario.input.id
            );
            for order in orders {
                let scratch = aethyme_testkit::tmp_dir();
                let (outcome, behavior) =
                    match text_compose(&case, &scenario.input.baseline, order, scratch.path()) {
                        None => ("conflict", None),
                        Some(dir) => {
                            // Judge behaviors only, as if a candidate were acceptable.
                            let verdict =
                                judge(&case, &scenario.input.id, Observed::Candidate(&dir));
                            let behavior = if verdict.accepted { "passes" } else { "fails" };
                            ("candidate", Some((behavior, verdict.failures)))
                        }
                    };
                let label = format!("{}/{} {order:?}", case.case, scenario.input.id);
                report.push(format!(
                    "{label}: {outcome} {:?}",
                    behavior.as_ref().map(|b| b.0)
                ));
                if outcome != expected
                    || behavior.as_ref().map(|b| b.0) != column.behavior.as_deref()
                {
                    mismatches.push(format!(
                        "{label}: measured {outcome} {behavior:?}; case.json says {expected} {:?}",
                        column.behavior
                    ));
                }
            }
        }
    }
    eprintln!("{}", report.join("\n"));
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

/// `oracle-selftest/<case>/<scenario>/<variant>/`: `correct*` trees must be
/// accepted, `wrong-*` trees rejected. Each case has at least one wrong one.
#[test]
fn the_oracle_accepts_correct_and_rejects_wrong_compositions() {
    let root = fixtures::fixtures_root().join("oracle-selftest");
    for case in all_cases() {
        let mut wrong = 0;
        let mut correct = 0;
        for scenario in &case.scenarios {
            let id = &scenario.input.id;
            let dir = root.join(&case.case).join(id);
            let accepts_candidate = scenario.required_outcomes.contains(&Outcome::Candidate);
            let mut variants: Vec<PathBuf> = match std::fs::read_dir(&dir) {
                Ok(entries) => entries.map(|entry| entry.unwrap().path()).collect(),
                Err(_) => Vec::new(),
            };
            variants.sort();
            if accepts_candidate {
                assert!(
                    variants.iter().any(|v| v
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("correct")),
                    "{}/{id}: no correct composition to test the oracle with",
                    case.case
                );
            }
            for variant in variants {
                let name = variant.file_name().unwrap().to_str().unwrap().to_owned();
                let verdict = judge(&case, id, Observed::Candidate(&variant));
                if name.starts_with("correct") {
                    correct += 1;
                    assert!(
                        verdict.accepted,
                        "{}/{id}/{name} rejected: {:?}",
                        case.case, verdict.failures
                    );
                } else {
                    assert!(
                        name.starts_with("wrong-"),
                        "{}/{id}/{name}: name a variant correct* or wrong-*",
                        case.case
                    );
                    wrong += 1;
                    assert!(!verdict.accepted, "{}/{id}/{name} accepted", case.case);
                }
            }
            // Refusals: every required one is accepted, a candidate where none is allowed is not.
            for outcome in &scenario.required_outcomes {
                if *outcome != Outcome::Candidate {
                    assert!(judge(&case, id, Observed::Refused(*outcome)).accepted);
                }
            }
            for outcome in [
                Outcome::Conflict,
                Outcome::Unsupported,
                Outcome::UnknownBase,
                Outcome::BudgetExhausted,
            ] {
                let accepted = judge(&case, id, Observed::Refused(outcome)).accepted;
                assert_eq!(
                    accepted,
                    scenario.required_outcomes.contains(&outcome),
                    "{}/{id} {outcome:?}",
                    case.case
                );
            }
            assert!(!judge(&case, id, Observed::Refused(Outcome::Candidate)).accepted);
        }
        if !case.held_out {
            assert!(
                wrong >= 1,
                "{}: the oracle must reject at least one wrong composition",
                case.case
            );
            assert!(
                correct >= 1
                    || case
                        .scenarios
                        .iter()
                        .all(|s| !s.required_outcomes.contains(&Outcome::Candidate))
            );
        }
    }
}

#[test]
fn materialize_commits_each_contribution_on_its_exact_base() {
    let scratch = aethyme_testkit::tmp_dir();
    for case in all_cases() {
        let repo = materialize(&case, scratch.path());
        let rev = |args: &[&str]| {
            let output = Command::new("git")
                .arg("-C")
                .arg(&repo.repo)
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{args:?}");
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };
        for contribution in &case.contributions {
            let commits = &repo.contributions[&contribution.id];
            assert_eq!(
                rev(&["rev-parse", &format!("{}^", commits.result)]),
                commits.base
            );
            let expected_base = match contribution.base_ref() {
                fixtures::BaseRef::Tree(name) => repo.trees[&name].clone(),
                fixtures::BaseRef::Contribution(id) => repo.contributions[&id].result.clone(),
            };
            assert_eq!(
                commits.base, expected_base,
                "{}/{}",
                case.case, contribution.id
            );
            for path in tree_files(&case.result_dir(&contribution.id)) {
                let blob = rev(&["show", &format!("{}:{path}", commits.result)]);
                let file =
                    std::fs::read_to_string(case.result_dir(&contribution.id).join(&path)).unwrap();
                assert_eq!(
                    blob,
                    file.trim_end(),
                    "{}/{}/{path}",
                    case.case,
                    contribution.id
                );
            }
            let listed = rev(&["ls-tree", "-r", "--name-only", &commits.result]);
            assert_eq!(
                listed.lines().collect::<Vec<_>>(),
                tree_files(&case.result_dir(&contribution.id))
            );
        }
        for (name, tree) in &case.trees {
            if let Some(parent) = &tree.parent {
                assert_eq!(
                    rev(&["rev-parse", &format!("{}^", repo.trees[name])]),
                    repo.trees[parent]
                );
            }
        }
        // Deterministic: a second materialization gives the same commits.
        let again = materialize(&case, &scratch.path().join("again"));
        assert_eq!(again.trees, repo.trees);
        assert_eq!(again.contributions, repo.contributions);
    }
}
