//! X2 prototype (#684): one immutable analysed base plus one private overlay
//! for a retained candidate snapshot (plan §6.14–6.16; D42, D43, D49).
//!
//! This is an experiment, not a product surface: a library API only, built
//! to answer whether private incremental views agree with fresh analysis and
//! what they cost. Decision record: `local-v3-x2-private-views.md`.
//!
//! ```text
//! <collaboration project dir>/views/<view>/
//!   .build.lock          exclusive while a generation is being built
//!   CURRENT              "gen-<n>": the published generation
//!   RETIRED              present once the view is retired
//!   gen-<n>/             sealed: never modified after publication
//!     manifest.json      identity, base reference, mask, replacements, costs
//!     facts/.aethyme/graph/   linked fragments for this snapshot
//!     extractions/       per-unit extractions (all for a base, replacements
//!                        only for an overlay)
//!     .pin               readers hold it shared; reclamation needs it exclusive
//!   building-<n>-<pid>/  in progress; never read
//! ```
//!
//! - **Facts.** The structural indexer's per-file extraction depends only on
//!   the repository name, the unit (path, language, content) and the indexer
//!   profile; nothing else is read. An overlay reuses the base's extraction
//!   for every unit whose content key matches, and extracts the rest (its
//!   **replacements**). Base units without a matching key are its **mask**.
//! - **Derived facts.** Non-code relationships and linking are cross-file:
//!   they resolve names against the whole set, including names that were
//!   absent. They are always recomputed over the merged set. That
//!   over-invalidates on purpose (§6.15 allows it, never the reverse), so a
//!   renamed export, a new declaration or a deleted target can never leave a
//!   stale or ghost link behind.
//! - **Isolation.** Nothing here writes a repository, its canonical
//!   `.aethyme/graph` fragments or producer `_overlays`, or calls the graph
//!   refresh, so the active-session refresh guard is never bypassed: it is
//!   never reached. The source analysed is a retained snapshot materialised
//!   inside the view's own build directory.
//! - **Generations.** A generation is built in a private directory, sealed
//!   with its manifest, published by one rename, and only then named by
//!   `CURRENT` (temporary file plus rename). Readers pin the generation they
//!   opened, so a crash, a newer generation or reclamation never shows a
//!   reader a mix.
//! - **Depth.** One base plus one overlay. An overlay on an overlay is
//!   refused; flattening means building a new base (a fresh analysis).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use aethyme_contracts::experimental_v0::analysis::ANALYSIS_PROFILE_SCHEMA;
use aethyme_contracts::experimental_v0::analysis::{
    AnalysisEnvelope, Coverage, Freshness, Limits, Operation, Outcome, ProfileRef, Subject,
    profile_id,
};
use aethyme_contracts::experimental_v0::canonical_json::{Object, Value};
use aethyme_contracts::experimental_v0::{Record, SourceSnapshotId};
use aethyme_graph_indexer::{
    CachedExtraction, ExtractionCache, ExtractionUnit, IndexerContext, WalkOptions,
    default_registry, index_repo_to_disk_cached, link_repo,
};
use aethyme_graph_schema::{EdgeKind, NodeId, NodeKind};
use aethyme_graph_storage::{
    CoverageFileStatus, ExclusionReason, FragmentStore, read_fragment_bytes, write_fragment_bytes,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::collaboration_state::CollaborationStore;

/// The manifest schema of this prototype.
pub const VIEW_SCHEMA: &str = "aethyme.analysis-view/x2-prototype-v0";
const PRODUCER: &str = "aethyme-structural-indexer";
const VIEWS_DIR: &str = "views";
/// Languages whose parser is part of the profile.
const PROFILE_LANGUAGES: &[&str] = &["javascript", "php", "python", "rust", "typescript"];
/// Edge kinds the queries traverse.
const QUERY_EDGES: &[EdgeKind] = &[EdgeKind::Calls, EdgeKind::Imports, EdgeKind::References];

/// What produced a view's facts. Two views compare only under equal
/// profiles; a base under another profile is never reused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewProfile {
    pub producer: String,
    pub producer_version: String,
    pub repo_name: String,
    /// Language → parser, as the indexer registry reports it.
    pub parsers: BTreeMap<String, String>,
}

impl ViewProfile {
    /// This binary's structural indexer for `repo_name`.
    pub fn current(repo_name: &str) -> Self {
        Self::with_version(repo_name, env!("CARGO_PKG_VERSION"))
    }

    /// The same profile under another producer version (an upgrade, T90).
    pub fn with_version(repo_name: &str, producer_version: &str) -> Self {
        let registry = default_registry();
        Self {
            producer: PRODUCER.into(),
            producer_version: producer_version.into(),
            repo_name: repo_name.into(),
            parsers: PROFILE_LANGUAGES
                .iter()
                .filter_map(|language| {
                    registry
                        .parser_for(language)
                        .map(|parser| ((*language).to_string(), parser.to_string()))
                })
                .collect(),
        }
    }

    pub fn digest(&self) -> String {
        hex(&Sha256::digest(
            serde_json::to_vec(self).expect("a profile serializes"),
        ))
    }

    /// The pinned AQ0 profile record for this profile.
    pub fn reference(&self) -> ProfileRef {
        let body = serde_json::json!({
            "schema": ANALYSIS_PROFILE_SCHEMA.name,
            "producer": self.producer,
            "producer_version": self.producer_version,
            "schema_version": VIEW_SCHEMA,
            "languages": self.parsers.keys().collect::<Vec<_>>(),
            "edge_kinds": QUERY_EDGES.iter().map(|kind| kind.name()).collect::<Vec<_>>(),
            "configuration_digest": self.digest(),
        });
        let bytes = serde_json::to_vec(&body).expect("a profile record serializes");
        let record = Record::decode(&bytes, &[&ANALYSIS_PROFILE_SCHEMA])
            .expect("the profile record is valid by construction");
        ProfileRef::Pinned(profile_id(&record).expect("an analysis profile record"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewKind {
    /// A sealed full analysis that overlays may build on.
    Base,
    /// One private overlay on a base.
    Overlay,
    /// An isolated full analysis: the fallback, and AQ2's reference.
    Fresh,
}

/// A view's published state (§6.16). Building and failed generations are
/// never published, so a reader only ever sees these two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewState {
    Ready,
    /// Published, but some files were not fully analysed.
    Partial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseRef {
    pub view: String,
    pub generation: u64,
    pub snapshot: String,
    pub manifest_sha256: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewCosts {
    pub build_us: u128,
    /// Materialising the retained snapshot.
    pub materialize_us: u128,
    /// Walking, reading and extracting (parsing only the replacements).
    pub index_us: u128,
    /// Linking: always over the whole snapshot.
    pub link_us: u128,
    pub units_extracted: usize,
    pub units_reused: usize,
    pub fact_bytes: u64,
    pub extraction_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewManifest {
    pub schema: String,
    pub view: String,
    pub kind: ViewKind,
    pub generation: u64,
    pub snapshot: String,
    pub profile: ViewProfile,
    pub profile_digest: String,
    pub base: Option<BaseRef>,
    /// Every extracted unit: path → content key.
    pub units: BTreeMap<String, String>,
    /// Base units this view does not reuse (changed, moved or deleted).
    pub mask: Vec<String>,
    /// Units this view extracted itself.
    pub replacements: Vec<String>,
    pub state: ViewState,
    pub coverage_mode: String,
    pub safe_to_use: bool,
    pub coverage_gaps: Vec<String>,
    /// SHA-256 over every linked fragment, in path order.
    pub fragment_set_sha256: String,
    pub costs: ViewCosts,
}

#[derive(Debug, thiserror::Error)]
pub enum ViewError {
    #[error("view {view} does not exist or has no published generation")]
    NotFound { view: String },
    #[error("view {view} is retired")]
    Retired { view: String },
    #[error(
        "view {base} is itself an overlay; one base plus one overlay is the limit, so build a \
         new base (a fresh analysis) instead"
    )]
    UnsupportedStacking { base: String },
    #[error(
        "base {base} was analysed under another profile ({found}); its facts cannot be reused \
         under {expected}"
    )]
    IncompatibleProfile {
        base: String,
        expected: String,
        found: String,
    },
    #[error("view {view} has {dependents} dependent overlay(s); retire them first")]
    HasDependents { view: String, dependents: usize },
    #[error("view {view} generation {generation} is pinned by a reader")]
    Pinned { view: String, generation: u64 },
    #[error("snapshot {snapshot} could not be materialised: {source}")]
    Source {
        snapshot: String,
        source: crate::collaboration_archive::ArchiveError,
    },
    #[error("indexing failed: {0}")]
    Index(String),
    #[error("corrupt view {view}: {detail}")]
    Corrupt { view: String, detail: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[cfg(test)]
    #[error("injected crash at {0:?}")]
    Injected(Fault),
}

impl ViewError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "not_found",
            Self::Retired { .. } => "retired",
            Self::UnsupportedStacking { .. } => "unsupported_stacking",
            Self::IncompatibleProfile { .. } => "incompatible_analysis_profile",
            Self::HasDependents { .. } => "has_dependents",
            Self::Pinned { .. } => "pinned",
            Self::Source { .. } => "source_unavailable",
            Self::Index(_) => "index_failed",
            Self::Corrupt { .. } => "corrupt_view",
            Self::Io { .. } => "io",
            #[cfg(test)]
            Self::Injected(_) => "injected",
        }
    }
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ViewError + '_ {
    move |source| ViewError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Points at which a test can stop a build, as a crash would.
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    AfterIndex,
    AfterLink,
    BeforePublish,
    BeforeCurrent,
}

