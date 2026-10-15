//! Explicit operator commands; all native capabilities are acquired locally.
use super::*;
use crate::collaboration_state::{CollaborationSnapshot, inspect_for_repository};
use crate::{
    collaboration_sensitive as response, execution_verifier as verifier,
    local_authority as authority, local_budget as budget, local_capture_notice as notices,
    local_handoff as handoff, local_restore as restore, local_target as target,
};
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;

const MAX_ARGUMENTS: usize = 512;
const MAX_SPEC: u64 = 256 * 1024;

struct Flags<'a>(BTreeMap<&'a str, Vec<&'a str>>);
impl<'a> Flags<'a> {
    fn parse(rest: &[&'a str], allowed: &[&str], repeated: &[&str]) -> Result<Self, Failure> {
        if rest.len() > MAX_ARGUMENTS || !rest.len().is_multiple_of(2) {
            return Err(Failure::usage("expected a finite set of flag/value pairs"));
        }
        let mut values = BTreeMap::<&str, Vec<&str>>::new();
        for pair in rest.chunks_exact(2) {
            let (flag, value) = (pair[0], pair[1]);
            if !allowed.contains(&flag)
                || value.trim().is_empty()
                || value.starts_with("--")
                || value.len() > 4096
                || value.contains('\0')
            {
                return Err(Failure::usage("unknown flag or invalid flag value"));
            }
            let entry = values.entry(flag).or_default();
            if !entry.is_empty() && !repeated.contains(&flag) {
                return Err(Failure::usage(format!("duplicate flag {flag}")));
            }
            entry.push(value);
        }
        Ok(Self(values))
    }
    fn one(&self, name: &str) -> Result<&'a str, Failure> {
        self.0
            .get(name)
            .and_then(|v| v.first())
            .copied()
            .ok_or_else(|| Failure::usage(format!("{name} is required")))
    }
    fn many(&self, name: &str) -> Vec<&'a str> {
        self.0.get(name).cloned().unwrap_or_default()
    }
}
fn sqlite(error: rusqlite::Error) -> Failure {
    Failure::domain("sqlite", error.to_string())
}
fn io(error: std::io::Error) -> Failure {
    Failure::domain("io", error.to_string())
}
fn observe(root: &Path) -> Result<CollaborationSnapshot, Failure> {
    let enabled = require_enabled(root)?;
    inspect_for_repository(root, &enabled.project)
        .map_err(|e| Failure::domain(e.code(), e.to_string()))?
        .ok_or_else(|| {
            Failure::refused(
                "not_initialized",
                "no Local state exists",
                next_action_for("not_initialized").unwrap_or_default(),
            )
        })
}
fn require_table(snapshot: &CollaborationSnapshot, table: &str) -> Result<(), Failure> {
    if !snapshot.has_table(table) {
        return Err(Failure::refused(
            "migration_required",
            "this read-only view predates the requested Local package",
            "open existing state with an explicitly requested mutation, then inspect again",
        ));
    }
    Ok(())
}
fn authority_error(error: authority::AuthorityError) -> Failure {
    let code = match &error {
        authority::AuthorityError::Frozen => "authority_frozen",
        authority::AuthorityError::Stale => "stale_epoch",
        authority::AuthorityError::Invalid(_) => "invalid_authority",
        authority::AuthorityError::Io(_) => "io",
        authority::AuthorityError::Sqlite(_) => "sqlite",
        _ => "failed",
    };
    Failure::domain(code, error.to_string())
}
fn target_error(error: target::TargetError) -> Failure {
    if let target::TargetError::Authority(error) = error {
        return authority_error(error);
    }
    let code = match &error {
        target::TargetError::Invalid(_) => "invalid_target",
        target::TargetError::ControllerBusy => "controller_busy",
        target::TargetError::Unauthorized => "issuer_mismatch",
        target::TargetError::Missing => "target_missing",
        target::TargetError::Frozen => "target_frozen",
        target::TargetError::Transition { .. } => "invalid_transition",
        target::TargetError::StaleVersion => "stale_version",
        target::TargetError::VerificationUnavailable => "unsupported_profile",
        target::TargetError::Released => "released_revision",
        target::TargetError::SelectionRefused { .. } => "selection_refused",
        target::TargetError::Io(_) => "io",
        target::TargetError::Sqlite(_) => "sqlite",
        _ => "failed",
    };
    Failure::domain(code, error.to_string())
}
fn native_gate_error(error: crate::native_gates::NativeGateError) -> Failure {
    use crate::native_gates::NativeGateError;
    match error {
        NativeGateError::Target(error) => target_error(error),
        NativeGateError::Sqlite(error) => sqlite(error),
        NativeGateError::Io(error) => io(error),
        NativeGateError::Mismatch => Failure::refused(
            "native_job_missing",
            "native job does not exist for this appointed target",
            "list native jobs for the current target and inspect the exact job ID",
        ),
        NativeGateError::RootsReleased => Failure::refused(
            "native_job_roots_released",
            "native job roots were released and the history is not dispatchable",
            "inspect the retained job status; do not retry a released job",
        ),
        NativeGateError::Unknown => Failure::refused(
            "native_job_unknown",
            "native job state changed during the operation",
            "inspect the exact job status before taking further action",
        ),
        error => Failure::domain("native_gate", error.to_string()),
    }
}
fn existing_owner(
    store: &mut CollaborationStore,
    name: &str,
    issuer: &str,
) -> Result<target::ControllerOwner, Failure> {
    let actual: Option<String> = store
        .read_connection()
        .query_row(
            "SELECT issuer FROM local_targets WHERE target=?1",
            [name],
            |r| r.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    if actual.as_deref() != Some(issuer) {
        return Err(Failure::refused(
            if actual.is_none() {
                "target_missing"
            } else {
                "issuer_mismatch"
            },
            "existing target and issuer must match before controller acquisition",
            "inspect the appointed target and use its issuer label",
        ));
    }
    target::acquire_controller(store, name, issuer).map_err(target_error)
}
fn parse_record(value: &str) -> Result<RecordId, Failure> {
    RecordId::parse(value).map_err(|e| Failure::usage(e.to_string()))
}
fn version(flags: &Flags<'_>) -> Result<i64, Failure> {
    let n = flags
        .one("--version")?
        .parse::<i64>()
        .map_err(|_| Failure::usage("--version must be a nonnegative integer"))?;
    if n < 0 {
        return Err(Failure::usage("--version must be a nonnegative integer"));
    }
    Ok(n)
}
fn read_spec(path: &str) -> Result<target::TargetSpec, Failure> {
    if !std::fs::symlink_metadata(path)
        .map_err(io)?
        .file_type()
        .is_file()
    {
        return Err(Failure::usage(
            "target specification must be a regular file",
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io)?;
    if !file.metadata().map_err(io)?.is_file() {
        return Err(Failure::usage(
            "target specification must be a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_SPEC + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_SPEC {
        return Err(Failure::refused(
            "file_too_large",
            "target specification exceeds 256 KiB",
            "supply a bounded target specification",
        ));
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Failure::usage(format!("invalid strict target specification: {e}")))
}
pub(super) fn authority_status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    if !rest.is_empty() {
        return Err(Failure::usage("authority status takes no argument flags"));
    }
    let snapshot = observe(root)?;
    let observed = authority::inspect(&snapshot).map_err(authority_error)?;
    Ok((
        json!({"schema":"aethyme.collab-authority-status/experimental-v0","observation":observed,"grants_authority":false}),
        format!("Local authority {} (observation only)\n", observed.state),
    ))
}
type TargetStatusRow = (String, Option<String>, String, Option<String>, i64);

pub(super) fn target_status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--target"], &[])?;
    let name = flags.one("--target")?;
    let snapshot = observe(root)?;
    require_table(&snapshot, "local_targets")?;
    let row: Option<TargetStatusRow>=snapshot.connection().query_row("SELECT issuer,active_revision,phase,resume_phase,version FROM local_targets WHERE target=?1",[name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(sqlite)?;
    let (issuer, revision, phase, resume, version) = row.ok_or_else(|| {
        Failure::refused(
            "target_missing",
            "target is not appointed",
            "explicitly propose a valid target first",
        )
    })?;
    Ok((
        json!({"schema":"aethyme.collab-target-status/experimental-v0","target":name,"issuer":issuer,"revision":revision,"phase":phase,"resume_phase":resume,"version":version,"grants_authority":false}),
        format!("target {name}: {phase}, version {version}\n"),
    ))
}

pub(super) fn native_gate_retention(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    use crate::native_gates as native;
    let Some(verb @ ("list" | "status" | "release")) = rest.first().copied() else {
        return Err(Failure::usage(
            "native-gates requires list, status, or release",
        ));
    };
    let flags = Flags::parse(&rest[1..], &["--target", "--issuer", "--job"], &[])?;
    let name = flags.one("--target")?;
    let issuer = flags.one("--issuer")?;
    if verb == "list" && flags.0.contains_key("--job") {
        return Err(Failure::usage("native-gates list does not take --job"));
    }
    if verb != "list" && !flags.0.contains_key("--job") {
        return Err(Failure::usage(
            "native-gates status and release require --job",
        ));
    }
    let key = if verb == "list" {
        None
    } else {
        Some(parse_record(flags.one("--job")?)?)
    };
    let mut store = open(root)?;
    let owner = existing_owner(&mut store, name, issuer)?;
    match verb {
        "list" => {
            let listing = native::list_retention(&store, &owner).map_err(native_gate_error)?;
            let jobs: Vec<Value> = listing
                .jobs
                .iter()
                .map(|job| {
                    json!({
                        "id":job.id,
                        "target":job.target,
                        "state":job.state,
                        "roots_retained":job.roots_released_ms.is_none(),
                        "roots_released_ms":job.roots_released_ms,
                    })
                })
                .collect();
            let job_count = jobs.len();
            Ok((
                json!({"schema":"aethyme.native-gate-retention/experimental-v1","action":"list","jobs":jobs,"truncated":listing.truncated}),
                format!(
                    "{} native gate job(s) listed{}\n",
                    job_count,
                    if listing.truncated {
                        "; list truncated at 500"
                    } else {
                        ""
                    }
                ),
            ))
        }
        "status" => {
            let job = native::retention_status_for(&store, &owner, key.as_ref().unwrap())
                .map_err(native_gate_error)?;
            let roots_retained = job.roots_released_ms.is_none();
            let job_id = job.id.clone();
            let state = job.state.clone();
            Ok((
                json!({"schema":"aethyme.native-gate-retention/experimental-v1","action":"status","job":job,"roots_retained":roots_retained}),
                format!(
                    "native gate job {job_id}: {state}, roots {}\n",
                    if roots_retained {
                        "retained"
                    } else {
                        "released"
                    }
                ),
            ))
        }
        "release" => {
            let release = native::release_roots(&mut store, &owner, key.as_ref().unwrap())
                .map_err(native_gate_error)?;
            let roots_retained = release.job.roots_released_ms.is_none();
            let job_id = release.job.id.clone();
            let status = release.status;
            Ok((
                json!({"schema":"aethyme.native-gate-retention/experimental-v1","action":"release","result":release,"roots_retained":roots_retained,"dispatch_authority_granted":false}),
                format!(
                    "native gate job {job_id}: {status:?}; roots {}\n",
                    if roots_retained {
                        "retained"
                    } else {
                        "released"
                    }
                ),
            ))
        }
        _ => unreachable!(),
    }
}

pub(super) fn target_change(root: &Path, verb: &str, rest: &[&str]) -> Result<Output, Failure> {
    let extra = match verb {
        "propose" => "--file",
        "select" => "--revision",
        _ => "--version",
    };
    let flags = Flags::parse(rest, &["--target", "--issuer", extra], &[])?;
    let name = flags.one("--target")?;
    let issuer = flags.one("--issuer")?;
    // Parse all command input before writable state or appointment.
    let spec = if verb == "propose" {
        Some(read_spec(flags.one("--file")?)?)
    } else {
        None
    };
    let revision = if verb == "select" {
        Some(parse_record(flags.one("--revision")?)?)
    } else {
        None
    };
    let expected = if matches!(verb, "hold" | "resume") {
        Some(version(&flags)?)
    } else {
        None
    };
    let mut store = open(root)?;
    let owner = if let Some(spec) = &spec {
        target::validate_proposal(&store, spec.clone()).map_err(target_error)?;
        target::acquire_controller(&mut store, name, issuer).map_err(target_error)?
    } else {
        existing_owner(&mut store, name, issuer)?
    };
    let proposal = if let Some(spec) = spec {
        Some(target::propose_revision(&mut store, &owner, spec).map_err(target_error)?)
    } else {
        None
    };
    if let Some(revision) = revision {
        target::select_revision(&mut store, &owner, &revision).map_err(target_error)?;
    }
    if verb == "hold" {
        target::transition(
            &mut store,
            &owner,
            expected.unwrap(),
            target::Phase::WaitingForInput,
        )
        .map_err(target_error)?;
    }
    if verb == "resume" {
        target::resume(&mut store, &owner, expected.unwrap()).map_err(target_error)?;
    }
    let status = target::status(&store, name).map_err(target_error)?;
    Ok((
        json!({"schema":"aethyme.collab-target-change/experimental-v0","operation":verb,"target":name,"proposal":proposal.map(|r|r.id.to_string()),"status":{"revision":status.revision.map(|r|r.to_string()),"phase":status.phase,"resume_phase":status.resume_phase,"version":status.version},"dispatch_started":false}),
        format!("target {name}: {verb}; no work dispatched\n"),
    ))
}

/// Configure the immutable project-wide budget before any objective is
/// admitted. The policy is a bounded operator file and the stored ID is the
/// canonical policy identity, so a lost response can be checked by retrying
/// the same file without replenishing the cap.
pub(super) fn budget_project_configure(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--target", "--issuer", "--file"], &[])?;
    let policy: budget::ProjectPolicy = read_operator(flags.one("--file")?)?;
    let mut store = open(root)?;
    let owner = existing_owner(&mut store, flags.one("--target")?, flags.one("--issuer")?)?;
    let id = budget::configure_project(&mut store, &owner, policy)
        .map_err(|e| Failure::domain("budget_configuration", e.to_string()))?;
    Ok((
        json!({"schema":"aethyme.collab-budget-project-configure/experimental-v0","target":owner.target(),"policy":id.as_str(),"immutable":true,"grants_model_dispatch":false}),
        format!(
            "project budget policy {} recorded; no work dispatched\n",
            id.as_str()
        ),
    ))
}

/// Authorize one immutable objective cap for the selected target revision.
pub(super) fn budget_objective_authorize(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--target", "--issuer", "--file"], &[])?;
    let cap: budget::Units = read_operator(flags.one("--file")?)?;
    let mut store = open(root)?;
    let owner = existing_owner(&mut store, flags.one("--target")?, flags.one("--issuer")?)?;
    let id = budget::authorize_objective(&mut store, &owner, cap)
        .map_err(|e| Failure::domain("budget_configuration", e.to_string()))?;
    Ok((
        json!({"schema":"aethyme.collab-budget-objective-authorize/experimental-v0","target":owner.target(),"authorization":id.as_str(),"immutable":true,"grants_model_dispatch":false}),
        format!(
            "objective cap {} recorded; no work dispatched\n",
            id.as_str()
        ),
    ))
}

/// Read immutable budget appointments and ledger status after a lost response.
pub(super) fn budget_status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--target", "--issuer", "--reservation"], &[])?;
    let reservation = flags
        .0
        .get("--reservation")
        .and_then(|values| values.first())
        .map(|value| parse_record(value))
        .transpose()?;
    let store = open(root)?;
    let status = budget::status(
        &store,
        flags.one("--target")?,
        flags.one("--issuer")?,
        reservation.as_ref(),
    )
    .map_err(|error| Failure::domain("budget_status", error.to_string()))?;
    let reservations_shown = status.reservations.len();
    let omitted = status
        .reservation_count
        .saturating_sub(reservations_shown as u64);
    let target_name = status.target.clone();
    Ok((
        json!({
            "schema":"aethyme.collab-budget-status/experimental-v0",
            "status":status,
            "reservation_page_limit":budget::STATUS_RESERVATION_LIMIT,
            "reservation_rows_omitted":omitted,
            "charged_is_root_ledger_amount_not_measured_usage":true,
            "grants_dispatch":false
        }),
        format!(
            "budget status for {target_name}: {} of {} reservation rows shown{}; no dispatch authority granted\n",
            reservations_shown,
            status.reservation_count,
            if omitted > 0 {
                format!("; {omitted} older rows omitted")
            } else {
                String::new()
            }
        ),
    ))
}

/// Append an idempotent notice for an already retained selected contribution.
/// The ingress path intentionally does not acquire the target owner lock.
pub(super) fn capture_notice(root: &Path, verb: &str, rest: &[&str]) -> Result<Output, Failure> {
    match verb {
        "ingest" => {
            let flags = Flags::parse(
                rest,
                &[
                    "--target",
                    "--issuer",
                    "--revision",
                    "--alias",
                    "--operation",
                ],
                &[],
            )?;
            let revision = parse_record(flags.one("--revision")?)?;
            let operation = OperationId::parse(flags.one("--operation")?)
                .map_err(|error| Failure::usage(error.to_string()))?;
            let mut store = open(root)?;
            let receipt = notices::ingest(
                &mut store,
                flags.one("--target")?,
                flags.one("--issuer")?,
                &revision,
                flags.one("--alias")?,
                &operation,
            )
            .map_err(|error| Failure::domain("capture_notice", error.to_string()))?;
            Ok((
                json!({
                    "schema":"aethyme.collab-capture-notice/experimental-v0",
                    "action":"ingest",
                    "notice":receipt.id.as_str(),
                    "status":receipt.status,
                    "received_ms":receipt.received_ms,
                    "idempotent_retry_safe":true,
                    "available_to_wave":false,
                    "dispatch_authority_granted":false
                }),
                format!(
                    "capture notice {}: {}; the owner tick must revalidate it before wave admission\n",
                    receipt.id.as_str(),
                    receipt.status
                ),
            ))
        }
        "list" => {
            let flags = Flags::parse(rest, &["--target", "--limit"], &[])?;
            let limit = flags
                .0
                .get("--limit")
                .and_then(|values| values.first())
                .map(|value| {
                    value
                        .parse::<usize>()
                        .map_err(|_| Failure::usage("--limit must be an integer from 1 to 512"))
                })
                .transpose()?
                .unwrap_or(100);
            let store = open(root)?;
            let rows = notices::list_for_target(&store, flags.one("--target")?, limit)
                .map_err(|error| Failure::domain("capture_notice", error.to_string()))?;
            let count = rows.len();
            Ok((
                json!({
                    "schema":"aethyme.collab-capture-notice/experimental-v0",
                    "action":"list",
                    "notices":rows,
                    "limit":limit,
                    "may_be_truncated":count == limit,
                    "dispatch_authority_granted":false
                }),
                format!(
                    "{count} capture notice row(s) listed; statuses are not dispatch authority\n"
                ),
            ))
        }
        _ => Err(Failure::usage("capture-notice requires ingest or list")),
    }
}

/// Compute an acceptance-policy ID before writing it into an immutable target
/// proposal. This is pure validation and does not appoint a producer.
pub(super) fn verifier_policy_id(_root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--file"], &[])?;
    let policy: verifier::VerifierPolicy = read_operator(flags.one("--file")?)?;
    let record = policy
        .record()
        .map_err(|e| Failure::domain("verifier_policy", e.to_string()))?;
    Ok((
        json!({"schema":"aethyme.collab-verifier-policy-id/experimental-v0","policy":record.id().as_str(),"profile":policy.profile,"runtime":policy.runtime,"image":policy.image,"harness":policy.harness,"inventory":policy.inventory,"appoints_producer":false}),
        format!(
            "verifier policy {} validated; no producer appointed\n",
            record.id().as_str()
        ),
    ))
}

/// Retain an exact harness commit and its canonical literal inventory, then
/// derive the runtime and policy identities from the explicit Docker endpoint.
/// This produces no verifier authority or workload dispatch.
pub(super) fn verifier_prepare(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &[
            "--harness-repo",
            "--commit",
            "--inventory",
            "--operation",
            "--docker",
            "--endpoint",
            "--file",
        ],
        &[],
    )?;
    let plan: verifier::VerifierPlan = read_operator(flags.one("--file")?)?;
    let inventory = read_operator_bytes(flags.one("--inventory")?, 4 * 1024 * 1024)?;
    let repository = std::path::PathBuf::from(flags.one("--harness-repo")?);
    let docker = std::path::PathBuf::from(flags.one("--docker")?);
    if !repository.is_absolute() || !docker.is_absolute() {
        return Err(Failure::usage(
            "absolute harness repository and Docker CLI paths are required",
        ));
    }
    let operation = OperationId::parse(flags.one("--operation")?)
        .map_err(|error| Failure::usage(error.to_string()))?;
    let mut store = open(root)?;
    let prepared = verifier::prepare(
        &mut store,
        operation,
        &repository,
        flags.one("--commit")?,
        &inventory,
        plan,
        docker,
        flags.one("--endpoint")?,
    )
    .map_err(|error| Failure::domain("verifier_preparation", error.to_string()))?;
    let capture = &prepared.capture;
    let policy = serde_json::to_value(&prepared.policy)
        .map_err(|error| Failure::domain("verifier_preparation", error.to_string()))?;
    let runtime = serde_json::to_value(&prepared.runtime)
        .map_err(|error| Failure::domain("verifier_preparation", error.to_string()))?;
    Ok((
        json!({
            "schema":"aethyme.collab-verifier-prepare/experimental-v0",
            "policy":policy,
            "policy_id":prepared.policy_id.as_str(),
            "runtime":runtime,
            "runtime_id":prepared.policy.runtime,
            "harness_snapshot":prepared.policy.harness,
            "inventory_id":prepared.policy.inventory,
            "inventory_sha256":prepared.inventory_sha256,
            "inventory_cases":prepared.inventory_cases,
            "capture":{
                "operation":capture.operation_id.as_str(),
                "status":capture.status,
                "durability":capture.durability,
                "contribution":capture.contribution.as_str(),
                "base":capture.base.as_str(),
                "result":capture.result.as_str(),
                "retention":"until_released"
            },
            "producer_configured":false,
            "dispatch_started":false
        }),
        format!(
            "verifier policy {} prepared from retained harness {}; no producer appointed and no work dispatched\n",
            prepared.policy_id.as_str(),
            prepared.policy.harness
        ),
    ))
}

/// Appoint or rotate a producer with an explicit compare-and-swap generation.
pub(super) fn verifier_configure(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &[
            "--target",
            "--issuer",
            "--producer",
            "--expected-generation",
            "--file",
        ],
        &[],
    )?;
    let policy: verifier::VerifierPolicy = read_operator(flags.one("--file")?)?;
    let expected = match flags.one("--expected-generation")? {
        "none" => None,
        value => {
            let generation = value
                .parse::<i64>()
                .map_err(|_| Failure::usage("--expected-generation must be `none` or positive"))?;
            if generation < 1 {
                return Err(Failure::usage(
                    "--expected-generation must be `none` or positive",
                ));
            }
            Some(generation)
        }
    };
    let mut store = open(root)?;
    let owner = existing_owner(&mut store, flags.one("--target")?, flags.one("--issuer")?)?;
    let permit = verifier::configure(
        &mut store,
        &owner,
        flags.one("--producer")?,
        expected,
        policy,
    )
    .map_err(|e| Failure::domain("verifier_configuration", e.to_string()))?;
    let policy_id = permit
        .policy()
        .record()
        .map_err(|e| Failure::domain("verifier_policy", e.to_string()))?
        .id();
    Ok((
        json!({"schema":"aethyme.collab-verifier-configure/experimental-v0","target":owner.target(),"producer":permit.producer(),"generation":permit.generation(),"policy":policy_id.as_str(),"dispatch_started":false}),
        format!(
            "producer {} appointed at generation {}; no dispatch started\n",
            permit.producer(),
            permit.generation()
        ),
    ))
}

/// Read producer generation without changing authority.
pub(super) fn verifier_status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--target", "--issuer", "--producer"], &[])?;
    let target_name = flags.one("--target")?;
    let issuer = flags.one("--issuer")?;
    let producer = flags.one("--producer")?;
    let snapshot = observe(root)?;
    require_table(&snapshot, "local_verifier_producers")?;
    let _response = response::response_use_for_project_dir(&snapshot.project_dir)
        .map_err(|error| Failure::domain("sensitive_response", error.to_string()))?;
    let target_issuer: Option<String> = snapshot
        .connection()
        .query_row(
            "SELECT issuer FROM local_targets WHERE target=?1",
            [target_name],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    if target_issuer.as_deref() != Some(issuer) {
        return Err(Failure::refused(
            if target_issuer.is_none() {
                "target_missing"
            } else {
                "issuer_mismatch"
            },
            "target and issuer must match before producer status is read",
            "inspect the appointed target and use its issuer label",
        ));
    }
    let row: Option<(i64, String, String, bool)> = snapshot
        .connection()
        .query_row(
            "SELECT generation,policy,epoch,revoked FROM local_verifier_producers WHERE target=?1 AND producer=?2",
            rusqlite::params![target_name, producer],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sqlite)?;
    let Some((generation, policy, epoch, revoked)) = row else {
        return Err(Failure::refused(
            "producer_missing",
            "no verifier producer is appointed under this name",
            "configure a producer for the selected target revision",
        ));
    };
    require_table(&snapshot, "local_target_revisions")?;
    let current: Option<(String, Option<String>, Option<Vec<u8>>)> = snapshot
        .connection()
        .query_row(
            "SELECT t.issuer,t.active_revision,r.body
             FROM local_targets t
             LEFT JOIN local_target_revisions r ON r.revision=t.active_revision
             WHERE t.target=?1",
            [target_name],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(sqlite)?;
    let Some((current_issuer, current_revision, revision_body)) = current else {
        return Err(Failure::refused(
            "target_missing",
            "target disappeared while verifier status was read",
            "inspect target status again",
        ));
    };
    if current_issuer != issuer {
        return Err(Failure::refused(
            "issuer_mismatch",
            "target issuer changed while verifier status was read",
            "inspect target status and use its current issuer label",
        ));
    }
    let selected_policy = match (current_revision.as_deref(), revision_body) {
        (None, None) => None,
        (Some(revision), Some(body)) => {
            let revision_id = parse_record(revision)?;
            let record = aethyme_contracts::experimental_v0::Record::decode(
                &body,
                &[&target::TARGET_REVISION_SCHEMA],
            )
            .map_err(|error| Failure::domain("corrupt_target", error.to_string()))?;
            if record.id() != revision_id || record.canonical_bytes() != body {
                return Err(Failure::domain(
                    "corrupt_target",
                    "selected revision record identity differs".into(),
                ));
            }
            let value: Value = serde_json::from_slice(&body)
                .map_err(|error| Failure::domain("corrupt_target", error.to_string()))?;
            if value["target"].as_str() != Some(target_name)
                || value["issuer"].as_str() != Some(issuer)
            {
                return Err(Failure::domain(
                    "corrupt_target",
                    "selected revision target binding differs".into(),
                ));
            }
            let spec: target::TargetSpec = serde_json::from_value(value["spec"].clone())
                .map_err(|error| Failure::domain("corrupt_target", error.to_string()))?;
            Some(spec.acceptance_policy)
        }
        _ => {
            return Err(Failure::domain(
                "corrupt_target",
                "selected target revision record is incomplete".into(),
            ));
        }
    };
    let authority = authority::inspect(&snapshot).map_err(authority_error)?;
    let witness_epoch = authority
        .witness
        .as_ref()
        .map(|witness| witness.epoch.as_str());
    let appointment_state = if revoked {
        "revoked"
    } else if authority.state != "active" || !authority.binding_matches {
        match authority.state {
            "frozen" => "authority_frozen",
            "drift" => "authority_drift",
            "unbound" | "witness_missing" => "authority_unavailable",
            _ => "authority_unavailable",
        }
    } else if witness_epoch != Some(epoch.as_str()) {
        "stale_epoch"
    } else if selected_policy.as_deref() != Some(policy.as_str()) {
        "stale_selected_policy"
    } else {
        "appointed_current"
    };
    Ok((
        json!({
            "schema":"aethyme.collab-verifier-status/experimental-v0",
            "target":target_name,
            "producer":producer,
            "generation":generation,
            "policy":policy,
            "appointed_epoch":epoch,
            "authority_state":authority.state,
            "authority_binding_matches":authority.binding_matches,
            "selected_revision":current_revision,
            "selected_acceptance_policy":selected_policy,
            "revoked":revoked,
            "appointment_state":appointment_state,
            "dispatch_authority_granted":false
        }),
        format!("producer {producer}: generation {generation}, appointment {appointment_state}\n"),
    ))
}

/// Revoke exactly the currently observed producer generation. Run this before
/// freezing project authority; a later generation is never guessed.
pub(super) fn verifier_revoke(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &["--target", "--issuer", "--producer", "--generation"],
        &[],
    )?;
    let generation = flags
        .one("--generation")?
        .parse::<i64>()
        .map_err(|_| Failure::usage("--generation must be positive"))?;
    if generation < 1 {
        return Err(Failure::usage("--generation must be positive"));
    }
    let next_generation = generation
        .checked_add(1)
        .ok_or_else(|| Failure::usage("--generation is too large"))?;
    let mut store = open(root)?;
    let owner = existing_owner(&mut store, flags.one("--target")?, flags.one("--issuer")?)?;
    verifier::revoke(&mut store, &owner, flags.one("--producer")?, generation)
        .map_err(|e| Failure::domain("verifier_revocation", e.to_string()))?;
    Ok((
        json!({"schema":"aethyme.collab-verifier-revoke/experimental-v0","target":owner.target(),"producer":flags.one("--producer")?,"revoked_generation":generation,"next_generation":next_generation,"dispatch_started":false}),
        format!(
            "producer {} generation {generation} revoked; no dispatch started\n",
            flags.one("--producer")?
        ),
    ))
}

/// Inspect one durable finite-service appointment after a lost process or
/// stdout response. A dispatching row is intentionally reported as unknown;
/// this command never resumes or replays it.
pub(super) fn service_status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--target", "--issuer", "--appointment"], &[])?;
    let target_name = flags.one("--target")?;
    let issuer = flags.one("--issuer")?;
    let appointment = parse_record(flags.one("--appointment")?)?;
    let store = open(root)?;
    let _response = response::response_use(&store)
        .map_err(|e| Failure::domain("sensitive_response", e.to_string()))?;
    let actual_issuer: Option<String> = store
        .read_connection()
        .query_row(
            "SELECT issuer FROM local_targets WHERE target=?1",
            [target_name],
            |row| row.get(0),
        )
        .optional()
        .map_err(sqlite)?;
    if actual_issuer.as_deref() != Some(issuer) {
        return Err(Failure::refused(
            if actual_issuer.is_none() {
                "target_missing"
            } else {
                "issuer_mismatch"
            },
            "target and issuer must match before service status is read",
            "inspect the appointed target and use its issuer label",
        ));
    }
    let row: Option<(String, String, String, String, Option<Vec<u8>>)> = store
        .read_connection()
        .query_row(
            "SELECT target,revision,epoch,state,report FROM local_controller_services WHERE id=?1",
            [appointment.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(sqlite)?;
    let Some((target, revision, epoch, state, bytes)) = row else {
        return Err(Failure::refused(
            "appointment_missing",
            "no service appointment has this ID",
            "use the appointment ID returned by target run",
        ));
    };
    if target != target_name {
        return Err(Failure::refused(
            "appointment_target_mismatch",
            "appointment belongs to a different target",
            "inspect only the target named by the original run command",
        ));
    }
    if !matches!(state.as_str(), "dispatching" | "held" | "completed") {
        return Err(Failure::domain(
            "appointment_corrupt",
            "service appointment has an unknown persisted state",
        ));
    }
    let report = match bytes {
        Some(bytes) => {
            let report: crate::local_controller::ServiceReport = serde_json::from_slice(&bytes)
                .map_err(|e| Failure::domain("appointment_corrupt", e.to_string()))?;
            let expected_state = if report.outcome == crate::local_controller::ServiceOutcome::Held
            {
                "held"
            } else {
                "completed"
            };
            if report.appointment != appointment.as_str()
                || report.target != target
                || report.revision != revision
                || report.epoch != epoch
                || state != expected_state
            {
                return Err(Failure::domain(
                    "appointment_corrupt",
                    "stored service report does not match its appointment identity",
                ));
            }
            Some(
                serde_json::to_value(report)
                    .map_err(|e| Failure::domain("appointment_corrupt", e.to_string()))?,
            )
        }
        None if state == "dispatching" => None,
        None => {
            return Err(Failure::domain(
                "appointment_corrupt",
                "terminal service appointment has no stored report",
            ));
        }
    };
    let unknown = state == "dispatching";
    let reconciliation_required = unknown || state == "held";
    Ok((
        json!({"schema":"aethyme.collab-service-status/experimental-v0","appointment":appointment.as_str(),"target":target,"revision":revision,"epoch":epoch,"state":if unknown { "unknown" } else { state.as_str() },"report":report,"reconciliation_required":reconciliation_required,"automatic_replay":false}),
        format!(
            "appointment {}: {}; automatic replay is disabled\n",
            appointment.as_str(),
            if unknown {
                "outcome unknown"
            } else {
                state.as_str()
            }
        ),
    ))
}
pub(super) fn stop(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--epoch", "--reason"], &[])?;
    let expected = flags.one("--epoch")?;
    let reason = flags.one("--reason")?;
    parse_record(expected)?;
    if reason.trim().is_empty() {
        return Err(Failure::usage("--reason must be nonempty"));
    }
    let mut store = open(root)?;
    let witness = authority::status(&store).map_err(authority_error)?;
    if witness.epoch.as_str() != expected {
        return Err(Failure::refused(
            "stale_epoch",
            "observed epoch changed",
            "inspect authority status and reconcile before stopping",
        ));
    }
    let frozen = authority::freeze(&mut store, &witness.epoch, reason).map_err(authority_error)?;
    Ok((
        json!({"schema":"aethyme.collab-stop/experimental-v0","witness":frozen,"live_effects":"unknown","processes_cancelled":false}),
        "Local ingress frozen; running and external effects require reconciliation\n".into(),
    ))
}
pub(super) fn handoff_show(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--id"], &[])?;
    let id = parse_record(flags.one("--id")?)?;
    let store = open(root)?;
    let view = handoff::show(&store, &id).map_err(|e| {
        Failure::domain(
            if matches!(e, handoff::HandoffError::Missing) {
                "handoff_missing"
            } else {
                "failed"
            },
            e.to_string(),
        )
    })?;
    Ok(handoff_observation(view))
}
fn handoff_observation(view: handoff::HandoffView) -> Output {
    (
        json!({"schema":"aethyme.collab-handoff-show/experimental-v0","id":view.id.as_str(),"request":view.request,"state":view.state,"receipt":view.receipt.map(|r|r.to_string()),"historical_only":true,"grants_agent_authority":false}),
        "Retained handoff observation; no current agent authority\n".into(),
    )
}
pub(super) fn sync_status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    if !rest.is_empty() {
        return Err(Failure::usage("sync status takes no argument flags"));
    }
    let snapshot = observe(root)?;
    let mut counts = serde_json::Map::new();
    for table in [
        "local_exchange_bindings",
        "local_exchange_messages",
        "local_exchange_events",
        "local_exchange_streams",
    ] {
        require_table(&snapshot, table)?;
        let count: i64 = snapshot
            .connection()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .map_err(sqlite)?;
        counts.insert(table.into(), json!(count));
    }
    Ok((
        json!({"schema":"aethyme.collab-sync-status/experimental-v0","local_bookkeeping":counts,"remote_delivery_confirmed":false,"source_retention_promised":false}),
        "Local exchange bookkeeping; no remote delivery or source promise\n".into(),
    ))
}
pub(super) fn export(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(
        rest,
        &[
            "--target",
            "--issuer",
            "--source",
            "--record",
            "--audience",
            "--output",
        ],
        &["--source", "--record"],
    )?;
    let name = flags.one("--target")?;
    let issuer = flags.one("--issuer")?;
    let audience = flags.one("--audience")?;
    let output = flags.one("--output")?;
    let sources = flags
        .many("--source")
        .iter()
        .map(|s| SourceSnapshotId::parse(s).map_err(|e| Failure::usage(e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    let records = flags
        .many("--record")
        .iter()
        .map(|s| {
            let (id, digest) = s
                .split_once('@')
                .ok_or_else(|| Failure::usage("--record requires ID@SHA256"))?;
            let id = parse_record(id)?;
            let digest = parse_record(digest)?;
            Ok(restore::RecordRef {
                id,
                object: crate::collaboration_archive::ObjectDigest::from_bytes(
                    (0..32)
                        .map(|i| {
                            u8::from_str_radix(&digest.as_str()[7 + i * 2..9 + i * 2], 16)
                                .expect("validated canonical digest")
                        })
                        .collect::<Vec<_>>()
                        .try_into()
                        .expect("32 digest bytes"),
                ),
            })
        })
        .collect::<Result<Vec<_>, Failure>>()?;
    if sources.is_empty() || sources.len() > 64 || records.len() > 128 {
        return Err(Failure::usage(
            "export needs 1..64 sources and at most 128 records",
        ));
    }
    let mut store = open(root)?;
    let owner = existing_owner(&mut store, name, issuer)?;
    let scope = restore::Scope {
        origin: store.project().as_str().into(),
        visibility: "local_project".into(),
        audience: audience.into(),
    };
    let bytes = restore::export_to_path(
        &mut store,
        &owner,
        &scope,
        &restore::Selection { sources, records },
        Path::new(output),
    )
    .map_err(|e| {
        let code = match &e {
            restore::RestoreError::Invalid(_) => "invalid_export",
            restore::RestoreError::Unsupported(_) => "unsupported_profile",
            restore::RestoreError::Missing(_) => "export_missing",
            restore::RestoreError::Binding => "export_binding",
            restore::RestoreError::Unknown => "export_unknown",
            restore::RestoreError::Io(_) => "io",
            restore::RestoreError::Sqlite(_) => "sqlite",
            _ => "failed",
        };
        Failure::domain(code, e.to_string())
    })?;
    Ok((
        json!({"schema":"aethyme.collab-export/experimental-v0","profile":restore::PROFILE,"bytes":bytes,"historical_only":true,"grants_authority":false,"source_released":false,"publication_guard_held_through_sync":true}),
        "Historical source bundle written exclusively; source remains retained\n".into(),
    ))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ResourceAppointment {
    host_state: std::path::PathBuf,
    temporary_root: std::path::PathBuf,
    meter_image: String,
    memory_mib: u32,
    disk_mib: u32,
    process_slots: u32,
    job_slots: u32,
    host_floor_bytes: u64,
    daemon_floor_bytes: u64,
}
fn read_operator<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, Failure> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io)?;
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() || metadata.len() > MAX_SPEC {
        return Err(Failure::usage(
            "bounded regular operator specification required",
        ));
    }
    let mut bytes = vec![];
    file.take(MAX_SPEC + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_SPEC {
        return Err(Failure::usage("operator specification too large"));
    }
    serde_json::from_slice(&bytes).map_err(|e| Failure::usage(e.to_string()))
}

fn read_operator_bytes(path: &str, max_bytes: u64) -> Result<Vec<u8>, Failure> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(io)?;
    let metadata = file.metadata().map_err(io)?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(Failure::usage("bounded regular operator file required"));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > max_bytes {
        return Err(Failure::usage("operator file too large"));
    }
    Ok(bytes)
}
pub(super) fn target_run(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    use crate::execution_supervisor::DockerExecutor;
    use crate::execution_verifier;
    use crate::local_controller::{self as controller, ServicePolicy};
    let flags = Flags::parse(
        rest,
        &[
            "--target",
            "--issuer",
            "--appointment",
            "--resources",
            "--producer",
            "--generation",
            "--session",
            "--remote",
            "--destination",
            "--docker",
            "--endpoint",
            "--timeout",
            "--authorization",
            "--allow-replacement",
        ],
        &[],
    )?;
    let policy: ServicePolicy = read_operator(flags.one("--appointment")?)?;
    let resources: ResourceAppointment = read_operator(flags.one("--resources")?)?;
    let generation = flags
        .one("--generation")?
        .parse::<i64>()
        .map_err(|_| Failure::usage("positive producer generation required"))?;
    let session = flags
        .one("--session")?
        .parse::<i64>()
        .map_err(|_| Failure::usage("positive broker session required"))?;
    let timeout = flags
        .one("--timeout")?
        .parse::<u64>()
        .map_err(|_| Failure::usage("finite transport timeout required"))?;
    let replacement = match flags.one("--allow-replacement")? {
        "yes" => true,
        "no" => false,
        _ => return Err(Failure::usage("--allow-replacement requires yes or no")),
    };
    if generation < 1 || session < 1 || !(1..=60).contains(&timeout) {
        return Err(Failure::usage(
            "positive identities and timeout at most60 seconds required",
        ));
    }
    let cli = std::path::PathBuf::from(flags.one("--docker")?);
    let endpoint = flags.one("--endpoint")?;
    if !cli.is_absolute()
        || !resources.host_state.is_absolute()
        || !resources.temporary_root.is_absolute()
    {
        return Err(Failure::usage(
            "absolute native client and resource paths required",
        ));
    }
    let mut store = open(root)?;
    let owner = existing_owner(&mut store, flags.one("--target")?, flags.one("--issuer")?)?;
    let mut broker =
        crate::Broker::open(root).map_err(|e| Failure::domain("broker", e.to_string()))?;
    let carrier = crate::local_source_transport::GitCarrier::new(
        &mut broker,
        session,
        flags.one("--remote")?,
        flags.one("--destination")?,
        timeout,
        flags.one("--authorization")?,
        replacement,
    )
    .map_err(|e| Failure::domain("transport", e.to_string()))?;
    crate::local_controller::bare_destination::validate_endpoint(&carrier.identity.push_url)
        .map_err(|e| {
            Failure::refused(
                "unsupported_profile",
                e.to_string(),
                "appoint an exact local bare Git destination without linked worktrees",
            )
        })?;
    let permit =
        execution_verifier::existing_permit(&store, &owner, flags.one("--producer")?, generation)
            .map_err(|e| Failure::domain("verifier_authority", e.to_string()))?;
    let native = DockerExecutor::probe_at(cli, endpoint)
        .map_err(|e| Failure::domain("native_runtime", e.to_string()))?;
    let resource_policy = crate::execution_resources::ResourcePolicy {
        host_state: resources.host_state,
        temporary_root: resources.temporary_root,
        runtime: native.runtime().id(),
        daemon: native.runtime().daemon_id.clone(),
        meter_image: resources.meter_image,
        memory_mib: resources.memory_mib,
        disk_mib: resources.disk_mib,
        process_slots: resources.process_slots,
        job_slots: resources.job_slots,
        host_floor_bytes: resources.host_floor_bytes,
        daemon_floor_bytes: resources.daemon_floor_bytes,
    };
    let executor = native
        .with_resources(resource_policy)
        .map_err(|e| Failure::domain("host_resources", e.to_string()))?;
    let report = controller::serve(
        &mut store,
        &owner,
        &mut broker,
        &carrier,
        &executor,
        &permit,
        &policy,
        &std::sync::atomic::AtomicBool::new(false),
    )
    .map_err(|e| Failure::domain("controller", e.to_string()))?;
    let text = format!(
        "Local appointment {}: {:?}; {} accepted wave(s) in {} tick(s)\n",
        report.appointment,
        report.outcome,
        report.completions.len(),
        report.ticks
    );
    Ok((
        serde_json::to_value(report)
            .map_err(|e| Failure::domain("controller_report", e.to_string()))?,
        text,
    ))
}

pub(super) fn unsupported() -> Failure {
    Failure::refused(
        "unsupported_profile",
        "no qualified appointed adapter is installed for this lifecycle command",
        "use the reviewed Local package APIs with an explicitly qualified native adapter; gate integration and composition qualification remain pending",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collaboration_state::inspect_at;
    use crate::local_target::tests::Fixture;

    #[test]
    fn lifecycle_flags_reject_duplicates_unknowns_and_unbounded_input() {
        assert!(Flags::parse(&["--target", "A", "--target", "B"], &["--target"], &[]).is_err());
        assert!(Flags::parse(&["--unknown", "A"], &["--target"], &[]).is_err());
        assert!(Flags::parse(&["--target"], &["--target"], &[]).is_err());
        assert!(Flags::parse(&["--target", "--issuer"], &["--target"], &[]).is_err());
        let long = "x".repeat(4097);
        assert!(Flags::parse(&["--target", &long], &["--target"], &[]).is_err());
        let valid = Flags::parse(
            &["--source", "A", "--source", "B", "--target", "T"],
            &["--source", "--target"],
            &["--source"],
        )
        .unwrap();
        assert_eq!(valid.many("--source"), vec!["A", "B"]);
        assert_eq!(valid.one("--target").unwrap(), "T");
    }
    #[test]
    fn verifier_prepare_refuses_oversized_literal_inventory_before_opening_state() {
        let directory = tempfile::tempdir().unwrap();
        let plan = directory.path().join("plan.json");
        std::fs::write(
            &plan,
            serde_json::to_vec(&verifier::VerifierPlan {
                image: format!("sha256:{}", "0".repeat(64)),
                command: crate::execution_snapshot::CommandSpec {
                    program: "/bin/sh".into(),
                    arguments: vec!["/artifact/app.sh".into()],
                    environment: Default::default(),
                },
                max_age_ms: 1000,
            })
            .unwrap(),
        )
        .unwrap();
        let inventory = directory.path().join("inventory.json");
        std::fs::write(&inventory, vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();
        let operation = OperationId::mint().unwrap().to_string();
        let args = [
            "--harness-repo",
            "/no-such-harness",
            "--commit",
            "not-a-commit",
            "--inventory",
            inventory.to_str().unwrap(),
            "--operation",
            operation.as_str(),
            "--docker",
            "/no-such-docker",
            "--endpoint",
            "unix:///no-such-endpoint",
            "--file",
            plan.to_str().unwrap(),
        ];
        let failure = verifier_prepare(directory.path(), &args).unwrap_err();
        assert_eq!(failure.exit, exit_status::USAGE);
        assert_eq!(failure.message, "operator file too large");
    }
    #[test]
    fn native_gate_retention_cli_rejects_invalid_and_incomplete_arguments() {
        let root = tempfile::tempdir().unwrap();
        let too_many: Vec<&str> = (0..=MAX_ARGUMENTS / 2)
            .flat_map(|_| ["--target", "T"])
            .collect();
        for (args, expected) in [
            (
                &["unknown"][..],
                "native-gates requires list, status, or release",
            ),
            (&["list"][..], "--target is required"),
            (&["list", "--target", "T"][..], "--issuer is required"),
            (
                &["list", "--target", "T", "--issuer", "I", "--job", "bad"][..],
                "native-gates list does not take --job",
            ),
            (
                &["status", "--target", "T", "--issuer", "I"][..],
                "native-gates status and release require --job",
            ),
            (
                &["release", "--target", "T", "--issuer", "I", "--job", "bad"][..],
                "malformed record id \"bad\": expected sha256:<64 lowercase hex>",
            ),
        ] {
            let failure = native_gate_retention(root.path(), args).unwrap_err();
            assert_eq!(failure.exit, exit_status::USAGE, "args: {args:?}");
            assert_eq!(failure.message, expected, "args: {args:?}");
        }
        let mut oversized = vec!["list"];
        oversized.extend(too_many);
        let failure = native_gate_retention(root.path(), &oversized).unwrap_err();
        assert_eq!(failure.exit, exit_status::USAGE);
        assert_eq!(failure.message, "expected a finite set of flag/value pairs");
    }
    #[test]
    fn malformed_regular_target_input_and_symlinks_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input");
        std::fs::write(&path, b"{\"unknown\":true}").unwrap();
        assert_eq!(
            read_spec(path.to_str().unwrap()).unwrap_err().exit,
            exit_status::USAGE
        );
        std::fs::write(&path, vec![b'x'; MAX_SPEC as usize + 1]).unwrap();
        assert_eq!(
            read_spec(path.to_str().unwrap()).unwrap_err().code,
            "file_too_large"
        );
        assert!(read_spec(dir.path().to_str().unwrap()).is_err());
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert!(read_spec(link.to_str().unwrap()).is_err());
            let fifo = dir.path().join("pipe");
            let output = std::process::Command::new("/usr/bin/mkfifo")
                .arg(&fifo)
                .output()
                .unwrap();
            assert!(output.status.success());
            assert!(read_spec(fifo.to_str().unwrap()).is_err());
        }
    }
    #[test]
    fn whitespace_operator_labels_cannot_initialize_native_authority() {
        let mut f = Fixture::new();
        let snapshot = inspect_at(f.store.project_dir().into(), f.store.project()).unwrap();
        for (name, issuer) in [("   ", "issuer"), ("T", "   ")] {
            assert!(matches!(
                target::acquire_controller(&mut f.store, name, issuer),
                Err(target::TargetError::Invalid(_))
            ));
            assert_eq!(
                authority::inspect(&snapshot).unwrap().state,
                "unbound",
                "invalid labels must not initialize authority"
            );
            let rows: i64 = f
                .store
                .read_connection()
                .query_row("SELECT count(*) FROM local_targets", [], |r| r.get(0))
                .unwrap();
            assert_eq!(rows, 0);
            assert!(
                Flags::parse(
                    &["--target", name, "--issuer", issuer],
                    &["--target", "--issuer"],
                    &[]
                )
                .is_err()
            );
        }
    }
    #[test]
    fn unknown_owner_does_not_appoint_or_initialize_authority() {
        let mut f = Fixture::new();
        let snapshot = inspect_at(f.store.project_dir().into(), f.store.project()).unwrap();
        assert_eq!(authority::inspect(&snapshot).unwrap().state, "unbound");
        let before: i64 = f
            .store
            .read_connection()
            .query_row("SELECT count(*) FROM local_targets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(
            existing_owner(&mut f.store, "unknown", "issuer")
                .unwrap_err()
                .code,
            "target_missing"
        );
        let after: i64 = f
            .store
            .read_connection()
            .query_row("SELECT count(*) FROM local_targets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(before, after);
        assert_eq!(authority::inspect(&snapshot).unwrap().state, "unbound");
    }
    #[test]
    fn proposal_preflight_rejects_malformed_and_unretained_subjects_without_appointment() {
        let f = Fixture::new();
        let snapshot = inspect_at(f.store.project_dir().into(), f.store.project()).unwrap();
        let mut spec = f.spec();
        spec.objective_authorization = "invalid".into();
        assert!(target::validate_proposal(&f.store, spec).is_err());
        let mut spec = f.spec();
        spec.baseline = format!("sha256:{}", "1".repeat(64));
        assert!(target::validate_proposal(&f.store, spec).is_err());
        let count: i64 = f
            .store
            .read_connection()
            .query_row("SELECT count(*) FROM local_targets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        assert_eq!(authority::inspect(&snapshot).unwrap().state, "unbound");
        assert!(target::validate_proposal(&f.store, f.spec()).is_ok());
        assert_eq!(authority::inspect(&snapshot).unwrap().state, "unbound");
    }
    #[test]
    fn existing_owner_checks_issuer_before_acquisition_and_respects_live_lock() {
        let mut f = Fixture::new();
        let owner = target::acquire_controller(&mut f.store, "T", "issuer").unwrap();
        let before = authority::status(&f.store).unwrap();
        assert_eq!(
            existing_owner(&mut f.store, "T", "wrong").unwrap_err().code,
            "issuer_mismatch"
        );
        assert_eq!(
            existing_owner(&mut f.store, "T", "issuer")
                .unwrap_err()
                .code,
            "controller_busy"
        );
        assert_eq!(authority::status(&f.store).unwrap(), before);
        drop(owner);
        assert!(existing_owner(&mut f.store, "T", "issuer").is_ok());
    }
    #[test]
    fn bounded_authority_inspection_exposes_epoch_without_changing_files() {
        let mut f = Fixture::new();
        let owner = target::acquire_controller(&mut f.store, "T", "issuer").unwrap();
        let snapshot = inspect_at(f.store.project_dir().into(), f.store.project()).unwrap();
        let database = f.store.project_dir().join("state.db");
        let before = std::fs::read(&database).unwrap();
        let witness = authority::status(&f.store).unwrap();
        let observed = authority::inspect(&snapshot).unwrap();
        assert_eq!(observed.state, "active");
        assert!(observed.binding_matches);
        assert_eq!(observed.witness.as_ref().unwrap(), &witness);
        assert_eq!(std::fs::read(&database).unwrap(), before);
        assert_eq!(authority::status(&f.store).unwrap(), witness);
        f.store
            .connection()
            .execute(
                "UPDATE local_authority_state SET epoch=?1",
                [format!("sha256:{}", "1".repeat(64))],
            )
            .unwrap();
        let drift = authority::inspect(&snapshot).unwrap();
        assert_eq!(drift.state, "drift");
        assert!(!drift.binding_matches);
        f.store
            .connection()
            .execute(
                "UPDATE local_authority_state SET epoch=?1",
                [owner.authority_epoch().as_str()],
            )
            .unwrap();
        let frozen = authority::freeze(
            &mut f.store,
            &observed.witness.unwrap().epoch,
            "operator stop",
        )
        .unwrap();
        assert_eq!(authority::inspect(&snapshot).unwrap().state, "frozen");
        assert!(matches!(
            authority::freeze(&mut f.store, owner.authority_epoch(), "stale"),
            Err(authority::AuthorityError::Stale)
        ));
        assert_eq!(authority::status(&f.store).unwrap(), frozen);
    }
    #[cfg(unix)]
    #[test]
    fn retained_handoff_projection_preserves_request_without_agent_authority() {
        // This package fixture models trusted verifier evidence; its projection
        // is not a live provider or native verifier qualification result.
        let mut f = crate::local_generation::tests::Fixture::new();
        let generation = f.prepare("cli-observation").unwrap();
        let agent = handoff::appoint(&mut f.v.f.store, &f.v.owner, "cli-observer").unwrap();
        let grant = crate::local_acceptance::qualify(
            &mut f.v.f.store,
            &f.v.owner,
            &mut f.broker,
            &f.carrier,
            &f.binding,
            &f.v.execution,
            &f.v.artifact,
            &f.v.permit,
            &f.evidence,
        )
        .unwrap();
        let expiry = grant.expires_ms().min(crate::clock::epoch_ms() + 120_000);
        let request = handoff::prepare(
            &mut f.v.f.store,
            &f.v.owner,
            &agent,
            &mut f.broker,
            &f.carrier,
            &grant,
            generation.id(),
            "cli-inspection",
            expiry,
        )
        .unwrap();
        drop(grant);
        let retained = handoff::show(&f.v.f.store, request.id()).unwrap();
        let exact = serde_json::to_value(&retained.request).unwrap();
        let (json, text) = handoff_observation(retained);
        assert_eq!(json["id"], request.id().as_str());
        assert_eq!(json["state"], "prepared");
        assert_eq!(json["request"], exact);
        assert!(json["receipt"].is_null());
        assert_eq!(json["historical_only"], true);
        assert_eq!(
            json["grants_agent_authority"], false,
            "retained inspection cannot grant current agent authority"
        );
        assert!(text.contains("no current agent authority"));
        assert_eq!(
            handoff::show(&f.v.f.store, request.id()).unwrap().state,
            "prepared"
        );
    }
}
