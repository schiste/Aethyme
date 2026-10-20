//! Canonical graph publication belongs to the reviewed CLI refresh transaction.
//! Standalone producers may write only in isolated repositories. This is a
//! coordination fence, not protection against hostile same-user filesystem races.
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path};

/// A deny-only marker in Git's common metadata survives broker DB overrides.
pub const BROKER_MANAGED_GRAPH_MARKER: &str = "aethyme-broker-managed-graph-v1";

fn refused(reason: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "canonical graph write refused: {reason}; use the reviewed `aethyme graph refresh` workflow"
        ),
    )
}

/// Record that this Git common directory belongs to a mutable broker. This
/// operation only adds a restriction; it never grants a producer write access.
/// Snapshot opens must not call it. Marker contents are diagnostic, not authority.
pub fn mark_broker_managed_graph(git_common_dir: &Path) -> io::Result<()> {
    let common = git_common_dir.canonicalize()?;
    let marker = common.join(BROKER_MANAGED_GRAPH_MARKER);
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
    {
        Ok(mut file) => {
            file.write_all(b"aethyme-broker-managed-graph/v1\n")?;
            file.sync_all()?;
            // Opening/syncing a directory through std is not portable to
            // Windows. The marker file itself is flushed on every platform;
            // directory-entry durability is additionally requested on Unix.
            #[cfg(unix)]
            fs::File::open(&common)?.sync_all()?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(marker)?;
            if metadata.is_file() && !metadata.file_type().is_symlink() {
                Ok(())
            } else {
                Err(refused("managed-graph marker is not a regular file"))
            }
        }
        Err(error) => Err(error),
    }
}

