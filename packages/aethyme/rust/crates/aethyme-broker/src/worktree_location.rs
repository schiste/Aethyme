//! An optional repository-configured place for session worktrees.
//!
//! ```toml
//! [worktrees]
//! root = "/Volumes/T7/aethyme-worktrees"
//! min_free_bytes = 8589934592
//! ```
//!
//! The default stays the per-user host-state root. A configured root
//! supersedes it only while it is usable on this machine; because the key is
//! committed, a machine without that path simply keeps the default. The file
//! is read under the same rule as `[promote]`: the copy committed on the
//! fetched default branch, else the main checkout's working tree.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeLocationConfig {
    /// Directory holding one subdirectory per repository. It must already
    /// exist: the broker never creates it, so an unmounted drive cannot be
    /// silently replaced by an empty directory on the boot disk.
    pub root: Option<PathBuf>,
    /// Free space the root must keep before a new session is placed there.
    /// Defaults to the headroom a gate needs to start.
    pub min_free_bytes: Option<u64>,
}

impl WorktreeLocationConfig {
    pub fn min_free_bytes(&self) -> u64 {
        self.min_free_bytes
            .unwrap_or(crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES)
    }

    /// `[worktrees]` from configuration text; `Ok(None)` when absent.
    pub fn from_config_text(text: &str) -> Result<Option<Self>, String> {
        let value = text
            .parse::<toml::Value>()
            .map_err(|error| format!(".aethyme/config.toml is invalid: {error}"))?;
        let Some(section) = value.get("worktrees") else {
            return Ok(None);
        };
        let config: Self = section
            .clone()
            .try_into()
            .map_err(|error| format!("[worktrees] is invalid: {error}"))?;
        if let Some(root) = &config.root
            && !root.is_absolute()
        {
            return Err(format!(
                "[worktrees] root must be an absolute path, got {}",
                root.display()
            ));
        }
        Ok(Some(config))
    }

    /// The configured section for the repository at `main_root`.
    pub fn load(main_root: &Path) -> Result<Option<Self>, String> {
        match crate::merge::repository_config_text(main_root) {
            Some(text) => Self::from_config_text(&text),
            None => Ok(None),
        }
    }
}

/// Why `base` cannot hold worktrees right now, if it cannot.
///
/// Only presence is judged here; free space and repository containment are
/// the caller's, because they depend on the repository being placed.
pub fn base_unavailable_reason(base: &Path) -> Option<String> {
    base_unavailable_reason_with_volumes(base, Path::new("/Volumes"))
}

pub(crate) fn base_unavailable_reason_with_volumes(base: &Path, volumes: &Path) -> Option<String> {
    if !base.is_absolute() {
        return Some(format!("{} is not an absolute path", base.display()));
    }
    // A path on a removable volume must be on that volume. When the drive is
    // unplugged, `/Volumes/<name>` is either absent or a plain directory on the
    // boot disk, and writing there would scatter worktrees onto the wrong disk.
    if let Ok(rest) = base.strip_prefix(volumes)
        && let Some(name) = rest.components().next()
    {
        let mount = volumes.join(name);
        if !mount.is_dir() {
            return Some(format!("volume {} is not mounted", mount.display()));
        }
        if same_device(&mount, volumes) {
            return Some(format!(
                "{} is not a mounted volume (it is a directory on the startup disk)",
                mount.display()
            ));
        }
    }
    if !base.is_dir() {
        return Some(format!(
            "{} does not exist; the broker never creates a configured worktree root",
            base.display()
        ));
    }
    None
}

fn same_device(left: &Path, right: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(left), std::fs::metadata(right)) {
        (Ok(left), Ok(right)) => left.dev() == right.dev(),
        _ => false,
    }
}

/// When `path` lies under the repository's configured worktree root and that
/// root is not available, the reason. A missing worktree there is unreachable,
/// not deleted: callers must keep its records and propose nothing.
pub fn unavailable_configured_location(main_root: &Path, path: &Path) -> Option<String> {
    let root = WorktreeLocationConfig::load(main_root).ok()??.root?;
    // Session rows record canonical paths (`/private/var/...` on macOS) while
    // the configured root is stored as written, and an unplugged root cannot
    // be canonicalized itself -- so both are compared through the nearest
    // ancestor that still exists.
    if !path.starts_with(&root) && !canonical_prefix(path).starts_with(canonical_prefix(&root)) {
        return None;
    }
    base_unavailable_reason(&root).map(|reason| {
        format!("worktree is on the configured worktree root, which is unavailable: {reason}")
    })
}

/// `path` with its nearest existing ancestor canonicalized and the missing
/// remainder re-attached.
fn canonical_prefix(path: &Path) -> PathBuf {
    for ancestor in path.ancestors() {
        if let Ok(canonical) = ancestor.canonicalize() {
            let rest = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return canonical.join(rest);
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_base_is_unavailable_and_is_not_created() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("drive/worktrees");
        let reason = base_unavailable_reason(&base).unwrap();
        assert!(reason.contains("does not exist"), "{reason}");
        assert!(
            !base.exists(),
            "checking availability must not create the root"
        );
    }

    #[test]
    fn an_existing_base_is_available() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(base_unavailable_reason(tmp.path()), None);
    }

    /// An unplugged drive leaves `/Volumes/<name>` absent, or a stale directory
    /// on the startup disk. Either must read as unavailable.
    #[test]
    fn a_volume_that_is_not_a_separate_mount_is_unavailable() {
        let tmp = tempfile::tempdir().unwrap();
        let volumes = tmp.path().join("Volumes");
        std::fs::create_dir_all(volumes.join("T7/worktrees")).unwrap();
        let reason =
            base_unavailable_reason_with_volumes(&volumes.join("T7/worktrees"), &volumes).unwrap();
        assert!(reason.contains("not a mounted volume"), "{reason}");

        let reason =
            base_unavailable_reason_with_volumes(&volumes.join("Gone/worktrees"), &volumes)
                .unwrap();
        assert!(reason.contains("not mounted"), "{reason}");
    }

    #[test]
    fn the_section_parses_and_defaults_its_free_space_floor() {
        let config = WorktreeLocationConfig::from_config_text(
            "[worktrees]\nroot = \"/Volumes/T7/aethyme-worktrees\"\n",
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            config.root.as_deref(),
            Some(Path::new("/Volumes/T7/aethyme-worktrees"))
        );
        assert_eq!(
            config.min_free_bytes(),
            crate::disk_headroom::DEFAULT_GATE_HEADROOM_BYTES
        );
        assert_eq!(
            WorktreeLocationConfig::from_config_text("schema = 1\n").unwrap(),
            None
        );
    }

    #[test]
    fn a_relative_or_unknown_key_is_refused() {
        assert!(
            WorktreeLocationConfig::from_config_text("[worktrees]\nroot = \"worktrees\"\n")
                .unwrap_err()
                .contains("absolute")
        );
        assert!(WorktreeLocationConfig::from_config_text("[worktrees]\npath = \"/x\"\n").is_err());
    }
}
