#!/usr/bin/env python3
"""Opt-in reuse measurements on an operator-staged, synthetic Playground.

Planning is read-only. Execution uses the shared bounded process supervisor,
never grants gate trust, and retains the fixture and all failed receipts.
"""
import argparse
import json
import os
from pathlib import Path
import resource
import shutil
import stat
import time
import tempfile

from broker_concurrent_submit import (
    BoundedCommandRunner, ReceiptLog, canonical_json_bytes, create_shims,
    hash_file, make_env, validate_output_path,
)
from broker_latency import fixture_path

PROFILES = {'rust-heavy', 'python-service', 'node-service'}
PHASES = ('fresh_initial', 'fresh_warm', 'continuation', 'reuse')
MAX_FILES = 500_000
MAX_BYTES = 16 * 1024**3
MAX_BINARY_BYTES = 256 * 1024**2
ORIGIN = 'git@github.com:playground/reuse.git'


def storage(root):
    """Inventory names without following links; count physical blocks once/inode."""
    root = Path(root)
    seen = set()
    result = {'files': 0, 'directories': 0, 'symlinks': 0,
              'unique_file_inodes': 0, 'logical_bytes': 0, 'allocated_bytes': 0}
    if not root.exists():
        return result

    def failed(error):
        raise RuntimeError(f'storage inventory incomplete: {error}') from error

    for directory, names, files in os.walk(root, followlinks=False, onerror=failed):
        result['directories'] += 1
        for name in [*names, *files]:
            item = Path(directory) / name
            info = item.lstat()
            if stat.S_ISLNK(info.st_mode):
                result['symlinks'] += 1
            elif stat.S_ISREG(info.st_mode):
                result['files'] += 1
                result['logical_bytes'] += info.st_size
                key = (info.st_dev, info.st_ino)
                if key not in seen:
                    seen.add(key)
                    result['unique_file_inodes'] += 1
                    result['allocated_bytes'] += info.st_blocks * 512
            elif not stat.S_ISDIR(info.st_mode):
                raise RuntimeError(f'unsupported storage object: {item}')
            if (result['files'] + result['directories'] + result['symlinks'] > MAX_FILES
                    or result['logical_bytes'] > MAX_BYTES):
                raise RuntimeError('Playground storage inventory exceeded its bound')
    return result


def load_fixture(root, source_root):
    root = Path(root)
    if not root.is_absolute() or root.is_symlink():
        raise ValueError('fixture root must be an absolute non-symlink directory')
    root = root.resolve(strict=True)
    source_root = source_root.resolve(strict=True)
    if root.is_relative_to(source_root) or source_root.is_relative_to(root):
        raise ValueError('reuse measurements require a separate synthetic Playground')
    marker = root / 'PLAYGROUND.json'
    if marker.is_symlink() or marker.stat().st_size > 64 * 1024:
        raise ValueError('invalid Playground declaration')
    spec = json.loads(marker.read_text())
    if (not isinstance(spec, dict) or spec.get('schema_version') != 1
            or spec.get('synthetic') is not True or spec.get('profile') not in PROFILES
            or not isinstance(spec.get('workload_description'), str)
            or len(spec['workload_description'].strip()) < 40):
        raise ValueError('declare the synthetic workload and its representative limits')
    import re
    if not re.fullmatch('[0-9a-f]{40}', spec.get('baseline', '')):
        raise ValueError('fixture requires a full baseline commit')
    if not re.fullmatch('[A-Za-z0-9][A-Za-z0-9_.-]{0,63}', spec.get('gate', '')):
        raise ValueError('fixture requires one explicitly selected gate')
    locks = spec.get('lockfiles')
    if not isinstance(locks, dict) or not locks:
        raise ValueError('fixture must pin its dependency lockfiles')
    repo = fixture_path(root / 'repo', root, 'Playground repository', must_exist=True)
    for relative, expected in locks.items():
        if (not isinstance(relative, str) or Path(relative).is_absolute()
                or '..' in Path(relative).parts
                or not isinstance(expected, str) or not re.fullmatch('[0-9a-f]{64}', expected)):
            raise ValueError('invalid lockfile pin')
        path = fixture_path(repo / relative, repo, 'dependency lockfile', must_exist=True)
        if hash_file(path, 16 * 1024**2)['sha256'] != expected:
            raise ValueError('dependency lockfile does not match its pin')
    for name in ('home', 'tmp', 'remote.git'):
        fixture_path(root / name, root, name, must_exist=True)
    for name in ('bin', 'host-state', 'host-cache', 'worktrees', 'preparation'):
        fixture_path(root / name, root, name)
    return root, spec


