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
                || value.is_empty()
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
pub(super) fn target_status(root: &Path, rest: &[&str]) -> Result<Output, Failure> {
    let flags = Flags::parse(rest, &["--target"], &[])?;
    let name = flags.one("--target")?;
    let snapshot = observe(root)?;
    require_table(&snapshot, "local_targets")?;
    let row: Option<(String,Option<String>,String,Option<String>,i64)>=snapshot.connection().query_row("SELECT issuer,active_revision,phase,resume_phase,version FROM local_targets WHERE target=?1",[name],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(sqlite)?;
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
    Ok((
        json!({"schema":"aethyme.collab-handoff-show/experimental-v0","id":view.id.as_str(),"request":view.request,"state":view.state,"receipt":view.receipt.map(|r|r.to_string()),"historical_only":true,"grants_agent_authority":false}),
        "Retained handoff observation; no current agent authority\n".into(),
    ))
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
}
