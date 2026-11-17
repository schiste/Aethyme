//! Compress-then-move, crash-safe, and its inverse.
//!
//! Archiving one transcript:
//! 1. stat the source (size, mtime, inode, device) and confirm no process
//!    has it open;
//! 2. stream it through zstd (level 3, with the frame checksum) into a new
//!    0600 temporary file (`O_CREAT | O_EXCL | O_NOFOLLOW`) in the
//!    destination directory, hashing the source bytes with SHA-256 as they
//!    are read, then flush it to stable storage (`F_FULLFSYNC` on macOS);
//! 3. stat the source again: a changed size, mtime, inode or device, or a
//!    byte count that differs from the size, discards the temporary file and
//!    skips;
//! 4. decompress the temporary file and compare its length and SHA-256 with
//!    the source's; any difference discards it and skips;
//! 5. publish it with a no-clobber `linkat` and flush the directory (any
//!    sync failure removes the copy and keeps the source) and record the
//!    index row ([`archive_one`] stops here and returns a [`Published`]);
//! 6. later, in a batch ([`remove_published`]): check the source is not
//!    open in a host-wide open-file observation taken after the batch's
//!    last copy was verified, and that its size, mtime, inode and device
//!    are still the ones compressed, then unlink it (relative to its
//!    validated root) and flush its directory; otherwise discard the copy.
//!
//! The source is never removed before a verified, durable copy exists, and
//! the copy is only ever removed while the source still exists, so the two
//! are never both gone. A crash between steps 5 and 6 leaves both; the next
//! run finds the destination and verifies it decodes to exactly the
//! source's bytes, then removes the source in its own batch. If the source
//! changed meanwhile and the copy is the one this lane indexed, the stale
//! copy is discarded, so a source and a divergent copy of it never both
//! remain. A crash before step 5 leaves a temporary file, which the next
//! run removes.
//!
//! Every file operation goes through [`super::safe_fs`]: no symbolic link is
//! followed beneath a provider root and no mount point is crossed.

use std::ffi::CString;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::eligibility::{OpenState, SkipReason};
use super::index::{ArchiveRow, Index};
use super::open_files::{OpenFileProbe, OpenFiles};
use super::pacing::Pacer;
use super::providers::{TEMP_PREFIX, Transcript};
use super::safe_fs::{self, Dir, Root, Stamp};

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

/// Paces the bytes read from the inner reader.
struct PacedReader<'p, R> {
    inner: R,
    pacer: &'p mut Pacer,
}

impl<R: Read> Read for PacedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.pacer.consume(read as u64);
        Ok(read)
    }
}

/// Length and SHA-256 of what an open zstd file decompresses to. Paced on
/// the compressed bytes read from disk, which is the I/O the pacing bounds;
/// the decoded stream (about five times larger) is hashed in full but not
/// paced again.
fn digest_compressed(file: File, pacer: &mut Pacer) -> io::Result<(u64, String)> {
    let paced = PacedReader { inner: file, pacer };
    digest_reader(
        zstd::stream::read::Decoder::new(paced)?,
        &mut Pacer::new(None),
    )
}

/// Stream `source` through zstd into `temporary`, returning the bytes read
/// and their SHA-256. The temporary file is on stable storage before this
/// returns.
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
    safe_fs::full_sync(temporary.as_raw_fd())?;
    Ok((total, hex(&hasher.finalize())))
}

/// The roots one transcript is confined to.
pub(crate) struct Confinement<'r> {
    /// Where the transcript lives (Codex home, Claude projects).
    pub source: &'r Root,
    /// Where its archive goes (Codex home, Aethyme's transcript state).
    pub destination: &'r Root,
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

impl Entries {
    /// Remove the copy (the source still exists) and its index row.
    fn discard_copy(&self, index: &Index, transcript: &Transcript) {
        super::best_effort(self.destination_dir.unlink(&self.destination_name));
        super::best_effort(self.destination_dir.sync());
        super::best_effort(index.remove_archive(&transcript.path));
    }
}

