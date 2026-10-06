//! Whether a `gh` command may write a branch ref, decided by allowlist (#393).
//!
//! Interpreting gh's option grammar is how the earlier guard was bypassed:
//! every difference between this parser and gh's is a way past it. So this
//! module does not interpret options. It compares tokens for equality:
//!
//! - [`Verdict::NoRefWrite`]: the command and action are on a short list that
//!   cannot write a branch ref, or the call is a `gh api` read whose every
//!   token is on a read-only allowlist.
//! - [`Verdict::PrMerge`]: exactly `pr merge <N> (--merge|--squash|--rebase)
//!   [-d|--delete-branch] [--match-head-commit <sha>]`, in any order, each at
//!   most once and nothing else.
//! - [`Verdict::DeleteBranch`]: exactly `api -X DELETE
//!   repos/<owner>/<repo>/git/refs/heads/<branch>` for the target repository.
//! - [`Verdict::SafeWrite`]: an exact `gh api -X POST|PATCH|DELETE` write to
//!   one of a few comment, review, label and issue endpoints, which cannot
//!   touch a git ref (see [`safe_write_endpoint`]).
//! - [`Verdict::Unverifiable`]: everything else. The caller refuses it unless
//!   the operator acknowledges it explicitly.
//!
//! There is no fall-through that allows: every branch of [`assess`] that is
//! not an exact match returns `Unverifiable`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    NoRefWrite,
    PrMerge {
        number: String,
        deletes_head: bool,
        match_head_commit: Option<String>,
    },
    DeleteBranch(String),
    /// Exactly `pr update-branch <N>`: merges the base into the PR's head
    /// branch, whose owner the caller checks.
    PrUpdateBranch(String),
    /// `pr edit|create` setting the base branch to this exact name, which a
    /// later merge would advance; the caller checks its owner.
    PrBase(String),
    /// An exact `gh api` write to a comment, review, label or issue endpoint
    /// of the target repository, which cannot touch a git ref.
    SafeWrite,
    Unverifiable(String),
}

/// `(command, action)` pairs that cannot write a branch ref. `*` matches any
/// action. Anything absent, including every alias and extension, is not here.
const NO_REF_WRITE: &[(&str, &str)] = &[
    ("--version", "*"),
    ("version", "*"),
    ("help", "*"),
    ("status", "*"),
    ("search", "*"),
    ("completion", "*"),
    ("auth", "*"),
    ("alias", "*"),
    ("issue", "*"),
    ("label", "*"),
    ("project", "*"),
    ("gist", "*"),
    ("ssh-key", "*"),
    ("gpg-key", "*"),
    ("org", "*"),
    ("ruleset", "*"),
    ("attestation", "*"),
    ("secret", "*"),
    ("variable", "*"),
    ("cache", "*"),
    // Dispatching, re-running or cancelling a workflow writes no ref itself.
    ("run", "*"),
    ("workflow", "*"),
    // Releases and their tags; tags are never session branches.
    ("release", "*"),
    // Installing or listing an extension runs nothing; `exec` is not here.
    ("extension", "list"),
    ("extension", "install"),
    ("extension", "upgrade"),
    ("extension", "remove"),
    ("extension", "search"),
    ("extension", "browse"),
    ("pr", "view"),
    ("pr", "list"),
    ("pr", "status"),
    ("pr", "diff"),
    ("pr", "checks"),
    ("pr", "comment"),
    ("pr", "review"),
    ("pr", "ready"),
    ("pr", "edit"),
    ("pr", "reopen"),
    ("pr", "lock"),
    ("pr", "unlock"),
    // Pushes only the current branch, never with force.
    ("pr", "create"),
    ("repo", "view"),
    ("repo", "list"),
];

/// Tokens a `gh api` read may contain besides its endpoint. Pairs take the
/// next token as their value; no `=`-joined or clustered spelling is
/// accepted, and nothing that sets a method or a body.
const API_READ_FLAGS: &[&str] = &["--paginate", "--slurp", "-i", "--include", "--silent"];
const API_READ_PAIRS: &[&str] = &["-q", "--jq", "-t", "--template", "-H", "--header"];

