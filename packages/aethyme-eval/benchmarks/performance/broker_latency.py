#!/usr/bin/env python3
"""Warning-only CLI budgets on a disposable Playground with local Git transport. No builds."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import shutil
import sqlite3
import statistics
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent
MAX_AGED_RELEASED_RESOURCE_LEASES = 50_000
MAX_BROKER_LATENCY_SESSIONS = 64
HOST_RESOURCE_STORE_SCHEMA_VERSION = 2


def assess(budgets, samples):
    if (budgets['schema_version'] != 1 or budgets['enforcement'] != 'warning'
            or budgets['unit'] != 'milliseconds' or budgets['statistic'] != 'median'):
        raise ValueError('unsupported budget contract')
    if set(samples) != set(budgets['budgets']):
        raise ValueError('missing or unexpected measurements')
    results = {}
    for name, spec in budgets['budgets'].items():
        limit = spec['limit_ms']
        values = samples[name]
        if (isinstance(limit, bool) or not isinstance(limit, (int, float))
                or not math.isfinite(limit) or limit <= 0):
            raise ValueError('invalid budget')
        if (not values or any(isinstance(v, bool) or not isinstance(v, (int, float))
                              or not math.isfinite(v) or v < 0 for v in values)):
            raise ValueError('invalid or empty timing samples')
        median = statistics.median(values)
        results[name] = {**spec, 'samples_ms': values, 'median_ms': median,
                         'verdict': 'warning' if median > limit else 'within_budget'}
    return results


def validate_aged_released_resource_lease_count(value):
    if (isinstance(value, bool) or not isinstance(value, int)
            or value < 0 or value > MAX_AGED_RELEASED_RESOURCE_LEASES):
        raise ValueError(
            f'aged released resource lease count must be between 0 and '
            f'{MAX_AGED_RELEASED_RESOURCE_LEASES}')
    return value


def summarize_diagnostic(samples):
    if (not samples or any(isinstance(value, bool)
                           or not isinstance(value, (int, float))
                           or not math.isfinite(value) or value < 0
                           for value in samples)):
        raise ValueError('invalid or empty diagnostic timing samples')
    return {'samples_ms': samples, 'median_ms': statistics.median(samples),
            'budget_ms': None, 'verdict': 'diagnostic_only'}


def validate_session_count(value):
    if (isinstance(value, bool) or not isinstance(value, int)
            or value < 1 or value > MAX_BROKER_LATENCY_SESSIONS):
        raise ValueError(
            f'broker session count must be between 1 and {MAX_BROKER_LATENCY_SESSIONS}')
    return value


def status_diagnostics_enabled(session_count, requested=False):
    session_count = validate_session_count(session_count)
    if not isinstance(requested, bool):
        raise ValueError('status diagnostics selection must be boolean')
    return requested or session_count > 1


def parse_status_diagnostics(stdout, expected_sessions, require_open_timing=True):
    expected_sessions = validate_session_count(expected_sessions)
    try:
        payload = json.loads(stdout)
    except (TypeError, json.JSONDecodeError) as error:
        raise RuntimeError('status command did not return JSON') from error
    if not isinstance(payload, dict) or not isinstance(payload.get('summary'), dict):
        raise RuntimeError('status output has no summary object')
    summary = payload['summary']
    counts = {}
    for field in ('live_sessions', 'active_sessions', 'idle_sessions', 'stale_sessions'):
        value = summary.get(field)
        if isinstance(value, bool) or not isinstance(value, int) or value < 0:
            raise RuntimeError(f'status summary has an invalid {field} count')
        counts[field] = value
    if counts['live_sessions'] != expected_sessions:
        raise RuntimeError(
            f"status reports {counts['live_sessions']} live sessions; "
            f'expected {expected_sessions}')
    if sum(counts[name] for name in ('active_sessions', 'idle_sessions', 'stale_sessions')) \
            != counts['live_sessions']:
        raise RuntimeError('status session-state counts do not sum to the live-session count')
    timings = payload.get('phase_timings_ms')
    open_ms = timings.get('open') if isinstance(timings, dict) else None
    if (require_open_timing and
            (isinstance(open_ms, bool) or not isinstance(open_ms, int) or open_ms < 0)):
        raise RuntimeError('status output has no valid Broker::open timing')
    return counts, open_ms


def fixture_path(path, root, label, must_exist=False):
    path = Path(path)
    if not path.is_absolute() or path.is_symlink():
        raise RuntimeError(f'{label} is not an absolute, non-symlink fixture path')
    try:
        resolved = path.resolve(strict=must_exist)
    except OSError as error:
        raise RuntimeError(f'cannot resolve {label} inside the fixture') from error
    if not resolved.is_relative_to(root):
        raise RuntimeError(f'{label} escaped the Playground fixture')
    return resolved


def validate_fixture_environment(env, root):
    expected = {
        'AETHYME_HOST_STATE_DIR': root / 'host-state',
        'AETHYME_HOST_CACHE_DIR': root / 'host-cache',
        'AETHYME_WORKTREE_ROOT': root / 'worktrees',
        'AETHYME_PREPARATION_CACHE_ROOT': root / 'preparation',
    }
    actual_keys = {key for key in env if key.startswith('AETHYME_')}
    if actual_keys != set(expected):
        raise RuntimeError('child environment contains an unexpected Aethyme storage override')
    for key, path in expected.items():
        if env.get(key) != str(path):
            raise RuntimeError(f'{key} is not bound to the synthetic fixture')
        fixture_path(Path(env[key]), root, key)
    return {key: str(path) for key, path in expected.items()}


def validate_resource_inventory(inventory, expected_repository, allow_released=True):
    if not isinstance(inventory, list):
        raise RuntimeError('host-resource inventory is not a JSON array')
    seen = set()
    for lease in inventory:
        if not isinstance(lease, dict) or 'ownership_token' in lease:
            raise RuntimeError('host-resource inventory has an invalid or secret-bearing lease')
        lease_id = lease.get('lease_id')
        fingerprint = lease.get('worktree_fingerprint')
        if not isinstance(lease_id, str) or not lease_id or lease_id in seen:
            raise RuntimeError('host-resource inventory has a missing or duplicate lease id')
        seen.add(lease_id)
        if lease.get('repository') != expected_repository:
            raise RuntimeError('host-resource inventory contains a non-fixture repository')
        if not isinstance(fingerprint, str) or not re.fullmatch(r'[0-9a-f]{64}', fingerprint):
            raise RuntimeError('host-resource inventory has a non-fixture worktree fingerprint')
        if lease.get('state') not in {'active', 'quarantined', 'released'}:
            raise RuntimeError('host-resource inventory has an unknown lease state')
        if not allow_released and lease['state'] == 'released':
            raise RuntimeError('released host-resource lease appeared in the default inventory')
        allocations = lease.get('allocations')
        if not isinstance(allocations, list) or not allocations:
            raise RuntimeError('host-resource inventory lease has no allocations')
    return inventory


def seed_released_resource_history(binary, command, root, cwd, repo, count):
    """Seed aged released rows only in this run's fixture-owned host database."""
    count = validate_aged_released_resource_lease_count(count)
    if count == 0:
        return {'enabled': False, 'aged_released_host_resource_leases': 0,
                'fixture_leases_before_seed': None, 'total_host_resource_leases': None,
                'total_released_host_resource_leases': None,
                'active_host_resource_leases': None,
                'quarantined_host_resource_leases': None,
                'visible_unreleased_host_resource_leases': None}

    fixture_root = fixture_path(root, root, 'fixture root', must_exist=True)
    repo = fixture_path(repo, fixture_root, 'Playground repository', must_exist=True)
    cwd = fixture_path(cwd, fixture_root, 'broker worktree', must_exist=True)
    expected_repository = hashlib.sha256(b'github.com/playground/latency').hexdigest()
    worktree_fingerprint = hashlib.sha256(os.fsencode(str(repo))).hexdigest()
    request_path = root / 'aged-history-bootstrap-request.json'
    grant_path = root / 'aged-history-bootstrap-grant.json'
    fixture_path(request_path, fixture_root, 'bootstrap request')
    fixture_path(grant_path, fixture_root, 'bootstrap grant')
    request = {
        'schema_version': 1,
        'request_id': 'aged-history-bootstrap',
        'repository': expected_repository,
        'worktree_fingerprint': worktree_fingerprint,
        'run_id': 'aged-history-bootstrap',
        'ttl_seconds': 3600,
        'holder_pid': None,
        'resources': [{
            'key': 'aged-history-bootstrap-capacity',
            'kind': 'capacity',
            'pool': 'aged-history-bootstrap',
            'units': 1,
            'limit': 1,
        }],
    }
    request_path.write_text(json.dumps(request, indent=2) + '\n')
    acquire_stdout, _ = command([str(binary), 'broker', 'advanced', 'resources', 'acquire',
                                 str(request_path), '--grant-out', str(grant_path), '--json'],
                                cwd, label='aged_history_database_bootstrap_acquire')
    if 'ownership_token' in json.loads(acquire_stdout):
        raise RuntimeError('resource acquire exposed a grant token despite --grant-out')
    grant_path = fixture_path(grant_path, fixture_root, 'bootstrap grant', must_exist=True)
    if grant_path.stat().st_mode & 0o077:
        raise RuntimeError('fixture grant file is not private')
    grant = json.loads(grant_path.read_text())
    token = grant.get('ownership_token')
    if not isinstance(token, str) or len(token) < 32:
        raise RuntimeError('resource acquire created an invalid fixture grant')
    release_stdout, _ = command([str(binary), 'broker', 'advanced', 'resources', 'release',
                                 str(grant_path), '--json'], cwd,
                                label='aged_history_database_bootstrap_release',
                                redact_values=(token,))
    if token in release_stdout:
        raise RuntimeError('fixture grant token was not removed from the command receipt')
    grant_path.unlink()
    request_path.unlink()
    inventory_stdout, _ = command([str(binary), 'broker', 'advanced', 'resources', 'list',
                                   '--all', '--json'], cwd,
                                  label='aged_history_fixture_inventory_before_seed')
    public_inventory = validate_resource_inventory(json.loads(inventory_stdout),
                                                   expected_repository)
    public_by_id = {lease['lease_id']: lease for lease in public_inventory}

    state_root = fixture_path(root / 'host-state', fixture_root, 'fixture host-state root',
                              must_exist=True)
    database = fixture_path(state_root / 'host-resources.db', fixture_root,
                            'fixture host-resource database', must_exist=True)
    if not database.is_file():
        raise RuntimeError('fixture host-resource database is not a regular file')
    connection = sqlite3.connect(database.as_uri() + '?mode=rw', uri=True, timeout=10,
                                 isolation_level=None)
    try:
        connection.execute('PRAGMA foreign_keys = ON')
        connection.execute('BEGIN IMMEDIATE')
        schema_version = connection.execute("SELECT value FROM meta WHERE key='schema_version'").fetchone()
        if schema_version != (HOST_RESOURCE_STORE_SCHEMA_VERSION,):
            raise RuntimeError(f'unsupported host-resource schema: {schema_version!r}')
        for table, required in {
                'resource_leases': {'lease_id', 'request_id', 'request_digest', 'repository',
                                    'worktree_fingerprint', 'generation', 'ownership_token',
                                    'state', 'created_at', 'expires_at', 'updated_at', 'released_at'},
                'resource_allocations': {'lease_id', 'resource_key', 'kind', 'value', 'units',
                                         'capacity_limit'}}.items():
            columns = {row[1] for row in connection.execute(f'PRAGMA table_info({table})')}
            if not required <= columns:
                raise RuntimeError(f'host-resource schema is missing columns from {table}')

        initial_rows = connection.execute(
            'SELECT lease_id,repository,worktree_fingerprint,generation,state '
            'FROM resource_leases ORDER BY generation').fetchall()
        if len(initial_rows) != len(public_inventory):
            raise RuntimeError('public resource inventory does not match the isolated database')
        initial_counts = dict(connection.execute(
            'SELECT state,COUNT(*) FROM resource_leases GROUP BY state').fetchall())
        for lease_id, repository, fingerprint, generation, _ in initial_rows:
            public_lease = public_by_id[lease_id]
            if (repository != expected_repository
                    or fingerprint != public_lease['worktree_fingerprint']
                    or repository != public_lease['repository']
                    or generation != public_lease['generation']):
                raise RuntimeError('fixture database contains a non-fixture lease identity')
        bootstrap = connection.execute(
            "SELECT lease_id,generation,state FROM resource_leases "
            "WHERE request_id='aged-history-bootstrap'").fetchone()
        if bootstrap is None or bootstrap[2] != 'released':
            raise RuntimeError('fixture bootstrap lease is missing or invalid')
        generation = connection.execute("SELECT value FROM meta WHERE key='generation'").fetchone()
        max_generation = connection.execute(
            'SELECT COALESCE(MAX(generation),0) FROM resource_leases').fetchone()[0]
        if generation is None or generation[0] != max_generation or max_generation < bootstrap[1]:
            raise RuntimeError('host-resource generation metadata is inconsistent')

        now_ms = int(time.time() * 1000)
        aged_created_at = now_ms - 365 * 24 * 60 * 60 * 1000 - 3600_000
        aged_expires_at = aged_created_at + 3600_000
        connection.execute(
            'UPDATE resource_leases SET created_at=?,expires_at=?,updated_at=?,released_at=? '
            "WHERE lease_id=? AND state='released'",
            (aged_created_at, aged_expires_at, aged_expires_at + 1,
             aged_expires_at + 1, bootstrap[0]),
        )
        if connection.execute('SELECT changes()').fetchone()[0] != 1:
            raise RuntimeError('fixture bootstrap lease was not transitioned exactly once')

        def inserted_rows():
            for index in range(2, count + 1):
                lease_id, pool = f'aged-history-{index:06d}', f'aged-history-pool-{index:06d}'
                row_request = {
                'schema_version': 1,
                'request_id': lease_id,
                'repository': expected_repository,
                'worktree_fingerprint': worktree_fingerprint,
                'run_id': lease_id,
                'ttl_seconds': 3600,
                'holder_pid': None,
                'resources': [{'key': 'aged-history-capacity', 'kind': 'capacity',
                               'pool': pool, 'units': 1, 'limit': 1}],
                }
                digest = hashlib.sha256(json.dumps(
                    row_request, separators=(',', ':')).encode()).hexdigest()
                created_at, expires_at = aged_created_at - index * 1000, aged_expires_at - index * 1000
                released_at = expires_at + 1
                yield (
                    lease_id, lease_id, digest,
                    expected_repository, worktree_fingerprint, lease_id,
                    max_generation + index - 1,
                    hashlib.sha256(f'fixture-retired-token-{index}'.encode()).hexdigest(),
                    'released', None, created_at, expires_at, released_at, released_at,
                )

        connection.executemany(
            'INSERT INTO resource_leases '
            '(lease_id,request_id,request_digest,repository,worktree_fingerprint,run_id,'
            'generation,ownership_token,state,holder_pid,created_at,expires_at,updated_at,released_at) '
            'VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?)', inserted_rows())
        connection.executemany(
            'INSERT INTO resource_allocations '
            '(lease_id,resource_key,kind,value,units,capacity_limit) '
            'VALUES (?,?,?,?,?,?)',
            ((f'aged-history-{index:06d}', 'aged-history-capacity', 'capacity',
              f'aged-history-pool-{index:06d}', 1, 1)
             for index in range(2, count + 1)))
        connection.execute(
            "UPDATE meta SET value=? WHERE key='generation'",
            (max_generation + count - 1,),
        )
        state_counts = dict(connection.execute(
            'SELECT state,COUNT(*) FROM resource_leases GROUP BY state').fetchall())
        if (sum(state_counts.values()) != len(initial_rows) + count - 1
                or state_counts.get('released', 0)
                != initial_counts.get('released', 0) + count - 1
                or state_counts.get('active', 0) != initial_counts.get('active', 0)
                or state_counts.get('quarantined', 0) != initial_counts.get('quarantined', 0)):
            raise RuntimeError('seeded host-resource lease counts do not match the request')
        if connection.execute('PRAGMA foreign_key_check').fetchall():
            raise RuntimeError('seeded host-resource database has foreign-key violations')
        if connection.execute('PRAGMA quick_check').fetchone() != ('ok',):
            raise RuntimeError('seeded host-resource database failed SQLite quick_check')
        connection.commit()
    except Exception:
        connection.rollback()
        raise
    finally:
        connection.close()

    return {
        'enabled': True,
        'database_relative_path': 'host-state/host-resources.db',
        'host_resource_store_schema_version': HOST_RESOURCE_STORE_SCHEMA_VERSION,
        'aged_released_host_resource_leases': count,
        'fixture_leases_before_seed': len(initial_rows),
        'total_host_resource_leases': len(initial_rows) + count - 1,
        'total_released_host_resource_leases': state_counts.get('released', 0),
        'active_host_resource_leases': state_counts.get('active', 0),
        'quarantined_host_resource_leases': state_counts.get('quarantined', 0),
        'visible_unreleased_host_resource_leases': (
            state_counts.get('active', 0) + state_counts.get('quarantined', 0)),
        'public_bootstrap_leases': 1,
        'transactionally_seeded_released_leases': count - 1,
        'oldest_lease_age_days': 365,
        'fixture_repository_sha256': expected_repository,
        'seed_method': 'public acquire/release bootstrap, then isolated SQLite transaction',
    }


