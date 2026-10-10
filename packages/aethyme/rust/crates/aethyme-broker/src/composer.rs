//! A provisional composer over retained contributions (L4, #664; plan v3
//! §7.2-7.4).
//!
//! [`compose`] builds one candidate from an accepted baseline and a selection
//! of contributions the archive retained (#657, #658), following plan §7.3
//! steps 1-5:
//!
//! 1. **Retain.** Every input is read from the archive, never a worktree:
//!    the lineage record is re-hashed and every manifest and blob is checked
//!    against its digest. An input that is not retained is `missing_input`.
//! 2. **Close dependencies.** Two revisions of one contribution are
//!    `competing_revisions`, as is a requirement satisfied only by another
//!    revision of it, or a synthesized result selected beside one of its
//!    constituents. A requirement, or an atomic group member, that is not
//!    selected is `missing_input`; a cycle is `dependency_cycle`.
//! 3. **Normalize lineage.** A contribution applies only its own change from
//!    base to result. Its base is the baseline, or exactly the result of
//!    another selected contribution (which then goes first). The result of a
//!    known but unselected contribution is `missing_input`; anything else is
//!    `unknown_base`. The same contribution delivered twice applies once.
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
//! any concurrent change to a binary file or symlink, and a concurrent
//! change to a file one side **moved a block** within are conflicts. The
//! last one is the profile's own limit: a line merge cannot carry an edit
//! along a moved block, and where the block has an identical twin it would
//! put the edit on the wrong copy without any conflict (FX02).
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

/// Where a composer candidate's baseline came from.
pub const COMPOSER_BASELINE_SOURCE: &str = "retained";

pub const COMPOSITION_RECIPE_SCHEMA_NAME: &str = "aethyme.composition-recipe/experimental-v0";

/// Consecutive non-blank lines (whitespace-insensitive) that must leave one
/// place and reappear in another for a change to count as moving a block.
const MOVE_WINDOW: usize = 3;

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
    ],
    capabilities: &[],
};

/// One contribution the caller knows about, by its retained lineage record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContributionSpec {
    /// The caller's name for it; requirements and deliveries use it.
    pub id: String,
    pub lineage: RecordId,
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
    pub baseline: SourceSnapshotId,
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
}

impl ComposeError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Archive(error) => error.code(),
            Self::InvalidRequest(_) => "invalid_request",
            Self::Git(_) => "git",
            Self::Io { .. } => "io",
            Self::SubjectMismatch { .. } => "subject_mismatch",
        }
    }
}

fn io(path: &Path, source: std::io::Error) -> ComposeError {
    ComposeError::Io {
        path: path.to_path_buf(),
        source,
    }
}

type Entries = BTreeMap<Vec<u8>, (EntryKind, [u8; 32])>;

struct Refused(Refusal, String);

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
    compose_inner(store, repo, request, &request.deliveries, None)
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
        if collaboration_archive::retained_contribution(store, &spec.lineage)?.is_none() {
            return refuse(format!(
                "constituent {id} is no longer retained, so {} cannot be recomposed without {}",
                from.id,
                subtraction.remove.join(", ")
            ));
        }
    }
    let from_result = collaboration_archive::retained_contribution(store, &from.lineage)?
        .map(|retained| retained.result.snapshot_id);
    for kept in &subtraction.keep {
        if depends_on(&catalog, kept, &from.id) {
            return refuse(format!(
                "{kept} requires {}, which the selection removes",
                from.id
            ));
        }
        if let (Some(spec), Some(from_result)) = (catalog.get(kept.as_str()), &from_result)
            && let Some(retained) =
                collaboration_archive::retained_contribution(store, &spec.lineage)?
            && retained.base.snapshot_id == *from_result
        {
            return refuse(format!("{kept} was built on {}", from.id));
        }
    }
    let mut deliveries = originals.clone();
    deliveries.extend(subtraction.keep.iter().cloned());
    compose_inner(store, repo, request, &deliveries, Some(subtraction))
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
    deliveries: &[String],
    recomposition: Option<&Subtraction>,
) -> Result<Composition, ComposeError> {
    // Reclamation cannot remove an input while this composition reads it.
    let _use = crate::collaboration_gc::archive_use(store)
        .map_err(|source| io(&crate::collaboration_gc::lock_path(store), source))?;
    let mut usage = BudgetUsage::default();
    let mut order = Vec::new();
    let (candidate, plan) = match build(store, repo, request, deliveries, &mut usage, &mut order)? {
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
    let (bytes, id) = recipe_record(&candidate, &plan, request, deliveries, usage, recomposition)?;
    let object = collaboration_archive::put_object(store, &bytes)?;
    Ok(Composition {
        outcome: CompositionOutcome::Candidate(candidate),
        recipe: Some(RecipeRef { id, object }),
        order,
        usage,
    })
}

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
    engine_version: String,
}

