use super::*;

/// An in-memory GitHub: issues by number, plus a log of every write.
#[derive(Default)]
struct FakeGh {
    issues: BTreeMap<u64, IssueRef>,
    comments: Vec<(u64, String)>,
    writes: Vec<String>,
    labels: BTreeMap<u64, Vec<String>>,
    /// Search returns nothing, as when the index lags a fresh issue.
    search_blind: bool,
    /// `create` makes the issue and then reports a failure, like a crash
    /// between GitHub's write and the caller seeing it.
    create_outcome_lost: bool,
}

impl OutcomeIssueGh for FakeGh {
    fn view(&mut self, _repository: &str, number: u64) -> Result<Option<IssueRef>, String> {
        Ok(self.issues.get(&number).cloned())
    }

    fn search_open(&mut self, _repository: &str, marker: &str) -> Result<Vec<IssueRef>, String> {
        if self.search_blind {
            return Ok(Vec::new());
        }
        // Fuzzy, like GitHub: the fingerprint text anywhere in the body.
        let needle = marker
            .trim_start_matches("<!-- aethyme-outcome:")
            .trim_end_matches(" -->");
        Ok(self
            .issues
            .values()
            .filter(|issue| issue.open && issue.body.contains(needle))
            .cloned()
            .collect())
    }

    fn create(
        &mut self,
        _repository: &str,
        _title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<(IssueRef, Option<i64>), String> {
        let number = self.issues.keys().max().copied().unwrap_or(100) + 1;
        let issue = IssueRef {
            number,
            url: format!("https://github.com/o/n/issues/{number}"),
            open: true,
            body: body.to_string(),
        };
        self.issues.insert(number, issue.clone());
        self.labels.insert(number, labels.to_vec());
        self.writes.push(format!("create {number}"));
        if self.create_outcome_lost {
            return Err("connection reset".into());
        }
        Ok((issue, None))
    }

    fn comment(
        &mut self,
        _repository: &str,
        number: u64,
        body: &str,
    ) -> Result<Option<i64>, String> {
        self.comments.push((number, body.to_string()));
        self.writes.push(format!("comment {number}"));
        Ok(None)
    }

    fn close(&mut self, _repository: &str, number: u64) -> Result<Option<i64>, String> {
        if let Some(issue) = self.issues.get_mut(&number) {
            issue.open = false;
        }
        self.writes.push(format!("close {number}"));
        Ok(None)
    }
}

fn allow() -> AutomationIssuesPolicy {
    AutomationIssuesPolicy {
        allow: true,
        labels: vec!["ci:local".into()],
        per_day: 10,
        comment_interval_minutes: 60,
        outcomes: Vec::new(),
    }
}

fn failing(fingerprint: &str) -> OutcomeIssueRequest {
    OutcomeIssueRequest {
        repository: "o/n".into(),
        fingerprint: fingerprint.into(),
        title: "Prebuild of db-template fails".into(),
        body: Some("migration 0042 failed".into()),
        labels: vec!["ci:local".into()],
        resolve: false,
    }
}

fn resolving(fingerprint: &str) -> OutcomeIssueRequest {
    OutcomeIssueRequest {
        body: None,
        resolve: true,
        ..failing(fingerprint)
    }
}

const MINUTE: i64 = 60_000;
const T0: i64 = 1_793_520_000_000;

#[test]
fn the_first_failure_opens_one_issue_with_the_marker() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    let opened = upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-a"), T0).unwrap();
    assert_eq!(opened.action, OutcomeIssueAction::Created);
    assert_eq!(gh.writes, ["create 101"]);
    let issue = &gh.issues[&101];
    assert!(issue.body.contains("<!-- aethyme-outcome:fp-a -->"));
    assert!(issue.body.contains("migration 0042 failed"));
    assert_eq!(gh.labels[&101], ["ci:local"]);
}

