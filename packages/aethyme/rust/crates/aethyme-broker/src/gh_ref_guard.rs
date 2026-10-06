//! Which branch ref a `gh` command can write, decided fail-closed (#393).
//!
//! The broker runs `gh` with arguments it did not write, and a cross-session
//! guard is only as good as its agreement with how `gh` parses them. Matching
//! spellings one at a time does not converge, so this module answers a
//! narrower question and refuses to guess:
//!
//! - [`RefWrite::None`]: provably writes no branch ref.
//! - [`RefWrite::Branch`]: writes exactly this branch.
//! - [`RefWrite::PrHead`]: writes the head branch of a pull request, which
//!   the caller resolves with a `gh pr view` read before running anything.
//! - [`RefWrite::Uncertain`]: may write a ref the broker cannot pin down. The
//!   caller refuses it unless the operator acknowledges it explicitly.
//!
//! Options are parsed with gh's own rules (cobra/pflag): `--flag=value`,
//! `--flag value`, `-abc` clusters where a value-taking letter consumes the
//! rest of the cluster or the next argument, the last occurrence wins, and
//! `--` ends options. An option this does not know makes the command
//! uncertain, because its value could be read as an operand.

/// What a `gh` invocation can do to branch refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RefWrite {
    None,
    Branch(String),
    PrHead { selector: Option<String> },
    Uncertain(String),
}

impl RefWrite {
    pub(crate) fn writes(&self) -> bool {
        !matches!(self, RefWrite::None)
    }
}

/// One gh command's option grammar.
struct Grammar<'a> {
    bool_short: &'a str,
    value_short: &'a str,
    bool_long: &'a [&'a str],
    value_long: &'a [&'a str],
}

/// Options gh parsed, in order, plus positionals.
#[derive(Default)]
struct Parsed {
    /// `(name, value)`; booleans carry `"true"` or their `=value`. Short
    /// letters are stored as `-x`, long names as `--name`.
    options: Vec<(String, String)>,
    positionals: Vec<String>,
}

impl Parsed {
    fn last(&self, names: &[&str]) -> Option<&str> {
        self.options
            .iter()
            .rev()
            .find(|(name, _)| names.contains(&name.as_str()))
            .map(|(_, value)| value.as_str())
    }

    fn all<'s>(&'s self, names: &'s [&'s str]) -> impl Iterator<Item = &'s str> + 's {
        self.options
            .iter()
            .filter(move |(name, _)| names.contains(&name.as_str()))
            .map(|(_, value)| value.as_str())
    }

    fn any(&self, names: &[&str]) -> bool {
        self.options
            .iter()
            .any(|(name, _)| names.contains(&name.as_str()))
    }

    /// pflag's bool parsing (`strconv.ParseBool`), last occurrence wins.
    fn flag(&self, names: &[&str]) -> Result<bool, String> {
        match self.last(names) {
            None => Ok(false),
            Some(value) => parse_bool(value)
                .ok_or_else(|| format!("{} has a non-boolean value {value:?}", names[0])),
        }
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Parse `args` (after the command words) the way pflag would.
fn parse(args: &[String], grammar: &Grammar<'_>) -> Result<Parsed, String> {
    let mut parsed = Parsed::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            parsed.positionals.extend(iter.by_ref().cloned());
            break;
        }
        if let Some(long) = arg.strip_prefix("--") {
            let (name, inline) = match long.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (long, None),
            };
            if grammar.bool_long.contains(&name) || matches!(name, "help") {
                parsed
                    .options
                    .push((format!("--{name}"), inline.unwrap_or("true").to_string()));
            } else if grammar.value_long.contains(&name) {
                let value = match inline {
                    Some(value) => value.to_string(),
                    None => iter
                        .next()
                        .ok_or_else(|| format!("--{name} needs a value"))?
                        .clone(),
                };
                parsed.options.push((format!("--{name}"), value));
            } else {
                return Err(format!("unrecognized option --{name}"));
            }
            continue;
        }
        if let Some(cluster) = arg.strip_prefix('-').filter(|rest| !rest.is_empty()) {
            for (index, letter) in cluster.char_indices() {
                if grammar.bool_short.contains(letter) || letter == 'h' {
                    // `-d=false` is pflag's spelling of a short bool's value.
                    let rest = &cluster[index + letter.len_utf8()..];
                    if let Some(value) = rest.strip_prefix('=') {
                        parsed
                            .options
                            .push((format!("-{letter}"), value.to_string()));
                        break;
                    }
                    parsed.options.push((format!("-{letter}"), "true".into()));
                } else if grammar.value_short.contains(letter) {
                    let rest = &cluster[index + letter.len_utf8()..];
                    let value = if rest.is_empty() {
                        iter.next()
                            .ok_or_else(|| format!("-{letter} needs a value"))?
                            .clone()
                    } else {
                        rest.strip_prefix('=').unwrap_or(rest).to_string()
                    };
                    parsed.options.push((format!("-{letter}"), value));
                    break;
                } else {
                    return Err(format!("unrecognized option -{letter} in {arg:?}"));
                }
            }
            continue;
        }
        parsed.positionals.push(arg.clone());
    }
    Ok(parsed)
}