fn build(
    store: &CollaborationStore,
    repo: &Path,
    request: &CompositionRequest,
    deliveries: &[String],
    usage: &mut BudgetUsage,
    order: &mut Vec<String>,
) -> Result<Result<Built, Refused>, ComposeError> {
    let plan = match plan(store, request, deliveries, usage)? {
        Ok(plan) => plan,
        Err(refusal) => return Ok(Err(refusal)),
    };
    order.extend(plan.steps.iter().map(|step| step.spec.id.clone()));

    let mut reader = Reader {
        store,
        cache: HashMap::new(),
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
                scratch.path(),
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
                Ok(entry) => staged.push((path.clone(), entry)),
                Err(reason) => conflicts.push(CompositionConflict {
                    path: String::from_utf8_lossy(path).into_owned(),
                    input: step.retained.result.commit.clone(),
                    reason,
                }),
            }
        }
        if !conflicts.is_empty() {
            return Ok(Ok(Built::Other(CompositionOutcome::Conflict {
                baseline: plan.baseline.commit.clone(),
                conflicts,
            })));
        }
        for (path, entry) in staged {
            match entry {
                Some(entry) => accumulator.insert(path, entry),
                None => accumulator.remove(&path),
            };
        }
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
            profile: PROVISIONAL_TEXT_PROFILE.to_string(),
        },
        mode: CompositionMode::Text,
    };
    Ok(Ok(Built::Candidate(candidate, plan)))
}

// ---------------------------------------------------------------- planning

fn plan(
    store: &CollaborationStore,
    request: &CompositionRequest,
    deliveries: &[String],
    usage: &mut BudgetUsage,
) -> Result<Result<Plan, Refused>, ComposeError> {
    let catalog = catalog(request)?;

    // Deliveries in policy order; a repeat is the same contribution.
    let mut selected: Vec<&ContributionSpec> = Vec::new();
    for id in deliveries {
        let Some(spec) = catalog.get(id.as_str()) else {
            return Ok(Err(refused(
                Refusal::MissingInput,
                format!("{id} is not a known contribution"),
            )));
        };
        if !selected.iter().any(|known| known.id == spec.id) {
            selected.push(spec);
        }
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
    let is_selected = |id: &str| selected.iter().any(|spec| spec.id == id);

    // Revisions of one contribution share a line, named by its first
    // revision.
    let line = |id: &str| -> String {
        let mut current = id;
        let mut seen = BTreeSet::new();
        while let Some(previous) = catalog
            .get(current)
            .and_then(|spec| spec.revision_of.as_deref())
        {
            if !seen.insert(current) {
                break;
            }
            current = previous;
        }
        current.to_string()
    };
    let mut lines: BTreeMap<String, &str> = BTreeMap::new();
    for spec in &selected {
        if let Some(other) = lines.insert(line(&spec.id), &spec.id) {
            return Ok(Err(refused(
                Refusal::CompetingRevisions,
                format!("{other} and {} are revisions of one contribution", spec.id),
            )));
        }
    }
    for spec in &selected {
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
    for spec in &selected {
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
    let groups: BTreeSet<&str> = selected
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
    let Some(baseline) = collaboration_archive::retained(store, &request.baseline)? else {
        return Ok(Err(refused(
            Refusal::MissingInput,
            format!("baseline {} is not retained", request.baseline),
        )));
    };
    let mut retained = Vec::with_capacity(selected.len());
    for spec in &selected {
        let Some(contribution) =
            collaboration_archive::retained_contribution(store, &spec.lineage)?
        else {
            return Ok(Err(refused(
                Refusal::MissingInput,
                format!("{} ({}) is not retained", spec.id, spec.lineage),
            )));
        };
        retained.push(contribution);
    }

    // Lineage: each base is the baseline or exactly a selected result.
    let mut edges: Vec<BTreeSet<usize>> = vec![BTreeSet::new(); selected.len()];
    for (index, contribution) in retained.iter().enumerate() {
        let base = &contribution.base.snapshot_id;
        if *base == request.baseline {
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
            if let Some(known) = collaboration_archive::retained_contribution(store, &spec.lineage)?
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
                "{} starts from {base}, which is neither the baseline nor a known result",
                selected[index].id
            ),
        )));
    }
    for (index, spec) in selected.iter().enumerate() {
        for required in &spec.requires {
            if let Some(parent) = selected.iter().position(|other| other.id == *required) {
                edges[index].insert(parent);
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
        engine_version: git_version()?,
    }))
}

// ----------------------------------------------------------------- reading

struct Reader<'a> {
    store: &'a CollaborationStore,
    cache: HashMap<[u8; 32], Vec<u8>>,
    manifests: HashMap<SourceSnapshotId, Entries>,
    /// Bytes read from the archive so far.
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
        self.bytes += bytes.len() as u64;
        self.cache.insert(*digest, bytes.clone());
        Ok(bytes)
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

type Entry = (EntryKind, [u8; 32]);

/// Apply one contribution's change to one path: `Ok(Ok(new entry or
/// removal))`, `Ok(Err(reason))` for a conflict.
fn apply_path(
    reader: &mut Reader<'_>,
    scratch: &Path,
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
    if moves_a_block(&base_text, &current_text) || moves_a_block(&base_text, &result_text) {
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
    let Some(merged) = merge_file(scratch, &base_text, &current_text, &result_text)? else {
        return Ok(Ok(Err(ConflictReason::Content)));
    };
    let digest: [u8; 32] = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&merged).into()
    };
    reader.cache.insert(digest, merged);
    Ok(Ok(Ok(Some((kind, digest)))))
}

