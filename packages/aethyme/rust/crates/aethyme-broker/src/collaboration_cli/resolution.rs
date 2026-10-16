//! Explicit, local-only handoff of one durably charged resolution attempt.
//! Session labels are provenance; these commands neither dispatch an agent
//! nor authenticate one, and an imported proposal is never accepted here.
use super::lifecycle::{
    Flags, existing_owner, observe, read_operator_bytes, target_error, version,
};
use super::{Failure, Output, open};
use crate::collaboration_archive::ObjectDigest;
use crate::resolution::{
    self as resolution_api, BeginResolutionAttempt, DispatchLimits, RecordRef,
};
use crate::{local_target, local_wave};
use aethyme_contracts::experimental_v0::{EntryKind, RecordId, SourceSnapshotId};
use serde_json::{Value, json};
use std::path::Path;

const MAX_LIMITS_DOCUMENT: u64 = 16 * 1024;

fn error(error: resolution_api::ResolutionError) -> Failure {
    Failure::domain(error.code(), error.to_string())
}

fn wave_error(error: local_wave::WaveError) -> Failure {
    use local_wave::WaveError;
    match error {
        WaveError::Target(error) => target_error(error),
        WaveError::Resolution(error) => self::error(*error),
        WaveError::ResolutionUnsupported => {
            Failure::domain("unsupported_profile", error.to_string())
        }
        WaveError::Io(error) => Failure::domain("io", error.to_string()),
        WaveError::Sqlite(error) => Failure::domain("sqlite", error.to_string()),
        WaveError::Stale | WaveError::Invalid(_) | WaveError::OutcomeUnknown => {
            Failure::domain("resolution_wave_refused", error.to_string())
        }
        _ => Failure::domain("failed", error.to_string()),
    }
}

fn output(schema: &str, mut value: Value) -> Result<Output, Failure> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| Failure::domain("state", "resolution output is not an object".into()))?;
    object.insert("schema".into(), json!(schema));
    object.insert("dispatches_agent".into(), json!(false));
    object.insert("grants_acceptance".into(), json!(false));
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| Failure::domain("state", error.to_string()))?;
    Ok((value, format!("{text}\n")))
}

fn request(flags: &Flags<'_>) -> Result<RecordRef, Failure> {
    let id = RecordId::parse(flags.one("--request")?)
        .map_err(|error| Failure::usage(error.to_string()))?;
    let object = SourceSnapshotId::parse(flags.one("--object")?)
        .map_err(|error| Failure::usage(error.to_string()))?;
    Ok(RecordRef {
        id,
        object: ObjectDigest::of_snapshot(&object),
    })
}

struct CurrentWave<'a> {
    target: &'a str,
    issuer: &'a str,
    wave: RecordId,
    version: i64,
}

impl<'a> CurrentWave<'a> {
    fn parse(flags: &Flags<'a>) -> Result<Self, Failure> {
        Ok(Self {
            target: flags.one("--target")?,
            issuer: flags.one("--issuer")?,
            wave: RecordId::parse(flags.one("--wave")?)
                .map_err(|error| Failure::usage(error.to_string()))?,
            version: version(flags)?,
        })
    }

    fn acquire(
        &self,
        store: &mut crate::collaboration_state::CollaborationStore,
        repo: &Path,
        request: &RecordRef,
    ) -> Result<(local_target::ControllerOwner, crate::local_authority::Guard), Failure> {
        let owner = existing_owner(store, self.target, self.issuer)?;
        let guard = local_target::authorize(store, &owner).map_err(target_error)?;
        local_wave::require_current_resolution_request(
            store,
            repo,
            &owner,
            &self.wave,
            self.version,
            request,
        )
        .map_err(wave_error)?;
        Ok((owner, guard))
    }
}

pub(super) fn prepare(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &[
            "--request",
            "--object",
            "--session-label",
            "--client-key",
            "--limits",
            "--target",
            "--issuer",
            "--wave",
            "--version",
        ],
        &[],
    )?;
    let current = CurrentWave::parse(&flags)?;
    let record = request(&flags)?;
    let label = flags.one("--session-label")?;
    let key = flags.one("--client-key")?;
    let bytes = read_operator_bytes(flags.one("--limits")?, MAX_LIMITS_DOCUMENT)?;
    let limits: DispatchLimits = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::usage(format!("invalid resolution limits: {error}")))?;
    limits.validate().map_err(error)?;
    let mut store = open(root)?;
    let (_owner, _authority) = current.acquire(&mut store, root, &record)?;
    match resolution_api::begin_attempt(&mut store, root, &record, label, key, limits)
        .map_err(error)?
    {
        BeginResolutionAttempt::Started {
            status,
            request,
            input_manifest,
        } => output(
            "aethyme.collab-resolution-prepare/experimental-v0",
            json!({"started":true,"status":status.to_json(),"packet":request.export_json(&status,&input_manifest).map_err(error)?}),
        ),
        BeginResolutionAttempt::Existing(status) => output(
            "aethyme.collab-resolution-prepare/experimental-v0",
            json!({"started":false,"status":status.to_json(),"packet":null}),
        ),
        BeginResolutionAttempt::BudgetExhausted(allowance) => Err(Failure::refused(
            "budget_exhausted",
            format!(
                "resolution decision used {} of {} attempts",
                allowance.used, allowance.limit
            ),
            "inspect the existing attempts; preparing another packet cannot replenish this allowance",
        )),
    }
}

