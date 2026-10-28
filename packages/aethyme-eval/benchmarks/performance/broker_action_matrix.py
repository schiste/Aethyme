#!/usr/bin/env python3
"""Paired, warning-only broker action measurements on synthetic Playgrounds."""
import argparse
from contextlib import contextmanager
import concurrent.futures
import hashlib
import json
import math
import os
from pathlib import Path
import re
import shlex
import stat
import statistics
import sys
import time

from broker_concurrent_submit import (
    BoundedCommandRunner,
    MAX_BINARY_BYTES,
    MAX_OUTPUT_BYTES,
    MAX_STDERR_BYTES,
    MAX_STDOUT_BYTES,
    ReceiptLog,
    canonical_json_bytes,
    create_shims,
    fail_walk,
    hash_file,
    make_env,
    output_tree_size,
    run_git,
    scenario_fixture,
    start_session,
    tree_size,
    validate_output_path,
)
from broker_latency import (
    parse_status_diagnostics,
    seed_released_resource_history,
)

HERE = Path(__file__).resolve().parent
MATRIX_BUDGETS = HERE / 'broker-action-matrix-v2.json'
SCENARIOS = HERE / 'broker-action-scenarios-v1.json'
ABSOLUTE_BUDGETS = HERE / 'broker-budgets.json'
MAX_CONTRACT_BYTES = 128 * 1024
MAX_REFERENCE_BYTES = 1024 * 1024
MAX_TREE_ENTRIES = 20_000
MAX_TREE_PATH_BYTES = 8 * 1024 * 1024
MAX_TREE_DEPTH = 64
WORKTREE_REPO = 'playground/latency'
REMOTE_URL = 'git@github.com:playground/latency.git'
FETCH_REMOTE = 'perf-local'
FETCH_SOURCE_REF = 'refs/heads/perf-proven'
FETCH_DESTINATION_REF = f'refs/remotes/{FETCH_REMOTE}/perf-proven'
BASELINE_UNSUPPORTED_FETCH_OPTIONS = (
    '--no-auto-maintenance', '--no-write-commit-graph', '--no-prune', '--no-prune-tags',
)
UNPROVABLE_FETCH_REASON = (
    'A safe public-CLI injection cannot yet prove that the provider ran while '
    'withholding its exit witness; no command or synthetic journal was created.'
)
GIT_ENV_OVERRIDES = {
    'GIT_DIR', 'GIT_WORK_TREE', 'GIT_COMMON_DIR', 'GIT_INDEX_FILE',
    'GIT_OBJECT_DIRECTORY', 'GIT_ALTERNATE_OBJECT_DIRECTORIES', 'GIT_NAMESPACE',
    'GIT_CEILING_DIRECTORIES', 'GIT_DISCOVERY_ACROSS_FILESYSTEM', 'GIT_CONFIG',
    'GIT_CONFIG_COUNT', 'GIT_CONFIG_PARAMETERS', 'GIT_SSH', 'GIT_SSH_COMMAND',
    'GIT_PROXY_COMMAND', 'GIT_TRACE', 'GIT_TRACE_PACKET', 'GIT_TRACE_SETUP',
    'GIT_TRACE2_EVENT', 'GIT_TRACE2_CONFIG_PARAMS',
}
REPO_ROOT = HERE.parents[3]
HARNESS_FILES = (
    HERE / 'broker_action_matrix.py',
    HERE / 'broker_concurrent_submit.py',
    HERE / 'broker_latency.py',
)


def _read_contract(path, max_bytes=MAX_CONTRACT_BYTES):
    path = Path(path)
    if path.is_symlink() or not path.is_file() or path.stat().st_size > max_bytes:
        raise ValueError(f'contract is not a bounded regular file: {path.name}')
    return path.read_bytes()


def load_contracts(matrix_path=MATRIX_BUDGETS, scenarios_path=SCENARIOS,
                   absolute_path=ABSOLUTE_BUDGETS):
    matrix_bytes = _read_contract(matrix_path)
    scenario_bytes = _read_contract(scenarios_path)
    absolute_bytes = _read_contract(absolute_path)
    matrix = json.loads(matrix_bytes)
    scenarios = json.loads(scenario_bytes)
    absolute = json.loads(absolute_bytes)
    if (not isinstance(matrix, dict) or not isinstance(scenarios, dict)
            or not isinstance(absolute, dict)):
        raise ValueError('action-matrix contracts must be JSON objects')
    paired = matrix.get('paired_comparison')
    runner = matrix.get('runner')
    if (matrix.get('schema_version') != 2 or matrix.get('enforcement') != 'warning'
            or matrix.get('statistic') != 'median' or matrix.get('unit') != 'milliseconds'
            or matrix.get('absolute_budget_reference') != {
                'path': Path(absolute_path).name, 'schema_version': 1}
            or not isinstance(paired, dict) or paired.get('enforcement') != 'warning'
            or paired.get('order') != 'alternating'
            or not isinstance(runner, dict)):
        raise ValueError('unsupported action-matrix budget contract')
    repetitions = paired.get('repetitions')
    percent = paired.get('max_regression_percent')
    floor = paired.get('absolute_jitter_floor_ms')
    if (isinstance(repetitions, bool) or not isinstance(repetitions, int)
            or repetitions < 3 or repetitions > 15
            or isinstance(percent, bool) or not isinstance(percent, (int, float))
            or not math.isfinite(percent) or percent < 0 or percent > 100
            or isinstance(floor, bool) or not isinstance(floor, (int, float))
            or not math.isfinite(floor) or floor < 0):
        raise ValueError('invalid paired comparison bounds')
    required_runner = {
        'command_timeout_seconds': (1, 600),
        'history_rows': (0, 50_000),
        'slow_hook_minimum_hold_ms': (0, 10_000),
    }
    for key, (minimum, maximum) in required_runner.items():
        value = runner.get(key)
        if (isinstance(value, bool) or not isinstance(value, int)
                or value < minimum or value > maximum):
            raise ValueError(f'invalid matrix runner bound: {key}')
    if (absolute.get('schema_version') != 1 or absolute.get('enforcement') != 'warning'
            or absolute.get('unit') != 'milliseconds'
            or absolute.get('statistic') != 'median'
            or not isinstance(absolute.get('budgets'), dict)):
        raise ValueError('the referenced absolute budget file is not schema v1')
    if (scenarios.get('schema_version') != 1
            or scenarios.get('enforcement') != 'diagnostic_only'
            or not isinstance(scenarios.get('fixture'), dict)
            or scenarios.get('fixture', {}).get('synthetic') is not True
            or scenarios.get('fixture', {}).get('network_sandbox') is not False
            or not isinstance(scenarios.get('scenarios'), list)):
        raise ValueError('unsupported action-matrix scenario contract')
    rows = scenarios['scenarios']
    names = [row.get('id') for row in rows if isinstance(row, dict)]
    if (len(names) != len(rows) or any(not isinstance(name, str) for name in names)
            or len(names) != len(set(names))):
        raise ValueError('scenario ids must be unique objects')
    required = {
        'start', 'status_summary', 'lease_plan', 'lease_claim', 'lease_release',
        'overlap_detail', 'broker_commit_slow_hook', 'broker_commit_independent',
        'fetch_provable', 'fetch_recovery_show', 'fetch_unprovable',
        'deploy_plan_diff', 'status_aged_history', 'finish', 'bounded_output_control',
    }
    if set(names) != required:
        raise ValueError('scenario contract has missing or unexpected action ids')
    for row in rows:
        kind = row.get('kind')
        if (not isinstance(kind, str)
                or kind not in {'timing', 'semantic_timing', 'semantic_control',
                                'harness_control'}):
            raise ValueError(f"unsupported scenario kind: {row.get('id')}")
        if row.get('absolute_budget') is not None:
            budget_name = row['absolute_budget']
            if not isinstance(budget_name, str):
                raise ValueError(f"unknown absolute budget for scenario {row['id']}")
            budget = absolute['budgets'].get(budget_name)
            if (not isinstance(budget, dict) or isinstance(budget.get('limit_ms'), bool)
                    or not isinstance(budget.get('limit_ms'), (int, float))
                    or not math.isfinite(budget['limit_ms']) or budget['limit_ms'] <= 0):
                raise ValueError(f"unknown absolute budget for scenario {row['id']}")
    history_scenario = next(row for row in rows if row['id'] == 'status_aged_history')
    if history_scenario.get('history_rows') != runner['history_rows']:
        raise ValueError('aged history scenario and runner bounds disagree')
    return {
        'matrix': matrix,
        'matrix_bytes': matrix_bytes,
        'scenario_manifest': scenarios,
        'scenario_bytes': scenario_bytes,
        'absolute_budgets': absolute,
        'absolute_bytes': absolute_bytes,
        'matrix_sha256': hashlib.sha256(matrix_bytes).hexdigest(),
        'scenario_sha256': hashlib.sha256(scenario_bytes).hexdigest(),
        'absolute_sha256': hashlib.sha256(absolute_bytes).hexdigest(),
    }


def paired_order(repetition):
    if isinstance(repetition, bool) or not isinstance(repetition, int) or repetition < 0:
        raise ValueError('repetition index must be a nonnegative integer')
    return ('baseline', 'candidate') if repetition % 2 == 0 else ('candidate', 'baseline')


