//! Explicit, local-only handoff of one durably charged resolution attempt.
//! Session labels are provenance; these commands neither dispatch an agent
//! nor authenticate one, and an imported proposal is never accepted here.
use super::lifecycle::{Flags, observe, read_operator_bytes};
use super::{Failure, Output, open};
use crate::collaboration_archive::ObjectDigest;
use crate::resolution::{
    self as resolution_api, BeginResolutionAttempt, DispatchLimits, RecordRef,
};
use aethyme_contracts::experimental_v0::{EntryKind, RecordId, SourceSnapshotId};
use serde_json::{Value, json};
use std::path::Path;

const MAX_LIMITS_DOCUMENT: u64 = 16 * 1024;

fn error(error: resolution_api::ResolutionError) -> Failure {
    Failure::domain(error.code(), error.to_string())
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

pub(super) fn prepare(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &[
            "--request",
            "--object",
            "--session-label",
            "--client-key",
            "--limits",
        ],
        &[],
    )?;
    let record = request(&flags)?;
    let label = flags.one("--session-label")?;
    let key = flags.one("--client-key")?;
    let bytes = read_operator_bytes(flags.one("--limits")?, MAX_LIMITS_DOCUMENT)?;
    let limits: DispatchLimits = serde_json::from_slice(&bytes)
        .map_err(|error| Failure::usage(format!("invalid resolution limits: {error}")))?;
    limits.validate().map_err(error)?;
    let mut store = open(root)?;
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
        for command in [prepare, status, packet, input, import] {
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
}