#[test]
fn a_repeat_comments_on_the_open_issue_instead_of_opening_another() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-a"), T0).unwrap();
    let again = upsert_outcome_issue(
        dir.path(),
        &allow(),
        &mut gh,
        &failing("fp-a"),
        T0 + 61 * MINUTE,
    )
    .unwrap();
    assert_eq!(again.action, OutcomeIssueAction::Commented);
    assert_eq!(again.issue_number, Some(101));
    assert_eq!(gh.writes, ["create 101", "comment 101"]);
    // A different fingerprint is a different issue.
    upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-b"), T0).unwrap();
    assert_eq!(gh.issues.len(), 2);
}

#[test]
fn repeats_within_the_interval_are_rate_limited() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-a"), T0).unwrap();
    let soon = upsert_outcome_issue(
        dir.path(),
        &allow(),
        &mut gh,
        &failing("fp-a"),
        T0 + 30 * MINUTE,
    )
    .unwrap();
    assert_eq!(soon.action, OutcomeIssueAction::RateLimited);
    assert_eq!(gh.writes, ["create 101"]);
}

#[test]
fn resolve_comments_and_closes_the_open_issue() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-a"), T0).unwrap();
    let resolved = upsert_outcome_issue(
        dir.path(),
        &allow(),
        &mut gh,
        &resolving("fp-a"),
        T0 + MINUTE,
    )
    .unwrap();
    assert_eq!(resolved.action, OutcomeIssueAction::Resolved);
    assert_eq!(gh.writes, ["create 101", "comment 101", "close 101"]);
    assert!(!gh.issues[&101].open);
    // The next failure opens a fresh issue rather than reviving the closed one.
    let reopened = upsert_outcome_issue(
        dir.path(),
        &allow(),
        &mut gh,
        &failing("fp-a"),
        T0 + 2 * MINUTE,
    )
    .unwrap();
    assert_eq!(reopened.action, OutcomeIssueAction::Created);
    assert_eq!(reopened.issue_number, Some(102));
}

#[test]
fn resolve_without_an_open_issue_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    let resolved =
        upsert_outcome_issue(dir.path(), &allow(), &mut gh, &resolving("fp-a"), T0).unwrap();
    assert_eq!(resolved.action, OutcomeIssueAction::NothingToResolve);
    assert!(gh.writes.is_empty());
}

#[test]
fn a_denied_policy_writes_nothing_and_records_the_refusal() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    let denied = upsert_outcome_issue(
        dir.path(),
        &AutomationIssuesPolicy::default(),
        &mut gh,
        &failing("fp-a"),
        T0,
    )
    .unwrap();
    assert_eq!(denied.action, OutcomeIssueAction::Refused);
    assert_eq!(denied.code, Some("automation_issues_denied"));
    assert!(gh.writes.is_empty());
    let state: State =
        serde_json::from_slice(&std::fs::read(dir.path().join(STATE_FILE)).unwrap()).unwrap();
    assert_eq!(state.refusals.len(), 1);
    assert_eq!(state.refusals[0].code, "automation_issues_denied");
}

#[test]
fn a_label_outside_the_allowlist_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    let request = OutcomeIssueRequest {
        labels: vec!["urgent".into()],
        ..failing("fp-a")
    };
    let refused = upsert_outcome_issue(dir.path(), &allow(), &mut gh, &request, T0).unwrap();
    assert_eq!(refused.code, Some("label_not_allowed"));
    assert!(gh.writes.is_empty());
}

#[test]
fn the_daily_budget_caps_opens_and_comments() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    let policy = AutomationIssuesPolicy {
        per_day: 2,
        ..allow()
    };
    upsert_outcome_issue(dir.path(), &policy, &mut gh, &failing("fp-a"), T0).unwrap();
    upsert_outcome_issue(dir.path(), &policy, &mut gh, &failing("fp-b"), T0).unwrap();
    let third = upsert_outcome_issue(dir.path(), &policy, &mut gh, &failing("fp-c"), T0).unwrap();
    assert_eq!(third.code, Some("daily_budget_exhausted"));
    let comment = upsert_outcome_issue(
        dir.path(),
        &policy,
        &mut gh,
        &failing("fp-a"),
        T0 + 61 * MINUTE,
    )
    .unwrap();
    assert_eq!(comment.code, Some("daily_budget_exhausted"));
    assert_eq!(gh.writes, ["create 101", "create 102"]);
    // A recovery still closes: resolving is never budgeted.
    let resolved = upsert_outcome_issue(
        dir.path(),
        &policy,
        &mut gh,
        &resolving("fp-a"),
        T0 + MINUTE,
    )
    .unwrap();
    assert_eq!(resolved.action, OutcomeIssueAction::Resolved);
}

