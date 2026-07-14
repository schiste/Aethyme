//! Building blocks shared across multiple node kinds.
//!
//! Three types live here:
//!
//! - [`SourceRange`] — `(start_line, end_line)` carried by every node
//!   that has a known location in source. Per schema doc §3, every
//!   kind whose `required` fields include `start_line` / `end_line`
//!   carries a `SourceRange`.
//! - [`Visibility`] — `Public` / `Private` / `Protected` / `Module`,
//!   the canonical four-variant taxonomy used by every kind that has
//!   a visibility modifier (classes, functions, methods, fields,
//!   etc.).
//! - [`ModificationHistory`] — bounded ring buffer of the last three
//!   modifications to a node (commit, timestamp, author). Per schema
//!   doc §7.
//!
//! These types live in their own module rather than alongside any
//! one kind so that 24 different node-kind structs (commits
//! 1.7–1.11) can `use` them without circular dependencies.

use serde::{Deserialize, Serialize};

// ─── SourceRange ─────────────────────────────────────────────────────

/// A range of lines in a source file.
///
/// `start_line` and `end_line` are 1-based and inclusive. The range
/// `(1, 1)` is a single line. `end_line` must be `>= start_line`;
/// the constructor validates this.
///
/// `u32` is sufficient for any realistic file (4 billion lines is
/// ~120 PB of source code at 30 bytes per line) and saves 4 bytes
/// vs. `u64` per range. Fragment payloads carry many ranges; the
/// saving compounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize)]
pub struct SourceRange {
    start_line: u32,
    end_line: u32,
}

impl SourceRange {
    /// Construct a SourceRange. Returns [`InvalidSourceRange`] if
    /// `start_line == 0` (lines are 1-based) or
    /// `end_line < start_line`.
    pub const fn new(start_line: u32, end_line: u32) -> Result<Self, InvalidSourceRange> {
        if start_line == 0 {
            return Err(InvalidSourceRange::StartLineZero);
        }
        if end_line < start_line {
            return Err(InvalidSourceRange::EndBeforeStart {
                start_line,
                end_line,
            });
        }
        Ok(SourceRange {
            start_line,
            end_line,
        })
    }

    pub const fn start_line(self) -> u32 {
        self.start_line
    }

    pub const fn end_line(self) -> u32 {
        self.end_line
    }

    /// Number of lines covered by this range (inclusive).
    pub const fn line_count(self) -> u32 {
        self.end_line - self.start_line + 1
    }
}

impl<'de> Deserialize<'de> for SourceRange {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            start_line: u32,
            end_line: u32,
        }
        let raw = Raw::deserialize(d)?;
        SourceRange::new(raw.start_line, raw.end_line).map_err(serde::de::Error::custom)
    }
}

/// Returned by [`SourceRange::new`] when the inputs don't form a
/// valid range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidSourceRange {
    /// `start_line == 0` — lines are 1-based.
    StartLineZero,
    /// `end_line < start_line`.
    EndBeforeStart { start_line: u32, end_line: u32 },
}

impl std::fmt::Display for InvalidSourceRange {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvalidSourceRange::StartLineZero => {
                f.write_str("SourceRange: start_line must be >= 1 (1-based)")
            }
            InvalidSourceRange::EndBeforeStart {
                start_line,
                end_line,
            } => write!(
                f,
                "SourceRange: end_line ({end_line}) < start_line ({start_line})"
            ),
        }
    }
}

impl std::error::Error for InvalidSourceRange {}

// ─── Visibility ──────────────────────────────────────────────────────

/// Symbol visibility, in the canonical four-variant taxonomy.
///
/// Languages with finer-grained visibility map to the closest variant:
/// - Rust `pub(crate)` → `Module`
/// - C# / Java `internal` / package-private → `Module`
/// - Python's `_underscore` convention → `Private` (when respected)
/// - TS access modifiers map 1:1 (public/private/protected)
///
/// Languages without an explicit visibility (e.g., Go's
/// capitalization convention) carry the resolved visibility in this
/// field (uppercased identifier → Public, lowercased → Module).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Accessible everywhere.
    Public,
    /// Accessible only within the defining type / function.
    Private,
    /// Accessible within the defining type and its subtypes.
    Protected,
    /// Accessible only within the defining module / package / crate.
    Module,
}

