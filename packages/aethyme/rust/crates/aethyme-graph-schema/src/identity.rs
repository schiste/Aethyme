//! Node identifiers (content-addressed via BLAKE3).
//!
//! A [`NodeId`] is a stable opaque handle for a node, formatted as
//! `<kind>:<repo>:<hash>`:
//!
//! - `<kind>` is the canonical snake_case name of the node's kind
//!   (see [`crate::NodeKind::name`]).
//! - `<repo>` is the repo's stable namespace identifier. Validated at
//!   construction to contain no `:` characters (otherwise the
//!   three-component format would be ambiguous to parse).
//! - `<hash>` is BLAKE3 truncated to 128 bits, base32-encoded
//!   (RFC 4648 alphabet, lowercase, no padding) — exactly 26 chars.
//!
//! ## What the ID survives
//!
//! Hash inputs are the **identifier tuple** `(repo, file_path,
//! symbol_name, kind_name)`. Body content is deliberately not in the
//! hash. This means:
//!
//! | Operation | ID survives? |
//! |---|---|
//! | Edit a function body (no name change) | ✅ |
//! | Edit an unrelated function in the same file | ✅ |
//! | Reformat whitespace | ✅ |
//! | Move a function to a different file | ❌ |
//! | Rename a function in place | ❌ |
//! | Change kind (refactor function → method) | ❌ |
//!
//! Rename tracking is intentionally NOT in scope (per the design
//! conversation's topic 2.2). Renames produce new IDs; consumers cope
//! via their own cross-rename heuristics if they need to.
//!
//! ## Determinism
//!
//! The hashing path is byte-deterministic. Three discipline choices
//! enforce this:
//!
//! 1. Inputs are length-prefixed (u64 little-endian length + bytes)
//!    so concatenation ambiguity cannot leak (`repo="foo"+path="bar"`
//!    vs `repo="foob"+path="ar"` produce different hashes).
//! 2. The order of the four inputs is fixed forever: repo, file_path,
//!    symbol_name, kind. Reordering would change every existing ID
//!    invisibly — exactly the forever-mistake "no schema versioning"
//!    forbids.
//! 3. The base32 alphabet is the standard RFC 4648 (lowercased), not
//!    Crockford or another variant. The schema doc previously
//!    specified Crockford; we settled on RFC 4648 lowercase because
//!    (a) `data-encoding` ships it as `BASE32_NOPAD` without a custom
//!    alphabet table, (b) the ambiguous-character benefit of
//!    Crockford (0/O, 1/I/l avoidance) is moot when the alphabet is
//!    case-normalized and we display IDs in monospace, and (c) RFC
//!    4648 is the more universally understood format.

use blake3::Hasher;
use data_encoding::BASE32_NOPAD;
use serde::{Deserialize, Deserializer, Serialize};

use crate::NodeKind;

/// Exactly 26 base32 characters (the encoded form of a 128-bit hash).
const HASH_SUFFIX_LEN: usize = 26;

/// A content-addressed node identifier.
///
/// Constructed via [`NodeId::new`] or [`NodeId::parse`]. The wire form
/// (e.g. for serialization to NDJSON shards or for log lines) is
/// [`NodeId::as_str`]. The component accessors ([`NodeId::kind`],
/// [`NodeId::repo`], [`NodeId::hash_suffix`]) parse on demand from the
/// canonical string; no parsed-form cache.
///
/// `Box<str>` rather than `String` for memory compactness (16 bytes
/// vs. 24 on 64-bit). Most consumers will store many NodeIds in
/// collections, so the per-instance saving compounds.
///
/// `Serialize` is `#[serde(transparent)]` (emits as a bare JSON
/// string). `Deserialize` is implemented manually to route through
/// [`NodeId::parse`], so the invariant that every `NodeId` has a
/// well-formed canonical shape is enforced even at deserialization
/// boundaries. Without this, an untrusted NDJSON shard could
/// produce a `NodeId` whose [`NodeId::kind`] would panic at runtime.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct NodeId(Box<str>);

impl<'de> Deserialize<'de> for NodeId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Deserialize the transparent string form, then route through
        // parse to enforce the canonical-shape invariant.
        let s = <String as Deserialize>::deserialize(d)?;
        NodeId::parse(&s).map_err(serde::de::Error::custom)
    }
}