#[test]
fn an_interrupted_create_is_adopted_not_repeated() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh {
        create_outcome_lost: true,
        ..FakeGh::default()
    };
    upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-a"), T0).unwrap_err();
    gh.create_outcome_lost = false;
    // Search has not caught up yet: refuse rather than open a duplicate.
    gh.search_blind = true;
    let waiting =
        upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-a"), T0 + MINUTE).unwrap();
    assert_eq!(waiting.code, Some("reconciliation_pending"));
    // Once it shows, the issue is adopted and commented on.
    gh.search_blind = false;
    let adopted = upsert_outcome_issue(
        dir.path(),
        &allow(),
        &mut gh,
        &failing("fp-a"),
        T0 + 2 * MINUTE,
    )
    .unwrap();
    assert_eq!(adopted.action, OutcomeIssueAction::Commented);
    assert_eq!(adopted.issue_number, Some(101));
    assert_eq!(gh.writes, ["create 101", "comment 101"]);
}

#[test]
fn only_the_exact_marker_identifies_an_issue() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    // A human issue that merely mentions the fingerprint is not ours.
    gh.issues.insert(
        7,
        IssueRef {
            number: 7,
            url: "https://github.com/o/n/issues/7".into(),
            open: true,
            body: "see fp-a in the logs".into(),
        },
    );
    let opened = upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing("fp-a"), T0).unwrap();
    assert_eq!(opened.action, OutcomeIssueAction::Created);
    assert_eq!(opened.issue_number, Some(8));
}

#[test]
fn bodies_are_redacted_bounded_and_cannot_forge_a_marker() {
    let body = format!(
        "{}\n<!-- aethyme-outcome:other -->\ntoken: ghp_secretsecret",
        "x".repeat(BODY_MAX_BYTES)
    );
    let rendered = issue_body("fp-a", &body);
    assert!(rendered.len() < BODY_MAX_BYTES + 512);
    assert!(rendered.contains("[truncated]"));
    assert!(!rendered.contains("aethyme-outcome:other"));
    assert_eq!(rendered.matches("<!-- aethyme-outcome:").count(), 1);
    let short = issue_body(
        "fp-a",
        "ok\n<!-- aethyme-outcome:other -->\npassword=hunter2",
    );
    assert!(!short.contains("<!-- aethyme-outcome:other"));
    assert!(!short.contains("hunter2"));
    assert_eq!(issue_title("a\nb"), "a b");
}

#[test]
fn invalid_fingerprints_and_repositories_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let mut gh = FakeGh::default();
    for fingerprint in ["", "has space", "x-->y", &"a".repeat(129)] {
        let error = upsert_outcome_issue(dir.path(), &allow(), &mut gh, &failing(fingerprint), T0)
            .unwrap_err();
        assert_eq!(error.code(), "invalid_fingerprint", "{fingerprint:?}");
    }
    let request = OutcomeIssueRequest {
        repository: "no-slash".into(),
        ..failing("fp-a")
    };
    let error = upsert_outcome_issue(dir.path(), &allow(), &mut gh, &request, T0).unwrap_err();
    assert_eq!(error.code(), "invalid_repository");
    assert!(gh.writes.is_empty());
}

