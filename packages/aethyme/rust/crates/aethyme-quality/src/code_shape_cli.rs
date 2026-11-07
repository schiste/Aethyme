//! CLI for configurable, role-aware source shape diagnostics.
use crate::code_shape::{self, Level, Metrics, Policy, Violation};
use crate::snapshot::TrackedSnapshot;
use crate::{code_shape_graph, code_shape_scan};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const HELP: &str = "Usage: aethyme quality code-shape [--repo PATH] [--config PATH] [--level warning|error|off] [--format json|md]\n\nAnalyze tracked JavaScript, TypeScript and Python behavior modules.\nDefaults to advisory warnings. Error mode exits 1 for violations and 2 for incomplete analysis.\nConfiguration defaults to .aethyme/code-shape.toml if present.\n";
#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Config {
    policy: Policy,
    /// Exact dependency names known to refer to packages outside the repository.
    external_imports: Vec<String>,
    /// Additional paths or directory prefixes excluded from behavior findings.
    exclude: Vec<String>,
}
#[derive(Serialize)]
struct Finding {
    file: String,
    #[serde(flatten)]
    violation: Violation,
}
#[derive(Serialize)]
struct Report {
    schema_version: u32,
    kind: &'static str,
    level: Level,
    complete: bool,
    files_analyzed: usize,
    behavior_files: usize,
    policy: Policy,
    incomplete: Vec<String>,
    findings: Vec<Finding>,
}
fn supported(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some("py" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs")
    )
}
fn excluded(path: &Path) -> bool {
    let s = path.to_string_lossy();
    let components = path.components().filter_map(|c| c.as_os_str().to_str());
    components.into_iter().any(|c| {
        matches!(
            c,
            "tests"
                | "test"
                | "__tests__"
                | "fixtures"
                | "__fixtures__"
                | "e2e"
                | "load_tests"
                | "load-tests"
                | "migrations"
                | "migration_history"
                | "generated"
                | ".next"
                | "__pycache__"
                | "stories"
        )
    }) || s.contains(".test.")
        || s.contains(".spec.")
        || s.contains(".stories.")
        || path.file_name().and_then(|s| s.to_str()).is_some_and(|n| {
            n.starts_with("test_") && n.ends_with(".py") || n.ends_with("_test.py")
        })
}
fn behavior(path: &Path, additional: &[String]) -> bool {
    let s = path.to_string_lossy();
    if additional
        .iter()
        .any(|p| s == *p || s.starts_with(&format!("{}/", p.trim_end_matches('/'))))
    {
        return false;
    }
    let parts: Vec<_> = s.split('/').collect();
    let name = parts.last().copied().unwrap_or("");
    if matches!(
        name,
        "index.ts"
            | "index.tsx"
            | "index.js"
            | "index.jsx"
            | "index.mjs"
            | "__init__.py"
            | "server.mjs"
            | "asgi.py"
            | "wsgi.py"
            | "urls.py"
            | "serializers.py"
            | "api_views.py"
            | "views.py"
            | "settings.py"
            | "types.ts"
    ) {
        return false;
    }
    if parts.iter().any(|p| {
        matches!(
            *p,
            "adapters" | "adapter" | "registry" | "config" | "types" | "contracts" | "schemas"
        )
    }) {
        return false;
    }
    if parts.len() == 4
        && parts[0] == "apps"
        && parts[2] == "src"
        && (name.starts_with("main.") || name.starts_with("App."))
    {
        return false;
    }
    if s == "backend/manage.py" {
        return false;
    }
    if matches!(parts.first(), Some(&"Agents" | &"devops" | &"scripts")) {
        return false;
    }
    !(name.starts_with("views_")
        || name.ends_with("_views.py")
        || name.ends_with("Api.ts")
        || name.ends_with("API.ts")
        || name.starts_with("registry.")
        || name.ends_with("Registry.ts")
        || name.ends_with("registry.py")
        || name.ends_with(".config.ts")
        || name.ends_with(".d.ts")
        || name.contains(".zod.")
        || name.contains(".schema."))
}
fn execute(args: &[String]) -> Result<(Report, bool), String> {
    let mut repo = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut config_path = None::<PathBuf>;
    let mut level = None;
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        let value = || {
            args.get(i + 1)
                .ok_or_else(|| format!("{} requires a value", args[i]))
        };
        match args[i].as_str() {
            "--repo" => {
                repo = PathBuf::from(value()?);
                i += 2;
            }
            "--config" => {
                config_path = Some(PathBuf::from(value()?));
                i += 2;
            }
            "--format" | "-f" => {
                json = match value()?.as_str() {
                    "json" => true,
                    "md" => false,
                    _ => return Err("--format must be json or md".into()),
                };
                i += 2;
            }
            "--level" => {
                level = Some(match value()?.as_str() {
                    "warning" => Level::Warning,
                    "error" => Level::Error,
                    "off" => Level::Off,
                    _ => return Err("--level must be warning, error or off".into()),
                });
                i += 2;
            }
            s => return Err(format!("unknown option {s}")),
        }
    }
    let snapshot = TrackedSnapshot::materialize(&repo)?;
    let explicit_config = config_path.is_some();
    let config_path =
        config_path.unwrap_or_else(|| snapshot.source_root().join(".aethyme/code-shape.toml"));
    let mut config: Config = match std::fs::read_to_string(&config_path) {
        Ok(raw) => {
            toml::from_str(&raw).map_err(|e| format!("invalid {}: {e}", config_path.display()))?
        }
        Err(e) if !explicit_config && e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(format!("cannot read {}: {e}", config_path.display())),
    };
    if let Some(level) = level {
        config.policy.level = level;
    }
    config.policy.validate()?;
    if config.policy.level == Level::Off {
        return Ok((
            Report {
                schema_version: 1,
                kind: "code_shape",
                level: Level::Off,
                complete: true,
                files_analyzed: 0,
                behavior_files: 0,
                policy: config.policy,
                incomplete: vec![],
                findings: vec![],
            },
            json,
        ));
    }
    let mut sources = BTreeMap::new();
    for path in snapshot.relative_paths() {
        if supported(path) && !excluded(path) {
            let content = std::fs::read_to_string(snapshot.root().join(path))
                .map_err(|e| format!("{}: {e}", path.display()))?;
            sources.insert(path.clone(), content);
        }
    }
    if sources.is_empty() && config.policy.level != Level::Off {
        return Err("no eligible JavaScript, TypeScript or Python files found".into());
    }
    let graph = code_shape_graph::build(&sources, &config.external_imports)?;
    let complete = graph.incomplete.is_empty();
    let mut findings = Vec::new();
    let mut count = 0;
    for (file, source) in &sources {
        if !behavior(file, &config.exclude) {
            continue;
        }
        count += 1;
        let m = code_shape_scan::measure(file, source)?;
        let metrics = Metrics {
            source_lines: m.source_lines,
            callables: m.callables,
            exports: m.exports,
            internal_callables: m.internal_callables,
            runtime_fan_out: graph.edges.get(file).map_or(0, |e| e.len()),
            runtime_fan_in: graph.fan_in(file),
            max_callable_lines: m.max_callable_lines,
            max_cyclomatic_complexity: m.max_cyclomatic_complexity,
            runtime_cycle: graph.in_cycle(file),
        };
        for violation in code_shape::violations(&config.policy, &metrics)? {
            if !complete
                && matches!(
                    violation.metric,
                    "runtime_fan_out" | "runtime_fan_in" | "runtime_cycle"
                )
            {
                continue;
            }
            findings.push(Finding {
                file: file.display().to_string(),
                violation,
            });
        }
    }
    Ok((
        Report {
            schema_version: 1,
            kind: "code_shape",
            level: config.policy.level,
            complete,
            files_analyzed: sources.len(),
            behavior_files: count,
            policy: config.policy,
            incomplete: graph.incomplete,
            findings,
        },
        json,
    ))
}
pub fn run(args: &[String]) -> u8 {
    if args.iter().any(|s| s == "--help" || s == "-h") {
        print!("{HELP}");
        return 0;
    }
    match execute(args) {
        Err(e) => {
            eprintln!("Error: {e}");
            2
        }
        Ok((report, json)) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).expect("report serializes")
                );
            } else {
                println!(
                    "Code shape: {} behavior files; {} findings; analysis {}",
                    report.behavior_files,
                    report.findings.len(),
                    if report.complete {
                        "complete"
                    } else {
                        "incomplete"
                    }
                );
                for finding in &report.findings {
                    println!(
                        "{:?}: {}: {} is {}; expected {}",
                        finding.violation.level,
                        finding.file,
                        finding.violation.metric,
                        finding.violation.actual,
                        finding.violation.expected
                    );
                }
                for problem in &report.incomplete {
                    println!("incomplete: {problem}");
                }
            }
            if report.level == Level::Error && !report.complete {
                2
            } else if report.level == Level::Error && !report.findings.is_empty() {
                1
            } else {
                0
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roles_and_tests_are_excluded() {
        assert!(!behavior(Path::new("src/index.ts"), &[]));
        assert!(!behavior(Path::new("src/adapters/api.ts"), &[]));
        assert!(behavior(Path::new("src/order.ts"), &[]));
        assert!(excluded(Path::new("src/order.test.ts")));
    }
    #[test]
    fn warning_error_config_and_incomplete_imports_work_end_to_end() {
        use std::{
            fs,
            process::Command,
            time::{SystemTime, UNIX_EPOCH},
        };
        let root = std::env::temp_dir().join(format!(
            "aethyme-shape-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            root.join("src/behavior.ts"),
            "export function go() { return 1; }\n",
        )
        .unwrap();
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["add", "."])
                .status()
                .unwrap()
                .success()
        );
        let args = vec!["--repo".into(), root.display().to_string()];
        let (report, _) = execute(&args).unwrap();
        assert!(report.complete);
        assert!(!report.findings.is_empty());
        assert_eq!(run(&args), 0);
        let mut enforce = args.clone();
        enforce.extend(["--level".into(), "error".into()]);
        assert_eq!(run(&enforce), 1);
        fs::write(
            root.join("src/behavior.ts"),
            "import x from 'unknown'; export function go() { return x; }\n",
        )
        .unwrap();
        let (report, _) = execute(&enforce).unwrap();
        assert!(!report.complete);
        assert_eq!(run(&enforce), 2);
        let config = root.join("shape.toml");
        fs::write(
            &config,
            "external_imports = ['unknown']\n[policy]\nmin_source_lines = 1\nmin_callables = 1\n",
        )
        .unwrap();
        enforce.extend(["--config".into(), config.display().to_string()]);
        assert_eq!(run(&enforce), 0);
        fs::remove_dir_all(root).unwrap();
    }
}
