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
from broker_action_matrix import (
    _baseline_unsupported_fetch_option,
    FETCH_FLAGS,
    _classify_deploy_preview,
    _compare_action,
    _fetch_profile_witness,
    _missing_fetch_witness_semantic,
    _parse_deploy_preview,
    compare_samples,
    load_contracts,
    load_v41_context,
    paired_order,
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
                         {'routine_status': 150, 'guard_overhead': 500,
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


class PairedActionMatrixTests(unittest.TestCase):
    def setUp(self):
        self.contracts = load_contracts()
        self.paired = dict(self.contracts['matrix']['paired_comparison'])

    def test_action_contract_references_v1_budgets_without_changing_them(self):
        self.assertEqual(self.contracts['absolute_budgets']['schema_version'], 1)
        self.assertEqual(
            self.contracts['matrix']['absolute_budget_reference'],
            {'path': 'broker-budgets.json', 'schema_version': 1})
        scenarios = {row['id']: row for row in self.contracts['scenario_manifest']['scenarios']}
        self.assertEqual(scenarios['status_aged_history']['history_rows'],
                         self.contracts['matrix']['runner']['history_rows'])
        self.assertEqual(scenarios['fetch_unprovable']['kind'], 'semantic_control')

    def test_malformed_action_contract_types_fail_with_value_error(self):
        for field, value in (('kind', []), ('absolute_budget', [])):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as temporary:
                directory = Path(temporary)
                matrix_path = directory / 'broker-action-matrix-v2.json'
                scenarios_path = directory / 'broker-action-scenarios-v1.json'
                absolute_path = directory / 'broker-budgets.json'
                matrix_path.write_bytes(self.contracts['matrix_bytes'])
                absolute_path.write_bytes(self.contracts['absolute_bytes'])
                manifest = copy.deepcopy(self.contracts['scenario_manifest'])
                target = next(row for row in manifest['scenarios']
                              if row['id'] == 'status_summary')
                target[field] = value
                scenarios_path.write_text(json.dumps(manifest), encoding='utf-8')
                with self.assertRaises(ValueError):
                    load_contracts(matrix_path, scenarios_path, absolute_path)

    def test_pair_order_alternates_and_rejects_invalid_indices(self):
        self.assertEqual(paired_order(0), ('baseline', 'candidate'))
        self.assertEqual(paired_order(1), ('candidate', 'baseline'))
        self.assertEqual(paired_order(2), ('baseline', 'candidate'))
        for value in (-1, True, 1.5):
            with self.subTest(value=value), self.assertRaises(ValueError):
                paired_order(value)

    def test_comparison_warns_only_above_percent_or_jitter_floor(self):
        values = {'repetitions': 5, 'max_regression_percent': 10,
                  'absolute_jitter_floor_ms': 100}
        at_threshold = compare_samples([1_000] * 5, [1_100] * 5, values)
        above_threshold = compare_samples([1_000] * 5, [1_101] * 5, values)
        zero_baseline = compare_samples([0] * 5, [100] * 5, values)
        self.assertEqual(at_threshold['verdict'], 'within_warning_band')
        self.assertEqual(above_threshold['verdict'], 'warning')
        self.assertEqual(zero_baseline['verdict'], 'within_warning_band')
        self.assertEqual(above_threshold['enforcement'], 'warning')
        self.assertEqual(above_threshold['sample_count'], 5)

    def test_missing_strict_fetch_witness_is_not_a_timing_sample(self):
        baseline = [{'status': 'complete', 'semantic': {
            'fetch_provable': {'status': 'unsupported', 'comparison_eligible': False,
                               'reason': 'baseline has no durable provider witness'}}}]
        candidate = [{'status': 'complete', 'timings_ms': {'fetch_provable': 5.0},
                      'semantic': {'fetch_provable': {'status': 'verified',
                                                     'comparison_eligible': True}}}]
        result = _compare_action(baseline, candidate, 'fetch_provable',
                                 {**self.paired, 'repetitions': 1})
        self.assertEqual(result['status'], 'unavailable')
        self.assertIn('baseline has no durable provider witness', result['reason'])
        self.assertNotIn('baseline_median_ms', result)

    @staticmethod
    def deploy_preview(source='a' * 40, plan_digest='b' * 64, full_bytes=0,
                       full_sha=('e3b0c44298fc1c149afbf4c8996fb924'
                                 '27ae41e4649b934ca495991b7852b855'),
                       migration_body='(no changes)\n', notices=''):
        header = '\n'.join((
            'First-enrollment generated output preview for origin:',
            f'Configured origin/default ref: origin/refs/heads/main @ {source}',
            'Generated outputs grouped by ownership; per-file generator-source mapping is unavailable.',
            f'Plan digest: {plan_digest}',
            f'Raw generated diff: {full_bytes} bytes; SHA-256 (raw) {full_sha}',
            'Aethyme-owned outputs:',
            'Managed-block outputs:',
            'Repository-owned policy/config outputs:',
        ))
        return f'{header}\nMigration diff:\n{migration_body}{notices}'

    def test_generator_inventory_can_be_supported_without_generated_content(self):
        diff = 'diff --git a/a b/a\n' * 20
        shown = 'diff --git a/a b/a\n'
        notice = (
            '[terminal-safe diff preview truncated at a complete line boundary: '
            f'showing {len(shown.encode())} escaped bytes from {len(diff.encode())} raw diff bytes; '
            'raw SHA-256 covers the complete proposal]\n')
        preview = self.deploy_preview(
            full_bytes=len(diff.encode()), full_sha='c' * 64,
            migration_body=shown, notices=notice)
        result = _parse_deploy_preview(preview, 'a' * 40)
        self.assertTrue(result['inventory_supported'])
        self.assertFalse(result['generated_content_available'])
        self.assertTrue(result['source_matches_seed'])
        self.assertTrue(result['truncation_explicit'])

    def test_zero_byte_generated_diff_is_still_available_content(self):
        result = _parse_deploy_preview(self.deploy_preview(), 'a' * 40)
        self.assertTrue(result['inventory_supported'])
        self.assertTrue(result['generated_content_available'])
        self.assertEqual(result['content_identity']['raw_diff_bytes'], 0)

    def test_actual_untruncated_enrollment_diff_format_is_comparable_content(self):
        diff = 'diff --git a/file b/file\n'
        preview = self.deploy_preview(
            plan_digest='b' * 64, full_bytes=len(diff.encode()), full_sha='c' * 64,
            migration_body=diff)
        result = _parse_deploy_preview(preview, 'a' * 40)
        self.assertTrue(result['inventory_supported'])
        self.assertTrue(result['generated_content_available'])
        self.assertEqual(result['content_identity']['raw_diff_sha256'], 'c' * 64)

    def test_word_truncated_in_generated_diff_is_not_a_renderer_notice(self):
        diff = 'diff --git a/file b/file\n+This text was truncated earlier\n'
        result = _parse_deploy_preview(
            self.deploy_preview(full_bytes=len(diff.encode()), full_sha='c' * 64,
                                migration_body=diff), 'a' * 40)
        self.assertFalse(result['truncation_explicit'])
        self.assertTrue(result['generated_content_available'])

    def test_outer_inventory_truncation_keeps_content_unavailable(self):
        notice = '[rendered preview truncated at a complete line boundary; output byte limit reached]\n'
        diff = 'diff --git a/file b/file\n'
        preview = self.deploy_preview(
            full_bytes=len(diff.encode()), full_sha='c' * 64,
            migration_body=diff, notices=notice)
        result = _parse_deploy_preview(preview, 'a' * 40)
        self.assertTrue(result['inventory_supported'])
        self.assertFalse(result['generated_content_available'])
        self.assertTrue(result['truncation_explicit'])

    def test_diff_truncation_notice_must_match_the_raw_diff_header(self):
        notice = (
            '[terminal-safe diff preview truncated at a complete line boundary: '
            'showing 1 escaped bytes from 7 raw diff bytes; '
            'raw SHA-256 covers the complete proposal]\n')
        result = _parse_deploy_preview(
            self.deploy_preview(full_bytes=8, full_sha='c' * 64,
                                migration_body='x\n', notices=notice), 'a' * 40)
        self.assertTrue(result['truncation_explicit'])
        self.assertFalse(result['truncation_evidence_matches'])
        self.assertFalse(result['generated_content_available'])

    def test_empty_diff_requires_the_actual_empty_sha256(self):
        preview = self.deploy_preview(full_sha='c' * 64)
        result = _parse_deploy_preview(preview, 'a' * 40)
        self.assertTrue(result['inventory_supported'])
        self.assertFalse(result['generated_content_available'])
        self.assertIsNone(result['content_identity'])

    def test_deploy_timing_requires_complete_valid_utf8_preview(self):
        text = self.deploy_preview()
        row = {'stdout_complete': True, 'stdout_utf8_valid': False}
        result = _classify_deploy_preview(text, row, 'a' * 40)
        self.assertEqual(result['status'], 'output_incomplete')
        self.assertFalse(result['comparison_eligible'])

    def test_ineligible_timing_and_different_preview_content_are_not_compared(self):
        paired = {**self.paired, 'repetitions': 1}
        baseline_preview = _parse_deploy_preview(
            self.deploy_preview(plan_digest='b' * 64, full_bytes=5,
                                full_sha='a' * 64,
                                migration_body='diff\n'), 'a' * 40)
        candidate_preview = _parse_deploy_preview(
            self.deploy_preview(plan_digest='d' * 64, full_bytes=5,
                                full_sha='a' * 64,
                                migration_body='diff\n'), 'a' * 40)
        baseline = [{'status': 'complete', 'timings_ms': {'deploy_plan_diff': 10.0},
                     'semantic': {'deploy_plan_diff': {
                         'status': 'supported_content_available',
                         'comparison_eligible': False,
                         'comparison_ineligibility_reason': 'invalid stdout evidence',
                         'content_identity': baseline_preview['content_identity'],
                     }}}]
        candidate = [{'status': 'complete', 'timings_ms': {'deploy_plan_diff': 10.0},
                      'semantic': {'deploy_plan_diff': {
                          'status': 'supported_content_available',
                          'comparison_eligible': True,
                          'content_identity': candidate_preview['content_identity'],
                      }}}]
        result = _compare_action(baseline, candidate, 'deploy_plan_diff', paired)
        self.assertEqual(result['status'], 'unavailable')
        self.assertIn('invalid stdout evidence', result['reason'])

        baseline[0]['semantic']['deploy_plan_diff']['comparison_eligible'] = True
        self.assertEqual(
            baseline[0]['semantic']['deploy_plan_diff']['content_identity'],
            candidate[0]['semantic']['deploy_plan_diff']['content_identity'],
            'fixture-specific plan digests should not make identical generated content incomparable')
        result = _compare_action(baseline, candidate, 'deploy_plan_diff', paired)
        self.assertEqual(result['status'], 'compared')

        different_preview = _parse_deploy_preview(
            self.deploy_preview(plan_digest='e' * 64, full_bytes=5,
                                full_sha='b' * 64,
                                migration_body='other\n'), 'a' * 40)
        candidate[0]['semantic']['deploy_plan_diff']['content_identity'] = (
            different_preview['content_identity'])
        result = _compare_action(baseline, candidate, 'deploy_plan_diff', paired)
        self.assertEqual(result['status'], 'unavailable')
        self.assertIn('content contracts differ', result['reason'])

    def test_fetch_comparison_requires_schema_one_provider_completion_witness(self):
        with tempfile.TemporaryDirectory() as temporary:
            common = Path(temporary) / '.git'
            common.mkdir()
            common_stat = common.stat()
            repository = f'local:{Path(temporary).as_posix()}'
            expected_args = ['fetch', *FETCH_FLAGS, '--', 'perf-local',
                             'refs/heads/perf-proven:refs/remotes/perf-local/perf-proven']
            expected = {
                'session_id': 7,
                'repository': repository,
                'remote_path': Path(temporary).as_posix(),
                'remote_name': 'perf-local',
                'source_ref': 'refs/heads/perf-proven',
                'destination_ref': 'refs/remotes/perf-local/perf-proven',
                'common_directory': common.resolve(),
                'common_device': common_stat.st_dev,
                'common_inode': common_stat.st_ino,
                'expected_args': expected_args,
            }
            payload = {
                'resolved_target': {
                    'remote_name': 'perf-local',
                    'normalized_host': 'local',
                    'repository_path': Path(temporary).as_posix(),
                    'coordination_key': repository,
                    'caller_assertion': None,
                    'evidence': {
                        'caller_assertion': 'not_supplied',
                        'identity': 'fetch_and_push_matched',
                        'fetch': {'syntax': 'local_path'},
                        'push': {'syntax': 'local_path'},
                    },
                },
                'operation': {
                    'id': 11,
                    'provider': 'git',
                    'session_id': 7,
                    'status': 'succeeded',
                    'repository': repository,
                    'command_json': json.dumps(['git', *expected_args]),
                    'details_json': json.dumps({'fetch_reconciliation': {
                        'schema_version': 1,
                        'operation_id': 11,
                        'repository': repository,
                        'footprint': {
                            'remote': 'perf-local',
                            'source_ref': 'refs/heads/perf-proven',
                            'destination_ref': 'refs/remotes/perf-local/perf-proven',
                        },
                        'common_directory': str(common.resolve()),
                        'common_device': common_stat.st_dev,
                        'common_inode': common_stat.st_ino,
                        'pre_destination': None,
                        'provider': {
                            'pid': 2000, 'process_group': 2000, 'start_time': 3000,
                            'start_time_clock': 'fixture-clock',
                        },
                        'completion': {
                            'pid': 2000, 'success': True, 'exit_code': 0,
                            'observed_at': 4000,
                        },
                    }}),
                },
            }
            witness = _fetch_profile_witness(payload, expected)
            self.assertEqual(witness['schema_version'], 1)
            self.assertEqual(witness['provider_pid'], 2000)

            valid_completion = copy.deepcopy(payload)
            payload['operation']['details_json'] = '{}'
            self.assertIsNone(_fetch_profile_witness(payload, expected))
            invalid_completion = valid_completion
            invalid_journal = json.loads(invalid_completion['operation']['details_json'])
            invalid_journal['fetch_reconciliation']['completion'].update(
                {'success': False, 'exit_code': 1})
            invalid_completion['operation']['details_json'] = json.dumps(invalid_journal)
            with self.assertRaisesRegex(RuntimeError, 'exact completed profile'):
                _fetch_profile_witness(invalid_completion, expected)

    def test_missing_fetch_witness_is_only_an_unsupported_baseline_control(self):
        baseline = _missing_fetch_witness_semantic('baseline', 11, 12.5)
        self.assertEqual(baseline['status'], 'unsupported')
        self.assertFalse(baseline['comparison_eligible'])
        self.assertEqual(baseline['observed_command_duration_ms'], 12.5)
        with self.assertRaisesRegex(RuntimeError, 'candidate omitted durable'):
            _missing_fetch_witness_semantic('candidate', 11, 12.5)

    def test_baseline_fetch_downgrade_requires_exact_complete_refusal_receipt(self):
        expected_argv = ['/fixture/aethyme', 'broker', 'advanced', 'git', '--session',
                         '7', '--effect', 'write', '--scope', 'fetch', '--reason',
                         'synthetic paired action measurement', '--json', '--', 'fetch',
                         *FETCH_FLAGS, '--', 'perf-local',
                         'refs/heads/perf-proven:refs/remotes/perf-local/perf-proven']
        message = (
            'Error: unsupported fetch option "--no-auto-maintenance" prevents '
            'deterministic remote resolution')
        receipt = {
            'label': 'action:fetch_provable',
            'argv': expected_argv,
            'exit_code': 2,
            'failure': 'command exited with status 2',
            'stdout_complete': False,
            'stdout_utf8_valid': True,
            'stdout': message,
            'stderr': '',
            'stdout_bytes': len(message.encode()),
            'stderr_bytes': 0,
            'stdout_sha256': 'a' * 64,
            'stderr_sha256': 'b' * 64,
            'duration_ms': 12.5,
        }
        self.assertEqual(
            _baseline_unsupported_fetch_option('baseline', receipt, expected_argv),
            '--no-auto-maintenance')
        self.assertIsNone(
            _baseline_unsupported_fetch_option('candidate', receipt, expected_argv))

        for changed in (
                {'failure': 'command exceeded 60 seconds'},
                {'failure': 'command exited with status 2', 'exit_code': 0},
                {'stdout': 'Error: fetch network connection failed'},
                {'argv': expected_argv[:-1]},
                {'stdout_complete': True},
                {'stderr_sha256': None}):
            with self.subTest(changed=changed):
                self.assertIsNone(_baseline_unsupported_fetch_option(
                    'baseline', {**receipt, **changed}, expected_argv))

    def test_v41_live_receipt_is_context_only_and_pins_both_executables(self):
        cli_hash = 'a' * 64
        engine_hash = 'b' * 64
        receipt = {
            'version': '0.8.41',
            'source_commit': 'c' * 40,
            'release': 'https://example.invalid/releases/tag/v0.8.41',
            'hosted_workspace_passed': 1,
            'local_native_builds_or_tests': False,
            'roots': {'install-root': {
                'aethyme': {'sha256': cli_hash},
                'aethyme-engine-cli': {'sha256': engine_hash},
            }},
            'repos': {'mockup': {'seconds': {'start': 2.5}}},
        }
        identity = {'cli_sha256': cli_hash, 'engine_sha256': engine_hash}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'v41.json'
            path.write_text(json.dumps(receipt))
            context = load_v41_context(path, identity)
            self.assertFalse(context['comparison_eligible'])
            self.assertEqual(context['uncontrolled_real_rollout_seconds']['mockup']['start'],
                             2.5)
            with self.assertRaisesRegex(ValueError, 'does not match'):
                load_v41_context(path, {**identity, 'engine_sha256': 'd' * 64})


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