const PR_MERGE: Grammar<'static> = Grammar {
    bool_short: "dmrs",
    value_short: "AbFtR",
    bool_long: &[
        "admin",
        "auto",
        "delete-branch",
        "disable-auto",
        "merge",
        "rebase",
        "squash",
    ],
    value_long: &[
        "author-email",
        "body",
        "body-file",
        "match-head-commit",
        "subject",
        "repo",
    ],
};

const PR_CLOSE: Grammar<'static> = Grammar {
    bool_short: "d",
    value_short: "cR",
    bool_long: &["delete-branch"],
    value_long: &["comment", "repo"],
};

const PR_UPDATE_BRANCH: Grammar<'static> = Grammar {
    bool_short: "",
    value_short: "R",
    bool_long: &["rebase"],
    value_long: &["repo"],
};

const PR_CHECKOUT: Grammar<'static> = Grammar {
    bool_short: "f",
    value_short: "bR",
    bool_long: &["force", "detach", "recurse-submodules"],
    value_long: &["branch", "repo"],
};

const REPO_SYNC: Grammar<'static> = Grammar {
    bool_short: "",
    value_short: "bs",
    bool_long: &["force"],
    value_long: &["branch", "source"],
};

const API: Grammar<'static> = Grammar {
    bool_short: "i",
    value_short: "XfFHqtp",
    bool_long: &["include", "paginate", "slurp", "silent", "verbose"],
    value_long: &[
        "method",
        "raw-field",
        "field",
        "header",
        "input",
        "jq",
        "template",
        "cache",
        "preview",
        "hostname",
    ],
};

/// Built-in gh commands. Anything else is an alias or an extension, which
/// can expand to anything.
pub(crate) const BUILTIN: &[&str] = &[
    "agent-task",
    "alias",
    "api",
    "attestation",
    "auth",
    "browse",
    "cache",
    "codespace",
    "completion",
    "config",
    "copilot",
    "extension",
    "gist",
    "gpg-key",
    "help",
    "issue",
    "label",
    "org",
    "pr",
    "preview",
    "project",
    "release",
    "repo",
    "ruleset",
    "run",
    "search",
    "secret",
    "ssh-key",
    "status",
    "variable",
    "version",
    "workflow",
    "--version",
    "--help",
];

