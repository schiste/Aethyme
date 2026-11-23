//! Parity harness: every in-process answer must equal the `git` program's
//! answer for the same query on the same repository, and every case the
//! module documents as unsupported must fall back instead of answering.

use super::*;
use crate::git::{GitRepo, ProgramRead};
use std::path::Path;

fn git(dir: &Path, args: &[&str]) -> String {
    let output = crate::git::git_command()
        .args([
            "-c",
            "user.name=Parity",
            "-c",
            "user.email=parity@example.invalid",
            "-c",
            "init.defaultBranch=main",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

fn commit(dir: &Path, file: &str, message: &str) -> String {
    std::fs::write(dir.join(file), message).unwrap();
    git(dir, &["add", file]);
    git(dir, &["commit", "-qm", message]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Shape {
    c1: String,
    c2: String,
    f1: String,
    blob: String,
}

/// `main`: c1 - c2, `feature` (packed, then moved loose): c1 - f1, tags of
/// every kind, a remote with a symbolic HEAD, a name that is both a tag and
/// a branch, and a dangling symbolic ref.
fn shaped(dir: &Path) -> Shape {
    git(dir, &["init", "-q"]);
    let c1 = commit(dir, "a.txt", "one");
    let c2 = commit(dir, "a.txt", "two");
    git(dir, &["branch", "feature", &c1]);
    git(dir, &["branch", "topic/x", &c2]);
    git(dir, &["tag", "-a", "-m", "annotated", "v1", &c2]);
    git(dir, &["tag", "lw", &c1]);
    git(dir, &["tag", "-a", "-m", "nested", "tagtag", "v1"]);
    git(dir, &["tag", "treetag", &format!("{c2}^{{tree}}")]);
    git(dir, &["update-ref", "refs/remotes/origin/main", &c2]);
    git(
        dir,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    git(dir, &["branch", "dup", &c1]);
    git(dir, &["tag", "dup", &c2]);
    git(
        dir,
        &["symbolic-ref", "refs/heads/dangling", "refs/heads/nope"],
    );
    git(dir, &["pack-refs", "--all"]);
    git(dir, &["switch", "-q", "feature"]);
    let f1 = commit(dir, "b.txt", "feature");
    git(dir, &["switch", "-q", "main"]);
    let blob = git(dir, &["rev-parse", &format!("{c2}:a.txt")]);
    Shape { c1, c2, f1, blob }
}

fn specs(shape: &Shape) -> Vec<String> {
    let mut specs = [
        "HEAD",
        "main",
        "refs/heads/main",
        "heads/main",
        "feature",
        "topic/x",
        "v1",
        "lw",
        "tagtag",
        "treetag",
        "origin/main",
        "origin",
        "refs/remotes/origin/HEAD",
        "dup",
        "dangling",
        "nope",
        "HEAD~1",
        "main^",
        "@",
        "FETCH_HEAD",
        "ORIG_HEAD",
        "",
        "0123456789abcdef0123456789abcdef01234567",
    ]
    .map(str::to_string)
    .to_vec();
    specs.extend([
        shape.c1.clone(),
        shape.c2.clone(),
        shape.f1.clone(),
        shape.c1[..12].to_string(),
        shape.c1.to_ascii_uppercase(),
        shape.blob.clone(),
    ]);
    specs
}

/// Compare every query on `root`; return how many were answered in-process.
fn assert_parity(root: &Path, specs: &[String]) -> usize {
    let program = ProgramRead { root };
    let mut answered = 0;
    // Every pair would be quadratic in Git processes; these four cover
    // ancestors, descendants, unrelated branches and an annotated tag.
    let ancestry = [
        "lw".to_string(),
        "feature".to_string(),
        "main".to_string(),
        "v1".to_string(),
    ];
    for spec in specs {
        let expected = program.resolve_commit(spec).ok();
        if let Ok(value) = in_process_only(root, ReadKind::ResolveCommit, |git| {
            git.resolve_commit(spec)
        }) {
            assert_eq!(Some(value), expected, "resolve_commit({spec:?})");
            answered += 1;
        }
        let expected = program.commit_tree(spec).ok();
        if let Ok(value) = in_process_only(root, ReadKind::CommitTree, |git| git.commit_tree(spec))
        {
            assert_eq!(Some(value), expected, "commit_tree({spec:?})");
            answered += 1;
        }
        for other in [&ancestry[0], &ancestry[1], &ancestry[2], &ancestry[3]] {
            let expected = program.is_ancestor(spec, other).ok();
            if let Ok(value) = in_process_only(root, ReadKind::IsAncestor, |git| {
                git.is_ancestor(spec, other)
            }) {
                assert_eq!(Some(value), expected, "is_ancestor({spec:?}, {other:?})");
                answered += 1;
            }
        }
    }
    let expected = program.head_commit().ok();
    if let Ok(value) = in_process_only(root, ReadKind::HeadCommit, |git| git.head_commit()) {
        assert_eq!(Some(value), expected, "head_commit");
        answered += 1;
    }
    answered
}

fn resolved(root: &Path, spec: &str) -> Result<String, Fallback> {
    in_process_only(root, ReadKind::ResolveCommit, |git| {
        git.resolve_commit(spec)
    })
}

#[test]
fn loose_packed_tags_and_symbolic_refs_match_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    assert!(assert_parity(dir.path(), &specs(&shape)) > 0);
    for spec in [
        "HEAD",
        "main",
        "refs/heads/main",
        "heads/main",
        "feature",
        "topic/x",
        "v1",
        "lw",
        "tagtag",
        "origin/main",
        "origin",
        "dup",
        shape.c1.as_str(),
    ] {
        assert!(resolved(dir.path(), spec).is_ok(), "{spec} is answered");
    }
    // The loose `feature` written after `pack-refs` wins over the packed one.
    assert_eq!(resolved(dir.path(), "feature").unwrap(), shape.f1);
    // Git's rule order puts tags before branches.
    assert_eq!(resolved(dir.path(), "dup").unwrap(), shape.c2);
    for (spec, reason) in [
        ("dangling", Fallback::Unresolved),
        ("nope", Fallback::NotFound),
        ("treetag", Fallback::NotACommit),
        ("HEAD~1", Fallback::UnsupportedSpec),
        ("@", Fallback::UnsupportedSpec),
        ("FETCH_HEAD", Fallback::UnsupportedSpec),
        (&shape.c1[..12], Fallback::UnsupportedSpec),
        (shape.blob.as_str(), Fallback::NotACommit),
        (
            "0123456789abcdef0123456789abcdef01234567",
            Fallback::ObjectMissing,
        ),
    ] {
        assert_eq!(resolved(dir.path(), spec), Err(reason), "{spec}");
    }
}

#[test]
fn a_file_in_the_git_directory_named_like_a_branch_is_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    std::fs::write(dir.path().join(".git/feature"), format!("{}\n", shape.c2)).unwrap();
    assert_eq!(
        resolved(dir.path(), "feature"),
        Err(Fallback::RootFileShadow)
    );
    assert_parity(dir.path(), &specs(&shape));
}

#[test]
fn linked_worktrees_read_their_own_head_and_the_shared_refs() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let linked = dir.path().join("linked");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "linked",
            linked.to_str().unwrap(),
            &shape.c1,
        ],
    );
    let linked_head = in_process_only(&linked, ReadKind::HeadCommit, |git| git.head_commit());
    assert_eq!(linked_head, Ok(shape.c1.clone()));
    assert!(assert_parity(&linked, &specs(&shape)) > 0);
}

#[test]
fn alternates_and_commit_graphs_give_the_same_answers() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let shape = shaped(&source);
    let shared = dir.path().join("shared");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--shared",
            source.to_str().unwrap(),
            shared.to_str().unwrap(),
        ],
    );
    assert!(shared.join(".git/objects/info/alternates").is_file());
    assert_eq!(resolved(&shared, &shape.c2), Ok(shape.c2.clone()));
    assert!(assert_parity(&shared, &specs(&shape)) > 0);

    git(&source, &["commit-graph", "write", "--reachable"]);
    let with_graph = assert_parity(&source, &specs(&shape));
    git(&source, &["config", "core.commitGraph", "false"]);
    assert_eq!(assert_parity(&source, &specs(&shape)), with_graph);
}