def session_payload(payload, root, expected=None):
    # Both SessionStartReport and AdoptReport flatten the session fields.
    session = payload if isinstance(payload, dict) else {}
    identity = session.get('id')
    raw = session.get('worktree_path')
    if isinstance(identity, bool) or not isinstance(identity, int) or identity <= 0 or not isinstance(raw, str):
        raise RuntimeError('start omitted a valid session identity')
    worktree = fixture_path(Path(raw), root / 'worktrees', 'session worktree', must_exist=True)
    if expected is not None and (identity, str(worktree)) != expected:
        raise RuntimeError('reuse changed the session or mutable worktree identity')
    return identity, str(worktree)


def validate_preparation(payload, session):
    if (not isinstance(payload, dict) or payload.get('session_id') != session
            or payload.get('state') != 'current' or not payload.get('digest')
            or not isinstance(payload.get('steps'), list)):
        raise RuntimeError('preparation did not reach the declared current state')
    for step in payload['steps']:
        if not isinstance(step, dict) or step.get('succeeded') is not True or step.get('exit_code') != 0:
            raise RuntimeError('preparation contains an unsuccessful step')
    return {'digest': payload['digest'], 'executed_steps': len(payload['steps']),
            'shared_cache_coordinated': payload.get('shared_cache_coordinated')}


def validate_gate(payload, gate):
    if (not isinstance(payload, list) or len(payload) != 1
            or not isinstance(payload[0], dict) or payload[0].get('gate') != gate
            or payload[0].get('status') != 'pass' or type(payload[0].get('cached')) is not bool
            or not payload[0].get('tree_hash') or not payload[0].get('definition_hash')):
        raise RuntimeError('gate did not return exactly the selected passing outcome')
    row = payload[0]
    return {key: row.get(key) for key in (
        'gate', 'cached', 'tree_hash', 'definition_hash', 'cache_provenance',
        'managed_cache', 'run_id', 'duration_ms', 'wait_duration_ms')}


def matrix(command, binary, root, spec, verify_checkout):
    """Fixed comparable paths; command is injectable for offline protocol controls."""
    repo = root / 'repo'
    sessions = []
    rows = []
    for phase in PHASES:
        before = storage(root)
        start = time.perf_counter_ns()
        if phase.startswith('fresh_'):
            response = command([binary, 'broker', 'start', '--task', f'reuse Playground {phase}',
                                '--short-name', phase, '--agent', 'Reuse Benchmark', '--json'],
                               repo, f'{phase}:start')
            session = session_payload(response, root)
            if session in sessions or any(item[1] == session[1] for item in sessions):
                raise RuntimeError('fresh sessions shared a mutable worktree')
            sessions.append(session)
        else:
            session = sessions[-1]
            if phase == 'reuse':
                response = command([binary, 'broker', 'start', '--reuse', '--task',
                                    'reuse Playground resumed', '--agent', 'Reuse Benchmark', '--json'],
                                   Path(session[1]), f'{phase}:start')
                session_payload(response, root, expected=session)
            else:
                command([binary, 'broker', 'sync', '--session', str(session[0]), '--json'],
                        Path(session[1]), f'{phase}:sync')
        checkout_ms = (time.perf_counter_ns() - start) / 1_000_000
        cwd = Path(session[1])
        verify_checkout(cwd, phase + ':before')
        preparation = command([binary, 'broker', 'submit', 'prepare', '--session',
                               str(session[0]), '--json'], cwd, f'{phase}:prepare')
        preparation = validate_preparation(preparation, session[0])
        gate = command([binary, 'broker', 'advanced', 'gates', 'run', '--session',
                        str(session[0]), '--only', spec['gate'], '--json'], cwd, f'{phase}:gate')
        gate = validate_gate(gate, spec['gate'])
        if gate['tree_hash'] != spec['tree']:
            raise RuntimeError('gate evidence is for a different source tree')
        verify_checkout(cwd, phase + ':after')
        after = storage(root)
        rows.append({'phase': phase, 'session_id': session[0], 'worktree': session[1],
                     'start_or_sync_wall_ms': checkout_ms, 'preparation': preparation,
                     'gate': gate, 'storage_before': before, 'storage_after': after,
                     'storage_delta': {key: after[key] - before[key] for key in after}})
    return rows