/// Decide what `gh <args>` can do to branch refs of `repository`
/// (`owner/name`, the exact `--repo` target the broker gives gh). With no
/// repository, an endpoint's repository is not checked: that form only
/// decides whether the command can write a ref at all (its effect class).
pub(crate) fn analyze(args: &[String], repository: Option<&str>) -> RefWrite {
    let uncertain = |why: String| RefWrite::Uncertain(why);
    let Some(command) = args.first().map(String::as_str) else {
        return RefWrite::None;
    };
    if !BUILTIN.contains(&command) {
        return uncertain(format!(
            "`{command}` is not a built-in gh command, so it is an alias or extension that can \
             expand to anything"
        ));
    }
    let action = args.get(1).map(String::as_str);
    let rest = args.get(2..).unwrap_or_default();
    match (command, action) {
        ("extension", Some("exec" | "e")) => {
            uncertain("`gh extension exec` runs an extension".into())
        }
        ("pr", Some("merge" | "close" | "update-branch" | "checkout")) => {
            let (grammar, deleting): (&Grammar<'_>, &[&str]) = match action {
                Some("merge") => (&PR_MERGE, &["-d", "--delete-branch"]),
                Some("close") => (&PR_CLOSE, &["-d", "--delete-branch"]),
                Some("update-branch") => (&PR_UPDATE_BRANCH, &["--rebase"]),
                _ => (&PR_CHECKOUT, &["-f", "--force"]),
            };
            // Any mention of the rewriting flag that this cannot parse is
            // uncertain; a command that cannot rewrite the head stays None.
            let parsed = match parse(rest, grammar) {
                Ok(parsed) => parsed,
                Err(why) if mentions_any(rest, deleting) => return uncertain(why),
                Err(_) => return RefWrite::None,
            };
            match parsed.flag(deleting) {
                Ok(false) => RefWrite::None,
                Err(why) => uncertain(why),
                // The broker already gave gh the exact repository; a second
                // target (`-dRother/repo` slips past a prefix check) is not it.
                Ok(true) if parsed.any(&["-R", "--repo"]) => {
                    uncertain("a -R/--repo override of the broker's --repo".into())
                }
                Ok(true) if parsed.positionals.len() > 1 => uncertain(format!(
                    "more than one pull request operand: {:?}",
                    parsed.positionals
                )),
                Ok(true) => RefWrite::PrHead {
                    selector: parsed.positionals.first().cloned(),
                },
            }
        }
        ("repo", Some("sync")) => {
            let parsed = match parse(rest, &REPO_SYNC) {
                Ok(parsed) => parsed,
                Err(why) if mentions_any(rest, &["--force"]) => return uncertain(why),
                Err(_) => return RefWrite::None,
            };
            match parsed.flag(&["--force"]) {
                Ok(false) => RefWrite::None,
                Err(why) => uncertain(why),
                Ok(true) if !parsed.positionals.is_empty() => uncertain(format!(
                    "`gh repo sync --force` names destination {:?}",
                    parsed.positionals[0]
                )),
                Ok(true) => match parsed.last(&["-b", "--branch"]) {
                    Some(branch) => RefWrite::Branch(branch.to_string()),
                    None => uncertain(
                        "`gh repo sync --force` without --branch resets a branch gh chooses".into(),
                    ),
                },
            }
        }
        ("api", _) => analyze_api(&args[1..], repository),
        _ => RefWrite::None,
    }
}

/// True when some argument could be one of `flags` in any spelling, used only
/// to decide that an unparseable command is uncertain rather than harmless.
fn mentions_any(args: &[String], flags: &[&str]) -> bool {
    args.iter().any(|arg| {
        flags.iter().any(|flag| match flag.strip_prefix("--") {
            Some(long) => arg
                .strip_prefix("--")
                .is_some_and(|name| name.split('=').next() == Some(long)),
            None => {
                let letter = flag.trim_start_matches('-');
                arg.strip_prefix('-')
                    .is_some_and(|cluster| !cluster.starts_with('-') && cluster.contains(letter))
            }
        })
    })
}

