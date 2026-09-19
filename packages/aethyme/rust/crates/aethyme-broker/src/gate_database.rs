//! Repository-bound gate overrides, with the legacy override as a safe fallback.
//!
//! A gate's children can open independent fixture repositories. Redirecting all
//! of them to one SQLite file merges their sessions and leases. Only the gate's
//! canonical primary checkout (including linked worktrees) needs redirection.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) const SCOPE_ENV: &str = "AETHYME_GATE_BROKER_DATABASES";

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Scope {
    fallback: PathBuf,
    repositories: BTreeMap<PathBuf, PathBuf>,
}

fn parse(value: &OsStr) -> Option<Scope> {
    let scope: Scope = serde_json::from_str(value.to_str()?).ok()?;
    (scope.fallback.is_absolute()
        && scope.repositories.values().any(|db| db == &scope.fallback)
        && scope
            .repositories
            .iter()
            .all(|(root, db)| root.is_absolute() && db.is_absolute()))
    .then_some(scope)
}

/// Preserve ancestor gate bindings so an inner gate cannot expose an outer
/// gate's primary checkout. Failure to identify the repository refuses launch.
pub(crate) fn for_child(
    cwd: &Path,
    database: &Path,
    inherited: Option<&OsStr>,
) -> std::io::Result<String> {
    let root = crate::GitRepo::discover(cwd)
        .and_then(|repo| repo.main_root())
        .map_err(std::io::Error::other)?
        .canonicalize()?;
    let mut scope = match inherited {
        Some(value) => parse(value).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid inherited gate database scope; refusing to drop ancestor isolation",
            )
        })?,
        None => Scope::default(),
    };
    // The tempfile parent exists already; the database need not exist yet.
    let database = database
        .parent()
        .ok_or_else(|| std::io::Error::other("gate database has no parent"))?
        .canonicalize()?
        .join(
            database
                .file_name()
                .ok_or_else(|| std::io::Error::other("gate database has no filename"))?,
        );
    scope.repositories.insert(root, database.clone());
    scope.fallback = database;
    serde_json::to_string(&scope).map_err(std::io::Error::other)
}

pub(crate) fn resolve(
    repo_root: &Path,
    override_value: Option<&OsStr>,
    scope_value: Option<&OsStr>,
) -> PathBuf {
    let Some(pinned) = override_value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    else {
        return repo_root.join(crate::BROKER_DB_RELPATH);
    };
    let Some(scope) = scope_value.and_then(parse) else {
        return pinned;
    };
    // A harness can still explicitly pin a different database. Scope metadata
    // only qualifies the exact legacy override exported by the gate runner.
    if pinned != scope.fallback {
        return pinned;
    }
    let Ok(root) = repo_root.canonicalize() else {
        // Uncertain identity must not fall back to a potentially live DB.
        return pinned;
    };
    scope
        .repositories
        .get(&root)
        .cloned()
        .unwrap_or_else(|| repo_root.join(crate::BROKER_DB_RELPATH))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(root: &Path, database: &Path) -> String {
        serde_json::to_string(&Scope {
            fallback: database.to_owned(),
            repositories: BTreeMap::from([(root.canonicalize().unwrap(), database.to_owned())]),
        })
        .unwrap()
    }

    #[test]
    fn scope_protects_the_primary_but_does_not_combine_independent_repositories() {
        let primary = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let db = primary.path().join("isolated.db");
        let scope = encoded(primary.path(), &db);
        for root in [primary.path(), &primary.path().join(".")] {
            assert_eq!(
                resolve(root, Some(db.as_os_str()), Some(OsStr::new(&scope))),
                db
            );
        }
        for fixture in [first.path(), second.path()] {
            assert_eq!(
                resolve(fixture, Some(db.as_os_str()), Some(OsStr::new(&scope))),
                fixture.join(crate::BROKER_DB_RELPATH)
            );
        }
    }

    #[test]
    fn explicit_overrides_and_unset_overrides_keep_their_existing_meaning() {
        let repo = tempfile::tempdir().unwrap();
        let db = repo.path().join("gate.db");
        let scope = encoded(repo.path(), &db);
        let explicit = OsStr::new("relative-explicit.db");
        assert_eq!(
            resolve(repo.path(), Some(explicit), Some(OsStr::new(&scope))),
            PathBuf::from(explicit)
        );
        for empty in [None, Some(OsStr::new(""))] {
            assert_eq!(
                resolve(repo.path(), empty, Some(OsStr::new(&scope))),
                repo.path().join(crate::BROKER_DB_RELPATH)
            );
        }
    }

    #[test]
    fn invalid_scope_or_unknown_repository_identity_keeps_the_safe_override() {
        let repo = tempfile::tempdir().unwrap();
        let db = repo.path().join("gate.db");
        let scope = encoded(repo.path(), &db);
        for invalid in [
            "",
            "{}",
            "not json",
            r#"{"fallback":"relative.db","repositories":{}}"#,
        ] {
            assert_eq!(
                resolve(repo.path(), Some(db.as_os_str()), Some(OsStr::new(invalid))),
                db
            );
        }
        assert_eq!(
            resolve(
                &repo.path().join("missing"),
                Some(db.as_os_str()),
                Some(OsStr::new(&scope))
            ),
            db
        );
    }
}