def compare_samples(baseline, candidate, paired):
    repetitions = paired['repetitions']
    for values in (baseline, candidate):
        if (not isinstance(values, list) or len(values) != repetitions
                or any(isinstance(value, bool) or not isinstance(value, (int, float))
                       or not math.isfinite(value) or value < 0 for value in values)):
            raise ValueError('paired comparison requires one valid sample per repetition')
    baseline_median = statistics.median(baseline)
    candidate_median = statistics.median(candidate)
    change = candidate_median - baseline_median
    threshold = max(paired['absolute_jitter_floor_ms'],
                    baseline_median * paired['max_regression_percent'] / 100)
    return {
        'baseline_samples_ms': baseline,
        'candidate_samples_ms': candidate,
        'baseline_median_ms': baseline_median,
        'candidate_median_ms': candidate_median,
        'change_ms': change,
        'change_percent': (change / baseline_median * 100 if baseline_median else None),
        'warning_threshold_ms': threshold,
        'verdict': 'warning' if change > threshold else 'within_warning_band',
        'enforcement': 'warning',
        'sample_count': repetitions,
        'uncertainty': 'small sample; descriptive median comparison, not a significance test',
    }


def _strict_hash(path):
    path = Path(path)
    resolved = path.resolve(strict=True)
    if not resolved.is_file() or resolved.stat().st_size > MAX_BINARY_BYTES:
        raise ValueError('binary pair member is not a bounded regular file')
    return resolved, hash_file(resolved, MAX_BINARY_BYTES)


def binary_identity(cli_path):
    cli, cli_hash = _strict_hash(cli_path)
    engine, engine_hash = _strict_hash(cli.parent / 'aethyme-engine-cli')
    return {
        'cli_argument': str(cli_path),
        'cli_path': str(cli),
        'cli_sha256': cli_hash['sha256'],
        'cli_bytes': cli_hash['bytes'],
        'engine_path': str(engine),
        'engine_sha256': engine_hash['sha256'],
        'engine_bytes': engine_hash['bytes'],
    }


def load_v41_context(path, baseline_identity):
    raw = _read_contract(path, MAX_REFERENCE_BYTES)
    report = json.loads(raw)
    source_commit = report.get('source_commit') if isinstance(report, dict) else None
    if (not isinstance(report, dict) or report.get('version') != '0.8.41'
            or not isinstance(source_commit, str)
            or not re.fullmatch(r'[0-9a-f]{40}', source_commit)
            or report.get('local_native_builds_or_tests') is not False
            or not isinstance(report.get('release'), str)
            or not report['release'].endswith('/v0.8.41')
            or not isinstance(report.get('roots'), dict) or not report['roots']
            or isinstance(report.get('hosted_workspace_passed'), bool)
            or not isinstance(report.get('hosted_workspace_passed'), int)
            or report['hosted_workspace_passed'] < 1
            or not isinstance(report.get('repos'), dict)):
        raise ValueError('v0.8.41 live reference has an unsupported or incomplete shape')
    observed_pairs = set()
    for root in report['roots'].values():
        if not isinstance(root, dict):
            raise ValueError('v0.8.41 live reference has a malformed binary root')
        cli = root.get('aethyme')
        engine = root.get('aethyme-engine-cli')
        if (not isinstance(cli, dict) or not isinstance(engine, dict)
                or not isinstance(cli.get('sha256'), str)
                or not isinstance(engine.get('sha256'), str)
                or not re.fullmatch(r'[0-9a-f]{64}', cli.get('sha256', ''))
                or not re.fullmatch(r'[0-9a-f]{64}', engine.get('sha256', ''))):
            raise ValueError('v0.8.41 live reference has malformed binary-pair hashes')
        observed_pairs.add((cli['sha256'], engine['sha256']))
    if len(observed_pairs) != 1:
        raise ValueError('v0.8.41 live reference roots do not agree on one binary pair')
    pair = next(iter(observed_pairs))
    if pair != (baseline_identity['cli_sha256'], baseline_identity['engine_sha256']):
        raise ValueError('baseline executable pair does not match the retained v0.8.41 reference')
    rollout = {}
    real_commit = report.get('real_commit')
    if isinstance(real_commit, dict):
        seconds = real_commit.get('seconds')
        if (isinstance(seconds, (int, float)) and not isinstance(seconds, bool)
                and math.isfinite(seconds) and seconds >= 0):
            rollout['real_commit_seconds'] = seconds
    for name, repo in report.get('repos', {}).items():
        seconds = repo.get('seconds') if isinstance(repo, dict) else None
        if isinstance(seconds, dict):
            rollout[name] = {
                key: value for key, value in seconds.items()
                if isinstance(value, (int, float)) and not isinstance(value, bool)
                and math.isfinite(value) and value >= 0
            }
    return {
        'source_commit': source_commit,
        'version': report['version'],
        'release': report.get('release'),
        'hosted_workspace_passed': report.get('hosted_workspace_passed'),
        'receipt_sha256': hashlib.sha256(raw).hexdigest(),
        'binary_pair_sha256': {'aethyme': pair[0], 'aethyme-engine_cli': pair[1]},
        'uncontrolled_real_rollout_seconds': rollout,
        'comparison_eligible': False,
        'comparison_exclusion_reason': (
            'live rollout observations lack paired binaries and identical controlled '
            'Playground workloads; they are context only'),
    }


def _build_commit(version_text):
    match = re.search(r'\bbuild_commit=([0-9a-f]{40,64})\b', version_text)
    return match.group(1) if match else None


def _json(row, label):
    try:
        value = json.loads(row['stdout'])
    except (KeyError, TypeError, json.JSONDecodeError) as error:
        raise RuntimeError(f'{label} did not return complete JSON') from error
    return value


def _status(runner, binary, cwd, label, expected_sessions):
    row = runner.run([str(binary), 'broker', 'status', '--summary', '--json'], cwd, label)
    counts, _ = parse_status_diagnostics(row['stdout'], expected_sessions,
                                         require_open_timing=False)
    return row, counts


def _session(runner, binary, repo, root, baseline, name, role):
    return start_session(runner, binary, repo, root, name, role, baseline)


def _validate_fixture_git_environment(env):
    present = sorted(key for key in env if key in GIT_ENV_OVERRIDES
                     or key.startswith('GIT_CONFIG_KEY_')
                     or key.startswith('GIT_CONFIG_VALUE_'))
    if present:
        raise RuntimeError('fixture inherited Git routing/configuration overrides')


def _create_commit_git_proxy(root, git):
    proxy_dir = Path(root) / 'commit-git-proxy'
    proxy_dir.mkdir(mode=0o700)
    real_git = Path(git).resolve(strict=True)
    if not real_git.is_file():
        raise RuntimeError('commit Git proxy requires the pinned real Git executable')
    proxy = proxy_dir / 'git'
    proxy.write_text(
        '#!/bin/sh\nset -eu\n'
        'exec /usr/bin/env GIT_CONFIG_NOSYSTEM=1 '
        f'GIT_CONFIG_GLOBAL={shlex.quote(os.devnull)} '
        f'{shlex.quote(str(real_git))} "$@"\n')
    proxy.chmod(0o700)
    return proxy_dir


def _commit_argv(playground, args):
    runner_env = playground['runner'].env
    existing_path = runner_env.get('PATH')
    proxy_dir = Path(playground['commit_git_proxy'])
    proxy = proxy_dir / 'git'
    if (not isinstance(existing_path, str) or not existing_path
            or proxy_dir.is_symlink() or not proxy_dir.is_dir()
            or proxy.is_symlink() or not proxy.is_file() or not os.access(proxy, os.X_OK)):
        raise RuntimeError('fixture commit Git proxy is missing or invalid')
    if not isinstance(args, (list, tuple)) or not all(isinstance(arg, str) for arg in args):
        raise ValueError('commit command arguments must be strings')
    child_path = os.pathsep.join((str(proxy_dir), existing_path))
    return ['/usr/bin/env', '-u', 'GIT_CONFIG_GLOBAL', '-u', 'GIT_CONFIG_NOSYSTEM',
            f'PATH={child_path}', str(playground['binary']), *args]


def _commit_git_isolation_context(playground):
    runner_env = playground['runner'].env
    proxy_dir = Path(playground['commit_git_proxy'])
    proxy = proxy_dir / 'git'
    child_path = os.pathsep.join((str(proxy_dir), runner_env['PATH']))
    return {
        'status': 'verified',
        'broker_child_unsets': ['GIT_CONFIG_GLOBAL', 'GIT_CONFIG_NOSYSTEM'],
        'provider_git_config': {
            'GIT_CONFIG_GLOBAL': os.devnull,
            'GIT_CONFIG_NOSYSTEM': '1',
        },
        'real_git_sha256': hash_file(Path(playground['git']).resolve(strict=True),
                                     MAX_BINARY_BYTES)['sha256'],
        'proxy_sha256': hash_file(proxy, MAX_CONTRACT_BYTES)['sha256'],
        'commit_child_path_sha256': hashlib.sha256(child_path.encode('utf-8')).hexdigest(),
    }


def _lease_plan_payload(row):
    payload = _json(row, 'lease plan')
    paths = payload.get('paths') if isinstance(payload, dict) else None
    if not isinstance(paths, list) or len(paths) != 1 or not isinstance(paths[0], dict):
        raise RuntimeError('lease plan did not return exactly one path result')
    return paths[0]


def _required_cli_capability_failure(error, receipt):
    text = f"{error}\n{receipt.get('stderr', '')}\n{receipt.get('stdout', '')}".lower()
    recognized = ('unknown command', 'unknown option', 'unrecognized option',
                  'unexpected argument', 'not supported by this version')
    return any(phrase in text for phrase in recognized)


class MatrixCommandRunner(BoundedCommandRunner):
    def __init__(self, fixture_root, output, env, receipts, command_timeout_seconds):
        super().__init__(fixture_root, output, env, receipts)
        self.command_timeout_seconds = command_timeout_seconds

    def run(self, argv, cwd, label, *args, **kwargs):
        kwargs.setdefault('timeout', self.command_timeout_seconds)
        return super().run(argv, cwd, label, *args, **kwargs)


