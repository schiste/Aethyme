//! Compress-then-move, crash-safe, and its inverse.
//!
//! Archiving one transcript:
//! 1. stat the source (size, mtime, inode) and confirm no process has it open;
//! 2. stream it through zstd (level 3, with the frame checksum) into a
//!    temporary file in the destination directory, hashing the source bytes
//!    with SHA-256 as they are read, then fsync the temporary file;
//! 3. stat the source again: a changed size, mtime or inode, or a byte count
//!    that differs from the size, discards the temporary file and skips;
//! 4. decompress the temporary file and compare its length and SHA-256 with
//!    the source's; any difference discards it and skips;
//! 5. publish it with a no-clobber link and fsync the directory, record the
//!    index row, then check the source once more (unchanged, not open);
//! 6. only then remove the source and fsync its directory.
//!
//! The source is never removed before a verified, durable copy exists, and
//! the copy is only ever removed while the source still exists, so the two
//! are never both gone. A crash between steps 5 and 6 leaves both; the next
//! run finds the destination, verifies it decodes to exactly the source's
//! bytes, and finishes the removal. A crash before step 5 leaves a temporary
//! file, which the next run removes.

use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::eligibility::{OpenState, SkipReason};
use super::index::{ArchiveRow, Index};
use super::open_files::{OpenFileProbe, OpenFiles};
use super::pacing::Pacer;
use super::providers::{TEMP_PREFIX, Transcript};

/// zstd level 3: zstd's own default, 4-5x on agent transcripts.
pub(crate) const ZSTD_LEVEL: i32 = 3;
const CHUNK: usize = 1024 * 1024;

/// Points where a test can intervene; production uses [`NoHooks`].
pub(crate) trait Hooks {
    /// After the temporary copy is written, before the source is re-checked.
    fn after_compress(&mut self, _source: &Path) {}
    /// Before the temporary copy is verified.
    fn before_verify(&mut self, _temporary: &Path) {}
    /// After the copy is published and indexed. `false` stops here, as a
    /// crash before the source is removed would.
    fn after_publish(&mut self, _source: &Path) -> bool {
        true
    }
}

/// No intervention.
pub(crate) struct NoHooks;

impl Hooks for NoHooks {}

/// A transcript left in place, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Skipped {
    pub reason: SkipReason,
    pub detail: String,
}

impl Skipped {
    fn new(reason: SkipReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }

    fn io(what: &str, path: &Path, error: &io::Error) -> Self {
        Self::new(
            SkipReason::Io,
            format!("{what} {}: {error}", path.display()),
        )
    }
}

/// What archiving one transcript did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Archived {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub sha256: String,
    /// True when an earlier run had already published the copy and this run
    /// only finished removing the source.
    pub resumed: bool,
    pub compress_time: Duration,
    pub verify_time: Duration,
}

/// Identity of a file at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<std::time::SystemTime>,
    inode: u64,
    device: u64,
}

fn stamp(path: &Path) -> io::Result<Stamp> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(Stamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        inode: metadata.ino(),
        device: metadata.dev(),
    })
}

/// fsync a directory so a rename or unlink in it is durable. Best effort on
/// file systems that refuse to sync a directory.
pub(crate) fn sync_directory(directory: &Path) {
    if let Ok(handle) = File::open(directory) {
        let _ = handle.sync_all();
    }
}

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Length and SHA-256 of a plain file.
pub(crate) fn digest_plain(path: &Path, pacer: &mut Pacer) -> io::Result<(u64, String)> {
    digest_reader(File::open(path)?, pacer)
}

/// Length and SHA-256 of what a zstd file decompresses to.
pub(crate) fn digest_compressed(path: &Path, pacer: &mut Pacer) -> io::Result<(u64, String)> {
    digest_reader(zstd::stream::read::Decoder::new(File::open(path)?)?, pacer)
}

fn digest_reader(mut reader: impl Read, pacer: &mut Pacer) -> io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
        total += read as u64;
        pacer.consume(read as u64);
    }
    Ok((total, hex(&hasher.finalize())))
}

