//! GitHub adapter for issue-set application. All mutations are coordinated;
//! reads use the broker's hardened GitHub subprocess environment.

use std::collections::BTreeSet;
use std::io::Write;

use serde_json::Value;

use crate::issue_set::Entry;
use crate::issue_set_apply::IssueSetBackend;

const ISSUE_SETS_POLICY_KEY: &str = "issue_workflows.issue_sets";

fn policy_allows_issue_sets(config: &str) -> Result<bool, String> {
    let value: toml::Value = config
        .parse()
        .map_err(|error| format!("target issue policy is invalid: {error}"))?;
    match value
        .get("issue_workflows")
        .and_then(|table| table.get("issue_sets"))
    {
        None => Ok(false),
        Some(value) if value.as_str() == Some("allow") => Ok(true),
        Some(value) if value.as_str() == Some("deny") => Ok(false),
        Some(_) => Err(format!(
            "{ISSUE_SETS_POLICY_KEY} must be \"allow\" or \"deny\""
        )),
    }
}

fn marker_matches(issues: &[Value], marker: &str) -> Result<Vec<u64>, String> {
    let mut matches = Vec::new();
    for issue in issues {
        if issue.get("pull_request").is_some() {
            continue;
        }
        let number = issue
            .get("number")
            .and_then(Value::as_u64)
            .filter(|number| *number > 0)
            .ok_or("GitHub issue listing omitted a valid issue number")?;
        let body = match issue.get("body") {
            Some(Value::String(body)) => body.as_str(),
            Some(Value::Null) => "",
            _ => return Err("GitHub issue listing omitted a valid body".into()),
        };
        if body.contains(marker) {
            matches.push(number);
        }
    }
    Ok(matches)
}

fn contains_database_id(issues: &[Value], expected: u64) -> Result<bool, String> {
    let mut found = false;
    for issue in issues {
        let id = issue
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or("GitHub relationship listing omitted a valid issue ID")?;
        found |= id == expected;
    }
    Ok(found)
}

pub(crate) struct GithubIssueSet<'a> {
    pub broker: &'a mut crate::Broker,
    pub session_id: i64,
    pub repository: String,
    pub reason: String,
}

