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

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use toml::Value;

use crate::fix::command::{CommandRunner, RunOutcome, SystemCommandRunner};
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

pub struct FormatFixer {
    repo_path: PathBuf,
    repo_path_prefix: PathBuf,
    runner: Box<dyn CommandRunner>,
    /// `(language, tools)` in `FORMATTERS` order; every language key is
    /// present even when its tool list is empty, matching the Python
    /// dict.
    available_tools: Vec<(&'static str, Vec<&'static str>)>,
}

impl FormatFixer {
    pub fn new(repo_path: &Path) -> Self {
        Self::with_repo_path_and_runner(repo_path, Box::new(SystemCommandRunner))
    }

    /// Preserve the injectable constructor for callers that do not need to
    /// supply a repository root. Such callers resolve relative paths from the
    /// current directory captured at construction time.
    pub fn with_runner(runner: Box<dyn CommandRunner>) -> Self {
        Self::with_repo_path_and_runner(Path::new("."), runner)
    }

    /// Port of `__init__`: discovery runs eagerly, one `--version`
    /// probe per (language, tool) pair — so `prettier` is probed twice,
    /// once for javascript and once for typescript.
    fn with_repo_path_and_runner(repo_path: &Path, runner: Box<dyn CommandRunner>) -> Self {
        let repo_path_prefix = repo_path.to_path_buf();
        let repo_path = freeze_repo_path(repo_path);
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
            repo_path,
            repo_path_prefix,
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
            "black" => self.runner.run(
                &["black", "--quiet", "-"],
                Some(input),
                Some(FORMAT_TIMEOUT),
                None,
            ),
            "prettier" => {
                let parser = prettier_parser(file_path);
                self.runner.run(
                    &["prettier", "--parser", parser],
                    Some(input),
                    Some(FORMAT_TIMEOUT),
                    None,
                )
            }
            "gofmt" => self
                .runner
                .run(&["gofmt"], Some(input), Some(FORMAT_TIMEOUT), None),
            "rustfmt" => {
                let edition = rust_edition(&self.repo_path, &self.repo_path_prefix, file_path)?;
                self.runner.run(
                    &["rustfmt", "--edition", edition.as_str(), "--emit", "stdout"],
                    Some(input),
                    Some(FORMAT_TIMEOUT),
                    None,
                )
            }
            "autopep8" => {
                self.runner
                    .run(&["autopep8", "-"], Some(input), Some(FORMAT_TIMEOUT), None)
            }
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
        runner.run(&[tool, "--version"], None, Some(VERSION_TIMEOUT), None),
        RunOutcome::Completed { code: 0, .. }
    )
}

/// Editions Cargo currently accepts. Refuse unknown values so rustfmt
/// cannot silently use its default edition when the manifest is malformed
/// or ahead of the installed toolchain.
fn supported_rust_edition(edition: &str) -> bool {
    matches!(edition, "2015" | "2018" | "2021" | "2024")
}

/// Capture the repository root at construction time so a later current
/// directory change cannot retarget edition lookup.
fn freeze_repo_path(repo_path: &Path) -> PathBuf {
    let absolute = if repo_path.is_absolute() {
        repo_path.to_path_buf()
    } else if let Ok(current_dir) = std::env::current_dir() {
        current_dir.join(repo_path)
    } else {
        return repo_path.to_path_buf();
    };
    fs::canonicalize(&absolute).unwrap_or(absolute)
}

/// Return `None` for manifest access or parse errors, `Some(None)` when the
/// manifest does not exist, and `Some(Some(_))` for a parsed manifest.
fn manifest_if_present(path: &Path) -> Option<Option<Value>> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_manifest(path).map(Some),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(None),
        Err(_) => None,
    }
}