#[test]
fn shallow_and_partial_clones_never_answer_from_missing_history() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let shape = shaped(&source);
    git(&source, &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", source.display());

    let shallow = dir.path().join("shallow");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            &url,
            shallow.to_str().unwrap(),
        ],
    );
    assert_eq!(resolved(&shallow, "HEAD"), Ok(shape.c2.clone()));
    assert_eq!(
        in_process_only(&shallow, ReadKind::IsAncestor, |git| git
            .is_ancestor(&shape.c2, "HEAD")),
        Err(Fallback::HistoryOverride)
    );
    assert_parity(&shallow, &specs(&shape));

    // The source's `treetag` names a tree directly, which a tree-less clone
    // must send; clone only `main`, without tags.
    let treeless = dir.path().join("treeless");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--no-checkout",
            "--filter=tree:0",
            "--single-branch",
            "--no-tags",
            &url,
            treeless.to_str().unwrap(),
        ],
    );
    assert_eq!(
        in_process_only(&treeless, ReadKind::CommitTree, |git| git
            .commit_tree(&shape.c1)),
        Err(Fallback::ObjectMissing),
        "a missing tree is unknown, and is never fetched"
    );
    let blobless = dir.path().join("blobless");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--no-checkout",
            "--filter=blob:none",
            "--single-branch",
            "--no-tags",
            &url,
            blobless.to_str().unwrap(),
        ],
    );
    assert_eq!(
        resolved(&blobless, &shape.blob),
        Err(Fallback::ObjectMissing)
    );
    assert_eq!(resolved(&blobless, "origin/main"), Ok(shape.c2.clone()));
}

#[test]
fn grafts_and_replace_refs_are_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    git(dir.path(), &["replace", &shape.c2, &shape.f1]);
    assert_eq!(resolved(dir.path(), "main"), Err(Fallback::HistoryOverride));
    git(dir.path(), &["replace", "-d", &shape.c2]);
    assert_eq!(resolved(dir.path(), "main"), Ok(shape.c2.clone()));

    std::fs::create_dir_all(dir.path().join(".git/info")).unwrap();
    std::fs::write(
        dir.path().join(".git/info/grafts"),
        format!("{}\n", shape.c2),
    )
    .unwrap();
    assert_eq!(resolved(dir.path(), "main"), Err(Fallback::HistoryOverride));
}

#[test]
fn sha256_repositories_are_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "--object-format=sha256"]);
    let head = commit(dir.path(), "a.txt", "one");
    assert_eq!(head.len(), 64);
    let answer = resolved(dir.path(), "HEAD");
    assert!(answer.is_err(), "{answer:?}");
    let repo = GitRepo::discover(dir.path()).unwrap();
    assert_eq!(repo.resolve_ref("HEAD"), Some(head.clone()));
    assert_eq!(repo.head_commit().unwrap(), head);
}

#[test]
fn configuration_includes_are_read_like_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    std::fs::write(
        dir.path().join(".git/extra.cfg"),
        "[core]\n\tcommitGraph = false\n",
    )
    .unwrap();
    git(dir.path(), &["config", "include.path", "extra.cfg"]);
    assert!(assert_parity(dir.path(), &specs(&shape)) > 0);
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_repository_paths_are_supported() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir
        .path()
        .join(std::ffi::OsStr::from_bytes(b"repo-\xff-latin1"));
    std::fs::create_dir(&root).unwrap();
    let shape = shaped(&root);
    assert_eq!(resolved(&root, "main"), Ok(shape.c2.clone()));
    assert!(assert_parity(&root, &specs(&shape)) > 0);
}

