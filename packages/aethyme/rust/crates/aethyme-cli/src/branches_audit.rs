//! `aethyme repo branches audit`: a read-only answer to "which of my local
//! branches still hold work that exists nowhere else?".
//!
//! Comparing tip SHAs alone overstates what is at risk: a branch whose PR
//! was squash-merged, or rebased before merging, has a tip the remote never
//! saw although its content landed. Each branch is therefore classified with
//! the evidence that decided it:
//!
//! - `on-remote`: the tip equals a remote branch tip;
//! - `contained`: the tip is an ancestor of a remote branch tip;
//! - `merged-via-pr`: every commit not reachable from the remote is
//!   patch-equivalent (verbatim patch ids) to a commit on the default branch or on
//!   a merged PR's head, or the branch's whole diff matches the PR's squash
//!   commit or the PR head's tree;
//! - `local-only`: some commits match nothing on the remote; they are listed.
//!
//! Remote tips come from `git ls-remote` (no refs move); when that fails the
//! remote-tracking refs are used and the report says so. A remote tip whose
//! objects are not in the local repository cannot prove containment, so the
//! report counts those refs: a `git fetch` may turn a `local-only` verdict
//! into `contained`. Merged PRs are looked up with `gh` only for GitHub
//! remotes and only for branches the local evidence did not settle.
//!
//! Nothing here writes: no fetch, no deletion, and `git status` runs with
//! optional locks disabled so not even the index is refreshed.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

/// The broker's integration branch; deleting it breaks the broker.
const INTEGRATION_BRANCH: &str = "aethyme/integration";
/// Branches with more unpushed commits than this are not compared patch by
/// patch; they are reported `local-only` with the count.
const MAX_COMPARED_COMMITS: usize = 500;
/// How many uncovered commits a `local-only` verdict lists.
const MAX_LISTED_COMMITS: usize = 50;

const USAGE: &str =
    "aethyme repo branches audit [<repo_path>] [--remote <name>] [--no-gh] [--json-output]";

/// Run `repo branches <args>`; returns the process exit code.
pub fn run(args: &[String]) -> u8 {
    let Some(action) = args.first() else {
        eprintln!("Error: missing branches action. Usage: {USAGE}");
        return 2;
    };
    if action != "audit" {
        eprintln!("Error: unsupported branches action: {action}. Usage: {USAGE}");
        return 2;
    }
    let options = match Options::parse(&args[1..]) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("Error: {message}. Usage: {USAGE}");
            return 2;
        }
    };
    match audit(&options) {
        Ok(report) => {
            if options.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_default()
                );
            } else {
                print!("{}", render(&report));
            }
            0
        }
        Err(message) => {
            eprintln!("Error: {message}");
            1
        }
    }
}

struct Options {
    repo: PathBuf,
    remote: String,
    use_gh: bool,
    json: bool,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self, String> {
        let mut options = Options {
            repo: PathBuf::from("."),
            remote: "origin".to_string(),
            use_gh: true,
            json: false,
        };
        let mut repo_given = false;
        let mut index = 0;
        while index < args.len() {
            let arg = args[index].as_str();
            match arg {
                "--json-output" | "--json" => options.json = true,
                "--no-gh" => options.use_gh = false,
                "--remote" => {
                    index += 1;
                    options.remote = args
                        .get(index)
                        .ok_or("option '--remote' requires a value")?
                        .clone();
                }
                _ if arg.starts_with("--remote=") => {
                    options.remote = arg["--remote=".len()..].to_string();
                }
                _ if arg.starts_with('-') => return Err(format!("no such option: {arg}")),
                _ if !repo_given => {
                    options.repo = PathBuf::from(arg);
                    repo_given = true;
                }
                _ => return Err(format!("unexpected argument: {arg}")),
            }
            index += 1;
        }
        Ok(options)
    }
}

// ── git plumbing ────────────────────────────────────────────────────────────

