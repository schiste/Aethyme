//! A provisional composer over retained contributions (L4, #664; plan v3
//! §7.2-7.4).
//!
//! [`compose`] builds one candidate from an accepted baseline and a selection
//! of contributions the archive retained (#657, #658), following plan §7.3
//! steps 1-5:
//!
//! 1. **Retain.** Every input is read from the archive, never a worktree:
//!    the lineage record is re-hashed and every manifest and blob is checked
//!    against its digest. An input that is not retained, or known by id
//!    only, is `missing_input`. The baseline commit is retained first, read
//!    from Git exactly as capture reads it.
//! 2. **Close dependencies.** Two revisions of one contribution are
//!    `competing_revisions`, as is a requirement satisfied only by another
//!    revision of it, or a synthesized result selected beside one of its
//!    constituents. A requirement, or an atomic group member, that is not
//!    selected is `missing_input`; a cycle is `dependency_cycle`.
//! 3. **Normalize lineage.** A contribution applies only its own change from
//!    base to result. Its base is the baseline or accepted history the
//!    baseline descends from, or exactly the result of another selected
//!    contribution (which then goes first). The result of a known but
//!    unselected contribution is `missing_input`; anything else is
//!    `unknown_base`. A contribution delivered twice, by name or by lineage,
//!    applies once.
//! 4. **Freeze the order.** Topological, then the order of first delivery.
//!    The order, the profile, the engine version and every input digest go
//!    into a composition recipe record.
//! 5. **Build.** Starting from the baseline, each contribution's change is
//!    applied to the accumulator, path by path, with the three-way inputs
//!    `(base, accumulator, result)`. A contribution applies atomically: one
//!    conflicting path and none of its paths apply.
//!
//! The only profile is [`PROVISIONAL_TEXT_PROFILE`]: a line merge by `git
//! merge-file`, standing in until E1 (#650) selects a structural engine
//! (D04). Its limits are reported, never smoothed over: deleting and
//! modifying one path, adding one path twice, changing one mode two ways,
//! any concurrent change to a binary file or symlink, and a file left where
//! a directory is needed are conflicts. So is a concurrent change to a file
//! one side deleted **a twinned run** from (`ambiguous_anchor`: which copy
//! went is the diff's guess, and a line merge would put the other change
//! on the copy that stayed, silently) or **moved a unique block** within
//! (`moved_block`: a line merge cannot carry the edit along). Those two are
//! the profile's own limits; both sides of every step are checked, so the
//! outcome does not depend on delivery order.
//!
//! The candidate is written as Git objects only (no ref moves), retained in
//! the archive, and returned as a [`Candidate`] through the #663 boundary.
//! It is **unverified**: a clean text merge is not behavior. Verification,
//! resolution requests (#665) and acceptance are separate steps, and the
//! composer has no canonical write capability. Retained candidates have no
//! retention root yet, so reclamation (#659) may remove them; keeping one is
//! the acceptance step's job.
//!
//! [`recompose_without`] forms "X without B" from X's retained
//! constituents (§7.4), or refuses with `inseparable_selection`; it never
//! relabels X.
//!
//! See `docs/architecture/local-v3-l4-composer.md`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use aethyme_contracts::experimental_v0::canonical_json::{Object, Value};
use aethyme_contracts::experimental_v0::{
    EntryKind, FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceEntry, SourceSnapshot,
    SourceSnapshotId,
};

use crate::collaboration_archive::{
    self, ArchiveError, CommitOid, ObjectDigest, RetainedContribution, RetainedSnapshot,
};
use crate::collaboration_state::CollaborationStore;
use crate::composition::{
    Candidate, CandidateInput, CompositionConflict, CompositionMode, CompositionOutcome,
    ConflictReason, Producer, Refusal,
};

/// The provisional profile: sequential three-way line merges by
/// `git merge-file`, with the limits in the module documentation.
pub const PROVISIONAL_TEXT_PROFILE: &str = "aethyme.compose.git-merge-file/provisional-v0";

pub mod structural;
use structural::{MergeMethod, MergeObservation, MergirafEngine};

struct MergeProfile<'a> {
    engine: Option<&'a MergirafEngine>,
    observations: Vec<MergeObservation>,
    text_merges: usize,
    accepted_baseline: Option<AcceptedBaseline>,
}

struct AcceptedBaseline {
    completion: RecordId,
    members: Vec<String>,
    lineages: BTreeSet<RecordId>,
}

impl MergeProfile<'_> {
    fn name(&self) -> &'static str {
        if self.engine.is_some() {
            structural::HTML_PROFILE
        } else if self.accepted_baseline.is_some() {
            "aethyme.compose.git-merge-file/accepted-baseline-v0"
        } else {
            PROVISIONAL_TEXT_PROFILE
        }
    }

    fn mode(&self) -> CompositionMode {
        let structural = self
            .observations
            .iter()
            .filter(|o| {
                matches!(
                    o.method,
                    MergeMethod::StructuredResolution | MergeMethod::FullyStructured
                )
            })
            .count();
        if structural == 0 {
            CompositionMode::Text
        } else if self.text_merges > 0 || structural < self.observations.len() {
            CompositionMode::Hybrid
        } else {
            CompositionMode::Structural
        }
    }
}

/// The opt-in external profile and its actual merge observations, including
/// refused/conflicting attempts. Default text composition remains unchanged.
pub struct StructuralComposition {
    pub composition: Composition,
    pub observations: Vec<MergeObservation>,
}

pub fn compose_structural(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    engine: &MergirafEngine,
) -> Result<StructuralComposition, ComposeError> {
    let mut profile = MergeProfile {
        engine: Some(engine),
        observations: Vec::new(),
        text_merges: 0,
        accepted_baseline: None,
    };
    let composition = compose_inner(
        store,
        repo,
        request,
        CompositionInputs {
            deliveries: &request.deliveries,
            recomposition: None,
            revision_bindings: None,
        },
        None,
        &mut profile,
    )?;
    Ok(StructuralComposition {
        composition,
        observations: profile.observations,
    })
}

/// Where a composer candidate's baseline came from.
pub const COMPOSER_BASELINE_SOURCE: &str = "retained";

pub const COMPOSITION_RECIPE_SCHEMA_NAME: &str = "aethyme.composition-recipe/experimental-v0";

/// Consecutive non-blank lines (whitespace-insensitive) that must leave one
/// place and reappear in another for a change to count as moving a block.
const MOVE_WINDOW: usize = 3;

/// The line diff the profile's own rules use. A test keeps the version in
/// step with Cargo.lock.
const DIFF_ENGINE: &str = "similar 3.2.0 (Myers, lines)";

const fn field(name: &'static str, kind: FieldKind, required: bool) -> FieldSpec {
    FieldSpec {
        name,
        required,
        kind,
        capability: None,
    }
}

/// How one candidate was composed: enough to reproduce it from the archive.
pub static COMPOSITION_RECIPE_SCHEMA: RecordSchema = RecordSchema {
    name: COMPOSITION_RECIPE_SCHEMA_NAME,
    fields: &[
        field("profile", FieldKind::String, true),
        field("engine", FieldKind::Opaque, true),
        field("baseline", FieldKind::String, true),
        field("baseline_commit", FieldKind::String, true),
        field("candidate", FieldKind::String, true),
        field("candidate_commit", FieldKind::String, true),
        field("order", FieldKind::Opaque, true),
        field("deliveries", FieldKind::Integer, true),
        field("budget", FieldKind::Opaque, true),
        field("recomposition", FieldKind::Opaque, false),
        FieldSpec {
            name: "accepted_baseline",
            kind: FieldKind::Opaque,
            required: false,
            capability: Some("accepted-baseline-v1"),
        },
    ],
    capabilities: &["accepted-baseline-v1"],
};

/// One contribution the caller knows about, by its lineage record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionSpec {
    /// The caller's name for it; requirements and deliveries use it.
    pub id: String,
    /// Its lineage record, or `None` when the caller knows the contribution
    /// exists but holds no record of it: it is not retained either way.
    pub lineage: Option<RecordId>,
    /// Contributions this one needs in the same candidate.
    pub requires: Vec<String>,
    /// Contributions sharing a group are accepted together or not at all.
    pub atomic_group: Option<String>,
    /// The contribution this one revises: both cannot be selected.
    pub revision_of: Option<String>,
    /// For a synthesized result, the contributions it was synthesized from.
    pub derived_from: Vec<String>,
}

/// Objective limits on one composition. Exceeding one is
/// `budget_exhausted`, never a partial candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompositionBudget {
    pub max_contributions: usize,
    /// Paths a contribution's change touches, summed over contributions.
    pub max_paths: usize,
    /// Source bytes read from the archive.
    pub max_bytes: u64,
    /// Three-way line merges run.
    pub max_merges: usize,
}

impl Default for CompositionBudget {
    fn default() -> Self {
        Self {
            max_contributions: 64,
            max_paths: 10_000,
            max_bytes: 256 * 1024 * 1024,
            max_merges: 1_000,
        }
    }
}

/// What a composition consumed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BudgetUsage {
    pub contributions: usize,
    pub paths: usize,
    pub bytes: u64,
    pub merges: usize,
}

/// One composition: a baseline, the known contributions, and which of them
/// to deliver, in policy order. Repeats in `deliveries` apply once.
#[derive(Debug, Clone)]
pub struct CompositionRequest {
    /// The accepted commit to build on. The composer retains its snapshot,
    /// read and verified from Git as capture reads it.
    pub baseline: CommitOid,
    /// Accepted history the baseline descends from. A contribution based
    /// on one of these applies its change three-way onto the baseline.
    pub accepted: Vec<CommitOid>,
    pub catalog: Vec<ContributionSpec>,
    pub deliveries: Vec<String>,
    pub budget: CompositionBudget,
}

/// "`from` without `remove`, plus `keep`" (§7.4).
#[derive(Debug, Clone)]
pub struct Subtraction {
    pub from: String,
    pub remove: Vec<String>,
    pub keep: Vec<String>,
}

/// The result of one composition.
#[derive(Debug, Clone)]
pub struct Composition {
    pub outcome: CompositionOutcome,
    /// The recipe record, for a candidate only.
    pub recipe: Option<RecipeRef>,
    /// Contribution IDs in application order, once planning succeeded.
    pub order: Vec<String>,
    pub usage: BudgetUsage,
}

/// Where a composition's recipe record is.
#[derive(Debug, Clone)]
pub struct RecipeRef {
    pub id: RecordId,
    /// The archive object holding the record's canonical bytes.
    pub object: ObjectDigest,
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error(transparent)]
    Archive(#[from] ArchiveError),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("engine budget: {0}")]
    EngineBudget(String),
    #[error("git: {0}")]
    Git(String),
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("candidate commit {commit} names {observed}, but the composition is {expected}")]
    SubjectMismatch {
        commit: String,
        expected: String,
        observed: String,
    },
    #[error("candidate retention: {0}")]
    CandidateRetention(String),
}

