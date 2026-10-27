#!/usr/bin/env python3
"""Opt-in synthetic shared-history submit diagnostic; no providers or evaluations."""
import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import selectors
import shlex
import shutil
import signal
import stat
import subprocess
import tempfile
import threading
import time

from broker_latency import fixture_path, validate_fixture_environment

MAX_HISTORY_COMMITS = 4
BINARY_BYTES_PER_COMMIT = 256 * 1024
MAX_PATCH_AUDIT_BYTES = 4 * 1024 * 1024
MAX_STDOUT_BYTES = 4 * 1024 * 1024
MAX_STDERR_BYTES = 1024 * 1024
MAX_TRACE2_BYTES = 8 * 1024 * 1024
MAX_COMMANDS = 80
MAX_RECEIPTS_BYTES = 24 * 1024 * 1024
MAX_OUTPUT_BYTES = 64 * 1024 * 1024
MAX_BINARY_BYTES = 256 * 1024 * 1024
COMMAND_TIMEOUT_SECONDS = 60
PAIR_BARRIER_SECONDS = 15
FIXED_GIT_DATE = '2020-01-02T03:04:05+00:00'
POLICY_TEXT = (
    '[delivery]\n'
    'push_session_branches = true\n'
    '[promote]\n'
    'mode = "manual"\n'
    '[cleanup]\n'
    'auto_remove = false\n'
)


def canonical_json_bytes(value):
    return (json.dumps(value, indent=2, sort_keys=True) + '\n').encode('utf-8')


def sha256_bytes(value):
    return hashlib.sha256(value).hexdigest()


def redact_captured_output(stdout, stderr, values):
    """Redact named secrets before a receipt is persisted; hash original bytes."""
    if not isinstance(stdout, bytes) or not isinstance(stderr, bytes):
        raise ValueError('captured output must be bytes')
    if not isinstance(values, (tuple, list)):
        raise ValueError('redaction values must be a sequence')
    redacted_stdout = stdout
    redacted_stderr = stderr
    occurrences = 0
    for value in values:
        if not isinstance(value, str) or not value:
            raise ValueError('redaction values must be nonempty strings')
        token = value.encode('utf-8')
        occurrences += redacted_stdout.count(token) + redacted_stderr.count(token)
        redacted_stdout = redacted_stdout.replace(token, b'[redacted sensitive value]')
        redacted_stderr = redacted_stderr.replace(token, b'[redacted sensitive value]')
    return redacted_stdout, redacted_stderr, occurrences


def hash_file(path, limit=None):
    digest = hashlib.sha256()
    total = 0
    with Path(path).open('rb') as stream:
        while True:
            block = stream.read(64 * 1024)
            if not block:
                break
            total += len(block)
            if limit is not None and total > limit:
                raise RuntimeError(f'{path} exceeded its {limit}-byte limit')
            digest.update(block)
    return {'bytes': total, 'sha256': digest.hexdigest()}


def deterministic_payload(index, length=BINARY_BYTES_PER_COMMIT):
    if isinstance(index, bool) or not isinstance(index, int) or not 0 <= index < MAX_HISTORY_COMMITS:
        raise ValueError('history payload index is outside the fixed workload')
    if isinstance(length, bool) or not isinstance(length, int) or length != BINARY_BYTES_PER_COMMIT:
        raise ValueError('history payload length must use the fixed bounded size')
    seed = f'aethyme-concurrent-submit-v1:history:{index}'.encode('ascii')
    return hashlib.shake_256(seed).digest(length)


def validate_output_path(path, source_root):
    path = Path(path)
    if not path.is_absolute() or path.exists() or path.is_symlink():
        raise RuntimeError('output must be a new absolute directory path')
    parent = path.parent.resolve(strict=True)
    if not parent.is_dir():
        raise RuntimeError('output parent is not a directory')
    output = parent / path.name
    source_root = Path(source_root).resolve(strict=True)
    if output.is_relative_to(source_root):
        raise RuntimeError('output must be outside the source checkout')
    output.mkdir(mode=0o700)
    if output.is_symlink() or not output.is_dir():
        raise RuntimeError('output directory is not a private regular directory')
    return output.resolve(strict=True)


def fail_walk(error):
    raise RuntimeError(f'could not inspect directory tree: {error}') from error


def tree_size(root, max_files=20_000):
    total = 0
    count = 0
    for directory, names, files in os.walk(root, followlinks=False, onerror=fail_walk):
        for name in names:
            item = Path(directory) / name
            if item.is_symlink():
                raise RuntimeError(f'fixture contains an unexpected symlink: {item}')
        for name in files:
            item = Path(directory) / name
            info = item.lstat()
            if not stat.S_ISREG(info.st_mode):
                raise RuntimeError(f'fixture contains a non-regular file: {item}')
            count += 1
            total += info.st_size
            if count > max_files:
                raise RuntimeError('fixture exceeded its file-count limit')
            if total > MAX_OUTPUT_BYTES:
                raise RuntimeError('fixture exceeded its aggregate storage limit')
    return {'files': count, 'bytes': total}


def output_tree_size(root):
    total = 0
    count = 0
    for directory, names, files in os.walk(root, followlinks=False, onerror=fail_walk):
        for name in names:
            if (Path(directory) / name).is_symlink():
                raise RuntimeError('output contains an unexpected symlink')
        for name in files:
            item = Path(directory) / name
            info = item.lstat()
            if not stat.S_ISREG(info.st_mode):
                raise RuntimeError('output contains a non-regular file')
            count += 1
            total += info.st_size
            if count > 10_000 or total > MAX_OUTPUT_BYTES:
                raise RuntimeError('output exceeded its file-count or storage limit')
    return {'files': count, 'bytes': total}


class ProcessCleanupIncomplete(RuntimeError):
    """The harness could not prove its owned process group is absent."""


def require_waitid_support():
    required = ('waitid', 'P_PID', 'WEXITED', 'WNOHANG', 'WNOWAIT')
    missing = [name for name in required if not hasattr(os, name)]
    if missing:
        raise RuntimeError('safe process-group cleanup requires os.' + ', os.'.join(missing))