fn unverifiable(why: impl Into<String>) -> Verdict {
    Verdict::Unverifiable(why.into())
}

/// Assess `gh <args>` run against `repository` (`owner/name`, the exact
/// `--repo` the broker hands gh).
pub(crate) fn assess(args: &[String], repository: &str) -> Verdict {
    let tokens: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some((&command, rest)) = tokens.split_first() else {
        return unverifiable("no gh command");
    };
    let action = rest.first().copied().unwrap_or("");
    if command == "api" {
        return assess_api(rest, repository);
    }
    // `--web` runs the configured browser, which is a command.
    if rest.iter().any(|token| *token == "--web" || *token == "-w") {
        return unverifiable("`--web` / `-w` opens a browser, which runs a configurable command");
    }
    // A download into a `.git` directory can plant hooks.
    if matches!(command, "run" | "release")
        && action == "download"
        && rest.iter().any(|token| {
            token
                .split(['/', '\\', '='])
                .any(|part| part.eq_ignore_ascii_case(".git"))
        })
    {
        return unverifiable("a download into a `.git` directory");
    }
    if command == "pr" && action == "merge" {
        return assess_pr_merge(&rest[1..]);
    }
    if let ["pr", "close", number] = tokens.as_slice()
        && is_number(number)
    {
        return Verdict::NoRefWrite;
    }
    if let ["pr", "update-branch", number] = tokens.as_slice()
        && is_number(number)
    {
        return Verdict::PrUpdateBranch((*number).to_string());
    }
    if command == "pr" && matches!(action, "edit" | "create") {
        return assess_pr_base(&rest[1..]);
    }
    if NO_REF_WRITE.iter().any(|(known, known_action)| {
        *known == command && (*known_action == "*" || *known_action == action)
    }) {
        return Verdict::NoRefWrite;
    }
    unverifiable(format!(
        "`gh {command} {action}` is not on the broker's list of gh commands that cannot write \
         a branch ref (an alias, an extension, or a command that can delete or rewrite one)"
    ))
}

/// `pr edit|create` may set the base only as a separate `--base <name>` or
/// `-B <name>` pair, at most once. Any other token that could spell the base
/// option (`--base=…`, `-Bname`, a cluster containing `B`) is unverifiable.
fn assess_pr_base(tokens: &[&str]) -> Verdict {
    let mut base = None;
    let mut iter = tokens.iter();
    while let Some(&token) = iter.next() {
        if matches!(token, "--base" | "-B") {
            match (iter.next(), base.is_none()) {
                (Some(name), true) if !name.is_empty() && !name.starts_with('-') => {
                    base = Some((*name).to_string());
                }
                _ => return unverifiable("a repeated or missing --base value"),
            }
        } else if token.starts_with("--base")
            || token
                .strip_prefix('-')
                .is_some_and(|cluster| !cluster.starts_with('-') && cluster.contains('B'))
        {
            return unverifiable(format!(
                "base option spelled {token:?}; use `--base <name>`"
            ));
        }
    }
    match base {
        Some(base) => Verdict::PrBase(base),
        None => Verdict::NoRefWrite,
    }
}

fn is_number(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_digit())
}

