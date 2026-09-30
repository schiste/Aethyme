//! Chau7 MCP calls made by the broker when a session gets a short name.
//!
//! Tab selection stays fail-closed: a session is matched by its worktree and
//! branch when possible, then by its explicitly recorded tab name or previous
//! broker-generated title. A missing or ambiguous match never renames a tab.

use std::env;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{Chau7Tab, Session, resolve_session_tab};

const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const MCP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(3);
const MCP_BRIDGE_SHUTDOWN_GRACE: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub(crate) enum SessionTabRename {
    Renamed { tab_id: String, title: String },
    Pending { reason: String },
    Refused { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionTabIdentity<'a> {
    id: i64,
    worktree_path: &'a str,
    branch: &'a str,
    repository_name: Option<&'a str>,
    tab_name: Option<&'a str>,
    short_name: &'a str,
}

impl<'a> SessionTabIdentity<'a> {
    fn from_session(
        session: &'a Session,
        repository_name_fallback: Option<&'a str>,
    ) -> Option<Self> {
        Some(Self {
            id: session.id,
            worktree_path: &session.worktree_path,
            branch: &session.branch,
            repository_name: session
                .repository_name
                .as_deref()
                .or(repository_name_fallback),
            tab_name: session.tab_name.as_deref(),
            short_name: session.short_name.as_deref()?,
        })
    }

    fn title(&self) -> String {
        session_tab_title(self.id, self.short_name)
    }
}

fn session_tab_title(session_id: i64, short_name: &str) -> String {
    format!("{session_id} - {short_name}")
}

pub(crate) fn rename_session_tab(
    session: &Session,
    previous_short_name: Option<&str>,
    repository_name_fallback: Option<&str>,
) -> SessionTabRename {
    let Some(identity) = SessionTabIdentity::from_session(session, repository_name_fallback) else {
        return SessionTabRename::Pending {
            reason: "session has no short name".into(),
        };
    };

    let mut command = bridge_command();
    let mut api = match McpStdioClient::connect(&mut command) {
        Ok(api) => api,
        Err(error) => {
            return SessionTabRename::Pending {
                reason: format!("Chau7 MCP is unavailable: {error}"),
            };
        }
    };
    rename_session_tab_with_api(&identity, previous_short_name, &mut api)
}

fn bridge_command() -> Command {
    if let Some(path) = env::var_os("AETHYME_CHAU7_MCP_BRIDGE") {
        return Command::new(path);
    }

    if let Some(home) = env::var_os("HOME") {
        let path = PathBuf::from(home).join(".chau7/bin/chau7-mcp-bridge");
        if path.is_file() {
            return Command::new(path);
        }
    }

    Command::new("chau7-mcp-bridge")
}

trait Chau7Api {
    fn tabs(&mut self) -> Result<Vec<Chau7Tab>, String>;
    fn rename_tab(&mut self, tab_id: &str, title: &str) -> Result<(), String>;
}

fn rename_session_tab_with_api(
    session: &SessionTabIdentity<'_>,
    previous_short_name: Option<&str>,
    api: &mut impl Chau7Api,
) -> SessionTabRename {
    let tabs = match api.tabs() {
        Ok(tabs) => tabs,
        Err(error) => {
            return SessionTabRename::Pending {
                reason: format!("could not list Chau7 tabs: {error}"),
            };
        }
    };

    let tab_id = match resolve_target_tab_id(session, previous_short_name, &tabs) {
        Ok(tab_id) => tab_id,
        Err(TabResolution::Pending(reason)) => return SessionTabRename::Pending { reason },
        Err(TabResolution::Refused(reason)) => return SessionTabRename::Refused { reason },
    };
    let title = session.title();

    if tabs
        .iter()
        .any(|tab| tab.tab_id == tab_id && tab.tab_name.as_deref() == Some(title.as_str()))
    {
        return SessionTabRename::Renamed { tab_id, title };
    }

    match api.rename_tab(&tab_id, &title) {
        Ok(()) => SessionTabRename::Renamed { tab_id, title },
        Err(error) => SessionTabRename::Pending {
            reason: format!("Chau7 refused or could not apply the title: {error}"),
        },
    }
}

enum TabResolution {
    Pending(String),
    Refused(String),
}

fn resolve_target_tab_id(
    session: &SessionTabIdentity<'_>,
    previous_short_name: Option<&str>,
    tabs: &[Chau7Tab],
) -> Result<String, TabResolution> {
    let path_match = match resolve_session_tab(tabs, session.worktree_path, session.branch) {
        Ok(resolution) => Some(resolution.tab_id),
        Err(crate::Chau7ResolutionRefusal::NoTabForWorktree { .. }) => None,
        Err(reason) => {
            return Err(TabResolution::Refused(format!(
                "session worktree did not resolve uniquely: {reason:?}"
            )));
        }
    };

    let Some(repository_name) = session.repository_name else {
        return Err(TabResolution::Pending(
            "cannot safely match a Chau7 tab by title without repository identity; supply --repo-name"
                .into(),
        ));
    };
    let mut expected_names = Vec::new();
    if let Some(name) = session.tab_name.filter(|name| !name.trim().is_empty()) {
        expected_names.push(name.to_string());
    }
    if let Some(previous) = previous_short_name {
        expected_names.push(session_tab_title(session.id, previous));
    }

    if let Some(path_match) = path_match {
        let tab = tabs
            .iter()
            .find(|tab| tab.tab_id == path_match)
            .ok_or_else(|| {
                TabResolution::Refused("resolved Chau7 tab disappeared from its snapshot".into())
            })?;
        if !repository_matches(session.repository_name, tab) {
            return Err(TabResolution::Refused(format!(
                "tab {path_match} belongs to a different repository"
            )));
        }
        return Ok(path_match);
    }

    let mut matched_ids = Vec::new();
    for expected_name in expected_names {
        let matches: Vec<&Chau7Tab> = tabs
            .iter()
            .filter(|tab| {
                tab.tab_name.as_deref() == Some(expected_name.as_str())
                    && tab_repository_name(tab).as_deref() == Some(repository_name)
            })
            .collect();
        if matches.len() > 1 {
            return Err(TabResolution::Refused(format!(
                "tab name {expected_name:?} matches multiple Chau7 tabs"
            )));
        }
        if let Some(tab) = matches.first()
            && !matched_ids.iter().any(|id| id == &tab.tab_id)
        {
            matched_ids.push(tab.tab_id.clone());
        }
    }

    match matched_ids.as_slice() {
        [tab_id] => Ok(tab_id.clone()),
        [] => Err(TabResolution::Pending(
            "no Chau7 tab matched the session worktree or its recorded title".into(),
        )),
        _ => Err(TabResolution::Refused(
            "the session worktree and recorded title identify different Chau7 tabs".into(),
        )),
    }
}

fn repository_matches(repository_name: Option<&str>, tab: &Chau7Tab) -> bool {
    repository_name
        .map(|expected| tab_repository_name(tab).as_deref() == Some(expected))
        .unwrap_or(true)
}

fn tab_repository_name(tab: &Chau7Tab) -> Option<String> {
    tab.session_context().repository_name
}

struct McpStdioClient {
    child: Child,
    stdin: Option<ChildStdin>,
    responses: Receiver<Result<String, String>>,
    reader: Option<JoinHandle<()>>,
    next_request_id: u64,
}

impl McpStdioClient {
    fn connect(command: &mut Command) -> Result<Self, String> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not launch the Chau7 MCP bridge: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Chau7 MCP bridge has no stdout pipe".to_string())?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Chau7 MCP bridge has no stdin pipe".to_string())?;
        let (sender, responses) = mpsc::channel();
        let reader = match thread::Builder::new()
            .name("aethyme-chau7-mcp-reader".into())
            .spawn(move || read_responses(stdout, sender))
        {
            Ok(reader) => reader,
            Err(error) => {
                crate::warn_unrecorded(
                    "terminate Chau7 MCP bridge after reader startup failed",
                    child.kill(),
                );
                crate::warn_unrecorded(
                    "reap Chau7 MCP bridge after reader startup failed",
                    child.wait(),
                );
                return Err(format!("could not start the Chau7 MCP reader: {error}"));
            }
        };
        let mut client = Self {
            child,
            stdin: Some(stdin),
            responses,
            reader: Some(reader),
            next_request_id: 1,
        };

        client.request(
            "initialize",
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "aethyme-broker",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        client.notify("notifications/initialized", json!({}))?;
        Ok(client)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_request_id;
        self.next_request_id += 1;
        self.write_message(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))?;

        let deadline = Instant::now() + MCP_RESPONSE_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!("timed out waiting for Chau7 MCP {method}"));
            }
            let line = match self.responses.recv_timeout(remaining) {
                Ok(Ok(line)) => line,
                Ok(Err(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("timed out waiting for Chau7 MCP {method}"));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("Chau7 MCP bridge closed before replying".into());
                }
            };
            let message: Value = serde_json::from_str(&line)
                .map_err(|error| format!("Chau7 MCP returned invalid JSON: {error}"))?;
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(format!("Chau7 MCP request failed: {error}"));
            }
            return message
                .get("result")
                .cloned()
                .ok_or_else(|| "Chau7 MCP response has no result".into());
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write_message(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
    }

    fn write_message(&mut self, message: Value) -> Result<(), String> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| "Chau7 MCP bridge input is closed".to_string())?;
        serde_json::to_writer(&mut *stdin, &message)
            .map_err(|error| format!("could not encode Chau7 MCP request: {error}"))?;
        stdin
            .write_all(b"\n")
            .and_then(|()| stdin.flush())
            .map_err(|error| format!("could not send Chau7 MCP request: {error}"))
    }
}