fn analyze_api(args: &[String], repository: Option<&str>) -> RefWrite {
    let uncertain = |why: String| RefWrite::Uncertain(why);
    let parsed = match parse(args, &API) {
        Ok(parsed) => parsed,
        Err(why) => return uncertain(format!("gh api: {why}")),
    };
    let [endpoint] = parsed.positionals.as_slice() else {
        return uncertain(format!(
            "gh api takes one endpoint, got {:?}",
            parsed.positionals
        ));
    };
    if parsed
        .all(&["-H", "--header"])
        .any(|header| header.to_ascii_lowercase().contains("method-override"))
    {
        return uncertain("an HTTP method-override header".into());
    }
    if parsed.any(&["--hostname"]) {
        return uncertain("--hostname targets another GitHub host".into());
    }
    let has_method = parsed.any(&["-X", "--method"]);
    let has_body = parsed.any(&["-f", "--raw-field", "-F", "--field", "--input"]);
    let path = match normalize_endpoint(endpoint, repository) {
        Ok(path) => path,
        Err(why) => return uncertain(why),
    };
    // A GET with no body and no method flag in any spelling.
    let provably_read = !has_method && !has_body;
    let segments: Vec<&str> = path.split('/').collect();
    if segments == ["graphql"] {
        return analyze_graphql(&parsed);
    }
    let touches_refs = segments.iter().any(|segment| {
        segment.eq_ignore_ascii_case("refs") || segment.eq_ignore_ascii_case("matching-refs")
    });
    let renames_branch = matches!(
        segments.as_slice(),
        ["repos", _, _, "branches", _, "rename"]
    );
    if (!touches_refs && !renames_branch) || provably_read {
        return RefWrite::None;
    }
    match segments.as_slice() {
        ["repos", _, _, "git", "refs", "heads", branch @ ..] if !branch.is_empty() => {
            RefWrite::Branch(branch.join("/"))
        }
        ["repos", _, _, "branches", branch, "rename"] => RefWrite::Branch((*branch).to_string()),
        ["repos", _, _, "git", "refs", "tags", tag @ ..] if !tag.is_empty() => RefWrite::None,
        _ => uncertain(format!(
            "a write to ref endpoint {endpoint:?} whose branch cannot be determined"
        )),
    }
}

/// The canonical `owner/repo/...` path of an endpoint, or why it is not
/// certain: another host, an unresolved placeholder, dot or empty segments,
/// a trailing slash, a different repository, or anything still encoded after
/// decoding.
fn normalize_endpoint(endpoint: &str, repository: Option<&str>) -> Result<String, String> {
    let refuse = |why: &str| Err(format!("endpoint {endpoint:?}: {why}"));
    let mut path = endpoint.to_string();
    if let Some((scheme, rest)) = path.split_once("://") {
        let (host, rest) = rest.split_once('/').unwrap_or((rest, ""));
        if !scheme.eq_ignore_ascii_case("https") || !host.eq_ignore_ascii_case("api.github.com") {
            return refuse("not https://api.github.com");
        }
        path = rest.to_string();
    }
    if path.contains(['?', '#']) {
        path = path
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .to_string();
    }
    let (owner, name) = repository
        .and_then(|repository| repository.split_once('/'))
        .unwrap_or(("owner", "repo"));
    path = path.replace("{owner}", owner).replace("{repo}", name);
    if path.contains(['{', '}']) {
        return refuse("an unresolved placeholder");
    }
    for _ in 0..4 {
        let decoded = percent_decode(&path).ok_or("invalid percent-encoding")?;
        if decoded == path {
            break;
        }
        path = decoded;
    }
    if path.contains('%') {
        return refuse("still percent-encoded after decoding");
    }
    if path
        .chars()
        .any(|ch| ch.is_control() || ch == '\\' || !ch.is_ascii())
    {
        return refuse("control, backslash or non-ASCII characters");
    }
    let path = path.strip_prefix('/').unwrap_or(&path).to_string();
    if path
        .split('/')
        .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return refuse("empty or dot path segments");
    }
    let segments: Vec<&str> = path.split('/').collect();
    if let (Some(repository), ["repos", endpoint_owner, endpoint_name, ..]) =
        (repository, segments.as_slice())
        && !format!("{endpoint_owner}/{endpoint_name}").eq_ignore_ascii_case(repository)
    {
        return refuse(&format!("a repository other than {repository}"));
    }
    if segments
        .first()
        .is_some_and(|first| first.eq_ignore_ascii_case("repos") && *first != "repos")
    {
        return refuse("unusual casing");
    }
    Ok(path)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = (*bytes.get(index + 1)? as char).to_digit(16)?;
            let low = (*bytes.get(index + 2)? as char).to_digit(16)?;
            out.push((high * 16 + low) as u8);
            index += 3;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A GraphQL request writes nothing only when every document is visible and
