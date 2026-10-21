"""Fast, dependency-free controls for the warning policy; no builds or broker calls."""
import copy
import json
from pathlib import Path
import unittest

from broker_latency import (
    assess,
    summarize_diagnostic,
    validate_aged_released_resource_lease_count,
    validate_resource_inventory,
)


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
        for invalid in (
                {**lease, 'repository': 'outside-playground'},
                {**lease, 'worktree_fingerprint': '/outside/repository'},
                {**lease, 'ownership_token': 'secret'},
                {**lease, 'state': 'released'},
        ):
            with self.subTest(invalid=invalid), self.assertRaises(RuntimeError):
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


if __name__ == '__main__':
    unittest.main()
