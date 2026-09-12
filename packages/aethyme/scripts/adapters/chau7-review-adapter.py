#!/usr/bin/env python3
"""Start the Chau7 reviews the Aethyme review router asked for.

This is the transport half of review routing, and it lives outside the broker
for the same reason the delivery adapter does: the broker decides *which*
review, *which* workspace, and *which* prompt (`review tick`); it never speaks
to Chau7, so it builds and tests with no Chau7 present. This script performs the
side effects that decision implies.

Loop, per invocation:

    tab_list (Chau7)  ->  review tick (broker)  ->  per teardown:
                                                      tab_close
                                                          |
                                                    per handoff:
                                                      checkout head into
                                                      the workspace
                                                          |
                                                      tab_create + tab_exec
                                                          |
                                                      review state --state
                                                        running | abandoned

Teardown comes first, and both halves of that matter. A reviewer's shell is
interactive, so it never exits on its own; until 2026-09-12 nothing ever closed
one, and a finished review held its workspace until `stale_after_minutes`
reclaimed the row as `abandoned` -- filing a review that ran and posted as one
that never happened. And a tick may both reclaim a dimension's workspace and
dispatch a new review of that dimension into it, which is what the first tick
after a push to an already-reviewed pull request does: the other order spawns
into an occupied directory.

The broker decides which tabs those are; it reads the ledger, and a tab is
closed only when no row for its dimension is still in flight. This script does
not look at tab status, and could not: Chau7 reports `running` both for a shell
that is thinking and for one sitting at its prompt with the review posted an
hour ago.

The broker has already written each handoff's ledger row as `requested` before
printing it -- record before perform -- so this script never creates a row, only
closes one. Two consequences worth knowing:

* A review this script cannot start is closed `abandoned`, which is the single
  revivable state: the next tick asks for it again. Closing it `failed` would
  claim a reviewer looked and found nothing, and would settle the dimension for
  that head forever.
* If this script dies between the tick and the state call, the row stays
  `requested` and the route's `stale_after_minutes` reclaims it. That window is
  exactly what it is for; nothing here needs its own crash recovery.

The workspace must exist and hold a checkout of the pull request's head -- the
broker says so and deliberately does not do it, because the workspace path *is*
the identity of an in-flight review and creating one is a shared-git mutation.
Every git write here therefore runs through `aethyme broker git`, which is the
coordinated lane; running git directly would race the sessions that share the
repository.

Usage:
    chau7-review-adapter.py --session <id> --repo <owner/name>
                            [--repo-path <path>] [--limit <n>]
                            [--agent <command>] [--broker <bin>] [--dry-run]
"""

from __future__ import annotations

import argparse
import json
import os
import shlex
import socket
import subprocess
import sys
import tempfile
from typing import Any

SOCKET_PATH = os.path.expanduser("~/.chau7/mcp.sock")
PROTOCOL_VERSION = "2025-11-25"


class Chau7Error(RuntimeError):
    """Chau7 is unreachable or answered in a shape we will not guess about."""


class Chau7Client:
    """Minimal MCP client over Chau7's unix socket.

    Newline-delimited JSON-RPC, and only the calls this adapter needs. Kept
    separate from the delivery adapter's copy on purpose: these two scripts are
    installed independently and a shared module would make either one
    un-runnable on its own.
    """

    def __init__(self, path: str = SOCKET_PATH) -> None:
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
                "clientInfo": {"name": "aethyme-chau7-review-adapter", "version": "1"},
            },
        )
        self._send({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}})

    def call_tool(self, name: str, arguments: dict[str, Any]) -> Any:
        result = self._request("tools/call", {"name": name, "arguments": arguments})
        if result.get("isError"):
            raise Chau7Error(f"{name} returned an error: {result}")
        for block in result.get("content") or []:
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

    def start_review(self, workspace: str, command: str, title: str) -> str:
        """Open a tab in `workspace` and run the reviewing agent in it.

        The prompt is passed as an argument to the agent rather than typed into
        it. Typing would mean waiting for the agent's own prompt to appear --
        a readiness signal Chau7 cannot report, since it belongs to whatever
        program the tab is running -- and a prompt sent too early is silently
        swallowed by the shell.
        """
        created = self.call_tool("tab_create", {"directory": workspace})
        tab_id = created.get("tab_id") if isinstance(created, dict) else None
        if not tab_id:
            raise Chau7Error(f"tab_create did not return a tab_id: {created!r}")
        # Gates on exec acceptance, not on the shell being idle: Chau7 queues
        # the command through bootstrap, and a rejected exec is the one failure
        # that would leave an empty tab sitting in the workspace, which the
        # router then reads as a review already in flight.
        self.call_tool("tab_wait_ready", {"tab_id": tab_id, "timeout_ms": 30000})
        self.call_tool("tab_exec", {"tab_id": tab_id, "command": command})
        self.call_tool("tab_rename", {"tab_id": tab_id, "title": title})
        return tab_id

    def close_review(self, tab_id: str) -> None:
        """Close a reviewer tab whose review has settled.

        The goal state is "that tab is not standing in the workspace", so a
        tab that is already gone is success, not an error. Chau7 spells that
        refusal several ways and none of them mean the workspace is still
        occupied -- the next tick reads `tab_list` again and will simply not
        plan this teardown a second time.
        """
        self.call_tool("tab_close", {"tab_id": tab_id})


