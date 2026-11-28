//! Candidate-built contract checker for gate use. The checker itself is shared
//! with the broker so the lightweight gate and the public CLI cannot drift.
use std::path::{Path, PathBuf};

#[path = "../../aethyme-broker/src/git_porcelain_probe.rs"]
mod git_porcelain_probe;

mod git_probe_cache {
    pub(crate) const PROBE_DIRECTORY_PREFIX: &str = "aethyme-git-probe-";
}

mod git {
    use crate::git_porcelain_probe::{PorcelainProbe, porcelain_probe};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::OnceLock;

    pub(crate) fn remove_git_environment(command: &mut Command) {
        for (key, _) in std::env::vars_os() {
            if key
                .to_string_lossy()
                .to_ascii_uppercase()
                .starts_with("GIT_")
            {
                command.env_remove(key);
            }
        }
    }

    fn select(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
        candidates.into_iter().find(|path| {
            path.is_file() && matches!(porcelain_probe(path), PorcelainProbe::Undecorated)
        })
    }

    #[cfg(all(test, unix))]
    mod tests {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        #[test]
        fn bypasses_a_decorating_wrapper_and_refuses_when_none_is_trusted() {
            let tmp = tempfile::tempdir().unwrap();
            let wrapper = tmp.path().join("git");
            std::fs::write(&wrapper, "#!/bin/sh\nprintf 'decorated\\n'\n").unwrap();
            std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(select([wrapper.clone()]), None);
            let path = std::env::var_os("PATH").unwrap();
            let cwd = std::env::current_dir().unwrap();
            let real = select(std::env::split_paths(&path).map(|dir| {
                let absolute = if dir.is_absolute() {
                    dir
                } else {
                    cwd.join(dir)
                };
                absolute.join("git")
            }))
            .expect("a trusted git is needed for this test");
            assert_eq!(select([wrapper, real.clone()]), Some(real));
        }
    }

    pub(crate) fn trusted_program() -> Option<&'static Path> {
        static PROGRAM: OnceLock<Option<PathBuf>> = OnceLock::new();
        let program = PROGRAM.get_or_init(|| {
            let cwd = std::env::current_dir().ok()?;
            let path = std::env::var_os("PATH")?;
            select(std::env::split_paths(&path).map(|dir| {
                let absolute = if dir.is_absolute() {
                    dir
                } else {
                    cwd.join(dir)
                };
                absolute.join("git")
            }))
        });
        program.as_deref()
    }

    pub(crate) fn git_command() -> Command {
        Command::new(trusted_program().unwrap_or_else(|| Path::new("__aethyme_no_trusted_git__")))
    }
}

struct GitRepo {
    root: PathBuf,
}
impl GitRepo {
    fn discover(path: &Path) -> Result<Self, String> {
        git::trusted_program().ok_or("no trustworthy git executable found on PATH")?;
        let output = git::git_command()
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(path)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
        }
        Ok(Self {
            root: PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()),
        })
    }
    fn root(&self) -> &Path {
        &self.root
    }
}

#[allow(dead_code)]
#[path = "../../aethyme-broker/src/contract_check.rs"]
mod contract_check;

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    std::process::exit(i32::from(contract_check::run(&args)));
}