/// A verified, durable, indexed copy whose source is still in place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Published {
    pub archived: Archived,
    /// The source as it was compressed.
    pub before: Stamp,
}

/// Remove the source of a [`Published`] copy, or discard the copy.
///
/// The caller judges a whole batch against one host-wide observation it
/// renewed after the batch's last copy was verified
/// ([`OpenFiles::renew`]). Safety: the source is unlinked only when that
/// observation is complete and does not show it open, and, checked after
/// it, its size, mtime, inode and device are still the ones compressed
/// (once here and once more by `unlink_if`). Anything else discards the
/// copy and keeps the source. Residual window: a process that opens the
/// source after the batch observation and has not written by the unlink
/// would write to the unlinked file; that window is one listing per batch.
pub(crate) fn remove_published<P: OpenFileProbe>(
    index: &Index,
    open_files: &mut OpenFiles<P>,
    transcript: &Transcript,
    confinement: &Confinement<'_>,
    before: Stamp,
) -> Result<(), Skipped> {
    let entries = entries_for(transcript, confinement, false)?;
    let open = open_files.state_of(&transcript.path);
    if open != OpenState::Closed {
        entries.discard_copy(index, transcript);
        let reason = if open == OpenState::Open {
            SkipReason::OpenByProcess
        } else {
            SkipReason::OpenCheckUnavailable
        };
        return Err(Skipped::new(
            reason,
            "source open, or its open state unknown, before removal; copy discarded",
        ));
    }
    let unchanged = entries
        .source_dir
        .stamp(&entries.source_name)
        .is_ok_and(|now| now == before);
    if !unchanged {
        entries.discard_copy(index, transcript);
        return Err(Skipped::new(
            SkipReason::ModifiedDuringCompression,
            "source changed before removal; copy discarded",
        ));
    }
    entries
        .source_dir
        .unlink_if(&entries.source_name, before)
        .map_err(|error| Skipped::io("cannot remove", &transcript.path, &error))?;
    // The copy is durable already; a failure here only delays the removal's
    // durability, it cannot lose data.
    super::best_effort(entries.source_dir.sync());
    Ok(())
}

/// Open the source and destination entries beneath their roots.
fn entries_for(
    transcript: &Transcript,
    confinement: &Confinement<'_>,
    create_destination: bool,
) -> Result<Entries, Skipped> {
    let (source_dir, source_name) = confinement
        .source
        .parent_of(&transcript.path, false)
        .map_err(|error| unsafe_path(&transcript.path, &error))?;
    let (destination_dir, destination_name) = confinement
        .destination
        .parent_of(&transcript.destination, create_destination)
        .map_err(|error| unsafe_path(&transcript.destination, &error))?;
    Ok(Entries {
        source_dir,
        source_name,
        destination_dir,
        destination_name,
    })
}

/// Finish an archive an interrupted run published but did not complete:
/// the destination must decode to exactly the source's bytes.
fn resume<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    entries: &Entries,
    before: Stamp,
) -> Result<Published, Skipped> {
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
        let ours = context
            .index
            .archive_for(&transcript.path)
            .ok()
            .flatten()
            .is_some_and(|row| row.sha256 == copy.1);
        if ours {
            // This lane's copy of an older version of a source that has
            // since changed: the source is the newer data, so the stale copy
            // goes and the source is archived afresh once it is idle again.
            entries.discard_copy(context.index, transcript);
            return Err(Skipped::new(
                SkipReason::ModifiedDuringCompression,
                "source changed after an earlier run published its copy; stale copy discarded",
            ));
        }
        return Err(conflict(format!(
            "{destination} exists with different content"
        )));
    }
    // The copy may predate a durable sync: make it durable before relying
    // on it.
    if let Err(error) = entries.destination_dir.sync() {
        return Err(Skipped::new(
            SkipReason::SyncFailed,
            format!(
                "cannot sync {}: {error}; source kept",
                transcript.destination.display()
            ),
        ));
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
    Ok(Published {
        archived: Archived {
            bytes_in: source.0,
            bytes_out: copy_stamp.len,
            sha256: source.1,
            resumed: true,
            compress_time: Duration::ZERO,
            verify_time,
        },
        before,
    })
}