@contextmanager
def action_playground(name, binary, git, sample_output, command_timeout_seconds):
    with scenario_fixture(name, sample_output) as (root, fixture_state):
        repo, home, tools, remote = (root / key for key in
                                     ('repo', 'home', 'bin', 'remote.git'))
        for path in (repo, home, root / 'tmp', root / 'preparation',
                     root / 'host-state', root / 'host-cache', root / 'worktrees'):
            path.mkdir(mode=0o700, parents=True, exist_ok=True)
        create_shims(tools)
        env = make_env(root, tools, binary, git, remote)
        _validate_fixture_git_environment(env)
        commit_git_proxy = _create_commit_git_proxy(root, git)
        receipts = ReceiptLog(sample_output)
        runner = MatrixCommandRunner(root, sample_output, env, receipts,
                                     command_timeout_seconds)
        fixture_state['runner'] = runner
        git_version = run_git(runner, git, repo, f'{name}:git-version', '--version').strip()
        run_git(runner, git, root, f'{name}:init-remote', 'init', '--bare', '-q', '-b',
                'main', str(remote))
        run_git(runner, git, repo, f'{name}:init-repository', 'init', '-q', '--template=',
                '-b', 'main')
        run_git(runner, git, repo, f'{name}:configure-ssh', 'config', 'core.sshCommand',
                shlex.quote(str(tools / 'ssh')))
        run_git(runner, git, repo, f'{name}:configure-identity-name', 'config', 'user.name',
                'Action Matrix Fixture')
        run_git(runner, git, repo, f'{name}:configure-identity-email', 'config', 'user.email',
                'action-matrix@example.invalid')
        run_git(runner, git, repo, f'{name}:disable-signing', 'config', 'commit.gpgSign',
                'false')
        (repo / '.gitignore').write_text('.aethyme/*\n!.aethyme/config.toml\n')
        (repo / '.aethyme').mkdir()
        (repo / '.aethyme' / 'config.toml').write_text(
            '[cleanup]\nauto_remove = false\n[delivery]\npush_session_branches = false\n')
        (repo / 'tracked.txt').write_text('synthetic broker action matrix\n')
        run_git(runner, git, repo, f'{name}:stage-seed', 'add', '--all')
        run_git(runner, git, repo, f'{name}:commit-seed', 'commit', '-q', '-m',
                'chore(perf): seed action matrix Playground')
        baseline = run_git(runner, git, repo, f'{name}:baseline-head', 'rev-parse', 'HEAD').strip()
        seed_tree = run_git(runner, git, repo, f'{name}:seed-tree', 'rev-parse',
                            f'{baseline}^{{tree}}').strip()
        run_git(runner, git, repo, f'{name}:add-origin', 'remote', 'add', 'origin', REMOTE_URL)
        run_git(runner, git, repo, f'{name}:add-local-fetch-remote', 'remote', 'add',
                FETCH_REMOTE, str(remote))
        run_git(runner, git, repo, f'{name}:publish-main', 'push', '-q', 'origin', 'main')
        run_git(runner, git, repo, f'{name}:publish-provable-fetch-ref', 'push', '-q', 'origin',
                'main:refs/heads/perf-proven')
        yield {
            'root': root,
            'repo': repo,
            'remote': remote,
            'runner': runner,
            'receipts': receipts,
            'baseline_commit': baseline,
            'seed_tree': seed_tree,
            'git_version': git_version,
            'binary': binary,
            'git': git,
            'commit_git_proxy': commit_git_proxy,
            'sample_output': sample_output,
        }


def _cli(playground, args, cwd, label, timeout=None):
    argv = [str(playground['binary']), *args]
    options = {} if timeout is None else {'timeout': timeout}
    return playground['runner'].run(argv, cwd, label, **options)


def _setup_sessions(playground, scenario, roles):
    runner = playground['runner']
    binary = playground['binary']
    repo = playground['repo']
    root = playground['root']
    baseline = playground['baseline_commit']
    sessions = {}
    for role in roles:
        sessions[role] = _session(runner, binary, repo, root, baseline, scenario, role)
    return sessions


def _record(timings, name, row):
    duration = row.get('duration_ms')
    if (isinstance(duration, bool) or not isinstance(duration, (int, float))
            or not math.isfinite(duration) or duration < 0):
        raise RuntimeError(f'{name} did not produce a valid end-to-end duration')
    timings[name] = duration
    return row


def _check_commit(row, label):
    payload = _json(row, label)
    if (not isinstance(payload, dict) or payload.get('ok') is not True
            or payload.get('command_ran') is not True
            or payload.get('post_commit_audit_complete') is not True):
        raise RuntimeError(f'{label} did not report a successful audited commit')
    return payload


def _run_slow_hook_commits(playground, sessions, timings, semantic, hold_ms):
    runner, git, repo = (playground[key] for key in ('runner', 'git', 'repo'))
    root = playground['root']
    slow = sessions['commit_slow']
    peer = sessions['commit_peer']
    slow_path = 'matrix-slow-commit.txt'
    peer_path = 'matrix-peer-commit.txt'
    for session, relative, contents, label in (
            (slow, slow_path, 'slow commit\n', 'slow'),
            (peer, peer_path, 'independent commit\n', 'peer')):
        _cli(playground, ['broker', 'advanced', 'leases', 'claim', relative, '--session',
                          str(session['id']), '--json'], session['worktree'],
             f'commit-setup:{label}:claim')
        (Path(session['worktree']) / relative).write_text(contents)
        run_git(runner, git, Path(session['worktree']), f'commit-setup:{label}:stage',
                'add', '--', relative)

    hook_dir = root / 'slow-hook'
    hook_dir.mkdir(mode=0o700)
    entered = root / 'slow-hook-entered'
    release = root / 'slow-hook-release'
    hook = hook_dir / 'pre-commit'
    hook.write_text(
        '#!/bin/sh\nset -eu\n'
        'top=$(git rev-parse --show-toplevel)\n'
        f'if [ "$top" = {shlex.quote(str(slow["worktree"]))} ]; then\n'
        f'  : > {shlex.quote(str(entered))}\n'
        f'  while [ ! -f {shlex.quote(str(release))} ]; do sleep 0.01; done\n'
        'fi\n')
    hook.chmod(0o700)
    run_git(runner, git, repo, 'commit-setup:configure-hook', 'config', 'core.hooksPath',
            str(hook_dir))

    def commit(session, message, label):
        return runner.run(
            _commit_argv(playground,
                         ['broker', 'commit', '--session', str(session['id']), '--timeout', '30',
                          '--json', '--', '-m', message]),
            session['worktree'], label)

    start_commit_ns = None
    enter_deadline = time.monotonic() + 10
    slow_row = None
    peer_row = None
    with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
        future = executor.submit(commit, slow, 'perf: matrix slow-hook commit',
                                 'action:broker_commit_slow_hook')
        try:
            while not entered.exists():
                if future.done():
                    future.result()
                    raise RuntimeError('slow pre-commit hook exited before its barrier')
                if time.monotonic() >= enter_deadline:
                    raise RuntimeError('slow pre-commit hook did not reach its barrier')
                time.sleep(0.01)
            start_commit_ns = time.monotonic_ns()
            peer_row = commit(peer, 'perf: matrix independent commit',
                              'action:broker_commit_independent')
            _check_commit(peer_row, 'independent session commit')
            minimum_release_ns = start_commit_ns + hold_ms * 1_000_000
            while time.monotonic_ns() < minimum_release_ns:
                time.sleep(0.005)
        finally:
            # Always release a blocked hook, even if the independent commit fails.
            release.touch(exist_ok=True)
            slow_row = future.result(timeout=runner.command_timeout_seconds + 10)
    if slow_row is None or peer_row is None:
        raise RuntimeError('slow-hook commit pair did not complete')
    _check_commit(slow_row, 'slow-hook session commit')
    _record(timings, 'broker_commit_slow_hook', slow_row)
    _record(timings, 'broker_commit_independent', peer_row)
    semantic['commit_git_isolation'] = _commit_git_isolation_context(playground)
    semantic['slow_hook_independent_commit'] = {
        'status': 'verified',
        'peer_completed_before_hook_release': True,
        'minimum_hook_hold_ms': hold_ms,
        'slow_commit_ms': slow_row['duration_ms'],
        'independent_commit_ms': peer_row['duration_ms'],
        'hook_entered': entered.is_file(),
        'hook_released': release.is_file(),
    }


def _operation_report(row):
    payload = _json(row, 'coordinated Git operation')
    operation = payload.get('operation') if isinstance(payload, dict) else None
    if not isinstance(operation, dict):
        raise RuntimeError('coordinated Git result omitted its operation identity')
    operation_id = operation.get('id')
    if isinstance(operation_id, bool) or not isinstance(operation_id, int) or operation_id <= 0:
        raise RuntimeError('coordinated Git result has an invalid operation id')
    return payload, operation


FETCH_FLAGS = [
    '--atomic', '--no-tags', '--no-write-fetch-head', '--no-recurse-submodules',
    '--no-auto-maintenance', '--no-write-commit-graph', '--no-prune',
    '--no-prune-tags', '--refmap=',
]


