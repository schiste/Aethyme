use super::*;
use std::io::Read;

const WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES: u64 = 8 * 1024;
const CLEANUP_TARGET_SNAPSHOT_UNAVAILABLE: &str = "the cleanup plan could not resolve primary HEAD, so session eligibility was not checked against a target bound to the plan";

#[derive(Clone, Copy)]
enum CleanupDeliveryTargets<'a> {
    /// Resolve fresh for one direct cleanup inspection.
    Fresh,
    /// Reuse the targets bound into a cleanup plan.
    Snapshot(&'a [String]),
    /// The plan's initial primary HEAD lookup failed, so its target set is not
    /// suitable for proving that any session's work is represented.
    SnapshotUnavailable,
}

#[derive(Clone, Copy)]
struct CleanupStatusLimits {
    deadline: std::time::Instant,
    output_bytes: usize,
    path_fields: usize,
}

impl CleanupStatusLimits {
    fn new(
        overall_deadline: Option<std::time::Instant>,
        output_bytes: usize,
        path_fields: usize,
    ) -> Self {
        let per_call_deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        Self {
            deadline: overall_deadline.map_or(per_call_deadline, |deadline| {
                deadline.min(per_call_deadline)
            }),
            output_bytes,
            path_fields,
        }
    }
}

struct CleanupInspectionContext<'a> {
    branch_tips: Option<&'a std::collections::BTreeMap<String, String>>,
    delivery_targets: CleanupDeliveryTargets<'a>,
    landing_cache: &'a mut crate::representation::LandingVerdictCache,
    status_limits: CleanupStatusLimits,
}

/// Read the two small files used as ownership witnesses without following a
/// replacement symlink or blocking on a FIFO. A witness that is not a
/// bounded regular file is a conclusive non-match; other I/O errors remain
/// unavailable so callers can preserve their retry state.
fn read_bounded_ownership_witness(path: &Path) -> Result<Option<Vec<u8>>, GitError> {
    Ok(read_bounded_ownership_witness_with_metadata(path)?.map(|witness| witness.bytes))
}

struct BoundedOwnershipWitness {
    bytes: Vec<u8>,
    metadata: std::fs::Metadata,
}

fn read_bounded_ownership_witness_with_metadata(
    path: &Path,
) -> Result<Option<BoundedOwnershipWitness>, GitError> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open the reparse point itself so the
        // handle metadata below can reject it without following its target.
        options.custom_flags(0x0020_0000);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        #[cfg(unix)]
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Ok(None),
        Err(error) => return Err(ownership_inspection_error(path, "open", error)),
    };
    let metadata = file
        .metadata()
        .map_err(|error| ownership_inspection_error(path, "inspect opened", error))?;
    if !metadata.file_type().is_file() || metadata.len() > WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES {
        return Ok(None);
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ownership_inspection_error(path, "read", error))?;
    if bytes.len() as u64 > WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES {
        return Ok(None);
    }
    Ok(Some(BoundedOwnershipWitness { bytes, metadata }))
}

fn ownership_inspection_error(path: &Path, action: &str, error: std::io::Error) -> GitError {
    GitError::UntrustedOutput {
        detail: format!(
            "could not {action} worktree ownership path {}: {error}",
            path.display()
        ),
    }
}

fn worktree_size_estimate(
    path: &Path,
    record_key: &str,
    worktree_present: bool,
    broker_owned: bool,
    scan: crate::SizeScan,
    records: &mut crate::measurement::SizeRecords,
    measure: impl FnOnce(&Path) -> Option<crate::disk_headroom::DirectoryUsage>,
) -> (Option<u64>, Option<u64>) {
    if !worktree_present {
        return (Some(0), Some(0));
    }
    if !broker_owned {
        return (None, None);
    }
    if scan.measures() {
        let measured = measure(path);
        if let Some(usage) = measured {
            records.record_usage(record_key, usage.bytes, Some(usage.inodes), now_ms());
        }
        measured
            .map(|usage| (Some(usage.bytes), Some(usage.inodes)))
            .unwrap_or((None, None))
    } else {
        records
            .get(record_key)
            .map(|record| (Some(record.bytes), record.inodes))
            .unwrap_or((None, None))
    }
}

