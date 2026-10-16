//! Experimental advisory adapter for explicitly bound clone symbol mappings.
//!
//! The wire claim stays `proposed`. Only this non-serializable, in-process
//! binding token plus a closed set of canonical records can produce
//! `EstablishedWithinBinding`. That result is scoped to the exact project,
//! two clone identities, source snapshots, profile and independent origins;
//! it carries no verification, acceptance or execution authority.

use std::collections::{BTreeMap, BTreeSet};

use aethyme_contracts::experimental_v0::canonical_json::Value;
use aethyme_contracts::experimental_v0::symbol_identity::{
    EXPLICIT_SYMBOL_MAPPING_EVIDENCE_SCHEMA_NAME, OBSERVED_SYMBOL_REVISION_SCHEMA_NAME,
    SYMBOL_MAPPING_AUTHORITY, SYMBOL_MAPPING_CLAIM_SCHEMA_NAME, SYMBOL_ORIGIN_EVIDENCE_SCHEMA_NAME,
    SYMBOL_REF_SCHEMA_NAME, SymbolScopeKind, decode_symbol_identity_record, symbol_scope_kind,
};
use aethyme_contracts::experimental_v0::{
    Record, RecordId, SourceSnapshot, SourceSnapshotId, StateReading,
};
use aethyme_graph_schema::NodeId;

/// A local binding token that is not representable on the wire. Callers make
/// it from the exact source snapshots and repo namespace values they captured
/// for two distinct clones, then pass it alongside the serialized proposal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneSnapshotBinding {
    project_id: RecordId,
    left_clone_id: RecordId,
    right_clone_id: RecordId,
    left_source_snapshot: SourceSnapshotId,
    right_source_snapshot: SourceSnapshotId,
    profile: RecordId,
    left_repo_component: String,
    right_repo_component: String,
}

/// Locally captured identity and source for one clone in a mapping binding.
#[derive(Debug, Clone)]
pub struct CapturedCloneSnapshot<'a> {
    clone_id: RecordId,
    repo_component: String,
    source: &'a SourceSnapshot,
}

impl<'a> CapturedCloneSnapshot<'a> {
    pub fn new(
        clone_id: RecordId,
        repo_component: impl Into<String>,
        source: &'a SourceSnapshot,
    ) -> Self {
        Self {
            clone_id,
            repo_component: repo_component.into(),
            source,
        }
    }
}

impl CloneSnapshotBinding {
    pub fn from_snapshots(
        project_id: RecordId,
        left: CapturedCloneSnapshot<'_>,
        right: CapturedCloneSnapshot<'_>,
        profile: RecordId,
    ) -> Result<Self, BindingError> {
        let CapturedCloneSnapshot {
            clone_id: left_clone_id,
            repo_component: left_repo_component,
            source: left_source,
        } = left;
        let CapturedCloneSnapshot {
            clone_id: right_clone_id,
            repo_component: right_repo_component,
            source: right_source,
        } = right;
        if left_clone_id == right_clone_id {
            return Err(BindingError::SameClone);
        }
        if left_repo_component.is_empty()
            || right_repo_component.is_empty()
            || left_repo_component.contains(':')
            || right_repo_component.contains(':')
        {
            return Err(BindingError::InvalidRepoComponent);
        }
        Ok(Self {
            project_id,
            left_clone_id,
            right_clone_id,
            left_source_snapshot: left_source.id(),
            right_source_snapshot: right_source.id(),
            profile,
            left_repo_component,
            right_repo_component,
        })
    }

    pub fn project_id(&self) -> &RecordId {
        &self.project_id
    }

    pub fn left_clone_id(&self) -> &RecordId {
        &self.left_clone_id
    }

    pub fn right_clone_id(&self) -> &RecordId {
        &self.right_clone_id
    }

    pub fn left_source_snapshot(&self) -> &SourceSnapshotId {
        &self.left_source_snapshot
    }

    pub fn right_source_snapshot(&self) -> &SourceSnapshotId {
        &self.right_source_snapshot
    }