class BrokerError(RuntimeError):
    """A broker command refused or failed."""


def broker_json(broker: str, cwd: str | None, args: list[str]) -> Any:
    result = subprocess.run(
        [broker, "broker", *args],
        capture_output=True,
        text=True,
        cwd=cwd,
        check=False,
    )
    if result.returncode != 0:
        raise BrokerError(f"{' '.join(args)} failed: {result.stderr.strip()}")
    return json.loads(result.stdout)


def broker_git(broker: str, cwd: str | None, session: str, repository: str,
               reason: str, args: list[str]) -> None:
    """Run one coordinated git command against `repository`.

    `--repo` is unconditional even though the broker only demands it for remote
    commands. Every call here targets the pull request's repository, and naming
    it per call site would be a list of which spellings reach the network --
    maintained against a broker that is free to add one.
    """
    result = subprocess.run(
        [broker, "broker", "git", "--session", session, "--repo", repository,
         "--reason", reason, "--", *args],
        capture_output=True,
        text=True,
        cwd=cwd,
        check=False,
    )
    if result.returncode != 0:
        raise BrokerError(f"git {' '.join(args)} failed: {result.stderr.strip()}")


def close_row(
    broker: str,
    cwd: str | None,
    repository: str,
    pull_request: int,
    review_type: str,
    head: str | None,
    state: str,
    note: str | None = None,
) -> None:
    command = [broker, "broker", "review", "state", "--repo", repository,
               "--pr", str(pull_request), "--type", review_type, "--state", state]
    if head:
        command += ["--head", head]
    if note:
        command += ["--note", note]
    result = subprocess.run(command, capture_output=True, text=True, cwd=cwd, check=False)
    if result.returncode != 0:
        # Worth shouting about rather than swallowing: the row is now stuck in
        # `requested` and only the staleness window will free it.
        print(f"could not close the {review_type} row on #{pull_request}: "
              f"{result.stderr.strip()}", file=sys.stderr)