impl NodeId {
    /// Construct a NodeId for a symbol at a given path with a given name.
    ///
    /// Returns [`NodeIdConstructionError`] if `repo` is empty or
    /// contains `:`. Other inputs (file_path, symbol_name) are not
    /// validated at this layer — semantic validity per-kind is the
    /// caller's responsibility (e.g., the `File` struct constructor in
    /// commit 1.6 will enforce that File IDs have a non-empty
    /// file_path).
    ///
    /// The construction is deterministic: same inputs always produce
    /// the same NodeId, on any machine, any OS, any build of the
    /// crate. This is the foundation for Option C's per-file fragment
    /// merge story.
    pub fn new(
        kind: NodeKind,
        repo: &str,
        file_path: &str,
        symbol_name: &str,
    ) -> Result<Self, NodeIdConstructionError> {
        if repo.is_empty() {
            return Err(NodeIdConstructionError::EmptyRepo);
        }
        if repo.contains(':') {
            return Err(NodeIdConstructionError::RepoContainsColon { given: repo.into() });
        }

        let hash = compute_hash(kind, repo, file_path, symbol_name);
        let encoded = BASE32_NOPAD.encode(&hash).to_lowercase();
        debug_assert_eq!(
            encoded.len(),
            HASH_SUFFIX_LEN,
            "base32 encoding of 16 bytes should be 26 chars"
        );
        let id = format!("{}:{}:{}", kind.name(), repo, encoded);
        Ok(NodeId(id.into_boxed_str()))
    }

    /// Parse a NodeId from its canonical string form.
    ///
    /// Validates the three-component shape, the kind component
    /// (must be a known [`NodeKind`]), and the hash suffix
    /// (must be exactly 26 chars of lowercase base32). Does not
    /// verify that the hash was computed over the inputs that
    /// would round-trip to this kind/repo — that's not knowable
    /// from the ID alone.
    pub fn parse(s: &str) -> Result<Self, NodeIdParseError> {
        let parts: Vec<&str> = s.splitn(3, ':').collect();
        if parts.len() != 3 {
            return Err(NodeIdParseError::WrongComponentCount {
                given: s.into(),
                found: parts.len(),
            });
        }
        let (kind_str, repo, hash) = (parts[0], parts[1], parts[2]);

        if kind_str.is_empty() {
            return Err(NodeIdParseError::EmptyKindComponent { given: s.into() });
        }
        // Validate kind is a known variant — this is the strongest
        // cheap check we can do without recomputing the hash.
        NodeKind::from_name(kind_str).map_err(|_| NodeIdParseError::UnknownKind {
            given: s.into(),
            kind_str: kind_str.into(),
        })?;

        if repo.is_empty() {
            return Err(NodeIdParseError::EmptyRepoComponent { given: s.into() });
        }

        if hash.len() != HASH_SUFFIX_LEN {
            return Err(NodeIdParseError::WrongHashLength {
                given: s.into(),
                hash_len: hash.len(),
            });
        }
        // Validate hash chars are lowercase base32 alphabet
        // (RFC 4648 alphabet, lowercased = abcdefghijklmnopqrstuvwxyz234567).
        for c in hash.chars() {
            if !is_lowercase_base32_char(c) {
                return Err(NodeIdParseError::InvalidHashChar {
                    given: s.into(),
                    invalid_char: c,
                });
            }
        }

        Ok(NodeId(s.to_owned().into_boxed_str()))
    }

    /// The canonical string form (`<kind>:<repo>:<hash>`).
    ///
    /// This is the form that appears in fragments, on the wire, and
    /// in log lines. Equal NodeIds always have equal `as_str()`
    /// output (the inverse holds too: equal strings ↔ equal IDs).
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The node kind, parsed from the leading `<kind>:` component.
    ///
    /// This is infallible at the API level because [`NodeId::new`]
    /// and [`NodeId::parse`] both validate the kind component at
    /// construction time. Panics here would indicate a NodeId
    /// constructed by some non-canonical path — which is impossible
    /// given the private field.
    pub fn kind(&self) -> NodeKind {
        let kind_str = self.0.split_once(':').map(|(k, _)| k).unwrap_or("");
        NodeKind::from_name(kind_str)
            .expect("NodeId invariant: kind component is always a known NodeKind")
    }

    /// The repo namespace, parsed from the middle `:<repo>:` component.
    pub fn repo(&self) -> &str {
        // Skip past the first colon (after kind), then up to the
        // next colon.
        let after_kind = self.0.split_once(':').map(|(_, r)| r).unwrap_or("");
        after_kind.split_once(':').map(|(r, _)| r).unwrap_or("")
    }

