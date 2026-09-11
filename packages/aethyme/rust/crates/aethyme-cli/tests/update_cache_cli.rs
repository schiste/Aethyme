//! `update check` grew a cache so that something other than a person could
//! afford to run it. These pin the properties that make an automatic caller
//! safe, and every one of them is a property of a *sequence* of runs rather
//! than of a single answer: the second ask must cost no request, a new release
//! must not have to wait out the cache to be seen, and an unreachable server
//! must degrade to dated news rather than to an error.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{Value, json};

/// A release server made of files, so nothing here touches the network.
struct FakeRelease {
    _temp: tempfile::TempDir,
    root: PathBuf,
}

impl FakeRelease {
    fn new(version: &str) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let release = FakeRelease {
            root: temp.path().to_path_buf(),
            _temp: temp,
        };
        release.publish(version);
        release
    }

    fn publish(&self, version: &str) {
        let directory = self.root.join("releases/latest/download");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("release-manifest.json"), manifest(version)).unwrap();
    }

    /// Take the server away, the way a lost network takes it away.
    fn go_offline(&self) {
        fs::remove_file(self.manifest_path()).unwrap();
    }

    fn manifest_path(&self) -> PathBuf {
        self.root
            .join("releases/latest/download/release-manifest.json")
    }

    fn cache(&self) -> PathBuf {
        self.root.join("host-cache")
    }

    fn check(&self, extra: &[&str]) -> Output {
        let mut args = vec!["update", "check"];
        args.extend_from_slice(extra);
        Command::new(env!("CARGO_BIN_EXE_aethyme"))
            .args(&args)
            .env(
                "AETHYME_RELEASE_BASE_URL",
                format!("file://{}", self.root.display()),
            )
            .env("AETHYME_HOST_CACHE_DIR", self.cache())
            .output()
            .unwrap()
    }

    /// Age every cached entry past any plausible lifetime.
    fn age_cache(&self, days: i64) {
        let directory = self.cache().join("update-manifests");
        for entry in fs::read_dir(&directory).unwrap().flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let mut value: Value =
                serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
            let fetched = value["fetched_at_unix_ms"].as_i64().unwrap();
            value["fetched_at_unix_ms"] = json!(fetched - days * 24 * 60 * 60 * 1000);
            fs::write(&path, value.to_string()).unwrap();
        }
    }

    fn entry_count(&self) -> usize {
        count_with_extension(&self.cache().join("update-manifests"), "json")
    }
}