impl ComposeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Archive(error) => error.code(),
            Self::InvalidRequest(_) => "invalid_request",
            Self::EngineBudget(_) => "budget_exhausted",
            Self::Git(_) => "git",
            Self::Io { .. } => "io",
            Self::SubjectMismatch { .. } => "subject_mismatch",
            Self::CandidateRetention(_) => "candidate_retention",
        }
    }
}

fn io(path: &Path, source: std::io::Error) -> ComposeError {
    ComposeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

pub(crate) type Entries = BTreeMap<Vec<u8>, (EntryKind, [u8; 32])>;

struct Refused(Refusal, String);

#[derive(Clone, Copy)]
struct CompositionInputs<'a> {
    deliveries: &'a [String],
    recomposition: Option<&'a Subtraction>,
    revision_bindings: Option<&'a BTreeMap<String, RecordId>>,
}

fn refused(reason: Refusal, detail: impl Into<String>) -> Refused {
    Refused(reason, detail.into())
}

/// Compose `request` into a candidate in `repo`'s object database, reading
/// every input from `store`.
pub fn compose(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
) -> Result<Composition, ComposeError> {
    compose_inner(
        store,
        repo,
        request,
        CompositionInputs {
            deliveries: &request.deliveries,
            recomposition: None,
            revision_bindings: None,
        },
        None,
        &mut MergeProfile {
            engine: None,
            observations: Vec::new(),
            text_merges: 0,
            accepted_baseline: None,
        },
    )
}

/// Compose a candidate and let the caller attach its retention roots while
/// this composition's archive-use guard is still held. This closes the GC
/// window between writing an unrooted candidate and making it durable to a
/// target. Ordinary composer callers keep the unretained [`compose`] contract.
pub(crate) fn compose_with_candidate_retainer(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    retainer: &mut CandidateRetainer<'_>,
) -> Result<Composition, ComposeError> {
    compose_inner(
        store,
        repo,
        request,
        CompositionInputs {
            deliveries: &request.deliveries,
            recomposition: None,
            revision_bindings: None,
        },
        Some(retainer),
        &mut MergeProfile {
            engine: None,
            observations: Vec::new(),
            text_merges: 0,
            accepted_baseline: None,
        },
    )
}

/// Internal Local D10 adapter: every delivered contribution must have one
/// exact immutable revision pin, which is copied into the ordered recipe.
/// The caller validates the project/capture/FileId binding before calling.
pub(crate) fn compose_with_candidate_retainer_and_revisions(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    revisions: &BTreeMap<String, RecordId>,
    retainer: &mut CandidateRetainer<'_>,
) -> Result<Composition, ComposeError> {
    compose_inner(
        store,
        repo,
        request,
        CompositionInputs {
            deliveries: &request.deliveries,
            recomposition: None,
            revision_bindings: Some(revisions),
        },
        Some(retainer),
        &mut MergeProfile {
            engine: None,
            observations: Vec::new(),
            text_merges: 0,
            accepted_baseline: None,
        },
    )
}

/// Internal wave adapter only: the caller has rechecked a private completion
/// and its accepted wave against the exact baseline. All constituents remain
/// in the lineage/dependency plan and recipe; delivered deltas are not replayed.
#[cfg(test)]
pub(crate) fn compose_after_accepted(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    completion: RecordId,
    members: Vec<String>,
) -> Result<Composition, ComposeError> {
    compose_after_accepted_inner(store, repo, request, completion, members, None, None)
}

pub(crate) fn compose_after_accepted_with_candidate_retainer(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    completion: RecordId,
    members: Vec<String>,
    retainer: &mut CandidateRetainer<'_>,
) -> Result<Composition, ComposeError> {
    compose_after_accepted_inner(
        store,
        repo,
        request,
        completion,
        members,
        None,
        Some(retainer),
    )
}

pub(crate) fn compose_after_accepted_with_candidate_retainer_and_revisions(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    completion: RecordId,
    members: Vec<String>,
    revisions: &BTreeMap<String, RecordId>,
    retainer: &mut CandidateRetainer<'_>,
) -> Result<Composition, ComposeError> {
    compose_after_accepted_inner(
        store,
        repo,
        request,
        completion,
        members,
        Some(revisions),
        Some(retainer),
    )
}

fn compose_after_accepted_inner(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    completion: RecordId,
    members: Vec<String>,
    revision_bindings: Option<&BTreeMap<String, RecordId>>,
    retainer: Option<&mut CandidateRetainer<'_>>,
) -> Result<Composition, ComposeError> {
    let mut lineages = BTreeSet::new();
    for member in &members {
        if !request.deliveries.contains(member) {
            return Err(ComposeError::InvalidRequest(
                "accepted member is absent from cumulative delivery".into(),
            ));
        }
        let lineage = request
            .catalog
            .iter()
            .find(|s| &s.id == member)
            .and_then(|s| s.lineage.clone())
            .ok_or_else(|| {
                ComposeError::InvalidRequest("accepted lineage is unavailable".into())
            })?;
        lineages.insert(lineage);
    }
    compose_inner(
        store,
        repo,
        request,
        CompositionInputs {
            deliveries: &request.deliveries,
            recomposition: None,
            revision_bindings,
        },
        retainer,
        &mut MergeProfile {
            engine: None,
            observations: Vec::new(),
            text_merges: 0,
            accepted_baseline: Some(AcceptedBaseline {
                completion,
                members,
                lineages,
            }),
        },
    )
}

/// Recompose `subtraction.from` without `subtraction.remove` from its
/// retained constituents, plus `subtraction.keep`. `request.deliveries` is
/// ignored. Refuses with `inseparable_selection` when `from` records no
/// constituents, a constituent is no longer retained, or a kept
/// contribution builds on `from` itself.
pub fn recompose_without(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    subtraction: &Subtraction,
) -> Result<Composition, ComposeError> {
    recompose_inner(
        store,
        repo,
        request,
        subtraction,
        &mut MergeProfile {
            engine: None,
            observations: Vec::new(),
            text_merges: 0,
            accepted_baseline: None,
        },
    )
}

pub fn recompose_structural_without(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    subtraction: &Subtraction,
    engine: &MergirafEngine,
) -> Result<StructuralComposition, ComposeError> {
    let mut profile = MergeProfile {
        engine: Some(engine),
        observations: Vec::new(),
        text_merges: 0,
        accepted_baseline: None,
    };
    let composition = recompose_inner(store, repo, request, subtraction, &mut profile)?;
    Ok(StructuralComposition {
        composition,
        observations: profile.observations,
    })
}

fn recompose_inner(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    subtraction: &Subtraction,
    profile: &mut MergeProfile<'_>,
) -> Result<Composition, ComposeError> {
    let catalog = catalog(request)?;
    let refuse = |detail: String| {
        Ok(Composition {
            outcome: CompositionOutcome::Refused {
                reason: Refusal::InseparableSelection,
                detail,
            },
            recipe: None,
            order: Vec::new(),
            usage: BudgetUsage::default(),
        })
    };
    let Some(from) = catalog.get(subtraction.from.as_str()) else {
        return refuse(format!("{} is not a known contribution", subtraction.from));
    };
    if from.derived_from.is_empty() {
        return refuse(format!(
            "{} records no constituents to recompose from",
            from.id
        ));
    }
    for removed in &subtraction.remove {
        if !from.derived_from.contains(removed) {
            return refuse(format!("{removed} is not a constituent of {}", from.id));
        }
    }
    for kept in &subtraction.keep {
        if subtraction.remove.contains(kept) || *kept == from.id {
            // Keeping what is removed, or X itself, would rebuild X.
            return refuse(format!("{kept} is both kept and removed from {}", from.id));
        }
    }
    let originals: Vec<String> = from
        .derived_from
        .iter()
        .filter(|id| !subtraction.remove.contains(id))
        .cloned()
        .collect();
    for id in &originals {
        let Some(spec) = catalog.get(id.as_str()) else {
            return refuse(format!("constituent {id} is not a known contribution"));
        };
        if retained_of(store, spec)?.is_none() {
            return refuse(format!(
                "constituent {id} is no longer retained, so {} cannot be recomposed without {}",
                from.id,
                subtraction.remove.join(", ")
            ));
        }
    }
    let from_result = retained_of(store, from)?.map(|retained| retained.result.snapshot_id);
    for kept in &subtraction.keep {
        if depends_on(&catalog, kept, &from.id) {
            return refuse(format!(
                "{kept} requires {}, which the selection removes",
                from.id
            ));
        }
        if let (Some(spec), Some(from_result)) = (catalog.get(kept.as_str()), &from_result)
            && let Some(retained) = retained_of(store, spec)?
            && retained.base.snapshot_id == *from_result
        {
            return refuse(format!("{kept} was built on {}", from.id));
        }
    }
    let mut deliveries = originals.clone();
    deliveries.extend(subtraction.keep.iter().cloned());
    compose_inner(
        store,
        repo,
        request,
        CompositionInputs {
            deliveries: &deliveries,
            recomposition: Some(subtraction),
            revision_bindings: None,
        },
        None,
        profile,
    )
}

/// The archive's entry for `spec`, if it has a lineage and it is retained.
fn retained_of(
    store: &CollaborationStore,
    spec: &ContributionSpec,
) -> Result<Option<RetainedContribution>, ArchiveError> {
    match &spec.lineage {
        Some(lineage) => collaboration_archive::retained_contribution(store, lineage),
        None => Ok(None),
    }
}

fn depends_on(catalog: &BTreeMap<&str, &ContributionSpec>, id: &str, target: &str) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![id];
    while let Some(next) = stack.pop() {
        if !seen.insert(next) {
            continue;
        }
        if let Some(spec) = catalog.get(next) {
            for required in &spec.requires {
                if required == target {
                    return true;
                }
                stack.push(required);
            }
        }
    }
    false
}

fn catalog(
    request: &CompositionRequest,
) -> Result<BTreeMap<&str, &ContributionSpec>, ComposeError> {
    let mut catalog = BTreeMap::new();
    for spec in &request.catalog {
        if catalog.insert(spec.id.as_str(), spec).is_some() {
            return Err(ComposeError::InvalidRequest(format!(
                "contribution {} appears twice in the catalog",
                spec.id
            )));
        }
    }
    Ok(catalog)
}

