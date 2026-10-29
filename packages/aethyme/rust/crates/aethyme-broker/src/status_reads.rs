//! Invocation-local setup observations for routine status only.
//!
//! Never used to authorize a mutation. Each outer invocation starts empty;
//! nested router/broker dispatch shares it and panic unwinding drops it.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Default)]
struct Context {
    identities: BTreeMap<(PathBuf, Vec<String>), String>,
    configurations: BTreeMap<PathBuf, ConfigurationRead>,
    committed_configurations: BTreeMap<PathBuf, CommittedConfigurationRead>,
}
thread_local! {
    static CONTEXT: RefCell<Option<Context>> = const { RefCell::new(None) };
}
struct Scope(bool);
impl Drop for Scope {
    fn drop(&mut self) {
        if self.0 {
            CONTEXT.with(|context| *context.borrow_mut() = None);
        }
    }
}

pub(crate) fn with_context<T>(operation: impl FnOnce() -> T) -> T {
    let owner = CONTEXT.with(|context| {
        let mut context = context.borrow_mut();
        if context.is_some() {
            false
        } else {
            *context = Some(Context::default());
            true
        }
    });
    let _scope = Scope(owner);
    operation()
}

pub(crate) fn git_identity(
    cwd: &Path,
    args: &[&str],
    read: impl FnOnce() -> Result<String, crate::GitError>,
) -> Result<String, crate::GitError> {
    // Only checkout identity is shared. Refs, PIDs, locks, worktree activity,
    // remote targets and arbitrary Git commands are always read normally.
    if !matches!(args, ["rev-parse", "--show-toplevel" | "--git-common-dir"])
        || !CONTEXT.with(|context| context.borrow().is_some())
    {
        return read();
    }
    let key = (
        cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
        args.iter().map(|arg| (*arg).to_string()).collect(),
    );
    if let Some(value) = CONTEXT.with(|context| {
        context
            .borrow()
            .as_ref()
            .and_then(|context| context.identities.get(&key).cloned())
    }) {
        return Ok(value);
    }
    let value = read()?;
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow_mut().as_mut()
            && context.identities.len() < 64
        {
            context.identities.insert(key, value.clone());
        }
    });
    Ok(value)
}

/// The repository configuration as TOML text with its source, or why it could
/// not be resolved (for example both `config.toml` and `config.yaml`).
pub(crate) type ConfigurationRead =
    Result<Option<(String, &'static str)>, aethyme_config::ConfigSourceError>;

pub(crate) type CommittedConfigurationRead =
    Result<Option<String>, aethyme_config::ConfigSourceError>;

/// The committed default-branch config observed during this invocation.
///
/// Absence and failures are cached too: every consumer in one command uses
/// the same observation, while the next invocation resolves the ref again.
pub(crate) fn committed_configuration(
    root: &Path,
    read: impl FnOnce() -> CommittedConfigurationRead,
) -> CommittedConfigurationRead {
    if let Some(value) = CONTEXT.with(|context| {
        context
            .borrow()
            .as_ref()
            .and_then(|context| context.committed_configurations.get(root).cloned())
    }) {
        return value;
    }
    let value = read();
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow_mut().as_mut()
            && context.committed_configurations.len() < 64
        {
            context
                .committed_configurations
                .insert(root.to_path_buf(), value.clone());
        }
    });
    value
}

pub(crate) fn configuration(
    root: &Path,
    read: impl FnOnce() -> ConfigurationRead,
) -> ConfigurationRead {
    if let Some(value) = CONTEXT.with(|context| {
        context
            .borrow()
            .as_ref()
            .and_then(|context| context.configurations.get(root).cloned())
    }) {
        return value;
    }
    let value = read();
    if matches!(value, Ok(None)) {
        return value;
    }
    CONTEXT.with(|context| {
        if let Some(context) = context.borrow_mut().as_mut()
            && context.configurations.len() < 64
        {
            context
                .configurations
                .insert(root.to_path_buf(), value.clone());
        }
    });
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn setup_reads_are_shared_only_inside_one_invocation() {
        let calls = Cell::new(0);
        let read = || {
            calls.set(calls.get() + 1);
            Ok(calls.get().to_string())
        };
        with_context(|| {
            assert_eq!(
                git_identity(Path::new("."), &["rev-parse", "--git-common-dir"], read).unwrap(),
                "1"
            );
            with_context(|| {
                assert_eq!(
                    git_identity(Path::new("."), &["rev-parse", "--git-common-dir"], read).unwrap(),
                    "1"
                );
            });
            assert_eq!(calls.get(), 1);
        });
        with_context(|| {
            assert_eq!(
                git_identity(Path::new("."), &["rev-parse", "--git-common-dir"], read).unwrap(),
                "2"
            );
        });
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn arbitrary_reads_and_errors_are_never_shared() {
        let calls = Cell::new(0);
        with_context(|| {
            for _ in 0..2 {
                git_identity(Path::new("."), &["rev-parse", "HEAD"], || {
                    calls.set(calls.get() + 1);
                    Ok("head".into())
                })
                .unwrap();
                assert!(
                    git_identity(Path::new("."), &["rev-parse", "--git-common-dir"], || {
                        calls.set(calls.get() + 1);
                        Err(crate::GitError::NotARepository {
                            path: PathBuf::from("."),
                        })
                    })
                    .is_err()
                );
            }
        });
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn configuration_is_fresh_after_scope_exit_including_unwind() {
        let root = Path::new("repo");
        let _ = std::panic::catch_unwind(|| {
            with_context(|| {
                assert_eq!(
                    configuration(root, || Ok(Some(("first".into(), "committed"))))
                        .unwrap()
                        .unwrap()
                        .0,
                    "first"
                );
                assert_eq!(
                    configuration(root, || panic!("duplicate config read"))
                        .unwrap()
                        .unwrap()
                        .0,
                    "first"
                );
                panic!("exercise scope cleanup");
            })
        });
        with_context(|| {
            assert_eq!(
                configuration(root, || Ok(Some(("second".into(), "committed"))))
                    .unwrap()
                    .unwrap()
                    .0,
                "second"
            )
        });
    }
}