impl Broker {
    /// Claim repository-wide checkout lifecycle mutations.
    ///
    /// `Broker::open` resolves both fields from the repository's primary
    /// checkout, even when the command started inside a linked worktree. Check
    /// that invariant again before deriving the lock location so the claim is
    /// always stored in the same repository state directory, outside linked
    /// checkouts that a lifecycle operation may remove.
    pub(crate) fn claim_worktree_lifecycle(
        &self,
    ) -> Result<crate::file_lock::ExclusiveFileLock, BrokerOpError> {
        self.store.require_live_schema_compatible()?;
        let main_root = self
            .main_root
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.clone(),
                source,
            })?;
        let broker_root = self
            .repo
            .root()
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.repo.root().to_path_buf(),
                source,
            })?;
        let common_dir = self
            .main_git_common_dir()?
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: self.main_root.join(".git"),
                source,
            })?;
        if broker_root != main_root || common_dir.parent() != Some(main_root.as_path()) {
            return Err(GitError::UntrustedOutput {
                detail:
                    "worktree lifecycle claim could not confirm the primary repository identity"
                        .into(),
            }
            .into());
        }
        #[cfg(test)]
        if let Some(observer) = &self.worktree_lifecycle_test_observer {
            observer(&main_root);
        }
        let claim =
            crate::file_lock::acquire_repository_lifecycle_lock(&main_root).map_err(|source| {
                BrokerError::Io {
                    path: main_root.join(".aethyme/run"),
                    source,
                }
            })?;
        // The floor may have advanced while this controller waited. Keep the
        // claim only if the currently committed markers still permit us.
        self.store.require_live_schema_compatible()?;
        Ok(claim)
    }

    /// Whether a directory is the remains of a worktree of *this* repository
    /// whose registration is already gone.
    ///
    /// The `.git` file survives the interrupted removal pointing at
    /// `<common dir>/worktrees/<name>`, and that pointer is the evidence. It is
    /// checked rather than trusted for containment -- the caller has already
    /// proven the path sits under the broker-owned root, so this only has to
    /// establish which repository the remains belong to, not that they are safe
    /// to touch.
    pub(super) fn is_orphaned_worktree_of_this_repository_checked(
        &self,
        path: &Path,
    ) -> Result<bool, GitError> {
        let expected = self.main_git_common_dir()?;
        let pointer_path = path.join(".git");
        let Some(pointer) = read_bounded_ownership_witness(&pointer_path)? else {
            return Ok(false);
        };
        let Ok(pointer) = std::str::from_utf8(&pointer) else {
            return Ok(false);
        };
        let Some(gitdir) = pointer.trim().strip_prefix("gitdir:") else {
            return Ok(false);
        };
        let gitdir = PathBuf::from(gitdir.trim());
        let gitdir = if gitdir.is_absolute() {
            gitdir
        } else {
            path.join(gitdir)
        };
        // The gitdir itself is gone -- that is the definition of this state --
        // so compare the `worktrees/<name>` parent it named, not the leaf.
        let Some(worktrees_dir) = gitdir.parent() else {
            return Ok(false);
        };
        if worktrees_dir.file_name() != Some(std::ffi::OsStr::new("worktrees")) {
            return Ok(false);
        }
        let Some(claimed_common_dir) = worktrees_dir.parent() else {
            return Ok(false);
        };
        match std::fs::symlink_metadata(&gitdir) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(ownership_inspection_error(&gitdir, "inspect", error)),
        }
        let canonical = |candidate: &Path| match candidate.canonicalize() {
            Ok(path) => Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(candidate.to_path_buf())
            }
            Err(error) => Err(ownership_inspection_error(candidate, "resolve", error)),
        };
        Ok(canonical(claimed_common_dir)? == canonical(&expected)?)
    }

    pub(super) fn worktree_root_marker_matches_checked(
        &self,
        root: &Path,
    ) -> Result<bool, GitError> {
        let marker_path = root.join(WORKTREE_ROOT_MARKER);
        let Some(bytes) = read_bounded_ownership_witness(&marker_path)? else {
            return Ok(false);
        };
        let Ok(marker) = serde_json::from_slice::<WorktreeRootMarker>(&bytes) else {
            return Ok(false);
        };
        if marker.schema_version != WORKTREE_ROOT_SCHEMA_VERSION {
            return Ok(false);
        }
        let common_dir = self.main_git_common_dir()?;
        let repository_key = crate::host_state::repository_key(&self.main_root, Some(&common_dir));
        Ok(marker.repository_key == repository_key && marker.repository_root == self.main_root)
    }

    /// The reason for a cleanup inspection that could not complete. In a
    /// shallow clone the usual cause is a merge-base across the shallow
    /// boundary, which git reports with no message at all, so the reason
    /// names the clone and how to deepen it (#525).
    pub(super) fn cleanup_inspection_failure(&self, error: &dyn std::fmt::Display) -> String {
        let mut reason = format!("cleanup inspection failed: {error}");
        if self.repo.is_shallow() {
            reason.push_str(
                " (this repository is a shallow clone, so history older than its \
                 shallow boundary is missing and the session cannot be related to a \
                 delivery target; deepen it with `aethyme broker advanced git --session \
                 <id> --repo <owner/name> --reason \"<authorization>\" -- fetch \
                 --shallow-since=<date> origin <default-branch>`, or `--unshallow`, \
                 then re-plan)",
            );
        }
        reason
    }

    pub(super) fn cleanup_eligibility(
        &self,
        session_id: i64,
        worktree_path: &Path,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        let delivery_targets = self.cleanup_delivery_targets()?;
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_eligibility_with_targets_and_cache(
            session_id,
            worktree_path,
            &delivery_targets,
            &mut landing_cache,
        )
    }

    fn cleanup_eligibility_with_targets_and_cache(
        &self,
        session_id: i64,
        worktree_path: &Path,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        self.cleanup_eligibility_with_status_limits(
            session_id,
            worktree_path,
            delivery_targets,
            landing_cache,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            ),
        )
    }

    fn cleanup_eligibility_with_status_limits(
        &self,
        session_id: i64,
        worktree_path: &Path,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
        status_limits: CleanupStatusLimits,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        if worktree_path == self.main_root.as_path() {
            return Ok((
                CleanupDisposition::UnsafePath,
                "the primary checkout is never removable by broker cleanup".into(),
                None,
            ));
        }
        let metadata =
            std::fs::symlink_metadata(worktree_path).map_err(|source| BrokerError::Io {
                path: worktree_path.to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Ok((
                CleanupDisposition::UnsafePath,
                "worktree path is a symlink".into(),
                None,
            ));
        }

        let checkout = GitRepo::discover(worktree_path)?;
        let canonical_path = worktree_path
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: worktree_path.to_path_buf(),
                source,
            })?;
        if checkout.root() != canonical_path {
            return Ok((
                CleanupDisposition::UnsafePath,
                "worktree path resolves to a parent repository rather than its own checkout".into(),
                None,
            ));
        }
        match checkout.is_dirty_until_with_limits(
            status_limits.deadline,
            status_limits.output_bytes,
            status_limits.path_fields,
        ) {
            Ok(false) => {}
            Ok(true) => {
                return Ok((
                    CleanupDisposition::Dirty,
                    "worktree has uncommitted or untracked changes".into(),
                    None,
                ));
            }
            Err(error) => {
                return Ok((
                    CleanupDisposition::InspectionFailed,
                    self.cleanup_inspection_failure(&error),
                    None,
                ));
            }
        }

        let ignored_verdict = crate::auto_cleanup::AutoCleanupPolicy::load(self.main_root())
            .and_then(|policy| crate::auto_cleanup::ignored_content_reason(worktree_path, &policy));
        match ignored_verdict {
            Ok(None) => {}
            Ok(Some(reason)) | Err(reason) => {
                return Ok((CleanupDisposition::Dirty, reason, None));
            }
        }

        let session_head = checkout.head_commit()?;
        let session = self.store.session(session_id)?;
        let (provenance, reason) = self.cleanup_provenance_with_cache(
            &session,
            &session_head,
            delivery_targets,
            landing_cache,
        )?;
        let disposition = match provenance.representation {
            CleanupRepresentation::Represented => CleanupDisposition::Eligible,
            CleanupRepresentation::Pending => CleanupDisposition::PendingCommits,
            CleanupRepresentation::Unproven => CleanupDisposition::UnprovenProvenance,
        };
        Ok((disposition, reason, Some(provenance)))
    }

    /// The commits cleanup judges a session's work against: the primary
    /// checkout's HEAD, the integration tip and the configured upstream,
    /// sorted and deduplicated.
    pub(crate) fn cleanup_delivery_targets(&self) -> Result<Vec<String>, BrokerOpError> {
        let mut delivery_targets = vec![self.repo.head_commit()?];
        if let Some(integration) = self.integration_tip() {
            delivery_targets.push(integration);
        }
        if let Some((_upstream, head)) = self.repo.tracking_upstream() {
            delivery_targets.push(head);
        }
        delivery_targets.sort();
        delivery_targets.dedup();
        Ok(delivery_targets)
    }

    /// Whether a session's work is represented on a delivery target.
    ///
    /// Two kinds of evidence answer this, and both are needed. Ancestry is the
    /// primary one: it is cheap, always available, and needs no prior record.
    /// It is also blind to a squash merge, which rewrites the SHA so the
    /// session's commits appear nowhere in the target's history even though
    /// its content is there -- and a session that merged that way is left
    /// uncleanable forever (#164). A recorded representation covers exactly
    /// that gap, so it is consulted only when ancestry comes up short.
    pub(super) fn cleanup_provenance(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
    ) -> Result<(CleanupProvenance, String), BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_provenance_with_cache(
            session,
            session_head,
            delivery_targets,
            &mut landing_cache,
        )
    }

    fn cleanup_provenance_with_cache(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<(CleanupProvenance, String), BrokerOpError> {
        let (mut provenance, reason) =
            self.cleanup_provenance_from_ancestry(session, session_head, delivery_targets)?;
        if matches!(
            provenance.representation,
            CleanupRepresentation::Represented
        ) {
            return Ok((provenance, reason));
        }
        let Some(evidence) =
            self.recorded_representation_evidence(session, session_head, delivery_targets)?
        else {
            // The predicate the representation scan and the cleanup audit use
            // (#408). Without it the audit called rebased-then-merged work
            // `in_target` while this plan held the same worktree as unproven.
            // Every positive names fixed commits, so the plan digest over this
            // reason stays stable until a delivery target moves.
            if let Some((target, evidence, landed_by)) = self
                .landing_on_delivery_targets_with_cache(
                    session_head,
                    delivery_targets,
                    landing_cache,
                )?
            {
                provenance.representation = CleanupRepresentation::Represented;
                provenance.pending_commit_count = 0;
                provenance.represented_on = Some(target.clone());
                let by = landed_by
                    .as_deref()
                    .map(|commit| format!(" (landed by {})", short_commit(commit)))
                    .unwrap_or_default();
                provenance.represented_by_commit = landed_by;
                return Ok((
                    provenance,
                    format!(
                        "session head {} is represented on delivery target {} by {}{by}",
                        short_commit(session_head),
                        short_commit(&target),
                        evidence.as_str()
                    ),
                ));
            }
            // Last resort, and the one that survives any merge strategy: the
            // commits exist on a remote, so this directory is not where the
            // work lives.
            if let Some((remote_ref, tracked)) = self.remote_durability_evidence(session_head) {
                provenance.representation = CleanupRepresentation::Represented;
                provenance.pending_commit_count = 0;
                provenance.represented_on = Some(tracked.clone());
                return Ok((
                    provenance,
                    format!(
                        "session head {} is reachable from {}, verified current against the remote",
                        short_commit(session_head),
                        remote_ref
                    ),
                ));
            }
            return Ok((provenance, reason));
        };
        provenance.representation = CleanupRepresentation::Represented;
        provenance.pending_commit_count = 0;
        let reason = match evidence {
            RecordedRepresentationEvidence::Landed {
                representing,
                target,
            } => {
                let text = format!(
                    "recorded representation: session head {} landed as {} on delivery target {}",
                    short_commit(session_head),
                    short_commit(&representing),
                    short_commit(&target)
                );
                provenance.represented_on = Some(target);
                provenance.represented_by_commit = Some(representing);
                text
            }
            RecordedRepresentationEvidence::AlreadyHeld => format!(
                "recorded representation: the default branch already held the net content of \
                 session head {}",
                short_commit(session_head)
            ),
        };
        Ok((provenance, reason))
    }

    /// The first delivery target `head`'s work landed on, with its evidence.
    ///
    /// Ancestry is asked of every target before any target gets the deep
    /// content and patch search (#588). The targets arrive sorted by SHA, so
    /// asking each one the full question in turn made a merged branch pay a
    /// candidate walk over whichever non-containing target sorted first --
    /// a trailing primary checkout, or an integration branch that diverged --
    /// before reaching the one a single `merge-base --is-ancestor` settles.
    /// Only squash and rebase deliveries, which ancestry cannot see, reach the
    /// search.
    pub(super) fn landing_on_delivery_targets(
        &self,
        head: &str,
        delivery_targets: &[String],
    ) -> Result<Option<(String, crate::LandingEvidence, Option<String>)>, BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.landing_on_delivery_targets_with_cache(head, delivery_targets, &mut landing_cache)
    }

    fn landing_on_delivery_targets_with_cache(
        &self,
        head: &str,
        delivery_targets: &[String],
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<Option<(String, crate::LandingEvidence, Option<String>)>, BrokerOpError> {
        if let Some(target) = delivery_targets
            .iter()
            .find(|target| self.repo.is_ancestor(head, target))
        {
            return Ok(Some((
                target.clone(),
                crate::LandingEvidence::Ancestry,
                None,
            )));
        }
        for target in delivery_targets {
            if let crate::LandingVerdict::Landed {
                evidence,
                landed_by,
            } = crate::representation::work_landed_with_cache(
                &self.repo,
                head,
                target,
                self.landing_deadline.get(),
                crate::representation::LandingSearchProfile::default(),
                landing_cache,
            )? {
                return Ok(Some((target.clone(), evidence, landed_by)));
            }
        }
        Ok(None)
    }

    /// A recorded representation, when it proves this exact head landed.
    ///
    /// The record is evidence because of how narrowly it is bound: to one
    /// reviewed `(session, head)` pair, naming the one commit that carried the
    /// work. This checks that binding rather than the far weaker claim that
    /// the content turns up somewhere. A record written for a different head
    /// does not apply -- a session that has committed since is judged on its
    /// current head and finds nothing here -- and a representing commit that
    /// has not reached a delivery target proves nothing either.
    pub(super) fn recorded_representation_evidence(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
    ) -> Result<Option<RecordedRepresentationEvidence>, BrokerOpError> {
        let Some(record) = self
            .store
            .session_representation(session.id, session_head)?
        else {
            return Ok(None);
        };
        let Some(representing) = record.representing_commit else {
            // Recorded with no carrying commit: the branch already held this
            // head's net content, so there is nothing left to locate.
            return Ok(Some(RecordedRepresentationEvidence::AlreadyHeld));
        };
        let Some(target) = delivery_targets
            .iter()
            .find(|target| self.repo.is_ancestor(&representing, target))
            .cloned()
        else {
            return Ok(None);
        };
        Ok(Some(RecordedRepresentationEvidence::Landed {
            representing,
            target,
        }))
    }

    pub(super) fn cleanup_provenance_from_ancestry(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: &[String],
    ) -> Result<(CleanupProvenance, String), BrokerOpError> {
        let released_queue_entry_id = if session.accepted_queue_entry_id.is_none() {
            self.store.released_checkpoint_queue_entry(session.id)?
        } else {
            None
        };
        let accepted_queue_entry_id = session.accepted_queue_entry_id.or(released_queue_entry_id);
        let mut provenance = CleanupProvenance {
            representation: CleanupRepresentation::Unproven,
            session_head: session_head.into(),
            adopted_head: session.adopted_head.clone(),
            accepted_session_head: session.accepted_session_head.clone(),
            accepted_integration_commit: session.accepted_integration_commit.clone(),
            accepted_integration_tree: session.accepted_integration_tree.clone(),
            accepted_queue_entry_id,
            accepted_queue_status: None,
            represented_on: None,
            represented_by_commit: None,
            pending_commit_count: 0,
        };

        let Some(accepted_head) = session.accepted_session_head.as_deref() else {
            let adopted_head = session
                .adopted_head
                .as_deref()
                .or(session.adoption_base.as_deref())
                .or(session.diff_base.as_deref());
            if adopted_head == Some(session_head) {
                let represented_on = delivery_targets
                    .iter()
                    .find(|target| self.repo.is_ancestor(session_head, target))
                    .cloned();
                if let Some(represented_on) = represented_on {
                    provenance.representation = CleanupRepresentation::Represented;
                    provenance.represented_on = Some(represented_on.clone());
                    return Ok((
                        provenance,
                        format!(
                            "unchanged adoption boundary is represented on delivery target {}",
                            short_commit(&represented_on)
                        ),
                    ));
                }
                return Ok((
                    provenance,
                    "unchanged adoption boundary is not represented on any delivery target".into(),
                ));
            }
            if let Some(adopted_head) = adopted_head
                && self.repo.is_ancestor(adopted_head, session_head)
            {
                let pending = self.repo.commit_count_between(adopted_head, session_head)?;
                provenance.representation = CleanupRepresentation::Pending;
                provenance.pending_commit_count = pending;
                return Ok((
                    provenance,
                    format!(
                        "{pending} session commit(s) after the adoption boundary have never been accepted"
                    ),
                ));
            }
            return Ok((
                provenance,
                "session HEAD cannot be related safely to its recorded adoption boundary".into(),
            ));
        };

        if accepted_head != session_head {
            if self.repo.is_ancestor(accepted_head, session_head) {
                let pending = self
                    .repo
                    .commit_count_between(accepted_head, session_head)?;
                provenance.representation = CleanupRepresentation::Pending;
                provenance.pending_commit_count = pending;
                return Ok((
                    provenance,
                    format!(
                        "{pending} session commit(s) remain after the last accepted checkpoint"
                    ),
                ));
            }
            return Ok((
                provenance,
                "session HEAD rewrites or diverges from the last accepted checkpoint".into(),
            ));
        }

        let (Some(queue_entry_id), Some(integration_commit), Some(integration_tree)) = (
            accepted_queue_entry_id,
            session.accepted_integration_commit.as_deref(),
            session.accepted_integration_tree.as_deref(),
        ) else {
            return Ok((
                provenance,
                "accepted checkpoint is missing queue, integration commit, or integration tree provenance"
                    .into(),
            ));
        };
        let entry = self.store.merge_queue_entry_if_exists(queue_entry_id)?;
        let Some(entry) = entry else {
            if released_queue_entry_id == Some(queue_entry_id)
                && self.repo.commit_tree_id(integration_commit).ok().as_deref()
                    == Some(integration_tree)
            {
                let represented_on = delivery_targets
                    .iter()
                    .find(|target| self.repo.is_ancestor(integration_commit, target))
                    .cloned();
                if let Some(represented_on) = represented_on {
                    provenance.representation = CleanupRepresentation::Represented;
                    provenance.represented_on = Some(represented_on.clone());
                    return Ok((
                        provenance,
                        format!(
                            "accepted queue entry {queue_entry_id} was GC-released after its checkpoint pin; integration commit is represented on {represented_on}"
                        ),
                    ));
                }
            }
            return Ok((
                provenance,
                format!("accepted queue entry {queue_entry_id} is missing"),
            ));
        };
        provenance.accepted_queue_status = Some(entry.status);
        if entry.session_id != session.id
            || entry.head_commit != accepted_head
            || !matches!(
                entry.status,
                MergeStatus::Promoted | MergeStatus::ExternallyLanded | MergeStatus::Superseded
            )
        {
            return Ok((
                provenance,
                format!(
                    "accepted queue entry {queue_entry_id} does not prove this session checkpoint"
                ),
            ));
        }
        if self.repo.commit_tree_id(integration_commit).ok().as_deref() != Some(integration_tree) {
            return Ok((
                provenance,
                "accepted integration commit and tree provenance do not match".into(),
            ));
        }
        let represented_on = delivery_targets
            .iter()
            .find(|target| self.repo.is_ancestor(integration_commit, target))
            .cloned();
        let Some(represented_on) = represented_on else {
            return Ok((
                provenance,
                "accepted integration commit is not represented on local main, integration, or configured upstream"
                    .into(),
            ));
        };
        provenance.representation = CleanupRepresentation::Represented;
        provenance.represented_on = Some(represented_on.clone());
        Ok((
            provenance,
            format!(
                "accepted checkpoint is represented on delivery target {}",
                short_commit(&represented_on)
            ),
        ))
    }

    /// Eligibility judged from the session branch rather than the worktree.
    fn branch_provenance_disposition(
        &self,
        session: &Session,
        session_head: &str,
        delivery_targets: CleanupDeliveryTargets<'_>,
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        let owned_delivery_targets;
        let delivery_targets = match delivery_targets {
            CleanupDeliveryTargets::Snapshot(targets) => targets,
            CleanupDeliveryTargets::Fresh => {
                owned_delivery_targets = self.cleanup_delivery_targets()?;
                &owned_delivery_targets
            }
            CleanupDeliveryTargets::SnapshotUnavailable => {
                return Ok((
                    CleanupDisposition::InspectionFailed,
                    CLEANUP_TARGET_SNAPSHOT_UNAVAILABLE.into(),
                    None,
                ));
            }
        };
        Ok(
            match self.cleanup_provenance_with_cache(
                session,
                session_head,
                delivery_targets,
                landing_cache,
            ) {
                Ok((provenance, reason)) => {
                    let disposition = match provenance.representation {
                        CleanupRepresentation::Represented => CleanupDisposition::Eligible,
                        CleanupRepresentation::Pending => CleanupDisposition::PendingCommits,
                        CleanupRepresentation::Unproven => CleanupDisposition::UnprovenProvenance,
                    };
                    (disposition, reason, Some(provenance))
                }
                Err(error) => (
                    CleanupDisposition::InspectionFailed,
                    self.cleanup_inspection_failure(&error),
                    None,
                ),
            },
        )
    }

    pub(super) fn cleanup_item(
        &self,
        session: &Session,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let mut records = crate::measurement::SizeRecords::default();
        self.cleanup_item_scanned(session, crate::SizeScan::Measure, &mut records)
    }

    /// One retained worktree, sized according to `scan`.
    ///
    /// `estimated_bytes: None` already meant "the broker does not know", so a
    /// recorded-size pass reuses it rather than inventing a second way to say
    /// so. Everything else here -- git dirtiness, ancestry, provenance -- is
    /// milliseconds and runs either way; the walk is the whole cost (#176).
    pub(crate) fn cleanup_item_scanned(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        self.cleanup_item_scanned_with_status_limits(
            session,
            scan,
            records,
            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
        )
    }

    pub(crate) fn cleanup_item_scanned_with_status_limits(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_output_bytes: usize,
        status_path_fields: usize,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        self.cleanup_item_scanned_with_limits(
            session,
            scan,
            records,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                status_output_bytes,
                status_path_fields,
            ),
        )
    }

    fn cleanup_item_scanned_with_limits(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        status_limits: CleanupStatusLimits,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_item_scanned_with_tips(
            session,
            scan,
            records,
            CleanupInspectionContext {
                branch_tips: None,
                delivery_targets: CleanupDeliveryTargets::Fresh,
                landing_cache: &mut landing_cache,
                status_limits,
            },
        )
    }

    /// [`Self::cleanup_item_scanned`] with every local branch tip already read
    /// (see [`GitRepo::local_branch_tips`]). A plan visits every session ever
    /// recorded, most of whose branch and worktree are long gone; looking the
    /// branch up in one listing lets those return without forking `git`.
    fn cleanup_item_scanned_with_tips(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        inspection: CleanupInspectionContext<'_>,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
        let CleanupInspectionContext {
            branch_tips,
            delivery_targets,
            landing_cache,
            status_limits,
        } = inspection;
        let worktree_path = PathBuf::from(&session.worktree_path);
        let worktree_present = worktree_path.exists();
        let branch_ref = format!("refs/heads/{}", session.branch);
        let branch_tip = match branch_tips {
            Some(tips) => tips.get(&branch_ref).cloned(),
            None => self.repo.resolve_ref(&branch_ref),
        };
        if !worktree_present && branch_tip.is_none() {
            return Ok(None);
        }
        let unreachable = (!worktree_present)
            .then(|| {
                crate::worktree_location::unavailable_configured_location(
                    &self.main_root,
                    &worktree_path,
                )
            })
            .flatten();
        let broker_owned =
            unreachable.is_none() && self.is_broker_owned_worktree(session, &worktree_path);
        let (estimated_bytes, estimated_inodes) = worktree_size_estimate(
            &worktree_path,
            &session.worktree_path,
            worktree_present,
            broker_owned,
            scan,
            records,
            |path| crate::disk_headroom::directory_usage_without_following_links(path).ok(),
        );
        let (mut disposition, mut reason, provenance) = if let Some(reason) = unreachable {
            // An unplugged drive is not a deleted worktree. Proposing cleanup
            // here would act on a checkout nobody can see, so it stays blocked
            // until the volume is back.
            (CleanupDisposition::InspectionFailed, reason, None)
        } else if !broker_owned {
            (
                CleanupDisposition::UnsafePath,
                "spawned session path is outside the broker-owned worktree directory".into(),
                None,
            )
        } else if worktree_present {
            if let Some(proof) = self.interrupted_finish_orphan_cleanup_proof(session) {
                self.branch_provenance_disposition(
                    session,
                    &proof.head,
                    delivery_targets,
                    landing_cache,
                )?
            } else {
                match self.is_orphaned_worktree_of_this_repository_checked(&worktree_path) {
                    Err(error) => (
                        CleanupDisposition::InspectionFailed,
                        self.cleanup_inspection_failure(&error),
                        None,
                    ),
                    Ok(true) => (
                        CleanupDisposition::UnprovenProvenance,
                        "the orphaned checkout has no matching unfinished finish-cleanup proof; preserved it for inspection or an explicit force cleanup".into(),
                        None,
                    ),
                    Ok(false) if is_orphaned_worktree_directory(&worktree_path) => (
                        CleanupDisposition::InspectionFailed,
                        "the checkout is not a usable worktree and its repository ownership could not be proven; preserved it for inspection".into(),
                        None,
                    ),
                    Ok(false) => {
                        let eligibility = match delivery_targets {
                            CleanupDeliveryTargets::Snapshot(targets) => self
                                .cleanup_eligibility_with_status_limits(
                                    session.id,
                                    &worktree_path,
                                    targets,
                                    landing_cache,
                                    status_limits,
                                ),
                            CleanupDeliveryTargets::Fresh => {
                                let targets = self.cleanup_delivery_targets()?;
                                self.cleanup_eligibility_with_status_limits(
                                    session.id,
                                    &worktree_path,
                                    &targets,
                                    landing_cache,
                                    status_limits,
                                )
                            }
                            CleanupDeliveryTargets::SnapshotUnavailable => Ok((
                                CleanupDisposition::InspectionFailed,
                                CLEANUP_TARGET_SNAPSHOT_UNAVAILABLE.into(),
                                None,
                            )),
                        };
                        match eligibility {
                            Ok(result) => result,
                            Err(error) => (
                                CleanupDisposition::InspectionFailed,
                                self.cleanup_inspection_failure(&error),
                                None,
                            ),
                        }
                    }
                }
            }
        } else if let Some(session_head) = branch_tip.as_deref() {
            // Once the worktree is gone, only the branch remains to judge its
            // commit provenance. Existing orphan directories take the
            // proof-bound branch above because their remaining bytes are not
            // described by the commit alone.
            self.branch_provenance_disposition(
                session,
                session_head,
                delivery_targets,
                landing_cache,
            )?
        } else {
            unreachable!("entries without a worktree or a branch return None above")
        };
        if worktree_present
            && let Some(worktree_head) = provenance
                .as_ref()
                .map(|provenance| provenance.session_head.as_str())
            && branch_tip.as_deref() != Some(worktree_head)
        {
            // A detached worktree sitting somewhere other than its branch is
            // two commits to account for, not one, and the provenance above
            // only spoke for the branch. Removing the directory would drop
            // whatever the worktree is detached on.
            //
            // Unless both are durable. Then nothing is dropped, and refusing
            // only keeps a directory whose entire content exists on a remote.
            // Both must prove it: proving one and guessing the other is how a
            // detached head gets discarded quietly.
            let worktree_durable = self.remote_durability_evidence(worktree_head);
            let branch_durable = match branch_tip.as_deref() {
                // No branch left to lose, so the worktree head answers alone.
                None => Some(("(no branch ref)".to_string(), String::new())),
                Some(tip) => self.remote_durability_evidence(tip),
            };
            match (worktree_durable, branch_durable) {
                (Some((worktree_ref, _)), Some(_)) => {
                    disposition = CleanupDisposition::Eligible;
                    reason = format!(
                        "worktree is detached on {}, which diverges from the session branch; both are reachable from remote refs and verified current, so neither is lost ({worktree_ref})",
                        short_commit(worktree_head)
                    );
                }
                _ => {
                    disposition = CleanupDisposition::UnprovenProvenance;
                    reason = "session branch ref is missing or does not match the retained                               worktree HEAD"
                        .into();
                }
            }
        }
        let session_head = provenance
            .as_ref()
            .map(|provenance| provenance.session_head.as_str())
            .or(branch_tip.as_deref());
        let mut inspection_commands = Vec::new();
        if let Some(session_head) = session_head {
            inspection_commands.push(format!("git show --stat --oneline {session_head}"));
        }
        if let Some(provenance) = provenance.as_ref()
            && let Some(accepted_head) = provenance.accepted_session_head.as_deref()
            && accepted_head != provenance.session_head
        {
            inspection_commands.push(format!(
                "git log --oneline {accepted_head}..{}",
                provenance.session_head
            ));
        }
        Ok(Some(CleanupWorktreePlan {
            session_id: session.id,
            worktree_path: session.worktree_path.clone(),
            worktree_present,
            branch_ref,
            branch_tip: branch_tip.clone(),
            delete_branch: branch_tip.is_some(),
            origin: session.origin,
            disposition,
            provenance,
            estimated_bytes,
            estimated_inodes,
            reason,
            inspection_commands,
            force_cleanup_command: format!("aethyme broker finish cleanup {} --force", session.id),
        }))
    }

    /// Read-only inventory of retained broker-owned worktrees belonging to
    /// sessions that are already closed in broker state.
    ///
    /// Walks and sizes every retained worktree. This is the expensive path and
    /// the only one whose digest may authorize a removal.
    pub fn cleanup_plan(&self) -> Result<CleanupPlan, BrokerOpError> {
        self.cleanup_plan_scanned(crate::SizeScan::Measure)
    }

    /// The same inventory, assembled from sizes an earlier walk recorded.
    ///
    /// This is what `broker status` and `doctor` use. It touches no worktree
    /// contents, so its byte totals omit anything never measured -- read
    /// `unmeasured_worktree_count` before reporting them as totals -- and it
    /// carries no digest, because a plan that does not know how big things are
    /// must not be able to authorize removing them.
    ///
    /// Before returning it spends `routine_size_budget_ms` measuring at most
    /// one directory, so the records fill in over successive routine checks
    /// instead of waiting for somebody to run `gc plan` (#176).
    pub fn cleanup_plan_recorded(&self) -> Result<CleanupPlan, BrokerOpError> {
        self.cleanup_plan_recorded_within(HEALTH_CHECK_ELIGIBILITY_BUDGET)
    }

    /// [`Self::cleanup_plan_recorded`] with an explicit eligibility budget.
    /// Once `budget` is spent the remaining worktrees are listed with
    /// eligibility not inspected; the plan carries no digest either way, so a
    /// shorter budget can never authorize a removal.
    pub fn cleanup_plan_recorded_within(
        &self,
        budget: std::time::Duration,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let plan = self.cleanup_plan_scanned_within(crate::SizeScan::Recorded, Some(budget))?;
        self.warm_one_size_record(&plan)?;
        Ok(plan)
    }

    fn cleanup_plan_recorded_until(
        &self,
        deadline: std::time::Instant,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let budget = deadline.saturating_duration_since(std::time::Instant::now());
        let plan = self.cleanup_plan_scanned_within(crate::SizeScan::Recorded, Some(budget))?;
        if std::time::Instant::now() < deadline {
            self.warm_one_size_record(&plan)?;
        }
        Ok(plan)
    }

    /// A recorded-size pass is bounded by [`HEALTH_CHECK_ELIGIBILITY_BUDGET`];
    /// a measuring pass, the only one whose digest authorizes removal, always
    /// inspects every worktree.
    pub(crate) fn cleanup_plan_scanned(
        &self,
        scan: crate::SizeScan,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let budget = (!scan.measures()).then_some(HEALTH_CHECK_ELIGIBILITY_BUDGET);
        self.cleanup_plan_scanned_within(scan, budget)
    }

    pub(crate) fn cleanup_plan_scanned_within(
        &self,
        scan: crate::SizeScan,
        budget: Option<std::time::Duration>,
    ) -> Result<CleanupPlan, BrokerOpError> {
        let mut landing_cache = crate::representation::LandingVerdictCache::default();
        self.cleanup_plan_scanned_within_and_cache(scan, budget, &mut landing_cache)
    }

    pub(crate) fn cleanup_plan_scanned_within_and_cache(
        &self,
        scan: crate::SizeScan,
        budget: Option<std::time::Duration>,
        landing_cache: &mut crate::representation::LandingVerdictCache,
    ) -> Result<CleanupPlan, BrokerOpError> {
        debug_assert!(
            budget.is_none() || !scan.measures(),
            "a measuring plan authorizes removal and must inspect every worktree"
        );
        let started = std::time::Instant::now();
        let deadline = budget.map(|budget| started + budget);
        // Cleared on every exit below, including an error return.
        struct ClearOnDrop<'a>(&'a std::cell::Cell<Option<std::time::Instant>>);
        impl Drop for ClearOnDrop<'_> {
            fn drop(&mut self) {
                self.0.set(None);
            }
        }
        self.landing_deadline.set(deadline);
        let _clear_landing_deadline = ClearOnDrop(&self.landing_deadline);
        let target_snapshot = self.cleanup_target_snapshot();
        let mut plan = CleanupPlan {
            target_snapshot: target_snapshot.refs.clone(),
            ..CleanupPlan::default()
        };
        let mut records = crate::measurement::load_size_records(&self.main_root);
        let mut retained = crate::MeasuredTotal::default();
        let mut reclaimable = crate::MeasuredTotal::default();
        let mut known = std::collections::BTreeSet::new();
        // One listing instead of one `rev-parse` per session ever recorded:
        // this plan is on `broker status`'s path through `cleanup_retention`.
        let branch_tips = self.repo.local_branch_tips();
        for session in self.store.cleaned_sessions()? {
            if session.origin != SessionOrigin::Spawned {
                continue;
            }
            let mut over_budget = budget.is_some_and(|budget| started.elapsed() >= budget);
            let item = if over_budget {
                self.cleanup_item_not_inspected(
                    &session,
                    &records,
                    branch_tips.as_ref(),
                    format!(
                        "eligibility not inspected within the {} s health-check budget; run `aethyme broker gc plan` for the full inspection",
                        budget.unwrap_or_default().as_secs()
                    ),
                )
            } else {
                let item = self.cleanup_item_scanned_with_tips(
                    &session,
                    scan,
                    &mut records,
                    CleanupInspectionContext {
                        branch_tips: branch_tips.as_ref(),
                        delivery_targets: target_snapshot
                            .delivery_targets
                            .as_deref()
                            .map(CleanupDeliveryTargets::Snapshot)
                            .unwrap_or(CleanupDeliveryTargets::SnapshotUnavailable),
                        landing_cache,
                        status_limits: CleanupStatusLimits::new(
                            deadline,
                            crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                            crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
                        ),
                    },
                )?;
                // A landing search the deadline stopped reads as a failed
                // inspection; it is the budget, so say so instead (#460).
                if item
                    .as_ref()
                    .is_some_and(|item| item.disposition == CleanupDisposition::InspectionFailed)
                    && budget.is_some_and(|budget| started.elapsed() >= budget)
                {
                    over_budget = true;
                    self.cleanup_item_not_inspected(
                        &session,
                        &records,
                        branch_tips.as_ref(),
                        format!(
                            "eligibility not inspected within the {} s health-check budget; run `aethyme broker gc plan` for the full inspection",
                            budget.unwrap_or_default().as_secs()
                        ),
                    )
                } else {
                    item
                }
            };
            let Some(item) = item else {
                continue;
            };
            if over_budget {
                plan.eligibility_not_inspected_count += 1;
            }
            known.insert(item.worktree_path.clone());
            if item.worktree_present {
                plan.retained_worktree_count += 1;
            }
            if item.branch_tip.is_some() {
                plan.retained_branch_count += 1;
            }
            let measured_at = records
                .get(&item.worktree_path)
                .map(|record| record.measured_at_ms);
            let eligible = item.eligible();
            match (item.estimated_bytes, measured_at) {
                // A worktree that is gone contributes a known zero and no
                // record; there was nothing to walk.
                (Some(bytes), None) => {
                    retained.add_measured(bytes, i64::MAX);
                    if eligible {
                        reclaimable.add_measured(bytes, i64::MAX);
                    }
                }
                (Some(bytes), Some(at)) => {
                    retained.add_measured(bytes, at);
                    if eligible {
                        reclaimable.add_measured(bytes, at);
                    }
                }
                (None, _) => {
                    retained.add_unmeasured();
                    if eligible {
                        reclaimable.add_unmeasured();
                    }
                }
            }
            if eligible {
                if item.worktree_present {
                    plan.eligible_worktree_count += 1;
                }
                if item.branch_tip.is_some() {
                    plan.eligible_branch_count += 1;
                }
            }
            plan.worktrees.push(item);
        }
        plan.estimated_retained_bytes = retained.bytes;
        plan.estimated_reclaimable_bytes = reclaimable.bytes;
        plan.unmeasured_worktree_count = retained.unmeasured;
        if scan.measures() {
            // Only a pass that enumerated everything may prune: a routine
            // check sees whatever subset it asked about.
            records.retain_paths(&known);
            crate::warn_unrecorded(
                "save worktree size records",
                crate::measurement::save_size_records(&self.main_root, &records),
            );
            let bytes = serde_json::to_vec(&plan)?;
            plan.digest = format!("{:x}", Sha256::digest(bytes));
        }
        // Set after the digest, deliberately. The digest is what an operator
        // confirms and what `cleanup_cleaned_worktrees` re-plans to match, so
        // it must cover what would be removed and nothing else. *When* the
        // sizes were taken is a reporting field; hashing it would make every
        // plan's digest unique to the instant it was built and no confirmation
        // could ever match (#176).
        plan.sizes_measured_at_ms = retained
            .oldest_measured_at_ms
            .filter(|oldest| *oldest != i64::MAX);
        Ok(plan)
    }

    /// Inventory only: one branch listing and filesystem existence checks.
    /// No content comparison, directory walk, or deletion authorization.
    pub(super) fn cleanup_plan_observed(&self) -> Result<(CleanupPlan, usize), BrokerOpError> {
        self.cleanup_plan_observed_with_budget(std::time::Duration::from_millis(250))
    }

    pub(super) fn cleanup_plan_observed_with_budget(
        &self,
        budget: std::time::Duration,
    ) -> Result<(CleanupPlan, usize), BrokerOpError> {
        let mut plan = CleanupPlan::default();
        let records = crate::measurement::load_size_records(&self.main_root);
        let Some(tips) = self.repo.local_branch_tips() else {
            return Ok((plan, self.store.cleaned_sessions()?.len()));
        };
        let mut retained = crate::MeasuredTotal::default();
        let sessions = self.store.cleaned_sessions()?;
        let started = std::time::Instant::now();
        let mut deferred = 0;
        for (index, session) in sessions.iter().enumerate() {
            if started.elapsed() >= budget {
                deferred = sessions.len() - index;
                break;
            }
            if session.origin != SessionOrigin::Spawned {
                continue;
            }
            let Some(item) = self.cleanup_item_not_inspected(
                session,
                &records,
                Some(&tips),
                "eligibility not inspected by routine status".into(),
            ) else {
                continue;
            };
            let record = records.get(&session.worktree_path);
            if item.worktree_present {
                plan.retained_worktree_count += 1;
            }
            if item.branch_tip.is_some() {
                plan.retained_branch_count += 1;
            }
            match item.estimated_bytes {
                Some(bytes) => retained
                    .add_measured(bytes, record.map(|r| r.measured_at_ms).unwrap_or(i64::MAX)),
                None => retained.add_unmeasured(),
            }
            plan.worktrees.push(item);
        }
        plan.estimated_retained_bytes = retained.bytes;
        plan.unmeasured_worktree_count = retained.unmeasured;
        plan.sizes_measured_at_ms = retained.oldest_measured_at_ms.filter(|at| *at != i64::MAX);
        Ok((plan, deferred))
    }

    /// A plan entry for a closed session's worktree that lists what is on
    /// disk without judging eligibility: existence, the branch tip and the
    /// recorded size only. It is never eligible, so it cannot authorize a
    /// removal. `None` when there is nothing left to list.
    pub(super) fn cleanup_item_not_inspected(
        &self,
        session: &Session,
        records: &crate::measurement::SizeRecords,
        tips: Option<&std::collections::BTreeMap<String, String>>,
        reason: String,
    ) -> Option<CleanupWorktreePlan> {
        let path = Path::new(&session.worktree_path);
        if !self.is_broker_owned_worktree(session, path) {
            return None;
        }
        let present = path.exists();
        let branch_ref = format!("refs/heads/{}", session.branch);
        let tip = tips.and_then(|tips| tips.get(&branch_ref).cloned());
        if !present && tip.is_none() {
            return None;
        }
        let (estimated_bytes, estimated_inodes) = if present {
            records
                .get(&session.worktree_path)
                .map(|record| (Some(record.bytes), record.inodes))
                .unwrap_or((None, None))
        } else {
            (Some(0), Some(0))
        };
        Some(CleanupWorktreePlan {
            session_id: session.id,
            worktree_path: session.worktree_path.clone(),
            worktree_present: present,
            branch_ref,
            branch_tip: tip,
            delete_branch: false,
            origin: session.origin,
            disposition: CleanupDisposition::InspectionFailed,
            provenance: None,
            estimated_bytes,
            estimated_inodes,
            reason,
            inspection_commands: vec!["aethyme broker gc plan".into()],
            force_cleanup_command: String::new(),
        })
    }

    /// Measure one directory the broker has never sized, or whose recorded
    /// size has aged out, and write the result down.
    ///
    /// Bounded to one directory and to `routine_size_budget_ms`, because the
    /// caller is a routine check. A measurement that does not finish inside
    /// the budget records nothing: a partial sum written down as a size would
    /// be indistinguishable from a real one afterwards.
    pub(super) fn warm_one_size_record(&self, plan: &CleanupPlan) -> Result<(), BrokerOpError> {
        // Status must remain able to report a malformed policy. It uses the
        // conservative defaults for its picture and simply skips warming when
        // the configured warming budget cannot be trusted.
        let Ok(policy) = crate::load_retention_policy(&self.main_root) else {
            return Ok(());
        };
        let mut records = crate::measurement::load_size_records(&self.main_root);
        let paths = size_warm_candidates(plan);
        let ttl_ms = i64::from(policy.size_record_ttl_hours).saturating_mul(3_600_000);
        let Some(path) = records.next_to_measure(&paths, now_ms(), ttl_ms) else {
            return Ok(());
        };
        // No explicit test for `routine_size_budget_ms == 0`: a walk checks the
        // deadline before its first `read_dir`, so a zero budget cannot
        // complete a measurement and nothing is ever recorded. An early return
        // here would say the same thing twice, and only one of the two could
        // be falsified by a test.
        let deadline = std::time::Instant::now()
            + std::time::Duration::from_millis(policy.routine_size_budget_ms);
        let Some(usage) = crate::disk_headroom::directory_usage_bounded(Path::new(&path), deadline)
        else {
            return Ok(());
        };
        records.record_usage(&path, usage.bytes, Some(usage.inodes), now_ms());
        crate::warn_unrecorded(
            "save worktree size records",
            crate::measurement::save_size_records(&self.main_root, &records),
        );
        Ok(())
    }

    /// Remove every currently eligible worktree from a read-only cleanup
    /// plan. Apply revalidates each worktree independently and never forces a
    /// dirty, adopted, symlinked, or unrepresented checkout.
    pub fn cleanup_cleaned_worktrees(
        &mut self,
        apply: bool,
        confirm: Option<&str>,
    ) -> Result<CleanupSweepReport, BrokerOpError> {
        let plan = self.cleanup_plan()?;
        let mut removed_session_ids = Vec::new();
        let mut failures = Vec::new();
        if apply {
            let confirm = confirm.ok_or(BrokerOpError::CleanupConfirmationNotSha256)?;
            if confirm.len() != 64 || !confirm.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(BrokerOpError::CleanupConfirmationNotSha256);
            }
            if confirm != plan.digest {
                return Err(BrokerOpError::CleanupConfirmationMismatch {
                    actual: confirm.into(),
                });
            }
            for item in plan.worktrees.iter().filter(|item| item.eligible()) {
                match self.cleanup(item.session_id, false) {
                    Ok(()) => removed_session_ids.push(item.session_id),
                    Err(error) => failures.push(CleanupSweepFailure {
                        session_id: item.session_id,
                        reason: error.to_string(),
                    }),
                }
            }
        }
        Ok(CleanupSweepReport {
            applied: apply,
            plan,
            removed_session_ids,
            failures,
        })
    }

    /// The retention picture `broker status` shows.
    ///
    /// Deliberately the recorded-size path. Status is the mandated first step
    /// of every session, so it runs constantly; sizing every retained worktree
    /// here is what made the routine check and the five-minute audit the same
    /// code (#176).
    pub(super) fn cleanup_retention(&self, now_ms: i64) -> Result<CleanupRetention, BrokerOpError> {
        self.cleanup_retention_with_audit(now_ms, true)
    }

    pub(super) fn cleanup_retention_with_audit(
        &self,
        now_ms: i64,
        audit: bool,
    ) -> Result<CleanupRetention, BrokerOpError> {
        let (policy, retention_config) = match crate::load_retention_policy_report(&self.main_root)
        {
            Ok(report) => (
                report.policy,
                RetentionConfigStatus {
                    warnings: report.warnings,
                    error: None,
                },
            ),
            Err(error) => (
                crate::RetentionPolicy::default(),
                RetentionConfigStatus {
                    warnings: Vec::new(),
                    error: Some(error.to_string()),
                },
            ),
        };
        let deadline = crate::git::active_git_deadline();
        let (plan, deferred) = if audit {
            let plan = match deadline {
                Some(deadline) => self.cleanup_plan_recorded_until(deadline)?,
                None => self.cleanup_plan_recorded()?,
            };
            let deferred = plan.eligibility_not_inspected_count;
            (plan, deferred)
        } else {
            self.cleanup_plan_observed()?
        };
        let closed_sessions = self.store.cleaned_sessions()?;
        let (worktree_inodes, worktree_inode_unmeasured, inode_summary_complete) =
            self.recorded_worktree_inode_summary(&closed_sessions, deadline)?;
        let closed_worktrees = crate::retention::ClosedWorktreeSummary::from_cleanup(
            &plan,
            &closed_sessions,
            &self.main_root,
        );
        let mut oldest_closed_at: Option<i64> = None;
        let mut oldest_closed_complete = true;
        for session in &closed_sessions {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                oldest_closed_complete = false;
                break;
            }
            let path = Path::new(&session.worktree_path);
            if let Some(closed_at) = session.closed_at
                && path.exists()
                && self.is_broker_owned_worktree(session, path)
            {
                oldest_closed_at =
                    Some(oldest_closed_at.map_or(closed_at, |oldest| oldest.min(closed_at)));
            }
        }
        let oldest_closed_age_days = oldest_closed_at
            .map(|closed_at| now_ms.saturating_sub(closed_at).max(0) as u64 / 86_400_000)
            .unwrap_or_else(|| {
                if oldest_closed_complete {
                    0
                } else {
                    u64::from(policy.closed_worktrees_days)
                }
            });
        // One reading, consumed by both the severity below and the evidence
        // line in the advice that reports it.
        let (gate_headroom, gate_headroom_deferred) = self.gate_headroom();
        let (host_volume_probe, host_available_bytes) = gate_headroom
            .bytes
            .map(|probe| (Some(probe.path), Some(probe.available)))
            .unwrap_or((None, None));
        let (host_inode_volume_probe, inodes_free) = gate_headroom
            .inodes
            .map(|probe| (Some(probe.path), Some(probe.available)))
            .unwrap_or((None, None));
        let reconciliation = self.reconcile_worktree_directories_until(false, deadline)?;
        let inventory_complete = deferred == 0
            && inode_summary_complete
            && oldest_closed_complete
            && reconciliation.complete;
        let severity = if inventory_complete {
            cleanup_retention_severity(
                plan.retained_worktree_count,
                plan.estimated_retained_bytes,
                oldest_closed_age_days,
                policy.closed_worktrees_days,
                policy.retained_bytes_budget,
            )
        } else {
            StatusAdviceSeverity::Warning
        };
        let estimated_blocked_bytes = plan
            .estimated_retained_bytes
            .saturating_sub(plan.estimated_reclaimable_bytes);
        let retained_total = crate::MeasuredTotal {
            bytes: plan.estimated_retained_bytes,
            measured: plan
                .worktrees
                .len()
                .saturating_sub(plan.unmeasured_worktree_count),
            unmeasured: plan.unmeasured_worktree_count,
            oldest_measured_at_ms: plan.sizes_measured_at_ms,
        };
        let mut retained_total = retained_total;
        retained_total.unmeasured = retained_total
            .unmeasured
            .saturating_add(deferred)
            .saturating_add(usize::from(
                !inode_summary_complete || !oldest_closed_complete || !reconciliation.complete,
            ));
        let budget_verdict = crate::budget_verdict(&retained_total, policy.retained_bytes_budget);
        // Only `Over` asserts that the budget is broken. A floor under the
        // budget is not a pass -- the bytes it skipped are exactly the ones
        // that would have decided it.
        let over_retained_bytes_budget = budget_verdict.exceeded();
        Ok(CleanupRetention {
            inventory_complete,
            inventory_deferred_sessions: deferred,
            gate_headroom_deferred,
            eligibility_checked: audit,
            broker_owned_worktree_count: plan.retained_worktree_count,
            retained_session_branch_count: plan.retained_branch_count,
            eligible_worktree_count: plan.eligible_worktree_count,
            estimated_retained_bytes: plan.estimated_retained_bytes,
            estimated_reclaimable_bytes: plan.estimated_reclaimable_bytes,
            estimated_blocked_bytes,
            retained_bytes_budget: policy.retained_bytes_budget,
            over_retained_bytes_budget,
            retained_bytes_deficit: crate::reclaim_order::deficit_bytes(
                plan.estimated_retained_bytes,
                policy.retained_bytes_budget,
            ),
            clears_retained_bytes_budget: inventory_complete
                && crate::reclaim_order::clears_budget(
                    plan.estimated_retained_bytes,
                    policy.retained_bytes_budget,
                    plan.estimated_reclaimable_bytes,
                ),
            budget_verdict,
            unmeasured_worktree_count: plan.unmeasured_worktree_count,
            sizes_measured_at_ms: plan.sizes_measured_at_ms,
            oldest_closed_age_days,
            closed_worktrees_policy_days: policy.closed_worktrees_days,
            host_available_bytes,
            host_volume_probe,
            host_inode_volume_probe,
            inodes_free,
            worktree_inodes,
            worktree_inode_unmeasured,
            severity,
            retention_config,
            reconciliation,
            closed_worktrees,
        })
    }

    /// Free space on volumes where gate work can write: repository state and
    /// logs, the broker database, the selected session-worktree root, the
    /// exact-tree verification slot, and managed build cache when configured.
    /// Each path is read at its nearest existing directory; this method does
    /// not create a missing root or cache.
    pub(super) fn gate_headroom_probes(&self) -> Vec<PathBuf> {
        let mut probes = std::collections::BTreeSet::from([
            self.main_root.join(".aethyme"),
            self.store
                .db_path()
                .parent()
                .unwrap_or(&self.main_root)
                .to_path_buf(),
        ]);
        let worktree_root_plan = self.worktree_root_plan().ok();
        if let Some(plan) = &worktree_root_plan
            && let Some((root, _)) = crate::broker::lifecycle::worktree_root_candidates_for(plan)
                .into_iter()
                .find(|(root, source)| {
                    *source != crate::WorktreeRootSource::RepositoryFallback
                        && crate::verification::removable_volume_unavailable_reason(root).is_none()
                })
        {
            probes.insert(root);
        }
        let verification = worktree_root_plan.as_ref().map_or_else(
            || crate::verification::plan_slot_placement(&self.main_root, "merge-sim"),
            |plan| {
                crate::verification::plan_slot_placement_with_worktree_root_plan(
                    &self.main_root,
                    "merge-sim",
                    plan,
                )
            },
        );
        if crate::verification::removable_volume_unavailable_reason(&verification.directory)
            .is_none()
        {
            probes.insert(verification.directory);
        }

        let (uses_managed_cache, uses_host_resource_db) = crate::gates::load_gates(&self.main_root)
            .map(|gates| {
                (
                    gates.iter().any(|gate| gate.managed_cache.is_some()),
                    gates
                        .iter()
                        .any(|gate| !gate.resources.is_empty() || gate.managed_cache.is_some()),
                )
            })
            .unwrap_or_default();
        let gate_storage = self.storage_context().gate_storage_locations();
        if uses_managed_cache && let Some(cache) = gate_storage.cache_dir() {
            probes.insert(cache);
        }
        if uses_host_resource_db && let Ok(database) = gate_storage.resource_db_path() {
            probes.insert(database.parent().unwrap_or(&self.main_root).to_path_buf());
        }
        probes.into_iter().collect()
    }

    fn gate_headroom(&self) -> (GateHeadroom, bool) {
        let deadline = crate::git::active_git_deadline();
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return (GateHeadroom::default(), true);
        }
        let probes = self.gate_headroom_probes();
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            return (GateHeadroom::default(), true);
        }
        let headroom = lowest_headroom_with(&probes, |probe| {
            crate::disk_headroom::available_headroom_at_or_above_for(&self.main_root, probe)
        });
        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            (GateHeadroom::default(), true)
        } else {
            (headroom, false)
        }
    }

    pub(super) fn recorded_worktree_inode_summary(
        &self,
        closed_sessions: &[Session],
        deadline: Option<std::time::Instant>,
    ) -> Result<(u64, usize, bool), BrokerOpError> {
        let mut paths = std::collections::BTreeSet::new();
        let live_sessions = self.store.live_sessions()?;
        let total_sessions = live_sessions.len().saturating_add(closed_sessions.len());
        let mut complete = true;
        let mut unmeasured = 0_usize;
        for (index, session) in live_sessions
            .iter()
            .chain(closed_sessions.iter())
            .enumerate()
        {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                unmeasured = unmeasured.saturating_add(total_sessions.saturating_sub(index));
                break;
            }
            if session.origin != SessionOrigin::Spawned {
                continue;
            }
            let path = Path::new(&session.worktree_path);
            if is_real_directory(path) && self.is_broker_owned_worktree(session, path) {
                paths.insert(session.worktree_path.clone());
            }
        }

        if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
            complete = false;
            unmeasured = unmeasured.saturating_add(paths.len());
            return Ok((0, unmeasured, complete));
        }
        let records = crate::measurement::load_size_records(&self.main_root);
        let mut known = 0_u64;
        let path_count = paths.len();
        for (index, path) in paths.into_iter().enumerate() {
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                complete = false;
                unmeasured = unmeasured.saturating_add(path_count.saturating_sub(index));
                break;
            }
            match records.get(&path).and_then(|record| record.inodes) {
                Some(inodes) => known = known.saturating_add(inodes),
                None => unmeasured = unmeasured.saturating_add(1),
            }
        }
        Ok((known, unmeasured, complete))
    }

    /// The exact durable proof captured before finish began removing this
    /// checkout. Older cleanup-started events remain readable, but their
    /// missing proof cannot authorize deleting an orphan whose contents Git
    /// can no longer inspect.
    fn finish_cleanup_recovery_intent(
        &self,
        session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        let current = self.store.session(session.id).ok()?;
        if current.origin != SessionOrigin::Spawned
            || current.cleanup_state != crate::SessionCleanupState::Closed
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return None;
        }
        let event = self
            .store
            .latest_session_finish_cleanup_started_event(session.id)
            .ok()??;
        let payload = event
            .payload_json
            .as_deref()
            .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok())?;
        if payload
            .get("worktree_present")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
            || payload
                .get("branch_present")
                .and_then(serde_json::Value::as_bool)
                != Some(true)
        {
            return None;
        }
        let proof = serde_json::from_value::<crate::events::FinishCleanupRecoveryProof>(
            payload.get("recovery_proof")?.clone(),
        )
        .ok()?;
        if proof.schema_version != 1
            || proof.branch_ref != format!("refs/heads/{}", current.branch)
            || proof.head != proof.branch_tip
        {
            return None;
        }
        Some(proof)
    }

    /// Remove a session's worktree and mark it cleaned. Refuses when the
    /// worktree has uncommitted changes or commits not represented on a
    /// delivery target, unless `force`.
    pub(super) fn interrupted_finish_cleanup_proof(
        &self,
        session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        self.interrupted_finish_cleanup_proof_with_status_limits(
            session,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            ),
        )
    }

    fn interrupted_finish_cleanup_proof_with_status_limits(
        &self,
        session: &Session,
        status_limits: CleanupStatusLimits,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        let current = self.store.session(session.id).ok()?;
        let proof = self.finish_cleanup_recovery_intent(&current)?;

        let worktree_path = Path::new(&current.worktree_path);
        let path_metadata = std::fs::symlink_metadata(worktree_path).ok()?;
        if path_metadata.file_type().is_symlink() || !path_metadata.is_dir() {
            return None;
        }
        let canonical_path = worktree_path.canonicalize().ok()?;
        if canonical_path.to_str()? != proof.worktree_path
            || !self
                .is_broker_owned_worktree_checked(&current, worktree_path)
                .ok()?
        {
            return None;
        }
        let checkout = GitRepo::discover(&canonical_path).ok()?;
        if checkout.root() != canonical_path
            || checkout.finish_cleanup_recovery_proof(&proof.branch_ref)? != proof
            || self.repo.resolve_ref(&proof.branch_ref).as_deref()
                != Some(proof.branch_tip.as_str())
        {
            return None;
        }
        let cleanup_policy = crate::AutoCleanupPolicy::load(self.main_root()).ok()?;
        if !checkout
            .has_only_unstaged_tracked_deletions_until_with_limits(
                &cleanup_policy,
                status_limits.deadline,
                status_limits.output_bytes,
                status_limits.path_fields,
            )
            .ok()?
            || !checkout
                .is_dirty_until_with_limits(
                    status_limits.deadline,
                    status_limits.output_bytes,
                    status_limits.path_fields,
                )
                .ok()?
        {
            return None;
        }

        let inventory = self.repo.worktree_inventory().ok()?;
        let matching_path = inventory
            .iter()
            .filter(|entry| entry.path.canonicalize().ok().as_ref() == Some(&canonical_path))
            .collect::<Vec<_>>();
        if matching_path.len() != 1 {
            return None;
        }
        let registration = matching_path[0];
        if registration.prunable
            || registration.bare
            || (registration.locked
                && registration.lock_reason.as_deref()
                    != Some(crate::git::BROKER_WORKTREE_LOCK_REASON))
            || registration.head.as_deref() != Some(proof.head.as_str())
            || registration.branch.as_deref() != Some(proof.branch_ref.as_str())
            || inventory
                .iter()
                .filter(|entry| entry.branch.as_deref() == Some(proof.branch_ref.as_str()))
                .count()
                != 1
        {
            return None;
        }

        let delivery_targets = self.cleanup_delivery_targets().ok()?;
        let (provenance, _) = self
            .cleanup_provenance(&current, &proof.head, &delivery_targets)
            .ok()?;
        if provenance.representation != CleanupRepresentation::Represented
            || self
                .refuse_live_checkout(current.id, worktree_path)
                .is_err()
        {
            return None;
        }
        Some(proof)
    }

    /// Validate a persisted finish removal after Git has already deregistered
    /// the checkout. The start event is written only after bounded cleanup
    /// eligibility has checked status under the lifecycle claim; its durable
    /// proof remains the authority when a partial removal has already deleted
    /// the worktree admin/index and fresh `git status` is no longer possible.
    /// The recorded root, pointer, admin directory, common directory, branch,
    /// and head identities bind that authority to this one checkout. A
    /// missing `.git` pointer is acceptable only under that exact proof,
    /// because a partial `remove_dir_all` may have unlinked it before dying.
    fn interrupted_finish_orphan_cleanup_proof(
        &self,
        _session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        #[cfg(unix)]
        {
            self.interrupted_finish_orphan_cleanup_proof_unix(_session)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    #[cfg(unix)]
    fn interrupted_finish_orphan_cleanup_proof_unix(
        &self,
        session: &Session,
    ) -> Option<crate::events::FinishCleanupRecoveryProof> {
        use std::os::unix::fs::MetadataExt;

        let current = self.store.session(session.id).ok()?;
        let proof = self.finish_cleanup_recovery_intent(&current)?;
        if current.origin != SessionOrigin::Spawned
            || current.cleanup_state != crate::SessionCleanupState::Closed
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return None;
        }

        let worktree_path = Path::new(&current.worktree_path);
        let path_metadata = std::fs::symlink_metadata(worktree_path).ok()?;
        if path_metadata.file_type().is_symlink()
            || !path_metadata.is_dir()
            || path_metadata.dev() != proof.worktree_device
            || path_metadata.ino() != proof.worktree_inode
        {
            return None;
        }
        let canonical_path = worktree_path.canonicalize().ok()?;
        if canonical_path.to_str()? != proof.worktree_path
            || !self
                .is_broker_owned_worktree_checked(&current, worktree_path)
                .ok()?
        {
            return None;
        }

        let common_path = self.main_git_common_dir().ok()?.canonicalize().ok()?;
        let common_metadata = std::fs::symlink_metadata(&common_path).ok()?;
        if common_metadata.file_type().is_symlink()
            || !common_metadata.is_dir()
            || common_path.to_str()? != proof.git_common_path
            || common_metadata.dev() != proof.git_common_device
            || common_metadata.ino() != proof.git_common_inode
        {
            return None;
        }
        let worktrees_path = common_path.join("worktrees");
        let worktrees_metadata = std::fs::symlink_metadata(&worktrees_path).ok()?;
        if worktrees_metadata.file_type().is_symlink() || !worktrees_metadata.is_dir() {
            return None;
        }
        let canonical_worktrees = worktrees_path.canonicalize().ok()?;
        let admin_path = Path::new(&proof.git_admin_path);
        let admin_name = admin_path.file_name()?;
        if !admin_path.is_absolute()
            || !matches!(admin_name.to_str(), Some(name) if !name.is_empty() && name != "." && name != "..")
            || canonical_worktrees.join(admin_name) != admin_path
        {
            return None;
        }
        match std::fs::symlink_metadata(admin_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) | Err(_) => return None,
        }

        let pointer_path = canonical_path.join(".git");
        match std::fs::symlink_metadata(&pointer_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
            Ok(_) => {
                let witness =
                    read_bounded_ownership_witness_with_metadata(&pointer_path).ok()??;
                if witness.metadata.dev() != proof.git_pointer_device
                    || witness.metadata.ino() != proof.git_pointer_inode
                {
                    return None;
                }
                let pointer_text = std::str::from_utf8(&witness.bytes).ok()?;
                let pointed_admin = pointer_text
                    .lines()
                    .find_map(|line| line.strip_prefix("gitdir:"))?
                    .trim();
                let pointed_admin = PathBuf::from(pointed_admin);
                let pointed_admin = if pointed_admin.is_absolute() {
                    pointed_admin
                } else {
                    canonical_path.join(pointed_admin)
                };
                let pointed_admin_parent = pointed_admin.parent()?.canonicalize().ok()?;
                let pointed_admin = pointed_admin_parent.join(pointed_admin.file_name()?);
                if pointed_admin != admin_path {
                    return None;
                }
            }
        }

        let expected_branch_ref = format!("refs/heads/{}", current.branch);
        if proof.branch_ref != expected_branch_ref
            || proof.head != proof.branch_tip
            || self.repo.resolve_ref(&expected_branch_ref).as_deref()
                != Some(proof.branch_tip.as_str())
        {
            return None;
        }
        let inventory = self.repo.worktree_inventory().ok()?;
        if inventory.iter().any(|entry| {
            entry.branch.as_deref() == Some(expected_branch_ref.as_str())
                || entry.path == canonical_path
                || entry.path.canonicalize().ok().as_ref() == Some(&canonical_path)
        }) {
            return None;
        }
        let delivery_targets = self.cleanup_delivery_targets().ok()?;
        let (provenance, _) = self
            .cleanup_provenance(&current, &proof.head, &delivery_targets)
            .ok()?;
        if provenance.representation != CleanupRepresentation::Represented
            || self
                .refuse_live_checkout(current.id, worktree_path)
                .is_err()
        {
            return None;
        }
        Some(proof)
    }

    fn resume_interrupted_finish_cleanup(
        &mut self,
        session: &Session,
        expected_proof: &crate::events::FinishCleanupRecoveryProof,
        status_limits: CleanupStatusLimits,
    ) -> Result<(), BrokerOpError> {
        let worktree_path = PathBuf::from(&session.worktree_path);
        self.refuse_live_checkout(session.id, &worktree_path)?;
        if self.interrupted_finish_cleanup_proof_with_status_limits(session, status_limits)
            != Some(expected_proof.clone())
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the interrupted finish cleanup identity or safety checks changed; inspect the worktree before retrying".into(),
            });
        }

        let policy = crate::AutoCleanupPolicy::load(self.main_root()).map_err(|reason| {
            BrokerOpError::DirtyWorktree {
                id: session.id,
                reason,
            }
        })?;
        if let Some(reason) = crate::auto_cleanup::ignored_content_reason(&worktree_path, &policy)
            .map_err(|reason| BrokerOpError::DirtyWorktree {
            id: session.id,
            reason,
        })? {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason,
            });
        }

        // `--force` is confined to this validated interrupted finish. The
        // exact registered checkout, represented head, and unstaged-deletion
        // only status were rechecked immediately above. Preserve Git's
        // refusal if its registration or the directory changed after the
        // proof; the generic orphan fallback is not authorized here.
        self.repo.worktree_remove_registered(&worktree_path, true)?;
        match std::fs::symlink_metadata(&worktree_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(BrokerOpError::DirtyWorktree {
                    id: session.id,
                    reason: "the interrupted finish worktree path still exists after Git removal"
                        .into(),
                });
            }
            Err(source) => {
                return Err(BrokerError::Io {
                    path: worktree_path.clone(),
                    source,
                }
                .into());
            }
        }

        self.refuse_live_checkout(session.id, &worktree_path)?;
        let current = self.store.session(session.id)?;
        if current.cleanup_state != crate::SessionCleanupState::Closed
            || current.origin != SessionOrigin::Spawned
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session is no longer the same closed cleanup owner".into(),
            });
        }
        let inventory = self.repo.worktree_inventory()?;
        if inventory
            .iter()
            .any(|entry| entry.branch.as_deref() == Some(expected_proof.branch_ref.as_str()))
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session branch is now checked out by a registered worktree".into(),
            });
        }
        let delivery_targets = self.cleanup_delivery_targets()?;
        let (provenance, _) =
            self.cleanup_provenance(&current, &expected_proof.head, &delivery_targets)?;
        if provenance.representation != CleanupRepresentation::Represented {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session head is no longer represented on a delivery target".into(),
            });
        }
        self.repo
            .delete_branch_ref_checked(&session.branch, &expected_proof.branch_tip)?;
        self.store
            .set_session_status(session.id, SessionStatus::Cleaned, None)?;
        Ok(())
    }

    fn resume_interrupted_finish_orphan_cleanup(
        &mut self,
        session: &Session,
        expected_proof: &crate::events::FinishCleanupRecoveryProof,
    ) -> Result<(), BrokerOpError> {
        let worktree_path = PathBuf::from(&session.worktree_path);
        self.refuse_live_checkout(session.id, &worktree_path)?;
        if self.interrupted_finish_orphan_cleanup_proof(session) != Some(expected_proof.clone()) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the interrupted orphan cleanup identity or safety checks changed; inspect the remaining files before retrying".into(),
            });
        }

        let policy = crate::AutoCleanupPolicy::load(self.main_root()).map_err(|reason| {
            BrokerOpError::DirtyWorktree {
                id: session.id,
                reason,
            }
        })?;
        if policy.strict {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "strict cleanup preserves interrupted orphan residue; Git metadata is unavailable to classify it, so remove it manually".into(),
            });
        }

        let outcome = crate::removal::remove_tree(&worktree_path);
        if !outcome.removed {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: format!(
                    "interrupted orphan removal stopped after {} attempt(s); preserved the branch and session for retry: {}",
                    outcome.attempts,
                    outcome.error.as_deref().unwrap_or("the path remains")
                ),
            });
        }
        require_worktree_path_absent(&worktree_path, session.id)?;
        self.refuse_live_checkout(session.id, &worktree_path)?;
        let current = self.store.session(session.id)?;
        if current.cleanup_state != crate::SessionCleanupState::Closed
            || current.origin != SessionOrigin::Spawned
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session is no longer the same closed orphan cleanup owner".into(),
            });
        }
        self.ensure_orphan_branch_not_checked_out(
            session.id,
            &expected_proof.branch_ref,
            &expected_proof.branch_tip,
            None,
        )?;
        let delivery_targets = self.cleanup_delivery_targets()?;
        let (provenance, _) =
            self.cleanup_provenance(&current, &expected_proof.head, &delivery_targets)?;
        if provenance.representation != CleanupRepresentation::Represented {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the session head is no longer represented on a delivery target".into(),
            });
        }
        self.repo
            .delete_branch_ref_checked(&session.branch, &expected_proof.branch_tip)?;
        self.store
            .set_session_status(session.id, SessionStatus::Cleaned, None)?;
        Ok(())
    }

    fn remove_proven_orphan_worktree(
        &self,
        session: &Session,
        item: Option<&CleanupWorktreePlan>,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(String, String), BrokerOpError> {
        let current = self.store.session(session.id)?;
        let Some(item) = item else {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason:
                    "the orphaned worktree has no current cleanup plan; preserved it for inspection"
                        .into(),
            });
        };
        let expected_branch_ref = format!("refs/heads/{}", session.branch);
        if current.cleanup_state != crate::SessionCleanupState::Closed
            || current.origin != SessionOrigin::Spawned
            || current.worktree_path != session.worktree_path
            || current.branch != session.branch
            || item.session_id != session.id
            || item.origin != SessionOrigin::Spawned
            || item.worktree_path != session.worktree_path
            || !item.worktree_present
            || (!force && !item.eligible())
            || item.branch_ref != expected_branch_ref
            || (!force
                && item.provenance.as_ref().is_none_or(|provenance| {
                    provenance.representation != CleanupRepresentation::Represented
                }))
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the orphaned checkout lacks a current represented cleanup proof; preserved it for inspection".into(),
            });
        }
        let branch_tip =
            item.branch_tip
                .as_deref()
                .ok_or_else(|| {
                    BrokerOpError::DirtyWorktree {
                id: session.id,
                reason:
                    "the orphaned checkout has no branch tip to preserve; left it for inspection"
                        .into(),
            }
                })?;
        if self.repo.resolve_ref(&expected_branch_ref).as_deref() != Some(branch_tip) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the orphaned checkout branch moved after its cleanup plan; preserved it for inspection".into(),
            });
        }
        if !force
            && self
                .interrupted_finish_orphan_cleanup_proof(session)
                .is_none()
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "non-force orphan removal requires the matching unfinished finish-cleanup proof; preserved the checkout for inspection".into(),
            });
        }
        if !self.is_broker_owned_worktree_checked(&current, worktree_path)?
            || !self.is_orphaned_worktree_of_this_repository_checked(worktree_path)?
        {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the worktree is not a positively identified orphan of this repository; preserved it for inspection".into(),
            });
        }
        let canonical_path = worktree_path
            .canonicalize()
            .map_err(|source| BrokerError::Io {
                path: worktree_path.to_path_buf(),
                source,
            })?;
        self.ensure_orphan_branch_not_checked_out(
            session.id,
            &expected_branch_ref,
            branch_tip,
            Some(&canonical_path),
        )?;
        self.refuse_live_checkout(session.id, worktree_path)?;

        // This path is no longer registered, so Git cannot remove it. Reach
        // this filesystem removal only with the current represented branch,
        // a same-repository `.git` pointer whose admin entry is explicitly
        // absent, and the session/path ownership checks above. In particular,
        // a Git discovery error alone never authorizes recursive removal.
        if !self.is_orphaned_worktree_of_this_repository_checked(worktree_path)? {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: "the orphan identity changed before removal; preserved the checkout".into(),
            });
        }
        let outcome = crate::removal::remove_tree(worktree_path);
        if !outcome.removed {
            return Err(BrokerOpError::DirtyWorktree {
                id: session.id,
                reason: format!(
                    "orphan removal stopped after {} attempt(s); preserved the branch and session for retry: {}",
                    outcome.attempts,
                    outcome.error.as_deref().unwrap_or("the path remains")
                ),
            });
        }
        Ok((expected_branch_ref, branch_tip.to_owned()))
    }

    fn ensure_orphan_branch_not_checked_out(
        &self,
        session_id: i64,
        branch_ref: &str,
        expected_tip: &str,
        worktree_path: Option<&Path>,
    ) -> Result<(), BrokerOpError> {
        if self.repo.resolve_ref(branch_ref).as_deref() != Some(expected_tip) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: "the orphan cleanup branch tip changed; preserved the branch and session for inspection".into(),
            });
        }
        let canonical_path =
            worktree_path.map(|path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
        let inventory = self.repo.worktree_inventory()?;
        if inventory.iter().any(|entry| {
            entry.branch.as_deref() == Some(branch_ref)
                || canonical_path.as_ref().is_some_and(|canonical| {
                    entry.path == *canonical
                        || entry.path.canonicalize().ok().as_ref() == Some(canonical)
                })
        }) {
            return Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: "the orphan cleanup branch or path is registered by another worktree; preserved it for inspection".into(),
            });
        }
        Ok(())
    }

    pub fn cleanup(&mut self, session_id: i64, force: bool) -> Result<(), BrokerOpError> {
        let initial_session = self.store.session(session_id)?;
        // Adoption and checkout creation take the same repository-wide claim
        // before changing ownership. Hold it from the final proof/live-session
        // checks through Git removal and the terminal database update.
        let _lifecycle_claim = self.claim_worktree_lifecycle()?;
        let session = self.store.session(session_id)?;
        ensure_lifecycle_session_unchanged(session_id, &initial_session, &session)?;
        self.cleanup_under_lifecycle_claim(&session, force, None)
    }

    /// Remove a worktree only after an archive was written and verified.
    /// The exact reviewed plan, including protected ignored files and policy,
    /// is recomputed under the lifecycle claim immediately before removal.
    pub(crate) fn cleanup_after_recovery_archive(
        &mut self,
        session_id: i64,
        expected_digest: &str,
        archive_path: &Path,
    ) -> Result<(), BrokerOpError> {
        let initial_session = self.store.session(session_id)?;
        let _lifecycle_claim = self.claim_worktree_lifecycle()?;
        let session = self.store.session(session_id)?;
        ensure_lifecycle_session_unchanged(session_id, &initial_session, &session)?;
        self.cleanup_under_lifecycle_claim(&session, true, Some((expected_digest, archive_path)))
    }

    /// Perform cleanup while the caller holds the repository lifecycle claim.
    /// Finish uses this after recording its durable cleanup intent under the
    /// same claim; reacquiring here would deadlock on the persistent lock.
    pub(super) fn cleanup_under_lifecycle_claim(
        &mut self,
        session: &Session,
        force: bool,
        archive_guard: Option<(&str, &Path)>,
    ) -> Result<(), BrokerOpError> {
        self.cleanup_under_lifecycle_claim_with_status_limits(
            session,
            force,
            archive_guard,
            CleanupStatusLimits::new(
                self.landing_deadline.get(),
                crate::git::MAX_CLEANUP_STATUS_OUTPUT_BYTES,
                crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
            ),
        )
    }

    fn cleanup_under_lifecycle_claim_with_status_limits(
        &mut self,
        session: &Session,
        force: bool,
        archive_guard: Option<(&str, &Path)>,
        status_limits: CleanupStatusLimits,
    ) -> Result<(), BrokerOpError> {
        let session_id = session.id;
        let worktree_path = PathBuf::from(&session.worktree_path);
        if archive_guard.is_some() && session.origin != SessionOrigin::Spawned {
            return Err(BrokerOpError::CleanupResolveRefused {
                id: session_id,
                reason: "only broker-spawned worktrees can be removed after archive resolution"
                    .into(),
            });
        }
        if !force
            && let Some(proof) =
                self.interrupted_finish_cleanup_proof_with_status_limits(session, status_limits)
        {
            return self.resume_interrupted_finish_cleanup(session, &proof, status_limits);
        }
        if !force && let Some(proof) = self.interrupted_finish_orphan_cleanup_proof(session) {
            return self.resume_interrupted_finish_orphan_cleanup(session, &proof);
        }
        if session.origin == SessionOrigin::Adopted {
            if worktree_path_is_real_directory(&worktree_path, session_id)? {
                if !force {
                    let targets = self.cleanup_delivery_targets()?;
                    let mut landing_cache = crate::representation::LandingVerdictCache::default();
                    let (disposition, reason, _) = self.cleanup_eligibility_with_status_limits(
                        session_id,
                        &worktree_path,
                        &targets,
                        &mut landing_cache,
                        status_limits,
                    )?;
                    if disposition != CleanupDisposition::Eligible {
                        return Err(BrokerOpError::DirtyWorktree {
                            id: session_id,
                            reason,
                        });
                    }
                }
                self.refuse_live_checkout(session_id, &worktree_path)?;
                self.repo
                    .worktree_remove_registered(&worktree_path, force)?;
            }
            require_worktree_path_absent(&worktree_path, session_id)?;
            self.store
                .set_session_status(session_id, SessionStatus::Cleaned, None)?;
            return Ok(());
        }
        // Recorded sizes, not a walk: removal needs the provenance verdict,
        // not the byte count, and a walk here only widens the window between
        // the live-checkout check below and the removal.
        let mut records = crate::measurement::SizeRecords::default();
        let item = self.cleanup_item_scanned_with_limits(
            session,
            crate::SizeScan::Recorded,
            &mut records,
            status_limits,
        )?;
        if !force
            && let Some(item) = item.as_ref()
            && !item.eligible()
        {
            let inspection = if item.inspection_commands.is_empty() {
                String::new()
            } else {
                format!(
                    "; inspect first with: {}",
                    item.inspection_commands.join("; ")
                )
            };
            return Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: format!(
                    "{}{}; exact discard command: {}",
                    item.reason, inspection, item.force_cleanup_command
                ),
            });
        }

        let mut removed_orphan_branch = None;
        if worktree_path_is_real_directory(&worktree_path, session_id)? {
            self.refuse_live_checkout(session_id, &worktree_path)?;
            if let Some((expected_digest, archive_path)) = archive_guard {
                let current = self.cleanup_resolve_plan(session_id)?;
                if current.digest != expected_digest {
                    return Err(BrokerOpError::CleanupResolveRefused {
                        id: session_id,
                        reason: "worktree contents or ignored cleanup policy changed after archive verification; the archive was kept and the worktree remains in place; review a new resolve plan before retrying".into(),
                    });
                }
                crate::cleanup_resolve::verify_recovery_archive_for_plan(
                    self.repo_handle(),
                    archive_path,
                    &current,
                )
                .map_err(|reason| BrokerOpError::CleanupResolveRefused {
                    id: session_id,
                    reason: format!(
                        "the verified archive changed before worktree removal; the worktree remains in place: {reason}"
                    ),
                })?;
            }
            if self.is_orphaned_worktree_of_this_repository_checked(&worktree_path)? {
                removed_orphan_branch = Some(self.remove_proven_orphan_worktree(
                    session,
                    item.as_ref(),
                    &worktree_path,
                    force,
                )?);
            } else {
                self.repo
                    .worktree_remove_registered(&worktree_path, force)?;
            }
        }
        require_worktree_path_absent(&worktree_path, session_id)?;
        if let Some((branch_ref, branch_tip)) = removed_orphan_branch.as_ref() {
            self.refuse_live_checkout(session_id, &worktree_path)?;
            self.ensure_orphan_branch_not_checked_out(session_id, branch_ref, branch_tip, None)?;
        }
        if let Some(branch_tip) = item.and_then(|item| item.branch_tip)
            && session.origin == SessionOrigin::Spawned
        {
            self.repo
                .delete_branch_ref_checked(&session.branch, &branch_tip)?;
        }
        self.store
            .set_session_status(session_id, SessionStatus::Cleaned, None)?;
        Ok(())
    }

    /// Refuse to remove a checkout any other live session works in.
    ///
    /// A closed session's directory can be adopted again (`start --adopt`,
    /// `--replace-stale`), leaving the closed row naming a live checkout.
    /// Its cleanliness and provenance then describe the live session's work,
    /// so no proof about the closed row authorizes removing it -- and no
    /// `--force` does either. Checked immediately before removal, because
    /// adoption takes no GC lock; paths are compared canonically, and a live
    /// checkout nested inside the directory counts too.
    pub(crate) fn refuse_live_checkout(
        &self,
        session_id: i64,
        worktree: &Path,
    ) -> Result<(), BrokerOpError> {
        let canonical =
            |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let root = canonical(worktree);
        for live in self.store.live_sessions()? {
            if live.id != session_id && canonical(Path::new(&live.worktree_path)).starts_with(&root)
            {
                return Err(BrokerOpError::WorktreeInUseByLiveSession {
                    id: session_id,
                    live_id: live.id,
                    path: worktree.to_string_lossy().into_owned(),
                });
            }
        }
        Ok(())
    }
}

