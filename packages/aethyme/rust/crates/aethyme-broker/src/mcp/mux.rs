//! Multiplexing several MCP clients over one upstream server.
//!
//! Pure bookkeeping, no I/O, so every rule is unit-tested:
//! - client request ids are rewritten to hub-unique numbers and mapped back
//!   per (client, original id), keeping the original id's type (number or
//!   string), so two clients may both use id 1;
//! - progress tokens are rewritten the same way and mapped back on
//!   `notifications/progress`;
//! - a client's `notifications/cancelled` is translated to the hub id;
//! - `notifications/initialized` from clients is swallowed (the hub
//!   initialized the upstream once);
//! - server notifications (`*/list_changed`, logging, ...) go to every
//!   client;
//! - server-initiated requests (sampling, roots, elicitation) are routed only
//!   when exactly one client request is in flight; otherwise the upstream gets
//!   a JSON-RPC error. `ping` is answered by the hub;
//! - a disconnected client's requests are cancelled upstream and their late
//!   replies dropped;
//! - when the upstream dies, every in-flight request fails with a retryable
//!   error. Nothing is ever replayed.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Value, json};

use super::jsonrpc::{self, Kind};

pub type ClientId = u64;

/// The id the hub uses for its own `initialize`; client ids start above it.
pub const HUB_INITIALIZE_ID: u64 = 0;

/// Where a message goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Upstream(Value),
    Client(ClientId, Value),
    Broadcast(Value),
}

#[derive(Debug)]
struct Pending {
    client: ClientId,
    original_id: Value,
    progress: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Router {
    next: u64,
    pending: HashMap<u64, Pending>,
    by_client: HashMap<(ClientId, String), u64>,
    progress: HashMap<u64, (ClientId, Value)>,
    server_requests: HashMap<u64, (ClientId, Value)>,
}

/// Ids compare by their JSON text, so `1` and `"1"` stay distinct.
fn id_key(id: &Value) -> String {
    id.to_string()
}

impl Router {
    pub fn new() -> Self {
        Self {
            next: HUB_INITIALIZE_ID + 1,
            ..Self::default()
        }
    }

    fn allocate(&mut self) -> u64 {
        let id = self.next;
        self.next += 1;
        id
    }

    /// Client requests waiting for the upstream.
    pub fn in_flight(&self) -> usize {
        self.pending.len()
    }

    /// A client request bound for the upstream, with id and progress token
    /// rewritten.
    pub fn client_request(&mut self, client: ClientId, mut message: Value) -> Value {
        let original = message.get("id").cloned().unwrap_or(Value::Null);
        let hub_id = self.allocate();
        let mut progress = None;
        if let Some(token) = message.pointer("/params/_meta/progressToken").cloned() {
            let hub_token = self.allocate();
            self.progress.insert(hub_token, (client, token));
            if let Some(slot) = message.pointer_mut("/params/_meta/progressToken") {
                *slot = json!(hub_token);
            }
            progress = Some(hub_token);
        }
        self.by_client.insert((client, id_key(&original)), hub_id);
        self.pending.insert(
            hub_id,
            Pending {
                client,
                original_id: original,
                progress,
            },
        );
        message["id"] = json!(hub_id);
        message
    }

    /// A client notification for the upstream, if it should reach it:
    /// `initialized` is swallowed, `cancelled` translated (or dropped when
    /// the request is unknown).
    pub fn client_notification(&mut self, client: ClientId, mut message: Value) -> Option<Value> {
        match message.get("method").and_then(Value::as_str) {
            Some("notifications/initialized") => None,
            Some("notifications/cancelled") => {
                let request = message.pointer("/params/requestId")?.clone();
                let hub_id = *self.by_client.get(&(client, id_key(&request)))?;
                if let Some(slot) = message.pointer_mut("/params/requestId") {
                    *slot = json!(hub_id);
                }
                Some(message)
            }
            _ => Some(message),
        }
    }

    /// A client's answer to a server-initiated request routed to it.
    pub fn client_response(&mut self, client: ClientId, mut message: Value) -> Option<Value> {
        let id = message.get("id")?.as_u64()?;
        let (owner, _) = self.server_requests.get(&id)?;
        if *owner != client {
            return None;
        }
        let (_, original) = self.server_requests.remove(&id)?;
        message["id"] = original;
        Some(message)
    }