/// How to build a view.
#[derive(Debug, Clone)]
pub struct ViewOptions {
    pub profile: ViewProfile,
    #[cfg(test)]
    pub fault: Option<Fault>,
}

impl ViewOptions {
    pub fn new(profile: ViewProfile) -> Self {
        Self {
            profile,
            #[cfg(test)]
            fault: None,
        }
    }

    #[cfg(test)]
    fn check(&self, at: Fault) -> Result<(), ViewError> {
        if self.fault == Some(at) {
            return Err(ViewError::Injected(at));
        }
        Ok(())
    }
}

macro_rules! fault {
    ($options:expr, $at:ident) => {
        #[cfg(test)]
        $options.check(Fault::$at)?;
    };
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn short(id: &SourceSnapshotId) -> &str {
    let digest = id.as_str().trim_start_matches("sha256:");
    &digest[..16]
}

fn views_root(store: &CollaborationStore) -> PathBuf {
    store.project_dir().join(VIEWS_DIR)
}

/// The name of `kind`'s view of `snapshot` under `profile`.
pub fn view_name(
    kind: ViewKind,
    snapshot: &SourceSnapshotId,
    profile: &ViewProfile,
    base: Option<&SourceSnapshotId>,
) -> String {
    let profile = &profile.digest()[..16];
    match (kind, base) {
        (ViewKind::Overlay, Some(base)) => {
            format!("overlay-{}-{profile}-on-{}", short(snapshot), short(base))
        }
        (ViewKind::Base, _) => format!("base-{}-{profile}", short(snapshot)),
        _ => format!("fresh-{}-{profile}", short(snapshot)),
    }
}

// ----------------------------------------------------------------- locks

fn open_lock(path: &Path) -> Result<File, ViewError> {
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .map_err(io(path))
}

/// Try to hold `path` exclusively; `None` while anyone holds it.
fn try_exclusive(path: &Path) -> Result<Option<File>, ViewError> {
    let file = open_lock(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(source)) => Err(ViewError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

// ------------------------------------------------------------ generations

fn generations(view_dir: &Path) -> Result<Vec<u64>, ViewError> {
    let mut found = Vec::new();
    let entries = match std::fs::read_dir(view_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(found),
        Err(source) => {
            return Err(ViewError::Io {
                path: view_dir.to_path_buf(),
                source,
            });
        }
    };
    for entry in entries {
        let entry = entry.map_err(io(view_dir))?;
        if let Some(n) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.strip_prefix("gen-"))
            .and_then(|n| n.parse().ok())
        {
            found.push(n);
        }
    }
    found.sort_unstable();
    Ok(found)
}

fn current_generation(view_dir: &Path) -> Result<Option<u64>, ViewError> {
    let path = view_dir.join("CURRENT");
    match std::fs::read_to_string(&path) {
        Ok(text) => text
            .trim()
            .strip_prefix("gen-")
            .and_then(|n| n.parse().ok())
            .map(Some)
            .ok_or_else(|| ViewError::Corrupt {
                view: view_dir.display().to_string(),
                detail: format!("CURRENT holds {text:?}"),
            }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(ViewError::Io { path, source }),
    }
}

fn read_manifest(dir: &Path) -> Result<ViewManifest, ViewError> {
    let path = dir.join("manifest.json");
    let bytes = std::fs::read(&path).map_err(io(&path))?;
    serde_json::from_slice(&bytes).map_err(|error| ViewError::Corrupt {
        view: dir.display().to_string(),
        detail: error.to_string(),
    })
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), ViewError> {
    let mut file = File::create(path).map_err(io(path))?;
    file.write_all(bytes).map_err(io(path))?;
    file.sync_all().map_err(io(path))
}

/// Every file under `root`, with its bytes, in path order.
fn files_under(root: &Path) -> Result<Vec<(PathBuf, u64)>, ViewError> {
    let mut out = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(ViewError::Io { path: dir, source }),
        };
        for entry in entries {
            let entry = entry.map_err(io(&dir))?;
            let kind = entry.file_type().map_err(io(&dir))?;
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                let len = entry.metadata().map_err(io(&dir))?.len();
                out.push((entry.path(), len));
            }
        }
    }
    out.sort();
    Ok(out)
}

// ---------------------------------------------------- extraction reuse

#[derive(Serialize, Deserialize)]
struct ExtractionMeta {
    status: CoverageFileStatus,
    exclusion_reason: Option<ExclusionReason>,
}

/// Reuses a base's extractions by content key and records the rest.
struct ViewCache {
    profile_digest: String,
    repo_name: String,
    base: Option<(PathBuf, BTreeSet<String>)>,
    out: Option<PathBuf>,
    /// path → (key, reused)
    units: Mutex<BTreeMap<String, (String, bool)>>,
    errors: Mutex<Vec<String>>,
}