impl Visibility {
    pub const fn name(self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::Private => "private",
            Visibility::Protected => "protected",
            Visibility::Module => "module",
        }
    }

    pub fn from_name(name: &str) -> Result<Visibility, UnknownVisibility> {
        Ok(match name {
            "public" => Visibility::Public,
            "private" => Visibility::Private,
            "protected" => Visibility::Protected,
            "module" => Visibility::Module,
            _ => return Err(UnknownVisibility { given: name.into() }),
        })
    }
}

/// Every [`Visibility`] in declaration order.
pub const ALL_VISIBILITIES: &[Visibility] = &[
    Visibility::Public,
    Visibility::Private,
    Visibility::Protected,
    Visibility::Module,
];

/// Returned by [`Visibility::from_name`] when the given string does
/// not match any known visibility.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnknownVisibility {
    given: Box<str>,
}

impl UnknownVisibility {
    pub fn given(&self) -> &str {
        &self.given
    }
}

impl std::fmt::Display for UnknownVisibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "unknown visibility: {:?}", self.given)
    }
}

impl std::error::Error for UnknownVisibility {}

// ─── Modification + ModificationHistory ──────────────────────────────

/// A single modification event on a node, as recorded from git.
///
/// `commit` is a git SHA (any length — hex string). `unix_timestamp`
/// is seconds since 1970-01-01 UTC; integer to preserve determinism
/// (no `chrono::DateTime` library version drift, no float).
/// `author` is the commit author name or email (whichever git
/// reports).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Modification {
    pub commit: Box<str>,
    pub unix_timestamp: u64,
    pub author: Box<str>,
}

/// Bounded ring buffer of the last three [`Modification`] entries.
///
/// Per schema doc §7: every node carries a `modification_history` of
/// at most 3 entries, ordered most-recent-first, with evict-oldest
/// semantics. The size cap (3) is a deliberate compromise between
/// "no history" (loses too much) and "full revision history"
/// (storage and complexity explosion). Three covers most "who
/// recently touched this?" queries; deeper history requires
/// consulting git directly.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(transparent)]
pub struct ModificationHistory {
    /// Most recent first. Length 0..=3.
    entries: Vec<Modification>,
}

impl ModificationHistory {
    /// The hard cap on stored entries.
    pub const MAX_ENTRIES: usize = 3;

    /// Construct an empty history.
    pub const fn new() -> Self {
        ModificationHistory {
            entries: Vec::new(),
        }
    }

    /// Construct from a list, taking the first [`Self::MAX_ENTRIES`]
    /// in the given order (which is assumed to be most-recent-first).
    /// Longer inputs are truncated; shorter inputs are accepted as-is.
    pub fn from_most_recent_first(entries: Vec<Modification>) -> Self {
        let mut entries = entries;
        entries.truncate(Self::MAX_ENTRIES);
        ModificationHistory { entries }
    }

    /// Record a new most-recent modification. If the buffer is at
    /// capacity, evicts the oldest entry.
    pub fn push_most_recent(&mut self, m: Modification) {
        self.entries.insert(0, m);
        if self.entries.len() > Self::MAX_ENTRIES {
            self.entries.truncate(Self::MAX_ENTRIES);
        }
    }

    pub fn entries(&self) -> &[Modification] {
        &self.entries
    }

    pub fn most_recent(&self) -> Option<&Modification> {
        self.entries.first()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'de> Deserialize<'de> for ModificationHistory {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let entries = <Vec<Modification> as Deserialize>::deserialize(d)?;
        if entries.len() > Self::MAX_ENTRIES {
            return Err(serde::de::Error::custom(format!(
                "ModificationHistory: deserialized {} entries, but \
                 the buffer caps at {}",
                entries.len(),
                Self::MAX_ENTRIES,
            )));
        }
        Ok(ModificationHistory { entries })
    }
}
