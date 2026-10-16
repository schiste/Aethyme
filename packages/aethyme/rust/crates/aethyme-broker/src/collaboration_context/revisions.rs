//! Explicit immutable revision selection, sharing L3 ranking and budgets.
//!
//! This profile does not scan a mutable "latest" index. The caller names the
//! exact revisions that may be considered. An answer is advisory and is not
//! inserted into the legacy lineage cache.

use super::*;
use crate::collaboration_identity_store::{
    IdentityStoreError, ResolvedContributionRevision, resolve_approved_project_binding,
    resolve_bound_contribution_revision,
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
    #[error("reader visibility changed while exact revision context was read")]
    VisibilityChanged,
}

impl RevisionContextError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Context(error) => error.code(),
            Self::Identity(_) => "revision_identity",
            Self::ConflictingPin => "conflicting_revision_pin",
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
    let _archive = archive_read(store)?;
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
    let binding = resolve_approved_project_binding(store)?.ok_or(IdentityStoreError::Unbound)?;
    let mut resolved = Vec::new();
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
        let capture = capture.ok_or_else(|| {
            IdentityStoreError::UnknownRevision(pin.revision_record_id.as_str().to_string())
        })?;
        if !visibility.visible(&capture) {
            continue;
        }
        let revision = resolve_bound_contribution_revision(
            store,
            &binding,
            &pin.revision_record_id,
            pin.object_digest,
            &pin.contribution_id,
        )?;
        if visibility.visible(revision.capture_record_id().as_str()) {
            resolved.push(revision);
        }
    }
    let mut analysis = Vec::new();
    let mut related_count = 0;
    let mut related_truncated = false;
    for envelope in &query.analysis {
        let scoped = envelope_is_scoped(store, envelope, &scope)?;
        let mut summary = AnalysisSummary::of(envelope, query.source.as_ref(), scoped);
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
    let mut selection = select(
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
    let available: BTreeSet<_> = resolved.iter().map(|r| r.record_id().clone()).collect();
    for revision in &resolved {
        for prerequisite in revision.revision().depends_on() {
            if !available.contains(prerequisite) {
                selection
                    .gaps
                    .insert(format!("missing_prerequisite:{prerequisite}"));
            }
        }
    }
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
    loop {
        let returned: BTreeSet<RecordId> = selection
            .items
            .iter()
            .map(|item| resolved[item.candidate].record_id().clone())
            .collect();
        for item in &selection.items {
            for prerequisite in resolved[item.candidate].revision().depends_on() {
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
            "capture lineage belongs to multiple logical contributions; select an exact operation"
                .into(),
        ));
    }
    let operation = crate::collaboration_capture::OperationId::parse(&operations[0])
        .map_err(|error| ContextError::RevisionIdentity(error.to_string()))?;
    let binding = resolve_approved_project_binding(store)
        .map_err(|error| ContextError::RevisionIdentity(error.to_string()))?
        .ok_or_else(|| ContextError::RevisionIdentity("project binding is missing".into()))?;
    let resolved = crate::collaboration_identity_store::revise_contribution_for_operation(
        store,
        &binding,
        crate::collaboration_identity_store::ContributionRevisionRequest {
            operation_id: &operation,
            file_operations: None,
            brief: Some(brief),
            depends_on: None,
        },
    )
    .map_err(|error| ContextError::RevisionIdentity(error.to_string()))?;
    resolved
        .revision()
        .brief()
        .map(|reference| Some(reference.record_id.clone()))
        .ok_or_else(|| {
            ContextError::RevisionIdentity("committed revision has no requested brief".into())
        })
}