fn git_command(repo: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        // Read-only: never take the index lock to refresh stat data.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null());
    command
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let output = git_command(repo)
        .args(args)
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn git_succeeds(repo: &Path, args: &[&str]) -> bool {
    git_command(repo)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn git_with_input(repo: &Path, args: &[&str], input: &[u8]) -> Result<String, String> {
    let mut child = git_command(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not run git: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("could not feed git {}: {error}", args.join(" ")))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("could not wait for git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Verbatim patch id of the change `from..to`, or `None` for an empty change.
/// `--verbatim` keeps whitespace, so a whitespace-only difference is never
/// read as the same patch (and the branch never as safe to delete).
fn patch_id(repo: &Path, from: &str, to: &str) -> Option<String> {
    // Plumbing `diff-tree`, not porcelain `diff`: wrappers that rewrite
    // `git diff` output must not change the patch being identified.
    let patch = git(repo, &["diff-tree", "-p", "--no-color", from, to]).ok()?;
    if patch.trim().is_empty() {
        return None;
    }
    let ids = git_with_input(repo, &["patch-id", "--verbatim"], patch.as_bytes()).ok()?;
    ids.split_whitespace().next().map(str::to_string)
}

fn commit_present(repo: &Path, sha: &str) -> bool {
    git_succeeds(repo, &["cat-file", "-e", &format!("{sha}^{{commit}}")])
}

// ── remote state ────────────────────────────────────────────────────────────

struct RemoteState {
    source: String,
    warning: Option<String>,
    default_branch: Option<String>,
    /// Remote branch name -> tip SHA.
    heads: BTreeMap<String, String>,
    /// Tips whose objects exist locally, so ancestry can be tested.
    present: Vec<String>,
    missing_locally: Vec<String>,
}

fn remote_state(repo: &Path, remote: &str) -> Result<RemoteState, String> {
    let mut heads = BTreeMap::new();
    let mut default_branch = None;
    let (source, warning) = match git(
        repo,
        &["ls-remote", "--symref", remote, "HEAD", "refs/heads/*"],
    ) {
        Ok(listing) => {
            for line in listing.lines() {
                if let Some(symref) = line.strip_prefix("ref: ") {
                    if let Some((target, "HEAD")) = symref.split_once('\t') {
                        default_branch = target.strip_prefix("refs/heads/").map(str::to_string);
                    }
                } else if let Some((sha, name)) = line.split_once('\t')
                    && let Some(branch) = name.strip_prefix("refs/heads/")
                {
                    heads.insert(branch.to_string(), sha.to_string());
                }
            }
            (format!("git ls-remote {remote}"), None)
        }
        Err(error) => {
            let prefix = format!("refs/remotes/{remote}/");
            let listing = git(
                repo,
                &[
                    "for-each-ref",
                    "--format=%(refname)%00%(objectname)",
                    &prefix,
                ],
            )?;
            for line in listing.lines() {
                if let Some((name, sha)) = line.split_once('\0')
                    && let Some(branch) = name.strip_prefix(&prefix)
                    && branch != "HEAD"
                {
                    heads.insert(branch.to_string(), sha.to_string());
                }
            }
            default_branch = git(
                repo,
                &[
                    "symbolic-ref",
                    "--short",
                    &format!("refs/remotes/{remote}/HEAD"),
                ],
            )
            .ok()
            .and_then(|target| {
                target
                    .trim()
                    .strip_prefix(&format!("{remote}/"))
                    .map(str::to_string)
            });
            (
                format!("remote-tracking refs refs/remotes/{remote}/* (may be stale)"),
                Some(format!(
                    "ls-remote failed, so remote tips may be stale: {error}"
                )),
            )
        }
    };
    if default_branch.is_none() {
        default_branch = ["main", "master"]
            .into_iter()
            .find(|name| heads.contains_key(*name))
            .map(str::to_string);
    }
    let unique: BTreeSet<&String> = heads.values().collect();
    let (present, missing_locally): (Vec<String>, Vec<String>) = unique
        .into_iter()
        .cloned()
        .partition(|sha| commit_present(repo, sha));
    Ok(RemoteState {
        source,
        warning,
        default_branch,
        heads,
        present,
        missing_locally,
    })
}

/// `owner/name` when the remote's configured URL is a GitHub URL.
fn github_slug(repo: &Path, remote: &str) -> Option<String> {
    // The raw configured URL, before any `insteadOf` rewriting.
    let url = git(repo, &["config", "--get", &format!("remote.{remote}.url")]).ok()?;
    let url = url.trim();
    let rest = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("http://github.com/"))
        .or_else(|| url.strip_prefix("git@github.com:"))
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let mut parts = rest.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => {
            Some(format!("{owner}/{name}"))
        }
        _ => None,
    }
}

struct MergedPr {
    number: u64,
    url: String,
    merged_at: String,
    head: String,
    merge_commit: Option<String>,
}

fn merged_prs(slug: &str, branch: &str) -> Result<Vec<MergedPr>, String> {
    let output = Command::new("gh")
        .args([
            "pr",
            "list",
            "-R",
            slug,
            "--head",
            branch,
            "--state",
            "merged",
            "--limit",
            "10",
            "--json",
            "number,url,mergedAt,headRefOid,mergeCommit",
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("could not run gh: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "gh pr list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let parsed: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("gh pr list returned invalid JSON: {error}"))?;
    Ok(parsed
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    Some(MergedPr {
                        number: item["number"].as_u64()?,
                        url: item["url"].as_str().unwrap_or_default().to_string(),
                        merged_at: item["mergedAt"].as_str().unwrap_or_default().to_string(),
                        head: item["headRefOid"].as_str()?.to_string(),
                        merge_commit: item["mergeCommit"]["oid"].as_str().map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

// ── classification ──────────────────────────────────────────────────────────

struct LocalBranch {
    name: String,
    tip: String,
    committed_at: String,
}

fn local_branches(repo: &Path) -> Result<Vec<LocalBranch>, String> {
    let listing = git(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(objectname)%00%(committerdate:iso-strict)",
            "refs/heads",
        ],
    )?;
    Ok(listing
        .lines()
        .filter_map(|line| {
            let mut fields = line.split('\0');
            Some(LocalBranch {
                name: fields.next()?.to_string(),
                tip: fields.next()?.to_string(),
                committed_at: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect())
}

/// Commits reachable from `tip` and from no locally present remote tip.
fn unpushed_commits(repo: &Path, tip: &str, remote: &RemoteState) -> Result<Vec<String>, String> {
    let mut args = vec!["rev-list", tip];
    if !remote.present.is_empty() {
        args.push("--not");
        args.extend(remote.present.iter().map(String::as_str));
    }
    Ok(git(repo, &args)?.lines().map(str::to_string).collect())
}

/// Commits of `tip` with an equivalent patch reachable from `upstream`.
/// The broker's verbatim comparison, not `git cherry`, whose patch ids ignore
/// whitespace: a whitespace-only difference is not an equivalent patch. Any
/// failure reports no equivalents, so nothing is called landed by mistake.
fn patch_equivalent(repo: &Path, upstream: &str, tip: &str) -> BTreeSet<String> {
    aethyme_broker::GitRepo::discover(repo)
        .and_then(|git| git.cherry_marked(upstream, tip, aethyme_broker::CherrySide::Right))
        .map(|marked| {
            marked
                .into_iter()
                .filter_map(|(commit, equivalent)| equivalent.then_some(commit))
                .collect()
        })
        .unwrap_or_default()
}

fn describe_commit(repo: &Path, sha: &str) -> Value {
    let line = git(repo, &["show", "-s", "--format=%h%x00%cI%x00%s", sha]).unwrap_or_default();
    let mut fields = line.trim_end().splitn(3, '\0');
    json!({
        "sha": sha,
        "short": fields.next().unwrap_or_default(),
        "committed_at": fields.next().unwrap_or_default(),
        "subject": fields.next().unwrap_or_default(),
    })
}

struct Context<'a> {
    repo: &'a Path,
    remote: &'a RemoteState,
    default_tip: Option<String>,
    gh_slug: Option<String>,
    gh_status: String,
}

fn classify(context: &mut Context<'_>, branch: &LocalBranch) -> Result<Value, String> {
    let repo = context.repo;
    let remote = context.remote;
    let matching: Vec<&String> = remote
        .heads
        .iter()
        .filter(|(_, sha)| **sha == branch.tip)
        .map(|(name, _)| name)
        .collect();
    if !matching.is_empty() {
        return Ok(json!({
            "classification": "on-remote",
            "evidence": { "remote_branches": matching },
        }));
    }

    let unpushed = unpushed_commits(repo, &branch.tip, remote)?;
    if unpushed.is_empty() {
        // Name one remote branch that contains the tip, default branch first.
        let mut candidates: Vec<(&String, &String)> = remote.heads.iter().collect();
        candidates.sort_by_key(|(name, _)| {
            Some(name.as_str()) != context.remote.default_branch.as_deref()
        });
        let container = candidates.into_iter().find(|(_, sha)| {
            remote.present.contains(sha)
                && git_succeeds(repo, &["merge-base", "--is-ancestor", &branch.tip, sha])
        });
        return Ok(json!({
            "classification": "contained",
            "evidence": { "contained_in": container.map(|(name, _)| name) },
        }));
    }
    if unpushed.len() > MAX_COMPARED_COMMITS {
        return Ok(json!({
            "classification": "local-only",
            "unpushed_commit_count": unpushed.len(),
            "local_commits": unpushed.iter().take(MAX_LISTED_COMMITS).map(|sha| describe_commit(repo, sha)).collect::<Vec<_>>(),
            "evidence": { "note": format!("more than {MAX_COMPARED_COMMITS} unpushed commits; not compared patch by patch") },
        }));
    }

    let mut uncovered: BTreeSet<String> = unpushed.iter().cloned().collect();
    let mut matched: Vec<Value> = Vec::new();
    if let (Some(default_branch), Some(default_tip)) = (
        context.remote.default_branch.as_deref(),
        context.default_tip.as_deref(),
    ) {
        let equivalent = patch_equivalent(repo, default_tip, &branch.tip);
        let hits: Vec<String> = uncovered.intersection(&equivalent).cloned().collect();
        if !hits.is_empty() {
            matched
                .push(json!({ "kind": "patch-id", "on": default_branch, "commits": hits.len() }));
            uncovered.retain(|sha| !equivalent.contains(sha));
        }
    }

    let mut pull_requests: Vec<Value> = Vec::new();
    if !uncovered.is_empty()
        && let Some(slug) = context.gh_slug.clone()
    {
        match merged_prs(&slug, &branch.name) {
            Ok(prs) => {
                for pr in prs {
                    let head_present = commit_present(repo, &pr.head);
                    pull_requests.push(json!({
                        "number": pr.number,
                        "url": pr.url,
                        "merged_at": pr.merged_at,
                        "head": pr.head,
                        "head_present_locally": head_present,
                    }));
                    if uncovered.is_empty() {
                        continue;
                    }
                    if head_present {
                        if git(repo, &["rev-parse", &format!("{}^{{tree}}", branch.tip)]).ok()
                            == git(repo, &["rev-parse", &format!("{}^{{tree}}", pr.head)]).ok()
                        {
                            matched.push(json!({ "kind": "pr-head-tree", "pr": pr.number }));
                            uncovered.clear();
                            continue;
                        }
                        let in_head: BTreeSet<String> = uncovered
                            .iter()
                            .filter(|sha| {
                                git_succeeds(repo, &["merge-base", "--is-ancestor", sha, &pr.head])
                            })
                            .cloned()
                            .collect();
                        let equivalent = patch_equivalent(repo, &pr.head, &branch.tip);
                        let before = uncovered.len();
                        uncovered.retain(|sha| !in_head.contains(sha) && !equivalent.contains(sha));
                        if uncovered.len() < before {
                            matched.push(json!({
                                "kind": "pr-head-commits",
                                "pr": pr.number,
                                "commits": before - uncovered.len(),
                            }));
                        }
                    }
                    if !uncovered.is_empty()
                        && let Some(merge_commit) = pr.merge_commit.as_deref()
                        && commit_present(repo, merge_commit)
                        && let Some(default_tip) = context.default_tip.as_deref()
                        && let Ok(base) = git(repo, &["merge-base", default_tip, &branch.tip])
                    {
                        // A squash merge folds the branch into one commit:
                        // compare the branch's whole diff with that commit's.
                        let branch_patch = patch_id(repo, base.trim(), &branch.tip);
                        let squash_patch =
                            patch_id(repo, &format!("{merge_commit}^"), merge_commit);
                        if branch_patch.is_some() && branch_patch == squash_patch {
                            matched.push(json!({ "kind": "pr-squash-patch", "pr": pr.number }));
                            uncovered.clear();
                        }
                    }
                }
            }
            Err(error) => {
                context.gh_status = format!("unavailable: {error}");
                context.gh_slug = None;
            }
        }
    }

    if uncovered.is_empty() {
        return Ok(json!({
            "classification": "merged-via-pr",
            "unpushed_commit_count": unpushed.len(),
            "evidence": { "matched_by": matched, "pull_requests": pull_requests },
        }));
    }
    // Keep rev-list order (newest first) for the listing.
    let remaining: Vec<&String> = unpushed
        .iter()
        .filter(|sha| uncovered.contains(*sha))
        .collect();
    let mut evidence = json!({ "matched_by": matched, "pull_requests": pull_requests });
    if !pull_requests.is_empty() {
        evidence["note"] = json!(
            "a merged PR exists for this branch name, but these commits match nothing it merged; \
             they may be drafts the PR superseded. Read them before deleting the branch."
        );
    }
    Ok(json!({
        "classification": "local-only",
        "unpushed_commit_count": unpushed.len(),
        "uncovered_commit_count": remaining.len(),
        "local_commits": remaining.iter().take(MAX_LISTED_COMMITS).map(|sha| describe_commit(repo, sha)).collect::<Vec<_>>(),
        "evidence": evidence,
    }))
}

// ── worktrees ───────────────────────────────────────────────────────────────

/// True when `path` sits in a broker worktree container: `$AETHYME_WORKTREE_ROOT`
/// or the default `<host state>/Aethyme/worktrees/<repo>/<slug>` layout.
fn broker_managed(path: &Path) -> bool {
    if let Some(root) = std::env::var_os("AETHYME_WORKTREE_ROOT").filter(|root| !root.is_empty())
        && path.starts_with(PathBuf::from(root))
    {
        return true;
    }
    let components: Vec<String> = path
        .components()
        .map(|component| component.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    components
        .windows(2)
        .any(|pair| pair[0] == "aethyme" && pair[1] == "worktrees")
}

fn worktrees(repo: &Path) -> Result<Vec<Value>, String> {
    let listing = git(repo, &["worktree", "list", "--porcelain"])?;
    let mut entries = Vec::new();
    for block in listing.split("\n\n") {
        let mut path = None;
        let mut head = None;
        let mut branch = None;
        let mut detached = false;
        let mut bare = false;
        let mut prunable = None;
        let mut locked = false;
        for line in block.lines() {
            let (key, value) = line.split_once(' ').unwrap_or((line, ""));
            match key {
                "worktree" => path = Some(PathBuf::from(value)),
                "HEAD" => head = Some(value.to_string()),
                "branch" => branch = value.strip_prefix("refs/heads/").map(str::to_string),
                "detached" => detached = true,
                "bare" => bare = true,
                "prunable" => prunable = Some(value.to_string()),
                "locked" => locked = true,
                _ => {}
            }
        }
        let Some(path) = path else { continue };
        let exists = path.is_dir();
        let dirty_paths = if exists && !bare {
            git(
                &path,
                &["status", "--porcelain", "--untracked-files=normal"],
            )
            .ok()
            .map(|status| status.lines().count())
        } else {
            None
        };
        entries.push(json!({
            "path": path.display().to_string(),
            "branch": branch,
            "head": head,
            "detached": detached,
            "bare": bare,
            "exists": exists,
            "dirty_paths": dirty_paths,
            "prunable": prunable,
            "locked": locked,
            "broker_managed": broker_managed(&path),
        }));
    }
    Ok(entries)
}

// ── report ──────────────────────────────────────────────────────────────────

fn audit(options: &Options) -> Result<Value, String> {
    let repo = PathBuf::from(
        git(&options.repo, &["rev-parse", "--show-toplevel"])
            .map_err(|_| format!("{} is not inside a git repository", options.repo.display()))?
            .trim(),
    );
    let remote = remote_state(&repo, &options.remote)?;
    let default_tip = remote
        .default_branch
        .as_ref()
        .and_then(|name| remote.heads.get(name))
        .filter(|sha| remote.present.contains(sha))
        .cloned();
    let gh_slug = if options.use_gh {
        github_slug(&repo, &options.remote)
    } else {
        None
    };
    let gh_status = match (options.use_gh, &gh_slug) {
        (false, _) => "disabled (--no-gh)".to_string(),
        (true, None) => "skipped: remote is not a GitHub URL".to_string(),
        (true, Some(slug)) => format!("used for {slug}"),
    };
    let mut context = Context {
        repo: &repo,
        remote: &remote,
        default_tip,
        gh_slug,
        gh_status,
    };

    let worktree_entries = worktrees(&repo)?;
    let mut checked_out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for entry in &worktree_entries {
        if let Some(branch) = entry["branch"].as_str() {
            checked_out
                .entry(branch.to_string())
                .or_default()
                .push(entry["path"].as_str().unwrap_or_default().to_string());
        }
    }

    let mut summary: BTreeMap<String, u64> =
        ["on-remote", "contained", "merged-via-pr", "local-only"]
            .into_iter()
            .map(|name| (name.to_string(), 0))
            .collect();
    let mut branches = Vec::new();
    for branch in local_branches(&repo)? {
        let mut entry = classify(&mut context, &branch)?;
        if let Some(name) = entry["classification"].as_str() {
            *summary.entry(name.to_string()).or_default() += 1;
        }
        let mut reasons = Vec::new();
        if remote.default_branch.as_deref() == Some(branch.name.as_str()) {
            reasons.push("default branch".to_string());
        }
        if branch.name == INTEGRATION_BRANCH {
            reasons.push("broker integration branch".to_string());
        }
        for path in checked_out.get(&branch.name).into_iter().flatten() {
            reasons.push(format!("checked out in worktree {path}"));
        }
        entry["name"] = json!(branch.name);
        entry["tip"] = json!(branch.tip);
        entry["committed_at"] = json!(branch.committed_at);
        entry["protected"] = json!(!reasons.is_empty());
        entry["protected_reasons"] = json!(reasons);
        branches.push(entry);
    }

    Ok(json!({
        "repo": repo.display().to_string(),
        "remote": options.remote,
        "remote_source": remote.source,
        "remote_warning": remote.warning,
        "default_branch": remote.default_branch,
        "remote_branch_count": remote.heads.len(),
        "remote_tips_missing_locally": remote.missing_locally,
        "gh": context.gh_status,
        "summary": summary,
        "branches": branches,
        "worktrees": worktree_entries,
    }))
}

fn render(report: &Value) -> String {
    let mut out = String::new();
    let text = |value: &Value| value.as_str().unwrap_or_default().to_string();
    out.push_str(&format!(
        "Branch audit for {} against {} ({})\n",
        text(&report["repo"]),
        text(&report["remote"]),
        text(&report["remote_source"])
    ));
    if let Some(warning) = report["remote_warning"].as_str() {
        out.push_str(&format!("Warning: {warning}\n"));
    }
    let missing = report["remote_tips_missing_locally"]
        .as_array()
        .map_or(0, Vec::len);
    if missing > 0 {
        out.push_str(&format!(
            "Warning: {missing} remote tip(s) are not in the local repository, so containment in them cannot be proven; `git fetch` may turn local-only verdicts into contained.\n"
        ));
    }
    out.push_str(&format!("GitHub PR lookup: {}\n", text(&report["gh"])));
    let summary = &report["summary"];
    out.push_str(&format!(
        "Summary: {} on-remote, {} contained, {} merged-via-pr, {} local-only\n",
        summary["on-remote"], summary["contained"], summary["merged-via-pr"], summary["local-only"]
    ));

    for class in ["local-only", "merged-via-pr", "contained", "on-remote"] {
        let members: Vec<&Value> = report["branches"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|branch| branch["classification"] == class)
            .collect();
        if members.is_empty() {
            continue;
        }
        out.push_str(&format!("\n{class}:\n"));
        for branch in members {
            let protected = branch["protected_reasons"]
                .as_array()
                .filter(|reasons| !reasons.is_empty())
                .map(|reasons| {
                    format!(
                        " [protected: {}]",
                        reasons.iter().map(text).collect::<Vec<_>>().join("; ")
                    )
                })
                .unwrap_or_default();
            let detail = match class {
                "on-remote" => {
                    let names: Vec<String> = branch["evidence"]["remote_branches"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(text)
                        .collect();
                    format!("same tip as {}", names.join(", "))
                }
                "contained" => match branch["evidence"]["contained_in"].as_str() {
                    Some(name) => format!("ancestor of {name}"),
                    None => "reachable from remote tips".to_string(),
                },
                "merged-via-pr" => {
                    let matched: Vec<String> = branch["evidence"]["matched_by"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|item| match item["kind"].as_str() {
                            Some("patch-id") => {
                                format!(
                                    "{} commit(s) patch-equivalent on {}",
                                    item["commits"],
                                    text(&item["on"])
                                )
                            }
                            Some("pr-head-commits") => {
                                format!("{} commit(s) in PR #{} head", item["commits"], item["pr"])
                            }
                            Some("pr-head-tree") => format!("same tree as PR #{} head", item["pr"]),
                            Some("pr-squash-patch") => {
                                format!("same diff as PR #{} squash commit", item["pr"])
                            }
                            _ => item.to_string(),
                        })
                        .collect();
                    matched.join("; ")
                }
                _ => format!(
                    "{} commit(s) match nothing on the remote",
                    branch["uncovered_commit_count"]
                        .as_u64()
                        .or_else(|| branch["unpushed_commit_count"].as_u64())
                        .unwrap_or_default()
                ),
            };
            out.push_str(&format!(
                "  {}  {detail}{protected}\n",
                text(&branch["name"])
            ));
            if class == "local-only" {
                for commit in branch["local_commits"].as_array().into_iter().flatten() {
                    out.push_str(&format!(
                        "      {} {} {}\n",
                        text(&commit["short"]),
                        text(&commit["committed_at"]),
                        text(&commit["subject"])
                    ));
                }
                if let Some(note) = branch["evidence"]["note"].as_str() {
                    out.push_str(&format!("      note: {note}\n"));
                }
            }
        }
    }

    out.push_str("\nWorktrees:\n");
    for worktree in report["worktrees"].as_array().into_iter().flatten() {
        let branch = worktree["branch"]
            .as_str()
            .map_or_else(|| "(detached)".to_string(), str::to_string);
        let mut flags = Vec::new();
        if worktree["broker_managed"] == true {
            flags.push("broker".to_string());
        }
        match worktree["dirty_paths"].as_u64() {
            Some(0) => flags.push("clean".to_string()),
            Some(count) => flags.push(format!("{count} dirty path(s)")),
            None if worktree["exists"] == false => flags.push("missing".to_string()),
            None => {}
        }
        if let Some(reason) = worktree["prunable"].as_str() {
            flags.push(format!("prunable: {reason}"));
        }
        if worktree["locked"] == true {
            flags.push("locked".to_string());
        }
        out.push_str(&format!(
            "  {}  {branch}  [{}]\n",
            text(&worktree["path"]),
            flags.join(", ")
        ));
    }
    out.push_str("\nRead-only report: nothing was fetched or deleted.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broker_managed_recognises_the_default_layout() {
        assert!(broker_managed(Path::new(
            "/Users/x/Library/Application Support/Aethyme/worktrees/repo-1234/fix-thing"
        )));
        assert!(!broker_managed(Path::new(
            "/Users/x/Repositories/sp42-phase3"
        )));
    }

    #[test]
    fn options_parse_defaults_and_flags() {
        let options = Options::parse(&[]).expect("defaults");
        assert_eq!(options.remote, "origin");
        assert!(options.use_gh && !options.json);
        let args: Vec<String> = ["repo", "--remote=upstream", "--no-gh", "--json"]
            .iter()
            .map(|arg| (*arg).to_string())
            .collect();
        let options = Options::parse(&args).expect("flags");
        assert_eq!(options.repo, PathBuf::from("repo"));
        assert_eq!(options.remote, "upstream");
        assert!(!options.use_gh && options.json);
        assert!(Options::parse(&["--bogus".to_string()]).is_err());
    }
}