/// none contains the `mutation` keyword: GraphQL keywords are literal, so an
/// operation that mutates must spell it. Anything not visible is uncertain.
fn analyze_graphql(parsed: &Parsed) -> RefWrite {
    let uncertain = |why: &str| RefWrite::Uncertain(format!("gh api graphql: {why}"));
    let mut documents = Vec::new();
    for (name, value) in &parsed.options {
        match name.as_str() {
            "-f" | "--raw-field" => documents.push(value.clone()),
            "-F" | "--field" => match value.split_once("=@") {
                Some((_, "-")) => return uncertain("a field read from stdin"),
                Some((_, path)) => match std::fs::read_to_string(path) {
                    Ok(text) => documents.push(text),
                    Err(_) => return uncertain("a field file the broker cannot read"),
                },
                None => documents.push(value.clone()),
            },
            "--input" => {
                if value == "-" {
                    return uncertain("a request body read from stdin");
                }
                let Ok(text) = std::fs::read_to_string(value) else {
                    return uncertain("a request body the broker cannot read");
                };
                // JSON can escape the keyword (`mutation`), so read the
                // strings it decodes to rather than the raw bytes.
                let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else {
                    return uncertain("a request body that is not JSON");
                };
                collect_strings(&body, &mut documents);
            }
            _ => {}
        }
    }
    if documents
        .iter()
        .any(|document| document.contains("mutation"))
    {
        return uncertain("a mutation, which can delete or move a ref by node id");
    }
    RefWrite::None
}