fn compose_inner(
    store: &mut CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    inputs: CompositionInputs<'_>,
    candidate_retainer: Option<&mut CandidateRetainer<'_>>,
    profile: &mut MergeProfile<'_>,
) -> Result<Composition, ComposeError> {
    if let Some(revisions) = inputs.revision_bindings {
        validate_revision_bindings(request, inputs.deliveries, revisions)?;
    }
    // Reclamation cannot remove an input while this composition reads it.
    let _use = crate::collaboration_gc::archive_use(store)
        .map_err(|source| io(&crate::collaboration_gc::lock_path(store), source))?;
    let mut usage = BudgetUsage::default();
    let mut order = Vec::new();
    let unavailable = |detail: String, error: ArchiveError| -> Result<Composition, ComposeError> {
        let unsupported = |reason| CompositionOutcome::Unsupported {
            reason,
            detail: format!("{detail}: {error}"),
        };
        let outcome = match &error {
            ArchiveError::UnsupportedEntry { .. } | ArchiveError::InvalidSnapshot(_) => {
                unsupported(crate::composition::Unsupported::SnapshotEntry)
            }
            ArchiveError::UnsupportedFilter { .. } => {
                unsupported(crate::composition::Unsupported::TransformingAttribute)
            }
            ArchiveError::PartialClone { .. } => {
                unsupported(crate::composition::Unsupported::PartialClone)
            }
            error
                if error.is_incomplete()
                    || matches!(
                        error,
                        ArchiveError::NotACommit { .. } | ArchiveError::UnknownRevision { .. }
                    ) =>
            {
                CompositionOutcome::Refused {
                    reason: Refusal::MissingInput,
                    detail: format!("{detail}: {error}"),
                }
            }
            _ => return Err(error.into()),
        };
        Ok(Composition {
            outcome,
            recipe: None,
            order: Vec::new(),
            usage: BudgetUsage::default(),
        })
    };
    // The baseline is retained like any input, and the candidate's parent
    // is the commit asked for, whichever commit first retained its content.
    let baseline = match collaboration_archive::retain_snapshot_with(
        store,
        repo,
        &request.baseline,
        &mut |_| Ok(()),
    ) {
        Ok(retained) => RetainedSnapshot {
            commit: request.baseline.clone(),
            ..retained
        },
        Err(error) => return unavailable(format!("baseline {}", request.baseline.as_str()), error),
    };
    let mut accepted = Vec::with_capacity(request.accepted.len());
    for commit in &request.accepted {
        if !is_ancestor(repo, commit, &request.baseline)? {
            return Ok(Composition {
                outcome: CompositionOutcome::Refused {
                    reason: Refusal::UnknownBase,
                    detail: format!(
                        "accepted commit {} is not an ancestor of the baseline {}",
                        commit.as_str(),
                        request.baseline.as_str()
                    ),
                },
                recipe: None,
                order: Vec::new(),
                usage: BudgetUsage::default(),
            });
        }
        match collaboration_archive::snapshot_of_commit(repo, commit) {
            Ok(snapshot) => accepted.push(snapshot.id()),
            Err(error) => {
                return unavailable(format!("accepted commit {}", commit.as_str()), error);
            }
        }
    }
    let (candidate, plan) = match build(
        store,
        repo,
        request,
        &Baseline {
            retained: baseline,
            accepted,
        },
        inputs.deliveries,
        (&mut usage, &mut order),
        profile,
    )? {
        Ok(Built::Candidate(candidate, plan)) => (candidate, plan),
        Ok(Built::Other(outcome)) => {
            return Ok(Composition {
                outcome,
                recipe: None,
                order,
                usage,
            });
        }
        Err(Refused(reason, detail)) => {
            return Ok(Composition {
                outcome: CompositionOutcome::Refused { reason, detail },
                recipe: None,
                order,
                usage,
            });
        }
    };
    let retained =
        collaboration_archive::retain_snapshot_with(store, repo, &candidate.commit, &mut |_| {
            Ok(())
        })?;
    if retained.snapshot_id != candidate.subject {
        return Err(ComposeError::SubjectMismatch {
            commit: candidate.commit.as_str().to_string(),
            expected: candidate.subject.to_string(),
            observed: retained.snapshot_id.to_string(),
        });
    }
    let (bytes, id) = recipe_record(&candidate, &plan, request, inputs, usage, profile)?;
    let object = collaboration_archive::put_object(store, &bytes)?;
    let recipe = RecipeRef { id, object };
    if let Some(retain) = candidate_retainer {
        retain(store, &candidate, &recipe, &_use)?;
    }
    Ok(Composition {
        outcome: CompositionOutcome::Candidate(candidate),
        recipe: Some(recipe),
        order,
        usage,
    })
}

fn validate_revision_bindings(
    request: &CompositionRequest,
    deliveries: &[String],
    revisions: &BTreeMap<String, RecordId>,
) -> Result<(), ComposeError> {
    let catalog = catalog(request)?;
    let mut unique_revisions = BTreeSet::new();
    for (contribution, revision) in revisions {
        if !catalog.contains_key(contribution.as_str()) {
            return Err(ComposeError::InvalidRequest(format!(
                "revision pin names unknown contribution {contribution}"
            )));
        }
        if !unique_revisions.insert(revision) {
            return Err(ComposeError::InvalidRequest(
                "one immutable revision is pinned to multiple contributions".into(),
            ));
        }
    }
    for contribution in deliveries {
        if !revisions.contains_key(contribution) {
            return Err(ComposeError::InvalidRequest(format!(
                "delivered contribution {contribution} has no immutable revision pin"
            )));
        }
    }
    Ok(())
}

pub(crate) type CandidateRetainer<'a> = dyn FnMut(
        &mut CollaborationStore,
        &Candidate,
        &RecipeRef,
        &crate::collaboration_gc::ArchiveUse,
    ) -> Result<(), ComposeError>
    + 'a;

enum Built {
    Candidate(Candidate, Plan),
    Other(CompositionOutcome),
}

struct Planned {
    spec: ContributionSpec,
    retained: RetainedContribution,
}

struct Plan {
    baseline: RetainedSnapshot,
    steps: Vec<Planned>,
    coupled: Vec<Vec<String>>,
}

/// The baseline, retained, and the snapshots of accepted history.
struct Baseline {
    retained: RetainedSnapshot,
    accepted: Vec<SourceSnapshotId>,
}

/// Validate an archived target with the same revision, atomic group, and
/// source-lineage rules as composition. This never invokes Git or writes a
/// recipe, candidate, or ref. Accepted history is absent unless explicitly
/// represented by the target's baseline or selected contributions.
pub(crate) type RetainedSelection = Result<Vec<Vec<String>>, (Refusal, String)>;

pub(crate) fn validate_retained_selection(
    store: &CollaborationStore,
    baseline: RetainedSnapshot,
    catalog: Vec<ContributionSpec>,
    deliveries: Vec<String>,
) -> Result<RetainedSelection, ComposeError> {
    let request = CompositionRequest {
        baseline: baseline.commit.clone(),
        accepted: vec![],
        catalog,
        deliveries,
        budget: CompositionBudget {
            max_contributions: 1024,
            ..CompositionBudget::default()
        },
    };
    let baseline = Baseline {
        retained: baseline,
        accepted: vec![],
    };
    Ok(
        match plan(
            store,
            &request,
            &baseline,
            &request.deliveries,
            &mut BudgetUsage::default(),
        )? {
            Ok(plan) => Ok(plan.coupled),
            Err(Refused(reason, detail)) => Err((reason, detail)),
        },
    )
}