def leader_exited_without_reaping(process):
    """Observe our direct child while keeping its process-group ID anchored."""
    if process.returncode is not None:
        raise ProcessCleanupIncomplete('owned command leader was reaped before group cleanup')
    try:
        result = os.waitid(os.P_PID, process.pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
    except InterruptedError:
        return False
    except (ChildProcessError, OSError) as error:
        raise ProcessCleanupIncomplete(
            f'could not observe unreaped command leader: {error}') from error
    return result is not None and result.si_pid == process.pid


def wait_for_leader_exit(process, deadline):
    while True:
        if leader_exited_without_reaping(process):
            return True
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        time.sleep(min(0.01, remaining))


def process_group_absent(process_group, timeout=1.0):
    """Verify cleanup using signal 0 only after the group leader is reaped."""
    deadline = time.monotonic() + timeout
    last = 'process group remained present'
    while True:
        try:
            os.killpg(process_group, 0)
        except ProcessLookupError:
            return
        except PermissionError as error:
            last = f'process group state is inconclusive: {error}'
        except OSError as error:
            raise ProcessCleanupIncomplete(
                f'could not inspect process-group state: {error}') from error
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise ProcessCleanupIncomplete(last)
        time.sleep(min(0.02, remaining))


def terminate_owned_group(process, term_grace=0.2):
    """Stop descendants while the unreaped leader pins the owned process group."""
    require_waitid_support()
    if process.returncode is not None:
        process_group_absent(process.pid)
        return

    try:
        os.killpg(process.pid, signal.SIGTERM)
    except OSError:
        # The final signal-0 probe decides whether cleanup completed.
        pass

    if term_grace > 0:
        grace_deadline = time.monotonic() + term_grace
        while time.monotonic() < grace_deadline:
            # Observe without reaping, but let the whole group use the bounded
            # grace even if its direct leader exits before its descendants.
            leader_exited_without_reaping(process)
            time.sleep(min(0.01, grace_deadline - time.monotonic()))

    # Keep the direct child unreaped even if it exited during TERM grace. It
    # pins the original process-group identity while SIGKILL stops descendants.
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except OSError:
        # The final signal-0 probe decides whether cleanup completed.
        pass

    reap_deadline = time.monotonic() + 3.0
    while not leader_exited_without_reaping(process):
        remaining = reap_deadline - time.monotonic()
        if remaining <= 0:
            raise ProcessCleanupIncomplete(
                'owned command leader did not exit before cleanup deadline')
        time.sleep(min(0.01, remaining))

    try:
        process.wait()
    except OSError as error:
        raise ProcessCleanupIncomplete(
            f'could not reap observed command leader: {error}') from error

    # The final post-reap signal-0 probe is authoritative. A TERM/KILL
    # operation can report EPERM/ESRCH while the owned group is disappearing;
    # tolerate that only when group absence is then positively established.
    process_group_absent(process.pid)


class ReceiptLog:
    def __init__(self, output):
        self.output = output
        self.path = output / 'commands.json'
        self.rows = []
        self.lock = threading.Lock()

    def append(self, row):
        with self.lock:
            self.rows.append(row)
            encoded = canonical_json_bytes(self.rows)
            if len(encoded) > MAX_RECEIPTS_BYTES:
                self.rows.pop()
                raise RuntimeError('command receipts exceeded their aggregate size limit')
            temporary = self.path.with_suffix('.json.tmp')
            temporary.write_bytes(encoded)
            temporary.replace(self.path)


class BoundedCommandRunner:
    def __init__(self, fixture_root, output, env, receipts):
        self.root = fixture_root.resolve(strict=True)
        self.output = output.resolve(strict=True)
        self.env = dict(env)
        self.receipts = receipts
        self.count = 0
        self.lock = threading.Lock()
        self.cleanup_incomplete = False

    def run(self, argv, cwd, label, timeout=COMMAND_TIMEOUT_SECONDS,
            trace_path=None, input_bytes=None, barrier=None, redact_values=()):
        if self.cleanup_incomplete:
            raise RuntimeError('scenario stopped after uncertain process cleanup')
        require_waitid_support()
        if not isinstance(argv, (list, tuple)) or not argv or not all(
                isinstance(arg, str) for arg in argv):
            raise RuntimeError('command argv must be a non-empty string sequence')
        if not isinstance(redact_values, (tuple, list)) or any(
                not isinstance(value, str) or not value for value in redact_values):
            raise RuntimeError('redaction values must be nonempty strings')
        if any(value in arg for value in redact_values for arg in argv):
            raise RuntimeError('a sensitive value appeared in command arguments')
        cwd = fixture_path(cwd, self.root, 'command working directory', must_exist=True)
        validate_fixture_environment(self.env, self.root)
        with self.lock:
            self.count += 1
            if self.count > MAX_COMMANDS:
                raise RuntimeError('scenario exceeded its command-count limit')
        child_env = dict(self.env)
        trace_path = Path(trace_path) if trace_path is not None else None
        if trace_path is not None:
            if not trace_path.is_absolute() or not trace_path.parent.resolve().is_relative_to(self.output):
                raise RuntimeError('Trace2 output path escaped the requested output directory')
            if trace_path.exists() or trace_path.is_symlink():
                raise RuntimeError('Trace2 output path already exists')
            child_env['GIT_TRACE2_EVENT'] = str(trace_path)
        if input_bytes is not None and (not isinstance(input_bytes, bytes)
                                        or len(input_bytes) > 64 * 1024):
            raise RuntimeError('command input exceeded its 64 KiB limit')
        if input_bytes is not None and any(
                value.encode('utf-8') in input_bytes for value in redact_values):
            raise RuntimeError('a sensitive value appeared in command input')

        barrier_ready_ns = None
        if barrier is not None:
            try:
                barrier.wait(timeout=PAIR_BARRIER_SECONDS)
            except threading.BrokenBarrierError as error:
                raise RuntimeError('concurrent submit start barrier timed out') from error
            barrier_ready_ns = time.monotonic_ns()
        started_ns = time.monotonic_ns()
        started_wall = time.time_ns()
        stdout = bytearray()
        stderr = bytearray()
        failure = None
        process = None
        cleanup_attempted = False
        selector = selectors.DefaultSelector()
        try:
            process = subprocess.Popen(
                argv, cwd=cwd, env=child_env,
                stdin=subprocess.PIPE if input_bytes is not None else subprocess.DEVNULL,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, close_fds=True,
                start_new_session=True,
            )
            for stream, label_name in ((process.stdout, 'stdout'), (process.stderr, 'stderr')):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, label_name)
            input_offset = 0
            if input_bytes is not None:
                os.set_blocking(process.stdin.fileno(), False)
                if input_bytes:
                    selector.register(process.stdin, selectors.EVENT_WRITE, 'stdin')
                else:
                    process.stdin.close()
            deadline = time.monotonic() + timeout
            while selector.get_map():
                if trace_path is not None and trace_path.exists():
                    if trace_path.is_symlink() or trace_path.stat().st_size > MAX_TRACE2_BYTES:
                        failure = 'Trace2 output exceeded its size/type limit'
                        break
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    failure = f'command exceeded {timeout} seconds'
                    break
                events = selector.select(min(0.05, remaining))
                for key, _ in events:
                    if key.data == 'stdin':
                        try:
                            amount = os.write(process.stdin.fileno(), input_bytes[input_offset:input_offset + 64 * 1024])
                        except BrokenPipeError:
                            failure = 'child closed bounded stdin before consuming it'
                            break
                        input_offset += amount
                        if input_offset == len(input_bytes):
                            selector.unregister(process.stdin)
                            process.stdin.close()
                    else:
                        target = stdout if key.data == 'stdout' else stderr
                        limit = MAX_STDOUT_BYTES if key.data == 'stdout' else MAX_STDERR_BYTES
                        try:
                            chunk = os.read(key.fileobj.fileno(), min(64 * 1024, limit - len(target) + 1))
                        except BlockingIOError:
                            continue
                        if not chunk:
                            selector.unregister(key.fileobj)
                            key.fileobj.close()
                            continue
                        remaining_capacity = limit - len(target)
                        target.extend(chunk[:remaining_capacity])
                        if len(chunk) > remaining_capacity:
                            failure = f'{key.data} exceeded its {limit}-byte limit'
                            break
                if failure is not None:
                    break
            if failure is None:
                try:
                    exited = wait_for_leader_exit(process, deadline)
                except ProcessCleanupIncomplete as error:
                    self.cleanup_incomplete = True
                    failure = f'command cleanup ownership became uncertain: {error}'
                else:
                    if not exited:
                        failure = f'command remained alive after {timeout} seconds'
                    else:
                        cleanup_attempted = True
                        try:
                            # The command is complete. Kill any unexpected
                            # in-group descendant immediately, without adding
                            # the failure-path TERM grace to measured latency.
                            terminate_owned_group(process, term_grace=0)
                        except ProcessCleanupIncomplete as error:
                            self.cleanup_incomplete = True
                            failure = f'command process group was not quiescent: {error}'
                        except (OSError, RuntimeError) as error:
                            failure = f'command process cleanup failed: {error}'
        except OSError as error:
            failure = (f'command supervision failed: {error}' if process is not None
                       else f'could not start command: {error}')
        finally:
            cleanup_error = None
            if (process is not None and process.returncode is None
                    and not cleanup_attempted):
                if failure is None:
                    failure = 'command runner exited before its child completed'
                cleanup_attempted = True
                try:
                    terminate_owned_group(process)
                except ProcessCleanupIncomplete as error:
                    self.cleanup_incomplete = True
                    cleanup_error = str(error)
                except (OSError, RuntimeError) as error:
                    cleanup_error = str(error)
            try:
                for key in list(selector.get_map().values()):
                    try:
                        selector.unregister(key.fileobj)
                    except Exception:
                        pass
                    try:
                        key.fileobj.close()
                    except Exception:
                        pass
                selector.close()
            finally:
                if process is not None:
                    for stream in (process.stdin, process.stdout, process.stderr):
                        if stream is not None and not stream.closed:
                            stream.close()

        if cleanup_error is not None:
            failure = (f'{failure}; process cleanup failed: {cleanup_error}'
                       if failure is not None else f'process cleanup failed: {cleanup_error}')
        if self.cleanup_incomplete:
            failure = (f'{failure}; fixture root retained at {self.root}'
                       if failure is not None else
                       f'process cleanup was incomplete; fixture root retained at {self.root}')

        ended_ns = time.monotonic_ns()
        ended_wall = time.time_ns()
        returncode = process.returncode if process is not None else None
        if returncode not in (0, None) and failure is None:
            failure = f'command exited with status {returncode}'
        trace_info = None
        if trace_path is not None and trace_path.exists() and not trace_path.is_symlink():
            trace_info = hash_file(trace_path, MAX_TRACE2_BYTES)
        raw_stdout = bytes(stdout)
        raw_stderr = bytes(stderr)
        try:
            raw_stdout.decode('utf-8')
        except UnicodeDecodeError:
            stdout_utf8_valid = False
        else:
            stdout_utf8_valid = True
        receipt_stdout, receipt_stderr, redacted_occurrences = redact_captured_output(
            raw_stdout, raw_stderr, redact_values)
        row = {
            'label': label, 'argv': list(argv), 'cwd': str(cwd),
            'started_wall_time_ns': started_wall, 'duration_ms': (ended_ns - started_ns) / 1_000_000,
            'start_monotonic_ns': started_ns, 'end_monotonic_ns': ended_ns,
            'barrier_ready_monotonic_ns': barrier_ready_ns, 'exit_code': returncode,
            'failure': failure, 'stdout_complete': failure is None,
            'stdout_utf8_valid': stdout_utf8_valid,
            'stdout_bytes': len(raw_stdout), 'stdout_sha256': sha256_bytes(raw_stdout),
            'stdout': receipt_stdout.decode('utf-8', errors='replace'),
            'stderr_bytes': len(raw_stderr), 'stderr_sha256': sha256_bytes(raw_stderr),
            'stderr': receipt_stderr.decode('utf-8', errors='replace'),
            'stdin_bytes': len(input_bytes) if input_bytes is not None else 0,
            'stdin_sha256': sha256_bytes(input_bytes) if input_bytes is not None else None,
            'environment_sha256': sha256_bytes(canonical_json_bytes(child_env)),
            'trace2_path': str(trace_path) if trace_path is not None else None,
            'trace2': trace_info,
        }
        if redact_values:
            row['redacted_value_occurrences'] = redacted_occurrences
        self.receipts.append(row)
        if failure is not None:
            raise RuntimeError(f'{label}: {failure}')
        return row


