//! Subprocess invocation of the `aethyme` router.
//!
//! Successor to `tests/support/cli_invoke.py`. The contract it froze is
//! preserved exactly: run the built router as a child process, merge
//! stdout and stderr into one `output` string (matching what the pytest
//! assertions searched), and expose the exit code.
//!
//! One line of the Python helper is deliberately NOT carried over: it
//! prepended the test interpreter's `bin` dir to `PATH` so the router's
//! `python3` fallback resolved to the venv running the suite. There is
//! no delegation path any more (python-retirement Phase 6 made every
//! command native), so that line would only have re-introduced an
//! interpreter into an environment that must not need one.
//!
//! `AETHYME_ROOT` is still pinned to this checkout. The router needs a
//! root for `{{AETHYME_ROOT}}` substitution in `enhance` and the two
//! skill-compiling `repo` subcommands, and pinning it keeps tests off
//! the pointer file and off upward walks from temp directories.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::bins::aethyme_bin;
use crate::paths::package_root;

/// Mirrors the fields the retired pytest assertions used.
#[derive(Debug, Clone)]
pub struct InvokeResult {
    pub exit_code: i32,
    /// stdout followed by stderr, matching the Python helper's merge.
    pub output: String,
}

impl InvokeResult {
    /// Assert success and return the merged output.
    #[track_caller]
    pub fn ok(&self) -> &str {
        assert_eq!(self.exit_code, 0, "expected exit 0, got:\n{}", self.output);
        &self.output
    }

    /// Assert an exact exit code and return the merged output.
    #[track_caller]
    pub fn expect_code(&self, code: i32) -> &str {
        assert_eq!(
            self.exit_code, code,
            "expected exit {code}, got {}:\n{}",
            self.exit_code, self.output
        );
        &self.output
    }

    /// Parse the output as JSON.
    #[track_caller]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.output)
            .unwrap_or_else(|error| panic!("output is not JSON ({error}):\n{}", self.output))
    }

    #[track_caller]
    pub fn assert_contains(&self, needle: &str) -> &Self {
        assert!(
            self.output.contains(needle),
            "expected output to contain {needle:?}:\n{}",
            self.output
        );
        self
    }

    #[track_caller]
    pub fn assert_lacks(&self, needle: &str) -> &Self {
        assert!(
            !self.output.contains(needle),
            "expected output NOT to contain {needle:?}:\n{}",
            self.output
        );
        self
    }
}

/// Builder for a router invocation with optional cwd and stdin.
pub struct Invoke {
    args: Vec<String>,
    cwd: Option<PathBuf>,
    stdin: Option<String>,
}

impl Invoke {
    pub fn new<I, S>(args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Self {
            args: args
                .into_iter()
                .map(|arg| arg.as_ref().to_string_lossy().into_owned())
                .collect(),
            cwd: None,
            stdin: None,
        }
    }

    pub fn cwd(mut self, dir: impl AsRef<Path>) -> Self {
        self.cwd = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Feed the child's stdin, for reader commands accepting `--from -`.
    pub fn stdin(mut self, text: impl Into<String>) -> Self {
        self.stdin = Some(text.into());
        self
    }

    pub fn run(self) -> InvokeResult {
        let mut command = Command::new(aethyme_bin());
        command
            .args(&self.args)
            .env("AETHYME_ROOT", package_root())
            .stdin(if self.stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = &self.cwd {
            command.current_dir(dir);
        }

        let mut child = command
            .spawn()
            .unwrap_or_else(|error| panic!("spawn aethyme {:?}: {error}", self.args));
        if let Some(text) = &self.stdin {
            child
                .stdin
                .as_mut()
                .expect("piped stdin")
                .write_all(text.as_bytes())
                .expect("write child stdin");
        }
        let output = child
            .wait_with_output()
            .unwrap_or_else(|error| panic!("wait for aethyme {:?}: {error}", self.args));

        InvokeResult {
            // 128 + signal, matching how a shell reports a signalled child.
            exit_code: output.status.code().unwrap_or(-1),
            output: format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            ),
        }
    }
}

/// Run `aethyme <args>` and return exit code + merged output.
pub fn invoke_aethyme<I, S>(args: I) -> InvokeResult
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Invoke::new(args).run()
}
