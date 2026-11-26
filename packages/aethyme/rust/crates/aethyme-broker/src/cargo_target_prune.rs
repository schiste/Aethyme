//! Pruning superseded build units from a managed cargo target before the
//! whole tree is rotated.
//!
//! A managed gate cache that outgrows `max_bytes` used to be renamed aside
//! and deleted whole, so the next gate rebuilt everything from nothing. Most
//! of what makes it grow is not what the next build needs: every release
//! bumps the workspace version, which gives every workspace crate a new
//! metadata hash, and cargo keeps the previous generation's artefacts beside
//! the new ones. On 2026-11-21 the busiest shared target held 14.6 GiB, of
//! which 10.2 GiB were such superseded generations.
//!
//! A cargo unit is `.fingerprint/<package>-<hash>/` plus the `deps/` and
//! `examples/` files and the `build/<package>-<hash>/` directory carrying the
//! same hash. Units of one package with the same unit kinds (the hash files
//! in the fingerprint directory: `lib-x`, `test-lib-x`, `bin-y`,
//! `build-script-build-script-build`, `run-build-script-build-script-build`
//! ...) are generations of each other. An older generation is a candidate
//! only when nothing has read its fingerprint since the newest generation
//! was built: every cargo build that considers a unit reads its fingerprint
//! to decide freshness (and, for a unit's dependencies, theirs too), so a
//! generation a live build still uses -- one dependency in two feature sets,
//! or a build-script run whose `out` directory a kept unit compiles against
//! -- shows a later access time and is kept. Candidates go oldest first, only
//! until the requested number of bytes is staged or the deadline passes.
//!
//! Nothing is deleted here. Each pruned unit is renamed into one staging
//! directory on the same volume, fingerprint first, so an interrupted prune
//! leaves a unit cargo rebuilds, never one cargo trusts without its outputs.
//! The caller hands the staging directory to a deferred deleter. The caller
//! holds the cache's exclusive lease, so no build is using the tree.
//! Anything unexpected (an unreadable entry, a name that is not a cargo unit,
//! a rename that fails) is left in place; the caller still rotates the tree
//! when pruning did not bring it under its ceiling.
//!
//! [`UnitScope::Worker`] (the storage worker's prune of gate caches and of
//! the Cargo targets it manages) limits candidates to local packages --
//! workspace members and other path packages -- by the target's own records:
//! a unit's dep-info file
//! (`deps/<name>-<hash>.d`) lists the sources it was built from, and cargo
//! builds a registry or git dependency from its own source directories
//! (`<cargo home>/registry/src/...`, `<cargo home>/git/checkouts/...`). A
//! package is local only when some candidate unit's dep-info names sources
//! and none names a registry or git source; a package without readable
//! dep-info is never a candidate.
//!
//! The worker's in-use signal is also stricter than "nothing read it since
//! its successor was built" ([`worker_prunable`]): under `relatime` (and
//! other lazy access-time updates) a read is recorded only when the
//! previous recorded access is older than the file's modification or a day
//! old, so a generation read again soon after an earlier read keeps the
//! earlier time and looked unused. A superseded generation is pruned only
//! once its last recorded access (or modification) is older than the
//! artifact max age (a day when that is off). In a gate cache that signal
//! is also written, not just read: every gate advances the sources of the
//! tree it tests (#391), so cargo rebuilds -- rewrites -- every local unit
//! the gate uses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// What one prune staged.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct PruneOutcome {
    /// Superseded units moved into the staging directory.
    pub(crate) units: u64,
    /// Logical bytes of the regular files staged, the measure `max_bytes`
    /// is compared with. A unit's `build/` directory is not walked to size
    /// it and counts zero.
    pub(crate) bytes: u64,
    /// Superseded units found, staged or not.
    pub(crate) candidates: u64,
    /// The deadline passed before enough bytes were staged.
    pub(crate) deadline_stopped: bool,
    /// The staged units as `<profile>/.fingerprint/<package>-<hash>`
    /// relative to the target, to tell later whether cargo rebuilt one.
    pub(crate) staged: Vec<String>,
    /// Superseded units left alone because their package is not local
    /// ([`UnitScope::Worker`] only; not counted in `candidates`).
    pub(crate) not_local: u64,
    /// Superseded local units left alone because they were used within the
    /// rule's `unused_for` ([`UnitScope::Worker`] only; not counted in
    /// `candidates`).
    pub(crate) in_use: u64,
}