def _fetch_profile_witness(payload, expected):
    operation = payload.get('operation') if isinstance(payload, dict) else None
    target = payload.get('resolved_target') if isinstance(payload, dict) else None
    if not isinstance(operation, dict) or not isinstance(target, dict):
        raise RuntimeError('fetch omitted its operation or resolved-remote identity')
    operation_id = operation.get('id')
    session_id = operation.get('session_id')
    if (isinstance(operation_id, bool) or not isinstance(operation_id, int)
            or operation_id <= 0
            or isinstance(session_id, bool) or not isinstance(session_id, int)
            or operation.get('provider') != 'git'
            or operation.get('session_id') != expected['session_id']
            or operation.get('status') != 'succeeded'
            or operation.get('repository') != expected['repository']):
        raise RuntimeError('fetch operation identity or successful status did not match')
    try:
        command = json.loads(operation['command_json'])
    except (KeyError, TypeError, json.JSONDecodeError) as error:
        raise RuntimeError('fetch operation omitted its recorded Git argv') from error
    if command != ['git', *expected['expected_args']]:
        raise RuntimeError('fetch operation did not record the exact bounded argv profile')
    evidence = target.get('evidence')
    fetch_evidence = evidence.get('fetch') if isinstance(evidence, dict) else None
    push_evidence = evidence.get('push') if isinstance(evidence, dict) else None
    if (target.get('remote_name') != expected['remote_name']
            or target.get('normalized_host') != 'local'
            or target.get('repository_path') != expected['remote_path']
            or target.get('coordination_key') != expected['repository']
            or target.get('caller_assertion') is not None
            or not isinstance(evidence, dict)
            or evidence.get('caller_assertion') != 'not_supplied'
            or evidence.get('identity') != 'fetch_and_push_matched'
            or not isinstance(fetch_evidence, dict)
            or fetch_evidence.get('syntax') != 'local_path'
            or not isinstance(push_evidence, dict)
            or push_evidence.get('syntax') != 'local_path'):
        raise RuntimeError('fetch operation did not bind the expected local-path identity')

    details_raw = operation.get('details_json')
    if details_raw is None:
        return None
    if not isinstance(details_raw, str):
        raise RuntimeError('fetch operation details were not serialized JSON')
    try:
        details = json.loads(details_raw)
    except json.JSONDecodeError as error:
        raise RuntimeError('fetch operation details were malformed') from error
    journal = details.get('fetch_reconciliation') if isinstance(details, dict) else None
    if journal is None:
        return None
    if not isinstance(journal, dict):
        raise RuntimeError('fetch reconciliation proof was not an object')
    footprint = journal.get('footprint')
    expected_footprint = {
        'remote': expected['remote_name'],
        'source_ref': expected['source_ref'],
        'destination_ref': expected['destination_ref'],
    }
    common_raw = journal.get('common_directory')
    if not isinstance(common_raw, str) or not Path(common_raw).is_absolute():
        raise RuntimeError('fetch proof common-directory path was not absolute')
    try:
        common_directory = Path(common_raw).resolve(strict=True)
    except (KeyError, OSError, TypeError, ValueError) as error:
        raise RuntimeError('fetch proof common-directory identity was unavailable') from error
    provider = journal.get('provider')
    completion = journal.get('completion')
    if not isinstance(provider, dict) or not isinstance(completion, dict):
        raise RuntimeError('fetch proof omitted its actual provider completion witness')
    provider_pid = provider.get('pid')
    provider_group = provider.get('process_group')
    provider_start = provider.get('start_time')
    completion_pid = completion.get('pid')
    completion_exit = completion.get('exit_code')
    schema_version = journal.get('schema_version')
    common_device = journal.get('common_device')
    common_inode = journal.get('common_inode')
    if (isinstance(schema_version, bool) or schema_version != 1
            or isinstance(journal.get('operation_id'), bool)
            or not isinstance(journal.get('operation_id'), int)
            or journal.get('operation_id') != operation_id
            or journal.get('repository') != expected['repository']
            or footprint != expected_footprint
            or not common_directory.is_absolute()
            or common_directory != expected['common_directory']
            or isinstance(common_device, bool) or not isinstance(common_device, int)
            or common_device != expected['common_device']
            or isinstance(common_inode, bool) or not isinstance(common_inode, int)
            or common_inode != expected['common_inode']
            or journal.get('pre_destination') is not None
            or isinstance(provider_pid, bool) or not isinstance(provider_pid, int)
            or provider_pid <= 1
            or isinstance(provider_group, bool) or not isinstance(provider_group, int)
            or provider_group <= 1
            or isinstance(provider_start, bool) or not isinstance(provider_start, int)
            or provider_start <= 0
            or not isinstance(provider.get('start_time_clock'), str)
            or not provider['start_time_clock']
            or isinstance(completion_pid, bool) or not isinstance(completion_pid, int)
            or completion_pid != provider_pid
            or completion.get('success') is not True
            or isinstance(completion_exit, bool) or not isinstance(completion_exit, int)
            or completion_exit != 0
            or isinstance(completion.get('observed_at'), bool)
            or not isinstance(completion.get('observed_at'), int)
            or completion['observed_at'] <= 0):
        raise RuntimeError('fetch reconciliation proof did not match the exact completed profile')
    return {
        'schema_version': 1,
        'provider_pid': provider_pid,
        'provider_start_time': provider_start,
        'completion_observed_at': completion['observed_at'],
    }


def _missing_fetch_witness_semantic(role, operation_id, duration_ms):
    if role == 'candidate':
        raise RuntimeError('candidate omitted durable strict-fetch completion evidence')
    if role != 'baseline':
        raise RuntimeError(f'unknown strict-fetch measurement role: {role}')
    return {
        'status': 'unsupported',
        'reason': 'baseline completed the fetch but did not persist fetch_reconciliation schema 1',
        'operation_id': operation_id,
        'observed_command_duration_ms': duration_ms,
        'comparison_eligible': False,
    }


def _baseline_unsupported_fetch_option(role, receipt, expected_argv):
    if role != 'baseline' or not isinstance(receipt, dict):
        return None
    exit_code = receipt.get('exit_code')
    if (receipt.get('label') != 'action:fetch_provable'
            or receipt.get('argv') != expected_argv
            or isinstance(exit_code, bool) or not isinstance(exit_code, int)
            or exit_code <= 0
            or receipt.get('failure') != f'command exited with status {exit_code}'
            or receipt.get('stdout_complete') is not False
            or receipt.get('stdout_utf8_valid') is not True
            or not isinstance(receipt.get('stdout'), str)
            or not isinstance(receipt.get('stderr'), str)):
        return None
    for field, limit in (('stdout_bytes', MAX_STDOUT_BYTES),
                         ('stderr_bytes', MAX_STDERR_BYTES)):
        size = receipt.get(field)
        if (isinstance(size, bool) or not isinstance(size, int)
                or size < 0 or size > limit):
            return None
    duration = receipt.get('duration_ms')
    if (isinstance(duration, bool) or not isinstance(duration, (int, float))
            or not math.isfinite(duration) or duration < 0):
        return None
    for field in ('stdout_sha256', 'stderr_sha256'):
        digest = receipt.get(field)
        if not isinstance(digest, str) or not re.fullmatch(r'[0-9a-f]{64}', digest):
            return None
    output = f"{receipt['stdout']}\n{receipt['stderr']}"
    for option in BASELINE_UNSUPPORTED_FETCH_OPTIONS:
        refusal = (
            f'unsupported fetch option "{option}" prevents deterministic remote resolution')
        if refusal in output:
            return option
    return None


