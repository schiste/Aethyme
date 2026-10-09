//! Source snapshot identity (`SourceSnapshotId`).
//!
//! A [`SourceSnapshotId`] names an exact set of source files: their raw path
//! bytes, kind and mode, and raw content bytes. Two snapshots have the same ID
//! if and only if they contain byte-identical files at byte-identical paths
//! with the same modes. It is the identity a contribution's base and result
//! (plan §5.2) and an E1 evidence bundle refer to.
//!
//! ## What it is not
//!
//! - **Not a Git object id.** A Git tree id hashes Git's own object encoding
//!   with SHA-1 (or SHA-256 in SHA-256 repositories). This ID hashes raw bytes
//!   with SHA-256 and does not depend on Git, so a snapshot captured from a
//!   filesystem, an archive or a Worker gets the same ID as the same files
//!   committed to Git.
//! - **Not the graph manifest digest.** `GraphAuthorityManifest`'s
//!   `source_tree_sha256` hashes `git ls-tree` records (Git object ids); see the
//!   L0 audit, slice D. Different preimage, different purpose.
//! - **Not execution identity.** Equal source does not imply equal build inputs
//!   (toolchain, environment, generated files); that is #670's
//!   ExecutionSnapshot.
//!
//! ## Encoding (v0)
//!
//! The ID is `sha256:` followed by the lowercase hex SHA-256 of the manifest:
//!
//! ```text
//! manifest = "aethyme source-snapshot v0" NUL
//!            entry*                          ; sorted by raw path bytes
//! entry    = mode SP content-sha256-hex SP path NUL
//! mode     = "100644" | "100755" | "120000"  ; Git's modes for these kinds
//! ```
//!
//! The record layout mirrors `git ls-tree -z`, so it is easy to inspect and
//! to reproduce with standard tools. The NUL terminator is unambiguous because
//! paths containing NUL are rejected. `content-sha256-hex` is the plain SHA-256
//! of the file's bytes (for a symlink, of its target bytes), so any single
//! entry can be checked with `shasum -a 256`.
//!
//! A one-file snapshot can be checked from a shell. Emit each NUL on its own:
//! in `printf`, a `\0` followed by digits is read as an octal escape, so
//! `'v0\0100644'` silently produces `v0@644`.
//!
//! ```sh
//! c=$(printf 'hello\n' | shasum -a 256 | cut -d' ' -f1)
//! { printf 'aethyme source-snapshot v0'; printf '\000'
//!   printf '100644 %s README.md' "$c"; printf '\000'; } | shasum -a 256
//! # d92b8298c08acf7af553350cfd4fa426c784f4274cf4f28d8cac625da1765378
//! ```
//!
//! No normalization is applied: not to line endings, not to Unicode, not to
//! case. Paths sort by their raw bytes. A path the rules below cannot represent
//! is refused, never rewritten (plan §5.3).

use std::collections::HashSet;

use data_encoding::HEXLOWER;
use sha2::{Digest, Sha256};

/// Domain-separation header that starts every v0 manifest.
pub const MANIFEST_HEADER: &[u8] = b"aethyme source-snapshot v0\0";

/// Prefix of every encoded [`SourceSnapshotId`]: the digest algorithm.
pub const ID_PREFIX: &str = "sha256:";

/// Longest accepted path, in bytes. Longer paths are refused, not truncated.
pub const MAX_PATH_BYTES: usize = 4096;

/// The kind and mode of one entry. v0 supports exactly the three kinds a
/// plain checkout can materialize without extra machinery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// A non-executable regular file (Git mode `100644`).
    Regular,
    /// An executable regular file (Git mode `100755`).
    Executable,
    /// A symbolic link; its content is the raw link-target bytes (Git mode
    /// `120000`).
    Symlink,
}

impl EntryKind {
    /// The Git mode string that encodes this kind in the manifest.
    pub fn git_mode(self) -> &'static str {
        match self {
            Self::Regular => "100644",
            Self::Executable => "100755",
            Self::Symlink => "120000",
        }
    }

    /// Parse a Git mode string as printed by `git ls-tree`. Submodules
    /// (`160000`), trees and anything else are refused explicitly: v0 cannot
    /// identify their content, and silently skipping them would misreport a
    /// partial snapshot as complete.
    pub fn from_git_mode(mode: &str) -> Result<Self, SourceSnapshotError> {
        match mode {
            "100644" => Ok(Self::Regular),
            "100755" => Ok(Self::Executable),
            "120000" => Ok(Self::Symlink),
            other => Err(SourceSnapshotError::UnsupportedMode {
                mode: other.to_string(),
            }),
        }
    }
}

