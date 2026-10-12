//! Explicit external HTML merge profile. Tool identity and observed fallback
//! travel with each recipe; a clean result grants no verification authority.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{ComposeError, io};
use crate::bounded_output::{LimitedOutput, output_limited};

pub const HTML_PROFILE: &str = "aethyme.compose.mergiraf-html/experimental-v1";
pub const MERGIRAF_VERSION: &str = "0.19.1";
const TOOL_LIMIT: usize = 32 * 1024 * 1024;
const INPUT_LIMIT: usize = 4 * 1024 * 1024;
const OUTPUT_LIMIT: usize = 8 * 1024 * 1024;

/// A privately copied, hash-checked external executable. No Git configuration,
/// attributes, refs or shared tool installation is modified by this profile.
pub struct MergirafEngine {
    root: tempfile::TempDir,
    executable: PathBuf,
    sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MergeMethod {
    LineBased,
    StructuredResolution,
    FullyStructured,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergeObservation {
    pub path: String,
    pub method: MergeMethod,
    pub timed_fallback: bool,
    pub exit_code: Option<i32>,
    pub base_sha256: String,
    pub current_sha256: String,
    pub result_sha256: String,
    pub output_sha256: String,
    pub diagnostic_sha256: String,
}

pub(crate) struct EngineMerge {
    pub bytes: Option<Vec<u8>>,
    pub observation: MergeObservation,
}

impl MergirafEngine {
    /// Verify the requested digest and version before admitting any inputs.
    /// Executing a private copy binds subsequent calls to the acquired bytes.
    pub fn pin(executable: &Path, expected_sha256: &str) -> Result<Self, ComposeError> {
        if !executable.is_absolute()
            || expected_sha256.len() != 64
            || !expected_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ComposeError::InvalidRequest(
                "absolute executable and lowercase SHA-256 required".into(),
            ));
        }
        let mut bytes = Vec::new();
        std::fs::File::open(executable)
            .map_err(|source| io(executable, source))?
            .take((TOOL_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| io(executable, source))?;
        if bytes.len() > TOOL_LIMIT || digest(&bytes) != expected_sha256 {
            return Err(ComposeError::InvalidRequest(
                "structural executable size or digest mismatch".into(),
            ));
        }
        let root = tempfile::tempdir().map_err(|source| io(Path::new("<engine>"), source))?;
        let copied = root.path().join(if cfg!(windows) {
            "mergiraf.exe"
        } else {
            "mergiraf"
        });
        std::fs::write(&copied, &bytes).map_err(|source| io(&copied, source))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&copied, std::fs::Permissions::from_mode(0o500))
                .map_err(|source| io(&copied, source))?;
        }
        let engine = Self {
            root,
            executable: copied,
            sha256: expected_sha256.to_string(),
        };
        let observed = output_limited(
            engine.command().arg("--version"),
            Duration::from_secs(3),
            4096,
        )
        .map_err(|source| io(&engine.executable, source))?;
        match observed {
            LimitedOutput::Completed(output)
                if output.status.success()
                    && output.stdout == format!("mergiraf {MERGIRAF_VERSION}\n").as_bytes() =>
            {
                Ok(engine)
            }
            _ => Err(ComposeError::InvalidRequest(
                "structural executable version unavailable or mismatched".into(),
            )),
        }
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.root.path())
            .env("XDG_CACHE_HOME", self.root.path())
            .env("NO_COLOR", "1")
            .current_dir(self.root.path());
        command
    }

    pub(crate) fn merge(
        &self,
        path: &[u8],
        base: &[u8],
        current: &[u8],
        result: &[u8],
    ) -> Result<EngineMerge, ComposeError> {
        if [base, current, result]
            .iter()
            .any(|bytes| bytes.len() > INPUT_LIMIT)
        {
            return Err(ComposeError::InvalidRequest(
                "structural input exceeds 4 MiB".into(),
            ));
        }
        let scratch = tempfile::tempdir_in(self.root.path())
            .map_err(|source| io(self.root.path(), source))?;
        let base_path = scratch.path().join("base.html");
        let current_path = scratch.path().join("current.html");
        let result_path = scratch.path().join("result.html");
        for (file, bytes) in [
            (&base_path, base),
            (&current_path, current),
            (&result_path, result),
        ] {
            std::fs::write(file, bytes).map_err(|source| io(file, source))?;
        }
        let observed = output_limited(
            self.command()
                .args([
                    "merge",
                    "--verbose",
                    "--language",
                    "html",
                    "--allow-parse-errors=false",
                    "--compact=false",
                    "--timeout",
                    "2000",
                    "--base-name",
                    "base",
                    "--left-name",
                    "current",
                    "--right-name",
                    "result",
                ])
                .arg(&base_path)
                .arg(&current_path)
                .arg(&result_path),
            Duration::from_secs(5),
            OUTPUT_LIMIT,
        )
        .map_err(|source| io(&self.executable, source))?;
        let output = match observed {
            LimitedOutput::Completed(output) => output,
            LimitedOutput::Deadline => {
                return Err(ComposeError::InvalidRequest(
                    "structural process deadline exhausted".into(),
                ));
            }
            LimitedOutput::OutputLimit => {
                return Err(ComposeError::InvalidRequest(
                    "structural process output limit exhausted".into(),
                ));
            }
        };
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        let observation = MergeObservation {
            path: String::from_utf8_lossy(path).into_owned(),
            method: selected_method(&diagnostic),
            timed_fallback: diagnostic.contains("falling back to Git"),
            exit_code: output.status.code(),
            base_sha256: digest(base),
            current_sha256: digest(current),
            result_sha256: digest(result),
            output_sha256: digest(&output.stdout),
            diagnostic_sha256: digest(&output.stderr),
        };
        let bytes = match output.status.code() {
            Some(0) if std::str::from_utf8(&output.stdout).is_ok() => Some(output.stdout),
            Some(1) => None,
            _ => {
                return Err(ComposeError::InvalidRequest(
                    "structural process failed or returned non-text output".into(),
                ));
            }
        };
        Ok(EngineMerge { bytes, observation })
    }
}

