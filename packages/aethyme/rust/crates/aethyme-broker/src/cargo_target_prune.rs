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
//! A cargo unit is `.fingerprint/<package>-<hash>/` plus the `deps/` files
//! and `build/<package>-<hash>/` directory carrying the same hash. Units of
//! one package with the same unit kinds (the hash files in the fingerprint
//! directory: `lib-x`, `test-lib-x`, `bin-y`, `build-script-build-script-build`
//! ...) are generations of each other. An older generation is a candidate
//! only when nothing has read its fingerprint since the newest generation
//! was built: every cargo build that considers a unit reads its fingerprint
//! to decide freshness, so a generation a live build still alternates with
//! (two feature sets of one dependency, say) shows a later access time and
//! is kept. Candidates go oldest first, only until the requested number of
//! bytes is freed.
//!
//! The caller holds the cache's exclusive lease, so no build is using the
//! tree. The fingerprint directory goes first: a prune interrupted after that
//! leaves a unit cargo rebuilds, never one cargo trusts without its outputs.
//! Anything unexpected (an unreadable entry, a name that is not a cargo unit)
//! is left alone; the caller still rotates the tree when pruning did not
//! bring it under its ceiling.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// What one prune removed.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PruneOutcome {
    /// Superseded units removed.
    pub(crate) units: u64,
    /// Logical bytes of the regular files removed, the measure `max_bytes`
    /// is compared with.
    pub(crate) bytes: u64,
    /// Superseded units found, removed or not.
    pub(crate) candidates: u64,
}

struct Unit {
    profile: PathBuf,
    directory_name: String,
    hash: String,
    built_at: SystemTime,
    read_at: SystemTime,
}

