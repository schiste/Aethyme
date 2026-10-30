//! Filesystem capabilities used by platform-sensitive fixtures.

use std::io;
use std::path::{Path, PathBuf};

/// Create a file whose name contains invalid UTF-8 bytes.
///
/// `Ok(None)` is returned only when macOS reports `EILSEQ` for the exact
/// filename and a directory scan confirms that no entry was created. Every
/// other filesystem error is returned to the caller.
pub fn write_non_utf8_file(
    directory: &Path,
    filename: &[u8],
    contents: &[u8],
) -> io::Result<Option<PathBuf>> {
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;

    if filename.is_empty()
        || filename.contains(&0)
        || filename.contains(&b'/')
        || std::str::from_utf8(filename).is_ok()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "fixture name must be one invalid-UTF-8 path component",
        ));
    }

    let path = directory.join(std::ffi::OsStr::from_bytes(filename));
    let mut file = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if macos_rejects_invalid_path(&error) => {
            for entry in fs::read_dir(directory)? {
                if entry?.file_name().as_bytes() == filename {
                    return Err(io::Error::other(
                        "filesystem returned EILSEQ but created the invalid-byte entry",
                    ));
                }
            }
            eprintln!(
                "unsupported: macOS returned EILSEQ for invalid-byte fixture name {:?}; no entry was created",
                filename
            );
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    file.write_all(contents)?;
    Ok(Some(path))
}

#[cfg(target_os = "macos")]
fn macos_rejects_invalid_path(error: &io::Error) -> bool {
    error.raw_os_error() == Some(libc::EILSEQ)
}

#[cfg(not(target_os = "macos"))]
fn macos_rejects_invalid_path(_: &io::Error) -> bool {
    false
}