fn build(
    store: &CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    baseline: &Baseline,
    deliveries: &[String],
    progress: (&mut BudgetUsage, &mut Vec<String>),
    profile: &mut MergeProfile<'_>,
) -> Result<Result<Built, Refused>, ComposeError> {
    let (usage, order) = progress;
    let plan = match plan(store, request, baseline, deliveries, usage)? {
        Ok(plan) => plan,
        Err(refusal) => return Ok(Err(refusal)),
    };
    order.extend(plan.steps.iter().map(|step| step.spec.id.clone()));

    let mut reader = Reader {
        store,
        cache: HashMap::new(),
        cached_bytes: 0,
        charged: HashSet::new(),
        manifests: HashMap::new(),
        bytes: 0,
    };
    let baseline_entries = reader.entries(&plan.baseline.snapshot_id)?;
    if let Err(refusal) = reader.within(usage, request) {
        return Ok(Err(refusal));
    }
    let mut accumulator = baseline_entries.clone();
    let scratch = tempfile::tempdir().map_err(|source| io(Path::new("<tempdir>"), source))?;
    for step in &plan.steps {
        if profile
            .accepted_baseline
            .as_ref()
            .is_some_and(|accepted| accepted.lineages.contains(&step.retained.lineage_record_id))
        {
            continue;
        }
        let base = reader.entries(&step.retained.base.snapshot_id)?;
        let result = reader.entries(&step.retained.result.snapshot_id)?;
        if let Err(refusal) = reader.within(usage, request) {
            return Ok(Err(refusal));
        }
        let paths: BTreeSet<&Vec<u8>> = base
            .keys()
            .chain(result.keys())
            .filter(|path| base.get(*path) != result.get(*path))
            .collect();
        usage.paths += paths.len();
        if usage.paths > request.budget.max_paths {
            return Ok(Err(refused(
                Refusal::BudgetExhausted,
                format!(
                    "{} changes {} paths, over the budget of {} in all",
                    step.spec.id,
                    paths.len(),
                    request.budget.max_paths
                ),
            )));
        }
        let mut staged = Vec::new();
        let mut conflicts = Vec::new();
        for path in paths {
            let applied = match apply_path(
                &mut reader,
                PathMerge {
                    scratch: scratch.path(),
                    path,
                    profile,
                },
                base.get(path).copied(),
                accumulator.get(path).copied(),
                result.get(path).copied(),
                usage,
                request,
            )? {
                Ok(applied) => applied,
                Err(refusal) => return Ok(Err(refusal)),
            };
            match applied {
                Ok(entry) => {
                    // What the candidate will hold is charged before it is
                    // read or written, whether it is added, replaced or
                    // merged.
                    if let Some((_, digest)) = entry {
                        reader.charge(&digest)?;
                    }
                    staged.push((path.clone(), entry));
                }
                Err(reason) => conflicts.push(CompositionConflict {
                    path: String::from_utf8_lossy(path).into_owned(),
                    input: step.retained.result.commit.clone(),
                    reason,
                }),
            }
        }
        if let Err(refusal) = reader.within(usage, request) {
            return Ok(Err(refusal));
        }
        if conflicts.is_empty() {
            let mut next = accumulator.clone();
            for (path, entry) in &staged {
                match entry {
                    Some(entry) => next.insert(path.clone(), *entry),
                    None => next.remove(path),
                };
            }
            for (path, entry) in &staged {
                if entry.is_some() && collides(&next, path) {
                    conflicts.push(CompositionConflict {
                        path: String::from_utf8_lossy(path).into_owned(),
                        input: step.retained.result.commit.clone(),
                        reason: ConflictReason::DirectoryFile,
                    });
                }
            }
            if conflicts.is_empty() {
                accumulator = next;
                continue;
            }
        }
        return Ok(Ok(Built::Other(CompositionOutcome::Conflict {
            baseline: plan.baseline.commit.clone(),
            conflicts,
        })));
    }
    if accumulator == baseline_entries {
        return Ok(Ok(Built::Other(CompositionOutcome::NoChange {
            baseline: plan.baseline.commit.clone(),
        })));
    }

    let snapshot = SourceSnapshot::new(
        accumulator
            .iter()
            .map(|(path, (kind, digest))| {
                SourceEntry::from_content_digest(path.clone(), *kind, *digest)
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| ComposeError::Archive(ArchiveError::InvalidSnapshot(error)))?;
    let (tree, commit) = materialize(
        &mut reader,
        repo,
        scratch.path(),
        &plan.baseline,
        &baseline_entries,
        &accumulator,
    )?;
    usage.bytes = reader.bytes;
    let observed = collaboration_archive::snapshot_of_commit(repo, &commit)?.id();
    if observed != snapshot.id() {
        return Err(ComposeError::SubjectMismatch {
            commit: commit.as_str().to_string(),
            expected: snapshot.id().to_string(),
            observed: observed.to_string(),
        });
    }
    let inputs = plan
        .steps
        .iter()
        .map(|step| CandidateInput {
            base: step.retained.base.commit.clone(),
            result: step.retained.result.commit.clone(),
            base_snapshot: Some(step.retained.base.snapshot_id.clone()),
            result_snapshot: Some(step.retained.result.snapshot_id.clone()),
        })
        .collect();
    let candidate = Candidate {
        subject: snapshot.id(),
        tree,
        commit,
        baseline: plan.baseline.commit.clone(),
        baseline_source: COMPOSER_BASELINE_SOURCE,
        inputs,
        producer: Producer::Composer {
            profile: profile.name().to_string(),
        },
        mode: profile.mode(),
    };
    Ok(Ok(Built::Candidate(candidate, plan)))
}

/// Whether `path` is a file where `entries` also needs a directory: one of
/// its parents is a file, or another path lies beneath it.
fn collides(entries: &Entries, path: &[u8]) -> bool {
    let parent_is_file = path
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'/')
        .any(|(index, _)| entries.contains_key(&path[..index]));
    let mut beneath = path.to_vec();
    beneath.push(b'/');
    let has_children = entries
        .range(beneath.clone()..)
        .next()
        .is_some_and(|(other, _)| other.starts_with(&beneath));
    parent_is_file || has_children
}

// ---------------------------------------------------------------- planning

fn plan(
    store: &CollaborationStore,
    request: &CompositionRequest,
    known: &Baseline,
    deliveries: &[String],
    usage: &mut BudgetUsage,
) -> Result<Result<Plan, Refused>, ComposeError> {
    let catalog = catalog(request)?;

    // Deliveries in policy order. Every delivered name is checked against
    // its own requirements and group below; a repeat by name, or by
    // lineage under another name, then applies once.
    let mut delivered: Vec<&ContributionSpec> = Vec::new();
    for id in deliveries {
        let Some(spec) = catalog.get(id.as_str()) else {
            return Ok(Err(refused(
                Refusal::MissingInput,
                format!("{id} is not a known contribution"),
            )));
        };
        if !delivered.iter().any(|known| known.id == spec.id) {
            delivered.push(spec);
        }
    }
    let mut selected: Vec<&ContributionSpec> = Vec::new();
    // Each delivered name to the index of the contribution it applies as.
    let mut applies_as: BTreeMap<&str, usize> = BTreeMap::new();
    for spec in &delivered {
        let index = match selected
            .iter()
            .position(|known| known.lineage.is_some() && known.lineage == spec.lineage)
        {
            Some(index) => index,
            None => {
                selected.push(spec);
                selected.len() - 1
            }
        };
        applies_as.insert(&spec.id, index);
    }
    usage.contributions = selected.len();
    if selected.len() > request.budget.max_contributions {
        return Ok(Err(refused(
            Refusal::BudgetExhausted,
            format!(
                "{} contributions selected, over the budget of {}",
                selected.len(),
                request.budget.max_contributions
            ),
        )));
    }
    let is_selected = |id: &str| applies_as.contains_key(id);

    // Revisions of one contribution share a line, named by its first
    // revision, or, when `revision_of` loops, by the loop's least name.
    let line = |id: &str| -> String {
        let mut chain: Vec<&str> = vec![id];
        while let Some(previous) = catalog
            .get(chain[chain.len() - 1])
            .and_then(|spec| spec.revision_of.as_deref())
        {
            if let Some(start) = chain.iter().position(|seen| *seen == previous) {
                return chain[start..].iter().min().expect("non-empty").to_string();
            }
            chain.push(previous);
        }
        chain[chain.len() - 1].to_string()
    };
    let mut lines: BTreeMap<String, &str> = BTreeMap::new();
    for spec in &delivered {
        if let Some(other) = lines.insert(line(&spec.id), &spec.id) {
            return Ok(Err(refused(
                Refusal::CompetingRevisions,
                format!("{other} and {} are revisions of one contribution", spec.id),
            )));
        }
    }
    for spec in &delivered {
        for constituent in &spec.derived_from {
            if let Some(other) = lines.get(&line(constituent)) {
                return Ok(Err(refused(
                    Refusal::CompetingRevisions,
                    format!(
                        "{} was synthesized from {constituent}; {other} cannot be applied again",
                        spec.id
                    ),
                )));
            }
        }
    }
    for spec in &delivered {
        for required in &spec.requires {
            if is_selected(required) {
                continue;
            }
            if let Some(other) = lines.get(&line(required)) {
                return Ok(Err(refused(
                    Refusal::CompetingRevisions,
                    format!(
                        "{} requires {required}, but the selection has the revision {other}",
                        spec.id
                    ),
                )));
            }
            return Ok(Err(refused(
                Refusal::MissingInput,
                format!("{} requires {required}, which is not selected", spec.id),
            )));
        }
    }
    let groups: BTreeSet<&str> = delivered
        .iter()
        .filter_map(|spec| spec.atomic_group.as_deref())
        .collect();
    for group in groups {
        for member in request
            .catalog
            .iter()
            .filter(|spec| spec.atomic_group.as_deref() == Some(group))
        {
            if !lines.contains_key(&line(&member.id)) {
                return Ok(Err(refused(
                    Refusal::MissingInput,
                    format!("atomic group {group} also needs {}", member.id),
                )));
            }
        }
    }

    // Retrieve. Lineage is read from the archive, not trusted from the
    // caller.
    let baseline = known.retained.clone();
    let mut retained = Vec::with_capacity(selected.len());
    for spec in &selected {
        let Some(contribution) = retained_of(store, spec)? else {
            return Ok(Err(refused(
                Refusal::MissingInput,
                format!("{} is not retained", spec.id),
            )));
        };
        retained.push(contribution);
    }

    // Lineage: each base is the baseline or exactly a selected result.
    let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); selected.len()];
    for (index, contribution) in retained.iter().enumerate() {
        let base = &contribution.base.snapshot_id;
        if *base == baseline.snapshot_id || known.accepted.contains(base) {
            continue;
        }
        if let Some(parent) = retained
            .iter()
            .enumerate()
            .find(|(other, candidate)| *other != index && candidate.result.snapshot_id == *base)
            .map(|(other, _)| other)
        {
            edges[index].insert(parent);
            continue;
        }
        for spec in &request.catalog {
            if is_selected(&spec.id) {
                continue;
            }
            if let Some(known) = retained_of(store, spec)?
                && known.result.snapshot_id == *base
            {
                return Ok(Err(refused(
                    Refusal::MissingInput,
                    format!(
                        "{} was built on the result of {}, which is not selected",
                        selected[index].id, spec.id
                    ),
                )));
            }
        }
        return Ok(Err(refused(
            Refusal::UnknownBase,
            format!(
                "{} starts from {base}, which is neither accepted history nor a known result",
                selected[index].id
            ),
        )));
    }
    for spec in &delivered {
        let index = applies_as[spec.id.as_str()];
        for required in &spec.requires {
            // A literal self requirement is a cycle. A different delivered
            // alias of the same retained input still satisfies a prerequisite
            // without applying that input again.
            if let Some(&parent) = applies_as.get(required.as_str())
                && (parent != index || required == &spec.id)
            {
                edges[index].insert(parent);
            }
        }
    }

    // Admission groups include dependency closure and atomic replacements.
    // Grouping is conservative: coupled work receives service together.
    let mut roots: Vec<usize> = (0..selected.len()).collect();
    fn root(roots: &[usize], mut index: usize) -> usize {
        while roots[index] != index {
            index = roots[index];
        }
        index
    }
    fn join(roots: &mut [usize], a: usize, b: usize) {
        let a = root(roots, a);
        let b = root(roots, b);
        roots[b] = a;
    }
    for (child, parents) in edges.iter().enumerate() {
        for &parent in parents {
            join(&mut roots, child, parent);
        }
    }
    let mut atomic: BTreeMap<&str, usize> = BTreeMap::new();
    for member in &request.catalog {
        if let Some(group) = member.atomic_group.as_deref()
            && let Some(delivered) = lines.get(&line(&member.id))
            && let Some(&index) = applies_as.get(delivered)
        {
            if let Some(&other) = atomic.get(group) {
                join(&mut roots, other, index);
            } else {
                atomic.insert(group, index);
            }
        }
    }

    // Topological order; ties keep the order of first delivery.
    let mut placed = vec![false; selected.len()];
    let mut steps = Vec::with_capacity(selected.len());
    while steps.len() < selected.len() {
        let Some(next) = (0..selected.len())
            .find(|&index| !placed[index] && edges[index].iter().all(|&parent| placed[parent]))
        else {
            let stuck: Vec<&str> = (0..selected.len())
                .filter(|&index| !placed[index])
                .map(|index| selected[index].id.as_str())
                .collect();
            return Ok(Err(refused(
                Refusal::DependencyCycle,
                format!("{} depend on each other", stuck.join(", ")),
            )));
        };
        placed[next] = true;
        steps.push(next);
    }
    let mut coupled: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for &index in &steps {
        coupled
            .entry(root(&roots, index))
            .or_default()
            .push(selected[index].id.clone());
    }
    let mut retained: Vec<Option<RetainedContribution>> = retained.into_iter().map(Some).collect();
    let steps = steps
        .into_iter()
        .map(|index| Planned {
            spec: selected[index].clone(),
            retained: retained[index].take().expect("each index placed once"),
        })
        .collect();
    Ok(Ok(Plan {
        baseline,
        steps,
        coupled: coupled.into_values().collect(),
    }))
}

// ----------------------------------------------------------------- reading

/// Blob bytes kept in memory at once; past this the cache starts over and
/// later reads go back to the archive.
const CACHE_LIMIT: u64 = 64 * 1024 * 1024;

struct Reader<'a> {
    store: &'a CollaborationStore,
    cache: HashMap<[u8; 32], Vec<u8>>,
    cached_bytes: u64,
    /// Blobs already counted in `bytes`.
    charged: HashSet<[u8; 32]>,
    manifests: HashMap<SourceSnapshotId, Entries>,
    /// Manifest bytes read plus the size of every distinct blob read,
    /// merged or staged into the candidate.
    bytes: u64,
}

