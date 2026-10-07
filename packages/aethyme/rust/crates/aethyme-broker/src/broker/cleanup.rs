use super::*;

impl Broker {
    /// Whether a directory is the remains of a worktree of *this* repository
    /// whose registration is already gone.
    ///
    /// The `.git` file survives the interrupted removal pointing at
    /// `<common dir>/worktrees/<name>`, and that pointer is the evidence. It is
    /// checked rather than trusted for containment -- the caller has already
    /// proven the path sits under the broker-owned root, so this only has to
    /// establish which repository the remains belong to, not that they are safe
    /// to touch.
    pub(super) fn is_orphaned_worktree_of_this_repository(&self, path: &Path) -> bool {
        let Ok(expected) = self.main_git_common_dir() else {
            return false;
        };
        let Ok(pointer) = std::fs::read_to_string(path.join(".git")) else {
            return false;
        };
        let Some(gitdir) = pointer.trim().strip_prefix("gitdir:") else {
            return false;
        };
        let gitdir = PathBuf::from(gitdir.trim());
        // The gitdir itself is gone -- that is the definition of this state --
        // so compare the `worktrees/<name>` parent it named, not the leaf.
        let Some(worktrees_dir) = gitdir.parent() else {
            return false;
        };
        if worktrees_dir.file_name() != Some(std::ffi::OsStr::new("worktrees")) {
            return false;
        }
        let Some(claimed_common_dir) = worktrees_dir.parent() else {
            return false;
        };
        let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        canonical(claimed_common_dir) == canonical(&expected)
    }

