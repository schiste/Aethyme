"""Fast, dependency-free controls for the warning policy; no builds or broker calls."""
import copy
import json
from pathlib import Path
import unittest
import tempfile
from unittest.mock import patch

from broker_latency import (
    assess,
    run_playground,
    parse_doctor_diagnostics,
    parse_status_diagnostics,
    summarize_diagnostic,
    status_diagnostics_enabled,
    validate_aged_released_resource_lease_count,
    validate_session_count,
    validate_resource_inventory,
)


class PlaygroundSetupTests(unittest.TestCase):
    def test_worktree_root_exists_before_first_child(self):
        class StopBeforeDispatch(Exception):
            pass

        def inspect_child(*args, **kwargs):
            worktree_root = Path(kwargs['env']['AETHYME_WORKTREE_ROOT'])
            self.assertTrue(worktree_root.is_dir(),
                            'worktree root must exist before dispatch')
            raise StopBeforeDispatch()

        with tempfile.TemporaryDirectory() as temporary:
            with patch('broker_latency.subprocess.run', side_effect=inspect_child) as child:
                with self.assertRaises(StopBeforeDispatch):
                    run_playground(Path('/usr/bin/true'), 1, Path(temporary))
                child.assert_called_once()


class BudgetPolicyTests(unittest.TestCase):
    def setUp(self):
        self.budgets = json.loads(Path(__file__).with_name('broker-budgets.json').read_text())
        self.samples = {name: [spec['limit_ms']] for name, spec in self.budgets['budgets'].items()}

    def test_approved_limits_and_warning_policy(self):
        self.assertEqual(self.budgets['enforcement'], 'warning')
        self.assertEqual({k: v['limit_ms'] for k, v in self.budgets['budgets'].items()},
                         {'routine_status': 500, 'guard_overhead': 500,
                          'unchanged_sync': 1000, 'push': 500})

    def test_exact_limit_is_within_budget(self):
        self.assertTrue(all(v['verdict'] == 'within_budget'
                            for v in assess(self.budgets, self.samples).values()))

    def test_overruns_are_warnings_and_keep_raw_samples(self):
        for name in self.samples:
            self.samples[name][0] += 1
        results = assess(self.budgets, self.samples)
        self.assertTrue(all(v['verdict'] == 'warning' for v in results.values()))
        self.assertEqual(results['push']['samples_ms'], [501])

    def test_median_is_used(self):
        self.samples['push'] = [1, 2, 9000]
        self.assertEqual(assess(self.budgets, self.samples)['push']['median_ms'], 2)

    def test_missing_or_unknown_action_is_invalid(self):
        for name in ('missing', 'unknown'):
            samples = copy.deepcopy(self.samples)
            if name == 'missing':
                del samples['push']
            else:
                samples['unknown'] = [1]
            with self.subTest(name=name), self.assertRaises(ValueError):
                assess(self.budgets, samples)

    def test_invalid_samples_are_not_reported_as_fast(self):
        for values in ([], [-1], [True], [float('nan')], [float('inf')], ['1']):
            with self.subTest(values=values), self.assertRaises(ValueError):
                assess(self.budgets, {**self.samples, 'push': values})

    def test_invalid_limits_are_rejected(self):
        for limit in (0, -1, True, float('inf'), '500'):
            budgets = copy.deepcopy(self.budgets)
            budgets['budgets']['push']['limit_ms'] = limit
            with self.subTest(limit=limit), self.assertRaises(ValueError):
                assess(budgets, self.samples)

    def test_unknown_schema_or_hard_enforcement_is_rejected(self):
        for field, value in (('schema_version', 2), ('enforcement', 'error'),
                             ('statistic', 'minimum'), ('unit', 'seconds')):
            with self.subTest(field=field), self.assertRaises(ValueError):
                assess({**self.budgets, field: value}, self.samples)


