//! Candidate-built contract checker for gate use. The checker itself is shared
//! with the broker so the lightweight gate and the public CLI cannot drift.
use std::path::{Path, PathBuf};

mod git {
    pub(crate) fn git_command() -> std::process::Command {
        std::process::Command::new("git")
    }
}

struct GitRepo {
    root: PathBuf,
}
impl GitRepo {
    fn discover(path: &Path) -> Result<Self, String> {
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