/// Which superseded units may be pruned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnitScope {
    /// Any package's, once nothing read it since its successor was built
    /// (the gate's own prune of a cache over its ceiling).
    Any,
    /// The storage worker's: local packages' only (workspace members and
    /// other path packages, by their dep-info files), once unused for the
    /// rule's time ([`worker_prunable`]).
    Worker(WorkerRule),
}

/// When the storage worker may prune a superseded local generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkerRule {
    pub(crate) now: SystemTime,
    /// How long its last recorded access (or modification) must be past:
    /// the artifact max age, or [`ACCESS_TIME_WINDOW`] when that is off.
    pub(crate) unused_for: Duration,
}

/// How long a lazily updated access time (`relatime`) may lag behind a
/// read: a read is recorded at the latest once the previous recorded access
/// is this old.
pub(crate) const ACCESS_TIME_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);

/// Whether the worker may prune a superseded generation of a package that is
/// `local`, last recorded as accessed or modified at `read_at`: local, and
/// unused for the rule's time. Nothing else about its successor is asked:
/// a recent read, recorded or not, shows up as an access within a day of
/// the last recorded one, and the newest generation is never superseded.
pub(crate) fn worker_prunable(rule: WorkerRule, local: bool, read_at: SystemTime) -> bool {
    local
        && rule
            .now
            .duration_since(read_at)
            .is_ok_and(|unused| unused >= rule.unused_for)
}

struct Unit {
    profile: PathBuf,
    package: String,
    profile_name: String,
    directory_name: String,
    hash: String,
    built_at: SystemTime,
    read_at: SystemTime,
    /// When the newest generation of its package and kinds was built.
    successor_built_at: SystemTime,
    /// Logical bytes of the fingerprint directory's files.
    bytes: u64,
}

/// A `deps/` or `examples/` entry of one unit and its logical size, from
/// the listing's own metadata.
struct Output {
    path: PathBuf,
    bytes: u64,
}

/// Move superseded units under `target` into `staging` (created on first
/// use, on the target's volume), oldest first, until at least `wanted_bytes`
/// logical bytes are staged, no candidate remains, or `deadline` passes.
pub(crate) fn prune_superseded_units(
    target: &Path,
    staging: &Path,
    wanted_bytes: u64,
    deadline: Instant,
) -> PruneOutcome {
    prune_superseded_units_in(target, staging, wanted_bytes, deadline, UnitScope::Any)
}