fn worktree_path_is_real_directory(path: &Path, session_id: i64) -> Result<bool, BrokerOpError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(BrokerOpError::DirtyWorktree {
                id: session_id,
                reason: format!(
                    "worktree path {} is not a real directory; preserved the path and session, and did not delete the branch",
                    path.display()
                ),
            })
        }
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(BrokerError::Io {
            path: path.to_path_buf(),
            source,
        }
        .into()),
    }
}

fn require_worktree_path_absent(path: &Path, session_id: i64) -> Result<(), BrokerOpError> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(BrokerOpError::DirtyWorktree {
            id: session_id,
            reason: format!(
                "Git reported worktree removal but {} still exists; preserved the path, branch, and session for inspection",
                path.display()
            ),
        }),
        Err(source) => Err(BrokerError::Io {
            path: path.to_path_buf(),
            source,
        }
        .into()),
    }
}

pub(super) fn ensure_lifecycle_session_unchanged(
    session_id: i64,
    before: &Session,
    after: &Session,
) -> Result<(), BrokerOpError> {
    if before.worktree_path != after.worktree_path
        || before.branch != after.branch
        || before.origin != after.origin
        || before.status != after.status
        || before.cleanup_state != after.cleanup_state
        || before.closed_at != after.closed_at
        || before.task != after.task
        || before.diff_base != after.diff_base
        || before.adoption_base != after.adoption_base
        || before.adopted_head != after.adopted_head
        || before.pid != after.pid
        || before.command != after.command
        || before.log_path != after.log_path
        || before.agent_identity != after.agent_identity
        || before.updated_at != after.updated_at
        || before.last_activity_at != after.last_activity_at
    {
        return Err(BrokerOpError::DirtyWorktree {
            id: session_id,
            reason: "the session owner, activity, or cleanup state changed while waiting for the worktree lifecycle claim".into(),
        });
    }
    Ok(())
}