def run_playground(binary, samples, output, aged_released_resource_leases=0,
                   session_count=1, request_status_diagnostics=False):
    aged_released_resource_leases = validate_aged_released_resource_lease_count(
        aged_released_resource_leases)
    session_count = validate_session_count(session_count)
    collect_status_diagnostics = status_diagnostics_enabled(
        session_count, request_status_diagnostics)
    receipts = []
    with tempfile.TemporaryDirectory(prefix='aethyme-broker-latency-') as temporary:
        root = Path(temporary).resolve()
        repo, home, tools = (root / name for name in ('repo', 'home', 'bin'))
        for path in (repo, home, tools, root / 'preparation', root / 'tmp'):
            path.mkdir()
        shutil.copytree(HERE / 'fixture', repo, dirs_exist_ok=True)
        # Allow only process/platform basics, never inherited credentials, proxies,
        # provider settings or product/Git storage selectors.
        env = {k: v for k, v in os.environ.items()
               if k in {'PATH', 'LANG', 'LC_ALL', 'TZ', 'TMPDIR', 'SystemRoot', 'WINDIR'}}
        env.update(TMPDIR=str(root / 'tmp'), TMP=str(root / 'tmp'), TEMP=str(root / 'tmp'),
                   HOME=str(home), XDG_CONFIG_HOME=str(home / 'config'),
                   XDG_DATA_HOME=str(home / 'data'), XDG_STATE_HOME=str(home / 'state'),
                   XDG_CACHE_HOME=str(home / 'cache'),
                   AETHYME_HOST_STATE_DIR=str(root / 'host-state'),
                   AETHYME_HOST_CACHE_DIR=str(root / 'host-cache'),
                   AETHYME_WORKTREE_ROOT=str(root / 'worktrees'),
                   AETHYME_PREPARATION_CACHE_ROOT=str(root / 'preparation'),
                   GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL='/dev/null',
                   GIT_AUTHOR_NAME='Latency Playground', GIT_AUTHOR_EMAIL='latency@example.invalid',
                   GIT_COMMITTER_NAME='Latency Playground',
                   GIT_COMMITTER_EMAIL='latency@example.invalid',
                   GIT_TERMINAL_PROMPT='0')
        fixture_environment = validate_fixture_environment(env, root)
        workload = {
            'session_count_requested': session_count,
            'status_diagnostics_enabled': collect_status_diagnostics,
            'created_broker_sessions': 0,
            'created_worktrees': 0,
            'created_session_ids': [],
            'open_broker_sessions': 0,
            'active_broker_sessions': 0,
            'live_broker_sessions': 0,
            'idle_broker_sessions': 0,
            'stale_broker_sessions': 0,
            'observed_status_session_counts': None,
            'closed_broker_sessions': 0,
            'terminal_queue_entries': 0,
            'aged_released_host_resource_leases_requested': aged_released_resource_leases,
            'aged_released_host_resource_leases': 0,
            'host_resource_history_seed_status': 'pending',
            'fixture_environment': fixture_environment,
        }
        (output / 'workload.json').write_text(json.dumps(workload, indent=2) + '\n')
        git = shutil.which('git', path=env['PATH'])
        if not git:
            raise RuntimeError('Git is required')
        git = str(Path(git).resolve())
        # The GitHub-shaped origin is served by a local bare repository; neither
        # expected Git/gh paths use these shims. This is not an OS network sandbox.
        (tools / 'ssh').write_text(
            '#!/bin/sh\ncase "$*" in\n'
            ' *git-upload-pack*) exec "$LATENCY_GIT" upload-pack "$LATENCY_REMOTE" ;;\n'
            ' *git-receive-pack*) exec "$LATENCY_GIT" receive-pack "$LATENCY_REMOTE" ;;\n'
            ' *) exit 64 ;;\nesac\n')
        (tools / 'gh').write_text('#!/bin/sh\ncase "$*" in\n'
                                  ' "pr list"*) printf "[]\\n" ;;\n * ) exit 64 ;;\nesac\n')
        for name in ('ssh', 'gh'):
            (tools / name).chmod(0o700)
        env.update(PATH=str(tools) + os.pathsep + env['PATH'],
                   LATENCY_GIT=git, LATENCY_REMOTE=str(root / 'remote.git'))

        def command(argv, cwd=repo, label=None, redact_values=()):
            command_cwd = fixture_path(cwd, root, 'child working directory', must_exist=True)
            child_environment = validate_fixture_environment(env, root)
            started = time.perf_counter_ns()
            completed = subprocess.run(argv, cwd=command_cwd, env=env, capture_output=True,
                                       text=True, timeout=60, check=False)
            elapsed = (time.perf_counter_ns() - started) / 1_000_000
            stdout = completed.stdout
            stderr = completed.stderr
            redacted_occurrences = 0
            for secret in redact_values:
                if not isinstance(secret, str) or not secret:
                    raise RuntimeError('invalid command output redaction value')
                if secret in argv:
                    raise RuntimeError('a sensitive grant token appeared in command arguments')
                redacted_occurrences += stdout.count(secret) + stderr.count(secret)
                stdout = stdout.replace(secret, '[redacted grant token]')
                stderr = stderr.replace(secret, '[redacted grant token]')
            # Recheck storage paths after each child command in case a child changed them.
            validate_fixture_environment(env, root)
            receipt = {'argv': argv, 'cwd': str(command_cwd), 'wall_ms': elapsed,
                       'exit_code': completed.returncode,
                       'stdout': stdout, 'stderr': stderr,
                       'fixture_storage_environment': child_environment}
            if label is not None:
                receipt['label'] = label
            if redact_values:
                receipt['redacted_grant_token_occurrences'] = redacted_occurrences
            receipts.append(receipt)
            # Persist failed setup as well as successful samples.
            (output / 'commands.json').write_text(json.dumps(receipts, indent=2) + '\n')
            if completed.returncode:
                raise RuntimeError(f'command failed: {argv!r}: {stdout} {stderr}')
            return stdout, elapsed

        binary_version_raw, _ = command([str(binary), '--version'], label='binary_identity')
        command([git, 'init', '--bare', '-q', '-b', 'main', env['LATENCY_REMOTE']])
        command([git, 'init', '-q', '--template=', '-b', 'main'])
        command([git, 'config', 'core.sshCommand', shlex.quote(str(tools / 'ssh'))])
        (repo / '.gitignore').write_text('.aethyme/*\n!.aethyme/config.toml\n')
        (repo / '.aethyme').mkdir()
        (repo / '.aethyme/config.toml').write_text(
            '[delivery]\npush_session_branches = true\n[cleanup]\nauto_remove = false\n')
        (repo / 'tracked.txt').write_text('synthetic Playground\n')
        command([git, 'add', '-A'])
        command([git, 'commit', '-qm', 'chore(perf): seed Playground'])
        command([git, 'remote', 'add', 'origin', 'git@github.com:playground/latency.git'])
        command([git, 'push', '-qu', 'origin', 'main'])
        sessions = []
        worktrees = []
        for index in range(1, session_count + 1):
            label = f'{index:02d}'
            started, _ = command(
                [str(binary), 'broker', 'start', '--task',
                 f'latency Playground worker {label}', '--short-name', f'latency-{label}',
                 '--agent', f'Latency worker {label} <latency-{label}@example.invalid>',
                 '--json'],
                label=f'fixture_session_{label}')
            session = json.loads(started)
            if not isinstance(session, dict) or not isinstance(session.get('id'), (str, int)):
                raise RuntimeError('broker start did not return a session identity')
            sessions.append(session)
            if isinstance(session.get('worktree_path'), str):
                worktrees.append(Path(session['worktree_path']))
            workload.update({
                'created_broker_sessions': len(sessions),
                'created_worktrees': len(worktrees),
                'created_session_ids': [str(item['id']) for item in sessions],
                'open_broker_sessions': len(sessions),
            })
            (output / 'workload.json').write_text(json.dumps(workload, indent=2) + '\n')
            candidate = fixture_path(
                Path(session['worktree_path']), root, 'broker-reported worktree',
                must_exist=True)
            worktree_root = fixture_path(root / 'worktrees', root,
                                         'fixture worktree root', must_exist=True)
            if not candidate.is_relative_to(worktree_root):
                raise RuntimeError('broker escaped fixture worktree root')
            worktrees[-1] = candidate

        # Keep one session for the existing guard/sync/push measurements. All
        # sessions remain in the same isolated broker database for status scaling.
        session = sessions[0]
        worktree = worktrees[0]
        sid = str(session['id'])
        workload.update({
            'created_broker_sessions': len(sessions),
            'created_worktrees': len(worktrees),
            'created_session_ids': [str(item['id']) for item in sessions],
            'open_broker_sessions': len(sessions),
        })
        (output / 'workload.json').write_text(json.dumps(workload, indent=2) + '\n')
        # Publish once outside measurement; timed pushes are unchanged, with local transport.
        command([str(binary), 'broker', 'push', '--session', sid], worktree)
        try:
            history = seed_released_resource_history(
                binary, command, root, worktree, repo, aged_released_resource_leases)
        except Exception as error:
            workload['host_resource_history_seed_status'] = 'failed'
            workload['host_resource_history_seed_error'] = str(error)
            (output / 'workload.json').write_text(json.dumps(workload, indent=2) + '\n')
            raise
        workload.update({
            'aged_released_host_resource_leases': (
                history['aged_released_host_resource_leases']),
            'host_resource_history_seed_status': 'complete' if history['enabled'] else 'disabled',
            'active_host_resource_leases': history['active_host_resource_leases'],
            'quarantined_host_resource_leases': history['quarantined_host_resource_leases'],
            'visible_unreleased_host_resource_leases': (
                history['visible_unreleased_host_resource_leases']),
            'aged_host_resource_history': history,
        })
        (output / 'workload.json').write_text(json.dumps(workload, indent=2) + '\n')
        commands = {
            'routine_status': [str(binary), 'broker', 'status', '--summary', '--json'],
            'guard_overhead': [str(binary), 'broker', 'advanced', 'exec', '--session', sid,
                               '--json', '--', '/usr/bin/true'],
            'unchanged_sync': [str(binary), 'broker', 'sync', '--session', sid, '--json'],
            'push': [str(binary), 'broker', 'push', '--session', sid],
        }
        if collect_status_diagnostics:
            commands.update({
                'status_full': [str(binary), 'broker', 'status', '--json'],
                'doctor': [str(binary), 'broker', 'doctor', '--json'],
            })
        if aged_released_resource_leases:
            commands['resource_history_list'] = [
                str(binary), 'broker', 'advanced', 'resources', 'list', '--json']
        diagnostic_names = {'status_full', 'doctor'}
        measured = {name: [] for name in commands
                    if name not in diagnostic_names | {'resource_history_list'}}
        diagnostic_samples = ({
            'status_full': [],
            'doctor': [],
            'status_open_phase_summary': [],
            'status_open_phase_full': [],
        } if collect_status_diagnostics else {})
        history_samples = []
        for sample_index in range(samples):
            for name, argv in commands.items():
                stdout, elapsed = command(
                    argv, worktree, label=f'sample_{sample_index + 1}:{name}')
                if name == 'routine_status':
                    counts, open_ms = parse_status_diagnostics(
                        stdout, session_count, require_open_timing=collect_status_diagnostics)
                    workload['observed_status_session_counts'] = counts
                    workload['active_broker_sessions'] = counts['active_sessions']
                    workload['live_broker_sessions'] = counts['live_sessions']
                    workload['idle_broker_sessions'] = counts['idle_sessions']
                    workload['stale_broker_sessions'] = counts['stale_sessions']
                    (output / 'workload.json').write_text(
                        json.dumps(workload, indent=2) + '\n')
                    if collect_status_diagnostics:
                        diagnostic_samples['status_open_phase_summary'].append(open_ms)
                    measured[name].append(elapsed)
                elif name == 'status_full':
                    _, open_ms = parse_status_diagnostics(stdout, session_count)
                    diagnostic_samples['status_full'].append(elapsed)
                    diagnostic_samples['status_open_phase_full'].append(open_ms)
                elif name == 'doctor':
                    try:
                        report = json.loads(stdout)
                    except json.JSONDecodeError as error:
                        raise RuntimeError('doctor command did not return JSON') from error
                    if (not isinstance(report, dict)
                            or not isinstance(report.get('deferred_checks'), list)):
                        raise RuntimeError('doctor output has no deferred-check inventory')
                    diagnostic_samples[name].append(elapsed)
                elif name == 'guard_overhead':
                    if not json.loads(stdout).get('ok'):
                        raise RuntimeError('guard audit did not pass')
                    measured[name].append(elapsed)
                elif name == 'resource_history_list':
                    inventory = validate_resource_inventory(
                        json.loads(stdout),
                        history['fixture_repository_sha256'],
                        allow_released=False)
                    if len(inventory) != history['visible_unreleased_host_resource_leases']:
                        raise RuntimeError(
                            'default resource inventory count differs from the fixture database')
                    history_samples.append(elapsed)
                else:
                    measured[name].append(elapsed)
        return measured, diagnostic_samples, history_samples, workload, binary_version_raw


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--samples', type=int, choices=range(1, 11), default=3)
    parser.add_argument(
        '--sessions', type=int, default=1, metavar='N',
        help=('create N isolated broker sessions/worktrees '
              f'(1..{MAX_BROKER_LATENCY_SESSIONS}; default: 1; values above 1 enable status diagnostics)'))
    parser.add_argument(
        '--status-diagnostics', action='store_true',
        help='also measure full status, doctor and status-open phases (automatically enabled with --sessions > 1)')
    parser.add_argument(
        '--aged-released-resource-leases', type=int, default=0, metavar='N',
        help=('opt in to seed N old released host-resource lease records in the isolated '
              f'Playground fixture (0..{MAX_AGED_RELEASED_RESOURCE_LEASES}; default: 0)'))
    args = parser.parse_args()
    try:
        aged_released_resource_leases = validate_aged_released_resource_lease_count(
            args.aged_released_resource_leases)
        session_count = validate_session_count(args.sessions)
    except ValueError as error:
        parser.error(str(error))
    binary_argument = str(args.binary)
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    budget_bytes = (HERE / 'broker-budgets.json').read_bytes()
    budgets = json.loads(budget_bytes)
    measured, diagnostic_samples, history_samples, workload, binary_version_raw = run_playground(
        binary, args.samples, args.output, aged_released_resource_leases, session_count,
        args.status_diagnostics)
    results = assess(budgets, measured)
    diagnostics = {name: summarize_diagnostic(values)
                   for name, values in diagnostic_samples.items()}
    if history_samples:
        diagnostics['resource_history_list'] = summarize_diagnostic(history_samples)
    profile = ('parcel_playground_one_session_unchanged_push'
               if session_count == 1 and not args.status_diagnostics
               else f'parcel_playground_{session_count}_sessions_status_scale')
    report = {'schema_version': 2, 'enforcement': 'warning',
              'binary_argument': binary_argument,
              'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'binary_version_raw': binary_version_raw,
              'budget_sha256': hashlib.sha256(budget_bytes).hexdigest(),
              'profile': profile,
              'transport': 'local_bare_remote_via_ssh_shim',
              'network_duration': 'not_instrumented', 'workload': workload,
              'results': results, 'diagnostics': diagnostics}
    (args.output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    lines = ['## Broker latency budgets (warning only)', '',
             '| Action | Median ms | Budget ms | Result |', '|---|---:|---:|---|']
    for name, item in results.items():
        lines.append(f"| {name} | {item['median_ms']:.1f} | {item['limit_ms']} | {item['verdict']} |")
        if item['verdict'] == 'warning':
            print(f"::warning title=Broker latency budget::{name}: "
                  f"{item['median_ms']:.1f} ms exceeds {item['limit_ms']} ms")
    lines += ['', 'Budgeted values are complete CLI wall times, including Git work.',
              'Supplementary status-open-phase values are internal phase times. No network time is subtracted; the synthetic local transport is not an OS network sandbox.']
    summary = '\n'.join(lines) + '\n'
    if diagnostics:
        summary += (
            '\n### Supplementary diagnostics (unbudgeted)\n\n'
            '| Action | Median ms | Raw samples ms | Enforcement |\n'
            '|---|---:|---|---|\n'
        )
        for name, diagnostic in diagnostics.items():
            summary += (
                f"| {name} | {diagnostic['median_ms']:.1f} | "
                f"{', '.join(f'{value:.1f}' for value in diagnostic['samples_ms'])} | "
                'diagnostic only |\n'
            )
        if history_samples:
            summary += (
                f"\nSeeded {workload['aged_released_host_resource_leases']} aged released "
                'host-resource lease records in the isolated Playground; budgets are unchanged.\n'
            )
        if workload['status_diagnostics_enabled']:
            summary += (
                f"\nSession-scale profile requested {workload['session_count_requested']} sessions and created "
                f"{workload['created_worktrees']} worktrees; observed status states are "
                f"{json.dumps(workload['observed_status_session_counts'], sort_keys=True)}.\n"
                'These synthetic fixture measurements do not establish production latency.\n'
            )
    (args.output / 'summary.md').write_text(summary)
    print(summary)
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as stream:
            stream.write(summary)


if __name__ == '__main__':
    main()