def _run_provable_fetch(playground, sessions, timings, semantic, role):
    _validate_fixture_git_environment(playground['runner'].env)
    session = sessions['fetch_provable']
    repo = Path(playground['repo'])
    worktree = Path(session['worktree'])
    remote_path = Path(playground['remote']).resolve(strict=True)
    expected_repository_path = remote_path.as_posix().removesuffix('.git').rstrip('/')
    expected_repository = f'local:{expected_repository_path}'
    expected_ref = FETCH_DESTINATION_REF
    before_ref = run_git(
        playground['runner'], playground['git'], worktree,
        'fetch-proven:pre-ref', 'for-each-ref', '--count=2',
        '--format=%(refname) %(objectname)', '--', expected_ref).strip()
    if before_ref:
        raise RuntimeError('strict-fetch destination unexpectedly existed before the sample')
    common_raw = run_git(playground['runner'], playground['git'], worktree,
                         'fetch-proven:common-directory', 'rev-parse',
                         '--git-common-dir').strip()
    common_directory = Path(common_raw)
    if not common_directory.is_absolute():
        common_directory = worktree / common_directory
    common_directory = common_directory.resolve(strict=True)
    common_metadata = common_directory.stat()
    source_ref = FETCH_SOURCE_REF
    refspec = f'{source_ref}:{expected_ref}'
    args = [
        'broker', 'advanced', 'git', '--session', str(session['id']),
        '--effect', 'write', '--scope', 'fetch',
        '--reason', 'synthetic paired action measurement', '--json', '--',
        'fetch', *FETCH_FLAGS, '--', FETCH_REMOTE, refspec,
    ]
    ssh_command = run_git(playground['runner'], playground['git'], repo,
                           'fetch-proven:read-fixture-ssh-command',
                           'config', '--get', 'core.sshCommand').strip()
    if not ssh_command:
        raise RuntimeError('the fixture SSH shim was not configured before strict fetch')
    run_git(playground['runner'], playground['git'], repo,
            'fetch-proven:disable-ssh-override', 'config', '--unset', 'core.sshCommand')
    try:
        expected_argv = [str(playground['binary']), *args]
        try:
            row = _cli(playground, args, worktree, 'action:fetch_provable')
        except RuntimeError:
            receipt = playground['runner'].receipts.rows[-1] \
                if playground['runner'].receipts.rows else None
            unsupported_option = _baseline_unsupported_fetch_option(
                role, receipt, expected_argv)
            if unsupported_option is None:
                raise
            refusal_reason = (
                f'baseline CLI refused strict-fetch option {unsupported_option} before '
                'deterministic remote resolution; no fetch operation was created')
            semantic['fetch_provable'] = {
                'status': 'unsupported',
                'reason': refusal_reason,
                'unsupported_option': unsupported_option,
                'observed_command_duration_ms': receipt['duration_ms'],
                'comparison_eligible': False,
            }
            semantic['fetch_recovery_show'] = {
                'status': 'unsupported',
                'reason': refusal_reason + '; operation readback was not attempted',
                'comparison_eligible': False,
            }
            return
    finally:
        run_git(playground['runner'], playground['git'], repo,
                'fetch-proven:restore-ssh-override', 'config', 'core.sshCommand', ssh_command)
    payload, operation = _operation_report(row)
    if operation.get('status') != 'succeeded':
        raise RuntimeError('provable fetch did not have a succeeded operation status')
    actual_ref = run_git(playground['runner'], playground['git'],
                         worktree, 'fetch-proven:verify-ref',
                         'rev-parse', '--verify', expected_ref).strip()
    if actual_ref != playground['baseline_commit']:
        raise RuntimeError('fetch did not update exactly the pinned remote-tracking ref')
    witness = _fetch_profile_witness(payload, {
        'session_id': session['id'],
        'repository': expected_repository,
        'remote_path': expected_repository_path,
        'remote_name': FETCH_REMOTE,
        'source_ref': source_ref,
        'destination_ref': expected_ref,
        'common_directory': common_directory,
        'common_device': common_metadata.st_dev,
        'common_inode': common_metadata.st_ino,
        'expected_args': ['fetch', *FETCH_FLAGS, '--', FETCH_REMOTE, refspec],
    })
    if witness is None:
        semantic['fetch_provable'] = _missing_fetch_witness_semantic(
            role, operation['id'], row['duration_ms'])
    else:
        _record(timings, 'fetch_provable', row)
        semantic['fetch_provable'] = {
            'status': 'verified',
            'comparison_eligible': True,
            'operation_id': operation['id'],
            'operation_status': operation['status'],
            'expected_remote_ref': expected_ref,
            'expected_commit': playground['baseline_commit'],
            'observed_commit': actual_ref,
            'fetch_reconciliation_schema_version': witness['schema_version'],
            'provider_pid': witness['provider_pid'],
            'provider_start_time': witness['provider_start_time'],
            'completion_observed_at': witness['completion_observed_at'],
            'canonical_repository': expected_repository,
            'remote_name': FETCH_REMOTE,
            'git_environment': (
                'fixture allowlist; inherited Git routing/configuration/SSH selectors absent; '
                'local-path remote with no custom core.sshCommand during fetch'),
        }
    show = _cli(playground,
                ['broker', 'advanced', 'operations', 'show', str(operation['id']), '--json'],
                playground['repo'], 'action:fetch_recovery_show')
    shown = _json(show, 'fetch recovery operation show')
    recovered = shown.get('operation') if isinstance(shown, dict) else None
    if (not isinstance(recovered, dict) or recovered.get('id') != operation['id']
            or recovered.get('status') != 'succeeded'
            or recovered.get('details_json') != operation.get('details_json')):
        raise RuntimeError('fetch recovery readback did not preserve the exact success witness')
    _record(timings, 'fetch_recovery_show', show)
    semantic['fetch_recovery_show'] = {
        'status': 'verified',
        'operation_id': operation['id'],
        'operation_status': recovered['status'],
        'same_details_json': recovered.get('details_json') == operation.get('details_json'),
    }


def _parse_deploy_preview(text, expected_source):
    source = re.search(
        r'Configured origin/default ref: origin/refs/heads/main @ ([0-9a-f]{40,64})', text)
    digest = re.search(r'Plan digest: ([0-9a-f]{64})', text)
    full_diff = re.search(
        r'Raw generated diff: ([0-9]+) bytes; SHA-256 \(raw\) ([0-9a-f]{64})', text)
    migration_heading = re.search(r'(?m)^Migration diff:\r?\n', text)
    diff_truncation = re.search(
        r'(?m)^\[terminal-safe diff preview truncated at a complete line boundary: '
        r'showing ([0-9]+) escaped bytes from ([0-9]+) raw diff bytes; '
        r'raw SHA-256 covers the complete proposal\][ \t]*\r?$', text)
    output_truncation = re.search(
        r'(?m)^\[rendered preview truncated at a complete line boundary; '
        r'output byte limit reached\][ \t]*\r?$', text)
    output_truncated = output_truncation is not None
    sections = ('Aethyme-owned outputs:', 'Managed-block outputs:',
                'Repository-owned policy/config outputs:')
    present_sections = [name for name in sections if name in text]
    generated_bytes = int(full_diff.group(1)) if full_diff else None
    preview_escaped_bytes = int(diff_truncation.group(1)) if diff_truncation else None
    preview_raw_bytes = int(diff_truncation.group(2)) if diff_truncation else None
    source_matches_seed = bool(source and source.group(1) == expected_source)
    ownership_grouping = (
        'Generated outputs grouped by ownership; per-file generator-source mapping is unavailable.'
        in text)
    inventory_supported = bool(
        source and digest and ownership_grouping and len(present_sections) == len(sections)
        and migration_heading)
    migration_body = text[migration_heading.end():] if migration_heading else ''
    for marker in (
            diff_truncation.group(0) if diff_truncation else None,
            output_truncation.group(0) if output_truncation else None):
        if marker:
            migration_body = migration_body.split(marker, maxsplit=1)[0]
    migration_body_size = len(migration_body.encode('utf-8'))
    migration_body_content = migration_body.strip()
    empty_diff_sha256 = hashlib.sha256(b'').hexdigest()
    truncation_evidence_matches = bool(
        not diff_truncation
        or (preview_raw_bytes == generated_bytes
            and preview_escaped_bytes in {
                migration_body_size, max(0, migration_body_size - 1)}))
    complete_diff_body = bool(
        full_diff and migration_heading and not diff_truncation and not output_truncated
        and ((generated_bytes == 0 and full_diff.group(2) == empty_diff_sha256
              and migration_body_content == '(no changes)')
             or (generated_bytes is not None and generated_bytes > 0
                 and bool(migration_body_content))))
    generated_content_available = complete_diff_body
    preview_truncated = bool(diff_truncation or output_truncated)
    content_identity = None
    if inventory_supported and generated_content_available:
        content_identity = {
            'contract': 'first_enrollment_generated_diff_preview_v1',
            'source_commit': source.group(1),
            'raw_diff_bytes': generated_bytes,
            'raw_diff_sha256': full_diff.group(2),
            'rendered_diff_preview_sha256': hashlib.sha256(
                migration_body.encode('utf-8')).hexdigest(),
        }
    return {
        'source_commit': source.group(1) if source else None,
        'source_matches_seed': source_matches_seed,
        'plan_digest': digest.group(1) if digest else None,
        'full_generated_diff_bytes': generated_bytes,
        'full_generated_diff_sha256': full_diff.group(2) if full_diff else None,
        'ownership_sections_present': present_sections,
        'ownership_grouping_present': ownership_grouping,
        'migration_preview_escaped_bytes': preview_escaped_bytes,
        'migration_preview_raw_bytes': preview_raw_bytes,
        'migration_preview_raw_sha256': None,
        'truncation_evidence_matches': truncation_evidence_matches,
        'migration_diff_present': 'Migration diff:' in text,
        'truncation_explicit': preview_truncated,
        'inventory_supported': inventory_supported,
        'generated_content_available': generated_content_available,
        'content_contract': (content_identity['contract']
                             if content_identity is not None else None),
        'content_identity': content_identity,
        'preview_bytes': len(text.encode('utf-8')),
        'preview_sha256': hashlib.sha256(text.encode('utf-8')).hexdigest(),
    }


def _classify_deploy_preview(text, row, expected_source):
    preview = _parse_deploy_preview(text, expected_source)
    preview['utf8_complete'] = (
        row.get('stdout_complete') is True
        and row.get('stdout_utf8_valid') is True)
    if not preview['inventory_supported']:
        preview['status'] = 'output_incomplete'
    elif not preview['source_matches_seed']:
        preview['status'] = 'source_identity_mismatch'
    elif not preview['utf8_complete']:
        preview['status'] = 'output_incomplete'
    elif preview['generated_content_available']:
        preview['status'] = 'supported_content_available'
    else:
        preview['status'] = 'supported_inventory_content_missing'
    preview['comparison_eligible'] = preview['status'] == 'supported_content_available'
    if not preview['comparison_eligible']:
        preview['comparison_ineligibility_reason'] = (
            'preview is incomplete, has the wrong source identity, or lacks '
            'matching raw-diff content evidence')
    return preview