impl ViewCache {
    fn key(&self, unit: &ExtractionUnit<'_>) -> String {
        let mut hasher = Sha256::new();
        for part in [
            self.profile_digest.as_bytes(),
            self.repo_name.as_bytes(),
            unit.source_path.as_bytes(),
            unit.language.as_bytes(),
            unit.content.as_bytes(),
        ] {
            hasher.update((part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
        hex(&hasher.finalize())
    }

    fn load(dir: &Path, key: &str) -> Option<CachedExtraction> {
        let fragment =
            read_fragment_bytes(&std::fs::read(dir.join(format!("{key}.frag"))).ok()?).ok()?;
        let meta: ExtractionMeta =
            serde_json::from_slice(&std::fs::read(dir.join(format!("{key}.json"))).ok()?).ok()?;
        Some(CachedExtraction {
            fragment,
            status: meta.status,
            exclusion_reason: meta.exclusion_reason,
        })
    }

    fn note(&self, unit: &ExtractionUnit<'_>, key: String, reused: bool) {
        self.units
            .lock()
            .expect("unit map")
            .insert(unit.source_path.to_string(), (key, reused));
    }
}

impl ExtractionCache for ViewCache {
    fn lookup(&self, unit: &ExtractionUnit<'_>) -> Option<CachedExtraction> {
        let (dir, keys) = self.base.as_ref()?;
        let key = self.key(unit);
        if !keys.contains(&key) {
            return None;
        }
        // An unreadable base extraction is extracted again, never trusted.
        let hit = Self::load(dir, &key)?;
        self.note(unit, key, true);
        Some(hit)
    }

    fn record(&self, unit: &ExtractionUnit<'_>, extraction: &CachedExtraction) {
        let key = self.key(unit);
        if let Some(out) = &self.out {
            let written = write_fragment_bytes(&extraction.fragment)
                .map_err(|error| error.to_string())
                .and_then(|bytes| {
                    std::fs::write(out.join(format!("{key}.frag")), bytes)
                        .map_err(|error| error.to_string())
                })
                .and_then(|()| {
                    let meta = ExtractionMeta {
                        status: extraction.status,
                        exclusion_reason: extraction.exclusion_reason,
                    };
                    std::fs::write(
                        out.join(format!("{key}.json")),
                        serde_json::to_vec(&meta).expect("meta serializes"),
                    )
                    .map_err(|error| error.to_string())
                });
            if let Err(error) = written {
                self.errors
                    .lock()
                    .expect("error list")
                    .push(format!("{}: {error}", unit.source_path));
            }
        }
        self.note(unit, key, false);
    }
}

// ------------------------------------------------------------------ build

/// A published, pinned generation.
#[derive(Debug)]
pub struct ViewReader {
    dir: PathBuf,
    manifest: ViewManifest,
    _pin: File,
}

struct BuildPlan<'a> {
    kind: ViewKind,
    snapshot: &'a SourceSnapshotId,
    base: Option<&'a ViewReader>,
    options: &'a ViewOptions,
}

/// Build and publish a new generation of `plan`'s view.
fn build(store: &CollaborationStore, plan: BuildPlan<'_>) -> Result<String, ViewError> {
    let started = Instant::now();
    let profile = &plan.options.profile;
    let base_snapshot = plan
        .base
        .map(|base| SourceSnapshotId::parse(&base.manifest.snapshot))
        .transpose()
        .map_err(|error| ViewError::Corrupt {
            view: "base".into(),
            detail: error.to_string(),
        })?;
    let name = view_name(plan.kind, plan.snapshot, profile, base_snapshot.as_ref());
    let view_dir = views_root(store).join(&name);
    std::fs::create_dir_all(&view_dir).map_err(io(&view_dir))?;
    let lock_path = view_dir.join(".build.lock");
    let lock = open_lock(&lock_path)?;
    lock.lock().map_err(io(&lock_path))?;
    let _ = std::fs::remove_file(view_dir.join("RETIRED"));

    let generation = generations(&view_dir)?.last().copied().unwrap_or(0) + 1;
    let building = view_dir.join(format!("building-{generation}-{}", std::process::id()));
    if building.exists() {
        std::fs::remove_dir_all(&building).map_err(io(&building))?;
    }
    std::fs::create_dir_all(&building).map_err(io(&building))?;

    // The retained snapshot, materialised privately. Its own `.aethyme/`
    // is not source: the walker skips it and the indexer writes there.
    let source = building.join("source");
    crate::collaboration_archive::reconstruct(store, plan.snapshot, &source).map_err(|source| {
        ViewError::Source {
            snapshot: plan.snapshot.to_string(),
            source,
        }
    })?;
    let materialize_us = started.elapsed().as_micros();
    let committed_graph = source.join(".aethyme");
    if committed_graph.exists() {
        std::fs::remove_dir_all(&committed_graph).map_err(io(&committed_graph))?;
    }

    let extractions = building.join("extractions");
    std::fs::create_dir_all(&extractions).map_err(io(&extractions))?;
    let cache = ViewCache {
        profile_digest: profile.digest(),
        repo_name: profile.repo_name.clone(),
        base: plan.base.map(|base| {
            (
                base.dir.join("extractions"),
                base.manifest.units.values().cloned().collect(),
            )
        }),
        out: (plan.kind != ViewKind::Fresh).then(|| extractions.clone()),
        units: Mutex::new(BTreeMap::new()),
        errors: Mutex::new(Vec::new()),
    };
    let index_started = Instant::now();
    let ctx = IndexerContext::new(&profile.repo_name, &source, &profile.producer_version)
        .map_err(|error| ViewError::Index(error.to_string()))?;
    let summary = index_repo_to_disk_cached(
        &ctx,
        &WalkOptions::default(),
        &default_registry(),
        Some(&cache),
    )
    .map_err(|error| ViewError::Index(error.to_string()))?;
    let errors = cache.errors.into_inner().expect("error list");
    if !errors.is_empty() {
        return Err(ViewError::Index(errors.join("; ")));
    }
    let index_us = index_started.elapsed().as_micros();
    fault!(plan.options, AfterIndex);
    let link_started = Instant::now();
    link_repo(&ctx).map_err(|error| ViewError::Index(error.to_string()))?;
    let link_us = link_started.elapsed().as_micros();
    fault!(plan.options, AfterLink);

    // Keep the linked facts; drop the materialised source.
    let facts = building.join("facts");
    std::fs::create_dir_all(facts.join(".aethyme")).map_err(io(&facts))?;
    let graph = source.join(".aethyme/graph");
    std::fs::rename(&graph, facts.join(".aethyme/graph")).map_err(io(&graph))?;
    std::fs::remove_dir_all(&source).map_err(io(&source))?;

    let mut set = Sha256::new();
    let mut fact_bytes = 0;
    let fact_root = facts.join(".aethyme/graph");
    for (path, len) in files_under(&fact_root)? {
        let relative = path.strip_prefix(&fact_root).unwrap_or(&path);
        let bytes = std::fs::read(&path).map_err(io(&path))?;
        set.update((relative.as_os_str().len() as u64).to_le_bytes());
        set.update(relative.as_os_str().as_encoded_bytes());
        set.update((bytes.len() as u64).to_le_bytes());
        set.update(&bytes);
        fact_bytes += len;
    }
    let extraction_bytes = files_under(&extractions)?.iter().map(|(_, len)| len).sum();

    let units_seen = cache.units.into_inner().expect("unit map");
    let units: BTreeMap<String, String> = units_seen
        .iter()
        .map(|(path, (key, _))| (path.clone(), key.clone()))
        .collect();
    let replacements: Vec<String> = units_seen
        .iter()
        .filter(|(_, (_, reused))| !reused)
        .map(|(path, _)| path.clone())
        .collect();
    let mask: Vec<String> = plan
        .base
        .map(|base| {
            base.manifest
                .units
                .iter()
                .filter(|(path, key)| units.get(*path) != Some(*key))
                .map(|(path, _)| path.clone())
                .collect()
        })
        .unwrap_or_default();
    let report = &summary.coverage.report;
    let manifest = ViewManifest {
        schema: VIEW_SCHEMA.into(),
        view: name.clone(),
        kind: plan.kind,
        generation,
        snapshot: plan.snapshot.to_string(),
        profile: profile.clone(),
        profile_digest: profile.digest(),
        base: plan.base.map(|base| BaseRef {
            view: base.manifest.view.clone(),
            generation: base.manifest.generation,
            snapshot: base.manifest.snapshot.clone(),
            manifest_sha256: hex(&Sha256::digest(
                std::fs::read(base.dir.join("manifest.json")).unwrap_or_default(),
            )),
        }),
        units,
        mask,
        replacements: replacements.clone(),
        state: if report.safe_to_use && report.coverage_mode == "complete" {
            ViewState::Ready
        } else {
            ViewState::Partial
        },
        coverage_mode: report.coverage_mode.clone(),
        safe_to_use: report.safe_to_use,
        coverage_gaps: report.gaps.clone(),
        fragment_set_sha256: hex(&set.finalize()),
        costs: ViewCosts {
            build_us: started.elapsed().as_micros(),
            materialize_us,
            index_us,
            link_us,
            units_extracted: replacements.len(),
            units_reused: units_seen.values().filter(|(_, reused)| *reused).count(),
            fact_bytes,
            extraction_bytes,
        },
    };
    write_synced(
        &building.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest).expect("a manifest serializes"),
    )?;
    // Readers pin through this file; it exists before the generation does.
    write_synced(&building.join(".pin"), b"")?;
    sync_dir(&building)?;
    fault!(plan.options, BeforePublish);

    // Publish: one rename makes the sealed generation visible as a whole.
    let published = view_dir.join(format!("gen-{generation}"));
    std::fs::rename(&building, &published).map_err(io(&published))?;
    sync_dir(&view_dir)?;
    fault!(plan.options, BeforeCurrent);
    let pointer = view_dir.join("CURRENT.tmp");
    write_synced(&pointer, format!("gen-{generation}\n").as_bytes())?;
    std::fs::rename(&pointer, view_dir.join("CURRENT")).map_err(io(&pointer))?;
    sync_dir(&view_dir)?;
    drop(lock);
    Ok(name)
}

fn sync_dir(dir: &Path) -> Result<(), ViewError> {
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(io(dir))
}

/// Build a base: a sealed full analysis whose extractions overlays reuse.
pub fn build_base(
    store: &CollaborationStore,
    snapshot: &SourceSnapshotId,
    options: &ViewOptions,
) -> Result<String, ViewError> {
    build(
        store,
        BuildPlan {
            kind: ViewKind::Base,
            snapshot,
            base: None,
            options,
        },
    )
}

/// An isolated full analysis of `snapshot`, reusing nothing.
pub fn build_fresh(
    store: &CollaborationStore,
    snapshot: &SourceSnapshotId,
    options: &ViewOptions,
) -> Result<String, ViewError> {
    build(
        store,
        BuildPlan {
            kind: ViewKind::Fresh,
            snapshot,
            base: None,
            options,
        },
    )
}

/// One private overlay for `snapshot` on the base view `base`.
pub fn build_overlay(
    store: &CollaborationStore,
    base: &str,
    snapshot: &SourceSnapshotId,
    options: &ViewOptions,
) -> Result<String, ViewError> {
    // The base stays pinned while its extractions are read.
    let base = open_view(store, base)?;
    if base.manifest.kind == ViewKind::Overlay {
        return Err(ViewError::UnsupportedStacking {
            base: base.manifest.view.clone(),
        });
    }
    if base.manifest.profile != options.profile {
        return Err(ViewError::IncompatibleProfile {
            base: base.manifest.view.clone(),
            expected: options.profile.digest(),
            found: base.manifest.profile_digest.clone(),
        });
    }
    build(
        store,
        BuildPlan {
            kind: ViewKind::Overlay,
            snapshot,
            base: Some(&base),
            options,
        },
    )
}

/// How a view for a candidate was obtained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewOutcome {
    Incremental {
        view: String,
    },
    /// The overlay could not be built on that base; this is a fresh analysis.
    Fresh {
        view: String,
        reason: &'static str,
    },
}