fn unsafe_path(path: &Path, error: &io::Error) -> Skipped {
    Skipped::new(
        SkipReason::UnsafePath,
        format!("{}: {error}", path.display()),
    )
}

/// Archive one transcript up to a published, indexed copy (steps 1-5 of the
/// module documentation); the source is removed later, in a batch, by
/// [`remove_published`].
pub(crate) fn archive_one<P: OpenFileProbe>(
    context: &mut ArchiveContext<'_, P>,
    transcript: &Transcript,
    confinement: &Confinement<'_>,
) -> Result<Published, Skipped> {
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
    match context.open_files.state_of(&transcript.path) {
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
        Ok(true) => return resume(context, transcript, &entries, before),
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

    match temporary.publish(&entries.destination_name) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return Err(Skipped::new(
                SkipReason::DestinationConflict,
                "destination appeared meanwhile",
            ));
        }
        Err(error) => {
            // The link may exist but is not proven durable: drop it, keep the
            // source, record no row.
            if entries
                .destination_dir
                .exists(&entries.destination_name)
                .unwrap_or(false)
            {
                entries.discard_copy(context.index, transcript);
            }
            return Err(Skipped::new(
                SkipReason::SyncFailed,
                format!(
                    "cannot publish {} durably: {error}; source kept",
                    transcript.destination.display()
                ),
            ));
        }
    }
    let row = row_for(transcript, bytes_in, bytes_out, &sha256, context.now_ms);
    if let Err(error) = context.index.record_archive(&row) {
        // Without its index row a copy cannot be restored with its hash
        // checked; keep the source and drop the copy.
        entries.discard_copy(context.index, transcript);
        return Err(Skipped::new(SkipReason::Io, error));
    }
    if !context.hooks.after_publish(&transcript.path) {
        return Err(Skipped::new(
            SkipReason::Interrupted,
            "stopped after publishing",
        ));
    }
    Ok(Published {
        archived: Archived {
            bytes_in,
            bytes_out,
            sha256,
            resumed: false,
            compress_time,
            verify_time,
        },
        before,
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
    /// Bytes of the restored file.
    pub bytes: u64,
    /// SHA-256 of the archived content (what the index recorded).
    pub sha256: String,
    pub hash_checked: bool,
    /// Bytes of a newer transcript found at the original path and appended
    /// after the archived content (0 when there was none).
    pub merged_fragment_bytes: u64,
    /// The file at the original path already started with the archive's
    /// exact bytes (an interrupted earlier restore): only the cleanup ran.
    pub already_restored: bool,
    /// A fragment backup that could not be reconciled and was kept.
    pub fragment_backup_kept: Option<PathBuf>,
}

/// Copies the archive's decoded content, then an optional fragment, into a
/// temporary file, hashing what it writes.
struct Merge<'w> {
    writer: BufWriter<&'w File>,
    archive_hasher: Sha256,
    all_hasher: Sha256,
    archive_bytes: u64,
    total: u64,
    last: Option<u8>,
}

impl<'w> Merge<'w> {
    fn new(file: &'w File) -> Self {
        Self {
            writer: BufWriter::with_capacity(CHUNK, file),
            archive_hasher: Sha256::new(),
            all_hasher: Sha256::new(),
            archive_bytes: 0,
            total: 0,
            last: None,
        }
    }

