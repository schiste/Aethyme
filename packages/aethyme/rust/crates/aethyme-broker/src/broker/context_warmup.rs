//! Materialize navigation once for a newly observed default-branch commit.
//! The scheduler reads committed Git objects and never updates a checkout or
//! runs repository commands. A task consumes only its own commit's store.

use super::*;
use aethyme_engine::store::redb::graph_store::{GRAPH_STORE_SCHEMA_VERSION, GraphStore};
use aethyme_graph_storage::{
    GRAPH_MANIFEST_RELPATH, GraphAuthorityManifest, GraphStoreArtifactCache, GraphStoreCacheKey,
    graph_fragment_set_digest,
};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const MAX_GRAPH_BYTES: usize = 32 * 1024 * 1024;
const MAX_GRAPH_FILES: usize = 1_024;
const GIT_BUDGET: Duration = Duration::from_secs(10);

pub(super) fn cache_root(main_root: &Path) -> PathBuf {
    main_root.join(".aethyme/run/start-context")
}

impl Broker {
    /// The existing PR-watch tick lock serializes publishers. Repeated passes
    /// cheaply return an unchanged store, including when sessions are idle.
    pub(crate) fn warm_default_task_context(&self) -> Result<(), String> {
        use crate::git::with_git_deadline;
        let _span = crate::phase_telemetry::span("start_context_warmup");
        let observed = with_git_deadline(GIT_BUDGET, || {
            let repo = self.repo_handle();
            let Some((_, commit)) = repo.upstream_default() else {
                return Ok(None);
            };
            let destination = cache_root(self.main_root());
            if graph_matches(&destination, &commit) {
                return Ok(None);
            }
            let policy = crate::graph_integrity::load_graph_policy_at_commit(repo, &commit)
                .map_err(message)?;
            if !policy.enforces_committed_fragments() {
                return Ok(None);
            }
            let (files, manifest, manifest_digest) = read_verified_fragments(repo, &commit)?;
            Ok::<_, String>(Some((commit, files, manifest, manifest_digest)))
        })?;
        let Some((commit, files, manifest, manifest_digest)) = observed else {
            return Ok(());
        };

        let destination =
            crate::file_lock::ensure_task_context_cache(self.main_root()).map_err(message)?;
        let key = GraphStoreCacheKey {
            source_tree_sha256: manifest.source_tree_sha256,
            fragment_manifest_sha256: manifest_digest,
            engine_version: manifest.engine_version,
            engine_protocol_version: aethyme_engine::daemon::ENGINE_PROTOCOL_VERSION,
            storage_schema_version: GRAPH_STORE_SCHEMA_VERSION,
        };
        let cache = GraphStoreArtifactCache::for_environment(self.main_root());
        let entry = cache
            .as_ref()
            .and_then(|cache| cache.try_acquire(key).ok().flatten());
        if let Some(entry) = &entry
            && let Ok(Some(artifact)) = entry.lookup()
            && GraphStore::install_cached_artifact(&destination, &artifact.path, &commit).is_ok()
        {
            return Ok(());
        }

        let _build = crate::phase_telemetry::span("start_context_warmup_materialize");
        let temporary = tempfile::Builder::new()
            .prefix("fragments-")
            .tempdir_in(&destination)
            .map_err(message)?;
        for (path, _, bytes) in files {
            let output = temporary.path().join(path);
            std::fs::create_dir_all(output.parent().ok_or("invalid graph path")?)
                .map_err(message)?;
            std::fs::write(output, bytes).map_err(message)?;
        }
        let (mut map, _) =
            aethyme_engine::map::RepositoryMap::build_from_fragments(temporary.path())?;
        map.snapshot.root = self.main_root().to_string_lossy().into_owned();
        aethyme_engine::index_store::materialize_graph_store(&destination, &map, &commit)?;
        if let Some(entry) = entry {
            crate::warn_unrecorded(
                "store a warmed graph artifact",
                entry.store(&GraphStore::final_path(&destination)),
            );
        }
        Ok(())
    }
}

fn graph_matches(root: &Path, commit: &str) -> bool {
    GraphStore::open_read_only(root)
        .ok()
        .and_then(|store| store.repo_metadata().ok().flatten())
        .is_some_and(|metadata| metadata.commit_hash.as_deref() == Some(commit))
}

type Fragments = Vec<(String, String, Vec<u8>)>;