pub(super) fn status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--attempt", "--session-label"], &[])?;
    let token = flags.one("--attempt")?;
    let label = flags.one("--session-label")?;
    let snapshot = observe(root)?;
    let status =
        resolution_api::attempt_status_from_connection(snapshot.connection(), token, label)
            .map_err(error)?;
    output(
        "aethyme.collab-resolution-status/experimental-v0",
        json!({"status":status.to_json()}),
    )
}

/// Record the intent before an operator hands the packet to a local agent.
/// This is ledger bookkeeping; it never starts or authorizes a process.
pub(super) fn mark_dispatching(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &[
            "--attempt",
            "--session-label",
            "--target",
            "--issuer",
            "--wave",
            "--version",
        ],
        &[],
    )?;
    let current = CurrentWave::parse(&flags)?;
    let token = flags.one("--attempt")?;
    let label = flags.one("--session-label")?;
    let mut store = open(root)?;
    let prior = resolution_api::attempt_status(&store, token, label).map_err(error)?;
    let (_owner, _authority) = current.acquire(&mut store, root, &prior.request)?;
    let status =
        resolution_api::mark_attempt_dispatching(&mut store, token, label).map_err(error)?;
    output(
        "aethyme.collab-resolution-mark-dispatching/experimental-v0",
        json!({"status":status.to_json()}),
    )
}

pub(super) fn mark_unknown(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--attempt", "--session-label", "--detail"], &[])?;
    let token = flags.one("--attempt")?;
    let label = flags.one("--session-label")?;
    let detail = read_operator_bytes(flags.one("--detail")?, 4096)?;
    let detail = std::str::from_utf8(&detail)
        .map_err(|_| Failure::usage("unknown-attempt detail must be UTF-8"))?;
    if detail.trim().is_empty() {
        return Err(Failure::usage("unknown-attempt detail must not be empty"));
    }
    let mut store = open(root)?;
    let status =
        resolution_api::mark_attempt_unknown(&mut store, token, label, detail).map_err(error)?;
    output(
        "aethyme.collab-resolution-mark-unknown/experimental-v0",
        json!({"status":status.to_json(),"automatic_replay":false}),
    )
}

pub(super) fn packet(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--attempt", "--session-label"], &[])?;
    let token = flags.one("--attempt")?;
    let label = flags.one("--session-label")?;
    let mut store = open(root)?;
    let status = resolution_api::attempt_status(&store, token, label).map_err(error)?;
    let (request, manifest) =
        resolution_api::exported_attempt_packet(&mut store, root, token, label).map_err(error)?;
    output(
        "aethyme.collab-resolution-packet/experimental-v0",
        json!({"status":status.to_json(),"packet":request.export_json(&status,&manifest).map_err(error)?}),
    )
}

fn hex(bytes: &[u8]) -> Result<String, Failure> {
    let length = bytes
        .len()
        .checked_mul(2)
        .ok_or_else(|| Failure::usage("input encoding is too large"))?;
    let mut encoded = String::new();
    encoded
        .try_reserve_exact(length)
        .map_err(|_| Failure::domain("io", "cannot allocate input encoding".into()))?;
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        encoded.push(DIGITS[(byte >> 4) as usize] as char);
        encoded.push(DIGITS[(byte & 15) as usize] as char);
    }
    Ok(encoded)
}

