//! Fresh exact-snapshot analysis views (#684, plan §6.14–6.16; D42, D49).
//!
//! A view is an isolated structural analysis of one retained snapshot, kept
//! as a sealed generation and answered through the AQ0 envelope. It is what
//! X3 (#685) and L3 consume to ask about a candidate without touching
//! canonical graph state.
//!
//! The X2 experiment also built incremental overlays: a candidate reusing an
//! analysed base's per-file extractions. They were exactly as correct as a
//! fresh analysis but gave no measurable gain, because walking, reading,
//! decoding and linking cost as much as the parsing saved. The user decided
//! to drop them (decision record: `local-v3-x2-private-views.md`). Overlays
//! and stacking are refused explicitly, and the indexer is unchanged.
//!
//! ```text
//! <collaboration project dir>/views/<view>/
//!   .build.lock          exclusive while a generation is being built
//!   CURRENT              "gen-<n>": the published generation
//!   RETIRED              present once the view is retired
//!   holds/<id>.json      consumers that rely on the view (X3 reports, L3)
//!   gen-<n>/             sealed: never modified after publication
//!     manifest.json      identity, profile, coverage, fact digest, costs
//!     facts/.aethyme/graph/   linked fragments for this snapshot
//!     .pin               readers hold it shared
//!   building-<n>-<pid>/  in progress; never read
//! ```
//!
//! - **Isolation.** Nothing here writes a repository, its canonical
//!   `.aethyme/graph` fragments or producer `_overlays`, or calls the graph
//!   refresh, so the active-session refresh guard is never reached. The
//!   retained snapshot is materialised inside the view's build directory.
//! - **Generations.** Built privately, sealed with a manifest, published by
//!   one rename, and only then named by `CURRENT`. A crash before either step
//!   publishes nothing.
//! - **Readers** pin a generation with a shared lock on its `.pin`, taken
//!   under the archive's store-wide shared lock like every other pin, so a
//!   reclamation apply never races a new reader.
//! - **Reclamation** belongs to `collaboration_gc` (the `view` class): it
//!   removes retired and superseded generations and stale build directories,
//!   never `CURRENT`, a pinned generation or a held view.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aethyme_contracts::experimental_v0::analysis::ANALYSIS_PROFILE_SCHEMA;
use aethyme_contracts::experimental_v0::analysis::{
    AnalysisEnvelope, Coverage, Freshness, Limits, Operation, Outcome, ProfileRef, Subject,
    profile_id,
};
use aethyme_contracts::experimental_v0::canonical_json::{Object, Value};
use aethyme_contracts::experimental_v0::{Record, SourceSnapshotId};
use aethyme_graph_indexer::{
    IndexerContext, WalkOptions, default_registry, index_repo_to_disk_with, link_repo,
};
use aethyme_graph_schema::{EdgeKind, NodeId, NodeKind};
use aethyme_graph_storage::FragmentStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::collaboration_state::CollaborationStore;

/// The manifest schema of a view generation.
pub const VIEW_SCHEMA: &str = "aethyme.analysis-view/experimental-v0";
const PRODUCER: &str = "aethyme-structural-indexer";
const VIEWS_DIR: &str = "views";
/// Languages whose parser is part of the profile.
const PROFILE_LANGUAGES: &[&str] = &["javascript", "php", "python", "rust", "typescript"];
/// Edge kinds the queries traverse.
const QUERY_EDGES: &[EdgeKind] = &[EdgeKind::Calls, EdgeKind::Imports, EdgeKind::References];

/// What produced a view's facts. A view answers only under the profile it
/// was built with; another profile gets its own view.
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