impl Chau7Api for McpStdioClient {
    fn tabs(&mut self) -> Result<Vec<Chau7Tab>, String> {
        let response =
            self.request("tools/call", json!({ "name": "tab_list", "arguments": {} }))?;
        let payload = unwrap_tool_payload(response)?;
        let payload = if let Value::String(encoded) = payload {
            serde_json::from_str(&encoded)
                .map_err(|error| format!("Chau7 tab list is invalid JSON: {error}"))?
        } else {
            payload
        };
        let tabs = match &payload {
            Value::Array(_) => payload.clone(),
            Value::Object(object) => object
                .get("tabs")
                .or_else(|| object.get("items"))
                .cloned()
                .ok_or_else(|| "Chau7 tab list has no tabs array".to_string())?,
            _ => return Err("Chau7 tab list is not an array".into()),
        };
        serde_json::from_value(tabs)
            .map_err(|error| format!("Chau7 tab list has an unknown shape: {error}"))
    }

    fn rename_tab(&mut self, tab_id: &str, title: &str) -> Result<(), String> {
        let response = self.request(
            "tools/call",
            json!({
                "name": "tab_rename",
                "arguments": { "tab_id": tab_id, "title": title },
            }),
        )?;
        unwrap_tool_payload(response).map(|_| ())
    }
}

