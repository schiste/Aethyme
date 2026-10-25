use rusqlite::{OptionalExtension as _, TransactionBehavior, params};

use crate::BrokerError;
use crate::base_incidents::{
    BaseFailureIncident, MAX_OPEN_BASE_FAILURE_INCIDENTS, RETAIN_RESOLVED_BASE_FAILURE_INCIDENTS,
    invalid, normalize_test_ids,
};

const COLUMNS: &str =
    "id, base_commit, tree_hash, gate_name, definition_hash, execution_profile_hash,
    owner_session_id, repair_pr, test_ids_json, confirmed_run_id, confirmed_at,
    resolved_run_id, resolved_base_commit, resolved_at, confirmed_run_created_at";

pub(crate) struct GateProof {
    pub gate_name: String,
    pub definition_hash: String,
    tree_hash: String,
    profile: String,
    status: String,
    failure_class: Option<String>,
    exit_code: Option<i64>,
    created_at: i64,
    cleared: bool,
}

fn proof(conn: &rusqlite::Connection, id: i64) -> Result<GateProof, BrokerError> {
    if id <= 0 {
        return Err(invalid("gate run id must be positive"));
    }
    let record = conn
        .query_row(
            "SELECT gate_name, definition_hash, tree_hash, execution_profile_hash,
                status, failure_class, exit_code, created_at, cleared_at
         FROM gate_results WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| invalid("gate run is missing or was reclaimed"))?;
    let profile = record
        .3
        .filter(|value| value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| {
            invalid(
                "gate run lacks a complete execution-profile digest; rerun a profile-bound gate",
            )
        })?;
    Ok(GateProof {
        gate_name: record.0,
        definition_hash: record.1,
        tree_hash: record.2,
        profile,
        status: record.4,
        failure_class: record.5,
        exit_code: record.6,
        created_at: record.7,
        cleared: record.8.is_some(),
    })
}

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<BaseFailureIncident> {
    let text: String = row.get(8)?;
    let test_ids = serde_json::from_str(&text).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(8, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(BaseFailureIncident {
        id: row.get(0)?,
        base_commit: row.get(1)?,
        tree_hash: row.get(2)?,
        gate_name: row.get(3)?,
        definition_hash: row.get(4)?,
        execution_profile_hash: row.get(5)?,
        owner_session_id: row.get(6)?,
        repair_pr: row.get(7)?,
        test_ids,
        confirmed_run_id: row.get(9)?,
        confirmed_run_created_at: row.get(14)?,
        confirmed_at: row.get(10)?,
        resolved_run_id: row.get(11)?,
        resolved_base_commit: row.get(12)?,
        resolved_at: row.get(13)?,
    })
}

fn get(conn: &rusqlite::Connection, id: i64) -> Result<BaseFailureIncident, BrokerError> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM base_failure_incidents WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()?
    .ok_or_else(|| invalid("incident not found"))
}

fn check_oid(value: &str) -> Result<(), BrokerError> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(invalid(
            "base commit and tree must be full object identifiers",
        ))
    }
}

impl super::BrokerStore {
    pub(crate) fn base_incident_gate_proof(&self, id: i64) -> Result<GateProof, BrokerError> {
        proof(&self.conn, id)
    }