    fn write(&mut self, bytes: &[u8], from_archive: bool) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if from_archive {
            self.archive_hasher.update(bytes);
            self.archive_bytes += bytes.len() as u64;
        }
        self.all_hasher.update(bytes);
        self.writer.write_all(bytes)?;
        self.total += bytes.len() as u64;
        self.last = bytes.last().copied();
        Ok(())
    }

    fn copy(&mut self, mut reader: impl Read, from_archive: bool) -> io::Result<()> {
        let mut buffer = vec![0u8; CHUNK];
        loop {
            let read = match reader.read(&mut buffer) {
                Ok(0) => return Ok(()),
                Ok(read) => read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            self.write(&buffer[..read], from_archive)?;
        }
    }
}

/// Decompress `plan.archive` (beneath `archive_root`) back to
/// `plan.original` (beneath `original_root`), verifying the hash, then
/// remove the archive.
///
/// When a newer transcript (a fragment an agent started at the same path
/// after the archive was taken) exists at the original path, the result is
/// the archived content followed by the fragment's lines: written to a
/// temporary file, verified by re-reading it, and atomically put in place
/// of the fragment. The fragment is kept under a backup name until the
/// merged file is published and every byte written to it meanwhile has been
/// carried over; a backup that cannot be reconciled is kept.
///
/// A restore intent is recorded before publishing and cleared after the
/// archive is removed. When the file at the original path already starts
/// with exactly the archive's bytes (a restore that published and stopped
/// before removing the archive), nothing is merged again: only the cleanup
/// is finished.
pub(crate) fn restore_one(
    plan: &RestorePlan,
    archive_root: &Root,
    original_root: &Root,
    index: Option<&Index>,
    now_ms: i64,
) -> Result<Restored, String> {
    let original = plan.original.display();
    let archive = plan.archive.display();
    let (original_dir, original_name) = original_root
        .parent_of(&plan.original, true)
        .map_err(|error| format!("unsafe restore target {original}: {error}"))?;
    let fragment = match original_dir.exists(&original_name) {
        Ok(false) => None,
        Ok(true) => Some(
            original_dir
                .stamp(&original_name)
                .map_err(|error| format!("{original} exists but is not a plain file: {error}"))?,
        ),
        Err(error) => return Err(format!("cannot stat {original}: {error}")),
    };
    let (archive_dir, archive_name) = archive_root
        .parent_of(&plan.archive, false)
        .map_err(|error| format!("unsafe archive path {archive}: {error}"))?;
    let open_archive = || {
        archive_dir
            .open_read(&archive_name)
            .map_err(|error| format!("cannot open {archive}: {error}"))
    };
    let (_, archive_stamp) = open_archive()?;
    let cleanup = |restored: Restored| -> Result<Restored, String> {
        archive_dir
            .unlink_if(&archive_name, archive_stamp)
            .map_err(|error| {
                format!("restored {original} but could not remove {archive}: {error}")
            })?;
        super::best_effort(archive_dir.sync());
        if let Some(index) = index {
            index.remove_archive(&plan.original)?;
            index.clear_restore_intent(&plan.original)?;
        }
        Ok(restored)
    };

    if let Some(present) = fragment {
        let (archive_file, _) = open_archive()?;
        let mut unpaced = Pacer::new(None);
        let (archive_len, archive_digest) = digest_compressed(archive_file, &mut unpaced)
            .map_err(|error| format!("cannot decode {archive}: {error}"))?;
        if let Some(expected) = &plan.sha256
            && *expected != archive_digest
        {
            return Err(format!(
                "{archive} decodes to SHA-256 {archive_digest}, the index recorded {expected}; archive kept"
            ));
        }
        if present.len >= archive_len
            && prefix_digest(&original_dir, &original_name, archive_len).as_deref()
                == Some(archive_digest.as_str())
        {
            return cleanup(Restored {
                archive: plan.archive.clone(),
                original: plan.original.clone(),
                bytes: present.len,
                sha256: archive_digest,
                hash_checked: plan.sha256.is_some(),
                merged_fragment_bytes: 0,
                already_restored: true,
                fragment_backup_kept: None,
            });
        }
    }

    let (archive_file, _) = open_archive()?;
    // Keep the fragment reachable under a backup name until the merged file
    // is published and reconciled.
    let backup = match fragment {
        Some(_) => Some(
            original_dir
                .link_unique(&original_name, TEMP_PREFIX, ".fragment")
                .map_err(|error| format!("cannot back up {original}: {error}"))?,
        ),
        None => None,
    };
    if let Some(index) = index
        && let Err(error) = index.record_restore_intent(&plan.original, &plan.archive, now_ms)
    {
        if let Some(backup) = &backup {
            super::best_effort(original_dir.unlink(backup));
        }
        return Err(error);
    }
    let result = write_restored(plan, &original_dir, &original_name, archive_file, fragment);
    let backup_kept = match (&backup, fragment) {
        (Some(backup), Some(before)) => {
            let published = result.is_ok();
            reconcile_backup(&original_dir, &original_name, backup, before, published).map(|kept| {
                plan.original
                    .with_file_name(kept.to_string_lossy().into_owned())
            })
        }
        _ => None,
    };
    let mut restored = result?;
    restored.fragment_backup_kept = backup_kept;
    cleanup(restored)
}