def parse_trace2(path):
    info = hash_file(path, MAX_TRACE2_BYTES)
    starts = []
    full_patch_logs = []
    with Path(path).open('r', encoding='utf-8') as stream:
        for line_number, line in enumerate(stream, start=1):
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError as error:
                raise RuntimeError(f'malformed Trace2 JSON at line {line_number}') from error
            if not isinstance(event, dict):
                raise RuntimeError(f'Trace2 event at line {line_number} is not an object')
            argv = event.get('argv')
            if event.get('event') in {'start', 'child_start'} and isinstance(argv, list) and argv:
                normalized = [str(arg) for arg in argv]
                starts.append({'event': event['event'], 'argv': normalized,
                               'sid': event.get('sid'), 'child_id': event.get('child_id')})
                executable = Path(normalized[0]).name.lower()
                git_args = normalized[1:] if executable in {'git', 'git.exe'} else normalized
                if (git_args and git_args[0] == 'log'
                        and '-p' in git_args and '--stdin' in git_args):
                    full_patch_logs.append({'event': event['event'], 'argv': normalized,
                                            'sid': event.get('sid'),
                                            'child_id': event.get('child_id')})
    if not starts:
        raise RuntimeError(f'{path} contains no Git Trace2 process starts')
    return {'bytes': info['bytes'], 'sha256': info['sha256'],
            'git_process_start_count': len(starts), 'git_process_starts': starts,
            'full_patch_log_start_count': len(full_patch_logs),
            'full_patch_log_starts': full_patch_logs}