fn exists_without_following(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Refuse raw publication into broker-managed checkouts, including all linked
/// worktrees. Fresh private clones do not inherit Git metadata or ignored DBs.
/// The process environment is deliberately not an exemption or an identity.
pub fn ensure_isolated_graph_write(repo_root: &Path) -> io::Result<()> {
    let root = repo_root.canonicalize()?;
    // Enrollment accepts paths nested beneath a checkout. Resolve the same
    // enclosing markers so a nested output root cannot miss its marker.
    let mut ancestor = root.as_path();
    for depth in 0..256 {
        // Older brokers predate the common-directory marker. Check their state
        // at each enclosing candidate. An unmarked nested .git is not an exemption.
        let legacy = ancestor.join(".aethyme/broker.db");
        ensure_output_path(ancestor, &legacy)?;
        if exists_without_following(&legacy)? {
            return Err(refused("repository contains broker state"));
        }
        let git = ancestor.join(".git");
        match fs::symlink_metadata(&git) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                if exists_without_following(&git.join(BROKER_MANAGED_GRAPH_MARKER))? {
                    return Err(refused("repository is broker-managed"));
                }
            }
            Ok(_) => {
                return Err(refused(
                    "linked or indirect Git metadata requires isolated generation",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let Some(parent) = ancestor.parent() else {
            break;
        };
        if depth == 255 {
            return Err(refused("repository ancestry exceeds component budget"));
        }
        ancestor = parent;
    }
    ensure_output_path(&root, &root.join(".aethyme/graph/_index"))?;
    ensure_output_path(&root, &root.join(".aethyme/graph/_overlays"))
}

/// Recheck the repository fence and a specific output before a mutation.
pub fn ensure_graph_write_target(repo_root: &Path, target: &Path) -> io::Result<()> {
    ensure_isolated_graph_write(repo_root)?;
    ensure_output_path(repo_root, target)
}

/// Check each existing output component without following symbolic links.
/// Recheck immediately before each filesystem mutation. This does not promise
/// atomic exclusion against an uncooperative process replacing a parent path.
pub(crate) fn ensure_output_path(repo_root: &Path, target: &Path) -> io::Result<()> {
    let relative = target
        .strip_prefix(repo_root)
        .map_err(|_| refused("output escapes repository"))?;
    let mut path = repo_root.canonicalize()?;
    let components = relative.components().collect::<Vec<_>>();
    if components.len() > 1024 {
        return Err(refused("output path exceeds component budget"));
    }
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(refused("output path contains a non-normal component"));
        };
        path.push(name);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink()
                    || (index + 1 < components.len() && !metadata.is_dir())
                    || (index + 1 == components.len() && !metadata.is_file() && !metadata.is_dir())
                {
                    return Err(refused("output path contains a link or unsupported file"));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_refuses_raw_writes_without_a_local_database() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        mark_broker_managed_graph(&root.join(".git")).unwrap();
        mark_broker_managed_graph(&root.join(".git")).unwrap();
        assert!(!root.join(".aethyme/broker.db").exists());
        assert_eq!(
            ensure_isolated_graph_write(&root).unwrap_err().kind(),
            io::ErrorKind::PermissionDenied
        );
        assert!(crate::bootstrap_repo(&root, "fixture").is_err());
        assert!(!root.join(".aethyme").exists());
    }

    #[test]
    fn legacy_broker_state_and_linked_worktrees_are_refused() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".aethyme")).unwrap();
        fs::write(root.join(".aethyme/broker.db"), b"fixture").unwrap();
        assert!(ensure_isolated_graph_write(&root).is_err());
        fs::remove_file(root.join(".aethyme/broker.db")).unwrap();
        fs::write(root.join(".git"), b"gitdir: /not-a-write-exemption").unwrap();
        assert!(ensure_isolated_graph_write(&root).is_err());
        assert!(!root.join(".aethyme/graph").exists());
    }

    #[test]
    fn nested_roots_cannot_hide_an_ancestor_repository_fence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        let nested = root.join("nested/output");
        fs::create_dir_all(&nested).unwrap();
        mark_broker_managed_graph(&root.join(".git")).unwrap();
        assert!(crate::bootstrap_repo(&nested, "fixture").is_err());
        assert!(!nested.join(".aethyme").exists());
        // Nested Git metadata grants no exemption from the enclosing managed
        // checkout, whether it is empty or belongs to another repository.
        fs::create_dir(nested.join(".git")).unwrap();
        assert!(crate::bootstrap_repo(&nested, "fixture").is_err());
        assert!(!nested.join(".aethyme").exists());
        fs::remove_dir(nested.join(".git")).unwrap();
        fs::remove_file(root.join(".git").join(BROKER_MANAGED_GRAPH_MARKER)).unwrap();
        fs::create_dir(root.join(".aethyme")).unwrap();
        fs::write(root.join(".aethyme/broker.db"), b"legacy").unwrap();
        assert!(crate::bootstrap_repo(&nested, "fixture").is_err());
        assert!(!nested.join(".aethyme").exists());
    }

    #[test]
    fn raw_shards_coverage_and_manifest_refuse_before_side_effects() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        mark_broker_managed_graph(&root.join(".git")).unwrap();
        let fragment = crate::Fragment::new("source.rs", vec![], vec![]).unwrap();
        let overlay =
            crate::OverlayFragment::new("fixture", "fixture", Vec::<String>::new()).unwrap();
        assert!(crate::write_fragment(&root, "source.rs", &fragment).is_err());
        assert!(crate::write_overlay(&root, "fixture", &overlay).is_err());
        assert!(crate::write_index_shard(&root, "fixture", &[]).is_err());
        let coverage = crate::GraphCoverage::unavailable("fixture");
        assert!(crate::write_coverage_artifacts(&root, &coverage, &[]).is_err());
        assert!(
            crate::write_graph_authority_manifest(&root, "HEAD", "fixture", "fixture").is_err()
        );
        assert!(!root.join(".aethyme").exists());
    }

    #[test]
    fn isolated_generation_remains_available() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        crate::bootstrap_repo(&root, "fixture").unwrap();
        assert_eq!(
            fs::read_to_string(root.join(".aethyme/engine-version")).unwrap(),
            "fixture\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn nested_links_and_preexisting_temporary_links_are_refused() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        crate::bootstrap_repo(&root, "fixture").unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join(".aethyme/graph/package")).unwrap();
        let fragment = crate::Fragment::new("package/source.rs", vec![], vec![]).unwrap();
        assert!(crate::write_fragment(&root, "package/source.rs", &fragment).is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);

        let sentinel = outside.path().join("keep");
        fs::write(&sentinel, b"preserved").unwrap();
        let target = root.join(".aethyme/graph/_index/safe.ndjson");
        let mut temporary = target.as_os_str().to_owned();
        temporary.push(format!(".tmp.{}", std::process::id()));
        symlink(&sentinel, std::path::PathBuf::from(temporary)).unwrap();
        assert!(crate::write_index_shard(&root, "safe", &[]).is_err());
        assert_eq!(fs::read(&sentinel).unwrap(), b"preserved");
        assert!(!target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn linked_output_never_writes_through() {
        use std::os::unix::fs::symlink;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let outside = tempfile::tempdir().unwrap();
        symlink(outside.path(), root.join(".aethyme")).unwrap();
        assert!(crate::bootstrap_repo(&root, "fixture").is_err());
        assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
