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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

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
}

struct Unit {
    profile: PathBuf,
    profile_name: String,
    directory_name: String,
    hash: String,
    built_at: SystemTime,
    read_at: SystemTime,
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
    let mut outcome = PruneOutcome::default();
    if wanted_bytes == 0 {
        return outcome;
    }
    let mut candidates = Vec::new();
    for profile in profiles(target) {
        candidates.extend(superseded_units(&profile));
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

/// Every unit of this profile that a newer generation superseded and that
/// nothing read since that generation was built.
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
                profile_name: profile_name.clone(),
                directory_name: name.clone(),
                hash: hash.to_string(),
                built_at: facts.built_at,
                read_at: facts.read_at,
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
                .filter(|unit| unit.built_at < newest_built_at && unit.read_at <= newest_built_at),
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
