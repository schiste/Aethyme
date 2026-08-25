//! Canonical, credential-free identity for a configured Git remote.
//!
//! A remote's transport spelling is not its coordination identity:
//! `https://host/owner/repo.git`, `ssh://git@host/owner/repo.git`, and
//! `git@host:owner/repo.git` must serialize writes through the same lock.
//! Resolution therefore separates normalized identity from the sanitized
//! fetch/push URLs retained as operator evidence.

use std::path::{Component, Path, PathBuf};

use crate::git::{GitError, GitRepo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteUrlSyntax {
    Http,
    Https,
    Ssh,
    Git,
    Scp,
    File,
    LocalPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteUrlSource {
    GitRemoteGetUrl,
    GitRemoteGetPushUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RemoteUrlEvidence {
    pub source: RemoteUrlSource,
    pub syntax: RemoteUrlSyntax,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteAssertionEvidence {
    NotSupplied,
    MatchedRepositoryPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RemoteIdentityEvidence {
    FetchAndPushMatched,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RemoteResolutionEvidence {
    pub fetch: RemoteUrlEvidence,
    pub push: RemoteUrlEvidence,
    pub identity: RemoteIdentityEvidence,
    pub caller_assertion: RemoteAssertionEvidence,
}

/// Fully resolved identity of one configured Git remote.
///
/// Raw Git configuration values never enter this type. Both URL fields are
/// credential-free normalized renderings safe for plans, journals, and JSON.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ResolvedRemoteTarget {
    pub normalized_host: String,
    pub repository_path: String,
    pub coordination_key: String,
    pub display_slug: String,
    pub remote_name: String,
    pub fetch_url: String,
    pub push_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_assertion: Option<String>,
    pub evidence: RemoteResolutionEvidence,
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteTargetError {
    #[error(transparent)]
    Git(#[from] GitError),
    #[error("remote name must be non-empty and contain no control characters")]
    InvalidRemoteName,
    #[error("{role} remote URL uses an unsupported or invalid Git URL form: {reason}")]
    InvalidUrl {
        role: &'static str,
        reason: &'static str,
    },
    #[error(
        "remote fetch and push URLs identify different repositories ({fetch_key} vs {push_key})"
    )]
    FetchPushMismatch { fetch_key: String, push_key: String },
    #[error("caller repository assertion is invalid: {reason}")]
    InvalidAssertion { reason: &'static str },
    #[error(
        "caller repository assertion {assertion:?} does not match resolved repository {resolved:?}"
    )]
    AssertionMismatch { assertion: String, resolved: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedRemoteUrl {
    normalized_host: String,
    repository_path: String,
    coordination_key: String,
    sanitized_url: String,
    syntax: RemoteUrlSyntax,
}

pub fn resolve_remote_target(
    repo: &GitRepo,
    remote_name: &str,
    caller_assertion: Option<&str>,
) -> Result<ResolvedRemoteTarget, RemoteTargetError> {
    validate_remote_name(remote_name)?;
    let fetch = parse_remote_url(&repo.remote_url(remote_name)?, repo.root(), "fetch")?;
    let push = parse_remote_url(&repo.remote_push_url(remote_name)?, repo.root(), "push")?;
    if fetch.coordination_key != push.coordination_key {
        return Err(RemoteTargetError::FetchPushMismatch {
            fetch_key: fetch.coordination_key,
            push_key: push.coordination_key,
        });
    }

    let caller_assertion = caller_assertion.map(str::trim).map(str::to_string);
    let normalized_assertion = caller_assertion
        .as_deref()
        .map(normalize_assertion)
        .transpose()?;
    let assertion_evidence = match normalized_assertion.as_deref() {
        Some(assertion) if assertion == fetch.repository_path => {
            RemoteAssertionEvidence::MatchedRepositoryPath
        }
        Some(assertion) => {
            return Err(RemoteTargetError::AssertionMismatch {
                assertion: assertion.into(),
                resolved: fetch.repository_path,
            });
        }
        None => RemoteAssertionEvidence::NotSupplied,
    };

    Ok(ResolvedRemoteTarget {
        normalized_host: fetch.normalized_host,
        repository_path: fetch.repository_path.clone(),
        coordination_key: fetch.coordination_key,
        display_slug: fetch.repository_path,
        remote_name: remote_name.into(),
        fetch_url: fetch.sanitized_url,
        push_url: push.sanitized_url,
        caller_assertion,
        evidence: RemoteResolutionEvidence {
            fetch: RemoteUrlEvidence {
                source: RemoteUrlSource::GitRemoteGetUrl,
                syntax: fetch.syntax,
            },
            push: RemoteUrlEvidence {
                source: RemoteUrlSource::GitRemoteGetPushUrl,
                syntax: push.syntax,
            },
            identity: RemoteIdentityEvidence::FetchAndPushMatched,
            caller_assertion: assertion_evidence,
        },
    })
}

fn validate_remote_name(remote_name: &str) -> Result<(), RemoteTargetError> {
    if remote_name.trim().is_empty()
        || remote_name != remote_name.trim()
        || remote_name.starts_with('-')
        || remote_name.chars().any(char::is_whitespace)
    {
        return Err(RemoteTargetError::InvalidRemoteName);
    }
    Ok(())
}

fn normalize_assertion(assertion: &str) -> Result<String, RemoteTargetError> {
    let assertion = assertion.trim();
    if assertion.is_empty()
        || assertion.contains("://")
        || assertion.contains('@')
        || assertion.contains(':')
        || assertion.contains('?')
        || assertion.contains('#')
    {
        return Err(RemoteTargetError::InvalidAssertion {
            reason: "expected a repository path such as owner/repo",
        });
    }
    normalize_repository_path(assertion)
        .map_err(|reason| RemoteTargetError::InvalidAssertion { reason })
}

fn parse_remote_url(
    value: &str,
    repo_root: &Path,
    role: &'static str,
) -> Result<ParsedRemoteUrl, RemoteTargetError> {
    if let Some((scheme, rest)) = value.split_once("://") {
        if scheme.eq_ignore_ascii_case("http") {
            return parse_network_url("http", rest, RemoteUrlSyntax::Http, role);
        }
        if scheme.eq_ignore_ascii_case("https") {
            return parse_network_url("https", rest, RemoteUrlSyntax::Https, role);
        }
        if scheme.eq_ignore_ascii_case("ssh") {
            return parse_network_url("ssh", rest, RemoteUrlSyntax::Ssh, role);
        }
        if scheme.eq_ignore_ascii_case("git+ssh") {
            return parse_network_url("ssh", rest, RemoteUrlSyntax::Ssh, role);
        }
        if scheme.eq_ignore_ascii_case("git") {
            return parse_network_url("git", rest, RemoteUrlSyntax::Git, role);
        }
        if scheme.eq_ignore_ascii_case("file") {
            return parse_file_url(rest, repo_root, role);
        }
        return Err(RemoteTargetError::InvalidUrl {
            role,
            reason: "supported schemes are http, https, ssh, git, and file",
        });
    }
    if looks_like_scp(value) {
        return parse_scp_url(value, role);
    }
    parse_local_path(value, repo_root, RemoteUrlSyntax::LocalPath, role)
}

fn parse_network_url(
    scheme: &'static str,
    rest: &str,
    syntax: RemoteUrlSyntax,
    role: &'static str,
) -> Result<ParsedRemoteUrl, RemoteTargetError> {
    let without_suffix = strip_query_and_fragment(rest);
    let (authority, raw_path) =
        without_suffix
            .split_once('/')
            .ok_or(RemoteTargetError::InvalidUrl {
                role,
                reason: "missing repository path",
            })?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let normalized_host = normalize_host(host, scheme, role)?;
    let repository_path = normalize_repository_path(raw_path)
        .map_err(|reason| RemoteTargetError::InvalidUrl { role, reason })?;
    let coordination_key = format!("{normalized_host}/{repository_path}");
    let sanitized_url = format!("{scheme}://{normalized_host}/{repository_path}");
    Ok(ParsedRemoteUrl {
        normalized_host,
        repository_path,
        coordination_key,
        sanitized_url,
        syntax,
    })
}

fn parse_scp_url(value: &str, role: &'static str) -> Result<ParsedRemoteUrl, RemoteTargetError> {
    let without_suffix = strip_query_and_fragment(value);
    let (authority, raw_path) =
        without_suffix
            .split_once(':')
            .ok_or(RemoteTargetError::InvalidUrl {
                role,
                reason: "missing SCP repository path",
            })?;
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let normalized_host = normalize_host(host, "ssh", role)?;
    let repository_path = normalize_repository_path(raw_path)
        .map_err(|reason| RemoteTargetError::InvalidUrl { role, reason })?;
    Ok(ParsedRemoteUrl {
        coordination_key: format!("{normalized_host}/{repository_path}"),
        sanitized_url: format!("{normalized_host}:{repository_path}"),
        normalized_host,
        repository_path,
        syntax: RemoteUrlSyntax::Scp,
    })
}

fn parse_file_url(
    rest: &str,
    repo_root: &Path,
    role: &'static str,
) -> Result<ParsedRemoteUrl, RemoteTargetError> {
    let without_suffix = strip_query_and_fragment(rest);
    let path = if without_suffix.starts_with('/') {
        without_suffix.to_string()
    } else if let Some((host, path)) = without_suffix.split_once('/') {
        if !matches!(host, "" | "localhost") {
            return Err(RemoteTargetError::InvalidUrl {
                role,
                reason: "file URLs must be local",
            });
        }
        format!("/{path}")
    } else {
        return Err(RemoteTargetError::InvalidUrl {
            role,
            reason: "missing local repository path",
        });
    };
    let mut parsed = parse_local_path(&path, repo_root, RemoteUrlSyntax::File, role)?;
    parsed.sanitized_url = format!("file://{}", parsed.repository_path);
    Ok(parsed)
}

fn parse_local_path(
    value: &str,
    repo_root: &Path,
    syntax: RemoteUrlSyntax,
    role: &'static str,
) -> Result<ParsedRemoteUrl, RemoteTargetError> {
    let value = strip_query_and_fragment(value);
    if value.is_empty() {
        return Err(RemoteTargetError::InvalidUrl {
            role,
            reason: "missing local repository path",
        });
    }
    let path = Path::new(value);
    let absolute = if path.is_absolute() {
        normalize_local_components(path)
    } else {
        normalize_local_components(&repo_root.join(path))
    };
    let mut repository_path = absolute.to_string_lossy().into_owned();
    strip_dot_git(&mut repository_path);
    if repository_path.is_empty() {
        return Err(RemoteTargetError::InvalidUrl {
            role,
            reason: "missing local repository path",
        });
    }
    Ok(ParsedRemoteUrl {
        normalized_host: "local".into(),
        coordination_key: format!("local:{repository_path}"),
        sanitized_url: repository_path.clone(),
        repository_path,
        syntax,
    })
}

fn normalize_local_components(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn normalize_host(
    host: &str,
    scheme: &str,
    role: &'static str,
) -> Result<String, RemoteTargetError> {
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(RemoteTargetError::InvalidUrl {
            role,
            reason: "missing or invalid host",
        });
    }
    let mut host = host.to_ascii_lowercase();
    for suffix in match scheme {
        "http" => [":80", ""],
        "https" => [":443", ""],
        "ssh" => [":22", ""],
        "git" => [":9418", ""],
        _ => ["", ""],
    } {
        if !suffix.is_empty() && host.ends_with(suffix) {
            host.truncate(host.len() - suffix.len());
            break;
        }
    }
    if host.is_empty() {
        return Err(RemoteTargetError::InvalidUrl {
            role,
            reason: "missing or invalid host",
        });
    }
    Ok(host)
}

fn normalize_repository_path(value: &str) -> Result<String, &'static str> {
    let mut value = value.trim_matches('/').to_string();
    strip_dot_git(&mut value);
    if value.is_empty() {
        return Err("missing repository path");
    }
    if value.split('/').any(|part| {
        part.is_empty() || matches!(part, "." | "..") || part.chars().any(char::is_control)
    }) {
        return Err("repository path contains an invalid segment");
    }
    Ok(value)
}

fn strip_dot_git(value: &mut String) {
    while value.ends_with('/') {
        value.pop();
    }
    if value.ends_with(".git") {
        value.truncate(value.len() - 4);
    }
}

fn strip_query_and_fragment(value: &str) -> &str {
    value.split(['?', '#']).next().unwrap_or_default()
}

fn looks_like_scp(value: &str) -> bool {
    let Some((authority, path)) = value.split_once(':') else {
        return false;
    };
    !authority.is_empty()
        && !path.is_empty()
        && !authority.contains('/')
        && !authority.eq_ignore_ascii_case("file")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_syntaxes_share_one_coordination_identity() {
        let root = Path::new("/repo");
        let cases = [
            (
                "https://token:secret@GitHub.COM/Owner/Repo.git?access_token=hidden",
                RemoteUrlSyntax::Https,
                "https://github.com/Owner/Repo",
            ),
            (
                "ssh://git:secret@github.com:22/Owner/Repo.git#ignored",
                RemoteUrlSyntax::Ssh,
                "ssh://github.com/Owner/Repo",
            ),
            (
                "git@GITHUB.com:Owner/Repo.git",
                RemoteUrlSyntax::Scp,
                "github.com:Owner/Repo",
            ),
        ];
        for (url, syntax, sanitized) in cases {
            let parsed = parse_remote_url(url, root, "fetch").unwrap();
            assert_eq!(parsed.normalized_host, "github.com");
            assert_eq!(parsed.repository_path, "Owner/Repo");
            assert_eq!(parsed.coordination_key, "github.com/Owner/Repo");
            assert_eq!(parsed.syntax, syntax);
            assert_eq!(parsed.sanitized_url, sanitized);
            assert!(!parsed.sanitized_url.contains("secret"));
            assert!(!parsed.sanitized_url.contains("token"));
        }
    }

    #[test]
    fn explicit_non_default_ports_remain_part_of_identity() {
        let parsed = parse_remote_url(
            "ssh://git@example.test:2222/team/repo.git",
            Path::new("/repo"),
            "fetch",
        )
        .unwrap();
        assert_eq!(parsed.normalized_host, "example.test:2222");
        assert_eq!(parsed.coordination_key, "example.test:2222/team/repo");
    }

    #[test]
    fn assertions_are_paths_not_urls_or_credentials() {
        assert_eq!(normalize_assertion(" team/repo.git ").unwrap(), "team/repo");
        for assertion in ["https://host/team/repo", "git@host:team/repo", "team:repo"] {
            assert!(normalize_assertion(assertion).is_err(), "{assertion}");
        }
    }
}
