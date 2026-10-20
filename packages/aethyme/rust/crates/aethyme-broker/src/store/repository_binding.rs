//! Durable, fail-closed repository worktree-root binding state.
//!
//! Schema v53 creates one `legacy_unbound` row for existing databases. The
//! migration does not inspect configuration, the filesystem, or the host
//! registry. A fresh staged database may move to `fresh_unbound` only through
//! the bootstrap API below; legacy adoption requires a separately reviewed
//! digest and never moves or creates a worktree root.

use super::*;

use crate::broker::WorktreeRootSource;

const BINDING_RECORD_SCHEMA_VERSION: i64 = 1;
const MAX_IDENTITY_JSON_BYTES: usize = 4096;
const MAX_REPOSITORY_KEY_BYTES: usize = 1024;
const BOOTSTRAP_NONCE_HEX_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryWorktreeBindingState {
    LegacyUnbound,
    FreshUnbound,
    FreshReserved,
    Bound,
    RecoveryRequired,
}

impl RepositoryWorktreeBindingState {
    fn as_str(self) -> &'static str {
        match self {
            Self::LegacyUnbound => "legacy_unbound",
            Self::FreshUnbound => "fresh_unbound",
            Self::FreshReserved => "fresh_reserved",
            Self::Bound => "bound",
            Self::RecoveryRequired => "recovery_required",
        }
    }

    fn parse(value: &str) -> Result<Self, BrokerError> {
        match value {
            "legacy_unbound" => Ok(Self::LegacyUnbound),
            "fresh_unbound" => Ok(Self::FreshUnbound),
            "fresh_reserved" => Ok(Self::FreshReserved),
            "bound" => Ok(Self::Bound),
            "recovery_required" => Ok(Self::RecoveryRequired),
            _ => Err(invalid_binding(format!("unknown state {value:?}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryWorktreeRootLayout {
    KeyedContainer,
    DirectRoot,
}

impl RepositoryWorktreeRootLayout {
    fn as_str(self) -> &'static str {
        match self {
            Self::KeyedContainer => "keyed_container",
            Self::DirectRoot => "direct_root",
        }
    }

    fn parse(value: &str) -> Result<Self, BrokerError> {
        match value {
            "keyed_container" => Ok(Self::KeyedContainer),
            "direct_root" => Ok(Self::DirectRoot),
            _ => Err(invalid_binding(format!("unknown root layout {value:?}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryWorktreeBinding {
    pub state: RepositoryWorktreeBindingState,
    pub origin: Option<RepositoryWorktreeBindingOrigin>,
    pub repository_root: Option<PathBuf>,
    pub git_common_dir: Option<PathBuf>,
    pub repository_key: Option<String>,
    pub broker_db_path: Option<PathBuf>,
    pub bootstrap_nonce: Option<String>,
    pub worktree_root: Option<PathBuf>,
    pub root_layout: Option<RepositoryWorktreeRootLayout>,
    pub root_source: Option<WorktreeRootSource>,
    pub reservation_digest: Option<String>,
    pub parent_identity_json: Option<String>,
    pub worktree_identity_json: Option<String>,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepositoryWorktreeBindingOrigin {
    Legacy,
    Fresh,
}

impl RepositoryWorktreeBindingOrigin {
    fn parse(value: &str) -> Result<Self, BrokerError> {
        match value {
            "legacy" => Ok(Self::Legacy),
            "fresh" => Ok(Self::Fresh),
            _ => Err(invalid_binding(format!("unknown binding origin {value:?}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryRootIdentity {
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub repository_key: String,
    pub broker_db_path: PathBuf,
    pub bootstrap_nonce: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FreshRepositoryRootReservation {
    pub identity: RepositoryRootIdentity,
    pub worktree_root: PathBuf,
    pub layout: RepositoryWorktreeRootLayout,
    pub source: WorktreeRootSource,
    pub reservation_digest: String,
    pub parent_identity_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyRepositoryRootConfirmation {
    pub repository_root: PathBuf,
    pub git_common_dir: PathBuf,
    pub repository_key: String,
    pub broker_db_path: PathBuf,
    pub worktree_root: PathBuf,
    pub layout: RepositoryWorktreeRootLayout,
    pub source: WorktreeRootSource,
    pub plan_digest: String,
    pub confirmed_digest: String,
    pub parent_identity_json: String,
    pub worktree_identity_json: String,
}

impl BrokerStore {
    /// Read the singleton binding record. A missing or malformed row is a
    /// recovery condition; callers must not infer a root from current config.
    pub(crate) fn repository_worktree_binding(
        &self,
    ) -> Result<RepositoryWorktreeBinding, BrokerError> {
        read_repository_worktree_binding(&self.conn)
    }

    /// Initialize a database that was created as a private, unpublished fresh
    /// stage. This must not be called for a path opened through the ordinary
    /// legacy migration route. The stage publisher is responsible for proving
    /// that restriction before invoking this transition. The permit can only
    /// be constructed by the fresh-stage implementation in `open.rs`.
    // Staged before the fresh database publisher is wired into `open`.
    #[allow(dead_code)]
    pub(super) fn initialize_fresh_repository_worktree_binding(
        &mut self,
        _permit: &super::open::FreshDatabaseStagePermit,
        identity: &RepositoryRootIdentity,
    ) -> Result<RepositoryWorktreeBinding, BrokerError> {
        validate_repository_identity(identity)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_current_v53(&tx)?;
        let current = read_repository_worktree_binding(&tx)?;
        ensure_state(
            current.state,
            RepositoryWorktreeBindingState::LegacyUnbound,
            RepositoryWorktreeBindingState::FreshUnbound,
        )?;
        if current.origin != Some(RepositoryWorktreeBindingOrigin::Legacy)
            || current.repository_root.is_some()
            || current.bootstrap_nonce.is_some()
            || current.worktree_root.is_some()
            || current.reservation_digest.is_some()
        {
            return Err(invalid_binding(
                "legacy-unbound row contains unexpected binding data".to_owned(),
            ));
        }
        let now = super::now_ms();
        let changed = tx.execute(
            "UPDATE repository_worktree_binding
             SET state = 'fresh_unbound', origin = 'fresh', path_encoding = ?1,
                 repository_root = ?2, git_common_dir = ?3, repository_key = ?4,
                 broker_db_path = ?5, bootstrap_nonce = ?6, updated_at_ms = ?7
             WHERE singleton = 1 AND state = 'legacy_unbound' AND origin = 'legacy'",
            params![
                current_path_encoding(),
                encode_path(&identity.repository_root),
                encode_path(&identity.git_common_dir),
                identity.repository_key,
                encode_path(&identity.broker_db_path),
                identity.bootstrap_nonce,
                now,
            ],
        )?;
        if changed != 1 {
            return Err(invalid_binding(
                "fresh bootstrap lost its compare-and-set".to_owned(),
            ));
        }
        crate::schema::record_min_compatible_schema_floor(&tx, 53)?;
        tx.commit()?;
        self.repository_worktree_binding()
    }

    /// Reserve exactly one fresh root after external preflight has selected it.
    /// The identity fields must still match the fresh database's bootstrap
    /// record; retries cannot switch roots after this point.
    // Runtime callers are added with the reviewed bootstrap transition.
    #[allow(dead_code)]
    pub(crate) fn reserve_fresh_repository_worktree_root(
        &mut self,
        reservation: &FreshRepositoryRootReservation,
    ) -> Result<RepositoryWorktreeBinding, BrokerError> {
        validate_repository_identity(&reservation.identity)?;
        validate_reservation(reservation)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_current_v53(&tx)?;
        let current = read_repository_worktree_binding(&tx)?;
        ensure_state(
            current.state,
            RepositoryWorktreeBindingState::FreshUnbound,
            RepositoryWorktreeBindingState::FreshReserved,
        )?;
        ensure_fresh_identity_matches(&current, &reservation.identity)?;
        let now = super::now_ms();
        let changed = tx.execute(
            "UPDATE repository_worktree_binding
             SET state = 'fresh_reserved', worktree_root = ?1, root_layout = ?2,
                 root_source = ?3, reservation_digest = ?4,
                 parent_identity_json = ?5, updated_at_ms = ?6
             WHERE singleton = 1 AND state = 'fresh_unbound' AND origin = 'fresh'
               AND bootstrap_nonce = ?7",
            params![
                encode_path(&reservation.worktree_root),
                reservation.layout.as_str(),
                root_source_db_value(reservation.source),
                reservation.reservation_digest,
                reservation.parent_identity_json,
                now,
                reservation.identity.bootstrap_nonce,
            ],
        )?;
        if changed != 1 {
            return Err(invalid_binding(
                "fresh root reservation lost its compare-and-set".to_owned(),
            ));
        }
        crate::schema::record_min_compatible_schema_floor(&tx, 53)?;
        tx.commit()?;
        self.repository_worktree_binding()
    }

    /// Record that the reserved fresh path needs manual or reviewed recovery.
    /// This state is deliberately not a fallback: only the same reservation
    /// can later be completed.
    // Runtime callers are added with the reviewed bootstrap transition.
    #[allow(dead_code)]
    pub(crate) fn mark_repository_worktree_recovery_required(
        &mut self,
        bootstrap_nonce: &str,
        reservation_digest: &str,
    ) -> Result<RepositoryWorktreeBinding, BrokerError> {
        validate_hex_digest(reservation_digest, "reservation digest")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_current_v53(&tx)?;
        let current = read_repository_worktree_binding(&tx)?;
        ensure_state(
            current.state,
            RepositoryWorktreeBindingState::FreshReserved,
            RepositoryWorktreeBindingState::RecoveryRequired,
        )?;
        if current.bootstrap_nonce.as_deref() != Some(bootstrap_nonce)
            || current.reservation_digest.as_deref() != Some(reservation_digest)
        {
            return Err(invalid_binding(
                "recovery identity does not match the reserved root".to_owned(),
            ));
        }
        let changed = tx.execute(
            "UPDATE repository_worktree_binding
             SET state = 'recovery_required', updated_at_ms = ?1
             WHERE singleton = 1 AND state = 'fresh_reserved'
               AND bootstrap_nonce = ?2 AND reservation_digest = ?3",
            params![super::now_ms(), bootstrap_nonce, reservation_digest],
        )?;
        if changed != 1 {
            return Err(invalid_binding(
                "recovery transition lost its compare-and-set".to_owned(),
            ));
        }
        tx.commit()?;
        self.repository_worktree_binding()
    }

    /// Complete only the root already reserved by a fresh bootstrap. The
    /// caller must verify the marker and observed root identity before passing
    /// the evidence here.
    // Runtime callers are added with the reviewed bootstrap transition.
    #[allow(dead_code)]
    pub(crate) fn complete_fresh_repository_worktree_binding(
        &mut self,
        identity: &RepositoryRootIdentity,
        reservation_digest: &str,
        worktree_identity_json: &str,
    ) -> Result<RepositoryWorktreeBinding, BrokerError> {
        validate_repository_identity(identity)?;
        validate_hex_digest(reservation_digest, "reservation digest")?;
        validate_identity_json(worktree_identity_json, "worktree identity")?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_current_v53(&tx)?;
        let current = read_repository_worktree_binding(&tx)?;
        if !matches!(
            current.state,
            RepositoryWorktreeBindingState::FreshReserved
                | RepositoryWorktreeBindingState::RecoveryRequired
        ) {
            return Err(transition_error(
                current.state,
                RepositoryWorktreeBindingState::Bound,
            ));
        }
        ensure_fresh_identity_matches(&current, identity)?;
        if current.reservation_digest.as_deref() != Some(reservation_digest)
            || current.worktree_root.is_none()
            || current.root_layout.is_none()
            || current.root_source.is_none()
            || current.parent_identity_json.is_none()
        {
            return Err(invalid_binding(
                "completion does not match a complete root reservation".to_owned(),
            ));
        }
        let changed = tx.execute(
            "UPDATE repository_worktree_binding
             SET state = 'bound', worktree_identity_json = ?1, updated_at_ms = ?2
             WHERE singleton = 1 AND origin = 'fresh'
               AND state IN ('fresh_reserved', 'recovery_required')
               AND bootstrap_nonce = ?3 AND reservation_digest = ?4",
            params![
                worktree_identity_json,
                super::now_ms(),
                identity.bootstrap_nonce,
                reservation_digest,
            ],
        )?;
        if changed != 1 {
            return Err(invalid_binding(
                "fresh root completion lost its compare-and-set".to_owned(),
            ));
        }
        crate::schema::record_min_compatible_schema_floor(&tx, 53)?;
        tx.commit()?;
        self.repository_worktree_binding()
    }

    /// Bind an existing migrated database only after the plan/apply layer has
    /// rechecked every filesystem and registry fact under its admission lock.
    /// The store enforces the explicit digest and the legacy-only transition;
    /// it does not discover or adopt a path itself.
    // Runtime callers are added with the reviewed legacy-adoption route.
    #[allow(dead_code)]
    pub(crate) fn bind_legacy_repository_worktree(
        &mut self,
        confirmation: &LegacyRepositoryRootConfirmation,
    ) -> Result<RepositoryWorktreeBinding, BrokerError> {
        validate_hex_digest(&confirmation.plan_digest, "plan digest")?;
        validate_hex_digest(&confirmation.confirmed_digest, "confirmation digest")?;
        if confirmation.plan_digest != confirmation.confirmed_digest {
            return Err(BrokerError::RepositoryWorktreeBindingConfirmationMismatch);
        }
        validate_legacy_binding(confirmation)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_current_v53(&tx)?;
        let current = read_repository_worktree_binding(&tx)?;
        ensure_state(
            current.state,
            RepositoryWorktreeBindingState::LegacyUnbound,
            RepositoryWorktreeBindingState::Bound,
        )?;
        if current.origin != Some(RepositoryWorktreeBindingOrigin::Legacy)
            || current.repository_root.is_some()
            || current.worktree_root.is_some()
        {
            return Err(invalid_binding(
                "legacy-unbound row contains unexpected binding data".to_owned(),
            ));
        }
        let changed = tx.execute(
            "UPDATE repository_worktree_binding
             SET state = 'bound', origin = 'legacy', path_encoding = ?1,
                 repository_root = ?2, git_common_dir = ?3, repository_key = ?4,
                 broker_db_path = ?5, worktree_root = ?6, root_layout = ?7,
                 root_source = ?8, reservation_digest = ?9,
                 parent_identity_json = ?10, worktree_identity_json = ?11,
                 updated_at_ms = ?12
             WHERE singleton = 1 AND state = 'legacy_unbound' AND origin = 'legacy'",
            params![
                current_path_encoding(),
                encode_path(&confirmation.repository_root),
                encode_path(&confirmation.git_common_dir),
                confirmation.repository_key,
                encode_path(&confirmation.broker_db_path),
                encode_path(&confirmation.worktree_root),
                confirmation.layout.as_str(),
                root_source_db_value(confirmation.source),
                confirmation.plan_digest,
                confirmation.parent_identity_json,
                confirmation.worktree_identity_json,
                super::now_ms(),
            ],
        )?;
        if changed != 1 {
            return Err(invalid_binding(
                "legacy binding lost its compare-and-set".to_owned(),
            ));
        }
        crate::schema::record_min_compatible_schema_floor(&tx, 53)?;
        tx.commit()?;
        self.repository_worktree_binding()
    }
}

fn read_repository_worktree_binding(
    conn: &Connection,
) -> Result<RepositoryWorktreeBinding, BrokerError> {
    let raw = conn
        .query_row(
            "SELECT record_schema_version,
                    CASE WHEN length(state) <= 64 THEN state ELSE '' END,
                    CASE WHEN origin IS NULL THEN NULL
                         WHEN length(origin) <= 16 THEN origin ELSE '' END,
                    CASE WHEN path_encoding IS NULL THEN NULL
                         WHEN length(path_encoding) <= 32 THEN path_encoding ELSE '' END,
                    CASE WHEN repository_root IS NULL THEN NULL
                         WHEN length(repository_root) <= 16384 THEN repository_root ELSE X'00' END,
                    CASE WHEN git_common_dir IS NULL THEN NULL
                         WHEN length(git_common_dir) <= 16384 THEN git_common_dir ELSE X'00' END,
                    CASE WHEN repository_key IS NULL THEN NULL
                         WHEN length(repository_key) <= 1024 THEN repository_key ELSE '' END,
                    CASE WHEN broker_db_path IS NULL THEN NULL
                         WHEN length(broker_db_path) <= 16384 THEN broker_db_path ELSE X'00' END,
                    CASE WHEN bootstrap_nonce IS NULL THEN NULL
                         WHEN length(bootstrap_nonce) <= 128 THEN bootstrap_nonce ELSE '' END,
                    CASE WHEN worktree_root IS NULL THEN NULL
                         WHEN length(worktree_root) <= 16384 THEN worktree_root ELSE X'00' END,
                    CASE WHEN root_layout IS NULL THEN NULL
                         WHEN length(root_layout) <= 32 THEN root_layout ELSE '' END,
                    CASE WHEN root_source IS NULL THEN NULL
                         WHEN length(root_source) <= 32 THEN root_source ELSE '' END,
                    CASE WHEN reservation_digest IS NULL THEN NULL
                         WHEN length(reservation_digest) <= 64 THEN reservation_digest ELSE '' END,
                    CASE WHEN parent_identity_json IS NULL THEN NULL
                         WHEN length(parent_identity_json) <= 4096 THEN parent_identity_json ELSE '' END,
                    CASE WHEN worktree_identity_json IS NULL THEN NULL
                         WHEN length(worktree_identity_json) <= 4096 THEN worktree_identity_json ELSE '' END,
                    updated_at_ms
             FROM repository_worktree_binding WHERE singleton = 1",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<Vec<u8>>>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<Vec<u8>>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, Option<String>>(13)?,
                    row.get::<_, Option<String>>(14)?,
                    row.get::<_, i64>(15)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| invalid_binding("singleton row is absent".to_owned()))?;
    let (
        record_schema_version,
        state,
        origin,
        path_encoding,
        repository_root,
        git_common_dir,
        repository_key,
        broker_db_path,
        bootstrap_nonce,
        worktree_root,
        root_layout,
        root_source,
        reservation_digest,
        parent_identity_json,
        worktree_identity_json,
        updated_at_ms,
    ) = raw;
    if record_schema_version != BINDING_RECORD_SCHEMA_VERSION {
        return Err(invalid_binding(format!(
            "record schema {record_schema_version} is unsupported"
        )));
    }
    let encoding = path_encoding.as_deref();
    let record = RepositoryWorktreeBinding {
        state: RepositoryWorktreeBindingState::parse(&state)?,
        origin: origin
            .as_deref()
            .map(RepositoryWorktreeBindingOrigin::parse)
            .transpose()?,
        repository_root: decode_optional_path(repository_root, encoding)?,
        git_common_dir: decode_optional_path(git_common_dir, encoding)?,
        repository_key,
        broker_db_path: decode_optional_path(broker_db_path, encoding)?,
        bootstrap_nonce,
        worktree_root: decode_optional_path(worktree_root, encoding)?,
        root_layout: root_layout
            .as_deref()
            .map(RepositoryWorktreeRootLayout::parse)
            .transpose()?,
        root_source: root_source.as_deref().map(parse_root_source).transpose()?,
        reservation_digest,
        parent_identity_json,
        worktree_identity_json,
        updated_at_ms,
    };
    validate_binding_record(&record, encoding)?;
    let floor = read_min_compatible_schema_floor(conn)?;
    let required_floor = if record.state == RepositoryWorktreeBindingState::LegacyUnbound {
        crate::schema::MIN_COMPATIBLE_SCHEMA
    } else {
        53
    };
    if floor < required_floor {
        return Err(invalid_binding(format!(
            "compatibility floor {floor} is below {required_floor} for state {}",
            record.state.as_str()
        )));
    }
    Ok(record)
}

fn read_min_compatible_schema_floor(conn: &Connection) -> Result<i64, BrokerError> {
    let value = conn
        .query_row(
            "SELECT CASE WHEN length(value) <= 32 THEN value ELSE '' END
             FROM meta WHERE key = 'min_compatible_schema'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .ok_or_else(|| invalid_binding("compatibility floor is absent".to_owned()))?;
    value
        .parse::<i64>()
        .map_err(|_| invalid_binding("compatibility floor is malformed".to_owned()))
}

fn validate_binding_record(
    record: &RepositoryWorktreeBinding,
    encoding: Option<&str>,
) -> Result<(), BrokerError> {
    if record.updated_at_ms < 0 {
        return Err(invalid_binding("negative update timestamp".to_owned()));
    }
    let has_repository_identity = record.repository_root.is_some()
        && record.git_common_dir.is_some()
        && record.repository_key.is_some()
        && record.broker_db_path.is_some();
    let has_root_reservation = record.worktree_root.is_some()
        && record.root_layout.is_some()
        && record.root_source.is_some()
        && record.reservation_digest.is_some()
        && record.parent_identity_json.is_some();
    let expected_shape = match record.state {
        RepositoryWorktreeBindingState::LegacyUnbound => {
            record.origin == Some(RepositoryWorktreeBindingOrigin::Legacy)
                && encoding.is_none()
                && record.repository_root.is_none()
                && record.git_common_dir.is_none()
                && record.repository_key.is_none()
                && record.broker_db_path.is_none()
                && record.bootstrap_nonce.is_none()
                && record.worktree_root.is_none()
                && record.root_layout.is_none()
                && record.root_source.is_none()
                && record.reservation_digest.is_none()
                && record.parent_identity_json.is_none()
                && record.worktree_identity_json.is_none()
        }
        RepositoryWorktreeBindingState::FreshUnbound => {
            record.origin == Some(RepositoryWorktreeBindingOrigin::Fresh)
                && encoding == Some(current_path_encoding())
                && has_repository_identity
                && record.bootstrap_nonce.is_some()
                && record.worktree_root.is_none()
                && record.root_layout.is_none()
                && record.root_source.is_none()
                && record.reservation_digest.is_none()
                && record.parent_identity_json.is_none()
                && record.worktree_identity_json.is_none()
        }
        RepositoryWorktreeBindingState::FreshReserved
        | RepositoryWorktreeBindingState::RecoveryRequired => {
            record.origin == Some(RepositoryWorktreeBindingOrigin::Fresh)
                && encoding == Some(current_path_encoding())
                && has_repository_identity
                && record.bootstrap_nonce.is_some()
                && has_root_reservation
                && record.worktree_identity_json.is_none()
        }
        RepositoryWorktreeBindingState::Bound => {
            record.origin.is_some()
                && encoding == Some(current_path_encoding())
                && has_repository_identity
                && match record.origin {
                    Some(RepositoryWorktreeBindingOrigin::Legacy) => {
                        record.bootstrap_nonce.is_none()
                    }
                    Some(RepositoryWorktreeBindingOrigin::Fresh) => {
                        record.bootstrap_nonce.is_some()
                    }
                    None => false,
                }
                && has_root_reservation
                && record.worktree_identity_json.is_some()
        }
    };
    if !expected_shape {
        return Err(invalid_binding(format!(
            "row fields do not match state {}",
            record.state.as_str()
        )));
    }
    if let Some(nonce) = &record.bootstrap_nonce {
        validate_bootstrap_nonce(nonce)?;
    }
    if let Some(digest) = &record.reservation_digest {
        validate_hex_digest(digest, "reservation digest")?;
    }
    if let Some(key) = &record.repository_key {
        validate_repository_key(key)?;
    }
    for (path, field) in [
        (record.repository_root.as_deref(), "repository root"),
        (record.git_common_dir.as_deref(), "Git common directory"),
        (record.broker_db_path.as_deref(), "broker database path"),
        (record.worktree_root.as_deref(), "worktree root"),
    ] {
        if let Some(path) = path {
            validate_absolute_path(path, field)?;
        }
    }
    if let Some(identity) = &record.parent_identity_json {
        validate_identity_json(identity, "parent identity")?;
    }
    if let Some(identity) = &record.worktree_identity_json {
        validate_identity_json(identity, "worktree identity")?;
    }
    Ok(())
}

fn ensure_current_v53(conn: &Connection) -> Result<(), BrokerError> {
    let found = crate::schema::current_version(conn)?;
    if found != crate::schema::SCHEMA_VERSION || found != 53 {
        return Err(invalid_binding(format!(
            "transition requires schema 53, found {found}"
        )));
    }
    Ok(())
}

fn ensure_state(
    current: RepositoryWorktreeBindingState,
    expected: RepositoryWorktreeBindingState,
    requested: RepositoryWorktreeBindingState,
) -> Result<(), BrokerError> {
    if current != expected {
        return Err(transition_error(current, requested));
    }
    Ok(())
}

fn transition_error(
    current: RepositoryWorktreeBindingState,
    requested: RepositoryWorktreeBindingState,
) -> BrokerError {
    BrokerError::RepositoryWorktreeBindingTransition {
        current: current.as_str().to_owned(),
        requested: requested.as_str().to_owned(),
    }
}

fn ensure_fresh_identity_matches(
    record: &RepositoryWorktreeBinding,
    identity: &RepositoryRootIdentity,
) -> Result<(), BrokerError> {
    if record.origin != Some(RepositoryWorktreeBindingOrigin::Fresh)
        || record.repository_root.as_deref() != Some(identity.repository_root.as_path())
        || record.git_common_dir.as_deref() != Some(identity.git_common_dir.as_path())
        || record.repository_key.as_deref() != Some(identity.repository_key.as_str())
        || record.broker_db_path.as_deref() != Some(identity.broker_db_path.as_path())
        || record.bootstrap_nonce.as_deref() != Some(identity.bootstrap_nonce.as_str())
    {
        return Err(invalid_binding(
            "repository identity differs from the fresh bootstrap".to_owned(),
        ));
    }
    Ok(())
}

fn validate_repository_identity(identity: &RepositoryRootIdentity) -> Result<(), BrokerError> {
    validate_absolute_path(&identity.repository_root, "repository root")?;
    validate_absolute_path(&identity.git_common_dir, "Git common directory")?;
    validate_absolute_path(&identity.broker_db_path, "broker database path")?;
    validate_repository_key(&identity.repository_key)?;
    validate_bootstrap_nonce(&identity.bootstrap_nonce)?;
    Ok(())
}

fn validate_reservation(reservation: &FreshRepositoryRootReservation) -> Result<(), BrokerError> {
    validate_absolute_path(&reservation.worktree_root, "worktree root")?;
    validate_hex_digest(&reservation.reservation_digest, "reservation digest")?;
    validate_identity_json(&reservation.parent_identity_json, "parent identity")
}

fn validate_legacy_binding(
    confirmation: &LegacyRepositoryRootConfirmation,
) -> Result<(), BrokerError> {
    validate_absolute_path(&confirmation.repository_root, "repository root")?;
    validate_absolute_path(&confirmation.git_common_dir, "Git common directory")?;
    validate_absolute_path(&confirmation.broker_db_path, "broker database path")?;
    validate_absolute_path(&confirmation.worktree_root, "worktree root")?;
    validate_repository_key(&confirmation.repository_key)?;
    validate_identity_json(&confirmation.parent_identity_json, "parent identity")?;
    validate_identity_json(&confirmation.worktree_identity_json, "worktree identity")
}

fn validate_repository_key(value: &str) -> Result<(), BrokerError> {
    if value.is_empty()
        || value.len() > MAX_REPOSITORY_KEY_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(invalid_binding("repository key is invalid".to_owned()));
    }
    Ok(())
}

fn validate_bootstrap_nonce(value: &str) -> Result<(), BrokerError> {
    if value.len() != BOOTSTRAP_NONCE_HEX_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_binding("bootstrap nonce is invalid".to_owned()));
    }
    Ok(())
}

fn validate_absolute_path(path: &Path, field: &str) -> Result<(), BrokerError> {
    if !path.is_absolute() {
        return Err(invalid_binding(format!("{field} must be absolute")));
    }
    let encoded = encode_path(path);
    if encoded.len() > 16 * 1024 {
        return Err(invalid_binding(format!("{field} exceeds the path bound")));
    }
    Ok(())
}

fn validate_identity_json(value: &str, field: &str) -> Result<(), BrokerError> {
    if value.len() > MAX_IDENTITY_JSON_BYTES {
        return Err(invalid_binding(format!("{field} exceeds the size bound")));
    }
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|_| invalid_binding(format!("{field} is not valid JSON")))?;
    if !parsed.is_object() {
        return Err(invalid_binding(format!("{field} must be a JSON object")));
    }
    Ok(())
}

fn validate_hex_digest(value: &str, field: &str) -> Result<(), BrokerError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(invalid_binding(format!(
            "{field} must be lowercase SHA-256"
        )));
    }
    Ok(())
}

fn root_source_db_value(source: WorktreeRootSource) -> &'static str {
    match source {
        WorktreeRootSource::HostState => "host_state",
        WorktreeRootSource::RepositoryConfig => "repository_config",
        WorktreeRootSource::EnvironmentOverride => "environment_override",
        WorktreeRootSource::LibraryOverride => "library_override",
        WorktreeRootSource::RepositoryFallback => "repository_fallback",
    }
}

fn parse_root_source(value: &str) -> Result<WorktreeRootSource, BrokerError> {
    match value {
        "host_state" => Ok(WorktreeRootSource::HostState),
        "repository_config" => Ok(WorktreeRootSource::RepositoryConfig),
        "environment_override" => Ok(WorktreeRootSource::EnvironmentOverride),
        "library_override" => Ok(WorktreeRootSource::LibraryOverride),
        "repository_fallback" => Ok(WorktreeRootSource::RepositoryFallback),
        _ => Err(invalid_binding(format!("unknown root source {value:?}"))),
    }
}

fn current_path_encoding() -> &'static str {
    #[cfg(unix)]
    {
        "unix_bytes"
    }
    #[cfg(windows)]
    {
        "windows_utf16le"
    }
    #[cfg(not(any(unix, windows)))]
    {
        "utf8"
    }
}

fn encode_path(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect()
    }
    #[cfg(not(any(unix, windows)))]
    {
        path.to_string_lossy().as_bytes().to_vec()
    }
}

fn decode_optional_path(
    bytes: Option<Vec<u8>>,
    encoding: Option<&str>,
) -> Result<Option<PathBuf>, BrokerError> {
    let Some(bytes) = bytes else {
        return Ok(None);
    };
    let expected = current_path_encoding();
    if encoding != Some(expected) {
        return Err(invalid_binding(format!(
            "path encoding {:?} cannot be read as {expected}",
            encoding
        )));
    }
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        Ok(Some(PathBuf::from(OsString::from_vec(bytes))))
    }
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        if bytes.len() % 2 != 0 {
            return Err(invalid_binding("odd UTF-16 path byte count".to_owned()));
        }
        let words = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]));
        Ok(Some(PathBuf::from(OsString::from_wide(
            &words.collect::<Vec<_>>(),
        ))))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let path = String::from_utf8(bytes)
            .map_err(|_| invalid_binding("path is not UTF-8".to_owned()))?;
        Ok(Some(PathBuf::from(path)))
    }
}

fn invalid_binding(message: String) -> BrokerError {
    BrokerError::InvalidRepositoryWorktreeBinding(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(directory: &tempfile::TempDir) -> RepositoryRootIdentity {
        RepositoryRootIdentity {
            repository_root: directory.path().join("repo"),
            git_common_dir: directory.path().join("repo/.git"),
            repository_key: "repo-key".to_owned(),
            broker_db_path: directory.path().join("repo/.aethyme/broker.db"),
            bootstrap_nonce: "a1".repeat(32),
        }
    }

    fn open_store(directory: &tempfile::TempDir) -> BrokerStore {
        BrokerStore::open(&directory.path().join("broker.db")).unwrap()
    }

    fn floor(store: &BrokerStore) -> String {
        store
            .conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn binding_row(store: &BrokerStore) -> Vec<rusqlite::types::Value> {
        let mut statement = store
            .conn
            .prepare("SELECT * FROM repository_worktree_binding WHERE singleton = 1")
            .unwrap();
        let columns = statement.column_count();
        statement
            .query_row([], |row| {
                (0..columns)
                    .map(|column| row.get::<_, rusqlite::types::Value>(column))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .unwrap()
    }

    fn fresh_reservation(identity: RepositoryRootIdentity) -> FreshRepositoryRootReservation {
        FreshRepositoryRootReservation {
            worktree_root: identity.repository_root.join(".worktrees"),
            identity,
            layout: RepositoryWorktreeRootLayout::KeyedContainer,
            source: WorktreeRootSource::RepositoryConfig,
            reservation_digest: "b2".repeat(32),
            parent_identity_json: r#"{"device":1,"inode":2}"#.to_owned(),
        }
    }

    fn legacy_confirmation(directory: &tempfile::TempDir) -> LegacyRepositoryRootConfirmation {
        LegacyRepositoryRootConfirmation {
            repository_root: directory.path().join("repo"),
            git_common_dir: directory.path().join("repo/.git"),
            repository_key: "repo-key".to_owned(),
            broker_db_path: directory.path().join("repo/.aethyme/broker.db"),
            worktree_root: directory.path().join("worktrees"),
            layout: RepositoryWorktreeRootLayout::DirectRoot,
            source: WorktreeRootSource::LibraryOverride,
            plan_digest: "c3".repeat(32),
            confirmed_digest: "c3".repeat(32),
            parent_identity_json: r#"{"device":1,"inode":2}"#.to_owned(),
            worktree_identity_json: r#"{"device":1,"inode":3}"#.to_owned(),
        }
    }

    #[test]
    fn fresh_binding_requires_exact_bootstrap_and_reservation_before_bound() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = open_store(&directory);
        assert_eq!(floor(&store), "52");
        let identity = identity(&directory);
        let stage_permit = super::super::open::test_fresh_database_stage_permit();

        let fresh = store
            .initialize_fresh_repository_worktree_binding(&stage_permit, &identity)
            .unwrap();
        assert_eq!(fresh.state, RepositoryWorktreeBindingState::FreshUnbound);
        assert_eq!(fresh.origin, Some(RepositoryWorktreeBindingOrigin::Fresh));
        assert_eq!(floor(&store), "53");

        let mut wrong_nonce = fresh_reservation(identity.clone());
        wrong_nonce.identity.bootstrap_nonce = "d4".repeat(32);
        assert!(matches!(
            store.reserve_fresh_repository_worktree_root(&wrong_nonce),
            Err(BrokerError::InvalidRepositoryWorktreeBinding(_))
        ));
        assert_eq!(
            store.repository_worktree_binding().unwrap().state,
            RepositoryWorktreeBindingState::FreshUnbound
        );

        let reservation = fresh_reservation(identity.clone());
        let reserved = store
            .reserve_fresh_repository_worktree_root(&reservation)
            .unwrap();
        assert_eq!(
            reserved.state,
            RepositoryWorktreeBindingState::FreshReserved
        );
        assert_eq!(
            reserved.worktree_root,
            Some(reservation.worktree_root.clone())
        );

        let recovery = store
            .mark_repository_worktree_recovery_required(
                &identity.bootstrap_nonce,
                &reservation.reservation_digest,
            )
            .unwrap();
        assert_eq!(
            recovery.state,
            RepositoryWorktreeBindingState::RecoveryRequired
        );

        assert!(matches!(
            store.complete_fresh_repository_worktree_binding(
                &identity,
                &"d4".repeat(32),
                r#"{"device":1,"inode":9}"#,
            ),
            Err(BrokerError::InvalidRepositoryWorktreeBinding(_))
        ));
        assert_eq!(
            store.repository_worktree_binding().unwrap().state,
            RepositoryWorktreeBindingState::RecoveryRequired
        );

        let bound = store
            .complete_fresh_repository_worktree_binding(
                &identity,
                &reservation.reservation_digest,
                r#"{"device":1,"inode":9}"#,
            )
            .unwrap();
        assert_eq!(bound.state, RepositoryWorktreeBindingState::Bound);
        assert_eq!(bound.origin, Some(RepositoryWorktreeBindingOrigin::Fresh));
        assert_eq!(floor(&store), "53");
        assert!(matches!(
            store.reserve_fresh_repository_worktree_root(&reservation),
            Err(BrokerError::RepositoryWorktreeBindingTransition { .. })
        ));
    }

    #[test]
    fn legacy_binding_requires_matching_confirmation_and_raises_floor_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = open_store(&directory);
        let legacy = store.repository_worktree_binding().unwrap();
        assert_eq!(legacy.state, RepositoryWorktreeBindingState::LegacyUnbound);
        assert_eq!(floor(&store), "52");

        let mut wrong = legacy_confirmation(&directory);
        wrong.confirmed_digest = "d4".repeat(32);
        assert!(matches!(
            store.bind_legacy_repository_worktree(&wrong),
            Err(BrokerError::RepositoryWorktreeBindingConfirmationMismatch)
        ));
        assert_eq!(floor(&store), "52");
        assert_eq!(
            store.repository_worktree_binding().unwrap().state,
            RepositoryWorktreeBindingState::LegacyUnbound
        );

        let bound = store
            .bind_legacy_repository_worktree(&legacy_confirmation(&directory))
            .unwrap();
        assert_eq!(bound.state, RepositoryWorktreeBindingState::Bound);
        assert_eq!(bound.origin, Some(RepositoryWorktreeBindingOrigin::Legacy));
        assert_eq!(bound.reservation_digest, Some("c3".repeat(32)));
        assert_eq!(
            bound.root_layout,
            Some(RepositoryWorktreeRootLayout::DirectRoot)
        );
        assert_eq!(floor(&store), "53");
    }

    #[test]
    fn nonlegacy_binding_states_require_floor_without_repairing_binding_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = open_store(&directory);
        let identity = identity(&directory);
        let stage_permit = super::super::open::test_fresh_database_stage_permit();
        store
            .initialize_fresh_repository_worktree_binding(&stage_permit, &identity)
            .unwrap();

        let check_low_floor = |store: &mut BrokerStore| {
            let row_before = binding_row(store);
            store
                .conn
                .execute(
                    "UPDATE meta SET value = '52' WHERE key = 'min_compatible_schema'",
                    [],
                )
                .unwrap();
            assert!(matches!(
                store.repository_worktree_binding(),
                Err(BrokerError::InvalidRepositoryWorktreeBinding(_))
            ));
            assert_eq!(binding_row(store), row_before);
            assert_eq!(floor(store), "52");
            store
                .conn
                .execute(
                    "UPDATE meta SET value = '53' WHERE key = 'min_compatible_schema'",
                    [],
                )
                .unwrap();
        };

        check_low_floor(&mut store);
        let reservation = fresh_reservation(identity.clone());
        store
            .reserve_fresh_repository_worktree_root(&reservation)
            .unwrap();
        check_low_floor(&mut store);
        store
            .mark_repository_worktree_recovery_required(
                &identity.bootstrap_nonce,
                &reservation.reservation_digest,
            )
            .unwrap();
        check_low_floor(&mut store);
        store
            .complete_fresh_repository_worktree_binding(
                &identity,
                &reservation.reservation_digest,
                r#"{"device":1,"inode":9}"#,
            )
            .unwrap();

        let row_before = binding_row(&store);
        store
            .conn
            .execute("DELETE FROM meta WHERE key = 'min_compatible_schema'", [])
            .unwrap();
        assert!(matches!(
            store.repository_worktree_binding(),
            Err(BrokerError::InvalidRepositoryWorktreeBinding(_))
        ));
        assert_eq!(binding_row(&store), row_before);
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT count(*) FROM meta WHERE key = 'min_compatible_schema'",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn missing_binding_table_is_not_recreated_as_legacy_unbound() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = open_store(&directory);
        store
            .conn
            .execute("DROP TABLE repository_worktree_binding", [])
            .unwrap();
        crate::schema::migrate(&store.conn).unwrap();
        assert!(matches!(
            store.repository_worktree_binding(),
            Err(BrokerError::Sqlite(_))
        ));
        assert!(matches!(
            store.initialize_fresh_repository_worktree_binding(
                &super::super::open::test_fresh_database_stage_permit(),
                &identity(&directory),
            ),
            Err(BrokerError::Sqlite(_))
        ));
    }

    #[test]
    fn malformed_binding_state_is_refused_instead_of_reconstructed() {
        let directory = tempfile::tempdir().unwrap();
        let store = open_store(&directory);
        store
            .conn
            .execute(
                "UPDATE repository_worktree_binding SET state = 'bound' WHERE singleton = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.repository_worktree_binding(),
            Err(BrokerError::InvalidRepositoryWorktreeBinding(_))
        ));
    }

    #[test]
    fn nullable_binding_fields_preserve_null_and_reject_empty_values() {
        let directory = tempfile::tempdir().unwrap();
        let store = open_store(&directory);
        let legacy = store.repository_worktree_binding().unwrap();
        assert_eq!(legacy.state, RepositoryWorktreeBindingState::LegacyUnbound);
        assert_eq!(legacy.origin, Some(RepositoryWorktreeBindingOrigin::Legacy));
        assert!(legacy.repository_root.is_none());

        store
            .conn
            .execute(
                "UPDATE repository_worktree_binding SET path_encoding = '' WHERE singleton = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.repository_worktree_binding(),
            Err(BrokerError::InvalidRepositoryWorktreeBinding(_))
        ));

        store
            .conn
            .execute(
                "UPDATE repository_worktree_binding
                 SET path_encoding = NULL, repository_root = X'' WHERE singleton = 1",
                [],
            )
            .unwrap();
        assert!(matches!(
            store.repository_worktree_binding(),
            Err(BrokerError::InvalidRepositoryWorktreeBinding(_))
        ));
    }

    #[test]
    fn path_encoding_preserves_non_utf8_unix_paths() {
        #[cfg(unix)]
        {
            use std::ffi::OsString;
            use std::os::unix::ffi::OsStringExt;

            let path = PathBuf::from(OsString::from_vec(vec![b'/', b't', b'm', b'p', b'/', 0xff]));
            assert_eq!(
                decode_optional_path(Some(encode_path(&path)), Some("unix_bytes")).unwrap(),
                Some(path)
            );
        }
    }
}