#[test]
fn a_ref_rewritten_during_reads_is_never_absent() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    git(dir.path(), &["update-ref", "refs/heads/race", &shape.c1]);
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let writer = {
        let stop = stop.clone();
        let root = dir.path().to_path_buf();
        let values = [shape.c1.clone(), shape.c2.clone()];
        std::thread::spawn(move || {
            let mut round = 0_usize;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                git(
                    &root,
                    &["update-ref", "refs/heads/race", &values[round % 2]],
                );
                if round.is_multiple_of(3) {
                    git(&root, &["pack-refs", "--all", "--prune"]);
                }
                round += 1;
            }
        })
    };
    let repo = GitRepo::discover(dir.path()).unwrap();
    for _ in 0..150 {
        if let Ok(value) = resolved(dir.path(), "race") {
            assert!(value == shape.c1 || value == shape.c2, "{value}");
        }
        let value = repo.resolve_ref("race").expect("never absent");
        assert!(value == shape.c1 || value == shape.c2, "{value}");
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    writer.join().unwrap();
}

#[test]
fn the_configuration_kill_switch_reaches_linked_worktrees() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let linked = dir.path().join("linked");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            &shape.c1,
        ],
    );
    assert!(resolved(&linked, "main").is_ok());
    std::fs::create_dir(dir.path().join(".aethyme")).unwrap();
    std::fs::write(
        dir.path().join(".aethyme/config.toml"),
        "[git]\nin_process = false\n",
    )
    .unwrap();
    assert_eq!(resolved(dir.path(), "main"), Err(Fallback::Disabled));
    assert_eq!(resolved(&linked, "main"), Err(Fallback::Disabled));
    std::fs::write(
        dir.path().join(".aethyme/config.toml"),
        "[git]\nin_process = true\n",
    )
    .unwrap();
    assert!(resolved(&linked, "main").is_ok());
}

#[test]
fn kill_switch_values() {
    for value in ["0", "false", "OFF", " no ", "", "disabled", "2"] {
        assert!(disabled_by_environment(Some(value)), "{value:?} is off");
    }
    for value in [None, Some("1"), Some("true"), Some(" ON "), Some("yes")] {
        assert!(!disabled_by_environment(value), "{value:?}");
    }
    let parse = |text: &str| in_process_off(&text.parse::<toml::Value>().unwrap());
    assert!(!parse(""));
    assert!(!parse("[git]\n"));
    assert!(!parse("[git]\nin_process = true\n"));
    assert!(parse("[git]\nin_process = false\n"));
    assert!(
        parse("[git]\nin_process = \"no\"\n"),
        "an invalid value is off"
    );
}

#[test]
fn answers_and_fallbacks_are_counted_and_replace_git_processes() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let repo = GitRepo::discover(dir.path()).unwrap();
    let recorder = crate::phase_telemetry::Recorder::default();
    {
        let _installed = crate::phase_telemetry::install(recorder.clone());
        assert_eq!(repo.resolve_ref("main"), Some(shape.c2.clone()));
        assert!(repo.is_ancestor_checked(&shape.c1, "main").unwrap());
        assert_eq!(repo.resolve_ref("nope"), None);
    }
    let counts = recorder.totals().1;
    assert_eq!(counts.get("subprocess_git"), Some(&1), "{counts:?}");
    let reads = recorder.attachments().remove("git_read").unwrap();
    assert_eq!(reads["resolve_commit.answered"]["count"], 1, "{reads}");
    assert_eq!(reads["is_ancestor.answered"]["count"], 1, "{reads}");
    assert_eq!(
        reads["resolve_commit.fallback.not_found"]["count"], 1,
        "{reads}"
    );
}

#[test]
fn spec_classification() {
    for spec in [
        "main",
        "refs/heads/main",
        "topic/x",
        "v1.2.3",
        "a-b_c",
        "HEAD",
    ] {
        assert_eq!(classify(spec), Ok(Spec::Name(spec)), "{spec}");
    }
    for spec in [
        "",
        "HEAD~1",
        "main^{tree}",
        "@{u}",
        "a..b",
        "a/.hidden",
        "a.lock",
        "a/",
        "/a",
        "a//b",
        "-x",
        "trailing.",
        "FETCH_HEAD",
        "MERGE_HEAD",
        "cafe",
        "deadbeef",
        "refs/worktree/x",
        "worktree/x",
        "bisect/bad",
        "main-worktree/HEAD",
        "worktrees/a/HEAD",
        "with space",
        "caf\u{e9}",
    ] {
        assert_eq!(classify(spec), Err(Fallback::UnsupportedSpec), "{spec:?}");
    }
    assert!(matches!(
        classify("0123456789abcdef0123456789abcdef01234567"),
        Ok(Spec::Id(_))
    ));
    assert_eq!(
        classify("0123456789ABCDEF0123456789abcdef01234567"),
        Err(Fallback::UnsupportedSpec)
    );
    assert_eq!(candidates("x")[5], "refs/remotes/x/HEAD");
}

fn symbolic_chain(dir: &Path, prefix: &str, hops: usize) -> String {
    let mut target = "refs/heads/main".to_string();
    for hop in 0..hops {
        let name = format!("refs/heads/{prefix}{hop}");
        git(dir, &["symbolic-ref", &name, &target]);
        target = name;
    }
    target.trim_start_matches("refs/heads/").to_string()
}

#[test]
fn symbolic_chains_longer_than_git_follows_are_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let four = symbolic_chain(dir.path(), "four", 4);
    let five = symbolic_chain(dir.path(), "five", 5);
    let six = symbolic_chain(dir.path(), "six", 6);
    assert_eq!(resolved(dir.path(), &four), Ok(shape.c2.clone()));
    assert_eq!(resolved(dir.path(), &five), Err(Fallback::Unresolved));
    assert_eq!(resolved(dir.path(), &six), Err(Fallback::Unresolved));
    assert!(assert_parity(dir.path(), &[four, five, six]) > 0);
}

#[test]
fn a_packed_ref_deleted_after_open_is_never_answered() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    git(
        dir.path(),
        &["update-ref", "refs/heads/packed-only", &shape.c1],
    );
    git(dir.path(), &["pack-refs", "--all", "--prune"]);
    let reader = InProcess::open(dir.path(), ReadKind::ResolveCommit).unwrap();
    git(dir.path(), &["update-ref", "-d", "refs/heads/packed-only"]);
    let answer = reader.guarded(|git| git.resolve_commit("packed-only"));
    assert_eq!(answer, Err(Fallback::NotFound));
}

