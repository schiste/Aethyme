use super::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Stage {
    Intent,
    Spawned,
    Ready,
    Adopted,
}
/// Fixed wrapper, private READY/GO pipes, and owned kernel Child. Workload stdout
/// and stderr are discarded by this narrow profile; results use separate guarded
/// APIs. The helper acknowledges its physical root before receiving GO.
#[allow(clippy::too_many_arguments)]
pub fn dispatch(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    agent: &Agent,
    broker: &mut Broker,
    carrier: &GitCarrier,
    grant: &QualifiedAcceptance<'_>,
    request: &PreparedHandoff,
    command: CommandSpec,
) -> Result<RunningCommand, HandoffError> {
    dispatch_with(
        store,
        owner,
        agent,
        broker,
        carrier,
        grant,
        request,
        command,
        &mut |_, _| Ok(()),
    )
}
#[allow(clippy::too_many_arguments)]
pub(super) fn dispatch_with(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    agent: &Agent,
    broker: &mut Broker,
    carrier: &GitCarrier,
    grant: &QualifiedAcceptance<'_>,
    request: &PreparedHandoff,
    command: CommandSpec,
    hook: &mut impl FnMut(Stage, &mut CollaborationStore) -> Result<(), HandoffError>,
) -> Result<RunningCommand, HandoffError> {
    command.validate()?;
    let _archive = gc::archive_use(store)?;
    if agent.project != store.project_dir()
        || request.spec.target != agent.target
        || request.spec.session != agent.session
        || request.spec.agent_nonce != agent.nonce
        || request.spec.epoch != agent.epoch.as_str()
    {
        return Err(HandoffError::Stale);
    };
    let view = show(store, &request.id)?;
    if view.state != "prepared" || view.request != request.spec {
        return Err(HandoffError::Unknown);
    };
    let prepared = generation::load_prepared(store, &id(&request.spec.generation)?)?;
    mapping(&prepared, owner, grant)?;
    if prepared.absolute_root() != request.spec.absolute_root
        || prepared.receipt().as_str() != request.spec.prepared_receipt
        || prepared.intent().source != request.spec.source
        || now()? >= request.spec.expires_ms
    {
        return Err(HandoffError::Stale);
    };
    let root = prepared.absolute_root().to_owned();
    let root_bytes = root.as_os_str().as_encoded_bytes();
    if root_bytes.len() > 4096 || root_bytes.contains(&b'\n') || root_bytes.contains(&b'\r') {
        return Err(invalid("acknowledgment root"));
    }
    // Keep both the native producer/acceptance guard and external epoch lock
    // through READY, the adoption commit and GO. Freeze cannot cross dispatch.
    let guard = acceptance::current_guard(store, owner, broker, carrier, grant)?;
    let target_version = target::status(store, owner.target())?.version;
    let spec = DispatchSpec {
        profile: PROFILE.into(),
        handoff: request.id.to_string(),
        command: command.clone(),
    };
    let dispatch_record = record(&DISPATCH, &spec)?;
    let digest = archive::put_object(store, &dispatch_record.canonical_bytes())?;
    {
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_target(&tx, &request.spec, target_version)?;
        if agent_current(&tx, owner, agent)? != "paused" {
            return Err(HandoffError::Busy);
        };
        let changed=tx.execute("UPDATE local_handoffs SET state='dispatching' WHERE id=?1 AND state='prepared' AND body=?2",params![request.id.as_str(),record(&REQUEST,&request.spec)?.canonical_bytes()])?;
        if changed != 1 {
            return Err(HandoffError::Unknown);
        };
        tx.execute("INSERT INTO local_agent_dispatches(id,handoff,command,state) VALUES(?1,?2,?3,'intent')",params![dispatch_record.id().as_str(),request.id.as_str(),dispatch_record.canonical_bytes()])?;
        tx.execute(
            "UPDATE local_agent_sessions SET phase='unknown' WHERE target=?1 AND session=?2",
            params![agent.target, agent.session],
        )?;
        pin(&tx, &digest, dispatch_record.id().as_str())?;
        guard.check_clock()?;
        if now()? >= request.spec.expires_ms {
            return Err(HandoffError::Deadline);
        };
        tx.commit()?;
    }
    hook(Stage::Intent, store)?;
    guard.check_clock()?;
    if now()? >= request.spec.expires_ms {
        return Err(HandoffError::Deadline);
    };
    let mut native = Command::new("/bin/sh");
    native.args(["-c","pwd -P || exit 125; IFS= read -r gate || exit 125; test \"$gate\" = GO || exit 125; exec \"$@\" >/dev/null 2>&1","aethyme-controlled-session"])
        .arg(&command.executable).args(&command.arguments).current_dir(&root)
        .env_clear().env("PATH","/usr/bin:/bin").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let child = native.spawn()?;
    let mut running = RunningCommand {
        child,
        dispatch: dispatch_record.id(),
        handoff: request.id.clone(),
        generation: prepared.id().clone(),
        session: agent.session.clone(),
        nonce: agent.nonce.clone(),
        epoch: agent.epoch.clone(),
        project: agent.project.clone(),
        root: root.clone(),
        settled: false,
    };
    store.connection().execute(
        "UPDATE local_agent_dispatches SET pid=?2 WHERE id=?1",
        params![running.dispatch.as_str(), running.child.id()],
    )?;
    hook(Stage::Spawned, store)?;
    let stdout = running
        .child
        .stdout
        .take()
        .ok_or_else(|| invalid("native acknowledgment pipe"))?;
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = BufReader::new(stdout.take(4098))
            .read_until(b'\n', &mut bytes)
            .map(|_| bytes);
        if sender.send(result).is_err() { /* Parent timed out; no current authority is inferred. */
        }
    });
    let deadline =
        std::time::Instant::now() + Duration::from_millis(u64::from(command.acknowledgment_ms));
    let result =
        receiver.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()));
    let bytes = match result {
        Ok(result) => result?,
        Err(_) => {
            crate::warn_unrecorded("handoff timed out owned child kill", running.child.kill());
            crate::warn_unrecorded("handoff timed out owned child reap", running.child.wait());
            running.settled = true;
            if reader.join().is_err() {
                return Err(invalid("ack reader panic"));
            }
            return Err(HandoffError::Deadline);
        }
    };
    if reader.join().is_err() {
        return Err(invalid("ack reader panic"));
    }
    if bytes.len() != root_bytes.len() + 1
        || bytes.last() != Some(&b'\n')
        || &bytes[..bytes.len() - 1] != root_bytes
    {
        return Err(invalid("native physical root"));
    }
    hook(Stage::Ready, store)?;
    // The wrapper is paused at the gate, so the complete prepared checkout can
    // still be checked before any appointed workload receives GO.
    let checked = generation::load_prepared(store, prepared.id())?;
    if checked != prepared
        || std::time::Instant::now() >= deadline
        || now()? >= request.spec.expires_ms
    {
        return Err(HandoffError::Deadline);
    };
    let adopted = AdoptionSpec {
        profile: PROFILE.into(),
        handoff: request.id.to_string(),
        dispatch: running.dispatch.to_string(),
        generation: prepared.id().to_string(),
        source: prepared.intent().source.clone(),
        session: agent.session.clone(),
        agent_nonce: agent.nonce.clone(),
        epoch: agent.epoch.as_str().into(),
        absolute_root: root,
        native_pid: running.child.id(),
    };
    let receipt = record(&ADOPTION, &adopted)?;
    let receipt_digest = archive::put_object(store, &receipt.canonical_bytes())?;
    {
        let tx = store
            .connection()
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        check_target(&tx, &request.spec, target_version)?;
        if agent_current(&tx, owner, agent)? != "unknown" {
            return Err(HandoffError::Stale);
        };
        let changed=tx.execute("UPDATE local_handoffs SET state='adopted',receipt=?2 WHERE id=?1 AND state='dispatching' AND body=?3",params![request.id.as_str(),receipt.id().as_str(),record(&REQUEST,&request.spec)?.canonical_bytes()])?;
        if changed != 1 {
            return Err(HandoffError::Unknown);
        };
        tx.execute(
            "INSERT INTO local_adoption_receipts(id,handoff,body) VALUES(?1,?2,?3)",
            params![
                receipt.id().as_str(),
                request.id.as_str(),
                receipt.canonical_bytes()
            ],
        )?;
        tx.execute(
            "UPDATE local_agent_dispatches SET state='acknowledged' WHERE id=?1 AND state='intent'",
            [running.dispatch.as_str()],
        )?;
        tx.execute("UPDATE local_agent_sessions SET current_handoff=?3,phase='running' WHERE target=?1 AND session=?2",params![agent.target,agent.session,request.id.as_str()])?;
        pin(&tx, &receipt_digest, receipt.id().as_str())?;
        guard.check_clock()?;
        if now()? >= request.spec.expires_ms || std::time::Instant::now() >= deadline {
            return Err(HandoffError::Deadline);
        };
        tx.commit()?;
    }
    hook(Stage::Adopted, store)?;
    guard.check_clock()?;
    if now()? >= request.spec.expires_ms || std::time::Instant::now() >= deadline {
        return Err(HandoffError::Deadline);
    };
    // Hold the writer reservation from final target admission through GO.
    // Target stop, revision changes and another handoff cannot cross dispatch.
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    check_target(&tx, &request.spec, target_version)?;
    if agent_current(&tx, owner, agent)? != "running" {
        return Err(HandoffError::Stale);
    }
    let go = running
        .child
        .stdin
        .take()
        .ok_or_else(|| invalid("native GO pipe"))?
        .write_all(b"GO\n");
    if let Err(error) = go {
        tx.execute(
            "UPDATE local_agent_dispatches SET state='unknown' WHERE id=?1",
            [running.dispatch.as_str()],
        )?;
        tx.execute(
            "UPDATE local_agent_sessions SET phase='unknown' WHERE target=?1 AND session=?2",
            params![agent.target, agent.session],
        )?;
        tx.commit()?;
        return Err(error.into());
    }
    tx.commit()?;
    Ok(running)
}
