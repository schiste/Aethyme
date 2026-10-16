//! Public, bounded CLI surface for appending one exact capture-operation revision.
use std::path::Path;

use aethyme_contracts::experimental_v0::brief::Brief;
use aethyme_contracts::experimental_v0::canonical_json::{self, Object, Value};
use aethyme_contracts::experimental_v0::{FileOperation, RecordId};
use serde_json::json;

use crate::collaboration_capture::OperationId;
use crate::collaboration_identity_store::{self as identity_store, ContributionRevisionRequest};
use crate::collaboration_state::{CollaborationRoot, CollaborationStore};

use super::{Failure, Output};

const SCHEMA: &str = "aethyme.collab-contribution-revise/experimental-v0";
const FILE_OPERATIONS_SCHEMA: &str = "aethyme.collab-file-operations/experimental-v0";
const MAX_FILE_OPERATIONS_BYTES: u64 = canonical_json::MAX_DOCUMENT_BYTES as u64;
const MAX_DEPENDENCIES: usize = canonical_json::MAX_CONTAINER_ENTRIES;

pub(super) fn revise(main_root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    revise_with_root(main_root, rest, None)
}

#[cfg(test)]
fn revise_at_root(
    main_root: &Path,
    rest: &[&str],
    root: &CollaborationRoot,
) -> Result<Output, Failure> {
    revise_with_root(main_root, rest, Some(root))
}

fn revise_with_root(
    main_root: &Path,
    rest: &[&str],
    explicit_root: Option<&CollaborationRoot>,
) -> Result<Output, Failure> {
    let parsed = parse_args(rest)?;
    let file_operations = parsed
        .file_operations
        .map(parse_file_operations)
        .transpose()?;
    let brief = parsed.brief_file.map(read_brief).transpose()?;

    let enabled = super::require_enabled(main_root)?;
    let mut store = if let Some(root) = explicit_root {
        CollaborationStore::open(root, &enabled.project, &[])
            .map_err(|error| Failure::domain(error.code(), error.to_string()))?
    } else {
        super::open(main_root)?
    };
    let binding = identity_store::resolve_approved_project_binding(&store)
        .map_err(|error| Failure::domain(error.code(), error.to_string()))?
        .ok_or_else(|| {
            Failure::refused(
                "project_unbound",
                "this local collaboration store has no explicitly approved project descriptor binding",
                "run aethyme collab project bind --confirm <descriptor-record-id> after committing the reviewed descriptor pointer",
            )
        })?;

    let revision = identity_store::revise_contribution_for_operation(
        &mut store,
        &binding,
        ContributionRevisionRequest {
            operation_id: &parsed.operation_id,
            file_operations: file_operations.as_deref(),
            brief: brief.as_ref(),
            depends_on: parsed.depends_on.as_deref(),
        },
    )
    .map_err(|error| Failure::domain(error.code(), error.to_string()))?;

    let brief_record_id = revision
        .revision()
        .brief()
        .map(|reference| reference.record_id.as_str());
    let value = json!({
        "schema": SCHEMA,
        "project_id": binding.project_id().as_str(),
        "descriptor_record_id": binding.descriptor_record_id().as_str(),
        "operation_id": parsed.operation_id.as_str(),
        "contribution_id": revision.revision().contribution_id().as_str(),
        "capture_record_id": revision.capture_lineage().as_str(),
        "revision_record_id": revision.record_id().as_str(),
        "revision_object_sha256": revision.object_digest().hex(),
        "base_snapshot": revision.revision().base().as_str(),
        "result_snapshot": revision.revision().result().as_str(),
        "brief_record_id": brief_record_id,
        "depends_on": revision.revision().depends_on().iter().map(RecordId::as_str).collect::<Vec<_>>(),
        "source_change_count": revision.revision().source_changes().len(),
        "identity_proof_revision_ids": revision.identity_proof_revision_ids().iter().map(RecordId::as_str).collect::<Vec<_>>(),
    });
    let text = format!(
        "revision {} for contribution {}\noperation {}\ncapture {}\nbase {}\nresult {}\nbrief {}\n",
        revision.record_id().as_str(),
        revision.revision().contribution_id().as_str(),
        parsed.operation_id.as_str(),
        revision.capture_lineage().as_str(),
        revision.revision().base().as_str(),
        revision.revision().result().as_str(),
        brief_record_id.unwrap_or("none"),
    );
    Ok((value, text))
}

