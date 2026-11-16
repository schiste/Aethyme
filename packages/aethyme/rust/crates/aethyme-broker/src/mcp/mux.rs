//! Multiplexing several MCP clients over one upstream server.
//!
//! Pure bookkeeping, no I/O, so every rule is unit-tested:
//! - client request ids are rewritten to hub-unique numbers and mapped back
//!   per (client, original id), keeping the original id's type (number or
//!   string), so two clients may both use id 1; a client reusing an id that
//!   is still in flight gets an error for the second request;
//! - progress tokens are rewritten the same way and mapped back on
//!   `notifications/progress`;
//! - a client's `notifications/cancelled` is translated to the hub id;
//!   `notifications/initialized` and `notifications/roots/list_changed` from
//!   clients are swallowed (the hub initialized the upstream itself, without
//!   client capabilities);
//! - per-session state never crosses clients: `logging/setLevel` is answered
//!   by the hub and never forwarded, upstream `notifications/message` is
//!   dropped (a log line may carry another client's arguments), and resource
//!   subscriptions are reference-counted: the upstream is subscribed on the
//!   first client's `resources/subscribe` and unsubscribed on the last
//!   client's `resources/unsubscribe` (or disconnect), and
//!   `notifications/resources/updated` reaches only that URI's subscribers;
//! - other server notifications (`*/list_changed`, ...) reach every client;
//! - server-initiated requests other than `ping` (sampling, roots,
//!   elicitation, ...) are refused with method-not-found: the hub declared
//!   no client capabilities, and no request is ever routed to a client that
//!   did not send it;
//! - a disconnected client's requests are cancelled upstream and their late
//!   replies dropped;
//! - when the upstream dies, every in-flight request fails with a retryable
//!   error. Nothing is ever replayed.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{Value, json};

use super::jsonrpc::{self, Kind};

pub type ClientId = u64;

/// The id the hub uses for its own `initialize`; other ids are above it.
pub const HUB_INITIALIZE_ID: u64 = 0;

/// JSON-RPC "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC "invalid request".
pub const INVALID_REQUEST: i64 = -32600;

/// Where a message goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Upstream(Value),
    Client(ClientId, Value),
    Broadcast(Value),
}

/// What to do with a client request.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Forward this (rewritten) request to the upstream.
    Upstream(Value),
    /// Answer the client with this, without involving the upstream.
    Reply(Value),
}

#[derive(Debug)]
struct Pending {
    client: ClientId,
    original_id: Value,
    progress: Option<u64>,
}

/// Counts of messages the router refused or dropped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Dropped {
    pub server_requests: u64,
    pub log_messages: u64,
}

#[derive(Debug, Default)]
pub struct Router {
    next: u64,
    pending: HashMap<u64, Pending>,
    by_client: HashMap<(ClientId, String), u64>,
    per_client: HashMap<ClientId, usize>,
    progress: HashMap<u64, (ClientId, Value)>,
    /// URI -> subscribed clients.
    subscriptions: BTreeMap<String, BTreeSet<ClientId>>,
    /// Client -> requested log level (answered locally, never forwarded).
    log_levels: HashMap<ClientId, Value>,
    pub dropped: Dropped,
}

/// Ids compare by their JSON text, so `1` and `"1"` stay distinct.
fn id_key(id: &Value) -> String {
    id.to_string()
}

