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
//! The lookup never shells out. Results are cached per process, keyed by the
//! device number together with the mount's source and mount point, so a
//! device number later reused by a different mount misses the cache.
#![cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]

use std::path::Path;

/// A volume's stable identifier, or the explicit reason it is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};

    use super::VolumeIdentity;

    const MAX_CACHED_MOUNTS: usize = 64;

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    struct MountKey {
        device: u64,
        mounted_from: Vec<u8>,
        mounted_on: Vec<u8>,
    }

    fn cache() -> &'static Mutex<HashMap<MountKey, [u8; 16]>> {
        static CACHE: OnceLock<Mutex<HashMap<MountKey, [u8; 16]>>> = OnceLock::new();
        CACHE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn c_text(raw: &[libc::c_char]) -> Vec<u8> {
        raw.iter()
            .take_while(|byte| **byte != 0)
            .map(|byte| *byte as u8)
            .collect()
    }

    fn mount_key(path: &Path, device: u64) -> Option<MountKey> {
        use std::os::unix::ffi::OsStrExt;
        let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
        let mut stat = std::mem::MaybeUninit::<libc::statfs>::zeroed();
        // SAFETY: encoded is NUL terminated and stat points to writable
        // storage of the exact type statfs fills; it is read only on success.
        if unsafe { libc::statfs(encoded.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: successful statfs initialized the structure (zeroed too).
        let stat = unsafe { stat.assume_init() };
        Some(MountKey {
            device,
            mounted_from: c_text(&stat.f_mntfromname),
            mounted_on: c_text(&stat.f_mntonname),
        })
    }

    fn object_unchanged(path: &Path, device: u64, inode: u64) -> bool {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).is_ok_and(|metadata| {
            !metadata.file_type().is_symlink()
                && metadata.dev() == device
                && metadata.ino() == inode
        })
    }

    pub(super) fn volume_identity(path: &Path, device: u64, inode: u64) -> VolumeIdentity {
        let Some(key) = mount_key(path, device) else {
            return VolumeIdentity::Unknown("the mount of the object could not be read");
        };
        let cached = cache()
            .lock()
            .ok()
            .and_then(|cache| cache.get(&key).copied());
        if let Some(uuid) = cached {
            if object_unchanged(path, device, inode) {
                return VolumeIdentity::Known(uuid);
            }
            return VolumeIdentity::Unknown("the object changed while its volume was read");
        }
        match crate::worktree_registry::stable_volume_uuid_for_object(path, device, inode) {
            Ok(uuid) if uuid.iter().any(|byte| *byte != 0) => {
                if let Ok(mut cache) = cache().lock() {
                    if cache.len() >= MAX_CACHED_MOUNTS {
                        cache.clear();
                    }
                    cache.insert(key, uuid);
                }
                VolumeIdentity::Known(uuid)
            }
            Ok(_) => VolumeIdentity::Unknown("the filesystem returned a nil volume UUID"),
            Err(_) => VolumeIdentity::Unknown("the filesystem did not report a volume UUID"),
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
        assert_eq!(first, second, "the cached answer must equal the first one");
        #[cfg(target_os = "macos")]
        assert!(matches!(first, VolumeIdentity::Known(_)), "{first:?}");
        #[cfg(not(target_os = "macos"))]
        assert!(matches!(first, VolumeIdentity::Unknown(_)), "{first:?}");
        // A stale observation never borrows the cached identity.
        assert!(matches!(
            volume_identity(directory.path(), metadata.dev(), metadata.ino() + 1),
            VolumeIdentity::Unknown(_)
        ));
    }
}