    pub fn profile(&self) -> &RecordId {
        &self.profile
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BindingError {
    #[error("clone binding must name two distinct clones")]
    SameClone,
    #[error("repo namespace must be nonempty and contain no colon")]
    InvalidRepoComponent,
}

/// A positive result exists only in process and only within one exact binding.
/// It is not an assertion of universal symbol identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstablishedWithinBinding {
    binding_record: RecordId,
    left_symbol_ref: RecordId,
    right_symbol_ref: RecordId,
    left_observation: RecordId,
    right_observation: RecordId,
    left_origin: RecordId,
    right_origin: RecordId,
}

impl EstablishedWithinBinding {
    pub fn binding_record(&self) -> &RecordId {
        &self.binding_record
    }
    pub fn left_symbol_ref(&self) -> &RecordId {
        &self.left_symbol_ref
    }
    pub fn right_symbol_ref(&self) -> &RecordId {
        &self.right_symbol_ref
    }
    pub fn observations(&self) -> (&RecordId, &RecordId) {
        (&self.left_observation, &self.right_observation)
    }
    pub fn origins(&self) -> (&RecordId, &RecordId) {
        (&self.left_origin, &self.right_origin)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingResolution {
    EstablishedWithinBinding(EstablishedWithinBinding),
    Unknown(&'static str),
}

/// Resolve an advisory claim against one local binding and exactly the seven
/// canonical records in its reference closure: mapping evidence, two
/// observations, two symbol references and two independent origins.
pub fn resolve_within_binding(
    local_binding: &CloneSnapshotBinding,
    claim_bytes: &[u8],
    evidence_bytes: &[&[u8]],
) -> MappingResolution {
    let claim = match decode_symbol_identity_record(claim_bytes) {
        Ok(record) if record.schema() == SYMBOL_MAPPING_CLAIM_SCHEMA_NAME => record,
        _ => return MappingResolution::Unknown("invalid_or_non_claim_record"),
    };
    if !matches!(claim.state("state"), StateReading::Known("proposed")) {
        return MappingResolution::Unknown("claim_not_proposed");
    }
    let Some(binding_id) = record_id(&claim, "binding") else {
        return MappingResolution::Unknown("invalid_binding_reference");
    };
    if evidence_bytes.len() > 7 {
        return MappingResolution::Unknown("evidence_set_not_exact");
    }
    let mut records = BTreeMap::new();
    for bytes in evidence_bytes {
        let record = match decode_symbol_identity_record(bytes) {
            Ok(record) => record,
            Err(_) => return MappingResolution::Unknown("invalid_evidence_record"),
        };
        let id = record.id();
        if records.insert(id, record).is_some() {
            return MappingResolution::Unknown("duplicate_evidence_record");
        }
    }
    let Some(binding) = records.get(&binding_id) else {
        return MappingResolution::Unknown("missing_binding_evidence");
    };
    if binding.schema() != EXPLICIT_SYMBOL_MAPPING_EVIDENCE_SCHEMA_NAME {
        return MappingResolution::Unknown("wrong_binding_schema");
    }

    let Some(left_observation_id) = record_id(&claim, "left_observation") else {
        return MappingResolution::Unknown("invalid_left_observation_reference");
    };
    let Some(right_observation_id) = record_id(&claim, "right_observation") else {
        return MappingResolution::Unknown("invalid_right_observation_reference");
    };
    let Some(left_symbol_id) = record_id(binding, "left_symbol_ref") else {
        return MappingResolution::Unknown("invalid_left_symbol_reference");
    };
    let Some(right_symbol_id) = record_id(binding, "right_symbol_ref") else {
        return MappingResolution::Unknown("invalid_right_symbol_reference");
    };
    let Some(left_origin_id) = record_id(binding, "left_origin") else {
        return MappingResolution::Unknown("invalid_left_origin_reference");
    };
    let Some(right_origin_id) = record_id(binding, "right_origin") else {
        return MappingResolution::Unknown("invalid_right_origin_reference");
    };

    let Some(left_observation) = records.get(&left_observation_id) else {
        return MappingResolution::Unknown("missing_left_observation");
    };
    let Some(right_observation) = records.get(&right_observation_id) else {
        return MappingResolution::Unknown("missing_right_observation");
    };
    let Some(left_symbol) = records.get(&left_symbol_id) else {
        return MappingResolution::Unknown("missing_left_symbol");
    };
    let Some(right_symbol) = records.get(&right_symbol_id) else {
        return MappingResolution::Unknown("missing_right_symbol");
    };
    let Some(left_origin) = records.get(&left_origin_id) else {
        return MappingResolution::Unknown("missing_left_origin");
    };
    let Some(right_origin) = records.get(&right_origin_id) else {
        return MappingResolution::Unknown("missing_right_origin");
    };

    if left_observation.schema() != OBSERVED_SYMBOL_REVISION_SCHEMA_NAME
        || right_observation.schema() != OBSERVED_SYMBOL_REVISION_SCHEMA_NAME
        || left_symbol.schema() != SYMBOL_REF_SCHEMA_NAME
        || right_symbol.schema() != SYMBOL_REF_SCHEMA_NAME
        || left_origin.schema() != SYMBOL_ORIGIN_EVIDENCE_SCHEMA_NAME
        || right_origin.schema() != SYMBOL_ORIGIN_EVIDENCE_SCHEMA_NAME
    {
        return MappingResolution::Unknown("wrong_referenced_record_schema");
    }
    if !claim_binding_matches_claim(
        &claim,
        &binding_id,
        &left_observation_id,
        &right_observation_id,
        binding,
        local_binding,
    ) {
        return MappingResolution::Unknown("binding_scope_mismatch");
    }

    let expected = [
        binding_id.clone(),
        left_observation_id.clone(),
        right_observation_id.clone(),
        left_symbol_id.clone(),
        right_symbol_id.clone(),
        left_origin_id.clone(),
        right_origin_id.clone(),
    ];
    let expected_set: BTreeSet<_> = expected.iter().cloned().collect();
    let claimed_origins = string_set(&claim, "evidence");
    let mut expected_origins = [left_origin_id.clone(), right_origin_id.clone()];
    expected_origins.sort();
    if expected_set.len() != 7
        || records.len() != 7
        || records.keys().cloned().collect::<BTreeSet<_>>() != expected_set
        || claimed_origins.as_deref() != Some(expected_origins.as_slice())
    {
        return MappingResolution::Unknown("evidence_set_not_exact");
    }

    let Some(left_clone) = record_id(binding, "left_clone_id") else {
        return MappingResolution::Unknown("invalid_left_clone_reference");
    };
    let Some(right_clone) = record_id(binding, "right_clone_id") else {
        return MappingResolution::Unknown("invalid_right_clone_reference");
    };
    let Some(profile) = record_id(binding, "profile") else {
        return MappingResolution::Unknown("invalid_profile_reference");
    };
    let Some(project) = record_id(binding, "project_id") else {
        return MappingResolution::Unknown("invalid_project_reference");
    };
    if project != local_binding.project_id
        || left_clone != local_binding.left_clone_id
        || right_clone != local_binding.right_clone_id
        || text(binding, "left_source_snapshot")
            != Some(local_binding.left_source_snapshot.as_str())
        || text(binding, "right_source_snapshot")
            != Some(local_binding.right_source_snapshot.as_str())
        || profile != local_binding.profile
    {
        return MappingResolution::Unknown("captured_clone_binding_mismatch");
    }

    if !observation_matches(
        left_observation,
        &left_symbol_id,
        &left_origin_id,
        local_binding.left_source_snapshot.as_str(),
        &profile,
    ) || !observation_matches(
        right_observation,
        &right_symbol_id,
        &right_origin_id,
        local_binding.right_source_snapshot.as_str(),
        &profile,
    ) {
        return MappingResolution::Unknown("observation_binding_mismatch");
    }
    if !symbol_matches(
        left_symbol,
        &project,
        &left_clone,
        &local_binding.left_repo_component,
    ) || !symbol_matches(
        right_symbol,
        &project,
        &right_clone,
        &local_binding.right_repo_component,
    ) {
        return MappingResolution::Unknown("symbol_scope_mismatch");
    }
    if let Err(reason) = mapping_scope_compatibility(left_symbol, right_symbol) {
        return MappingResolution::Unknown(reason);
    }
    if !origin_matches(
        left_origin,
        &project,
        &left_clone,
        &left_symbol_id,
        &profile,
        left_observation,
    ) || !origin_matches(
        right_origin,
        &project,
        &right_clone,
        &right_symbol_id,
        &profile,
        right_observation,
    ) {
        return MappingResolution::Unknown("origin_binding_mismatch");
    }

    MappingResolution::EstablishedWithinBinding(EstablishedWithinBinding {
        binding_record: binding_id,
        left_symbol_ref: left_symbol_id,
        right_symbol_ref: right_symbol_id,
        left_observation: left_observation_id,
        right_observation: right_observation_id,
        left_origin: left_origin_id,
        right_origin: right_origin_id,
    })
}

fn claim_binding_matches_claim(
    claim: &Record,
    binding_id: &RecordId,
    left_observation: &RecordId,
    right_observation: &RecordId,
    binding: &Record,
    local_binding: &CloneSnapshotBinding,
) -> bool {
    record_id(claim, "binding").as_ref() == Some(binding_id)
        && record_id(binding, "left_observation").as_ref() == Some(left_observation)
        && record_id(binding, "right_observation").as_ref() == Some(right_observation)
        && text(binding, "authority") == Some(SYMBOL_MAPPING_AUTHORITY)
        && text(claim, "authority") == Some(SYMBOL_MAPPING_AUTHORITY)
        && record_id(binding, "project_id").as_ref() == Some(&local_binding.project_id)
}

fn observation_matches(
    observation: &Record,
    symbol: &RecordId,
    origin: &RecordId,
    source: &str,
    profile: &RecordId,
) -> bool {
    record_id(observation, "symbol_ref").as_ref() == Some(symbol)
        && record_id(observation, "origin_record").as_ref() == Some(origin)
        && text(observation, "source_snapshot") == Some(source)
        && record_id(observation, "profile").as_ref() == Some(profile)
        && record_id(observation, "fact_revision").is_some()
}

fn symbol_matches(
    symbol: &Record,
    project: &RecordId,
    clone: &RecordId,
    expected_repo: &str,
) -> bool {
    if record_id(symbol, "project_id").as_ref() != Some(project)
        || record_id(symbol, "clone_id").as_ref() != Some(clone)
    {
        return false;
    }
    match symbol_scope_kind(symbol) {
        Some(SymbolScopeKind::EngineNode) => {
            let Some(Value::Object(scope)) = symbol.get("scope") else {
                return false;
            };
            let Some(Value::String(node_id)) = scope.get("node_id") else {
                return false;
            };
            NodeId::parse(node_id).is_ok_and(|parsed| parsed.repo() == expected_repo)
        }
        Some(SymbolScopeKind::ProducerLocal | SymbolScopeKind::ExternalPackage) => true,
        Some(SymbolScopeKind::Unknown) | None => false,
    }
}

fn mapping_scope_compatibility(left: &Record, right: &Record) -> Result<(), &'static str> {
    match (symbol_scope_kind(left), symbol_scope_kind(right)) {
        (Some(SymbolScopeKind::ExternalPackage), Some(SymbolScopeKind::ExternalPackage)) => {
            let Some((left_producer, left_package, left_version)) =
                external_package_coordinates(left)
            else {
                return Err("symbol_scope_mismatch");
            };
            let Some((right_producer, right_package, right_version)) =
                external_package_coordinates(right)
            else {
                return Err("symbol_scope_mismatch");
            };
            if left_version != right_version {
                return Err("cross_version_mapping_unproven");
            }
            if left_producer != right_producer || left_package != right_package {
                return Err("symbol_scope_mismatch");
            }
            Ok(())
        }
        (Some(SymbolScopeKind::ExternalPackage), _)
        | (_, Some(SymbolScopeKind::ExternalPackage)) => Err("symbol_scope_mismatch"),
        _ => Ok(()),
    }
}

fn external_package_coordinates(record: &Record) -> Option<(&str, &str, &str)> {
    let Value::Object(scope) = record.get("scope")? else {
        return None;
    };
    let Value::String(producer) = scope.get("producer_namespace")? else {
        return None;
    };
    let Value::String(package) = scope.get("package")? else {
        return None;
    };
    let Value::String(version) = scope.get("version")? else {
        return None;
    };
    Some((producer, package, version))
}

fn origin_matches(
    origin: &Record,
    project: &RecordId,
    clone: &RecordId,
    symbol: &RecordId,
    profile: &RecordId,
    observation: &Record,
) -> bool {
    record_id(origin, "project_id").as_ref() == Some(project)
        && record_id(origin, "clone_id").as_ref() == Some(clone)
        && record_id(origin, "symbol_ref").as_ref() == Some(symbol)
        && record_id(origin, "profile").as_ref() == Some(profile)
        && text(origin, "source_snapshot") == text(observation, "source_snapshot")
        && record_id(origin, "fact_revision") == record_id(observation, "fact_revision")
        && text(origin, "authority") == Some(SYMBOL_MAPPING_AUTHORITY)
        && text(origin, "origin_namespace").is_some()
}

fn record_id(record: &Record, field: &str) -> Option<RecordId> {
    let Value::String(encoded) = record.get(field)? else {
        return None;
    };
    RecordId::parse(encoded).ok()
}

fn text<'a>(record: &'a Record, field: &str) -> Option<&'a str> {
    match record.get(field)? {
        Value::String(value) => Some(value),
        _ => None,
    }
}

fn string_set(record: &Record, field: &str) -> Option<Vec<RecordId>> {
    let Value::Array(values) = record.get(field)? else {
        return None;
    };
    values
        .iter()
        .map(|value| match value {
            Value::String(value) => RecordId::parse(value).ok(),
            _ => None,
        })
        .collect()
}