fn success(id: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {}})
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

    /// This client's requests waiting for the upstream.
    pub fn client_in_flight(&self, client: ClientId) -> usize {
        self.per_client.get(&client).copied().unwrap_or(0)
    }

    /// The log level a client asked for, if any.
    pub fn log_level(&self, client: ClientId) -> Option<&Value> {
        self.log_levels.get(&client)
    }

    /// Clients subscribed to `uri`.
    pub fn subscribers(&self, uri: &str) -> Vec<ClientId> {
        self.subscriptions
            .get(uri)
            .map(|clients| clients.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Track a request bound for the upstream and rewrite its id and token.
    fn forward(&mut self, client: ClientId, mut message: Value) -> Value {
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
        *self.per_client.entry(client).or_default() += 1;
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

    /// A client request: forwarded (rewritten) or answered by the hub.
    pub fn client_request(&mut self, client: ClientId, message: Value) -> Action {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        if self.by_client.contains_key(&(client, id_key(&id))) {
            return Action::Reply(jsonrpc::error_response(
                id,
                INVALID_REQUEST,
                "this id is already used by a request still in flight",
                None,
            ));
        }
        let uri = message
            .pointer("/params/uri")
            .and_then(Value::as_str)
            .map(str::to_string);
        match message.get("method").and_then(Value::as_str) {
            Some("logging/setLevel") => {
                let level = message
                    .pointer("/params/level")
                    .cloned()
                    .unwrap_or(Value::Null);
                self.log_levels.insert(client, level);
                Action::Reply(success(id))
            }
            Some("resources/subscribe") => {
                let Some(uri) = uri else {
                    return Action::Upstream(self.forward(client, message));
                };
                let subscribers = self.subscriptions.entry(uri).or_default();
                let first = subscribers.is_empty();
                subscribers.insert(client);
                if first {
                    Action::Upstream(self.forward(client, message))
                } else {
                    Action::Reply(success(id))
                }
            }
            Some("resources/unsubscribe") => {
                let Some(uri) = uri else {
                    return Action::Upstream(self.forward(client, message));
                };
                let Some(subscribers) = self.subscriptions.get_mut(&uri) else {
                    return Action::Reply(success(id));
                };
                if !subscribers.remove(&client) {
                    return Action::Reply(success(id));
                }
                if subscribers.is_empty() {
                    self.subscriptions.remove(&uri);
                    Action::Upstream(self.forward(client, message))
                } else {
                    Action::Reply(success(id))
                }
            }
            _ => Action::Upstream(self.forward(client, message)),
        }
    }

    /// A client notification for the upstream, if it should reach it.
    pub fn client_notification(&mut self, client: ClientId, mut message: Value) -> Option<Value> {
        match message.get("method").and_then(Value::as_str) {
            Some("notifications/initialized" | "notifications/roots/list_changed") => None,
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

    fn settle(&mut self, hub_id: u64) -> Option<Pending> {
        let pending = self.pending.remove(&hub_id)?;
        self.by_client
            .remove(&(pending.client, id_key(&pending.original_id)));
        if let Some(count) = self.per_client.get_mut(&pending.client) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.per_client.remove(&pending.client);
            }
        }
        if let Some(token) = pending.progress {
            self.progress.remove(&token);
        }
        Some(pending)
    }

    /// One message from the upstream, routed.
    pub fn upstream_message(&mut self, mut message: Value) -> Vec<Route> {
        match jsonrpc::kind(&message) {
            Kind::Response => {
                let Some(hub_id) = message.get("id").and_then(Value::as_u64) else {
                    return Vec::new();
                };
                // Unknown or late (its client left): dropped.
                let Some(pending) = self.settle(hub_id) else {
                    return Vec::new();
                };
                message["id"] = pending.original_id;
                vec![Route::Client(pending.client, message)]
            }
            Kind::Notification => {
                let method = message
                    .get("method")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                match method.as_str() {
                    "notifications/progress" => {
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
                        vec![Route::Client(client, message)]
                    }
                    "notifications/message" => {
                        self.dropped.log_messages += 1;
                        Vec::new()
                    }
                    "notifications/resources/updated" => {
                        let Some(uri) = message.pointer("/params/uri").and_then(Value::as_str)
                        else {
                            return Vec::new();
                        };
                        self.subscribers(uri)
                            .into_iter()
                            .map(|client| Route::Client(client, message.clone()))
                            .collect()
                    }
                    // The hub sends no requests a client could see cancelled.
                    "notifications/cancelled" => Vec::new(),
                    _ => vec![Route::Broadcast(message)],
                }
            }
            Kind::Request => {
                let original = message.get("id").cloned().unwrap_or(Value::Null);
                if message.get("method").and_then(Value::as_str) == Some("ping") {
                    return vec![Route::Upstream(success(original))];
                }
                self.dropped.server_requests += 1;
                vec![Route::Upstream(jsonrpc::error_response(
                    original,
                    METHOD_NOT_FOUND,
                    "a shared aethyme mcp hub declares no client capabilities",
                    None,
                ))]
            }
            Kind::Invalid => Vec::new(),
        }
    }

    /// A client went away: its requests are cancelled upstream, its tokens
    /// and log level forgotten, and its subscriptions released (the upstream
    /// is unsubscribed from URIs nobody else follows).
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
            if self.settle(id).is_some() {
                upstream.push(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/cancelled",
                    "params": {"requestId": id, "reason": "client disconnected"},
                }));
            }
        }
        self.progress.retain(|_, (owner, _)| *owner != client);
        self.log_levels.remove(&client);
        let mut released = Vec::new();
        self.subscriptions.retain(|uri, subscribers| {
            if subscribers.remove(&client) && subscribers.is_empty() {
                released.push(uri.clone());
                return false;
            }
            true
        });
        for uri in released {
            // Nobody waits for this answer: its id is not pending.
            let id = self.allocate();
            upstream.push(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "resources/unsubscribe",
                "params": {"uri": uri},
            }));
        }
        upstream
    }

    /// The upstream died: every in-flight request fails with a retryable
    /// error. Subscriptions are forgotten (a new process has none).
    pub fn fail_all(&mut self, reason: &str) -> Vec<(ClientId, Value)> {
        let pending = std::mem::take(&mut self.pending);
        self.by_client.clear();
        self.per_client.clear();
        self.progress.clear();
        self.subscriptions.clear();
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

    fn forwarded(action: Action) -> Value {
        match action {
            Action::Upstream(message) => message,
            Action::Reply(reply) => panic!("answered locally: {reply}"),
        }
    }

    fn replied(action: Action) -> Value {
        match action {
            Action::Reply(reply) => reply,
            Action::Upstream(message) => panic!("forwarded: {message}"),
        }
    }

    #[test]
    fn colliding_ids_from_two_clients_map_back_with_their_types() {
        let mut router = Router::new();
        let first = forwarded(router.client_request(1, request(json!(1), "tools/list")));
        let second = forwarded(router.client_request(2, request(json!(1), "tools/list")));
        let third = forwarded(router.client_request(2, request(json!("1"), "tools/list")));
        let ids: Vec<u64> = [&first, &second, &third]
            .iter()
            .map(|message| message["id"].as_u64().unwrap())
            .collect();
        assert!(ids[0] != ids[1] && ids[1] != ids[2] && ids[0] != ids[2]);
        assert!(ids.iter().all(|id| *id != HUB_INITIALIZE_ID));
        assert_eq!(router.client_in_flight(2), 2);
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
        assert_eq!(router.client_in_flight(1), 0);
        assert!(router.upstream_message(answer(ids[0])).is_empty());
    }

    #[test]
    fn reusing_an_id_still_in_flight_is_refused() {
        let mut router = Router::new();
        forwarded(router.client_request(1, request(json!(5), "tools/call")));
        let refused = replied(router.client_request(1, request(json!(5), "tools/call")));
        assert_eq!(refused["error"]["code"], INVALID_REQUEST);
        assert_eq!(refused["id"], 5);
        forwarded(router.client_request(2, request(json!(5), "tools/call")));
    }

    #[test]
    fn progress_tokens_are_rewritten_and_mapped_back() {
        let mut router = Router::new();
        let sent = forwarded(router.client_request(
            7,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "x", "_meta": {"progressToken": "abc"}}}),
        ));
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
    fn cancellation_is_translated_and_client_capability_notifications_swallowed() {
        let mut router = Router::new();
        let sent = forwarded(router.client_request(4, request(json!("req-9"), "tools/call")));
        let cancel = router
            .client_notification(
                4,
                json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                       "params": {"requestId": "req-9", "reason": "user"}}),
            )
            .unwrap();
        assert_eq!(cancel["params"]["requestId"], sent["id"]);
        assert!(
            router
                .client_notification(
                    5,
                    json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                           "params": {"requestId": "req-9"}}),
                )
                .is_none()
        );
        for method in [
            "notifications/initialized",
            "notifications/roots/list_changed",
        ] {
            assert!(
                router
                    .client_notification(4, json!({"jsonrpc": "2.0", "method": method}))
                    .is_none()
            );
        }
    }

    #[test]
    fn every_server_request_but_ping_is_refused_and_counted() {
        let mut router = Router::new();
        // Even with exactly one request in flight nothing reaches a client.
        forwarded(router.client_request(3, request(json!(1), "tools/call")));
        for method in ["sampling/createMessage", "roots/list", "elicitation/create"] {
            let routed = router.upstream_message(
                json!({"jsonrpc": "2.0", "id": 900, "method": method, "params": {}}),
            );
            assert!(
                matches!(&routed[..], [Route::Upstream(message)] if message["error"]["code"] == METHOD_NOT_FOUND),
                "{routed:?}"
            );
        }
        assert_eq!(router.dropped.server_requests, 3);
        let ping = router.upstream_message(json!({"jsonrpc": "2.0", "id": 5, "method": "ping"}));
        assert_eq!(
            ping,
            vec![Route::Upstream(
                json!({"jsonrpc": "2.0", "id": 5, "result": {}})
            )]
        );
    }

    #[test]
    fn logging_stays_per_client_and_log_lines_are_dropped() {
        let mut router = Router::new();
        let reply = replied(router.client_request(
            1,
            json!({"jsonrpc": "2.0", "id": 2, "method": "logging/setLevel", "params": {"level": "debug"}}),
        ));
        assert_eq!(reply, json!({"jsonrpc": "2.0", "id": 2, "result": {}}));
        assert_eq!(router.log_level(1), Some(&json!("debug")));
        assert!(
            router
                .upstream_message(json!({"jsonrpc": "2.0", "method": "notifications/message",
                                         "params": {"level": "info", "data": "secret args"}}))
                .is_empty()
        );
        assert_eq!(router.dropped.log_messages, 1);
        let changed = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"});
        assert_eq!(
            router.upstream_message(changed.clone()),
            vec![Route::Broadcast(changed)]
        );
    }

    #[test]
    fn resource_subscriptions_are_reference_counted_and_updates_targeted() {
        let mut router = Router::new();
        let subscribe = |id: i64| json!({"jsonrpc": "2.0", "id": id, "method": "resources/subscribe", "params": {"uri": "file:///a"}});
        let unsubscribe = |id: i64| json!({"jsonrpc": "2.0", "id": id, "method": "resources/unsubscribe", "params": {"uri": "file:///a"}});
        let sent = forwarded(router.client_request(1, subscribe(1)));
        assert_eq!(sent["method"], "resources/subscribe");
        router.upstream_message(json!({"jsonrpc": "2.0", "id": sent["id"], "result": {}}));
        replied(router.client_request(2, subscribe(1)));
        let updated = json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": "file:///a"}});
        assert_eq!(
            router.upstream_message(updated.clone()),
            vec![
                Route::Client(1, updated.clone()),
                Route::Client(2, updated.clone())
            ]
        );
        let elsewhere = json!({"jsonrpc": "2.0", "method": "notifications/resources/updated", "params": {"uri": "file:///b"}});
        assert!(router.upstream_message(elsewhere).is_empty());
        replied(router.client_request(1, unsubscribe(2)));
        assert_eq!(router.subscribers("file:///a"), vec![2]);
        let last = forwarded(router.client_request(2, unsubscribe(2)));
        assert_eq!(last["method"], "resources/unsubscribe");
        assert!(router.subscribers("file:///a").is_empty());
    }

    #[test]
    fn a_gone_client_is_cancelled_unsubscribed_and_its_late_replies_dropped() {
        let mut router = Router::new();
        let sent = forwarded(router.client_request(1, request(json!(1), "tools/call")));
        let kept = forwarded(router.client_request(2, request(json!(1), "tools/call")));
        forwarded(router.client_request(
            1,
            json!({"jsonrpc": "2.0", "id": 3, "method": "resources/subscribe", "params": {"uri": "file:///x"}}),
        ));
        let upstream = router.client_gone(1);
        let methods: Vec<&str> = upstream
            .iter()
            .map(|message| message["method"].as_str().unwrap())
            .collect();
        assert_eq!(
            methods,
            vec![
                "notifications/cancelled",
                "notifications/cancelled",
                "resources/unsubscribe"
            ]
        );
        assert_eq!(upstream[0]["params"]["requestId"], sent["id"]);
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
        forwarded(router.client_request(1, request(json!("a"), "tools/call")));
        forwarded(router.client_request(2, request(json!(2), "tools/call")));
        let failed = router.fail_all("the upstream server exited; retry");
        assert_eq!(failed.len(), 2);
        assert_eq!(failed[0].0, 1);
        assert_eq!(failed[0].1["id"], "a");
        assert_eq!(failed[0].1["error"]["code"], jsonrpc::RETRYABLE);
        assert_eq!(failed[1].1["error"]["data"]["retryable"], true);
        assert_eq!(router.in_flight(), 0);
        assert_eq!(router.client_in_flight(1), 0);
    }
}