struct ParsedArgs<'a> {
    operation_id: OperationId,
    brief_file: Option<&'a str>,
    file_operations: Option<&'a str>,
    depends_on: Option<Vec<RecordId>>,
}

fn parse_args<'a>(rest: &'a [&'a str]) -> Result<ParsedArgs<'a>, Failure> {
    let mut operation = None;
    let mut brief_file = None;
    let mut file_operations = None;
    let mut depends_on = Vec::new();
    let mut clear_dependencies = false;
    let mut index = 0;

    while index < rest.len() {
        let flag = rest[index];
        match flag {
            "--operation" => {
                if operation.is_some() {
                    return Err(Failure::usage("duplicate --operation"));
                }
                let value = argument_value(rest, &mut index, flag)?;
                operation =
                    Some(OperationId::parse(value).map_err(|error| {
                        Failure::usage(format!("invalid --operation: {error}"))
                    })?);
            }
            "--brief-file" => {
                if brief_file.is_some() {
                    return Err(Failure::usage("duplicate --brief-file"));
                }
                brief_file = Some(argument_value(rest, &mut index, flag)?);
            }
            "--file-operations" => {
                if file_operations.is_some() {
                    return Err(Failure::usage("duplicate --file-operations"));
                }
                file_operations = Some(argument_value(rest, &mut index, flag)?);
            }
            "--depends-on" => {
                if depends_on.len() >= MAX_DEPENDENCIES {
                    return Err(Failure::usage("too many --depends-on values"));
                }
                let value = argument_value(rest, &mut index, flag)?;
                let record_id = RecordId::parse(value).map_err(|error| {
                    Failure::refused(
                        "invalid_record_id",
                        format!("{value}: {error}"),
                        "pass a valid immutable dependency RecordId",
                    )
                })?;
                if depends_on
                    .iter()
                    .any(|existing: &RecordId| existing == &record_id)
                {
                    return Err(Failure::usage(format!(
                        "duplicate --depends-on value {value}"
                    )));
                }
                depends_on.push(record_id);
            }
            "--clear-dependencies" => {
                if clear_dependencies {
                    return Err(Failure::usage("duplicate --clear-dependencies"));
                }
                clear_dependencies = true;
                index += 1;
            }
            unexpected if unexpected.starts_with("--") => {
                return Err(Failure::usage(format!("unknown flag {unexpected}")));
            }
            _ => return Err(Failure::usage("unexpected positional argument")),
        }
    }

    if clear_dependencies && !depends_on.is_empty() {
        return Err(Failure::usage(
            "--clear-dependencies cannot be combined with --depends-on",
        ));
    }
    let operation_id = operation.ok_or_else(|| Failure::usage("--operation <id> is required"))?;
    let depends_on = if clear_dependencies {
        Some(Vec::new())
    } else if depends_on.is_empty() {
        None
    } else {
        Some(depends_on)
    };
    Ok(ParsedArgs {
        operation_id,
        brief_file,
        file_operations,
        depends_on,
    })
}

fn argument_value<'a>(rest: &'a [&str], index: &mut usize, flag: &str) -> Result<&'a str, Failure> {
    let value = rest
        .get(*index + 1)
        .copied()
        .filter(|value| !value.starts_with("--"))
        .ok_or_else(|| Failure::usage(format!("{flag} needs a value")))?;
    *index += 2;
    Ok(value)
}

