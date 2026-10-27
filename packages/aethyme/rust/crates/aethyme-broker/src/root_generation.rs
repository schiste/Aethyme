//! Spawn-time generation markers for linked worktree roots.
//!
//! The marker is an xattr on the checkout root directory. A filesystem inode
//! number can be reused after a checkout is removed; this value distinguishes
//! a later directory at the same path even when its device and inode match.
//! Only spawn initializes it. Cleanup and recovery only read it, so they never
//! grant ownership to a replacement checkout.

use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos"))]
const TOKEN_BYTES: usize = 32;

#[cfg(target_os = "linux")]
const XATTR_NAME: &std::ffi::CStr = c"user.aethyme.root-generation";

#[cfg(target_os = "macos")]
const XATTR_NAME: &std::ffi::CStr = c"com.aethyme.root-generation";

/// Create a fresh marker once on a newly spawned worktree root and return its
/// digest for durable witnesses. Any unsupported filesystem or ambiguous
/// state disables unattended cleanup for this checkout.
pub(crate) fn initialize(path: &Path) -> Option<String> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::io::Read;
        use std::os::fd::AsRawFd;

        let directory = open_directory(path)?;
        let mut token = [0_u8; TOKEN_BYTES];
        std::fs::File::open("/dev/urandom")
            .ok()?
            .read_exact(&mut token)
            .ok()?;

        #[cfg(target_os = "linux")]
        // SAFETY: the descriptor is an open directory, the xattr name is a
        // static NUL-terminated string, and the token buffer remains live for
        // the duration of the call. XATTR_CREATE refuses to replace a marker.
        let result = unsafe {
            libc::fsetxattr(
                directory.as_raw_fd(),
                XATTR_NAME.as_ptr(),
                token.as_ptr().cast(),
                token.len(),
                libc::XATTR_CREATE,
            )
        };

        #[cfg(target_os = "macos")]
        // SAFETY: the descriptor is an open directory, the xattr name is a
        // static NUL-terminated string, and the token buffer remains live for
        // the duration of the call. XATTR_CREATE refuses to replace a marker.
        let result = unsafe {
            libc::fsetxattr(
                directory.as_raw_fd(),
                XATTR_NAME.as_ptr(),
                token.as_ptr().cast(),
                token.len(),
                0,
                libc::XATTR_CREATE,
            )
        };
        if result != 0 {
            return None;
        }

        // SAFETY: `directory` owns a live descriptor for the root whose xattr
        // was just created. Sync it before its digest can be committed to the
        // broker event stream.
        if unsafe { libc::fsync(directory.as_raw_fd()) } != 0 {
            return None;
        }
        let digest = sha256(&token);
        (read_fd(directory.as_raw_fd()).as_deref() == Some(digest.as_str())).then_some(digest)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = path;
        None
    }
}

/// Read the recorded generation from an already pinned root directory.
/// Returning `None` for absent, malformed, or unsupported xattrs is a
/// fail-closed result for automatic cleanup.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn read_fd(fd: std::os::fd::RawFd) -> Option<String> {
    let mut token = [0_u8; TOKEN_BYTES + 1];
    #[cfg(target_os = "linux")]
    // SAFETY: the caller supplies a live descriptor; the name and output
    // buffer are valid for the duration of the call.
    let result = unsafe {
        libc::fgetxattr(
            fd,
            XATTR_NAME.as_ptr(),
            token.as_mut_ptr().cast(),
            token.len(),
        )
    };
    #[cfg(target_os = "macos")]
    // SAFETY: the caller supplies a live descriptor; the name and output
    // buffer are valid for the duration of the call.
    let result = unsafe {
        libc::fgetxattr(
            fd,
            XATTR_NAME.as_ptr(),
            token.as_mut_ptr().cast(),
            token.len(),
            0,
            0,
        )
    };
    (usize::try_from(result).ok()? == TOKEN_BYTES).then(|| sha256(&token[..TOKEN_BYTES]))
}

/// Read a generation marker and the identity of the directory descriptor it
/// was read from, so callers can bind both to one checkout root.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn read_path_with_identity(path: &Path) -> Option<(u64, u64, String)> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;

    let directory = open_directory(path)?;
    let metadata = directory.metadata().ok()?;
    let digest = read_fd(directory.as_raw_fd())?;
    Some((metadata.dev(), metadata.ino(), digest))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn read_path_with_identity(_path: &Path) -> Option<(u64, u64, String)> {
    None
}

pub(crate) fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(bytes))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_directory(path: &Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = std::fs::OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let directory = options.open(path).ok()?;
    directory.metadata().ok()?.is_dir().then_some(directory)
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::{initialize, read_fd, read_path_with_identity, valid_digest};
    use std::os::fd::AsRawFd;

    #[test]
    fn root_generation_is_create_once_and_changes_when_the_root_is_recreated() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("checkout");
        std::fs::create_dir(&root).unwrap();

        let generation = initialize(&root).expect("the test filesystem supports root xattrs");
        assert!(valid_digest(&generation));
        let directory = std::fs::File::open(&root).unwrap();
        assert_eq!(
            read_fd(directory.as_raw_fd()).as_deref(),
            Some(generation.as_str())
        );
        assert!(
            initialize(&root).is_none(),
            "initialization must not overwrite"
        );
        drop(directory);

        std::fs::remove_dir(&root).unwrap();
        std::fs::create_dir(&root).unwrap();
        assert!(read_path_with_identity(&root).is_none());
        let replacement = initialize(&root).expect("the replacement root supports xattrs");
        assert_ne!(replacement, generation);

        let link = temporary.path().join("checkout-link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(read_path_with_identity(&link).is_none());
    }
}