fn unwrap_tool_payload(response: Value) -> Result<Value, String> {
    if response.get("isError").and_then(Value::as_bool) == Some(true) {
        let detail = response
            .get("content")
            .and_then(Value::as_array)
            .and_then(|items| {
                items.iter().find_map(|item| {
                    (item.get("type").and_then(Value::as_str) == Some("text"))
                        .then(|| item.get("text").and_then(Value::as_str))
                        .flatten()
                })
            })
            .unwrap_or("Chau7 API rejected the request");
        return Err(detail.to_string());
    }
    if let Some(structured) = response.get("structuredContent") {
        return Ok(structured.clone());
    }
    if let Some(text) = response
        .get("content")
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                (item.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| item.get("text").and_then(Value::as_str))
                    .flatten()
            })
        })
    {
        return Ok(Value::String(text.to_string()));
    }
    Ok(response)
}

fn read_responses(stdout: impl std::io::Read, sender: mpsc::Sender<Result<String, String>>) {
    for line in BufReader::new(stdout).lines() {
        match line {
            Ok(line) => {
                if sender.send(Ok(line)).is_err() {
                    break;
                }
            }
            Err(error) => {
                if sender
                    .send(Err(format!("could not read Chau7 MCP response: {error}")))
                    .is_err()
                {
                    return;
                }
                break;
            }
        }
    }
}