/// Resolve the edition for a Rust source file. A source tree without any
/// Cargo manifest keeps rustfmt's historical 2015 default. If a manifest is
/// present but malformed, has no owning package, or declares unresolved
/// inheritance, decline the proposal instead of formatting with the wrong
/// edition.
fn rust_edition(repo_path: &Path, repo_path_prefix: &Path, file_path: &Path) -> Option<String> {
    let repo_root = fs::canonicalize(repo_path).ok()?;
    let candidate = if file_path.is_absolute() {
        file_path.to_path_buf()
    } else {
        let relative_path = file_path
            .strip_prefix(repo_path_prefix)
            .unwrap_or(file_path);
        repo_root.join(relative_path)
    };
    let file_path = fs::canonicalize(candidate).ok()?;
    if !file_path.starts_with(&repo_root) {
        return None;
    }

    let mut directory = file_path.parent()?.to_path_buf();
    loop {
        let manifest_path = directory.join("Cargo.toml");
        if let Some(manifest) = manifest_if_present(&manifest_path)? {
            let canonical_manifest = fs::canonicalize(&manifest_path).ok()?;
            if !canonical_manifest.starts_with(&repo_root) {
                return None;
            }
            let package = manifest.get("package")?.as_table()?;
            return match package.get("edition") {
                None => Some("2015".to_owned()),
                Some(Value::String(edition)) if supported_rust_edition(edition) => {
                    Some(edition.clone())
                }
                Some(Value::Table(inheritance))
                    if inheritance.len() == 1
                        && inheritance.get("workspace").and_then(Value::as_bool) == Some(true) =>
                {
                    inherited_workspace_edition(&manifest_path, &manifest, package)
                }
                _ => None,
            };
        }

        if directory == repo_root {
            break;
        }
        let parent = directory.parent()?;
        if !parent.starts_with(&repo_root) {
            break;
        }
        directory = parent.to_path_buf();
    }

    Some("2015".to_owned())
}

fn read_manifest(path: &Path) -> Option<Value> {
    fs::read_to_string(path).ok()?.parse::<Value>().ok()
}

fn inherited_workspace_edition(
    package_manifest_path: &Path,
    package_manifest: &Value,
    package: &toml::map::Map<String, Value>,
) -> Option<String> {
    let workspace_manifest_path = match package.get("workspace") {
        Some(Value::String(path)) if package_manifest.get("workspace").is_none() => {
            let workspace_root = Path::new(path);
            let workspace_root = if workspace_root.is_absolute() {
                workspace_root.to_path_buf()
            } else {
                package_manifest_path.parent()?.join(workspace_root)
            };
            absolute_path(&workspace_root)?.join("Cargo.toml")
        }
        Some(_) => return None,
        None => nearest_workspace_manifest(package_manifest_path, package_manifest)?,
    };
    let workspace_manifest = if workspace_manifest_path == package_manifest_path {
        package_manifest.clone()
    } else {
        read_manifest(&workspace_manifest_path)?
    };
    let workspace = workspace_manifest.get("workspace")?.as_table()?;
    let workspace_package = workspace.get("package")?.as_table()?;
    let edition = workspace_package.get("edition")?.as_str()?;
    supported_rust_edition(edition).then(|| edition.to_owned())
}

