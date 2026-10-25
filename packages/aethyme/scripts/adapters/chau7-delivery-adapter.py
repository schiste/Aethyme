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

I/O limits: one shared 45-second pass budget, at most ten seconds per call,
four MiB of combined broker output or buffered MCP data, and 128 interleaved
messages per request. Direct broker children are stopped/reaped on failure;
this does not establish descendant-group cleanup or exactly-once delivery.

Usage:
    chau7-delivery-adapter.py --worker <id> [--repo <path>] [--max <n>] [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import os
import selectors
import socket
import subprocess
import sys
import time
from typing import Any

SOCKET_PATH = os.path.expanduser("~/.chau7/mcp.sock")
PROTOCOL_VERSION = "2025-11-25"


class Chau7Error(RuntimeError):
    """Chau7 is unreachable or answered in a shape we will not guess about."""


PASS_SECONDS = 45
CALL_SECONDS = 10
MAX_WIRE_BYTES = 4 * 1024 * 1024
MAX_INTERLEAVED_MESSAGES = 128


class Deadline:
    def __init__(self, seconds: float = PASS_SECONDS, clock=time.monotonic):
        self.clock = clock
        self.expires = clock() + seconds

    def remaining(self, end: float | None = None) -> float:
        left = min(self.expires, end if end is not None else self.expires) - self.clock()
        if left <= 0:
            raise Chau7Error("adapter pass or call deadline exhausted")
        return left


def run_broker(command, repo, payload=None, budget=None, call_seconds=CALL_SECONDS):
    """Bound one owned direct child, input, combined output and pipe draining.

    This does not establish process-group or escaped-descendant cleanup.
    """
    budget = budget or Deadline()
    end = min(budget.expires, budget.clock() + call_seconds)
    budget.remaining(end)  # Refuse before spawning after the pass deadline.
    data = payload.encode() if payload is not None else b""
    if len(data) > MAX_WIRE_BYTES:
        raise Chau7Error("broker input exceeded four MiB")
    selector = selectors.DefaultSelector()
    try:
        process = subprocess.Popen(command, cwd=repo, stdin=subprocess.PIPE,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    except BaseException:
        selector.close()
        raise
    output = {"stdout": bytearray(), "stderr": bytearray()}
    total = 0
    offset = 0
    try:
        for stream, name in ((process.stdout, "stdout"), (process.stderr, "stderr")):
            os.set_blocking(stream.fileno(), False)
            selector.register(stream, selectors.EVENT_READ, name)
        if data:
            os.set_blocking(process.stdin.fileno(), False)
            selector.register(process.stdin, selectors.EVENT_WRITE, "stdin")
        else:
            process.stdin.close()
        while selector.get_map():
            for key, _ in selector.select(budget.remaining(end)):
                stream = key.fileobj
                if key.data == "stdin":
                    try:
                        offset += os.write(stream.fileno(), data[offset:offset + 4096])
                    except BrokenPipeError:
                        offset = len(data)
                    if offset == len(data):
                        selector.unregister(stream)
                        stream.close()
                    continue
                chunk = os.read(stream.fileno(), 65536)
                if not chunk:
                    selector.unregister(stream)
                    stream.close()
                    continue
                total += len(chunk)
                if total > MAX_WIRE_BYTES:
                    raise Chau7Error("broker output exceeded four MiB")
                output[key.data].extend(chunk)
        try:
            code = process.wait(timeout=budget.remaining(end))
        except subprocess.TimeoutExpired as error:
            raise Chau7Error("broker call deadline exhausted") from error
        return subprocess.CompletedProcess(command, code,
                                           bytes(output["stdout"]).decode("utf-8"),
                                           bytes(output["stderr"]).decode("utf-8"))
    finally:
        selector.close()
        for stream in (process.stdin, process.stdout, process.stderr):
            if stream is not None:
                stream.close()
        # Popen retains direct-child identity and protects against signaling a
        # reaped/reused PID. Never signal arbitrary recorded PIDs or groups.
        if process.poll() is None:
            process.kill()
        try:
            process.wait(timeout=1)
        except subprocess.TimeoutExpired as error:
            raise Chau7Error("cannot reap owned broker child after bounded stop") from error


class Chau7Client:
    """Minimal MCP client over Chau7's unix socket.

    Newline-delimited JSON-RPC. Only the three calls this adapter needs are
    implemented -- a fuller client would be more surface to keep correct for no
    benefit here.
    """

    def __init__(self, path: str = SOCKET_PATH, budget: Deadline | None = None) -> None:
        self._path = path
        self._next_id = 0
        self._budget = budget or Deadline()
        self._request_deadline = None
        self._buffer = b""
        self._sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self._sock.settimeout(min(CALL_SECONDS, self._budget.remaining()))
            self._sock.connect(path)
            self._initialize()
        except (OSError, Chau7Error) as error:
            self.close()
            raise Chau7Error(f"cannot initialize Chau7 at {path}: {error}") from error

    def close(self) -> None:
        self._sock.close()

    def _io_budget(self) -> None:
        self._sock.settimeout(min(CALL_SECONDS, self._budget.remaining(self._request_deadline)))

    def _send(self, payload: dict[str, Any]) -> None:
        encoded = json.dumps(payload).encode() + b"\n"
        if len(encoded) > MAX_WIRE_BYTES:
            raise Chau7Error("Chau7 request exceeded four MiB")
        self._io_budget()
        self._sock.sendall(encoded)

    def _read_message(self) -> dict[str, Any]:
        while b"\n" not in self._buffer:
            self._io_budget()
            chunk = self._sock.recv(min(65536, MAX_WIRE_BYTES + 1 - len(self._buffer)))
            if not chunk:
                raise Chau7Error("Chau7 closed the connection mid-request")
            self._buffer += chunk
            if len(self._buffer) > MAX_WIRE_BYTES:
                raise Chau7Error("Chau7 message exceeded four MiB")
        self._io_budget()
        line, self._buffer = self._buffer.split(b"\n", 1)
        try:
            message = json.loads(line)
        except (ValueError, UnicodeError) as error:
            raise Chau7Error("Chau7 sent malformed JSON") from error
        if not isinstance(message, dict):
            raise Chau7Error("Chau7 reply is not an object")
        return message

    def _request(self, method: str, params: dict[str, Any]) -> Any:
        self._next_id += 1
        request_id = self._next_id
        previous = self._request_deadline
        self._request_deadline = min(self._budget.expires, self._budget.clock() + CALL_SECONDS)
        try:
            self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
            # Notifications may interleave, but cannot extend time or memory.
            for _ in range(MAX_INTERLEAVED_MESSAGES):
                message = self._read_message()
                if message.get("id") != request_id:
                    continue
                if "error" in message:
                    raise Chau7Error(f"{method} failed: {message['error']}")
                return message.get("result")
            raise Chau7Error("too many interleaved Chau7 messages")
        except OSError as error:
            raise Chau7Error(f"Chau7 request failed: {error}") from error
        finally:
            self._request_deadline = previous

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
        if not isinstance(result, dict):
            raise Chau7Error(f"{name} returned a non-object result")
        is_error = result.get("isError", False)
        if not isinstance(is_error, bool):
            raise Chau7Error(f"{name} returned a non-boolean isError")
        if is_error:
            raise Chau7Error(f"{name} returned an error: {result}")
        content = result.get("content", [])
        if not isinstance(content, list) or any(not isinstance(block, dict) for block in content):
            raise Chau7Error(f"{name} returned malformed content")
        for block in content:
            if block.get("type") == "text" and not isinstance(block.get("text"), str):
                raise Chau7Error(f"{name} returned non-text text content")
        for block in content:
            if block.get("type") == "text":
                text = block["text"]
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
    broker: str, repo: str | None, worker: str, tabs: list[dict[str, Any]], budget=None
) -> dict[str, Any]:
    # No --tabs-file: the broker reads the snapshot from stdin when the flag is
    # absent. Passing "-" looks conventional but is taken as a literal path.
    command = [broker, "broker", "advanced", "deliveries", "dispatch", "--adapter", "chau7",
               "--worker", worker, "--json"]
    result = run_broker(command, repo, json.dumps(tabs), budget)
    if result.returncode != 0:
        raise RuntimeError(f"deliveries dispatch failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def complete(broker: str, repo: str | None, delivery_id: int, worker: str,
             generation: int, outcome: str, error_code: str | None = None, budget=None) -> None:
    command = [broker, "broker", "advanced", "deliveries", "complete", "--id", str(delivery_id),
               "--worker", worker, "--generation", str(generation), "--outcome", outcome]
    if error_code:
        command += ["--error-code", error_code]
    result = run_broker(command, repo, budget=budget)
    if result.returncode != 0:
        raise RuntimeError(f"deliveries complete failed: {result.stderr.strip()}")


def main(argv=None, budget=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--worker", required=True, help="stable id fencing this worker's claims")
    parser.add_argument("--repo", default=None, help="repository to run the broker in")
    parser.add_argument("--broker", default="aethyme", help="broker binary")
    parser.add_argument("--max", type=int, default=10, help="deliveries per invocation")
    parser.add_argument("--dry-run", action="store_true",
                        help="decide and report, but perform no transport")
    args = parser.parse_args(argv)

    budget = budget or Deadline()
    try:
        client = Chau7Client(budget=budget)
    except Chau7Error as error:
        # Not a failure of any delivery: nothing was claimed, so nothing is
        # lost, and the next scheduled invocation retries.
        print(f"chau7 unavailable: {error}", file=sys.stderr)
        return 0

    try:
        tabs = client.tabs()
        sent = deferred = abandoned = 0

        for _ in range(max(1, args.max)):
            report = dispatch_once(args.broker, args.repo, args.worker, tabs, budget=budget)
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
                         report["generation"], "retry", "dry_run", budget=budget)
                continue

            try:
                client.deliver(tab_id, prompt)
            except Chau7Error as error:
                # The transport failed, so the delivery has not landed. Retry keeps
                # it claimable rather than marking a message delivered that no
                # agent ever saw.
                complete(args.broker, args.repo, report["delivery_id"], args.worker,
                         report["generation"], "retry", "transport_failed", budget=budget)
                print(f"transport failed for {tab_id}: {error}", file=sys.stderr)
                continue

            complete(args.broker, args.repo, report["delivery_id"], args.worker,
                     report["generation"], "delivered", budget=budget)
            sent += 1
            print(f"delivered {report['delivery_id']} to {tab_id}")

        print(f"sent={sent} deferred={deferred} abandoned={abandoned}")
        return 0
    finally:
        client.close()


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (Chau7Error, RuntimeError, ValueError) as error:
        print(f"adapter stopped: {error}", file=sys.stderr)
        sys.exit(1)