class AgedHistoryTests(unittest.TestCase):
    def test_released_lease_count_is_bounded_and_zero_is_default(self):
        for count in (0, 1, 30_000, 50_000):
            with self.subTest(count=count):
                self.assertEqual(validate_aged_released_resource_lease_count(count), count)
        for count in (-1, 50_001, True, 1.5):
            with self.subTest(count=count), self.assertRaises(ValueError):
                validate_aged_released_resource_lease_count(count)

    def test_fixture_resource_inventory_requires_fixture_identity_and_no_tokens(self):
        repo = 'a' * 64
        lease = {
            'lease_id': 'fixture-lease', 'repository': repo,
            'worktree_fingerprint': 'b' * 64, 'generation': 2,
            'state': 'released', 'allocations': [{'kind': 'capacity'}],
        }
        self.assertEqual(validate_resource_inventory([lease], repo), [lease])
        active = {**lease, 'state': 'active'}
        self.assertEqual(validate_resource_inventory([active], repo, allow_released=False),
                         [active])
        for invalid, message in (
                ({**active, 'repository': 'outside-playground'}, 'non-fixture repository'),
                ({**active, 'worktree_fingerprint': '/outside/repository'},
                 'non-fixture worktree fingerprint'),
                ({**active, 'ownership_token': 'secret'}, 'secret-bearing lease'),
                ({**active, 'state': 'released'}, 'released host-resource lease'),
        ):
            with self.subTest(invalid=invalid), self.assertRaisesRegex(RuntimeError, message):
                validate_resource_inventory([invalid], repo, allow_released=False)

    def test_aged_history_samples_are_unbudgeted_diagnostics(self):
        result = summarize_diagnostic([4.0, 2.0, 12.0])
        self.assertEqual(result['samples_ms'], [4.0, 2.0, 12.0])
        self.assertEqual(result['median_ms'], 4.0)
        self.assertIsNone(result['budget_ms'])
        self.assertEqual(result['verdict'], 'diagnostic_only')
        for samples in ([], [-1], [True], [float('nan')]):
            with self.subTest(samples=samples), self.assertRaises(ValueError):
                summarize_diagnostic(samples)


class SessionScaleTests(unittest.TestCase):
    def test_session_count_is_bounded_with_one_as_the_default(self):
        for count in (1, 2, 32, 64):
            with self.subTest(count=count):
                self.assertEqual(validate_session_count(count), count)
        for count in (0, 65, True, 1.5, '2'):
            with self.subTest(count=count), self.assertRaises(ValueError):
                validate_session_count(count)

    def test_status_diagnostics_leave_single_session_default_light(self):
        self.assertFalse(status_diagnostics_enabled(1))
        self.assertTrue(status_diagnostics_enabled(1, requested=True))
        self.assertTrue(status_diagnostics_enabled(2))
        with self.assertRaises(ValueError):
            status_diagnostics_enabled(1, requested=1)

    def test_status_counts_and_open_phase_match_the_requested_fixture(self):
        payload = {
            'summary': {'live_sessions': 3, 'active_sessions': 1,
                        'idle_sessions': 1, 'stale_sessions': 1},
            'phase_timings_ms': {'open': 117, 'command_total': 216},
        }
        counts, open_ms = parse_status_diagnostics(json.dumps(payload), 3)
        self.assertEqual(counts, {'live_sessions': 3, 'active_sessions': 1,
                                  'idle_sessions': 1, 'stale_sessions': 1})
        self.assertEqual(open_ms, 117)

    def test_open_phase_is_optional_for_default_status_measurement(self):
        payload = {'summary': {'live_sessions': 1, 'active_sessions': 1,
                               'idle_sessions': 0, 'stale_sessions': 0}}
        counts, open_ms = parse_status_diagnostics(
            json.dumps(payload), 1, require_open_timing=False)
        self.assertEqual(counts['live_sessions'], 1)
        self.assertIsNone(open_ms)

    def test_status_scale_controls_refuse_missing_or_wrong_counts(self):
        invalid_payloads = (
            {},
            {'summary': {'live_sessions': 2, 'active_sessions': 1,
                         'idle_sessions': 0, 'stale_sessions': 1},
             'phase_timings_ms': {'open': 100}},
            {'summary': {'live_sessions': True, 'active_sessions': 1,
                         'idle_sessions': 0, 'stale_sessions': 0},
             'phase_timings_ms': {'open': 100}},
            {'summary': {'live_sessions': 3, 'active_sessions': 1,
                         'idle_sessions': 1, 'stale_sessions': 1},
             'phase_timings_ms': {}},
            {'summary': {'live_sessions': 3, 'active_sessions': 2,
                         'idle_sessions': 1, 'stale_sessions': 1},
             'phase_timings_ms': {'open': 100}},
        )
        for payload in invalid_payloads:
            with self.subTest(payload=payload), self.assertRaises(RuntimeError):
                parse_status_diagnostics(json.dumps(payload), 3)
        with self.assertRaises(RuntimeError):
            parse_status_diagnostics('not json', 3)

    def test_current_doctor_report_shape_is_accepted_and_malformed_output_refused(self):
        report = {
            'integrity': 'ok', 'missing_worktrees': [], 'orphaned_pidfiles': [],
            'blockers': [], 'phase_timings_ms': {'total': 123}, 'retention': {},
        }
        self.assertEqual(parse_doctor_diagnostics(json.dumps(report)), report)
        for invalid in (
                'not json',
                json.dumps({**report, 'missing_worktrees': None}),
                json.dumps({key: value for key, value in report.items() if key != 'retention'}),
        ):
            with self.subTest(invalid=invalid), self.assertRaises(RuntimeError):
                parse_doctor_diagnostics(invalid)


if __name__ == '__main__':
    unittest.main()