fn size_warm_candidates(plan: &CleanupPlan) -> Vec<String> {
    plan.worktrees
        .iter()
        .filter(|item| item.worktree_present && item.disposition != CleanupDisposition::UnsafePath)
        .map(|item| item.worktree_path.clone())
        .collect()
}

#[cfg(test)]
mod cleanup_size_ownership_tests {
    use super::{size_warm_candidates, worktree_size_estimate};
    use crate::measurement::{SizeRecords, SizeScan};
    use crate::{CleanupDisposition, CleanupPlan, CleanupWorktreePlan, SessionOrigin};

    fn worktree_plan(
        path: &str,
        present: bool,
        disposition: CleanupDisposition,
    ) -> CleanupWorktreePlan {
        CleanupWorktreePlan {
            session_id: 1,
            worktree_path: path.to_owned(),
            worktree_present: present,
            branch_ref: "refs/heads/test".to_owned(),
            branch_tip: None,
            delete_branch: false,
            origin: SessionOrigin::Spawned,
            disposition,
            provenance: None,
            estimated_bytes: None,
            estimated_inodes: None,
            reason: String::new(),
            inspection_commands: Vec::new(),
            force_cleanup_command: String::new(),
        }
    }

    #[test]
    fn warm_candidates_skip_unsafe_and_absent_worktrees_but_keep_owned_dirty_entries() {
        let plan = CleanupPlan {
            worktrees: vec![
                worktree_plan("/unsafe", true, CleanupDisposition::UnsafePath),
                worktree_plan("/owned-dirty", true, CleanupDisposition::Dirty),
                worktree_plan(
                    "/owned-unproven",
                    true,
                    CleanupDisposition::UnprovenProvenance,
                ),
                worktree_plan("/absent", false, CleanupDisposition::Eligible),
            ],
            ..CleanupPlan::default()
        };

        assert_eq!(
            size_warm_candidates(&plan),
            vec!["/owned-dirty", "/owned-unproven"]
        );
    }