    pub(crate) fn base_incidents(&self) -> Result<Vec<BaseFailureIncident>, BrokerError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM base_failure_incidents ORDER BY id DESC LIMIT ?1"
        ))?;
        let records = stmt.query_map(
            [(MAX_OPEN_BASE_FAILURE_INCIDENTS + RETAIN_RESOLVED_BASE_FAILURE_INCIDENTS) as i64],
            from_row,
        )?;
        records.collect::<Result<Vec<_>, _>>().map_err(Into::into)
    }

    pub(crate) fn confirm_base_incident(
        &mut self,
        owner: i64,
        run: i64,
        base: &str,
        tree: &str,
        test_ids: &[String],
        repair_pr: Option<i64>,
    ) -> Result<BaseFailureIncident, BrokerError> {
        check_oid(base)?;
        check_oid(tree)?;
        if repair_pr.is_some_and(|number| number <= 0) {
            return Err(invalid("repair PR must be positive"));
        }
        let ids = normalize_test_ids(test_ids)?;
        let encoded = serde_json::to_string(&ids).map_err(|error| invalid(error.to_string()))?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let active: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1 AND status <> 'cleaned')",
            [owner],
            |row| row.get(0),
        )?;
        if !active {
            return Err(invalid("repair owner must be an open session"));
        }
        let p = proof(&tx, run)?;
        if p.tree_hash != tree
            || p.status != "fail"
            || p.cleared
            || p.exit_code.is_none_or(|code| code == 0)
            || !matches!(
                p.failure_class.as_deref(),
                Some("test_failure" | "build_failure")
            )
        {
            return Err(invalid(
                "confirmation requires an uncleared test/build failure on the exact base tree with nonzero exit",
            ));
        }
        let existing = tx
            .query_row(
                "SELECT id FROM base_failure_incidents WHERE base_commit = ?1 AND gate_name = ?2
                AND definition_hash = ?3 AND execution_profile_hash = ?4 AND resolved_at IS NULL",
                params![base, p.gate_name, p.definition_hash, p.profile],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        if let Some(id) = existing {
            let record = get(&tx, id)?;
            tx.commit()?;
            return Ok(record);
        }
        let open: i64 = tx.query_row(
            "SELECT COUNT(*) FROM base_failure_incidents WHERE resolved_at IS NULL",
            [],
            |row| row.get(0),
        )?;
        if open >= MAX_OPEN_BASE_FAILURE_INCIDENTS as i64 {
            return Err(invalid(
                "open incident capacity reached; resolve existing incidents first",
            ));
        }
        // Keep only bounded resolved metadata. No active incident is discarded,
        // and no source gate or session row is pinned by this advisory registry.
        tx.execute("DELETE FROM base_failure_incidents WHERE resolved_at IS NOT NULL AND id NOT IN (
            SELECT id FROM base_failure_incidents WHERE resolved_at IS NOT NULL ORDER BY id DESC LIMIT ?1)",
            [RETAIN_RESOLVED_BASE_FAILURE_INCIDENTS as i64])?;
        tx.execute(
            "INSERT INTO base_failure_incidents (
            base_commit, tree_hash, gate_name, definition_hash, execution_profile_hash,
            owner_session_id, repair_pr, test_ids_json, confirmed_run_id, confirmed_at, confirmed_run_created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                base,
                tree,
                p.gate_name,
                p.definition_hash,
                p.profile,
                owner,
                repair_pr,
                encoded,
                run,
                super::now_ms(),
                p.created_at
            ],
        )?;
        let record = get(&tx, tx.last_insert_rowid())?;
        tx.commit()?;
        Ok(record)
    }

    pub(crate) fn resolve_base_incident(
        &mut self,
        id: i64,
        run: i64,
        base: &str,
        tree: &str,
    ) -> Result<BaseFailureIncident, BrokerError> {
        check_oid(base)?;
        check_oid(tree)?;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = get(&tx, id)?;
        // Idempotence after gate history reclamation; different proof is refused.
        if record.resolved_at.is_some() {
            if record.resolved_run_id == Some(run)
                && record.resolved_base_commit.as_deref() == Some(base)
            {
                tx.commit()?;
                return Ok(record);
            }
            return Err(invalid("incident already resolved with a different proof"));
        }
        let p = proof(&tx, run)?;
        if p.tree_hash != tree
            || p.status != "pass"
            || p.exit_code != Some(0)
            || p.cleared
            || p.gate_name != record.gate_name
            || p.definition_hash != record.definition_hash
            || p.profile != record.execution_profile_hash
            || run <= record.confirmed_run_id
        {
            return Err(invalid(
                "resolution requires a newer uncleared passing base run with the same gate definition and execution profile",
            ));
        }
        if p.created_at < record.confirmed_run_created_at {
            return Err(invalid("passing proof predates the confirmed failure"));
        }
        let resolved_at = super::now_ms();
        tx.execute(
            "UPDATE base_failure_incidents SET resolved_run_id = ?2, resolved_base_commit = ?3,
            resolved_at = ?4 WHERE id = ?1 AND resolved_at IS NULL",
            params![id, run, base, resolved_at],
        )?;
        // Resolving may grow the terminal set; prune here as well as on insert.
        tx.execute("DELETE FROM base_failure_incidents WHERE resolved_at IS NOT NULL AND id NOT IN (
            SELECT id FROM base_failure_incidents WHERE resolved_at IS NOT NULL ORDER BY id DESC LIMIT ?1)",
            [RETAIN_RESOLVED_BASE_FAILURE_INCIDENTS as i64])?;
        // The just-resolved record can be older than retained history. Return
        // its exact updated snapshot, even if the bounded history discarded it.
        let mut result = record;
        result.resolved_run_id = Some(run);
        result.resolved_base_commit = Some(base.into());
        result.resolved_at = Some(resolved_at);
        tx.commit()?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> super::super::BrokerStore {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::migrate(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO sessions (
            id, worktree_path, branch, origin, status, created_at, updated_at, last_activity_at)
            VALUES (1, '/repo/one', 'agent/one', 'adopted', 'active', 1, 1, 1),
                   (2, '/repo/two', 'agent/two', 'adopted', 'active', 1, 1, 1);",
        )
        .unwrap();
        super::super::BrokerStore {
            conn,
            path: ":memory:".into(),
            _snapshot_dir: None,
            binding_scope: None,
        }
    }

    fn run(
        store: &super::super::BrokerStore,
        status: &str,
        tree: &str,
        profile: Option<&str>,
        class: Option<&str>,
    ) -> i64 {
        store
            .conn
            .execute(
                "INSERT INTO gate_results (
            gate_name, tree_hash, definition_hash, execution_profile_hash, status,
            failure_class, exit_code, created_at)
            VALUES ('unit', ?1, 'definition', ?2, ?3, ?4, ?5, 100)",
                params![
                    tree,
                    profile,
                    status,
                    class,
                    if status == "pass" { 0 } else { 1 }
                ],
            )
            .unwrap();
        store.conn.last_insert_rowid()
    }

    fn confirm(
        store: &mut super::super::BrokerStore,
        owner: i64,
        run: i64,
    ) -> Result<BaseFailureIncident, BrokerError> {
        store.confirm_base_incident(
            owner,
            run,
            &"a".repeat(40),
            &"b".repeat(40),
            &["unit::regression".into()],
            Some(17),
        )
    }

    #[test]
    fn first_incident_owner_and_declared_tests_are_immutable_on_duplicate_confirmation() {
        let mut store = fixture();
        let run = run(
            &store,
            "fail",
            &"b".repeat(40),
            Some(&"c".repeat(64)),
            Some("test_failure"),
        );
        let first = confirm(&mut store, 1, run).unwrap();
        let second = store
            .confirm_base_incident(
                2,
                run,
                &first.base_commit,
                &first.tree_hash,
                &["different_test".into()],
                Some(99),
            )
            .unwrap();
        assert_eq!(second, first);
        assert_eq!(store.base_incidents().unwrap(), vec![first]);
    }

    #[test]
    fn confirmation_refuses_unqualified_or_nonbase_failures_without_recording() {
        let mut store = fixture();
        for (status, tree, profile, class) in [
            (
                "fail",
                "d".repeat(40),
                Some("c".repeat(64)),
                Some("test_failure"),
            ),
            ("pass", "b".repeat(40), Some("c".repeat(64)), None),
            ("fail", "b".repeat(40), None, Some("test_failure")),
            (
                "fail",
                "b".repeat(40),
                Some("c".repeat(64)),
                Some("cached_prior_fail"),
            ),
            (
                "fail",
                "b".repeat(40),
                Some("c".repeat(64)),
                Some("environment"),
            ),
        ] {
            let run = run(&store, status, &tree, profile.as_deref(), class);
            assert!(confirm(&mut store, 1, run).is_err());
        }
        let valid = run(
            &store,
            "fail",
            &"b".repeat(40),
            Some(&"c".repeat(64)),
            Some("test_failure"),
        );
        assert!(confirm(&mut store, 99, valid).is_err());
        assert!(store.base_incidents().unwrap().is_empty());
    }

    #[test]
    fn only_matching_profile_and_current_base_pass_resolves_and_retry_is_idempotent() {
        let mut store = fixture();
        let failed = run(
            &store,
            "fail",
            &"b".repeat(40),
            Some(&"c".repeat(64)),
            Some("test_failure"),
        );
        let incident = confirm(&mut store, 1, failed).unwrap();
        let wrong_profile = run(&store, "pass", &"e".repeat(40), Some(&"d".repeat(64)), None);
        assert!(
            store
                .resolve_base_incident(incident.id, wrong_profile, &"f".repeat(40), &"e".repeat(40))
                .is_err()
        );
        let pass = run(&store, "pass", &"e".repeat(40), Some(&"c".repeat(64)), None);
        assert!(
            store
                .resolve_base_incident(incident.id, pass, &"f".repeat(40), &"b".repeat(40))
                .is_err()
        );
        assert_eq!(store.base_incidents().unwrap()[0], incident);
        let resolved = store
            .resolve_base_incident(incident.id, pass, &"f".repeat(40), &"e".repeat(40))
            .unwrap();
        assert_eq!(resolved.resolved_run_id, Some(pass));
        assert_eq!(
            resolved.resolved_base_commit.as_deref(),
            Some("f".repeat(40).as_str())
        );
        assert_eq!(
            store
                .resolve_base_incident(incident.id, pass, &"f".repeat(40), &"e".repeat(40))
                .unwrap(),
            resolved
        );
        assert!(
            store
                .resolve_base_incident(incident.id, wrong_profile, &"f".repeat(40), &"e".repeat(40))
                .is_err()
        );
    }

    #[test]
    fn active_incident_capacity_is_bounded_and_not_silently_reaped() {
        let mut store = fixture();
        let run = run(
            &store,
            "fail",
            &"b".repeat(40),
            Some(&"c".repeat(64)),
            Some("build_failure"),
        );
        for i in 0..MAX_OPEN_BASE_FAILURE_INCIDENTS {
            store
                .confirm_base_incident(
                    1,
                    run,
                    &format!("{i:040x}"),
                    &"b".repeat(40),
                    &["compile".into()],
                    None,
                )
                .unwrap();
        }
        assert!(
            store
                .confirm_base_incident(
                    2,
                    run,
                    &"f".repeat(40),
                    &"b".repeat(40),
                    &["compile".into()],
                    None
                )
                .is_err()
        );
        assert_eq!(
            store.base_incidents().unwrap().len(),
            MAX_OPEN_BASE_FAILURE_INCIDENTS
        );
        assert!(
            store
                .base_incidents()
                .unwrap()
                .iter()
                .all(|r| r.resolved_at.is_none())
        );
    }

    #[test]
    fn test_identifiers_are_bounded_normalized_and_do_not_accept_control_text() {
        assert!(normalize_test_ids(&[]).is_err());
        assert!(normalize_test_ids(&["bad\nidentifier".into()]).is_err());
        assert!(normalize_test_ids(&[" test".into()]).is_err());
        assert!(normalize_test_ids(&["x".repeat(201)]).is_err());
        assert!(normalize_test_ids(&vec!["x".into(); 65]).is_err());
        assert_eq!(
            normalize_test_ids(&["z".into(), "a".into(), "z".into()]).unwrap(),
            vec!["a", "z"]
        );
    }

    #[test]
    fn resolved_history_is_bounded_while_open_incidents_survive() {
        let mut store = fixture();
        let failed = run(
            &store,
            "fail",
            &"b".repeat(40),
            Some(&"c".repeat(64)),
            Some("test_failure"),
        );
        let pass = run(&store, "pass", &"b".repeat(40), Some(&"c".repeat(64)), None);
        let open = confirm(&mut store, 1, failed).unwrap();
        for i in 0..=RETAIN_RESOLVED_BASE_FAILURE_INCIDENTS {
            let incident = store
                .confirm_base_incident(
                    1,
                    failed,
                    &format!("{i:040x}"),
                    &"b".repeat(40),
                    &["compile".into()],
                    None,
                )
                .unwrap();
            store
                .resolve_base_incident(incident.id, pass, &"f".repeat(40), &"b".repeat(40))
                .unwrap();
        }
        let records = store.base_incidents().unwrap();
        assert_eq!(records.len(), 1 + RETAIN_RESOLVED_BASE_FAILURE_INCIDENTS);
        assert_eq!(records.iter().find(|r| r.id == open.id), Some(&open));
    }

    #[test]
    fn schema_56_repairs_its_advisory_table_without_raising_the_floor() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::schema::migrate(&conn).unwrap();
        let floor: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        conn.execute_batch("DROP TABLE base_failure_incidents")
            .unwrap();
        crate::schema::migrate(&conn).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM base_failure_incidents", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(count, 0);
        let after: String = conn
            .query_row(
                "SELECT value FROM meta WHERE key = 'min_compatible_schema'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(after, floor);
    }
}