impl Reader<'_> {
    fn entries(&mut self, id: &SourceSnapshotId) -> Result<Entries, ComposeError> {
        if let Some(entries) = self.manifests.get(id) {
            return Ok(entries.clone());
        }
        let bytes = collaboration_archive::read_object(self.store, &ObjectDigest::of_snapshot(id))?;
        self.bytes += bytes.len() as u64;
        let snapshot = collaboration_archive::parse_manifest(&bytes)
            .filter(|snapshot| snapshot.id() == *id)
            .ok_or_else(|| ArchiveError::CorruptObject {
                digest: ObjectDigest::of_snapshot(id).hex(),
            })?;
        let entries: Entries = snapshot
            .entries()
            .iter()
            .map(|entry| {
                (
                    entry.path().to_vec(),
                    (entry.kind(), *entry.content_sha256()),
                )
            })
            .collect();
        self.manifests.insert(id.clone(), entries.clone());
        Ok(entries)
    }

    fn blob(&mut self, digest: &[u8; 32]) -> Result<Vec<u8>, ComposeError> {
        if let Some(bytes) = self.cache.get(digest) {
            return Ok(bytes.clone());
        }
        let bytes =
            collaboration_archive::read_object(self.store, &ObjectDigest::from_bytes(*digest))?;
        if self.charged.insert(*digest) {
            self.bytes += bytes.len() as u64;
        }
        self.remember(*digest, &bytes);
        Ok(bytes)
    }

    fn remember(&mut self, digest: [u8; 32], bytes: &[u8]) {
        let size = bytes.len() as u64;
        if size > CACHE_LIMIT {
            return;
        }
        if self.cached_bytes + size > CACHE_LIMIT {
            self.cache.clear();
            self.cached_bytes = 0;
        }
        self.cached_bytes += size;
        self.cache.insert(digest, bytes.to_vec());
    }

    /// Count a blob the candidate will hold, by its stored size, without
    /// reading it.
    fn charge(&mut self, digest: &[u8; 32]) -> Result<(), ComposeError> {
        if !self.charged.insert(*digest) {
            return Ok(());
        }
        let path =
            collaboration_archive::object_path(self.store, &ObjectDigest::from_bytes(*digest));
        let size = std::fs::metadata(&path)
            .map_err(|source| match source.kind() {
                std::io::ErrorKind::NotFound => {
                    ComposeError::Archive(ArchiveError::MissingObject {
                        digest: ObjectDigest::from_bytes(*digest).hex(),
                    })
                }
                _ => io(&path, source),
            })?
            .len();
        self.bytes += size;
        Ok(())
    }

    /// Keep a merge's output in the archive, so it is read back like any
    /// other blob and the cache can forget it.
    fn store_merged(&mut self, bytes: &[u8]) -> Result<[u8; 32], ComposeError> {
        let digest = collaboration_archive::put_object(self.store, bytes)?;
        let raw = digest.raw();
        self.remember(raw, bytes);
        Ok(raw)
    }

    /// Refuse once more bytes were read than the budget allows.
    fn within(&self, usage: &mut BudgetUsage, request: &CompositionRequest) -> Result<(), Refused> {
        usage.bytes = self.bytes;
        if self.bytes > request.budget.max_bytes {
            return Err(refused(
                Refusal::BudgetExhausted,
                format!(
                    "read {} source bytes, over the budget of {}",
                    self.bytes, request.budget.max_bytes
                ),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- applying

pub(crate) type Entry = (EntryKind, [u8; 32]);

struct PathMerge<'a, 'engine> {
    scratch: &'a Path,
    path: &'a [u8],
    profile: &'a mut MergeProfile<'engine>,
}

/// Apply one contribution's change to one path: `Ok(Ok(new entry or
/// removal))`, `Ok(Err(reason))` for a conflict.
fn apply_path(
    reader: &mut Reader<'_>,
    merger: PathMerge<'_, '_>,
    base: Option<Entry>,
    current: Option<Entry>,
    result: Option<Entry>,
    usage: &mut BudgetUsage,
    request: &CompositionRequest,
) -> Result<Result<Result<Option<Entry>, ConflictReason>, Refused>, ComposeError> {
    if current == result {
        return Ok(Ok(Ok(current)));
    }
    if current == base {
        return Ok(Ok(Ok(result)));
    }
    let (Some(base), Some(current), Some(result)) = (base, current, result) else {
        let reason = if base.is_none() {
            ConflictReason::AddAdd
        } else {
            ConflictReason::DeleteModify
        };
        return Ok(Ok(Err(reason)));
    };
    let kind = if current.0 == base.0 {
        result.0
    } else if result.0 == base.0 || result.0 == current.0 {
        current.0
    } else {
        return Ok(Ok(Err(ConflictReason::Mode)));
    };
    if current.1 == base.1 {
        return Ok(Ok(Ok(Some((kind, result.1)))));
    }
    if result.1 == base.1 || result.1 == current.1 {
        return Ok(Ok(Ok(Some((kind, current.1)))));
    }
    if [base.0, current.0, result.0].contains(&EntryKind::Symlink) {
        return Ok(Ok(Err(ConflictReason::Binary)));
    }
    let base_text = reader.blob(&base.1)?;
    let current_text = reader.blob(&current.1)?;
    let result_text = reader.blob(&result.1)?;
    if let Err(refusal) = reader.within(usage, request) {
        return Ok(Err(refusal));
    }
    if [&base_text, &current_text, &result_text]
        .iter()
        .any(|text| is_binary(text))
    {
        return Ok(Ok(Err(ConflictReason::Binary)));
    }
    if deletes_a_twin(&base_text, &current_text) || deletes_a_twin(&base_text, &result_text) {
        return Ok(Ok(Err(ConflictReason::AmbiguousAnchor)));
    }
    let moved = moves_a_block(&base_text, &current_text) || moves_a_block(&base_text, &result_text);
    let html_engine = merger
        .profile
        .engine
        .filter(|_| structural::supports(merger.path));
    if moved && html_engine.is_none() {
        return Ok(Ok(Err(ConflictReason::MovedBlock)));
    }
    usage.merges += 1;
    if usage.merges > request.budget.max_merges {
        return Ok(Err(refused(
            Refusal::BudgetExhausted,
            format!(
                "{} line merges, over the budget of {}",
                usage.merges, request.budget.max_merges
            ),
        )));
    }
    let merged = if let Some(engine) = html_engine {
        let merged = match engine.merge(merger.path, &base_text, &current_text, &result_text) {
            Ok(merged) => merged,
            Err(ComposeError::EngineBudget(detail)) => {
                return Ok(Err(refused(Refusal::BudgetExhausted, detail)));
            }
            Err(error) => return Err(error),
        };
        let observed_structural = matches!(
            merged.observation.method,
            MergeMethod::StructuredResolution | MergeMethod::FullyStructured
        );
        let retained_diagnostics: usize = merger
            .profile
            .observations
            .iter()
            .map(|observation| observation.diagnostic.len())
            .sum();
        if retained_diagnostics.saturating_add(merged.observation.diagnostic.len())
            > 16 * 1024 * 1024
        {
            return Ok(Err(refused(
                Refusal::BudgetExhausted,
                "composition diagnostic allowance exhausted",
            )));
        }
        merger.profile.observations.push(merged.observation);
        if moved && !observed_structural {
            return Ok(Ok(Err(ConflictReason::MovedBlock)));
        }
        merged.bytes
    } else {
        merger.profile.text_merges += 1;
        match merge_file(
            merger.scratch,
            &base_text,
            &current_text,
            &result_text,
            merger.profile.engine.is_some(),
        ) {
            Ok(merged) => merged,
            Err(ComposeError::EngineBudget(detail)) => {
                return Ok(Err(refused(Refusal::BudgetExhausted, detail)));
            }
            Err(error) => return Err(error),
        }
    };
    let Some(merged) = merged else {
        return Ok(Ok(Err(ConflictReason::Content)));
    };
    let digest = reader.store_merged(&merged)?;
    Ok(Ok(Ok(Some((kind, digest)))))
}

/// Git's own test: a NUL in the first 8000 bytes.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
}

/// Whether `side` deletes, with nothing put in its place, a run of `base`
/// lines whose non-blank lines (compared without surrounding whitespace)
/// also appear, in order and contiguously, elsewhere in `base`. Which copy
/// went is then a guess the line diff makes, and a line merge would put
/// another side's change inside either copy on whichever one stayed. That
/// holds whether the deletion is half of a move, within this file or into
/// another, or a plain removal, so any concurrent change conflicts.
fn deletes_a_twin(base: &[u8], side: &[u8]) -> bool {
    use similar::{DiffOp, TextDiff};

    let base = String::from_utf8_lossy(base);
    let side = String::from_utf8_lossy(side);
    let diff = TextDiff::from_lines(base.as_ref(), side.as_ref());
    let lines: Vec<&str> = (0..)
        .map_while(|index| diff.old_slice(index))
        .map(|line| line.trim())
        .collect();
    // Non-blank base lines, with their positions.
    let content: Vec<(usize, &str)> = lines
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, line)| !line.is_empty())
        .collect();
    diff.ops().iter().any(|op| {
        let DiffOp::Delete {
            old_index, old_len, ..
        } = *op
        else {
            return false;
        };
        let deleted: Vec<usize> = (0..content.len())
            .filter(|&at| (old_index..old_index + old_len).contains(&content[at].0))
            .collect();
        let (Some(&first), Some(&last)) = (deleted.first(), deleted.last()) else {
            return false;
        };
        let run: Vec<&str> = content[first..=last]
            .iter()
            .map(|(_, line)| *line)
            .collect();
        content
            .windows(run.len())
            .enumerate()
            .any(|(start, window)| {
                start != first && window.iter().map(|(_, line)| *line).eq(run.iter().copied())
            })
    })
}

/// Whether `side` moves a block of `base`: [`MOVE_WINDOW`] consecutive
/// non-blank lines (compared without surrounding whitespace) that the line
/// diff deletes in one place and inserts in another. A line merge pairs a
/// moved block with nothing, so another side's edit inside it either
/// conflicts or, when the block has an identical twin, lands on the wrong
/// copy.
fn moves_a_block(base: &[u8], side: &[u8]) -> bool {
    use similar::{ChangeTag, TextDiff};

    let base = String::from_utf8_lossy(base);
    let side = String::from_utf8_lossy(side);
    let diff = TextDiff::from_lines(base.as_ref(), side.as_ref());
    let mut deleted: Vec<Vec<String>> = Vec::new();
    let mut inserted: Vec<Vec<String>> = Vec::new();
    let mut last = ChangeTag::Equal;
    for change in diff.iter_all_changes() {
        let tag = change.tag();
        let line = change.value().trim().to_string();
        let runs = match tag {
            ChangeTag::Equal => {
                last = tag;
                continue;
            }
            ChangeTag::Delete => &mut deleted,
            ChangeTag::Insert => &mut inserted,
        };
        if tag != last || runs.is_empty() {
            runs.push(Vec::new());
        }
        last = tag;
        if !line.is_empty() {
            runs.last_mut().expect("pushed above").push(line);
        }
    }
    let windows: HashSet<&[String]> = inserted
        .iter()
        .flat_map(|run| run.windows(MOVE_WINDOW))
        .collect();
    deleted
        .iter()
        .flat_map(|run| run.windows(MOVE_WINDOW))
        .any(|window| windows.contains(window))
}

/// A clean three-way line merge of `current` and `result` from `base`, or
/// `None` on any conflict.
fn merge_file(
    scratch: &Path,
    base: &[u8],
    current: &[u8],
    result: &[u8],
    bounded: bool,
) -> Result<Option<Vec<u8>>, ComposeError> {
    let write = |name: &str, bytes: &[u8]| -> Result<PathBuf, ComposeError> {
        let path = scratch.join(name);
        std::fs::write(&path, bytes).map_err(|source| io(&path, source))?;
        Ok(path)
    };
    let current = write("current", current)?;
    let base = write("base", base)?;
    let result = write("result", result)?;
    let mut command = engine(scratch);
    command
        .args([
            "merge-file",
            "-p",
            "-L",
            "current",
            "-L",
            "base",
            "-L",
            "result",
        ])
        .arg(&current)
        .arg(&base)
        .arg(&result)
        .stdin(Stdio::null());
    let output = if bounded {
        match crate::bounded_output::output_limited(
            &mut command,
            std::time::Duration::from_secs(5),
            8 * 1024 * 1024,
        )
        .map_err(|source| io(Path::new("git"), source))?
        {
            crate::bounded_output::LimitedOutput::Completed(output) => output,
            crate::bounded_output::LimitedOutput::Deadline => {
                return Err(ComposeError::EngineBudget(
                    "text fallback deadline exhausted".into(),
                ));
            }
            crate::bounded_output::LimitedOutput::OutputLimit => {
                return Err(ComposeError::EngineBudget(
                    "text fallback output limit exhausted".into(),
                ));
            }
        }
    } else {
        command
            .output()
            .map_err(|source| io(Path::new("git"), source))?
    };
    match output.status.code() {
        Some(0) => Ok(Some(output.stdout)),
        Some(code) if (1..=127).contains(&code) => Ok(None),
        _ => Err(ComposeError::Git(format!(
            "merge-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// `git` run in `dir`, outside any repository (discovery stops at `dir`'s
/// parent), with no system or global configuration, so neither a
/// configured conflict style nor a repository's own configuration can
/// change what the profile does.
fn engine(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(dir)
        .env("GIT_CEILING_DIRECTORIES", dir.parent().unwrap_or(dir))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    command
}

fn git_version() -> Result<String, ComposeError> {
    let output = engine(&std::env::temp_dir())
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| io(Path::new("git"), source))?;
    if !output.status.success() {
        return Err(ComposeError::Git("git --version failed".into()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

// ------------------------------------------------- shared with resolution

/// The entries of retained snapshot `id`, verified against its digest.
pub(crate) fn snapshot_entries(
    store: &CollaborationStore,
    id: &SourceSnapshotId,
) -> Result<Entries, ComposeError> {
    let mut reader = Reader {
        store,
        cache: HashMap::new(),
        cached_bytes: 0,
        charged: HashSet::new(),
        manifests: HashMap::new(),
        bytes: 0,
    };
    reader.entries(id)
}

/// The bytes of one retained blob, verified against its digest.
pub(crate) fn blob(store: &CollaborationStore, digest: &[u8; 32]) -> Result<Vec<u8>, ComposeError> {
    let mut reader = Reader {
        store,
        cache: HashMap::new(),
        cached_bytes: 0,
        charged: HashSet::new(),
        manifests: HashMap::new(),
        bytes: 0,
    };
    reader.blob(digest)
}

/// Write `entries` as a candidate commit on `baseline` and retain it, the
/// way [`compose`] does: Git objects only, the commit's snapshot checked
/// against `entries`, then retained. `blobs` holds content the archive does
/// not have yet, by SHA-256. Returns the subject, tree and commit.
pub(crate) fn materialize_candidate(
    store: &mut CollaborationStore,
    repo: &Path,
    baseline: &RetainedSnapshot,
    entries: &Entries,
    blobs: HashMap<[u8; 32], Vec<u8>>,
) -> Result<(SourceSnapshotId, String, CommitOid), ComposeError> {
    let expected = SourceSnapshot::new(
        entries
            .iter()
            .map(|(path, (kind, digest))| {
                SourceEntry::from_content_digest(path.clone(), *kind, *digest)
            })
            .collect::<Vec<_>>(),
    )
    .map_err(|error| ComposeError::Archive(ArchiveError::InvalidSnapshot(error)))?
    .id();
    let (tree, commit) = {
        let _use = crate::collaboration_gc::archive_use(store)
            .map_err(|source| io(&crate::collaboration_gc::lock_path(store), source))?;
        // Proposed content goes into the archive first, as merged content
        // does, so the reader finds it whatever its cache holds.
        for (digest, bytes) in &blobs {
            let stored = collaboration_archive::put_object(store, bytes)?;
            if stored != ObjectDigest::from_bytes(*digest) {
                return Err(ComposeError::Archive(ArchiveError::CorruptObject {
                    digest: stored.hex(),
                }));
            }
        }
        let mut reader = Reader {
            store,
            cache: HashMap::new(),
            cached_bytes: 0,
            charged: HashSet::new(),
            manifests: HashMap::new(),
            bytes: 0,
        };
        let baseline_entries = reader.entries(&baseline.snapshot_id)?;
        let scratch = tempfile::tempdir().map_err(|source| io(Path::new("<tempdir>"), source))?;
        materialize(
            &mut reader,
            repo,
            scratch.path(),
            baseline,
            &baseline_entries,
            entries,
        )?
    };
    let observed = collaboration_archive::snapshot_of_commit(repo, &commit)?.id();
    if observed != expected {
        return Err(ComposeError::SubjectMismatch {
            commit: commit.as_str().to_string(),
            expected: expected.to_string(),
            observed: observed.to_string(),
        });
    }
    let retained =
        collaboration_archive::retain_snapshot_with(store, repo, &commit, &mut |_| Ok(()))?;
    if retained.snapshot_id != expected {
        return Err(ComposeError::SubjectMismatch {
            commit: commit.as_str().to_string(),
            expected: expected.to_string(),
            observed: retained.snapshot_id.to_string(),
        });
    }
    Ok((expected, tree, commit))
}

/// The retained snapshot of `commit`, read from Git as capture reads it.
pub(crate) fn retain_commit(
    store: &mut CollaborationStore,
    repo: &Path,
    commit: &CommitOid,
) -> Result<RetainedSnapshot, ComposeError> {
    let retained =
        collaboration_archive::retain_snapshot_with(store, repo, commit, &mut |_| Ok(()))?;
    Ok(RetainedSnapshot {
        commit: commit.clone(),
        ..retained
    })
}

/// Whether `ancestor` is `descendant` or one of its ancestors.
fn is_ancestor(
    repo: &Path,
    ancestor: &CommitOid,
    descendant: &CommitOid,
) -> Result<bool, ComposeError> {
    let output = collaboration_archive::git(repo)
        .args([
            "merge-base",
            "--is-ancestor",
            ancestor.as_str(),
            descendant.as_str(),
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|source| io(Path::new("git"), source))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(ComposeError::Git(format!(
            "merge-base --is-ancestor: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

// ------------------------------------------------------------ materializing

/// Write the candidate as Git objects in `repo`: blobs, trees and one commit
/// whose only parent is the baseline. No ref, index or worktree changes.
fn materialize(
    reader: &mut Reader<'_>,
    repo: &Path,
    scratch: &Path,
    baseline: &RetainedSnapshot,
    baseline_entries: &Entries,
    entries: &Entries,
) -> Result<(String, CommitOid), ComposeError> {
    let parent = baseline.commit.as_str();
    let existing = repo_git(
        repo,
        &["cat-file", "-e", &format!("{parent}^{{commit}}")],
        None,
    );
    if existing.is_err() {
        return Err(ComposeError::Git(format!(
            "baseline commit {parent} is not in {}",
            repo.display()
        )));
    }
    // Blobs the baseline commit already has are reused; the final snapshot
    // check proves the reuse right.
    let listed = repo_git(repo, &["ls-tree", "-r", "-z", "--full-tree", parent], None)?;
    let mut known: HashMap<Vec<u8>, String> = HashMap::new();
    for record in listed.split(|byte| *byte == 0).filter(|r| !r.is_empty()) {
        let Some(tab) = record.iter().position(|byte| *byte == b'\t') else {
            continue;
        };
        let header = String::from_utf8_lossy(&record[..tab]);
        if let Some(oid) = header.split(' ').nth(2) {
            known.insert(record[tab + 1..].to_vec(), oid.to_string());
        }
    }

    let tree = materialize_tree(reader, repo, scratch, baseline_entries, &known, entries)?;
    let commit = repo_git_env(
        repo,
        &[
            "commit-tree",
            &tree,
            "-p",
            parent,
            "-m",
            "chore(composer): composition candidate",
        ],
    )?;
    Ok((tree, CommitOid::parse(&text_of(&commit))?))
}

/// Shared raw CAS-to-Git tree writer. Candidate composition can reuse exact
/// baseline blobs; a standalone generation passes an empty known baseline.
fn materialize_tree(
    reader: &mut Reader<'_>,
    repo: &Path,
    scratch: &Path,
    baseline_entries: &Entries,
    known: &HashMap<Vec<u8>, String>,
    entries: &Entries,
) -> Result<String, ComposeError> {
    // Blobs the baseline lacks are written in one hash-object, and the
    // tree in one index: a fixed number of processes, whatever the size.
    let blobs = scratch.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(|source| io(&blobs, source))?;
    let mut oids: BTreeMap<Vec<u8>, (EntryKind, String)> = BTreeMap::new();
    let mut pending: Vec<(Vec<u8>, EntryKind, PathBuf)> = Vec::new();
    let mut written: HashMap<[u8; 32], PathBuf> = HashMap::new();
    for (path, (kind, digest)) in entries {
        if baseline_entries.get(path) == Some(&(*kind, *digest))
            && let Some(oid) = known.get(path)
        {
            oids.insert(path.clone(), (*kind, oid.clone()));
            continue;
        }
        let file = match written.get(digest) {
            Some(file) => file.clone(),
            None => {
                let file = blobs.join(written.len().to_string());
                let bytes = reader.blob(digest)?;
                std::fs::write(&file, &bytes).map_err(|source| io(&file, source))?;
                written.insert(*digest, file.clone());
                file
            }
        };
        pending.push((path.clone(), *kind, file));
    }
    if !pending.is_empty() {
        let mut paths = Vec::new();
        for (_, _, file) in &pending {
            paths.extend_from_slice(file.as_os_str().as_encoded_bytes());
            paths.push(b'\n');
        }
        let output = run(
            collaboration_archive::git(repo).args([
                "hash-object",
                "-w",
                "--no-filters",
                "--stdin-paths",
            ]),
            Some(&paths),
        )?;
        let hashed: Vec<String> = String::from_utf8_lossy(&output)
            .lines()
            .map(str::to_string)
            .collect();
        if hashed.len() != pending.len() {
            return Err(ComposeError::Git(format!(
                "hash-object wrote {} of {} blobs",
                hashed.len(),
                pending.len()
            )));
        }
        for ((path, kind, _), oid) in pending.into_iter().zip(hashed) {
            oids.insert(path, (kind, oid));
        }
    }
    write_tree(repo, scratch, &oids)
}

/// Write `entries` as one tree through a private index file, leaving the
/// repository's own index alone.
fn write_tree(
    repo: &Path,
    scratch: &Path,
    entries: &BTreeMap<Vec<u8>, (EntryKind, String)>,
) -> Result<String, ComposeError> {
    let index = scratch.join("index");
    let mut lines: Vec<u8> = Vec::new();
    for (path, (kind, oid)) in entries {
        lines.extend_from_slice(format!("{} {oid}\t", kind.git_mode()).as_bytes());
        lines.extend_from_slice(path);
        lines.push(0);
    }
    run(
        collaboration_archive::git(repo)
            .env("GIT_INDEX_FILE", &index)
            .args(["update-index", "-z", "--add", "--index-info"]),
        Some(&lines),
    )?;
    let output = run(
        collaboration_archive::git(repo)
            .env("GIT_INDEX_FILE", &index)
            .args(["write-tree"]),
        None,
    )?;
    Ok(text_of(&output))
}

fn text_of(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).trim().to_string()
}

fn repo_git(repo: &Path, args: &[&str], file: Option<&Path>) -> Result<Vec<u8>, ComposeError> {
    let mut command = collaboration_archive::git(repo);
    command.args(args);
    if let Some(file) = file {
        command.arg(file);
    }
    run(&mut command, None)
}

/// A commit whose identity and date are fixed, so the same composition of
/// the same inputs is the same commit.
fn repo_git_env(repo: &Path, args: &[&str]) -> Result<Vec<u8>, ComposeError> {
    let mut command = collaboration_archive::git(repo);
    command
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "i18n.commitEncoding=UTF-8",
        ])
        .args(args)
        .env("GIT_AUTHOR_NAME", "Aethyme composer")
        .env("GIT_AUTHOR_EMAIL", "composer@aethyme.invalid")
        .env("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z")
        .env("GIT_COMMITTER_NAME", "Aethyme composer")
        .env("GIT_COMMITTER_EMAIL", "composer@aethyme.invalid")
        .env("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z");
    run(&mut command, None)
}

fn run(command: &mut Command, input: Option<&[u8]>) -> Result<Vec<u8>, ComposeError> {
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|source| io(Path::new("git"), source))?;
    if let Some(input) = input {
        let mut stdin = child.stdin.take().expect("piped");
        stdin
            .write_all(input)
            .map_err(|source| io(Path::new("git"), source))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|source| io(Path::new("git"), source))?;
    if !output.status.success() {
        return Err(ComposeError::Git(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    Ok(output.stdout)
}

// ------------------------------------------------------------------ recipe

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn integer(value: usize) -> Value {
    Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))
}

fn object(members: Vec<(&str, Value)>) -> Value {
    Value::Object(
        Object::new(
            members
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
        .expect("distinct keys"),
    )
}

fn merge_observation_record(observation: &MergeObservation) -> Value {
    object(vec![
        ("path", text(&observation.path)),
        (
            "method",
            text(match observation.method {
                MergeMethod::LineBased => "line_based",
                MergeMethod::StructuredResolution => "structured_resolution",
                MergeMethod::FullyStructured => "fully_structured",
                MergeMethod::Unknown => "unknown",
            }),
        ),
        ("timed_fallback", Value::Bool(observation.timed_fallback)),
        (
            "exit_code",
            text(
                &observation
                    .exit_code
                    .map(|code| code.to_string())
                    .unwrap_or_else(|| "unknown".into()),
            ),
        ),
        ("base_sha256", text(&observation.base_sha256)),
        ("current_sha256", text(&observation.current_sha256)),
        ("result_sha256", text(&observation.result_sha256)),
        ("output_sha256", text(&observation.output_sha256)),
    ])
}

fn recipe_record(
    candidate: &Candidate,
    plan: &Plan,
    request: &CompositionRequest,
    inputs: CompositionInputs<'_>,
    usage: BudgetUsage,
    profile: &MergeProfile<'_>,
) -> Result<(Vec<u8>, RecordId), ComposeError> {
    let engine_version = git_version()?;
    let order = plan
        .steps
        .iter()
        .map(|step| {
            recipe_order_entry(
                &step.spec.id,
                step.retained.lineage_record_id.as_str(),
                step.retained.base.snapshot_id.as_str(),
                step.retained.result.snapshot_id.as_str(),
                inputs
                    .revision_bindings
                    .and_then(|bindings| bindings.get(&step.spec.id))
                    .map(RecordId::as_str),
            )
        })
        .collect();
    let budget = |contributions: usize, paths: usize, bytes: u64, merges: usize| {
        object(vec![
            ("contributions", integer(contributions)),
            ("paths", integer(paths)),
            ("bytes", text(&bytes.to_string())),
            ("merges", integer(merges)),
        ])
    };
    let limits = request.budget;
    let mut members = vec![
        ("schema", text(COMPOSITION_RECIPE_SCHEMA_NAME)),
        ("profile", text(profile.name())),
        (
            "engine",
            if let Some(engine) = profile.engine {
                object(vec![
                    ("name", text("mergiraf")),
                    ("version", text(structural::MERGIRAF_VERSION)),
                    ("sha256", text(engine.sha256())),
                    ("grammar", text("html")),
                    ("license", text("GPL-3.0-only; external executable")),
                    ("wall_ms", integer(5000)),
                    ("structural_timeout_ms", integer(2000)),
                    ("max_input_bytes", integer(4 * 1024 * 1024)),
                    ("max_stream_bytes", integer(8 * 1024 * 1024)),
                    ("text_merges", integer(profile.text_merges)),
                    (
                        "observations",
                        Value::Array(
                            profile
                                .observations
                                .iter()
                                .map(merge_observation_record)
                                .collect(),
                        ),
                    ),
                    ("text_fallback_version", text(&engine_version)),
                ])
            } else {
                object(vec![
                    ("name", text("git merge-file")),
                    ("version", text(&engine_version)),
                    ("diff", text(DIFF_ENGINE)),
                    ("move_window", integer(MOVE_WINDOW)),
                ])
            },
        ),
        ("baseline", text(plan.baseline.snapshot_id.as_str())),
        ("baseline_commit", text(plan.baseline.commit.as_str())),
        ("candidate", text(candidate.subject.as_str())),
        ("candidate_commit", text(candidate.commit.as_str())),
        ("order", Value::Array(order)),
        ("deliveries", integer(inputs.deliveries.len())),
        (
            "budget",
            object(vec![
                (
                    "limits",
                    budget(
                        limits.max_contributions,
                        limits.max_paths,
                        limits.max_bytes,
                        limits.max_merges,
                    ),
                ),
                (
                    "used",
                    budget(usage.contributions, usage.paths, usage.bytes, usage.merges),
                ),
            ]),
        ),
    ];
    if let Some(accepted) = &profile.accepted_baseline {
        members.push((
            "accepted_baseline",
            object(vec![
                ("completion", text(accepted.completion.as_str())),
                (
                    "members",
                    Value::Array(accepted.members.iter().map(|s| text(s)).collect()),
                ),
            ]),
        ));
        members.push((
            "requires",
            Value::Array(
                COMPOSITION_RECIPE_SCHEMA
                    .required_capabilities(members.iter().map(|(key, _)| *key))
                    .iter()
                    .map(|s| text(s))
                    .collect(),
            ),
        ));
    }
    if let Some(subtraction) = inputs.recomposition {
        members.push((
            "recomposition",
            object(vec![
                ("from", text(&subtraction.from)),
                (
                    "remove",
                    Value::Array(subtraction.remove.iter().map(|id| text(id)).collect()),
                ),
                (
                    "keep",
                    Value::Array(subtraction.keep.iter().map(|id| text(id)).collect()),
                ),
            ]),
        ));
    }
    let bytes = object(members).to_canonical_bytes();
    let id = Record::decode(&bytes, &[&COMPOSITION_RECIPE_SCHEMA])
        .map_err(|error| ComposeError::InvalidRequest(format!("recipe record: {error}")))?
        .id();
    Ok((bytes, id))
}

fn recipe_order_entry(
    contribution: &str,
    lineage: &str,
    base: &str,
    result: &str,
    revision: Option<&str>,
) -> Value {
    let mut fields = vec![
        ("id", text(contribution)),
        ("lineage", text(lineage)),
        ("base", text(base)),
        ("result", text(result)),
    ];
    if let Some(revision) = revision {
        fields.push(("revision", text(revision)));
    }
    object(fields)
}

/// Read a recipe back from the archive.
pub fn read_recipe(store: &CollaborationStore, recipe: &RecipeRef) -> Result<Record, ComposeError> {
    let bytes = collaboration_archive::read_object(store, &recipe.object)?;
    let record = Record::decode(&bytes, &[&COMPOSITION_RECIPE_SCHEMA])
        .map_err(|error| ComposeError::InvalidRequest(format!("recipe record: {error}")))?;
    if record.id() != recipe.id {
        return Err(ComposeError::Archive(ArchiveError::CorruptObject {
            digest: recipe.object.hex(),
        }));
    }
    Ok(record)
}

/// Independent editable checkout metadata from retained bytes. This reuses the
/// candidate writer's raw CAS/blob/private-index plumbing, with no parent or
/// dependency on an old Git object database. The caller supplies a new root.
pub(crate) fn generation_checkout(
    store: &CollaborationStore,
    repo: &Path,
    scratch: &Path,
    snapshot: &SourceSnapshot,
) -> Result<(String, CommitOid), ComposeError> {
    repo_git(
        repo,
        &[
            "init",
            "-q",
            "--template=",
            "--object-format=sha1",
            "--initial-branch=agent-generation",
        ],
        None,
    )?;
    // A generated repository copies no configuration from the old checkout or
    // remote. Disable hooks/fsmonitor persistently for later ordinary Git use.
    repo_git(repo, &["config", "core.hooksPath", "/dev/null"], None)?;
    repo_git(repo, &["config", "core.fsmonitor", "false"], None)?;
    repo_git(repo, &["config", "core.autocrlf", "false"], None)?;
    repo_git(repo, &["config", "core.filemode", "true"], None)?;
    let entries: Entries = snapshot
        .entries()
        .iter()
        .map(|e| (e.path().to_vec(), (e.kind(), *e.content_sha256())))
        .collect();
    let mut reader = Reader {
        store,
        cache: HashMap::new(),
        cached_bytes: 0,
        charged: HashSet::new(),
        manifests: HashMap::new(),
        bytes: 0,
    };
    let tree = materialize_tree(
        &mut reader,
        repo,
        scratch,
        &Entries::new(),
        &HashMap::new(),
        &entries,
    )?;
    let commit = repo_git_env(
        repo,
        &[
            "commit-tree",
            &tree,
            "-m",
            "chore(generation): retained source representation",
        ],
    )?;
    let commit = CommitOid::parse(&text_of(&commit))?;
    // Only the new standalone repository's private branch/index is changed.
    repo_git(
        repo,
        &["update-ref", "refs/heads/agent-generation", commit.as_str()],
        None,
    )?;
    std::fs::copy(scratch.join("index"), repo.join(".git/index"))
        .map_err(|source| io(repo, source))?;
    Ok((tree, commit))
}

/// Capture the complete actual directory, ignoring only its own top-level .git.
/// This is source verification, not read-only execution permission sealing.
fn generation_files(root: &Path, omit_git: bool) -> Result<SourceSnapshot, ComposeError> {
    fn collect(
        root: &Path,
        dir: &Path,
        omit_git: bool,
        entries: &mut Vec<SourceEntry>,
        size: &mut usize,
    ) -> Result<(), ComposeError> {
        for entry in std::fs::read_dir(dir).map_err(|source| io(dir, source))? {
            let entry = entry.map_err(|source| io(dir, source))?;
            let path = entry.path();
            let relative = path.strip_prefix(root).expect("directory child");
            if omit_git && relative == Path::new(".git") {
                continue;
            }
            let m = std::fs::symlink_metadata(&path).map_err(|source| io(&path, source))?;
            if m.file_type().is_symlink() {
                return Err(ComposeError::Git("generation contains a symlink".into()));
            }
            if m.is_dir() {
                collect(root, &path, omit_git, entries, size)?;
                continue;
            }
            if !m.is_file()
                || m.len() > 64 * 1024 * 1024
                || entries.len() >= if omit_git { 8192 } else { 32768 }
            {
                return Err(ComposeError::Git("generation file boundary".into()));
            }
            let bytes = std::fs::read(&path).map_err(|source| io(&path, source))?;
            *size = size
                .checked_add(bytes.len())
                .ok_or_else(|| ComposeError::Git("generation byte limit".into()))?;
            if *size > 256 * 1024 * 1024 {
                return Err(ComposeError::Git("generation byte limit".into()));
            }
            let kind = {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if m.permissions().mode() & 0o111 != 0 {
                        EntryKind::Executable
                    } else {
                        EntryKind::Regular
                    }
                }
                #[cfg(not(unix))]
                {
                    EntryKind::Regular
                }
            };
            entries.push(SourceEntry::from_content(
                relative.as_os_str().as_encoded_bytes().to_vec(),
                kind,
                &bytes,
            ));
        }
        Ok(())
    }
    let m = std::fs::symlink_metadata(root).map_err(|source| io(root, source))?;
    if !m.is_dir() || m.file_type().is_symlink() {
        return Err(ComposeError::Git(
            "generation root is not a directory".into(),
        ));
    }
    let mut entries = Vec::new();
    let mut size = 0;
    collect(root, root, omit_git, &mut entries, &mut size)?;
    SourceSnapshot::new(entries)
        .map_err(|e| ComposeError::Archive(ArchiveError::InvalidSnapshot(e)))
}
/// Digest all private Git files before invoking Git while loading a generation.
pub(crate) fn generation_metadata_digest(root: &Path) -> Result<String, ComposeError> {
    Ok(generation_files(&root.join(".git"), false)?
        .id()
        .to_string())
}
pub(crate) fn verify_generation_checkout(
    root: &Path,
    snapshot: &SourceSnapshot,
    tree: &str,
    commit: &str,
) -> Result<(), ComposeError> {
    if generation_files(root, true)? != *snapshot {
        return Err(ComposeError::Git("generation source differs".into()));
    }
    let commit = CommitOid::parse(commit)?;
    if collaboration_archive::raw_commit_tree(root, &commit)? != tree
        || collaboration_archive::snapshot_of_commit(root, &commit)? != *snapshot
        || text_of(&repo_git(root, &["rev-parse", "--verify", "HEAD"], None)?) != commit.as_str()
    {
        return Err(ComposeError::Git(
            "generation Git/source mapping differs".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_observations_do_not_depend_on_diagnostic_timing() {
        let first = MergeObservation {
            path: "component.html".into(),
            method: MergeMethod::FullyStructured,
            timed_fallback: false,
            exit_code: Some(0),
            base_sha256: "a".repeat(64),
            current_sha256: "b".repeat(64),
            result_sha256: "c".repeat(64),
            output_sha256: "d".repeat(64),
            diagnostic_sha256: "e".repeat(64),
            diagnostic: "merge took 1ms".into(),
        };
        let mut second = first.clone();
        second.diagnostic_sha256 = "f".repeat(64);
        second.diagnostic = "merge took 2ms".into();
        assert_eq!(
            merge_observation_record(&first),
            merge_observation_record(&second)
        );
        second.output_sha256 = "0".repeat(64);
        assert_ne!(
            merge_observation_record(&first),
            merge_observation_record(&second)
        );
    }

    #[test]
    fn a_block_moved_elsewhere_is_a_move_even_reindented() {
        let base = "a\nb\nc\nd\ne\nf\n";
        let side = "a\n    d\n    e\n    f\nb\nc\n";
        assert!(moves_a_block(base.as_bytes(), side.as_bytes()));
    }

    #[test]
    fn edits_insertions_and_deletions_are_not_moves() {
        let base = "a\nb\nc\nd\ne\n";
        for side in [
            "a\nB\nc\nd\ne\n",
            "a\nb\nc\nd\ne\nf\ng\nh\n",
            "a\ne\n",
            "x\ny\nz\na\nb\nc\nd\ne\n",
        ] {
            assert!(!moves_a_block(base.as_bytes(), side.as_bytes()), "{side:?}");
        }
    }

    #[test]
    fn re_adding_fewer_lines_than_the_window_is_not_a_move() {
        let base = "<ul>\n  <li>one</li>\n</ul>\n<p>text</p>\n";
        let side = "<p>text</p>\n<ul>\n  <li>two</li>\n</ul>\n";
        assert!(!moves_a_block(base.as_bytes(), side.as_bytes()));
    }

    #[test]
    fn blank_lines_do_not_split_or_pad_a_moved_block() {
        // The diff keeps the longer run p..u and moves a, b, (blank), c.
        let base = "p\nq\nr\ns\nt\nu\na\nb\n\nc\n";
        let side = "a\nb\n\nc\np\nq\nr\ns\nt\nu\n";
        assert!(moves_a_block(base.as_bytes(), side.as_bytes()));
    }

    #[test]
    fn deleting_one_of_two_identical_runs_is_ambiguous() {
        let base =
            "<ul>\n  <li>tip</li>\n  <li>more</li>\n  <li>tip</li>\n  <li>more</li>\n</ul>\n";
        let side = "<ul>\n  <li>tip</li>\n  <li>more</li>\n</ul>\n";
        assert!(deletes_a_twin(base.as_bytes(), side.as_bytes()));
        // Indentation does not make a copy distinct.
        let indented = "<ul>\n  <li>tip</li>\n  <li>more</li>\n      <li>tip</li>\n      <li>more</li>\n</ul>\n";
        assert!(deletes_a_twin(indented.as_bytes(), side.as_bytes()));
    }

    #[test]
    fn deleting_a_unique_run_or_changing_a_twin_is_not_ambiguous() {
        let base = "a\nb\nc\nb\nd\n";
        // A unique line goes.
        assert!(!deletes_a_twin(base.as_bytes(), b"a\nb\nb\nd\n"));
        // A twin is changed in place, not deleted.
        assert!(!deletes_a_twin(base.as_bytes(), b"a\nB\nc\nb\nd\n"));
        // Only a blank line goes.
        assert!(!deletes_a_twin(b"a\n\nb\n\nc\n", b"a\nb\n\nc\n"));
    }

    #[test]
    fn the_recorded_diff_engine_is_the_locked_version() {
        let lock = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"),
        )
        .unwrap();
        let version = lock
            .split("[[package]]")
            .find(|package| package.contains("name = \"similar\""))
            .and_then(|package| {
                package
                    .lines()
                    .find_map(|line| line.strip_prefix("version = \""))
                    .map(|rest| rest.trim_end_matches('"').to_string())
            })
            .expect("similar is locked");
        assert!(
            DIFF_ENGINE.starts_with(&format!("similar {version} ")),
            "{DIFF_ENGINE} vs locked {version}"
        );
    }

    #[test]
    fn binary_is_git_s_nul_test() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text\n"));
        let mut late = vec![b'a'; 8000];
        late.push(0);
        assert!(!is_binary(&late));
    }
    #[test]
    #[cfg(unix)]
    fn unsupported_grammar_never_dispatches_the_structural_tool() {
        use std::os::unix::fs::PermissionsExt;
        let f = crate::local_target::tests::Fixture::new();
        let root = tempfile::tempdir().unwrap();
        let tool = root.path().join("control");
        let bytes=b"#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'mergiraf 0.19.1\\n'; exit 0; fi\nprintf 'Selected best merge: fully_structured\\n' >&2\nprintf 'wrong-control-output\\n'\n";
        std::fs::write(&tool, bytes).unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o700)).unwrap();
        let hash = ObjectDigest::of(bytes).hex();
        let engine = MergirafEngine::pin(&tool, &hash).unwrap();
        let mut profile = MergeProfile {
            engine: Some(&engine),
            observations: Vec::new(),
            text_merges: 0,
            accepted_baseline: None,
        };
        let mut reader = Reader {
            store: &f.store,
            cache: HashMap::new(),
            cached_bytes: 0,
            charged: HashSet::new(),
            manifests: HashMap::new(),
            bytes: 0,
        };
        let entry = |bytes: &[u8]| {
            Some((
                EntryKind::Regular,
                collaboration_archive::put_object(&f.store, bytes)
                    .unwrap()
                    .raw(),
            ))
        };
        let request = CompositionRequest {
            baseline: f.a.base.commit.clone(),
            accepted: vec![],
            catalog: vec![],
            deliveries: vec![],
            budget: Default::default(),
        };
        let mut usage = BudgetUsage::default();
        let merged = apply_path(
            &mut reader,
            PathMerge {
                scratch: root.path(),
                path: b"source.rs",
                profile: &mut profile,
            },
            entry(b"alpha\nbeta\ngamma\ndelta\nomega\n"),
            entry(b"ALPHA\nbeta\ngamma\ndelta\nomega\n"),
            entry(b"alpha\nbeta\ngamma\ndelta\nOMEGA\n"),
            &mut usage,
            &request,
        )
        .unwrap()
        .unwrap_or_else(|error| panic!("control merge refused: {}", error.1))
        .unwrap()
        .unwrap();
        assert_eq!(
            reader.blob(&merged.1).unwrap(),
            b"ALPHA\nbeta\ngamma\ndelta\nOMEGA\n",
            "unsupported grammar uses exact text merge"
        );
        assert!(
            profile.observations.is_empty(),
            "unsupported grammar must never dispatch external engine"
        );
        assert_eq!(profile.text_merges, 1);
    }
}

#[cfg(test)]
#[path = "composer/revision_tests.rs"]
mod revision_tests;
