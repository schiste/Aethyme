//! Oracle-side tests. They live inside the module because they read the
//! answer key, which nothing public exposes.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::html::Html;
use super::oracle::{
    self, argument_count, call_arities, definition_arity, id_refs, strip_comments,
};
use super::*;

fn all_cases() -> Vec<CaseInput> {
    let mut cases = cases();
    cases.extend(held_out_cases());
    cases
}

fn case(id: &str) -> CaseInput {
    all_cases()
        .into_iter()
        .find(|case| case.case == id)
        .expect("case")
}

fn selftest(case: &str, scenario: &str, variant: &str) -> PathBuf {
    fixtures_root()
        .join("oracle-selftest")
        .join(case)
        .join(scenario)
        .join(variant)
}

fn copy_tree(from: &Path, to: &Path) {
    let _ = std::fs::remove_dir_all(to);
    for path in tree_files(from) {
        let dest = to.join(&path);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::copy(from.join(&path), dest).unwrap();
    }
}

// ------------------------------------------------- the measured text column

/// Sequential three-way text merge per plan §7.3 step 5: each contribution's
/// `(exact base, accumulator, result)` merged file by file with
/// `git merge-file`. A measuring instrument for the column, not a composer.
fn text_compose(
    case: &CaseInput,
    baseline: &str,
    order: &[String],
    scratch: &Path,
) -> Option<PathBuf> {
    let acc = scratch.join("acc");
    copy_tree(&case.tree_dir(baseline), &acc);
    for id in order {
        let (base, result) = (case.base_dir(id), case.result_dir(id));
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
                Some(bytes) => std::fs::write(acc.join(&path), bytes).unwrap(),
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

#[test]
fn the_provisional_text_column_is_measured() {
    let mut report = Vec::new();
    let mut mismatches = Vec::new();
    for case in all_cases() {
        let expectations = oracle::expectations(&case);
        for scenario in &case.scenarios {
            let column = &expectations.scenarios[&scenario.id].provisional_text;
            let Some(expected) = column.outcome.as_deref() else {
                continue;
            };
            let orders = if column.measured_orders.is_empty() {
                &scenario.orders
            } else {
                &column.measured_orders
            };
            assert!(
                !orders.is_empty(),
                "{}/{}: nothing to measure",
                case.case,
                scenario.id
            );
            for order in orders {
                let scratch = crate::tmp_dir();
                let (outcome, behavior) =
                    match text_compose(&case, &scenario.baseline, order, scratch.path()) {
                        None => ("conflict", None),
                        Some(dir) => {
                            let verdict = judge(&case, &scenario.id, Observed::Candidate(&dir));
                            let behavior = if verdict.accepted { "passes" } else { "fails" };
                            ("candidate", Some((behavior, verdict.failures)))
                        }
                    };
                let label = format!("{}/{} {order:?}", case.case, scenario.id);
                report.push(format!("{label}: {outcome} {behavior:?}"));
                if outcome != expected
                    || behavior.as_ref().map(|b| b.0) != column.behavior.as_deref()
                {
                    mismatches.push(format!("{label}: measured {outcome} {behavior:?}; expectations say {expected} {:?}", column.behavior));
                }
            }
        }
    }
    eprintln!("{}", report.join("\n"));
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

// ------------------------------------------------------- oracle self-tests

/// `oracle-selftest/<case>/<scenario>/<variant>/`: `correct*` trees must be
/// accepted, `wrong-*` trees rejected. Every case, held-out included, has a
/// wrong one, and every scenario that accepts a candidate has a correct one.
#[test]
fn the_oracle_accepts_correct_and_rejects_wrong_compositions() {
    for case in all_cases() {
        let expectations = oracle::expectations(&case);
        let mut wrong = 0;
        for scenario in &case.scenarios {
            let id = &scenario.id;
            let required = &expectations.scenarios[id].required_outcomes;
            let dir = fixtures_root()
                .join("oracle-selftest")
                .join(&case.case)
                .join(id);
            let mut variants: Vec<PathBuf> = std::fs::read_dir(&dir)
                .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
                .unwrap_or_default();
            variants.sort();
            let name =
                |variant: &PathBuf| variant.file_name().unwrap().to_str().unwrap().to_owned();
            if required.contains(&Outcome::Candidate) {
                assert!(
                    variants.iter().any(|v| name(v).starts_with("correct")),
                    "{}/{id}: no correct composition",
                    case.case
                );
            }
            for variant in &variants {
                let verdict = judge(&case, id, Observed::Candidate(variant));
                if name(variant).starts_with("correct") {
                    assert!(
                        verdict.accepted,
                        "{}/{id}/{} rejected: {:?}",
                        case.case,
                        name(variant),
                        verdict.failures
                    );
                } else {
                    assert!(
                        name(variant).starts_with("wrong-"),
                        "{}/{id}/{}: name it correct* or wrong-*",
                        case.case,
                        name(variant)
                    );
                    wrong += 1;
                    assert!(
                        !verdict.accepted,
                        "{}/{id}/{} accepted",
                        case.case,
                        name(variant)
                    );
                }
            }
            for outcome in Outcome::ALL {
                let accepted = judge(&case, id, Observed::Refused(outcome)).accepted;
                let allowed = outcome != Outcome::Candidate && required.contains(&outcome);
                assert_eq!(
                    accepted, allowed,
                    "{}/{id} refused with {outcome:?}",
                    case.case
                );
            }
        }
        assert!(
            wrong >= 1,
            "{}: the oracle must reject at least one wrong composition",
            case.case
        );
    }
}

// ------------------------------------- adversarial review (must reject)

/// Copy `from`, then apply `(file, find, replace)` edits (`find` empty
/// appends to a possibly new file).
fn mutate(from: &Path, edits: &[(&str, &str, &str)]) -> tempfile::TempDir {
    let temp = crate::tmp_dir();
    copy_tree(from, temp.path());
    for (file, find, replace) in edits {
        let path = temp.path().join(file);
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            find.is_empty() || text.contains(find),
            "{file}: no {find:?}"
        );
        let next = if find.is_empty() {
            format!("{text}{replace}")
        } else {
            text.replacen(find, replace, 1)
        };
        std::fs::write(&path, next).unwrap();
    }
    temp
}

fn must_reject(label: &str, case: &CaseInput, scenario: &str, dir: &Path) {
    let verdict = judge(case, scenario, Observed::Candidate(dir));
    assert!(!verdict.accepted, "{label}: accepted");
}

#[test]
fn review_attacks_are_rejected() {
    let fx01 = case("FX01");
    let attack = mutate(
        &selftest("FX01", "s1", "correct"),
        &[
            ("app.html", "  <h1 id=\"title\">Join the list</h1>\n", ""),
            ("app.html", "type=\"email\"", "type=\"text\""),
        ],
    );
    must_reject(
        "FX01 dropped title, input type changed",
        &fx01,
        "s1",
        attack.path(),
    );
    let attack = mutate(
        &selftest("FX01", "s1", "correct"),
        &[("app.html", "type=\"email\"", "type=\"text\"")],
    );
    must_reject("FX01 input type changed", &fx01, "s1", attack.path());

    let fx02 = case("FX02");
    let attack = mutate(
        &selftest("FX02", "s1", "correct"),
        &[
            ("app.html", "        <h3>News</h3>\n", ""),
            ("app.html", "    <h2 id=\"sidebar-title\">Pinned</h2>\n", ""),
        ],
    );
    must_reject("FX02 dropped children", &fx02, "s1", attack.path());
    let attack = mutate(
        &selftest("FX02", "s1", "correct"),
        &[("app.html", "        <h3>News</h3>\n", "")],
    );
    must_reject(
        "FX02 dropped the moved card's heading",
        &fx02,
        "s1",
        attack.path(),
    );
    for variant in ["wrong-lucky-guess", "wrong-edit-on-stayed-twin"] {
        must_reject(
            "FX02 twins guess",
            &fx02,
            "s2",
            &selftest("FX02", "s2", variant),
        );
    }

    let fx03 = case("FX03");
    must_reject("FX03 whole c2 tree", &fx03, "s1", &fx03.result_dir("c2"));
    must_reject(
        "FX03 dup whole c2 tree",
        &fx03,
        "s2",
        &fx03.result_dir("c2"),
    );
    // A lineage-ignoring merge (every contribution against the baseline,
    // not its exact base) must conflict or produce a rejected candidate.
    let scratch = crate::tmp_dir();
    let acc = scratch.path().join("acc");
    copy_tree(&fx03.tree_dir("t1"), &acc);
    let mut conflicted = false;
    for id in ["c1", "c2"] {
        let (base, result) = (fx03.tree_dir("t1"), fx03.result_dir(id));
        for path in tree_files(&result) {
            let read = |dir: &Path| std::fs::read(dir.join(&path)).ok();
            match merge_file(
                scratch.path(),
                read(&base).as_deref(),
                read(&acc).as_deref(),
                read(&result).as_deref(),
            ) {
                Some(bytes) => std::fs::write(acc.join(&path), bytes).unwrap(),
                None => conflicted = true,
            }
        }
    }
    if !conflicted {
        must_reject("FX03 wrong-base text merge", &fx03, "s1", &acc);
    }

    let fx04 = case("FX04");
    let correct = selftest("FX04", "s1", "correct");
    let enter = "    if (event.key === \"Enter\") doc.getElementById(\"send-signup\")?.click();\n";
    for (label, find, replace) in [
        ("FX04 gutted Enter handler", enter, ""),
        (
            "FX04 old id through querySelector",
            "doc.getElementById(\"send-signup\")?.click()",
            "doc.querySelector<HTMLButtonElement>(\"#submit\")?.click()",
        ),
        (
            "FX04 commented-out listener",
            "  email?.addEventListener(\"keydown\"",
            "  // email?.addEventListener(\"keydown\"",
        ),
    ] {
        let attack = mutate(&correct, &[("app.ts", find, replace)]);
        must_reject(label, &fx04, "s1", attack.path());
    }
    let attack = mutate(
        &correct,
        &[
            (
                "app.ts",
                "  email?.addEventListener(\"keydown\", (event) => {\n",
                "  /* email?.addEventListener(\"keydown\", (event) => {\n",
            ),
            ("app.ts", "click();\n  });\n}", "click();\n  }); */\n}"),
        ],
    );
    must_reject("FX04 block-commented listener", &fx04, "s1", attack.path());

    let fx06 = case("FX06");
    let correct = selftest("FX06", "s1", "correct");
    let attack = mutate(
        &correct,
        &[(
            "labels.ts",
            "export const EMPTY = \"Your cart is empty.\";",
            "  <<<<<<< ours\nexport const EMPTY = \"Your cart is empty.\";\n  =======\nexport const EMPTY = \"Nothing here yet.\";\n  >>>>>>> theirs",
        )],
    );
    must_reject("FX06 indented conflict markers", &fx06, "s1", attack.path());
    // In a file the contributions do change, where only the marker check can see it.
    let attack = mutate(
        &correct,
        &[(
            "cart.ts",
            "  return formatPrice(total, \"EUR\");\n",
            "  <<<<<<< ours\n  return formatPrice(total, \"EUR\");\n  =======\n  return formatPrice(total, \"EUR\");\n  >>>>>>> theirs\n",
        )],
    );
    must_reject(
        "FX06 indented markers in a changed file",
        &fx06,
        "s1",
        attack.path(),
    );
    let attack = crate::tmp_dir();
    copy_tree(&correct, attack.path());
    std::fs::remove_file(attack.path().join("labels.ts")).unwrap();
    must_reject("FX06 untouched file deleted", &fx06, "s1", attack.path());
    let attack = mutate(
        &correct,
        &[(
            "price.ts.orig",
            "",
            "export function formatPrice(amount: number): string {}\n",
        )],
    );
    must_reject("FX06 leftover file", &fx06, "s1", attack.path());
    let attack = mutate(
        &correct,
        &[(
            "labels.ts",
            "\nexport const EMPTY = \"Your cart is empty.\";",
            "",
        )],
    );
    must_reject("FX06 dropped EMPTY", &fx06, "s1", attack.path());

    let fx07 = case("FX07");
    let attack = mutate(
        &selftest("FX07", "s1", "correct"),
        &[(
            "app.html",
            "  <footer id=\"legal\">Unsubscribe at any time.</footer>\n",
            "",
        )],
    );
    must_reject("FX07 dropped footer", &fx07, "s1", attack.path());
    must_reject("FX07 relabelled X", &fx07, "s1", &fx07.result_dir("c3"));

    let hx01 = case("HX01");
    let attack = mutate(
        &selftest("HX01", "s1", "correct"),
        &[("page.html", "  <h2 id=\"heading\">Reach us</h2>\n", "")],
    );
    must_reject("HX01 dropped heading", &hx01, "s1", attack.path());

    // Ambiguous correspondence only accepts refusals.
    for outcome in [
        Outcome::Conflict,
        Outcome::ResolutionRequired,
        Outcome::Unsupported,
    ] {
        assert!(judge(&fx02, "s2", Observed::Refused(outcome)).accepted);
    }
}

// ------------------------------------------------------------ judge_scenario

#[test]
fn judge_scenario_requires_repeats_one_outcome_and_identical_trees() {
    let fx01 = case("FX01");
    let correct = selftest("FX01", "s1", "correct");
    let everything: Vec<(Vec<String>, Observed<'_>)> = fx01
        .scenario("s1")
        .orders
        .iter()
        .flat_map(|order| {
            [
                (order.clone(), Observed::Candidate(&correct)),
                (order.clone(), Observed::Candidate(&correct)),
            ]
        })
        .collect();
    assert!(judge_scenario(&fx01, "s1", &everything).accepted);

    // One run per order is not enough.
    let once: Vec<_> = everything.iter().step_by(2).cloned().collect();
    assert!(!judge_scenario(&fx01, "s1", &once).accepted);

    // A commutative scenario: a byte difference in one order is rejected.
    let reformatted = mutate(&correct, &[("app.html", "</main>\n", "</main>\n\n")]);
    assert!(
        judge(&fx01, "s1", Observed::Candidate(reformatted.path())).accepted,
        "the variant itself is acceptable"
    );
    let mut differing = everything.clone();
    differing[2].1 = Observed::Candidate(reformatted.path());
    differing[3].1 = Observed::Candidate(reformatted.path());
    assert!(!judge_scenario(&fx01, "s1", &differing).accepted);

    // A non-commutative scenario: repeats of one order must agree.
    let fx03 = case("FX03");
    let correct = selftest("FX03", "s1", "correct");
    let reformatted = mutate(&correct, &[("app.html", "</main>\n", "</main>\n\n")]);
    let order = fx03.scenario("s1").orders[0].clone();
    assert!(
        judge_scenario(
            &fx03,
            "s1",
            &[
                (order.clone(), Observed::Candidate(&correct)),
                (order.clone(), Observed::Candidate(&correct))
            ]
        )
        .accepted
    );
    assert!(
        !judge_scenario(
            &fx03,
            "s1",
            &[
                (order.clone(), Observed::Candidate(&correct)),
                (order.clone(), Observed::Candidate(reformatted.path()))
            ]
        )
        .accepted
    );
    // An undeclared order is rejected.
    let reversed: Vec<String> = order.iter().rev().cloned().collect();
    assert!(
        !judge_scenario(
            &fx03,
            "s1",
            &[
                (order.clone(), Observed::Candidate(&correct)),
                (order.clone(), Observed::Candidate(&correct)),
                (reversed, Observed::Candidate(&correct))
            ]
        )
        .accepted
    );

    // Outcomes must agree across runs, even when each is acceptable.
    let fx04 = case("FX04");
    let correct = selftest("FX04", "s1", "correct");
    let mixed: Vec<_> = fx04
        .scenario("s1")
        .orders
        .iter()
        .flat_map(|order| {
            [
                (order.clone(), Observed::Candidate(&correct)),
                (
                    order.clone(),
                    Observed::Refused(Outcome::ResolutionRequired),
                ),
            ]
        })
        .collect();
    assert!(!judge_scenario(&fx04, "s1", &mixed).accepted);

    // A subtraction has no declared order: run it twice with an empty one.
    let fx07 = case("FX07");
    let refused = [
        (Vec::new(), Observed::Refused(Outcome::InseparableSelection)),
        (Vec::new(), Observed::Refused(Outcome::InseparableSelection)),
    ];
    assert!(judge_scenario(&fx07, "s2", &refused).accepted);
    assert!(!judge_scenario(&fx07, "s2", &refused[..1]).accepted);
}

// ------------------------------------------------- inputs carry no answers

#[test]
fn inputs_hold_only_inputs_under_neutral_ids() {
    let neutral = |id: &str, prefix: char| {
        id.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
    };
    for case in all_cases() {
        let entries: Vec<String> = std::fs::read_dir(&case.dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        let mut entries = entries;
        entries.sort();
        assert_eq!(
            entries,
            ["contributions", "inputs.json", "trees"],
            "{}",
            case.case
        );
        for name in case.trees.keys() {
            assert!(neutral(name, 't'), "{}: tree {name}", case.case);
        }
        for contribution in &case.contributions {
            assert!(
                neutral(&contribution.id, 'c'),
                "{}: {}",
                case.case,
                contribution.id
            );
            if let Some(group) = &contribution.atomic_group {
                assert!(neutral(group, 'g'), "{}: group {group}", case.case);
            }
        }
        for scenario in &case.scenarios {
            assert!(
                neutral(&scenario.id, 's'),
                "{}: scenario {}",
                case.case,
                scenario.id
            );
        }
    }
}

// --------------------------------------------------------- parser details

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
    let source = "export function f(a: number, b: Map<string, number>): string {}\nf(1, g(2, 3)); f(); x.f(9);\nf((x) => x > 1, a < b, c);";
    assert_eq!(definition_arity(source, "f"), Some(2));
    assert_eq!(call_arities(source, "f"), vec![2, 0, 3]);
    assert_eq!(argument_count("a => b, c)"), 2);
}

#[test]
fn comments_are_stripped_and_selector_lookups_are_found() {
    let source = "// a(\"#x\")\nconst s = \"//not a comment\"; /* b\n c */ doc.querySelector<HTMLElement>(\"#real\"); doc.querySelectorAll('#two.x');";
    let stripped = strip_comments(source);
    assert!(stripped.contains("\"//not a comment\""));
    assert!(!stripped.contains("a(\"#x\")"));
    assert_eq!(id_refs(&stripped), ["real", "two"]);
}
