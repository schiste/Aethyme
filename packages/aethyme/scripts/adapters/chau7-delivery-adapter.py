#!/usr/bin/env python3
"""Deliver Aethyme PR-activity notifications into the Chau7 tab running the session.

This is the transport half of the delivery loop, and it lives outside the
broker on purpose. The broker decides *which* tab and *whether now*
(`deliveries dispatch`); it never speaks to Chau7, so it builds and tests with
no Chau7 present. This script performs the side effects that decision implies.

Loop, per invocation:

    tab_list (Chau7)  ->  deliveries dispatch (broker)  ->  send | defer | abandon
                                                              |
                                             tab_send_input + tab_submit_prompt
                                                              |
                                                     deliveries complete

Deferred and abandoned deliveries are completed by the broker itself, so this
script only ever completes a `send`, and only after the transport has actually
landed. That ordering is the point: completing first would lose a notification
whenever the send fails.

Usage:
    chau7-delivery-adapter.py --worker <id> [--repo <path>] [--max <n>] [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import os
import socket
import subprocess
import sys
from typing import Any

SOCKET_PATH = os.path.expanduser("~/.chau7/mcp.sock")
PROTOCOL_VERSION = "2025-11-25"


class Chau7Error(RuntimeError):
    """Chau7 is unreachable or answered in a shape we will not guess about."""


class Chau7Client:
    """Minimal MCP client over Chau7's unix socket.

    Newline-delimited JSON-RPC. Only the three calls this adapter needs are
    implemented -- a fuller client would be more surface to keep correct for no
    benefit here.
    """

    def __init__(self, path: str = SOCKET_PATH) -> None:
        self._path = path
        self._next_id = 0
        try:
            self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            self._sock.settimeout(30)
            self._sock.connect(path)
        except OSError as error:
            raise Chau7Error(f"cannot connect to Chau7 at {path}: {error}") from error
        self._buffer = b""
        self._initialize()

    def _send(self, payload: dict[str, Any]) -> None:
        self._sock.sendall(json.dumps(payload).encode() + b"\n")

    def _read_message(self) -> dict[str, Any]:
        while b"\n" not in self._buffer:
            chunk = self._sock.recv(65536)
            if not chunk:
                raise Chau7Error("Chau7 closed the connection mid-request")
            self._buffer += chunk
        line, self._buffer = self._buffer.split(b"\n", 1)
        return json.loads(line)

    def _request(self, method: str, params: dict[str, Any]) -> Any:
        self._next_id += 1
        request_id = self._next_id
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        # Notifications may interleave with the reply; match on id.
        while True:
            message = self._read_message()
            if message.get("id") != request_id:
                continue
            if "error" in message:
                raise Chau7Error(f"{method} failed: {message['error']}")
            return message.get("result")

    def _initialize(self) -> None:
        self._request(
            "initialize",
            {
                "protocolVersion": PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {"name": "aethyme-chau7-delivery-adapter", "version": "1"},
            },
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})

    def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        result = self._request("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise Chau7Error(f"{name} returned an error: {result}")
        content = result.get("content") or []
        for block in content:
            if block.get("type") == "text":
                text = block.get("text", "")
                try:
                    return json.loads(text)
                except json.JSONDecodeError:
                    return text
        return result

    def tabs(self) -> list[dict[str, Any]]:
        listed = self.call_tool("tab_list", {})
        if not isinstance(listed, list):
            raise Chau7Error(f"tab_list did not return an array: {type(listed).__name__}")
        return listed

    def deliver(self, tab_id: str, prompt: str) -> None:
        """Type the prompt, then submit it.

        Two calls because Chau7 separates them: `tab_send_input` does not append
        a newline, and submission is an explicit Enter. Sending without
        submitting would leave the agent with an unsent draft -- silently no
        delivery at all, which is worse than a visible failure.
        """
        self.call_tool("tab_send_input", {"tab_id": tab_id, "input": prompt})
        self.call_tool("tab_submit_prompt", {"tab_id": tab_id})


def dispatch_once(
    broker: str, repo: str | None, worker: str, tabs: list[dict[str, Any]]
) -> dict[str, Any]:
    command = [broker, "broker", "deliveries", "dispatch", "--adapter", "chau7",
               "--worker", worker, "--tabs-file", "-", "--json"]
    result = subprocess.run(
        command,
        input=json.dumps(tabs),
        capture_output=True,
        text=True,
        cwd=repo,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError(f"deliveries dispatch failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def complete(broker: str, repo: str | None, delivery_id: int, worker: str,
             generation: int, outcome: str, error_code: str | None = None) -> None:
    command = [broker, "broker", "deliveries", "complete", "--id", str(delivery_id),
               "--worker", worker, "--generation", str(generation), "--outcome", outcome]
    if error_code:
        command += ["--error-code", error_code]
    result = subprocess.run(command, capture_output=True, text=True, cwd=repo, check=False)
    if result.returncode != 0:
        raise RuntimeError(f"deliveries complete failed: {result.stderr.strip()}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", required=True, help="stable id fencing this worker's claims")
    parser.add_argument("--repo", default=None, help="repository to run the broker in")
    parser.add_argument("--broker", default="aethyme", help="broker binary")
    parser.add_argument("--max", type=int, default=10, help="deliveries per invocation")
    parser.add_argument("--dry-run", action="store_true",
                        help="decide and report, but perform no transport")
    args = parser.parse_args()

    try:
        client = Chau7Client()
    except Chau7Error as error:
        # Not a failure of any delivery: nothing was claimed, so nothing is
        # lost, and the next scheduled invocation retries.
        print(f"chau7 unavailable: {error}", file=sys.stderr)
        return 0

    tabs = client.tabs()
    sent = deferred = abandoned = 0

    for _ in range(max(1, args.max)):
        report = dispatch_once(args.broker, args.repo, args.worker, tabs)
        if not report.get("claimed"):
            break
        action = report["action"]
        kind = action["action"]

        if kind == "defer":
            deferred += 1
            continue
        if kind == "abandon":
            abandoned += 1
            print(f"abandoned delivery {report['delivery_id']}: {action['why']}", file=sys.stderr)
            continue

        tab_id, prompt = action["tab_id"], action["prompt"]
        if args.dry_run:
            print(f"[dry-run] would send delivery {report['delivery_id']} to {tab_id}")
            complete(args.broker, args.repo, report["delivery_id"], args.worker,
                     report["generation"], "retry", "dry_run")
            continue

        try:
            client.deliver(tab_id, prompt)
        except Chau7Error as error:
            # The transport failed, so the delivery has not landed. Retry keeps
            # it claimable rather than marking a message delivered that no
            # agent ever saw.
            complete(args.broker, args.repo, report["delivery_id"], args.worker,
                     report["generation"], "retry", "transport_failed")
            print(f"transport failed for {tab_id}: {error}", file=sys.stderr)
            continue

        complete(args.broker, args.repo, report["delivery_id"], args.worker,
                 report["generation"], "delivered")
        sent += 1
        print(f"delivered {report['delivery_id']} to {tab_id}")

    print(f"sent={sent} deferred={deferred} abandoned={abandoned}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