/// An overlay on `base` when it can be built, otherwise a fresh analysis.
/// Stacking, a profile mismatch or a missing base never yield a guessed
/// incremental answer.
pub fn view_for_candidate(
    store: &CollaborationStore,
    base: &str,
    snapshot: &SourceSnapshotId,
    options: &ViewOptions,
) -> Result<ViewOutcome, ViewError> {
    match build_overlay(store, base, snapshot, options) {
        Ok(view) => Ok(ViewOutcome::Incremental { view }),
        Err(
            error @ (ViewError::UnsupportedStacking { .. }
            | ViewError::IncompatibleProfile { .. }
            | ViewError::NotFound { .. }
            | ViewError::Retired { .. }),
        ) => Ok(ViewOutcome::Fresh {
            view: build_fresh(store, snapshot, options)?,
            reason: error.code(),
        }),
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------- readers

/// Pin `view`'s published generation for reading.
pub fn open_view(store: &CollaborationStore, view: &str) -> Result<ViewReader, ViewError> {
    let view_dir = views_root(store).join(view);
    if view_dir.join("RETIRED").exists() {
        return Err(ViewError::Retired { view: view.into() });
    }
    // CURRENT may move, and an old generation may be reclaimed, between
    // reading the pointer and pinning; pin, then confirm it still exists.
    for _ in 0..3 {
        let Some(generation) = current_generation(&view_dir)? else {
            return Err(ViewError::NotFound { view: view.into() });
        };
        let dir = view_dir.join(format!("gen-{generation}"));
        let pin_path = dir.join(".pin");
        // Opened, never created: a sealed generation is not modified.
        let Ok(pin) = File::open(&pin_path) else {
            continue;
        };
        pin.lock_shared().map_err(io(&pin_path))?;
        if !dir.join("manifest.json").is_file() {
            continue;
        }
        let manifest = read_manifest(&dir)?;
        return Ok(ViewReader {
            dir,
            manifest,
            _pin: pin,
        });
    }
    Err(ViewError::NotFound { view: view.into() })
}

/// What a query found, before it is wrapped in the AQ0 envelope.
struct Answer {
    complete: bool,
    truncated: bool,
    gaps: Vec<String>,
    result: Value,
}

struct Facts {
    file_of: BTreeMap<NodeId, String>,
    name_of: BTreeMap<NodeId, (NodeKind, Option<String>)>,
    /// target → (source, kind)
    incoming: BTreeMap<NodeId, Vec<(NodeId, EdgeKind)>>,
    by_file: BTreeMap<String, Vec<NodeId>>,
}

impl ViewReader {
    pub fn manifest(&self) -> &ViewManifest {
        &self.manifest
    }

    fn facts(&self) -> Result<Facts, ViewError> {
        let corrupt = |detail: String| ViewError::Corrupt {
            view: self.manifest.view.clone(),
            detail,
        };
        let store = FragmentStore::open(self.dir.join("facts"))
            .map_err(|error| corrupt(error.to_string()))?;
        let mut facts = Facts {
            file_of: BTreeMap::new(),
            name_of: BTreeMap::new(),
            incoming: BTreeMap::new(),
            by_file: BTreeMap::new(),
        };
        for path in store
            .list_indexed_source_paths()
            .map_err(|error| corrupt(error.to_string()))?
        {
            let fragment = store
                .read_fragment(&path)
                .map_err(|error| corrupt(error.to_string()))?;
            for node in fragment.nodes() {
                facts.file_of.insert(node.id().clone(), path.clone());
                facts.name_of.insert(
                    node.id().clone(),
                    (node.kind(), node.name().map(str::to_string)),
                );
                facts
                    .by_file
                    .entry(path.clone())
                    .or_default()
                    .push(node.id().clone());
            }
            for edge in fragment.edges() {
                if QUERY_EDGES.contains(&edge.kind()) {
                    facts
                        .incoming
                        .entry(edge.dst_id().clone())
                        .or_default()
                        .push((edge.src_id().clone(), edge.kind()));
                }
            }
        }
        for sources in facts.incoming.values_mut() {
            sources.sort();
            sources.dedup();
        }
        Ok(facts)
    }

    fn envelope(
        &self,
        operation: Operation,
        subject: Subject,
        base_subject: Option<Subject>,
        answer: Answer,
    ) -> AnalysisEnvelope {
        let Answer {
            complete,
            truncated,
            gaps,
            result,
        } = answer;
        let mut provenance = vec![
            ("view", text(&self.manifest.view)),
            ("kind", text(kind_name(self.manifest.kind))),
            ("generation", int(self.manifest.generation)),
            (
                "fragment_set_sha256",
                text(&self.manifest.fragment_set_sha256),
            ),
            (
                "mode",
                text(match self.manifest.kind {
                    ViewKind::Overlay => "incremental",
                    _ => "fresh",
                }),
            ),
        ];
        if let Some(base) = &self.manifest.base {
            provenance.push(("base_view", text(&base.view)));
            provenance.push(("base_generation", int(base.generation)));
        }
        AnalysisEnvelope {
            operation,
            subject,
            base_subject,
            profile: self.manifest.profile.reference(),
            outcome: Outcome::Available,
            // Facts were computed from the exact retained bytes.
            freshness: Freshness::Exact,
            coverage: if complete {
                Coverage::CompleteWithinProfile
            } else {
                Coverage::Partial
            },
            limits: if truncated {
                Limits::Truncated
            } else {
                Limits::WithinLimits
            },
            reason: None,
            gaps,
            heuristic_confidence: None,
            provenance: Some(object(provenance)),
            limit_detail: None,
            result: Some(result),
        }
    }

    fn snapshot(&self) -> Result<SourceSnapshotId, ViewError> {
        SourceSnapshotId::parse(&self.manifest.snapshot).map_err(|error| ViewError::Corrupt {
            view: self.manifest.view.clone(),
            detail: error.to_string(),
        })
    }

    fn view_gaps(&self) -> Vec<String> {
        let mut gaps: Vec<String> = self
            .manifest
            .coverage_gaps
            .iter()
            .map(|gap| format!("coverage:{gap}"))
            .collect();
        if self.manifest.state == ViewState::Partial {
            gaps.push("view_partial".into());
        }
        gaps
    }

    /// Every resolved reference (call, import, reference) to a symbol named
    /// `symbol`, at most `budget` of them. References that never resolved to
    /// a definition are counted as a gap, not dropped.
    pub fn find_references(
        &self,
        symbol: &str,
        budget: usize,
    ) -> Result<AnalysisEnvelope, ViewError> {
        let facts = self.facts()?;
        let named = |id: &NodeId| {
            facts
                .name_of
                .get(id)
                .is_some_and(|(_, name)| name.as_deref() == Some(symbol))
        };
        let mut references = BTreeSet::new();
        let mut unresolved = 0;
        for (target, sources) in &facts.incoming {
            if !named(target) {
                continue;
            }
            let is_placeholder = facts
                .name_of
                .get(target)
                .is_some_and(|(kind, _)| *kind == NodeKind::UnresolvedSymbol);
            for (source, kind) in sources {
                if is_placeholder {
                    unresolved += 1;
                    continue;
                }
                references.insert((
                    facts.file_of.get(source).cloned().unwrap_or_default(),
                    kind.name().to_string(),
                    facts.file_of.get(target).cloned().unwrap_or_default(),
                ));
            }
        }
        let truncated = references.len() > budget;
        let mut gaps = self.view_gaps();
        if unresolved > 0 {
            gaps.push(format!("unresolved_references:{unresolved}"));
        }
        let result = object([(
            "references",
            Value::Array(
                references
                    .iter()
                    .take(budget)
                    .map(|(from, kind, to)| {
                        object([("from", text(from)), ("kind", text(kind)), ("to", text(to))])
                    })
                    .collect(),
            ),
        )]);
        let complete = self.manifest.state == ViewState::Ready && unresolved == 0;
        Ok(self.envelope(
            Operation::FindReferences,
            Subject::Snapshot(self.snapshot()?),
            None,
            Answer {
                complete,
                truncated,
                gaps,
                result,
            },
        ))
    }

    /// Files whose symbols call, import or reference anything in
    /// `changed_paths`, transitively, visiting at most `budget` symbols.
    pub fn explain_impact(
        &self,
        changed_paths: &[&str],
        budget: usize,
    ) -> Result<AnalysisEnvelope, ViewError> {
        let facts = self.facts()?;
        let mut paths: Vec<&str> = changed_paths.to_vec();
        paths.sort_unstable();
        paths.dedup();
        let mut seen: BTreeSet<NodeId> = BTreeSet::new();
        let mut queue: VecDeque<NodeId> = VecDeque::new();
        for path in &paths {
            for id in facts.by_file.get(*path).into_iter().flatten() {
                if seen.insert(id.clone()) {
                    queue.push_back(id.clone());
                }
            }
        }
        let mut truncated = false;
        let mut impacted = BTreeSet::new();
        while let Some(id) = queue.pop_front() {
            for (source, _) in facts.incoming.get(&id).into_iter().flatten() {
                if seen.contains(source) {
                    continue;
                }
                if seen.len() >= budget {
                    truncated = true;
                    break;
                }
                seen.insert(source.clone());
                if let Some(file) = facts.file_of.get(source)
                    && !paths.contains(&file.as_str())
                {
                    impacted.insert(file.clone());
                }
                queue.push_back(source.clone());
            }
        }
        let mut digest = Sha256::new();
        for path in &paths {
            digest.update((path.len() as u64).to_le_bytes());
            digest.update(path.as_bytes());
        }
        let result = object([(
            "impacted",
            Value::Array(impacted.iter().map(|file| text(file)).collect()),
        )]);
        Ok(self.envelope(
            Operation::ExplainImpact,
            Subject::ChangedPaths(hex(&digest.finalize())),
            Some(Subject::Snapshot(self.snapshot()?)),
            Answer {
                complete: self.manifest.state == ViewState::Ready,
                truncated,
                gaps: self.view_gaps(),
                result,
            },
        ))
    }
}

fn kind_name(kind: ViewKind) -> &'static str {
    match kind {
        ViewKind::Base => "base",
        ViewKind::Overlay => "overlay",
        ViewKind::Fresh => "fresh",
    }
}

fn text(value: &str) -> Value {
    Value::String(value.to_string())
}

fn int(value: u64) -> Value {
    Value::Integer(i64::try_from(value).expect("generation numbers are small"))
}

fn object(members: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
    let members = members
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
    Value::Object(Object::new(members).expect("distinct keys"))
}

// -------------------------------------------------------------- lifecycle

/// What [`sweep`] removed and kept.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub removed: Vec<String>,
    pub pinned: Vec<String>,
    pub building: bool,
}