def validate_submit_outcome(payload, session_id, baseline, integration_head, session_head,
                             expected_owned_commits=None):
    if not isinstance(payload, dict) or not isinstance(payload.get('submission_plan'), dict):
        raise RuntimeError('submit JSON omitted its complete submission_plan')
    plan = payload['submission_plan']
    if (plan.get('session_id') != session_id or plan.get('recorded_baseline') != baseline
            or plan.get('integration_head') != integration_head
            or plan.get('session_head') != session_head or plan.get('safe') is not True):
        raise RuntimeError('submit plan provenance did not match the frozen fixture heads')
    commits = plan.get('commits')
    if not isinstance(commits, list):
        raise RuntimeError('submit plan omitted commit provenance')
    owned = [commit for commit in commits if isinstance(commit, dict)
             and commit.get('ownership') == 'session_owned']
    owned_ids = [commit.get('commit') for commit in owned]
    expected = expected_owned_commits if expected_owned_commits is not None else [session_head]
    if owned_ids != expected or session_head not in owned_ids:
        raise RuntimeError('submit plan did not preserve the expected session commit provenance')
    if (payload.get('promoted') is not False or payload.get('no_changes') is not False
            or payload.get('conflicts') != []):
        raise RuntimeError('submit did not complete as a clean, unpromoted manual submission')
    entry = payload.get('entry')
    gate = payload.get('gate_verification')
    if (not isinstance(entry, dict) or entry.get('status') != 'verified'
            or not isinstance(gate, dict) or gate.get('status') != 'no_configuration'
            or gate.get('configured_gates') != 0 or gate.get('selected_gates') != 0
            or gate.get('executed_gates') != 0 or gate.get('cached_gates') != 0):
        raise RuntimeError('submit did not follow the no-gates conflict-only policy')
    return plan


def make_env(root, tools, binary, git, remote):
    root = Path(root).resolve(strict=True)
    home = root / 'home'
    env = {
        'PATH': os.pathsep.join((str(tools), str(binary.parent), str(Path(git).parent),
                                 '/usr/bin', '/bin', '/usr/sbin', '/sbin')),
        'HOME': str(home), 'TMPDIR': str(root / 'tmp'), 'TMP': str(root / 'tmp'),
        'TEMP': str(root / 'tmp'), 'XDG_CONFIG_HOME': str(home / 'config'),
        'XDG_DATA_HOME': str(home / 'data'), 'XDG_STATE_HOME': str(home / 'state'),
        'XDG_CACHE_HOME': str(home / 'cache'),
        'AETHYME_HOST_STATE_DIR': str(root / 'host-state'),
        'AETHYME_HOST_CACHE_DIR': str(root / 'host-cache'),
        'AETHYME_WORKTREE_ROOT': str(root / 'worktrees'),
        'AETHYME_PREPARATION_CACHE_ROOT': str(root / 'preparation'),
        'GIT_CONFIG_NOSYSTEM': '1', 'GIT_CONFIG_GLOBAL': os.devnull,
        'GIT_TERMINAL_PROMPT': '0', 'GIT_AUTHOR_NAME': 'Concurrent Submit Fixture',
        'GIT_AUTHOR_EMAIL': 'fixture@example.invalid',
        'GIT_COMMITTER_NAME': 'Concurrent Submit Fixture',
        'GIT_COMMITTER_EMAIL': 'fixture@example.invalid',
        'GIT_AUTHOR_DATE': FIXED_GIT_DATE, 'GIT_COMMITTER_DATE': FIXED_GIT_DATE,
        'TZ': 'UTC', 'LATENCY_GIT': str(git), 'LATENCY_REMOTE': str(remote),
    }
    if 'AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS' in env:
        raise RuntimeError('test-only trust escape must never be set')
    validate_fixture_environment(env, root)
    return env


def create_shims(tools):
    tools.mkdir(mode=0o700)
    ssh = tools / 'ssh'
    ssh.write_text('#!/bin/sh\ncase "$*" in\n'
                   ' *git-upload-pack*) exec "$LATENCY_GIT" upload-pack "$LATENCY_REMOTE" ;;\n'
                   ' *git-receive-pack*) exec "$LATENCY_GIT" receive-pack "$LATENCY_REMOTE" ;;\n'
                   ' *) exit 64 ;;\nesac\n')
    gh = tools / 'gh'
    gh.write_text('#!/bin/sh\ncase "$*" in\n'
                  ' "pr list"*) printf "[]\\n" ;;\n'
                  ' *) exit 64 ;;\nesac\n')
    for item in (ssh, gh):
        item.chmod(0o700)