def _tree_fingerprint(root):
    root = Path(root)
    if root.is_symlink():
        raise RuntimeError('fixture root is a symlink')
    root = root.resolve(strict=True)
    root_info = root.lstat()
    if not stat.S_ISDIR(root_info.st_mode):
        raise RuntimeError('fixture root is not a real directory')
    rows = [('root', '.', stat.S_IMODE(root_info.st_mode))]
    entry_count = 0
    path_bytes = 0
    file_count = 0
    total_bytes = 0
    for directory, names, files in os.walk(root, followlinks=False, onerror=fail_walk):
        directory_path = Path(directory)
        if len(directory_path.relative_to(root).parts) > MAX_TREE_DEPTH:
            raise RuntimeError('fixture exceeded its tree-depth limit')
        for name in names:
            path = directory_path / name
            info = path.lstat()
            if path.is_symlink() or not stat.S_ISDIR(info.st_mode):
                raise RuntimeError('fixture contains a non-directory tree entry')
            relative = path.relative_to(root).as_posix()
            path_bytes += len(os.fsencode(relative))
            entry_count += 1
            if entry_count > MAX_TREE_ENTRIES or path_bytes > MAX_TREE_PATH_BYTES:
                raise RuntimeError('fixture exceeded its entry or path-byte limit')
            rows.append(('directory', relative, stat.S_IMODE(info.st_mode)))
        for name in files:
            path = directory_path / name
            info = path.lstat()
            if path.is_symlink() or not stat.S_ISREG(info.st_mode):
                raise RuntimeError('fixture contains a non-regular file')
            relative = path.relative_to(root).as_posix()
            path_bytes += len(os.fsencode(relative))
            entry_count += 1
            file_count += 1
            total_bytes += info.st_size
            if (entry_count > MAX_TREE_ENTRIES or path_bytes > MAX_TREE_PATH_BYTES
                    or total_bytes > MAX_OUTPUT_BYTES):
                raise RuntimeError('fixture exceeded its tree inventory limits')
            file_identity = hash_file(path, MAX_OUTPUT_BYTES)
            after_info = path.lstat()
            if (file_identity['bytes'] != info.st_size
                    or (after_info.st_dev, after_info.st_ino, after_info.st_size,
                        after_info.st_mtime_ns, stat.S_IMODE(after_info.st_mode))
                    != (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
                        stat.S_IMODE(info.st_mode))):
                raise RuntimeError('fixture file changed while its identity was captured')
            rows.append(('file', relative,
                         stat.S_IMODE(info.st_mode), info.st_size,
                         file_identity['sha256']))
    rows.sort()
    digest = hashlib.sha256(canonical_json_bytes(rows)).hexdigest()
    return {'files': file_count, 'bytes': total_bytes,
            'entries': entry_count, 'path_bytes': path_bytes,
            'tree_sha256': digest}


def _bounded_output_control(playground):
    runner, root = playground['runner'], playground['root']
    before = _tree_fingerprint(root)
    command = [sys.executable, '-c',
               'import os\nblock=b"x"*(64*1024)\nwhile True: os.write(1, block)']
    label = 'control:bounded_output_limit'
    try:
        runner.run(command, playground['repo'], label)
    except RuntimeError:
        row = runner.receipts.rows[-1]
    else:
        raise RuntimeError('bounded output control unexpectedly completed')
    after = _tree_fingerprint(root)
    expected_failure = f"stdout exceeded its {MAX_STDOUT_BYTES}-byte limit"
    if (row.get('failure') != expected_failure or row.get('stdout_complete') is not False
            or row.get('stdout_bytes') != MAX_STDOUT_BYTES
            or runner.cleanup_incomplete or before != after):
        raise RuntimeError('bounded output control did not fail closed with an unchanged fixture')
    return {
        'status': 'expected_refusal_observed',
        'failure': row['failure'],
        'stdout_bytes': row['stdout_bytes'],
        'stdout_sha256': row['stdout_sha256'],
        'process_group_cleanup_complete': not runner.cleanup_incomplete,
        'fixture_inventory_unchanged': before == after,
        'fixture_tree_sha256': before['tree_sha256'],
        'receipt_label': label,
        'comparison_eligible': False,
    }


def _measure_profile(role, repetition, identity, git, output, contracts,
                     baseline_source_commit=None):
    sample_name = f'r{repetition + 1:02d}-{role}'
    sample_output = output / 'samples' / sample_name
    sample_output.mkdir(mode=0o700, parents=True, exist_ok=False)
    sample = {
        'schema_version': 1,
        'role': role,
        'repetition': repetition + 1,
        'execution_order': list(paired_order(repetition)),
        'binary': identity,
        'scenario_manifest_sha256': contracts['scenario_sha256'],
        'matrix_budget_sha256': contracts['matrix_sha256'],
        'absolute_budget_sha256': contracts['absolute_sha256'],
        'timings_ms': {},
        'semantic': {},
        'fixture': {},
        'status': 'running',
        'sample_json': str(sample_output / 'sample.json'),
    }
    fixture_root = None
    runner = None
    try:
        with action_playground(
                sample_name, Path(identity['cli_path']), git, sample_output,
                contracts['matrix']['runner']['command_timeout_seconds']) as playground:
            fixture_root = playground['root']
            runner = playground['runner']
            version_row = runner.run([identity['cli_path'], '--version'], playground['repo'],
                                     f'{sample_name}:cli-version')
            engine_row = runner.run([identity['engine_path'], '--version'], playground['repo'],
                                    f'{sample_name}:engine-version')
            version_text = version_row['stdout'].strip()
            engine_version_text = engine_row['stdout'].strip()
            if not version_text or not engine_version_text:
                raise RuntimeError('binary pair did not report both version identities')
            build_commit = _build_commit(version_text)
            if role == 'baseline' and build_commit != baseline_source_commit:
                raise RuntimeError(
                    'baseline CLI build_commit does not match the retained v0.8.41 source commit')
            sample['binary_version'] = {
                'aethyme': version_text,
                'aethyme_build_commit': build_commit,
                'engine_cli': engine_version_text,
                'engine_build_commit': _build_commit(engine_version_text),
            }
            sample['fixture'].update({
                'profile': 'single_repository_multi_session',
                'synthetic': True,
                'network_sandbox': False,
                'git_version': playground['git_version'],
                'seed_commit': playground['baseline_commit'],
                'seed_tree': playground['seed_tree'],
                'canonical_repository': WORKTREE_REPO,
            })
            sessions = _setup_sessions(playground, sample_name, ('primary',))
            start_row = runner.receipts.rows[-1]
            if start_row.get('label') != f'{sample_name}:start:primary':
                raise RuntimeError('measured start receipt was not the first session creation')
            _record(sample['timings_ms'], 'start', start_row)
            sample['semantic']['start'] = {
                'status': 'verified',
                'measured_role': 'first_session_in_fresh_fixture',
                'duration_ms': start_row['duration_ms'],
            }
            status_row, status_counts = _status(
                runner, Path(identity['cli_path']), sessions['primary']['worktree'],
                'action:status_summary', 1)
            _record(sample['timings_ms'], 'status_summary', status_row)
            sample['semantic']['status_summary'] = {
                'status': 'verified', 'session_counts': status_counts,
                'profile': 'single_open_session_for_v1_routine_status_comparability',
            }

            sessions.update(_setup_sessions(
                playground, sample_name,
                ('lease_owner', 'overlap_reader', 'commit_slow', 'commit_peer',
                 'fetch_provable')))
            sample['fixture']['session_count'] = len(sessions)
            sample['fixture']['session_ids'] = {
                role_name: session['id'] for role_name, session in sessions.items()}

            target = 'tracked.txt'
            plan_row = _cli(
                playground, ['broker', 'advanced', 'leases', 'plan', target, '--session',
                             str(sessions['lease_owner']['id']), '--json'],
                sessions['lease_owner']['worktree'], 'action:lease_plan')
            plan_path = _lease_plan_payload(plan_row)
            if plan_path.get('path') != target or plan_path.get('would_conflict') is not False:
                raise RuntimeError('clear lease plan did not describe the expected path')
            _record(sample['timings_ms'], 'lease_plan', plan_row)
            sample['semantic']['lease_plan'] = {
                'status': 'verified', 'path': target, 'would_conflict': False,
            }

            claim_row = _cli(
                playground, ['broker', 'advanced', 'leases', 'claim', target, '--session',
                             str(sessions['lease_owner']['id']), '--json'],
                sessions['lease_owner']['worktree'], 'action:lease_claim')
            claim_report = _json(claim_row, 'lease claim')
            if (claim_report.get('path') != target
                    or claim_report.get('session_id') != sessions['lease_owner']['id']
                    or claim_report.get('accepted') is not True
                    or claim_report.get('blockers') != []):
                raise RuntimeError('lease claim did not accept only the requested exact path')
            _record(sample['timings_ms'], 'lease_claim', claim_row)
            sample['semantic']['lease_claim'] = {
                'status': 'verified', 'path': target,
                'session_id': sessions['lease_owner']['id'],
            }

            release_row = _cli(
                playground, ['broker', 'advanced', 'leases', 'release', target, '--session',
                             str(sessions['lease_owner']['id']), '--json'],
                sessions['lease_owner']['worktree'], 'action:lease_release')
            _json(release_row, 'lease release')
            _record(sample['timings_ms'], 'lease_release', release_row)
            sample['semantic']['lease_release'] = {
                'status': 'verified', 'path': target,
                'session_id': sessions['lease_owner']['id'],
            }

            overlap_target = 'tracked.txt'
            _cli(playground, ['broker', 'advanced', 'leases', 'claim', overlap_target, '--session',
                              str(sessions['lease_owner']['id']), '--json'],
                 sessions['lease_owner']['worktree'], 'overlap-setup:claim')
            overlap_row = _cli(
                playground, ['broker', 'advanced', 'leases', 'plan', overlap_target, '--session',
                             str(sessions['overlap_reader']['id']), '--json'],
                sessions['overlap_reader']['worktree'], 'action:overlap_detail')
            overlap_path = _lease_plan_payload(overlap_row)
            conflicts = overlap_path.get('conflicts')
            exact_owner = (isinstance(conflicts, list) and any(
                isinstance(item, dict)
                and item.get('session_id') == sessions['lease_owner']['id']
                and item.get('path') == overlap_target for item in conflicts))
            if (overlap_path.get('would_conflict') is not True or not exact_owner):
                raise RuntimeError('overlap plan omitted the exact claimed-path owner')
            _record(sample['timings_ms'], 'overlap_detail', overlap_row)
            _cli(playground, ['broker', 'advanced', 'leases', 'release', overlap_target, '--session',
                              str(sessions['lease_owner']['id']), '--json'],
                 sessions['lease_owner']['worktree'], 'overlap-setup:release')
            sample['semantic']['overlap_detail'] = {
                'status': 'verified',
                'path': overlap_target,
                'owner_session_id': sessions['lease_owner']['id'],
                'requester_session_id': sessions['overlap_reader']['id'],
            }

            _run_slow_hook_commits(
                playground, sessions, sample['timings_ms'], sample['semantic'],
                contracts['matrix']['runner']['slow_hook_minimum_hold_ms'])

            _run_provable_fetch(playground, sessions, sample['timings_ms'], sample['semantic'],
                                role)

            deploy_args = ['deploy', 'plan', '--repo', str(playground['repo']), '--diff']
            try:
                deploy_row = _cli(playground, deploy_args, playground['repo'],
                                  'action:deploy_plan_diff')
            except RuntimeError as error:
                receipt = runner.receipts.rows[-1] if runner.receipts.rows else {}
                if _required_cli_capability_failure(error, receipt):
                    sample['semantic']['deploy_plan_diff'] = {
                        'status': 'unsupported',
                        'reason': str(error),
                        'receipt_label': receipt.get('label'),
                        'comparison_eligible': False,
                    }
                else:
                    raise
            else:
                _record(sample['timings_ms'], 'deploy_plan_diff', deploy_row)
                preview_text = deploy_row['stdout']
                preview = _classify_deploy_preview(
                    preview_text, deploy_row, playground['baseline_commit'])
                sample['semantic']['deploy_plan_diff'] = preview

            def history_command(argv, cwd, label, redact_values=()):
                row = runner.run(argv, cwd, label, redact_values=redact_values)
                return row['stdout'], row['duration_ms']

            history = seed_released_resource_history(
                Path(identity['cli_path']), history_command, playground['root'],
                sessions['primary']['worktree'], playground['repo'],
                contracts['matrix']['runner']['history_rows'])
            sample['fixture']['aged_history'] = {
                key: value for key, value in history.items()
                if key not in {'fixture_repository_sha256'}
            }
            aged_row, aged_counts = _status(
                runner, Path(identity['cli_path']), sessions['primary']['worktree'],
                'action:status_aged_history', len(sessions))
            _record(sample['timings_ms'], 'status_aged_history', aged_row)
            sample['semantic']['status_aged_history'] = {
                'status': 'verified', 'session_counts': aged_counts,
                'aged_released_host_resource_leases': history['aged_released_host_resource_leases'],
            }

            finish_row = _cli(
                playground, ['broker', 'finish', '--session',
                             str(sessions['primary']['id']), '--json'],
                sessions['primary']['worktree'], 'action:finish')
            _json(finish_row, 'finish')
            _record(sample['timings_ms'], 'finish', finish_row)
            _, after_finish_counts = _status(
                runner, Path(identity['cli_path']), playground['repo'],
                'finish-control:status', len(sessions) - 1)
            sample['semantic']['finish'] = {
                'status': 'verified',
                'closed_session_id': sessions['primary']['id'],
                'live_sessions_after_finish': after_finish_counts['live_sessions'],
            }

            # No durable provider-exit witness is fabricated. A future harness
            # adapter may run this only when it can hold a real provider after
            # execution and prove complete process settlement before inspection.
            sample['semantic']['fetch_unprovable'] = {
                'status': 'unsupported',
                'reason': UNPROVABLE_FETCH_REASON,
                'command_dispatched': False,
                'comparison_eligible': False,
            }

            sample['semantic']['bounded_output_control'] = _bounded_output_control(playground)
            host_state = playground['root'] / 'host-state'
            host_database = host_state / 'host-resources.db'
            database_identity = None
            if history['enabled']:
                if host_database.is_symlink() or not host_database.is_file():
                    raise RuntimeError('fixture host-resource database is not a regular file')
                database_identity = hash_file(host_database, MAX_OUTPUT_BYTES)
            host_state_identity = _tree_fingerprint(host_state)
            sample['fixture'].update({
                'fixture_git_head_after_actions': run_git(
                    runner, git, playground['repo'], 'fixture-control:main-head',
                    'rev-parse', 'HEAD').strip(),
                'fixture_storage_after_actions': tree_size(playground['root']),
                'host_resource_state_scope': 'fresh isolated state per repetition and binary role',
                'host_resource_state_root': str(host_state),
                'host_resource_database': database_identity,
                'host_resource_state_identity': host_state_identity,
            })
            sample['status'] = 'complete'
    finally:
        sample['fixture']['retained_after_run'] = bool(
            fixture_root is not None and fixture_root.exists())
        sample['fixture']['cleanup_incomplete'] = bool(
            runner is not None and runner.cleanup_incomplete)
        sample['raw_receipts'] = str(sample_output / 'commands.json')
        (sample_output / 'sample.json').write_bytes(canonical_json_bytes(sample))
        output_tree_size(output)
    return sample


