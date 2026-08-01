//! Port of `src/autofixers/fixers/base.py`.

use std::path::{Path, PathBuf};

use crate::walk;

/// One proposed rewrite, the shape `BaseFixer.process_file` returns.
/// This replaces the `normalize_fixes` / `FixRecord` glue in `cli.py`:
/// the proposal is typed at the source, so there is nothing to
/// re-validate at the boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixProposal {
    pub file_path: PathBuf,
    pub original_content: String,
    pub new_content: String,
    pub fix_type: String,
}

/// Port of the `BaseFixer` abstract methods.
pub trait Fixer {
    fn fix_type(&self) -> &'static str;
    fn can_fix(&self, file_path: &Path) -> bool;
    /// `None` when no fix applies.
    fn fix(&self, file_path: &Path, content: &str) -> Option<String>;
}

/// Port of `BaseFixer.process_file`.
///
/// The read is `open(file_path, encoding='utf-8')` — strict UTF-8 in
/// text mode, so `\r\n` and lone `\r` are translated to `\n` before the
/// fixer sees them, and a non-UTF-8 file raises and is skipped (Python
/// records it in `self.errors`, which nothing reads).
pub fn process_file(fixer: &dyn Fixer, file_path: &Path) -> Option<FixProposal> {
    if !fixer.can_fix(file_path) {
        return None;
    }
    let original_content = walk::read_file_safe(file_path)?;
    let new_content = fixer.fix(file_path, &original_content)?;
    if new_content == original_content {
        return None;
    }
    Some(FixProposal {
        file_path: file_path.to_path_buf(),
        original_content,
        new_content,
        fix_type: fixer.fix_type().to_string(),
    })
}

/// Port of `BaseFixer.process_directory`.
///
/// Note what this does NOT do: no ignore rules. `rglob('*')` walks
/// `node_modules`, `.git`, `.venv`, `target` and everything else, and
/// filtering happens later in `SafetyEngine.assess_risk` — which
/// rejects those paths one at a time rather than pruning the walk. The
/// traversal order is CPython's `rglob` order (see `walk::rglob_all`),
/// and it determines patch order, hence diff order.
pub fn process_directory(fixer: &dyn Fixer, directory: &Path) -> Vec<FixProposal> {
    let mut fixes = Vec::new();
    for entry in walk::rglob_all(directory) {
        if entry.is_file
            && let Some(proposal) = process_file(fixer, &entry.path)
        {
            fixes.push(proposal);
        }
    }
    fixes
}
