//! Replay declarative, committed source transformations in disposable Git
//! metadata. This lane creates a review artifact and verifies it with the
//! ordinary gate runner under the repository's existing trust decision.

use super::*;
use crate::gates::{GateRunOutcome, select_gates};
use std::time::{Duration, Instant};

const MAX_FILES: usize = 64;
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_PATCH_BYTES: usize = 256 * 1024;
const MAX_REPLACEMENTS: usize = 256;
const GATE_BUDGET: Duration = Duration::from_secs(90);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeBook {
    recipe: Vec<Recipe>,
}

#[derive(serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Recipe {
    RustCall {
        name: String,
        paths: Vec<String>,
        from: String,
        to: String,
    },
}

#[derive(serde::Serialize)]
pub(crate) struct SemanticPatch {
    pub recipe: String,
    pub base_commit: String,
    pub candidate_tree: String,
    pub candidate_commit: String,
    pub patch_file: PathBuf,
    pub files: Vec<String>,
    pub replacements: usize,
    pub gate_status: &'static str,
    pub gate_outcomes: Vec<GateRunOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence_dir: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_diagnostic: Option<String>,
}

/// The task's full, structured value opts in. A mention in normal prose
/// cannot trigger speculative gate commands.
pub(crate) fn recipe_name(task: &str) -> Option<&str> {
    let name = task.trim().strip_prefix("recipe:")?;
    (!name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'))
    .then_some(name)
}

impl Broker {
    pub(crate) fn prepare_semantic_patch(
        &mut self,
        session: &crate::Session,
        name: &str,
        scratch: &Path,
    ) -> Result<SemanticPatch, String> {
        let _span = crate::phase_telemetry::span("semantic_patch_prepare");
        let _operation = crate::file_lock::try_acquire_session_operation_guard(
            self.main_root(),
            session.id,
            crate::file_lock::SessionOperationLockMode::Shared,
        )
        .map_err(message)?
        .ok_or("session is being retired")?;
        let current = self.store.session(session.id).map_err(message)?;
        if current.status.is_closed() || current.cleanup_state != crate::SessionCleanupState::Open {
            return Err("session has closed".into());
        }
        let base = session
            .adopted_head
            .as_deref()
            .or(session.diff_base.as_deref())
            .ok_or("session has no recorded checkout commit")?;
        let repo = GitRepo::discover(Path::new(&session.worktree_path)).map_err(message)?;
        let config = repo
            .config_toml_at_commit(base, "semantic-patches")
            .map_err(message)?
            .ok_or("this commit has no semantic patch recipes")?;
        let book: RecipeBook = toml::from_str(&config).map_err(message)?;
        let mut matches = book.recipe.into_iter().filter(|recipe| {
            let Recipe::RustCall {
                name: candidate, ..
            } = recipe;
            candidate == name
        });
        let Recipe::RustCall {
            paths, from, to, ..
        } = matches
            .next()
            .ok_or("no such semantic patch recipe at this commit")?;
        if matches.next().is_some() {
            return Err("the committed recipe name is ambiguous".into());
        }

        let (files, tree, commit, patch, replacements, checkout, candidate) = {
            let _span = crate::phase_telemetry::span("semantic_patch_candidate");
            let files = candidate_files(&repo, base, &paths)?;
            let candidate = tempfile::Builder::new()
                .prefix("semantic-source-")
                .tempdir_in(scratch)
                .map_err(message)?;
            let (bytes, inodes) = repo.graph_snapshot_inventory(base).map_err(message)?;
            crate::verification::ensure_graph_snapshot_storage_available(
                repo.root(),
                candidate.path(),
                bytes,
                inodes,
            )
            .map_err(message)?;
            let checkout = repo
                .isolated_checkout_at(&candidate.path().join("checkout"), base)
                .map_err(message)?;
            if checkout.working_tree_hash().map_err(message)?
                != repo.commit_tree_id(base).map_err(message)?
            {
                return Err("disposable checkout differs from the recorded commit".into());
            }
            let (files, replacements) = apply_rust_calls(checkout.root(), files, &from, &to)?;
            let tree = checkout.working_tree_hash().map_err(message)?;
            let patch = checkout.isolated_working_patch(base).map_err(message)?;
            if patch.is_empty() || patch.len() > MAX_PATCH_BYTES {
                return Err("patch is empty or exceeds the artifact limit".into());
            }
            let commit = checkout
                .isolated_candidate_commit(base, &tree)
                .map_err(message)?;
            (
                files,
                tree,
                commit,
                patch,
                replacements,
                checkout,
                candidate,
            )
        };

        let mut artifact = tempfile::Builder::new()
            .prefix("semantic-")
            .suffix(".patch")
            .tempfile_in(scratch)
            .map_err(message)?;
        std::io::Write::write_all(artifact.as_file_mut(), &patch).map_err(message)?;
        let (_, patch_file) = artifact.keep().map_err(message)?;
        let mut result = SemanticPatch {
            recipe: name.to_owned(),
            base_commit: base.to_owned(),
            candidate_tree: tree,
            candidate_commit: commit,
            patch_file,
            files,
            replacements,
            gate_status: "unavailable",
            gate_outcomes: Vec::new(),
            evidence_dir: None,
            gate_diagnostic: None,
        };
        let verification =
            self.verify_semantic_patch(session.id, &checkout, candidate.path(), &result);
        match verification {
            Ok(outcomes) => {
                result.gate_status = if outcomes.is_empty() {
                    "no_gates_selected"
                } else if outcomes
                    .iter()
                    .all(|outcome| outcome.status == crate::GateStatus::Pass)
                {
                    "selected_gates_passed"
                } else {
                    "selected_gate_failed"
                };
                if result.gate_status == "selected_gates_passed" {
                    let head_matches = checkout
                        .head_commit()
                        .is_ok_and(|head| head == result.candidate_commit);
                    let tree_matches = checkout
                        .working_tree_hash()
                        .is_ok_and(|tree| tree == result.candidate_tree);
                    if !head_matches || !tree_matches {
                        result.gate_status = "candidate_changed";
                    }
                }
                // Keep logs and any uncertain writer evidence until the
                // session's scratch directory is reclaimed.
                result.gate_outcomes = outcomes;
                if !result.gate_outcomes.is_empty() {
                    result.evidence_dir = Some(candidate.keep());
                }
            }
            Err(error) => {
                result.gate_diagnostic = Some(error);
                result.evidence_dir = Some(candidate.keep());
            }
        }
        Ok(result)
    }

    fn verify_semantic_patch(
        &mut self,
        session_id: i64,
        checkout: &GitRepo,
        evidence_root: &Path,
        candidate: &SemanticPatch,
    ) -> Result<Vec<GateRunOutcome>, String> {
        let _span = crate::phase_telemetry::span("semantic_patch_verify");
        self.require_trusted_policy_at_commit(&candidate.base_commit, Some(session_id))
            .map_err(message)?;
        let Some(config) = checkout
            .config_toml_at_commit(&candidate.base_commit, "gates")
            .map_err(message)?
        else {
            return Ok(Vec::new());
        };
        let gates = crate::gates::parse_gates(&config).map_err(message)?;
        if select_gates(&gates, &candidate.files).is_empty() {
            return Ok(Vec::new());
        }
        // Preview receipts have their own database and cannot become cache
        // evidence for a subsequent real submission.
        let mut store =
            crate::BrokerStore::open(&evidence_root.join("preview.db")).map_err(message)?;
        let storage = self.storage_context().gate_storage_locations();
        crate::gates::run_affected_bounded_in(
            &mut store,
            checkout.root(),
            checkout,
            &gates,
            &candidate.files,
            GATE_BUDGET,
            Some(&candidate.base_commit),
            storage,
        )
        .map_err(message)
    }
}

fn candidate_files(
    repo: &GitRepo,
    commit: &str,
    patterns: &[String],
) -> Result<Vec<String>, String> {
    if patterns.is_empty() || patterns.len() > MAX_FILES {
        return Err("a recipe must declare a bounded nonempty path scope".into());
    }
    let mut globs = globset::GlobSetBuilder::new();
    for pattern in patterns {
        globs.add(
            globset::GlobBuilder::new(pattern)
                .literal_separator(true)
                .build()
                .map_err(message)?,
        );
    }
    let globs = globs.build().map_err(message)?;
    let entries = repo
        .tracked_tree_files_at(commit, Instant::now() + Duration::from_secs(5))
        .map_err(message)?;
    let files: Vec<_> = entries
        .into_iter()
        .filter(|entry| {
            entry.path.ends_with(".rs")
                && !entry.path.starts_with(".aethyme/")
                && globs.is_match(&entry.path)
        })
        .collect();
    if files.is_empty() || files.len() > MAX_FILES {
        return Err("recipe scope matches no Rust files or exceeds the file limit".into());
    }
    if files
        .iter()
        .any(|entry| !matches!(entry.mode.as_str(), "100644" | "100755"))
    {
        return Err("recipe scope includes a nonregular source file".into());
    }
    Ok(files.into_iter().map(|entry| entry.path).collect())
}

fn apply_rust_calls(
    root: &Path,
    files: Vec<String>,
    from: &str,
    to: &str,
) -> Result<(Vec<String>, usize), String> {
    let mut changed = Vec::new();
    let mut replacements = 0;
    for path in files {
        let location = root.join(&path);
        let metadata = std::fs::symlink_metadata(&location).map_err(message)?;
        if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
            return Err(format!("unsupported source file {path:?}"));
        }
        let original = std::fs::read_to_string(&location).map_err(message)?;
        let (updated, count) = aethyme_graph_indexer::replace_rust_calls(&original, from, to)
            .map_err(|reason| format!("{path:?}: {reason}"))?;
        replacements += count;
        if replacements > MAX_REPLACEMENTS {
            return Err("recipe exceeds the replacement limit".into());
        }
        if count > 0 {
            std::fs::write(&location, updated).map_err(message)?;
            changed.push(path);
        }
    }
    if changed.is_empty() {
        return Err("recipe did not find a matching call".into());
    }
    Ok((changed, replacements))
}