def head_of(workspace: str) -> str | None:
    """The commit an existing review workspace is sitting on, if any."""
    result = subprocess.run(
        ["git", "-C", workspace, "rev-parse", "HEAD"],
        capture_output=True, text=True, check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def prepare_workspace(
    broker: str, repo_path: str | None, session: str, repository: str,
    pull_request: int, head: str, workspace: str,
) -> None:
    """Leave `workspace` holding a detached checkout of exactly `head`.

    Reuses a workspace already on that commit -- a previous review of the same
    head whose tab has since closed -- and otherwise replaces it. Replacing
    rather than updating is deliberate: a review of the wrong commit reports on
    code nobody pushed, which is worse than no review, and this is the one place
    that can tell the difference.
    """
    if os.path.isdir(workspace):
        if head_of(workspace) == head:
            return
        broker_git(broker, repo_path, session, repository,
                   f"replace the stale review workspace for {repository}#{pull_request}",
                   ["worktree", "remove", "--force", workspace])

    os.makedirs(os.path.dirname(workspace) or ".", exist_ok=True)
    # The head of a pull request is not necessarily a local ref: it may live on
    # a fork, and fetching the pull ref is the only spelling that reaches both.
    broker_git(broker, repo_path, session, repository,
               f"fetch the head of {repository}#{pull_request} to review it",
               ["fetch", "origin", f"refs/pull/{pull_request}/head"])
    broker_git(broker, repo_path, session, repository,
               f"check out {repository}#{pull_request} for review",
               ["worktree", "add", "--detach", workspace, head])


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--session", required=True, help="broker session id for the coordinated writes")
    parser.add_argument("--repo", required=True, help="owner/name of the repository to sweep")
    parser.add_argument("--repo-path", default=None, help="local checkout to run the broker in")
    parser.add_argument("--limit", type=int, default=20, help="open pull requests per sweep")
    parser.add_argument("--agent", default="claude", help="command that performs a review")
    parser.add_argument("--broker", default="aethyme", help="broker binary")
    parser.add_argument("--dry-run", action="store_true",
                        help="plan the sweep and report it, but start nothing")
    args = parser.parse_args()

    try:
        client = Chau7Client()
    except Chau7Error as error:
        # Nothing has been asked for yet, so nothing is lost. Routing without a
        # tab snapshot would defer every Chau7 review anyway; leaving the tick
        # unrun keeps the next invocation's decision honest.
        print(f"chau7 unavailable: {error}", file=sys.stderr)
        return 0

    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as snapshot:
        json.dump(client.tabs(), snapshot)
        tabs_file = snapshot.name

    try:
        tick = ["review", "tick", "--session", args.session, "--repo", args.repo,
                "--limit", str(args.limit), "--tabs-file", tabs_file]
        if args.dry_run:
            tick.append("--dry-run")
        report = broker_json(args.broker, args.repo_path, tick)
    except BrokerError as error:
        print(str(error), file=sys.stderr)
        return 1
    finally:
        os.unlink(tabs_file)

    started = abandoned = closed = 0
    for visited in report.get("visited") or []:
        if not visited.get("ok"):
            print(f"#{visited['pull_request']}: {visited.get('error')}", file=sys.stderr)
            continue
        pull_request = visited["pull_request"]
        head = visited.get("head")

        # Before any spawn, for this pull request: a handoff below may be for
        # the very dimension being reclaimed here, and its workspace has to be
        # free before a second reviewer is put in it.
        for teardown in visited.get("chau7_teardown") or []:
            review_type, workspace = teardown["review_type"], teardown["workspace"]
            if args.dry_run:
                print(f"[dry-run] would close {len(teardown['tab_ids'])} tab(s) in {workspace}")
                continue
            for tab_id in teardown["tab_ids"]:
                try:
                    client.close_review(tab_id)
                except Chau7Error as error:
                    # Never fatal, and never a reason to touch the ledger. The
                    # row this tab belonged to is already settled -- that is
                    # why the broker planned the teardown -- so there is
                    # nothing here to abandon or retry. The cost of failing is
                    # that the workspace stays occupied and the next tick
                    # plans the same teardown again, which is the right
                    # outcome and needs no bookkeeping.
                    print(f"could not close {tab_id} in {workspace}: {error}",
                          file=sys.stderr)
                    continue
                closed += 1
            print(f"released the {review_type} workspace on #{pull_request}: "
                  f"{teardown['why']}")

        for handoff in visited.get("chau7_handoff") or []:
            review_type, workspace = handoff["review_type"], handoff["workspace"]
            if args.dry_run:
                print(f"[dry-run] would review {review_type} on #{pull_request} in {workspace}")
                continue
            try:
                prepare_workspace(args.broker, args.repo_path, args.session, args.repo,
                                  pull_request, head, workspace)
                client.start_review(
                    workspace,
                    f"{args.agent} {shlex.quote(handoff['prompt'])}",
                    f"{review_type} review #{pull_request}",
                )
            except (Chau7Error, BrokerError, OSError) as error:
                close_row(args.broker, args.repo_path, args.repo, pull_request,
                          review_type, head, "abandoned", str(error)[:500])
                abandoned += 1
                print(f"could not start the {review_type} review on "
                      f"#{pull_request}: {error}", file=sys.stderr)
                continue
            close_row(args.broker, args.repo_path, args.repo, pull_request,
                      review_type, head, "running")
            started += 1
            print(f"started the {review_type} review on #{pull_request} in {workspace}")

    print(f"started={started} abandoned={abandoned} closed={closed}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
