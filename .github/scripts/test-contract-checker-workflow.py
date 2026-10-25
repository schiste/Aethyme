"""Run workflow shell blocks with synthetic Git and a fake compiler, never Cargo."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

WORKFLOW = Path('.github/workflows/cross-process-contract.yml').read_text()


def shell_block(name):
    section = WORKFLOW.split('      - name: ' + name + '\n', 1)[1]
    section = section.split('      - name: ', 1)[0]
    lines = section.split('        run: |\n', 1)[1].splitlines()
    result = []
    for line in lines:
        if line and not line.startswith('          '):
            break
        result.append(line[10:] if line else '')
    return '\n'.join(result) + '\n'


with tempfile.TemporaryDirectory(prefix='contract workflow space ') as temporary:
    root = Path(temporary)
    repo, tools, stage = (root / name for name in ('repo', 'tools', 'stage'))
    for directory in (repo, tools, stage):
        directory.mkdir()
    real_git = shutil.which('git')
    assert real_git, 'Git must be installed'
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env.update(
        PATH=str(tools) + os.pathsep + os.environ['PATH'],
        RUNNER_TEMP=str(stage), GITHUB_WORKSPACE=str(repo),
        GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM='1',
        POLICY_BASE_REF='main', POLICY_DEFAULT_REF='main',
        CONTROL_REAL_GIT=real_git,
        CONTROL_CHECKER_CALLED=str(stage / 'checker-called'),
        CONTROL_CARGO_CALLED=str(stage / 'cargo-called'),
    )

    def git(*args):
        return subprocess.check_output([real_git, *args], cwd=repo, env=env, text=True).strip()

    git('init', '-q', '-b', 'main')
    git('config', 'user.name', 'Workflow Control')
    git('config', 'user.email', 'control@example.invalid')
    checker = repo / 'checker-fixture.py'
    checker.write_text('''#!/usr/bin/python3
import json, os, pathlib, subprocess, sys
pathlib.Path(os.environ['CONTROL_CHECKER_CALLED']).touch()
assert subprocess.check_output([os.environ['CONTROL_REAL_GIT'], 'rev-parse', 'HEAD'], text=True).strip() == os.environ['PR_HEAD_SHA']
assert sys.argv[1:4] == ['broker', 'check-contract', '--base']
assert sys.argv[4] == os.environ['PR_BASE_SHA']
assert '--commit-messages' in sys.argv
body_path = sys.argv[sys.argv.index('--pr-body') + 1]
assert pathlib.Path(body_path).read_text() == os.environ['CONTROL_EXPECTED_BODY'] + '\\n'
print('PINNED_BASE_CHECKER_IN_PR_CHECKOUT')
''')
    checker.chmod(0o700)
    git('add', 'checker-fixture.py')
    git('commit', '-qm', 'base checker')
    base = git('rev-parse', 'HEAD')
    checker.write_text('#!/usr/bin/python3\nraise SystemExit(77)\n')
    git('add', 'checker-fixture.py')
    git('commit', '-qm', 'head checker must never run')
    head = git('rev-parse', 'HEAD')
    git('checkout', '-q', base)

    cargo = tools / 'cargo'
    cargo.write_text('''#!/usr/bin/python3
import json, os, pathlib, shutil, subprocess, sys
pathlib.Path(os.environ['CONTROL_CARGO_CALLED']).touch()
root = pathlib.Path.cwd()
target = root / 'target'
if sys.argv[1] == 'build':
 assert subprocess.check_output([os.environ['CONTROL_REAL_GIT'], 'rev-parse', 'HEAD'], text=True).strip() == os.environ['CHECKER_BASE_SHA']
 (target / 'debug').mkdir(parents=True, exist_ok=True)
 shutil.copy2(root / 'checker-fixture.py', target / 'debug/aethyme')
elif sys.argv[1] == 'metadata':
 print(json.dumps({'target_directory': str(target)}))
else:
 raise SystemExit(98)
''')
    cargo.chmod(0o700)
    digest_tool = tools / 'sha256sum'
    digest_tool.write_text('''#!/usr/bin/python3
import hashlib, pathlib, sys
if sys.argv[1] == '--check':
 for row in pathlib.Path(sys.argv[2]).read_text().splitlines():
  digest, filename = row.split('  ', 1)
  if hashlib.sha256(pathlib.Path(filename).read_bytes()).hexdigest() != digest:
   raise SystemExit(1)
else:
 source = pathlib.Path(sys.argv[1])
 print(hashlib.sha256(source.read_bytes()).hexdigest() + '  ' + str(source))
''')
    digest_tool.chmod(0o700)
    body = 'Contract: literal $(touch should-not-exist) `text`\nSecond line.'
    event = root / 'event.json'
    event.write_text(json.dumps({'pull_request': {'body': body}}))
    env.update(CHECKER_BASE_SHA=base, PR_BASE_SHA=base, PR_HEAD_SHA=head,
               GITHUB_EVENT_PATH=str(event), CONTROL_EXPECTED_BODY=body)
    receipts = []

    def run(label, name, expected=0, overrides=None, checker_called=False, cargo_called=False):
        environment = dict(env, **(overrides or {}))
        for marker in ('CONTROL_CHECKER_CALLED', 'CONTROL_CARGO_CALLED'):
            Path(env[marker]).unlink(missing_ok=True)
        result = subprocess.run(['/bin/bash', '-eo', 'pipefail', '-c', shell_block(name)],
                                cwd=repo, env=environment, text=True, capture_output=True)
        assert result.returncode == expected, (label, result.returncode, result.stdout, result.stderr)
        assert Path(env['CONTROL_CHECKER_CALLED']).exists() == checker_called, label
        assert Path(env['CONTROL_CARGO_CALLED']).exists() == cargo_called, label
        receipts.append(dict(control=label, code=result.returncode,
                             stdout=result.stdout, stderr=result.stderr))
        return result

    run('default branch accepted', 'Verify default-branch policy source')
    run('foreign policy branch refused', 'Verify default-branch policy source', 2,
        {'POLICY_BASE_REF': 'agent/foreign'})
    run('wrong base refused before compile', 'Build and pin the base policy checker', 1,
        {'CHECKER_BASE_SHA': head})
    run('compile and pin exact base', 'Build and pin the base policy checker', cargo_called=True)
    git('checkout', '-q', head)
    run('literal event body captured', 'Capture PR body')
    assert not (repo / 'should-not-exist').exists()
    result = run('base checker runs against exact head and body',
                 'Run cross-process contract check', checker_called=True)
    assert 'PINNED_BASE_CHECKER_IN_PR_CHECKOUT' in result.stdout
    artifact = stage / 'aethyme-contract-checker'
    original = artifact.read_bytes()
    artifact.write_bytes(original + b'\n# tamper\n')
    run('changed executable refused before dispatch', 'Run cross-process contract check', 1)
    artifact.write_bytes(original)
    run('wrong head refused before dispatch', 'Run cross-process contract check', 1,
        {'PR_HEAD_SHA': base})
    run('restored bytes accepted', 'Run cross-process contract check', checker_called=True)
    output = os.environ.get('AETHYME_CONTRACT_WORKFLOW_CONTROL_OUTPUT')
    if output:
        Path(output).write_text(json.dumps(dict(
            scope='Workflow shell blocks; synthetic Git and fake compiler/hash tools; no native build',
            base=base, head=head, controls=receipts), indent=2) + '\n')
    print('9 offline workflow controls passed; hosted actions and Rust cache reuse remain separate qualification')