fn count_with_extension(directory: &Path, extension: &str) -> usize {
    let Ok(entries) = fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().and_then(|v| v.to_str()) == Some(extension))
        .count()
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// A manifest the CLI's own validation accepts: artifact names have to carry
/// the version they belong to, so a version bump is not a one-field edit.
fn manifest(version: &str) -> Vec<u8> {
    let targets = [
        "aarch64-apple-darwin",
        "x86_64-apple-darwin",
        "x86_64-unknown-linux-gnu",
    ];
    let artifacts = targets
        .iter()
        .map(|target| {
            json!({
                "archive": format!("aethyme-v{version}-{target}.tar.gz"),
                "binaries": ["aethyme", "aethyme-engine-cli"],
                "sha256": "b".repeat(64),
                "size_bytes": 123,
                "target": target,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_vec_pretty(&json!({
        "artifacts": artifacts,
        "compatibility": {
            "broker_storage": {"current_schema": 7, "minimum_readable_schema": 1},
            "engine_protocol": 1,
            "minimum_git_version": "2.38"
        },
        "installer": {"filename": "install.sh", "sha256": "c".repeat(64), "size_bytes": 42},
        "release_channel": "stable",
        "required_binaries": ["aethyme", "aethyme-engine-cli"],
        "schema_version": 1,
        "source_sha": "a".repeat(40),
        "supported_platforms": targets,
        "version": version
    }))
    .unwrap()
}

/// The point of the exercise: the second ask reuses the first, and says so
/// rather than passing dated bytes off as live ones.
#[test]
fn the_second_check_reuses_the_first_and_admits_it() {
    let release = FakeRelease::new("9.0.0");

    let first = release.check(&[]);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        !stdout(&first).contains("cached"),
        "a cold cache must produce a live answer: {}",
        stdout(&first)
    );
    assert_eq!(
        release.entry_count(),
        1,
        "the live answer must be written down"
    );

    let second = release.check(&[]);
    assert!(second.status.success());
    assert!(
        stdout(&second).contains("cached release manifest"),
        "the second check must disclose that it is cached: {}",
        stdout(&second)
    );
}

/// `--refresh` has to actually re-ask. A stale entry no command could clear
/// would make the cache unfixable from the command line.
#[test]
fn refresh_re_asks_even_when_the_entry_is_fresh() {
    let release = FakeRelease::new("9.0.0");
    release.check(&[]);

    let refreshed = release.check(&["--refresh"]);
    assert!(refreshed.status.success());
    assert!(
        !stdout(&refreshed).contains("cached"),
        "--refresh must reach the server: {}",
        stdout(&refreshed)
    );
}

/// Caching the manifest rather than the verdict is the load-bearing choice.
/// A cached *plan* would keep naming the old target until the entry expired,
/// including right after the upgrade it told you to run.
#[test]
fn the_plan_is_recomputed_from_cached_bytes_rather_than_stored() {
    let release = FakeRelease::new("9.0.0");
    release.check(&[]);

    release.publish("99.0.0");
    let refreshed = release.check(&["--refresh", "--json"]);
    assert!(refreshed.status.success());
    let plan: Value = serde_json::from_str(&stdout(&refreshed)).unwrap();
    assert_eq!(plan["target_version"], "99.0.0");
    assert_eq!(plan["manifest_from_cache"], false);

    let cached = release.check(&["--json"]);
    let plan: Value = serde_json::from_str(&stdout(&cached)).unwrap();
    assert_eq!(plan["target_version"], "99.0.0");
    assert_eq!(plan["manifest_from_cache"], true);
    assert!(
        plan["manifest_age_seconds"].as_i64().unwrap() >= 0,
        "a cached answer must carry its age as data too"
    );
}

/// An expired entry plus an unreachable server is still an answer, and its
/// age travels with it. This is the laptop-without-a-network case.
#[test]
fn an_unreachable_server_falls_back_to_the_expired_entry_and_dates_it() {
    let release = FakeRelease::new("9.0.0");
    release.check(&[]);
    release.age_cache(3);
    release.go_offline();

    let offline = release.check(&[]);
    assert!(
        offline.status.success(),
        "dated news beats no news: {}",
        String::from_utf8_lossy(&offline.stderr)
    );
    let text = stdout(&offline);
    assert!(text.contains("offline"), "{text}");
    assert!(text.contains("3d old"), "the age has to be stated: {text}");
}

/// The fallback must never invent an answer out of an empty cache.
#[test]
fn an_unreachable_server_with_nothing_cached_is_an_error() {
    let release = FakeRelease::new("9.0.0");
    release.go_offline();

    let output = release.check(&[]);
    assert!(
        !output.status.success(),
        "a cold cache and no server cannot succeed: {}",
        stdout(&output)
    );
}

/// A zero lifetime asks for live answers only — including no stale fallback,
/// which would otherwise quietly reintroduce the caching it switched off.
#[test]
fn a_zero_lifetime_writes_nothing() {
    let release = FakeRelease::new("9.0.0");
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["update", "check"])
        .env(
            "AETHYME_RELEASE_BASE_URL",
            format!("file://{}", release.root.display()),
        )
        .env("AETHYME_HOST_CACHE_DIR", release.cache())
        .env("AETHYME_UPDATE_CACHE_TTL_SECONDS", "0")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        release.entry_count(),
        0,
        "a disabled cache must not write entries"
    );
}

/// `update check` had no options at all before this. A silently ignored typo
/// is exactly how a `--refresh` that never refreshes goes unnoticed.
#[test]
fn an_unknown_check_option_is_refused_by_name() {
    let output = Command::new(env!("CARGO_BIN_EXE_aethyme"))
        .args(["update", "check", "--force"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(text.contains("--force"), "{text}");
    assert!(
        text.contains("--refresh"),
        "the refusal has to name what is accepted: {text}"
    );
}
