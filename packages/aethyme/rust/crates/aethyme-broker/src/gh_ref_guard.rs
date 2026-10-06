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
    ("browse", "*"),
    ("auth", "status"),
    ("auth", "token"),
    ("issue", "*"),
    ("label", "*"),
    ("run", "*"),
    ("workflow", "list"),
    ("workflow", "view"),
    ("release", "list"),
    ("release", "view"),
    ("release", "download"),
    ("secret", "list"),
    ("variable", "list"),
    ("variable", "get"),
    ("cache", "list"),
    ("project", "*"),
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
    if command == "pr" && action == "merge" {
        return assess_pr_merge(&rest[1..]);
    }
    // Exact closes and branch updates that cannot delete or rewrite the head.
    if let ["pr", "close" | "update-branch", number] = tokens.as_slice()
        && is_number(number)
    {
        return Verdict::NoRefWrite;
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

fn is_number(token: &str) -> bool {
    !token.is_empty() && token.bytes().all(|byte| byte.is_ascii_digit())
}

fn is_full_sha(token: &str) -> bool {
    token.len() == 40
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
                Some(sha) if is_full_sha(sha) => match_head_commit = Some((*sha).to_string()),
                _ => return unverifiable("--match-head-commit needs a full 40-character sha"),
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

fn assess_api(tokens: &[&str], repository: &str) -> Verdict {
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
            &["pr", "merge", "7", "--squash", "--match-head-commit", "abc"],
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
            &["pr", "update-branch", "7"],
            &["pr", "view", "7"],
            &["issue", "comment", "1", "--body", "x"],
        ] {
            assert_eq!(assess_line(line), Verdict::NoRefWrite, "{line:?}");
        }
    }
}
