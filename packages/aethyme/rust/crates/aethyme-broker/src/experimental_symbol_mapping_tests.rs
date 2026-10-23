#![cfg(unix)]
//! Advisory symbol mapping is exercised against two real disposable clones of
//! the checked-in synthetic Playground seed. The tests do not infer mappings
//! from names or existing NodeIds: a positive result needs the exact records.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::collaboration_cli::{project_association, project_command_for_test};
use crate::collaboration_state::{CollaborationRoot, CollaborationStore, ProjectKey};
use crate::experimental_symbol_mapping::{
    CapturedCloneSnapshot, CloneSnapshotBinding, MappingResolution, resolve_associated_mapping,
    resolve_within_binding,
};
use aethyme_contracts::experimental_v0::source_snapshot::{EntryKind, SourceEntry};
use aethyme_contracts::experimental_v0::symbol_identity::{
    SYMBOL_MAPPING_AUTHORITY, decode_symbol_identity_record,
};
use aethyme_contracts::experimental_v0::{
    ProjectId, Record, RecordId, SourceSnapshot, StateReading,
};
use aethyme_graph_schema::{NodeId, NodeKind};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const SEED: &str = "tests/fixtures/synthetic-playground-symbol-identity/seed";
const PROJECT: &str = "proj:aaaaaaaaaaaaaaaaaaaaaaaaaa";
const PEER_PROJECT: &str = "proj:bbbbbbbbbbbbbbbbbbbbbbbbba";
const OTHER_PROJECT: &str = "proj:ccccccccccccccccccccccccca";
const PROFILE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const FIXTURE_GIT_PATH: &str = "/usr/bin:/bin";
const FIXTURE_GIT_NULL_DEVICE: &str = "/dev/null";

struct Clones {
    _root: TempDir,
    left: PathBuf,
    right: PathBuf,
}

#[derive(Debug, Clone, Copy)]
enum RecordScenario {
    Exact,
    CrossProject,
    SameClone,
    CloneBindingMismatch,
    SourceMismatch,
    ProfileMismatch,
    OriginMismatch,
    MalformedNodeId,
    WrongNodeIdRepo,
    UnknownScope,
    AliasedOrigin,
    ExternalPackageVersionMismatch,
}

struct MappingFixture {
    binding: CloneSnapshotBinding,
    claim: Vec<u8>,
    records: Vec<Vec<u8>>,
    left_node: NodeId,
    right_node: NodeId,
}

fn git_command(cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .env_clear()
        .current_dir(cwd)
        .env("PATH", FIXTURE_GIT_PATH)
        .env("HOME", "")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_SYSTEM", FIXTURE_GIT_NULL_DEVICE)
        .env("GIT_CONFIG_GLOBAL", FIXTURE_GIT_NULL_DEVICE)
        .env("GIT_ATTR_NOSYSTEM", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_AUTHOR_NAME", "symbol identity test")
        .env("GIT_AUTHOR_EMAIL", "symbol-identity@example.invalid")
        .env("GIT_COMMITTER_NAME", "symbol identity test")
        .env("GIT_COMMITTER_EMAIL", "symbol-identity@example.invalid")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2000-01-01T00:00:00Z")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.attributesFile=/dev/null",
            "-c",
            "core.excludesFile=/dev/null",
            "-c",
            "commit.template=",
        ])
        .args(args)
        .output()
        .expect("start git")
}

