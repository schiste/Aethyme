"""Offline controls for the synthetic concurrent-submit diagnostic; no broker calls."""
import hashlib
import json
import os
from pathlib import Path
import signal
import shutil
import sys
import tempfile
import time
import unittest
from types import SimpleNamespace
from unittest import mock

from broker_concurrent_submit import (
    BINARY_BYTES_PER_COMMIT,
    BoundedCommandRunner,
    MAX_HISTORY_COMMITS,
    MAX_STDOUT_BYTES,
    POLICY_TEXT,
    ReceiptLog,
    command_overlap_ns,
    concurrent_pair_claim,
    deterministic_payload,
    make_env,
    output_tree_size,
    redact_captured_output,
    parse_trace2,
    scenario_fixture,
    setup_scenario,
    terminate_owned_group,
    tree_size,
    validate_output_path,
    validate_submit_outcome,
    validate_shared_history,
)


class SyntheticWorkloadTests(unittest.TestCase):
    def test_binary_history_is_fixed_sized_deterministic_and_bounded(self):
        first = deterministic_payload(0)
        self.assertEqual(len(first), BINARY_BYTES_PER_COMMIT)
        self.assertEqual(first, deterministic_payload(0))
        self.assertNotEqual(first, deterministic_payload(1))
        for index in (-1, MAX_HISTORY_COMMITS, True, 0.5):
            with self.subTest(index=index), self.assertRaises(ValueError):
                deterministic_payload(index)
        for length in (0, BINARY_BYTES_PER_COMMIT - 1, BINARY_BYTES_PER_COMMIT + 1, True):
            with self.subTest(length=length), self.assertRaises(ValueError):
                deterministic_payload(0, length)

    def test_positive_runner_overlap_is_required_for_concurrent_claim(self):
        self.assertEqual(command_overlap_ns([
            {'start_monotonic_ns': 10, 'end_monotonic_ns': 30},
            {'start_monotonic_ns': 20, 'end_monotonic_ns': 40},
        ]), 10)
        self.assertEqual(command_overlap_ns([
            {'start_monotonic_ns': 10, 'end_monotonic_ns': 20},
            {'start_monotonic_ns': 20, 'end_monotonic_ns': 40},
        ]), 0)
        self.assertIsNone(command_overlap_ns([{'start_monotonic_ns': 10, 'end_monotonic_ns': 20}]))
        with self.assertRaisesRegex(RuntimeError, 'reversed monotonic bounds'):
            command_overlap_ns([
                {'start_monotonic_ns': 20, 'end_monotonic_ns': 10},
                {'start_monotonic_ns': 5, 'end_monotonic_ns': 30},
            ])

    def test_concurrent_claim_never_proves_broker_side_overlap(self):
        positive = concurrent_pair_claim(True)
        self.assertIn('runner-call windows overlapped', positive)
        self.assertIn('including spawn, output drain, and owned-group cleanup', positive)
        self.assertIn('does not demonstrate overlapping broker-side execution', positive)
        negative = concurrent_pair_claim(False)
        self.assertIn('runner-call window overlap was not observed', negative)
        self.assertIn('comparison is inconclusive', negative)
        self.assertNotIn('broker-side execution', negative)

    def test_tree_walk_errors_fail_closed(self):
        def denied_walk(root, *, followlinks, onerror):
            onerror(PermissionError('fixture traversal denied'))
            yield root, [], []

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for tree_function in (tree_size, output_tree_size):
                with self.subTest(tree_function=tree_function.__name__), mock.patch(
                        'broker_concurrent_submit.os.walk', side_effect=denied_walk):
                    with self.assertRaisesRegex(RuntimeError, 'fixture traversal denied'):
                        tree_function(root)

    def test_seed_content_accepts_merge_linear_and_content_promotions(self):
        seeds = ['a', 'b', 'c', 'd']
        validate_shared_history(seeds, ['merge'], seeds + ['merge'], 'merge', 'tree', 'tree')
        validate_shared_history(seeds, seeds, seeds, 'd', 'tree', 'tree')
        validate_shared_history(seeds, ['content'], ['content'], 'content', 'tree', 'tree')
        for first_parent, reachable, tip, tree in (
                ([], seeds, 'd', 'tree'),
                (['merge'], seeds + ['merge'], 'other', 'tree'),
                (['merge', 'merge'], seeds + ['merge'], 'merge', 'tree'),
                (['merge'], seeds + ['merge', 'merge'], 'merge', 'tree'),
                (['merge'], seeds, 'merge', 'tree'),
                (['content'], ['content'], 'content', 'changed-tree')):
            with self.subTest(first_parent=first_parent, reachable=reachable, tip=tip, tree=tree):
                with self.assertRaisesRegex(RuntimeError, 'bounded seed content'):
                    validate_shared_history(seeds, first_parent, reachable, tip, 'tree', tree)

    def test_policy_is_manual_without_gate_configuration(self):
        self.assertIn('[promote]\nmode = "manual"', POLICY_TEXT)
        self.assertNotIn('[gates]', POLICY_TEXT)
        self.assertNotIn('AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS', POLICY_TEXT)

    def test_output_must_be_new_absolute_and_outside_source_checkout(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source'
            source.mkdir()
            outside = root / 'artifacts'
            result = validate_output_path(outside, source)
            self.assertTrue(result.is_dir())
            with self.assertRaisesRegex(RuntimeError, 'outside the source checkout'):
                validate_output_path(source / 'out', source)
            with self.assertRaisesRegex(RuntimeError, 'new absolute'):
                validate_output_path(result, source)
            with self.assertRaisesRegex(RuntimeError, 'new absolute'):
                validate_output_path(Path('relative'), source)

    @unittest.skipUnless(os.name == 'posix' and shutil.which('git'), 'requires POSIX Git')
    def test_transport_handles_output_paths_with_spaces_and_apostrophes(self):
        # Exercise the real setup through its first successful local push,
        # then stop before starting a broker or any measured command.
        with tempfile.TemporaryDirectory(prefix="aethyme's transport ") as temporary:
            output = Path(temporary).resolve()
            git = str(Path(shutil.which('git')).resolve())
            binary = output / 'unused-binary'
            receipts = ReceiptLog(output)
            with mock.patch('broker_concurrent_submit.start_session',
                            side_effect=RuntimeError('transport-qualified-stop')) as start:
                with self.assertRaisesRegex(RuntimeError, 'transport-qualified-stop'):
                    with setup_scenario('quoted-path', binary, git, output, receipts):
                        self.fail('setup must stop before any broker session starts')
            start.assert_called_once()
            commands = json.loads((output / 'commands.json').read_text())
            push = [row for row in commands if row['label'] == 'quoted-path:push-default']
            self.assertEqual(len(push), 1)
            self.assertEqual(push[0]['exit_code'], 0)
            self.assertFalse(any(path.name.startswith('.fixture-') for path in output.iterdir()))

    def test_child_environment_does_not_inherit_test_trust_escape(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / 'bin' / 'aethyme'
            git = root / 'git' / 'git'
            tools = root / 'fixture-tools'
            remote = root / 'remote.git'
            tools.mkdir()
            with mock.patch.dict(os.environ, {'AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS': '1'}):
                env = make_env(root, tools, binary, git, remote)
            self.assertNotIn('AETHYME_TRUST_NONINTERACTIVE_FOR_TESTS', env)
            self.assertEqual(env['AETHYME_HOST_STATE_DIR'], str(root.resolve() / 'host-state'))
            self.assertEqual(env['AETHYME_HOST_CACHE_DIR'], str(root.resolve() / 'host-cache'))
            self.assertEqual(env['AETHYME_WORKTREE_ROOT'], str(root.resolve() / 'worktrees'))
            self.assertEqual(env['AETHYME_PREPARATION_CACHE_ROOT'], str(root.resolve() / 'preparation'))
            self.assertEqual(env['GIT_CONFIG_NOSYSTEM'], '1')


class ProvenanceValidationTests(unittest.TestCase):
    def setUp(self):
        self.baseline = 'b' * 40
        self.integration = 'i' * 40
        self.session_head = 'c' * 40
        self.plan = {
            'session_id': 17,
            'recorded_baseline': self.baseline,
            'integration_head': self.integration,
            'session_head': self.session_head,
            'safe': True,
            'commits': [{'commit': self.session_head, 'ownership': 'session_owned'}],
        }
        self.payload = {
            'submission_plan': self.plan,
            'promoted': False,
            'no_changes': False,
            'conflicts': [],
            'entry': {'id': 9, 'status': 'verified'},
            'gate_verification': {
                'status': 'no_configuration', 'configured_gates': 0,
                'selected_gates': 0, 'executed_gates': 0, 'cached_gates': 0,
            },
        }

    def test_accepts_only_complete_safe_manual_no_gate_plan(self):
        result = validate_submit_outcome(
            self.payload, 17, self.baseline, self.integration, self.session_head)
        self.assertEqual(result, self.plan)

    def test_refuses_wrong_identity_or_policy_outcome(self):
        mutations = (
            ({**self.plan, 'recorded_baseline': 'd' * 40}, 'plan provenance'),
            ({**self.plan, 'integration_head': 'e' * 40}, 'plan provenance'),
            ({**self.plan, 'safe': False}, 'plan provenance'),
            ({**self.plan, 'commits': []}, 'expected session commit provenance'),
        )
        for plan, message in mutations:
            with self.subTest(plan=plan), self.assertRaisesRegex(RuntimeError, message):
                validate_submit_outcome(
                    {**self.payload, 'submission_plan': plan}, 17, self.baseline,
                    self.integration, self.session_head)
        for payload, message in (
                ({**self.payload, 'promoted': True}, 'clean, unpromoted'),
                ({**self.payload, 'conflicts': ['conflict']}, 'clean, unpromoted'),
                ({**self.payload, 'gate_verification': {
                    **self.payload['gate_verification'], 'status': 'verified'}},
                 'no-gates conflict-only policy'),
                ({**self.payload, 'gate_verification': {
                    **self.payload['gate_verification'], 'executed_gates': 1}},
                 'no-gates conflict-only policy'),
                ({**self.payload, 'gate_verification': {
                    **self.payload['gate_verification'], 'selected_gates': 1}},
                 'no-gates conflict-only policy'),
                ({**self.payload, 'gate_verification': {
                    **self.payload['gate_verification'], 'cached_gates': 1}},
                 'no-gates conflict-only policy'),
        ):
            with self.subTest(payload=payload), self.assertRaisesRegex(RuntimeError, message):
                validate_submit_outcome(
                    payload, 17, self.baseline, self.integration, self.session_head)

    def test_trace2_counts_full_patch_log_and_rejects_bad_input(self):
        with tempfile.TemporaryDirectory() as temporary:
            trace = Path(temporary) / 'trace.jsonl'
            events = [
                {'event': 'start', 'argv': ['/usr/bin/git', 'status', '--short']},
                {'event': 'child_start', 'argv': ['git', 'log', '--stdin', '-p', '--binary']},
            ]
            trace.write_text('\n'.join(json.dumps(event) for event in events) + '\n')
            result = parse_trace2(trace)
            self.assertEqual(result['git_process_start_count'], 2)
            self.assertEqual(result['full_patch_log_start_count'], 1)
            self.assertEqual(result['bytes'], trace.stat().st_size)
            trace.write_text('{broken json}\n')
            with self.assertRaisesRegex(RuntimeError, 'malformed Trace2 JSON'):
                parse_trace2(trace)
            trace.write_text(json.dumps({'event': 'region_enter', 'argv': []}) + '\n')
            with self.assertRaisesRegex(RuntimeError, 'no Git Trace2 process starts'):
                parse_trace2(trace)


@unittest.skipUnless(os.name == 'posix', 'bounded process-group controls require POSIX')
class BoundedProcessTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        root = Path(self.temporary.name)
        self.root = root / 'fixture'
        self.output = root / 'output'
        self.root.mkdir()
        self.output.mkdir()
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        self.remote = self.root / 'remote.git'
        self.git = Path('/usr/bin/git') if Path('/usr/bin/git').is_file() else Path('/bin/git')
        self.binary = Path(sys.executable)
        self.env = make_env(self.root, self.tools, self.binary, self.git, self.remote)
        self.receipts = ReceiptLog(self.output)
        self.runner = BoundedCommandRunner(self.root, self.output, self.env, self.receipts)

    def tearDown(self):
        self.temporary.cleanup()

    def test_captures_complete_bounded_stdout_and_stderr(self):
        row = self.runner.run(
            [sys.executable, '-c', 'import sys; print("out"); print("err", file=sys.stderr)'],
            self.root, 'small-output')
        self.assertEqual(row['stdout'], 'out\n')
        self.assertEqual(row['stderr'], 'err\n')
        self.assertTrue(row['stdout_complete'])
        self.assertEqual(row['exit_code'], 0)

    def test_named_output_redaction_preserves_raw_hashes_and_never_persists_secret(self):
        secret = 'private-' + 'token-12345'
        code = (
            'import os; value="private-" + "token-12345"; '
            'os.write(1, value.encode()); os.write(2, value.encode())'
        )
        row = self.runner.run([sys.executable, '-c', code], self.root,
                              'redacted-output', redact_values=(secret,))
        raw = secret.encode()
        saved = self.receipts.path.read_bytes()
        self.assertNotIn(secret.encode(), saved)
        self.assertEqual(row['stdout'], '[redacted sensitive value]')
        self.assertEqual(row['stderr'], '[redacted sensitive value]')
        self.assertEqual(row['stdout_bytes'], len(raw))
        self.assertEqual(row['stderr_bytes'], len(raw))
        self.assertEqual(row['stdout_sha256'], hashlib.sha256(raw).hexdigest())
        self.assertEqual(row['stderr_sha256'], hashlib.sha256(raw).hexdigest())
        self.assertEqual(row['redacted_value_occurrences'], 2)

    def test_runner_marks_invalid_utf8_from_original_stdout_bytes(self):
        raw = b'\xed\xa0\x80'
        row = self.runner.run(
            [sys.executable, '-c', 'import os; os.write(1, b"\\xed\\xa0\\x80")'],
            self.root, 'invalid-utf8-output')
        self.assertFalse(row['stdout_utf8_valid'])
        self.assertEqual(row['stdout_bytes'], len(raw))
        self.assertEqual(row['stdout_sha256'], hashlib.sha256(raw).hexdigest())
        self.assertEqual(row['stdout'], raw.decode('utf-8', errors='replace'))

    def test_byte_redaction_rejects_invalid_or_missing_token_inputs(self):
        self.assertEqual(redact_captured_output(b'a-secret-b', b'secret', ('secret',)),
                         (b'a-[redacted sensitive value]-b',
                          b'[redacted sensitive value]', 2))
        for values in (None, ('',), (b'secret',)):
            with self.subTest(values=values), self.assertRaises(ValueError):
                redact_captured_output(b'', b'', values)

    def test_output_limit_refuses_and_records_bounded_failure(self):
        code = (f'import sys, time; sys.stdout.write("x" * {MAX_STDOUT_BYTES + 1}); '
                'sys.stdout.flush(); time.sleep(10)')
        with self.assertRaisesRegex(RuntimeError, 'exceeded its'):
            self.runner.run([sys.executable, '-c', code], self.root, 'oversized-output')
        self.assertEqual(len(self.receipts.rows), 1)
        row = self.receipts.rows[0]
        self.assertLessEqual(row['stdout_bytes'], MAX_STDOUT_BYTES)
        self.assertFalse(row['stdout_complete'])
        self.assertIsNotNone(row['exit_code'])

    def test_reaped_leader_only_uses_signal_zero_for_group_observation(self):
        process = SimpleNamespace(pid=321987, returncode=-signal.SIGTERM)
        with mock.patch('broker_concurrent_submit.os.killpg',
                        side_effect=ProcessLookupError) as killpg:
            terminate_owned_group(process)
        killpg.assert_called_once_with(process.pid, 0)

    def test_fixture_is_retained_when_group_absence_cannot_be_proven(self):
        with tempfile.TemporaryDirectory() as temporary:
            with scenario_fixture('unverified-cleanup', Path(temporary)) as (root, state):
                state['runner'] = SimpleNamespace(cleanup_incomplete=True)
            self.assertTrue(root.is_dir())

    def test_shared_incomplete_flag_does_not_skip_this_command_cleanup(self):
        pid_path = self.root / 'long-lived-command.pid'
        code = (
            'import pathlib, os, time; '
            f'pathlib.Path({str(pid_path)!r}).write_text(str(os.getpid())); '
            'time.sleep(30)'
        )

        def mark_other_cleanup_incomplete(timeout):
            deadline = time.monotonic() + 1
            while not pid_path.is_file() and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertTrue(pid_path.is_file())
            self.runner.cleanup_incomplete = True
            time.sleep(min(timeout, 0.01))
            return []

        with mock.patch(
                'broker_concurrent_submit.selectors.DefaultSelector.select',
                side_effect=mark_other_cleanup_incomplete):
            with self.assertRaisesRegex(RuntimeError, 'command exceeded 0.2 seconds'):
                self.runner.run(
                    [sys.executable, '-c', code], self.root,
                    'shared-cleanup-state', timeout=0.2)

        child_pid = int(pid_path.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(child_pid, 0)

    def test_term_ignoring_descendant_is_killed_before_group_anchor_reaped(self):
        pid_path = self.root / 'descendant.pid'
        ready_path = self.root / 'descendant.ready'
        child_code = (
            'import pathlib, signal, time; '
            'signal.signal(signal.SIGTERM, signal.SIG_IGN); '
            f'pathlib.Path({str(ready_path)!r}).write_text("ready"); '
            'time.sleep(30)'
        )
        leader_code = (
            'import pathlib, subprocess, sys, time\n'
            f'child = subprocess.Popen([sys.executable, "-c", {child_code!r}])\n'
            f'pathlib.Path({str(pid_path)!r}).write_text(str(child.pid))\n'
            'deadline = time.monotonic() + 2\n'
            f'ready = pathlib.Path({str(ready_path)!r})\n'
            'while not ready.exists() and time.monotonic() < deadline:\n'
            '    time.sleep(0.01)\n'
            'print("leader exited", flush=True)\n'
        )
        with self.assertRaisesRegex(RuntimeError, 'command exceeded 3.5 seconds'):
            self.runner.run(
                [sys.executable, '-c', leader_code], self.root,
                'term-ignoring-descendant', timeout=3.5)
        self.assertTrue(ready_path.is_file())
        descendant_pid = int(pid_path.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(descendant_pid, 0)
        self.assertFalse(self.runner.cleanup_incomplete)

    def test_output_limit_escalates_ignored_term_before_reaping_leader(self):
        code = (f'import signal, sys, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); '
                f'sys.stdout.write("x" * {MAX_STDOUT_BYTES + 1}); sys.stdout.flush(); '
                'time.sleep(10)')
        with self.assertRaisesRegex(RuntimeError, 'exceeded its'):
            self.runner.run([sys.executable, '-c', code], self.root, 'ignored-term-output')
        self.assertEqual(len(self.receipts.rows), 1)
        row = self.receipts.rows[0]
        self.assertLessEqual(row['stdout_bytes'], MAX_STDOUT_BYTES)
        self.assertFalse(row['stdout_complete'])
        self.assertEqual(row['exit_code'], -signal.SIGKILL)


if __name__ == '__main__':
    unittest.main()