    #[test]
    fn unowned_replacement_is_not_measured_or_reported_from_cache() {
        let temp = tempfile::tempdir().unwrap();
        let broker_root = temp.path().join("broker-worktrees");
        let replacement = temp.path().join("replaced-session-path");
        std::fs::create_dir_all(&broker_root).unwrap();
        std::fs::create_dir_all(&replacement).unwrap();
        assert!(!replacement.starts_with(&broker_root));

        let key = replacement.to_string_lossy().into_owned();
        let mut records = SizeRecords::default();
        records.record_usage(&key, 4096, Some(3), 1);
        let mut measurement_calls = 0;
        let measured = worktree_size_estimate(
            &replacement,
            &key,
            true,
            false,
            SizeScan::Measure,
            &mut records,
            |_| {
                measurement_calls += 1;
                panic!("an unowned path must not be traversed for size")
            },
        );
        assert_eq!(measured, (None, None));
        assert_eq!(measurement_calls, 0);

        let recorded = worktree_size_estimate(
            &replacement,
            &key,
            true,
            false,
            SizeScan::Recorded,
            &mut records,
            |_| {
                measurement_calls += 1;
                None
            },
        );
        assert_eq!(recorded, (None, None));
        assert_eq!(measurement_calls, 0);
    }

    #[test]
    fn owned_existing_path_is_measured_once_and_recorded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("owned-worktree");
        std::fs::create_dir(&path).unwrap();
        let key = path.to_string_lossy().into_owned();
        let mut records = SizeRecords::default();
        let mut measurement_calls = 0;