    pub(super) fn worktree_root_marker_matches(&self, root: &Path) -> bool {
        let Ok(bytes) = std::fs::read(root.join(WORKTREE_ROOT_MARKER)) else {
            return false;
        };
        let Ok(marker) = serde_json::from_slice::<WorktreeRootMarker>(&bytes) else {
            return false;
        };
        marker.schema_version == WORKTREE_ROOT_SCHEMA_VERSION
            && self
                .repository_worktree_key()
                .is_ok_and(|key| marker.repository_key == key)
            && marker.repository_root == self.main_root
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
        if checkout.is_dirty()? {
            return Ok((
                CleanupDisposition::Dirty,
                "worktree has uncommitted or untracked changes".into(),
                None,
            ));
        }

        let session_head = checkout.head_commit()?;
        let session = self.store.session(session_id)?;
        let delivery_targets = self.cleanup_delivery_targets()?;
        let (provenance, reason) =
            self.cleanup_provenance(&session, &session_head, &delivery_targets)?;
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
            if let Some((target, evidence, landed_by)) =
                self.landing_on_delivery_targets(session_head, delivery_targets)?
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
            } = crate::representation::work_landed_within(
                &self.repo,
                head,
                target,
                self.landing_deadline.get(),
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
        let entry = self
            .store
            .merge_queue()?
            .into_iter()
            .find(|entry| entry.id == queue_entry_id);
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
    pub(super) fn branch_provenance_disposition(
        &self,
        session: &Session,
        session_head: &str,
    ) -> Result<(CleanupDisposition, String, Option<CleanupProvenance>), BrokerOpError> {
        let mut delivery_targets = vec![self.repo.head_commit()?];
        if let Some(integration) = self.integration_tip() {
            delivery_targets.push(integration);
        }
        if let Some((_upstream, head)) = self.repo.tracking_upstream() {
            delivery_targets.push(head);
        }
        delivery_targets.sort();
        delivery_targets.dedup();
        Ok(
            match self.cleanup_provenance(session, session_head, &delivery_targets) {
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
        self.cleanup_item_scanned_with_tips(session, scan, records, None)
    }

    /// [`Self::cleanup_item_scanned`] with every local branch tip already read
    /// (see [`GitRepo::local_branch_tips`]). A plan visits every session ever
    /// recorded, most of whose branch and worktree are long gone; looking the
    /// branch up in one listing lets those return without forking `git`.
    pub(super) fn cleanup_item_scanned_with_tips(
        &self,
        session: &Session,
        scan: crate::SizeScan,
        records: &mut crate::measurement::SizeRecords,
        branch_tips: Option<&std::collections::BTreeMap<String, String>>,
    ) -> Result<Option<CleanupWorktreePlan>, BrokerOpError> {
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
        let (estimated_bytes, estimated_inodes) = if !worktree_present {
            // Nothing on disk is a measured zero, not an unknown.
            (Some(0), Some(0))
        } else if scan.measures() {
            let measured =
                crate::disk_headroom::directory_usage_without_following_links(&worktree_path).ok();
            if let Some(usage) = measured {
                records.record_usage(
                    &session.worktree_path,
                    usage.bytes,
                    Some(usage.inodes),
                    now_ms(),
                );
            }
            measured
                .map(|usage| (Some(usage.bytes), Some(usage.inodes)))
                .unwrap_or((None, None))
        } else {
            records
                .get(&session.worktree_path)
                .map(|record| (Some(record.bytes), record.inodes))
                .unwrap_or((None, None))
        };
        let unreachable = (!worktree_present)
            .then(|| {
                crate::worktree_location::unavailable_configured_location(
                    &self.main_root,
                    &worktree_path,
                )
            })
            .flatten();
        let (mut disposition, mut reason, provenance) = if let Some(reason) = unreachable {
            // An unplugged drive is not a deleted worktree. Proposing cleanup
            // here would act on a checkout nobody can see, so it stays blocked
            // until the volume is back.
            (CleanupDisposition::InspectionFailed, reason, None)
        } else if !self.is_broker_owned_worktree(session, &worktree_path) {
            (
                CleanupDisposition::UnsafePath,
                "spawned session path is outside the broker-owned worktree directory".into(),
                None,
            )
        } else if worktree_present && !is_orphaned_worktree_directory(&worktree_path) {
            match self.cleanup_eligibility(session.id, &worktree_path) {
                Ok(result) => result,
                Err(error) => (
                    CleanupDisposition::InspectionFailed,
                    self.cleanup_inspection_failure(&error),
                    None,
                ),
            }
        } else if let Some(session_head) = branch_tip.as_deref() {
            // Two states share this path. A worktree that is simply gone, and
            // one whose removal was interrupted after deregistration (#165):
            // its files are on disk but git can no longer read them, so the
            // branch is the only remaining record of what the session did. It
            // is also the better record -- the interrupted removal already
            // deleted part of the tree, so the directory describes nothing.
            self.branch_provenance_disposition(session, session_head)?
        } else if worktree_present {
            (
                CleanupDisposition::UnprovenProvenance,
                "worktree directory is no longer a registered git worktree and its branch is gone, \
                 so nothing records what it held"
                    .into(),
                None,
            )
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

    pub(super) fn cleanup_plan_scanned_within(
        &self,
        scan: crate::SizeScan,
        budget: Option<std::time::Duration>,
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
        let mut plan = CleanupPlan {
            target_snapshot: self.cleanup_target_snapshot(),
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
                    branch_tips.as_ref(),
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
        let paths = plan
            .worktrees
            .iter()
            .filter(|item| item.worktree_present)
            .map(|item| item.worktree_path.clone())
            .collect::<Vec<_>>();
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
        let (plan, deferred) = if audit {
            (self.cleanup_plan_recorded()?, 0)
        } else {
            self.cleanup_plan_observed()?
        };
        let closed_sessions = self.store.cleaned_sessions()?;
        let (worktree_inodes, worktree_inode_unmeasured) =
            self.recorded_worktree_inode_summary(&closed_sessions)?;
        let closed_worktrees = crate::retention::ClosedWorktreeSummary::from_cleanup(
            &plan,
            &closed_sessions,
            &self.main_root,
        );
        let oldest_closed_at = closed_sessions
            .iter()
            .filter(|session| {
                let path = Path::new(&session.worktree_path);
                path.exists() && self.is_broker_owned_worktree(session, path)
            })
            .filter_map(|session| session.closed_at)
            .min();
        let oldest_closed_age_days = oldest_closed_at
            .map(|closed_at| now_ms.saturating_sub(closed_at).max(0) as u64 / 86_400_000)
            .unwrap_or(0);
        // One reading, consumed by both the severity below and the evidence
        // line in the advice that reports it.
        let gate_headroom = self.gate_headroom();
        let (host_volume_probe, host_available_bytes) = gate_headroom
            .bytes
            .map(|probe| (Some(probe.path), Some(probe.available)))
            .unwrap_or((None, None));
        let (host_inode_volume_probe, inodes_free) = gate_headroom
            .inodes
            .map(|probe| (Some(probe.path), Some(probe.available)))
            .unwrap_or((None, None));
        let severity = cleanup_retention_severity(
            plan.retained_worktree_count,
            plan.estimated_retained_bytes,
            oldest_closed_age_days,
            policy.closed_worktrees_days,
            policy.retained_bytes_budget,
        );
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
        retained_total.unmeasured = retained_total.unmeasured.saturating_add(deferred);
        let budget_verdict = crate::budget_verdict(&retained_total, policy.retained_bytes_budget);
        // Only `Over` asserts that the budget is broken. A floor under the
        // budget is not a pass -- the bytes it skipped are exactly the ones
        // that would have decided it.
        let over_retained_bytes_budget = budget_verdict.exceeded();
        Ok(CleanupRetention {
            inventory_complete: deferred == 0,
            inventory_deferred_sessions: deferred,
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
            clears_retained_bytes_budget: crate::reclaim_order::clears_budget(
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
            reconciliation: self.reconcile_worktree_directories(false)?,
            closed_worktrees,
        })
    }

    /// Free space where this repository's gates run, and the directory read.
    ///
    /// A gate refuses on the free space at its own checkout, and no gate runs
    /// beside the repository: a session gate runs in the session worktree,
    /// under the broker worktree root, and a merge verification slot is placed
    /// in host state beside it. Reading the repository's volume instead says
    /// nothing about either when the repository sits on another disk -- one
    /// checked out on an external drive reported that drive's space while
    /// every gate refused on a full startup disk.
    ///
    /// Both locations are read and the lower wins, because either kind of
    /// gate refusing is enough to stop work. The repository is the fallback
    /// only when neither location is known (an ephemeral repository with no
    /// host state). Each location is read at its nearest existing directory:
    /// a worktree root does not exist before the first session, and a missing
    /// path would read as unknown, which never escalates.
    pub(super) fn gate_headroom_probes(&self) -> Vec<PathBuf> {
        let worktree_root = self
            .worktree_root_plan()
            .ok()
            .and_then(|plan| plan.preferred_root);
        let host_state = crate::host_state::default_host_state_dir();
        let probes: Vec<PathBuf> = worktree_root.into_iter().chain(host_state).collect();
        if probes.is_empty() {
            vec![self.main_root.clone()]
        } else {
            probes
        }
    }

    pub(super) fn gate_headroom(&self) -> GateHeadroom {
        lowest_headroom_with(&self.gate_headroom_probes(), |probe| {
            crate::disk_headroom::available_headroom_at_or_above_for(&self.main_root, probe)
        })
    }

    pub(super) fn recorded_worktree_inode_summary(
        &self,
        closed_sessions: &[Session],
    ) -> Result<(u64, usize), BrokerOpError> {
        let mut paths = std::collections::BTreeSet::new();
        for session in self
            .store
            .live_sessions()?
            .iter()
            .chain(closed_sessions.iter())
        {
            if session.origin != SessionOrigin::Spawned {
                continue;
            }
            let path = Path::new(&session.worktree_path);
            if is_real_directory(path) && self.is_broker_owned_worktree(session, path) {
                paths.insert(session.worktree_path.clone());
            }
        }

        let records = crate::measurement::load_size_records(&self.main_root);
        let mut known = 0_u64;
        let mut unmeasured = 0_usize;
        for path in paths {
            match records.get(&path).and_then(|record| record.inodes) {
                Some(inodes) => known = known.saturating_add(inodes),
                None => unmeasured = unmeasured.saturating_add(1),
            }
        }
        Ok((known, unmeasured))
    }

    /// Remove a session's worktree and mark it cleaned. Refuses when the
    /// worktree has uncommitted changes or commits not represented on a
    /// delivery target, unless `force`.
    pub fn cleanup(&mut self, session_id: i64, force: bool) -> Result<(), BrokerOpError> {
        let session = self.store.session(session_id)?;
        let worktree_path = PathBuf::from(&session.worktree_path);
        if session.origin == SessionOrigin::Adopted {
            if worktree_path.exists() {
                if !force {
                    let (disposition, reason, _) =
                        self.cleanup_eligibility(session_id, &worktree_path)?;
                    if disposition != CleanupDisposition::Eligible {
                        return Err(BrokerOpError::DirtyWorktree {
                            id: session_id,
                            reason,
                        });
                    }
                }
                self.refuse_live_checkout(session_id, &worktree_path)?;
                self.repo.worktree_remove(&worktree_path, force)?;
            }
            self.store
                .set_session_status(session_id, SessionStatus::Cleaned, None)?;
            return Ok(());
        }
        // Recorded sizes, not a walk: removal needs the provenance verdict,
        // not the byte count, and a walk here only widens the window between
        // the live-checkout check below and the removal.
        let mut records = crate::measurement::SizeRecords::default();
        let item = self.cleanup_item_scanned(&session, crate::SizeScan::Recorded, &mut records)?;
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

        if worktree_path.exists() {
            self.refuse_live_checkout(session_id, &worktree_path)?;
            self.repo.worktree_remove(&worktree_path, force)?;
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