fn message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_preview_runs_gates_on_the_candidate_and_leaves_session_clean() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        std::fs::create_dir(root.join(".aethyme")).unwrap();
        std::fs::write(
            root.join("main.rs"),
            "fn demo() { crate::legacy::call(); }\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/semantic-patches.toml"),
            r#"
[[recipe]]
name = "new-api"
kind = "rust_call"
paths = ["main.rs"]
from = "crate::legacy::call"
to = "crate::api::call"
"#,
        )
        .unwrap();
        std::fs::write(
            root.join(".aethyme/gates.toml"),
            r#"
[[gate]]
name = "preview-test"
command = 'test "$(git rev-parse HEAD^)" = "$AETHYME_GATE_BASE_COMMIT" && git show HEAD:main.rs | grep -Fq "crate::api::call()"'
triggers = ["main.rs"]
cache = false
"#,
        )
        .unwrap();
        for args in [
            vec!["init", "-qb", "main"],
            vec!["add", "main.rs", ".aethyme"],
            vec![
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-qm",
                "fixture",
            ],
        ] {
            let output = crate::git::git_command()
                .args(args)
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let storage = crate::test_storage::TestStorage::new();
        let mut broker = storage.open(&root).unwrap();
        let policy = super::super::gate_trust::policy_at_root(&root).unwrap();
        super::super::gate_trust::record_trust_for_host_state(
            &root,
            &storage.host_state_dir(),
            &[&policy.policy_sha256],
            "fixture",
        )
        .unwrap();
        let session = broker.start_worktree("recipe:new-api", None).unwrap();
        let scratch = broker.ensure_session_scratch(session.id);
        let result = broker
            .prepare_semantic_patch(&session, "new-api", scratch.path.as_deref().unwrap())
            .unwrap();
        assert_eq!(
            result.gate_status, "selected_gates_passed",
            "{:?}",
            result.gate_diagnostic
        );
        assert_eq!(result.gate_outcomes.len(), 1);
        assert_eq!(result.gate_outcomes[0].tree_hash, result.candidate_tree);
        let patch = std::fs::read_to_string(result.patch_file).unwrap();
        assert!(patch.contains("+fn demo() { crate::api::call(); }"));
        let unchanged =
            std::fs::read_to_string(Path::new(&session.worktree_path).join("main.rs")).unwrap();
        assert_eq!(unchanged, "fn demo() { crate::legacy::call(); }\n");
    }

    #[test]
    fn semantic_recipe_rewrites_only_direct_qualified_calls() {
        let mut source = tempfile::NamedTempFile::with_suffix(".rs").unwrap();
        std::io::Write::write_all(
            source.as_file_mut(),
            br#"fn demo() {
            crate::legacy::call(crate::legacy::call(1));
            let text = "crate::legacy::call(2)";
            // crate::legacy::call(3);
            passthrough! { crate::legacy::call(4) }
            legacy::call(5);
        }"#,
        )
        .unwrap();
        let name = source
            .path()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let (paths, count) = apply_rust_calls(
            source.path().parent().unwrap(),
            vec![name.clone()],
            "crate::legacy::call",
            "crate::new::call",
        )
        .unwrap();
        let contents = std::fs::read_to_string(source.path()).unwrap();
        assert_eq!(paths, [name]);
        assert_eq!(count, 2);
        assert!(contents.contains("crate::new::call(crate::new::call(1))"));
        assert!(contents.contains(r#""crate::legacy::call(2)""#));
        assert!(contents.contains("// crate::legacy::call(3)"));
        assert!(contents.contains("passthrough! { crate::legacy::call(4) }"));
        assert!(contents.contains("legacy::call(5)"));
        assert_eq!(recipe_name("Inspect recipe:rename"), None);
        assert_eq!(recipe_name("recipe:rename"), Some("rename"));
    }
}