fn collect_strings(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => out.push(text.clone()),
        serde_json::Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        serde_json::Value::Object(map) => map.values().for_each(|item| collect_strings(item, out)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(line: &[&str]) -> RefWrite {
        analyze(
            &line
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>(),
            Some("o/n"),
        )
    }

    fn head(selector: Option<&str>) -> RefWrite {
        RefWrite::PrHead {
            selector: selector.map(str::to_string),
        }
    }

    #[test]
    fn clustered_short_flags_follow_pflag() {
        assert_eq!(run(&["pr", "merge", "7", "-sd"]), head(Some("7")));
        // `-t` consumes the rest of the cluster: `d` is part of the subject.
        assert_eq!(run(&["pr", "merge", "7", "-tsubjd"]), RefWrite::None);
        assert_eq!(run(&["pr", "merge", "-sdtsubj", "7"]), head(Some("7")));
        assert!(matches!(
            run(&["pr", "merge", "7", "-sdZ"]),
            RefWrite::Uncertain(_)
        ));
    }

    #[test]
    fn equals_joined_values_follow_pflag() {
        assert_eq!(
            run(&["pr", "merge", "7", "--delete-branch=1"]),
            head(Some("7"))
        );
        assert_eq!(run(&["pr", "merge", "7", "-d=true"]), head(Some("7")));
        assert_eq!(
            run(&["pr", "merge", "7", "--delete-branch=False"]),
            RefWrite::None
        );
        assert_eq!(
            run(&["pr", "merge", "--subject=-d", "7"]),
            RefWrite::None,
            "a value is not a flag"
        );
        assert!(matches!(
            run(&["pr", "merge", "7", "--delete-branch=maybe"]),
            RefWrite::Uncertain(_)
        ));
    }

    #[test]
    fn the_last_repeated_flag_wins() {
        assert_eq!(
            run(&["pr", "merge", "7", "--delete-branch=false", "-d"]),
            head(Some("7"))
        );
        assert_eq!(
            run(&["pr", "merge", "7", "-d", "--delete-branch=false"]),
            RefWrite::None
        );
        assert_eq!(
            run(&[
                "api",
                "-X",
                "GET",
                "-X",
                "DELETE",
                "repos/o/n/git/refs/heads/a/b"
            ]),
            RefWrite::Branch("a/b".into())
        );
    }

    #[test]
    fn fields_imply_a_write_and_only_a_bare_get_is_a_read() {
        assert_eq!(
            run(&["api", "repos/o/n/git/refs/heads/a", "-F", "sha=0"]),
            RefWrite::Branch("a".into())
        );
        assert_eq!(
            run(&["api", "repos/o/n/git/refs/heads/a", "--input", "-"]),
            RefWrite::Branch("a".into())
        );
        assert_eq!(run(&["api", "repos/o/n/git/refs/heads/a"]), RefWrite::None);
        assert_eq!(
            run(&["api", "-XGET", "repos/o/n/git/refs/heads/a"]),
            RefWrite::Branch("a".into()),
            "an explicit method is not provably a read"
        );
    }

    #[test]
    fn graphql_is_a_read_only_without_a_visible_mutation() {
        assert_eq!(
            run(&["api", "graphql", "-f", "query={ viewer { login } }"]),
            RefWrite::None
        );
        for line in [
            &[
                "api",
                "graphql",
                "-f",
                "query=mutation { deleteRef(input:{}) { x } }",
            ][..],
            &["api", "graphql", "--input", "-"],
            &["api", "graphql", "-F", "query=@-"],
            &["api", "/graphql", "-fquery=mutation{x}"],
            &[
                "api",
                "https://api.github.com/graphql",
                "-f",
                "query=mutation{x}",
            ],
        ] {
            assert!(matches!(run(line), RefWrite::Uncertain(_)), "{line:?}");
        }
    }

    #[test]
    fn unnormalizable_endpoints_are_uncertain() {
        for endpoint in [
            "repos/o/n/git/refs/heads/a/",
            "repos/o/n/git/refs/tags/../heads/a",
            "repos/o/n/git/refs/heads%2Fa",
            "repos/o/n/git/refs/heads/a%25zz",
            "repos/o/n/git/refs/heads/{branch}",
            "https://evil.example/repos/o/n/git/refs/heads/a",
            "REPOS/o/n/git/refs/heads/a",
            "repos/o/n/git/Refs/Heads/a",
        ] {
            let result = run(&["api", "-X", "DELETE", endpoint]);
            assert!(
                matches!(result, RefWrite::Uncertain(_)) || result == RefWrite::Branch("a".into()),
                "{endpoint}: {result:?}"
            );
            assert_ne!(result, RefWrite::None, "{endpoint}");
        }
        assert_eq!(
            run(&[
                "api",
                "-X",
                "DELETE",
                "/repos/{owner}/{repo}/git/refs/heads/a%252Fb"
            ]),
            RefWrite::Branch("a/b".into())
        );
    }

    #[test]
    fn another_repository_or_host_is_uncertain() {
        for line in [
            &["api", "-X", "DELETE", "repos/other/n/git/refs/heads/a"][..],
            &[
                "api",
                "--hostname",
                "ghe.example",
                "-X",
                "DELETE",
                "repos/o/n/git/refs/heads/a",
            ],
            &["pr", "merge", "7", "-dRother/repo"],
        ] {
            let result = run(line);
            // `-R` in a pr cluster is a known option; the repository check
            // for it happens where `--repo` is compared (it is refused there).
            assert_ne!(result, RefWrite::None, "{line:?}");
        }
    }

    #[test]
    fn aliases_extensions_and_forced_syncs() {
        assert!(matches!(run(&["co", "7"]), RefWrite::Uncertain(_)));
        assert!(matches!(
            run(&["extension", "exec", "x"]),
            RefWrite::Uncertain(_)
        ));
        assert_eq!(
            run(&["repo", "sync", "--force", "--branch", "agent/a"]),
            RefWrite::Branch("agent/a".into())
        );
        assert_eq!(
            run(&["repo", "sync", "-bagent/a", "--force=true"]),
            RefWrite::Branch("agent/a".into())
        );
        assert!(matches!(
            run(&["repo", "sync", "o/fork", "--force"]),
            RefWrite::Uncertain(_)
        ));
        assert_eq!(
            run(&["repo", "sync", "--branch", "agent/a"]),
            RefWrite::None
        );
        assert_eq!(run(&["pr", "merge", "7", "--squash"]), RefWrite::None);
        assert_eq!(run(&["pr", "close", "7", "-d"]), head(Some("7")));
        assert_eq!(run(&["pr", "update-branch", "--rebase"]), head(None));
        assert!(matches!(
            run(&["pr", "merge", "7", "8", "-d"]),
            RefWrite::Uncertain(_)
        ));
    }
}
