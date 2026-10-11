use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::process::{Command, Output};
const OLD_COMMIT: &str = "5e8daf712e26c63f1d4082e9a617f556cf995de2";
const OLD_DIGEST: &str = "edc1ad2829e08c6d5aa2512964e117685cc1ea4502caa765dc1e0d0b9b0a4261";
const CLEAN_PATH: &str = "/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin";
const DENY_NETWORK: &str = "(version 1) (allow default) (deny network*)";
fn verify_binary(path: &Path) {
    assert!(path.is_absolute());
    let mut file = std::fs::File::open(&path).unwrap();
    let mut hash = Sha256::new();
    let mut chunk = [0u8; 65536];
    loop {
        let count = file.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&chunk[..count]);
    }
    assert_eq!(
        format!("{:x}", hash.finalize()),
        OLD_DIGEST,
        "old binary differs from the independently recorded profile"
    );
    let version = Command::new(&path)
        .arg("--version")
        .env_clear()
        .env("PATH", CLEAN_PATH)
        .output()
        .unwrap();
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).contains(OLD_COMMIT));
}
fn binary() -> PathBuf {
    let path = PathBuf::from(
        std::env::var_os("AETHYME_COMPAT_OLD_BINARY")
            .expect("explicit independently pinned old binary required"),
    );
    verify_binary(&path);
    path
}
fn run(binary: &Path, f: &Fixture, args: &[&str]) -> Output {
    verify_binary(binary);
    let output = Command::new("/usr/bin/sandbox-exec")
        .args(["-p", DENY_NETWORK])
        .arg(binary)
        .args(args)
        .current_dir(f.repo.path())
        .env_clear()
        .env("PATH", CLEAN_PATH)
        .env("AETHYME_HOST_STATE_DIR", f.host.path())
        .env("AETHYME_WORKTREE_ROOT", f._worktrees.path())
        .output()
        .unwrap();
    eprintln!(
        "compatibility native old-binary transcript {}",
        serde_json::json!({"source":OLD_COMMIT,"binary_sha256":OLD_DIGEST,"network":"kernel-denied","args":args,"status":output.status.code(),"stdout_sha256":crate::collaboration_archive::ObjectDigest::of(&output.stdout).hex(),"stderr_sha256":crate::collaboration_archive::ObjectDigest::of(&output.stderr).hex()})
    );
    output
}
fn assert_network_denied() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port().to_string();
    let output = Command::new("/usr/bin/sandbox-exec")
        .args([
            "-p",
            DENY_NETWORK,
            "/usr/bin/nc",
            "-z",
            "-w",
            "1",
            "127.0.0.1",
            &port,
        ])
        .output()
        .unwrap();
    assert!(
        !output.status.success(),
        "native network-denial profile must be active"
    );
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}
#[test]
#[ignore = "requires the independently pinned pre-stack macOS binary and kernel network-denial profile"]
fn actual_pre_stack_binary_keeps_legacy_status_but_refuses_required_capture() {
    let binary = binary();
    assert_network_denied();
    for (policy, allowed) in [("off", true), ("advisory", true), ("required", false)] {
        let f = Fixture::new(Some(&format!(
            "[collaboration]\ncapture='{policy}'\nproject='proj-compat'\n"
        )));
        let db = f.repo.path().join(".aethyme/broker.db");
        let conn = rusqlite::Connection::open(db).unwrap();
        let before: Vec<(String, String)> = conn
            .prepare("SELECT key,value FROM meta ORDER BY key")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        drop(conn);
        let result = run(&binary, &f, &["broker", "status", "--json"]);
        assert_eq!(
            result.status.success(),
            allowed,
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        if allowed {
            let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
            assert!(value.get("agents").is_some());
        } else {
            let conn =
                rusqlite::Connection::open(f.repo.path().join(".aethyme/broker.db")).unwrap();
            let after: Vec<(String, String)> = conn
                .prepare("SELECT key,value FROM meta ORDER BY key")
                .unwrap()
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert_eq!(
                before, after,
                "unsupported old writer leaves native broker marker intact"
            );
            assert!(String::from_utf8_lossy(&result.stderr).contains("schema"));
        }
    }
}
#[test]
#[ignore = "requires the independently pinned pre-stack macOS binary and kernel network-denial profile"]
fn actual_old_cleanup_cannot_remove_new_retained_source_or_history() {
    let binary = binary();
    assert_network_denied();
    let mut f = Fixture::new(None);
    let root = CollaborationRoot::under_host_state(f.host.path());
    let mut store = CollaborationStore::open(
        &root,
        &ProjectKey::parse("cleanup-isolated").unwrap(),
        &crate::collaboration_state::forbidden_roots(f.repo.path()),
    )
    .unwrap();
    let base =
        crate::collaboration_archive::CommitOid::parse(&git(f.repo.path(), &["rev-parse", "main"]))
            .unwrap();
    let result =
        crate::collaboration_archive::CommitOid::parse(&git(&f.worktree, &["rev-parse", "HEAD"]))
            .unwrap();
    let retained =
        crate::collaboration_archive::retain_contribution(&mut store, &f.worktree, &base, &result)
            .unwrap();
    let manifest = crate::collaboration_archive::read_object(
        &store,
        &crate::collaboration_archive::ObjectDigest::of_snapshot(&retained.result.snapshot_id),
    )
    .unwrap();
    // Empty untracked runtime state is separately closed by the existing broker
    // fixture policy. The old process performs the actual eligible cleanup.
    f.broker
        .finish_abandoning_unpushed(
            f.session,
            crate::FinishOptions {
                keep_worktree: true,
            },
            "operator discards only this disposable compatibility fixture",
        )
        .unwrap();
    assert!(
        f.worktree.exists(),
        "current close must retain the old cleanup fixture"
    );
    let branch = git(&f.worktree, &["symbolic-ref", "HEAD"]);
    let tip = git(f.repo.path(), &["rev-parse", &branch]);
    assert_eq!(tip, result.as_str());
    let output = run(
        &binary,
        &f,
        &[
            "broker",
            "finish",
            "cleanup",
            &f.session.to_string(),
            "--json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["cleaned"], f.session);
    assert!(!f.worktree.exists());
    assert!(
        git(
            f.repo.path(),
            &["for-each-ref", "--format=%(refname)", &branch]
        )
        .is_empty()
    );

    assert_eq!(
        crate::collaboration_archive::read_object(
            &store,
            &crate::collaboration_archive::ObjectDigest::of_snapshot(&retained.result.snapshot_id)
        )
        .unwrap(),
        manifest
    );
    let restored = f._worktrees.path().join("restored-source");
    crate::collaboration_archive::reconstruct(&store, &retained.result.snapshot_id, &restored)
        .unwrap();
    assert_eq!(
        std::fs::read(restored.join("independent.txt")).unwrap(),
        b"independent Local edit\n"
    );
}
