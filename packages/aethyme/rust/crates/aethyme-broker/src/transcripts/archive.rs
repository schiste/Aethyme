//! Compress-then-move, crash-safe, and its inverse.
//!
//! Archiving one transcript:
//! 1. stat the source (size, mtime, inode, device) and confirm no process
//!    has it open;
//! 2. stream it through zstd (level 3, with the frame checksum) into a new
//!    0600 temporary file (`O_CREAT | O_EXCL | O_NOFOLLOW`) in the
//!    destination directory, hashing the source bytes with SHA-256 as they
//!    are read, then fsync the temporary file;
//! 3. stat the source again: a changed size, mtime, inode or device, or a
//!    byte count that differs from the size, discards the temporary file and
//!    skips;
//! 4. decompress the temporary file and compare its length and SHA-256 with
//!    the source's; any difference discards it and skips;
//! 5. publish it with a no-clobber `linkat` and fsync the directory, record
//!    the index row, then check the source once more (unchanged, not open);
//! 6. only then unlink the source (relative to its validated root, and only
//!    if its identity is still the one compressed) and fsync its directory.
//!
//! The source is never removed before a verified, durable copy exists, and
//! the copy is only ever removed while the source still exists, so the two
//! are never both gone. A crash between steps 5 and 6 leaves both; the next
//! run finds the destination, verifies it decodes to exactly the source's
//! bytes, and finishes the removal. A crash before step 5 leaves a temporary
//! file, which the next run removes.
//!
//! Every file operation goes through [`super::safe_fs`]: no symbolic link is
//! followed beneath a provider root and no mount point is crossed.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::eligibility::{OpenState, SkipReason};
use super::index::{ArchiveRow, Index};
use super::open_files::{OpenFileProbe, OpenFiles};
use super::pacing::Pacer;
use super::providers::{TEMP_PREFIX, Transcript};
use super::safe_fs::{Dir, Root, Stamp};

/// zstd level 3: zstd's own default, 4-5x on agent transcripts.
pub(crate) const ZSTD_LEVEL: i32 = 3;
const CHUNK: usize = 1024 * 1024;
const TEMP_SUFFIX: &str = ".part";

/// Points where a test can intervene; production uses [`NoHooks`].
pub(crate) trait Hooks {
    /// After the temporary copy is written, before the source is re-checked.
    fn after_compress(&mut self, _source: &Path) {}
    /// Before the temporary copy is verified, with its full path.
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