#[test]
fn loose_refs_git_treats_as_broken_are_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    std::fs::write(
        dir.path().join(".git/refs/heads/garbled"),
        format!("{}garbage\n", shape.c1),
    )
    .unwrap();
    std::fs::write(
        dir.path().join(".git/refs/heads/spaced"),
        format!("{} trailing\n", shape.c1),
    )
    .unwrap();
    assert_eq!(resolved(dir.path(), "garbled"), Err(Fallback::BrokenRef));
    assert_eq!(resolved(dir.path(), "spaced"), Ok(shape.c1.clone()));
    assert_parity(dir.path(), &["garbled".to_string(), "spaced".to_string()]);
}

#[test]
fn a_tag_whose_type_disagrees_with_its_target_is_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let body = format!(
        "object {}\ntype tree\ntag liar\ntagger Parity <parity@example.invalid> 0 +0000\n\nliar\n",
        shape.c1
    );
    let body_path = dir.path().join("liar.tag");
    std::fs::write(&body_path, body).unwrap();
    let tag = git(
        dir.path(),
        &[
            "hash-object",
            "-t",
            "tag",
            "-w",
            "--literally",
            body_path.to_str().unwrap(),
        ],
    );
    git(dir.path(), &["update-ref", "refs/tags/liar", &tag]);
    assert_eq!(resolved(dir.path(), "liar"), Err(Fallback::TagTypeMismatch));
    assert_parity(dir.path(), &["liar".to_string()]);
}

#[test]
fn ancestry_under_a_git_deadline_is_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let repo = GitRepo::discover(dir.path()).unwrap();
    let recorder = crate::phase_telemetry::Recorder::default();
    {
        let _installed = crate::phase_telemetry::install(recorder.clone());
        let answer = crate::git::with_git_deadline(std::time::Duration::from_secs(600), || {
            repo.is_ancestor_checked(&shape.c1, "main")
        });
        assert!(answer.unwrap());
    }
    let reads = recorder.attachments().remove("git_read").unwrap();
    assert_eq!(
        reads["is_ancestor.fallback.deadline"]["count"], 1,
        "{reads}"
    );
}

#[test]
fn only_known_extensions_are_read_in_process() {
    let dir = tempfile::tempdir().unwrap();
    let check = |text: &str| {
        std::fs::write(dir.path().join("config"), text).unwrap();
        known_extensions_only(dir.path())
    };
    assert!(check("[core]\n\trepositoryformatversion = 0\n"));
    assert!(check(
        "[core]\n\trepositoryformatversion = 1\n[extensions]\n\tworktreeConfig = true\n\tpartialclone = origin\n"
    ));
    assert!(!check("[extensions]\n\tfutureThing = true\n"));
    assert!(!check(
        "[Extensions]\n\tobjectformat = sha1\n\tsubmodulePathConfig = true\n"
    ));
    assert!(!check("[extensions \"x\"]\n\tnoop = true\n"));
    assert!(!check("[extensions]\n\tnoop = a \\\n"));
    assert!(!known_extensions_only(&dir.path().join("missing")));
}

#[test]
fn a_missing_parent_never_truncates_ancestry_into_a_no() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    // Fresh commits are loose: remove c1, the parent of both c2 and f1.
    let object = dir
        .path()
        .join(".git/objects")
        .join(&shape.c1[..2])
        .join(&shape.c1[2..]);
    std::fs::remove_file(object).unwrap();
    let ancestry = |ancestor: &str, descendant: &str| {
        in_process_only(dir.path(), ReadKind::IsAncestor, |git| {
            git.is_ancestor(ancestor, descendant)
        })
    };
    assert_eq!(ancestry("feature", "main"), Err(Fallback::ObjectMissing));
    assert_eq!(ancestry(&shape.f1, &shape.c2), Err(Fallback::ObjectMissing));
    assert_eq!(ancestry(&shape.c2, &shape.c2), Ok(true));
}

#[test]
fn ancestry_in_a_promisor_repository_is_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    git(dir.path(), &["config", "core.repositoryformatversion", "1"]);
    git(dir.path(), &["config", "extensions.partialClone", "origin"]);
    assert_eq!(
        in_process_only(dir.path(), ReadKind::IsAncestor, |git| git
            .is_ancestor(&shape.c1, &shape.c2)),
        Err(Fallback::ObjectMissing)
    );
    assert_eq!(resolved(dir.path(), "main"), Ok(shape.c2.clone()));
}

// Committed-configuration reads: symbolic refs, the remote default branch,
// tree entries, blobs and files at a revision.

fn symbolic(root: &Path, name: &str) -> Result<Option<String>, Fallback> {
    in_process_only(root, ReadKind::SymbolicRef, |git| git.symbolic_ref(name))
}

/// Compare `symbolic-ref --quiet` for every name; return how many were
/// answered in-process.
fn assert_symbolic_parity(root: &Path, names: &[&str]) -> usize {
    let program = ProgramRead { root };
    let mut answered = 0;
    for name in names {
        if let Ok(value) = symbolic(root, name) {
            let expected = program.symbolic_ref(name);
            assert!(
                matches!(&expected, Ok(found) if *found == value),
                "symbolic_ref({name:?}): {value:?} in-process, {expected:?} from Git"
            );
            answered += 1;
        }
    }
    answered
}