fn command(cwd: &Path, args: &[&str]) {
    let output = git_command(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_text(cwd: &Path, args: &[&str]) -> String {
    let output = git_command(cwd, args);
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn actual_clones() -> Clones {
    let root = tempfile::tempdir().expect("temporary playground clones");
    let seed = root.path().join("seed");
    let templates = root.path().join("empty-templates");
    fs::create_dir_all(seed.join("src")).unwrap();
    fs::create_dir_all(&templates).unwrap();
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join(SEED);
    for relative in ["src/lib.ts", "src/other.ts"] {
        let source = fixture.join(relative);
        let destination = seed.join(relative);
        fs::create_dir_all(destination.parent().unwrap()).unwrap();
        fs::copy(source, destination).unwrap();
    }
    let template_arg = format!("--template={}", templates.display());
    command(&seed, &["init", "-q", "-b", "main", template_arg.as_str()]);
    command(&seed, &["add", "-A"]);
    command(&seed, &["commit", "-qm", "synthetic Playground seed"]);
    let left = root.path().join("clone-a");
    let right = root.path().join("clone-b");
    command(
        root.path(),
        &["clone", "--quiet", "--local", "seed", "clone-a"],
    );
    command(
        root.path(),
        &["clone", "--quiet", "--local", "seed", "clone-b"],
    );
    assert_eq!(git_root(&left), left.canonicalize().unwrap());
    assert_eq!(git_root(&right), right.canonicalize().unwrap());
    Clones {
        _root: root,
        left,
        right,
    }
}

fn git_root(root: &Path) -> PathBuf {
    let output = git_command(root, &["rev-parse", "--show-toplevel"]);
    assert!(output.status.success());
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

fn commit(root: &Path, message: &str) {
    command(root, &["add", "-A"]);
    command(root, &["commit", "-qm", message]);
}

fn repo_component(root: &Path) -> String {
    root.canonicalize()
        .unwrap()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned()
}

fn source_snapshot(root: &Path) -> SourceSnapshot {
    let mut entries = Vec::new();
    collect_entries(root, root, &mut entries);
    SourceSnapshot::new(entries).expect("complete clone source snapshot")
}

fn collect_entries(root: &Path, directory: &Path, entries: &mut Vec<SourceEntry>) {
    for entry in fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        let relative = path.strip_prefix(root).unwrap();
        if relative == Path::new(".git") {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).unwrap();
        if metadata.file_type().is_dir() {
            collect_entries(root, &path, entries);
        } else if metadata.file_type().is_file() {
            let relative = relative
                .to_str()
                .expect("UTF-8 fixture path")
                .replace('\\', "/");
            entries.push(SourceEntry::from_content(
                relative.into_bytes(),
                EntryKind::Regular,
                &fs::read(path).unwrap(),
            ));
        } else if metadata.file_type().is_symlink() {
            let relative = relative
                .to_str()
                .expect("UTF-8 fixture path")
                .replace('\\', "/");
            let target = fs::read_link(path).unwrap();
            entries.push(SourceEntry::from_content(
                relative.into_bytes(),
                EntryKind::Symlink,
                target.to_string_lossy().as_bytes(),
            ));
        }
    }
}

fn function_declarations(source: &str) -> Vec<(String, String)> {
    source
        .lines()
        .filter_map(|line| {
            let declaration = line.trim().strip_prefix("export function ")?;
            let name_end = declaration.find('(')?;
            Some((declaration[..name_end].to_owned(), declaration.to_owned()))
        })
        .collect()
}

fn node_ids(root: &Path, relative: &str, name: &str) -> NodeId {
    let source = fs::read_to_string(root.join(relative)).expect("source file for NodeId");
    assert!(
        function_declarations(&source)
            .iter()
            .any(|(declared, _)| declared == name),
        "{name:?} is declared in {relative:?}"
    );
    NodeId::new(NodeKind::Function, &repo_component(root), relative, name).expect("fixture NodeId")
}

fn rid(label: &str) -> RecordId {
    let digest = Sha256::digest(label.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    RecordId::parse(&format!("sha256:{hex}")).expect("canonical test record id")
}

fn canonical_record(value: Value) -> (Record, Vec<u8>) {
    let bytes = serde_json::to_vec(&value).expect("JSON record");
    let record = decode_symbol_identity_record(&bytes).expect("canonical experimental record");
    (record, bytes)
}

fn record_id(record: &Record) -> RecordId {
    record.id()
}

fn independently_enroll_and_bind_clone(repo: &Path, key_text: &str, operation: &str) {
    fs::create_dir_all(repo.join(".aethyme")).unwrap();
    let key = ProjectKey::parse(key_text).unwrap();
    fs::write(
        repo.join(".aethyme/config.toml"),
        format!(
            "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\n",
            key.as_str()
        ),
    )
    .unwrap();
    command(repo, &["add", "-A"]);
    command(repo, &["commit", "-qm", "enroll independent clone"]);

    let anchor = crate::collaboration_archive::pin_commit(repo, "HEAD").unwrap();
    let root = CollaborationRoot::resolve().unwrap();
    let mut store = CollaborationStore::open(&root, &key, &[]).unwrap();
    let request = crate::collaboration_capture::CaptureRequest {
        operation_id: crate::collaboration_capture::OperationId::parse(operation).unwrap(),
        repository: repo.to_path_buf(),
        base: anchor.clone(),
        result: anchor.clone(),
        policy: crate::collaboration_capture::CapturePolicy::Required,
        retention: crate::collaboration_capture::RetentionBoundary::UntilReleased,
    };
    assert!(matches!(
        crate::collaboration_capture::capture(&mut store, &request).unwrap(),
        crate::collaboration_capture::CaptureOutcome::Acknowledged(_)
    ));

    let anchor_text = anchor.as_str();
    let enrolled =
        project_command_for_test(repo, repo, &["enroll", "--commit", anchor_text, "--write"])
            .unwrap();
    let descriptor_id = enrolled["descriptor_record_id"].as_str().unwrap();
    fs::write(
        repo.join(".aethyme/config.toml"),
        format!(
            "[collaboration]\ncapture = \"required\"\nproject = \"{}\"\ndescriptor = \".aethyme/project-descriptor.json\"\n",
            key.as_str()
        ),
    )
    .unwrap();
    command(repo, &["add", "-A"]);
    command(repo, &["commit", "-qm", "confirm descriptor pointer"]);
    let bound =
        project_command_for_test(repo, repo, &["bind", "--confirm", descriptor_id]).unwrap();
    assert_eq!(bound["descriptor_record_id"], descriptor_id);
    assert!(
        crate::collaboration_identity_store::resolve_approved_project_binding(&store)
            .unwrap()
            .is_some(),
        "the actual project bind route persists the independent identity"
    );
}

fn associated_mapping_records(
    association_record_id: &RecordId,
    local: &project_association::ProjectContext,
    peer: &project_association::ProjectContext,
) -> (RecordId, Vec<u8>, Vec<Vec<u8>>) {
    let profile = rid("association-resolver-profile");
    let left_project = local.binding.project_id();
    let right_project = peer.binding.project_id();
    let left_clone = local.binding.descriptor_record_id();
    let right_clone = peer.binding.descriptor_record_id();
    let left_source = &local._source_snapshot;
    let right_source = &peer._source_snapshot;
    let left_node = node_ids(&local.root, "src/lib.ts", "stable");
    let right_node = node_ids(&peer.root, "src/lib.ts", "stable");

    let (left_symbol, left_symbol_bytes) = symbol_record(
        left_project.as_str(),
        left_clone.as_str(),
        &left_node.to_string(),
    );
    let (right_symbol, right_symbol_bytes) = symbol_record(
        right_project.as_str(),
        right_clone.as_str(),
        &right_node.to_string(),
    );
    let left_symbol_id = left_symbol.id();
    let right_symbol_id = right_symbol.id();
    let left_fact = rid("association-resolver-left-fact");
    let right_fact = rid("association-resolver-right-fact");
    let (left_origin, left_origin_bytes) = canonical_record(json!({
        "schema": "aethyme.symbol-origin-evidence/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "project_id": left_project.as_str(),
        "clone_id": left_clone.as_str(),
        "symbol_ref": left_symbol_id.as_str(),
        "source_snapshot": left_source.id().as_str(),
        "profile": profile.as_str(),
        "fact_revision": left_fact.as_str(),
        "origin_namespace": "independently-enrolled-left"
    }));
    let (right_origin, right_origin_bytes) = canonical_record(json!({
        "schema": "aethyme.symbol-origin-evidence/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "project_id": right_project.as_str(),
        "clone_id": right_clone.as_str(),
        "symbol_ref": right_symbol_id.as_str(),
        "source_snapshot": right_source.id().as_str(),
        "profile": profile.as_str(),
        "fact_revision": right_fact.as_str(),
        "origin_namespace": "independently-enrolled-right"
    }));
    let left_origin_id = left_origin.id();
    let right_origin_id = right_origin.id();
    let (left_observation, left_observation_bytes) = canonical_record(json!({
        "schema": "aethyme.observed-symbol-revision/experimental-v0",
        "symbol_ref": left_symbol_id.as_str(),
        "source_snapshot": left_source.id().as_str(),
        "profile": profile.as_str(),
        "fact_revision": left_fact.as_str(),
        "origin_record": left_origin_id.as_str()
    }));
    let (right_observation, right_observation_bytes) = canonical_record(json!({
        "schema": "aethyme.observed-symbol-revision/experimental-v0",
        "symbol_ref": right_symbol_id.as_str(),
        "source_snapshot": right_source.id().as_str(),
        "profile": profile.as_str(),
        "fact_revision": right_fact.as_str(),
        "origin_record": right_origin_id.as_str()
    }));
    let mut evidence_origins = [left_origin_id.clone(), right_origin_id.clone()];
    evidence_origins.sort();
    let (mapping, mapping_bytes) = canonical_record(json!({
        "schema": "aethyme.explicit-symbol-mapping-evidence/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "association_record_id": association_record_id.as_str(),
        "left_project_id": left_project.as_str(),
        "right_project_id": right_project.as_str(),
        "left_clone_id": left_clone.as_str(),
        "right_clone_id": right_clone.as_str(),
        "left_source_snapshot": left_source.id().as_str(),
        "right_source_snapshot": right_source.id().as_str(),
        "profile": profile.as_str(),
        "left_symbol_ref": left_symbol_id.as_str(),
        "right_symbol_ref": right_symbol_id.as_str(),
        "left_observation": left_observation.id().as_str(),
        "right_observation": right_observation.id().as_str(),
        "left_origin": left_origin_id.as_str(),
        "right_origin": right_origin_id.as_str()
    }));
    let (_, claim_bytes) = canonical_record(json!({
        "schema": "aethyme.symbol-mapping-claim/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "binding": mapping.id().as_str(),
        "evidence": [evidence_origins[0].as_str(), evidence_origins[1].as_str()],
        "left_observation": left_observation.id().as_str(),
        "right_observation": right_observation.id().as_str(),
        "state": "proposed"
    }));
    (
        profile,
        claim_bytes,
        vec![
            mapping_bytes,
            left_observation_bytes,
            right_observation_bytes,
            left_symbol_bytes,
            right_symbol_bytes,
            left_origin_bytes,
            right_origin_bytes,
        ],
    )
}

fn symbol_record(project: &str, clone: &str, node_id: &str) -> (Record, Vec<u8>) {
    canonical_record(json!({
        "schema": "aethyme.symbol-ref/experimental-v0",
        "project_id": project,
        "clone_id": clone,
        "scope": { "kind": "engine_node", "node_id": node_id }
    }))
}

fn external_symbol_record(project: &str, clone: &str, version: &str) -> (Record, Vec<u8>) {
    canonical_record(json!({
        "schema": "aethyme.symbol-ref/experimental-v0",
        "project_id": project,
        "clone_id": clone,
        "scope": {
            "kind": "external_package",
            "producer_namespace": "synthetic-package-index",
            "package": "example-package",
            "version": version,
            "symbol": "Widget"
        }
    }))
}

fn mapping_fixture(clones: &Clones, scenario: RecordScenario) -> MappingFixture {
    mapping_fixture_named(clones, scenario, "stable")
}

fn mapping_fixture_named(
    clones: &Clones,
    scenario: RecordScenario,
    symbol_name: &str,
) -> MappingFixture {
    let left_source = source_snapshot(&clones.left);
    let right_source = source_snapshot(&clones.right);
    let left_repo = repo_component(&clones.left);
    let right_repo = repo_component(&clones.right);
    let left_node = node_ids(&clones.left, "src/lib.ts", symbol_name);
    let right_node = node_ids(&clones.right, "src/lib.ts", symbol_name);
    let left_clone = rid("clone-a");
    let right_clone = rid("clone-b");
    let left_project = ProjectId::parse(PROJECT).unwrap();
    let right_project = ProjectId::parse(PEER_PROJECT).unwrap();
    let right_project_in_records = if matches!(scenario, RecordScenario::CrossProject) {
        ProjectId::parse(OTHER_PROJECT).unwrap()
    } else {
        right_project.clone()
    };
    let profile = if matches!(scenario, RecordScenario::ProfileMismatch) {
        rid("different-profile")
    } else {
        RecordId::parse(PROFILE).unwrap()
    };
    let right_clone_in_records = match scenario {
        RecordScenario::SameClone => left_clone.clone(),
        RecordScenario::CloneBindingMismatch => rid("clone-c"),
        _ => right_clone.clone(),
    };
    let right_source_id = if matches!(scenario, RecordScenario::SourceMismatch) {
        SourceSnapshot::new(std::iter::empty::<SourceEntry>())
            .unwrap()
            .id()
    } else {
        right_source.id()
    };
    let right_source_encoded = right_source_id.as_str();
    let association_record_id = rid("association-clone-a-to-b");
    let local_binding = CloneSnapshotBinding::from_snapshots_for_test(
        association_record_id.clone(),
        left_project.clone(),
        right_project.clone(),
        CapturedCloneSnapshot::new(left_clone.clone(), left_repo, &left_source),
        CapturedCloneSnapshot::new(right_clone.clone(), right_repo, &right_source),
        RecordId::parse(PROFILE).unwrap(),
    )
    .unwrap();

    let left_fact = rid("left-fact-revision");
    let right_fact = rid("right-fact-revision");
    let left_origin_namespace = "synthetic-playground-clone-a";
    let right_origin_namespace = "synthetic-playground-clone-b";
    let left_symbol_node = left_node.to_string();
    let right_symbol_node = match scenario {
        RecordScenario::MalformedNodeId => "not-a-node-id".to_owned(),
        RecordScenario::WrongNodeIdRepo => NodeId::new(
            NodeKind::Function,
            "different-repo",
            "src/lib.ts",
            symbol_name,
        )
        .unwrap()
        .to_string(),
        _ => right_node.to_string(),
    };
    let (left_symbol, left_symbol_bytes) =
        if matches!(scenario, RecordScenario::ExternalPackageVersionMismatch) {
            external_symbol_record(left_project.as_str(), left_clone.as_str(), "1.0.0")
        } else {
            symbol_record(
                left_project.as_str(),
                left_clone.as_str(),
                &left_symbol_node,
            )
        };
    let (right_symbol, right_symbol_bytes) = if matches!(scenario, RecordScenario::UnknownScope) {
        canonical_record(json!({
            "schema": "aethyme.symbol-ref/experimental-v0",
            "project_id": right_project_in_records.as_str(),
            "clone_id": right_clone_in_records.as_str(),
            "scope": { "kind": "future_scope", "opaque": "retained" }
        }))
    } else if matches!(scenario, RecordScenario::ExternalPackageVersionMismatch) {
        external_symbol_record(
            right_project_in_records.as_str(),
            right_clone_in_records.as_str(),
            "2.0.0",
        )
    } else {
        symbol_record(
            right_project_in_records.as_str(),
            right_clone_in_records.as_str(),
            &right_symbol_node,
        )
    };
    let left_symbol_id = record_id(&left_symbol);
    let right_symbol_id = record_id(&right_symbol);

    let (left_origin, left_origin_bytes) = canonical_record(json!({
        "schema": "aethyme.symbol-origin-evidence/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "project_id": left_project.as_str(),
        "clone_id": left_clone.as_str(),
        "symbol_ref": left_symbol_id.as_str(),
        "source_snapshot": left_source.id().as_str(),
        "profile": profile.as_str(),
        "fact_revision": left_fact.as_str(),
        "origin_namespace": left_origin_namespace
    }));
    let left_origin_id = record_id(&left_origin);
    let right_origin_symbol = if matches!(scenario, RecordScenario::OriginMismatch) {
        left_symbol_id.as_str()
    } else {
        right_symbol_id.as_str()
    };
    let (right_origin, right_origin_bytes) = canonical_record(json!({
        "schema": "aethyme.symbol-origin-evidence/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "project_id": right_project_in_records.as_str(),
        "clone_id": right_clone_in_records.as_str(),
        "symbol_ref": right_origin_symbol,
        "source_snapshot": right_source_encoded,
        "profile": profile.as_str(),
        "fact_revision": right_fact.as_str(),
        "origin_namespace": right_origin_namespace
    }));
    let right_origin_id = record_id(&right_origin);

    let (left_observation, left_observation_bytes) = canonical_record(json!({
        "schema": "aethyme.observed-symbol-revision/experimental-v0",
        "symbol_ref": left_symbol_id.as_str(),
        "source_snapshot": left_source.id().as_str(),
        "profile": profile.as_str(),
        "fact_revision": left_fact.as_str(),
        "origin_record": left_origin_id.as_str()
    }));
    let (right_observation, right_observation_bytes) = canonical_record(json!({
        "schema": "aethyme.observed-symbol-revision/experimental-v0",
        "symbol_ref": right_symbol_id.as_str(),
        "source_snapshot": right_source_encoded,
        "profile": profile.as_str(),
        "fact_revision": right_fact.as_str(),
        "origin_record": right_origin_id.as_str()
    }));
    let left_observation_id = record_id(&left_observation);
    let right_observation_id = record_id(&right_observation);

    let mapping_value = json!({
        "schema": "aethyme.explicit-symbol-mapping-evidence/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "association_record_id": association_record_id.as_str(),
        "left_project_id": left_project.as_str(),
        "right_project_id": right_project_in_records.as_str(),
        "left_clone_id": left_clone.as_str(),
        "right_clone_id": right_clone_in_records.as_str(),
        "left_source_snapshot": left_source.id().as_str(),
        "right_source_snapshot": right_source_encoded,
        "profile": profile.as_str(),
        "left_symbol_ref": left_symbol_id.as_str(),
        "right_symbol_ref": right_symbol_id.as_str(),
        "left_observation": left_observation_id.as_str(),
        "right_observation": right_observation_id.as_str(),
        "left_origin": left_origin_id.as_str(),
        "right_origin": if matches!(scenario, RecordScenario::AliasedOrigin) {
            left_origin_id.as_str()
        } else {
            right_origin_id.as_str()
        }
    });
    let (mapping_id, mapping_bytes) = if matches!(
        scenario,
        RecordScenario::SameClone | RecordScenario::AliasedOrigin
    ) {
        (
            rid("invalid mapping record"),
            serde_json::to_vec(&mapping_value).unwrap(),
        )
    } else {
        let (mapping, bytes) = canonical_record(mapping_value);
        (record_id(&mapping), bytes)
    };
    let mut origin_ids = [left_origin_id, right_origin_id];
    origin_ids.sort();
    let (_claim, claim_bytes) = canonical_record(json!({
        "schema": "aethyme.symbol-mapping-claim/experimental-v0",
        "authority": SYMBOL_MAPPING_AUTHORITY,
        "binding": mapping_id.as_str(),
        "evidence": [origin_ids[0].as_str(), origin_ids[1].as_str()],
        "left_observation": left_observation_id.as_str(),
        "right_observation": right_observation_id.as_str(),
        "state": "proposed"
    }));
    MappingFixture {
        binding: local_binding,
        claim: claim_bytes,
        records: vec![
            mapping_bytes,
            left_observation_bytes,
            right_observation_bytes,
            left_symbol_bytes,
            right_symbol_bytes,
            left_origin_bytes,
            right_origin_bytes,
        ],
        left_node,
        right_node,
    }
}

fn claim_with_state(claim: &[u8], state: &str) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(claim).unwrap();
    value["state"] = json!(state);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn actual_playground_clones_bind_observations_and_keep_existing_node_ids() {
    let clones = actual_clones();
    let before = source_snapshot(&clones.right);
    let before_id = node_ids(&clones.right, "src/lib.ts", "stable");
    let lib = clones.right.join("src/lib.ts");
    let body = fs::read_to_string(&lib).unwrap();
    fs::write(
        &lib,
        body.replace("return `seed:${value}`", "return `edited:${value}`"),
    )
    .unwrap();
    commit(&clones.right, "edit function body");
    let after = source_snapshot(&clones.right);
    let after_id = node_ids(&clones.right, "src/lib.ts", "stable");
    assert_ne!(before.id(), after.id(), "source observation changed");
    assert_eq!(before_id, after_id, "NodeId tuple excludes function body");

    let fixture = mapping_fixture(&clones, RecordScenario::Exact);
    assert_ne!(
        fixture.left_node, fixture.right_node,
        "clone namespaces differ"
    );
    let records: Vec<&[u8]> = fixture.records.iter().map(Vec::as_slice).collect();
    let MappingResolution::EstablishedWithinBinding(established) =
        resolve_within_binding(&fixture.binding, &fixture.claim, &records)
    else {
        panic!("exact scoped evidence should resolve within its binding");
    };
    let (left_origin, right_origin) = established.origins();
    assert_ne!(
        left_origin, right_origin,
        "independent origin records are retained"
    );
    let (left_observation, right_observation) = established.observations();
    assert_ne!(left_observation, right_observation);
}

#[test]
fn associated_mapping_reloads_real_bindings_and_fails_closed_when_one_goes_stale() {
    const CHILD: &str = "AETHYME_ASSOCIATION_RESOLVER_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        associated_mapping_reloads_real_bindings_and_fails_closed_when_one_goes_stale_child();
        return;
    }

    let host_state = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "experimental_symbol_mapping::tests::associated_mapping_reloads_real_bindings_and_fails_closed_when_one_goes_stale",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("AETHYME_HOST_STATE_DIR", host_state.path())
        .env("HOME", home.path())
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CACHE_HOME")
        .output()
        .expect("spawn isolated resolver regression process");
    assert!(
        output.status.success(),
        "isolated resolver test failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("1 passed"),
        "the child harness must actually run the exact regression test:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

fn associated_mapping_reloads_real_bindings_and_fails_closed_when_one_goes_stale_child() {
    let clones = actual_clones();
    assert_eq!(
        git_text(&clones.left, &["rev-parse", "HEAD^{tree}"]),
        git_text(&clones.right, &["rev-parse", "HEAD^{tree}"]),
        "the independent clones start from the same committed tree"
    );
    assert_eq!(
        git_text(&clones.left, &["remote", "get-url", "origin"]),
        git_text(&clones.right, &["remote", "get-url", "origin"]),
        "the independent clones share the same origin URL"
    );
    independently_enroll_and_bind_clone(&clones.left, "association-left", "resolver-left-anchor");
    independently_enroll_and_bind_clone(
        &clones.right,
        "association-right",
        "resolver-right-anchor",
    );

    let (association_id, profile, claim, evidence) = {
        let peer_path = clones.right.to_str().unwrap();
        let proposal = project_command_for_test(
            &clones.left,
            &clones.left,
            &[
                "association",
                "propose",
                "--peer-repo",
                peer_path,
                "--write",
            ],
        )
        .unwrap();
        let association_id = proposal["association_record_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let confirmed = project_command_for_test(
            &clones.left,
            &clones.left,
            &[
                "association",
                "confirm",
                "--peer-repo",
                peer_path,
                "--confirm",
                &association_id,
            ],
        )
        .unwrap();
        assert_eq!(confirmed["status"], "confirmed");
        assert_eq!(confirmed["peer_store_written"], false);
        let (local_root, local_main_root) =
            project_association::repository_roots(&clones.left).unwrap();
        let (peer_root, peer_main_root) =
            project_association::repository_roots(&clones.right).unwrap();
        let local = project_association::inspect_project(&local_main_root, &local_root).unwrap();
        let peer = project_association::inspect_project(&peer_main_root, &peer_root).unwrap();
        assert_ne!(local.binding.project_id(), peer.binding.project_id());
        assert_ne!(
            local.binding.descriptor_record_id(),
            peer.binding.descriptor_record_id()
        );
        let (profile, claim, evidence) =
            associated_mapping_records(&RecordId::parse(&association_id).unwrap(), &local, &peer);
        drop(local);
        drop(peer);
        let evidence_refs: Vec<&[u8]> = evidence.iter().map(Vec::as_slice).collect();
        assert!(
            matches!(
                resolve_associated_mapping(
                    &clones.left,
                    &clones.right,
                    &RecordId::parse(&association_id).unwrap(),
                    &profile,
                    &claim,
                    &evidence_refs,
                ),
                MappingResolution::EstablishedWithinBinding(_)
            ),
            "a real confirmed association and both current approved bindings establish only the exact mapping proof"
        );
        (association_id, profile, claim, evidence)
    };

    let revoked = project_command_for_test(
        &clones.left,
        &clones.left,
        &["association", "revoke", "--confirm", &association_id],
    )
    .unwrap();
    assert_eq!(revoked["status"], "revoked");
    let evidence_refs: Vec<&[u8]> = evidence.iter().map(Vec::as_slice).collect();
    assert_eq!(
        resolve_associated_mapping(
            &clones.left,
            &clones.right,
            &RecordId::parse(&association_id).unwrap(),
            &profile,
            &claim,
            &evidence_refs,
        ),
        MappingResolution::Unknown("association_revoked")
    );

    // A new descriptor is committed through the normal proposal route but is
    // deliberately not bound. The local tombstone remains persisted;
    // resolution must reload the peer binding and fail closed on this stale
    // identity rather than trusting its old record or shared origin/tree.
    let peer_descriptor = clones.right.join(".aethyme/project-descriptor.json");
    fs::remove_file(&peer_descriptor).unwrap();
    let current = crate::collaboration_archive::pin_commit(&clones.right, "HEAD").unwrap();
    let current_text = current.as_str();
    let replacement = project_command_for_test(
        &clones.right,
        &clones.right,
        &["enroll", "--commit", current_text, "--write"],
    )
    .unwrap();
    assert!(replacement["descriptor_record_id"].as_str().is_some());
    command(&clones.right, &["add", "-A"]);
    command(
        &clones.right,
        &["commit", "-qm", "propose an unbound descriptor"],
    );
    let evidence_refs: Vec<&[u8]> = evidence.iter().map(Vec::as_slice).collect();
    assert_eq!(
        resolve_associated_mapping(
            &clones.left,
            &clones.right,
            &RecordId::parse(&association_id).unwrap(),
            &profile,
            &claim,
            &evidence_refs,
        ),
        MappingResolution::Unknown("peer_binding_unavailable")
    );
}

#[test]
fn identical_local_names_need_document_and_producer_scope_and_package_versions_survive() {
    let project = ProjectId::parse(PROJECT).unwrap();
    let clone = rid("scope-clone");
    let (doc_one, _) = canonical_record(json!({
        "schema": "aethyme.symbol-ref/experimental-v0",
        "project_id": project.as_str(), "clone_id": clone.as_str(),
        "scope": { "kind": "producer_local", "producer_namespace": "producer-a", "document": "src/one.ts", "local_symbol": "same" }
    }));
    let (doc_two, _) = canonical_record(json!({
        "schema": "aethyme.symbol-ref/experimental-v0",
        "project_id": project.as_str(), "clone_id": clone.as_str(),
        "scope": { "kind": "producer_local", "producer_namespace": "producer-a", "document": "src/two.ts", "local_symbol": "same" }
    }));
    let (producer_two, _) = canonical_record(json!({
        "schema": "aethyme.symbol-ref/experimental-v0",
        "project_id": project.as_str(), "clone_id": clone.as_str(),
        "scope": { "kind": "producer_local", "producer_namespace": "producer-b", "document": "src/one.ts", "local_symbol": "same" }
    }));
    assert_ne!(doc_one.id(), doc_two.id());
    assert_ne!(doc_one.id(), producer_two.id());

    let external = |version: &str| {
        canonical_record(json!({
            "schema": "aethyme.symbol-ref/experimental-v0",
            "project_id": project.as_str(), "clone_id": clone.as_str(),
            "scope": { "kind": "external_package", "producer_namespace": "ts-resolver", "package": "pkg", "version": version, "symbol": "Thing" }
        }))
        .0
    };
    assert_ne!(external("1.0.0").id(), external("2.0.0").id());
}

#[test]
fn path_moves_renames_copies_and_historical_overload_collisions_stay_unknown() {
    let clones = actual_clones();
    let fixture = mapping_fixture(&clones, RecordScenario::Exact);
    let overloaded = mapping_fixture_named(&clones, RecordScenario::Exact, "convert");
    let old_move = node_ids(&clones.right, "src/lib.ts", "move_me");
    let old_rename = node_ids(&clones.right, "src/lib.ts", "rename_me");
    let old_copy = node_ids(&clones.right, "src/lib.ts", "copy_me");
    let source = fs::read_to_string(clones.right.join("src/lib.ts")).unwrap();
    let renamed = source
        .replace("export function rename_me(", "export function renamed(")
        .replace("export function copy_me(", "");
    fs::write(clones.right.join("src/api.ts"), renamed).unwrap();
    fs::write(
        clones.right.join("src/lib.ts"),
        "export function stable(value: string) { return value; }\n",
    )
    .unwrap();
    fs::write(
        clones.right.join("src/copied.ts"),
        "export function copy_me(value: string) { return value; }\n",
    )
    .unwrap();
    command(&clones.right, &["add", "-A"]);
    command(&clones.right, &["commit", "-qm", "move rename and copy"]);
    assert_ne!(old_move, node_ids(&clones.right, "src/api.ts", "move_me"));
    assert_ne!(old_rename, node_ids(&clones.right, "src/api.ts", "renamed"));
    assert_ne!(
        old_copy,
        node_ids(&clones.right, "src/copied.ts", "copy_me")
    );
    let refreshed = CloneSnapshotBinding::from_snapshots_for_test(
        rid("association-clone-a-to-b"),
        ProjectId::parse(PROJECT).unwrap(),
        ProjectId::parse(PEER_PROJECT).unwrap(),
        CapturedCloneSnapshot::new(
            rid("clone-a"),
            repo_component(&clones.left),
            &source_snapshot(&clones.left),
        ),
        CapturedCloneSnapshot::new(
            rid("clone-b"),
            repo_component(&clones.right),
            &source_snapshot(&clones.right),
        ),
        RecordId::parse(PROFILE).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        resolve_within_binding(
            &refreshed,
            &fixture.claim,
            &fixture
                .records
                .iter()
                .map(Vec::as_slice)
                .collect::<Vec<_>>()
        ),
        MappingResolution::Unknown("captured_clone_binding_mismatch")
    ));

    let lib = fs::read_to_string(clones.left.join("src/lib.ts")).unwrap();
    let overload_ids: Vec<_> = function_declarations(&lib)
        .into_iter()
        .filter(|(name, _)| name == "convert")
        .map(|_| node_ids(&clones.left, "src/lib.ts", "convert"))
        .collect();
    assert_eq!(
        overload_ids.len(),
        3,
        "synthetic Playground has three overloads"
    );
    assert!(overload_ids.windows(2).all(|pair| pair[0] == pair[1]));
    let claim = claim_with_state(&overloaded.claim, "unknown");
    let records: Vec<&[u8]> = overloaded.records.iter().map(Vec::as_slice).collect();
    assert!(matches!(
        resolve_within_binding(&overloaded.binding, &claim, &records),
        MappingResolution::Unknown("claim_not_proposed")
    ));
}

#[test]
fn mismatched_or_incomplete_mapping_records_never_establish() {
    let clones = actual_clones();
    for (scenario, expected) in [
        (RecordScenario::CrossProject, "binding_scope_mismatch"),
        (RecordScenario::SameClone, "invalid_evidence_record"),
        (
            RecordScenario::CloneBindingMismatch,
            "captured_clone_binding_mismatch",
        ),
        (
            RecordScenario::SourceMismatch,
            "captured_clone_binding_mismatch",
        ),
        (
            RecordScenario::ProfileMismatch,
            "captured_clone_binding_mismatch",
        ),
        (RecordScenario::OriginMismatch, "origin_binding_mismatch"),
        (RecordScenario::MalformedNodeId, "symbol_scope_mismatch"),
        (RecordScenario::WrongNodeIdRepo, "symbol_scope_mismatch"),
        (RecordScenario::UnknownScope, "symbol_scope_mismatch"),
        (RecordScenario::AliasedOrigin, "invalid_evidence_record"),
        (
            RecordScenario::ExternalPackageVersionMismatch,
            "cross_version_mapping_unproven",
        ),
    ] {
        let fixture = mapping_fixture(&clones, scenario);
        let records: Vec<&[u8]> = fixture.records.iter().map(Vec::as_slice).collect();
        assert_eq!(
            resolve_within_binding(&fixture.binding, &fixture.claim, &records),
            MappingResolution::Unknown(expected),
            "{scenario:?}"
        );
    }

    let fixture = mapping_fixture(&clones, RecordScenario::Exact);
    let records: Vec<&[u8]> = fixture.records.iter().map(Vec::as_slice).collect();
    let right_observation = decode_symbol_identity_record(&fixture.records[2]).unwrap();
    let right_observation_id = right_observation.id();
    let mut wrong_evidence_claim: Value = serde_json::from_slice(&fixture.claim).unwrap();
    let first_origin = wrong_evidence_claim["evidence"][0]
        .as_str()
        .unwrap()
        .to_owned();
    let mut wrong_evidence_refs = [first_origin, right_observation_id.as_str().to_owned()];
    wrong_evidence_refs.sort();
    wrong_evidence_claim["evidence"] = json!(wrong_evidence_refs);
    let wrong_evidence_claim = serde_json::to_vec(&wrong_evidence_claim).unwrap();
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &wrong_evidence_claim, &records),
        MappingResolution::Unknown("evidence_set_not_exact")
    ));

    let malformed: &[u8] = b"not-json";
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &fixture.claim, &[malformed]),
        MappingResolution::Unknown("invalid_evidence_record")
    ));
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &fixture.claim, &[]),
        MappingResolution::Unknown("missing_binding_evidence")
    ));
    let missing_origin = &records[..records.len() - 1];
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &fixture.claim, missing_origin),
        MappingResolution::Unknown("missing_right_origin")
    ));
    let mut empty_claim: Value = serde_json::from_slice(&fixture.claim).unwrap();
    empty_claim["evidence"] = json!([]);
    let empty_claim = serde_json::to_vec(&empty_claim).unwrap();
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &empty_claim, &records),
        MappingResolution::Unknown("invalid_or_non_claim_record")
    ));
    let mut duplicate_claim: Value = serde_json::from_slice(&fixture.claim).unwrap();
    let first_origin = duplicate_claim["evidence"][0].clone();
    duplicate_claim["evidence"] = json!([first_origin.clone(), first_origin]);
    let duplicate_claim = serde_json::to_vec(&duplicate_claim).unwrap();
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &duplicate_claim, &records),
        MappingResolution::Unknown("invalid_or_non_claim_record")
    ));
    let mut duplicate = records[..records.len() - 1].to_vec();
    duplicate.push(records[0]);
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &fixture.claim, &duplicate),
        MappingResolution::Unknown("duplicate_evidence_record")
    ));

    let oversized = vec![malformed; 8];
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &fixture.claim, &oversized),
        MappingResolution::Unknown("evidence_set_not_exact")
    ));

    let established_wire = claim_with_state(&fixture.claim, "established");
    assert!(matches!(
        resolve_within_binding(&fixture.binding, &established_wire, &records),
        MappingResolution::Unknown("claim_not_proposed")
    ));

    let decoded = decode_symbol_identity_record(&fixture.claim).unwrap();
    assert!(matches!(
        decoded.state("state"),
        StateReading::Known("proposed")
    ));
}