def run_git(runner, git, cwd, label, *args, input_bytes=None):
    return runner.run([git, *args], cwd, label, input_bytes=input_bytes)['stdout']


def start_session(runner, binary, repo, root, scenario, role, baseline):
    label = f'{scenario}:start:{role}'
    row = runner.run(
        [str(binary), 'broker', 'start', '--task', f'synthetic shared-history {role}',
         '--short-name', f'{scenario}-{role}', '--agent',
         f'Concurrent Submit {role} <{role}@example.invalid>', '--json'],
        repo, label)
    payload = json.loads(row['stdout'])
    session_id = payload.get('id')
    worktree_raw = payload.get('worktree_path')
    start_base = payload.get('start_base')
    if isinstance(session_id, bool) or not isinstance(session_id, int):
        raise RuntimeError('broker start did not return a numeric session id')
    if not isinstance(worktree_raw, str) or not isinstance(start_base, dict):
        raise RuntimeError('broker start omitted worktree or base provenance')
    worktree = fixture_path(Path(worktree_raw), root, 'broker-reported session worktree',
                            must_exist=True)
    expected_worktrees = fixture_path(root / 'worktrees', root, 'fixture worktree root',
                                      must_exist=True)
    if not worktree.is_relative_to(expected_worktrees) or start_base.get('commit') != baseline:
        raise RuntimeError('broker session escaped its fixture or started from another baseline')
    return {'role': role, 'id': session_id, 'worktree': str(worktree),
            'branch': payload.get('branch'), 'start_base': start_base}


def validate_shared_history(seed_commits, first_parent_commits, reachable_commits,
                            integration, seed_tree, integration_tree):
    # Content promotion may rewrite source commits; preserve the exact seed
    # tree and record ancestry independently of first-parent patch candidates.
    if (not first_parent_commits or len(first_parent_commits) > 16
            or first_parent_commits[-1] != integration
            or len(reachable_commits) > MAX_HISTORY_COMMITS + 16
            or len(set(seed_commits)) != MAX_HISTORY_COMMITS
            or len(set(first_parent_commits)) != len(first_parent_commits)
            or len(set(reachable_commits)) != len(reachable_commits)
            or not set(first_parent_commits) <= set(reachable_commits)
            or not seed_tree or integration_tree != seed_tree):
        raise RuntimeError('promoted shared history did not preserve bounded seed content')


def commit_file(runner, git, worktree, relative_path, data, message, label):
    path = worktree / relative_path
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    run_git(runner, git, worktree, f'{label}:git-add', 'add', '--', relative_path)
    run_git(runner, git, worktree, f'{label}:git-commit', 'commit', '-q', '-m', message)
    head = run_git(runner, git, worktree, f'{label}:head', 'rev-parse', 'HEAD').strip()
    if len(head) != 40 or any(ch not in '0123456789abcdef' for ch in head):
        raise RuntimeError('fixture Git returned a malformed commit id')
    return head


@contextmanager
def scenario_fixture(name, output):
    root = Path(tempfile.mkdtemp(prefix=f'.fixture-{name}-', dir=output)).resolve(strict=True)
    state = {'runner': None}
    try:
        yield root, state
    finally:
        runner = state['runner']
        if runner is None or not runner.cleanup_incomplete:
            shutil.rmtree(root)


