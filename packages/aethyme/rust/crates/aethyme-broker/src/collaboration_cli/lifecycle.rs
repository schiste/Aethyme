//! Explicit operator commands; all native capabilities are acquired locally.
use super::*;
use crate::collaboration_state::{CollaborationSnapshot, inspect_for_repository};
use crate::{
    local_authority as authority, local_handoff as handoff, local_restore as restore,
    local_target as target,
};
use rusqlite::OptionalExtension;
use std::collections::BTreeMap;
use std::io::Write;

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
    let bytes = restore::export(
        &store,
        &owner,
        &scope,
        &restore::Selection { sources, records },
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
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(output).map_err(io)?;
    file.write_all(&bytes).map_err(io)?;
    file.sync_all().map_err(io)?;
    let parent = Path::new(output)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    crate::collaboration_state::sync_directory(parent)
        .map_err(|e| Failure::domain(e.code(), e.to_string()))?;
    Ok((
        json!({"schema":"aethyme.collab-export/experimental-v0","profile":restore::PROFILE,"bundle":crate::collaboration_archive::ObjectDigest::of(&bytes).hex(),"bytes":bytes.len(),"historical_only":true,"grants_authority":false,"source_released":false}),
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
    let native =
        DockerExecutor::probe(cli).map_err(|e| Failure::domain("native_runtime", e.to_string()))?;
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