    /// An I/O failure on a path beneath a provider root (never content).
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

fn hex(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Length and SHA-256 of everything `reader` yields.
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

/// Length and SHA-256 of what an open zstd file decompresses to.
fn digest_compressed(file: File, pacer: &mut Pacer) -> io::Result<(u64, String)> {
    digest_reader(zstd::stream::read::Decoder::new(file)?, pacer)
}

/// Stream `source` through zstd into `temporary`, returning the bytes read
/// and their SHA-256. The temporary file is synced before this returns.
fn compress_into(source: File, temporary: &File, pacer: &mut Pacer) -> io::Result<(u64, String)> {
    let mut reader = BufReader::with_capacity(CHUNK, source);
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

/// The roots one transcript is confined to.
pub(crate) struct Confinement<'r> {
    /// Where the transcript lives (Codex home, Claude projects).
    pub source: &'r Root,
    /// Where its archive goes (Codex home, Aethyme's transcript state).
    pub destination: &'r Root,
    /// The directory whose open-file listing covers the transcript.
    pub open_root: &'r Path,
}

/// Context one archiving pass shares across files.
pub(crate) struct ArchiveContext<'a, P> {
    pub index: &'a Index,
    pub open_files: &'a mut OpenFiles<P>,
    pub pacer: &'a mut Pacer,
    pub hooks: &'a mut dyn Hooks,
    pub now_ms: i64,
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

/// The source and destination entries, opened beneath their roots.
struct Entries {
    source_dir: Dir,
    source_name: CString,
    destination_dir: Dir,
    destination_name: CString,
}

/// Remove the source once a verified copy exists, after one last check that
/// it is unchanged and not open. Otherwise remove the (stale) copy instead.
fn remove_source<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    confinement: &Confinement<'_>,
    entries: &Entries,
    before: Stamp,
) -> Result<(), Skipped> {
    let unchanged = entries
        .source_dir
        .stamp(&entries.source_name)
        .is_ok_and(|now| now == before);
    let open = context
        .open_files
        .state_of(confinement.open_root, &transcript.path);
    if !unchanged || open != OpenState::Closed {
        // The source moved on after the copy was taken: the copy is stale.
        super::best_effort(entries.destination_dir.unlink(&entries.destination_name));
        entries.destination_dir.sync();
        super::best_effort(context.index.remove_archive(&transcript.path));
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
    entries
        .source_dir
        .unlink_if(&entries.source_name, before)
        .map_err(|error| Skipped::io("cannot remove", &transcript.path, &error))?;
    entries.source_dir.sync();
    Ok(())
}

/// Finish an archive an interrupted run published but did not complete:
/// the destination must decode to exactly the source's bytes.
fn resume<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    confinement: &Confinement<'_>,
    entries: &Entries,
    before: Stamp,
) -> Result<Archived, Skipped> {
    let started = Instant::now();
    let (source_file, opened) = entries
        .source_dir
        .open_read(&entries.source_name)
        .map_err(|error| Skipped::io("cannot read", &transcript.path, &error))?;
    if opened != before {
        return Err(Skipped::new(
            SkipReason::ModifiedDuringCompression,
            "source changed before it could be compared",
        ));
    }
    let source = digest_reader(source_file, context.pacer)
        .map_err(|error| Skipped::io("cannot read", &transcript.path, &error))?;
    let destination = transcript.destination.display().to_string();
    let conflict = |detail: String| Skipped::new(SkipReason::DestinationConflict, detail);
    let (copy_file, copy_stamp) = entries
        .destination_dir
        .open_read(&entries.destination_name)
        .map_err(|error| conflict(format!("{destination} exists and cannot be read: {error}")))?;
    let copy = digest_compressed(copy_file, context.pacer)
        .map_err(|error| conflict(format!("{destination} exists and does not decode: {error}")))?;
    if copy != source {
        return Err(conflict(format!(
            "{destination} exists with different content"
        )));
    }
    let verify_time = started.elapsed();
    let indexed = context
        .index
        .archive_for(&transcript.path)
        .ok()
        .flatten()
        .is_some_and(|row| row.sha256 == source.1);
    if !indexed {
        let row = row_for(
            transcript,
            source.0,
            copy_stamp.len,
            &source.1,
            context.now_ms,
        );
        context
            .index
            .record_archive(&row)
            .map_err(|error| Skipped::new(SkipReason::Io, error))?;
    }
    remove_source(context, transcript, confinement, entries, before)?;
    Ok(Archived {
        bytes_in: source.0,
        bytes_out: copy_stamp.len,
        sha256: source.1,
        resumed: true,
        compress_time: Duration::ZERO,
        verify_time,
    })
}

fn unsafe_path(path: &Path, error: &io::Error) -> Skipped {
    Skipped::new(
        SkipReason::UnsafePath,
        format!("{}: {error}", path.display()),
    )
}

/// Archive one transcript (see the module documentation for the steps).
pub(crate) fn archive_one<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    confinement: &Confinement<'_>,
) -> Result<Archived, Skipped> {
    if transcript.path.to_str().is_none() || transcript.destination.to_str().is_none() {
        return Err(Skipped::new(
            SkipReason::UnsupportedPath,
            "path is not UTF-8",
        ));
    }
    let (source_dir, source_name) = confinement
        .source
        .parent_of(&transcript.path, false)
        .map_err(|error| unsafe_path(&transcript.path, &error))?;
    let before = source_dir
        .stamp(&source_name)
        .map_err(|error| unsafe_path(&transcript.path, &error))?;
    match context
        .open_files
        .state_of(confinement.open_root, &transcript.path)
    {
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
    let (destination_dir, destination_name) = confinement
        .destination
        .parent_of(&transcript.destination, true)
        .map_err(|error| unsafe_path(&transcript.destination, &error))?;
    destination_dir
        .make_private()
        .map_err(|error| Skipped::io("cannot restrict", &transcript.destination, &error))?;
    let entries = Entries {
        source_dir,
        source_name,
        destination_dir,
        destination_name,
    };
    match entries.destination_dir.exists(&entries.destination_name) {
        Ok(true) => return resume(context, transcript, confinement, &entries, before),
        Ok(false) => {}
        Err(error) => return Err(Skipped::io("cannot stat", &transcript.destination, &error)),
    }

    let compress_started = Instant::now();
    let (source_file, opened) = entries
        .source_dir
        .open_read(&entries.source_name)
        .map_err(|error| unsafe_path(&transcript.path, &error))?;
    if opened != before {
        return Err(Skipped::new(
            SkipReason::ModifiedDuringCompression,
            "source changed before compression started",
        ));
    }
    let temporary = entries
        .destination_dir
        .create_temp(TEMP_PREFIX, TEMP_SUFFIX)
        .map_err(|error| {
            Skipped::io(
                "cannot create a temporary file beside",
                &transcript.destination,
                &error,
            )
        })?;
    let (bytes_in, sha256) = compress_into(source_file, &temporary.file, context.pacer)
        .map_err(|error| Skipped::io("cannot compress", &transcript.path, &error))?;
    let compress_time = compress_started.elapsed();
    context.hooks.after_compress(&transcript.path);

    let after = entries.source_dir.stamp(&entries.source_name).ok();
    if after != Some(before) || bytes_in != before.len {
        return Err(Skipped::new(
            SkipReason::ModifiedDuringCompression,
            "size, mtime or inode changed while compressing; copy discarded",
        ));
    }

    let verify_started = Instant::now();
    if let (Some(directory), Ok(name)) = (transcript.destination.parent(), temporary.name.to_str())
    {
        context.hooks.before_verify(&directory.join(name));
    }
    let verified = entries
        .destination_dir
        .open_read(&temporary.name)
        .and_then(|(file, _)| digest_compressed(file, context.pacer));
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
        .file
        .metadata()
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    temporary
        .publish(&entries.destination_name)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::AlreadyExists {
                Skipped::new(
                    SkipReason::DestinationConflict,
                    "destination appeared meanwhile",
                )
            } else {
                Skipped::io("cannot publish", &transcript.destination, &error)
            }
        })?;
    let row = row_for(transcript, bytes_in, bytes_out, &sha256, context.now_ms);
    if let Err(error) = context.index.record_archive(&row) {
        // Without its index row a copy cannot be restored with its hash
        // checked; keep the source and drop the copy.
        super::best_effort(entries.destination_dir.unlink(&entries.destination_name));
        entries.destination_dir.sync();
        return Err(Skipped::new(SkipReason::Io, error));
    }
    if !context.hooks.after_publish(&transcript.path) {
        return Err(Skipped::new(
            SkipReason::Interrupted,
            "stopped after publishing",
        ));
    }
    remove_source(context, transcript, confinement, &entries, before)?;
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

