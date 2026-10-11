//! Actual disabled native CLI parity against the independently pinned old
//! executable. Paths and status timings are the only declared normalizations.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::{Command, Output, Stdio};
struct Client {
    child: std::process::Child,
    reaped: bool,
}
impl Drop for Client {
    fn drop(&mut self) {
        if !self.reaped {
            crate::warn_unrecorded("stop exact compatibility client", self.child.kill());
            crate::warn_unrecorded("reap exact compatibility client", self.child.wait());
        }
    }
}
pub(super) fn bounded(mut command: Command) -> Output {
    let stdout = tempfile::NamedTempFile::new().unwrap();
    let stderr = tempfile::NamedTempFile::new().unwrap();
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout.reopen().unwrap()))
        .stderr(Stdio::from(stderr.reopen().unwrap()))
        .spawn()
        .unwrap();
    let mut client = Client {
        child,
        reaped: false,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let status = loop {
        if let Some(status) = client.child.try_wait().unwrap() {
            break status;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "native compatibility deadline"
        );
        assert!(stdout.as_file().metadata().unwrap().len() <= 8 * 1024 * 1024);
        assert!(stderr.as_file().metadata().unwrap().len() <= 8 * 1024 * 1024);
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    client.reaped = true;
    let stdout = std::fs::read(stdout.path()).unwrap();
    let stderr = std::fs::read(stderr.path()).unwrap();
    assert!(stdout.len() <= 8 * 1024 * 1024 && stderr.len() <= 8 * 1024 * 1024);
    Output {
        status,
        stdout,
        stderr,
    }
}
fn current() -> PathBuf {
    let binary = PathBuf::from(
        std::env::var_os("AETHYME_COMPAT_CURRENT_BINARY").expect("explicit current binary"),
    );
    assert!(binary.is_absolute());
    let expected = std::env::var("AETHYME_COMPAT_CURRENT_SHA256")
        .expect("independently recorded current binary digest");
    let mut hash = Sha256::new();
    let mut file = std::fs::File::open(&binary).unwrap();
    let mut bytes = [0u8; 65536];
    loop {
        let n = file.read(&mut bytes).unwrap();
        if n == 0 {
            break;
        }
        hash.update(&bytes[..n]);
    }
    assert_eq!(format!("{:x}", hash.finalize()), expected);
    let mut command = Command::new(&binary);
    command
        .arg("--version")
        .env_clear()
        .env("PATH", super::old_binary::CLEAN_PATH);
    let version = bounded(command);
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(
        &std::env::var("AETHYME_COMPAT_CURRENT_SOURCE").expect("exact compiled current source")
    ));
    binary
}
fn run(binary: &Path, f: &Fixture, args: &[&str]) -> Output {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command
        .args(["-p", super::old_binary::DENY_NETWORK])
        .arg(binary)
        .args(args)
        .current_dir(f.repo.path())
        .env_clear()
        .env("PATH", super::old_binary::CLEAN_PATH)
        .env("AETHYME_HOST_STATE_DIR", f.host.path())
        .env("AETHYME_WORKTREE_ROOT", f._worktrees.path());
    let result = bounded(command);
    eprintln!(
        "compatibility current CLI transcript {}",
        serde_json::json!({"argv":args,"status":result.status.code(),"stdout_sha256":crate::collaboration_archive::ObjectDigest::of(&result.stdout).hex(),"stderr_sha256":crate::collaboration_archive::ObjectDigest::of(&result.stderr).hex()})
    );
    result
}
fn equivalent(old: &Output, new: &Output) {
    assert_eq!(old.status.code(), new.status.code());
    assert_eq!(old.stdout, new.stdout, "CLI stdout changed");
    assert_eq!(old.stderr, new.stderr, "CLI stderr changed");
}
#[test]
#[ignore = "actual current and independently pinned old macOS CLI under kernel network denial; explicit executable digests required"]
fn actual_disabled_cli_preserves_queue_errors_status_and_hook_contract() {
    let old = super::old_binary::binary();
    let new = current();
    super::old_binary::assert_network_denied();
    for config in [
        None,
        Some("[collaboration]\ncapture='off'\nproject='disabled-native-parity'\n"),
    ] {
        let mut f = Fixture::new(config);
        assert!(matches!(
            f.submit(),
            CaptureSubmit::Submitted { capture: None, .. }
        ));
        for args in [
            vec!["broker", "queue", "--json"],
            vec!["broker", "queue"],
            vec!["broker", "submit", "--session", "999999", "--json"],
            vec!["broker", "submit", "--session", "999999"],
        ] {
            equivalent(
                &super::old_binary::run(&old, &f, &args),
                &run(&new, &f, &args),
            );
        }
        let old_status =
            super::old_binary::run(&old, &f, &["broker", "status", "--summary", "--json"]);
        let new_status = run(&new, &f, &["broker", "status", "--summary", "--json"]);
        assert!(old_status.status.success() && new_status.status.success());
        let mut a: serde_json::Value = serde_json::from_slice(&old_status.stdout).unwrap();
        let mut b: serde_json::Value = serde_json::from_slice(&new_status.stdout).unwrap();
        assert!(
            a.as_object_mut()
                .unwrap()
                .remove("phase_timings_ms")
                .is_some()
        );
        assert!(
            b.as_object_mut()
                .unwrap()
                .remove("phase_timings_ms")
                .is_some()
        );
        assert_eq!(
            a, b,
            "disabled native status contract changed beyond measured timings"
        );
        for hook in ["pre-commit", "post-commit", "pre-push"] {
            let args = ["broker", "advanced", "hooks", "snippet", hook, "--json"];
            let old_result = super::old_binary::run(&old, &f, &args);
            let new_result = run(&new, &f, &args);
            assert!(old_result.status.success() && new_result.status.success());
            let a = String::from_utf8(old_result.stdout)
                .unwrap()
                .replace(old.to_str().unwrap(), "<installed-binary>");
            let b = String::from_utf8(new_result.stdout)
                .unwrap()
                .replace(new.to_str().unwrap(), "<installed-binary>");
            assert_eq!(a, b, "only exact installed executable path may differ");
            assert_eq!(old_result.stderr, new_result.stderr);
        }
        for executable in [&old, &new] {
            let installed = if executable == &old {
                super::old_binary::run(
                    executable,
                    &f,
                    &["broker", "advanced", "hooks", "install", "--json"],
                )
            } else {
                run(
                    executable,
                    &f,
                    &["broker", "advanced", "hooks", "install", "--json"],
                )
            };
            assert!(installed.status.success());
            let pre_commit = f.repo.path().join(".git/hooks/pre-commit");
            assert!(pre_commit.is_file());
            let mut hook = Command::new("/usr/bin/sandbox-exec");
            hook.args(["-p", super::old_binary::DENY_NETWORK, "/bin/sh"])
                .arg(pre_commit)
                .current_dir(&f.worktree)
                .env_clear()
                .env("PATH", super::old_binary::CLEAN_PATH)
                .env("AETHYME_HOST_STATE_DIR", f.host.path())
                .env("AETHYME_WORKTREE_ROOT", f._worktrees.path());
            let output = bounded(hook);
            assert!(
                output.status.success(),
                "actual owned pre-commit hook failed: {:?}",
                output
            );
            let removed = if executable == &old {
                super::old_binary::run(
                    executable,
                    &f,
                    &["broker", "advanced", "hooks", "uninstall", "--json"],
                )
            } else {
                run(
                    executable,
                    &f,
                    &["broker", "advanced", "hooks", "uninstall", "--json"],
                )
            };
            assert!(removed.status.success());
        }
        assert!(!f.host.path().join("collaboration").exists());
    }
}