def _atomic_json(path, value):
    path = Path(path)
    temporary = path.with_suffix(path.suffix + '.tmp')
    temporary.write_bytes(canonical_json_bytes(value))
    temporary.replace(path)


def _harness_identity():
    files = {}
    for path in HARNESS_FILES:
        resolved = path.resolve(strict=True)
        if not resolved.is_file() or resolved.stat().st_size > MAX_CONTRACT_BYTES * 16:
            raise RuntimeError(f'harness source is not a bounded regular file: {path.name}')
        files[path.name] = hash_file(resolved, MAX_CONTRACT_BYTES * 16)
    return files


def _timed_scenarios(contracts):
    return [row['id'] for row in contracts['scenario_manifest']['scenarios']
            if row['kind'] in {'timing', 'semantic_timing'}]


def _compare_action(baseline_samples, candidate_samples, action, paired):
    all_samples = [('baseline', sample) for sample in baseline_samples]
    all_samples.extend(('candidate', sample) for sample in candidate_samples)
    ineligible = []
    if (len(baseline_samples) != paired['repetitions']
            or len(candidate_samples) != paired['repetitions']):
        ineligible.append('one or more paired samples are missing')
    for role, sample in all_samples:
        if sample.get('status') != 'complete':
            ineligible.append(f'{role}: sample {sample.get("repetition")} incomplete')
            continue
        semantic = sample.get('semantic', {}).get(action)
        if isinstance(semantic, dict):
            if semantic.get('status') == 'unsupported':
                ineligible.append(f'{role}: {semantic.get("reason", "unsupported")}')
            elif semantic.get('comparison_eligible') is False:
                ineligible.append(
                    f'{role}: {semantic.get("comparison_ineligibility_reason", "semantic check failed")}')
            elif action == 'deploy_plan_diff':
                if (semantic.get('status') != 'supported_content_available'
                        or semantic.get('comparison_eligible') is not True
                        or not isinstance(semantic.get('content_identity'), dict)):
                    ineligible.append(
                        f'{role}: complete generated-preview content is unavailable')
        elif action == 'deploy_plan_diff':
            ineligible.append(f'{role}: generated-preview semantics are missing')
    baseline = [sample.get('timings_ms', {}).get(action) for sample in baseline_samples]
    candidate = [sample.get('timings_ms', {}).get(action) for sample in candidate_samples]
    if (len(baseline) != paired['repetitions']
            or len(candidate) != paired['repetitions']
            or any(value is None for value in baseline + candidate)):
        ineligible.append('one or more paired timing samples are missing')
    if ineligible:
        return {
            'status': 'unavailable',
            'reason': '; '.join(ineligible),
            'enforcement': 'warning',
        }
    if action == 'deploy_plan_diff':
        identities = [sample['semantic'][action]['content_identity']
                      for _, sample in all_samples]
        if any(identity != identities[0] for identity in identities[1:]):
            return {
                'status': 'unavailable',
                'reason': 'baseline and candidate generated-preview content contracts differ',
                'enforcement': 'warning',
            }
    return {'status': 'compared', **compare_samples(baseline, candidate, paired)}


def _absolute_status_warning(candidate_samples, contracts):
    budget_name = 'routine_status'
    spec = contracts['absolute_budgets']['budgets'].get(budget_name)
    samples = [sample.get('timings_ms', {}).get('status_summary')
               for sample in candidate_samples]
    if not isinstance(spec, dict) or any(value is None for value in samples):
        return {'status': 'unavailable', 'budget_name': budget_name,
                'enforcement': 'warning'}
    median = statistics.median(samples)
    limit = spec['limit_ms']
    return {
        'status': 'warning' if median > limit else 'within_budget',
        'budget_name': budget_name,
        'measurement': spec.get('measurement'),
        'samples_ms': samples,
        'median_ms': median,
        'limit_ms': limit,
        'enforcement': 'warning',
    }


def _sample_summaries(samples):
    rows = []
    for sample in samples:
        rows.append({
            'role': sample.get('role'),
            'repetition': sample.get('repetition'),
            'status': sample.get('status'),
            'sample_json': sample.get('sample_json'),
            'raw_receipts': sample.get('raw_receipts'),
            'error': sample.get('error'),
        })
    return rows


def _write_run_report(output, report):
    _atomic_json(output / 'report.json', report)


def _enforce_output_limit(output):
    size = output_tree_size(output)
    return size