@contextmanager
def setup_scenario(name, binary, git, output, receipts):
    with scenario_fixture(name, output) as (root, fixture_state):
        repo, home, tools, remote, trace_dir = (
            root / key for key in ('repo', 'home', 'bin', 'remote.git', 'trace2'))
        for path in (repo, home, root / 'tmp', root / 'preparation', root / 'host-state',
                     root / 'host-cache', root / 'worktrees', trace_dir):
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
        create_shims(tools)
        env = make_env(root, tools, binary, git, remote)
        runner = BoundedCommandRunner(root, output, env, receipts)
        fixture_state['runner'] = runner
        git_version = run_git(runner, git, repo, f'{name}:git-version', '--version').strip()
        run_git(runner, git, root, f'{name}:init-remote', 'init', '--bare', '-q', '-b',
                'main', str(remote))
        run_git(runner, git, repo, f'{name}:init-repo', 'init', '-q', '--template=', '-b', 'main')
        run_git(runner, git, repo, f'{name}:config-ssh', 'config', 'core.sshCommand',
                shlex.quote(str(tools / 'ssh')))
        run_git(runner, git, repo, f'{name}:config-identity-name', 'config', 'user.name',
                'Concurrent Submit Fixture')
        run_git(runner, git, repo, f'{name}:config-identity-email', 'config', 'user.email',
                'fixture@example.invalid')
        run_git(runner, git, repo, f'{name}:config-signing', 'config', 'commit.gpgSign', 'false')
        (repo / '.gitignore').write_text('.aethyme/*\n!.aethyme/config.toml\n')
        (repo / '.aethyme').mkdir()
        (repo / '.aethyme/config.toml').write_text(POLICY_TEXT)
        (repo / 'README.md').write_text('# Synthetic concurrent submit fixture\n')
        run_git(runner, git, repo, f'{name}:stage-baseline', 'add', '--all')
        run_git(runner, git, repo, f'{name}:commit-baseline', 'commit', '-q', '-m',
                'chore(perf): seed concurrent-submit Playground')
        baseline = run_git(runner, git, repo, f'{name}:baseline-head', 'rev-parse', 'HEAD').strip()
        run_git(runner, git, repo, f'{name}:create-integration', 'branch', 'aethyme/integration', baseline)
        run_git(runner, git, repo, f'{name}:add-origin', 'remote', 'add', 'origin',
                'git@github.com:playground/concurrent-submit.git')
        run_git(runner, git, repo, f'{name}:push-default', 'push', '-q', 'origin', 'main')

        sessions = {
            role: start_session(runner, binary, repo, root, name, role, baseline)
            for role in ('seed', 'worker-a', 'worker-b')
        }
        seed_worktree = Path(sessions['seed']['worktree'])
        seed_commits = []
        for index in range(MAX_HISTORY_COMMITS):
            payload = deterministic_payload(index)
            seed_commits.append(commit_file(
                runner, git, seed_worktree,
                f'history/payload-{index:02d}.bin', payload,
                f'perf: add deterministic history payload {index:02d}',
                f'{name}:history-{index:02d}'))

        seed_submit = runner.run(
            [str(binary), 'broker', 'submit', '--session', str(sessions['seed']['id']), '--json'],
            seed_worktree, f'{name}:seed-submit')
        seed_outcome = json.loads(seed_submit['stdout'])
        seed_plan = validate_submit_outcome(
            seed_outcome, sessions['seed']['id'], baseline, baseline, seed_commits[-1],
            expected_owned_commits=seed_commits)
        if len([c for c in seed_plan['commits'] if c.get('ownership') == 'session_owned']) != MAX_HISTORY_COMMITS:
            raise RuntimeError('seed submission plan omitted deterministic history commits')
        entry_id = seed_outcome['entry']['id']
        promote_row = runner.run(
            [str(binary), 'broker', 'submit', 'promote', '--entry', str(entry_id), '--json'],
            repo, f'{name}:seed-explicit-manual-promotion')
        promote_payload = json.loads(promote_row['stdout'])
        if promote_payload.get('promoted') != entry_id:
            raise RuntimeError('manual seed promotion did not return its entry identity')
        integration = run_git(runner, git, repo, f'{name}:integration-head', 'rev-parse',
                              'aethyme/integration').strip()
        shared_commits = run_git(
            runner, git, repo, f'{name}:shared-first-parent-candidates', 'rev-list',
            '--first-parent', '--reverse', integration, '--not', baseline).splitlines()
        reachable_commits = run_git(
            runner, git, repo, f'{name}:shared-reachable-commits', 'rev-list',
            '--reverse', integration, '--not', baseline).splitlines()
        seed_tree = run_git(runner, git, repo, f'{name}:seed-tree',
                            'rev-parse', f'{seed_commits[-1]}^{{tree}}').strip()
        integration_tree = run_git(runner, git, repo, f'{name}:integration-tree',
                                   'rev-parse', f'{integration}^{{tree}}').strip()
        validate_shared_history(seed_commits, shared_commits, reachable_commits,
                                integration, seed_tree, integration_tree)

        worker_commits = {}
        for role in ('worker-a', 'worker-b'):
            session = sessions[role]
            worktree = Path(session['worktree'])
            if run_git(runner, git, worktree, f'{name}:{role}:precommit-head', 'rev-parse',
                       'HEAD').strip() != baseline:
                raise RuntimeError(f'{role} worktree no longer reflects its recorded baseline')
            worker_commits[role] = commit_file(
                runner, git, worktree, f'workers/{role}.txt',
                f'deterministic session tail for {role}\n'.encode('ascii'),
                f'perf: add {role} tail', f'{name}:{role}')

        policy_path = repo / '.aethyme' / 'config.toml'
        gates_path = repo / '.aethyme' / 'gates.toml'
        if gates_path.exists() or policy_path.read_text() != POLICY_TEXT:
            raise RuntimeError('fixture policy drifted from the reviewed manual/no-gates contract')
        if 'AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS' in env:
            raise RuntimeError('noninteractive test trust escape is forbidden')

        workload = {
            'scenario': name,
            'fixture_root': str(root),
            'repository': str(repo),
            'remote': str(remote),
            'baseline_commit': baseline,
            'integration_commit': integration,
            'seed_session': sessions['seed'],
            'worker_sessions': {role: sessions[role] for role in ('worker-a', 'worker-b')},
            'seed_commits': seed_commits,
            'shared_first_parent_candidate_commits': shared_commits,
            'shared_reachable_commits': reachable_commits,
            'seed_tree': seed_tree,
            'integration_tree': integration_tree,
            'seed_content_preserved': True,
            'worker_commits': worker_commits,
            'binary_payload_bytes_each': BINARY_BYTES_PER_COMMIT,
            'binary_payload_count': MAX_HISTORY_COMMITS,
            'binary_payloads_sha256': [sha256_bytes(deterministic_payload(i))
                                       for i in range(MAX_HISTORY_COMMITS)],
            'policy': {
                'sha256': sha256_bytes(POLICY_TEXT.encode()),
                'promote_mode': 'manual', 'gates_file_present': False,
                'gate_trust_escape_present': False,
                'submit_semantics': 'no gates configured; conflict-only submit; no gate command run',
            },
            'environment': env,
            'git_version': git_version,
            'fixture_storage_after_setup': tree_size(root),
        }
        snapshot_dir = output / 'provenance'
        snapshot_dir.mkdir(mode=0o700, exist_ok=True)
        (snapshot_dir / f'{name}.json').write_bytes(canonical_json_bytes(workload))
        yield runner, root, repo, sessions, baseline, integration, shared_commits, worker_commits, workload


def audit_patch_stream(runner, git, repo, label, commits):
    if not commits or len(commits) > 16:
        raise RuntimeError('patch audit requires between 1 and 16 exact commit ids')
    if any(len(oid) != 40 or any(ch not in '0123456789abcdef' for ch in oid)
           for oid in commits):
        raise RuntimeError('patch audit received a malformed commit id')
    stdin = ('\n'.join(commits) + '\n').encode('ascii')
    row = runner.run(
        [git, 'log', '--no-walk=unsorted', '--stdin', '--no-color', '-p', '--binary',
         '--diff-merges=first-parent', '--format=commit %H'],
        repo, label, input_bytes=stdin)
    data = row['stdout'].encode('utf-8')
    if len(data) > MAX_PATCH_AUDIT_BYTES:
        raise RuntimeError('common-history patch stream exceeded its audit limit')
    return {'commit_ids': commits, 'stdin_sha256': sha256_bytes(stdin),
            'stream_bytes': len(data), 'stream_sha256': sha256_bytes(data),
            'argv': row['argv']}


