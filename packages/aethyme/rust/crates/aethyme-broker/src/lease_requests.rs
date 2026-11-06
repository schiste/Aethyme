//! Asking another session to release a lease (#359).
//!
//! A waiting agent used to poll lease state and send free-form notes to learn
//! whether a path was free. A release request records who wants which path
//! from whom and why; the holder acknowledges (releasing the lease) or
//! declines with a reason; and if the holder's process is gone past the
//! stale grace (#360), the request is granted for it. A holder whose liveness
//! is unknown is never granted against.
//!
//! Requests live in the event log, not a table of their own: a request is its
//! `lease.release_requested` event, and its outcome is the later event that
//! names it. A request also resolves when its requester closes or its holder
//! releases the path through another lifecycle operation. Nothing here changes
//! the broker schema.

use std::collections::HashMap;

use crate::BrokerStore;

pub const REQUESTED: &str = crate::events::LEASE_RELEASE_REQUESTED;
pub const ACKED: &str = crate::events::LEASE_RELEASE_ACKED;
pub const DECLINED: &str = crate::events::LEASE_RELEASE_DECLINED;
pub const GRANTED: &str = crate::events::LEASE_RELEASE_GRANTED;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestState {
    Pending,
    /// The holder acknowledged and released the lease.
    Acked,
    Declined,
    /// The holder was gone past the stale grace; released for it.
    Granted,
    /// The holder no longer holds the path for some other reason (finish,
    /// an explicit release, expiry).
    Released,
    /// The requester session closed while the holder still held the path.
    Cancelled,
}

impl RequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acked => "acked",
            Self::Declined => "declined",
            Self::Granted => "granted",
            Self::Released => "released",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LeaseReleaseRequest {
    pub request_id: i64,
    pub path: String,
    pub requester_session_id: i64,
    pub holder_session_id: i64,
    pub reason: String,
    pub requested_at: i64,
    pub state: RequestState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<i64>,
    /// Why it was declined, granted, released, or cancelled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolution_reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct RequestedPayload {
    path: String,
    requester_session_id: i64,
    holder_session_id: i64,
    reason: String,
}

#[derive(serde::Deserialize)]
struct OutcomePayload {
    request_id: i64,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(serde::Deserialize)]
struct ReleasedPayload {
    path: String,
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Debug, Clone)]
struct RequestResolution {
    event_id: i64,
    resolved_at: i64,
    state: RequestState,
    reason: Option<String>,
}

// Event IDs are the durable event-log order. Timestamps can be backdated to
// a lease's expires_at, so they must not decide which terminal outcome wins.
fn record_resolution(
    resolutions: &mut HashMap<i64, RequestResolution>,
    request_id: i64,
    candidate: RequestResolution,
) {
    let is_earlier = resolutions
        .get(&request_id)
        .map(|current| candidate.event_id < current.event_id)
        .unwrap_or(true);
    if is_earlier {
        resolutions.insert(request_id, candidate);
    }
}