impl GithubIssueSet<'_> {
    fn read(&self, path: &str) -> Result<Value, String> {
        let output = crate::operations::github_command()
            .args(["api", "--hostname", "github.com", path])
            .output()
            .map_err(|error| format!("cannot run GitHub client: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "GitHub read failed: {}",
                crate::text_redaction::redact_secrets(&String::from_utf8_lossy(&output.stderr))
            ));
        }
        if output.stdout.len() > 16 * 1024 * 1024 {
            return Err("GitHub response exceeded 16 MiB".into());
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("invalid GitHub response: {error}"))
    }

    /// Fetch all pages or fail closed rather than treat an incomplete search
    /// as proof that a marker or relationship does not exist.
    fn list(&self, path: &str) -> Result<Vec<Value>, String> {
        let mut result = Vec::new();
        let mut total_bytes = 0usize;
        let separator = if path.contains('?') { '&' } else { '?' };
        for page in 1..=100 {
            let values = self.read(&format!("{path}{separator}per_page=100&page={page}"))?;
            let values = values
                .as_array()
                .ok_or("GitHub returned a non-array page")?;
            total_bytes = total_bytes
                .saturating_add(serde_json::to_vec(values).map_err(|e| e.to_string())?.len());
            if total_bytes > 32 * 1024 * 1024 {
                return Err("GitHub pages exceeded the 32 MiB safety limit".into());
            }
            result.extend(values.iter().cloned());
            if values.len() < 100 {
                return Ok(result);
            }
        }
        Err("GitHub pagination exceeded 100 pages; cannot prove the result is complete".into())
    }

    fn write(&mut self, args: Vec<String>, scope: &str) -> Result<String, String> {
        let report = self
            .broker
            .run_coordinated_operation(crate::CoordinatedCommand {
                session_id: self.session_id,
                provider: crate::OperationProvider::Github,
                repository: Some(self.repository.clone()),
                resolved_target: None,
                scope: Some(scope.into()),
                declared_effect: Some(crate::OperationEffect::Write),
                destructive_confirmed: false,
                cross_session: None,
                ref_write_acknowledged: false,
                authorization_reason: Some(self.reason.clone()),
                args,
            })
            .map_err(|error| error.to_string())?;
        if let Some(recovery) = report.unknown_outcome_recovery() {
            return Err(recovery.to_string());
        }
        if !report.command_success {
            return Err(format!(
                "GitHub operation {} failed: {}",
                report.operation.id,
                report.stderr.trim()
            ));
        }
        Ok(report.stdout)
    }

    /// Read policy from the target's current default-branch commit, pinned by
    /// SHA so a worktree or a branch name supplied by the caller cannot grant
    /// permission. A missing or unreadable config denies.
    pub fn require_issue_sets_policy(&self) -> Result<(), String> {
        let commits = self.read(&format!("repos/{}/commits?per_page=1", self.repository))?;
        let sha = commits
            .as_array()
            .and_then(|items| items.first())
            .and_then(|item| item.get("sha"))
            .and_then(Value::as_str)
            .filter(|sha| {
                matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
            })
            .ok_or("cannot resolve the target repository default-branch commit")?;
        let path = format!(
            "repos/{}/contents/.aethyme/config.toml?ref={sha}",
            self.repository
        );
        let output = crate::operations::github_command()
            .args([
                "api",
                "--hostname",
                "github.com",
                "-H",
                "Accept: application/vnd.github.raw+json",
                &path,
            ])
            .output()
            .map_err(|error| format!("cannot read target issue policy: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "{ISSUE_SETS_POLICY_KEY} denied: target policy is absent or unreadable"
            ));
        }
        if output.stdout.len() > 1024 * 1024 {
            return Err("target issue policy exceeds 1 MiB".into());
        }
        let text =
            std::str::from_utf8(&output.stdout).map_err(|_| "target issue policy is not UTF-8")?;
        if !policy_allows_issue_sets(text)? {
            return Err(format!(
                "{ISSUE_SETS_POLICY_KEY} denied: set issue_sets = \"allow\" under [issue_workflows] on the target default branch"
            ));
        }
        Ok(())
    }

    fn issue(&self, number: u64) -> Result<Value, String> {
        let issue = self.read(&format!("repos/{}/issues/{number}", self.repository))?;
        if issue.get("pull_request").is_some()
            || issue.get("number").and_then(Value::as_u64) != Some(number)
        {
            return Err(format!("#{number} is not the requested issue"));
        }
        Ok(issue)
    }

    fn database_id(&self, number: u64) -> Result<u64, String> {
        self.issue(number)?
            .get("id")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or_else(|| format!("issue #{number} has no valid database ID"))
    }
}