def command_overlap_ns(rows):
    if len(rows) != 2:
        return None
    starts = [row.get('start_monotonic_ns') for row in rows]
    ends = [row.get('end_monotonic_ns') for row in rows]
    if any(type(value) is not int for value in starts + ends):
        raise RuntimeError('concurrent submit receipt omitted monotonic command bounds')
    if any(end < start for start, end in zip(starts, ends)):
        raise RuntimeError('concurrent submit receipt has reversed monotonic bounds')
    return max(0, min(ends) - max(starts))


def concurrent_pair_claim(overlap_observed):
    if overlap_observed:
        return (
            'two submit runner-call windows overlapped (including spawn, output drain, and owned-group cleanup); '
            'this does not demonstrate overlapping broker-side execution; no speedup claim'
        )
    return (
        'concurrent pair invocation attempted but runner-call window overlap was not observed; '
        'comparison is inconclusive and makes no speedup claim'
    )


def _measure_submit_scenario(name, binary, git, output, receipts, mode, setup):
    runner, root, repo, sessions, baseline, integration, shared_commits, worker_commits, workload = setup
    if mode == 'single_control':
        workers = ('worker-a',)
    elif mode == 'concurrent_pair':
        workers = ('worker-a', 'worker-b')
    else:
        raise ValueError('unknown submit scenario')
    barrier = threading.Barrier(len(workers)) if len(workers) > 1 else None
    command_rows = {}
    plan_paths = {}

    def submit(role):
        session = sessions[role]
        trace_path = output / 'trace2' / f'{name}-{role}.jsonl'
        row = runner.run(
            [str(binary), 'broker', 'submit', '--session', str(session['id']), '--json'],
            session['worktree'], f'{name}:submit:{role}', trace_path=trace_path,
            barrier=barrier)
        payload = json.loads(row['stdout'])
        plan = validate_submit_outcome(
            payload, session['id'], baseline, integration, worker_commits[role])
        plan_path = output / 'provenance' / f'{name}-{role}-submission-plan.json'
        plan_path.write_bytes(canonical_json_bytes(plan))
        plan_paths[role] = {'path': str(plan_path), **hash_file(plan_path)}
        command_rows[role] = row
        return {'role': role, 'session_id': session['id'], 'entry_id': payload['entry']['id'],
                'duration_ms': row['duration_ms'],
                'start_monotonic_ns': row['start_monotonic_ns'],
                'end_monotonic_ns': row['end_monotonic_ns'],
                'trace2': parse_trace2(trace_path), 'plan_path': plan_paths[role],
                'gate_verification': payload['gate_verification'],
                'submission_plan': plan}

    if barrier is None:
        results = [submit(workers[0])]
    else:
        import concurrent.futures
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
            futures = [executor.submit(submit, role) for role in workers]
            results = [future.result() for future in futures]
    if run_git(runner, git, repo, f'{name}:integration-after-measurement',
               'rev-parse', 'aethyme/integration').strip() != integration:
        raise RuntimeError('manual worker submissions moved the shared integration head')

    history_audit = audit_patch_stream(
        runner, git, repo, f'{name}:audit-shared-integration-patch-stream', shared_commits)
    for result in results:
        owned = [commit['commit'] for commit in result['submission_plan']['commits']
                 if commit['ownership'] == 'session_owned']
        result['session_patch_stream'] = audit_patch_stream(
            runner, git, repo, f"{name}:audit-{result['role']}-patch-stream", owned)
    overlap_ns = command_overlap_ns(results)
    overlap_observed = overlap_ns is not None and overlap_ns > 0
    workload.update({
        'measured_submits': results,
        'measured_submit_count': len(results),
        'manual_mode_kept_integration_fixed': True,
        'shared_integration_patch_stream': history_audit,
        'pair_command_overlap_ms': overlap_ns / 1_000_000 if overlap_ns is not None else None,
        'pair_command_overlap_observed': overlap_observed,
        'pair_command_overlap_basis': (
            'bounded runner call from before spawn through output drain and owned-group cleanup; not exact child execution time'
        ),
        'fixture_storage_after_measurement': tree_size(root),
    })
    (output / 'provenance' / f'{name}.json').write_bytes(canonical_json_bytes(workload))
    return workload


def run_submit_scenario(name, binary, git, output, receipts, mode):
    with setup_scenario(name, binary, git, output, receipts) as setup:
        return _measure_submit_scenario(name, binary, git, output, receipts, mode, setup)