/// Reclaim `view`'s unpublished build directories and its superseded or
/// retired generations that no reader pins. The current generation of a live
/// view is never reclaimed.
pub fn sweep(store: &CollaborationStore, view: &str) -> Result<SweepReport, ViewError> {
    let view_dir = views_root(store).join(view);
    let mut report = SweepReport::default();
    let Some(_build) = try_exclusive(&view_dir.join(".build.lock"))? else {
        report.building = true;
        return Ok(report);
    };
    let retired = view_dir.join("RETIRED").exists();
    let current = if retired {
        None
    } else {
        current_generation(&view_dir)?
    };
    for entry in std::fs::read_dir(&view_dir).map_err(io(&view_dir))? {
        let entry = entry.map_err(io(&view_dir))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("building-") {
            std::fs::remove_dir_all(entry.path()).map_err(io(&entry.path()))?;
            report.removed.push(name);
        } else if let Some(n) = name.strip_prefix("gen-").and_then(|n| n.parse().ok()) {
            if Some(n) == current {
                continue;
            }
            match try_exclusive(&entry.path().join(".pin"))? {
                Some(_held) => {
                    std::fs::remove_dir_all(entry.path()).map_err(io(&entry.path()))?;
                    report.removed.push(name);
                }
                None => report.pinned.push(name),
            }
        }
    }
    report.removed.sort();
    report.pinned.sort();
    Ok(report)
}