/// Every release request in the event log, oldest first, with its state.
/// `holds(holder, path)` supplies a current-state fallback when no
/// auditable release event remains for an unresolved request.
pub fn requests(
    store: &BrokerStore,
    holds: impl Fn(i64, &str) -> bool,
) -> Result<Vec<LeaseReleaseRequest>, crate::BrokerOpError> {
    let events = store.lease_release_events()?;
    let mut requests: Vec<LeaseReleaseRequest> = Vec::new();
    let mut outcome_resolutions = HashMap::<i64, RequestResolution>::new();
    for event in &events {
        let Some(payload) = event.payload_json.as_deref() else {
            continue;
        };
        if event.kind == REQUESTED {
            let Ok(requested) = serde_json::from_str::<RequestedPayload>(payload) else {
                continue;
            };
            requests.push(LeaseReleaseRequest {
                request_id: event.id,
                path: requested.path,
                requester_session_id: requested.requester_session_id,
                holder_session_id: requested.holder_session_id,
                reason: requested.reason,
                requested_at: event.ts,
                state: RequestState::Pending,
                resolved_at: None,
                resolution_reason: None,
            });
            continue;
        }
        let state = match event.kind.as_str() {
            ACKED => RequestState::Acked,
            DECLINED => RequestState::Declined,
            GRANTED => RequestState::Granted,
            _ => continue,
        };
        let Ok(outcome) = serde_json::from_str::<OutcomePayload>(payload) else {
            continue;
        };
        if requests
            .iter()
            .any(|request| request.request_id == outcome.request_id)
        {
            record_resolution(
                &mut outcome_resolutions,
                outcome.request_id,
                RequestResolution {
                    event_id: event.id,
                    resolved_at: event.ts,
                    state,
                    reason: outcome.reason,
                },
            );
        }
    }

    let mut resolutions = outcome_resolutions.clone();
    let mut held_by_request = HashMap::<i64, bool>::new();
    let mut release_resolutions = HashMap::<i64, RequestResolution>::new();
    let mut release_scan_before = HashMap::<i64, Option<(i64, i64)>>::new();
    let mut request_indexes_by_holder = HashMap::<i64, Vec<usize>>::new();
    let mut request_indexes_by_requester = HashMap::<i64, Vec<usize>>::new();
    for (index, request) in requests.iter().enumerate() {
        request_indexes_by_holder
            .entry(request.holder_session_id)
            .or_default()
            .push(index);
        request_indexes_by_requester
            .entry(request.requester_session_id)
            .or_default()
            .push(index);
        held_by_request.insert(
            request.request_id,
            holds(request.holder_session_id, &request.path),
        );
        let outcome_boundary = outcome_resolutions
            .get(&request.request_id)
            .map(|outcome| (outcome.event_id, outcome.resolved_at));
        match release_scan_before.entry(request.holder_session_id) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(outcome_boundary);
            }
            std::collections::hash_map::Entry::Occupied(mut entry) => {
                let before = match (*entry.get(), outcome_boundary) {
                    (Some(current), Some(next)) => {
                        Some((current.0.max(next.0), current.1.max(next.1)))
                    }
                    (None, _) | (_, None) => None,
                };
                entry.insert(before);
            }
        }
    }

    // Read each history once rather than once per session. A request has one
    // holder and one requester, so it still sees its events in event-id order.
    let first_request_id = requests.iter().map(|request| request.request_id).min();
    if let Some(first_request_id) = first_request_id {
        for event in store.lease_release_events_since(first_request_id)? {
            let Some(indexes) = event
                .session_id
                .and_then(|holder| request_indexes_by_holder.get(&holder))
            else {
                continue;
            };
            let before = event
                .session_id
                .and_then(|holder| release_scan_before.get(&holder).copied().flatten());
            if before.is_some_and(|(before_event_id, before_ts)| {
                event.id >= before_event_id && event.ts > before_ts
            }) {
                continue;
            }
            let Some(payload) = event.payload_json.as_deref() else {
                continue;
            };
            let Ok(released) = serde_json::from_str::<ReleasedPayload>(payload) else {
                continue;
            };
            for &index in indexes {
                let request = &requests[index];
                if request.request_id >= event.id
                    || !crate::leases::paths_overlap(&request.path, &released.path)
                {
                    continue;
                }
                let explicit_outcome = outcome_resolutions.get(&request.request_id);
                // Paired acknowledgement and grant releases are recorded
                // before their outcome event in the same operation.
                let paired_outcome = explicit_outcome.is_some_and(|outcome| {
                    outcome.event_id > event.id
                        && ((outcome.state == RequestState::Acked
                            && released.reason.as_deref() == Some("request_acked"))
                            || (outcome.state == RequestState::Granted
                                && released.reason.as_deref() == Some("request_granted")))
                });
                if paired_outcome {
                    continue;
                }
                // A request can cover several leases. Keep the final
                // overlapping release as its resolution only when the
                // holder no longer has any active lease for the requested
                // path; an earlier partial release must leave it pending.
                if held_by_request.get(&request.request_id) != Some(&false) {
                    continue;
                }
                let candidate = RequestResolution {
                    event_id: event.id,
                    resolved_at: event.ts,
                    state: RequestState::Released,
                    reason: Some(
                        released
                            .reason
                            .clone()
                            .unwrap_or_else(|| "lease_released".to_string()),
                    ),
                };
                match release_resolutions.entry(request.request_id) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(candidate);
                    }
                    std::collections::hash_map::Entry::Occupied(mut entry)
                        if candidate.event_id > entry.get().event_id =>
                    {
                        entry.insert(candidate);
                    }
                    std::collections::hash_map::Entry::Occupied(_) => {}
                }
            }
        }
    }

    for (request_id, resolution) in release_resolutions {
        record_resolution(&mut resolutions, request_id, resolution);
    }

    if let Some(first_request_id) = first_request_id {
        for event in store.lease_request_closure_events_since(first_request_id)? {
            let Some(indexes) = event
                .session_id
                .and_then(|requester| request_indexes_by_requester.get(&requester))
            else {
                continue;
            };
            for &index in indexes {
                let request = &requests[index];
                if request.request_id < event.id {
                    record_resolution(
                        &mut resolutions,
                        request.request_id,
                        RequestResolution {
                            event_id: event.id,
                            resolved_at: event.ts,
                            state: RequestState::Cancelled,
                            reason: Some("requester_closed".into()),
                        },
                    );
                }
            }
        }
    }

    for request in &mut requests {
        if let Some(resolution) = resolutions.remove(&request.request_id) {
            request.state = resolution.state;
            request.resolved_at = Some(resolution.resolved_at);
            request.resolution_reason = resolution.reason;
        }
    }

    let mut requester_closed_at = HashMap::<i64, Option<i64>>::new();
    for request in &mut requests {
        if request.state != RequestState::Pending {
            continue;
        }
        if held_by_request.get(&request.request_id) == Some(&false) {
            request.state = RequestState::Released;
            request.resolution_reason = Some("release_evidence_unavailable".into());
            continue;
        }
        if let std::collections::hash_map::Entry::Vacant(entry) =
            requester_closed_at.entry(request.requester_session_id)
        {
            let closed_at = match store.session(request.requester_session_id) {
                Ok(session) if session.status.is_closed() => {
                    Some(session.closed_at.unwrap_or(session.updated_at))
                }
                Ok(_) | Err(crate::BrokerError::SessionNotFound(_)) => None,
                Err(error) => return Err(error.into()),
            };
            entry.insert(closed_at);
        }
        if let Some(Some(closed_at)) = requester_closed_at.get(&request.requester_session_id)
            && *closed_at >= request.requested_at
        {
            request.state = RequestState::Cancelled;
            request.resolved_at = Some(*closed_at);
            request.resolution_reason = Some("requester_closed".into());
        }
    }
    Ok(requests)
}

