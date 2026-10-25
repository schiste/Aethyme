//! Strict cooperative generator claims, using the existing supervised host runner.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use unicode_normalization::UnicodeNormalization;

use crate::collaboration_archive::ObjectDigest;
use crate::{Broker, HostResourceKind, HostResourceRequest, HostResourceRequirement};

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error(transparent)]
    Broker(#[from] crate::BrokerOpError),
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error(transparent)]
    Store(#[from] crate::BrokerError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("invalid generated-artifact claim: {0}")]
    Invalid(String),
}

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}

fn normalized_path(path: &str) -> Result<String, Error> {
    crate::broker::normalize_lease_path(path)?;
    if path.ends_with('/') || path.contains(['\\', ':']) || path.chars().any(char::is_control) {
        return Err(invalid(
            "use one repository-relative file path without control characters, colons or backslashes",
        ));
    }
    // Conservatively collide case, Unicode normalization and trailing-dot/space
    // aliases even on filesystems where those names could be distinct.
    path.split('/')
        .map(|component| {
            let component: String = component
                .trim_end_matches(['.', ' '])
                .nfc()
                .flat_map(char::to_lowercase)
                .collect();
            if component.is_empty() || component == ".git" {
                return Err(invalid(
                    "empty or Git-administrative path components are not artifacts",
                ));
            }
            Ok(component)
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|components| components.join("/"))
}

fn reject_aliases(root: &Path, path: &str) -> Result<(), Error> {
    let mut current = root.to_path_buf();
    for component in path.split('/') {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(invalid(
                        "symlink aliases are unsupported; claim the actual repository path",
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt as _;
                    if metadata.is_file() && metadata.nlink() != 1 {
                        return Err(invalid("hard-linked artifacts are unsupported"));
                    }
                }
                if current == root.join(path) && !metadata.is_file() {
                    return Err(invalid(
                        "the artifact must name a file, not a directory or device",
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn resource_request(
    repository: &str,
    session_id: i64,
    worktree_path: &str,
    path: &str,
    nonce: &str,
) -> HostResourceRequest {
    HostResourceRequest {
        schema_version: crate::HOST_RESOURCE_REQUEST_SCHEMA_VERSION,
        request_id: format!("artifact:{nonce}"),
        repository: repository.into(),
        worktree_fingerprint: ObjectDigest::of(worktree_path.as_bytes()).hex(),
        run_id: format!("artifact-session:{session_id}:{nonce}"),
        ttl_seconds: 300,
        holder_pid: Some(std::process::id()),
        resources: vec![HostResourceRequirement {
            key: "generated_artifact".into(),
            resource: HostResourceKind::ExclusiveKey {
                name: format!(
                    "generated-artifact:{repository}:{}",
                    ObjectDigest::of(path.as_bytes()).hex()
                ),
            },
        }],
    }
}

pub(crate) fn request(
    broker: &Broker,
    cwd: &Path,
    session_id: i64,
    path: &str,
) -> Result<HostResourceRequest, Error> {
    let folded = normalized_path(path)?;
    let session = broker.store_ref().session(session_id)?;
    if session.status.is_closed() {
        return Err(invalid("the session is closed"));
    }
    let checkout = crate::GitRepo::discover(cwd)?;
    let root = checkout.root().canonicalize()?;
    if root != PathBuf::from(&session.worktree_path).canonicalize()? || cwd.canonicalize()? != root
    {
        return Err(invalid("run from the named session's own worktree root"));
    }
    reject_aliases(&root, path)?;
    let repository = broker.worktree_root_plan()?.repository_key;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| invalid("clock precedes the request identity epoch"))?
        .as_nanos();
    // This is a public one-shot request ID, not an ownership credential. Any
    // collision is refused by the supervised runner's existing replay fence.
    let nonce = format!(
        "{}:{epoch}:{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    Ok(resource_request(
        &repository,
        session.id,
        &session.worktree_path,
        &folded,
        &nonce,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_keys_collide_for_portable_aliases_and_refuse_ambiguous_paths() {
        assert_eq!(
            normalized_path("Generated/Cafe\u{301}.JSON").unwrap(),
            normalized_path("generated/caf\u{e9}.json. ").unwrap()
        );
        for path in [
            "",
            "/absolute",
            "../x",
            "a/../x",
            "a//x",
            "./a",
            "a/",
            "a\\b",
            "C:foo",
            ".git/config",
            ".GIT/config",
            "a/...",
        ] {
            assert!(normalized_path(path).is_err(), "{path:?}");
        }
    }
    #[test]
    fn sessions_share_one_exclusive_key_and_release_requires_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let mut coordinator =
            crate::HostResourceCoordinator::open(&temp.path().join("resources.db")).unwrap();
        let first = resource_request(
            "repository",
            1,
            "/first-worktree",
            "generated/file.json",
            "first",
        );
        let second = resource_request(
            "repository",
            2,
            "/second-worktree",
            "generated/file.json",
            "second",
        );
        assert_ne!(first.worktree_fingerprint, second.worktree_fingerprint);
        assert_eq!(first.resources, second.resources);
        let grant = coordinator.acquire(&first).unwrap();
        assert!(matches!(
            coordinator.acquire(&second),
            Err(crate::HostResourceError::Conflict { .. })
        ));
        assert!(
            coordinator
                .release(&grant.lease.lease_id, grant.lease.generation, "wrong token")
                .is_err()
        );
        assert!(coordinator.acquire(&second).is_err());
        coordinator
            .release(
                &grant.lease.lease_id,
                grant.lease.generation,
                &grant.ownership_token,
            )
            .unwrap();
        assert!(coordinator.acquire(&second).is_ok());
        let other_repo = resource_request(
            "another-repository",
            3,
            "/third",
            "generated/file.json",
            "third",
        );
        assert!(coordinator.acquire(&other_repo).is_ok());
        let wire = serde_json::to_string(&first).unwrap();
        assert!(!wire.contains("/first-worktree"));
        assert!(first.run_id.starts_with("artifact-session:1:"));
    }

    #[test]
    fn existing_directory_and_symlink_aliases_are_refused() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("directory")).unwrap();
        assert!(reject_aliases(root.path(), "directory").is_err());
        assert!(reject_aliases(root.path(), "new/file.json").is_ok());
        #[cfg(unix)]
        {
            std::fs::write(root.path().join("actual.json"), "{}").unwrap();
            std::os::unix::fs::symlink("actual.json", root.path().join("alias.json")).unwrap();
            assert!(reject_aliases(root.path(), "alias.json").is_err());
            std::fs::hard_link(
                root.path().join("actual.json"),
                root.path().join("hard.json"),
            )
            .unwrap();
            assert!(reject_aliases(root.path(), "actual.json").is_err());
            assert!(reject_aliases(root.path(), "hard.json").is_err());
        }
    }
}
