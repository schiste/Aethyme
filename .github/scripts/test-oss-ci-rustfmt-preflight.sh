#!/usr/bin/env bash
# Structural regression for the early rustfmt preflight in OSS CI.
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/../.." && pwd)
workflow="$repo_root/.github/workflows/oss-ci.yml"
full_path_condition="needs.classify_changes.result != 'success' || (needs.classify_changes.outputs.docs_fast != 'true' && needs.classify_changes.outputs.performance_fast != 'true')"
rust_tests_condition="\${{ !cancelled() && github.event_name == 'pull_request' }}"
examples_condition="\${{ !cancelled() && github.event_name == 'pull_request' && ($full_path_condition) }}"
product_condition="\${{ !cancelled() && (github.event_name != 'pull_request' || $full_path_condition) }}"

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

job_section() {
  local job=$1
  awk -v header="  $job:" '
    $0 == header { active = 1; start = NR }
    active && NR > start && /^  [[:alnum:]_-]+:$/ { exit }
    active { print }
  ' "$workflow"
}

step_section() {
  local section=$1
  local name=$2
  awk -v header="      - name: $name" '
    $0 == header { active = 1; start = NR }
    active && NR > start && /^      - name: / { exit }
    active { print }
  ' <<<"$section"
}

assert_contains() {
  local label=$1 section=$2 needle=$3
  if ! grep -Fq -- "$needle" <<<"$section"; then
    fail "$label is missing $needle"
  fi
}

assert_absent() {
  local label=$1 section=$2 needle=$3
  if grep -Fq -- "$needle" <<<"$section"; then
    fail "$label unexpectedly contains $needle"
  fi
}

line_of() {
  local section=$1 needle=$2
  awk -v needle="$needle" 'index($0, needle) { print NR; exit }' <<<"$section"
}

assert_before() {
  local label=$1 section=$2 first=$3 second=$4 first_line second_line
  first_line=$(line_of "$section" "$first")
  second_line=$(line_of "$section" "$second")
  if [[ -z $first_line || -z $second_line || $first_line -ge $second_line ]]; then
    fail "$label must appear before $second"
  fi
}

assert_preflight() {
  local job=$1 expected_full_condition=${2:-}
  local section step
  section=$(job_section "$job")
  step=$(step_section "$section" 'Check formatting before Rust work')
  [[ -n $step ]] || fail "$job has no formatting preflight"
  assert_contains "$job preflight" "$step" 'cargo fmt --all -- --check'
  assert_absent "$job preflight" "$step" 'continue-on-error:'
  if [[ -n $expected_full_condition ]]; then
    assert_contains "$job preflight" "$step" "$expected_full_condition"
  fi
  printf 'PASS: %s has an early rustfmt check\n' "$job"
}

[[ -f $workflow ]] || fail 'OSS CI workflow is missing'

controls_step=$(step_section "$(job_section classify_changes)" 'Verify classifier controls')
assert_contains 'classifier controls' "$controls_step" 'bash .github/scripts/test-oss-ci-rustfmt-preflight.sh'

rust_tests=$(job_section rust-tests)
assert_contains 'Rust workspace job' "$rust_tests" 'name: Rust workspace tests'
assert_contains 'Rust workspace job' "$rust_tests" 'needs: classify_changes'
assert_contains 'Rust workspace job' "$rust_tests" "if: $rust_tests_condition"
assert_contains 'Rust workspace job' "$rust_tests" 'working-directory: packages/aethyme/rust'
assert_preflight rust-tests "$full_path_condition"
assert_before 'Rust workspace preflight' "$rust_tests" 'name: Check formatting before Rust work' 'name: Cache Rust build'
assert_before 'Rust workspace preflight' "$rust_tests" 'name: Check formatting before Rust work' 'name: Install cargo-nextest'
assert_before 'Rust workspace preflight' "$rust_tests" 'name: Check formatting before Rust work' 'name: Build the product binaries'
assert_before 'Rust workspace preflight' "$rust_tests" 'name: Check formatting before Rust work' 'name: Compile the workspace tests'
assert_before 'Rust workspace preflight' "$rust_tests" 'name: Check formatting before Rust work' 'name: Run workspace tests'