fn read_brief(path: &str) -> Result<Brief, Failure> {
    let bytes = super::read_capped(path, super::MAX_BRIEF_FILE_BYTES, "decision file")?;
    Brief::from_decision_file(&bytes).map_err(|errors| {
        Failure::refused(
            "invalid_brief",
            format!("{path}: {errors}"),
            "fix the decision file; the message lists every problem",
        )
    })
}

fn parse_file_operations(path: &str) -> Result<Vec<FileOperation>, Failure> {
    let bytes = super::read_capped(path, MAX_FILE_OPERATIONS_BYTES, "file operation document")?;
    let value = canonical_json::parse(&bytes)
        .map_err(|error| invalid_file_operations(error.to_string()))?;
    let Value::Object(document) = value else {
        return Err(invalid_file_operations("the document must be an object"));
    };
    exact_keys(
        &document,
        &["schema", "operations"],
        "file operation document",
    )?;
    if string_field(&document, "schema")? != FILE_OPERATIONS_SCHEMA {
        return Err(invalid_file_operations(format!(
            "schema must be {FILE_OPERATIONS_SCHEMA}"
        )));
    }
    let operations = match document.get("operations") {
        Some(Value::Array(operations)) => operations,
        _ => return Err(invalid_file_operations("operations must be an array")),
    };
    let mut parsed = Vec::with_capacity(operations.len());
    for (index, value) in operations.iter().enumerate() {
        let Value::Object(object) = value else {
            return Err(invalid_file_operations(format!(
                "operations[{index}] must be an object"
            )));
        };
        let kind = string_field(object, "kind")?;
        match kind {
            "rename" | "copy" => {
                exact_keys(object, &["kind", "from_hex", "to_hex"], "file operation")?;
                let from = decode_path_hex(string_field(object, "from_hex")?)?;
                let to = decode_path_hex(string_field(object, "to_hex")?)?;
                parsed.push(if kind == "rename" {
                    FileOperation::Rename { from, to }
                } else {
                    FileOperation::Copy { from, to }
                });
            }
            "split" => {
                exact_keys(object, &["kind", "from_hex", "to_hex"], "split operation")?;
                let from = decode_path_hex(string_field(object, "from_hex")?)?;
                let to_values = match object.get("to_hex") {
                    Some(Value::Array(paths)) => paths,
                    _ => {
                        return Err(invalid_file_operations(
                            "split to_hex must be an array of lowercase hex paths",
                        ));
                    }
                };
                let to = to_values
                    .iter()
                    .map(|value| match value {
                        Value::String(path) => decode_path_hex(path),
                        _ => Err(invalid_file_operations(
                            "split to_hex entries must be lowercase hex strings",
                        )),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                parsed.push(FileOperation::Split { from, to });
            }
            _ => {
                return Err(invalid_file_operations(format!(
                    "operations[{index}].kind must be rename, copy or split"
                )));
            }
        }
    }
    Ok(parsed)
}

fn exact_keys(object: &Object, required: &[&str], what: &str) -> Result<(), Failure> {
    for (name, _) in object.iter() {
        if !required.contains(&name) {
            return Err(invalid_file_operations(format!(
                "{what} has unsupported field {name:?}"
            )));
        }
    }
    for name in required {
        if object.get(name).is_none() {
            return Err(invalid_file_operations(format!(
                "{what} is missing required field {name:?}"
            )));
        }
    }
    Ok(())
}

fn string_field<'a>(object: &'a Object, name: &str) -> Result<&'a str, Failure> {
    match object.get(name) {
        Some(Value::String(value)) => Ok(value),
        _ => Err(invalid_file_operations(format!("{name} must be a string"))),
    }
}

fn decode_path_hex(text: &str) -> Result<Vec<u8>, Failure> {
    if text.is_empty() || !text.len().is_multiple_of(2) {
        return Err(invalid_file_operations(
            "path_hex must be nonempty lowercase hex with an even number of digits",
        ));
    }
    let mut decoded = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
        let high = lower_hex_nibble(pair[0]).ok_or_else(|| {
            invalid_file_operations("path_hex must contain only lowercase hexadecimal digits")
        })?;
        let low = lower_hex_nibble(pair[1]).ok_or_else(|| {
            invalid_file_operations("path_hex must contain only lowercase hexadecimal digits")
        })?;
        decoded.push((high << 4) | low);
    }
    Ok(decoded)
}