#[test]
fn symbolic_refs_match_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let four = format!("refs/heads/{}", symbolic_chain(dir.path(), "four", 4));
    let five = format!("refs/heads/{}", symbolic_chain(dir.path(), "five", 5));
    let names = [
        "HEAD",
        "refs/remotes/origin/HEAD",
        "refs/heads/main",
        "refs/heads/dangling",
        "refs/heads/nope",
        "refs/tags/v1",
        four.as_str(),
        five.as_str(),
        "main",
        "FETCH_HEAD",
        "refs/worktree/x",
    ];
    assert!(assert_symbolic_parity(dir.path(), &names) >= 5);
    assert_eq!(
        symbolic(dir.path(), "HEAD"),
        Ok(Some("refs/heads/main".into()))
    );
    assert_eq!(
        symbolic(dir.path(), "refs/remotes/origin/HEAD"),
        Ok(Some("refs/remotes/origin/main".into()))
    );
    // Git prints the ref a chain ends at, not the first hop.
    assert_eq!(
        symbolic(dir.path(), &four),
        Ok(Some("refs/heads/main".into()))
    );
    assert_eq!(symbolic(dir.path(), "refs/heads/main"), Ok(None));
    assert_eq!(symbolic(dir.path(), "refs/tags/v1"), Ok(None));
    for (name, reason) in [
        ("refs/heads/dangling", Fallback::Unresolved),
        ("refs/heads/nope", Fallback::NotFound),
        (five.as_str(), Fallback::Unresolved),
        ("main", Fallback::UnsupportedSpec),
        ("FETCH_HEAD", Fallback::UnsupportedSpec),
        ("refs/worktree/x", Fallback::UnsupportedSpec),
    ] {
        assert_eq!(symbolic(dir.path(), name), Err(reason), "{name}");
    }

    // An unborn branch: HEAD names a ref that does not exist yet.
    let unborn = dir.path().join("unborn");
    std::fs::create_dir(&unborn).unwrap();
    git(&unborn, &["init", "-q"]);
    assert_eq!(symbolic(&unborn, "HEAD"), Err(Fallback::Unresolved));
    assert_symbolic_parity(&unborn, &["HEAD"]);

    // A detached HEAD is not symbolic; a linked worktree reads its own HEAD.
    let detached = dir.path().join("detached");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            detached.to_str().unwrap(),
            &shape.c1,
        ],
    );
    assert_eq!(symbolic(&detached, "HEAD"), Ok(None));
    let attached = dir.path().join("attached");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "attached",
            attached.to_str().unwrap(),
            &shape.c1,
        ],
    );
    assert_eq!(
        symbolic(&attached, "HEAD"),
        Ok(Some("refs/heads/attached".into()))
    );
    for root in [&detached, &attached] {
        assert!(assert_symbolic_parity(root, &names) >= 4);
    }
}

fn origin_default(root: &Path) -> Result<Option<(String, String)>, Fallback> {
    in_process_only(root, ReadKind::OriginHeadDefault, |git| {
        git.origin_head_default()
    })
}

/// The in-process answer, when there is one, equals the program's.
fn assert_origin_parity(root: &Path) {
    if let Ok(value) = origin_default(root) {
        let expected = ProgramRead { root }.origin_head_default();
        assert!(
            matches!(&expected, Ok(found) if *found == value),
            "origin_head_default: {value:?} in-process, {expected:?} from Git"
        );
    }
}

#[test]
fn the_remote_default_branch_matches_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let shape = shaped(root);
    let program = ProgramRead { root };
    // `origin/main` is packed; `origin/HEAD` is a loose symbolic ref.
    assert_eq!(
        origin_default(root),
        Ok(Some(("origin/main".into(), shape.c2.clone())))
    );
    assert_origin_parity(root);
    git(root, &["update-ref", "refs/remotes/origin/main", &shape.f1]);
    assert_eq!(
        origin_default(root),
        Ok(Some(("origin/main".into(), shape.f1.clone())))
    );
    assert_origin_parity(root);

    // A tag at the target is peeled by Git's checked path: left to it.
    let tag = git(root, &["rev-parse", "refs/tags/v1"]);
    git(root, &["update-ref", "refs/remotes/origin/tagged", &tag]);
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/tagged",
        ],
    );
    assert_eq!(origin_default(root), Err(Fallback::NotACommit));
    assert_eq!(
        program.origin_head_default().unwrap(),
        Some(("origin/tagged".into(), shape.c2.clone()))
    );

    // A target outside the origin remote is an error Git reports.
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/heads/main",
        ],
    );
    assert_eq!(origin_default(root), Err(Fallback::UnexpectedTarget));
    assert!(program.origin_head_default().is_err());

    // Dangling.
    git(
        root,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/gone",
        ],
    );
    assert_eq!(origin_default(root), Err(Fallback::Unresolved));
    assert!(program.origin_head_default().is_err());

    // Not symbolic.
    git(
        root,
        &[
            "update-ref",
            "--no-deref",
            "refs/remotes/origin/HEAD",
            &shape.c1,
        ],
    );
    assert_eq!(origin_default(root), Err(Fallback::UnexpectedTarget));
    assert!(program.origin_head_default().is_err());

    // Absent: Git proves the absence.
    git(
        root,
        &["update-ref", "-d", "--no-deref", "refs/remotes/origin/HEAD"],
    );
    assert_eq!(origin_default(root), Err(Fallback::NotFound));
    assert_eq!(program.origin_head_default().unwrap(), None);
}

/// A commit whose tree holds every entry shape a configuration read meets.
struct TreeShape {
    commit: String,
    config_blob: String,
    big_blob: String,
    tree: String,
}

const BIG_BYTES: usize = 5 * 1024 * 1024;

fn tree_shaped(dir: &Path) -> TreeShape {
    let shape = shaped(dir);
    let aethyme = dir.join(".aethyme");
    std::fs::create_dir_all(aethyme.join("prepare.toml")).unwrap();
    std::fs::write(aethyme.join("config.toml"), "[git]\nin_process = true\n").unwrap();
    std::fs::write(aethyme.join("c[o]nfig.toml"), "literal = true\n").unwrap();
    std::fs::write(aethyme.join("gates.toml"), "[gates]\n").unwrap();
    std::fs::write(aethyme.join("prepare.toml/inner"), "inner\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("config.toml", aethyme.join("broker.toml")).unwrap();
    std::fs::create_dir_all(dir.join("a/b")).unwrap();
    std::fs::write(dir.join("a/b/c.txt"), "deep\n\n  \n").unwrap();
    std::fs::write(dir.join("plain"), "plain\n").unwrap();
    std::fs::write(dir.join("latin1.txt"), b"caf\xe9 \xff\xfe end\r\n\t\n").unwrap();
    std::fs::write(dir.join("big.bin"), vec![b'x'; BIG_BYTES]).unwrap();
    git(
        dir,
        &["add", ".aethyme", "a", "plain", "latin1.txt", "big.bin"],
    );
    git(dir, &["update-index", "--chmod=+x", ".aethyme/gates.toml"]);
    git(
        dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},.aethyme/environments.toml", shape.c1),
        ],
    );
    git(dir, &["commit", "-qm", "configuration"]);
    let commit = git(dir, &["rev-parse", "HEAD"]);
    TreeShape {
        config_blob: git(dir, &["rev-parse", "HEAD:.aethyme/config.toml"]),
        big_blob: git(dir, &["rev-parse", "HEAD:big.bin"]),
        tree: git(dir, &["rev-parse", "HEAD:a"]),
        commit,
    }
}