/// A view's published state (§6.16). Building and failed generations are
/// never published, so a reader only ever sees these two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewState {
    Ready,
    /// Published, but some files were not fully analysed.
    Partial,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewCosts {
    pub build_us: u128,
    /// Materialising the retained snapshot.
    pub materialize_us: u128,
    /// Walking, reading and extracting.
    pub index_us: u128,
    /// Linking.
    pub link_us: u128,
    pub fact_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewManifest {
    pub schema: String,
    pub view: String,
    pub generation: u64,
    pub snapshot: String,
    pub profile: ViewProfile,
    pub profile_digest: String,
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
        "incremental overlays and stacked views are not supported: the X2 experiment measured \
         no gain over a fresh analysis, so analyse the candidate snapshot directly"
    )]
    OverlaysUnsupported,
    #[error("view {view} was built under profile {found}, not {expected}")]
    IncompatibleProfile {
        view: String,
        expected: String,
        found: String,
    },
    #[error("view {view} is held by {holders} consumer(s); release the holds first")]
    Held { view: String, holders: usize },
    #[error("view {view} generation {generation} is pinned by a reader")]
    Pinned { view: String, generation: u64 },
    #[error("holder must be 1-200 bytes")]
    InvalidHolder,
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
            Self::OverlaysUnsupported => "overlays_unsupported",
            Self::IncompatibleProfile { .. } => "incompatible_analysis_profile",
            Self::Held { .. } => "held",
            Self::Pinned { .. } => "pinned",
            Self::InvalidHolder => "invalid_holder",
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

pub(crate) fn views_root(store: &CollaborationStore) -> PathBuf {
    store.project_dir().join(VIEWS_DIR)
}

/// The name of `snapshot`'s view under `profile`.
pub fn view_name(snapshot: &SourceSnapshotId, profile: &ViewProfile) -> String {
    format!("view-{}-{}", short(snapshot), &profile.digest()[..16])
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

pub(crate) fn current_generation(view_dir: &Path) -> Result<Option<u64>, ViewError> {
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

// ------------------------------------------------------------------ build

/// A published, pinned generation.
#[derive(Debug)]
pub struct ViewReader {
    dir: PathBuf,
    manifest: ViewManifest,
    _pin: File,
}

/// Build and publish a new generation of `snapshot`'s view: an isolated full
/// analysis of the retained snapshot.
pub fn build_view(
    store: &CollaborationStore,
    snapshot: &SourceSnapshotId,
    options: &ViewOptions,
) -> Result<String, ViewError> {
    let started = Instant::now();
    let profile = &options.profile;
    let name = view_name(snapshot, profile);
    let view_dir = views_root(store).join(&name);
    std::fs::create_dir_all(&view_dir).map_err(io(&view_dir))?;
    let lock_path = view_dir.join(".build.lock");
    let lock = open_lock(&lock_path)?;
    lock.lock().map_err(io(&lock_path))?;
    // Building revives a retired view. A marker that cannot be removed would
    // leave the new generation looking retired, so that is an error.
    let retired = view_dir.join("RETIRED");
    match std::fs::remove_file(&retired) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io(&retired)(error)),
    }

    let generation = generations(&view_dir)?.last().copied().unwrap_or(0) + 1;
    let building = view_dir.join(format!("building-{generation}-{}", std::process::id()));
    if building.exists() {
        std::fs::remove_dir_all(&building).map_err(io(&building))?;
    }
    std::fs::create_dir_all(&building).map_err(io(&building))?;

    // The retained snapshot, materialised privately. Its own `.aethyme/`
    // is not source: the walker skips it and the indexer writes there.
    let source = building.join("source");
    crate::collaboration_archive::reconstruct(store, snapshot, &source).map_err(|source| {
        ViewError::Source {
            snapshot: snapshot.to_string(),
            source,
        }
    })?;
    let materialize_us = started.elapsed().as_micros();
    let committed_graph = source.join(".aethyme");
    if committed_graph.exists() {
        std::fs::remove_dir_all(&committed_graph).map_err(io(&committed_graph))?;
    }

    let index_started = Instant::now();
    let ctx = IndexerContext::new(&profile.repo_name, &source, &profile.producer_version)
        .map_err(|error| ViewError::Index(error.to_string()))?;
    let summary = index_repo_to_disk_with(&ctx, &WalkOptions::default(), &default_registry())
        .map_err(|error| ViewError::Index(error.to_string()))?;
    let index_us = index_started.elapsed().as_micros();
    fault!(options, AfterIndex);
    let link_started = Instant::now();
    link_repo(&ctx).map_err(|error| ViewError::Index(error.to_string()))?;
    let link_us = link_started.elapsed().as_micros();
    fault!(options, AfterLink);

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

    let report = &summary.coverage.report;
    let manifest = ViewManifest {
        schema: VIEW_SCHEMA.into(),
        view: name.clone(),
        generation,
        snapshot: snapshot.to_string(),
        profile: profile.clone(),
        profile_digest: profile.digest(),
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
            fact_bytes,
        },
    };
    write_synced(
        &building.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest).expect("a manifest serializes"),
    )?;
    // Readers pin through this file; it exists before the generation does.
    write_synced(&building.join(".pin"), b"")?;
    sync_dir(&building)?;
    fault!(options, BeforePublish);

    // Publish: one rename makes the sealed generation visible as a whole.
    let published = view_dir.join(format!("gen-{generation}"));
    std::fs::rename(&building, &published).map_err(io(&published))?;
    sync_dir(&view_dir)?;
    fault!(options, BeforeCurrent);
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