pub fn request_payload(path: &str, requester: i64, holder: i64, reason: &str) -> String {
    serde_json::json!({
        "path": path,
        "requester_session_id": requester,
        "holder_session_id": holder,
        "reason": reason,
    })
    .to_string()
}

pub fn outcome_payload(request_id: i64, path: &str, reason: Option<&str>) -> String {
    serde_json::json!({
        "request_id": request_id,
        "path": path,
        "reason": reason,
    })
    .to_string()
}

/// `aethyme broker advanced leases ack|decline <id> --session <id>`, quoted.
pub fn holder_commands(request: &LeaseReleaseRequest) -> Vec<String> {
    vec![
        format!(
            "aethyme broker advanced leases ack {} --session {}",
            request.request_id, request.holder_session_id
        ),
        format!(
            "aethyme broker advanced leases decline {} --session {} --reason {}",
            request.request_id,
            request.holder_session_id,
            crate::broker::shell_quote("<why>")
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BrokerStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = BrokerStore::open(&dir.path().join("broker.db")).unwrap();
        (dir, store)
    }

    fn request(store: &mut BrokerStore, path: &str) -> i64 {
        store
            .append_event(
                REQUESTED,
                Some(2),
                Some(&request_payload(path, 2, 1, "need it")),
            )
            .unwrap()
    }

    #[test]
    fn a_request_follows_its_first_outcome_event() {
        let (_dir, mut store) = store();
        let declined = request(&mut store, "a.rs");
        let acked = request(&mut store, "b.rs");
        let pending = request(&mut store, "c.rs");
        store
            .append_event(
                DECLINED,
                Some(1),
                Some(&outcome_payload(declined, "a.rs", Some("busy"))),
            )
            .unwrap();
        store
            .append_event(ACKED, Some(1), Some(&outcome_payload(acked, "b.rs", None)))
            .unwrap();
        // A later outcome never overrides the first.
        store
            .append_event(
                GRANTED,
                Some(1),
                Some(&outcome_payload(declined, "a.rs", None)),
            )
            .unwrap();
        let all = requests(&store, |_, _| true).unwrap();
        let state = |id: i64| all.iter().find(|r| r.request_id == id).unwrap().state;
        assert_eq!(state(declined), RequestState::Declined);
        assert_eq!(state(acked), RequestState::Acked);
        assert_eq!(state(pending), RequestState::Pending);
        let reason = all
            .iter()
            .find(|r| r.request_id == declined)
            .unwrap()
            .resolution_reason
            .clone();
        assert_eq!(reason.as_deref(), Some("busy"));
    }

    #[test]
    fn later_backdated_expiry_cannot_replace_acknowledgement() {
        let (_dir, mut store) = store();
        let id = request(&mut store, "draft.rs");
        let ack = store
            .append_event(ACKED, Some(1), Some(&outcome_payload(id, "draft.rs", None)))
            .unwrap();
        store.set_event_timestamp_for_test(ack, 200);
        let expired = store
            .append_event(
                crate::events::LEASE_RELEASED,
                Some(1),
                Some(r#"{"path":"draft.rs","reason":"expired"}"#),
            )
            .unwrap();
        store.set_event_timestamp_for_test(expired, 100);
        assert!(expired > ack, "expiry is appended after acknowledgement");

        let all = requests(&store, |_, _| false).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, RequestState::Acked);
        assert_eq!(all[0].resolved_at, Some(200));
    }

    #[test]
    fn final_overlapping_release_uses_append_order_despite_backdated_expiry() {
        let (_dir, mut store) = store();
        request(&mut store, "draft.rs");
        let first = store
            .append_event(
                crate::events::LEASE_RELEASED,
                Some(1),
                Some(r#"{"path":"draft.rs","reason":"earlier release"}"#),
            )
            .unwrap();
        store.set_event_timestamp_for_test(first, 200);
        let last = store
            .append_event(
                crate::events::LEASE_RELEASED,
                Some(1),
                Some(r#"{"path":"draft.rs","reason":"later expiry"}"#),
            )
            .unwrap();
        store.set_event_timestamp_for_test(last, 100);
        assert!(last > first, "expiry is the final durable release");

        let all = requests(&store, |_, _| false).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].state, RequestState::Released);
        assert_eq!(all[0].resolved_at, Some(100));
        assert_eq!(all[0].resolution_reason.as_deref(), Some("later expiry"));
    }

    #[test]
    fn release_request_scan_keeps_known_events_in_order_and_ignores_unknown_kinds() {
        let (_dir, mut store) = store();
        let first = request(&mut store, "a.rs");
        store
            .append_event(
                "lease.release_future",
                Some(1),
                Some(&outcome_payload(first, "a.rs", Some("ignored"))),
            )
            .unwrap();
        store
            .append_event("note.created", Some(2), Some("{}"))
            .unwrap();
        let acked = store
            .append_event(ACKED, Some(1), Some(&outcome_payload(first, "a.rs", None)))
            .unwrap();
        let malformed = store
            .append_event(REQUESTED, Some(2), Some("not-json"))
            .unwrap();
        let second = request(&mut store, "b.rs");

        let events = store.lease_release_events().unwrap();
        assert_eq!(
            events.iter().map(|event| event.id).collect::<Vec<_>>(),
            vec![first, acked, malformed, second]
        );
        assert_eq!(
            events
                .iter()
                .map(|event| event.kind.as_str())
                .collect::<Vec<_>>(),
            vec![REQUESTED, ACKED, REQUESTED, REQUESTED]
        );
        assert!(
            store
                .events_after_filtered(0, i64::MAX, Some("lease.release_"))
                .unwrap()
                .iter()
                .any(|event| event.kind == "lease.release_future")
        );

        let requests = requests(&store, |_, _| true).unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|request| request.request_id)
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        assert_eq!(requests[0].state, RequestState::Acked);
        assert_eq!(requests[1].state, RequestState::Pending);
    }

    #[test]
    fn a_pending_request_whose_holder_let_go_reads_released() {
        let (_dir, mut store) = store();
        let id = request(&mut store, "a.rs");
        let all = requests(&store, |_, _| false).unwrap();
        assert_eq!(all[0].request_id, id);
        assert_eq!(all[0].state, RequestState::Released);
    }

    fn request_between(store: &mut BrokerStore, path: &str, requester: i64, holder: i64) -> i64 {
        store
            .append_event(
                REQUESTED,
                Some(requester),
                Some(&request_payload(path, requester, holder, "need it")),
            )
            .unwrap()
    }

    fn release(store: &mut BrokerStore, holder: i64, path: &str) -> i64 {
        store
            .append_event(
                crate::events::LEASE_RELEASED,
                Some(holder),
                Some(&serde_json::json!({"path": path, "reason": "done"}).to_string()),
            )
            .unwrap()
    }

    fn store_event_ts(store: &BrokerStore, id: i64) -> i64 {
        store
            .connection()
            .query_row("SELECT ts FROM events WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .unwrap()
    }

    #[test]
    fn a_release_resolves_only_its_own_holders_later_requests() {
        let (_dir, mut store) = store();
        let before_any_request = release(&mut store, 10, "a.rs");
        let early = request_between(&mut store, "a.rs", 1, 10);
        let other_holder = request_between(&mut store, "a.rs", 1, 20);
        let other_path = request_between(&mut store, "b.rs", 1, 10);
        let released = release(&mut store, 10, "a.rs");
        let after_release = request_between(&mut store, "a.rs", 1, 10);
        // Latest, so it would win if a release could reach another holder's
        // requests, whichever holder's requests it reached.
        let stranger = store
            .append_event(
                crate::events::LEASE_RELEASED,
                Some(30),
                Some(&serde_json::json!({"path": "a.rs", "reason": "stranger"}).to_string()),
            )
            .unwrap();
        assert!(before_any_request < early && early < released && released < after_release);
        assert!(after_release < stranger);

        let all = requests(&store, |_, _| false).unwrap();
        let by_id = |id: i64| all.iter().find(|r| r.request_id == id).unwrap();
        assert_eq!(by_id(early).state, RequestState::Released);
        assert_eq!(by_id(early).resolution_reason.as_deref(), Some("done"));
        // Each of these has no matching release, so it falls through to the
        // holder-let-go evidence rather than borrowing another request's.
        for id in [other_holder, other_path, after_release] {
            assert_eq!(by_id(id).state, RequestState::Released);
            assert_eq!(
                by_id(id).resolution_reason.as_deref(),
                Some("release_evidence_unavailable")
            );
        }
    }

    #[test]
    fn a_release_after_the_outcome_cannot_displace_the_release_before_it() {
        let (_dir, mut store) = store();
        let id = request_between(&mut store, "a.rs", 1, 10);
        let released = release(&mut store, 10, "a.rs");
        let outcome = store
            .append_event(
                DECLINED,
                Some(10),
                Some(&outcome_payload(id, "a.rs", Some("busy"))),
            )
            .unwrap();
        let later = release(&mut store, 10, "a.rs");
        assert!(id < released && released < outcome && outcome < later);
        // Dated after the outcome, so it sits outside the holder's window.
        store
            .connection()
            .execute(
                "UPDATE events SET ts = ts + 1000 WHERE id = ?1",
                rusqlite::params![later],
            )
            .unwrap();

        let all = requests(&store, |_, _| false).unwrap();
        assert_eq!(all[0].state, RequestState::Released);
        assert_eq!(all[0].resolved_at, Some(store_event_ts(&store, released)));
    }

    #[test]
    fn a_closure_cancels_only_its_own_requesters_earlier_requests() {
        let (_dir, mut store) = store();
        let earlier_closure = store
            .append_event("session.closed", Some(1), Some("{}"))
            .unwrap();
        let cancelled = request_between(&mut store, "a.rs", 1, 10);
        let other_requester = request_between(&mut store, "b.rs", 2, 10);
        let closure = store
            .append_event("session.closed", Some(1), Some("{}"))
            .unwrap();
        let after_closure = request_between(&mut store, "c.rs", 1, 10);
        store
            .append_event("session.closed", Some(3), Some("{}"))
            .unwrap();
        assert!(earlier_closure < cancelled && cancelled < closure && closure < after_closure);

        let all = requests(&store, |_, _| true).unwrap();
        let by_id = |id: i64| all.iter().find(|r| r.request_id == id).unwrap();
        assert_eq!(by_id(cancelled).state, RequestState::Cancelled);
        assert_eq!(
            by_id(cancelled).resolution_reason.as_deref(),
            Some("requester_closed")
        );
        assert_eq!(by_id(other_requester).state, RequestState::Pending);
        assert_eq!(by_id(after_closure).state, RequestState::Pending);
    }
}