/// [`prune_superseded_units`] limited to the units `scope` admits.
pub(crate) fn prune_superseded_units_in(
    target: &Path,
    staging: &Path,
    wanted_bytes: u64,
    deadline: Instant,
    scope: UnitScope,
) -> PruneOutcome {
    let mut outcome = PruneOutcome::default();
    if wanted_bytes == 0 {
        return outcome;
    }
    let mut candidates = Vec::new();
    for profile in profiles(target) {
        let found = superseded_units(&profile);
        match scope {
            UnitScope::Any => candidates.extend(
                found
                    .into_iter()
                    .filter(|unit| unit.read_at <= unit.successor_built_at),
            ),
            UnitScope::Worker(rule) => {
                let local = local_packages(&profile, &found);
                for unit in found {
                    let is_local = local.contains(&unit.package);
                    if worker_prunable(rule, is_local, unit.read_at) {
                        candidates.push(unit);
                    } else if is_local {
                        outcome.in_use += 1;
                    } else {
                        outcome.not_local += 1;
                    }
                }
            }
        }
    }
    outcome.candidates = candidates.len() as u64;
    candidates.sort_by(|left, right| {
        left.built_at
            .cmp(&right.built_at)
            .then_with(|| left.directory_name.cmp(&right.directory_name))
    });
    let mut outputs_by_profile: BTreeMap<PathBuf, BTreeMap<String, Vec<Output>>> = BTreeMap::new();
    let mut staging_ready = false;
    let mut sequence = 0_u64;
    for unit in candidates {
        if outcome.bytes >= wanted_bytes {
            break;
        }
        if Instant::now() >= deadline {
            outcome.deadline_stopped = true;
            break;
        }
        if !staging_ready {
            if std::fs::create_dir(staging).is_err() || !is_real_directory(staging) {
                break;
            }
            staging_ready = true;
        }
        let outputs = outputs_by_profile
            .entry(unit.profile.clone())
            .or_insert_with(|| {
                let mut found = outputs_by_hash(&unit.profile.join("deps"));
                for (hash, mut more) in outputs_by_hash(&unit.profile.join("examples")) {
                    found.entry(hash).or_default().append(&mut more);
                }
                found
            });
        let mut stage = |source: &Path| -> bool {
            sequence += 1;
            let Some(name) = source.file_name() else {
                return false;
            };
            let destination = staging.join(format!("{sequence:06}-{}", name.to_string_lossy()));
            std::fs::rename(source, destination).is_ok()
        };
        let fingerprint = unit.profile.join(".fingerprint").join(&unit.directory_name);
        if !stage(&fingerprint) {
            // Cannot prove the unit gone without its fingerprint going
            // first; leave the rest of it.
            continue;
        }
        let mut staged_bytes = unit.bytes;
        for output in outputs.remove(&unit.hash).unwrap_or_default() {
            if stage(&output.path) {
                staged_bytes = staged_bytes.saturating_add(output.bytes);
            }
        }
        let build = unit.profile.join("build").join(&unit.directory_name);
        if std::fs::symlink_metadata(&build).is_ok() {
            stage(&build);
        }
        outcome.units += 1;
        outcome.bytes = outcome.bytes.saturating_add(staged_bytes);
        outcome.staged.push(format!(
            "{}/.fingerprint/{}",
            unit.profile_name, unit.directory_name
        ));
    }
    outcome
}

/// How many of `staged` (as [`PruneOutcome::staged`] names them) exist again
/// under `target`: units cargo rebuilt with the very hash a prune removed,
/// which means the prune misjudged them as unused.
pub(crate) fn rebuilt_units(target: &Path, staged: &[String]) -> u64 {
    staged
        .iter()
        .filter(|relative| {
            let relative = Path::new(relative.as_str());
            relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
                && is_real_directory(&target.join(relative))
        })
        .count() as u64
}

/// Profile directories directly under the target (`debug`, `release`, a
/// custom profile): those holding a `.fingerprint` directory.
fn profiles(target: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(target) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_real_directory(path) && is_real_directory(&path.join(".fingerprint")))
        .collect();
    found.sort();
    found
}

fn is_real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

/// `(package, hash)` for a cargo unit directory name `<package>-<16 hex>`.
fn unit_name(name: &str) -> Option<(&str, &str)> {
    let (package, hash) = name.rsplit_once('-')?;
    (!package.is_empty() && hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then_some((package, hash))
}

/// Every unit of this profile that a newer generation superseded, with when
/// that generation was built (the caller judges whether it is still used).
fn superseded_units(profile: &Path) -> Vec<Unit> {
    let Ok(entries) = std::fs::read_dir(profile.join(".fingerprint")) else {
        return Vec::new();
    };
    let profile_name = profile
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut generations: BTreeMap<(String, Vec<String>), Vec<Unit>> = BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((package, hash)) = unit_name(&name) else {
            continue;
        };
        let path = entry.path();
        if !is_real_directory(&path) {
            continue;
        }
        let Some(facts) = fingerprint_facts(&path) else {
            continue;
        };
        generations
            .entry((package.to_string(), facts.kinds))
            .or_default()
            .push(Unit {
                profile: profile.to_path_buf(),
                package: package.to_string(),
                profile_name: profile_name.clone(),
                directory_name: name.clone(),
                hash: hash.to_string(),
                built_at: facts.built_at,
                read_at: facts.read_at,
                successor_built_at: facts.built_at,
                bytes: facts.bytes,
            });
    }
    let mut superseded = Vec::new();
    for (_, mut units) in generations {
        if units.len() < 2 {
            continue;
        }
        units.sort_by_key(|unit| unit.built_at);
        let newest_built_at = units.last().map(|unit| unit.built_at);
        let Some(newest_built_at) = newest_built_at else {
            continue;
        };
        units.pop();
        superseded.extend(
            units
                .into_iter()
                .filter(|unit| unit.built_at < newest_built_at)
                .map(|mut unit| {
                    unit.successor_built_at = newest_built_at;
                    unit
                }),
        );
    }
    superseded
}

