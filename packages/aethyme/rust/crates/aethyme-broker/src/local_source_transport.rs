//! Local exact Git source transport (#674). Publication is availability, never
//! provider acceptance. Uses the existing broker write journal and Git service.
//! One intent dispatches at most once, including after a lost response. Observing
//! an absent or changed ref does not establish that a previous write had no effect.

use std::time::Duration;

use aethyme_contracts::experimental_v0::{
    FieldKind, FieldSpec, Record, RecordId, RecordSchema, SourceSnapshotId,
};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::collaboration_archive::{self as archive, CommitOid, ObjectDigest};
use crate::collaboration_state::CollaborationStore;
use crate::local_target::{self as target, ControllerOwner};
use crate::local_wave::{self as wave, WavePhase};
use crate::{Broker, CoordinatedCommand, GitRepo, OperationProvider, OperationStatus, QueueWait};

pub const PROFILE: &str = "aethyme.source-transport.git/provisional-v0";
const INTENT_SCHEMA: RecordSchema = RecordSchema {
    name: "aethyme.source-transport-intent/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
const RECEIPT_SCHEMA: RecordSchema = RecordSchema {
    name: "aethyme.source-transport-receipt/provisional-v0",
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("invalid or mismatched source transport: {0}")]
    Invalid(String),
    #[error("source transport is missing")]
    Missing,
    #[error("this intent has already dispatched; inspect it, never blindly repeat it")]
    AlreadyDispatched,
    #[error("an uncertain write on this route must be inspected first")]
    Unresolved,
    #[error(transparent)]
    Target(#[from] target::TargetError),
    #[error(transparent)]
    Wave(#[from] wave::WaveError),
    #[error(transparent)]
    Archive(#[from] archive::ArchiveError),
    #[error(transparent)]
    Git(#[from] crate::GitError),
    #[error(transparent)]
    Remote(#[from] crate::RemoteTargetError),
    #[error(transparent)]
    Broker(#[from] crate::BrokerOpError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Constructed by the native operator, not decoded from incoming records.
/// Ordinary publication policy and cross-session ref guards remain in force.
pub struct GitCarrier {
    session: i64,
    repo: GitRepo,
    remote: String,
    destination: String,
    identity: crate::ResolvedRemoteTarget,
    repository_binding: String,
    timeout: u64,
    reason: String,
    destructive_authorized: bool,
}
impl GitCarrier {
    pub fn new(
        broker: &mut Broker,
        session: i64,
        remote: &str,
        destination: &str,
        timeout_seconds: u64,
        reason: &str,
        destructive_authorized: bool,
    ) -> Result<Self, TransportError> {
        if !(1..=300).contains(&timeout_seconds)
            || reason.trim().is_empty()
            || reason.len() > 4096
            || reason.chars().any(char::is_control)
            || !destination.starts_with("refs/heads/")
        {
            return Err(TransportError::Invalid(
                "finite timeout, authorization and full branch ref required".into(),
            ));
        }
        let session_row = broker
            .store()
            .session(session)
            .map_err(crate::BrokerOpError::from)?;
        if session_row.status.is_closed() {
            return Err(TransportError::Invalid("closed session".into()));
        }
        let repo = GitRepo::discover(std::path::Path::new(&session_row.worktree_path))?;
        if repo.git_common_dir()? != broker.repo_handle().git_common_dir()? {
            return Err(TransportError::Invalid(
                "session belongs to another repository".into(),
            ));
        }
        repo.validate_push_destination(destination)?;
        let identity = crate::git::with_git_deadline(Duration::from_secs(timeout_seconds), || {
            repo.resolve_remote_target(remote, None)
        })?;
        if identity.fetch_url != identity.push_url {
            return Err(TransportError::Invalid(
                "this profile requires one identical fetch and push endpoint".into(),
            ));
        }
        let repository_binding =
            ObjectDigest::of(repo.git_common_dir()?.as_os_str().as_encoded_bytes()).hex();
        Ok(Self {
            session,
            repo,
            remote: remote.into(),
            destination: destination.into(),
            identity,
            repository_binding,
            timeout: timeout_seconds,
            reason: reason.into(),
            destructive_authorized,
        })
    }
    fn check_broker(&self, broker: &mut Broker) -> Result<(), TransportError> {
        let s = broker
            .store()
            .session(self.session)
            .map_err(crate::BrokerOpError::from)?;
        if s.status.is_closed()
            || GitRepo::discover(std::path::Path::new(&s.worktree_path))?.git_common_dir()?
                != self.repo.git_common_dir()?
            || broker.repo_handle().git_common_dir()? != self.repo.git_common_dir()?
        {
            return Err(TransportError::Invalid(
                "closed or foreign broker session".into(),
            ));
        }
        Ok(())
    }
    fn check(&self) -> Result<(), TransportError> {
        let current = crate::git::with_git_deadline(Duration::from_secs(self.timeout), || {
            self.repo.resolve_remote_target(&self.remote, None)
        })?;
        if current != self.identity {
            return Err(TransportError::Invalid(
                "remote configuration changed".into(),
            ));
        }
        Ok(())
    }
    fn command(&self, intent: &Intent, args: Vec<String>) -> CoordinatedCommand {
        CoordinatedCommand {
            session_id: self.session,
            provider: OperationProvider::Git,
            repository: None,
            resolved_target: Some(self.identity.clone()),
            scope: Some(scope(&intent.id)),
            declared_effect: None,
            destructive_confirmed: self.destructive_authorized,
            cross_session: None,
            ref_write_acknowledged: false,
            authorization_reason: Some(self.reason.clone()),
            args,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntentSpec {
    pub profile: String,
    pub nonce: String,
    pub project: String,
    pub target: String,
    pub issuer: String,
    pub epoch: String,
    pub revision: String,
    pub wave: String,
    pub source: String,
    pub commit: String,
    pub tree: String,
    pub peer: String,
    pub remote_binding: String,
    pub repository_binding: String,
    pub destination: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_old: Option<String>,
    pub session: i64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub id: RecordId,
    pub spec: IntentSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Observation {
    Available,
    Absent,
    Other,
    Unavailable,
}
impl Observation {
    fn code(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Absent => "absent",
            Self::Other => "other",
            Self::Unavailable => "unavailable",
        }
    }
}
/// Historical observation, with no API that can authorize acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReceiptSpec {
    pub profile: String,
    pub intent: String,
    pub source: String,
    pub commit: String,
    pub tree: String,
    pub peer: String,
    pub destination: String,
    pub observation: Observation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub observed_oid: Option<String>,
    pub content_checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_operation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub write_outcome: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_operation: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_outcome: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub id: RecordId,
    pub spec: ReceiptSpec,
}

fn canonical<T: Serialize>(
    schema: &'static RecordSchema,
    value: &T,
) -> Result<Record, TransportError> {
    Record::decode(
        &serde_json::to_vec(&serde_json::json!({"schema":schema.name,"spec":value}))?,
        &[schema],
    )
    .map_err(|e| TransportError::Invalid(e.to_string()))
}
fn parse_id(id: &str) -> Result<RecordId, TransportError> {
    RecordId::parse(id).map_err(|e| TransportError::Invalid(e.to_string()))
}
fn scope(id: &RecordId) -> String {
    format!("local-source-transport:{}", id.as_str())
}
fn remote_binding(carrier: &GitCarrier) -> Result<String, TransportError> {
    Ok(ObjectDigest::of(&serde_json::to_vec(&carrier.identity)?).hex())
}
fn current(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    intent: &Intent,
) -> Result<(), TransportError> {
    let s = &intent.spec;
    if store.project().as_str() != s.project
        || owner.target() != s.target
        || owner.issuer() != s.issuer
        || owner.authority_epoch().as_str() != s.epoch
    {
        return Err(TransportError::Invalid("project, owner or epoch".into()));
    }
    let state = target::status(store, owner.target())?;
    if state.revision.as_ref().map(RecordId::as_str) != Some(s.revision.as_str())
        || !matches!(
            state.phase,
            target::Phase::Verifying
                | target::Phase::ReadyForAcceptance
                | target::Phase::AwaitingProviderOrReview
        )
    {
        return Err(TransportError::Invalid(
            "stopped or superseded target".into(),
        ));
    }
    let w = wave::load(store, &parse_id(&s.wave)?)?;
    if w.phase != WavePhase::Verifying
        || w.spec.revision != s.revision
        || w.candidate_snapshot.as_ref().map(SourceSnapshotId::as_str) != Some(s.source.as_str())
        || w.candidate_commit.as_ref().map(CommitOid::as_str) != Some(s.commit.as_str())
    {
        return Err(TransportError::Invalid("candidate wave changed".into()));
    }
    Ok(())
}
fn route(carrier: &GitCarrier, intent: &Intent) -> Result<(), TransportError> {
    let s = &intent.spec;
    if s.profile != PROFILE
        || s.peer != carrier.identity.coordination_key
        || s.remote_binding != remote_binding(carrier)?
        || s.repository_binding != carrier.repository_binding
        || s.destination != carrier.destination
        || s.session != carrier.session
    {
        return Err(TransportError::Invalid(
            "exact carrier route differs".into(),
        ));
    }
    carrier.check()
}
fn source(
    store: &CollaborationStore,
    carrier: &GitCarrier,
    intent: &Intent,
) -> Result<(), TransportError> {
    let oid = CommitOid::parse(&intent.spec.commit)?;
    let actual = archive::snapshot_of_commit(carrier.repo.root(), &oid)?;
    if actual.id().as_str() != intent.spec.source
        || carrier.repo.commit_tree_id(oid.as_str())? != intent.spec.tree
    {
        return Err(TransportError::Invalid(
            "Git objects differ from retained source".into(),
        ));
    }
    let snapshot = SourceSnapshotId::parse(&intent.spec.source)
        .map_err(|e| TransportError::Invalid(e.to_string()))?;
    crate::composer::snapshot_entries(store, &snapshot).map_err(target::TargetError::from)?;
    Ok(())
}

/// Persist the exact subject and retained roots before any external write. A
/// caller nonce makes identical preparation idempotent; changing the subject
/// changes the ID. Missing Git history is reported, never invented from a ref.
pub fn prepare(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    carrier: &GitCarrier,
    wave_id: &RecordId,
    nonce: &str,
    expected_old: Option<&CommitOid>,
) -> Result<Intent, TransportError> {
    let _authority = target::authorize(store, owner)?;
    if nonce.len() != 64
        || !nonce
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(TransportError::Invalid(
            "nonce requires 64 lowercase hex digits".into(),
        ));
    }
    carrier.check()?;
    let w = wave::load(store, wave_id)?;
    let commit = w.candidate_commit.as_ref().ok_or(TransportError::Missing)?;
    if commit.as_str().len() != 40 || expected_old.is_some_and(|oid| oid.as_str().len() != 40) {
        return Err(TransportError::Invalid(
            "this concrete Git profile requires SHA-1 objects".into(),
        ));
    }
    let s = IntentSpec {
        profile: PROFILE.into(),
        nonce: nonce.into(),
        project: store.project().as_str().into(),
        target: owner.target().into(),
        issuer: owner.issuer().into(),
        epoch: owner.authority_epoch().as_str().into(),
        revision: w.spec.revision.clone(),
        wave: w.id.to_string(),
        source: w
            .candidate_snapshot
            .as_ref()
            .ok_or(TransportError::Missing)?
            .to_string(),
        commit: commit.as_str().into(),
        tree: carrier.repo.commit_tree_id(commit.as_str())?,
        peer: carrier.identity.coordination_key.clone(),
        remote_binding: remote_binding(carrier)?,
        repository_binding: carrier.repository_binding.clone(),
        destination: carrier.destination.clone(),
        expected_old: expected_old.map(|o| o.as_str().into()),
        session: carrier.session,
    };
    let record = canonical(&INTENT_SCHEMA, &s)?;
    let intent = Intent {
        id: record.id(),
        spec: s,
    };
    current(store, owner, &intent)?;
    source(store, carrier, &intent)?;
    target::retain_artifacts(
        store,
        owner,
        &parse_id(&intent.spec.revision)?,
        &[SourceSnapshotId::parse(&intent.spec.source)
            .map_err(|e| TransportError::Invalid(e.to_string()))?],
        &[],
    )?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let uncertain:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM local_source_transports WHERE peer=?1 AND destination=?2 AND dispatched=1 AND observation IN ('unobserved','unavailable') AND intent<>?3)",params![intent.spec.peer,intent.spec.destination,intent.id.as_str()],|r|r.get(0))?;
    if uncertain {
        return Err(TransportError::Unresolved);
    }
    tx.execute("INSERT OR IGNORE INTO local_source_transports (intent,revision,target,peer,destination,body,dispatched,observation,receipt) VALUES (?1,?2,?3,?4,?5,?6,0,'unobserved',NULL)",
        params![intent.id.as_str(),intent.spec.revision,intent.spec.target,intent.spec.peer,intent.spec.destination,record.canonical_bytes()])?;
    tx.commit()?;
    load(store, &intent.id)
}

pub fn load(store: &CollaborationStore, id: &RecordId) -> Result<Intent, TransportError> {
    let (bytes,revision,target,peer,destination):(Vec<u8>,String,String,String,String)=store.read_connection().query_row(
        "SELECT body,revision,target,peer,destination FROM local_source_transports WHERE intent=?1",[id.as_str()],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(TransportError::Missing)?;
    let record = Record::decode(&bytes, &[&INTENT_SCHEMA])
        .map_err(|e| TransportError::Invalid(e.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&record.canonical_bytes())?;
    let spec: IntentSpec = serde_json::from_value(value["spec"].clone())?;
    if record.id() != *id
        || bytes != record.canonical_bytes()
        || spec.revision != revision
        || spec.target != target
        || spec.peer != peer
        || spec.destination != destination
    {
        return Err(TransportError::Invalid(
            "forged transport index or record".into(),
        ));
    }
    Ok(Intent {
        id: id.clone(),
        spec,
    })
}

/// Dispatch once through the existing broker. A durable dispatched mark is
/// committed first; a crash in the inter-store gap remains uncertain. Local
/// control mutations wait for this bounded operation to finish. A nonzero Git
/// response still requires inspection, because the remote may have written.
#[derive(Debug)]
pub struct Dispatch {
    pub operation: i64,
    pub outcome: OperationStatus,
}

pub fn publish(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    carrier: &GitCarrier,
    broker: &mut Broker,
    id: &RecordId,
) -> Result<Dispatch, TransportError> {
    let _authority = target::authorize(store, owner)?;
    let intent = load(store, id)?;
    carrier.check_broker(broker)?;
    route(carrier, &intent)?;
    current(store, owner, &intent)?;
    source(store, carrier, &intent)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let uncertain:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM local_source_transports WHERE peer=?1 AND destination=?2 AND dispatched=1 AND observation IN ('unobserved','unavailable') AND intent<>?3)",params![intent.spec.peer,intent.spec.destination,id.as_str()],|r|r.get(0))?;
    if uncertain {
        return Err(TransportError::Unresolved);
    }
    if tx.execute(
        "UPDATE local_source_transports SET dispatched=1,observation='unobserved',receipt=NULL,operation=NULL,write_outcome=NULL WHERE intent=?1 AND dispatched=0",
        [id.as_str()],
    )? != 1
    {
        return Err(TransportError::AlreadyDispatched);
    }
    tx.commit()?;
    let _control = rusqlite::Transaction::new_unchecked(
        store.read_connection(),
        TransactionBehavior::Immediate,
    )?;
    current(store, owner, &intent)?;
    let args = push_args(carrier, &intent);
    let result = broker.run_coordinated_operation_at_with_hooks(
        carrier.command(&intent, args),
        carrier.repo.root(),
        QueueWait::Seconds(carrier.timeout),
        || {
            route(carrier, &intent)
                .and_then(|()| current(store, owner, &intent))
                .map_err(|e| e.to_string())
        },
        |_, _| Ok(None),
    );
    match result {
        Ok(report) => {
            store.read_connection().execute(
                "UPDATE local_source_transports SET operation=?2,write_outcome=?3 WHERE intent=?1",
                params![
                    id.as_str(),
                    report.operation.id,
                    report.operation.status.as_str()
                ],
            )?;
            _control.commit()?;
            Ok(Dispatch {
                operation: report.operation.id,
                outcome: report.operation.status,
            })
        }
        Err(e) => Err(e.into()),
    }
}

/// Inspect the actual exact push endpoint, then fetch that advertised object
/// without writing tracking refs or FETCH_HEAD. Current availability is separate
/// from the historical write outcome; absence does not prove a write never ran.
/// This does not reconcile a broker ambiguity on an operator's behalf.
pub fn inspect(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    carrier: &GitCarrier,
    broker: &mut Broker,
    id: &RecordId,
) -> Result<Receipt, TransportError> {
    let _authority = target::authorize(store, owner)?;
    let intent = load(store, id)?;
    carrier.check_broker(broker)?;
    route(carrier, &intent)?;
    // Historical observations may be collected after stop, but cannot dispatch.
    if intent.spec.project != store.project().as_str()
        || intent.spec.target != owner.target()
        || intent.spec.issuer != owner.issuer()
        || intent.spec.epoch != owner.authority_epoch().as_str()
    {
        return Err(TransportError::Invalid(
            "historical observation owner or epoch".into(),
        ));
    }
    let _control = rusqlite::Transaction::new_unchecked(
        store.read_connection(),
        TransactionBehavior::Immediate,
    )?;
    let op = find_operation(store, broker, carrier, &intent)?;
    let observed = crate::git::with_git_deadline(Duration::from_secs(carrier.timeout), || {
        carrier
            .repo
            .remote_ref_oids(&carrier.remote, &[carrier.destination.clone()])
    });
    let (observation, oid) = match observed {
        Ok(map) => match map.get(&carrier.destination).cloned().flatten() {
            Some(oid) if oid == intent.spec.commit => (Observation::Available, Some(oid)),
            Some(oid) => (Observation::Other, Some(oid)),
            None => (Observation::Absent, None),
        },
        Err(_) => (Observation::Unavailable, None),
    };
    let mut read_operation = None;
    let mut content_checked = false;
    let mut read_outcome = None;
    if observation == Observation::Available {
        let args = vec![
            "fetch".into(),
            "--no-write-fetch-head".into(),
            "--no-tags".into(),
            "--refmap=".into(),
            "--no-auto-gc".into(),
            "--no-recurse-submodules".into(),
            carrier.remote.clone(),
            intent.spec.commit.clone(),
        ];
        match broker.run_coordinated_operation_at_with_hooks(
            carrier.command(&intent, args),
            carrier.repo.root(),
            QueueWait::Seconds(carrier.timeout),
            || carrier.check().map_err(|e| e.to_string()),
            |_, _| Ok(None),
        ) {
            Ok(report) => {
                read_operation = Some(report.operation.id);
                read_outcome = Some(report.operation.status.as_str().into());
                if report.ok() && source(store, carrier, &intent).is_ok() {
                    content_checked = true;
                }
            }
            Err(_error) => {
                read_outcome = Some("admission_refused".into());
            }
        }
    }
    let s = ReceiptSpec {
        profile: PROFILE.into(),
        intent: id.to_string(),
        source: intent.spec.source.clone(),
        commit: intent.spec.commit.clone(),
        tree: intent.spec.tree.clone(),
        peer: intent.spec.peer.clone(),
        destination: intent.spec.destination.clone(),
        observation,
        observed_oid: oid,
        content_checked,
        write_operation: op.as_ref().map(|o| o.id),
        write_outcome: op.as_ref().map(|o| o.status.as_str().into()),
        read_operation,
        read_outcome,
    };
    let record = canonical(&RECEIPT_SCHEMA, &s)?;
    store.read_connection().execute(
        "INSERT OR IGNORE INTO local_source_transport_receipts VALUES (?1,?2,?3)",
        params![record.id().as_str(), id.as_str(), record.canonical_bytes()],
    )?;
    store.read_connection().execute(
        "UPDATE local_source_transports SET observation=?2,receipt=?3,write_outcome=?4,operation=?5 WHERE intent=?1",
        params![id.as_str(), observation.code(), record.canonical_bytes(),s.write_outcome,s.write_operation],
    )?;
    _control.commit()?;
    Ok(Receipt {
        id: record.id(),
        spec: s,
    })
}
fn push_args(carrier: &GitCarrier, intent: &Intent) -> Vec<String> {
    vec![
        "push".into(),
        "--porcelain".into(),
        format!(
            "--force-with-lease={}:{}",
            intent.spec.destination,
            intent.spec.expected_old.as_deref().unwrap_or("")
        ),
        carrier.remote.clone(),
        format!("{}:{}", intent.spec.commit, intent.spec.destination),
    ]
}
fn find_operation(
    store: &CollaborationStore,
    broker: &Broker,
    carrier: &GitCarrier,
    intent: &Intent,
) -> Result<Option<crate::CoordinatedOperation>, TransportError> {
    let mut before = None;
    let mut found = None;
    let expected = push_args(carrier, intent);
    let saved: Option<i64> = store.read_connection().query_row(
        "SELECT operation FROM local_source_transports WHERE intent=?1",
        [intent.id.as_str()],
        |r| r.get(0),
    )?;
    if let Some(saved) = saved {
        let op = broker
            .store_ref()
            .coordinated_operation(saved)
            .map_err(crate::BrokerOpError::from)?
            .ok_or_else(|| {
                TransportError::Invalid(
                    "recorded native operation is missing; reconcile journal loss".into(),
                )
            })?;
        let args: Vec<String> = serde_json::from_str(&op.command_json)?;
        if op.provider != OperationProvider::Git
            || op.session_id != carrier.session
            || op.scope != scope(&intent.id)
            || op.repository != intent.spec.peer
            || args.get(1..) != Some(expected.as_slice())
        {
            return Err(TransportError::Invalid(
                "recorded native operation binding differs".into(),
            ));
        }
        return Ok(Some(op));
    }
    for _ in 0..128 {
        let page = broker
            .store_ref()
            .operation_history(&crate::OperationHistoryQuery {
                limit: 100,
                before_id: before,
                session_id: Some(carrier.session),
                provider: Some(OperationProvider::Git),
                ..Default::default()
            })
            .map_err(crate::BrokerOpError::from)?;
        for op in page.operations {
            if op.scope != scope(&intent.id) {
                continue;
            }
            let args: Vec<String> = serde_json::from_str(&op.command_json)?;
            if args.get(1).is_none_or(|a| a != "push") {
                continue;
            }
            if args.get(1..) != Some(expected.as_slice())
                || op.repository != intent.spec.peer
                || found.is_some()
            {
                return Err(TransportError::Invalid(
                    "conflicting or duplicate native publication journal".into(),
                ));
            }
            found = Some(op);
        }
        before = page.next_before_id;
        if before.is_none() {
            return Ok(found);
        }
    }
    Err(TransportError::Invalid(
        "operation history inspection exceeded finite page limit".into(),
    ))
}

/// Read a past receipt with canonical and index checks. This is history, never
/// current authority: callers must obtain new native provider evidence.
pub fn last_receipt(
    store: &CollaborationStore,
    id: &RecordId,
) -> Result<Option<Receipt>, TransportError> {
    let intent = load(store, id)?;
    let (observation, bytes): (String, Option<Vec<u8>>) = store.read_connection().query_row(
        "SELECT observation,receipt FROM local_source_transports WHERE intent=?1",
        [id.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let Some(bytes) = bytes else {
        if observation != "unobserved" {
            return Err(TransportError::Invalid("missing receipt body".into()));
        }
        return Ok(None);
    };
    let record = Record::decode(&bytes, &[&RECEIPT_SCHEMA])
        .map_err(|e| TransportError::Invalid(e.to_string()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    let spec: ReceiptSpec = serde_json::from_value(value["spec"].clone())?;
    let archived: Option<Vec<u8>> = store
        .read_connection()
        .query_row(
            "SELECT body FROM local_source_transport_receipts WHERE receipt=?1 AND intent=?2",
            params![record.id().as_str(), id.as_str()],
            |r| r.get(0),
        )
        .optional()?;
    let valid_observation = match spec.observation {
        Observation::Available => spec.observed_oid.as_deref() == Some(spec.commit.as_str()),
        Observation::Other => spec
            .observed_oid
            .as_ref()
            .is_some_and(|oid| oid != &spec.commit && CommitOid::parse(oid).is_ok()),
        Observation::Absent | Observation::Unavailable => spec.observed_oid.is_none(),
    };
    if bytes != record.canonical_bytes()
        || archived.as_deref() != Some(bytes.as_slice())
        || !valid_observation
        || spec.profile != PROFILE
        || spec.intent != id.as_str()
        || spec.source != intent.spec.source
        || spec.commit != intent.spec.commit
        || spec.tree != intent.spec.tree
        || spec.peer != intent.spec.peer
        || spec.destination != intent.spec.destination
        || spec.observation.code() != observation
        || (spec.content_checked
            && (spec.read_operation.is_none_or(|id| id <= 0)
                || spec.read_outcome.as_deref() != Some("succeeded")
                || spec.observation != Observation::Available
                || spec.observed_oid.as_deref() != Some(spec.commit.as_str())))
    {
        return Err(TransportError::Invalid(
            "forged or mismatched historical receipt".into(),
        ));
    }
    Ok(Some(Receipt {
        id: record.id(),
        spec,
    }))
}
#[cfg(test)]
mod tests;