const STEMS: [&str; 7] = [
    "config",
    "gates",
    "c[o]nfig",
    "prepare",
    "broker",
    "environments",
    "missing",
];

const PATHS: [&str; 18] = [
    ".aethyme/config.toml",
    ".aethyme/gates.toml",
    ".aethyme/c[o]nfig.toml",
    ".aethyme/prepare.toml",
    ".aethyme/broker.toml",
    ".aethyme/environments.toml",
    ".aethyme/environments.toml/x",
    ".aethyme",
    "a/b/c.txt",
    "a/b",
    "a/missing",
    "missing/deep/x",
    "plain/below",
    "latin1.txt",
    "big.bin",
    "./a/b/c.txt",
    "a//b/c.txt",
    "a/b/c.txt/",
];

/// Compare every committed-configuration read on `root` at each revision;
/// return how many were answered in-process.
fn assert_tree_parity(root: &Path, revisions: &[&str], blobs: &[&str]) -> usize {
    let program = ProgramRead { root };
    let mut answered = 0;
    for revision in revisions {
        for stem in STEMS {
            let paths = aethyme_config::candidate_relpaths(stem).to_vec();
            if let Ok(value) = in_process_only(root, ReadKind::ConfigTreeEntries, |git| {
                git.config_tree_entries(revision, &paths)
            }) {
                let expected = program.config_tree_entries(revision, &paths);
                assert!(
                    matches!(&expected, Ok(found) if *found == value),
                    "config_tree_entries({revision:?}, {stem:?}): {value:?} vs {expected:?}"
                );
                answered += 1;
            }
        }
        for path in PATHS {
            if let Ok(value) = in_process_only(root, ReadKind::FileAtRevision, |git| {
                git.file_at_revision(revision, path)
            }) {
                let expected = program.file_at_revision(revision, path);
                assert!(
                    matches!(&expected, Ok(found) if *found == value),
                    "file_at_revision({revision:?}, {path:?}): {value:?} vs {expected:?}"
                );
                answered += 1;
            }
            if let Ok(value) = in_process_only(root, ReadKind::PathExistsAt, |git| {
                git.path_exists_at(revision, path)
            }) {
                let expected = program.path_exists_at(revision, path);
                assert!(
                    matches!(&expected, Ok(found) if *found == value),
                    "path_exists_at({revision:?}, {path:?}): {value:?} vs {expected:?}"
                );
                answered += 1;
            }
        }
    }
    for blob in blobs {
        for limit in [64, 4 * 1024 * 1024] {
            if let Ok(value) =
                in_process_only(root, ReadKind::BlobBytes, |git| git.blob_bytes(blob, limit))
            {
                let expected = program.blob_bytes(blob, limit);
                assert!(
                    matches!(&expected, Ok(found) if *found == value),
                    "blob_bytes({blob:?}, {limit})"
                );
                answered += 1;
            }
        }
    }
    answered
}

#[test]
fn tree_entries_files_and_blobs_match_the_program() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let tree = tree_shaped(root);
    let missing = "0123456789abcdef0123456789abcdef01234567";
    let revisions = [tree.commit.as_str(), "HEAD", "main", "HEAD~1", "nope"];
    let blobs = [
        tree.config_blob.as_str(),
        tree.big_blob.as_str(),
        tree.tree.as_str(),
        tree.commit.as_str(),
        missing,
        "HEAD:.aethyme/config.toml",
    ];
    assert!(assert_tree_parity(root, &revisions, &blobs) > 0);

    let entries = |stem: &str| {
        let paths = aethyme_config::candidate_relpaths(stem).to_vec();
        in_process_only(root, ReadKind::ConfigTreeEntries, |git| {
            git.config_tree_entries(&tree.commit, &paths)
        })
    };
    assert_eq!(
        entries("config"),
        Ok(BTreeMap::from([(
            ".aethyme/config.toml".to_string(),
            tree.config_blob.clone()
        )]))
    );
    assert_eq!(entries("gates").map(|found| found.len()), Ok(1));
    assert_eq!(entries("c[o]nfig").map(|found| found.len()), Ok(1));
    assert_eq!(entries("missing"), Ok(BTreeMap::new()));
    for stem in ["prepare", "environments"] {
        assert_eq!(entries(stem), Err(Fallback::NotARegularFile), "{stem}");
    }
    #[cfg(unix)]
    assert_eq!(entries("broker"), Err(Fallback::NotARegularFile));

    let file = |path: &str| {
        in_process_only(root, ReadKind::FileAtRevision, |git| {
            git.file_at_revision(&tree.commit, path)
        })
    };
    assert_eq!(
        file(".aethyme/config.toml"),
        Ok(Some("[git]\nin_process = true".into()))
    );
    #[cfg(unix)]
    assert_eq!(file(".aethyme/broker.toml"), Ok(Some("config.toml".into())));
    assert_eq!(file("a/b/c.txt"), Ok(Some("deep".into())));
    assert_eq!(
        file("latin1.txt"),
        Ok(Some("caf\u{fffd} \u{fffd}\u{fffd} end".into()))
    );
    assert_eq!(
        file("big.bin").map(|found| found.unwrap().len()),
        Ok(BIG_BYTES)
    );
    for path in ["a/missing", "missing/deep/x", ".aethyme/nothing.toml"] {
        assert_eq!(file(path), Ok(None), "{path}");
    }
    for (path, reason) in [
        ("a/b", Fallback::NotARegularFile),
        (".aethyme/prepare.toml", Fallback::NotARegularFile),
        (".aethyme/environments.toml", Fallback::NotARegularFile),
        ("plain/below", Fallback::PathThroughNonTree),
        (".aethyme/environments.toml/x", Fallback::PathThroughNonTree),
        ("./a/b/c.txt", Fallback::UnsupportedPath),
        ("a//b/c.txt", Fallback::UnsupportedPath),
        ("a/b/c.txt/", Fallback::UnsupportedPath),
        ("", Fallback::UnsupportedPath),
    ] {
        assert_eq!(file(path), Err(reason), "{path:?}");
    }

    let exists = |revision: &str, path: &str| {
        in_process_only(root, ReadKind::PathExistsAt, |git| {
            git.path_exists_at(revision, path)
        })
    };
    assert_eq!(exists("HEAD", "a/b"), Ok(true));
    assert_eq!(exists("main", ".aethyme/config.toml"), Ok(true));
    assert_eq!(exists("v1", ".aethyme/config.toml"), Ok(false));
    assert_eq!(exists("HEAD", "a/missing"), Ok(false));
    assert_eq!(
        exists("HEAD", ".aethyme/environments.toml"),
        Err(Fallback::NotARegularFile)
    );
    assert_eq!(exists("HEAD~1", "a/b"), Err(Fallback::UnsupportedSpec));
    assert_eq!(exists("nope", "a/b"), Err(Fallback::NotFound));

    let blob = |object: &str, limit: usize| {
        in_process_only(root, ReadKind::BlobBytes, |git| {
            git.blob_bytes(object, limit)
        })
    };
    assert_eq!(
        blob(&tree.config_blob, 4096),
        Ok(b"[git]\nin_process = true\n".to_vec())
    );
    assert_eq!(
        blob(&tree.big_blob, 4 * 1024 * 1024),
        Err(Fallback::TooLarge)
    );
    assert!(
        ProgramRead { root }
            .blob_bytes(&tree.big_blob, 4 * 1024 * 1024)
            .is_err(),
        "Git reports the limit"
    );
    assert_eq!(blob(&tree.tree, 4096), Err(Fallback::NotABlob));
    assert_eq!(blob(&tree.commit, 4096), Err(Fallback::NotABlob));
    assert_eq!(blob(missing, 4096), Err(Fallback::ObjectMissing));
    assert_eq!(
        blob("HEAD:.aethyme/config.toml", 4096),
        Err(Fallback::UnsupportedSpec)
    );
}

