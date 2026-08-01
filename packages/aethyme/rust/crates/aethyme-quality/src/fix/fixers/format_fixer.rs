//! Port of `src/autofixers/fixers/format_fixer.py`.
//!
//! This fixer's own logic is tool discovery plus subprocess
//! orchestration — it shells out to black/autopep8/prettier/gofmt/
//! rustfmt and returns whatever they print. The plan's risk item calls
//! out that formatter versions differ per machine, so the committed
//! tests assert on the ORCHESTRATION (which tool, which argv, which
//! stdin, which timeout, and the fallback order) through an injectable
//! runner, rather than on formatted bytes. Byte comparison of the
//! produced patches still happens in the parity corpus, where both
//! implementations shell out to the same installed binaries and so must
//! agree exactly.
//!
//! Note: the Python `__init__` also accepts and stores a `formatter`
//! argument that nothing ever reads. It is not carried over — it has no
//! observable behavior — and the CLI never passed it.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::fix::fixers::base::Fixer;
use crate::walk;

/// Per-language tool preference order. `yapf` is listed but has NO
/// dispatch branch in `run_formatter`, so when it is the only Python
/// formatter present `can_fix` says yes and `fix` always returns
/// nothing. Preserved.
const FORMATTERS: [(&str, &[&str]); 5] = [
    ("python", &["black", "autopep8", "yapf"]),
    ("javascript", &["prettier"]),
    ("typescript", &["prettier"]),
    ("go", &["gofmt"]),
    ("rust", &["rustfmt"]),
];

const VERSION_TIMEOUT: Duration = Duration::from_secs(5);
const FORMAT_TIMEOUT: Duration = Duration::from_secs(30);

fn language_for(file_path: &Path) -> Option<&'static str> {
    match walk::py_suffix(file_path).to_lowercase().as_str() {
        ".py" => Some("python"),
        ".js" | ".jsx" => Some("javascript"),
        ".ts" | ".tsx" => Some("typescript"),
        ".go" => Some("go"),
        ".rs" => Some("rust"),
        _ => None,
    }
}

/// Outcome of one subprocess invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Completed { code: i32, stdout: Vec<u8> },
    /// Spawn failure (`FileNotFoundError`) or timeout
    /// (`TimeoutExpired`) — both are caught in the Python.
    Failed,
}

