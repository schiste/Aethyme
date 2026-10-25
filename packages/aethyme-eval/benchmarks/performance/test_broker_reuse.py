"""Offline reuse protocol controls. No Cargo, broker, gates or preparation."""
import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from broker_reuse import (PHASES, load_fixture, matrix, session_payload,
                          storage, validate_gate, validate_preparation)


class ReuseProtocolTests(unittest.TestCase):
    def test_storage_deduplicates_blocks_but_counts_each_name(self):
        import os
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            (root / 'a').write_bytes(b'abc')
            os.link(root / 'a', root / 'b')
            (root / 'link').symlink_to('/not-read')
            report = storage(root)
            self.assertEqual(report['files'], 2)
            self.assertEqual(report['logical_bytes'], 6)
            self.assertEqual(report['unique_file_inodes'], 1)
            self.assertEqual(report['allocated_bytes'], (root / 'a').stat().st_blocks * 512)
            self.assertEqual(report['symlinks'], 1)

    def test_walk_error_is_not_empty_storage(self):
        def denied(*args, **kwargs):
            kwargs['onerror'](PermissionError('denied'))
            yield
        with tempfile.TemporaryDirectory() as temporary, mock.patch('broker_reuse.os.walk', denied):
            with self.assertRaisesRegex(RuntimeError, 'incomplete'):
                storage(Path(temporary))

    def test_unexpected_storage_objects_refuse(self):
        import os
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            os.mkfifo(root / 'fifo')
            with self.assertRaisesRegex(RuntimeError, 'unsupported storage'):
                storage(root)

    def test_successful_preparation_is_not_inferred_from_command_exit(self):
        good = {'session_id': 1, 'state': 'current', 'digest': 'abc', 'steps': [],
                'shared_cache_coordinated': False}
        self.assertEqual(validate_preparation(good, 1)['executed_steps'], 0)
        for field, value in [('session_id', 2), ('state', 'not_configured'), ('digest', ''),
                             ('steps', [{'succeeded': False, 'exit_code': 0}])]:
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                validate_preparation({**good, field: value}, 1)

    def test_gate_requires_selected_real_outcome_and_boolean_cache_evidence(self):
        good = [{'gate': 'verify', 'status': 'pass', 'cached': True,
                 'tree_hash': 'tree', 'definition_hash': 'definition'}]
        self.assertTrue(validate_gate(good, 'verify')['cached'])
        for payload in ([], good + good, [{**good[0], 'cached': 1}],
                        [{**good[0], 'status': 'fail'}], [{**good[0], 'gate': 'other'}]):
            with self.subTest(payload=payload), self.assertRaises(RuntimeError):
                validate_gate(payload, 'verify')

    def test_session_refuses_escape_or_reuse_identity_drift(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            worktree = root / 'worktrees' / 'one'
            worktree.mkdir(parents=True)
            payload = {'id': 1, 'worktree_path': str(worktree)}
            self.assertEqual(session_payload(payload, root), (1, str(worktree)))
            self.assertEqual(session_payload({'session': payload}, root), (1, str(worktree)))
            with self.assertRaisesRegex(RuntimeError, 'identity'):
                session_payload(payload, root, expected=(2, str(worktree)))
            with self.assertRaises(RuntimeError):
                session_payload({**payload, 'worktree_path': str(root)}, root)

    def test_full_fixed_matrix_dispatches_distinct_fresh_and_same_reused_checkouts(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            calls = []
            sessions = []
            def command(argv, cwd, label):
                calls.append((argv, cwd, label))
                phase = label.split(':')[0]
                if label.endswith(':start'):
                    if phase != 'reuse':
                        worktree = root / 'worktrees' / phase
                        worktree.mkdir(parents=True)
                        sessions.append({'id': len(sessions) + 1, 'worktree_path': str(worktree)})
                    return copy.deepcopy(sessions[-1])
                identity = sessions[-1]['id']
                if label.endswith(':prepare'):
                    return {'session_id': identity, 'state': 'current', 'digest': 'digest', 'steps': []}
                if label.endswith(':gate'):
                    return [{'gate': 'verify', 'status': 'pass', 'cached': phase != 'fresh_initial',
                             'tree_hash': 'tree', 'definition_hash': 'definition'}]
                return {}
            verified = []
            rows = matrix(command, '/fake/aethyme', root, {'gate': 'verify', 'tree': 'tree'},
                          lambda cwd, label: verified.append((cwd, label)))
            self.assertEqual(len(verified), 8)
            self.assertEqual([row['phase'] for row in rows], list(PHASES))
            self.assertEqual([row['session_id'] for row in rows], [1, 2, 2, 2])
            self.assertEqual(len(calls), 12)
            self.assertIn('--reuse', calls[9][0])
            self.assertIn('sync', calls[6][0])
            self.assertNotEqual(rows[0]['worktree'], rows[1]['worktree'])
            self.assertEqual(rows[1]['worktree'], rows[3]['worktree'])
            self.assertFalse(rows[0]['gate']['cached'])

    def test_fresh_identity_and_gate_tree_mismatch_stop_the_matrix(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            worktree = root / 'worktrees' / 'one'
            worktree.mkdir(parents=True)
            def command(argv, cwd, label):
                if label.endswith(':start'):
                    return {'id': 1, 'worktree_path': str(worktree)}
                if label.endswith(':prepare'):
                    return {'session_id': 1, 'state': 'current', 'digest': 'digest', 'steps': []}
                return [{'gate': 'verify', 'status': 'pass', 'cached': False,
                         'tree_hash': 'tree', 'definition_hash': 'definition'}]
            with self.assertRaisesRegex(RuntimeError, 'shared a mutable worktree'):
                matrix(command, '/fake/aethyme', root, {'gate': 'verify', 'tree': 'tree'},
                       lambda *args: None)
            with self.assertRaisesRegex(RuntimeError, 'different source tree'):
                matrix(command, '/fake/aethyme', root, {'gate': 'verify', 'tree': 'other'},
                       lambda *args: None)

    def test_marker_is_required_and_source_checkout_is_never_a_fixture(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            with self.assertRaisesRegex(ValueError, 'separate synthetic'):
                load_fixture(root, root)
            with self.assertRaises(FileNotFoundError):
                load_fixture(root, Path(__file__).parent)


if __name__ == '__main__':
    unittest.main()