/// SHA-256 of the first `len` bytes of `name`, or `None` when it is shorter
/// or unreadable.
fn prefix_digest(directory: &Dir, name: &CString, len: u64) -> Option<String> {
    let (file, _) = directory.open_read(name).ok()?;
    let mut unpaced = Pacer::new(None);
    let (read, digest) = digest_reader(file.take(len), &mut unpaced).ok()?;
    (read == len).then_some(digest)
}

/// Settle the fragment's backup link after a merge attempt. Returns the
/// backup's name when it had to be kept.
///
/// Unpublished: the original path still holds the fragment, so the backup
/// (the same file) is simply removed. Published: the backup is the old
/// fragment file; bytes a writer appended to it after the merge read it are
/// appended to the published file, and the backup is removed only once it
/// is exactly what was carried over. Anything else keeps it.
pub(crate) fn reconcile_backup(
    directory: &Dir,
    original_name: &CString,
    backup: &CString,
    before: Stamp,
    published: bool,
) -> Option<CString> {
    let Ok(now) = directory.stamp(backup) else {
        return Some(backup.clone());
    };
    if !published {
        let same_file = directory
            .stamp(original_name)
            .is_ok_and(|original| original.inode == now.inode && original.device == now.device);
        if same_file {
            super::best_effort(directory.unlink(backup));
            super::best_effort(directory.sync());
            return None;
        }
        return Some(backup.clone());
    }
    if now == before {
        super::best_effort(directory.unlink(backup));
        super::best_effort(directory.sync());
        return None;
    }
    let grew = now.inode == before.inode && now.device == before.device && now.len > before.len;
    if !grew {
        return Some(backup.clone());
    }
    let appended = append_growth(directory, original_name, backup, before, now);
    if appended.is_ok() && directory.stamp(backup).ok() == Some(now) {
        super::best_effort(directory.unlink(backup));
        super::best_effort(directory.sync());
        return None;
    }
    Some(backup.clone())
}

/// Append to the published file what a writer added to the old fragment
/// (now the backup) after the merge read it.
fn append_growth(
    directory: &Dir,
    original_name: &CString,
    backup: &CString,
    before: Stamp,
    now: Stamp,
) -> io::Result<()> {
    let (mut source, opened) = directory.open_read(backup)?;
    if opened != now {
        return Err(io::Error::other("the fragment changed again"));
    }
    std::io::Seek::seek(&mut source, std::io::SeekFrom::Start(before.len))?;
    let target = directory.open_append(original_name)?;
    let mut writer = BufWriter::with_capacity(CHUNK, &target);
    std::io::copy(&mut source.take(now.len - before.len), &mut writer)?;
    writer.flush()?;
    drop(writer);
    safe_fs::full_sync(target.as_raw_fd())
}

