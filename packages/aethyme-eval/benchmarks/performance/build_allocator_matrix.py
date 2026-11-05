#!/usr/bin/env python3
"""Build pinned default, rusty and mimalloc release executables for comparison."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import time

BINS = ('aethyme', 'aethyme-engine-cli', 'aethyme-graph-index',
        'aethyme-graph-link', 'aethyme-graph-query')


def require_admission(isolated_ci, env):
    if isolated_ci:
        if env.get('GITHUB_ACTIONS') == 'true' and env.get('RUNNER_ENVIRONMENT') == 'github-hosted':
            return 'isolated-github-runner'
        raise ValueError('--isolated-ci requires a GitHub-hosted Actions runner')
    lease = env.get('AETHYME_RESOURCE_LEASE_ID', '')
    generation = env.get('AETHYME_RESOURCE_GENERATION', '')
    if not re.fullmatch(r'[0-9a-f]{16,64}', lease) or not generation.isdecimal():
        raise ValueError('run local builds under broker advanced resources run')
    return dict(lease=lease, generation=generation)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--repo', type=Path, required=True)
    ap.add_argument('--output', type=Path, required=True)
    ap.add_argument('--target-dir', type=Path, required=True)
    ap.add_argument('--isolated-ci', action='store_true')
    args = ap.parse_args()
    admission = require_admission(args.isolated_ci, os.environ)
    repo = args.repo.resolve()
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, CARGO_TARGET_DIR=str(args.target_dir.resolve()),
               CARGO_INCREMENTAL='0', RUSTC_WRAPPER='')
    env.pop('RUSTFLAGS', None)
    report = dict(source_commit=subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=repo, text=True).strip(),
                  rustc=subprocess.check_output(['rustc', '-Vv'], cwd=repo, text=True),
                  platform=platform.platform(), cpu_count=os.cpu_count(), admission=admission,
                  run_url='https://github.com/'+env.get('GITHUB_REPOSITORY', '')+'/actions/runs/'+env.get('GITHUB_RUN_ID', ''),
                  arms={})
    for arm in ('default', 'rusty', 'mimalloc'):
        argv = ['cargo', 'build', '--release', '--locked', '-j1', '--manifest-path',
                str(repo/'packages/aethyme/rust/Cargo.toml'), '-p', 'aethyme-cli',
                '-p', 'aethyme-engine', '-p', 'aethyme-graph-indexer', '--bins']
        if arm != 'default':
            argv += ['--features', 'allocator-'+arm]
        print('building '+arm, flush=True)
        start = time.monotonic()
        expired = False
        with (out/(arm+'-build.log')).open('x') as log:
            child = subprocess.Popen(argv, cwd=repo, env=env, stdout=log,
                                     stderr=subprocess.STDOUT, start_new_session=True)
            try:
                child.wait(timeout=3600)
            except subprocess.TimeoutExpired:
                expired = True
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
        item = dict(argv=argv, exit_code=child.returncode, timeout=expired,
                    seconds=time.monotonic()-start, binaries={})
        report['arms'][arm] = item
        if not child.returncode and not expired:
            dest = out/'bins'/arm
            dest.mkdir(parents=True)
            for name in BINS:
                path = dest/name
                shutil.copy2(args.target_dir/'release'/name, path)
                item['binaries'][name] = dict(path=str(path), sha256=hashlib.sha256(path.read_bytes()).hexdigest(), bytes=path.stat().st_size)
        (out/'build.json').write_text(json.dumps(report, indent=2)+'\n')
        if child.returncode or expired:
            raise SystemExit('build failed; see '+str(out/(arm+'-build.log')))
        print('built '+arm, flush=True)


if __name__ == '__main__':
    main()