    /// One message from the upstream, routed.
    pub fn upstream_message(&mut self, mut message: Value) -> Vec<Route> {
        match jsonrpc::kind(&message) {
            Kind::Response => {
                let Some(hub_id) = message.get("id").and_then(Value::as_u64) else {
                    return Vec::new();
                };
                // Unknown or late (its client left): dropped.
                let Some(pending) = self.pending.remove(&hub_id) else {
                    return Vec::new();
                };
                self.by_client
                    .remove(&(pending.client, id_key(&pending.original_id)));
                if let Some(token) = pending.progress {
                    self.progress.remove(&token);
                }
                message["id"] = pending.original_id;
                vec![Route::Client(pending.client, message)]
            }
            Kind::Notification => {
                let method = message
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if method == "notifications/progress" {
                    let Some(token) = message
                        .pointer("/params/progressToken")
                        .and_then(Value::as_u64)
                    else {
                        return Vec::new();
                    };
                    let Some((client, original)) = self.progress.get(&token).cloned() else {
                        return Vec::new();
                    };
                    if let Some(slot) = message.pointer_mut("/params/progressToken") {
                        *slot = original;
                    }
                    return vec![Route::Client(client, message)];
                }
                if method == "notifications/cancelled" {
                    // The upstream withdraws one of its own requests.
                    let Some(request) = message.pointer("/params/requestId").cloned() else {
                        return Vec::new();
                    };
                    let found = self
                        .server_requests
                        .iter()
                        .find(|(_, (_, original))| *original == request)
                        .map(|(hub_id, (client, _))| (*hub_id, *client));
                    let Some((hub_id, client)) = found else {
                        return Vec::new();
                    };
                    self.server_requests.remove(&hub_id);
                    if let Some(slot) = message.pointer_mut("/params/requestId") {
                        *slot = json!(hub_id);
                    }
                    return vec![Route::Client(client, message)];
                }
                vec![Route::Broadcast(message)]
            }
            Kind::Request => {
                let original = message.get("id").cloned().unwrap_or(Value::Null);
                if message.get("method").and_then(Value::as_str) == Some("ping") {
                    return vec![Route::Upstream(
                        json!({"jsonrpc": "2.0", "id": original, "result": {}}),
                    )];
                }
                let clients: BTreeSet<ClientId> = self
                    .pending
                    .values()
                    .map(|pending| pending.client)
                    .collect();
                if self.pending.len() == 1
                    && let Some(client) = clients.into_iter().next()
                {
                    let hub_id = self.allocate();
                    self.server_requests.insert(hub_id, (client, original));
                    message["id"] = json!(hub_id);
                    return vec![Route::Client(client, message)];
                }
                vec![Route::Upstream(jsonrpc::error_response(
                    original,
                    -32601,
                    "a shared aethyme mcp hub routes server requests only while exactly one \
                     client request is in flight",
                    None,
                ))]
            }
            Kind::Invalid => Vec::new(),
        }
    }