/// Injection seam for the subprocess layer.
pub trait CommandRunner {
    fn run(&self, argv: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> RunOutcome;
}

/// The real runner.
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, argv: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> RunOutcome {
        let Some((program, args)) = argv.split_first() else {
            return RunOutcome::Failed;
        };
        let Ok(mut child) = Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        else {
            return RunOutcome::Failed;
        };

        // stdin/stdout are pumped on their own threads so a child that
        // fills its stdout pipe while we are still writing cannot
        // deadlock us — the same guarantee `subprocess.run(input=...)`
        // gives via communicate().
        let input = stdin.map(|bytes| bytes.to_vec()).unwrap_or_default();
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
                    if started.elapsed() >= timeout {
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

pub struct FormatFixer {
    runner: Box<dyn CommandRunner>,
    /// `(language, tools)` in `FORMATTERS` order; every language key is
    /// present even when its tool list is empty, matching the Python
    /// dict.
    available_tools: Vec<(&'static str, Vec<&'static str>)>,
}

impl FormatFixer {
    pub fn new(_repo_path: &Path) -> Self {
        Self::with_runner(Box::new(SystemCommandRunner))
    }

    /// Port of `__init__`: discovery runs eagerly, one `--version`
    /// probe per (language, tool) pair — so `prettier` is probed twice,
    /// once for javascript and once for typescript.
    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        let mut available_tools: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
        for (language, tools) in FORMATTERS {
            let mut found: Vec<&'static str> = Vec::new();
            for tool in tools {
                if is_tool_available(runner.as_ref(), tool) {
                    found.push(tool);
                }
            }
            available_tools.push((language, found));
        }
        FormatFixer {
            runner,
            available_tools,
        }
    }

    fn tools_for(&self, language: &str) -> Option<&Vec<&'static str>> {
        self.available_tools
            .iter()
            .find(|(name, _)| *name == language)
            .map(|(_, tools)| tools)
    }

    /// Port of `get_formatter_info`.
    pub fn get_formatter_info(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        self.available_tools.clone()
    }

    /// Port of `_run_formatter`'s dispatch table. `yapf`, and any tool
    /// not listed, returns nothing.
    fn run_formatter(&self, tool: &str, file_path: &Path, content: &str) -> Option<String> {
        let input = content.as_bytes();
        let outcome = match tool {
            "black" => self.runner.run(&["black", "--quiet", "-"], Some(input), FORMAT_TIMEOUT),
            "prettier" => {
                let parser = prettier_parser(file_path);
                self.runner
                    .run(&["prettier", "--parser", parser], Some(input), FORMAT_TIMEOUT)
            }
            "gofmt" => self.runner.run(&["gofmt"], Some(input), FORMAT_TIMEOUT),
            "rustfmt" => {
                self.runner
                    .run(&["rustfmt", "--emit", "stdout"], Some(input), FORMAT_TIMEOUT)
            }
            "autopep8" => self.runner.run(&["autopep8", "-"], Some(input), FORMAT_TIMEOUT),
            _ => return None,
        };
        match outcome {
            // Only a zero exit yields output; the decode is strict, so
            // non-UTF-8 formatter output is discarded.
            RunOutcome::Completed { code: 0, stdout } => String::from_utf8(stdout).ok(),
            _ => None,
        }
    }
}

/// Port of `_is_tool_available`.
fn is_tool_available(runner: &dyn CommandRunner, tool: &str) -> bool {
    matches!(
        runner.run(&[tool, "--version"], None, VERSION_TIMEOUT),
        RunOutcome::Completed { code: 0, .. }
    )
}

/// Port of the prettier parser map. Keyed on the VERBATIM suffix (not
/// lowercased), defaulting to `babel`.
fn prettier_parser(file_path: &Path) -> &'static str {
    match walk::py_suffix(file_path).as_str() {
        ".ts" | ".tsx" => "typescript",
        ".json" => "json",
        ".css" => "css",
        ".md" => "markdown",
        _ => "babel",
    }
}