def run(binary, root, output, timeout):
    source = Path(__file__).resolve().parents[4]
    root, spec = load_fixture(root, source)
    helpers = {name: hash_file(Path(__file__).with_name(name), 2 * 1024**2)
               for name in ('broker_reuse.py', 'broker_concurrent_submit.py', 'broker_latency.py')}
    output = Path(output)
    if output.resolve().is_relative_to(root) or root.is_relative_to(output.resolve()):
        raise ValueError('receipts and measured fixture storage must be separate')
    output = validate_output_path(output, source)
    binary = Path(binary).resolve(strict=True)
    identity = {'path': str(binary), **hash_file(binary, MAX_BINARY_BYTES)}
    engine = binary.with_name('aethyme-engine')
    identity['engine'] = ({'path': str(engine.resolve(strict=True)),
                           **hash_file(engine, MAX_BINARY_BYTES)} if engine.is_file() else None)
    git = shutil.which('git')
    if not git:
        raise RuntimeError('Git is required')
    # The root is explicitly staged and trusted by its operator. No inherited
    # credentials, global configuration or live GitHub transport is used.
    env = make_env(root, root / 'bin', binary, git, root / 'remote.git')
    with tempfile.TemporaryDirectory(prefix='reuse-shim-check-') as temporary:
        expected = Path(temporary) / 'bin'
        create_shims(expected)
        for name in ('ssh', 'gh'):
            shim = root / 'bin' / name
            if (shim.is_symlink() or not shim.is_file()
                    or shim.read_bytes() != (expected / name).read_bytes()):
                raise RuntimeError('stage the exact reviewed local transport shims before execution')
    # Toolchain binaries may be shared; package stores and mutable outputs may not.
    toolchain = spec.get('toolchain', {})
    if not isinstance(toolchain, dict) or set(toolchain) - {'bin', 'rustup_home'}:
        raise ValueError('unsupported toolchain declaration')
    for key, value in toolchain.items():
        if not isinstance(value, str) or not Path(value).is_absolute() or not Path(value).is_dir():
            raise ValueError('toolchain paths must name explicit existing absolute directories')
        if key == 'bin':
            env['PATH'] += os.pathsep + value
        else:
            env['RUSTUP_HOME'] = value
    env['CARGO_HOME'] = str(root / 'home' / 'cargo')
    env['npm_config_cache'] = str(root / 'home' / 'npm')
    env['PIP_CACHE_DIR'] = str(root / 'home' / 'pip')
    receipts = ReceiptLog(output)
    runner = BoundedCommandRunner(root, output, env, receipts)

    def raw(argv, cwd, label):
        before = resource.getrusage(resource.RUSAGE_CHILDREN)
        if shutil.disk_usage(root).free < 8 * 1024**3:
            raise RuntimeError('retain at least 8 GiB of fixture filesystem headroom')
        try:
            return runner.run(argv, cwd, label, timeout=timeout)
        finally:
            after = resource.getrusage(resource.RUSAGE_CHILDREN)
            cpu = {'label': label, 'child_user_seconds': after.ru_utime - before.ru_utime,
                   'child_system_seconds': after.ru_stime - before.ru_stime,
                   'basis': 'sequential harness RUSAGE_CHILDREN delta; not whole-host CPU'}
            with (output / 'cpu.jsonl').open('a') as stream:
                stream.write(json.dumps(cpu) + '\n')

    repo = root / 'repo'
    baseline = raw([git, 'rev-parse', 'HEAD'], repo, 'baseline')['stdout'].strip()
    spec = {**spec, 'tree': raw([git, 'rev-parse', 'HEAD^{tree}'], repo, 'baseline-tree')['stdout'].strip()}
    common = raw([git, 'rev-parse', '--path-format=absolute', '--git-common-dir'], repo, 'common-dir')['stdout'].strip()
    fixture_path(Path(common), root, 'Git common directory', must_exist=True)
    if baseline != spec['baseline']:
        raise RuntimeError('Playground baseline changed')
    origin = raw([git, 'remote', 'get-url', 'origin'], repo, 'origin')['stdout'].strip()
    if origin != ORIGIN:
        raise RuntimeError('Playground requires the fixed local-shim origin')
    configuration = raw([git, 'config', '--local', '--null', '--list'], repo, 'local-config')['stdout']
    import shlex
    for entry in configuration.split('\0'):
        if not entry:
            continue
        key, _, value = entry.partition('\n')
        if (key.startswith(('include.', 'includeif.', 'url.', 'http.', 'https.', 'credential.'))
                or key in {'core.gitproxy', 'core.fsmonitor'}):
            raise RuntimeError('Playground has unsupported transport or execution configuration')
        if key == 'core.sshcommand' and value != shlex.quote(str(root / 'bin' / 'ssh')):
            raise RuntimeError('Playground SSH command must use its exact reviewed shim')
    dirty = raw([git, '-c', 'core.fsmonitor=false', 'status', '--porcelain=v1', '--untracked-files=normal'], repo, 'clean-tree')['stdout']
    if dirty:
        raise RuntimeError('Playground must start with a clean source checkout')
    for name in ('worktrees', 'preparation', 'host-cache'):
        path = root / name
        if path.exists() and any(path.iterdir()):
            raise RuntimeError('initial measured managed stores must be empty; use a new fixture')
    # An exclusive intent is retained on failures. Never retry a partially run
    # fixture as though it were a new cold sample.
    with (root / 'REUSE-ATTEMPT.json').open('x') as stream:
        stream.write(json.dumps({'binary': identity, 'fixture': spec, 'output': str(output)}))
    raw([str(binary), '--version'], repo, 'binary-version')

    def command(argv, cwd, label):
        return json.loads(raw(argv, cwd, label)['stdout'])

    def verify_checkout(cwd, label):
        head = raw([git, 'rev-parse', 'HEAD'], cwd, label + ':head')['stdout'].strip()
        dirty = raw([git, '-c', 'core.fsmonitor=false', 'status', '--porcelain=v1',
                     '--untracked-files=no'], cwd, label + ':tracked-status')['stdout']
        if head != baseline or dirty:
            raise RuntimeError('measured checkout source changed')
        for relative, expected in spec['lockfiles'].items():
            path = fixture_path(cwd / relative, cwd, 'measured dependency lockfile', must_exist=True)
            if hash_file(path, 16 * 1024**2)['sha256'] != expected:
                raise RuntimeError('measured dependency lockfile changed')

    result = {'schema_version': 1, 'enforcement': 'diagnostic_only', 'binary': identity,
              'fixture': spec, 'harness_helpers': helpers,
              'rows': matrix(command, str(binary), root, spec, verify_checkout),
              'limits': ['managed-store initial/warm order; OS page caches are uncontrolled',
                         'profile representativeness is an operator declaration, not automatically proven',
                         'no stale/conflicting-session scenario measured by this matrix',
                         'fixtures retained; do not share mutable dependency trees between worktrees',
                         'command wall intervals include spawn, output drain and owned-group settlement',
                         'the transport shims are not an OS network sandbox']}
    for name, expected in helpers.items():
        if hash_file(Path(__file__).with_name(name), 2 * 1024**2) != expected:
            raise RuntimeError('measurement helper source changed during execution')
    if identity['engine'] is not None and hash_file(engine, MAX_BINARY_BYTES)['sha256'] != identity['engine']['sha256']:
        raise RuntimeError('engine binary changed during measurement')
    if hash_file(binary, MAX_BINARY_BYTES)['sha256'] != identity['sha256']:
        raise RuntimeError('CLI binary changed during measurement')
    (output / 'report.json').write_bytes(canonical_json_bytes(result))
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--fixture-root', type=Path, required=True)
    parser.add_argument('--output', type=Path)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--run', action='store_true', help='explicitly execute preparation and gates')
    mode.add_argument('--stage-shims', action='store_true', help='create local transport shims only; no child commands')
    parser.add_argument('--timeout-seconds', type=int, default=900)
    args = parser.parse_args()
    if not 1 <= args.timeout_seconds <= 1800:
        parser.error('timeout must be between 1 and 1800 seconds per command')
    if args.run:
        if args.output is None:
            parser.error('--run requires a new --output directory')
        result = run(args.binary, args.fixture_root, args.output, args.timeout_seconds)
        print(json.dumps({'report': str(args.output / 'report.json'), 'phases': len(result['rows'])}))
    else:
        root, spec = load_fixture(args.fixture_root, Path(__file__).resolve().parents[4])
        if args.stage_shims:
            create_shims(root / 'bin')
        git = shutil.which('git')
        if not git:
            raise RuntimeError('Git is required')
        env = make_env(root, root / 'bin', args.binary.resolve(), git, root / 'remote.git')
        print(json.dumps({'trust_environment': env,
                          'human_trust_argv': [str(args.binary), 'broker', 'advanced', 'trust', '--repo', str(root / 'repo')],
                          'execute': False, 'fixture': spec, 'root': str(root), 'phases': PHASES,
                          'requires': 'operator-approved synthetic fixture and human gate trust',
                          'native_commands_started': 0}, indent=2))


if __name__ == '__main__':
    main()
