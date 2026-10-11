//! Provisional Local budget reservation model (#668).
//! This records declared envelopes, not enforced process limits or invoices.
//! The future trusted executor must enforce a grant and provide usage receipts.
//! Host capacity grants remain in `resources::HostResourceStore`.

use aethyme_contracts::experimental_v0::{FieldKind, FieldSpec, Record, RecordId, RecordSchema};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::collaboration_state::CollaborationStore;
use crate::local_target::{self as target, ControllerOwner, Phase, TargetError};
use crate::local_wave::{self as wave, WavePhase};

const SCHEMA_NAME: &str = "aethyme.local-budget/provisional-v0";
static SCHEMA: RecordSchema = RecordSchema {
    name: SCHEMA_NAME,
    fields: &[FieldSpec {
        name: "spec",
        required: true,
        kind: FieldKind::Opaque,
        capability: None,
    }],
    capabilities: &[],
};
const MAX_LINEAGE_DEPTH: usize = 8;

/// Conservative cumulative envelope units. Memory/disk/slot amounts are not
/// reclaimed automatically after a lease expires or a process disappears.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Units {
    pub time_ms: i64,
    pub memory_bytes: i64,
    pub disk_bytes: i64,
    pub model_requests: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub spend_microunits: i64,
    pub slots: i64,
    pub previews: i64,
}
impl Units {
    fn values(self) -> [i64; 9] {
        [
            self.time_ms,
            self.memory_bytes,
            self.disk_bytes,
            self.model_requests,
            self.input_tokens,
            self.output_tokens,
            self.spend_microunits,
            self.slots,
            self.previews,
        ]
    }
    fn from_values(v: [i64; 9]) -> Self {
        Self {
            time_ms: v[0],
            memory_bytes: v[1],
            disk_bytes: v[2],
            model_requests: v[3],
            input_tokens: v[4],
            output_tokens: v[5],
            spend_microunits: v[6],
            slots: v[7],
            previews: v[8],
        }
    }
    fn validate(self) -> Result<(), BudgetError> {
        if self.values().iter().any(|v| *v < 0 || *v > i64::MAX / 4096) {
            return Err(BudgetError::Invalid("unit bounds".into()));
        }
        Ok(())
    }
    fn zero(self) -> bool {
        self == Self::default()
    }
    fn within(self, cap: Self) -> bool {
        self.values().iter().zip(cap.values()).all(|(a, b)| *a <= b)
    }
    fn add(self, other: Self) -> Result<Self, BudgetError> {
        let mut v = self.values();
        for (a, b) in v.iter_mut().zip(other.values()) {
            *a = a.checked_add(b).ok_or(BudgetError::Arithmetic)?;
        }
        Ok(Self::from_values(v))
    }
    fn subtract(self, other: Self) -> Result<Self, BudgetError> {
        if !other.within(self) {
            return Err(BudgetError::Arithmetic);
        }
        let mut v = self.values();
        for (a, b) in v.iter_mut().zip(other.values()) {
            *a -= b;
        }
        Ok(Self::from_values(v))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPolicy {
    pub authorization: String,
    pub price_profile: String,
    pub cap: Units,
    pub recovery_headroom: Units,
    pub mandatory_headroom: Units,
    pub max_root_reservations: usize,
}
impl ProjectPolicy {
    fn validate(&self) -> Result<(), BudgetError> {
        id(&self.authorization)?;
        id(&self.price_profile)?;
        self.cap.validate()?;
        self.recovery_headroom.validate()?;
        self.mandatory_headroom.validate()?;
        if self.cap.zero()
            || !self
                .recovery_headroom
                .add(self.mandatory_headroom)?
                .within(self.cap)
            || self.max_root_reservations == 0
            || self.max_root_reservations > 4096
        {
            return Err(BudgetError::Invalid("project policy bounds".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkKind {
    Composition,
    Verification,
    OptionalAnalysis,
    Recovery,
}
impl WorkKind {
    fn optional(self) -> bool {
        self == Self::OptionalAnalysis
    }
    fn recovery(self) -> bool {
        self == Self::Recovery
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub nonce: String,
    pub recipe: String,
    pub kind: WorkKind,
    pub units: Units,
    pub max_attempts: u32,
    pub deadline_ms: i64,
}
impl Request {
    fn validate(&self) -> Result<(), BudgetError> {
        id(&self.nonce)?;
        id(&self.recipe)?;
        self.units.validate()?;
        if self.units.zero()
            || self.max_attempts == 0
            || self.max_attempts > 8
            || self.deadline_ms < 0
        {
            return Err(BudgetError::Invalid("request bounds".into()));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReservationSpec {
    issuer: String,
    target: String,
    revision: String,
    wave: String,
    objective: String,
    project_policy: String,
    objective_policy: String,
    request: Request,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent: Option<String>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Reserved,
    PartlyConsumed,
    CompletionPending,
    Consumed,
    ReleasedUnused,
    ConsumptionUnknown,
}
impl State {
    fn held(self) -> bool {
        !matches!(self, Self::Consumed | Self::ReleasedUnused)
    }
    fn code(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::PartlyConsumed => "partly_consumed",
            Self::CompletionPending => "completion_pending",
            Self::Consumed => "consumed",
            Self::ReleasedUnused => "released_unused",
            Self::ConsumptionUnknown => "consumption_unknown",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reservation {
    pub id: RecordId,
    pub root: RecordId,
    pub state: State,
    pub allocated: Units,
    pub spent: Units,
    pub attempts: u32,
    pub version: i64,
    spec: ReservationSpec,
}

/// A non-cloneable attempt identity. This is a model dispatch permit, not an
/// executor capability, verification receipt or permission to publish source.
#[derive(Debug)]
pub struct Attempt {
    reservation: RecordId,
    version: i64,
    number: u32,
    remaining: Units,
}
impl Attempt {
    pub fn reservation(&self) -> &RecordId {
        &self.reservation
    }
    pub fn number(&self) -> u32 {
        self.number
    }
    pub fn remaining(&self) -> Units {
        self.remaining
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BudgetError {
    #[error("invalid budget ledger: {0}")]
    Invalid(String),
    #[error("budget arithmetic overflow or underflow")]
    Arithmetic,
    #[error("immutable budget authorization or idempotency key differs")]
    Immutable,
    #[error("budget unavailable in {0}")]
    Capacity(&'static str),
    #[error("reservation missing")]
    Missing,
    #[error("reservation has outstanding or unknown work")]
    Pending,
    #[error("reservation or attempt is stale")]
    Stale,
    #[error("retry allowance exhausted")]
    AttemptsExhausted,
    #[error("measured usage exceeded its reserved envelope; consumption remains unknown")]
    UsageExceeded,
    #[error(transparent)]
    Target(#[from] TargetError),
    #[error(transparent)]
    Wave(#[from] wave::WaveError),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}
fn id(value: &str) -> Result<RecordId, BudgetError> {
    RecordId::parse(value).map_err(|e| BudgetError::Invalid(e.to_string()))
}
fn record(spec: &impl Serialize) -> Result<Record, BudgetError> {
    let bytes = serde_json::to_vec(&serde_json::json!({"schema":SCHEMA_NAME,"spec":spec}))?;
    Record::decode(&bytes, &[&SCHEMA]).map_err(|e| BudgetError::Invalid(e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(key: &str, body: &[u8]) -> Result<T, BudgetError> {
    let r = Record::decode(body, &[&SCHEMA]).map_err(|e| BudgetError::Invalid(e.to_string()))?;
    if r.id() != id(key)? || r.canonical_bytes() != body {
        return Err(BudgetError::Invalid("record digest".into()));
    }
    let v: serde_json::Value = serde_json::from_slice(body)?;
    Ok(serde_json::from_value(v["spec"].clone())?)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectRecord {
    issuer: String,
    kind: String,
    policy: ProjectPolicy,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObjectiveRecord {
    issuer: String,
    kind: String,
    objective: String,
    project_policy: String,
    cap: Units,
}
fn project(conn: &Connection, issuer: &str) -> Result<(RecordId, ProjectPolicy), BudgetError> {
    let (key, body, appointed): (String, Vec<u8>, String) = conn
        .query_row(
            "SELECT policy,body,issuer FROM local_budget_project WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(BudgetError::Missing)?;
    let spec: ProjectRecord = decode(&key, &body)?;
    spec.policy.validate()?;
    if spec.kind != "project" || spec.issuer != issuer || appointed != issuer {
        return Err(BudgetError::Invalid("project issuer".into()));
    }
    Ok((id(&key)?, spec.policy))
}
fn objective(
    conn: &Connection,
    issuer: &str,
    auth: &str,
    project_id: &RecordId,
) -> Result<(RecordId, Units), BudgetError> {
    let (key, body, appointed): (String, Vec<u8>, String) = conn
        .query_row(
            "SELECT policy,body,issuer FROM local_budget_objectives WHERE objective=?1",
            [auth],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .ok_or(BudgetError::Missing)?;
    let spec: ObjectiveRecord = decode(&key, &body)?;
    spec.cap.validate()?;
    if spec.kind != "objective"
        || spec.issuer != issuer
        || appointed != issuer
        || spec.objective != auth
        || spec.project_policy != project_id.as_str()
    {
        return Err(BudgetError::Invalid("objective authority".into()));
    }
    Ok((id(&key)?, spec.cap))
}

/// Appoint one project budget issuer with immutable caps and protected lanes.
/// Project aggregate limits remain necessary even for fresh objective IDs.
pub fn configure_project(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    policy: ProjectPolicy,
) -> Result<RecordId, BudgetError> {
    target::authorize(store, owner)?;
    policy.validate()?;
    let r = record(&ProjectRecord {
        issuer: owner.issuer().into(),
        kind: "project".into(),
        policy,
    })?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT policy FROM local_budget_project WHERE singleton=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != r.id().as_str() {
            return Err(BudgetError::Immutable);
        }
    } else {
        tx.execute(
            "INSERT INTO local_budget_project(singleton,issuer,policy,body) VALUES(1,?1,?2,?3)",
            params![owner.issuer(), r.id().as_str(), r.canonical_bytes()],
        )?;
    }
    tx.commit()?;
    Ok(r.id())
}

/// A stable objective lineage. Calling this again cannot replenish its cap.
pub fn authorize_objective(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    cap: Units,
) -> Result<RecordId, BudgetError> {
    target::authorize(store, owner)?;
    cap.validate()?;
    let state = target::status(store, owner.target())?;
    let revision =
        target::load_revision(store, state.revision.as_ref().ok_or(TargetError::Missing)?)?;
    let (p, policy) = project(store.read_connection(), owner.issuer())?;
    if cap.zero() || !cap.within(policy.cap) {
        return Err(BudgetError::Invalid("objective cap".into()));
    }
    let auth = &revision.spec.objective_authorization;
    let r = record(&ObjectiveRecord {
        issuer: owner.issuer().into(),
        kind: "objective".into(),
        objective: auth.clone(),
        project_policy: p.to_string(),
        cap,
    })?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let existing: Option<String> = tx
        .query_row(
            "SELECT policy FROM local_budget_objectives WHERE objective=?1",
            [auth],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = existing {
        if existing != r.id().as_str() {
            return Err(BudgetError::Immutable);
        }
    } else {
        tx.execute(
            "INSERT INTO local_budget_objectives(objective,issuer,policy,body) VALUES(?1,?2,?3,?4)",
            params![auth, owner.issuer(), r.id().as_str(), r.canonical_bytes()],
        )?;
    }
    tx.commit()?;
    Ok(r.id())
}

fn load_conn(conn: &Connection, key: &RecordId) -> Result<Reservation, BudgetError> {
    load_conn_at(conn, key, 0)
}
fn load_conn_at(
    conn: &Connection,
    key: &RecordId,
    depth: usize,
) -> Result<Reservation, BudgetError> {
    if depth >= MAX_LINEAGE_DEPTH {
        return Err(BudgetError::Invalid("lineage depth".into()));
    }
    type Row = (
        Vec<u8>,
        String,
        Vec<u8>,
        u32,
        i64,
        String,
        Option<String>,
        String,
        String,
        String,
        String,
    );
    let row:Row=conn.query_row("SELECT body,phase,spent,attempts,version,root,parent,target,revision,wave,nonce FROM local_budget_reservations WHERE reservation=?1",[key.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?,r.get(9)?,r.get(10)?))).optional()?.ok_or(BudgetError::Missing)?;
    let spec: ReservationSpec = decode(key.as_str(), &row.0)?;
    spec.request.validate()?;
    let spent: Units = serde_json::from_slice(&row.2)?;
    spent.validate()?;
    let state: State = serde_json::from_value(serde_json::Value::String(row.1))?;
    if spec.parent != row.6
        || spec.target != row.7
        || spec.revision != row.8
        || spec.wave != row.9
        || spec.request.nonce != row.10
        || !spent.within(spec.request.units)
        || row.3 > spec.request.max_attempts
        || row.4 < 0
    {
        return Err(BudgetError::Invalid("reservation index".into()));
    }
    let (p, _) = project(conn, &spec.issuer)?;
    let (o, cap) = objective(conn, &spec.issuer, &spec.objective, &p)?;
    if spec.project_policy != p.as_str()
        || spec.objective_policy != o.as_str()
        || !spec.request.units.within(cap)
    {
        return Err(BudgetError::Invalid("reservation policy".into()));
    }
    let root = id(&row.5)?;
    if spec.parent.is_none() && root != *key {
        return Err(BudgetError::Invalid("root lineage".into()));
    }
    if let Some(parent) = &spec.parent {
        let parent = load_conn_at(conn, &id(parent)?, depth + 1)?;
        if parent.root != root
            || parent.spec.issuer != spec.issuer
            || parent.spec.target != spec.target
            || parent.spec.revision != spec.revision
            || parent.spec.wave != spec.wave
            || parent.spec.objective != spec.objective
            || parent.spec.request.kind != spec.request.kind
            || parent.spec.request.recipe != spec.request.recipe
            || spec.request.deadline_ms > parent.spec.request.deadline_ms
            || spec.request.max_attempts > parent.spec.request.max_attempts
            || !spec.request.units.within(parent.allocated)
        {
            return Err(BudgetError::Invalid("delegation authority".into()));
        }
    }
    Ok(Reservation {
        id: key.clone(),
        root,
        state,
        allocated: spec.request.units,
        spent,
        attempts: row.3,
        version: row.4,
        spec,
    })
}
pub fn load(store: &CollaborationStore, key: &RecordId) -> Result<Reservation, BudgetError> {
    let r = load_conn(store.read_connection(), key)?;
    let w = wave::load(store, &id(&r.spec.wave)?)?;
    if r.spec.target != w.spec.target
        || r.spec.issuer != w.spec.issuer
        || r.spec.revision != w.spec.revision
        || r.spec.objective != w.spec.objective_authorization
    {
        return Err(BudgetError::Invalid("wave authorization".into()));
    }
    Ok(r)
}

fn rows(conn: &Connection, sql: &str, args: &[&str]) -> Result<Vec<Reservation>, BudgetError> {
    let keys: Vec<String> = conn
        .prepare(sql)?
        .query_map(rusqlite::params_from_iter(args.iter()), |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    keys.into_iter()
        .map(|k| load_conn(conn, &id(&k)?))
        .collect()
}
fn charge(r: &Reservation) -> Units {
    if r.state.held() { r.allocated } else { r.spent }
}
fn sum(items: impl IntoIterator<Item = Units>) -> Result<Units, BudgetError> {
    items.into_iter().try_fold(Units::default(), Units::add)
}
fn guard(
    conn: &Connection,
    owner: &ControllerOwner,
    revision: &str,
    version: i64,
) -> Result<(), BudgetError> {
    let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM local_targets WHERE target=?1 AND issuer=?2 AND active_revision=?3 AND version=?4)",params![owner.target(),owner.issuer(),revision,version],|r|r.get(0))?;
    if !valid {
        return Err(BudgetError::Stale);
    }
    Ok(())
}
fn current(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    wave_id: &RecordId,
    kind: WorkKind,
) -> Result<(wave::Wave, target::ControllerStatus), BudgetError> {
    target::authorize(store, owner)?;
    let w = wave::load(store, wave_id)?;
    let s = target::status(store, owner.target())?;
    if w.spec.target != owner.target()
        || w.spec.issuer != owner.issuer()
        || s.revision.as_ref().map(RecordId::as_str) != Some(w.spec.revision.as_str())
    {
        return Err(BudgetError::Stale);
    }
    let phase_ok = match kind {
        WorkKind::Composition => w.phase == WavePhase::Frozen && s.phase == Phase::WaveFrozen,
        WorkKind::Verification => {
            w.phase == WavePhase::Verifying
                && (s.phase == Phase::Verifying
                    || (s.phase == Phase::WaitingForInput
                        && s.resume_phase == Some(Phase::Verifying)))
        }
        WorkKind::OptionalAnalysis => {
            matches!(w.phase, WavePhase::Frozen | WavePhase::Verifying)
                && !matches!(
                    s.phase,
                    Phase::StopRequested
                        | Phase::Stopped
                        | Phase::BlockedTargetDrift
                        | Phase::OutcomeUnknown
                )
        }
        WorkKind::Recovery => true,
    };
    if !phase_ok {
        return Err(BudgetError::Stale);
    }
    Ok((w, s))
}
fn insert(
    conn: &Connection,
    r: &Record,
    spec: &ReservationSpec,
    root: &RecordId,
) -> Result<(), BudgetError> {
    conn.execute("INSERT INTO local_budget_reservations(reservation,nonce,target,revision,wave,root,parent,body,phase,spent) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'reserved',?9)",params![r.id().as_str(),spec.request.nonce,spec.target,spec.revision,spec.wave,root.as_str(),spec.parent,r.canonical_bytes(),serde_json::to_vec(&Units::default())?])?;
    Ok(())
}
fn duplicate(
    conn: &Connection,
    nonce: &str,
    r: &Record,
) -> Result<Option<Reservation>, BudgetError> {
    let key: Option<String> = conn
        .query_row(
            "SELECT reservation FROM local_budget_reservations WHERE nonce=?1",
            [nonce],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(key) = key {
        if key != r.id().as_str() {
            return Err(BudgetError::Immutable);
        }
        return Ok(Some(load_conn(conn, &id(&key)?)?));
    }
    Ok(None)
}

/// Reserve declared aggregate and objective envelopes in one transaction.
/// Optional analysis cannot use the protected mandatory/recovery headroom.
pub fn reserve(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    wave_id: &RecordId,
    request: Request,
    now_ms: i64,
) -> Result<Reservation, BudgetError> {
    request.validate()?;
    if now_ms < 0
        || now_ms >= request.deadline_ms
        || request.deadline_ms.saturating_sub(now_ms) > 86_400_000
    {
        return Err(BudgetError::Invalid("deadline".into()));
    }
    let (w, state) = current(store, owner, wave_id, request.kind)?;
    let (p, policy) = project(store.read_connection(), owner.issuer())?;
    let (o, cap) = objective(
        store.read_connection(),
        owner.issuer(),
        &w.spec.objective_authorization,
        &p,
    )?;
    let spec = ReservationSpec {
        issuer: owner.issuer().into(),
        target: owner.target().into(),
        revision: w.spec.revision,
        wave: wave_id.to_string(),
        objective: w.spec.objective_authorization,
        project_policy: p.to_string(),
        objective_policy: o.to_string(),
        request,
        parent: None,
    };
    let r = record(&spec)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard(&tx, owner, &spec.revision, state.version)?;
    if let Some(existing) = duplicate(&tx, &spec.request.nonce, &r)? {
        return Ok(existing);
    }
    let roots = rows(
        &tx,
        "SELECT reservation FROM local_budget_reservations WHERE parent IS NULL",
        &[],
    )?;
    if roots.len() >= policy.max_root_reservations {
        return Err(BudgetError::Capacity("ledger bound"));
    }
    let total = sum(roots.iter().map(charge))?.add(spec.request.units)?;
    if !total.within(policy.cap) {
        return Err(BudgetError::Capacity("project"));
    }
    let objective = sum(roots
        .iter()
        .filter(|r| r.spec.objective == spec.objective)
        .map(charge))?
    .add(spec.request.units)?;
    if !objective.within(cap) {
        return Err(BudgetError::Capacity("objective"));
    }
    if !spec.request.kind.recovery() {
        let used = sum(roots
            .iter()
            .filter(|r| !r.spec.request.kind.recovery())
            .map(charge))?
        .add(spec.request.units)?;
        if !used.within(policy.cap.subtract(policy.recovery_headroom)?) {
            return Err(BudgetError::Capacity("recovery headroom"));
        }
    }
    if spec.request.kind.optional() {
        let used = sum(roots
            .iter()
            .filter(|r| r.spec.request.kind.optional())
            .map(charge))?
        .add(spec.request.units)?;
        if !used.within(
            policy
                .cap
                .subtract(policy.recovery_headroom)?
                .subtract(policy.mandatory_headroom)?,
        ) {
            return Err(BudgetError::Capacity("mandatory headroom"));
        }
    }
    insert(&tx, &r, &spec, &r.id())?;
    tx.commit()?;
    load(store, &r.id())
}

fn owned(
    store: &CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
) -> Result<Reservation, BudgetError> {
    target::authorize(store, owner)?;
    let r = load(store, key)?;
    if r.spec.target != owner.target() || r.spec.issuer != owner.issuer() {
        return Err(BudgetError::Stale);
    }
    Ok(r)
}
fn children(conn: &Connection, key: &RecordId) -> Result<Vec<Reservation>, BudgetError> {
    rows(
        conn,
        "SELECT reservation FROM local_budget_reservations WHERE parent=?1",
        &[key.as_str()],
    )
}

/// Delegation subdivides existing allowance; it never charges a new root cap.
pub fn delegate(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    parent: &RecordId,
    mut request: Request,
    now_ms: i64,
) -> Result<Reservation, BudgetError> {
    let parent = owned(store, owner, parent)?;
    request.validate()?;
    if !matches!(parent.state, State::Reserved | State::PartlyConsumed)
        || now_ms < 0
        || now_ms >= request.deadline_ms
        || request.deadline_ms > parent.spec.request.deadline_ms
        || request.kind != parent.spec.request.kind
        || request.recipe != parent.spec.request.recipe
        || request.max_attempts > parent.spec.request.max_attempts
    {
        return Err(BudgetError::Pending);
    }
    let (_, state) = current(store, owner, &id(&parent.spec.wave)?, request.kind)?;
    // Children inherit the pinned recipe/class and retry envelope; the issuer
    // cannot promote optional work by relabelling a delegated allocation.
    request.kind = parent.spec.request.kind;
    let spec = ReservationSpec {
        request,
        parent: Some(parent.id.to_string()),
        ..parent.spec.clone()
    };
    let record = record(&spec)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard(&tx, owner, &spec.revision, state.version)?;
    let latest = load_conn(&tx, &parent.id)?;
    if latest.version != parent.version {
        return Err(BudgetError::Stale);
    }
    if let Some(existing) = duplicate(&tx, &spec.request.nonce, &record)? {
        return Ok(existing);
    }
    let mut ancestor = Some(parent.id.clone());
    let mut depth = 0;
    while let Some(key) = ancestor {
        depth += 1;
        if depth >= MAX_LINEAGE_DEPTH {
            return Err(BudgetError::Capacity("delegation depth"));
        }
        let r = load_conn(&tx, &key)?;
        if r.root != parent.root {
            return Err(BudgetError::Invalid("delegation lineage".into()));
        }
        ancestor = r.spec.parent.as_deref().map(id).transpose()?;
    }
    let used = parent
        .spent
        .add(sum(children(&tx, &parent.id)?.iter().map(charge))?)?;
    if !used.add(spec.request.units)?.within(parent.allocated) {
        return Err(BudgetError::Capacity("parent"));
    }
    insert(&tx, &record, &spec, &parent.root)?;
    tx.execute(
        "UPDATE local_budget_reservations SET version=version+1 WHERE reservation=?1",
        [parent.id.as_str()],
    )?;
    tx.commit()?;
    load(store, &record.id())
}

/// Persist completion_pending before external dispatch. Deadline expiration
/// does not settle prior work, and retry counters never reset the spent units.
pub fn begin_attempt(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
    now_ms: i64,
) -> Result<Attempt, BudgetError> {
    let r = owned(store, owner, key)?;
    let (_, state) = current(store, owner, &id(&r.spec.wave)?, r.spec.request.kind)?;
    if now_ms < 0 || now_ms >= r.spec.request.deadline_ms {
        return Err(BudgetError::Invalid("deadline".into()));
    }
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    guard(&tx, owner, &r.spec.revision, state.version)?;
    let r = load_conn(&tx, key)?;
    if !matches!(r.state, State::Reserved | State::PartlyConsumed)
        || children(&tx, key)?.iter().any(|c| c.state.held())
    {
        return Err(BudgetError::Pending);
    }
    if r.attempts >= r.spec.request.max_attempts {
        return Err(BudgetError::AttemptsExhausted);
    }
    let root = load_conn(&tx, &r.root)?;
    let used_attempts: u32 = tx.query_row(
        "SELECT coalesce(sum(attempts),0) FROM local_budget_reservations WHERE root=?1",
        [r.root.as_str()],
        |row| row.get(0),
    )?;
    if used_attempts >= root.spec.request.max_attempts {
        return Err(BudgetError::AttemptsExhausted);
    }
    let used = r.spent.add(sum(children(&tx, key)?.iter().map(charge))?)?;
    let remaining = r.allocated.subtract(used)?;
    if remaining.zero() {
        return Err(BudgetError::Capacity("reservation"));
    }

    tx.execute("UPDATE local_budget_reservations SET phase='completion_pending',attempts=attempts+1,version=version+1 WHERE reservation=?1",[key.as_str()])?;
    tx.commit()?;
    Ok(Attempt {
        reservation: key.clone(),
        version: r.version + 1,
        number: r.attempts + 1,
        remaining,
    })
}

fn charge_usage(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
    version: i64,
    usage: Units,
    receipt: &RecordId,
    reconcile: bool,
) -> Result<Reservation, BudgetError> {
    owned(store, owner, key)?;
    usage.validate()?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let r = load_conn(&tx, key)?;
    let expected = if reconcile {
        State::ConsumptionUnknown
    } else {
        State::CompletionPending
    };
    if r.version != version || r.state != expected {
        return Err(BudgetError::Stale);
    }
    if children(&tx, key)?.iter().any(|c| c.state.held()) {
        return Err(BudgetError::Pending);
    }
    let next = r.spent.add(usage)?;
    let children_spent = sum(children(&tx, key)?.iter().map(charge))?;
    if !next.add(children_spent)?.within(r.allocated) {
        tx.execute("UPDATE local_budget_reservations SET phase='consumption_unknown',detail='usage exceeded reserved envelope',version=version+1 WHERE reservation=?1",[key.as_str()])?;
        tx.commit()?;
        return Err(BudgetError::UsageExceeded);
    }
    tx.execute("UPDATE local_budget_reservations SET phase='partly_consumed',spent=?2,receipt=?3,detail=NULL,version=version+1 WHERE reservation=?1",params![key.as_str(),serde_json::to_vec(&next)?,receipt.as_str()])?;
    tx.commit()?;
    load(store, key)
}

/// Owner-supplied accounting only. A future trusted receipt adapter must verify
/// producer, exact attempt/profile and actual usage before calling this API.
pub fn settle(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    attempt: &Attempt,
    usage: Units,
    receipt: &RecordId,
) -> Result<Reservation, BudgetError> {
    charge_usage(
        store,
        owner,
        &attempt.reservation,
        attempt.version,
        usage,
        receipt,
        false,
    )
}
/// Conservative crash/provider ambiguity. No automatic timeout release.
pub fn mark_unknown(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
    version: i64,
    detail: &str,
) -> Result<Reservation, BudgetError> {
    owned(store, owner, key)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let r = load_conn(&tx, key)?;
    if r.version != version || r.state != State::CompletionPending {
        return Err(BudgetError::Stale);
    }
    tx.execute("UPDATE local_budget_reservations SET phase='consumption_unknown',detail=?2,version=version+1 WHERE reservation=?1",params![key.as_str(),detail])?;
    tx.commit()?;
    load(store, key)
}
/// Only explicit reconciled usage can turn unknown consumption into a receipt.
pub fn reconcile(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
    version: i64,
    usage: Units,
    receipt: &RecordId,
) -> Result<Reservation, BudgetError> {
    charge_usage(store, owner, key, version, usage, receipt, true)
}

/// Release unused remainder only after all delegated/in-flight work settled.
/// Consumed descendants are rolled into the root charge exactly once.
pub fn close(
    store: &mut CollaborationStore,
    owner: &ControllerOwner,
    key: &RecordId,
    version: i64,
) -> Result<Reservation, BudgetError> {
    owned(store, owner, key)?;
    let tx = store
        .connection()
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let r = load_conn(&tx, key)?;
    if r.version != version {
        return Err(BudgetError::Stale);
    }
    if !matches!(r.state, State::Reserved | State::PartlyConsumed) {
        return Err(BudgetError::Pending);
    }
    let children = children(&tx, key)?;
    if children.iter().any(|c| c.state.held()) {
        return Err(BudgetError::Pending);
    }
    let spent = r.spent.add(sum(children.iter().map(charge))?)?;
    if !spent.within(r.allocated) {
        return Err(BudgetError::Invalid("delegated spending".into()));
    }
    let phase = if spent.zero() {
        State::ReleasedUnused
    } else {
        State::Consumed
    };
    tx.execute("UPDATE local_budget_reservations SET phase=?2,spent=?3,version=version+1 WHERE reservation=?1",params![key.as_str(),phase.code(),serde_json::to_vec(&spent)?])?;
    tx.commit()?;
    load(store, key)
}

/// Ledger totals count each root once. This report is not measured throughput.
pub fn charged(store: &CollaborationStore, owner: &ControllerOwner) -> Result<Units, BudgetError> {
    target::authorize(store, owner)?;
    project(store.read_connection(), owner.issuer())?;
    sum(rows(
        store.read_connection(),
        "SELECT reservation FROM local_budget_reservations WHERE parent IS NULL",
        &[],
    )?
    .iter()
    .map(charge))
}

#[cfg(test)]
mod tests;