#[test]
fn committed_configuration_reads_in_linked_worktrees_and_alternates() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let tree = tree_shaped(&source);
    let blobs = [tree.config_blob.as_str()];
    let revisions = [tree.commit.as_str(), "main"];

    let linked = dir.path().join("linked");
    git(
        &source,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            linked.to_str().unwrap(),
            &tree.commit,
        ],
    );
    assert!(assert_tree_parity(&linked, &revisions, &blobs) > 0);
    assert_eq!(symbolic(&linked, "HEAD"), Ok(None));
    assert_origin_parity(&linked);

    let shared = dir.path().join("shared");
    git(
        dir.path(),
        &[
            "clone",
            "-q",
            "--shared",
            source.to_str().unwrap(),
            shared.to_str().unwrap(),
        ],
    );
    assert!(shared.join(".git/objects/info/alternates").is_file());
    assert!(assert_tree_parity(&shared, &revisions, &blobs) > 0);
    // The clone's own `origin/HEAD` names the source's `main`.
    assert_eq!(
        origin_default(&shared),
        Ok(Some(("origin/main".into(), tree.commit.clone())))
    );
    assert_origin_parity(&shared);
    assert_eq!(
        assert_symbolic_parity(&shared, &["HEAD", "refs/remotes/origin/HEAD"]),
        2
    );
}

#[test]
fn partial_clones_never_answer_from_a_missing_blob_or_tree() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    std::fs::create_dir(&source).unwrap();
    let tree = tree_shaped(&source);
    git(&source, &["config", "uploadpack.allowFilter", "true"]);
    let url = format!("file://{}", source.display());
    let clone = |name: &str, filter: &str| {
        let target = dir.path().join(name);
        git(
            dir.path(),
            &[
                "clone",
                "-q",
                "--no-checkout",
                filter,
                "--single-branch",
                "--no-tags",
                &url,
                target.to_str().unwrap(),
            ],
        );
        target
    };
    let paths = aethyme_config::candidate_relpaths("config").to_vec();

    let blobless = clone("blobless", "--filter=blob:none");
    // Listing a tree reads no blob: answered, and equal to Git's listing.
    let listed = in_process_only(&blobless, ReadKind::ConfigTreeEntries, |git| {
        git.config_tree_entries(&tree.commit, &paths)
    });
    assert_eq!(
        listed,
        ProgramRead { root: &blobless }
            .config_tree_entries(&tree.commit, &paths)
            .map_err(|_| Fallback::Gix)
    );
    assert_eq!(
        in_process_only(&blobless, ReadKind::FileAtRevision, |git| git
            .file_at_revision(&tree.commit, ".aethyme/config.toml")),
        Err(Fallback::ObjectMissing),
        "a missing blob is unknown, and is never fetched"
    );
    assert_eq!(
        in_process_only(&blobless, ReadKind::PathExistsAt, |git| git
            .path_exists_at(&tree.commit, ".aethyme/config.toml")),
        Err(Fallback::ObjectMissing)
    );
    assert_eq!(
        in_process_only(&blobless, ReadKind::BlobBytes, |git| git
            .blob_bytes(&tree.config_blob, 4096)),
        Err(Fallback::ObjectMissing)
    );
    let treeless = clone("treeless", "--filter=tree:0");
    assert_eq!(
        in_process_only(&treeless, ReadKind::ConfigTreeEntries, |git| git
            .config_tree_entries(&tree.commit, &paths)),
        Err(Fallback::ObjectMissing)
    );
    assert_eq!(
        in_process_only(&treeless, ReadKind::PathExistsAt, |git| git
            .path_exists_at(&tree.commit, "a/missing")),
        Err(Fallback::ObjectMissing),
        "an absence read from a missing tree is unknown"
    );
}