def _run_matrix(args):
    contracts = load_contracts()
    repetitions = args.repetitions or contracts['matrix']['paired_comparison']['repetitions']
    if isinstance(repetitions, bool) or not isinstance(repetitions, int) \
            or repetitions < 3 or repetitions > 15:
        raise ValueError('repetitions must be between 3 and 15')
    paired = dict(contracts['matrix']['paired_comparison'])
    paired['repetitions'] = repetitions

    baseline_identity = binary_identity(args.baseline_cli)
    candidate_identity = binary_identity(args.candidate_cli)
    v41_context = load_v41_context(args.v41_live_reference, baseline_identity)
    git_argument = args.git
    if not git_argument.is_absolute():
        raise ValueError('--git must name an absolute executable path')
    if any(part.lower() in {'cto_bin', 'cto-bin'} for part in git_argument.parts):
        raise ValueError('--git must name the actual Git executable, not a cto_bin shim')
    git_path = git_argument.resolve(strict=True)
    if not git_path.is_file() or git_path.stat().st_size > MAX_BINARY_BYTES:
        raise RuntimeError('Git executable is not a bounded regular file')
    with git_path.open('rb') as stream:
        if stream.read(2) == b'#!':
            raise ValueError('--git must name the actual Git executable, not a script wrapper')
    output = validate_output_path(args.output, REPO_ROOT)
    (output / 'samples').mkdir(mode=0o700)

    report = {
        'schema_version': 2,
        'status': 'running',
        'enforcement': 'warning',
        'measurement': 'complete_end_to_end_cli_wall_time',
        'unit': 'milliseconds',
        'started_wall_time_ns': time.time_ns(),
        'paired_comparison': paired,
        'comparison_policy': {
            'statistic': 'median',
            'regression_threshold': (
                'candidate median delta exceeds max(absolute jitter floor, '
                'baseline median times percentage threshold)'),
            'warning_only': True,
            'does_not_fail_command_or_ci': True,
        },
        'matrix_contract_sha256': contracts['matrix_sha256'],
        'scenario_contract_sha256': contracts['scenario_sha256'],
        'absolute_budget_contract_sha256': contracts['absolute_sha256'],
        'absolute_budget_contract_schema': 1,
        'v0_8_41_live_context': v41_context,
        'baseline': baseline_identity,
        'candidate': candidate_identity,
        'git': {'argument_path': str(git_argument), 'path': str(git_path),
                'sha256': hash_file(git_path, MAX_BINARY_BYTES)['sha256']},
        'python': {'executable': sys.executable, 'version': sys.version},
        'harness_sources': _harness_identity(),
        'fixture_contract': contracts['scenario_manifest']['fixture'],
        'scenario_ids': [row['id'] for row in contracts['scenario_manifest']['scenarios']],
        'samples': [],
        'comparisons': {},
        'absolute_warning_only': {},
        'limitations': [
            'synthetic local bare-remote fixture; not an OS network sandbox',
            'v0.8.41 rollout timings are uncontrolled context and are not a paired baseline',
            ('fetch-unprovable is intentionally unsupported; no provider outcome or journal '
             'is fabricated'),
            'small repeated sample; descriptive medians are not a significance test',
        ],
    }
    _write_run_report(output, report)

    samples_by_role = {'baseline': [], 'candidate': []}
    samples_in_execution_order = []
    failure = None
    try:
        for repetition in range(repetitions):
            for role in paired_order(repetition):
                identity = baseline_identity if role == 'baseline' else candidate_identity
                try:
                    sample = _measure_profile(
                        role, repetition, identity, str(git_path), output, contracts,
                        v41_context['source_commit'])
                except Exception as error:
                    sample_path = (output / 'samples' / f'r{repetition + 1:02d}-{role}'
                                   / 'sample.json')
                    if sample_path.is_file() and not sample_path.is_symlink():
                        try:
                            sample = json.loads(sample_path.read_bytes())
                        except (OSError, json.JSONDecodeError):
                            sample = {}
                    else:
                        sample = {}
                    sample.update({
                        'role': role,
                        'repetition': repetition + 1,
                        'status': 'failed',
                        'error': f'{type(error).__name__}: {str(error)[:1024]}',
                    })
                    sample['sample_json'] = str(sample_path)
                    sample.setdefault('raw_receipts', str(sample_path.parent / 'commands.json'))
                    sample_path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                    _atomic_json(sample_path, sample)
                    failure = sample['error']
                else:
                    sample['sample_json'] = str(
                        output / 'samples' / f'r{repetition + 1:02d}-{role}' / 'sample.json')
                samples_by_role[role].append(sample)
                samples_in_execution_order.append(sample)
                report['samples'] = _sample_summaries(samples_in_execution_order)
                _write_run_report(output, report)
                _enforce_output_limit(output)
                if failure is not None:
                    raise RuntimeError(failure)

        if binary_identity(args.baseline_cli) != baseline_identity:
            raise RuntimeError('baseline executable pair changed during measurement')
        if binary_identity(args.candidate_cli) != candidate_identity:
            raise RuntimeError('candidate executable pair changed during measurement')
        if hash_file(git_path, MAX_BINARY_BYTES)['sha256'] != report['git']['sha256']:
            raise RuntimeError('Git executable changed during measurement')
        if _harness_identity() != report['harness_sources']:
            raise RuntimeError('performance harness source changed during measurement')
        if hashlib.sha256(_read_contract(MATRIX_BUDGETS)).hexdigest() != contracts['matrix_sha256']:
            raise RuntimeError('paired budget contract changed during measurement')
        if hashlib.sha256(_read_contract(SCENARIOS)).hexdigest() != contracts['scenario_sha256']:
            raise RuntimeError('scenario contract changed during measurement')
        current_absolute_sha = hashlib.sha256(_read_contract(ABSOLUTE_BUDGETS)).hexdigest()
        if current_absolute_sha != contracts['absolute_sha256']:
            raise RuntimeError('absolute budget contract changed during measurement')
        if hashlib.sha256(_read_contract(args.v41_live_reference,
                                         MAX_REFERENCE_BYTES)).hexdigest() \
                != v41_context['receipt_sha256']:
            raise RuntimeError('v0.8.41 rollout reference changed during measurement')

        report['comparisons'] = {
            action: _compare_action(samples_by_role['baseline'],
                                    samples_by_role['candidate'], action, paired)
            for action in _timed_scenarios(contracts)
        }
        report['absolute_warning_only'] = {
            f'{role}_status_summary': _absolute_status_warning(samples_by_role[role], contracts)
            for role in ('baseline', 'candidate')
        }
        report['status'] = 'complete'
        report['finished_wall_time_ns'] = time.time_ns()
        _write_run_report(output, report)
        _write_markdown_summary(output / 'summary.md', report)
        _enforce_output_limit(output)
        return report, output
    except Exception as error:
        report['status'] = 'failed'
        report['failure'] = f'{type(error).__name__}: {str(error)[:1024]}'
        report['finished_wall_time_ns'] = time.time_ns()
        report['samples'] = _sample_summaries(samples_in_execution_order)
        _write_run_report(output, report)
        try:
            _write_markdown_summary(output / 'summary.md', report)
        except OSError:
            pass
        raise RuntimeError(f"{report['failure']} (receipts retained in {output})") from error


def _write_markdown_summary(path, report):
    lines = [
        '# Broker action matrix (warning only)', '',
        f"Status: `{report['status']}`", '',
        '| Action | Baseline median ms | Candidate median ms | Change ms | Verdict |',
        '|---|---:|---:|---:|---|',
    ]
    for action, result in report['comparisons'].items():
        if result.get('status') != 'compared':
            lines.append(f"| {action} | — | — | — | unavailable: {result.get('reason', '')} |")
            continue
        lines.append(
            f"| {action} | {result['baseline_median_ms']:.1f} | "
            f"{result['candidate_median_ms']:.1f} | {result['change_ms']:.1f} | "
            f"{result['verdict']} |"
        )
    lines.extend([
        '', 'The paired median rule is warning-only. Absolute status uses the referenced '
        'schema-v1 budget, also warning-only.',
        'The v0.8.41 live rollout receipt is recorded as uncontrolled context and is not '
        'used as the paired baseline.',
        ('The local SSH shim serves a synthetic bare remote; this is not an OS-level '
         'network sandbox.'),
        'The unprovable-fetch scenario is explicitly unsupported and creates no synthetic outcome.',
    ])
    for role in ('baseline', 'candidate'):
        absolute = report['absolute_warning_only'].get(f'{role}_status_summary', {})
        if absolute.get('status') in {'warning', 'within_budget'}:
            lines.append(
                f"{role.title()} status median: {absolute['median_ms']:.1f} ms; "
                f"v1 limit: {absolute['limit_ms']} ms; result: {absolute['status']}."
            )
    path.write_text('\n'.join(lines) + '\n', encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--baseline-cli', type=Path, required=True,
                        help='CLI executable from the retained v0.8.41 binary pair')
    parser.add_argument('--candidate-cli', type=Path, required=True,
                        help='candidate CLI executable; its engine sibling is required')
    parser.add_argument('--v41-live-reference', type=Path, required=True,
                        help='retained v0.8.41 rollout receipt used for identity/context only')
    parser.add_argument('--git', type=Path, required=True,
                        help='absolute path to the actual Git executable (not a cto_bin shim)')
    parser.add_argument('--output', type=Path, required=True,
                        help='new absolute output directory outside the source checkout')
    parser.add_argument('--repetitions', type=int,
                        help='paired repetitions (default: the versioned contract value)')
    args = parser.parse_args()
    try:
        _, output = _run_matrix(args)
    except Exception as error:
        print(f'broker action matrix failed: {error}', file=sys.stderr)
        return 1
    print(f"Broker action matrix report: {output / 'report.json'}")
    print(f"Summary: {output / 'summary.md'}")
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