struct FingerprintFacts {
    /// The unit kinds: its hash files.
    kinds: Vec<String>,
    /// Newest file modification: when the unit was last built.
    built_at: SystemTime,
    /// Newest file access: when a build last considered the unit.
    read_at: SystemTime,
    bytes: u64,
}

/// Facts of one fingerprint directory. `None` when it holds no recognisable
/// hash file or a file cannot be inspected: such a unit is never a candidate.
fn fingerprint_facts(directory: &Path) -> Option<FingerprintFacts> {
    let mut facts = FingerprintFacts {
        kinds: Vec::new(),
        built_at: SystemTime::UNIX_EPOCH,
        read_at: SystemTime::UNIX_EPOCH,
        bytes: 0,
    };
    for entry in std::fs::read_dir(directory).ok()? {
        let entry = entry.ok()?;
        let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let modified = metadata.modified().ok()?;
        let accessed = metadata.accessed().ok()?;
        facts.built_at = facts.built_at.max(modified);
        facts.read_at = facts.read_at.max(accessed).max(modified);
        facts.bytes = facts.bytes.saturating_add(metadata.len());
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.ends_with(".json")
            || name.starts_with("dep-")
            || name.starts_with("output-")
            || name == "invoked.timestamp")
        {
            facts.kinds.push(name);
        }
    }
    if facts.kinds.is_empty() {
        return None;
    }
    facts.kinds.sort();
    Some(facts)
}

/// Where a unit's sources came from, by its dep-info file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceOrigin {
    /// A path package: a workspace member or another local package.
    Local,
    /// A registry or git dependency.
    External,
}

/// Path fragments of the source directories cargo builds registry and git
/// dependencies from, under its home.
const EXTERNAL_SOURCE_MARKERS: [&str; 2] = ["/registry/src/", "/git/checkouts/"];

/// The most of a dep-info file read to judge it.
const DEP_INFO_READ_LIMIT: u64 = 256 * 1024;

/// The origin a dep-info file's text gives: the sources of its first rule
/// (`<output>: <source> ...`). `None` when it names none.
fn origin_of_dep_info(text: &str) -> Option<SourceOrigin> {
    let (_, sources) = text.lines().next()?.split_once(": ")?;
    let sources = sources.trim();
    if sources.is_empty() {
        return None;
    }
    let sources = sources.replace('\\', "/");
    Some(
        if EXTERNAL_SOURCE_MARKERS
            .iter()
            .any(|marker| sources.contains(marker))
        {
            SourceOrigin::External
        } else {
            SourceOrigin::Local
        },
    )
}

/// The origin of the dep-info file at `path`, `None` when it cannot be read
/// or names no source.
fn dep_info_origin(path: &Path) -> Option<SourceOrigin> {
    use std::io::Read as _;
    let mut text = String::new();
    std::fs::File::open(path)
        .ok()?
        .take(DEP_INFO_READ_LIMIT)
        .read_to_string(&mut text)
        .ok()?;
    origin_of_dep_info(&text)
}

/// The packages of `units` (one profile's) that are local: some unit's
/// dep-info in `deps/` names local sources and none names an external one.
fn local_packages(profile: &Path, units: &[Unit]) -> std::collections::BTreeSet<String> {
    let outputs = outputs_by_hash(&profile.join("deps"));
    let mut verdicts: BTreeMap<&str, bool> = BTreeMap::new();
    for unit in units {
        let origin = outputs
            .get(&unit.hash)
            .into_iter()
            .flatten()
            .filter(|output| {
                output
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "d")
            })
            .find_map(|output| dep_info_origin(&output.path));
        match origin {
            Some(SourceOrigin::External) => {
                verdicts.insert(&unit.package, false);
            }
            Some(SourceOrigin::Local) => {
                verdicts.entry(&unit.package).or_insert(true);
            }
            None => {}
        }
    }
    verdicts
        .into_iter()
        .filter(|(_, local)| *local)
        .map(|(package, _)| package.to_string())
        .collect()
}

