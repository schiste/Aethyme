//! Validate exported output without unpacking untrusted archive paths.
use super::*;
use aethyme_contracts::experimental_v0::SourceSnapshot;
use aethyme_contracts::experimental_v0::source_snapshot::{EntryKind, SourceEntry};
use std::collections::BTreeMap;
use std::io::Read;

const MAX_ENTRIES: usize = 8192;
const MAX_FILE: u64 = 64 * 1024 * 1024;
#[derive(Debug)]
pub(crate) struct Output {
    pub manifest: SourceSnapshot,
    pub files: BTreeMap<Vec<u8>, Vec<u8>>,
}
#[derive(Clone, Copy)]
pub(super) enum Purpose {
    Input,
    Output,
}
pub(super) fn read(file: &Path, limit: u64, purpose: Purpose) -> Result<Output, SupervisorError> {
    let mut archive = tar::Archive::new(std::fs::File::open(file)?);
    let mut files = BTreeMap::new();
    let mut entries = Vec::new();
    let mut total = 0u64;
    for (number, entry) in archive.entries()?.enumerate() {
        if number >= MAX_ENTRIES {
            return Err(SupervisorError::Limit);
        }
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        let raw = entry.path_bytes().into_owned();
        // Docker's directory copy emits './'. Remove only that literal prefix;
        // source-snapshot validation refuses any remaining traversal or alias.
        let raw = raw.strip_prefix(b"./").unwrap_or(&raw).to_vec();
        if kind.is_dir() {
            continue;
        }
        if !kind.is_file() || entry.header().mode()? & 0o7000 != 0 || raw.is_empty() {
            return Err(SupervisorError::Tampered(
                "non-regular output or special mode".into(),
            ));
        }
        let size = entry.size();
        total = total.checked_add(size).ok_or(SupervisorError::Limit)?;
        if size > MAX_FILE || total > limit {
            return Err(SupervisorError::Limit);
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        if bytes.len() as u64 != size || files.contains_key(&raw) {
            return Err(SupervisorError::Tampered(
                "duplicate or truncated output".into(),
            ));
        }
        let mode = if entry.header().mode()? & 0o111 != 0 {
            EntryKind::Executable
        } else {
            EntryKind::Regular
        };
        entries.push(SourceEntry::from_content(raw.clone(), mode, &bytes));
        files.insert(raw, bytes);
    }
    if entries.is_empty() && matches!(purpose, Purpose::Output) {
        return Err(SupervisorError::Inconclusive("no build artifact".into()));
    }
    let manifest =
        SourceSnapshot::new(entries).map_err(|e| SupervisorError::Tampered(e.to_string()))?;
    snapshot::portable(&manifest).map_err(|e| SupervisorError::Tampered(e.to_string()))?;
    Ok(Output { manifest, files })
}