/// Git's own test: a NUL in the first 8000 bytes.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|byte| *byte == 0)
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
) -> Result<Option<Vec<u8>>, ComposeError> {
    let write = |name: &str, bytes: &[u8]| -> Result<PathBuf, ComposeError> {
        let path = scratch.join(name);
        std::fs::write(&path, bytes).map_err(|source| io(&path, source))?;
        Ok(path)
    };
    let current = write("current", current)?;
    let base = write("base", base)?;
    let result = write("result", result)?;
    let output = engine()
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
        .stdin(Stdio::null())
        .output()
        .map_err(|source| io(Path::new("git"), source))?;
    match output.status.code() {
        Some(0) => Ok(Some(output.stdout)),
        Some(code) if (1..=127).contains(&code) => Ok(None),
        _ => Err(ComposeError::Git(format!(
            "merge-file failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// `git` with no user or system configuration, so a configured merge driver,
/// conflict style or attribute cannot change what the profile does.
fn engine() -> Command {
    let mut command = Command::new("git");
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE");
    command
}

fn git_version() -> Result<String, ComposeError> {
    let output = engine()
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .map_err(|source| io(Path::new("git"), source))?;
    if !output.status.success() {
        return Err(ComposeError::Git("git --version failed".into()));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
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

    let mut oids: BTreeMap<Vec<u8>, (EntryKind, String)> = BTreeMap::new();
    for (path, (kind, digest)) in entries {
        if baseline_entries.get(path) == Some(&(*kind, *digest))
            && let Some(oid) = known.get(path)
        {
            oids.insert(path.clone(), (*kind, oid.clone()));
            continue;
        }
        let bytes = reader.blob(digest)?;
        let file = scratch.join("blob");
        std::fs::write(&file, &bytes).map_err(|source| io(&file, source))?;
        let oid = repo_git(
            repo,
            &["hash-object", "-w", "--no-filters", "--"],
            Some(&file),
        )?;
        oids.insert(path.clone(), (*kind, text_of(&oid)));
    }
    let tree = write_tree(repo, &oids, b"")?;
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

fn write_tree(
    repo: &Path,
    entries: &BTreeMap<Vec<u8>, (EntryKind, String)>,
    prefix: &[u8],
) -> Result<String, ComposeError> {
    let mut lines: Vec<u8> = Vec::new();
    let mut subdirectories: BTreeSet<Vec<u8>> = BTreeSet::new();
    for (path, (kind, oid)) in entries.range(prefix.to_vec()..) {
        let Some(rest) = path.strip_prefix(prefix) else {
            break;
        };
        match rest.iter().position(|byte| *byte == b'/') {
            Some(slash) => {
                subdirectories.insert(rest[..slash].to_vec());
            }
            None => {
                lines.extend_from_slice(format!("{} blob {oid}\t", kind.git_mode()).as_bytes());
                lines.extend_from_slice(rest);
                lines.push(0);
            }
        }
    }
    for name in subdirectories {
        let mut child = prefix.to_vec();
        child.extend_from_slice(&name);
        child.push(b'/');
        let oid = write_tree(repo, entries, &child)?;
        lines.extend_from_slice(format!("040000 tree {oid}\t").as_bytes());
        lines.extend_from_slice(&name);
        lines.push(0);
    }
    let output = run(
        collaboration_archive::git(repo).args(["mktree", "-z"]),
        Some(&lines),
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
        .args(["-c", "commit.gpgsign=false"])
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

fn recipe_record(
    candidate: &Candidate,
    plan: &Plan,
    request: &CompositionRequest,
    deliveries: &[String],
    usage: BudgetUsage,
    recomposition: Option<&Subtraction>,
) -> Result<(Vec<u8>, RecordId), ComposeError> {
    let order = plan
        .steps
        .iter()
        .map(|step| {
            object(vec![
                ("id", text(&step.spec.id)),
                ("lineage", text(step.retained.lineage_record_id.as_str())),
                ("base", text(step.retained.base.snapshot_id.as_str())),
                ("result", text(step.retained.result.snapshot_id.as_str())),
            ])
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
        ("profile", text(PROVISIONAL_TEXT_PROFILE)),
        (
            "engine",
            object(vec![
                ("name", text("git merge-file")),
                ("version", text(&plan.engine_version)),
                ("move_window", integer(MOVE_WINDOW)),
            ]),
        ),
        ("baseline", text(plan.baseline.snapshot_id.as_str())),
        ("baseline_commit", text(plan.baseline.commit.as_str())),
        ("candidate", text(candidate.subject.as_str())),
        ("candidate_commit", text(candidate.commit.as_str())),
        ("order", Value::Array(order)),
        ("deliveries", integer(deliveries.len())),
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
    if let Some(subtraction) = recomposition {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn binary_is_git_s_nul_test() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text\n"));
        let mut late = vec![b'a'; 8000];
        late.push(0);
        assert!(!is_binary(&late));
    }
}