/// Entries of `deps/` or `examples/` by the unit hash in their name:
/// `<stem>-<hash>` with an optional extension (`.rlib`, `.rmeta`, `.d`,
/// `.dSYM`, ...). Sizes come from this listing; a directory counts zero.
fn outputs_by_hash(directory: &Path) -> BTreeMap<String, Vec<Output>> {
    let mut found: BTreeMap<String, Vec<Output>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(directory) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stem = name.split_once('.').map_or(name.as_str(), |(stem, _)| stem);
        let Some((_, hash)) = unit_name(stem) else {
            continue;
        };
        let bytes = entry
            .metadata()
            .ok()
            .filter(std::fs::Metadata::is_file)
            .map_or(0, |metadata| metadata.len());
        found.entry(hash.to_string()).or_default().push(Output {
            path: entry.path(),
            bytes,
        });
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const OLD: &str = "aaaaaaaaaaaaaaaa";
    const NEW: &str = "bbbbbbbbbbbbbbbb";
    const ALTERNATE: &str = "cccccccccccccccc";
    const SOLE: &str = "dddddddddddddddd";

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000 + seconds)
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(60)
    }

    fn file(path: &Path, bytes: usize, modified: SystemTime, accessed: SystemTime) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0_u8; bytes]).unwrap();
        let handle = std::fs::File::options().write(true).open(path).unwrap();
        handle
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(modified)
                    .set_accessed(accessed),
            )
            .unwrap();
    }

    /// One library unit: its fingerprint (built at `built`, last read at
    /// `read`), a `bytes`-long rlib and a dep-info file.
    fn unit(profile: &Path, package: &str, hash: &str, built: u64, read: u64, bytes: usize) {
        let fingerprint = profile
            .join(".fingerprint")
            .join(format!("{package}-{hash}"));
        let crate_name = package.replace('-', "_");
        for name in [
            format!("lib-{crate_name}"),
            format!("lib-{crate_name}.json"),
            format!("dep-lib-{crate_name}"),
        ] {
            file(&fingerprint.join(name), 8, at(built), at(read));
        }
        file(
            &profile
                .join("deps")
                .join(format!("lib{crate_name}-{hash}.rlib")),
            bytes,
            at(built),
            at(read),
        );
        file(
            &profile.join("deps").join(format!("{crate_name}-{hash}.d")),
            4,
            at(built),
            at(read),
        );
    }

    /// One build-script run unit: its fingerprint and its `out` directory.
    fn run_unit(profile: &Path, package: &str, hash: &str, built: u64, read: u64) {
        let name = format!("{package}-{hash}");
        let fingerprint = profile.join(".fingerprint").join(&name);
        for kind in [
            "run-build-script-build-script-build",
            "run-build-script-build-script-build.json",
        ] {
            file(&fingerprint.join(kind), 8, at(built), at(read));
        }
        file(
            &profile.join("build").join(&name).join("out/generated.rs"),
            32,
            at(built),
            at(read),
        );
    }

    fn staged_names(staging: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(staging)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    #[test]
    fn stages_only_generations_nobody_read_since_their_successor_was_built() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let staging = tmp.path().join("staging");
        let profile = target.join("debug");
        // A workspace crate, superseded by a version bump and never read again.
        unit(&profile, "work-crate", OLD, 10, 20, 1_000);
        unit(&profile, "work-crate", NEW, 100, 110, 1_000);
        // A dependency built in two feature sets that builds still alternate
        // between: the older one was read after the newer one was built.
        unit(&profile, "dependency", ALTERNATE, 10, 150, 500);
        unit(&profile, "dependency", NEW, 100, 120, 500);
        // A crate with one generation only.
        unit(&profile, "alone", SOLE, 10, 10, 700);
        // A build script output for the superseded workspace generation.
        file(
            &profile.join(format!("build/work-crate-{OLD}/output")),
            50,
            at(10),
            at(10),
        );

        let outcome = prune_superseded_units(&target, &staging, u64::MAX, later());

        assert_eq!(outcome.candidates, 1);
        assert_eq!(outcome.units, 1);
        assert!(!outcome.deadline_stopped);
        // The fingerprint and both deps files; the build directory is moved
        // but not walked to size it.
        assert_eq!(outcome.bytes, 3 * 8 + 1_000 + 4);
        assert_eq!(
            outcome.staged,
            vec![format!("debug/.fingerprint/work-crate-{OLD}")]
        );
        // Moved, not deleted: the fingerprint first.
        let staged = staged_names(&staging);
        assert_eq!(staged.len(), 4, "{staged:?}");
        assert_eq!(staged[0], format!("000001-work-crate-{OLD}"));
        assert!(staged.contains(&format!("000004-work-crate-{OLD}")));
        for gone in [
            format!(".fingerprint/work-crate-{OLD}"),
            format!("deps/libwork_crate-{OLD}.rlib"),
            format!("deps/work_crate-{OLD}.d"),
            format!("build/work-crate-{OLD}"),
        ] {
            assert!(!profile.join(&gone).exists(), "{gone} must be staged");
        }
        for kept in [
            format!(".fingerprint/work-crate-{NEW}"),
            format!("deps/libwork_crate-{NEW}.rlib"),
            format!(".fingerprint/dependency-{ALTERNATE}"),
            format!("deps/libdependency-{ALTERNATE}.rlib"),
            format!(".fingerprint/dependency-{NEW}"),
            format!(".fingerprint/alone-{SOLE}"),
            format!("deps/libalone-{SOLE}.rlib"),
        ] {
            assert!(profile.join(&kept).exists(), "{kept} must be kept");
        }
        assert_eq!(rebuilt_units(&target, &outcome.staged), 0);
        // Cargo rebuilding the very unit is what the rebuild count reports.
        unit(&profile, "work-crate", OLD, 200, 200, 1_000);
        assert_eq!(rebuilt_units(&target, &outcome.staged), 1);
        assert_eq!(rebuilt_units(&target, &["../escape".to_string()]), 0);
    }

    /// A build-script run whose `out` directory a kept unit compiles against
    /// is read by every build of that unit, so it is kept even though a newer
    /// run generation exists.
    #[test]
    fn keeps_a_build_script_run_a_kept_unit_still_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let staging = tmp.path().join("staging");
        let profile = target.join("debug");
        run_unit(&profile, "native-sys", OLD, 10, 150);
        run_unit(&profile, "native-sys", NEW, 100, 110);
        unit(&profile, "native-user", SOLE, 10, 150, 100);

        let outcome = prune_superseded_units(&target, &staging, u64::MAX, later());

        assert_eq!(outcome.candidates, 0);
        assert_eq!(outcome.units, 0);
        assert!(
            profile
                .join(format!("build/native-sys-{OLD}/out/generated.rs"))
                .is_file()
        );
        assert!(!staging.exists(), "nothing staged, nothing created");
    }

    #[test]
    fn stops_once_enough_bytes_are_staged_oldest_generation_first() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let profile = target.join("debug");
        unit(&profile, "first", OLD, 10, 10, 1_000);
        unit(&profile, "first", NEW, 100, 100, 1_000);
        unit(&profile, "second", OLD, 20, 20, 1_000);
        unit(&profile, "second", NEW, 100, 100, 1_000);

        let outcome = prune_superseded_units(&target, &tmp.path().join("staging"), 500, later());

        assert_eq!(outcome.candidates, 2);
        assert_eq!(outcome.units, 1);
        assert!(!profile.join(format!(".fingerprint/first-{OLD}")).exists());
        assert!(profile.join(format!(".fingerprint/second-{OLD}")).exists());
        assert_eq!(
            prune_superseded_units(&target, &tmp.path().join("unused"), 0, later()),
            PruneOutcome::default()
        );
    }

    #[test]
    fn a_passed_deadline_stages_nothing_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let profile = target.join("debug");
        unit(&profile, "first", OLD, 10, 10, 1_000);
        unit(&profile, "first", NEW, 100, 100, 1_000);

        let outcome = prune_superseded_units(
            &target,
            &tmp.path().join("staging"),
            u64::MAX,
            Instant::now(),
        );

        assert_eq!(outcome.candidates, 1);
        assert_eq!(outcome.units, 0);
        assert!(outcome.deadline_stopped);
        assert!(profile.join(format!(".fingerprint/first-{OLD}")).exists());
    }

    fn dep_info(profile: &Path, package: &str, hash: &str, source: &str) {
        let crate_name = package.replace('-', "_");
        let deps = profile.join("deps");
        std::fs::write(
            deps.join(format!("{crate_name}-{hash}.d")),
            format!(
                "{}: {source}\n\n{source}:\n",
                deps.join(format!("lib{crate_name}-{hash}.rlib")).display()
            ),
        )
        .unwrap();
    }

    #[test]
    fn a_dep_info_names_a_local_or_an_external_origin() {
        use SourceOrigin::*;
        for (text, origin) in [
            (
                "/t/deps/libx-1.rlib: crates/x/src/lib.rs crates/x/src/a.rs\n",
                Some(Local),
            ),
            (
                "/t/deps/libx-1.rlib: /repo/crates/x/src/lib.rs\n",
                Some(Local),
            ),
            (
                "/t/deps/libserde-1.rlib: /home/u/.cargo/registry/src/index.crates.io-1/serde-1.0.0/src/lib.rs\n",
                Some(External),
            ),
            (
                "/t/deps/libdep-1.rlib: /home/u/.cargo/git/checkouts/dep-1/abc/src/lib.rs\n",
                Some(External),
            ),
            (
                "C:\\t\\deps\\libx-1.rlib: C:\\Users\\u\\.cargo\\registry\\src\\i\\x-1\\src\\lib.rs\n",
                Some(External),
            ),
            ("/t/deps/libx-1.rlib:\n", None),
            ("", None),
            ("not a rule\n", None),
        ] {
            assert_eq!(origin_of_dep_info(text), origin, "{text}");
        }
    }

    /// The storage worker's scope prunes superseded units of local packages
    /// only: never a registry dependency's, nor a package whose dep-info
    /// cannot tell.
    #[test]
    fn the_local_scope_prunes_only_superseded_local_packages() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let staging = tmp.path().join("staging");
        let profile = target.join("debug");
        // Unit hashes are unique in a cargo target.
        const DEP_OLD: &str = "1111111111111111";
        const DEP_NEW: &str = "2222222222222222";
        const UNKNOWN_OLD: &str = "3333333333333333";
        const UNKNOWN_NEW: &str = "4444444444444444";
        unit(&profile, "work-crate", OLD, 10, 20, 1_000);
        unit(&profile, "work-crate", NEW, 100, 110, 1_000);
        dep_info(&profile, "work-crate", OLD, "crates/work-crate/src/lib.rs");
        unit(&profile, "registry-dep", DEP_OLD, 10, 20, 500);
        unit(&profile, "registry-dep", DEP_NEW, 100, 110, 500);
        dep_info(
            &profile,
            "registry-dep",
            DEP_OLD,
            "/home/u/.cargo/registry/src/index.crates.io-1/registry-dep-1.0.0/src/lib.rs",
        );
        // An empty dep-info says nothing: the package is not judged local.
        unit(&profile, "unknown", UNKNOWN_OLD, 10, 20, 300);
        unit(&profile, "unknown", UNKNOWN_NEW, 100, 110, 300);
        std::fs::write(profile.join(format!("deps/unknown-{UNKNOWN_OLD}.d")), "").unwrap();

        let outcome = prune_superseded_units_in(
            &target,
            &staging,
            u64::MAX,
            later(),
            UnitScope::Worker(WorkerRule {
                now: at(110) + ACCESS_TIME_WINDOW,
                unused_for: ACCESS_TIME_WINDOW,
            }),
        );

        assert_eq!(outcome.candidates, 1);
        assert_eq!(outcome.not_local, 2);
        assert_eq!(outcome.in_use, 0);
        assert_eq!(
            outcome.staged,
            vec![format!("debug/.fingerprint/work-crate-{OLD}")]
        );
        assert!(
            !profile
                .join(format!("deps/libwork_crate-{OLD}.rlib"))
                .exists()
        );
        for kept in [
            format!(".fingerprint/work-crate-{NEW}"),
            format!(".fingerprint/registry-dep-{DEP_OLD}"),
            format!("deps/libregistry_dep-{DEP_OLD}.rlib"),
            format!(".fingerprint/unknown-{UNKNOWN_OLD}"),
        ] {
            assert!(profile.join(&kept).exists(), "{kept} must be kept");
        }
    }

    /// The worker's rule: local packages only, and only once the last
    /// recorded access is past `unused_for`, however it relates to the
    /// successor's build (a lazily updated access time may lag a day).
    #[test]
    fn the_worker_rule_needs_a_local_package_unused_for_its_time() {
        let rule = WorkerRule {
            now: at(1_000_000),
            unused_for: ACCESS_TIME_WINDOW,
        };
        let day = ACCESS_TIME_WINDOW.as_secs();
        assert!(worker_prunable(rule, true, at(1_000_000 - day)));
        assert!(worker_prunable(rule, true, at(10)));
        assert!(!worker_prunable(rule, true, at(1_000_000 - day + 1)));
        assert!(
            !worker_prunable(rule, false, at(10)),
            "never a dependency's"
        );
        // An access time ahead of the clock is not "unused".
        assert!(!worker_prunable(rule, true, at(2_000_000)));
    }

    /// Finding (b) of #2041: a generation read again after its successor was
    /// built can keep an earlier access time under `relatime`. The legacy
    /// rule ("not read since the successor was built") prunes it; the
    /// worker's keeps it until its last recorded access is a day old.
    #[test]
    fn a_generation_read_within_the_access_window_is_kept_by_the_worker() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        let profile = target.join("debug");
        unit(&profile, "work-crate", OLD, 10, 20, 1_000);
        unit(&profile, "work-crate", NEW, 100, 110, 1_000);
        std::fs::write(
            profile.join(format!("deps/work_crate-{OLD}.d")),
            format!("/t/deps/libwork_crate-{OLD}.rlib: crates/work/src/lib.rs\n"),
        )
        .unwrap();
        let rule = |now: SystemTime| {
            UnitScope::Worker(WorkerRule {
                now,
                unused_for: ACCESS_TIME_WINDOW,
            })
        };
        // An hour after the successor: the legacy rule would prune it.
        let kept = prune_superseded_units_in(
            &target,
            &tmp.path().join("staging-1"),
            u64::MAX,
            later(),
            rule(at(110 + 3_600)),
        );
        assert_eq!((kept.units, kept.in_use), (0, 1));
        assert!(
            profile
                .join(format!(".fingerprint/work-crate-{OLD}"))
                .exists()
        );
        // A day after its last recorded access: pruned.
        let pruned = prune_superseded_units_in(
            &target,
            &tmp.path().join("staging-2"),
            u64::MAX,
            later(),
            rule(at(20) + ACCESS_TIME_WINDOW),
        );
        assert_eq!(pruned.units, 1);
        assert!(
            !profile
                .join(format!(".fingerprint/work-crate-{OLD}"))
                .exists()
        );
        assert!(
            profile
                .join(format!(".fingerprint/work-crate-{NEW}"))
                .exists()
        );
    }

    #[test]
    fn leaves_a_tree_that_is_not_a_cargo_target_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        std::fs::create_dir_all(target.join("debug/deps")).unwrap();
        std::fs::write(target.join("large"), b"1234").unwrap();
        std::fs::write(target.join("debug/deps/libx-aaaaaaaaaaaaaaaa.rlib"), b"1").unwrap();

        assert_eq!(
            prune_superseded_units(&target, &tmp.path().join("staging"), u64::MAX, later()),
            PruneOutcome::default()
        );
        assert!(target.join("large").exists());
        assert!(
            target
                .join("debug/deps/libx-aaaaaaaaaaaaaaaa.rlib")
                .exists()
        );
    }
}