examples=$(job_section rust-examples-doctests)
assert_contains 'examples job' "$examples" 'name: Rust examples and doctests'
assert_contains 'examples job' "$examples" 'needs: classify_changes'
assert_contains 'examples job' "$examples" "if: $examples_condition"
assert_contains 'examples job' "$examples" 'working-directory: packages/aethyme/rust'
assert_preflight rust-examples-doctests
assert_before 'examples preflight' "$examples" 'name: Check formatting before Rust work' 'name: Cache Rust build'
assert_before 'examples preflight' "$examples" 'name: Check formatting before Rust work' 'name: Run example tests'
assert_before 'examples preflight' "$examples" 'name: Check formatting before Rust work' 'name: Run doctests'

product=$(job_section product-path-no-python)
assert_contains 'product job' "$product" 'name: Product path (no Python on PATH)'
assert_contains 'product job' "$product" 'needs: classify_changes'
assert_contains 'product job' "$product" "if: $product_condition"
product_preflight=$(step_section "$product" 'Check formatting before Rust work')
assert_contains 'product preflight working directory' "$product_preflight" 'working-directory: packages/aethyme/rust'
assert_preflight product-path-no-python
assert_before 'product preflight' "$product" 'name: Check formatting before Rust work' 'name: Cache Rust build'
assert_before 'product preflight' "$product" 'name: Check formatting before Rust work' 'name: Install the router and its sibling binaries'

warm=$(job_section warm-rust-cache)
assert_contains 'warm cache job' "$warm" "if: github.event_name == 'push'"
assert_preflight warm-rust-cache
assert_before 'warm cache preflight' "$warm" 'name: Check formatting before Rust work' 'name: Cache Rust build'
assert_before 'warm cache preflight' "$warm" 'name: Check formatting before Rust work' 'name: Install cargo-nextest'
assert_before 'warm cache preflight' "$warm" 'name: Check formatting before Rust work' 'name: Compile the workspace tests'

lint=$(job_section lint)
assert_contains 'lint job' "$lint" 'name: Rust lint (fmt, clippy)'
assert_contains 'lint job' "$lint" 'needs: classify_changes'
assert_contains 'lint job' "$lint" "if: $product_condition"
format_step=$(step_section "$lint" 'Check formatting')
assert_contains 'authoritative lint format step' "$format_step" 'id: format_check'
assert_contains 'authoritative lint format step' "$format_step" 'cargo fmt --all -- --check'
assert_absent 'authoritative lint format step' "$format_step" 'continue-on-error:'
patch_step=$(step_section "$lint" 'Materialize rustfmt patch')
assert_contains 'patch materializer' "$patch_step" "steps.format_check.outcome == 'failure'"
assert_contains 'patch materializer' "$patch_step" 'cargo fmt --all'
assert_contains 'patch materializer' "$patch_step" 'git diff --binary -- .'
assert_absent 'patch materializer' "$patch_step" 'git commit'
assert_absent 'patch materializer' "$patch_step" 'git push'
upload_step=$(step_section "$lint" 'Upload suggested rustfmt patch')
assert_contains 'patch upload' "$upload_step" "steps.rustfmt_patch.outputs.available == 'true'"
assert_contains 'patch upload' "$upload_step" 'actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9'
assert_contains 'patch upload' "$upload_step" 'name: rustfmt-patch-${{ github.run_id }}'
assert_contains 'patch upload' "$upload_step" 'path: ${{ runner.temp }}/rustfmt.patch'
assert_contains 'patch upload' "$upload_step" 'retention-days: 7'
assert_before 'lint format check' "$lint" 'name: Check formatting' 'name: Run clippy'

record_tree=$(job_section record-tested-tree)
assert_contains 'tested-tree job' "$record_tree" 'needs: [classify_changes, rust-tests, rust-examples-doctests]'

printf 'PASS: format failures remain in existing jobs; fast-lane predicates and tested-tree dependencies are retained\n'
