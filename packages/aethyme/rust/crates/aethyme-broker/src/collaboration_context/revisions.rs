//! Explicit immutable revision selection, sharing L3 ranking and budgets.
//!
//! This profile does not scan a mutable "latest" index. The caller names the
//! exact revisions that may be considered. An answer is advisory and is not
//! inserted into the legacy lineage cache.

use super::*;
use crate::collaboration_identity_store::{
    ContributionRevisionPin, IdentityStoreError, ResolvedContributionRevision,
    resolve_approved_project_binding_with_archive_use,
    resolve_bound_contribution_revisions_with_archive_use,
};
use aethyme_contracts::experimental_v0::ContributionId;

pub const REVISION_CONTEXT_PROFILE: &str = "aethyme.context.exact-revisions/experimental-v0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevisionPin {
    pub contribution_id: ContributionId,
    pub revision_record_id: RecordId,
    pub object_digest: ObjectDigest,
}

#[derive(Debug, thiserror::Error)]
pub enum RevisionContextError {
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error(transparent)]
    Identity(#[from] IdentityStoreError),
    #[error("the same immutable revision was selected with incompatible pins")]
    ConflictingPin,
    #[error("a selected immutable revision is unavailable to this reader")]
    RevisionUnavailable,
    #[error("reader visibility changed while exact revision context was read")]
    VisibilityChanged,
}

impl RevisionContextError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Context(error) => error.code(),
            Self::Identity(error) => error.code(),
            Self::ConflictingPin => "conflicting_revision_pin",
            Self::RevisionUnavailable => "revision_unavailable",
            Self::VisibilityChanged => "visibility_changed",
        }
    }
}

