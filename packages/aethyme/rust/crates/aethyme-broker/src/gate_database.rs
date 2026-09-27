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

/// Where a child process's broker database lives.
///
/// Outside a gate (no scope) this is the historical rule: the override
/// verbatim, else `<repo>/.aethyme/broker.db`. Inside a gate the scope names
/// the repositories whose real storage the gate must never touch (#361), and
/// resolution fails closed around them:
///
/// - a protected repository always resolves to its disposable database, even
///   when a child removed or blanked the override -- dropping an environment
///   variable must not be how a gate binary reaches the operator's state;
/// - an explicit override naming a protected repository's real database is
///   refused rather than honoured;
/// - an unreadable scope is refused rather than ignored, because ignoring it
///   is exactly the fallback to the live database the scope exists to stop.
///
/// Independent fixture repositories keep their own `<repo>/.aethyme/broker.db`,
/// and an explicit override that names some other file still wins, so a test
/// harness nested in a gate can pin a database it owns.
pub(crate) fn resolve(
    repo_root: &Path,
    override_value: Option<&OsStr>,
    scope_value: Option<&OsStr>,
) -> Result<PathBuf, String> {
    let pinned = override_value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    let Some(scope_value) = scope_value else {
        return Ok(pinned.unwrap_or_else(|| repo_root.join(crate::BROKER_DB_RELPATH)));
    };
    let scope = parse(scope_value).ok_or_else(|| {
        format!(
            "{SCOPE_ENV} is set but unreadable; refusing to resolve broker state \
             that could be the host's shared database"
        )
    })?;
    if let Some(pinned) = pinned.as_ref().filter(|pinned| **pinned != scope.fallback) {
        if let Some(root) = scope.protected_root_of(pinned) {
            return Err(format!(
                "{} names the shared broker database of {}, which this gate protects",
                crate::BROKER_DB_ENV,
                root.display()
            ));
        }
        return Ok(pinned.clone());
    }
    let Ok(root) = repo_root.canonicalize() else {
        // Uncertain identity must not fall back to a potentially live DB.
        return Ok(scope.fallback);
    };
    Ok(scope
        .repositories
        .get(&root)
        .cloned()
        .unwrap_or_else(|| repo_root.join(crate::BROKER_DB_RELPATH)))
}

impl Scope {
    /// The protected repository whose real database `path` names, if any.
    fn protected_root_of(&self, path: &Path) -> Option<&Path> {
        let canonical = path
            .parent()
            .and_then(|parent| parent.canonicalize().ok())
            .zip(path.file_name())
            .map(|(parent, name)| parent.join(name));
        self.repositories.keys().map(PathBuf::as_path).find(|root| {
            let live = root.join(crate::BROKER_DB_RELPATH);
            path == live || canonical.as_deref() == Some(live.as_path())
        })
    }
}

/// The broker database a gate child will resolve, recorded before it runs.
///
/// Serialised into the gate log ahead of the command's output and into the
/// gate's JSON result, so which database a verdict could have written is a
/// fact on record rather than an inference from environment variables.
#[derive(Debug, Clone, Serialize)]
pub struct GateBrokerDatabase {
    /// The disposable database every protected repository resolves to.
    pub path: PathBuf,
    /// Always `"disposable"`: gate children never receive shared state.
    pub kind: &'static str,
    /// Repository roots whose shared database the child is refused.
    pub protected_repositories: Vec<PathBuf>,
    /// What happens to `path` afterwards: removed when the gate exits.
    pub retention: &'static str,
}