impl Fixer for FormatFixer {
    fn fix_type(&self) -> &'static str {
        "format_fix"
    }

    fn can_fix(&self, file_path: &Path) -> bool {
        let Some(language) = language_for(file_path) else {
            return false;
        };
        self.tools_for(language).is_some_and(|tools| !tools.is_empty())
    }

    /// Port of `fix`: try each available tool in preference order and
    /// take the first result that is non-empty AND different from the
    /// input. An empty formatter output is treated as "no fix" because
    /// the Python truthiness test rejects the empty string.
    fn fix(&self, file_path: &Path, content: &str) -> Option<String> {
        let language = language_for(file_path)?;
        let tools = self.tools_for(language)?.clone();
        for tool in tools {
            if let Some(formatted) = self.run_formatter(tool, file_path, content)
                && !formatted.is_empty()
                && formatted != content
            {
                return Some(formatted);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Records every invocation and replies from a scripted table, so
    /// the assertions are about orchestration rather than formatter
    /// output.
    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<(Vec<String>, Option<String>, Duration)>>,
        available: Vec<&'static str>,
        replies: Mutex<Vec<RunOutcome>>,
        default_reply: Mutex<Option<RunOutcome>>,
    }

    impl FakeRunner {
        fn new(available: Vec<&'static str>) -> Arc<Self> {
            Arc::new(FakeRunner {
                available,
                ..Default::default()
            })
        }

        fn formatting(self: &Arc<Self>, stdout: &str) -> Arc<Self> {
            *self.default_reply.lock().unwrap() = Some(RunOutcome::Completed {
                code: 0,
                stdout: stdout.as_bytes().to_vec(),
            });
            self.clone()
        }

        fn scripted(self: &Arc<Self>, replies: Vec<RunOutcome>) -> Arc<Self> {
            *self.replies.lock().unwrap() = replies;
            self.clone()
        }

        /// Every non-`--version` invocation, argv only.
        fn format_calls(&self) -> Vec<Vec<String>> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(argv, _, _)| argv.get(1).map(String::as_str) != Some("--version"))
                .map(|(argv, _, _)| argv.clone())
                .collect()
        }

        fn version_calls(&self) -> Vec<(Vec<String>, Option<String>, Duration)> {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|(argv, _, _)| argv.get(1).map(String::as_str) == Some("--version"))
                .cloned()
                .collect()
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, argv: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> RunOutcome {
            self.calls.lock().unwrap().push((
                argv.iter().map(|s| s.to_string()).collect(),
                stdin.map(|b| String::from_utf8_lossy(b).to_string()),
                timeout,
            ));
            if argv.get(1) == Some(&"--version") {
                return if self.available.contains(&argv[0]) {
                    RunOutcome::Completed {
                        code: 0,
                        stdout: b"1.0\n".to_vec(),
                    }
                } else {
                    RunOutcome::Failed
                };
            }
            let mut scripted = self.replies.lock().unwrap();
            if !scripted.is_empty() {
                return scripted.remove(0);
            }
            self.default_reply
                .lock()
                .unwrap()
                .clone()
                .unwrap_or(RunOutcome::Failed)
        }
    }

    /// Adapter so a shared fake can be both injected and inspected.
    struct SharedRunner(Arc<FakeRunner>);

    impl CommandRunner for SharedRunner {
        fn run(&self, argv: &[&str], stdin: Option<&[u8]>, timeout: Duration) -> RunOutcome {
            self.0.run(argv, stdin, timeout)
        }
    }

    fn discovered(fake: &Arc<FakeRunner>) -> FormatFixer {
        FormatFixer::with_runner(Box::new(SharedRunner(fake.clone())))
    }

    /// A fixer with a pre-set tool table, skipping discovery so the
    /// recorded calls are only the formatting ones.
    fn preloaded(
        tools: Vec<(&'static str, Vec<&'static str>)>,
        fake: &Arc<FakeRunner>,
    ) -> FormatFixer {
        FormatFixer {
            runner: Box::new(SharedRunner(fake.clone())),
            available_tools: tools,
        }
    }

    // ── Discovery ────────────────────────────────────────────────────

    #[test]
    fn discovery_probes_every_language_tool_pair() {
        let fake = FakeRunner::new(vec![]);
        let fixer = discovered(&fake);
        let probes: Vec<Vec<String>> = fake.version_calls().into_iter().map(|c| c.0).collect();
        assert_eq!(
            probes,
            vec![
                vec!["black", "--version"],
                vec!["autopep8", "--version"],
                vec!["yapf", "--version"],
                vec!["prettier", "--version"],
                vec!["prettier", "--version"],
                vec!["gofmt", "--version"],
                vec!["rustfmt", "--version"],
            ],
            "prettier is probed once per language, exactly as in Python"
        );
        // Every language key is present even with no tools found.
        assert_eq!(
            fixer
                .get_formatter_info()
                .iter()
                .map(|(l, _)| *l)
                .collect::<Vec<_>>(),
            vec!["python", "javascript", "typescript", "go", "rust"]
        );
        assert!(fixer.get_formatter_info().iter().all(|(_, t)| t.is_empty()));
    }

    #[test]
    fn version_probes_carry_no_stdin_and_a_five_second_timeout() {
        let fake = FakeRunner::new(vec!["black"]);
        discovered(&fake);
        for (argv, stdin, timeout) in fake.version_calls() {
            assert!(stdin.is_none(), "{argv:?} must not receive stdin");
            assert_eq!(timeout, Duration::from_secs(5), "{argv:?}");
        }
    }

    #[test]
    fn discovery_keeps_only_tools_that_exit_zero() {
        let fake = FakeRunner::new(vec!["black", "yapf", "rustfmt"]);
        let fixer = discovered(&fake);
        assert_eq!(
            fixer.get_formatter_info(),
            vec![
                ("python", vec!["black", "yapf"]),
                ("javascript", vec![]),
                ("typescript", vec![]),
                ("go", vec![]),
                ("rust", vec!["rustfmt"]),
            ]
        );
    }

    #[test]
    fn can_fix_requires_an_available_tool_for_the_language() {
        let fixer = discovered(&FakeRunner::new(vec!["black"]));
        assert!(fixer.can_fix(Path::new("a.py")));
        for name in ["a.js", "a.go", "a.md", "a"] {
            assert!(!fixer.can_fix(Path::new(name)), "{name}");
        }
        let fixer = discovered(&FakeRunner::new(vec!["prettier"]));
        for name in ["a.js", "a.jsx", "a.ts", "a.tsx"] {
            assert!(fixer.can_fix(Path::new(name)), "{name}");
        }
        assert!(!fixer.can_fix(Path::new("a.py")));
    }

    // ── Orchestration ────────────────────────────────────────────────

    #[test]
    fn each_tool_gets_its_python_argv_and_the_content_on_stdin() {
        for (language, tool, file, expected) in [
            ("python", "black", "a.py", vec!["black", "--quiet", "-"]),
            ("python", "autopep8", "a.py", vec!["autopep8", "-"]),
            ("javascript", "prettier", "a.js", vec!["prettier", "--parser", "babel"]),
            ("javascript", "prettier", "a.jsx", vec!["prettier", "--parser", "babel"]),
            ("typescript", "prettier", "a.ts", vec!["prettier", "--parser", "typescript"]),
            ("typescript", "prettier", "a.tsx", vec!["prettier", "--parser", "typescript"]),
            ("go", "gofmt", "a.go", vec!["gofmt"]),
            ("rust", "rustfmt", "a.rs", vec!["rustfmt", "--emit", "stdout"]),
        ] {
            let fake = FakeRunner::new(vec![]).formatting("out\n");
            let fixer = preloaded(vec![(language, vec![tool])], &fake);
            assert_eq!(
                fixer.fix(Path::new(file), "in\n"),
                Some("out\n".to_string()),
                "{tool} on {file}"
            );
            assert_eq!(fake.format_calls(), vec![expected], "{tool} on {file}");
            let recorded = fake.calls.lock().unwrap();
            assert_eq!(recorded[0].1.as_deref(), Some("in\n"), "{tool} stdin");
            assert_eq!(recorded[0].2, Duration::from_secs(30), "{tool} timeout");
        }
    }

    #[test]
    fn yapf_has_no_dispatch_branch() {
        let fake = FakeRunner::new(vec![]).formatting("out\n");
        let fixer = preloaded(vec![("python", vec!["yapf"])], &fake);
        // can_fix says yes (a tool is "available") but nothing runs.
        assert!(fixer.can_fix(Path::new("a.py")));
        assert_eq!(fixer.fix(Path::new("a.py"), "in\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn tools_are_tried_in_order_until_one_changes_the_content() {
        let fake = FakeRunner::new(vec![]).scripted(vec![
            RunOutcome::Failed, // black errors out
            RunOutcome::Completed {
                code: 0,
                stdout: b"fixed\n".to_vec(),
            },
        ]);
        let fixer = preloaded(vec![("python", vec!["black", "autopep8"])], &fake);
        assert_eq!(fixer.fix(Path::new("a.py"), "in\n"), Some("fixed\n".to_string()));
        assert_eq!(
            fake.format_calls(),
            vec![vec!["black", "--quiet", "-"], vec!["autopep8", "-"]]
        );
    }

    #[test]
    fn the_first_changing_tool_wins_and_later_tools_never_run() {
        let fake = FakeRunner::new(vec![]).formatting("fixed\n");
        let fixer = preloaded(vec![("python", vec!["black", "autopep8"])], &fake);
        assert_eq!(fixer.fix(Path::new("a.py"), "in\n"), Some("fixed\n".to_string()));
        assert_eq!(fake.format_calls(), vec![vec!["black", "--quiet", "-"]]);
    }

    #[test]
    fn nonzero_exit_identical_and_empty_output_all_count_as_no_fix() {
        let fake = FakeRunner::new(vec![]).scripted(vec![
            RunOutcome::Completed {
                code: 1,
                stdout: b"ignored\n".to_vec(),
            },
            RunOutcome::Completed {
                code: 0,
                stdout: b"in\n".to_vec(), // identical to the input
            },
        ]);
        let fixer = preloaded(vec![("python", vec!["black", "autopep8", "yapf"])], &fake);
        assert_eq!(fixer.fix(Path::new("a.py"), "in\n"), None);
        assert_eq!(fake.format_calls().len(), 2, "yapf has no branch to call");

        // Empty output is falsy in Python, so it is not a fix either.
        let fake = FakeRunner::new(vec![]).formatting("");
        let fixer = preloaded(vec![("python", vec!["black"])], &fake);
        assert_eq!(fixer.fix(Path::new("a.py"), "in\n"), None);
    }

    #[test]
    fn non_utf8_formatter_output_is_discarded() {
        let fake = FakeRunner::new(vec![]).scripted(vec![RunOutcome::Completed {
            code: 0,
            stdout: vec![0xff, 0xfe],
        }]);
        let fixer = preloaded(vec![("python", vec!["black"])], &fake);
        assert_eq!(fixer.fix(Path::new("a.py"), "in\n"), None);
    }

    #[test]
    fn unknown_language_never_runs_a_tool() {
        let fake = FakeRunner::new(vec![]).formatting("out\n");
        let fixer = preloaded(vec![("python", vec!["black"])], &fake);
        assert_eq!(fixer.fix(Path::new("a.md"), "in\n"), None);
        assert_eq!(fixer.fix(Path::new("a.go"), "in\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn prettier_parser_map_uses_the_verbatim_suffix() {
        assert_eq!(prettier_parser(Path::new("a.ts")), "typescript");
        assert_eq!(prettier_parser(Path::new("a.tsx")), "typescript");
        assert_eq!(prettier_parser(Path::new("a.js")), "babel");
        assert_eq!(prettier_parser(Path::new("a.json")), "json");
        assert_eq!(prettier_parser(Path::new("a.css")), "css");
        assert_eq!(prettier_parser(Path::new("a.md")), "markdown");
        // Uppercase is NOT mapped — the Python keys on `suffix` as-is.
        assert_eq!(prettier_parser(Path::new("a.TS")), "babel");
        assert_eq!(prettier_parser(Path::new("a.unknown")), "babel");
    }

    // ── The real runner ──────────────────────────────────────────────

    #[test]
    fn system_runner_reports_exit_codes_stdin_and_spawn_failures() {
        let runner = SystemCommandRunner;
        assert_eq!(
            runner.run(&["/bin/echo", "hi"], None, FORMAT_TIMEOUT),
            RunOutcome::Completed {
                code: 0,
                stdout: b"hi\n".to_vec()
            }
        );
        assert_eq!(
            runner.run(&["/bin/cat"], Some(b"piped"), FORMAT_TIMEOUT),
            RunOutcome::Completed {
                code: 0,
                stdout: b"piped".to_vec()
            }
        );
        assert!(matches!(
            runner.run(&["/bin/sh", "-c", "exit 3"], None, FORMAT_TIMEOUT),
            RunOutcome::Completed { code: 3, .. }
        ));
        // FileNotFoundError equivalent.
        assert_eq!(
            runner.run(&["definitely-not-a-real-tool-xyz", "--version"], None, VERSION_TIMEOUT),
            RunOutcome::Failed
        );
    }

    #[test]
    fn system_runner_kills_on_timeout() {
        let runner = SystemCommandRunner;
        let started = Instant::now();
        assert_eq!(
            runner.run(&["/bin/sleep", "30"], None, Duration::from_millis(150)),
            RunOutcome::Failed
        );
        assert!(started.elapsed() < Duration::from_secs(5), "did not kill promptly");
    }

    #[test]
    fn system_runner_survives_large_output_while_writing_stdin() {
        // Would deadlock without the stdin/stdout pump threads.
        let runner = SystemCommandRunner;
        let input = "x".repeat(300_000);
        match runner.run(&["/bin/cat"], Some(input.as_bytes()), FORMAT_TIMEOUT) {
            RunOutcome::Completed { code: 0, stdout } => assert_eq!(stdout.len(), 300_000),
            other => panic!("expected completion, got {other:?}"),
        }
    }
}