/// A lowercase hex object id. A head-deleting merge additionally requires it
/// to equal the full head commit the guard reads, so a short value can only
/// make GitHub refuse the merge, never widen what is checked.
fn is_hex_sha(token: &str) -> bool {
    (1..=64).contains(&token.len())
        && token
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn assess_pr_merge(tokens: &[&str]) -> Verdict {
    let mut number = None;
    let mut method = None;
    let mut deletes_head = false;
    let mut match_head_commit = None;
    let mut iter = tokens.iter();
    while let Some(&token) = iter.next() {
        match token {
            "--merge" | "--squash" | "--rebase" if method.is_none() => method = Some(token),
            "-d" | "--delete-branch" if !deletes_head => deletes_head = true,
            "--match-head-commit" if match_head_commit.is_none() => match iter.next() {
                Some(sha) if is_hex_sha(sha) => match_head_commit = Some((*sha).to_string()),
                _ => return unverifiable("--match-head-commit needs a lowercase hex sha"),
            },
            _ if is_number(token) && number.is_none() => number = Some(token.to_string()),
            _ => {
                return unverifiable(format!(
                    "`gh pr merge` token {token:?} is outside the exact form \
                     `pr merge <N> --merge|--squash|--rebase [-d] [--match-head-commit <sha>]`"
                ));
            }
        }
    }
    match (number, method) {
        (Some(number), Some(_)) => Verdict::PrMerge {
            number,
            deletes_head,
            match_head_commit,
        },
        _ => unverifiable(
            "`gh pr merge` needs a PR number and exactly one of --merge, --squash or --rebase",
        ),
    }
}

/// Endpoints a `gh api` write may target without acknowledgement, as
/// segment patterns after `repos/<owner>/<repo>/`: `N` is a decimal number,
/// `L` a plain label name, anything else a literal segment. The methods each
/// accepts follow. None of these can create, move or delete a git ref.
const SAFE_WRITE_ENDPOINTS: &[(&[&str], &[&str])] = &[
    (&["issues", "N", "comments"], &["POST"]),
    (&["issues", "comments", "N"], &["PATCH", "DELETE"]),
    (&["pulls", "N", "comments"], &["POST"]),
    (&["pulls", "N", "comments", "N", "replies"], &["POST"]),
    (&["pulls", "comments", "N"], &["PATCH", "DELETE"]),
    (&["pulls", "N", "reviews"], &["POST"]),
    (&["pulls", "N", "reviews", "N"], &["PUT", "PATCH", "DELETE"]),
    (&["pulls", "N", "reviews", "N", "events"], &["POST"]),
    (&["issues", "N", "labels"], &["POST", "PUT", "DELETE"]),
    (&["issues", "N", "labels", "L"], &["DELETE"]),
    (&["issues", "N"], &["PATCH"]),
];

/// Fields an issue PATCH may set; anything else could be a field this list
/// does not anticipate.
const ISSUE_PATCH_FIELDS: &[&str] = &["title", "body", "state", "labels[]", "assignees[]"];

fn is_plain_label(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The [`SAFE_WRITE_ENDPOINTS`] entry `path` matches exactly for
/// `repository`, if any.
fn safe_write_endpoint(
    path: &str,
    repository: &str,
) -> Option<&'static (&'static [&'static str], &'static [&'static str])> {
    let rest = path
        .strip_prefix("repos/")?
        .strip_prefix(repository)?
        .strip_prefix('/')?;
    let segments: Vec<&str> = rest.split('/').collect();
    SAFE_WRITE_ENDPOINTS.iter().find(|(pattern, _)| {
        pattern.len() == segments.len()
            && pattern
                .iter()
                .zip(&segments)
                .all(|(want, got)| match *want {
                    "N" => is_number(got),
                    "L" => is_plain_label(got),
                    literal => literal == *got,
                })
    })
}

/// An exact allowlisted write: `-X <METHOD>` once, one endpoint, plain
/// `-f`/`-F key=value` fields (no `@file`), and optional output filters.
fn is_safe_write(tokens: &[&str], repository: &str) -> bool {
    let mut method = None;
    let mut endpoint = None;
    let mut fields = Vec::new();
    let mut iter = tokens.iter();
    while let Some(&token) = iter.next() {
        match token {
            "-X" if method.is_none() => method = iter.next().copied(),
            "-f" | "-F" => match iter.next() {
                Some(field) => fields.push((token, *field)),
                None => return false,
            },
            "--silent" => {}
            "-q" | "--jq" => {
                if iter.next().is_none() {
                    return false;
                }
            }
            _ if !token.starts_with('-') && endpoint.is_none() => endpoint = Some(token),
            _ => return false,
        }
    }
    let (Some(method), Some(endpoint)) = (method, endpoint) else {
        return false;
    };
    let Some((pattern, methods)) = safe_write_endpoint(endpoint, repository) else {
        return false;
    };
    if !methods.contains(&method) {
        return false;
    }
    let issue_patch = *pattern == ["issues", "N"];
    fields.iter().all(|(flag, field)| {
        let Some((key, value)) = field.split_once('=') else {
            return false;
        };
        let plain_key = !key.is_empty()
            && key
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'[' | b']'));
        // `-F key=@file` reads a file the broker cannot see.
        let plain_value = !(*flag == "-F" && value.starts_with('@'));
        plain_key && plain_value && (!issue_patch || ISSUE_PATCH_FIELDS.contains(&key))
    })
}

fn assess_api(tokens: &[&str], repository: &str) -> Verdict {
    if is_safe_write(tokens, repository) {
        return Verdict::SafeWrite;
    }
    if let ["-X", "DELETE", path] = tokens {
        return match branch_ref_path(path, repository) {
            Some(branch) => Verdict::DeleteBranch(branch),
            None => unverifiable(format!(
                "`gh api -X DELETE {path}` is not exactly \
                 repos/{repository}/git/refs/heads/<branch>"
            )),
        };
    }
    // A read: one endpoint, and only allowlisted read flags.
    let mut endpoint = None;
    let mut iter = tokens.iter();
    while let Some(&token) = iter.next() {
        if API_READ_FLAGS.contains(&token) {
            continue;
        }
        if API_READ_PAIRS.contains(&token) {
            let header = matches!(token, "-H" | "--header");
            match iter.next() {
                // Only content-negotiation headers: anything else might
                // override the method.
                Some(value)
                    if !header || {
                        let value = value.to_ascii_lowercase();
                        value.starts_with("accept:") || value.starts_with("x-github-api-version:")
                    } =>
                {
                    continue;
                }
                _ => return unverifiable("a gh api header or value the broker cannot vouch for"),
            }
        }
        if !token.starts_with('-') && endpoint.is_none() {
            endpoint = Some(token);
            continue;
        }
        return unverifiable(format!(
            "`gh api` token {token:?} can set a method or a body; only a plain GET or the exact \
             `-X DELETE repos/<owner>/<repo>/git/refs/heads/<branch>` is accepted"
        ));
    }
    match endpoint {
        Some(endpoint) if !endpoint.to_ascii_lowercase().contains("graphql") => Verdict::NoRefWrite,
        Some(_) => unverifiable("gh api graphql can run a mutation"),
        None => unverifiable("gh api without an endpoint"),
    }
}

/// The branch in exactly `repos/<owner>/<repo>/git/refs/heads/<branch>`.
/// Only plain branch characters; no encoding, dot segments, doubled or
/// trailing slashes, so the path GitHub routes is the path checked.
fn branch_ref_path(path: &str, repository: &str) -> Option<String> {
    let branch = path
        .strip_prefix("repos/")?
        .strip_prefix(repository)?
        .strip_prefix("/git/refs/heads/")?;
    let plain = |segment: &str| {
        !segment.is_empty()
            && segment != "."
            && segment != ".."
            && !segment.ends_with(".lock")
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    branch.split('/').all(plain).then(|| branch.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assess_line(line: &[&str]) -> Verdict {
        assess(
            &line
                .iter()
                .map(|token| (*token).to_string())
                .collect::<Vec<_>>(),
            "o/n",
        )
    }

    fn unverifiable_line(line: &[&str]) -> bool {
        matches!(assess_line(line), Verdict::Unverifiable(_))
    }

    #[test]
    fn only_the_exact_merge_form_is_assessed() {
        assert_eq!(
            assess_line(&["pr", "merge", "7", "--squash", "-d"]),
            Verdict::PrMerge {
                number: "7".into(),
                deletes_head: true,
                match_head_commit: None,
            }
        );
        assert_eq!(
            assess_line(&["pr", "merge", "--merge", "7"]),
            Verdict::PrMerge {
                number: "7".into(),
                deletes_head: false,
                match_head_commit: None,
            }
        );
        for line in [
            &["pr", "merge", "7", "-sd"][..],
            &["pr", "merge", "7", "--squash", "--delete-branch=true"],
            &["pr", "merge", "7", "--squash", "-d", "-d"],
            &["pr", "merge", "7", "--squash", "--admin"],
            &["pr", "merge", "#7", "--squash"],
            &["pr", "merge", "7", "8", "--squash"],
            &["pr", "merge", "7"],
            &["pr", "merge", "7", "--squash", "--match-head-commit", "ABC"],
            &["pr", "merge", "7", "--squash", "--match-head-commit", "-d"],
            &["pr", "merge", "7", "--squash", "--", "-d"],
        ] {
            assert!(unverifiable_line(line), "{line:?}");
        }
    }

    #[test]
    fn only_the_exact_branch_delete_is_assessed() {
        assert_eq!(
            assess_line(&["api", "-X", "DELETE", "repos/o/n/git/refs/heads/agent/a"]),
            Verdict::DeleteBranch("agent/a".into())
        );
        for line in [
            &["api", "--method", "DELETE", "repos/o/n/git/refs/heads/a"][..],
            &["api", "-XDELETE", "repos/o/n/git/refs/heads/a"],
            &["api", "-X", "delete", "repos/o/n/git/refs/heads/a"],
            &["api", "-X", "DELETE", "/repos/o/n/git/refs/heads/a"],
            &["api", "-X", "DELETE", "repos/o/n/git/refs/heads/a/"],
            &["api", "-X", "DELETE", "repos/o/n/git/refs/heads/a%2Fb"],
            &["api", "-X", "DELETE", "repos/o/n/git/refs/heads/../a"],
            &["api", "-X", "DELETE", "repos/O/N/git/refs/heads/a"],
            &["api", "-X", "DELETE", "repos/x/n/git/refs/heads/a"],
            &["api", "-X", "PATCH", "repos/o/n/git/refs/heads/a"],
            &["api", "repos/o/n/git/refs/heads/a", "-F", "sha=0"],
            &["api", "-X", "GET", "repos/o/n/git/refs/heads/a"],
            &["api", "graphql", "-f", "query={ viewer { login } }"],
            &["api", "repos/o/n", "-H", "X-HTTP-Method-Override: DELETE"],
            &["api", "repos/o/n", "-H", "X-HTTP-Method: DELETE"],
        ] {
            assert!(unverifiable_line(line), "{line:?}");
        }
        assert_eq!(
            assess_line(&["api", "repos/o/n/pulls/1", "--jq", ".head.ref"]),
            Verdict::NoRefWrite
        );
        assert_eq!(
            assess_line(&[
                "api",
                "repos/o/n",
                "-H",
                "Accept: application/vnd.github+json"
            ]),
            Verdict::NoRefWrite
        );
    }

    #[test]
    fn only_exact_comment_review_label_and_issue_writes_are_safe() {
        for line in [
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues/5/comments",
                "-f",
                "body=hi",
            ][..],
            &[
                "api",
                "-X",
                "PATCH",
                "repos/o/n/issues/comments/9",
                "-f",
                "body=x",
            ],
            &["api", "-X", "DELETE", "repos/o/n/issues/comments/9"],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/pulls/5/comments",
                "-f",
                "body=x",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/pulls/5/comments/9/replies",
                "-f",
                "body=x",
                "--jq",
                ".id",
            ],
            &[
                "api",
                "-X",
                "PATCH",
                "repos/o/n/pulls/comments/9",
                "-f",
                "body=x",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/pulls/5/reviews",
                "-f",
                "event=COMMENT",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/pulls/5/reviews/3/events",
                "-f",
                "event=APPROVE",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues/5/labels",
                "-f",
                "labels[]=bug",
            ],
            &[
                "api",
                "-X",
                "DELETE",
                "repos/o/n/issues/5/labels/needs-review",
            ],
            &[
                "api",
                "-X",
                "PATCH",
                "repos/o/n/issues/5",
                "-f",
                "state=closed",
                "--silent",
            ],
        ] {
            assert_eq!(assess_line(line), Verdict::SafeWrite, "{line:?}");
        }
        for line in [
            &[
                "api",
                "-X",
                "POST",
                "repos/x/n/issues/5/comments",
                "-f",
                "body=x",
            ][..],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues/five/comments",
                "-f",
                "body=x",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues/5/comments/extra",
                "-f",
                "body=x",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues/5/comments",
                "--input",
                "body.json",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues%2F5/comments",
                "-f",
                "body=x",
            ],
            &[
                "api",
                "-X",
                "POST",
                "-X",
                "POST",
                "repos/o/n/issues/5/comments",
            ],
            &[
                "api",
                "-X",
                "POST",
                "/repos/o/n/issues/5/comments",
                "-f",
                "body=x",
            ],
            &[
                "api",
                "-X",
                "PUT",
                "repos/o/n/issues/5/comments",
                "-f",
                "body=x",
            ],
            &[
                "api",
                "-X",
                "PATCH",
                "repos/o/n/issues/5",
                "-f",
                "milestone=1",
            ],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues/5/comments",
                "-F",
                "body=@x.md",
            ],
            &["api", "repos/o/n/issues/5/comments", "-f", "body=x"],
            &["api", "-X", "POST", "graphql", "-f", "query=mutation{x}"],
            &[
                "api",
                "-X",
                "POST",
                "repos/o/n/issues/5/comments",
                "-H",
                "Accept: x",
            ],
            &[
                "api",
                "--method",
                "POST",
                "repos/o/n/issues/5/comments",
                "-f",
                "body=x",
            ],
        ] {
            assert!(unverifiable_line(line), "{line:?}");
        }
    }

    #[test]
    fn browsers_bases_updates_and_git_downloads() {
        for line in [
            &["browse"][..],
            &["config", "set", "browser", "x"],
            &["pr", "view", "7", "--web"],
            &["issue", "view", "1", "-w"],
            &["run", "download", "1", "-D", ".git/hooks"],
            &["release", "download", "v1", "--dir=x/.git/hooks"],
            &["pr", "edit", "7", "--base=agent/x"],
            &["pr", "edit", "7", "-Bagent/x"],
            &["pr", "edit", "7", "--base", "a", "--base", "b"],
            &["pr", "create", "-fB", "agent/x"],
        ] {
            assert!(unverifiable_line(line), "{line:?}");
        }
        assert_eq!(
            assess_line(&["pr", "update-branch", "7"]),
            Verdict::PrUpdateBranch("7".into())
        );
        assert_eq!(
            assess_line(&["pr", "edit", "7", "--base", "agent/x"]),
            Verdict::PrBase("agent/x".into())
        );
        assert_eq!(
            assess_line(&["pr", "create", "-B", "main", "--title", "t"]),
            Verdict::PrBase("main".into())
        );
        assert_eq!(
            assess_line(&["pr", "edit", "7", "--title", "t"]),
            Verdict::NoRefWrite
        );
        assert_eq!(
            assess_line(&["run", "download", "1", "-D", "artifacts"]),
            Verdict::NoRefWrite
        );
    }

    #[test]
    fn unknown_commands_and_rewriting_forms_are_unverifiable() {
        for line in [
            &["co", "7"][..],
            &["extension", "exec", "x"],
            &["pr", "close", "7", "-d"],
            &["pr", "update-branch", "7", "--rebase"],
            &["pr", "checkout", "7"],
            &["repo", "sync", "--force"],
            &["repo", "rename", "x"],
            &["pr", "-R", "x", "view"],
            &[],
        ] {
            assert!(unverifiable_line(line), "{line:?}");
        }
        for line in [
            &["pr", "close", "7"][..],
            &["pr", "view", "7"],
            &["issue", "comment", "1", "--body", "x"],
        ] {
            assert_eq!(assess_line(line), Verdict::NoRefWrite, "{line:?}");
        }
    }
}