        let estimate = worktree_size_estimate(
            &path,
            &key,
            true,
            true,
            SizeScan::Measure,
            &mut records,
            |measured_path| {
                measurement_calls += 1;
                assert_eq!(measured_path, path.as_path());
                Some(crate::disk_headroom::DirectoryUsage {
                    bytes: 8192,
                    inodes: 5,
                })
            },
        );

        assert_eq!(estimate, (Some(8192), Some(5)));
        assert_eq!(measurement_calls, 1);
        assert_eq!(
            records
                .get(&key)
                .map(|record| (record.bytes, record.inodes)),
            Some((8192, Some(5)))
        );
    }

    #[test]
    fn absent_path_is_zero_without_measuring() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("absent-worktree");
        let key = path.to_string_lossy().into_owned();
        let mut records = SizeRecords::default();
        let mut measurement_calls = 0;

        let estimate = worktree_size_estimate(
            &path,
            &key,
            false,
            false,
            SizeScan::Measure,
            &mut records,
            |_| {
                measurement_calls += 1;
                panic!("an absent path must not be measured")
            },
        );

        assert_eq!(estimate, (Some(0), Some(0)));
        assert_eq!(measurement_calls, 0);
        assert_eq!(records.get(&key), None);
    }
}

#[cfg(test)]
mod ownership_witness_tests {
    use super::{WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES, read_bounded_ownership_witness};
    #[cfg(unix)]
    use std::time::Duration;

    #[test]
    fn ownership_witness_reads_are_size_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marker");
        std::fs::write(&path, b"small marker").unwrap();
        assert_eq!(
            read_bounded_ownership_witness(&path).unwrap().as_deref(),
            Some(&b"small marker"[..])
        );

        std::fs::write(
            &path,
            vec![b'x'; WORKTREE_OWNERSHIP_WITNESS_MAX_BYTES as usize + 1],
        )
        .unwrap();
        assert!(read_bounded_ownership_witness(&path).unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn ownership_witness_open_rejects_symlinks_and_does_not_block_on_fifos() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let target = temp.path().join("target");
        let linked = temp.path().join("linked-marker");
        std::fs::write(&target, b"target marker").unwrap();
        symlink(&target, &linked).unwrap();
        assert!(read_bounded_ownership_witness(&linked).unwrap().is_none());

        let fifo = temp.path().join("fifo-marker");
        let output = std::process::Command::new("/usr/bin/mkfifo")
            .arg(&fifo)
            .output()
            .unwrap();
        assert!(output.status.success());
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            assert!(sender.send(read_bounded_ownership_witness(&fifo)).is_ok());
        });
        assert!(matches!(
            receiver.recv_timeout(Duration::from_secs(1)),
            Ok(Ok(None))
        ));
    }
}

#[cfg(test)]
mod registered_removal_failure_tests {
    use super::CleanupStatusLimits;
    use crate::{SessionCleanupState, git::with_git_deadline};
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    fn initialized_repository(root: &Path) -> std::path::PathBuf {
        let repository = root.join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        git(&repository, &["config", "user.name", "Cleanup test"]);
        git(
            &repository,
            &["config", "user.email", "cleanup@example.invalid"],
        );
        git(&repository, &["commit", "--allow-empty", "-qm", "initial"]);
        repository
    }

    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn remove_worktree_admin_directory(worktree: &Path) {
        let pointer = std::fs::read_to_string(worktree.join(".git")).unwrap();
        let gitdir = pointer
            .trim()
            .strip_prefix("gitdir:")
            .expect("linked worktree has a gitdir pointer")
            .trim();
        let gitdir = Path::new(gitdir);
        let gitdir = if gitdir.is_absolute() {
            gitdir.to_path_buf()
        } else {
            worktree.join(gitdir)
        };
        std::fs::remove_dir_all(gitdir).unwrap();
    }

