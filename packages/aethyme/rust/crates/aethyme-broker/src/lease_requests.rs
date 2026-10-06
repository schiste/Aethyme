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
//! names it. Nothing here changes the broker schema.

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
}

impl RequestState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Acked => "acked",
            Self::Declined => "declined",
            Self::Granted => "granted",
            Self::Released => "released",
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
    /// Why it was declined, or `holder_stale` for a grant.
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

/// Every release request in the event log, oldest first, with its state.
/// `holds(holder, path)` says whether the holder still holds the path, which
/// turns a request whose lease went away by other means into `released`.
pub fn requests(
    store: &BrokerStore,
    holds: impl Fn(i64, &str) -> bool,
) -> Result<Vec<LeaseReleaseRequest>, crate::BrokerOpError> {
    let events = store.events_after_filtered(0, i64::MAX, Some("lease.release_"))?;
    let mut requests: Vec<LeaseReleaseRequest> = Vec::new();
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
        if let Some(request) = requests
            .iter_mut()
            .find(|request| request.request_id == outcome.request_id)
            && request.state == RequestState::Pending
        {
            request.state = state;
            request.resolved_at = Some(event.ts);
            request.resolution_reason = outcome.reason;
        }
    }
    for request in &mut requests {
        if request.state == RequestState::Pending
            && !holds(request.holder_session_id, &request.path)
        {
            request.state = RequestState::Released;
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
    fn a_pending_request_whose_holder_let_go_reads_released() {
        let (_dir, mut store) = store();
        let id = request(&mut store, "a.rs");
        let all = requests(&store, |_, _| false).unwrap();
        assert_eq!(all[0].request_id, id);
        assert_eq!(all[0].state, RequestState::Released);
    }
}