fn lower_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn invalid_file_operations(message: impl Into<String>) -> Failure {
    Failure::refused(
        "invalid_file_operations",
        message,
        "provide the bounded experimental file-operation JSON described by aethyme collab --help",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_cli::project_test_support::BoundProjectFixture;
    use crate::collaboration_identity_store::{self as identity_store, IdentityStateResolution};
    use crate::collaboration_state::CollaborationRoot;
    use aethyme_contracts::experimental_v0::ContributionId;
    use std::process::Command;

    fn run_at_fixture(
        fixture: &BoundProjectFixture,
        root: &CollaborationRoot,
        args: &[&str],
    ) -> Result<Output, Failure> {
        revise_at_root(fixture.repo_path(), args, root)
    }

    fn current_head(fixture: &BoundProjectFixture, contribution_id: &str) -> String {
        fixture.store().read_connection().query_row(
            "SELECT revision_record_id FROM contribution_revision_heads WHERE contribution_id=?1",
            [contribution_id],
            |row| row.get(0),
        ).expect("contribution revision head")
    }

    fn resolve_output(
        fixture: &BoundProjectFixture,
        output: &serde_json::Value,
    ) -> identity_store::ResolvedContributionRevision {
        let record_id = RecordId::parse(output["revision_record_id"].as_str().unwrap()).unwrap();
        let digest = crate::collaboration_archive::parse_digest(
            output["revision_object_sha256"].as_str().unwrap(),
        )
        .unwrap();
        let contribution_id =
            ContributionId::parse(output["contribution_id"].as_str().unwrap()).unwrap();
        identity_store::resolve_bound_contribution_revision(
            fixture.store(),
            &fixture.binding,
            &record_id,
            digest,
            &contribution_id,
        )
        .expect("resolve exact public revision")
    }

    fn git(repo: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Aethyme Fixture",
                "-c",
                "user.email=fixture@aethyme.invalid",
            ])
            .args(args)
            .current_dir(repo)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn public_handler_creates_rationale_only_revisions_and_old_retry_does_not_rewind_head() {
        let mut fixture = BoundProjectFixture::new().expect("real project enrollment and bind");
        let base = fixture.bootstrap_commit.clone();
        let result = fixture
            .commit_file("a.txt", b"updated after approved binding\n")
            .expect("commit result snapshot");
        fixture
            .capture_operation("op-revision-cli", &base, &result)
            .expect("committed capture operation");
        let root = CollaborationRoot::under_host_state(fixture.host.path());
        let files = tempfile::tempdir().expect("decision files");
        let brief_a = files.path().join("brief-a.json");
        let brief_b = files.path().join("brief-b.json");
        std::fs::write(&brief_a, br#"{"intent":"The first rationale."}"#)
            .expect("write first decision");
        std::fs::write(&brief_b, br#"{"intent":"A revised rationale."}"#)
            .expect("write revised decision");
        let brief_a = brief_a.to_str().expect("UTF-8 fixture path");
        let brief_b = brief_b.to_str().expect("UTF-8 fixture path");

        let first = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-cli", "--brief-file", brief_a],
        )
        .expect("first public revision")
        .0;
        let first_revision = first["revision_record_id"]
            .as_str()
            .expect("revision ID")
            .to_string();
        let contribution_id = first["contribution_id"]
            .as_str()
            .expect("logical contribution ID")
            .to_string();
        assert_eq!(current_head(&fixture, &contribution_id), first_revision);

        let revised = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-cli", "--brief-file", brief_b],
        )
        .expect("rationale-only revision")
        .0;
        let revised_id = revised["revision_record_id"]
            .as_str()
            .expect("revision ID")
            .to_string();
        assert_ne!(first_revision, revised_id);
        assert_eq!(revised["contribution_id"], contribution_id);
        assert_eq!(first["base_snapshot"], revised["base_snapshot"]);
        assert_eq!(first["result_snapshot"], revised["result_snapshot"]);
        assert_eq!(first["source_change_count"], revised["source_change_count"]);
        assert_ne!(first["brief_record_id"], revised["brief_record_id"]);
        assert_eq!(current_head(&fixture, &contribution_id), revised_id);

        let retry_old = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-cli", "--brief-file", brief_a],
        )
        .expect("exact retry of the first immutable revision")
        .0;
        assert_eq!(retry_old["revision_record_id"], first_revision);
        assert_eq!(current_head(&fixture, &contribution_id), revised_id);

        let retry_current = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-cli", "--brief-file", brief_b],
        )
        .expect("exact retry of current immutable revision")
        .0;
        assert_eq!(retry_current["revision_record_id"], revised_id);
        assert_eq!(current_head(&fixture, &contribution_id), revised_id);
    }

    #[test]
    fn two_real_clone_bindings_resolve_the_same_bootstrap_file_identity_map() {
        let fixture = BoundProjectFixture::new().expect("real project enrollment and bind");
        let clone = fixture.clone_and_bind().expect("second clone binding");
        assert_eq!(fixture.binding.project_id(), clone.binding.project_id());
        let original = identity_store::resolve_project_file_identity_state(
            fixture.store(),
            &fixture.binding,
            &fixture.bootstrap_snapshot,
        )
        .expect("original bootstrap map");
        let cloned = identity_store::resolve_project_file_identity_state(
            &clone.store,
            &clone.binding,
            &clone.bootstrap_snapshot,
        )
        .expect("clone bootstrap map");
        let original = match original {
            IdentityStateResolution::Bootstrap(state) => state,
            other => panic!("expected bootstrap map, got {other:?}"),
        };
        let cloned = match cloned {
            IdentityStateResolution::Bootstrap(state) => state,
            other => panic!("expected bootstrap map, got {other:?}"),
        };
        assert_eq!(original.snapshot_id(), cloned.snapshot_id());
        assert_eq!(original.state(), cloned.state());
    }

    #[test]
    fn public_handler_applies_valid_rename_and_exact_dependency_updates() {
        let mut fixture = BoundProjectFixture::new().expect("real project enrollment and bind");
        let root = CollaborationRoot::under_host_state(fixture.host.path());
        let base = fixture.bootstrap_commit.clone();

        fixture
            .capture_operation("op-revision-dependency", &base, &base)
            .expect("committed dependency capture operation");
        let dependency =
            run_at_fixture(&fixture, &root, &["--operation", "op-revision-dependency"])
                .expect("public prerequisite revision")
                .0;
        let dependency_id = dependency["revision_record_id"]
            .as_str()
            .expect("prerequisite revision RecordId")
            .to_string();

        std::fs::rename(
            fixture.repo_path().join("a.txt"),
            fixture.repo_path().join("b.txt"),
        )
        .expect("rename tracked file");
        git(fixture.repo_path(), &["add", "--all"]);
        git(
            fixture.repo_path(),
            &["commit", "-q", "-m", "fixture rename"],
        );
        let result = crate::collaboration_archive::pin_commit(fixture.repo_path(), "HEAD")
            .expect("pin rename result");
        fixture
            .capture_operation("op-revision-rename", &base, &result)
            .expect("committed rename capture operation");

        let files = tempfile::tempdir().expect("file-operation fixture");
        let operations = files.path().join("operations.json");
        std::fs::write(
            &operations,
            br#"{"operations":[{"from_hex":"612e747874","kind":"rename","to_hex":"622e747874"}],"schema":"aethyme.collab-file-operations/experimental-v0"}"#,
        )
        .expect("write valid rename operation");
        let operations = operations.to_str().expect("UTF-8 fixture path");

        let renamed = run_at_fixture(
            &fixture,
            &root,
            &[
                "--operation",
                "op-revision-rename",
                "--file-operations",
                operations,
                "--depends-on",
                &dependency_id,
            ],
        )
        .expect("public valid rename revision")
        .0;
        assert_eq!(renamed["depends_on"], json!([dependency_id]));
        let resolved = resolve_output(&fixture, &renamed);
        let rename = resolved
            .identity_transition()
            .changes()
            .iter()
            .find(|change| {
                change
                    .after
                    .as_ref()
                    .is_some_and(|after| after.path.as_slice() == b"b.txt")
            })
            .expect("destination rename transition");
        assert_eq!(rename.relation.as_str(), "explicit_rename");
        assert_eq!(
            resolved.base_state().state().file_id(b"a.txt"),
            resolved.result_state().state().file_id(b"b.txt")
        );

        let cleared = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-rename", "--clear-dependencies"],
        )
        .expect("public explicit dependency clear")
        .0;
        assert_eq!(cleared["depends_on"], json!([]));
        assert_eq!(cleared["contribution_id"], renamed["contribution_id"]);
        assert_ne!(cleared["revision_record_id"], renamed["revision_record_id"]);
        assert_eq!(
            current_head(&fixture, renamed["contribution_id"].as_str().unwrap()),
            cleared["revision_record_id"].as_str().unwrap()
        );
    }

    #[test]
    fn an_old_omitted_fields_retry_inherits_the_current_operations_and_dependencies() {
        let mut fixture = BoundProjectFixture::new().expect("real project enrollment and bind");
        let root = CollaborationRoot::under_host_state(fixture.host.path());
        let base = fixture.bootstrap_commit.clone();
        fixture
            .capture_operation("op-revision-hybrid-dependency", &base, &base)
            .expect("dependency capture operation");
        let dependency = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-hybrid-dependency"],
        )
        .expect("real dependency revision")
        .0["revision_record_id"]
            .as_str()
            .expect("dependency RecordId")
            .to_string();

        std::fs::rename(
            fixture.repo_path().join("a.txt"),
            fixture.repo_path().join("b.txt"),
        )
        .expect("rename tracked source");
        git(fixture.repo_path(), &["add", "--all"]);
        git(
            fixture.repo_path(),
            &["commit", "-q", "-m", "hybrid retry rename"],
        );
        let result = crate::collaboration_archive::pin_commit(fixture.repo_path(), "HEAD")
            .expect("pin renamed result");
        fixture
            .capture_operation("op-revision-hybrid", &base, &result)
            .expect("retained operation for the renamed source");

        let files = tempfile::tempdir().expect("revision input files");
        let brief_a = files.path().join("brief-a.json");
        let brief_b = files.path().join("brief-b.json");
        let operations = files.path().join("operations.json");
        std::fs::write(&brief_a, br#"{"intent":"Initial rationale."}"#).unwrap();
        std::fs::write(&brief_b, br#"{"intent":"New current rationale."}"#).unwrap();
        std::fs::write(
            &operations,
            br#"{"operations":[{"from_hex":"612e747874","kind":"rename","to_hex":"622e747874"}],"schema":"aethyme.collab-file-operations/experimental-v0"}"#,
        )
        .unwrap();
        let brief_a = brief_a.to_str().unwrap();
        let brief_b = brief_b.to_str().unwrap();
        let operations = operations.to_str().unwrap();

        let initial = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-hybrid", "--brief-file", brief_a],
        )
        .expect("initial revision with omitted operation/dependency fields")
        .0;
        assert_eq!(initial["depends_on"], json!([]));

        let current = run_at_fixture(
            &fixture,
            &root,
            &[
                "--operation",
                "op-revision-hybrid",
                "--brief-file",
                brief_b,
                "--file-operations",
                operations,
                "--depends-on",
                &dependency,
            ],
        )
        .expect("revision that changes current operation/dependency fields")
        .0;
        assert_eq!(current["depends_on"], json!([dependency]));
        assert_ne!(current["revision_record_id"], initial["revision_record_id"]);

        let retried_old_input = run_at_fixture(
            &fixture,
            &root,
            &["--operation", "op-revision-hybrid", "--brief-file", brief_a],
        )
        .expect("an omitted-field retry inherits the current head")
        .0;
        assert_eq!(
            retried_old_input["brief_record_id"],
            initial["brief_record_id"]
        );
        assert_eq!(retried_old_input["depends_on"], json!([dependency]));
        assert_ne!(
            retried_old_input["revision_record_id"], initial["revision_record_id"],
            "the old command input does not pin the omitted fields to its historical revision"
        );
        assert_ne!(
            retried_old_input["revision_record_id"],
            current["revision_record_id"]
        );
        let resolved = resolve_output(&fixture, &retried_old_input);
        assert!(
            resolved
                .identity_transition()
                .changes()
                .iter()
                .any(|change| change.relation.as_str() == "explicit_rename")
        );
        assert_eq!(
            current_head(
                &fixture,
                retried_old_input["contribution_id"].as_str().unwrap()
            ),
            retried_old_input["revision_record_id"].as_str().unwrap()
        );
    }

    #[test]
    fn distinct_capture_operations_for_the_same_pair_mint_distinct_contributions() {
        let mut fixture = BoundProjectFixture::new().expect("real project enrollment and bind");
        let root = CollaborationRoot::under_host_state(fixture.host.path());
        let same = fixture.bootstrap_commit.clone();
        fixture
            .capture_operation("op-revision-same-pair-a", &same, &same)
            .expect("first same-pair capture operation");
        fixture
            .capture_operation("op-revision-same-pair-b", &same, &same)
            .expect("second same-pair capture operation");

        let first = run_at_fixture(&fixture, &root, &["--operation", "op-revision-same-pair-a"])
            .expect("first same-pair logical contribution")
            .0;
        let second = run_at_fixture(&fixture, &root, &["--operation", "op-revision-same-pair-b"])
            .expect("second same-pair logical contribution")
            .0;

        assert_eq!(first["capture_record_id"], second["capture_record_id"]);
        assert_ne!(first["contribution_id"], second["contribution_id"]);
        assert_ne!(first["revision_record_id"], second["revision_record_id"]);
        assert_eq!(first["base_snapshot"], second["base_snapshot"]);
        assert_eq!(first["result_snapshot"], second["result_snapshot"]);
    }

    #[test]
    fn malformed_source_operations_and_dependencies_are_rejected_before_store_access() {
        let fixture = BoundProjectFixture::new().expect("real project enrollment and bind");
        let root = CollaborationRoot::under_host_state(fixture.host.path());
        let files = tempfile::tempdir().expect("operation file");
        let operations = files.path().join("operations.json");
        std::fs::write(&operations,
            br#"{"schema":"aethyme.collab-file-operations/experimental-v0","operations":[{"kind":"rename","from_hex":"61","to_hex":"6G"}]}"#,
        ).expect("write malformed operation");
        let operations = operations.to_str().expect("UTF-8 fixture path");
        let bad_operation = run_at_fixture(
            &fixture,
            &root,
            &[
                "--operation",
                "op-revision-cli",
                "--file-operations",
                operations,
            ],
        )
        .expect_err("invalid path hex is refused");
        assert_eq!(bad_operation.code, "invalid_file_operations");

        let bad_dependency = run_at_fixture(
            &fixture,
            &root,
            &[
                "--operation",
                "op-revision-cli",
                "--depends-on",
                "not-a-record",
            ],
        )
        .expect_err("invalid dependency ID is refused");
        assert_eq!(bad_dependency.code, "invalid_record_id");
        let revisions: i64 = fixture
            .store()
            .read_connection()
            .query_row("SELECT count(*) FROM contribution_revisions", [], |row| {
                row.get(0)
            })
            .expect("read unchanged revision history");
        assert_eq!(revisions, 0);
    }
}