    /// The base32 hash suffix, the last 26 chars of the canonical form.
    pub fn hash_suffix(&self) -> &str {
        let len = self.0.len();
        debug_assert!(len >= HASH_SUFFIX_LEN);
        &self.0[len - HASH_SUFFIX_LEN..]
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Compute the 128-bit BLAKE3 hash over the length-prefixed identifier
/// tuple. The order of inputs is fixed forever (no schema versioning):
/// repo → file_path → symbol_name → kind_name.
///
/// Length-prefixing prevents concatenation ambiguity: without it,
/// repo="foo"+path="bar" hashes the same as repo="foob"+path="ar".
fn compute_hash(kind: NodeKind, repo: &str, file_path: &str, symbol_name: &str) -> [u8; 16] {
    let mut hasher = Hasher::new();
    write_length_prefixed(&mut hasher, repo.as_bytes());
    write_length_prefixed(&mut hasher, file_path.as_bytes());
    write_length_prefixed(&mut hasher, symbol_name.as_bytes());
    write_length_prefixed(&mut hasher, kind.name().as_bytes());
    let full = hasher.finalize();
    let mut out = [0u8; 16];
    out.copy_from_slice(&full.as_bytes()[..16]);
    out
}

fn write_length_prefixed(hasher: &mut Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

fn is_lowercase_base32_char(c: char) -> bool {
    matches!(c, 'a'..='z' | '2'..='7')
}

/// Error returned by [`NodeId::new`] when input validation fails.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeIdConstructionError {
    /// `repo` was the empty string.
    EmptyRepo,
    /// `repo` contained a `:` character, which would make the
    /// resulting `<kind>:<repo>:<hash>` form ambiguous to parse.
    RepoContainsColon { given: Box<str> },
}

impl std::fmt::Display for NodeIdConstructionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeIdConstructionError::EmptyRepo => {
                f.write_str("NodeId construction: repo must not be empty")
            }
            NodeIdConstructionError::RepoContainsColon { given } => {
                write!(
                    f,
                    "NodeId construction: repo {given:?} contains ':', \
                     which would make the parsed form ambiguous"
                )
            }
        }
    }
}

impl std::error::Error for NodeIdConstructionError {}

/// Error returned by [`NodeId::parse`] when the input string is not a
/// well-formed NodeId.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeIdParseError {
    /// The input did not have exactly three colon-separated components.
    WrongComponentCount { given: Box<str>, found: usize },
    /// The first component (before the first `:`) was empty.
    EmptyKindComponent { given: Box<str> },
    /// The first component was non-empty but did not match any known
    /// [`NodeKind`].
    UnknownKind { given: Box<str>, kind_str: Box<str> },
    /// The middle component (between the two `:`) was empty.
    EmptyRepoComponent { given: Box<str> },
    /// The last component was not exactly 26 chars.
    WrongHashLength { given: Box<str>, hash_len: usize },
    /// The last component contained a char outside the lowercase
    /// base32 alphabet.
    InvalidHashChar { given: Box<str>, invalid_char: char },
}

impl std::fmt::Display for NodeIdParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NodeIdParseError::WrongComponentCount { given, found } => write!(
                f,
                "NodeId parse: expected 3 colon-separated components, \
                 found {found} in {given:?}"
            ),
            NodeIdParseError::EmptyKindComponent { given } => write!(
                f,
                "NodeId parse: kind component (before first ':') is \
                 empty in {given:?}"
            ),
            NodeIdParseError::UnknownKind { given, kind_str } => write!(
                f,
                "NodeId parse: kind component {kind_str:?} is not a \
                 known NodeKind in {given:?}"
            ),
            NodeIdParseError::EmptyRepoComponent { given } => write!(
                f,
                "NodeId parse: repo component (between the two ':') is \
                 empty in {given:?}"
            ),
            NodeIdParseError::WrongHashLength { given, hash_len } => write!(
                f,
                "NodeId parse: hash component should be exactly \
                 {HASH_SUFFIX_LEN} chars, got {hash_len} in {given:?}"
            ),
            NodeIdParseError::InvalidHashChar {
                given,
                invalid_char,
            } => write!(
                f,
                "NodeId parse: hash component contains invalid char \
                 {invalid_char:?} (lowercase base32 alphabet only) in \
                 {given:?}"
            ),
        }
    }
}

impl std::error::Error for NodeIdParseError {}
