//! Stable volume identity for durable filesystem records.
//!
//! A device number (`st_dev`) is assigned when a volume is mounted. macOS
//! gives an external volume a new one on every remount or restart, so a
//! durable record that saved `st_dev` and later compares it exactly declares
//! the same object "different" forever. Durable records identify a volume by
//! its stable identifier instead, and compare device numbers only when either
//! side has no stable identifier, which is exactly today's behaviour.
//!
//! Sources, per platform:
//! - macOS: the filesystem UUID (`getattrlist` `ATTR_VOL_UUID`, no-follow),
//!   read only while the caller's exact (device, inode) object is in place.
//! - Linux and every other target: `Unknown`. A Linux filesystem UUID does not
//!   name one inode namespace (btrfs subvolumes share a UUID and reuse inode
//!   numbers; bind mounts expose one filesystem at several paths), and the
//!   udev `by-uuid` links are not present everywhere (tmpfs, overlayfs and
//!   network mounts have none). Records there keep the exact device check.
//!
//! The lookup never shells out and is deliberately not cached: on macOS the
//! device number comes from the disk slot, so a different disk later mounted
//! at the same slot and mount point (for example a second drive with the same
//! default name) has the same device, mount source and mount point, and a
//! cache keyed on them would hand a long-lived process the old volume's UUID.
//! One `getattrlist` call is a single cheap system call.
#![cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]

use std::path::Path;

/// A volume's stable identifier, or the explicit reason it is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
// Only macOS constructs `Known`; elsewhere every answer is `Unknown`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum VolumeIdentity {
    Known([u8; 16]),
    Unknown(&'static str),
}

impl VolumeIdentity {
    pub(crate) fn uuid(self) -> Option<[u8; 16]> {
        match self {
            Self::Known(uuid) => Some(uuid),
            Self::Unknown(_) => None,
        }
    }
}

/// The same-volume rule for durable records: stable identifiers decide when
/// both sides carry one; otherwise the device numbers must match exactly, so
/// an unknown identifier is never weaker than the device comparison.
pub(crate) fn same_volume(
    saved_uuid: Option<[u8; 16]>,
    saved_device: u64,
    live_uuid: Option<[u8; 16]>,
    live_device: u64,
) -> bool {
    match (saved_uuid, live_uuid) {
        (Some(saved), Some(live)) => saved == live,
        _ => saved_device == live_device,
    }
}

/// Stable identity of the volume holding the object the caller observed at
/// `path` as (`device`, `inode`). Returns `Unknown` instead of guessing.
pub(crate) fn volume_identity(path: &Path, device: u64, inode: u64) -> VolumeIdentity {
    platform::volume_identity(path, device, inode)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::path::Path;

    use super::VolumeIdentity;

    pub(super) fn volume_identity(path: &Path, device: u64, inode: u64) -> VolumeIdentity {
        match crate::worktree_registry::stable_volume_uuid_for_object(path, device, inode) {
            Ok(uuid) if uuid.iter().any(|byte| *byte != 0) => VolumeIdentity::Known(uuid),
            Ok(_) => VolumeIdentity::Unknown("the filesystem returned a nil volume UUID"),
            Err(_) => VolumeIdentity::Unknown(
                "the filesystem did not report a volume UUID for this object",
            ),
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use std::path::Path;

    use super::VolumeIdentity;

    pub(super) fn volume_identity(_path: &Path, _device: u64, _inode: u64) -> VolumeIdentity {
        VolumeIdentity::Unknown("this platform has no stable volume identifier")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_identifiers_decide_when_both_sides_have_one() {
        assert!(same_volume(Some([1; 16]), 10, Some([1; 16]), 20));
        assert!(!same_volume(Some([1; 16]), 10, Some([2; 16]), 10));
    }

    #[test]
    fn an_unknown_identifier_falls_back_to_the_exact_device() {
        assert!(same_volume(None, 10, Some([1; 16]), 10));
        assert!(!same_volume(None, 10, Some([1; 16]), 20));
        assert!(same_volume(Some([1; 16]), 10, None, 10));
        assert!(!same_volume(Some([1; 16]), 10, None, 20));
        assert!(!same_volume(None, 10, None, 20));
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_reports_one_identity_or_an_explicit_unknown() {
        use std::os::unix::fs::MetadataExt;
        let directory = tempfile::tempdir().unwrap();
        let metadata = std::fs::symlink_metadata(directory.path()).unwrap();
        let first = volume_identity(directory.path(), metadata.dev(), metadata.ino());
        let second = volume_identity(directory.path(), metadata.dev(), metadata.ino());
        assert_eq!(first, second, "a repeated lookup must give the same answer");
        #[cfg(target_os = "macos")]
        assert!(matches!(first, VolumeIdentity::Known(_)), "{first:?}");
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(first, VolumeIdentity::Unknown(_)), "{first:?}");
        // A stale observation of the object never yields an identity.
        assert!(matches!(
            volume_identity(directory.path(), metadata.dev(), metadata.ino() + 1),
            VolumeIdentity::Unknown(_)
        ));
    }
}