    /// A client went away: its requests are cancelled upstream, its progress
    /// tokens forgotten, and server requests routed to it refused.
    pub fn client_gone(&mut self, client: ClientId) -> Vec<Value> {
        let mut upstream = Vec::new();
        let mut ids: Vec<u64> = self
            .pending
            .iter()
            .filter(|(_, pending)| pending.client == client)
            .map(|(id, _)| *id)
            .collect();
        ids.sort_unstable();
        for id in ids {
            if let Some(pending) = self.pending.remove(&id) {
                self.by_client
                    .remove(&(client, id_key(&pending.original_id)));
                upstream.push(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": id, "reason": "client disconnected"},
                }));
            }
        }
        self.progress.retain(|_, (owner, _)| *owner != client);
        let mut server_ids: Vec<u64> = self
            .server_requests
            .iter()
            .filter(|(_, (owner, _))| *owner == client)
            .map(|(id, _)| *id)
            .collect();
        server_ids.sort_unstable();
        for id in server_ids {
            if let Some((_, original)) = self.server_requests.remove(&id) {
                upstream.push(jsonrpc::error_response(
                    original,
                    -32603,
                    "the client this request was routed to disconnected",
                    None,
                ));
            }
        }
        upstream
    }

    /// The upstream died: every in-flight request fails with a retryable
    /// error, and all mappings are cleared.
    pub fn fail_all(&mut self, reason: &str) -> Vec<(ClientId, Value)> {
        let pending = std::mem::take(&mut self.pending);
        self.by_client.clear();
        self.progress.clear();
        self.server_requests.clear();
        let mut failed: Vec<(u64, ClientId, Value)> = pending
            .into_iter()
            .map(|(id, pending)| {
                (
                    id,
                    pending.client,
                    jsonrpc::retryable_error(pending.original_id, reason),
                )
            })
            .collect();
        failed.sort_by_key(|(id, _, _)| *id);
        failed
            .into_iter()
            .map(|(_, client, error)| (client, error))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: Value, method: &str) -> Value {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": {}})
    }

    #[test]
    fn colliding_ids_from_two_clients_map_back_with_their_types() {
        let mut router = Router::new();
        let first = router.client_request(1, request(json!(1), "tools/list"));
        let second = router.client_request(2, request(json!(1), "tools/list"));
        let third = router.client_request(2, request(json!("1"), "tools/list"));
        let ids: Vec<u64> = [&first, &second, &third]
            .iter()
            .map(|message| message["id"].as_u64().unwrap())
            .collect();
        assert_eq!(ids.len(), 3);
        assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
        assert!(ids.iter().all(|id| *id != HUB_INITIALIZE_ID));
        let answer = |id: u64| json!({"jsonrpc": "2.0", "id": id, "result": {"n": id}});
        assert_eq!(
            router.upstream_message(answer(ids[2])),
            vec![Route::Client(
                2,
                json!({"jsonrpc": "2.0", "id": "1", "result": {"n": ids[2]}})
            )]
        );
        assert_eq!(
            router.upstream_message(answer(ids[0])),
            vec![Route::Client(
                1,
                json!({"jsonrpc": "2.0", "id": 1, "result": {"n": ids[0]}})
            )]
        );
        assert_eq!(router.in_flight(), 1);
        // A reply nobody waits for is dropped.
        assert!(router.upstream_message(answer(ids[0])).is_empty());
    }

    #[test]
    fn progress_tokens_are_rewritten_and_mapped_back() {
        let mut router = Router::new();
        let sent = router.client_request(
            7,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "x", "_meta": {"progressToken": "abc"}}}),
        );
        let token = sent["params"]["_meta"]["progressToken"].as_u64().unwrap();
        let routed = router.upstream_message(json!({
            "jsonrpc": "2.0", "method": "notifications/progress",
            "params": {"progressToken": token, "progress": 1}
        }));
        assert_eq!(
            routed,
            vec![Route::Client(
                7,
                json!({"jsonrpc": "2.0", "method": "notifications/progress",
                       "params": {"progressToken": "abc", "progress": 1}})
            )]
        );
    }

    #[test]
    fn cancellation_is_translated_and_initialized_swallowed() {
        let mut router = Router::new();
        let sent = router.client_request(4, request(json!("req-9"), "tools/call"));
        let hub_id = sent["id"].clone();
        let cancel = router
            .client_notification(
                4,
                json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                       "params": {"requestId": "req-9", "reason": "user"}}),
            )
            .unwrap();
        assert_eq!(cancel["params"]["requestId"], hub_id);
        // Another client cannot cancel it.
        assert!(
            router
                .client_notification(
                    5,
                    json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                           "params": {"requestId": "req-9"}}),
                )
                .is_none()
        );
        assert!(
            router
                .client_notification(
                    4,
                    json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
                )
                .is_none()
        );
    }

    #[test]
    fn server_notifications_fan_out_and_server_requests_need_one_request_in_flight() {
        let mut router = Router::new();
        let changed = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        assert_eq!(
            router.upstream_message(changed.clone()),
            vec![Route::Broadcast(changed)]
        );
        let sampling =
            json!({"jsonrpc": "2.0", "id": 900, "method": "sampling/createMessage", "params": {}});
        // Nothing in flight: refused.
        let refused = router.upstream_message(sampling.clone());
        assert!(
            matches!(&refused[0], Route::Upstream(message) if message["error"]["code"] == -32601)
        );
        // Exactly one request in flight: routed to that client and answered back.
        router.client_request(3, request(json!(1), "tools/call"));
        let routed = router.upstream_message(sampling.clone());
        let Route::Client(3, forwarded) = &routed[0] else {
            panic!("{routed:?}");
        };
        let hub_id = forwarded["id"].as_u64().unwrap();
        let answered = router
            .client_response(
                3,
                json!({"jsonrpc": "2.0", "id": hub_id, "result": {"ok": true}}),
            )
            .unwrap();
        assert_eq!(answered["id"], 900);
        // Two in flight: refused again.
        router.client_request(4, request(json!(1), "tools/call"));
        let refused = router.upstream_message(sampling);
        assert!(matches!(&refused[0], Route::Upstream(message) if message.get("error").is_some()));
        // Ping is the hub's to answer.
        let ping = router.upstream_message(json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}));
        assert_eq!(
            ping,
            vec![Route::Upstream(
                json!({"jsonrpc": "2.0", "id": 5, "result": {}})
            )]
        );
    }

    #[test]
    fn a_gone_client_is_cancelled_upstream_and_its_late_replies_dropped() {
        let mut router = Router::new();
        let sent = router.client_request(1, request(json!(1), "tools/call"));
        let kept = router.client_request(2, request(json!(1), "tools/call"));
        let cancellations = router.client_gone(1);
        assert_eq!(cancellations.len(), 1);
        assert_eq!(cancellations[0]["method"], "notifications/cancelled");
        assert_eq!(cancellations[0]["params"]["requestId"], sent["id"]);
        assert!(
            router
                .upstream_message(json!({"jsonrpc": "2.0", "id": sent["id"], "result": {}}))
                .is_empty()
        );
        assert_eq!(
            router.upstream_message(json!({"jsonrpc": "2.0", "id": kept["id"], "result": {}})),
            vec![Route::Client(
                2,
                json!({"jsonrpc": "2.0", "id": 1, "result": {}})
            )]
        );
    }

    #[test]
    fn an_upstream_crash_fails_every_request_retryably() {
        let mut router = Router::new();
        router.client_request(1, request(json!("a"), "tools/call"));
        router.client_request(2, request(json!(2), "tools/call"));
        let failed = router.fail_all("the upstream server exited; retry");
        assert_eq!(failed.len(), 2);
        assert_eq!(failed[0].0, 1);
        assert_eq!(failed[0].1["id"], "a");
        assert_eq!(failed[0].1["error"]["code"], jsonrpc::RETRYABLE);
        assert_eq!(failed[1].1["error"]["data"]["retryable"], true);
        assert_eq!(router.in_flight(), 0);
    }
}