fn read_verified_fragments(
    repository: &GitRepo,
    commit: &str,
) -> Result<(Fragments, GraphAuthorityManifest, String), String> {
    let _span = crate::phase_telemetry::span("start_context_warmup_inputs");
    if let Some(reason) = crate::git::graph_verification_environment_refusal() {
        return Err(reason.into());
    }
    let _environment = crate::git::graph_verification_git_guard();
    let entries = repository
        .tracked_tree_files_at(
            commit,
            crate::git::active_git_deadline().unwrap_or_else(|| Instant::now() + GIT_BUDGET),
        )
        .map_err(message)?;
    let mut inputs = Sha256::new();
    let mut fragments = Vec::new();
    let mut available = MAX_GRAPH_BYTES;
    for entry in entries {
        if !entry.path.starts_with(".aethyme/graph/") {
            let kind = if entry.mode == "160000" {
                "commit"
            } else {
                "blob"
            };
            inputs.update(format!(
                "{} {kind} {}\t{}\0",
                entry.mode, entry.object_id, entry.path
            ));
            continue;
        }
        if !matches!(entry.mode.as_str(), "100644" | "100755")
            || fragments.len() >= MAX_GRAPH_FILES
            || available == 0
        {
            return Err(
                "committed graph uses unsupported entries or exceeds the warmup budget".into(),
            );
        }
        let bytes = repository
            .graph_blob_bytes(&entry.object_id, available)
            .map_err(message)?;
        available -= bytes.len();
        fragments.push((entry.path, entry.mode, bytes));
    }
    let bytes = fragments
        .iter()
        .find(|(path, _, _)| path == GRAPH_MANIFEST_RELPATH)
        .map(|(_, _, bytes)| bytes)
        .ok_or("no committed graph authority manifest")?;
    let manifest = GraphAuthorityManifest::decode(bytes).map_err(message)?;
    let manifest_digest = format!("{:x}", Sha256::digest(bytes));
    if format!("{:x}", inputs.finalize()) != manifest.source_tree_sha256
        || graph_fragment_set_digest(
            fragments
                .iter()
                .map(|(path, mode, bytes)| (path.as_str(), mode.as_str(), bytes.as_slice())),
        ) != manifest.fragment_set_sha256
    {
        return Err("committed graph authority does not match the observed default branch".into());
    }
    Ok((fragments, manifest, manifest_digest))
}

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_binds_graph_bytes_to_the_observed_commit() {
        let fixture = tempfile::tempdir().unwrap();
        let root = &fixture.path().canonicalize().unwrap();
        let version = env!("CARGO_PKG_VERSION");
        aethyme_graph_storage::bootstrap_repo(root, version).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("src/lib.rs"),
            "pub fn task_target() {}\npub fn call_target() { task_target(); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/config.toml"),
            "[graph]\nauthority = \"committed_fragments\"\nrepository = \"example/repo\"\n",
        )
        .unwrap();
        let git = |args: &[&str]| {
            let output = crate::git::git_command()
                .args(args)
                .current_dir(root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        };
        let commit = || {
            git(&[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "fixture",
            ])
        };
        git(&["init", "-qb", "main"]);
        git(&[
            "add",
            ".aethyme/config.toml",
            ".aethyme/engine-version",
            "src/lib.rs",
        ]);
        commit();
        let repo = GitRepo::discover(root).unwrap();
        let context =
            aethyme_graph_indexer::IndexerContext::new("example/repo", root.clone(), version)
                .unwrap();
        aethyme_graph_indexer::index_repo_to_disk(
            &context,
            &aethyme_graph_indexer::WalkOptions::default(),
        )
        .unwrap();
        aethyme_graph_indexer::link_repo(&context).unwrap();
        let manifest = GraphAuthorityManifest::build(
            root,
            &repo.head_commit().unwrap(),
            "example/repo",
            version,
        )
        .unwrap();
        std::fs::write(
            root.join(GRAPH_MANIFEST_RELPATH),
            manifest.encode().unwrap(),
        )
        .unwrap();
        git(&["add", ".aethyme/graph"]);
        commit();
        let accepted = repo.head_commit().unwrap();
        assert_eq!(
            read_verified_fragments(&repo, &accepted).unwrap().1,
            manifest
        );
        git(&["update-ref", "refs/remotes/origin/main", &accepted]);
        git(&[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ]);
        let storage = crate::test_storage::TestStorage::new();
        let broker = storage.open(root).unwrap();
        broker.warm_default_task_context().unwrap();
        assert!(graph_matches(&cache_root(root), &accepted));
        let context = super::super::scope_capture::derive_task_context(
            broker.store_ref(),
            root,
            root,
            &accepted,
            "change task_target in src/lib.rs",
        )
        .unwrap();
        assert_eq!(context.source_commit, accepted);
        assert!(context.paths.contains(&"src/lib.rs".to_owned()));

        std::fs::write(root.join("src/lib.rs"), "fn different_target() {}\n").unwrap();
        git(&["add", "src/lib.rs"]);
        commit();
        let stale = repo.head_commit().unwrap();
        assert!(
            read_verified_fragments(&repo, &stale)
                .unwrap_err()
                .contains("does not match")
        );
    }
}
