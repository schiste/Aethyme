//! The one list of directory names the broker treats as regenerable build
//! output.
//!
//! Three paths delete build output: the unattended sweep and `gc plan`
//! (`gc.rs`), the reviewed `gc reclaim` (`reclaim.rs`), and primary-checkout
//! storage cleanup (`storage.rs`). They used to keep separate lists that
//! disagreed: the sweep knew only `target` and `node_modules`, so a closed
//! session's `.venv` survived every sweep even though `gc reclaim` would have
//! removed it, and no path knew `.pnpm-store`.
//!
//! Every entry here is a name *and* a witness. A reviewed path may act on the
//! name alone, because an operator reads the plan and Git must still report
//! the directory ignored and untracked. The unattended sweep has no reader, so
//! it acts only on entries marked [`ArtifactKind::unattended`], and only once
//! the witness is present.

use std::path::Path;

use crate::retention::is_safe_artefact_directory_name;

/// Evidence that a directory with a catalogued name really is the build
/// output that name suggests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArtifactWitness {
    /// A file the producing tool always writes at the top of the directory.
    File(&'static str),
    /// The directory holds at least one entry. Used where no marker file
    /// exists; an empty directory holds nothing worth reclaiming.
    NonEmptyDirectory,
}

impl ArtifactWitness {
    /// The entry that must outlive the rest of the removal, if there is one.
    ///
    /// A witness file classifies the directory, so taking it first turns an
    /// interrupted removal into a directory GC can no longer explain. A
    /// directory witnessed only by being non-empty needs no such care: while
    /// anything is left it still witnesses itself.
    pub(crate) fn deferrable_entry(self) -> Option<&'static str> {
        match self {
            Self::File(name) => Some(name),
            Self::NonEmptyDirectory => None,
        }
    }

    pub(crate) fn confirms(self, path: &Path) -> bool {
        match self {
            Self::File(name) => path.join(name).is_file(),
            Self::NonEmptyDirectory => std::fs::read_dir(path)
                .map(|mut entries| entries.next().is_some())
                .unwrap_or(false),
        }
    }
}

/// One catalogued build-output directory name.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ArtifactKind {
    pub name: &'static str,
    pub witness: ArtifactWitness,
    /// Whether the unattended sweep may remove it without an operator reading
    /// a plan first. False for names generic enough that a project may use
    /// them for something it cannot rebuild.
    pub unattended: bool,
}

/// The built-in catalog. Deliberately narrow: "large and ignored" would also
/// match a downloaded dataset or a local database nobody can rebuild, and
/// every path that reads this list deletes things.
pub(crate) const CATALOG: &[ArtifactKind] = &[
    // Cargo writes `CACHEDIR.TAG` into every target directory it creates.
    ArtifactKind {
        name: "target",
        witness: ArtifactWitness::File("CACHEDIR.TAG"),
        unattended: true,
    },
    // npm, pnpm and yarn all reinstall it from the lockfile.
    ArtifactKind {
        name: "node_modules",
        witness: ArtifactWitness::NonEmptyDirectory,
        unattended: true,
    },
    // `venv` and `uv venv` both write `pyvenv.cfg`; without it the directory
    // is not a virtual environment and is left alone.
    ArtifactKind {
        name: ".venv",
        witness: ArtifactWitness::File("pyvenv.cfg"),
        unattended: true,
    },
    // pnpm's content-addressable store, placed inside the checkout when the
    // store directory resolves there. Every file in it is re-downloaded by
    // the next install.
    ArtifactKind {
        name: ".pnpm-store",
        witness: ArtifactWitness::NonEmptyDirectory,
        unattended: true,
    },
    // Common output names with no marker a tool reliably writes, and a name a
    // project may use for hand-maintained content. Reviewed paths only.
    ArtifactKind {
        name: "build",
        witness: ArtifactWitness::NonEmptyDirectory,
        unattended: false,
    },
    ArtifactKind {
        name: "dist",
        witness: ArtifactWitness::NonEmptyDirectory,
        unattended: false,
    },
];

fn configured(name: &str, extras: &[String]) -> bool {
    extras
        .iter()
        .any(|candidate| is_safe_artefact_directory_name(candidate) && candidate == name)
}

/// Whether a reviewed path may propose `name`: any built-in entry, plus
/// configured names. Configuration never removes a built-in name.
pub(crate) fn is_catalogued(name: &str, extras: &[String]) -> bool {
    CATALOG.iter().any(|kind| kind.name == name) || configured(name, extras)
}

/// The witness the unattended sweep requires for `name`, if it may sweep it
/// at all. Built-in entries keep their built-in witness even when a
/// configuration repeats the name, so configuration cannot weaken one; a
/// configured name is an explicit opt-in and is witnessed by being non-empty.
pub(crate) fn unattended_witness(name: &str, extras: &[String]) -> Option<ArtifactWitness> {
    match CATALOG.iter().find(|kind| kind.name == name) {
        Some(kind) if kind.unattended => Some(kind.witness),
        Some(_) => configured(name, extras).then_some(ArtifactWitness::NonEmptyDirectory),
        None => configured(name, extras).then_some(ArtifactWitness::NonEmptyDirectory),
    }
}

/// Whether `name` is a built-in entry that only reviewed paths may remove.
pub(crate) fn is_reviewed_only(name: &str, extras: &[String]) -> bool {
    CATALOG
        .iter()
        .any(|kind| kind.name == name && !kind.unattended)
        && !configured(name, extras)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_name_is_a_safe_single_component() {
        for kind in CATALOG {
            assert!(is_safe_artefact_directory_name(kind.name), "{}", kind.name);
            assert!(!kind.name.contains('/'), "{}", kind.name);
        }
    }

    /// The two lanes read one table, so they cannot drift apart again: every
    /// name the sweep may remove is also one a reviewed plan proposes.
    #[test]
    fn every_unattended_name_is_also_reviewable() {
        for kind in CATALOG {
            assert!(is_catalogued(kind.name, &[]), "{}", kind.name);
            assert_eq!(
                unattended_witness(kind.name, &[]).is_some(),
                kind.unattended,
                "{}",
                kind.name
            );
        }
    }

    #[test]
    fn generic_names_are_reviewed_only_unless_configured() {
        for name in ["build", "dist"] {
            assert!(unattended_witness(name, &[]).is_none(), "{name}");
            assert!(is_reviewed_only(name, &[]), "{name}");
            assert!(
                unattended_witness(name, &[name.to_string()]).is_some(),
                "an explicit configuration opts {name} in"
            );
        }
    }

    #[test]
    fn configuration_cannot_weaken_a_built_in_witness() {
        assert_eq!(
            unattended_witness(".venv", &[".venv".into()]),
            Some(ArtifactWitness::File("pyvenv.cfg"))
        );
    }
}