/// One file in a snapshot: raw path bytes, kind, and the SHA-256 of its raw
/// content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    path: Vec<u8>,
    kind: EntryKind,
    content_sha256: [u8; 32],
}

impl SourceEntry {
    /// Build an entry from the file's raw bytes.
    pub fn from_content(path: impl Into<Vec<u8>>, kind: EntryKind, content: &[u8]) -> Self {
        Self::from_content_digest(path, kind, Sha256::digest(content).into())
    }

    /// Build an entry from a SHA-256 the caller already computed, for callers
    /// that stream large files instead of holding them in memory.
    pub fn from_content_digest(
        path: impl Into<Vec<u8>>,
        kind: EntryKind,
        content_sha256: [u8; 32],
    ) -> Self {
        Self {
            path: path.into(),
            kind,
            content_sha256,
        }
    }

    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub fn kind(&self) -> EntryKind {
        self.kind
    }

    pub fn content_sha256(&self) -> &[u8; 32] {
        &self.content_sha256
    }
}

/// A validated set of entries in canonical (raw path byte) order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSnapshot {
    entries: Vec<SourceEntry>,
}

impl SourceSnapshot {
    /// Validate and canonically order `entries`. Input order does not matter;
    /// the first rule an entry breaks is reported.
    pub fn new(
        entries: impl IntoIterator<Item = SourceEntry>,
    ) -> Result<Self, SourceSnapshotError> {
        let mut entries: Vec<SourceEntry> = entries.into_iter().collect();
        for entry in &entries {
            validate_path(&entry.path)?;
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        for pair in entries.windows(2) {
            if pair[0].path == pair[1].path {
                return Err(SourceSnapshotError::DuplicatePath {
                    path: pair[0].path.clone(),
                });
            }
        }
        check_file_directory_conflicts(&entries)?;
        check_case_fold_collisions(&entries)?;
        Ok(Self { entries })
    }

    /// Entries in canonical order.
    pub fn entries(&self) -> &[SourceEntry] {
        &self.entries
    }

    /// The exact bytes the ID hashes. Exposed so another implementation can
    /// compare its manifest byte for byte, not just its final digest.
    pub fn manifest_bytes(&self) -> Vec<u8> {
        let mut manifest = MANIFEST_HEADER.to_vec();
        for entry in &self.entries {
            manifest.extend_from_slice(entry.kind.git_mode().as_bytes());
            manifest.push(b' ');
            manifest.extend_from_slice(HEXLOWER.encode(&entry.content_sha256).as_bytes());
            manifest.push(b' ');
            manifest.extend_from_slice(&entry.path);
            manifest.push(0);
        }
        manifest
    }

    pub fn id(&self) -> SourceSnapshotId {
        let digest = Sha256::digest(self.manifest_bytes());
        SourceSnapshotId(format!("{ID_PREFIX}{}", HEXLOWER.encode(&digest)))
    }
}

/// The encoded identity of a [`SourceSnapshot`]: `sha256:<64 lowercase hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SourceSnapshotId(String);

impl SourceSnapshotId {
    /// Parse an encoded ID. Only the exact canonical form is accepted:
    /// uppercase hex, other algorithms or other lengths are refused rather
    /// than normalized.
    pub fn parse(encoded: &str) -> Result<Self, SourceSnapshotError> {
        let well_formed = encoded.strip_prefix(ID_PREFIX).is_some_and(|hex| {
            hex.len() == 64
                && hex
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        });
        if well_formed {
            Ok(Self(encoded.to_string()))
        } else {
            Err(SourceSnapshotError::MalformedId {
                encoded: encoded.to_string(),
            })
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SourceSnapshotId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a path cannot be part of a v0 snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathRejection {
    #[error("the path is empty")]
    Empty,
    #[error("the path is longer than {MAX_PATH_BYTES} bytes")]
    TooLong,
    #[error("the path contains a NUL byte")]
    ContainsNul,
    #[error("the path is absolute")]
    Absolute,
    #[error("the path ends with '/'")]
    TrailingSlash,
    #[error("the path has an empty component ('//')")]
    EmptyComponent,
    #[error("the path has a '.' or '..' component")]
    RelativeComponent,
    #[error("the path has a '.git' component")]
    GitComponent,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceSnapshotError {
    #[error("invalid source path {}: {reason}", lossy(.path))]
    InvalidPath {
        path: Vec<u8>,
        reason: PathRejection,
    },
    #[error("duplicate source path {}", lossy(.path))]
    DuplicatePath { path: Vec<u8> },
    #[error("{} is both a file and a directory", lossy(.path))]
    FileDirectoryConflict { path: Vec<u8> },
    #[error(
        "{} and {} differ only by letter case, so they cannot both be checked out on a case-insensitive filesystem",
        lossy(.first),
        lossy(.second)
    )]
    CaseFoldCollision { first: Vec<u8>, second: Vec<u8> },
    #[error("unsupported entry mode {mode}: v0 supports only 100644, 100755 and 120000")]
    UnsupportedMode { mode: String },
    #[error("malformed source snapshot id {encoded:?}: expected sha256:<64 lowercase hex>")]
    MalformedId { encoded: String },
}

fn lossy(bytes: &[u8]) -> String {
    format!("{:?}", String::from_utf8_lossy(bytes))
}

fn validate_path(path: &[u8]) -> Result<(), SourceSnapshotError> {
    let reject = |reason| {
        Err(SourceSnapshotError::InvalidPath {
            path: path.to_vec(),
            reason,
        })
    };
    if path.is_empty() {
        return reject(PathRejection::Empty);
    }
    if path.len() > MAX_PATH_BYTES {
        return reject(PathRejection::TooLong);
    }
    if path.contains(&0) {
        return reject(PathRejection::ContainsNul);
    }
    if path[0] == b'/' {
        return reject(PathRejection::Absolute);
    }
    if path.ends_with(b"/") {
        return reject(PathRejection::TrailingSlash);
    }
    for component in path.split(|&b| b == b'/') {
        match component {
            b"" => return reject(PathRejection::EmptyComponent),
            b"." | b".." => return reject(PathRejection::RelativeComponent),
            // Git refuses `.git` in any letter case, because on a
            // case-insensitive filesystem `.GIT/` is the repository directory.
            c if c.eq_ignore_ascii_case(b".git") => return reject(PathRejection::GitComponent),
            _ => {}
        }
    }
    Ok(())
}

/// A path that is a file in one entry and a directory prefix of another
/// (`a` and `a/b`) cannot be materialized; refuse it rather than pick one.
fn check_file_directory_conflicts(entries: &[SourceEntry]) -> Result<(), SourceSnapshotError> {
    let mut directories: HashSet<&[u8]> = HashSet::new();
    for entry in entries {
        let path = entry.path.as_slice();
        for (index, &byte) in path.iter().enumerate() {
            if byte == b'/' {
                directories.insert(&path[..index]);
            }
        }
    }
    match entries
        .iter()
        .find(|e| directories.contains(e.path.as_slice()))
    {
        Some(entry) => Err(SourceSnapshotError::FileDirectoryConflict {
            path: entry.path.clone(),
        }),
        None => Ok(()),
    }
}

/// Decide what to do with paths that differ only by letter case, such as
/// `README.md` and `readme.md`.
///
/// Git and Linux treat them as two files. The default filesystems on macOS
/// (APFS) and Windows (NTFS) are case-insensitive, so checking out such a
/// snapshot there silently keeps one file and loses the other. This is where
/// E1 and the local verifier materialize snapshots.
///
/// Two defensible policies:
/// - **Identity stays faithful:** return `Ok(())`. Every valid Git tree gets an
///   ID, and refusing case-colliding trees becomes the job of materialization
///   (#670's "unsupported ... fail explicitly"). Real repositories do contain
///   such pairs (the Linux kernel's netfilter has `xt_TCPMSS.c` and
///   `xt_tcpmss.c`).
/// - **Refuse at identity:** return `CaseFoldCollision` for the first pair, so
///   no snapshot that cannot round-trip on this host is ever named.
///
/// Entries arrive in raw path byte order, which is not case-insensitive order.
/// If you refuse, decide whether ASCII case folding is enough, or whether
/// non-ASCII letters (`É` vs `é`) count too. (macOS also normalizes Unicode
/// composition in file names, which is a separate collision class.)
fn check_case_fold_collisions(entries: &[SourceEntry]) -> Result<(), SourceSnapshotError> {
    // TODO(#652): choose the policy described above; see the PR discussion.
    let _ = entries;
    Ok(())
}
