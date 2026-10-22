"""Offline controls for the synthetic concurrent-submit diagnostic; no broker calls."""
import json
import os
from pathlib import Path
import signal
import sys
import tempfile
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
    deterministic_payload,
    make_env,
    parse_trace2,
    terminate_owned_group,
    validate_output_path,
    validate_submit_outcome,
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