/// Remove superseded units under `target`, oldest first, until at least
/// `wanted_bytes` logical bytes are freed or no candidate remains.
pub(crate) fn prune_superseded_units(target: &Path, wanted_bytes: u64) -> PruneOutcome {
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
    let mut deps_by_profile: BTreeMap<PathBuf, BTreeMap<String, Vec<PathBuf>>> = BTreeMap::new();
    for unit in candidates {
        if outcome.bytes >= wanted_bytes {
            break;
        }
        let deps = deps_by_profile
            .entry(unit.profile.clone())
            .or_insert_with(|| deps_by_hash(&unit.profile.join("deps")));
        let fingerprint = unit.profile.join(".fingerprint").join(&unit.directory_name);
        let Some(fingerprint_bytes) = remove_tree(&fingerprint) else {
            // Cannot prove the unit gone without its fingerprint going
            // first; leave the rest of it.
            continue;
        };
        let mut freed = fingerprint_bytes;
        for path in deps.remove(&unit.hash).unwrap_or_default() {
            freed = freed.saturating_add(remove_tree(&path).unwrap_or(0));
        }
        let build = unit.profile.join("build").join(&unit.directory_name);
        if std::fs::symlink_metadata(&build).is_ok() {
            freed = freed.saturating_add(remove_tree(&build).unwrap_or(0));
        }
        outcome.units += 1;
        outcome.bytes = outcome.bytes.saturating_add(freed);
    }
    outcome
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
        let Some((kinds, built_at, read_at)) = fingerprint_facts(&path) else {
            continue;
        };
        generations
            .entry((package.to_string(), kinds))
            .or_default()
            .push(Unit {
                profile: profile.to_path_buf(),
                directory_name: name.clone(),
                hash: hash.to_string(),
                built_at,
                read_at,
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

/// The unit kinds (its hash files), when it was last built (newest file
/// modification) and last read (newest file access) from one fingerprint
/// directory. `None` when it holds no recognisable hash file or a file
/// cannot be inspected: such a unit is never a candidate.
fn fingerprint_facts(directory: &Path) -> Option<(Vec<String>, SystemTime, SystemTime)> {
    let mut kinds = Vec::new();
    let mut built_at = SystemTime::UNIX_EPOCH;
    let mut read_at = SystemTime::UNIX_EPOCH;
    for entry in std::fs::read_dir(directory).ok()? {
        let entry = entry.ok()?;
        let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
        if !metadata.is_file() {
            return None;
        }
        let modified = metadata.modified().ok()?;
        let accessed = metadata.accessed().ok()?;
        built_at = built_at.max(modified);
        read_at = read_at.max(accessed).max(modified);
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.ends_with(".json")
            || name.starts_with("dep-")
            || name.starts_with("output-")
            || name == "invoked.timestamp")
        {
            kinds.push(name);
        }
    }
    if kinds.is_empty() {
        return None;
    }
    kinds.sort();
    Some((kinds, built_at, read_at))
}

/// `deps/` entries by the unit hash in their name: `<stem>-<hash>` with an
/// optional extension (`.rlib`, `.rmeta`, `.d`, `.dSYM`, ...).
fn deps_by_hash(deps: &Path) -> BTreeMap<String, Vec<PathBuf>> {
    let mut found: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(deps) else {
        return found;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let stem = name.split_once('.').map_or(name.as_str(), |(stem, _)| stem);
        if let Some((_, hash)) = unit_name(stem) {
            found
                .entry(hash.to_string())
                .or_default()
                .push(entry.path());
        }
    }
    found
}

/// Remove a file or a directory tree without following links, returning the
/// logical bytes of the regular files it held. `None` when nothing could be
/// removed.
fn remove_tree(path: &Path) -> Option<u64> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if metadata.is_dir() {
        let bytes = crate::disk_headroom::directory_usage_best_effort(path).bytes;
        std::fs::remove_dir_all(path).ok()?;
        Some(bytes)
    } else {
        let bytes = if metadata.is_file() {
            metadata.len()
        } else {
            0
        };
        std::fs::remove_file(path).ok()?;
        Some(bytes)
    }
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

    /// One unit: its fingerprint (built at `built`, last read at `read`) and
    /// a `bytes`-long rlib.
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

    #[test]
    fn prunes_only_generations_nobody_read_since_their_successor_was_built() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("debug");
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

        let outcome = prune_superseded_units(tmp.path(), u64::MAX);

        assert_eq!(outcome.candidates, 1);
        assert_eq!(outcome.units, 1);
        assert_eq!(outcome.bytes, 3 * 8 + 1_000 + 4 + 50);
        assert!(
            !profile
                .join(format!(".fingerprint/work-crate-{OLD}"))
                .exists()
        );
        assert!(
            !profile
                .join(format!("deps/libwork_crate-{OLD}.rlib"))
                .exists()
        );
        assert!(!profile.join(format!("deps/work_crate-{OLD}.d")).exists());
        assert!(!profile.join(format!("build/work-crate-{OLD}")).exists());
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
    }

    #[test]
    fn stops_once_enough_bytes_are_freed_oldest_generation_first() {
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join("debug");
        unit(&profile, "first", OLD, 10, 10, 1_000);
        unit(&profile, "first", NEW, 100, 100, 1_000);
        unit(&profile, "second", OLD, 20, 20, 1_000);
        unit(&profile, "second", NEW, 100, 100, 1_000);

        let outcome = prune_superseded_units(tmp.path(), 500);

        assert_eq!(outcome.candidates, 2);
        assert_eq!(outcome.units, 1);
        assert!(!profile.join(format!(".fingerprint/first-{OLD}")).exists());
        assert!(profile.join(format!(".fingerprint/second-{OLD}")).exists());
        assert_eq!(
            prune_superseded_units(tmp.path(), 0),
            PruneOutcome::default()
        );
    }

    #[test]
    fn leaves_a_tree_that_is_not_a_cargo_target_alone() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("large"), b"1234").unwrap();
        std::fs::create_dir_all(tmp.path().join("debug/deps")).unwrap();
        std::fs::write(
            tmp.path().join("debug/deps/libx-aaaaaaaaaaaaaaaa.rlib"),
            b"1",
        )
        .unwrap();

        assert_eq!(
            prune_superseded_units(tmp.path(), u64::MAX),
            PruneOutcome::default()
        );
        assert!(tmp.path().join("large").exists());
        assert!(
            tmp.path()
                .join("debug/deps/libx-aaaaaaaaaaaaaaaa.rlib")
                .exists()
        );
    }
}