pub(super) fn input(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &["--attempt", "--session-label", "--snapshot", "--path"],
        &[],
    )?;
    let token = flags.one("--attempt")?;
    let label = flags.one("--session-label")?;
    let snapshot = SourceSnapshotId::parse(flags.one("--snapshot")?)
        .map_err(|error| Failure::usage(error.to_string()))?;
    let path = flags.one("--path")?;
    let mut store = open(root)?;
    let file = resolution_api::read_attempt_input(&mut store, root, token, label, &snapshot, path)
        .map_err(error)?;
    let file = match file {
        None => Value::Null,
        Some(file) => json!({
            "kind":match file.kind {
                EntryKind::Regular => "regular",
                EntryKind::Executable => "executable",
                EntryKind::Symlink => "symlink",
            },
            "bytes":file.bytes.len(),
            "sha256":ObjectDigest::of(&file.bytes).hex(),
            "bytes_hex":hex(&file.bytes)?,
        }),
    };
    output(
        "aethyme.collab-resolution-input/experimental-v0",
        json!({
            "attempt_token":token,"snapshot":snapshot.as_str(),"path":path,"file":file,
        }),
    )
}

pub(super) fn import(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--attempt", "--session-label", "--proposal"], &[])?;
    let token = flags.one("--attempt")?;
    let label = flags.one("--session-label")?;
    let proposal = flags.one("--proposal")?;
    let mut store = open(root)?;
    let status = resolution_api::attempt_status(&store, token, label).map_err(error)?;
    let bytes = read_operator_bytes(proposal, status.limits.max_response_bytes)?;
    let bound = resolution_api::decode_attempt_response_envelope(&status, &bytes).map_err(error)?;
    let result = resolution_api::finish_bound_attempt(&mut store, root, &bound).map_err(error)?;
    let synthesized = matches!(
        &result.outcome,
        resolution_api::ResolutionOutcome::Synthesized(_)
    );
    let status = resolution_api::attempt_status(&store, token, label).map_err(error)?;
    output(
        "aethyme.collab-resolution-import/experimental-v0",
        json!({
            "status":status.to_json(),"outcome":result.outcome.code(),
            "requires_fresh_verification":synthesized,"attached_to_wave":false,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_commands_reject_incomplete_and_duplicate_arguments_before_state_open() {
        let dir = tempfile::tempdir().unwrap();
        for command in [
            prepare,
            status,
            mark_dispatching,
            mark_unknown,
            packet,
            input,
            import,
        ] {
            assert_eq!(command(dir.path(), &[]).unwrap_err().code, "usage");
            assert_eq!(
                command(dir.path(), &["--unknown", "value"])
                    .unwrap_err()
                    .code,
                "usage"
            );
            assert_eq!(
                command(dir.path(), &["--attempt", "a", "--attempt", "b"])
                    .unwrap_err()
                    .code,
                "usage"
            );
        }
        assert!(std::fs::read_dir(dir.path()).unwrap().next().is_none());
    }

    #[test]
    fn wave_state_failures_remain_distinct_from_stale_refusals() {
        let stale = wave_error(local_wave::WaveError::Stale);
        assert_eq!(stale.code, "resolution_wave_refused");
        assert_eq!(stale.exit, super::super::exit_status::REFUSED);
        let failure = wave_error(local_wave::WaveError::Sqlite(rusqlite::Error::InvalidQuery));
        assert_eq!(failure.code, "sqlite");
        assert_eq!(failure.exit, super::super::exit_status::FAILED);
        let invalid = wave_error(local_wave::WaveError::from(
            resolution_api::ResolutionError::InvalidRequest("mismatched request".into()),
        ));
        assert_eq!(invalid.code, "invalid_request");
        assert_eq!(invalid.exit, super::super::exit_status::REFUSED);
        let unsupported = wave_error(local_wave::WaveError::ResolutionUnsupported);
        assert_eq!(unsupported.code, "unsupported_profile");
        assert_eq!(unsupported.exit, super::super::exit_status::REFUSED);
    }

    #[test]
    fn resolution_input_hex_preserves_all_bytes_without_terminal_controls() {
        let bytes: Vec<u8> = (0..=255).collect();
        let encoded = hex(&bytes).unwrap();
        assert_eq!(encoded.len(), 512);
        assert!(
            encoded
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        assert_eq!(hex(&[0, 10, 27, 255]).unwrap(), "000a1bff");
    }
    #[test]
    #[ignore = "isolated command subprocess for resolution_commands_complete_a_bound_round_trip"]
    fn resolution_command_child() {
        let arguments: Vec<String> =
            serde_json::from_str(&std::env::var("AETHYME_TEST_RESOLUTION_ARGUMENTS").unwrap())
                .unwrap();
        let result = super::super::options(&arguments).and_then(super::super::dispatch);
        let result = match result {
            Ok((value, _)) => json!({"exit":0,"value":value}),
            Err(error) => json!({"exit":error.exit,"code":error.code,"message":error.message}),
        };
        std::fs::write(
            std::env::var_os("AETHYME_TEST_RESOLUTION_REPLY").unwrap(),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn resolution_commands_complete_a_bound_round_trip() {
        use crate::local_target::tests::{Fixture, git};
        use crate::{local_target as target, local_wave as wave};
        use std::sync::atomic::AtomicBool;

        let mut fixture = Fixture::new();
        let repo = fixture._repo.path();
        std::fs::create_dir(repo.join(".aethyme")).unwrap();
        std::fs::write(
            repo.join(".aethyme/config.toml"),
            "[collaboration]\ncapture = \"advisory\"\nproject = \"local-target-test\"\n",
        )
        .unwrap();
        git(repo, &["add", ".aethyme/config.toml"]);
        git(
            repo,
            &["commit", "-qm", "chore: enable Playground collaboration"],
        );
        let owner = target::acquire_controller(&mut fixture.store, "target", "appointed").unwrap();
        let spec = fixture.spec();
        let revision = target::propose_revision(&mut fixture.store, &owner, spec).unwrap();
        target::select_revision(&mut fixture.store, &owner, &revision.id).unwrap();
        wave::available(&mut fixture.store, &owner, "A", 0).unwrap();
        let wave::Admission::Frozen(pending) = wave::admit(
            &mut fixture.store,
            &owner,
            wave::WavePolicy {
                debounce_ms: 0,
                ..Default::default()
            },
            0,
        )
        .unwrap() else {
            panic!("no frozen Playground wave")
        };
        let composed =
            wave::compose_with_source(&mut fixture.store, &owner, repo, &pending.id).unwrap();
        let source = composed
            .source
            .unwrap_or_else(|| panic!("Playground composition has no source: {:?}", composed.wave));
        let requested = wave::raise_resolution_for_failed_checks(
            &mut fixture.store,
            &owner,
            repo,
            &pending.id,
            &source,
            &[resolution_api::FailedCheck {
                check: "playground-native-check".into(),
            }],
            &AtomicBool::new(false),
        )
        .unwrap();
        let request = requested.resolution_request.unwrap();
        let version = target::status(&fixture.store, "target")
            .unwrap()
            .version
            .to_string();
        drop(owner);
        let files = tempfile::tempdir().unwrap();
        let limits = files.path().join("limits.json");
        std::fs::write(
            &limits,
            serde_json::to_vec(&DispatchLimits {
                max_manifest_bytes: 1024 * 1024,
                max_manifest_entries: 64,
                max_file_bytes: 64 * 1024,
                max_total_input_bytes: 1024 * 1024,
                max_response_bytes: 64 * 1024,
            })
            .unwrap(),
        )
        .unwrap();
        let reply = files.path().join("reply.json");
        let invoke = |arguments: &[&str]| -> Value {
            let mut full: Vec<String> = arguments.iter().map(|s| s.to_string()).collect();
            full.extend(["--repo".into(), repo.to_str().unwrap().into()]);
            if reply.exists() {
                std::fs::remove_file(&reply).unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "collaboration_cli::resolution::tests::resolution_command_child",
                    "--ignored",
                    "--nocapture",
                ])
                .env("AETHYME_HOST_STATE_DIR", fixture._host.path())
                .env("AETHYME_HOST_CACHE_DIR", files.path().join("cache"))
                .env("AETHYME_WORKTREE_ROOT", files.path().join("worktrees"))
                .env(
                    "AETHYME_TEST_RESOLUTION_ARGUMENTS",
                    serde_json::to_string(&full).unwrap(),
                )
                .env("AETHYME_TEST_RESOLUTION_REPLY", &reply)
                .env_remove("XDG_STATE_HOME")
                .env_remove("XDG_CACHE_HOME")
                .output()
                .unwrap();
            assert!(output.status.success(), "{arguments:?}: {output:?}");
            serde_json::from_slice(&std::fs::read(&reply).unwrap()).unwrap()
        };
        let context = [
            "--target",
            "target",
            "--issuer",
            "appointed",
            "--wave",
            pending.id.as_str(),
            "--version",
            &version,
        ];
        let object = format!("sha256:{}", request.object.hex());
        let mut prepare_args = vec![
            "resolution",
            "prepare",
            "--request",
            request.id.as_str(),
            "--object",
            &object,
            "--session-label",
            "local-reviewer",
            "--client-key",
            "one",
            "--limits",
            limits.to_str().unwrap(),
        ];
        prepare_args.extend(context);
        let stale_version = (version.parse::<i64>().unwrap() + 1).to_string();
        let mut stale_prepare = prepare_args.clone();
        *stale_prepare.last_mut().unwrap() = &stale_version;
        let stale = invoke(&stale_prepare);
        assert_ne!(
            stale["exit"], 0,
            "stale target version cannot charge: {stale}"
        );
        let before: i64 = fixture
            .store
            .read_connection()
            .query_row("SELECT count(*) FROM resolution_attempts", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(before, 0);
        let prepared = invoke(&prepare_args);
        assert_eq!(prepared["exit"], 0, "{prepared}");
        assert_eq!(prepared["value"]["started"], true);
        let token = prepared["value"]["status"]["token"].as_str().unwrap();
        let repeated = invoke(&prepare_args);
        assert_eq!(repeated["exit"], 0, "{repeated}");
        assert_eq!(repeated["value"]["started"], false);
        assert!(repeated["value"]["packet"].is_null());
        assert_eq!(repeated["value"]["status"]["attempt"], 1);
        let packet = invoke(&[
            "resolution",
            "packet",
            "--attempt",
            token,
            "--session-label",
            "local-reviewer",
        ]);
        assert_eq!(packet["exit"], 0, "{packet}");
        let manifest = &packet["value"]["packet"]["input_manifest"];
        let entry = manifest["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| !entry["kind"].is_null())
            .unwrap();
        let input = invoke(&[
            "resolution",
            "input",
            "--attempt",
            token,
            "--session-label",
            "local-reviewer",
            "--snapshot",
            entry["snapshot"].as_str().unwrap(),
            "--path",
            entry["path"].as_str().unwrap(),
        ]);
        assert_eq!(input["exit"], 0, "{input}");
        assert_eq!(input["value"]["file"]["sha256"], entry["sha256"]);
        let proposal = files.path().join("response.json");
        std::fs::write(&proposal, serde_json::to_vec(&json!({
            "schema":resolution_api::ASYNC_RESPONSE_ENVELOPE_SCHEMA,
            "attempt_token":token, "session_label":"local-reviewer",
            "request_record_id":request.id.as_str(), "request_object_sha256":request.object.hex(),
            "input_manifest_sha256":manifest["sha256"],
            "response":{"kind":"unresolved", "detail":"no safe change from this local reviewer"},
        })).unwrap()).unwrap();
        let import_args = [
            "resolution",
            "import",
            "--attempt",
            token,
            "--session-label",
            "local-reviewer",
            "--proposal",
            proposal.to_str().unwrap(),
        ];
        assert_ne!(
            invoke(&import_args)["exit"],
            0,
            "exported is not dispatching"
        );
        let mut mark_args = vec![
            "resolution",
            "mark-dispatching",
            "--attempt",
            token,
            "--session-label",
            "local-reviewer",
        ];
        mark_args.extend(context);
        let mut stale_mark = mark_args.clone();
        *stale_mark.last_mut().unwrap() = &stale_version;
        let refused = invoke(&stale_mark);
        assert_ne!(refused["exit"], 0, "stale wave cannot dispatch: {refused}");
        assert_eq!(
            resolution_api::attempt_status(&fixture.store, token, "local-reviewer")
                .unwrap()
                .state,
            resolution_api::ResolutionAttemptState::Exported
        );
        let marked = invoke(&mark_args);
        assert_eq!(marked["exit"], 0, "{marked}");
        assert_eq!(marked["value"]["status"]["state"], "dispatching");
        let detail = files.path().join("detail.txt");
        std::fs::write(&detail, "local response delivery was uncertain").unwrap();
        let unknown = invoke(&[
            "resolution",
            "mark-unknown",
            "--attempt",
            token,
            "--session-label",
            "local-reviewer",
            "--detail",
            detail.to_str().unwrap(),
        ]);
        assert_eq!(unknown["exit"], 0, "{unknown}");
        assert_eq!(unknown["value"]["status"]["state"], "outcome_unknown");
        let imported = invoke(&import_args);
        assert_eq!(imported["exit"], 0, "{imported}");
        assert_eq!(imported["value"]["outcome"], "unresolved");
        assert_eq!(imported["value"]["attached_to_wave"], false);
        assert_eq!(imported["value"]["grants_acceptance"], false);
        assert_ne!(
            invoke(&import_args)["exit"],
            0,
            "settled response cannot be imported twice"
        );
        let status = invoke(&[
            "resolution",
            "status",
            "--attempt",
            token,
            "--session-label",
            "local-reviewer",
        ]);
        assert_eq!(status["exit"], 0, "{status}");
        assert_eq!(status["value"]["status"]["state"], "settled");
        assert_eq!(status["value"]["status"]["attempt"], 1);
        assert_eq!(
            wave::load(&fixture.store, &pending.id).unwrap().phase,
            wave::WavePhase::Verifying
        );
    }
}
