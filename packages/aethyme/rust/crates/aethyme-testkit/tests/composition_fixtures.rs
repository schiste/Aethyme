//! The provisional composition fixtures (L4 #664, standing in for E1 #650)
//! through their public, composer-facing API only. The oracle's own tests
//! (measured column, self-tests, review attacks) are unit tests inside
//! `aethyme_testkit::composition_fixtures`, since they read the answer key.
use std::process::Command;

use aethyme_testkit::composition_fixtures::{
    self as fixtures, BaseRef, Observed, Outcome, judge_scenario,
};

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
    let held_out: Vec<String> = fixtures::held_out_cases()
        .into_iter()
        .map(|case| case.case)
        .collect();
    assert_eq!(
        held_out,
        ["HX01", "HX02", "HX03", "HX04", "HX05", "HX06", "HX07"]
    );
}

#[test]
fn materialize_puts_each_retained_contribution_on_its_exact_base() {
    let scratch = aethyme_testkit::tmp_dir();
    for case in fixtures::cases()
        .into_iter()
        .chain(fixtures::held_out_cases())
    {
        for scenario in &case.scenarios {
            let repo = case.materialize(&scenario.id, scratch.path());
            let git = |args: &[&str]| {
                let output = Command::new("git")
                    .arg("-C")
                    .arg(&repo.repo)
                    .args(args)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{args:?}");
                String::from_utf8(output.stdout).unwrap().trim().to_owned()
            };
            let label = format!("{}/{}", case.case, scenario.id);
            assert_eq!(repo.accepted.first(), Some(&repo.baseline), "{label}");
            for contribution in &case.contributions {
                let retained = !scenario.unretained.contains(&contribution.id);
                let Some(commits) = repo.contributions.get(&contribution.id) else {
                    assert!(!retained, "{label}: {} missing", contribution.id);
                    continue;
                };
                assert!(
                    retained,
                    "{label}: unretained {} materialized",
                    contribution.id
                );
                assert_eq!(
                    git(&["rev-parse", &format!("{}^", commits.result)]),
                    commits.base
                );
                if let BaseRef::Contribution(base) = &contribution.base {
                    assert_eq!(commits.base, repo.contributions[base].result, "{label}");
                }
            }
            // Neutral names only: each ref is `fixture/<id>`, its message the id.
            for line in git(&[
                "for-each-ref",
                "--format=%(refname:short) %(contents:subject)",
            ])
            .lines()
            {
                let (name, subject) = line.split_once(' ').unwrap();
                assert_eq!(name, format!("fixture/{subject}"), "{label}: {line}");
                let number = subject
                    .strip_prefix('t')
                    .or_else(|| subject.strip_prefix('c'))
                    .unwrap_or("");
                assert!(
                    !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()),
                    "{label}: {line}"
                );
            }
            let again = case.materialize(&scenario.id, &scratch.path().join("again"));
            assert_eq!(
                (&again.baseline, &again.accepted, &again.contributions),
                (&repo.baseline, &repo.accepted, &repo.contributions),
                "{label}: not deterministic"
            );
        }
    }
}

#[test]
fn an_unrecorded_base_is_not_accepted_history() {
    let scratch = aethyme_testkit::tmp_dir();
    let fx03 = fixtures::cases()
        .into_iter()
        .find(|case| case.case == "FX03")
        .unwrap();
    let repo = fx03.materialize("s3", scratch.path());
    let base = &repo.contributions["c3"].base;
    assert!(!repo.accepted.contains(base));
    assert!(
        repo.contributions
            .values()
            .all(|commits| &commits.result != base)
    );
    // c1's base is accepted history even though the baseline moved on.
    assert!(repo.accepted.contains(&repo.contributions["c1"].base));
    assert_ne!(repo.contributions["c1"].base, repo.baseline);
}

#[test]
fn judge_scenario_is_usable_from_outside() {
    let fx05 = fixtures::cases()
        .into_iter()
        .find(|case| case.case == "FX05")
        .unwrap();
    let runs: Vec<_> = fx05
        .scenario("s1")
        .orders
        .iter()
        .flat_map(|order| {
            [
                (order.clone(), Observed::Refused(Outcome::Conflict)),
                (order.clone(), Observed::Refused(Outcome::Conflict)),
            ]
        })
        .collect();
    assert!(judge_scenario(&fx05, "s1", &runs).accepted);
}