fn nearest_workspace_manifest(
    package_manifest_path: &Path,
    package_manifest: &Value,
) -> Option<PathBuf> {
    let mut directory = package_manifest_path.parent()?.to_path_buf();
    loop {
        let manifest_path = directory.join("Cargo.toml");
        if let Some(manifest) = manifest_if_present(&manifest_path)? {
            let manifest = if manifest_path == package_manifest_path {
                package_manifest.clone()
            } else {
                manifest
            };
            if manifest
                .get("workspace")
                .and_then(Value::as_table)
                .is_some()
            {
                return Some(manifest_path);
            }
        }

        let parent = directory.parent()?;
        if parent == directory {
            return None;
        }
        directory = parent.to_path_buf();
    }
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
        self.tools_for(language)
            .is_some_and(|tools| !tools.is_empty())
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
    /// One recorded runner invocation: argv, working directory, timeout.
    type RecordedCall = (Vec<String>, Option<String>, Option<Duration>);

    #[derive(Default)]
    struct FakeRunner {
        calls: Mutex<Vec<RecordedCall>>,
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

        fn version_calls(&self) -> Vec<(Vec<String>, Option<String>, Option<Duration>)> {
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
        fn run(
            &self,
            argv: &[&str],
            stdin: Option<&[u8]>,
            timeout: Option<Duration>,
            _cwd: Option<&Path>,
        ) -> RunOutcome {
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
        fn run(
            &self,
            argv: &[&str],
            stdin: Option<&[u8]>,
            timeout: Option<Duration>,
            cwd: Option<&Path>,
        ) -> RunOutcome {
            self.0.run(argv, stdin, timeout, cwd)
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
        let repo_path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        FormatFixer {
            repo_path: freeze_repo_path(&repo_path),
            repo_path_prefix: repo_path,
            runner: Box::new(SharedRunner(fake.clone())),
            available_tools: tools,
        }
    }

    struct TempRepo(PathBuf);

    impl TempRepo {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};

            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "aethyme-format-edition-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            TempRepo(path)
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn rust_fixer(repo: &TempRepo, fake: &Arc<FakeRunner>) -> FormatFixer {
        preloaded_at(&repo.0, vec![("rust", vec!["rustfmt"])], fake)
    }

    fn preloaded_at(
        repo_path: &Path,
        tools: Vec<(&'static str, Vec<&'static str>)>,
        fake: &Arc<FakeRunner>,
    ) -> FormatFixer {
        FormatFixer {
            repo_path: freeze_repo_path(repo_path),
            repo_path_prefix: repo_path.to_path_buf(),
            runner: Box::new(SharedRunner(fake.clone())),
            available_tools: tools,
        }
    }

    fn assert_rustfmt_edition(fake: &FakeRunner, edition: &str) {
        assert_eq!(
            fake.format_calls(),
            vec![vec![
                "rustfmt".to_string(),
                "--edition".to_string(),
                edition.to_string(),
                "--emit".to_string(),
                "stdout".to_string(),
            ]]
        );
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
            assert_eq!(timeout, Some(Duration::from_secs(5)), "{argv:?}");
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
            (
                "javascript",
                "prettier",
                "a.js",
                vec!["prettier", "--parser", "babel"],
            ),
            (
                "javascript",
                "prettier",
                "a.jsx",
                vec!["prettier", "--parser", "babel"],
            ),
            (
                "typescript",
                "prettier",
                "a.ts",
                vec!["prettier", "--parser", "typescript"],
            ),
            (
                "typescript",
                "prettier",
                "a.tsx",
                vec!["prettier", "--parser", "typescript"],
            ),
            ("go", "gofmt", "a.go", vec!["gofmt"]),
            (
                "rust",
                "rustfmt",
                "a.rs",
                vec!["rustfmt", "--edition", "2015", "--emit", "stdout"],
            ),
        ] {
            let repo = TempRepo::new();
            let file_path = repo.write(file, "in\n");
            let fake = FakeRunner::new(vec![]).formatting("out\n");
            let fixer = preloaded_at(&repo.0, vec![(language, vec![tool])], &fake);
            assert_eq!(
                fixer.fix(&file_path, "in\n"),
                Some("out\n".to_string()),
                "{tool} on {file}"
            );
            assert_eq!(fake.format_calls(), vec![expected], "{tool} on {file}");
            let recorded = fake.calls.lock().unwrap();
            assert_eq!(recorded[0].1.as_deref(), Some("in\n"), "{tool} stdin");
            assert_eq!(
                recorded[0].2,
                Some(Duration::from_secs(30)),
                "{tool} timeout"
            );
        }
    }

    #[test]
    fn rustfmt_uses_the_nearest_package_manifest_edition() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/member\"]\n[workspace.package]\nedition = \"2018\"\n",
        );
        repo.write(
            "crates/member/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        let file = repo.write("crates/member/src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), Some("formatted\n".to_string()));
        assert_rustfmt_edition(&fake, "2024");
    }

    #[test]
    fn repo_root_constructor_passes_the_supplied_root_to_rustfmt() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        repo.write("src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec!["rustfmt"]).formatting("formatted\n");
        let fixer =
            FormatFixer::with_repo_path_and_runner(&repo.0, Box::new(SharedRunner(fake.clone())));

        assert_eq!(
            fixer.fix(Path::new("src/lib.rs"), "input\n"),
            Some("formatted\n".to_string())
        );
        assert_rustfmt_edition(&fake, "2024");
    }

    #[test]
    fn rustfmt_inherits_edition_from_the_nearest_workspace() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/member\"]\n[workspace.package]\nedition = \"2024\"\n",
        );
        repo.write(
            "crates/member/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition.workspace = true\n",
        );
        repo.write("crates/member/src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(
            fixer.fix(Path::new("crates/member/src/lib.rs"), "input\n"),
            Some("formatted\n".to_string())
        );
        assert_rustfmt_edition(&fake, "2024");
    }

    #[test]
    fn rustfmt_prefers_a_package_explicit_workspace_path() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = []\n[workspace.package]\nedition = \"2018\"\n",
        );
        repo.write(
            "workspace-b/Cargo.toml",
            "[workspace]\nmembers = [\"../members/crate\"]\n[workspace.package]\nedition = \"2024\"\n",
        );
        repo.write(
            "members/crate/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nworkspace = \"../../workspace-b\"\nedition.workspace = true\n",
        );
        let file = repo.write("members/crate/src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), Some("formatted\n".to_string()));
        assert_rustfmt_edition(&fake, "2024");
    }

    #[test]
    fn rustfmt_uses_the_cargo_default_when_package_omits_edition() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"legacy\"\nversion = \"0.1.0\"\n",
        );
        let file = repo.write("src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), Some("formatted\n".to_string()));
        assert_rustfmt_edition(&fake, "2015");
    }

    #[test]
    fn rustfmt_declines_malformed_or_unknown_manifest_editions() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[workspace]\nmembers = [\"crates/malformed\", \"crates/nonstring\", \"crates/unknown\"]\n[workspace.package]\nedition = \"2024\"\n",
        );
        repo.write(
            "crates/malformed/Cargo.toml",
            "[package\nedition = \"2024\"\n",
        );
        repo.write(
            "crates/nonstring/Cargo.toml",
            "[package]\nname = \"nonstring\"\nversion = \"0.1.0\"\nedition = 2024\n",
        );
        repo.write(
            "crates/unknown/Cargo.toml",
            "[package]\nname = \"unknown\"\nversion = \"0.1.0\"\nedition = \"2030\"\n",
        );
        let malformed = repo.write("crates/malformed/src/lib.rs", "fn f() {}\n");
        let nonstring = repo.write("crates/nonstring/src/lib.rs", "fn f() {}\n");
        let unknown = repo.write("crates/unknown/src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&malformed, "input\n"), None);
        assert_eq!(fixer.fix(&nonstring, "input\n"), None);
        assert_eq!(fixer.fix(&unknown, "input\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn rustfmt_declines_unresolved_workspace_inheritance() {
        let repo = TempRepo::new();
        repo.write("Cargo.toml", "[workspace]\nmembers = [\"crates/member\"]\n");
        repo.write(
            "crates/member/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition.workspace = true\n",
        );
        let file = repo.write("crates/member/src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn rustfmt_declines_inheritance_without_a_discoverable_workspace() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"parent\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        repo.write(
            "nested/Cargo.toml",
            "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition.workspace = true\n",
        );
        let file = repo.write("nested/src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn rustfmt_declines_a_virtual_workspace_without_an_owning_package() {
        let repo = TempRepo::new();
        repo.write("Cargo.toml", "[workspace]\nmembers = []\n");
        let file = repo.write("src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn rustfmt_declines_nested_virtual_or_invalid_package_manifests() {
        let repo = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"outer\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        repo.write("nested-virtual/Cargo.toml", "[workspace]\nmembers = []\n");
        repo.write(
            "nested-wrong-type/Cargo.toml",
            "package = \"not a table\"\n",
        );
        let virtual_file = repo.write("nested-virtual/src/lib.rs", "fn f() {}\n");
        let wrong_type_file = repo.write("nested-wrong-type/src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&virtual_file, "input\n"), None);
        assert_eq!(fixer.fix(&wrong_type_file, "input\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn rustfmt_declines_a_present_but_unreadable_manifest() {
        let repo = TempRepo::new();
        fs::create_dir_all(repo.0.join("Cargo.toml")).unwrap();
        let file = repo.write("src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn rustfmt_resolves_symlinks_before_parent_components() {
        let repo = TempRepo::new();
        let outside = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"inside\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        repo.write("src/lib.rs", "fn f() {}\n");
        fs::create_dir_all(outside.0.join("nested")).unwrap();
        let outside_file = outside.write("src/lib.rs", "fn f() {}\n");
        std::os::unix::fs::symlink(outside.0.join("nested"), repo.0.join("alias")).unwrap();
        let path_with_symlink_and_parent = repo.0.join("alias/../src/lib.rs");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(
            fs::canonicalize(&path_with_symlink_and_parent).unwrap(),
            outside_file
        );
        assert_eq!(fixer.fix(&path_with_symlink_and_parent, "input\n"), None);
        assert!(fake.format_calls().is_empty());
    }

    #[test]
    fn rustfmt_declines_a_file_that_resolves_outside_the_repository() {
        let repo = TempRepo::new();
        let outside = TempRepo::new();
        repo.write(
            "Cargo.toml",
            "[package]\nname = \"inside\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        );
        let file = outside.write("src/lib.rs", "fn f() {}\n");
        let fake = FakeRunner::new(vec![]).formatting("formatted\n");
        let fixer = rust_fixer(&repo, &fake);

        assert_eq!(fixer.fix(&file, "input\n"), None);
        assert!(fake.format_calls().is_empty());
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
        assert_eq!(
            fixer.fix(Path::new("a.py"), "in\n"),
            Some("fixed\n".to_string())
        );
        assert_eq!(
            fake.format_calls(),
            vec![vec!["black", "--quiet", "-"], vec!["autopep8", "-"]]
        );
    }

    #[test]
    fn the_first_changing_tool_wins_and_later_tools_never_run() {
        let fake = FakeRunner::new(vec![]).formatting("fixed\n");
        let fixer = preloaded(vec![("python", vec!["black", "autopep8"])], &fake);
        assert_eq!(
            fixer.fix(Path::new("a.py"), "in\n"),
            Some("fixed\n".to_string())
        );
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
}