/// A private temporary file beside `target`, named so a later run
/// recognises it.
fn temporary_beside(target: &Path) -> io::Result<tempfile::NamedTempFile> {
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let temporary = tempfile::Builder::new()
        .prefix(TEMP_PREFIX)
        .suffix(".part")
        .tempfile_in(parent)?;
    crate::host_state::protect_host_state_path(temporary.path(), false)?;
    Ok(temporary)
}

/// Stream `source` through zstd into `temporary`, returning the bytes read
/// and their SHA-256. The temporary file is synced before this returns.
fn compress_into(source: &Path, temporary: &File, pacer: &mut Pacer) -> io::Result<(u64, String)> {
    let mut reader = BufReader::with_capacity(CHUNK, File::open(source)?);
    let mut encoder =
        zstd::stream::write::Encoder::new(BufWriter::with_capacity(CHUNK, temporary), ZSTD_LEVEL)?;
    encoder.include_checksum(true)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        hasher.update(&buffer[..read]);
        encoder.write_all(&buffer[..read])?;
        total += read as u64;
        pacer.consume(read as u64);
    }
    let mut writer = encoder.finish()?;
    writer.flush()?;
    drop(writer);
    temporary.sync_all()?;
    Ok((total, hex(&hasher.finalize())))
}

/// Context one archiving pass shares across files.
pub(crate) struct ArchiveContext<'a, P> {
    pub index: &'a Index,
    pub open_files: &'a mut OpenFiles<P>,
    pub pacer: &'a mut Pacer,
    pub hooks: &'a mut dyn Hooks,
    pub now_ms: i64,
}

fn open_state<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    root: &Path,
    path: &Path,
) -> OpenState {
    context.open_files.state_of(root, path)
}

fn row_for(
    transcript: &Transcript,
    bytes_in: u64,
    bytes_out: u64,
    sha256: &str,
    now_ms: i64,
) -> ArchiveRow {
    ArchiveRow {
        original_path: transcript.path.clone(),
        archive_path: transcript.destination.clone(),
        provider: transcript.provider.label().into(),
        agent_session_id: transcript.agent_session_id.clone(),
        bytes_in,
        bytes_out,
        sha256: sha256.into(),
        archived_at: now_ms,
    }
}

/// Remove the source once a verified copy exists, after one last check that
/// it is unchanged and not open.
fn remove_source<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    open_root: &Path,
    before: Stamp,
) -> Result<(), Skipped> {
    let unchanged = stamp(&transcript.path).is_ok_and(|now| now == before);
    let open = open_state(context, open_root, &transcript.path);
    if !unchanged || open != OpenState::Closed {
        // The source moved on after the copy was taken: the copy is stale.
        // Remove the copy (the source still exists) and its index row.
        let _ = fs::remove_file(&transcript.destination);
        if let Some(parent) = transcript.destination.parent() {
            sync_directory(parent);
        }
        let _ = context.index.remove_archive(&transcript.path);
        let reason = if !unchanged {
            SkipReason::ModifiedDuringCompression
        } else if open == OpenState::Open {
            SkipReason::OpenByProcess
        } else {
            SkipReason::OpenCheckUnavailable
        };
        return Err(Skipped::new(
            reason,
            "source changed or opened before removal; copy discarded",
        ));
    }
    fs::remove_file(&transcript.path)
        .map_err(|error| Skipped::io("cannot remove", &transcript.path, &error))?;
    if let Some(parent) = transcript.path.parent() {
        sync_directory(parent);
    }
    Ok(())
}