/// Incremental overlays were measured and dropped (X2): always refused.
pub fn build_overlay(
    _store: &CollaborationStore,
    _base: &str,
    _snapshot: &SourceSnapshotId,
    _options: &ViewOptions,
) -> Result<String, ViewError> {
    Err(ViewError::OverlaysUnsupported)
}

/// The published view of `snapshot` under `options`' profile, building it
/// first when there is none.
pub fn view_for_snapshot(
    store: &CollaborationStore,
    snapshot: &SourceSnapshotId,
    options: &ViewOptions,
) -> Result<ViewReader, ViewError> {
    let name = view_name(snapshot, &options.profile);
    match open_view_for(store, &name, &options.profile) {
        Err(ViewError::NotFound { .. } | ViewError::Retired { .. }) => {
            build_view(store, snapshot, options)?;
            open_view_for(store, &name, &options.profile)
        }
        other => other,
    }
}

// ---------------------------------------------------------------- readers

/// Pin `view`'s published generation for reading.
pub fn open_view(store: &CollaborationStore, view: &str) -> Result<ViewReader, ViewError> {
    let view_dir = views_root(store).join(view);
    // Pins are taken under the archive's shared lock, like every other pin,
    // so a reclamation apply (exclusive) never races a new reader.
    let _use = crate::collaboration_gc::archive_use(store).map_err(|source| ViewError::Io {
        path: crate::collaboration_gc::lock_path(store),
        source,
    })?;
    if view_dir.join("RETIRED").exists() {
        return Err(ViewError::Retired { view: view.into() });
    }
    // CURRENT may move between reading the pointer and pinning; pin, then
    // confirm the generation still exists.
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

/// [`open_view`], refusing a view built under another profile (T90).
pub fn open_view_for(
    store: &CollaborationStore,
    view: &str,
    profile: &ViewProfile,
) -> Result<ViewReader, ViewError> {
    let reader = open_view(store, view)?;
    if reader.manifest.profile != *profile {
        return Err(ViewError::IncompatibleProfile {
            view: view.into(),
            expected: profile.digest(),
            found: reader.manifest.profile_digest.clone(),
        });
    }
    Ok(reader)
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
        let provenance = vec![
            ("view", text(&self.manifest.view)),
            ("generation", int(self.manifest.generation)),
            (
                "fragment_set_sha256",
                text(&self.manifest.fragment_set_sha256),
            ),
            ("mode", text("fresh")),
        ];
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

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ViewHold {
    pub holder: String,
    pub until_ms: Option<i64>,
}

fn holds_dir(view_dir: &Path) -> PathBuf {
    view_dir.join("holds")
}

/// Record that `holder` (an X3 report, an L3 context) relies on `view` until
/// `until_ms`, or until released. A held view is neither retired nor
/// reclaimed. Taken under the archive's shared lock like other pins.
pub fn hold_view(
    store: &CollaborationStore,
    view: &str,
    holder: &str,
    until_ms: Option<i64>,
) -> Result<(), ViewError> {
    if holder.is_empty() || holder.len() > 200 {
        return Err(ViewError::InvalidHolder);
    }
    let _use = crate::collaboration_gc::archive_use(store).map_err(|source| ViewError::Io {
        path: crate::collaboration_gc::lock_path(store),
        source,
    })?;
    let view_dir = views_root(store).join(view);
    if current_generation(&view_dir)?.is_none() || view_dir.join("RETIRED").exists() {
        return Err(ViewError::NotFound { view: view.into() });
    }
    let dir = holds_dir(&view_dir);
    std::fs::create_dir_all(&dir).map_err(io(&dir))?;
    let path = dir.join(format!(
        "{}.json",
        &hex(&Sha256::digest(holder.as_bytes()))[..32]
    ));
    let body = serde_json::to_vec(&ViewHold {
        holder: holder.into(),
        until_ms,
    })
    .expect("a hold serializes");
    let temporary = dir.join(".hold.tmp");
    write_synced(&temporary, &body)?;
    std::fs::rename(&temporary, &path).map_err(io(&path))?;
    sync_dir(&dir)
}

/// Release `holder`'s hold on `view`. `false` when there was none.
pub fn release_view_hold(
    store: &CollaborationStore,
    view: &str,
    holder: &str,
) -> Result<bool, ViewError> {
    let dir = holds_dir(&views_root(store).join(view));
    let path = dir.join(format!(
        "{}.json",
        &hex(&Sha256::digest(holder.as_bytes()))[..32]
    ));
    match std::fs::remove_file(&path) {
        Ok(()) => {
            sync_dir(&dir)?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io(&path)(error)),
    }
}

/// The holds on `view_dir` that are still live at `now_ms`. A hold file that
/// cannot be read counts as live: an unknown dependent is protected.
pub(crate) fn live_holds(view_dir: &Path, now_ms: i64) -> usize {
    let Ok(entries) = std::fs::read_dir(holds_dir(view_dir)) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().ends_with(".json"))
        .filter(|entry| {
            std::fs::read(entry.path())
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ViewHold>(&bytes).ok())
                .is_none_or(|hold| hold.until_ms.is_none_or(|until| until > now_ms))
        })
        .count()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Retire `view`: no new reader may open it, and reclamation
/// (`collaboration_gc`, the `view` class) then removes its generations.
/// Refused while a consumer holds it or a reader pins it (T86).
///
/// A pin can outlive its reader by an instant when another thread of the
/// reading process forks a child, which shares the lock until it execs; a
/// `pinned` refusal right after a release is safe to retry.
pub fn retire(store: &CollaborationStore, view: &str) -> Result<(), ViewError> {
    let view_dir = views_root(store).join(view);
    let Some(generation) = current_generation(&view_dir)? else {
        return Err(ViewError::NotFound { view: view.into() });
    };
    let holders = live_holds(&view_dir, now_ms());
    if holders > 0 {
        return Err(ViewError::Held {
            view: view.into(),
            holders,
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

    use crate::collaboration_gc::{GcOptions, ItemKind, RetentionClass, apply, plan};

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

    /// Make every entry under `views/` three days old, as if the grace
    /// period had passed. Nothing is followed through a link.
    fn age_views(store: &CollaborationStore) {
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(3 * 86_400);
        let mut pending = vec![views_root(store)];
        while let Some(dir) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.is_dir() {
                    pending.push(path.clone());
                }
                if metadata.is_dir() || metadata.is_file() {
                    File::open(&path).unwrap().set_modified(then).unwrap();
                }
            }
        }
    }

    fn view_items(store: &mut CollaborationStore) -> Vec<(ItemKind, String)> {
        plan(store, &GcOptions::default())
            .unwrap()
            .reclaimable
            .into_iter()
            .filter(|item| item.class == RetentionClass::View)
            .map(|item| (item.kind, item.relpath))
            .collect()
    }

    fn reclaim_views(store: &mut CollaborationStore) -> Vec<String> {
        let plan = plan(store, &GcOptions::default()).unwrap();
        if plan.reclaimable.is_empty() {
            return Vec::new();
        }
        apply(store, &plan.digest, &GcOptions::default())
            .unwrap()
            .reclaimed
            .into_iter()
            .filter(|item| item.class == RetentionClass::View)
            .map(|item| item.relpath)
            .collect()
    }

    /// Independently annotated interactions on the fixture: written from
    /// the source, not from the indexer's output.
    #[test]
    fn known_interactions_hold() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        let reader = open_view(&store, &view).unwrap();
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
    }

    /// A deleted target leaves no ghost: the reference becomes an explicit
    /// unresolved gap and coverage is partial, never complete-and-empty.
    #[test]
    fn a_deleted_target_is_an_unresolved_gap_not_absence() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        std::fs::remove_file(repo.path().join("lib/a.py")).unwrap();
        git(repo.path(), &["add", "-A"]);
        git(repo.path(), &["commit", "-qm", "deleted"]);
        let snapshot = retain(&mut store, repo.path());
        let reader = view_for_snapshot(&store, &snapshot, &options()).unwrap();
        let envelope = reader.find_references("f", 64).unwrap();
        assert!(!envelope.absence_is_evidence());
        let (refs, gaps) = references(&reader, "f");
        assert!(refs.iter().all(|(_, _, to)| to != "lib/a.py"), "{refs:?}");
        assert!(
            gaps.iter()
                .any(|gap| gap.starts_with("unresolved_references")),
            "{gaps:?}"
        );
    }

    /// X2's overlays were measured and dropped; stacking with them.
    #[test]
    fn overlays_and_stacking_are_refused() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        let error = build_overlay(&store, &view, &snapshot, &options()).unwrap_err();
        assert_eq!(error.code(), "overlays_unsupported", "{error}");
    }

    /// T90: a view answers only under the profile it was built with; another
    /// producer version gets a separate view, never the old facts.
    #[test]
    fn a_view_answers_only_under_its_profile() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let old = ViewOptions::new(ViewProfile::with_version("fixture", "0.0.0-old"));
        let old_view = build_view(&store, &snapshot, &old).unwrap();
        let error = open_view_for(&store, &old_view, &options().profile).unwrap_err();
        assert_eq!(error.code(), "incompatible_analysis_profile", "{error}");
        let current = view_for_snapshot(&store, &snapshot, &options()).unwrap();
        assert_ne!(current.manifest().view, old_view);
        assert_eq!(current.manifest().profile, options().profile);
        assert_ne!(old.profile.reference(), options().profile.reference());
    }

    /// T76: a build stopped at any point publishes nothing, and readers keep
    /// the previous generation; reclamation removes the debris after grace.
    #[test]
    fn a_crash_never_publishes_a_partial_generation() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        let before = answers(&open_view(&store, &view).unwrap());
        for fault in [
            Fault::AfterIndex,
            Fault::AfterLink,
            Fault::BeforePublish,
            Fault::BeforeCurrent,
        ] {
            let mut crashing = options();
            crashing.fault = Some(fault);
            let error = build_view(&store, &snapshot, &crashing).unwrap_err();
            assert_eq!(error.code(), "injected", "{fault:?}");
            let reader = open_view(&store, &view).unwrap();
            assert_eq!(reader.manifest().generation, 1, "{fault:?}");
            assert_eq!(answers(&reader), before, "{fault:?}");
        }
        // Debris inside the grace period is kept.
        assert!(view_items(&mut store).is_empty());
        age_views(&store);
        let reclaimed = after_release(|| {
            let reclaimed = reclaim_views(&mut store);
            (!reclaimed.is_empty()).then_some(reclaimed)
        });
        // Each retry reuses `building-2-<pid>`; the last crash published
        // gen-2 without naming it, so that is the only debris left.
        assert_eq!(reclaimed, [format!("views/{view}/gen-2")]);
        assert_eq!(generations(&views_root(&store).join(&view)).unwrap(), [1]);
        assert_eq!(answers(&open_view(&store, &view).unwrap()), before);
    }

    /// T76: a reader keeps the generation it pinned while a newer one
    /// publishes; reclamation takes it only once the reader is gone, and
    /// never takes `CURRENT`.
    #[test]
    fn a_pinned_generation_and_current_are_never_reclaimed() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        let reader = open_view(&store, &view).unwrap();
        let before = answers(&reader);
        build_view(&store, &snapshot, &options()).unwrap();
        age_views(&store);
        let items = after_release(|| {
            let plan = plan(&mut store, &GcOptions::default()).unwrap();
            (plan
                .retained
                .get(&RetentionClass::View)
                .is_some_and(|total| total.objects == 1))
            .then_some(view_items(&mut store))
        });
        assert!(items.is_empty(), "pinned or current reclaimable: {items:?}");
        assert_eq!(answers(&reader), before);
        drop(reader);
        let reclaimed = after_release(|| {
            let reclaimed = reclaim_views(&mut store);
            (!reclaimed.is_empty()).then_some(reclaimed)
        });
        assert_eq!(reclaimed, [format!("views/{view}/gen-1")]);
        assert_eq!(open_view(&store, &view).unwrap().manifest().generation, 2);
    }

    /// An abandoned build directory is reclaimed once it is older than grace
    /// and nobody holds the build lock.
    #[test]
    fn stale_build_debris_is_reclaimed_after_grace() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        let mut crashing = options();
        crashing.fault = Some(Fault::AfterLink);
        build_view(&store, &snapshot, &crashing).unwrap_err();
        assert!(view_items(&mut store).is_empty());
        age_views(&store);
        let items = after_release(|| {
            let items = view_items(&mut store);
            (!items.is_empty()).then_some(items)
        });
        assert_eq!(
            items,
            [(
                ItemKind::ViewBuild,
                format!("views/{view}/building-2-{}", std::process::id())
            )]
        );
    }

    /// A superseded generation stays for the grace period after `CURRENT`
    /// moved on.
    #[test]
    fn a_superseded_generation_waits_for_grace() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        build_view(&store, &snapshot, &options()).unwrap();
        assert!(view_items(&mut store).is_empty());
        age_views(&store);
        let items = after_release(|| {
            let items = view_items(&mut store);
            (!items.is_empty()).then_some(items)
        });
        assert_eq!(
            items,
            [(ItemKind::ViewGeneration, format!("views/{view}/gen-1"))]
        );
    }

    /// Nothing in a view is reclaimed while it is being built, or while a
    /// consumer holds it.
    #[test]
    fn a_held_or_building_view_keeps_everything() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        build_view(&store, &snapshot, &options()).unwrap();
        hold_view(&store, &view, "x3-report-1", None).unwrap();
        age_views(&store);
        assert!(view_items(&mut store).is_empty(), "held");
        assert!(release_view_hold(&store, &view, "x3-report-1").unwrap());
        // An expired hold no longer protects.
        hold_view(&store, &view, "x3-report-2", Some(1)).unwrap();
        age_views(&store);
        let building = open_lock(&views_root(&store).join(&view).join(".build.lock")).unwrap();
        building.lock().unwrap();
        assert!(view_items(&mut store).is_empty(), "building");
        drop(building);
        let items = after_release(|| {
            let items = view_items(&mut store);
            (!items.is_empty()).then_some(items)
        });
        assert_eq!(items.len(), 1, "{items:?}");
    }

    /// T86: a held or pinned view is not retired; a retired view's
    /// generations go without waiting for grace, and retained source stays.
    #[test]
    fn retiring_waits_for_holds_and_readers() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        hold_view(&store, &view, "l3-context", None).unwrap();
        assert_eq!(retire(&store, &view).unwrap_err().code(), "held");
        release_view_hold(&store, &view, "l3-context").unwrap();
        let reader = open_view(&store, &view).unwrap();
        assert_eq!(retire(&store, &view).unwrap_err().code(), "pinned");
        drop(reader);
        after_release(|| match retire(&store, &view) {
            Ok(()) => Some(()),
            Err(error) if error.code() == "pinned" => None,
            Err(error) => panic!("{error}"),
        });
        assert_eq!(open_view(&store, &view).unwrap_err().code(), "retired");
        let reclaimed = after_release(|| {
            let reclaimed = reclaim_views(&mut store);
            (!reclaimed.is_empty()).then_some(reclaimed)
        });
        assert_eq!(reclaimed, [format!("views/{view}/gen-1")]);
        let dest = tempfile::tempdir().unwrap();
        crate::collaboration_archive::reconstruct(&store, &snapshot, &dest.path().join("t"))
            .unwrap();
    }

    /// A symlink under `views/` is unknown: reported, never followed or
    /// removed.
    #[test]
    fn a_symlinked_view_is_unknown_and_untouched() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        retire(&store, &view).unwrap();
        // Move the retired view elsewhere and link it back.
        let outside = tempfile::tempdir().unwrap();
        let moved = outside.path().join("view");
        std::fs::rename(views_root(&store).join(&view), &moved).unwrap();
        std::os::unix::fs::symlink(&moved, views_root(&store).join(&view)).unwrap();
        let plan = plan(&mut store, &GcOptions::default()).unwrap();
        assert!(
            plan.unknown.contains(&format!("views/{view}")),
            "{:?}",
            plan.unknown
        );
        assert!(view_items(&mut store).is_empty());
        assert!(moved.join("gen-1/manifest.json").is_file());
    }

    /// An apply interrupted while moving view directories is finished by
    /// resume, and the view stays readable throughout.
    #[test]
    fn an_interrupted_view_reclamation_resumes() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
        let snapshot = retain(&mut store, repo.path());
        let view = build_view(&store, &snapshot, &options()).unwrap();
        build_view(&store, &snapshot, &options()).unwrap();
        build_view(&store, &snapshot, &options()).unwrap();
        age_views(&store);
        let plan = after_release(|| {
            let plan = plan(&mut store, &GcOptions::default()).unwrap();
            (plan.reclaimable.len() == 2).then_some(plan)
        });
        let hooks = crate::collaboration_gc::GcHooks {
            fail_after_moves: Some(1),
            ..Default::default()
        };
        let error = crate::collaboration_gc::apply_with(
            &mut store,
            &plan.digest,
            &GcOptions::default(),
            &hooks,
        )
        .unwrap_err();
        assert!(error.to_string().contains("injected"), "{error}");
        assert_eq!(open_view(&store, &view).unwrap().manifest().generation, 3);
        let resumed = crate::collaboration_gc::resume(&mut store, &GcOptions::default()).unwrap();
        assert_eq!(resumed.len(), 1);
        assert_eq!(generations(&views_root(&store).join(&view)).unwrap(), [3]);
        assert_eq!(open_view(&store, &view).unwrap().manifest().generation, 3);
    }

    /// T75: two candidates' views built at once stay isolated, and no
    /// canonical graph state or repository is written.
    #[test]
    fn views_are_isolated_and_never_write_canonical_state() {
        let host = tempfile::tempdir().unwrap();
        let repo = fixture();
        let mut store = store(host.path());
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
                    scope.spawn(move || build_view(&store_for(host), snapshot, &options()).unwrap())
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect()
        });
        let a = open_view(&store, &built[0]).unwrap();
        let b = open_view(&store, &built[1]).unwrap();
        assert_ne!(
            a.manifest().fragment_set_sha256,
            b.manifest().fragment_set_sha256
        );
        // B's candidate removed the call to f; A's did not.
        assert!(!references(&a, "f").0.is_empty());
        assert!(references(&b, "f").0.is_empty());
        assert!(!repo.path().join(".aethyme").exists());
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
        let view = build_view(&store, &snapshot, &options()).unwrap();
        let held = open_lock(&views_root(&store).join(&view).join(".build.lock")).unwrap();
        held.lock().unwrap();
        let started = Instant::now();
        open_view(&store, &view)
            .unwrap()
            .find_references("f", 8)
            .unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