/// Retire `view`: no new reader may open it, and [`sweep`] then reclaims
/// it. Refused while an overlay builds on it or a reader pins it (T86).
///
/// A pin can outlive its reader by an instant when another thread of the
/// reading process forks a child, which shares the lock until it execs; a
/// `pinned` refusal right after a release is safe to retry.
pub fn retire(store: &CollaborationStore, view: &str) -> Result<(), ViewError> {
    let root = views_root(store);
    let view_dir = root.join(view);
    let Some(generation) = current_generation(&view_dir)? else {
        return Err(ViewError::NotFound { view: view.into() });
    };
    let dependents = std::fs::read_dir(&root)
        .map_err(io(&root))?
        .filter_map(Result::ok)
        .filter(|entry| !entry.path().join("RETIRED").exists())
        .filter_map(|entry| {
            let dir = entry.path();
            let n = current_generation(&dir).ok().flatten()?;
            read_manifest(&dir.join(format!("gen-{n}"))).ok()
        })
        .filter(|manifest| manifest.base.as_ref().is_some_and(|base| base.view == view))
        .count();
    if dependents > 0 {
        return Err(ViewError::HasDependents {
            view: view.into(),
            dependents,
        });
    }
    let pin = view_dir.join(format!("gen-{generation}/.pin"));
    let Some(_held) = try_exclusive(&pin)? else {
        return Err(ViewError::Pinned {
            view: view.into(),
            generation,
        });
    };
    write_synced(&view_dir.join("RETIRED"), b"retired\n")?;
    sync_dir(&view_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_archive::{pin_commit, retain_snapshot};
    use crate::collaboration_state::{CollaborationRoot, ProjectKey};

    fn git(repo: &Path, args: &[&str]) {
        let output = std::process::Command::new("/usr/bin/env")
            .arg("git")
            .args(args)
            .current_dir(repo)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(repo: &Path, path: &str, text: &str) {
        let path = repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    const A: &str = "def f():\n    return 1\n\n\ndef unused():\n    return 2\n";
    const B: &str = "from lib.a import f\n\n\ndef main():\n    return f()\n";
    const C: &str = "from app.b import main\n\n\ndef run():\n    return main() + h()\n";

    /// lib/a.py defines f; app/b.py calls it; app/c.py calls b.main and an
    /// absent h; the README links to lib/a.py.
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        write(repo, "lib/a.py", A);
        write(repo, "app/b.py", B);
        write(repo, "app/c.py", C);
        write(repo, "README.md", "See [a](lib/a.py).\n");
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "base"]);
        dir
    }

    fn store(host: &Path) -> CollaborationStore {
        CollaborationStore::open(
            &CollaborationRoot::under_host_state(host),
            &ProjectKey::parse("proj-x2").unwrap(),
            &[],
        )
        .unwrap()
    }

    fn retain(store: &mut CollaborationStore, repo: &Path) -> SourceSnapshotId {
        let commit = pin_commit(repo, "HEAD").unwrap();
        retain_snapshot(store, repo, &commit).unwrap().snapshot_id
    }

    fn options() -> ViewOptions {
        ViewOptions::new(ViewProfile::current("fixture"))
    }

    const SYMBOLS: &[&str] = &["f", "g", "h", "main", "run", "unused"];
    const CHANGED: &[&[&str]] = &[&["lib/a.py"], &["app/b.py"], &["lib/a2.py"], &["app/d.py"]];

    /// Every answer a view gives on the fixture queries, with provenance
    /// (generation, view name, mode) removed: AQ2 compares the rest exactly.
    fn answers(reader: &ViewReader) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |mut envelope: AnalysisEnvelope| {
            envelope.provenance = None;
            out.push(String::from_utf8(envelope.to_record().0).unwrap());
        };
        for symbol in SYMBOLS {
            push(reader.find_references(symbol, 64).unwrap());
        }
        for paths in CHANGED {
            push(reader.explain_impact(paths, 64).unwrap());
        }
        out
    }

    fn references(
        reader: &ViewReader,
        symbol: &str,
    ) -> (Vec<(String, String, String)>, Vec<String>) {
        let envelope = reader.find_references(symbol, 64).unwrap();
        let Some(Value::Object(result)) = &envelope.result else {
            panic!("no result")
        };
        let Some(Value::Array(items)) = result.get("references") else {
            panic!("no references")
        };
        let field = |item: &Value, key: &str| match item {
            Value::Object(item) => match item.get(key) {
                Some(Value::String(value)) => value.clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        (
            items
                .iter()
                .map(|item| (field(item, "from"), field(item, "kind"), field(item, "to")))
                .collect(),
            envelope.gaps,
        )
    }

    fn impacted(reader: &ViewReader, paths: &[&str]) -> Vec<String> {
        let envelope = reader.explain_impact(paths, 64).unwrap();
        let Some(Value::Object(result)) = &envelope.result else {
            panic!("no result")
        };
        match result.get("impacted") {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::String(path) => path.clone(),
                    other => panic!("{other:?}"),
                })
                .collect(),
            other => panic!("{other:?}"),
        }
    }

    /// Independently annotated interactions on the base fixture: written
    /// from the source, not from the indexer's output.
    #[test]
    fn known_interactions_hold_on_the_base() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &snapshot, &options()).unwrap();
        let reader = open_view(&store, &base).unwrap();
        let (refs, _) = references(&reader, "f");
        assert!(
            refs.iter()
                .any(|(from, kind, to)| from == "app/b.py" && kind == "calls" && to == "lib/a.py"),
            "{refs:?}"
        );
        let (refs, _) = references(&reader, "main");
        assert!(
            refs.iter()
                .any(|(from, kind, to)| from == "app/c.py" && kind == "calls" && to == "app/b.py"),
            "{refs:?}"
        );
        // c.py's call to the absent h is not invented.
        let (refs, _) = references(&reader, "h");
        assert!(refs.is_empty(), "{refs:?}");
        let impact = impacted(&reader, &["lib/a.py"]);
        assert!(impact.contains(&"app/b.py".to_string()), "{impact:?}");
        assert!(impact.contains(&"app/c.py".to_string()), "{impact:?}");
        assert_eq!(reader.manifest().kind, ViewKind::Base);
        assert!(reader.manifest().mask.is_empty());
    }

    /// AQ2, T71–T73 and the invalidation fixtures: for each mutation, the
    /// overlay on the base and a fresh analysis of the same snapshot hold the
    /// same linked facts byte for byte and give the same answers.
    #[test]
    fn an_overlay_agrees_with_fresh_analysis_for_every_mutation() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let base_snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &base_snapshot, &options()).unwrap();

        type Mutation = (
            &'static str,
            fn(&Path),
            &'static [&'static str],
            &'static [&'static str],
        );
        let mutations: &[Mutation] = &[
            (
                "body edit",
                |r| write(r, "lib/a.py", &A.replace("return 1", "return 3")),
                &["lib/a.py"],
                &["lib/a.py"],
            ),
            // T72: the export changes; the consumer's bytes do not.
            (
                "export renamed",
                |r| write(r, "lib/a.py", &A.replace("def f()", "def g()")),
                &["lib/a.py"],
                &["lib/a.py"],
            ),
            // T73: a previously absent name appears.
            (
                "absent name added",
                |r| write(r, "app/d.py", "def h():\n    return 0\n"),
                &[],
                &["app/d.py"],
            ),
            // T73: the target is deleted.
            (
                "target deleted",
                |r| {
                    std::fs::remove_file(r.join("lib/a.py")).unwrap();
                },
                &["lib/a.py"],
                &[],
            ),
            // T73: the file moves.
            (
                "file moved",
                |r| {
                    std::fs::rename(r.join("lib/a.py"), r.join("lib/a2.py")).unwrap();
                },
                &["lib/a.py"],
                &["lib/a2.py"],
            ),
        ];
        for (name, mutate, mask, replacements) in mutations {
            git(repo.path(), &["checkout", "-q", "--detach", "main"]);
            mutate(repo.path());
            git(repo.path(), &["add", "-A"]);
            git(repo.path(), &["commit", "-qm", name]);
            let snapshot = retain(&mut store, repo.path());

            let overlay = build_overlay(&store, &base, &snapshot, &options()).unwrap();
            let fresh = build_fresh(&store, &snapshot, &options()).unwrap();
            let overlay = open_view(&store, &overlay).unwrap();
            let fresh = open_view(&store, &fresh).unwrap();
            let (o, f) = (overlay.manifest(), fresh.manifest());
            assert_eq!(
                o.fragment_set_sha256, f.fragment_set_sha256,
                "{name}: facts differ"
            );
            assert_eq!(answers(&overlay), answers(&fresh), "{name}: answers differ");
            assert_eq!(o.mask, *mask, "{name}");
            assert_eq!(o.replacements, *replacements, "{name}");
            assert_eq!(
                o.costs.units_reused + o.costs.units_extracted,
                o.units.len(),
                "{name}"
            );
            assert!(o.costs.units_reused > 0, "{name}: nothing was reused");
        }

        // The deleted target leaves no ghost: b's call no longer resolves.
        git(repo.path(), &["checkout", "-q", "--detach", "main"]);
        std::fs::remove_file(repo.path().join("lib/a.py")).unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "deleted again"]);
        let deleted = retain(&mut store, repo.path());
        let overlay = build_overlay(&store, &base, &deleted, &options()).unwrap();
        let overlay = open_view(&store, &overlay).unwrap();
        let (refs, gaps) = references(&overlay, "f");
        assert!(refs.iter().all(|(_, _, to)| to != "lib/a.py"), "{refs:?}");
        assert!(
            gaps.iter()
                .any(|gap| gap.starts_with("unresolved_references")),
            "{gaps:?}"
        );
    }

    /// T77: one base plus one overlay is the limit.
    #[test]
    fn stacking_is_refused_and_the_candidate_falls_back_to_fresh() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let base_snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &base_snapshot, &options()).unwrap();
        write(repo.path(), "lib/a.py", "def f():\n    return 9\n");
        git(repo.path(), &["commit", "-qam", "a"]);
        let first = retain(&mut store, repo.path());
        let overlay = build_overlay(&store, &base, &first, &options()).unwrap();
        write(repo.path(), "app/b.py", "def main():\n    return 0\n");
        git(repo.path(), &["commit", "-qam", "b"]);
        let second = retain(&mut store, repo.path());

        let error = build_overlay(&store, &overlay, &second, &options()).unwrap_err();
        assert_eq!(error.code(), "unsupported_stacking", "{error}");
        match view_for_candidate(&store, &overlay, &second, &options()).unwrap() {
            ViewOutcome::Fresh { view, reason } => {
                assert_eq!(reason, "unsupported_stacking");
                assert_eq!(
                    open_view(&store, &view).unwrap().manifest().kind,
                    ViewKind::Fresh
                );
            }
            other => panic!("{other:?}"),
        }
    }

    /// T90: a base analysed under another producer version is never reused,
    /// and an unrelated edit re-extracts only its own unit.
    #[test]
    fn another_profile_is_never_reused_and_edits_stay_scoped() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let base_snapshot = retain(&mut store, repo.path());
        let old = ViewOptions::new(ViewProfile::with_version("fixture", "0.0.0-old"));
        let base = build_base(&store, &base_snapshot, &old).unwrap();
        write(
            repo.path(),
            "app/b.py",
            &B.replace("return f()", "return f() + 1"),
        );
        git(repo.path(), &["commit", "-qam", "edit"]);
        let snapshot = retain(&mut store, repo.path());

        let error = build_overlay(&store, &base, &snapshot, &options()).unwrap_err();
        assert_eq!(error.code(), "incompatible_analysis_profile", "{error}");
        match view_for_candidate(&store, &base, &snapshot, &options()).unwrap() {
            ViewOutcome::Fresh { reason, .. } => {
                assert_eq!(reason, "incompatible_analysis_profile")
            }
            other => panic!("{other:?}"),
        }

        let current = build_base(&store, &base_snapshot, &options()).unwrap();
        let overlay = build_overlay(&store, &current, &snapshot, &options()).unwrap();
        let manifest = open_view(&store, &overlay).unwrap().manifest().clone();
        assert_eq!(manifest.replacements, ["app/b.py"]);
        assert_eq!(manifest.costs.units_extracted, 1);
        // The envelope names the profile; the two profiles differ.
        assert_ne!(old.profile.reference(), options().profile.reference());
    }

    /// T76: a build stopped at any point publishes nothing; readers keep the
    /// previous generation, and cleanup reclaims the debris.
    #[test]
    fn a_crash_never_publishes_a_partial_generation() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &snapshot, &options()).unwrap();
        let before = answers(&open_view(&store, &base).unwrap());
        for fault in [
            Fault::AfterIndex,
            Fault::AfterLink,
            Fault::BeforePublish,
            Fault::BeforeCurrent,
        ] {
            let mut crashing = options();
            crashing.fault = Some(fault);
            let error = build_base(&store, &snapshot, &crashing).unwrap_err();
            assert_eq!(error.code(), "injected", "{fault:?}");
            let reader = open_view(&store, &base).unwrap();
            assert_eq!(reader.manifest().generation, 1, "{fault:?}");
            assert_eq!(answers(&reader), before, "{fault:?}");
            drop(reader);
            after_release(|| {
                let swept = sweep(&store, &base).unwrap();
                (swept.removed.len() == 1).then_some(())
            });
        }
        assert_eq!(generations(&views_root(&store).join(&base)).unwrap(), [1]);
    }

    /// T76: a reader keeps the generation it pinned while a newer one
    /// publishes; that generation is reclaimed only once the reader is gone.
    #[test]
    fn a_pinned_reader_keeps_its_generation() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &snapshot, &options()).unwrap();
        let reader = open_view(&store, &base).unwrap();
        let before = answers(&reader);
        build_base(&store, &snapshot, &options()).unwrap();
        assert_eq!(open_view(&store, &base).unwrap().manifest().generation, 2);
        // The finished build's lock can still be held for an instant (see
        // `after_release`); sweep then skips the view rather than guess.
        let swept = after_release(|| {
            let swept = sweep(&store, &base).unwrap();
            (!swept.building).then_some(swept)
        });
        assert_eq!(swept.pinned, ["gen-1"], "{swept:?}");
        assert_eq!(reader.manifest().generation, 1);
        let after = answers(&reader);
        for (index, (was, is)) in before.iter().zip(&after).enumerate() {
            assert_eq!(was, is, "answer {index} changed under a pinned reader");
        }
        assert_eq!(after.len(), before.len());
        drop(reader);
        after_release(|| {
            let swept = sweep(&store, &base).unwrap();
            (swept.removed == ["gen-1"]).then_some(())
        });
    }

    /// T86: a base is not retired under an overlay or a reader, and
    /// retiring views never touches retained source.
    #[test]
    fn retiring_a_base_waits_for_overlays_and_readers() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let base_snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &base_snapshot, &options()).unwrap();
        write(repo.path(), "lib/a.py", "def f():\n    return 5\n");
        git(repo.path(), &["commit", "-qam", "a"]);
        let snapshot = retain(&mut store, repo.path());
        let overlay = build_overlay(&store, &base, &snapshot, &options()).unwrap();

        assert_eq!(retire(&store, &base).unwrap_err().code(), "has_dependents");
        retire(&store, &overlay).unwrap();
        let reader = open_view(&store, &base).unwrap();
        assert_eq!(retire(&store, &base).unwrap_err().code(), "pinned");
        drop(reader);
        after_release(|| match retire(&store, &base) {
            Ok(()) => Some(()),
            Err(error) if error.code() == "pinned" => None,
            Err(error) => panic!("{error}"),
        });
        assert_eq!(open_view(&store, &base).unwrap_err().code(), "retired");
        after_release(|| (sweep(&store, &base).unwrap().removed == ["gen-1"]).then_some(()));
        // Retained source is a different lifecycle.
        let dest = tempfile::tempdir().unwrap();
        crate::collaboration_archive::reconstruct(&store, &base_snapshot, &dest.path().join("t"))
            .unwrap();
    }

    /// T75: two worktrees' candidates from one base, built at once, stay
    /// isolated, and no canonical graph state or repository is written.
    #[test]
    fn views_are_isolated_and_never_write_canonical_state() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let base_snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &base_snapshot, &options()).unwrap();
        let mut candidates = Vec::new();
        for (path, body) in [
            ("lib/a.py", "def f():\n    return 7\n"),
            ("app/b.py", "def main():\n    return 8\n"),
        ] {
            git(repo.path(), &["checkout", "-q", "--detach", "main"]);
            write(repo.path(), path, body);
            git(repo.path(), &["commit", "-qam", path]);
            candidates.push(retain(&mut store, repo.path()));
        }
        let built: Vec<String> = std::thread::scope(|scope| {
            let handles: Vec<_> = candidates
                .iter()
                .map(|snapshot| {
                    let host = host.path();
                    let base = &base;
                    scope.spawn(move || {
                        build_overlay(&store_for(host), base, snapshot, &options()).unwrap()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let a = open_view(&store, &built[0]).unwrap();
        let b = open_view(&store, &built[1]).unwrap();
        assert_eq!(a.manifest().replacements, ["lib/a.py"]);
        assert_eq!(b.manifest().replacements, ["app/b.py"]);
        assert_ne!(
            a.manifest().fragment_set_sha256,
            b.manifest().fragment_set_sha256
        );
        // B's candidate removed the call to f; A's did not.
        assert!(!references(&a, "f").0.is_empty());
        assert!(references(&b, "f").0.is_empty());
        // Nothing was written into the repository.
        assert!(!repo.path().join(".aethyme").exists());
        git(repo.path(), &["diff", "--quiet", "HEAD"]);
        let status = std::process::Command::new("/usr/bin/env")
            .args(["git", "status", "--porcelain"])
            .current_dir(repo.path())
            .output()
            .unwrap();
        assert!(
            status.stdout.is_empty(),
            "{}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    /// A lock released in this process can stay held for an instant when
    /// another thread forks a child (here, tests running `git`): the child
    /// shares the open file until it execs. Refusing is the safe answer, so
    /// a caller that just released retries briefly.
    fn after_release<T>(mut attempt: impl FnMut() -> Option<T>) -> T {
        for _ in 0..100 {
            if let Some(value) = attempt() {
                return value;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("still held after release")
    }

    fn store_for(host: &Path) -> CollaborationStore {
        store(host)
    }

    /// T83 (partial): readers never wait for a build.
    #[test]
    fn a_reader_does_not_wait_for_a_build() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let base = build_base(&store, &snapshot, &options()).unwrap();
        let held = open_lock(&views_root(&store).join(&base).join(".build.lock")).unwrap();
        held.lock().unwrap();
        let started = Instant::now();
        open_view(&store, &base)
            .unwrap()
            .find_references("f", 8)
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        assert!(sweep(&store, &base).unwrap().building);
    }

    /// Costs on a generated fixture, for the decision record:
    /// `cargo test -p aethyme-broker --lib analysis_view::tests::measure -- --ignored --nocapture`
    #[test]
    #[ignore = "measurement, not a check"]
    fn measure() {
        let host = tempfile::tempdir().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        let files = 400;
        for i in 0..files {
            let mut body = format!(
                "from pkg.m{} import f{}\n\n",
                (i + 1) % files,
                (i + 1) % files
            );
            for j in 0..20 {
                body.push_str(&format!(
                    "def f{i}_{j}(x):\n    y = x + {j}\n    return f{}(y)\n\n",
                    (i + 1) % files
                ));
            }
            body.push_str(&format!("def f{i}(x):\n    return x\n"));
            write(repo, &format!("pkg/m{i}.py"), &body);
        }
        git(repo, &["add", "-A"]);
        git(repo, &["commit", "-qm", "base"]);
        let mut store = store(host.path());
        let base_snapshot = retain(&mut store, repo);
        let base = build_base(&store, &base_snapshot, &options()).unwrap();
        write(repo, "pkg/m7.py", "def f7(x):\n    return x * 2\n");
        git(repo, &["commit", "-qam", "edit"]);
        let snapshot = retain(&mut store, repo);
        let overlay = build_overlay(&store, &base, &snapshot, &options()).unwrap();
        let fresh = build_fresh(&store, &snapshot, &options()).unwrap();
        for name in [&base, &overlay, &fresh] {
            let reader = open_view(&store, name).unwrap();
            let manifest = reader.manifest();
            let started = Instant::now();
            reader.explain_impact(&["pkg/m7.py"], 100_000).unwrap();
            println!(
                "{} {:?}: build {} ms (materialize {} ms, index {} ms, link {} ms), extracted {}, \
                 reused {}, facts {} B, extractions {} B, impact query {} ms, agree-with-fresh {}",
                name,
                manifest.kind,
                manifest.costs.build_us / 1000,
                manifest.costs.materialize_us / 1000,
                manifest.costs.index_us / 1000,
                manifest.costs.link_us / 1000,
                manifest.costs.units_extracted,
                manifest.costs.units_reused,
                manifest.costs.fact_bytes,
                manifest.costs.extraction_bytes,
                started.elapsed().as_millis(),
                manifest.fragment_set_sha256
                    == open_view(&store, &fresh)
                        .unwrap()
                        .manifest()
                        .fragment_set_sha256
            );
        }
    }
}