/// Finish an archive an interrupted run published but did not complete:
/// the destination must decode to exactly the source's bytes.
fn resume<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    open_root: &Path,
    before: Stamp,
) -> Result<Archived, Skipped> {
    let started = Instant::now();
    let source = digest_plain(&transcript.path, context.pacer)
        .map_err(|error| Skipped::io("cannot read", &transcript.path, &error))?;
    let copy = digest_compressed(&transcript.destination, context.pacer).map_err(|error| {
        Skipped::new(
            SkipReason::DestinationConflict,
            format!(
                "{} exists and does not decode: {error}",
                transcript.destination.display()
            ),
        )
    })?;
    if copy != source {
        return Err(Skipped::new(
            SkipReason::DestinationConflict,
            format!(
                "{} exists with different content",
                transcript.destination.display()
            ),
        ));
    }
    let bytes_out = fs::metadata(&transcript.destination)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    let verify_time = started.elapsed();
    let indexed = context
        .index
        .archive_for(&transcript.path)
        .ok()
        .flatten()
        .is_some_and(|row| row.sha256 == source.1);
    if !indexed {
        context
            .index
            .record_archive(&row_for(
                transcript,
                source.0,
                bytes_out,
                &source.1,
                context.now_ms,
            ))
            .map_err(|error| Skipped::new(SkipReason::Io, error))?;
    }
    remove_source(context, transcript, open_root, before)?;
    Ok(Archived {
        bytes_in: source.0,
        bytes_out,
        sha256: source.1,
        resumed: true,
        compress_time: Duration::ZERO,
        verify_time,
    })
}

/// Archive one transcript (see the module documentation for the steps).
/// `open_root` is the directory whose open-file listing covers it.
pub(crate) fn archive_one<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    open_root: &Path,
) -> Result<Archived, Skipped> {
    if transcript.path.to_str().is_none() || transcript.destination.to_str().is_none() {
        return Err(Skipped::new(
            SkipReason::UnsupportedPath,
            "path is not UTF-8",
        ));
    }
    let before = stamp(&transcript.path)
        .map_err(|error| Skipped::io("cannot stat", &transcript.path, &error))?;
    match open_state(context, open_root, &transcript.path) {
        OpenState::Closed => {}
        OpenState::Open => {
            return Err(Skipped::new(SkipReason::OpenByProcess, "open by a process"));
        }
        OpenState::Unknown => {
            return Err(Skipped::new(
                SkipReason::OpenCheckUnavailable,
                "open-file listing incomplete",
            ));
        }
    }
    match fs::symlink_metadata(&transcript.destination) {
        Ok(_) => return resume(context, transcript, open_root, before),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(Skipped::io("cannot stat", &transcript.destination, &error)),
    }
    let parent = transcript
        .destination
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    fs::create_dir_all(&parent).map_err(|error| Skipped::io("cannot create", &parent, &error))?;

    let compress_started = Instant::now();
    let temporary = temporary_beside(&transcript.destination)
        .map_err(|error| Skipped::io("cannot create a temporary file in", &parent, &error))?;
    let (bytes_in, sha256) = compress_into(&transcript.path, temporary.as_file(), context.pacer)
        .map_err(|error| Skipped::io("cannot compress", &transcript.path, &error))?;
    let compress_time = compress_started.elapsed();
    context.hooks.after_compress(&transcript.path);

    let after = stamp(&transcript.path).ok();
    if after != Some(before) || bytes_in != before.len {
        return Err(Skipped::new(
            SkipReason::ModifiedDuringCompression,
            "size, mtime or inode changed while compressing; copy discarded",
        ));
    }

    let verify_started = Instant::now();
    context.hooks.before_verify(temporary.path());
    let verified = digest_compressed(temporary.path(), context.pacer);
    let verify_time = verify_started.elapsed();
    match verified {
        Ok((length, digest)) if length == bytes_in && digest == sha256 => {}
        Ok(_) => {
            return Err(Skipped::new(
                SkipReason::VerifyFailed,
                "compressed copy decodes to different bytes; source kept",
            ));
        }
        Err(error) => {
            return Err(Skipped::new(
                SkipReason::VerifyFailed,
                format!("compressed copy does not decode: {error}; source kept"),
            ));
        }
    }
    let bytes_out = temporary
        .as_file()
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    temporary
        .persist_noclobber(&transcript.destination)
        .map_err(|error| {
            if error.error.kind() == io::ErrorKind::AlreadyExists {
                Skipped::new(
                    SkipReason::DestinationConflict,
                    "destination appeared meanwhile",
                )
            } else {
                Skipped::io("cannot publish", &transcript.destination, &error.error)
            }
        })?;
    sync_directory(&parent);
    if let Err(error) = context.index.record_archive(&row_for(
        transcript,
        bytes_in,
        bytes_out,
        &sha256,
        context.now_ms,
    )) {
        // Without its index row a copy cannot be restored with its hash
        // checked; keep the source and drop the copy.
        let _ = fs::remove_file(&transcript.destination);
        sync_directory(&parent);
        return Err(Skipped::new(SkipReason::Io, error));
    }
    if !context.hooks.after_publish(&transcript.path) {
        return Err(Skipped::new(
            SkipReason::Interrupted,
            "stopped after publishing",
        ));
    }
    remove_source(context, transcript, open_root, before)?;
    Ok(Archived {
        bytes_in,
        bytes_out,
        sha256,
        resumed: false,
        compress_time,
        verify_time,
    })
}