impl GateBrokerDatabase {
    pub(crate) fn from_scope(encoded: &str) -> std::io::Result<Self> {
        let scope = parse(OsStr::new(encoded)).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "unreadable gate scope")
        })?;
        Ok(Self {
            path: scope.fallback,
            kind: "disposable",
            protected_repositories: scope.repositories.into_keys().collect(),
            retention: "removed_on_gate_exit",
        })
    }
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

    fn scoped(root: &Path, pinned: Option<&OsStr>, scope: &str) -> Result<PathBuf, String> {
        resolve(root, pinned, Some(OsStr::new(scope)))
    }

    #[test]
    fn scope_protects_the_primary_but_does_not_combine_independent_repositories() {
        let primary = tempfile::tempdir().unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let db = primary.path().join("isolated.db");
        let scope = encoded(primary.path(), &db);
        for root in [primary.path(), &primary.path().join(".")] {
            assert_eq!(scoped(root, Some(db.as_os_str()), &scope), Ok(db.clone()));
        }
        for fixture in [first.path(), second.path()] {
            assert_eq!(
                scoped(fixture, Some(db.as_os_str()), &scope),
                Ok(fixture.join(crate::BROKER_DB_RELPATH))
            );
        }
    }

    #[test]
    fn without_a_scope_resolution_is_unchanged() {
        let repo = tempfile::tempdir().unwrap();
        let pinned = OsStr::new("relative-explicit.db");
        assert_eq!(
            resolve(repo.path(), Some(pinned), None),
            Ok(PathBuf::from(pinned))
        );
        for empty in [None, Some(OsStr::new(""))] {
            assert_eq!(
                resolve(repo.path(), empty, None),
                Ok(repo.path().join(crate::BROKER_DB_RELPATH))
            );
        }
    }

    #[test]
    fn an_explicit_override_naming_another_file_still_wins_inside_a_gate() {
        let repo = tempfile::tempdir().unwrap();
        let scope = encoded(repo.path(), &repo.path().join("gate.db"));
        let pinned = OsStr::new("relative-explicit.db");
        assert_eq!(
            scoped(repo.path(), Some(pinned), &scope),
            Ok(PathBuf::from(pinned))
        );
    }

    // #361: a child that removes or blanks AETHYME_BROKER_DB but keeps the
    // scope used to resolve `<primary>/.aethyme/broker.db` -- the operator's
    // shared database -- and a dirty-tree migration then ran against it.
    #[test]
    fn a_removed_override_cannot_reach_the_protected_shared_database() {
        let repo = tempfile::tempdir().unwrap();
        let db = repo.path().canonicalize().unwrap().join("gate.db");
        let scope = encoded(repo.path(), &db);
        for removed in [None, Some(OsStr::new(""))] {
            assert_eq!(scoped(repo.path(), removed, &scope), Ok(db.clone()));
        }
    }

    #[test]
    fn an_override_naming_the_protected_shared_database_is_refused() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".aethyme")).unwrap();
        let scope = encoded(repo.path(), &repo.path().join("gate.db"));
        let canonical = repo.path().canonicalize().unwrap();
        for live in [
            canonical.join(crate::BROKER_DB_RELPATH),
            // Spelled differently from the scope's canonical root.
            repo.path().join(".aethyme/../.aethyme/broker.db"),
        ] {
            let refused = scoped(repo.path(), Some(live.as_os_str()), &scope).unwrap_err();
            assert!(refused.contains("shared broker database"), "{refused}");
        }
    }

    #[test]
    fn an_unreadable_scope_is_refused_rather_than_ignored() {
        let repo = tempfile::tempdir().unwrap();
        let db = repo.path().join("gate.db");
        for invalid in [
            "",
            "{}",
            "not json",
            r#"{"fallback":"relative.db","repositories":{}}"#,
        ] {
            for pinned in [None, Some(db.as_os_str())] {
                let refused = scoped(repo.path(), pinned, invalid).unwrap_err();
                assert!(refused.contains(SCOPE_ENV), "{refused}");
            }
        }
    }

    #[test]
    fn unknown_repository_identity_resolves_to_the_disposable_database() {
        let repo = tempfile::tempdir().unwrap();
        let db = repo.path().join("gate.db");
        let scope = encoded(repo.path(), &db);
        for pinned in [None, Some(db.as_os_str())] {
            assert_eq!(
                scoped(&repo.path().join("missing"), pinned, &scope),
                Ok(db.clone())
            );
        }
    }

    #[test]
    fn the_reported_target_names_the_disposable_database_and_what_it_protects() {
        let repo = tempfile::tempdir().unwrap();
        let db = repo.path().join("gate.db");
        let target = GateBrokerDatabase::from_scope(&encoded(repo.path(), &db)).unwrap();
        assert_eq!(target.path, db);
        assert_eq!(target.kind, "disposable");
        assert_eq!(
            target.protected_repositories,
            vec![repo.path().canonicalize().unwrap()]
        );
        assert!(GateBrokerDatabase::from_scope("not json").is_err());
    }
}