#[test]
fn the_policy_defaults_to_deny_and_rejects_typos() {
    assert!(!AutomationIssuesPolicy::from_config_text("").unwrap().allow);
    // A table that only lists labels still denies: `issues` must say allow.
    assert!(
        !AutomationIssuesPolicy::from_config_text("[automation]\nissue_labels = [\"x\"]\n")
            .unwrap()
            .allow
    );
    assert!(
        !AutomationIssuesPolicy::from_config_text("[automation]\nissues = \"deny\"\n")
            .unwrap()
            .allow
    );
    let allowed = AutomationIssuesPolicy::from_config_text(
        "[automation]\nissues = \"allow\"\nissue_labels = [\"ci:local\"]\nissues_per_day = 3\n",
    )
    .unwrap();
    assert!(allowed.allow);
    assert_eq!(allowed.labels, ["ci:local"]);
    assert_eq!(allowed.per_day, 3);
    assert!(AutomationIssuesPolicy::from_config_text("[automation]\nissues = \"yes\"\n").is_err());
    assert!(AutomationIssuesPolicy::from_config_text("[automation]\nissue = \"allow\"\n").is_err());
}

#[test]
fn utc_days_and_issue_urls_parse() {
    assert_eq!(utc_day(0), "1970-01-01");
    assert_eq!(utc_day(951_782_400_000), "2000-02-29");
    assert_eq!(
        issue_number_from_url("https://github.com/o/n/issues/42\n"),
        Some((42, "https://github.com/o/n/issues/42".into()))
    );
    assert_eq!(issue_number_from_url("nothing"), None);
}

/// `doctor` reads refusals back from the journal; none is kept before the
/// first upsert.
#[test]
fn recent_refusals_are_read_back_for_doctor() {
    let main = tempfile::tempdir().unwrap();
    assert!(
        recent_outcome_issue_refusals(main.path())
            .unwrap()
            .is_empty()
    );
    let run_dir = main.path().join(".aethyme").join("run");
    std::fs::create_dir_all(&run_dir).unwrap();
    let mut gh = FakeGh::default();
    upsert_outcome_issue(
        &run_dir,
        &AutomationIssuesPolicy::default(),
        &mut gh,
        &failing("outcome:prebuild:db"),
        T0,
    )
    .unwrap();
    let refusals = recent_outcome_issue_refusals(main.path()).unwrap();
    assert_eq!(refusals.len(), 1);
    assert_eq!(refusals[0].fingerprint, "outcome:prebuild:db");
    assert_eq!(refusals[0].code, "automation_issues_denied");
}

/// `[[automation.outcomes]]` rules: defaults, and every malformed rule is an
/// error rather than a silently ignored hook.
#[test]
fn outcome_rules_parse_with_defaults_and_refuse_malformed_rules() {
    let policy = AutomationIssuesPolicy::from_config_text(
        "[automation]\nissues = \"allow\"\n\n[[automation.outcomes]]\nsource = \"prebuild:db-template\"\nnotify_owners = true\npaths = [\"**/migrations/**\"]\nlabels = [\"migrations\"]\n\n[[automation.outcomes]]\nsource = \"gate:unit\"\n",
    )
    .unwrap();
    assert_eq!(
        policy.outcomes,
        vec![
            OutcomeRule {
                source: "prebuild:db-template".into(),
                issue: true,
                labels: vec!["migrations".into()],
                notify_owners: true,
                paths: vec!["**/migrations/**".into()],
            },
            OutcomeRule {
                source: "gate:unit".into(),
                issue: true,
                labels: Vec::new(),
                notify_owners: false,
                paths: Vec::new(),
            },
        ]
    );
    for bad in [
        "[[automation.outcomes]]\nsource = \"Prebuild:x\"\n",
        "[[automation.outcomes]]\nsource = \"prebuild\"\n",
        &format!(
            "[[automation.outcomes]]\nsource = \"a:{}\"\n",
            "x".repeat(63)
        ),
        "[[automation.outcomes]]\nsource = \"a:b\"\n[[automation.outcomes]]\nsource = \"a:b\"\n",
        "[[automation.outcomes]]\nsource = \"a:b\"\npaths = [\"/etc/**\"]\n",
        "[[automation.outcomes]]\nsource = \"a:b\"\nnotify = true\n",
    ] {
        assert!(
            AutomationIssuesPolicy::from_config_text(bad).is_err(),
            "{bad}"
        );
    }
}