/// Where a restore puts a transcript back, and what it must hash to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestorePlan {
    pub archive: PathBuf,
    pub original: PathBuf,
    /// From the index; `None` when only the zstd frame checksum can vouch.
    pub sha256: Option<String>,
}

/// What a restore did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Restored {
    pub archive: PathBuf,
    pub original: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub hash_checked: bool,
}

/// Decompress `plan.archive` back to `plan.original`, verifying the hash,
/// then remove the archive. Refuses when the original path exists.
pub(crate) fn restore_one(plan: &RestorePlan, index: Option<&Index>) -> Result<Restored, String> {
    match fs::symlink_metadata(&plan.original) {
        Ok(_) => {
            return Err(format!(
                "{} already exists; move it aside before restoring",
                plan.original.display()
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("cannot stat {}: {error}", plan.original.display())),
    }
    let parent = plan
        .original
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", plan.original.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let temporary = temporary_beside(&plan.original).map_err(|error| {
        format!(
            "cannot create a temporary file in {}: {error}",
            parent.display()
        )
    })?;
    let mut decoder = zstd::stream::read::Decoder::new(
        File::open(&plan.archive)
            .map_err(|error| format!("cannot open {}: {error}", plan.archive.display()))?,
    )
    .map_err(|error| format!("cannot decode {}: {error}", plan.archive.display()))?;
    let mut hasher = Sha256::new();
    let mut writer = BufWriter::with_capacity(CHUNK, temporary.as_file());
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let read = match decoder.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("cannot decode {}: {error}", plan.archive.display())),
        };
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|error| format!("cannot write {}: {error}", parent.display()))?;
        total += read as u64;
    }
    writer
        .flush()
        .map_err(|error| format!("cannot write {}: {error}", parent.display()))?;
    drop(writer);
    temporary
        .as_file()
        .sync_all()
        .map_err(|error| format!("cannot sync {}: {error}", parent.display()))?;
    let digest = hex(&hasher.finalize());
    if let Some(expected) = &plan.sha256
        && *expected != digest
    {
        return Err(format!(
            "{} decodes to SHA-256 {digest}, the index recorded {expected}; archive kept",
            plan.archive.display()
        ));
    }
    temporary
        .persist_noclobber(&plan.original)
        .map_err(|error| {
            format!(
                "cannot publish {}: {}",
                plan.original.display(),
                error.error
            )
        })?;
    sync_directory(parent);
    fs::remove_file(&plan.archive).map_err(|error| {
        format!(
            "restored {} but could not remove {}: {error}",
            plan.original.display(),
            plan.archive.display()
        )
    })?;
    if let Some(archive_parent) = plan.archive.parent() {
        sync_directory(archive_parent);
    }
    if let Some(index) = index {
        index.remove_archive(&plan.original)?;
    }
    Ok(Restored {
        archive: plan.archive.clone(),
        original: plan.original.clone(),
        bytes: total,
        sha256: digest,
        hash_checked: plan.sha256.is_some(),
    })
}
