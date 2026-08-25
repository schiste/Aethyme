//! Canonical GitHub repository identity for coordinated `gh` operations.
//!
//! The broker's outer `--repo owner/name` is authoritative. GitHub treats
//! repository names case-insensitively, so locks and journal identity use a
//! lowercase `github.com/owner/repo` key while the operator's spelling remains
//! available for `GH_REPO` and audit output.

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GithubApiTargetEvidence {
    NoRepositoryEndpoint,
    MatchedExplicitRepository,
    MatchedRepositoryPlaceholders,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedGithubTarget {
    pub coordination_key: String,
    pub display_slug: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_endpoint_repository: Option<String>,
    pub api_target: GithubApiTargetEvidence,
}

#[derive(Debug, thiserror::Error)]
pub enum GithubTargetError {
    #[error("repository must be an exact owner/name slug, got {value:?}")]
    InvalidRepository { value: String },
    #[error("do not pass a second repository target after --; use broker gh --repo owner/name")]
    AlternateRepositoryFlag,
    #[error("unsupported gh api option {option:?} prevents deterministic endpoint validation")]
    UnsupportedApiOption { option: String },
    #[error("gh api repository endpoint is incomplete; expected repos/owner/repo/...")]
    IncompleteApiRepositoryEndpoint,
    #[error(
        "gh api repository endpoint {endpoint_repository:?} does not match broker --repo {broker_repository:?}"
    )]
    ApiRepositoryMismatch {
        endpoint_repository: String,
        broker_repository: String,
    },
}

pub fn resolve_github_target(
    repository: &str,
    args: &[String],
) -> Result<ResolvedGithubTarget, GithubTargetError> {
    validate_repository(repository)?;
    reject_alternate_repository_flags(args)?;
    let (display_owner, display_repo) = repository.split_once('/').expect("validated repository");
    let endpoint = gh_api_endpoint(args)?;
    let Some(endpoint) = endpoint else {
        return Ok(ResolvedGithubTarget {
            coordination_key: format!("github.com/{}", repository.to_ascii_lowercase()),
            display_slug: repository.into(),
            api_endpoint_repository: None,
            api_target: GithubApiTargetEvidence::NoRepositoryEndpoint,
        });
    };
    let Some((endpoint_owner, endpoint_repo)) = api_repository_components(endpoint)? else {
        return Ok(ResolvedGithubTarget {
            coordination_key: format!("github.com/{}", repository.to_ascii_lowercase()),
            display_slug: repository.into(),
            api_endpoint_repository: None,
            api_target: GithubApiTargetEvidence::NoRepositoryEndpoint,
        });
    };

    let used_placeholders = endpoint_owner == "{owner}" || endpoint_repo == "{repo}";
    let resolved_owner = if endpoint_owner == "{owner}" {
        display_owner
    } else {
        endpoint_owner
    };
    let resolved_repo = if endpoint_repo == "{repo}" {
        display_repo
    } else {
        endpoint_repo
    };
    let endpoint_repository = format!("{resolved_owner}/{resolved_repo}");
    if !endpoint_repository.eq_ignore_ascii_case(repository) {
        return Err(GithubTargetError::ApiRepositoryMismatch {
            endpoint_repository,
            broker_repository: repository.into(),
        });
    }

    Ok(ResolvedGithubTarget {
        coordination_key: format!("github.com/{}", repository.to_ascii_lowercase()),
        display_slug: repository.into(),
        api_endpoint_repository: Some(endpoint_repository),
        api_target: if used_placeholders {
            GithubApiTargetEvidence::MatchedRepositoryPlaceholders
        } else {
            GithubApiTargetEvidence::MatchedExplicitRepository
        },
    })
}