/// A tree object written byte for byte, bypassing Git's own sorting.
fn raw_tree(dir: &Path, entries: &[(&str, &str, &str)]) -> String {
    let mut body = Vec::new();
    for (mode, name, id) in entries {
        body.extend_from_slice(format!("{mode} {name}\0").as_bytes());
        body.extend(
            (0..id.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&id[at..at + 2], 16).unwrap()),
        );
    }
    let path = dir.join("raw.tree");
    std::fs::write(&path, body).unwrap();
    let tree = git(
        dir,
        &[
            "hash-object",
            "-t",
            "tree",
            "-w",
            "--literally",
            path.to_str().unwrap(),
        ],
    );
    git(dir, &["commit-tree", &tree, "-m", "raw"])
}

#[test]
fn trees_git_could_read_differently_are_left_to_git() {
    let dir = tempfile::tempdir().unwrap();
    let shape = shaped(dir.path());
    let file = |commit: &str, path: &str| {
        in_process_only(dir.path(), ReadKind::FileAtRevision, |git| {
            git.file_at_revision(commit, path)
        })
    };
    let sorted = raw_tree(
        dir.path(),
        &[("100644", "a", &shape.blob), ("100644", "b", &shape.blob)],
    );
    assert_eq!(file(&sorted, "b"), Ok(Some("two".into())));
    let unsorted = raw_tree(
        dir.path(),
        &[("100644", "b", &shape.blob), ("100644", "a", &shape.blob)],
    );
    assert_eq!(file(&unsorted, "a"), Err(Fallback::MalformedTree));
    let repeated = raw_tree(
        dir.path(),
        &[("100644", "a", &shape.blob), ("100644", "a", &shape.blob)],
    );
    assert_eq!(file(&repeated, "a"), Err(Fallback::MalformedTree));
    let odd_mode = raw_tree(dir.path(), &[("100664", "a", &shape.blob)]);
    let paths = vec!["a".to_string()];
    assert_eq!(
        in_process_only(dir.path(), ReadKind::ConfigTreeEntries, |git| git
            .config_tree_entries(&odd_mode, &paths)),
        Err(Fallback::NotARegularFile),
        "Git prints a canonical mode"
    );
}

#[test]
fn git_tree_order_sorts_trees_as_if_they_ended_in_a_slash() {
    use std::cmp::Ordering::{Equal, Greater, Less};
    assert_eq!(git_tree_order(b"a", false, b"a.b", false), Less);
    assert_eq!(git_tree_order(b"a.b", false, b"a", true), Less);
    assert_eq!(git_tree_order(b"a", true, b"a0", false), Less);
    assert_eq!(git_tree_order(b"a", false, b"a", true), Less);
    assert_eq!(git_tree_order(b"b", false, b"a", false), Greater);
    assert_eq!(git_tree_order(b"a", true, b"a", true), Equal);
}

#[test]
fn committed_configuration_reads_honor_the_kill_switch_and_are_counted() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let tree = tree_shaped(root);
    let repo = GitRepo::discover(root).unwrap();
    let read_everything = || {
        assert_eq!(repo.symbolic_ref("HEAD"), Some("refs/heads/main".into()));
        assert_eq!(
            repo.origin_head_default_checked().unwrap(),
            Some((
                "origin/main".into(),
                git(root, &["rev-parse", "origin/main"])
            ))
        );
        let config = repo
            .config_at_resolved_commit_checked(&tree.commit, "config")
            .unwrap()
            .unwrap();
        assert_eq!(config.text, "[git]\nin_process = true\n");
        assert_eq!(
            repo.config_toml_at_commit(&tree.commit, "config").unwrap(),
            Some("[git]\nin_process = true".into())
        );
        assert!(repo.path_exists_at("main", "a/b/c.txt").unwrap());
        assert_eq!(
            repo.file_at_commit(&tree.commit, ".aethyme/prepare.toml")
                .unwrap(),
            Some(git(
                root,
                &["show", &format!("{}:.aethyme/prepare.toml", tree.commit)]
            ))
        );
    };
    let recorder = crate::phase_telemetry::Recorder::default();
    {
        let _installed = crate::phase_telemetry::install(recorder.clone());
        read_everything();
    }
    let reads = recorder.attachments().remove("git_read").unwrap();
    for key in [
        "symbolic_ref.answered",
        "origin_head_default.answered",
        "config_tree_entries.answered",
        "blob_bytes.answered",
        "file_at_revision.answered",
        "path_exists_at.answered",
        "file_at_revision.fallback.not_a_regular_file",
    ] {
        assert!(
            reads[key]["count"].as_u64().unwrap_or(0) >= 1,
            "{key}: {reads}"
        );
    }
    // Only the tree that `show` prints as a listing reaches Git
    // (`cat-file -e`, then `show`).
    let counts = recorder.totals().1;
    assert_eq!(counts.get("subprocess_git"), Some(&2), "{counts:?}");

    // The configuration switch sends every read to Git, with the same answers.
    std::fs::write(
        root.join(".aethyme/config.toml"),
        "[git]\nin_process = false\n",
    )
    .unwrap();
    let recorder = crate::phase_telemetry::Recorder::default();
    {
        let _installed = crate::phase_telemetry::install(recorder.clone());
        read_everything();
    }
    let reads = recorder.attachments().remove("git_read").unwrap();
    for kind in [
        "symbolic_ref",
        "origin_head_default",
        "config_tree_entries",
        "blob_bytes",
        "file_at_revision",
        "path_exists_at",
    ] {
        let key = format!("{kind}.fallback.disabled");
        assert!(
            reads[&key]["count"].as_u64().unwrap_or(0) >= 1,
            "{key}: {reads}"
        );
        assert!(reads.get(format!("{kind}.answered")).is_none(), "{reads}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn non_utf8_names_beside_configuration_are_skipped_like_git() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let tree = tree_shaped(root);
    let odd = root
        .join(".aethyme")
        .join(std::ffi::OsStr::from_bytes(b"caf\xe9.toml"));
    std::fs::write(odd, "odd\n").unwrap();
    git(root, &["add", ".aethyme"]);
    git(root, &["commit", "-qm", "odd name"]);
    let blobs = [tree.config_blob.as_str()];
    assert!(assert_tree_parity(root, &["HEAD"], &blobs) > 0);
}