    fn repository_with_ignored_cleanup_fixture(root: &Path) -> std::path::PathBuf {
        let repository = initialized_repository(root);
        std::fs::write(
            repository.join(".gitignore"),
            "/.aethyme/\n/target/\n/local-dataset/\n",
        )
        .unwrap();
        std::fs::write(repository.join("README.md"), "tracked fixture\n").unwrap();
        git(&repository, &["add", ".gitignore", "README.md"]);
        git(&repository, &["commit", "-qm", "cleanup fixture"]);
        std::fs::create_dir_all(repository.join(".aethyme")).unwrap();
        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = false\n",
        )
        .unwrap();
        repository
    }

    fn start_represented_cleanup_session(
        broker: &mut crate::Broker,
        label: &str,
    ) -> (crate::Session, std::path::PathBuf) {
        let session = broker.start_worktree(label, None).unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        std::fs::write(worktree.join("session-change.txt"), "represented\n").unwrap();
        git(&worktree, &["add", "session-change.txt"]);
        git(&worktree, &["commit", "-qm", "represented cleanup head"]);
        assert!(
            broker.submit(session.id).unwrap().promoted,
            "recovery fixture must represent the session head before cleanup starts"
        );
        (session, worktree)
    }

    fn persist_finish_cleanup_intent(
        broker: &mut crate::Broker,
        session: &crate::Session,
        worktree: &Path,
    ) -> crate::events::FinishCleanupRecoveryProof {
        let branch_ref = format!("refs/heads/{}", session.branch);
        let checkout = crate::git::GitRepo::discover(worktree).unwrap();
        let proof = checkout
            .finish_cleanup_recovery_proof(&branch_ref)
            .expect("the represented linked checkout must yield a durable recovery proof");
        let payload = crate::events::session_finish_cleanup_started_payload_with_proof(
            true,
            true,
            Some(&proof),
        );
        broker
            .store()
            .begin_finish_cleanup(session.id, &payload)
            .unwrap();
        proof
    }

    fn advance_schema_floor(database: &Path) {
        let mut conn = rusqlite::Connection::open(database).unwrap();
        let tx = conn.transaction().unwrap();
        let newer = crate::SCHEMA_VERSION + 1;
        for key in ["schema_version", "min_compatible_schema"] {
            assert_eq!(
                tx.execute(
                    "UPDATE meta SET value=?2 WHERE key=?1",
                    rusqlite::params![key, newer.to_string()]
                )
                .unwrap(),
                1
            );
        }
        tx.commit().unwrap();
    }

    fn assert_new_schema_refusal(error: &crate::BrokerOpError) {
        assert!(
            matches!(error, crate::BrokerOpError::Store(crate::BrokerError::SchemaTooNew { found, supported })
            if *found == crate::SCHEMA_VERSION + 1 && *supported == crate::SCHEMA_VERSION),
            "{error:?}"
        );
    }

    #[test]
    fn incomplete_bounded_status_keeps_a_closed_cleanup_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repository_with_ignored_cleanup_fixture(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "bounded status refusal");
        broker.close(session.id).unwrap();

        let first = worktree.join("untracked-one.txt");
        let second = worktree.join("untracked-two.txt");
        std::fs::write(&first, "keep one\n").unwrap();
        std::fs::write(&second, "keep two\n").unwrap();
        let closed_session = broker.store().session(session.id).unwrap();
        let _claim = broker.claim_worktree_lifecycle().unwrap();

        let error = broker
            .cleanup_under_lifecycle_claim_with_status_limits(
                &closed_session,
                false,
                None,
                CleanupStatusLimits::new(None, 1024 * 1024, 1),
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("cleanup inspection failed"),
            "an incomplete status inventory must become an explicit cleanup refusal: {error}"
        );
        assert!(
            worktree.is_dir(),
            "incomplete inspection removed the checkout"
        );
        assert_eq!(std::fs::read(first).unwrap(), b"keep one\n");
        assert_eq!(std::fs::read(second).unwrap(), b"keep two\n");
    }

    #[test]
    fn incomplete_status_refuses_interrupted_finish_recovery() {
        let tmp = tempfile::tempdir().unwrap();
        let root = repository_with_ignored_cleanup_fixture(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "bounded finish recovery status");
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        let limits = CleanupStatusLimits::new(None, 1024 * 1024, 0);

        assert!(
            broker
                .interrupted_finish_cleanup_proof_with_status_limits(&closed, limits)
                .is_none(),
            "an incomplete --ignored=matching status inventory must not authorize recovery"
        );

        let _claim = broker.claim_worktree_lifecycle().unwrap();
        let error = broker
            .cleanup_under_lifecycle_claim_with_status_limits(
                &closed,
                false,
                None,
                CleanupStatusLimits::new(
                    Some(std::time::Instant::now() - std::time::Duration::from_millis(1)),
                    1024 * 1024,
                    crate::git::MAX_CLEANUP_STATUS_PATH_FIELDS,
                ),
            )
            .unwrap_err();
        assert!(
            error.to_string().contains("cleanup inspection failed"),
            "bounded status failure must become an explicit cleanup refusal: {error}"
        );
        assert!(worktree.is_dir(), "incomplete proof removed the worktree");
        assert!(worktree.join("session-change.txt").is_file());
        assert!(!worktree.join("README.md").exists());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(
            broker.repo.resolve_ref(&proof.branch_ref).is_some(),
            "incomplete status proof must retain the represented branch"
        );
    }

    #[test]
    fn lifecycle_claim_refuses_a_floor_raised_after_controller_open() {
        let tmp = tempfile::tempdir().unwrap();
        let root = initialized_repository(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let database = broker.store().db_path().to_path_buf();
        let reached_claim = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = std::sync::Arc::clone(&reached_claim);
        broker.worktree_lifecycle_test_observer = Some(std::sync::Arc::new(move |_| {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        }));
        advance_schema_floor(&database);
        let error = broker.claim_worktree_lifecycle().unwrap_err();
        assert_new_schema_refusal(&error);
        assert!(!reached_claim.load(std::sync::atomic::Ordering::SeqCst));
        assert!(!storage.worktree_root().exists());
    }

    #[test]
    fn start_rechecks_the_floor_after_waiting_for_the_lifecycle_claim() {
        let tmp = tempfile::tempdir().unwrap();
        let root = initialized_repository(tmp.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let database = broker.store().db_path().to_path_buf();
        let claim = crate::file_lock::acquire_repository_lifecycle_lock(&root).unwrap();
        let held = std::sync::Arc::new(std::sync::Mutex::new(Some(claim)));
        let held_at_boundary = std::sync::Arc::clone(&held);
        let reached = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reached_at_boundary = std::sync::Arc::clone(&reached);
        broker.worktree_lifecycle_test_observer = Some(std::sync::Arc::new(move |_| {
            reached_at_boundary.store(true, std::sync::atomic::Ordering::SeqCst);
            advance_schema_floor(&database);
            drop(held_at_boundary.lock().unwrap().take());
        }));
        let before = git_head(&root);
        let error = broker
            .start_worktree("must not allocate", None)
            .unwrap_err();
        assert_new_schema_refusal(&error);
        assert!(reached.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            !storage.worktree_root().exists(),
            "refusal must precede root preparation"
        );
        assert!(broker.store().live_sessions().unwrap().is_empty());
        assert_eq!(git_head(&root), before);
        // The failed post-acquisition check must release the claim.
        drop(crate::file_lock::acquire_repository_lifecycle_lock(&root).unwrap());
    }

    #[test]
    fn stale_controller_force_cleanup_preserves_registered_and_orphan_checkouts() {
        for orphan in [false, true] {
            let tmp = tempfile::tempdir().unwrap();
            let root = initialized_repository(tmp.path());
            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&root).unwrap();
            let session = broker
                .start_worktree("preserve while incompatible", None)
                .unwrap();
            broker.close(session.id).unwrap();
            let path = Path::new(&session.worktree_path);
            let evidence = path.join("service-data.txt");
            std::fs::write(&evidence, b"must survive").unwrap();
            if orphan {
                remove_worktree_admin_directory(path);
            }
            let database = broker.store().db_path().to_path_buf();
            let before = broker.store().session(session.id).unwrap();
            advance_schema_floor(&database);
            let error = broker.cleanup(session.id, true).unwrap_err();
            assert_new_schema_refusal(&error);
            assert_eq!(std::fs::read(&evidence).unwrap(), b"must survive");
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                before.cleanup_state
            );
            let branch = format!("refs/heads/{}", session.branch);
            assert!(broker.repo.resolve_ref(&branch).is_some());
        }
    }

    fn git_head(root: &Path) -> Vec<u8> {
        let mut command = Command::new("git");
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("GIT_") {
                command.env_remove(key);
            }
        }
        let output = command
            .current_dir(root)
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        output.stdout
    }

    #[test]
    fn non_force_orphan_cleanup_without_finish_intent_preserves_user_data() {
        for remove_pointer in [false, true] {
            let owner = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let repository = initialized_repository(owner.path());
            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("manual orphan without finish intent", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path).to_path_buf();
            broker.close(session.id).unwrap();
            let evidence = worktree.join("unique-user-data.txt");
            std::fs::write(&evidence, b"keep this unreviewed data").unwrap();
            remove_worktree_admin_directory(&worktree);
            if remove_pointer {
                std::fs::remove_file(worktree.join(".git")).unwrap();
            }

            let result = broker.cleanup(session.id, false);

            assert!(
                result.is_err(),
                "a manual orphan without finish intent must remain inspection-only"
            );
            assert_eq!(
                std::fs::read(&evidence).unwrap(),
                b"keep this unreviewed data",
                "manual admin loss must not authorize discarding user data"
            );
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                SessionCleanupState::Closed
            );
            assert!(
                crate::git::GitRepo::discover(&repository)
                    .unwrap()
                    .resolve_ref(&format!("refs/heads/{}", session.branch))
                    .is_some(),
                "a proofless orphan must retain its branch"
            );
        }
    }

    #[test]
    fn explicit_force_can_remove_a_positively_identified_same_repository_orphan() {
        let owner = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let repository = initialized_repository(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("explicitly forced same repository orphan", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        broker.close(session.id).unwrap();
        let evidence = worktree.join("discarded-by-explicit-force.txt");
        std::fs::write(&evidence, b"explicit force is a discard request").unwrap();
        remove_worktree_admin_directory(&worktree);

        broker.cleanup(session.id, true).unwrap();

        assert!(!worktree.exists());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Cleaned
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&format!("refs/heads/{}", session.branch))
                .is_none(),
            "explicit force should remove the owned session branch"
        );
    }

    #[test]
    fn registered_finish_recovery_obeys_strict_policy_flip_and_manual_residue_removal() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "registered recovery strict flip");

        let cache = worktree.join("target");
        std::fs::create_dir_all(&cache).unwrap();
        std::fs::write(cache.join("CACHEDIR.TAG"), "Signature: 8a477f597d28d172\n").unwrap();
        std::fs::write(cache.join("payload"), b"known ignored output").unwrap();
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_cleanup_proof(&closed),
            Some(proof.clone()),
            "the represented interrupted finish must be recoverable before policy changes"
        );
        assert!(broker.store().live_sessions().unwrap().is_empty());

        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = true\n",
        )
        .unwrap();
        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(
            worktree.exists(),
            "strict policy unexpectedly removed the checkout with ignored residue: {error}"
        );
        assert!(cache.join("payload").is_file());
        assert!(worktree.join("session-change.txt").is_file());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());

        // Strict mode permits finish recovery once the operator has removed
        // every ignored leftover by hand.
        std::fs::remove_dir_all(&cache).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_cleanup_proof(&closed),
            Some(proof.clone())
        );
        broker.cleanup(session.id, false).unwrap();
        assert!(!worktree.exists());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_none());
    }

    #[test]
    fn orphan_finish_recovery_requires_strict_policy_to_be_disabled() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "orphan recovery strict flip");

        let residue = worktree.join("local-dataset/saved-session.json");
        std::fs::create_dir_all(residue.parent().unwrap()).unwrap();
        std::fs::write(&residue, b"saved session data").unwrap();
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        remove_worktree_admin_directory(&worktree);
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_orphan_cleanup_proof(&closed),
            Some(proof.clone()),
            "the same-repository orphan must have a durable recovery proof before policy changes"
        );
        assert!(broker.store().live_sessions().unwrap().is_empty());

        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = true\n",
        )
        .unwrap();
        assert_eq!(
            broker.interrupted_finish_orphan_cleanup_proof(&closed),
            Some(proof.clone()),
            "the policy flip must not alter the identity proof"
        );
        let error = broker.cleanup(session.id, false).unwrap_err();
        assert!(
            error.to_string().contains("strict cleanup"),
            "strict policy should retain orphan residue: {error}"
        );
        assert!(residue.is_file());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());

        std::fs::write(
            repository.join(".aethyme/config.toml"),
            "[cleanup]\nstrict = false\n",
        )
        .unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_orphan_cleanup_proof(&closed),
            Some(proof.clone())
        );
        broker.cleanup(session.id, false).unwrap();
        assert!(!worktree.exists());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_none());
    }

    #[test]
    fn interrupted_finish_recovery_still_refuses_an_unrepresented_session_head() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("unrepresented recovery protection", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        std::fs::write(worktree.join("unpublished.txt"), "not submitted\n").unwrap();
        git(&worktree, &["add", "unpublished.txt"]);
        git(&worktree, &["commit", "-qm", "unrepresented session head"]);

        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        let targets = broker.cleanup_delivery_targets().unwrap();
        let (provenance, _) = broker
            .cleanup_provenance(&session, &proof.head, &targets)
            .unwrap();
        assert_ne!(
            provenance.representation,
            crate::CleanupRepresentation::Represented,
            "this fixture intentionally leaves its session commit unsubmitted"
        );
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert!(
            broker.interrupted_finish_cleanup_proof(&closed).is_none(),
            "a durable finish intent must not substitute for delivery representation"
        );

        assert!(broker.cleanup(session.id, false).is_err());
        assert!(worktree.join("unpublished.txt").is_file());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_some());
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
    }

    #[test]
    fn registered_finish_recovery_discards_opaque_unknown_ignored_directory_by_default() {
        let owner = tempfile::tempdir().unwrap();
        let repository = repository_with_ignored_cleanup_fixture(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let (session, worktree) =
            start_represented_cleanup_session(&mut broker, "opaque ignored recovery");

        let residue = worktree.join("local-dataset/saved-session.json");
        std::fs::create_dir_all(residue.parent().unwrap()).unwrap();
        std::fs::write(&residue, b"opaque ignored directory payload").unwrap();
        let proof = persist_finish_cleanup_intent(&mut broker, &session, &worktree);
        std::fs::remove_file(worktree.join("README.md")).unwrap();
        let closed = broker.store().session(session.id).unwrap();
        assert_eq!(
            broker.interrupted_finish_cleanup_proof(&closed),
            Some(proof.clone()),
            "unknown ignored directory contents are opaque in disposable mode"
        );

        broker.cleanup(session.id, false).unwrap();

        assert!(!worktree.exists());
        assert!(broker.repo.resolve_ref(&proof.branch_ref).is_none());
    }

    #[test]
    fn registered_cleanup_preserves_checkout_after_git_removal_timeout() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());

        let worktree = owner.path().join("registered-worktree");
        let worktree_text = worktree.to_string_lossy().into_owned();
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fixture-adopted",
                &worktree_text,
                "HEAD",
            ],
        );
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let adopted = broker
            .adopt(&worktree, Some("closed checkout cleanup"))
            .unwrap();
        broker.close(adopted.id).unwrap();
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve this checkout").unwrap();

        let result = with_git_deadline(Duration::ZERO, || broker.cleanup(adopted.id, true));
        assert!(result.is_err(), "expired Git budget must refuse removal");
        assert_eq!(std::fs::read(&evidence).unwrap(), b"preserve this checkout");
        assert_eq!(
            broker.store().session(adopted.id).unwrap().cleanup_state,
            SessionCleanupState::Closed,
            "failed removal must leave the session available for retry"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref("refs/heads/fixture-adopted")
                .is_some(),
            "failed removal must retain its branch"
        );
    }

    #[test]
    fn adopted_cleanup_preserves_non_directory_replacement_and_branch() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let parent = owner.path().join("adopted-parent");
        std::fs::create_dir(&parent).unwrap();
        let worktree = parent.join("checkout");
        let worktree_text = worktree.to_string_lossy().into_owned();
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fixture-adopted-replacement",
                &worktree_text,
                "HEAD",
            ],
        );
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve adopted checkout").unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let adopted = broker
            .adopt(&worktree, Some("replacement cleanup"))
            .unwrap();
        broker.close(adopted.id).unwrap();

        let preserved = parent.join("preserved-checkout");
        std::fs::rename(&worktree, &preserved).unwrap();
        std::fs::write(&worktree, b"replacement data").unwrap();

        let result = broker.cleanup(adopted.id, true);

        assert!(
            result.is_err(),
            "a non-directory replacement must be retained"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a real directory")
        );
        assert_eq!(
            broker.store().session(adopted.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert_eq!(std::fs::read(&worktree).unwrap(), b"replacement data");
        assert_eq!(
            std::fs::read(preserved.join("keep-for-retry.txt")).unwrap(),
            b"preserve adopted checkout"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref("refs/heads/fixture-adopted-replacement")
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn adopted_cleanup_preserves_session_and_branch_when_path_metadata_fails() {
        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let parent = owner.path().join("adopted-parent");
        std::fs::create_dir(&parent).unwrap();
        let worktree = parent.join("checkout");
        let worktree_text = worktree.to_string_lossy().into_owned();
        git(
            &repository,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "fixture-adopted",
                &worktree_text,
                "HEAD",
            ],
        );
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve adopted checkout").unwrap();

        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let adopted = broker
            .adopt(&worktree, Some("metadata-error cleanup"))
            .unwrap();
        broker.close(adopted.id).unwrap();

        let preserved_parent = owner.path().join("preserved-parent");
        std::fs::rename(&parent, &preserved_parent).unwrap();
        std::fs::write(&parent, b"not a directory").unwrap();

        let result = broker.cleanup(adopted.id, true);

        assert!(
            result.is_err(),
            "metadata uncertainty must retain the session"
        );
        assert_eq!(
            broker.store().session(adopted.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert_eq!(
            std::fs::read(preserved_parent.join("checkout/keep-for-retry.txt")).unwrap(),
            b"preserve adopted checkout"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref("refs/heads/fixture-adopted")
                .is_some()
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawned_cleanup_preserves_dangling_replacement_and_branch() {
        use std::os::unix::fs::symlink;

        let owner = tempfile::tempdir().unwrap();
        let repository = initialized_repository(owner.path());
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&repository).unwrap();
        let session = broker
            .start_worktree("dangling cleanup replacement", None)
            .unwrap();
        let worktree = Path::new(&session.worktree_path).to_path_buf();
        broker.close(session.id).unwrap();
        let evidence = worktree.join("keep-for-retry.txt");
        std::fs::write(&evidence, b"preserve spawned checkout").unwrap();

        let preserved = owner.path().join("preserved-spawned-checkout");
        std::fs::rename(&worktree, &preserved).unwrap();
        let missing_target = owner.path().join("missing-target");
        symlink(&missing_target, &worktree).unwrap();

        let result = broker.cleanup(session.id, true);

        assert!(
            result.is_err(),
            "a dangling symlink is not an absent worktree"
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a real directory")
        );
        assert_eq!(
            broker.store().session(session.id).unwrap().cleanup_state,
            SessionCleanupState::Closed
        );
        assert!(
            std::fs::symlink_metadata(&worktree)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read(preserved.join("keep-for-retry.txt")).unwrap(),
            b"preserve spawned checkout"
        );
        assert!(
            crate::git::GitRepo::discover(&repository)
                .unwrap()
                .resolve_ref(&format!("refs/heads/{}", session.branch))
                .is_some()
        );
    }

    #[test]
    fn spawned_cleanup_refuses_orphan_paths_without_a_valid_same_repository_pointer() {
        for invalid_pointer in ["missing", "malformed", "wrong-repository", "metadata-error"] {
            let owner = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
            let repository = initialized_repository(owner.path());
            let foreign_repository = if invalid_pointer == "wrong-repository" {
                Some(initialized_repository(&owner.path().join("foreign")))
            } else {
                None
            };
            let storage = crate::test_storage::TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("invalid orphan pointer", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path).to_path_buf();
            broker.close(session.id).unwrap();
            let evidence = worktree.join("keep-for-retry.txt");
            std::fs::write(&evidence, b"preserve this checkout").unwrap();
            remove_worktree_admin_directory(&worktree);

            match invalid_pointer {
                "missing" => std::fs::remove_file(worktree.join(".git")).unwrap(),
                "malformed" => {
                    std::fs::write(worktree.join(".git"), b"not a gitdir pointer\n").unwrap()
                }
                "wrong-repository" => {
                    let foreign = foreign_repository.as_ref().unwrap();
                    let gitdir = foreign.join(".git/worktrees/foreign-checkout");
                    std::fs::write(
                        worktree.join(".git"),
                        format!("gitdir: {}\n", gitdir.display()),
                    )
                    .unwrap();
                }
                "metadata-error" => {
                    let non_directory = repository.join(".git/not-a-directory");
                    std::fs::write(&non_directory, b"file where a directory is required").unwrap();
                    let gitdir = non_directory.join("worktrees/missing-admin");
                    std::fs::write(
                        worktree.join(".git"),
                        format!("gitdir: {}\n", gitdir.display()),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }

            let result = broker.cleanup(session.id, true);

            assert!(
                result.is_err(),
                "{invalid_pointer} orphan evidence must not authorize removal"
            );
            assert_eq!(
                std::fs::read(&evidence).unwrap(),
                b"preserve this checkout",
                "{invalid_pointer} pointer must leave the checkout contents intact"
            );
            assert_eq!(
                broker.store().session(session.id).unwrap().cleanup_state,
                SessionCleanupState::Closed,
                "{invalid_pointer} pointer must leave the session retryable"
            );
            assert!(
                crate::git::GitRepo::discover(&repository)
                    .unwrap()
                    .resolve_ref(&format!("refs/heads/{}", session.branch))
                    .is_some(),
                "{invalid_pointer} pointer must retain the branch"
            );
        }
    }
}

#[cfg(test)]
mod tracked_submodule_cleanup_tests {
    use super::CleanupDisposition;
    use crate::git::GitRepo;
    use crate::test_storage::TestStorage;
    use std::path::Path;
    use std::process::Command;

    fn git(root: &Path, args: &[&str]) {
        let mut command = Command::new("git");
        let git_keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in git_keys {
            command.env_remove(key);
        }
        let status = command
            .args(args)
            .current_dir(root)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "submodule cleanup test")
            .env("GIT_AUTHOR_EMAIL", "submodule-cleanup@example.invalid")
            .env("GIT_COMMITTER_NAME", "submodule cleanup test")
            .env("GIT_COMMITTER_EMAIL", "submodule-cleanup@example.invalid")
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?}");
    }

    #[test]
    fn cleanup_retains_dirty_submodule_files_under_ignore_all_configuration() {
        for child_state in ["modified", "untracked"] {
            let owner = tempfile::tempdir().unwrap();
            let repository = owner.path().join("repository");
            let source = owner.path().join("submodule-source");
            std::fs::create_dir(&repository).unwrap();
            std::fs::create_dir(&source).unwrap();
            git(&source, &["init", "-q", "-b", "main"]);
            git(&source, &["config", "user.name", "submodule cleanup test"]);
            git(
                &source,
                &["config", "user.email", "submodule-cleanup@example.invalid"],
            );
            std::fs::write(source.join("tracked.txt"), "initial child content\n").unwrap();
            git(&source, &["add", "tracked.txt"]);
            git(&source, &["commit", "-qm", "initial child commit"]);

            git(&repository, &["init", "-q", "-b", "main"]);
            git(
                &repository,
                &["config", "user.name", "submodule cleanup test"],
            );
            git(
                &repository,
                &["config", "user.email", "submodule-cleanup@example.invalid"],
            );
            std::fs::write(repository.join("README.md"), "base\n").unwrap();
            git(&repository, &["add", "README.md"]);
            git(&repository, &["commit", "-qm", "superproject base"]);
            let source_path = source.to_str().unwrap();
            git(
                &repository,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "add",
                    "--quiet",
                    source_path,
                    "sub",
                ],
            );
            git(
                &repository,
                &["config", "-f", ".gitmodules", "submodule.sub.ignore", "all"],
            );
            git(&repository, &["add", "-A"]);
            git(&repository, &["commit", "-qm", "add child submodule"]);
            git(&repository, &["config", "diff.ignoreSubmodules", "all"]);
            git(&repository, &["config", "submodule.sub.ignore", "all"]);

            let storage = TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker
                .start_worktree("dirty child submodule", None)
                .unwrap();
            let worktree = Path::new(&session.worktree_path);
            assert_eq!(
                GitRepo::discover(worktree).unwrap().head_commit().unwrap(),
                GitRepo::discover(&repository)
                    .unwrap()
                    .head_commit()
                    .unwrap(),
                "the superproject commit is already the parent checkout HEAD"
            );
            git(
                worktree,
                &[
                    "-c",
                    "protocol.file.allow=always",
                    "submodule",
                    "update",
                    "--init",
                    "--quiet",
                    "--",
                    "sub",
                ],
            );
            let child = worktree.join("sub");
            let dirty_path = match child_state {
                "modified" => {
                    let path = child.join("tracked.txt");
                    std::fs::write(&path, "changed child content\n").unwrap();
                    path
                }
                "untracked" => {
                    let path = child.join("new.txt");
                    std::fs::write(&path, "new child content\n").unwrap();
                    path
                }
                _ => unreachable!(),
            };
            broker.close(session.id).unwrap();

            let closed_session = broker.store().session(session.id).unwrap();
            let item = broker
                .cleanup_item(&closed_session)
                .unwrap()
                .expect("the closed broker worktree remains in cleanup planning");
            assert_eq!(item.disposition, CleanupDisposition::Dirty, "{child_state}");
            assert!(
                item.reason
                    .contains("worktree has uncommitted or untracked changes"),
                "{child_state}: {}",
                item.reason
            );
            let error = broker.cleanup(session.id, false).unwrap_err().to_string();
            assert!(
                error.contains("uncommitted or untracked changes"),
                "{error}"
            );
            assert!(
                worktree.is_dir(),
                "{child_state} submodule worktree was removed"
            );
            assert!(dirty_path.is_file(), "{child_state} child data was removed");
        }
    }
}

#[cfg(test)]
mod cleanup_target_snapshot_tests {
    use super::CleanupDisposition;
    use crate::test_storage::TestStorage;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const ROOT_ENV: &str = "AETHYME_CLEANUP_TARGET_TRACE_ROOT";
    const CHILD_TEST: &str = "broker::cleanup::cleanup_target_snapshot_tests::cleanup_plan_reuses_one_delivery_target_snapshot";
    const TRANSIENT_ROOT_ENV: &str = "AETHYME_CLEANUP_TARGET_FAILURE_ROOT";
    const FAIL_HEAD_ROOT_ENV: &str = "AETHYME_CLEANUP_TARGET_FAIL_HEAD_ROOT";
    const FAIL_HEAD_MARKER_ENV: &str = "AETHYME_CLEANUP_TARGET_FAIL_HEAD_MARKER";
    const REAL_GIT_ENV: &str = "AETHYME_CLEANUP_TARGET_REAL_GIT";
    const TRANSIENT_CHILD_TEST: &str = "broker::cleanup::cleanup_target_snapshot_tests::cleanup_plan_fails_closed_after_transient_primary_head_failure";

    fn clear_git_environment(command: &mut Command) {
        let keys = std::env::vars_os()
            .map(|(key, _)| key)
            .chain(command.get_envs().map(|(key, _)| key.to_os_string()))
            .filter(|key| key.to_string_lossy().starts_with("GIT_"))
            .collect::<Vec<_>>();
        for key in keys {
            command.env_remove(key);
        }
    }

    fn git(root: &Path, args: &[&str]) -> Output {
        let mut command = Command::new("git");
        clear_git_environment(&mut command);
        let output = command.args(args).current_dir(root).output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn git_text(root: &Path, args: &[&str]) -> String {
        String::from_utf8(git(root, args).stdout)
            .unwrap()
            .trim()
            .to_owned()
    }

    fn initialized_repository(parent: &Path) -> PathBuf {
        let repository = parent.join("repository");
        std::fs::create_dir_all(&repository).unwrap();
        git(&repository, &["init", "-q", "-b", "main"]);
        git(&repository, &["config", "user.name", "Cleanup target test"]);
        git(
            &repository,
            &["config", "user.email", "cleanup-target@example.invalid"],
        );
        std::fs::write(repository.join("README.md"), "base\n").unwrap();
        git(&repository, &["add", "README.md"]);
        git(&repository, &["commit", "-qm", "initial"]);
        repository
    }

    struct UpstreamLookupTrace {
        starts: Vec<String>,
    }

    impl UpstreamLookupTrace {
        fn count(&self) -> usize {
            self.starts.len()
        }

        fn diagnostic(&self) -> String {
            self.starts.join("\n")
        }
    }

    fn upstream_lookup_trace(trace: &Path) -> UpstreamLookupTrace {
        let contents = std::fs::read_to_string(trace)
            .unwrap_or_else(|error| panic!("read Git Trace2 file {}: {error}", trace.display()));
        let mut starts = Vec::new();
        for (index, line) in contents.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let event = serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|error| {
                panic!(
                    "invalid Git Trace2 JSON at {}:{}: {error}; record={line:?}",
                    trace.display(),
                    index + 1
                )
            });
            if event["event"] != "start" {
                continue;
            }
            let argv = event["argv"].as_array().unwrap_or_else(|| {
                panic!(
                    "Git Trace2 start event at {}:{} has no argv array: {line}",
                    trace.display(),
                    index + 1
                )
            });
            if argv.iter().any(|argument| argument == "@{upstream}") {
                // Trace2 may omit cwd when the event uses Git's default repo;
                // retain every valid upstream start record, including raw argv.
                starts.push(line.to_owned());
            }
        }
        UpstreamLookupTrace { starts }
    }

    #[test]
    fn cleanup_plan_reuses_one_delivery_target_snapshot() {
        // `cleanup_plan_scanned_within` captures targets once, then passes
        // that snapshot through `cleanup_item_scanned_with_tips`. Trace2
        // catches a regression that resolves `@{upstream}` per worktree.
        if let Some(parent) = std::env::var_os(ROOT_ENV) {
            let parent = PathBuf::from(parent);
            let repository = initialized_repository(&parent);
            let storage = TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let represented_one = broker.start_worktree("represented one", None).unwrap();
            let represented_one_worktree = PathBuf::from(&represented_one.worktree_path);
            std::fs::write(
                represented_one_worktree.join("represented-one.txt"),
                "delivered\n",
            )
            .unwrap();
            git(&represented_one_worktree, &["add", "represented-one.txt"]);
            git(
                &represented_one_worktree,
                &["commit", "-qm", "first delivered commit"],
            );
            git(
                &repository,
                &["merge", "--ff-only", represented_one.branch.as_str()],
            );
            let one_worktree_main_head = git_text(&repository, &["rev-parse", "HEAD"]);
            broker.close(represented_one.id).unwrap();

            let trace = std::env::var_os("GIT_TRACE2_EVENT")
                .map(PathBuf::from)
                .expect("parent supplies a private Git trace path");
            std::fs::write(&trace, "").unwrap();
            let one_worktree_plan = broker.cleanup_plan_recorded().unwrap();
            assert!(
                one_worktree_plan
                    .target_snapshot
                    .contains(&format!("HEAD={one_worktree_main_head}"))
            );
            assert_eq!(one_worktree_plan.worktrees.len(), 1);
            assert_eq!(
                one_worktree_plan.worktrees[0].disposition,
                CleanupDisposition::Eligible,
                "the worktree represented on the captured primary target remains eligible"
            );
            let one_worktree_trace = upstream_lookup_trace(&trace);
            assert!(
                one_worktree_trace.count() > 0,
                "the trace observes the target-capture lookup; Trace2 starts:\n{}",
                one_worktree_trace.diagnostic()
            );

            let represented_two = broker.start_worktree("represented two", None).unwrap();
            let pending = broker.start_worktree("pending", None).unwrap();
            let represented_two_worktree = PathBuf::from(&represented_two.worktree_path);
            let pending_worktree = PathBuf::from(&pending.worktree_path);
            std::fs::write(
                represented_two_worktree.join("represented-two.txt"),
                "delivered\n",
            )
            .unwrap();
            git(&represented_two_worktree, &["add", "represented-two.txt"]);
            git(
                &represented_two_worktree,
                &["commit", "-qm", "second delivered commit"],
            );
            git(
                &repository,
                &["merge", "--ff-only", represented_two.branch.as_str()],
            );
            let main_head = git_text(&repository, &["rev-parse", "HEAD"]);
            std::fs::write(pending_worktree.join("pending.txt"), "not delivered\n").unwrap();
            git(&pending_worktree, &["add", "pending.txt"]);
            git(&pending_worktree, &["commit", "-qm", "session-only commit"]);
            let pending_head = git_text(&pending_worktree, &["rev-parse", "HEAD"]);
            for session in [&represented_two, &pending] {
                broker.close(session.id).unwrap();
            }

            std::fs::write(&trace, "").unwrap();
            let plan = broker.cleanup_plan_recorded().unwrap();

            assert!(
                plan.target_snapshot.contains(&format!("HEAD={main_head}")),
                "the digest records the primary checkout target"
            );
            assert!(
                !plan
                    .target_snapshot
                    .contains(&format!("HEAD={pending_head}")),
                "a session checkout head must not become the plan target"
            );
            assert_eq!(plan.worktrees.len(), 3);
            assert_eq!(
                plan.worktrees
                    .iter()
                    .filter(|item| item.disposition == CleanupDisposition::Eligible)
                    .count(),
                2,
                "the two worktrees represented by a plan target remain eligible"
            );
            assert_eq!(
                plan.worktrees
                    .iter()
                    .find(|item| item.session_id == pending.id)
                    .map(|item| item.disposition),
                Some(CleanupDisposition::PendingCommits),
                "the unique session commit stays pending against the primary targets"
            );
            let three_worktree_trace = upstream_lookup_trace(&trace);
            assert_eq!(
                three_worktree_trace.count(),
                one_worktree_trace.count(),
                "a cleanup plan's upstream lookup count must not grow when worktrees grow from one to three; one-worktree Trace2 starts:\n{}\nthree-worktree Trace2 starts:\n{}",
                one_worktree_trace.diagnostic(),
                three_worktree_trace.diagnostic()
            );
            return;
        }

        let parent = tempfile::tempdir().unwrap();
        let parent_path = parent.path().canonicalize().unwrap();
        let trace = parent_path.join("git-trace.json");
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", CHILD_TEST, "--nocapture"]);
        clear_git_environment(&mut child);
        let output = child
            .env(ROOT_ENV, &parent_path)
            .env("GIT_TRACE2_EVENT", &trace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_plan_fails_closed_after_transient_primary_head_failure() {
        if let Some(parent) = std::env::var_os(TRANSIENT_ROOT_ENV) {
            let parent = PathBuf::from(parent);
            let repository = initialized_repository(&parent);
            let expected_primary = std::env::var_os(FAIL_HEAD_ROOT_ENV)
                .map(PathBuf::from)
                .expect("parent supplies the canonical primary checkout path");
            assert_eq!(repository.canonicalize().unwrap(), expected_primary);

            let storage = TestStorage::new();
            let mut broker = storage.open(&repository).unwrap();
            let session = broker.start_worktree("delivered", None).unwrap();
            let worktree = PathBuf::from(&session.worktree_path);
            std::fs::write(worktree.join("delivered.txt"), "represented on main\n").unwrap();
            git(&worktree, &["add", "delivered.txt"]);
            git(&worktree, &["commit", "-qm", "commit for main"]);
            let session_head = git_text(&worktree, &["rev-parse", "HEAD"]);
            git(
                &repository,
                &["merge", "--ff-only", session.branch.as_str()],
            );
            let main_head = git_text(&repository, &["rev-parse", "HEAD"]);
            assert_eq!(main_head, session_head);
            broker.close(session.id).unwrap();

            let marker = PathBuf::from(
                std::env::var_os(FAIL_HEAD_MARKER_ENV)
                    .expect("parent supplies one-shot failure marker"),
            );
            std::fs::write(&marker, "fail primary HEAD once").unwrap();
            let plan = broker.cleanup_plan_recorded().unwrap();
            assert!(
                !marker.exists(),
                "the Git shim must consume the one-shot primary HEAD failure"
            );
            assert!(
                !plan.target_snapshot.contains(&format!("HEAD={main_head}")),
                "a later successful lookup must not silently change the plan's target digest"
            );
            let item = plan
                .worktrees
                .iter()
                .find(|item| item.session_id == session.id)
                .expect("the closed session remains visible in the plan");
            assert_eq!(
                item.disposition,
                CleanupDisposition::InspectionFailed,
                "an unbound target must not make a session eligible"
            );
            assert!(item.reason.contains("could not resolve primary HEAD"));

            let (disposition, _, _) = broker
                .cleanup_eligibility(session.id, &worktree)
                .expect("the fresh direct inspection can now resolve primary HEAD");
            assert_eq!(
                disposition,
                CleanupDisposition::Eligible,
                "the same session is eligible once a fresh target set is resolved"
            );
            return;
        }

        let real_git = match crate::git::git_output_trust() {
            crate::git::GitOutputTrust::Undecorated { resolved, .. } => resolved
                .canonicalize()
                .expect("trusted Git executable should have a canonical path"),
            other => panic!("transient-failure fixture requires trusted Git: {other:?}"),
        };
        let parent = tempfile::tempdir().unwrap();
        let parent_path = parent.path().canonicalize().unwrap();
        let shim_dir = parent_path.join("bin");
        std::fs::create_dir(&shim_dir).unwrap();
        let marker = parent_path.join("fail-first-primary-head");
        let primary = parent_path.join("repository");
        let wrapper = shim_dir.join("git");
        std::fs::write(
            &wrapper,
            r#"#!/bin/sh
if [ "$1" = "rev-parse" ] &&
   [ "$2" = "HEAD" ] &&
   [ "$(pwd -P)" = "$AETHYME_CLEANUP_TARGET_FAIL_HEAD_ROOT" ] &&
   [ -f "$AETHYME_CLEANUP_TARGET_FAIL_HEAD_MARKER" ]; then
  rm -f "$AETHYME_CLEANUP_TARGET_FAIL_HEAD_MARKER"
  echo "one-shot injected primary HEAD failure" >&2
  exit 128
fi
exec "$AETHYME_CLEANUP_TARGET_REAL_GIT" "$@"
"#,
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let original_path = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(shim_dir).chain(std::env::split_paths(&original_path)),
        )
        .unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", TRANSIENT_CHILD_TEST, "--nocapture"]);
        clear_git_environment(&mut child);
        let output = child
            .env(TRANSIENT_ROOT_ENV, &parent_path)
            .env(FAIL_HEAD_ROOT_ENV, &primary)
            .env(FAIL_HEAD_MARKER_ENV, &marker)
            .env(REAL_GIT_ENV, &real_git)
            .env("PATH", path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