/// Write the restored transcript (archived content, then any fragment) to
/// a temporary file beside the original, verify it by re-reading it, and
/// publish it: no-clobber when there was no fragment, atomically in place
/// of the fragment otherwise.
fn write_restored(
    plan: &RestorePlan,
    original_dir: &Dir,
    original_name: &CString,
    archive_file: File,
    fragment: Option<Stamp>,
) -> Result<Restored, String> {
    let original = plan.original.display();
    let archive = plan.archive.display();
    let temporary = original_dir
        .create_temp(TEMP_PREFIX, TEMP_SUFFIX)
        .map_err(|error| format!("cannot create a temporary file beside {original}: {error}"))?;
    let mut merge = Merge::new(&temporary.file);
    let decoder = zstd::stream::read::Decoder::new(archive_file)
        .map_err(|error| format!("cannot decode {archive}: {error}"))?;
    merge
        .copy(decoder, true)
        .map_err(|error| format!("cannot restore {archive}: {error}"))?;
    let archive_digest = hex(&merge.archive_hasher.clone().finalize());
    if let Some(expected) = &plan.sha256
        && *expected != archive_digest
    {
        return Err(format!(
            "{archive} decodes to SHA-256 {archive_digest}, the index recorded {expected}; archive kept"
        ));
    }
    let mut merged_fragment_bytes = 0;
    if let Some(expected) = fragment {
        let (fragment_file, opened) = original_dir
            .open_read(original_name)
            .map_err(|error| format!("cannot read {original}: {error}"))?;
        if opened != expected {
            return Err(format!(
                "{original} changed during the restore; nothing was changed"
            ));
        }
        if expected.len > 0 && merge.last.is_some_and(|byte| byte != b'\n') {
            merge
                .write(b"\n", false)
                .map_err(|error| format!("cannot write beside {original}: {error}"))?;
        }
        // Exactly the bytes stamped: anything appended later is carried
        // over from the backup after publication.
        merge
            .copy(fragment_file.take(expected.len), false)
            .map_err(|error| format!("cannot merge {original}: {error}"))?;
        merged_fragment_bytes = expected.len;
    }
    merge
        .writer
        .flush()
        .map_err(|error| format!("cannot write beside {original}: {error}"))?;
    let total = merge.total;
    let written = hex(&merge.all_hasher.clone().finalize());
    drop(merge);
    // Verify what reached the file, not what was handed to the writer.
    let (reread, _) = original_dir
        .open_read(&temporary.name)
        .map_err(|error| format!("cannot verify the restore of {original}: {error}"))?;
    let mut unpaced = Pacer::new(None);
    let check = digest_reader(reread, &mut unpaced)
        .map_err(|error| format!("cannot verify the restore of {original}: {error}"))?;
    if check != (total, written) {
        return Err(format!(
            "the restore of {original} did not verify; nothing was changed"
        ));
    }
    match fragment {
        None => temporary
            .publish(original_name)
            .map_err(|error| format!("cannot publish {original}: {error}"))?,
        Some(expected) => {
            // Growth is carried over from the backup afterwards; a replaced
            // file is not, so it aborts the merge.
            let same_file = original_dir.stamp(original_name).is_ok_and(|now| {
                now.inode == expected.inode
                    && now.device == expected.device
                    && now.len >= expected.len
            });
            if !same_file {
                return Err(format!(
                    "{original} was replaced during the restore; nothing was changed"
                ));
            }
            temporary
                .publish_replacing(original_name)
                .map_err(|error| format!("cannot publish {original}: {error}"))?;
        }
    }
    Ok(Restored {
        archive: plan.archive.clone(),
        original: plan.original.clone(),
        bytes: total,
        sha256: archive_digest,
        hash_checked: plan.sha256.is_some(),
        merged_fragment_bytes,
        already_restored: false,
        fragment_backup_kept: None,
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
            super::best_effort(directory.sync());
            removed += 1;
        }
    }
    removed
}