def run(binary, output):
    if os.name != 'posix':
        raise RuntimeError('this bounded diagnostic currently requires POSIX process-group cleanup')
    git_raw = shutil.which('git')
    if not git_raw:
        raise RuntimeError('Git is required')
    git = str(Path(git_raw).resolve(strict=True))
    source_root = Path(__file__).resolve().parents[4]
    output = validate_output_path(output, source_root)
    (output / 'provenance').mkdir(mode=0o700)
    (output / 'trace2').mkdir(mode=0o700)
    receipts = ReceiptLog(output)
    if not binary.is_file() or binary.stat().st_size > MAX_BINARY_BYTES:
        raise RuntimeError('CLI binary is not a regular file within the 256 MiB limit')
    binary_info = hash_file(binary, MAX_BINARY_BYTES)
    engine = binary.with_name('aethyme-engine')
    engine_info = None
    if engine.is_file():
        if engine.stat().st_size > MAX_BINARY_BYTES:
            raise RuntimeError('engine sibling exceeds the 256 MiB limit')
        engine_info = hash_file(engine, MAX_BINARY_BYTES)
    binary_row = {
        'argument': str(binary), 'resolved_path': str(binary.resolve(strict=True)),
        'sha256': binary_info['sha256'], 'bytes': binary_info['bytes'],
        'engine_sibling': str(engine) if engine_info else None,
        'engine_sha256': engine_info['sha256'] if engine_info else None,
    }
    scenarios = {}
    scenarios['single_control'] = run_submit_scenario(
        'single-control', binary, git, output, receipts, 'single_control')
    scenarios['concurrent_pair'] = run_submit_scenario(
        'concurrent-pair', binary, git, output, receipts, 'concurrent_pair')
    if output_tree_size(output)['bytes'] > MAX_OUTPUT_BYTES:
        raise RuntimeError('completed diagnostic exceeded its aggregate output limit')
    if scenarios['single_control']['baseline_commit'] != scenarios['concurrent_pair']['baseline_commit']:
        raise RuntimeError('scenario fixtures did not use the same deterministic baseline')
    if scenarios['single_control']['integration_commit'] != scenarios['concurrent_pair']['integration_commit']:
        raise RuntimeError('scenario fixtures did not produce identical shared integration history')
    report = {
        'schema_version': 1,
        'profile': 'synthetic_shared_binary_history_single_control_and_concurrent_pair',
        'enforcement': 'diagnostic_only',
        'claim': (
            'one synthetic single-submit control and ' +
            concurrent_pair_claim(scenarios['concurrent_pair']['pair_command_overlap_observed'])
        ),
        'binary': binary_row,
        'git': {'path': git, 'version': scenarios['single_control']['git_version']},
        'harness': {
            'path': str(Path(__file__).resolve()),
            'sha256': hash_file(Path(__file__), 2 * 1024 * 1024)['sha256'],
        },
        'scenarios': scenarios,
        'comparison': {
            'baseline_commit_equal': True,
            'integration_commit_equal': True,
            'shared_candidate_commits_equal': (
                scenarios['single_control']['shared_first_parent_candidate_commits']
                == scenarios['concurrent_pair']['shared_first_parent_candidate_commits']),
            'shared_patch_stream_bytes': {
                key: scenarios[key]['shared_integration_patch_stream']['stream_bytes']
                for key in scenarios
            },
            'single_submit_wall_ms': scenarios['single_control']['measured_submits'][0]['duration_ms'],
            'concurrent_submit_wall_ms': [item['duration_ms'] for item in
                                          scenarios['concurrent_pair']['measured_submits']],
            'concurrent_pair_overlap_ms': scenarios['concurrent_pair']['pair_command_overlap_ms'],
            'concurrent_pair_overlap_observed': scenarios['concurrent_pair']['pair_command_overlap_observed'],
            'concurrent_pair_overlap_basis': scenarios['concurrent_pair']['pair_command_overlap_basis'],
            'concurrent_broker_execution_qualified': False,
            'concurrent_git_process_starts': [
                item['trace2']['git_process_start_count']
                for item in scenarios['concurrent_pair']['measured_submits']],
            'concurrent_full_patch_log_starts': [
                item['trace2']['full_patch_log_start_count']
                for item in scenarios['concurrent_pair']['measured_submits']],
        },
        'bounds': {
            'history_commits': MAX_HISTORY_COMMITS,
            'binary_bytes_per_commit': BINARY_BYTES_PER_COMMIT,
            'command_timeout_seconds': COMMAND_TIMEOUT_SECONDS,
            'max_commands_per_scenario': MAX_COMMANDS,
            'max_stdout_bytes': MAX_STDOUT_BYTES,
            'max_stderr_bytes': MAX_STDERR_BYTES,
            'max_trace2_bytes_per_submit': MAX_TRACE2_BYTES,
            'max_total_output_bytes': MAX_OUTPUT_BYTES,
        },
        'network_and_policy': {
            'transport': 'fixture-local bare Git remote via a strict SSH shim',
            'gh': 'local shim; any command other than pr list fails',
            'provider_credentials_forwarded': False,
            'provider_calls_requested': False,
            'network_sandbox': 'not provided; this harness does not claim OS-level network isolation',
            'trust_escape_set': False,
            'gate_policy': 'no gates.toml; submit must report no_configuration and execute zero gates',
            'promotion_policy': 'manual; seed promotion uses explicit broker submit promote; measured submits remain unpromoted',
        },
        'limitations': [
            'synthetic local workload only; not an evaluation or a production latency estimate',
            'Trace2 instrumentation is retained for process attribution; no timed trace-off comparison is made',
            'one control and one pair are diagnostic samples, not a statistical benchmark',
            'overlap is measured across runner calls, including spawn, pipe drain, and cleanup; it is not exact child execution time',
            'the local SSH shim does not provide OS-level network isolation',
        ],
    }
    report_bytes = canonical_json_bytes(report)
    if len(report_bytes) > 2 * 1024 * 1024:
        raise RuntimeError('report exceeded its size limit')
    (output / 'report.json').write_bytes(report_bytes)
    summary = {
        'profile': report['profile'], 'enforcement': 'diagnostic_only',
        'single_submit_wall_ms': report['comparison']['single_submit_wall_ms'],
        'concurrent_submit_wall_ms': report['comparison']['concurrent_submit_wall_ms'],
        'concurrent_pair_overlap_ms': report['comparison']['concurrent_pair_overlap_ms'],
        'shared_history_patch_bytes': report['comparison']['shared_patch_stream_bytes'],
        'concurrent_full_patch_log_starts': report['comparison']['concurrent_full_patch_log_starts'],
        'report_sha256': sha256_bytes(report_bytes),
        'summary': (
            'Synthetic fixture only; runner-call window overlap observed, not broker-side execution proof; '
            'no speedup claim; full plans and raw Trace2 retained.'
            if report['comparison']['concurrent_pair_overlap_observed'] else
            'Synthetic fixture only; runner-call overlap not observed, comparison inconclusive; '
            'full plans and raw Trace2 retained.'
        ),
    }
    (output / 'summary.json').write_bytes(canonical_json_bytes(summary))
    output_tree_size(output)
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True,
                        help='absolute path to the paired Aethyme CLI binary')
    parser.add_argument('--output', type=Path, required=True,
                        help='new absolute output directory; parent must already exist')
    args = parser.parse_args()
    try:
        binary = args.binary.resolve(strict=True)
        if not binary.is_file():
            raise RuntimeError('binary is not a regular file')
        report = run(binary, args.output)
    except (OSError, RuntimeError, ValueError, json.JSONDecodeError) as error:
        parser.exit(2, f'refusing diagnostic: {error}\n')
    print(json.dumps({
        'report': str(Path(args.output).resolve() / 'report.json'),
        'profile': report['profile'], 'enforcement': report['enforcement'],
        'comparison': report['comparison'],
    }, indent=2, sort_keys=True))


if __name__ == '__main__':
    main()
