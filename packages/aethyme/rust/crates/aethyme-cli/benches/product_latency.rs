use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use criterion::{BatchSize, Criterion, black_box, criterion_group, criterion_main};
use tempfile::TempDir;

struct Playground {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Playground {
    fn new(fixture: &Path) -> Self {
        let temp = tempfile::tempdir().expect("create isolated Playground");
        let root = temp.path().join("repo");
        let home = temp.path().join("home");
        fs::create_dir_all(&root).expect("create Playground repository");
        fs::create_dir_all(&home).expect("create isolated HOME");
        copy_tree(fixture, &root);
        let playground = Self {
            _temp: temp,
            root,
            home,
        };
        playground.git(&["init", "-q", "-b", "main"]);
        playground.git(&["add", "-A"]);
        playground.git(&["commit", "-qm", "chore(benchmark): seed fixed Playground"]);
        playground
    }

    fn git(&self, args: &[&str]) -> Output {
        let output = Command::new("git")
            .current_dir(&self.root)
            .args(args)
            .env("HOME", &self.home)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Aethyme benchmark")
            .env("GIT_AUTHOR_EMAIL", "benchmark@example.invalid")
            .env("GIT_COMMITTER_NAME", "Aethyme benchmark")
            .env("GIT_COMMITTER_EMAIL", "benchmark@example.invalid")
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn command(&self, binary: &Path, args: &[OsString]) -> Output {
        let output = Command::new(binary)
            .args(args)
            .env("HOME", &self.home)
            .env("AETHYME_HOST_CACHE_DIR", self.home.join("host-cache"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap_or_else(|error| panic!("spawn {}: {error}", binary.display()));
        assert!(
            output.status.success(),
            "{} {args:?} failed: {}",
            binary.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }

    fn aethyme_args(&self, command: &[&str]) -> Vec<OsString> {
        let mut args = command.iter().map(OsString::from).collect::<Vec<_>>();
        args.push("--repo".into());
        args.push(self.root.as_os_str().to_owned());
        args
    }

    fn prepare_graph(&self, aethyme: &Path) {
        self.command(
            aethyme,
            &self.aethyme_args(&[
                "deploy",
                "--with-graph",
                "--graph-repository",
                "playground/performance",
            ]),
        );
        self.git(&["add", "-A"]);
        self.git(&["commit", "-qm", "chore(benchmark): enroll Playground"]);

        let plan = self.command(
            aethyme,
            &self.aethyme_args(&["graph", "refresh", "plan", "--json"]),
        );
        let plan: serde_json::Value =
            serde_json::from_slice(&plan.stdout).expect("graph refresh plan JSON");
        let confirmation = plan["plan_sha256"]
            .as_str()
            .expect("graph refresh plan digest");
        self.command(
            aethyme,
            &self.aethyme_args(&[
                "graph",
                "refresh",
                "execute",
                "--confirm",
                confirmation,
                "--json",
            ]),
        );
        self.git(&["add", "--", ".aethyme/graph"]);
        self.git(&["commit", "-qm", "chore(benchmark): commit graph snapshot"]);
        self.command(
            aethyme,
            &self.aethyme_args(&["graph", "materialize", "--json"]),
        );
    }

    fn explore(&self, aethyme: &Path) -> Output {
        self.command(
            aethyme,
            &self.aethyme_args(&[
                "explore",
                "--request",
                "locate the primary application entrypoint",
                "--format",
                "answer-json",
                "--show-observability",
                "--depth",
                "0",
            ]),
        )
    }
}

fn copy_tree(source: &Path, destination: &Path) {
    for entry in fs::read_dir(source)
        .unwrap_or_else(|error| panic!("read fixture {}: {error}", source.display()))
    {
        let entry = entry.expect("read fixture entry");
        let from = entry.path();
        let to = destination.join(entry.file_name());
        if from.is_dir() {
            fs::create_dir_all(&to).expect("create fixture directory");
            copy_tree(&from, &to);
        } else {
            fs::copy(&from, &to).unwrap_or_else(|error| panic!("copy {}: {error}", from.display()));
        }
    }
}

fn binary_env(name: &str) -> PathBuf {
    std::env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .unwrap_or_else(|| {
            panic!("set {name} to the release executable; use scripts/bench-product-latency.sh")
        })
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../../packages/aethyme-eval/benchmarks/performance/fixture")
        .canonicalize()
        .expect("checked-in Playground fixture")
}

fn bench_product_latency(criterion: &mut Criterion) {
    let aethyme = binary_env("AETHYME_BENCH_BIN");
    let graph_index = binary_env("AETHYME_GRAPH_INDEX_BENCH_BIN");
    let fixture = fixture_root();

    let mut group = criterion.benchmark_group("product_latency");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(10));
    group.warm_up_time(Duration::from_secs(2));

    group.bench_function("explore_cold_process", |bencher| {
        bencher.iter_batched(
            || {
                let playground = Playground::new(&fixture);
                playground.prepare_graph(&aethyme);
                playground
            },
            |playground| black_box(playground.explore(&aethyme).stdout.len()),
            BatchSize::PerIteration,
        );
    });

    let warm_playground = Playground::new(&fixture);
    warm_playground.prepare_graph(&aethyme);
    black_box(warm_playground.explore(&aethyme));
    group.bench_function("explore_warm_process", |bencher| {
        bencher.iter(|| black_box(warm_playground.explore(&aethyme).stdout.len()));
    });

    let verify_playground = Playground::new(&fixture);
    verify_playground.prepare_graph(&aethyme);
    let answer = verify_playground.explore(&aethyme);
    let answer_path = verify_playground.root.join("explore-answer.json");
    fs::write(&answer_path, answer.stdout).expect("write Explore answer fixture");
    let verify_args = vec![
        OsString::from("verify-targets"),
        OsString::from("--from"),
        answer_path.as_os_str().to_owned(),
        OsString::from("--repo"),
        verify_playground.root.as_os_str().to_owned(),
        OsString::from("--max-targets"),
        OsString::from("2"),
        OsString::from("--max-lines"),
        OsString::from("80"),
    ];
    group.bench_function("verify_targets", |bencher| {
        bencher.iter(|| {
            black_box(
                verify_playground
                    .command(&aethyme, &verify_args)
                    .stdout
                    .len(),
            )
        });
    });

    group.bench_function("graph_index", |bencher| {
        bencher.iter_batched(
            || Playground::new(&fixture),
            |playground| {
                let args = vec![
                    OsString::from("--repo-root"),
                    playground.root.as_os_str().to_owned(),
                    OsString::from("--repo-name"),
                    OsString::from("playground/performance"),
                    OsString::from("--engine-version"),
                    OsString::from(env!("CARGO_PKG_VERSION")),
                ];
                black_box(playground.command(&graph_index, &args).stdout.len())
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_product_latency);
criterion_main!(benches);
