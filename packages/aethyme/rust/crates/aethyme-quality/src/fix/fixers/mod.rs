//! The five fixers (port of `src/autofixers/fixers/`).
//!
//! Each fixer proposes whole-file rewrites; `PatchGenerator` turns them
//! into diffs and decides whether they may be written.
//!
//! ## Detect→fix unification: deferred
//!
//! The retirement plan's work item 5 asks for `autofix` to consume
//! Phase 4's `Finding`s and map them to fixers, deleting the
//! `normalize_fixes` glue. The glue is gone (the fixers return typed
//! proposals), but the *scanning* is not unified: today's fixers do
//! their own scanning with their own patterns, and those patterns do
//! not agree with the detectors'. `folder-docs` flags directories with
//! more than N source files and no doc of ANY accepted name;
//! `DocsRegenerator` flags directories with ANY code file and no
//! `FOLDER.md` specifically. `data-ui-coverage` and `SelectorInserter`
//! disagree on which elements count. Swapping a fixer's scan for a
//! detector would change which patches are produced, and decision #2
//! (byte parity) outranks the unification.
//!
//! So each fixer's own scan is ported verbatim. The unification is a
//! post-parity refactor with its own commit, where the behavior change
//! can be reviewed on its own merits rather than smuggled through a
//! port.

pub mod base;
pub mod docs_regenerator;
pub mod link_fixer;
pub mod selector_inserter;

pub use base::{FixProposal, Fixer, process_directory};
pub use docs_regenerator::DocsRegenerator;
pub use link_fixer::LinkFixer;
pub use selector_inserter::SelectorInserter;