pub(crate) fn supports(path: &[u8]) -> bool {
    path.rsplit(|byte| *byte == b'.')
        .next()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(b"html") || ext.eq_ignore_ascii_case(b"htm"))
}

fn selected_method(diagnostic: &str) -> MergeMethod {
    // Only the tool's selected result is evidence; attempted algorithms do
    // not qualify the returned result. Multiple or unknown selections refuse
    // any structural claim.
    let mut methods = diagnostic.lines().filter_map(|line| {
        line.split_once("Selected best merge: ")
            .map(|(_, method)| method.trim())
    });
    let first = methods.next();
    if methods.next().is_some() {
        return MergeMethod::Unknown;
    }
    match first {
        Some("line_based") => MergeMethod::LineBased,
        Some("structured_resolution") => MergeMethod::StructuredResolution,
        Some("fully_structured") => MergeMethod::FullyStructured,
        _ => MergeMethod::Unknown,
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_method_requires_one_exact_selected_result() {
        assert_eq!(
            selected_method("DEBUG Selected best merge: fully_structured"),
            MergeMethod::FullyStructured
        );
        assert_eq!(
            selected_method("attempted fully_structured"),
            MergeMethod::Unknown
        );
        assert_eq!(
            selected_method(
                "Selected best merge: line_based\nSelected best merge: fully_structured"
            ),
            MergeMethod::Unknown
        );
        assert_eq!(
            selected_method("Selected best merge: future_mode"),
            MergeMethod::Unknown
        );
    }

    #[test]
    fn grammar_boundary_is_only_html_extensions() {
        assert!(supports(b"src/component.HTML"));
        assert!(!supports(b"src/component.ts"));
        assert!(!supports(b"src/html"));
    }

    #[test]
    fn wrong_identity_cannot_launch_an_engine() {
        let executable = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(executable.path(), b"not an engine").unwrap();
        assert!(MergirafEngine::pin(executable.path(), &"0".repeat(64)).is_err());
        assert!(MergirafEngine::pin(Path::new("mergiraf"), &"0".repeat(64)).is_err());
    }
}
