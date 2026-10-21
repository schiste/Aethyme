#!/usr/bin/env python3
"""Warning-only CLI budgets on a disposable Playground with local Git transport. No builds."""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import shlex
import shutil
import statistics
import subprocess
import tempfile
import time

HERE = Path(__file__).resolve().parent


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


def run_playground(binary, samples, output):
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

        def command(argv, cwd=repo):
            started = time.perf_counter_ns()
            completed = subprocess.run(argv, cwd=cwd, env=env, capture_output=True,
                                       text=True, timeout=60, check=False)
            elapsed = (time.perf_counter_ns() - started) / 1_000_000
            receipts.append({'argv': argv, 'cwd': str(cwd), 'wall_ms': elapsed,
                             'exit_code': completed.returncode,
                             'stdout': completed.stdout, 'stderr': completed.stderr})
            # Persist failed setup as well as successful samples.
            (output / 'commands.json').write_text(json.dumps(receipts, indent=2) + '\n')
            if completed.returncode:
                raise RuntimeError(f'command failed: {argv!r}: {completed.stdout} {completed.stderr}')
            return completed.stdout, elapsed

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
        started, _ = command([str(binary), 'broker', 'start', '--task', 'latency Playground',
                              '--short-name', 'latency', '--agent',
                              'Latency <latency@example.invalid>', '--json'])
        session = json.loads(started)
        worktree = Path(session['worktree_path']).resolve()
        if not worktree.is_relative_to(root / 'worktrees'):
            raise RuntimeError('broker escaped fixture worktree root')
        sid = str(session['id'])
        # Publish once outside measurement; timed pushes are unchanged, with local transport.
        command([str(binary), 'broker', 'push', '--session', sid], worktree)
        commands = {
            'routine_status': [str(binary), 'broker', 'status', '--summary', '--json'],
            'guard_overhead': [str(binary), 'broker', 'advanced', 'exec', '--session', sid,
                               '--json', '--', '/usr/bin/true'],
            'unchanged_sync': [str(binary), 'broker', 'sync', '--session', sid, '--json'],
            'push': [str(binary), 'broker', 'push', '--session', sid],
        }
        measured = {name: [] for name in commands}
        for _ in range(samples):
            for name, argv in commands.items():
                stdout, elapsed = command(argv, worktree)
                if name == 'routine_status':
                    if not isinstance(json.loads(stdout)['summary'], dict):
                        raise RuntimeError('missing status summary')
                if name == 'guard_overhead' and not json.loads(stdout).get('ok'):
                    raise RuntimeError('guard audit did not pass')
                measured[name].append(elapsed)
        return measured


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--samples', type=int, choices=range(1, 11), default=3)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    budget_bytes = (HERE / 'broker-budgets.json').read_bytes()
    budgets = json.loads(budget_bytes)
    results = assess(budgets, run_playground(binary, args.samples, args.output))
    report = {'schema_version': 1, 'enforcement': 'warning',
              'binary': str(binary), 'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
              'budget_sha256': hashlib.sha256(budget_bytes).hexdigest(),
              'profile': 'parcel_playground_one_session_unchanged_push',
              'transport': 'local_bare_remote_via_ssh_shim',
              'network_duration': 'not_instrumented', 'results': results}
    (args.output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    lines = ['## Broker latency budgets (warning only)', '',
             '| Action | Median ms | Budget ms | Result |', '|---|---:|---:|---|']
    for name, item in results.items():
        lines.append(f"| {name} | {item['median_ms']:.1f} | {item['limit_ms']} | {item['verdict']} |")
        if item['verdict'] == 'warning':
            print(f"::warning title=Broker latency budget::{name}: "
                  f"{item['median_ms']:.1f} ms exceeds {item['limit_ms']} ms")
    lines += ['', 'Local Git transport; complete CLI wall time, including Git/transport work.',
              'No network time is subtracted; no OS network sandbox. One-session results do not prove loaded-repository latency.']
    summary = '\n'.join(lines) + '\n'
    (args.output / 'summary.md').write_text(summary)
    print(summary)
    if os.environ.get('GITHUB_STEP_SUMMARY'):
        with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as stream:
            stream.write(summary)


if __name__ == '__main__':
    main()
