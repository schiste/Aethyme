//! Binary fragment encoder / decoder (bincode 1).
//!
//! These functions are pure byte-in / byte-out — no file I/O. The
//! caller is responsible for writing the bytes to disk at the right
//! path and reading them back. This keeps the determinism contract
//! testable in-process and lets the storage layout module (commit
//! 2.5) own filesystem details separately.

use crate::fragment::Fragment;

/// Encode a Fragment as bincode bytes.
pub fn write_fragment_bytes(
    fragment: &Fragment,
) -> Result<Vec<u8>, FragmentEncodeError> {
    bincode::serialize(fragment).map_err(|e| FragmentEncodeError::Bincode {
        message: e.to_string(),
    })
}

/// Decode bincode bytes into a Fragment.
///
/// On schema-version mismatch, returns a structured error rather
/// than attempting any forward/backward compatibility. Per the
/// "no schema versioning" rule, version bumps are forever-format
/// changes; loud failure is the right behavior.
pub fn read_fragment_bytes(
    bytes: &[u8],
) -> Result<Fragment, FragmentDecodeError> {
    let fragment: Fragment = bincode::deserialize(bytes).map_err(|e| {
        FragmentDecodeError::Bincode {
            message: e.to_string(),
        }
    })?;
    if fragment.schema_version() != Fragment::SCHEMA_VERSION {
        return Err(FragmentDecodeError::SchemaVersionMismatch {
            expected: Fragment::SCHEMA_VERSION,
            found: fragment.schema_version(),
        });
    }
    Ok(fragment)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentEncodeError {
    Bincode { message: String },
}

impl std::fmt::Display for FragmentEncodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bincode { message } => {
                write!(f, "Fragment encode failed: {message}")
            }
        }
    }
}

impl std::error::Error for FragmentEncodeError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FragmentDecodeError {
    Bincode {
        message: String,
    },
    /// The decoded fragment's schema_version differs from the
    /// reader's. Re-index from source to get a current fragment.
    SchemaVersionMismatch {
        expected: u32,
        found: u32,
    },
}

impl std::fmt::Display for FragmentDecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Bincode { message } => {
                write!(f, "Fragment decode failed: {message}")
            }
            Self::SchemaVersionMismatch { expected, found } => write!(
                f,
                "Fragment schema_version mismatch: reader expects \
                 {expected}, fragment was written with {found}. \
                 Re-index from source.",
            ),
        }
    }
}

impl std::error::Error for FragmentDecodeError {}