fn validate_repository(value: &str) -> Result<(), GithubTargetError> {
    let mut parts = value.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    let valid_component = |part: &str| {
        !part.is_empty()
            && part
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    if !valid_component(owner) || !valid_component(name) || parts.next().is_some() {
        return Err(GithubTargetError::InvalidRepository {
            value: value.into(),
        });
    }
    Ok(())
}

fn reject_alternate_repository_flags(args: &[String]) -> Result<(), GithubTargetError> {
    if args.iter().any(|arg| {
        matches!(arg.as_str(), "-R" | "--repo" | "--hostname")
            || arg.starts_with("--repo=")
            || arg.starts_with("--hostname=")
            || arg.starts_with("-R=")
            || arg
                .strip_prefix("-R")
                .is_some_and(|value| !value.is_empty())
    }) {
        return Err(GithubTargetError::AlternateRepositoryFlag);
    }
    Ok(())
}

fn gh_api_endpoint(args: &[String]) -> Result<Option<&str>, GithubTargetError> {
    if args.first().map(String::as_str) != Some("api") {
        return Ok(None);
    }
    let mut index = 1;
    while index < args.len() {
        let argument = args[index].as_str();
        if argument == "--" {
            return Ok(args.get(index + 1).map(String::as_str));
        }
        if !argument.starts_with('-') || argument == "-" {
            return Ok(Some(argument));
        }
        let option = argument.split('=').next().unwrap_or(argument);
        if api_option_takes_value(option) {
            if argument.contains('=') || short_option_has_attached_value(argument) {
                index += 1;
            } else {
                index += 2;
            }
            continue;
        }
        if api_option_is_flag(option) {
            index += 1;
            continue;
        }
        return Err(GithubTargetError::UnsupportedApiOption {
            option: option.into(),
        });
    }
    Ok(None)
}

fn api_option_takes_value(option: &str) -> bool {
    matches!(
        option,
        "--cache"
            | "--field"
            | "--header"
            | "--input"
            | "--jq"
            | "--method"
            | "--preview"
            | "--raw-field"
            | "--template"
            | "-F"
            | "-H"
            | "-X"
            | "-f"
            | "-p"
            | "-q"
            | "-t"
    )
}

fn short_option_has_attached_value(argument: &str) -> bool {
    ["-F", "-H", "-X", "-f", "-p", "-q", "-t"]
        .iter()
        .any(|option| argument.starts_with(option) && argument.len() > option.len())
}

fn api_option_is_flag(option: &str) -> bool {
    matches!(
        option,
        "--allow-escape-sequences"
            | "--help"
            | "--include"
            | "--paginate"
            | "--silent"
            | "--slurp"
            | "--verbose"
            | "-i"
    )
}

fn api_repository_components(endpoint: &str) -> Result<Option<(&str, &str)>, GithubTargetError> {
    let path = endpoint
        .split_once("//")
        .map(|(_, authority_and_path)| {
            authority_and_path
                .split_once('/')
                .map(|(_, path)| path)
                .unwrap_or_default()
        })
        .unwrap_or(endpoint)
        .trim_start_matches('/');
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let segments: Vec<&str> = path.split('/').collect();
    let Some(repos_index) = segments
        .iter()
        .position(|segment| segment.eq_ignore_ascii_case("repos"))
    else {
        return Ok(None);
    };
    let Some(owner) = segments.get(repos_index + 1).copied() else {
        return Err(GithubTargetError::IncompleteApiRepositoryEndpoint);
    };
    let Some(repo) = segments.get(repos_index + 2).copied() else {
        return Err(GithubTargetError::IncompleteApiRepositoryEndpoint);
    };
    if owner.is_empty() || repo.is_empty() {
        return Err(GithubTargetError::IncompleteApiRepositoryEndpoint);
    }
    Ok(Some((owner, repo)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|part| (*part).into()).collect()
    }

    #[test]
    fn coordination_is_case_insensitive_while_display_spelling_is_preserved() {
        let target = resolve_github_target("Schiste/Aethyme", &args(&["pr", "view", "1"])).unwrap();
        assert_eq!(target.coordination_key, "github.com/schiste/aethyme");
        assert_eq!(target.display_slug, "Schiste/Aethyme");
        assert_eq!(
            target.api_target,
            GithubApiTargetEvidence::NoRepositoryEndpoint
        );
    }

    #[test]
    fn api_repository_endpoints_match_case_insensitively_with_options() {
        let target = resolve_github_target(
            "Schiste/Aethyme",
            &args(&[
                "api",
                "--method",
                "GET",
                "/repos/schiste/AETHYME/issues?state=open",
            ]),
        )
        .unwrap();
        assert_eq!(
            target.api_endpoint_repository.as_deref(),
            Some("schiste/AETHYME")
        );
        assert_eq!(
            target.api_target,
            GithubApiTargetEvidence::MatchedExplicitRepository
        );

        let full_url = resolve_github_target(
            "Schiste/Aethyme",
            &args(&[
                "api",
                "--allow-escape-sequences",
                "https://api.github.com/repos/SCHISTE/aethyme/releases",
            ]),
        )
        .unwrap();
        assert_eq!(
            full_url.api_target,
            GithubApiTargetEvidence::MatchedExplicitRepository
        );
    }

    #[test]
    fn api_repository_placeholders_bind_to_the_outer_target() {
        let target = resolve_github_target(
            "Schiste/Aethyme",
            &args(&["api", "repos/{owner}/{repo}/issues"]),
        )
        .unwrap();
        assert_eq!(
            target.api_endpoint_repository.as_deref(),
            Some("Schiste/Aethyme")
        );
        assert_eq!(
            target.api_target,
            GithubApiTargetEvidence::MatchedRepositoryPlaceholders
        );
    }

    #[test]
    fn api_repository_mismatch_and_incomplete_paths_are_refused() {
        let mismatch = resolve_github_target(
            "Schiste/Aethyme",
            &args(&["api", "repos/Other/Repo/issues"]),
        )
        .unwrap_err();
        assert!(matches!(
            mismatch,
            GithubTargetError::ApiRepositoryMismatch { .. }
        ));
        let incomplete =
            resolve_github_target("Schiste/Aethyme", &args(&["api", "repos/Schiste"])).unwrap_err();
        assert!(matches!(
            incomplete,
            GithubTargetError::IncompleteApiRepositoryEndpoint
        ));
    }

    #[test]
    fn graphql_and_non_repository_api_paths_do_not_invent_a_target() {
        for endpoint in ["graphql", "user", "search/issues?q=repo:Other/Repo"] {
            let target =
                resolve_github_target("Schiste/Aethyme", &args(&["api", endpoint])).unwrap();
            assert_eq!(
                target.api_target,
                GithubApiTargetEvidence::NoRepositoryEndpoint
            );
        }
    }

    #[test]
    fn every_alternate_repository_flag_form_is_refused() {
        for arguments in [
            args(&["pr", "view", "1", "--repo", "Other/Repo"]),
            args(&["pr", "view", "1", "--repo=Other/Repo"]),
            args(&["pr", "view", "1", "-R", "Other/Repo"]),
            args(&["pr", "view", "1", "-R=Other/Repo"]),
            args(&["pr", "view", "1", "-ROther/Repo"]),
            args(&[
                "api",
                "--hostname",
                "github.example",
                "repos/Schiste/Aethyme",
            ]),
            args(&["api", "--hostname=github.example", "repos/Schiste/Aethyme"]),
        ] {
            assert!(matches!(
                resolve_github_target("Schiste/Aethyme", &arguments),
                Err(GithubTargetError::AlternateRepositoryFlag)
            ));
        }
    }
}