/// Resolve exact revisions under a consistent SQLite snapshot, then use the
/// existing L3 selector. Missing/corrupt selected revisions refuse the request;
/// a missing prerequisite is an explicit gap. No absence grants authority.
pub fn retrieve_revisions(
    store: &CollaborationStore,
    query: &ContextQuery,
    pins: &[RevisionPin],
    visibility: &dyn Visibility,
) -> Result<ContributionContext, RevisionContextError> {
    let archive_use = archive_read(store)?;
    let _response = crate::collaboration_sensitive::response_use(store)
        .map_err(ArchiveError::from)
        .map_err(ContextError::from)?;
    query.budget.check()?;
    if query.scope.is_empty() {
        return Err(ContextError::EmptyScope.into());
    }
    if pins.len() > MAX_CANDIDATES
        || query.scope.len() > MAX_SCOPE_PATHS
        || query
            .scope
            .iter()
            .any(|path| path.len() > MAX_SCOPE_PATH_BYTES)
        || query.analysis.len() > MAX_ANALYSIS_ENVELOPES
        || visibility.name().len() > MAX_SCOPE_PATH_BYTES
    {
        return Err(ContextError::RequestTooLarge.into());
    }
    let mut scope = query.scope.clone();
    scope.sort();
    scope.dedup();
    for path in &scope {
        validate_scope_path(path)?;
    }
    let mut ordered = BTreeMap::new();
    for pin in pins {
        if let Some(previous) = ordered.insert(pin.revision_record_id.clone(), pin)
            && previous != pin
        {
            return Err(RevisionContextError::ConflictingPin);
        }
    }
    let reader = visibility.name().to_string();
    let epoch = visibility.epoch();
    ensure_revocation_triggers(store.read_connection()).map_err(ContextError::from)?;
    let transaction = store
        .read_connection()
        .unchecked_transaction()
        .map_err(ContextError::from)?;
    let revocation = reader_revocation(&transaction, &reader).map_err(ContextError::from)?;
    let binding = resolve_approved_project_binding_with_archive_use(store, &archive_use)?
        .ok_or(IdentityStoreError::Unbound)?;
    let mut visible_pins = Vec::new();
    for pin in ordered.values() {
        // Apply the existing capture visibility boundary before loading data.
        // The resolver verifies the actual capture identity again below.
        let capture: Option<String> = transaction
            .query_row(
                "SELECT lineage_record_id FROM contribution_revisions WHERE revision_record_id=?1",
                [pin.revision_record_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(ContextError::from)?;
        // Missing and invisible pins have the same public refusal. A caller
        // cannot use exact selection to discover a hidden revision's existence.
        let capture = capture.ok_or(RevisionContextError::RevisionUnavailable)?;
        if !visibility.visible(&capture) {
            return Err(RevisionContextError::RevisionUnavailable);
        }
        visible_pins.push(ContributionRevisionPin {
            revision_record_id: &pin.revision_record_id,
            object_digest: pin.object_digest,
            contribution_id: &pin.contribution_id,
        });
    }
    // The selected set shares one bounded replay. Repeating the complete
    // history per pin would amplify one allowed request by MAX_CANDIDATES.
    let resolved: Vec<_> = resolve_bound_contribution_revisions_with_archive_use(
        store,
        &binding,
        &visible_pins,
        &archive_use,
    )?
    .into_values()
    .collect();
    if resolved
        .iter()
        .any(|revision| !visibility.visible(revision.capture_record_id().as_str()))
    {
        return Err(RevisionContextError::VisibilityChanged);
    }
    let mut analysis = Vec::new();
    let mut related_count = 0;
    let mut related_truncated = false;
    for envelope in &query.analysis {
        let (checked, _producer_status) =
            super::checked_analysis_envelope(store, envelope, query.source.as_ref());
        let scoped = envelope_is_scoped(store, &checked, &scope)?;
        let mut summary = AnalysisSummary::of(&checked, query.source.as_ref(), scoped);
        let remaining = MAX_RELATED_PATHS.saturating_sub(related_count);
        related_truncated |= summary.related.len() > remaining;
        summary.related.truncate(remaining);
        related_count += summary.related.len();
        analysis.push(summary);
    }
    let candidates: Vec<Candidate> = resolved
        .iter()
        .map(|resolved| {
            let changed: BTreeSet<Vec<u8>> = resolved
                .identity_transition()
                .changes()
                .iter()
                .flat_map(|change| [change.before.as_ref(), change.after.as_ref()])
                .flatten()
                .map(|state| state.path.clone())
                .collect();
            Candidate {
                id: resolved.record_id().as_str().to_string(),
                // Explicit immutable selection has no mutable arrival ordering.
                seq: 0,
                changed: changed.into_iter().collect(),
                brief: resolved.brief().map(|brief| CandidateBrief {
                    tokens: brief.token_count(),
                    scope_refs: brief
                        .decisions
                        .iter()
                        .map(|d| d.scope_ref.clone())
                        .collect(),
                }),
                visible: true,
            }
        })
        .collect();
    let selection = select(
        &SelectionQuery {
            scope: scope.clone(),
            has_source: query.source.is_some(),
            analysis,
            budget: query.budget,
            unreadable: Vec::new(),
            related_truncated,
            candidates_truncated: false,
        },
        &candidates,
    );
    let mut query_fields = vec![
        ("profile", text(REVISION_CONTEXT_PROFILE)),
        ("project", text(binding.project_id().as_str())),
        ("descriptor", text(binding.descriptor_record_id().as_str())),
        (
            "descriptor_object",
            text(&binding.descriptor_object().hex()),
        ),
        (
            "scope",
            Value::Array(scope.iter().map(|path| path_value(path)).collect()),
        ),
        (
            "analysis",
            Value::Array(
                query
                    .analysis
                    .iter()
                    .map(|e| text(e.to_record().1.as_str()))
                    .collect(),
            ),
        ),
        (
            "budget",
            object(vec![
                ("max_items", integer(query.budget.max_items)),
                ("max_brief_tokens", integer(query.budget.max_brief_tokens)),
                ("max_matched_paths", integer(query.budget.max_matched_paths)),
                ("max_bytes", integer(query.budget.max_bytes)),
            ]),
        ),
        (
            "pins",
            Value::Array(
                ordered
                    .values()
                    .map(|pin| {
                        object(vec![
                            ("contribution", text(pin.contribution_id.as_str())),
                            ("revision", text(pin.revision_record_id.as_str())),
                            ("object", text(&pin.object_digest.hex())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ];
    if let Some(source) = &query.source {
        query_fields.push(("source", text(source.as_str())));
    }
    let query_value = object(query_fields);
    let cache_inputs = object(vec![
        ("query", query_value.clone()),
        ("reader", text(&reader)),
        ("visibility_epoch", text(&epoch.to_string())),
        ("revocation_serial", Value::Integer(revocation.serial)),
        (
            "has_revocation_timestamp",
            Value::Bool(revocation.timestamp.is_some()),
        ),
        (
            "revocation_timestamp",
            text(revocation.timestamp.as_deref().unwrap_or("")),
        ),
        (
            "briefs",
            Value::Array(
                resolved
                    .iter()
                    .filter_map(|r| r.brief_digest())
                    .map(|digest| text(&digest.hex()))
                    .collect(),
            ),
        ),
    ]);
    let cache_key = format!(
        "sha256:{}",
        hex(&Sha256::digest(cache_inputs.to_canonical_bytes()))
    );
    let context = encode_context(
        query,
        visibility,
        &query_value,
        &cache_key,
        &resolved,
        selection,
    )?;
    transaction.commit().map_err(ContextError::from)?;
    if visibility.name() != reader
        || visibility.epoch() != epoch
        || reader_revocation(store.read_connection(), &reader).map_err(ContextError::from)?
            != revocation
        || resolved
            .iter()
            .any(|r| !visibility.visible(r.capture_record_id().as_str()))
    {
        return Err(RevisionContextError::VisibilityChanged);
    }
    Ok(context)
}

fn encode_context(
    query: &ContextQuery,
    visibility: &dyn Visibility,
    query_value: &Value,
    cache_key: &str,
    resolved: &[ResolvedContributionRevision],
    mut selection: Selection,
) -> Result<ContributionContext, ContextError> {
    let available: BTreeSet<_> = resolved.iter().map(|r| r.record_id().clone()).collect();
    loop {
        // Byte budgeting may remove items on the next iteration. Recompute
        // dependency gaps solely from the items in the response being encoded.
        selection.gaps.retain(|gap| {
            !gap.starts_with("missing_prerequisite:")
                && !gap.starts_with("prerequisite_not_returned:")
        });
        let returned: BTreeSet<RecordId> = selection
            .items
            .iter()
            .map(|item| resolved[item.candidate].record_id().clone())
            .collect();
        for item in &selection.items {
            for prerequisite in resolved[item.candidate].revision().depends_on() {
                if !available.contains(prerequisite) {
                    selection
                        .gaps
                        .insert(format!("missing_prerequisite:{prerequisite}"));
                }
                if !returned.contains(prerequisite) {
                    selection
                        .gaps
                        .insert(format!("prerequisite_not_returned:{prerequisite}"));
                }
            }
        }
        let items = selection
            .items
            .iter()
            .map(|selected| {
                let resolved = &resolved[selected.candidate];
                let revision = resolved.revision();
                let mut fields = vec![
                    ("rank", integer(selected.rank)),
                    ("contribution", text(revision.contribution_id().as_str())),
                    ("contribution_revision", text(resolved.record_id().as_str())),
                    ("revision_object", text(&resolved.object_digest().hex())),
                    ("capture", text(resolved.capture_record_id().as_str())),
                    ("base", text(revision.base().as_str())),
                    ("result", text(revision.result().as_str())),
                    ("acceptance", text("unknown")),
                    (
                        "depends_on",
                        Value::Array(
                            revision
                                .depends_on()
                                .iter()
                                .map(|id| text(id.as_str()))
                                .collect(),
                        ),
                    ),
                    (
                        "reasons",
                        Value::Array(
                            selected
                                .reasons
                                .iter()
                                .map(|reason| {
                                    object(vec![
                                        ("kind", text(reason.kind.as_str())),
                                        ("total", integer(reason.total)),
                                        (
                                            "values",
                                            Value::Array(
                                                reason
                                                    .values
                                                    .iter()
                                                    .map(|value| match value {
                                                        ReasonValue::Path(path) => path_value(path),
                                                        ReasonValue::ScopeRef(reference) => {
                                                            text(reference)
                                                        }
                                                        ReasonValue::Edge(path, edge) => {
                                                            object(vec![
                                                                ("path", path_value(path)),
                                                                ("edge", text(edge)),
                                                            ])
                                                        }
                                                    })
                                                    .collect(),
                                            ),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ];
                if let Some(source) = &query.source {
                    fields.push((
                        "applicability",
                        text(if source == revision.result() {
                            "in_source"
                        } else if source == revision.base() {
                            "same_base"
                        } else {
                            "other_base"
                        }),
                    ));
                }
                if let (Some(brief), Some(reference)) = (resolved.brief(), revision.brief()) {
                    if selected.brief_included == Some(true) {
                        let content = canonical_json::parse(
                            resolved
                                .brief_record_bytes()
                                .expect("validated exact Brief bytes"),
                        )
                        .expect("validated Brief encodes canonical JSON");
                        fields.push((
                            "brief",
                            object(vec![
                                ("record", text(reference.record_id.as_str())),
                                ("object", text(&reference.object_sha256)),
                                ("tokens", integer(brief.token_count())),
                                ("role", text(BRIEF_ROLE)),
                                ("content", content),
                            ]),
                        ));
                    } else {
                        fields.push((
                            "brief_omitted",
                            object(vec![
                                ("record", text(reference.record_id.as_str())),
                                ("reason", text("brief_tokens")),
                            ]),
                        ));
                    }
                }
                object(fields)
            })
            .collect();
        let mut fields = vec![
            ("schema", text(CONTEXT_SCHEMA_NAME)),
            ("query", query_value.clone()),
            ("authority", text(CONTEXT_AUTHORITY)),
            ("visibility", text(visibility.name())),
            ("coverage", text(selection.coverage())),
            ("limits", text(selection.limits())),
            (
                "gaps",
                Value::Array(selection.gaps.iter().map(|gap| text(gap)).collect()),
            ),
            (
                "limit_detail",
                object(vec![
                    ("matched", integer(selection.matched)),
                    ("returned", integer(selection.items.len())),
                    ("brief_tokens", integer(selection.brief_tokens)),
                    (
                        "truncated_by",
                        Value::Array(selection.truncated_by.iter().map(|cut| text(cut)).collect()),
                    ),
                ]),
            ),
            ("items", Value::Array(items)),
            (
                "cache",
                object(vec![
                    ("key", text(cache_key)),
                    ("stored", Value::Bool(false)),
                ]),
            ),
        ];
        if let Some(freshness) = selection.freshness() {
            fields.push(("freshness", text(freshness)));
        }
        let record = object(fields).to_canonical_bytes();
        if record.len() <= query.budget.max_bytes {
            let id = Record::decode(&record, &[&CONTEXT_SCHEMA])
                .expect("context matches its schema")
                .id();
            let contributions = selection
                .items
                .iter()
                .map(|selected| {
                    resolved[selected.candidate]
                        .record_id()
                        .as_str()
                        .to_string()
                })
                .collect();
            return Ok(ContributionContext {
                record,
                id,
                cache_key: cache_key.to_string(),
                selection,
                contributions,
                cacheable: false,
            });
        }
        let Some(dropped) = selection.items.pop() else {
            return Err(ContextError::BudgetTooSmall {
                needed: record.len(),
                max_bytes: query.budget.max_bytes,
            });
        };
        if dropped.brief_included == Some(true) {
            selection.brief_tokens -= resolved[dropped.candidate]
                .brief()
                .expect("included brief")
                .token_count();
        }
        selection.truncated_by.insert("bytes");
    }
}

/// Keep the legacy public brief command coherent after a capture has acquired
/// a D10 logical identity. A lineage shared by operations is not a selector.
pub(super) fn attach_bound_brief(
    store: &mut CollaborationStore,
    contribution: &RecordId,
    brief: &Brief,
    archive_use: &crate::collaboration_gc::ArchiveUse,
) -> Result<Option<RecordId>, ContextError> {
    if store.schema_version() < 26 {
        return Ok(None);
    }
    let operations: Vec<String> = store
        .read_connection()
        .prepare(
            "SELECT operation_id FROM local_logical_contributions WHERE lineage_record_id=?1
         ORDER BY operation_id LIMIT 2",
        )?
        .query_map([contribution.as_str()], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    if operations.is_empty() {
        return Ok(None);
    }
    if operations.len() != 1 {
        return Err(ContextError::RevisionIdentity(
            "capture lineage belongs to multiple logical contributions; use `aethyme collab brief attach --operation <id> <decision-file>`"
                .into(),
        ));
    }
    let operation = crate::collaboration_capture::OperationId::parse(&operations[0])
        .map_err(|error| ContextError::RevisionIdentity(error.to_string()))?;
    let binding = resolve_approved_project_binding_with_archive_use(store, archive_use)
        .map_err(ContextError::ImmutableRevision)?
        .ok_or_else(|| ContextError::RevisionIdentity("project binding is missing".into()))?;
    let resolved =
        crate::collaboration_identity_store::revise_contribution_for_operation_with_archive_use(
            store,
            &binding,
            crate::collaboration_identity_store::ContributionRevisionRequest {
                operation_id: &operation,
                file_operations: None,
                brief: Some(brief),
                depends_on: None,
            },
            archive_use,
        )
        .map_err(ContextError::ImmutableRevision)?;
    resolved
        .revision()
        .brief()
        .map(|reference| Some(reference.record_id.clone()))
        .ok_or_else(|| {
            ContextError::RevisionIdentity("committed revision has no requested brief".into())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_cli::project_test_support::BoundProjectFixture;
    use crate::collaboration_identity_store::{
        ContributionRevisionRequest, resolve_bound_contribution_revision,
        revise_contribution_for_operation,
    };
    use aethyme_contracts::experimental_v0::brief::Decision;

    fn brief(reason: &str) -> Brief {
        Brief {
            intent: "Keep the public interface stable".into(),
            decisions: vec![Decision {
                scope_ref: "a.txt".into(),
                choice: "preserve the source transition".into(),
                reason: reason.into(),
            }],
            ..Brief::default()
        }
    }

    fn pin(revision: &ResolvedContributionRevision) -> RevisionPin {
        RevisionPin {
            contribution_id: revision.revision().contribution_id().clone(),
            revision_record_id: revision.record_id().clone(),
            object_digest: revision.object_digest(),
        }
    }

    fn revise(
        fixture: &mut BoundProjectFixture,
        operation: &crate::collaboration_capture::OperationId,
        rationale: &Brief,
    ) -> ResolvedContributionRevision {
        revise_contribution_for_operation(
            &fixture.store,
            &fixture.binding,
            ContributionRevisionRequest {
                operation_id: operation,
                file_operations: None,
                brief: Some(rationale),
                depends_on: None,
            },
        )
        .unwrap()
    }

    fn first_revision(fixture: &mut BoundProjectFixture) -> ResolvedContributionRevision {
        let base = fixture.bootstrap_commit.clone();
        let result = fixture.commit_file("a.txt", b"first revision\n").unwrap();
        let receipt = fixture
            .capture_operation("context-first", &base, &result)
            .unwrap();
        revise(fixture, &receipt.operation_id, &brief("original rationale"))
    }

    fn query(revision: &ResolvedContributionRevision) -> ContextQuery {
        ContextQuery {
            scope: vec![b"a.txt".to_vec()],
            source: Some(revision.revision().base().clone()),
            analysis: Vec::new(),
            budget: Budget::default(),
        }
    }

    #[test]
    fn an_unresolved_profile_is_a_gap_and_its_paths_do_not_rank_revisions() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let revision = first_revision(&mut fixture);
        let view_options = crate::analysis_view::ViewOptions::new(
            crate::analysis_view::ViewProfile::current("fixture"),
        );
        let view = crate::analysis_view::view_for_snapshot(
            &mut fixture.store,
            revision.revision().base(),
            &view_options,
        )
        .unwrap();
        let mut analysis = view.find_references("a", 64).unwrap();
        analysis.provenance = None;
        analysis.result = Some(super::super::object(vec![(
            "callers",
            super::super::Value::Array(vec![super::super::text("a.txt")]),
        )]));

        let mut request = query(&revision);
        request.analysis.push(analysis);
        let result =
            retrieve_revisions(&fixture.store, &request, &[pin(&revision)], &LocalProject).unwrap();

        assert!(
            result
                .selection
                .gaps
                .iter()
                .any(|gap| gap == "analysis_unavailable")
        );
        assert!(result.selection.items.iter().all(|item| {
            item.reasons
                .iter()
                .all(|reason| reason.kind != ReasonKind::ImpactEdge)
        }));
    }

    #[test]
    fn exact_context_keeps_old_rationale_after_new_revision_and_legacy_attach() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let old = first_revision(&mut fixture);
        let old_pin = pin(&old);
        let request = query(&old);
        let before = retrieve_revisions(
            &fixture.store,
            &request,
            std::slice::from_ref(&old_pin),
            &LocalProject,
        )
        .unwrap();
        assert_eq!(before.selection.items.len(), 1);
        let updated = brief("new rationale with the same source bytes");
        let new_brief =
            attach_brief(&mut fixture.store, old.capture_record_id(), &updated).unwrap();
        assert_ne!(
            Some(new_brief.clone()),
            old.revision().brief().map(|r| r.record_id.clone())
        );
        let head: String = fixture.store.read_connection().query_row(
            "SELECT revision_record_id FROM contribution_revision_heads WHERE contribution_id=?1",
            [old.revision().contribution_id().as_str()], |row| row.get(0),
        ).unwrap();
        assert_ne!(head, old.record_id().as_str());
        let old_again =
            retrieve_revisions(&fixture.store, &request, &[old_pin], &LocalProject).unwrap();
        assert_eq!(before.record, old_again.record);
        assert_eq!(before.cache_key, old_again.cache_key);
        assert!(
            String::from_utf8(old_again.record)
                .unwrap()
                .contains("original rationale")
        );
        let alias: String = fixture
            .store
            .read_connection()
            .query_row(
                "SELECT brief_record_id FROM contribution_briefs WHERE lineage_record_id=?1",
                [old.capture_record_id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias, new_brief.as_str());
        let operation =
            crate::collaboration_capture::OperationId::parse(old.capture_operation_id().unwrap())
                .unwrap();
        let historical_retry = revise(&mut fixture, &operation, old.brief().unwrap());
        assert_eq!(historical_retry.record_id(), old.record_id());
        let head_after_retry: String = fixture.store.read_connection().query_row(
            "SELECT revision_record_id FROM contribution_revision_heads WHERE contribution_id=?1",
            [old.revision().contribution_id().as_str()], |row| row.get(0),
        ).unwrap();
        assert_eq!(
            head_after_retry, head,
            "retrying X after Y must not rewind the head"
        );
        let alias_after_retry: String = fixture
            .store
            .read_connection()
            .query_row(
                "SELECT brief_record_id FROM contribution_briefs WHERE lineage_record_id=?1",
                [old.capture_record_id().as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias_after_retry, new_brief.as_str());
        let digest: String = fixture
            .store
            .read_connection()
            .query_row(
                "SELECT record_sha256 FROM contribution_revisions WHERE revision_record_id=?1",
                [&head],
                |row| row.get(0),
            )
            .unwrap();
        let new_pin = RevisionPin {
            contribution_id: old.revision().contribution_id().clone(),
            revision_record_id: RecordId::parse(&head).unwrap(),
            object_digest: hex_digest(&digest).unwrap(),
        };
        let newest =
            retrieve_revisions(&fixture.store, &request, &[new_pin], &LocalProject).unwrap();
        assert_ne!(newest.cache_key, before.cache_key);
        assert!(
            String::from_utf8(newest.record)
                .unwrap()
                .contains("new rationale with the same source bytes")
        );
    }

    #[test]
    fn exact_context_zero_brief_budget_and_permuted_pins_are_deterministic() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let old = first_revision(&mut fixture);
        let operation =
            crate::collaboration_capture::OperationId::parse(old.capture_operation_id().unwrap())
                .unwrap();
        let new = revise(
            &mut fixture,
            &operation,
            &brief("second immutable rationale"),
        );
        let mut request = query(&old);
        request.budget.max_brief_tokens = 0;
        let forward = retrieve_revisions(
            &fixture.store,
            &request,
            &[pin(&old), pin(&new)],
            &LocalProject,
        )
        .unwrap();
        let reverse = retrieve_revisions(
            &fixture.store,
            &request,
            &[pin(&new), pin(&old)],
            &LocalProject,
        )
        .unwrap();
        assert_eq!(forward.record, reverse.record);
        assert_eq!(forward.selection.items.len(), 2);
        assert_eq!(forward.selection.brief_tokens, 0);
        assert!(
            forward
                .selection
                .items
                .iter()
                .all(|item| item.brief_included == Some(false))
        );
        let record = String::from_utf8(forward.record).unwrap();
        assert!(!record.contains("original rationale"));
        assert!(!record.contains("second immutable rationale"));
    }

    #[test]
    fn exact_context_refuses_conflicting_pin_and_changed_visibility_epoch() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let revision = first_revision(&mut fixture);
        let mut forged = pin(&revision);
        forged.object_digest = ObjectDigest::from_bytes([0; 32]);
        assert!(matches!(
            retrieve_revisions(
                &fixture.store,
                &query(&revision),
                &[pin(&revision), forged],
                &LocalProject
            ),
            Err(RevisionContextError::ConflictingPin)
        ));
        struct ChangingEpoch(std::cell::Cell<u64>);
        impl Visibility for ChangingEpoch {
            fn name(&self) -> &str {
                "changing-reader"
            }
            fn epoch(&self) -> u64 {
                let epoch = self.0.get();
                self.0.set(epoch + 1);
                epoch
            }
            fn visible(&self, _: &str) -> bool {
                true
            }
        }
        assert!(matches!(
            retrieve_revisions(
                &fixture.store,
                &query(&revision),
                &[pin(&revision)],
                &ChangingEpoch(std::cell::Cell::new(0))
            ),
            Err(RevisionContextError::VisibilityChanged)
        ));
    }

    #[test]
    fn exact_context_hidden_and_absent_revisions_have_the_same_public_refusal() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let revision = first_revision(&mut fixture);
        struct Hidden;
        impl Visibility for Hidden {
            fn name(&self) -> &str {
                "hidden-reader"
            }
            fn epoch(&self) -> u64 {
                0
            }
            fn visible(&self, _: &str) -> bool {
                false
            }
        }
        let hidden = retrieve_revisions(
            &fixture.store,
            &query(&revision),
            &[pin(&revision)],
            &Hidden,
        )
        .unwrap_err();
        let mut absent = pin(&revision);
        absent.revision_record_id = RecordId::parse(&format!("sha256:{}", "0".repeat(64))).unwrap();
        let unknown =
            retrieve_revisions(&fixture.store, &query(&revision), &[absent], &Hidden).unwrap_err();
        assert!(matches!(hidden, RevisionContextError::RevisionUnavailable));
        assert!(matches!(unknown, RevisionContextError::RevisionUnavailable));
        assert_eq!(hidden.code(), unknown.code());
        assert_eq!(hidden.to_string(), unknown.to_string());
    }

    #[test]
    fn exact_context_dependency_gaps_follow_selected_items_and_byte_budget() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let prerequisite = first_revision(&mut fixture);
        let operation = crate::collaboration_capture::OperationId::parse(
            prerequisite.capture_operation_id().unwrap(),
        )
        .unwrap();
        let selected_a = revise(
            &mut fixture,
            &operation,
            &brief("later independent rationale"),
        );
        // Capture B from the exact prior result, so only B changes. It cannot
        // match an A query through paths or a Brief's scope references.
        let result_a =
            crate::collaboration_archive::pin_commit(fixture.repo_path(), "HEAD").unwrap();
        let result_b = fixture.commit_file("b.txt", b"unrelated path\n").unwrap();
        let receipt = fixture
            .capture_operation("context-dependent", &result_a, &result_b)
            .unwrap();
        let dependencies = [prerequisite.record_id().clone()];
        let dependent = revise_contribution_for_operation(
            &mut fixture.store,
            &fixture.binding,
            ContributionRevisionRequest {
                operation_id: &receipt.operation_id,
                file_operations: None,
                brief: None,
                depends_on: Some(&dependencies),
            },
        )
        .unwrap();
        let request = query(&selected_a);
        let unrelated = retrieve_revisions(
            &fixture.store,
            &request,
            &[pin(&selected_a), pin(&dependent)],
            &LocalProject,
        )
        .unwrap();
        assert_eq!(unrelated.selection.items.len(), 1);
        assert!(
            !unrelated
                .selection
                .gaps
                .iter()
                .any(|gap| gap.starts_with("missing_prerequisite:")
                    || gap.starts_with("prerequisite_not_returned:"))
        );
        let mut request_b = query(&dependent);
        request_b.scope = vec![b"b.txt".to_vec()];
        let selected = retrieve_revisions(
            &fixture.store,
            &request_b,
            &[pin(&dependent)],
            &LocalProject,
        )
        .unwrap();
        assert!(selected.selection.gaps.contains(&format!(
            "missing_prerequisite:{}",
            prerequisite.record_id()
        )));
        assert!(selected.selection.gaps.contains(&format!(
            "prerequisite_not_returned:{}",
            prerequisite.record_id()
        )));
        // Exercise the encoder's byte-budget retry directly with the already
        // resolver-validated revision and selection from the actual store.
        let query_value = object(vec![("scope", Value::Array(vec![path_value(b"b.txt")]))]);
        let mut empty_selection = selected.selection.clone();
        empty_selection.items.clear();
        empty_selection.brief_tokens = 0;
        empty_selection.truncated_by.insert("bytes");
        let empty = encode_context(
            &request_b,
            &LocalProject,
            &query_value,
            "bounded",
            std::slice::from_ref(&dependent),
            empty_selection,
        )
        .unwrap();
        request_b.budget.max_bytes = empty.record.len();
        let bounded = encode_context(
            &request_b,
            &LocalProject,
            &query_value,
            "bounded",
            &[dependent],
            selected.selection,
        )
        .unwrap();
        assert!(bounded.selection.items.is_empty());
        assert!(
            !bounded
                .selection
                .gaps
                .iter()
                .any(|gap| gap.starts_with("missing_prerequisite:")
                    || gap.starts_with("prerequisite_not_returned:"))
        );
        assert_eq!(bounded.record, empty.record);
    }

    fn release_captures(store: &mut CollaborationStore) {
        let operations: Vec<String> = store.read_connection().prepare(
            "SELECT operation_id FROM retention_roots WHERE kind='contribution' AND released_ms IS NULL"
        ).unwrap().query_map([], |row| row.get(0)).unwrap().collect::<Result<_,_>>().unwrap();
        for operation in operations {
            assert!(crate::collaboration_gc::release_retention(store, &operation).unwrap());
        }
        let active: i64 = store
            .read_connection()
            .query_row(
                "SELECT count(*) FROM retention_roots WHERE released_ms IS NULL",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            active, 0,
            "proof retention must not be masked by capture roots"
        );
    }

    fn age_own_archive(store: &CollaborationStore) {
        let before_grace =
            std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 24 * 60 * 60);
        for directory in std::fs::read_dir(store.project_dir().join("objects/sha256")).unwrap() {
            for file in std::fs::read_dir(directory.unwrap().path()).unwrap() {
                let path = file.unwrap().path();
                assert!(std::fs::symlink_metadata(&path).unwrap().is_file());
                std::fs::File::options()
                    .write(true)
                    .open(path)
                    .unwrap()
                    .set_times(std::fs::FileTimes::new().set_modified(before_grace))
                    .unwrap();
            }
        }
    }

    fn collect(store: &mut CollaborationStore) -> crate::collaboration_gc::GcReport {
        let options = crate::collaboration_gc::GcOptions::default();
        let plan = crate::collaboration_gc::plan(store, &options).unwrap();
        assert!(plan.blockers.is_empty(), "{:?}", plan.blockers);
        crate::collaboration_gc::apply(store, &plan.digest, &options).unwrap()
    }

    #[test]
    fn exact_pin_reads_unrooted_history_during_grace_without_creating_retention() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let first = first_revision(&mut fixture);
        let first_result_commit =
            crate::collaboration_archive::pin_commit(fixture.repo.path(), "HEAD").unwrap();
        let second_commit = fixture
            .commit_file("a.txt", b"unrooted descendant source transition\n")
            .unwrap();
        let receipt = fixture
            .capture_operation(
                "context-grace-descendant",
                &first_result_commit,
                &second_commit,
            )
            .unwrap();
        let second = revise(
            &mut fixture,
            &receipt.operation_id,
            &brief("rationale retained only by the archive grace period"),
        );
        release_captures(&mut fixture.store);
        let root_counts = |store: &CollaborationStore| -> (i64, i64, i64) {
            store.read_connection().query_row(
                "SELECT (SELECT count(*) FROM object_pins), (SELECT count(*) FROM reader_leases), (SELECT count(*) FROM retention_roots WHERE released_ms IS NULL)",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap()
        };
        let before = root_counts(&fixture.store);
        assert_eq!(before.2, 0);
        let retrieved = retrieve_revisions(
            &fixture.store,
            &query(&second),
            &[pin(&second)],
            &LocalProject,
        )
        .expect("an exact pin seeds its retained proof closure during grace");
        assert_eq!(retrieved.selection.items.len(), 1);
        assert!(
            String::from_utf8(retrieved.record)
                .unwrap()
                .contains("rationale retained only by the archive grace period")
        );
        assert_eq!(
            root_counts(&fixture.store),
            before,
            "a read must not grant durable retention"
        );
        age_own_archive(&fixture.store);
        collect(&mut fixture.store);
        assert!(
            !crate::collaboration_archive::object_path(&fixture.store, &first.object_digest())
                .exists()
        );
        assert!(
            retrieve_revisions(
                &fixture.store,
                &query(&second),
                &[pin(&second)],
                &LocalProject
            )
            .is_err(),
            "retired proof must remain unavailable"
        );
    }

    #[test]
    fn exact_revision_pin_keeps_ancestor_proofs_then_gc_reclaims_unrooted_history() {
        let mut fixture = BoundProjectFixture::new().unwrap();
        let first = first_revision(&mut fixture);
        let first_result_commit =
            crate::collaboration_archive::pin_commit(fixture.repo.path(), "HEAD").unwrap();
        let second_commit = fixture
            .commit_file("a.txt", b"second source transition\n")
            .unwrap();
        let receipt = fixture
            .capture_operation("context-second", &first_result_commit, &second_commit)
            .unwrap();
        let second = revise(
            &mut fixture,
            &receipt.operation_id,
            &brief("descendant rationale"),
        );
        let dependency: i64 = fixture.store.read_connection().query_row(
            "SELECT count(*) FROM contribution_revision_identity_bases WHERE revision_record_id=?1 AND base_revision_record_id=?2",
            [second.record_id().as_str(), first.record_id().as_str()], |row| row.get(0),
        ).unwrap();
        assert_eq!(dependency, 1);
        let root = crate::collaboration_gc::pin_object(
            &mut fixture.store,
            crate::collaboration_gc::PinClass::CitedEvidence,
            &second.object_digest(),
            "exact-proof-root",
            None,
        )
        .unwrap();
        release_captures(&mut fixture.store);
        age_own_archive(&fixture.store);
        collect(&mut fixture.store);
        for digest in [
            first.object_digest(),
            first.brief_object_digest().unwrap(),
            second.object_digest(),
            second.brief_object_digest().unwrap(),
        ] {
            assert!(
                crate::collaboration_archive::object_path(&fixture.store, &digest).is_file(),
                "live exact proof {}",
                digest.hex()
            );
        }
        resolve_bound_contribution_revision(
            &fixture.store,
            &fixture.binding,
            second.record_id(),
            second.object_digest(),
            second.revision().contribution_id(),
        )
        .unwrap();
        assert!(crate::collaboration_gc::release_pin(&mut fixture.store, root).unwrap());
        age_own_archive(&fixture.store);
        let report = collect(&mut fixture.store);
        assert!(!report.reclaimed.is_empty());
        for digest in [
            first.object_digest(),
            first.brief_object_digest().unwrap(),
            second.object_digest(),
            second.brief_object_digest().unwrap(),
        ] {
            assert!(
                !crate::collaboration_archive::object_path(&fixture.store, &digest).exists(),
                "unrooted history {} must be reclaimed",
                digest.hex()
            );
        }
        assert!(
            crate::collaboration_archive::object_path(
                &fixture.store,
                &fixture.binding.descriptor_object()
            )
            .is_file()
        );
        assert!(
            crate::collaboration_archive::object_path(
                &fixture.store,
                &ObjectDigest::of_snapshot(&fixture.bootstrap_snapshot)
            )
            .is_file()
        );
        let binding =
            crate::collaboration_identity_store::resolve_approved_project_binding(&fixture.store)
                .unwrap()
                .unwrap();
        assert_eq!(
            binding.descriptor_record_id(),
            fixture.binding.descriptor_record_id()
        );
        assert!(
            resolve_bound_contribution_revision(
                &fixture.store,
                &binding,
                second.record_id(),
                second.object_digest(),
                second.revision().contribution_id()
            )
            .is_err()
        );
        // A new operation may capture the identical source lineage after GC.
        // It must not revive old immutable records or their retired Brief alias.
        let bootstrap = fixture.bootstrap_commit.clone();
        let recaptured = fixture
            .capture_operation("context-recapture", &bootstrap, &first_result_commit)
            .unwrap();
        assert_eq!(&recaptured.contribution, first.capture_record_id());
        let fresh = revise(
            &mut fixture,
            &recaptured.operation_id,
            &brief("new logical operation after reclamation"),
        );
        assert_ne!(
            fresh.revision().contribution_id(),
            first.revision().contribution_id()
        );
        assert_ne!(fresh.record_id(), first.record_id());
        let plan = crate::collaboration_gc::plan(
            &mut fixture.store,
            &crate::collaboration_gc::GcOptions::default(),
        )
        .unwrap();
        assert!(
            plan.blockers.is_empty(),
            "retired roots must not reappear: {:?}",
            plan.blockers
        );
        resolve_bound_contribution_revision(
            &fixture.store,
            &fixture.binding,
            fresh.record_id(),
            fresh.object_digest(),
            fresh.revision().contribution_id(),
        )
        .unwrap();
    }
}