impl Drop for McpStdioClient {
    fn drop(&mut self) {
        self.stdin.take();
        let deadline = Instant::now() + MCP_BRIDGE_SHUTDOWN_GRACE;
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    // This terminates only the bridge child created above; the
                    // Chau7 application remains owned by the user.
                    crate::warn_unrecorded("terminate Chau7 MCP bridge", self.child.kill());
                    crate::warn_unrecorded("reap Chau7 MCP bridge", self.child.wait());
                    break;
                }
            }
        }
        if let Some(reader) = self.reader.take() {
            crate::warn_unrecorded(
                "join Chau7 MCP response reader",
                reader.join().map_err(|_| "reader thread panicked"),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeApi {
        tabs: Vec<Chau7Tab>,
        renamed: Vec<(String, String)>,
        rename_error: Option<String>,
    }

    impl Chau7Api for FakeApi {
        fn tabs(&mut self) -> Result<Vec<Chau7Tab>, String> {
            Ok(self.tabs.clone())
        }

        fn rename_tab(&mut self, tab_id: &str, title: &str) -> Result<(), String> {
            self.renamed.push((tab_id.into(), title.into()));
            self.rename_error.clone().map_or(Ok(()), Err)
        }
    }

    fn session() -> SessionTabIdentity<'static> {
        SessionTabIdentity {
            id: 41,
            worktree_path: "/work/session",
            branch: "agent/session",
            repository_name: Some("Aethyme"),
            tab_name: Some("Broker task"),
            short_name: "Broker titles",
        }
    }

    fn tab(id: &str, cwd: &str, branch: &str, title: &str) -> Chau7Tab {
        Chau7Tab {
            tab_id: id.into(),
            tab_name: Some(title.into()),
            cwd: Some(cwd.into()),
            repo_root: Some("/work/Aethyme".into()),
            repo_name: Some("Aethyme".into()),
            git_branch: Some(branch.into()),
            ai_provider: Some("codex".into()),
            status: Some("idle".into()),
            is_mcp_controlled: Some(true),
        }
    }

    #[test]
    fn worktree_and_branch_select_the_session_tab_and_apply_the_composed_title() {
        let mut api = FakeApi {
            tabs: vec![tab(
                "tab_9",
                "/work/session",
                "agent/session",
                "Broker task",
            )],
            ..FakeApi::default()
        };

        let outcome = rename_session_tab_with_api(&session(), None, &mut api);

        assert_eq!(
            outcome,
            SessionTabRename::Renamed {
                tab_id: "tab_9".into(),
                title: "41 - Broker titles".into(),
            }
        );
        assert_eq!(
            api.renamed,
            vec![("tab_9".into(), "41 - Broker titles".into())]
        );
    }

    #[test]
    fn repo_root_identifies_the_repository_when_tab_list_omits_repo_name() {
        let mut api = FakeApi {
            tabs: vec![tab("tab_9", "/work/repo-root", "main", "Broker task")],
            ..FakeApi::default()
        };
        api.tabs[0].repo_name = None;
        let mut identity = session();
        identity.worktree_path = "/work/session-not-open";

        let outcome = rename_session_tab_with_api(&identity, None, &mut api);

        assert_eq!(
            outcome,
            SessionTabRename::Renamed {
                tab_id: "tab_9".into(),
                title: "41 - Broker titles".into(),
            }
        );
    }

    #[test]
    fn previous_broker_title_identifies_a_reused_session_tab() {
        let mut api = FakeApi {
            tabs: vec![tab(
                "tab_9",
                "/work/repo-root",
                "main",
                "41 - Previous title",
            )],
            ..FakeApi::default()
        };
        let mut identity = session();
        identity.worktree_path = "/work/session-not-open";

        let outcome = rename_session_tab_with_api(&identity, Some("Previous title"), &mut api);

        assert_eq!(
            outcome,
            SessionTabRename::Renamed {
                tab_id: "tab_9".into(),
                title: "41 - Broker titles".into(),
            }
        );
    }

    #[test]
    fn missing_or_ambiguous_tab_never_calls_rename() {
        let mut missing = FakeApi::default();
        assert!(matches!(
            rename_session_tab_with_api(&session(), None, &mut missing),
            SessionTabRename::Pending { .. }
        ));
        assert!(missing.renamed.is_empty());

        let mut ambiguous = FakeApi {
            tabs: vec![
                tab("tab_9", "/work/repo-root", "main", "Broker task"),
                tab("tab_10", "/work/other", "main", "Broker task"),
            ],
            ..FakeApi::default()
        };
        let mut identity = session();
        identity.worktree_path = "/work/session-not-open";
        assert!(matches!(
            rename_session_tab_with_api(&identity, None, &mut ambiguous),
            SessionTabRename::Refused { .. }
        ));
        assert!(ambiguous.renamed.is_empty());
    }

    #[test]
    fn branch_divergence_refuses_even_when_the_old_title_matches() {
        let mut api = FakeApi {
            tabs: vec![tab(
                "tab_9",
                "/work/session",
                "different-branch",
                "Broker task",
            )],
            ..FakeApi::default()
        };

        assert!(matches!(
            rename_session_tab_with_api(&session(), None, &mut api),
            SessionTabRename::Refused { .. }
        ));
        assert!(api.renamed.is_empty());
    }

    #[test]
    fn api_failure_is_retryable_and_idempotent_titles_skip_the_write() {
        let mut failing = FakeApi {
            tabs: vec![tab(
                "tab_9",
                "/work/session",
                "agent/session",
                "Broker task",
            )],
            rename_error: Some("control is required".into()),
            ..FakeApi::default()
        };
        assert!(matches!(
            rename_session_tab_with_api(&session(), None, &mut failing),
            SessionTabRename::Pending { .. }
        ));
        assert_eq!(failing.renamed.len(), 1);

        let mut already_named = FakeApi {
            tabs: vec![tab(
                "tab_9",
                "/work/session",
                "agent/session",
                "41 - Broker titles",
            )],
            ..FakeApi::default()
        };
        assert!(matches!(
            rename_session_tab_with_api(&session(), None, &mut already_named),
            SessionTabRename::Renamed { .. }
        ));
        assert!(already_named.renamed.is_empty());
    }

    #[test]
    fn a_known_repository_name_excludes_tabs_from_other_repositories() {
        let mut api = FakeApi {
            tabs: vec![Chau7Tab {
                repo_name: Some("AnotherRepo".into()),
                ..tab("tab_9", "/work/repo-root", "main", "Broker task")
            }],
            ..FakeApi::default()
        };
        let mut identity = session();
        identity.worktree_path = "/work/session-not-open";

        assert!(matches!(
            rename_session_tab_with_api(&identity, None, &mut api),
            SessionTabRename::Pending { .. }
        ));
        assert!(api.renamed.is_empty());
    }

    #[test]
    fn title_fallback_requires_repository_identity() {
        let mut api = FakeApi {
            tabs: vec![tab("tab_9", "/work/repo-root", "main", "Broker task")],
            ..FakeApi::default()
        };
        let mut identity = session();
        identity.repository_name = None;
        identity.worktree_path = "/work/session-not-open";

        assert!(matches!(
            rename_session_tab_with_api(&identity, None, &mut api),
            SessionTabRename::Pending { .. }
        ));
        assert!(api.renamed.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn stdio_bridge_uses_mcp_tool_calls_for_tab_list_and_rename() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let bridge = temp.path().join("fake-chau7-mcp-bridge");
        std::fs::write(
            &bridge,
            r##"#!/bin/sh
IFS= read -r initialize || exit 10
printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"test","version":"1"}}}'
IFS= read -r notification || exit 11
IFS= read -r list_request || exit 12
case "$list_request" in *'"name":"tab_list"'*) ;; *) exit 13 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"structuredContent":{"tabs":[{"tab_id":"tab_9","tab_name":"Broker task","cwd":"/work/session","repo_name":"Aethyme","git_branch":"agent/session"}]}}}'
IFS= read -r rename_request || exit 14
case "$rename_request" in *'"name":"tab_rename"'*) ;; *) exit 15 ;; esac
case "$rename_request" in *'"tab_id":"tab_9"'*) ;; *) exit 15 ;; esac
case "$rename_request" in *'"title":"41 - Broker titles"'*) ;; *) exit 15 ;; esac
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"structuredContent":{"renamed":true}}}'
"##,
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&bridge).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&bridge, permissions).unwrap();

        let mut api = McpStdioClient::connect(&mut Command::new(&bridge))
            .unwrap_or_else(|error| panic!("fake bridge should initialize: {error}"));
        let tabs = api
            .tabs()
            .unwrap_or_else(|error| panic!("fake tab list should decode: {error}"));
        assert_eq!(tabs.len(), 1);
        assert_eq!(tabs[0].tab_id, "tab_9");
        api.rename_tab("tab_9", "41 - Broker titles")
            .unwrap_or_else(|error| panic!("fake rename should succeed: {error}"));
    }
}
