//! The body `broker push --pr` gives the draft pull request it opens.
//!
//! A repository with a pull request template gets that template, filled from
//! what its commits already say: the `Problem`/`Decision`/`Rationale`
//! sections under the summary heading, `Validation` under the test heading,
//! and any `Contract decision:` line both ticked in the template's checkbox
//! and kept verbatim, because the contract check reads the PR body and not
//! the commits (a body without it fails CI, and a rerun replays the old
//! body). A repository without a template keeps the plain body.

/// Where GitHub looks for a single pull request template, compared
/// case-insensitively. The `PULL_REQUEST_TEMPLATE/` directory form is not
/// here: GitHub only applies those through a `?template=` query, never by
/// default, so there is no one template to pick.
const TEMPLATE_PATHS: [&str; 6] = [
    ".github/pull_request_template.md",
    "pull_request_template.md",
    "docs/pull_request_template.md",
    ".github/pull_request_template.txt",
    "pull_request_template.txt",
    "docs/pull_request_template.txt",
];

/// Commit-message trailers that are bookkeeping, not prose: they never belong
/// in a summary section even when Git folds them into the last one.
const TRAILER_PREFIXES: [&str; 6] = [
    "co-authored-by:",
    "signed-off-by:",
    "reviewed-by:",
    "claude-session:",
    "contract decision:",
    "contract justification:",
];

/// One commit as the body builder needs it, oldest first.
pub(crate) struct BodyCommit<'a> {
    pub sha: &'a str,
    pub subject: &'a str,
    pub message: &'a str,
}

/// The repository's single default pull request template among `paths`
/// (tracked paths at the base commit), if it has one.
pub(crate) fn template_path(paths: &[String]) -> Option<&str> {
    TEMPLATE_PATHS.iter().find_map(|wanted| {
        paths
            .iter()
            .find(|path| path.eq_ignore_ascii_case(wanted))
            .map(String::as_str)
    })
}

/// Whether a workflow file skips draft pull requests: it gates on
/// `github.event.pull_request.draft`. A text match is enough here; the answer
/// only decides whether `push --pr` adds a one-line note.
pub(crate) fn workflow_skips_drafts(workflow: &str) -> bool {
    workflow.contains("pull_request.draft")
}

