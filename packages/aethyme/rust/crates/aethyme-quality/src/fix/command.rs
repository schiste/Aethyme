//! Subprocess seam shared by the fix side.
//!
//! Both `format_fixer` (external formatters) and `github` (git and the
//! `gh` CLI) shell out. Routing every invocation through one injectable
//! trait keeps the committed tests free of any dependency on which
//! tools happen to be installed — and, for the PR helper, guarantees
//! the tests can never reach a real remote.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Outcome of one subprocess invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Completed { code: i32, stdout: Vec<u8> },
    /// Spawn failure (`FileNotFoundError`) or timeout
    /// (`TimeoutExpired`) — both are caught in the Python.
    Failed,
}

impl RunOutcome {
    pub fn succeeded(&self) -> bool {
        matches!(self, RunOutcome::Completed { code: 0, .. })
    }

    /// Stdout decoded as UTF-8, or empty for a failed run. Matches
    /// `subprocess.run(..., text=True).stdout` closely enough for the
    /// call sites, which only read git SHAs and PR URLs.
    pub fn stdout_text(&self) -> String {
        match self {
            RunOutcome::Completed { stdout, .. } => {
                String::from_utf8_lossy(stdout).to_string()
            }
            RunOutcome::Failed => String::new(),
        }
    }
}

/// Injection seam for the subprocess layer. `timeout: None` waits
/// indefinitely — the git calls in the Python pass no timeout at all.
pub trait CommandRunner {
    fn run(
        &self,
        argv: &[&str],
        stdin: Option<&[u8]>,
        timeout: Option<Duration>,
        cwd: Option<&Path>,
    ) -> RunOutcome;
}

/// The real runner.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(
        &self,
        argv: &[&str],
        stdin: Option<&[u8]>,
        timeout: Option<Duration>,
        cwd: Option<&Path>,
    ) -> RunOutcome {
        let Some((program, args)) = argv.split_first() else {
            return RunOutcome::Failed;
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(dir) = cwd {
            command.current_dir(dir);
        }
        let Ok(mut child) = command.spawn() else {
            return RunOutcome::Failed;
        };

        // stdin/stdout are pumped on their own threads so a child that
        // fills its stdout pipe while we are still writing cannot
        // deadlock us — the guarantee `subprocess.run(input=...)` gets
        // from communicate().
        let input = stdin.map(<[u8]>::to_vec).unwrap_or_default();
        let mut child_stdin = child.stdin.take();
        let writer = std::thread::spawn(move || {
            if let Some(handle) = child_stdin.as_mut() {
                let _ = handle.write_all(&input);
            }
            drop(child_stdin);
        });
        let mut child_stdout = child.stdout.take();
        let reader = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Some(handle) = child_stdout.as_mut() {
                let _ = handle.read_to_end(&mut buffer);
            }
            buffer
        });
        let mut child_stderr = child.stderr.take();
        let draining = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            if let Some(handle) = child_stderr.as_mut() {
                let _ = handle.read_to_end(&mut buffer);
            }
        });

        let started = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if let Some(limit) = timeout
                        && started.elapsed() >= limit
                    {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(_) => break None,
            }
        };

        let _ = writer.join();
        let stdout = reader.join().unwrap_or_default();
        let _ = draining.join();

        match status {
            Some(status) => RunOutcome::Completed {
                code: status.code().unwrap_or(-1),
                stdout,
            },
            None => RunOutcome::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_exit_codes_stdin_and_spawn_failures() {
        let runner = SystemCommandRunner;
        assert_eq!(
            runner.run(&["/bin/echo", "hi"], None, None, None),
            RunOutcome::Completed {
                code: 0,
                stdout: b"hi\n".to_vec()
            }
        );
        assert_eq!(
            runner.run(&["/bin/cat"], Some(b"piped"), None, None),
            RunOutcome::Completed {
                code: 0,
                stdout: b"piped".to_vec()
            }
        );
        assert!(matches!(
            runner.run(&["/bin/sh", "-c", "exit 3"], None, None, None),
            RunOutcome::Completed { code: 3, .. }
        ));
        // FileNotFoundError equivalent.
        assert_eq!(
            runner.run(&["definitely-not-a-real-tool-xyz"], None, None, None),
            RunOutcome::Failed
        );
    }

    #[test]
    fn honours_the_working_directory() {
        let runner = SystemCommandRunner;
        let outcome = runner.run(&["/bin/pwd"], None, None, Some(Path::new("/")));
        assert_eq!(outcome.stdout_text().trim_end(), "/");
    }

    #[test]
    fn kills_on_timeout_and_waits_forever_without_one() {
        let runner = SystemCommandRunner;
        let started = Instant::now();
        assert_eq!(
            runner.run(
                &["/bin/sleep", "30"],
                None,
                Some(Duration::from_millis(150)),
                None
            ),
            RunOutcome::Failed
        );
        assert!(started.elapsed() < Duration::from_secs(5), "did not kill promptly");
        // No timeout: a short-lived child still completes normally.
        assert!(
            runner
                .run(&["/bin/sleep", "0"], None, None, None)
                .succeeded()
        );
    }

    #[test]
    fn survives_large_output_while_writing_stdin() {
        // Would deadlock without the pump threads.
        let runner = SystemCommandRunner;
        let input = "x".repeat(300_000);
        match runner.run(&["/bin/cat"], Some(input.as_bytes()), None, None) {
            RunOutcome::Completed { code: 0, stdout } => assert_eq!(stdout.len(), 300_000),
            other => panic!("expected completion, got {other:?}"),
        }
    }
}
