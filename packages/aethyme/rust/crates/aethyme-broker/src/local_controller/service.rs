//! Finite native service appointment. Arrivals are read from durable state;
//! no callback or public receipt can manufacture a successful execution.
use super::*;
use crate::collaboration_archive::ObjectDigest;
use crate::execution_supervisor::DockerExecutor;
use crate::execution_verifier::ProducerPermit;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const SERVICE_SCHEMA: RecordSchema = RecordSchema {
    name: "aethyme.local-controller-service/experimental-v1",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};

/// Validate the immutable appointment body against its indexed service row.
/// This is used by lost-response status lookup, including lookups that span
/// revisions or authority epochs. The row is not a dispatch capability.
pub(crate) fn validate_status_binding(
    appointment: &str,
    target: &str,
    revision: &str,
    epoch: &str,
    nonce: &str,
    body: &[u8],
) -> Result<(), ControllerError> {
    let record = Record::decode(body, &[&SERVICE_SCHEMA])
        .map_err(|error| ControllerError::Canonical(error.to_string()))?;
    if record.canonical_bytes() != body {
        return Err(ControllerError::Canonical(
            "stored service appointment body is not canonical".into(),
        ));
    }
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let spec = value
        .get("spec")
        .and_then(serde_json::Value::as_object)
        .ok_or(ControllerError::Mismatch)?;
    let policy_nonce = spec
        .get("policy")
        .and_then(serde_json::Value::as_object)
        .and_then(|policy| policy.get("nonce"))
        .and_then(serde_json::Value::as_str);
    if record.id().as_str() != appointment
        || spec.get("target").and_then(serde_json::Value::as_str) != Some(target)
        || spec.get("revision").and_then(serde_json::Value::as_str) != Some(revision)
        || spec.get("epoch").and_then(serde_json::Value::as_str) != Some(epoch)
        || policy_nonce != Some(nonce)
    {
        return Err(ControllerError::Mismatch);
    }
    Ok(())
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServicePolicy {
    pub nonce: String,
    pub drive: DrivePolicy,
    pub max_ticks: u32,
    pub max_completions: u32,
    pub poll_ms: u64,
    pub max_elapsed_ms: i64,
}
impl ServicePolicy {
    fn validate(&self) -> Result<(), ControllerError> {
        self.drive.validate()?;
        if self.nonce.is_empty()
            || self.nonce.len() > 128
            || !self
                .nonce
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
            || !(1..=4096).contains(&self.max_ticks)
            || !(1..=256).contains(&self.max_completions)
            || !(10..=1000).contains(&self.poll_ms)
            || !(1..=900000).contains(&self.max_elapsed_ms)
        {
            return Err(ControllerError::Mismatch);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceOutcome {
    CompletionAllowance,
    TickAllowance,
    Deadline,
    Cancelled,
    Held,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceReport {
    pub appointment: String,
    pub target: String,
    pub revision: String,
    pub epoch: String,
    pub ticks: u32,
    pub completions: Vec<CompletionSpec>,
    pub outcome: ServiceOutcome,
    pub detail: Option<String>,
}
fn now() -> Result<i64, ControllerError> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| ControllerError::Canonical(e.to_string()))?
            .as_millis(),
    )
    .map_err(|_| ControllerError::Mismatch)
}
fn finish(store: &mut CollaborationStore, report: &ServiceReport) -> Result<(), ControllerError> {
    let state = if report.outcome == ServiceOutcome::Held {
        "held"
    } else {
        "completed"
    };
    if store.connection().execute("UPDATE local_controller_services SET state=?2,report=?3 WHERE id=?1 AND state='dispatching'",params![report.appointment,state,serde_json::to_vec(report)?])?!=1 {return Err(ControllerError::Mismatch);}
    Ok(())
}
/// Operator-appointed finite service. Unknown appointments never resume or
/// replay automatically. Stop/deadline/currentness are checked between stages;
/// the inherited native adapters retain their disclosed finite process bounds.
#[allow(clippy::too_many_arguments)]
pub fn serve(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    broker: &mut Broker,
    carrier: &GitCarrier,
    executor: &DockerExecutor,
    permit: &ProducerPermit,
    policy: &ServicePolicy,
    cancel: &AtomicBool,
) -> Result<ServiceReport, ControllerError> {
    policy.validate()?;
    let _authority = target::authorize(store, owner)?;
    if cancel.load(Ordering::SeqCst) {
        return Err(ControllerError::Mismatch);
    }
    let observed = target::status(store, owner.target())?;
    let revision = observed.revision.ok_or(ControllerError::Mismatch)?;
    let pinned = target::load_revision(store, &revision)?;
    if pinned.spec.acceptance_policy != permit.policy().record()?.id().as_str()
        || policy.drive.execution.profile.runtime_profile != executor.runtime().id()
    {
        return Err(ControllerError::Mismatch);
    }
    carrier.check_broker(broker)?;
    carrier.check()?;
    let destination = super::bare_destination::validate_endpoint(&carrier.identity.push_url)?;
    let started = now()?;
    let deadline = started
        .checked_add(policy.max_elapsed_ms)
        .ok_or(ControllerError::Mismatch)?;
    let record=Record::decode(&serde_json::to_vec(&json!({"schema":SERVICE_SCHEMA.name,"spec":{"target":owner.target(),"revision":revision.as_str(),"epoch":owner.authority_epoch().as_str(),"producer":permit.producer(),"producer_generation":permit.generation(),"verifier_policy":permit.policy().record()?.id().as_str(),"runtime":executor.runtime().id(),"transport":{"session":carrier.session,"remote_alias":carrier.remote,"destination":carrier.destination,"endpoint_sha256":ObjectDigest::of(carrier.identity.push_url.as_bytes()).hex(),"bare_path_sha256":ObjectDigest::of(destination.path().as_os_str().as_encoded_bytes()).hex()},"policy":policy,"started_ms":started,"deadline_ms":deadline}}))?,&[&SERVICE_SCHEMA]).map_err(|e|ControllerError::Canonical(e.to_string()))?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let count: i64 = tx.query_row(
        "SELECT count(*) FROM local_controller_services WHERE revision=?1",
        [revision.as_str()],
        |r| r.get(0),
    )?;
    if count >= 128 {
        return Err(ControllerError::DispatchHeld(
            "service appointment allowance exhausted".into(),
        ));
    }
    tx.execute("INSERT INTO local_controller_services(id,target,revision,epoch,nonce,body,state) VALUES(?1,?2,?3,?4,?5,?6,'dispatching')",params![record.id().as_str(),owner.target(),revision.as_str(),owner.authority_epoch().as_str(),policy.nonce,record.canonical_bytes()])?;
    tx.commit()?;
    drop(_authority);
    let mut report = ServiceReport {
        appointment: record.id().to_string(),
        target: owner.target().into(),
        revision: revision.to_string(),
        epoch: owner.authority_epoch().as_str().into(),
        ticks: 0,
        completions: vec![],
        outcome: ServiceOutcome::TickAllowance,
        detail: None,
    };
    loop {
        if cancel.load(Ordering::SeqCst) {
            report.outcome = ServiceOutcome::Cancelled;
            break;
        }
        if now()? >= deadline {
            report.outcome = ServiceOutcome::Deadline;
            break;
        }
        if report.ticks >= policy.max_ticks {
            break;
        }
        if report.completions.len() >= policy.max_completions as usize {
            report.outcome = ServiceOutcome::CompletionAllowance;
            break;
        }
        let current = target::status(store, owner.target())?;
        if current.revision.as_ref() != Some(&revision) || target::authorize(store, owner).is_err()
        {
            report.outcome = ServiceOutcome::Held;
            report.detail = Some("appointment authority or target revision changed".into());
            break;
        }
        report.ticks += 1;
        match super::drive::drive_until(
            store,
            owner,
            broker,
            carrier,
            executor,
            permit,
            &policy.drive,
            cancel,
            now()?,
            Some(deadline),
            Some(&destination),
        ) {
            Ok(DriveResult::Completed {
                completion,
                historical: false,
            }) => report.completions.push(completion),
            Ok(DriveResult::Waiting(_)) => {
                let wake = std::time::Instant::now() + Duration::from_millis(policy.poll_ms);
                while std::time::Instant::now() < wake && !cancel.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Ok(DriveResult::Held { detail, .. }) => {
                report.outcome = ServiceOutcome::Held;
                report.detail = Some(detail);
                break;
            }
            Ok(DriveResult::Completed {
                historical: true, ..
            }) => {
                report.outcome = ServiceOutcome::Held;
                report.detail =
                    Some("historical completion cannot satisfy a new appointment".into());
                break;
            }
            Err(error) => {
                report.outcome = ServiceOutcome::Held;
                report.detail = Some(error.to_string().chars().take(4096).collect());
                break;
            }
        }
    }
    finish(store, &report)?;
    Ok(report)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_appointment_refuses_unbounded_or_ambiguous_limits() {
        let drive = super::super::drive::tests::policy();
        let good = ServicePolicy {
            nonce: "appointed-1".into(),
            drive,
            max_ticks: 32,
            max_completions: 20,
            poll_ms: 20,
            max_elapsed_ms: 300000,
        };
        good.validate().unwrap();
        let mut bad = good.clone();
        bad.max_ticks = 0;
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.max_ticks = 4097;
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.max_completions = 257;
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.poll_ms = 1;
        assert!(bad.validate().is_err());
        let mut bad = good.clone();
        bad.max_elapsed_ms = 900001;
        assert!(bad.validate().is_err());
        let mut bad = good;
        bad.nonce = "secret/path".into();
        assert!(bad.validate().is_err());
    }
}

#[cfg(test)]
mod permit_tests {
    use super::*;
    use crate::execution_verifier::{self as verifier, VerifierPolicy};
    use crate::local_target::tests::Fixture;
    #[test]
    fn existing_producer_appointment_preserves_generation_and_refuses_revocation() {
        let mut f = Fixture::new();
        let owner =
            target::acquire_controller(&mut f.store, "existing-verifier", "operator").unwrap();
        let execution = super::super::drive::tests::policy().execution;
        let policy = VerifierPolicy {
            profile: verifier::PROFILE.into(),
            runtime: execution.profile.runtime_profile.clone(),
            image: execution.profile.verifier_image.clone(),
            harness: execution.harness.clone(),
            inventory: execution.assertion_inventory.clone(),
            command: execution.verify.clone(),
            max_age_ms: 300000,
        };
        let mut selected = f.spec();
        selected.acceptance_policy = policy.record().unwrap().id().to_string();
        let revision = target::propose_revision(&mut f.store, &owner, selected).unwrap();
        target::select_revision(&mut f.store, &owner, &revision.id).unwrap();
        let first =
            verifier::configure(&mut f.store, &owner, "existing-only", None, policy).unwrap();
        let existing =
            verifier::existing_permit(&f.store, &owner, "existing-only", first.generation())
                .unwrap();
        assert_eq!(existing.generation(), first.generation());
        assert_eq!(existing.policy(), first.policy());
        assert!(
            verifier::existing_permit(&f.store, &owner, "existing-only", first.generation() + 1)
                .is_err(),
            "wrong-generation must refuse without rotation"
        );
        verifier::revoke(&mut f.store, &owner, "existing-only", first.generation()).unwrap();
        assert!(
            verifier::existing_permit(&f.store, &owner, "existing-only", first.generation())
                .is_err(),
            "revoked catalogue is never revived"
        );
        let (generation,revoked):(i64,bool)=f.store.read_connection().query_row("SELECT generation,revoked FROM local_verifier_producers WHERE target='existing-verifier' AND producer='existing-only'",[],|r|Ok((r.get(0)?,r.get(1)?))).unwrap();
        assert_eq!(generation, first.generation() + 1);
        assert!(revoked);
    }
}
