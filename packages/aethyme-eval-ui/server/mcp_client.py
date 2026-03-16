"""Chau7 MCP client — talks to the local Unix socket."""

from __future__ import annotations

import json
import socket
from pathlib import Path
from typing import Any

MCP_SOCKET = Path.home() / ".chau7" / "mcp.sock"

_id_counter = 0


def _next_id() -> int:
    global _id_counter
    _id_counter += 1
    return _id_counter


def _send(method: str, params: dict[str, Any] | None = None) -> Any:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.settimeout(30)
    try:
        sock.connect(str(MCP_SOCKET))

        # Initialize
        init_msg = json.dumps({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "aethyme-eval-ui", "version": "0.1"},
            },
            "id": _next_id(),
        })
        sock.sendall((init_msg + "\n").encode())
        _read_response(sock)

        # Send initialized notification
        notif = json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"})
        sock.sendall((notif + "\n").encode())

        # Call the tool
        call_msg = json.dumps({
            "jsonrpc": "2.0",
            "method": "tools/call",
            "params": {
                "name": method,
                "arguments": params or {},
            },
            "id": _next_id(),
        })
        sock.sendall((call_msg + "\n").encode())
        response = _read_response(sock)

        if "error" in response:
            raise RuntimeError(response["error"].get("message", str(response["error"])))

        result = response.get("result", {})
        content = result.get("content", [])
        if content and isinstance(content, list):
            text_block = next((c for c in content if c.get("type") == "text"), None)
            if text_block:
                try:
                    return json.loads(text_block["text"])
                except json.JSONDecodeError:
                    return text_block["text"]
        return result
    finally:
        sock.close()


def _read_response(sock: socket.socket) -> dict:
    data = b""
    while True:
        chunk = sock.recv(8192)
        if not chunk:
            break
        data += chunk
        if b"\n" in data:
            break
    return json.loads(data.decode().strip())


# ── Public API ────────────────────────────────────────────────

def is_available() -> bool:
    return MCP_SOCKET.exists()


def tab_list() -> list[dict[str, Any]]:
    return _send("tab_list")


def tab_create(directory: str) -> dict[str, Any]:
    return _send("tab_create", {"directory": directory})


def tab_close(tab_id: str, force: bool = True) -> dict[str, Any]:
    return _send("tab_close", {"tab_id": tab_id, "force": force})


def tab_status(tab_id: str) -> dict[str, Any]:
    return _send("tab_status", {"tab_id": tab_id})


def tab_output(tab_id: str, lines: int = 50) -> dict[str, Any]:
    return _send("tab_output", {"tab_id": tab_id, "lines": lines})


def tab_exec(tab_id: str, command: str) -> dict[str, Any]:
    return _send("tab_exec", {"tab_id": tab_id, "command": command})


def tab_send_input(tab_id: str, input_text: str) -> dict[str, Any]:
    return _send("tab_send_input", {"tab_id": tab_id, "input": input_text})


def tab_set_cto(tab_id: str, override: str) -> dict[str, Any]:
    return _send("tab_set_cto", {"tab_id": tab_id, "override": override})


def tab_press_key(tab_id: str, key: str, modifiers: list[str] | None = None) -> dict[str, Any]:
    params: dict[str, Any] = {"tab_id": tab_id, "key": key}
    if modifiers:
        params["modifiers"] = modifiers
    return _send("tab_press_key", params)


def tab_submit_prompt(tab_id: str) -> dict[str, Any]:
    return _send("tab_submit_prompt", {"tab_id": tab_id})
