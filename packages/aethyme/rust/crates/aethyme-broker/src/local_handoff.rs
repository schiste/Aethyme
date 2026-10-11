//! Explicit Local controlled generation handoff (#678).
//! Native handles cannot be deserialized. Receipts never recreate live authority.
use crate::collaboration_state::CollaborationStore;
use crate::file_lock::{ExclusiveFileLock, open_lock_file};
use crate::local_acceptance::{self as acceptance, QualifiedAcceptance};
use crate::local_generation::{self as generation, PreparedGeneration};
use crate::local_source_transport::GitCarrier;
use crate::local_target::{self as target, ControllerOwner};
use crate::{Broker, collaboration_archive as archive, collaboration_gc as gc};
use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec, Record, RecordId, RecordSchema};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const PROFILE: &str = "aethyme.handoff.native-unix-command/provisional-v0";
const fn schema(name: &'static str) -> RecordSchema {
    RecordSchema {
        name,
        fields: &[FieldSpec {
            name: "spec",
            required: true,
            kind: FieldKind::Opaque,
            capability: None,
        }],
        capabilities: &[],
    }
}
static REQUEST: RecordSchema = schema("aethyme.handoff-request/provisional-v0");
static ADOPTION: RecordSchema = schema("aethyme.adoption-receipt/provisional-v0");
#[cfg(unix)]
static DISPATCH: RecordSchema = schema("aethyme.agent-dispatch/provisional-v0");
#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error("invalid handoff: {0}")]
    Invalid(String),
    #[error("agent or generation is stale")]
    Stale,
    #[error("agent command has not settled; retain its old attachment explicitly")]
    Busy,
    #[error("dispatch liveness is unknown; inspection cannot authorize a relaunch")]
    Unknown,
    #[error("native acknowledgment deadline expired")]
    Deadline,
    #[error("handoff is not available")]
    Missing,
    #[error("this controlled executor profile requires Unix")]
    Unsupported,
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Acceptance(#[from] acceptance::AcceptanceError),
    #[error(transparent)]
    Generation(#[from] generation::GenerationError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Gc(#[from] gc::GcError),
    #[error(transparent)]
    Contract(#[from] aethyme_contracts::experimental_v0::RecordError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
fn invalid(s: &str) -> HandoffError {
    HandoffError::Invalid(s.into())
}
fn now() -> Result<i64, HandoffError> {
    let duration = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| invalid("clock"))?;
    i64::try_from(duration.as_millis()).map_err(|_| invalid("clock overflow"))
}
fn token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
}
fn record<T: Serialize>(schema: &'static RecordSchema, spec: &T) -> Result<Record, HandoffError> {
    Record::decode(
        &serde_json::to_vec(&serde_json::json!({"schema":schema.name,"spec":spec}))?,
        &[schema],
    )
    .map_err(HandoffError::from)
}
fn id(s: &str) -> Result<RecordId, HandoffError> {
    RecordId::parse(s).map_err(|_| invalid("record identity"))
}
fn decode<T: for<'de> Deserialize<'de>>(
    schema: &'static RecordSchema,
    expected: &str,
    bytes: &[u8],
) -> Result<T, HandoffError> {
    let r = Record::decode(bytes, &[schema])?;
    if r.id().as_str() != expected || r.canonical_bytes() != bytes {
        return Err(invalid("journal identity"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)?;
    Ok(serde_json::from_value(value["spec"].clone())?)
}
fn pin(
    conn: &rusqlite::Connection,
    digest: &archive::ObjectDigest,
    holder: &str,
) -> Result<(), HandoffError> {
    conn.execute("INSERT INTO object_pins(class,object_sha256,holder,created_ms) VALUES('cited_evidence',?1,?2,?3)",params![digest.hex(),holder,now()?])?;
    Ok(())
}
/// Process-local appointment. The owner and OS lock, not a stored nonce, issue it.
pub struct Agent {
    project: PathBuf,
    target: String,
    session: String,
    nonce: String,
    epoch: crate::local_authority::Epoch,
    _lock: ExclusiveFileLock,
}
impl Agent {
    pub fn session(&self) -> &str {
        &self.session
    }
}
fn session_lock(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    session: &str,
) -> Result<ExclusiveFileLock, HandoffError> {
    let root = store.project_dir().join("agent-locks");
    std::fs::create_dir_all(&root)?;
    crate::host_state::protect_host_state_path(&root, true)?;
    let key = archive::ObjectDigest::of(format!("{}\n{session}", owner.target()).as_bytes()).hex();
    ExclusiveFileLock::try_acquire(open_lock_file(&root.join(format!("{key}.lock")))?)?
        .ok_or(HandoffError::Busy)
}

/// Reappointment fences earlier handles. Unknown native dispatch prevents it;
/// a stored adoption receipt does not recreate a live executor.
pub fn appoint(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    session: &str,
) -> Result<Agent, HandoffError> {
    let _guard = target::authorize(store, owner)?;
    if !token(session) {
        return Err(invalid("agent session"));
    }
    let lock = session_lock(store, owner, session)?;
    let nonce = crate::collaboration_capture::OperationId::mint()
        .map_err(|e| invalid(&e.to_string()))?
        .as_str()
        .to_owned();
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let phase: Option<String> = tx
        .query_row(
            "SELECT phase FROM local_agent_sessions WHERE target=?1 AND session=?2",
            params![owner.target(), session],
            |r| r.get(0),
        )
        .optional()?;
    if phase.as_deref().is_some_and(|p| p != "paused") {
        return Err(HandoffError::Unknown);
    }
    let count: i64 = tx.query_row("SELECT count(*) FROM local_agent_sessions", [], |r| {
        r.get(0)
    })?;
    if phase.is_none() && count >= 1024 {
        return Err(invalid("agent capacity"));
    }
    tx.execute("INSERT INTO local_agent_sessions(target,session,nonce,epoch,phase) VALUES(?1,?2,?3,?4,'paused') ON CONFLICT(target,session) DO UPDATE SET nonce=excluded.nonce,epoch=excluded.epoch,current_handoff=NULL,phase='paused'",params![owner.target(),session,nonce,owner.authority_epoch().as_str()])?;
    tx.commit()?;
    Ok(Agent {
        project: store.project_dir().to_owned(),
        target: owner.target().into(),
        session: session.into(),
        nonce,
        epoch: owner.authority_epoch().clone(),
        _lock: lock,
    })
}
fn agent_current(
    conn: &rusqlite::Connection,
    owner: &ControllerOwner,
    agent: &Agent,
) -> Result<String, HandoffError> {
    if agent.target != owner.target() || agent.epoch != *owner.authority_epoch() {
        return Err(HandoffError::Stale);
    }
    let row: Option<(String, String, String)> = conn
        .query_row(
            "SELECT nonce,epoch,phase FROM local_agent_sessions WHERE target=?1 AND session=?2",
            params![agent.target, agent.session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((nonce, epoch, phase)) = row else {
        return Err(HandoffError::Stale);
    };
    if nonce != agent.nonce || epoch != agent.epoch.as_str() {
        return Err(HandoffError::Stale);
    };
    Ok(phase)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    pub profile: String,
    pub operation: String,
    pub target: String,
    pub revision: String,
    pub epoch: String,
    pub session: String,
    pub agent_nonce: String,
    pub generation: String,
    pub prepared_receipt: String,
    pub source: String,
    pub absolute_root: PathBuf,
    pub acceptance_binding: String,
    pub expires_ms: i64,
}
/// Historical offer returned by inspection, never accepted by the dispatch API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffView {
    pub id: RecordId,
    pub request: RequestSpec,
    pub state: String,
    pub receipt: Option<RecordId>,
}
/// Non-deserializable request issued under actual current authority.
pub struct PreparedHandoff {
    id: RecordId,
    spec: RequestSpec,
}
impl PreparedHandoff {
    pub fn id(&self) -> &RecordId {
        &self.id
    }
}
fn mapping(
    prepared: &PreparedGeneration,
    owner: &ControllerOwner,
    grant: &QualifiedAcceptance<'_>,
) -> Result<(), HandoffError> {
    let intent = prepared.intent();
    let receipt = &grant.receipt().spec;
    if intent.target != owner.target()
        || intent.epoch != owner.authority_epoch().as_str()
        || intent.revision != receipt.revision
        || intent.source != receipt.source
        || intent.acceptance_binding != receipt.binding
        || Some(&intent.accepted_commit) != receipt.accepted_commit.as_ref()
        || Some(&intent.accepted_tree) != receipt.accepted_tree.as_ref()
    {
        return Err(HandoffError::Stale);
    };
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub fn prepare(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    agent: &Agent,
    broker: &mut Broker,
    carrier: &GitCarrier,
    grant: &QualifiedAcceptance<'_>,
    generation: &RecordId,
    operation: &str,
    expires_ms: i64,
) -> Result<PreparedHandoff, HandoffError> {
    let _archive = gc::archive_use(store)?;
    if agent.project != store.project_dir() || !token(operation) {
        return Err(invalid("request binding"));
    }
    let phase = agent_current(store.read_connection(), owner, agent)?;
    if phase != "paused" {
        return Err(HandoffError::Busy);
    };
    let prepared = generation::load_prepared(store, generation)?;
    mapping(&prepared, owner, grant)?;
    let t = now()?;
    if expires_ms <= t || expires_ms > grant.expires_ms() || expires_ms > t + 300_000 {
        return Err(invalid("handoff expiry"));
    }
    let spec = RequestSpec {
        profile: PROFILE.into(),
        operation: operation.into(),
        target: owner.target().into(),
        revision: prepared.intent().revision.clone(),
        epoch: agent.epoch.as_str().into(),
        session: agent.session.clone(),
        agent_nonce: agent.nonce.clone(),
        generation: generation.to_string(),
        prepared_receipt: prepared.receipt().to_string(),
        source: prepared.intent().source.clone(),
        absolute_root: prepared.absolute_root().to_owned(),
        acceptance_binding: prepared.intent().acceptance_binding.clone(),
        expires_ms,
    };
    let record = record(&REQUEST, &spec)?;
    let digest = archive::put_object(store, &record.canonical_bytes())?;
    let guard = acceptance::current_guard(store, owner, broker, carrier, grant)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if agent_current(&tx, owner, agent)? != "paused" {
        return Err(HandoffError::Busy);
    };
    let existing: Option<(String, String)> = tx
        .query_row(
            "SELECT id,state FROM local_handoffs WHERE target=?1 AND session=?2 AND operation=?3",
            params![agent.target, agent.session, operation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if let Some((existing, state)) = existing {
        if existing != record.id().as_str() {
            return Err(invalid("operation binding"));
        }
        if state != "prepared" {
            return Err(HandoffError::Unknown);
        };
    } else {
        let count: i64 = tx.query_row("SELECT count(*) FROM local_handoffs", [], |r| r.get(0))?;
        if count >= 4096 {
            return Err(invalid("handoff capacity"));
        }
        tx.execute("INSERT INTO local_handoffs(id,target,session,operation,generation,body,state) VALUES(?1,?2,?3,?4,?5,?6,'prepared')",params![record.id().as_str(),agent.target,agent.session,operation,spec.generation,record.canonical_bytes()])?;
        pin(&tx, &digest, record.id().as_str())?;
    }
    guard.check_clock()?;
    if now()? >= expires_ms {
        return Err(HandoffError::Deadline);
    };
    tx.commit()?;
    Ok(PreparedHandoff {
        id: record.id(),
        spec,
    })
}
pub fn show(store: &CollaborationStore, handoff: &RecordId) -> Result<HandoffView, HandoffError> {
    let _archive = gc::archive_use(store)?;
    let row: Option<(Vec<u8>, String, Option<String>)> = store
        .read_connection()
        .query_row(
            "SELECT body,state,receipt FROM local_handoffs WHERE id=?1",
            [handoff.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((body, state, receipt)) = row else {
        return Err(HandoffError::Missing);
    };
    let request = decode(&REQUEST, handoff.as_str(), &body)?;
    if archive::read_object(store, &archive::ObjectDigest::of(&body))? != body {
        return Err(invalid("retained request"));
    }
    Ok(HandoffView {
        id: handoff.clone(),
        request,
        state,
        receipt: receipt.map(|s| id(&s)).transpose()?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandSpec {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub acknowledgment_ms: u32,
}
#[cfg(any(unix, test))]
impl CommandSpec {
    fn validate(&self) -> Result<(), HandoffError> {
        if !self.executable.is_absolute()
            || self.executable.canonicalize()? != self.executable
            || !self.executable.is_file()
            || self.arguments.len() > 64
            || self
                .arguments
                .iter()
                .any(|s| s.len() > 4096 || s.contains('\0'))
            || !(1..=30_000).contains(&self.acknowledgment_ms)
        {
            return Err(invalid("native command limits"));
        }
        Ok(())
    }
}
#[cfg(unix)]
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DispatchSpec {
    profile: String,
    handoff: String,
    command: CommandSpec,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdoptionSpec {
    pub profile: String,
    pub handoff: String,
    pub dispatch: String,
    pub generation: String,
    pub source: String,
    pub session: String,
    pub agent_nonce: String,
    pub epoch: String,
    pub absolute_root: PathBuf,
    pub native_pid: u32,
}
/// Native Child ownership, not its stored PID, accounts for this direct process.
/// Detached descendants remain possible and old roots are never retired here.
pub struct RunningCommand {
    child: std::process::Child,
    dispatch: RecordId,
    handoff: RecordId,
    generation: RecordId,
    session: String,
    nonce: String,
    epoch: crate::local_authority::Epoch,
    project: PathBuf,
    root: PathBuf,
    settled: bool,
}
impl RunningCommand {
    pub fn absolute_root(&self) -> &Path {
        &self.root
    }
    pub fn dispatch(&self) -> &RecordId {
        &self.dispatch
    }
}
impl Drop for RunningCommand {
    fn drop(&mut self) {
        if !self.settled {
            if let Err(e) = self.child.kill() {
                crate::warn_unrecorded::<(), _>("handoff owned child kill", Err(e));
            }
            if let Err(e) = self.child.wait() {
                crate::warn_unrecorded::<(), _>("handoff owned child reap", Err(e));
            }
        }
    }
}
#[cfg(unix)]
mod native;
#[cfg(unix)]
pub use native::dispatch;
#[cfg(not(unix))]
#[allow(clippy::too_many_arguments)]
pub fn dispatch(
    _store: &mut CollaborationStore,
    _owner: &ControllerOwner,
    _agent: &Agent,
    _broker: &mut Broker,
    _carrier: &GitCarrier,
    _grant: &QualifiedAcceptance<'_>,
    _request: &PreparedHandoff,
    _command: CommandSpec,
) -> Result<RunningCommand, HandoffError> {
    Err(HandoffError::Unsupported)
}

/// Explicitly account for a still-owned command on its old generation, allowing
/// a later controlled command at a different root. This does not stop children.
pub fn retain_old_attachment(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    agent: &Agent,
    command: &RunningCommand,
) -> Result<(), HandoffError> {
    let _guard = target::authorize(store, owner)?;
    if command.project != store.project_dir()
        || command.session != agent.session
        || command.nonce != agent.nonce
        || command.epoch != agent.epoch
    {
        return Err(HandoffError::Stale);
    };
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    if agent_current(&tx, owner, agent)? != "running" {
        return Err(HandoffError::Busy);
    };
    let active: Option<String> = tx.query_row(
        "SELECT current_handoff FROM local_agent_sessions WHERE target=?1 AND session=?2",
        params![agent.target, agent.session],
        |r| r.get(0),
    )?;
    if active.as_deref() != Some(command.handoff.as_str()) {
        return Err(HandoffError::Stale);
    };
    let changed=tx.execute("UPDATE local_agent_dispatches SET state='attached_old' WHERE id=?1 AND state IN ('acknowledged','leader_completed','leader_completed_old')",[command.dispatch.as_str()])?;
    if changed != 1 {
        return Err(HandoffError::Unknown);
    };
    tx.execute(
        "UPDATE local_agent_sessions SET phase='paused' WHERE target=?1 AND session=?2",
        params![agent.target, agent.session],
    )?;
    tx.commit()?;
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Running,
    Current(i32),
    Historical(i32),
}
/// Observe direct leader completion; descendant liveness remains unknown. Historical completion cannot change the new
/// session's dispatch state or publish a current agent result.
pub fn poll_command(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    agent: &Agent,
    command: &mut RunningCommand,
) -> Result<Completion, HandoffError> {
    poll_command_with(
        store,
        owner,
        agent,
        command,
        &mut |_| Ok(()),
        &mut || Ok(()),
    )
}
fn poll_command_with(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    agent: &Agent,
    command: &mut RunningCommand,
    before: &mut dyn FnMut(&mut CollaborationStore) -> Result<(), HandoffError>,
    held: &mut dyn FnMut() -> Result<(), HandoffError>,
) -> Result<Completion, HandoffError> {
    let _guard = target::authorize(store, owner)?;
    if command.project != store.project_dir()
        || command.session != agent.session
        || command.nonce != agent.nonce
        || command.epoch != agent.epoch
    {
        return Err(HandoffError::Stale);
    }
    let Some(status) = command.child.try_wait()? else {
        return Ok(Completion::Running);
    };
    let code = status.code().unwrap_or(-1);
    command.settled = true;
    before(store)?;
    match current_agent(store, owner, agent, command) {
        Ok(guard) => {
            // Reuse the actual writer reservation, without a stale-currentness
            // gap. Check the clock after all fallible work at the commit point.
            held()?;
            let result = account_leader(
                &guard._transaction,
                owner,
                agent,
                command,
                code,
                guard.check_clock().is_ok(),
            )?;
            // If expiry crosses the accounting statements, rewrite as historical
            // in the same uncommitted transaction; leader accounting still pauses.
            let result = if result == Completion::Current(code) && guard.check_clock().is_err() {
                account_leader(&guard._transaction, owner, agent, command, code, false)?
            } else {
                result
            };
            guard._transaction.commit()?;
            Ok(result)
        }
        Err(_) => {
            // No current result authority is inferred from this fallback. It
            // accounts only the exact owned leader and retains descendant roots.
            let tx = store
                .connection()
                .transaction_with_behavior(TransactionBehavior::Immediate)?;
            let result = account_leader(&tx, owner, agent, command, code, false)?;
            tx.commit()?;
            Ok(result)
        }
    }
}
fn account_leader(
    tx: &rusqlite::Connection,
    owner: &ControllerOwner,
    agent: &Agent,
    command: &RunningCommand,
    code: i32,
    fresh: bool,
) -> Result<Completion, HandoffError> {
    agent_current(tx, owner, agent)?;
    let active: Option<String> = tx.query_row(
        "SELECT current_handoff FROM local_agent_sessions WHERE target=?1 AND session=?2",
        params![agent.target, agent.session],
        |r| r.get(0),
    )?;
    let state: String = tx.query_row(
        "SELECT state FROM local_agent_dispatches WHERE id=?1",
        [command.dispatch.as_str()],
        |r| r.get(0),
    )?;
    let current = fresh
        && active.as_deref() == Some(command.handoff.as_str())
        && matches!(state.as_str(), "acknowledged" | "leader_completed");
    let active_leader = active.as_deref() == Some(command.handoff.as_str())
        && matches!(
            state.as_str(),
            "acknowledged" | "leader_completed" | "leader_completed_old"
        );
    let completed_state =
        if !fresh || matches!(state.as_str(), "attached_old" | "leader_completed_old") {
            "leader_completed_old"
        } else {
            "leader_completed"
        };
    tx.execute(
        "UPDATE local_agent_dispatches SET state=?3,completion=?2 WHERE id=?1",
        params![command.dispatch.as_str(), code, completed_state],
    )?;
    if active_leader {
        tx.execute(
            "UPDATE local_agent_sessions SET phase='paused' WHERE target=?1 AND session=?2",
            params![agent.target, agent.session],
        )?;
    }
    Ok(if current {
        Completion::Current(code)
    } else {
        Completion::Historical(code)
    })
}
/// Hold this native epoch and database writer reservation through consumption
/// of current agent data. Handoff and target mutations cannot cross this guard.
/// The target owner also remains borrowed. It carries no verification, source
/// acceptance, publication, or ordinary broker submit permission.
pub struct CurrentAgentGuard<'a> {
    _epoch: crate::local_authority::Guard,
    _transaction: rusqlite::Transaction<'a>,
    _owner: &'a ControllerOwner,
    _agent: &'a Agent,
    expires_ms: i64,
}
impl CurrentAgentGuard<'_> {
    pub fn check_clock(&self) -> Result<(), HandoffError> {
        if now()? >= self.expires_ms {
            return Err(HandoffError::Stale);
        };
        Ok(())
    }
}
pub fn current_agent<'a>(
    store: &'a mut CollaborationStore,
    owner: &'a ControllerOwner,
    agent: &'a Agent,
    command: &RunningCommand,
) -> Result<CurrentAgentGuard<'a>, HandoffError> {
    let guard = target::authorize(store, owner)?;
    if command.project != store.project_dir()
        || agent.project != store.project_dir()
        || command.session != agent.session
        || command.nonce != agent.nonce
        || command.epoch != agent.epoch
    {
        return Err(HandoffError::Stale);
    };
    if agent_current(store.read_connection(), owner, agent)? == "unknown" {
        return Err(HandoffError::Unknown);
    };
    let active: Option<String> = store.read_connection().query_row(
        "SELECT current_handoff FROM local_agent_sessions WHERE target=?1 AND session=?2",
        params![agent.target, agent.session],
        |r| r.get(0),
    )?;
    let state: String = store.read_connection().query_row(
        "SELECT state FROM local_agent_dispatches WHERE id=?1",
        [command.dispatch.as_str()],
        |r| r.get(0),
    )?;
    let request = show(store, &command.handoff)?;
    let receipt = request.receipt.as_ref().ok_or(HandoffError::Unknown)?;
    let body: Vec<u8> = store.read_connection().query_row(
        "SELECT body FROM local_adoption_receipts WHERE id=?1 AND handoff=?2",
        params![receipt.as_str(), command.handoff.as_str()],
        |r| r.get(0),
    )?;
    let adopted: AdoptionSpec = decode(&ADOPTION, receipt.as_str(), &body)?;
    if archive::read_object(store, &archive::ObjectDigest::of(&body))? != body
        || adopted.profile != PROFILE
        || adopted.handoff != command.handoff.as_str()
        || adopted.dispatch != command.dispatch.as_str()
        || adopted.generation != command.generation.as_str()
        || adopted.source != request.request.source
        || adopted.session != agent.session
        || adopted.agent_nonce != agent.nonce
        || adopted.epoch != agent.epoch.as_str()
        || adopted.absolute_root != command.root
        || adopted.absolute_root != request.request.absolute_root
        || adopted.native_pid != command.child.id()
    {
        return Err(HandoffError::Stale);
    };
    let status = target::status(store, &agent.target)?;
    if active.as_deref() != Some(command.handoff.as_str())
        || !matches!(state.as_str(), "acknowledged" | "leader_completed")
        || request.state != "adopted"
        || request.request.agent_nonce != agent.nonce
        || request.request.session != agent.session
        || request.request.epoch != agent.epoch.as_str()
        || request.request.generation != command.generation.as_str()
        || status.revision.as_ref().map(RecordId::as_str) != Some(request.request.revision.as_str())
        || status.phase != target::Phase::Verifying
        || now()? >= request.request.expires_ms
    {
        return Err(HandoffError::Stale);
    };
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    check_target(&tx, &request.request, status.version)?;
    if agent_current(&tx, owner, agent)? == "unknown" {
        return Err(HandoffError::Unknown);
    }
    let current: Option<String> = tx.query_row(
        "SELECT current_handoff FROM local_agent_sessions WHERE target=?1 AND session=?2",
        params![agent.target, agent.session],
        |r| r.get(0),
    )?;
    let dispatch_state: String = tx.query_row(
        "SELECT state FROM local_agent_dispatches WHERE id=?1",
        [command.dispatch.as_str()],
        |r| r.get(0),
    )?;
    if current.as_deref() != Some(command.handoff.as_str())
        || !matches!(dispatch_state.as_str(), "acknowledged" | "leader_completed")
    {
        return Err(HandoffError::Stale);
    }
    Ok(CurrentAgentGuard {
        _transaction: tx,
        _epoch: guard,
        _owner: owner,
        _agent: agent,
        expires_ms: request.request.expires_ms,
    })
}

// Exact target admission while holding the database writer reservation. The
// version rejects stop/resume ABA even if the revision and phase return equal.
fn check_target(
    conn: &rusqlite::Connection,
    request: &RequestSpec,
    version: i64,
) -> Result<(), HandoffError> {
    let (revision, phase, observed): (Option<String>, String, i64) = conn.query_row(
        "SELECT active_revision,phase,version FROM local_targets WHERE target=?1",
        [request.target.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if revision.as_deref() != Some(request.revision.as_str())
        || phase != "verifying"
        || observed != version
        || now()? >= request.expires_ms
    {
        return Err(HandoffError::Stale);
    }
    Ok(())
}

static RECONCILIATION: RecordSchema = schema("aethyme.handoff-retained-unknown/provisional-v0");
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetainedUnknown {
    profile: String,
    handoff: String,
    session: String,
    target: String,
    generation: String,
    source: String,
    absolute_root: PathBuf,
    old_epoch: String,
    operator_epoch: String,
    reason: String,
}
/// Explicit operator reconciliation: retain the unresolved dispatch as an old
/// attachment with unknown liveness. A live appointed agent's OS lock refuses
/// this action. It never certifies process death, retries the original operation,
/// revives an imported handle, retires a root, or authorizes new execution.
/// Reappointment and a new request still require fresh native authority.
pub fn retain_unknown_dispatch(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    session: &str,
    expected: &RecordId,
    reason: &str,
) -> Result<RecordId, HandoffError> {
    let _epoch = target::authorize(store, owner)?;
    let _archive = gc::archive_use(store)?;
    if !token(session) || reason.trim().is_empty() || reason.len() > 4096 {
        return Err(invalid("reconciliation binding"));
    };
    let _lock = session_lock(store, owner, session)?;
    let view = show(store, expected)?;
    if view.request.target != owner.target()
        || view.request.session != session
        || !matches!(view.state.as_str(), "dispatching" | "adopted")
    {
        return Err(HandoffError::Stale);
    };
    let decision = RetainedUnknown {
        profile: PROFILE.into(),
        handoff: expected.to_string(),
        session: session.into(),
        target: owner.target().into(),
        generation: view.request.generation.clone(),
        source: view.request.source.clone(),
        absolute_root: view.request.absolute_root.clone(),
        old_epoch: view.request.epoch.clone(),
        operator_epoch: owner.authority_epoch().as_str().into(),
        reason: reason.into(),
    };
    let record = record(&RECONCILIATION, &decision)?;
    let digest = archive::put_object(store, &record.canonical_bytes())?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (nonce,epoch,phase,current):(String,String,String,Option<String>)=tx.query_row("SELECT nonce,epoch,phase,current_handoff FROM local_agent_sessions WHERE target=?1 AND session=?2",params![owner.target(),session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    if nonce != view.request.agent_nonce
        || epoch != view.request.epoch
        || !matches!(phase.as_str(), "running" | "unknown")
        || (phase == "running" && current.as_deref() != Some(expected.as_str()))
    {
        return Err(HandoffError::Stale);
    };
    let count:i64=tx.query_row("SELECT count(*) FROM local_agent_dispatches WHERE handoff=?1 AND state IN ('intent','acknowledged','unknown','leader_completed','leader_completed_old')",[expected.as_str()],|r|r.get(0))?;
    if count != 1 {
        return Err(HandoffError::Unknown);
    };
    let total: i64 = tx.query_row(
        "SELECT count(*) FROM local_agent_reconciliations",
        [],
        |r| r.get(0),
    )?;
    if total >= 4096 {
        return Err(invalid("reconciliation capacity"));
    };
    tx.execute(
        "INSERT INTO local_agent_reconciliations(id,handoff,body) VALUES(?1,?2,?3)",
        params![
            record.id().as_str(),
            expected.as_str(),
            record.canonical_bytes()
        ],
    )?;
    pin(&tx, &digest, record.id().as_str())?;
    tx.execute(
        "UPDATE local_agent_dispatches SET state='unknown' WHERE handoff=?1",
        [expected.as_str()],
    )?;
    tx.execute("UPDATE local_agent_sessions SET phase='paused',current_handoff=NULL WHERE target=?1 AND session=?2",params![owner.target(),session])?;
    tx.commit()?;
    Ok(record.id())
}

#[cfg(test)]
mod tests;