/// The draft PR body. `template` is the repository's template text, if any.
pub(crate) fn compose(
    task: Option<&str>,
    commits: &[BodyCommit<'_>],
    template: Option<&str>,
) -> String {
    let contract_lines = contract_lines(commits);
    let mut body = match template {
        Some(template) => fill_template(template, task, commits, &contract_lines),
        None => {
            let mut body = String::new();
            if let Some(task) = task {
                body.push_str(task.trim());
                body.push_str("\n\n");
            }
            if !contract_lines.is_empty() {
                body.push_str(&contract_lines.join("\n"));
                body.push_str("\n\n");
            }
            body
        }
    };
    body.push_str("Commits:\n");
    for commit in commits.iter().rev() {
        body.push_str(&format!(
            "- {} {}\n",
            &commit.sha[..commit.sha.len().min(10)],
            commit.subject
        ));
    }
    body.push_str("\nOpened by aethyme broker push as a draft.\n");
    body
}

/// `Contract decision:` and `Contract justification:` lines from every
/// commit, verbatim and de-duplicated, in commit order.
fn contract_lines(commits: &[BodyCommit<'_>]) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for commit in commits {
        for line in commit.message.lines() {
            let line = line.trim();
            let lower = line.to_ascii_lowercase();
            if (lower.starts_with("contract decision:")
                || lower.starts_with("contract justification:"))
                && !lines.iter().any(|seen| seen == line)
            {
                lines.push(line.to_string());
            }
        }
    }
    lines
}

fn without_trailers(text: &str) -> String {
    text.lines()
        .filter(|line| {
            let lower = line.trim().to_ascii_lowercase();
            !TRAILER_PREFIXES
                .iter()
                .any(|prefix| lower.starts_with(prefix))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

/// The summary prose and the validation prose the commits carry.
fn commit_prose(task: Option<&str>, commits: &[BodyCommit<'_>]) -> (String, String) {
    let mut summary: Vec<String> = Vec::new();
    let mut validation: Vec<String> = Vec::new();
    if let Some(task) = task.map(str::trim).filter(|task| !task.is_empty()) {
        summary.push(task.to_string());
    }
    let several = commits.len() > 1;
    for commit in commits {
        let sections = aethyme_enhance::hygiene::commit_message_sections(commit.message);
        let mut parts: Vec<String> = Vec::new();
        for (name, text) in &sections {
            let text = without_trailers(text);
            if text.is_empty() {
                continue;
            }
            if name == "Validation" {
                validation.push(if several {
                    format!("{}: {text}", commit.subject)
                } else {
                    text
                });
            } else if matches!(name.as_str(), "Problem" | "Decision" | "Rationale") {
                parts.push(format!("**{name}:** {text}"));
            }
        }
        if parts.is_empty() {
            // A sectionless commit still names what it did, unless it is the
            // only commit and the task already says so.
            if several || summary.is_empty() {
                summary.push(format!("- {}", commit.subject));
            }
        } else if several {
            summary.push(format!("**{}**\n\n{}", commit.subject, parts.join("\n\n")));
        } else {
            summary.push(parts.join("\n\n"));
        }
    }
    (summary.join("\n\n"), validation.join("\n\n"))
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    Summary,
    Validation,
    Contract,
}

fn heading_slot(line: &str) -> Option<Slot> {
    let text = line.trim_start().strip_prefix('#')?;
    let text = text.trim_start_matches('#').trim().to_ascii_lowercase();
    if text.contains("contract") {
        Some(Slot::Contract)
    } else if [
        "summary",
        "description",
        "overview",
        "what",
        "why",
        "changes",
    ]
    .iter()
    .any(|word| text.contains(word))
    {
        Some(Slot::Summary)
    } else if ["test", "validation", "verification"]
        .iter()
        .any(|word| text.contains(word))
    {
        Some(Slot::Validation)
    } else {
        None
    }
}

/// Tick the template's `- [ ] **<label>**` checkbox for each declared
/// contract decision, so a template-driven contract check sees it the same
/// way the verbatim line already says it.
fn tick_contract_boxes(template: &str, contract_lines: &[String]) -> String {
    let labels: Vec<String> = contract_lines
        .iter()
        .filter_map(|line| {
            let rest = line.get("contract decision:".len()..)?;
            line.to_ascii_lowercase()
                .starts_with("contract decision:")
                .then(|| {
                    rest.trim()
                        .split(|c: char| !(c.is_ascii_alphabetic() || c == '-'))
                        .next()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                })
        })
        .filter(|label| !label.is_empty())
        .collect();
    template
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            let ticked = labels.iter().any(|label| {
                trimmed
                    .to_ascii_lowercase()
                    .starts_with(&format!("- [ ] **{label}**"))
            });
            if ticked {
                line.replacen("- [ ]", "- [x]", 1)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn fill_template(
    template: &str,
    task: Option<&str>,
    commits: &[BodyCommit<'_>],
    contract_lines: &[String],
) -> String {
    let (summary, validation) = commit_prose(task, commits);
    let template = tick_contract_boxes(template, contract_lines);
    let contract = contract_lines.join("\n");
    let lines: Vec<&str> = template.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut placed = [false; 3];
    let mut index = 0;
    while index < lines.len() {
        out.push(lines[index].to_string());
        let slot = heading_slot(lines[index]);
        index += 1;
        let Some(slot) = slot else { continue };
        let (text, done) = match slot {
            Slot::Summary => (&summary, &mut placed[0]),
            Slot::Validation => (&validation, &mut placed[1]),
            Slot::Contract => (&contract, &mut placed[2]),
        };
        if *done || text.is_empty() {
            continue;
        }
        // Past the blank lines and the guidance comment that follow the
        // heading, so the filled text reads after the instructions.
        while index < lines.len() && lines[index].trim().is_empty() {
            out.push(lines[index].to_string());
            index += 1;
        }
        if index < lines.len() && lines[index].trim_start().starts_with("<!--") {
            while index < lines.len() {
                let closes = lines[index].contains("-->");
                out.push(lines[index].to_string());
                index += 1;
                if closes {
                    break;
                }
            }
            out.push(String::new());
        }
        out.push(text.clone());
        out.push(String::new());
        *done = true;
    }
    let mut body = out.join("\n").trim_end().to_string();
    body.push_str("\n\n");
    // Text with no heading to sit under still reaches the reader, and the
    // contract line still reaches the check.
    let mut prefix = String::new();
    if !placed[0] && !summary.is_empty() {
        prefix.push_str(&summary);
        prefix.push_str("\n\n");
    }
    if !placed[1] && !validation.is_empty() {
        body.push_str(&format!("Validation: {validation}\n\n"));
    }
    if !placed[2] && !contract.is_empty() {
        body.push_str(&contract);
        body.push_str("\n\n");
    }
    format!("{prefix}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn commit<'a>(subject: &'a str, message: &'a str) -> BodyCommit<'a> {
        BodyCommit {
            sha: SHA,
            subject,
            message,
        }
    }

    const MESSAGE: &str = "fix(broker): keep the sweep off the hot path\n\n\
        Problem: every command paid for a full sweep.\n\n\
        Decision: bound it to 250 ms.\n\n\
        Rationale: the sweep is advisory.\n\n\
        Validation: cargo test -p aethyme-broker gc passes.\n\n\
        Contract decision: none\n\
        Co-Authored-By: Someone <someone@example.com>\n";

    const TEMPLATE: &str = "<!-- top comment -->\n\n## Summary\n\n<!-- 1-3 sentences -->\n\n\
        ## Contract\n\n<!-- pick one -->\n\n- [ ] **none** — internal change.\n\
        - [ ] **introduce** — adds a new entry point.\n\n## Test plan\n\n- [ ] `cargo test`\n";

    #[test]
    fn without_a_template_the_body_is_unchanged_but_for_the_contract_line() {
        let plain = "fix(x): y\n\nJust a body.\n";
        let body = compose(Some("Do the thing"), &[commit("fix(x): y", plain)], None);
        assert_eq!(
            body,
            "Do the thing\n\nCommits:\n- 0123456789 fix(x): y\n\nOpened by aethyme broker push as a draft.\n"
        );
        let body = compose(Some("Do the thing"), &[commit("fix(x): y", MESSAGE)], None);
        assert!(body.contains("\nContract decision: none\n"), "{body}");
    }

    #[test]
    fn a_template_is_filled_from_the_commit_sections() {
        let body = compose(
            None,
            &[commit(
                "fix(broker): keep the sweep off the hot path",
                MESSAGE,
            )],
            Some(TEMPLATE),
        );
        let summary = body.find("## Summary").unwrap();
        let problem = body
            .find("**Problem:** every command paid for a full sweep.")
            .expect(&body);
        let contract = body.find("## Contract").unwrap();
        assert!(summary < problem && problem < contract, "{body}");
        assert!(
            body.find("<!-- 1-3 sentences -->").unwrap() < problem,
            "{body}"
        );
        let test_plan = body.find("## Test plan").unwrap();
        assert!(
            body[test_plan..].contains("cargo test -p aethyme-broker gc passes."),
            "{body}"
        );
        assert!(
            !body.contains("Co-Authored-By"),
            "trailers stay out of the prose: {body}"
        );
        assert!(
            body.ends_with("Opened by aethyme broker push as a draft.\n"),
            "{body}"
        );
    }

    #[test]
    fn the_contract_decision_reaches_the_contract_check() {
        let body = compose(None, &[commit("fix(broker): s", MESSAGE)], Some(TEMPLATE));
        assert!(body.contains("- [x] **none** — internal change."), "{body}");
        assert!(body.contains("- [ ] **introduce**"), "{body}");
        assert_eq!(
            crate::contract_check::parse_contract_decision(&body),
            Some(crate::contract_check::Decision::None)
        );
        let bare = compose(
            None,
            &[commit("fix(broker): s", MESSAGE)],
            Some("Describe the change.\n"),
        );
        assert_eq!(
            crate::contract_check::parse_contract_decision(&bare),
            Some(crate::contract_check::Decision::None),
            "a template without a contract heading still carries the line: {bare}"
        );
    }

    #[test]
    fn the_default_template_is_found_case_insensitively() {
        let paths = vec![
            "README.md".to_string(),
            ".github/PULL_REQUEST_TEMPLATE.md".to_string(),
            "docs/pull_request_template.md".to_string(),
        ];
        assert_eq!(
            template_path(&paths),
            Some(".github/PULL_REQUEST_TEMPLATE.md")
        );
        assert_eq!(template_path(&["src/lib.rs".to_string()]), None);
        assert_eq!(
            template_path(&[".github/PULL_REQUEST_TEMPLATE/bug.md".to_string()]),
            None,
            "a template directory has no default template"
        );
    }

    #[test]
    fn draft_skipping_workflows_are_recognised() {
        assert!(workflow_skips_drafts(
            "jobs:\n  build:\n    if: github.event.pull_request.draft == false\n"
        ));
        assert!(!workflow_skips_drafts("on: [push, pull_request]\n"));
    }
}
