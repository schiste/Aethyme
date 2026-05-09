"""Aethyme warm-state daemon.

A long-lived Python process per Aethyme-enhanced repository. Listens on a
unix socket at ``<repo>/.aethyme/aethyme.sock`` and dispatches incoming
requests to the same handlers that ``python -m src.cli`` invokes one-shot.

Why this exists
---------------
The cold-call cost of ``aethyme explore`` is dominated by Python interpreter
startup + module imports — ~1.5-3 seconds before the first line of useful
work runs. For an agent that calls Aethyme multiple times in a single task,
that overhead is paid on every call. The daemon eats that cost once at
startup and amortizes it across every subsequent request.

The daemon does NOT keep the Rust engine subprocess alive (the engine is
still spawned per ``task-localize`` call by the existing Python code). Engine
warm-state is a separate, larger workstream — see docs/architecture/.

Wire protocol
-------------
Line-delimited JSON over a unix stream socket:

  request:  ``{"command": "explore", "args": ["--repo", "X", "--request", "..."]}\\n``
  response: ``<JSON document or text>\\n`` (the same stdout the Click command
            would have produced) followed by EOF or another request.

The client (Rust ``aethyme`` binary) opens a socket per call, writes one
request line, reads to EOF. This keeps the protocol stateless from the
client's perspective.

Lifecycle
---------
``aethyme daemon start --repo X`` forks a daemon and exits. The daemon
self-exits after an idle timeout (default 30 minutes) so it doesn't linger
forever on a workstation.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import signal
import socket
import sys
import tempfile
import threading
import time
from pathlib import Path
from typing import Any

import click

DEFAULT_IDLE_TIMEOUT_SECONDS = 1800.0  # 30 min
PIDFILE_NAME = "aethyme.pid"
LOGFILE_NAME = "aethyme-daemon.log"
SOCKET_INFO_FILENAME = "aethyme-socket.path"

logger = logging.getLogger("aethyme.daemon")


# ── socket / file paths ────────────────────────────────────────────────────


def _aethyme_dir(repo: Path) -> Path:
    d = repo.resolve() / ".aethyme"
    d.mkdir(parents=True, exist_ok=True)
    return d


def _socket_path(repo: Path) -> Path:
    """Resolve the unix socket path for ``repo``.

    macOS caps AF_UNIX paths at ~104 bytes, which is shorter than many
    Aethyme-enhanced repo paths (e.g. the playground variants). We put the
    socket in ``$TMPDIR`` keyed by a stable hash of the resolved repo path
    so length is bounded, and write a sibling pointer file inside the repo's
    ``.aethyme/`` so anything looking at the repo can find the socket. Both
    sides (Python daemon, Rust client) compute the same hash.
    """
    resolved = str(repo.resolve())
    digest = hashlib.sha256(resolved.encode("utf-8")).hexdigest()[:16]
    tmp = Path(tempfile.gettempdir()) / "aethyme"
    tmp.mkdir(parents=True, exist_ok=True)
    sock = tmp / f"daemon-{digest}.sock"

    # Drop a pointer file inside the repo so tooling can find the socket
    # without recomputing the hash.
    info_path = _aethyme_dir(repo) / SOCKET_INFO_FILENAME
    try:
        if not info_path.exists() or info_path.read_text(encoding="utf-8").strip() != str(sock):
            info_path.write_text(str(sock), encoding="utf-8")
    except OSError:
        pass

    return sock


def _pidfile_path(repo: Path) -> Path:
    return _aethyme_dir(repo) / PIDFILE_NAME


def _logfile_path(repo: Path) -> Path:
    return _aethyme_dir(repo) / LOGFILE_NAME


# ── server ─────────────────────────────────────────────────────────────────


class _DaemonServer:
    """Owns the listen socket, idle timer, and dispatch loop.

    Single-threaded request handling keeps lifecycle and logging simple — at
    typical request rates (a few per second per agent), serial dispatch is
    fine. If contention shows up later, swap the inline handler for a small
    thread pool.
    """

    def __init__(self, repo: Path, idle_timeout_seconds: float) -> None:
        self.repo = repo.resolve()
        self.idle_timeout = idle_timeout_seconds
        self.last_activity = time.monotonic()
        self.lock = threading.Lock()
        self.shutdown = threading.Event()
        self._sock: socket.socket | None = None

    def serve_forever(self) -> None:
        sock_path = _socket_path(self.repo)
        if sock_path.exists():
            sock_path.unlink()

        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        sock.bind(str(sock_path))
        sock.listen(8)
        sock.settimeout(5.0)
        self._sock = sock

        logger.info("listening on %s (idle timeout %.0fs)", sock_path, self.idle_timeout)

        # Idle-watcher thread: shuts the daemon down if no requests arrive.
        idle_thread = threading.Thread(target=self._idle_watch, daemon=True)
        idle_thread.start()

        # Eagerly warm Python imports so the first real request doesn't pay
        # the lazy-import tax.
        self._warm_imports()

        try:
            while not self.shutdown.is_set():
                try:
                    conn, _ = sock.accept()
                except TimeoutError:
                    continue
                except OSError as exc:
                    if self.shutdown.is_set():
                        break
                    logger.warning("accept error: %s", exc)
                    continue
                with conn:
                    self._handle_one(conn)
        finally:
            try:
                sock.close()
            finally:
                if sock_path.exists():
                    sock_path.unlink(missing_ok=True)

    def stop(self) -> None:
        self.shutdown.set()
        try:
            if self._sock is not None:
                self._sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass

    # ── handler ────────────────────────────────────────────────────────

    def _handle_one(self, conn: socket.socket) -> None:
        with self.lock:
            self.last_activity = time.monotonic()
        try:
            request_line = self._read_line(conn)
            if not request_line:
                return
            request = json.loads(request_line)
            command = str(request.get("command", "")).strip()
            args = request.get("args", []) or []
            if not isinstance(args, list):
                args = []
            response_text = self._dispatch(command, [str(a) for a in args])
        except Exception as exc:  # pragma: no cover — top-level guard
            logger.exception("dispatch failed")
            response_text = json.dumps({"error": str(exc)})
        try:
            payload = response_text if response_text.endswith("\n") else response_text + "\n"
            conn.sendall(payload.encode("utf-8"))
        except OSError:
            pass

    def _read_line(self, conn: socket.socket) -> str:
        buf = bytearray()
        while True:
            chunk = conn.recv(4096)
            if not chunk:
                break
            buf.extend(chunk)
            if b"\n" in chunk:
                break
            if len(buf) > 1024 * 1024:
                break
        return buf.decode("utf-8", errors="replace").strip()

    def _dispatch(self, command: str, args: list[str]) -> str:
        # `explore` was historically routed here — it's now served natively by
        # `aethyme-engine-cli explore` (the Rust thin client in
        # rust/crates/aethyme-engine/src/bin/aethyme.rs detects an engine
        # daemon socket and invokes the Rust binary directly). After task #59
        # deleted the Python explore_command this dispatch became dead code.
        if command == "ping":
            return json.dumps({"pong": True, "repo": str(self.repo)})
        return json.dumps({"error": f"unknown daemon command: {command!r}"})

    # ── lifecycle helpers ──────────────────────────────────────────────

    def _idle_watch(self) -> None:
        while not self.shutdown.is_set():
            time.sleep(15.0)
            with self.lock:
                idle_for = time.monotonic() - self.last_activity
            if idle_for > self.idle_timeout:
                logger.info("idle for %.0fs, shutting down", idle_for)
                self.stop()
                return

    def _warm_imports(self) -> None:
        """Import the heavy modules at startup so the first call is fast."""
        try:
            # Importing `src.cli` pulls in click + the explore stack.
            import importlib

            importlib.import_module("src.cli")
        except Exception:
            logger.exception("import warmup failed; first call will pay the cost")



# ── click subcommands ──────────────────────────────────────────────────────


@click.group()
def daemon() -> None:
    """Manage the warm-state Aethyme daemon for a repository."""


@daemon.command("start")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
    help="Repository to serve",
)
@click.option(
    "--idle-timeout",
    "idle_timeout",
    type=float,
    default=DEFAULT_IDLE_TIMEOUT_SECONDS,
    show_default=True,
    help="Seconds of inactivity before the daemon self-exits",
)
@click.option(
    "--foreground",
    is_flag=True,
    help="Run in the foreground (do not detach). Useful for debugging.",
)
def daemon_start(repo_path: Path, idle_timeout: float, foreground: bool) -> None:
    """Start a daemon serving --repo. Detaches by default."""

    pidfile = _pidfile_path(repo_path)
    if pidfile.exists():
        try:
            pid = int(pidfile.read_text().strip())
            os.kill(pid, 0)
            click.echo(f"daemon already running (pid {pid})")
            return
        except (ValueError, ProcessLookupError, PermissionError):
            pidfile.unlink(missing_ok=True)

    if foreground:
        _run_daemon(repo_path, idle_timeout)
        return

    pid = os.fork()
    if pid > 0:
        # Parent: wait briefly for the daemon to come up.
        time.sleep(0.3)
        sock = _socket_path(repo_path)
        if sock.exists():
            click.echo(f"daemon started (pid {pid}, socket {sock})")
        else:
            click.echo(f"daemon spawning (pid {pid})")
        return

    # Child: detach from the controlling terminal and run the loop.
    os.setsid()
    _redirect_to_logfile(repo_path)
    pidfile.write_text(str(os.getpid()))
    try:
        _run_daemon(repo_path, idle_timeout)
    finally:
        pidfile.unlink(missing_ok=True)


@daemon.command("stop")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
def daemon_stop(repo_path: Path) -> None:
    """Terminate the daemon serving --repo."""
    pidfile = _pidfile_path(repo_path)
    if not pidfile.exists():
        click.echo("daemon: not running")
        return
    try:
        pid = int(pidfile.read_text().strip())
    except ValueError:
        pidfile.unlink(missing_ok=True)
        click.echo("daemon: stale pidfile removed")
        return
    try:
        os.kill(pid, signal.SIGTERM)
        click.echo(f"daemon: sent SIGTERM to pid {pid}")
    except ProcessLookupError:
        pidfile.unlink(missing_ok=True)
        click.echo("daemon: pidfile pointed at a dead process; cleaned up")


@daemon.command("status")
@click.option(
    "--repo",
    "repo_path",
    required=True,
    type=click.Path(exists=True, file_okay=False, path_type=Path),
)
def daemon_status(repo_path: Path) -> None:
    """Check daemon health for --repo."""
    sock_path = _socket_path(repo_path)
    if not sock_path.exists():
        click.echo("daemon: not running")
        sys.exit(1)
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as sock:
            sock.settimeout(2.0)
            sock.connect(str(sock_path))
            sock.sendall(b'{"command":"ping"}\n')
            data = sock.recv(4096)
            click.echo(f"daemon: running ({data.decode('utf-8', errors='replace').strip()})")
    except OSError as exc:
        click.echo(f"daemon: socket exists but connection failed: {exc}")
        sys.exit(1)


# ── internals ──────────────────────────────────────────────────────────────


def _run_daemon(repo_path: Path, idle_timeout: float) -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    server = _DaemonServer(repo_path, idle_timeout)

    def _on_signal(signum: int, _frame: Any) -> None:  # noqa: ARG001
        logger.info("received signal %s, shutting down", signum)
        server.stop()

    signal.signal(signal.SIGTERM, _on_signal)
    signal.signal(signal.SIGINT, _on_signal)
    server.serve_forever()


def _redirect_to_logfile(repo_path: Path) -> None:
    log_path = _logfile_path(repo_path)
    log_fd = os.open(str(log_path), os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o644)
    os.dup2(log_fd, 1)  # stdout
    os.dup2(log_fd, 2)  # stderr
    devnull = os.open(os.devnull, os.O_RDONLY)
    os.dup2(devnull, 0)  # stdin
    os.close(devnull)
    os.close(log_fd)