/// Decompress `plan.archive` (beneath `archive_root`) back to
/// `plan.original` (beneath `original_root`), verifying the hash, then
/// remove the archive. Refuses when the original path exists.
pub(crate) fn restore_one(
    plan: &RestorePlan,
    archive_root: &Root,
    original_root: &Root,
    index: Option<&Index>,
) -> Result<Restored, String> {
    let original = plan.original.display();
    let archive = plan.archive.display();
    let (original_dir, original_name) = original_root
        .parent_of(&plan.original, true)
        .map_err(|error| format!("unsafe restore target {original}: {error}"))?;
    match original_dir.exists(&original_name) {
        Ok(false) => {}
        Ok(true) => {
            return Err(format!(
                "{original} already exists; move it aside before restoring"
            ));
        }
        Err(error) => return Err(format!("cannot stat {original}: {error}")),
    }
    let (archive_dir, archive_name) = archive_root
        .parent_of(&plan.archive, false)
        .map_err(|error| format!("unsafe archive path {archive}: {error}"))?;
    let (archive_file, archive_stamp) = archive_dir
        .open_read(&archive_name)
        .map_err(|error| format!("cannot open {archive}: {error}"))?;
    let temporary = original_dir
        .create_temp(TEMP_PREFIX, TEMP_SUFFIX)
        .map_err(|error| format!("cannot create a temporary file beside {original}: {error}"))?;
    let mut decoder = zstd::stream::read::Decoder::new(archive_file)
        .map_err(|error| format!("cannot decode {archive}: {error}"))?;
    let mut hasher = Sha256::new();
    let mut writer = BufWriter::with_capacity(CHUNK, &temporary.file);
    let mut buffer = vec![0u8; CHUNK];
    let mut total = 0u64;
    loop {
        let read = match decoder.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(format!("cannot decode {archive}: {error}")),
        };
        hasher.update(&buffer[..read]);
        writer
            .write_all(&buffer[..read])
            .map_err(|error| format!("cannot write beside {original}: {error}"))?;
        total += read as u64;
    }
    writer
        .flush()
        .map_err(|error| format!("cannot write beside {original}: {error}"))?;
    drop(writer);
    let digest = hex(&hasher.finalize());
    if let Some(expected) = &plan.sha256
        && *expected != digest
    {
        return Err(format!(
            "{archive} decodes to SHA-256 {digest}, the index recorded {expected}; archive kept"
        ));
    }
    temporary
        .publish(&original_name)
        .map_err(|error| format!("cannot publish {original}: {error}"))?;
    archive_dir
        .unlink_if(&archive_name, archive_stamp)
        .map_err(|error| format!("restored {original} but could not remove {archive}: {error}"))?;
    archive_dir.sync();
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

/// Remove leftover temporary files beneath one of `roots`; returns how many.
pub(crate) fn remove_leftover_temps(paths: &[PathBuf], roots: &[&Root]) -> u64 {
    let mut removed = 0;
    for path in paths {
        let Some(root) = roots.iter().find(|root| root.contains(path)) else {
            continue;
        };
        let Ok((directory, name)) = root.parent_of(path, false) else {
            continue;
        };
        if directory.stamp(&name).is_ok() && directory.unlink(&name).is_ok() {
            directory.sync();
            removed += 1;
        }
    }
    removed
}