impl IssueSetBackend for GithubIssueSet<'_> {
    fn validate_labels(&mut self, labels: &BTreeSet<String>) -> Result<(), String> {
        let available: BTreeSet<String> = self
            .list(&format!("repos/{}/labels", self.repository))?
            .into_iter()
            .filter_map(|value| value.get("name").and_then(Value::as_str).map(str::to_owned))
            .collect();
        for label in labels {
            if label.contains(',') {
                return Err(format!(
                    "label {label:?} cannot be passed unambiguously to GitHub CLI"
                ));
            }
            if !available.contains(label) {
                return Err(format!(
                    "label {label:?} does not exist in {}",
                    self.repository
                ));
            }
        }
        Ok(())
    }

    fn find_marker(&mut self, marker: &str) -> Result<Vec<u64>, String> {
        let issues = self.list(&format!("repos/{}/issues?state=all", self.repository))?;
        marker_matches(&issues, marker)
    }

    fn has_marker(&mut self, number: u64, marker: &str) -> Result<bool, String> {
        Ok(self
            .issue(number)?
            .get("body")
            .and_then(Value::as_str)
            .is_some_and(|body| body.contains(marker)))
    }

    fn create(&mut self, entry: &Entry, marker: &str) -> Result<u64, String> {
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        file.write_all(marker.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut args = vec![
            "issue".into(),
            "create".into(),
            "--title".into(),
            entry.title.clone(),
            "--body-file".into(),
            file.path().to_string_lossy().into_owned(),
        ];
        for label in &entry.labels {
            args.extend(["--label".into(), label.clone()]);
        }
        let output = self.write(args, "issues/set/create")?;
        let url = output
            .trim()
            .strip_prefix("https://github.com/")
            .ok_or("GitHub create returned an invalid URL")?;
        let parts: Vec<_> = url.split('/').collect();
        let repo_parts: Vec<_> = self.repository.split('/').collect();
        let number = match (parts.as_slice(), repo_parts.as_slice()) {
            ([owner, repo, "issues", number], [expected_owner, expected_repo])
                if owner.eq_ignore_ascii_case(expected_owner)
                    && repo.eq_ignore_ascii_case(expected_repo) =>
            {
                number.parse::<u64>().ok().filter(|number| *number > 0)
            }
            _ => None,
        }
        .ok_or("GitHub create returned no matching issue URL; reconcile before retrying")?;
        Ok(number)
    }

    fn update(&mut self, number: u64, entry: &Entry, body: &str) -> Result<(), String> {
        let current = self.issue(number)?;
        let marker = body
            .lines()
            .last()
            .filter(|line| line.starts_with("<!-- aethyme-issue-set:") && line.ends_with(" -->"))
            .ok_or("rendered issue body has no issue-set marker")?;
        if !current
            .get("body")
            .and_then(Value::as_str)
            .is_some_and(|body| body.contains(marker))
        {
            return Err(format!(
                "issue #{number} no longer has its issue-set marker"
            ));
        }
        let existing: BTreeSet<_> = current
            .get("labels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|label| label.get("name").and_then(Value::as_str))
            .collect();
        let missing: Vec<_> = entry
            .labels
            .iter()
            .filter(|label| !existing.contains(label.as_str()))
            .cloned()
            .collect();
        if current.get("title").and_then(Value::as_str) == Some(entry.title.as_str())
            && current.get("body").and_then(Value::as_str) == Some(body)
            && missing.is_empty()
        {
            return Ok(());
        }
        let mut file = tempfile::NamedTempFile::new().map_err(|e| e.to_string())?;
        file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        let mut args = vec![
            "issue".into(),
            "edit".into(),
            number.to_string(),
            "--title".into(),
            entry.title.clone(),
            "--body-file".into(),
            file.path().to_string_lossy().into_owned(),
        ];
        for label in missing {
            args.extend(["--add-label".into(), label]);
        }
        self.write(args, &format!("issues/{number}/set/update"))?;
        Ok(())
    }

    fn ensure_sub_issue(&mut self, parent: u64, child: u64) -> Result<(), String> {
        let path = format!("repos/{}/issues/{parent}/sub_issues", self.repository);
        let id = self.database_id(child)?;
        if contains_database_id(&self.list(&path)?, id)? {
            return Ok(());
        }
        self.write(
            vec![
                "api".into(),
                "-X".into(),
                "POST".into(),
                path,
                "-F".into(),
                format!("sub_issue_id={id}"),
            ],
            "issues/sub-issues",
        )?;
        Ok(())
    }

    fn ensure_dependency(&mut self, issue: u64, blocked_by: u64) -> Result<(), String> {
        let path = format!(
            "repos/{}/issues/{issue}/dependencies/blocked_by",
            self.repository
        );
        let id = self.database_id(blocked_by)?;
        if contains_database_id(&self.list(&path)?, id)? {
            return Ok(());
        }
        self.write(
            vec![
                "api".into(),
                "-X".into(),
                "POST".into(),
                path,
                "-F".into(),
                format!("issue_id={id}"),
            ],
            "issues/dependencies",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn issue_set_policy_defaults_to_deny_and_requires_an_explicit_allow() {
        assert!(!policy_allows_issue_sets("").unwrap());
        assert!(!policy_allows_issue_sets("[issue_workflows]\nissue_sets = \"deny\"").unwrap());
        assert!(policy_allows_issue_sets("[issue_workflows]\nissue_sets = \"allow\"").unwrap());
        assert!(policy_allows_issue_sets("[issue_workflows]\nissue_sets = true").is_err());
    }

    #[test]
    fn marker_search_fails_closed_on_incomplete_issue_data() {
        let marker = "<!-- aethyme-issue-set:x/y -->";
        assert_eq!(
            marker_matches(
                &[
                    json!({"number": 7, "body": marker}),
                    json!({"number": 8, "body": null}),
                    json!({"pull_request": {}})
                ],
                marker
            )
            .unwrap(),
            vec![7]
        );
        assert!(marker_matches(&[json!({"body": marker})], marker).is_err());
        assert!(marker_matches(&[json!({"number": 7})], marker).is_err());
    }

    #[test]
    fn relationships_use_database_ids_and_reject_incomplete_pages() {
        assert!(!contains_database_id(&[json!({"id": 10, "number": 7})], 11).unwrap());
        assert!(contains_database_id(&[json!({"id": 11, "number": 7})], 11).unwrap());
        assert!(contains_database_id(&[json!({"number": 7})], 11).is_err());
    }
}
