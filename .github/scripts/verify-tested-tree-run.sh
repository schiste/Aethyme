#!/usr/bin/env bash
# usage: verify-tested-tree-run.sh <run id> <commit sha>
#
# Succeeds only when GitHub itself says that run <run id> passed OSS CI's
# whole workspace suite on the tree of <commit sha>. The `tested-tree-<tree>`
# artifact name is only an index: any run on any branch can upload an artifact
# with that name, so nothing here trusts it. Checked through the API:
#   - the run is `.github/workflows/oss-ci.yml`, a `workflow_dispatch` run in
#     this repository (never a fork), completed with conclusion `success`;
#   - the tree of the run's head commit equals the tree of <commit sha>;
#   - the suite's jobs (build, every shard, the verdict, examples and
#     doctests, and the tree record) each concluded `success`.
# Only workflow_dispatch: a dispatched run checks out its head commit, so that
# commit's tree is the tree it tested. A pull_request run tests GitHub's merge
# ref, whose tree the run's `head_sha` does not name.
# A dispatched run uses the workflow file at its head commit, which a branch
# can edit. The tree check covers that: the workflow and tests that ran are
# byte for byte the ones in <commit>, so faking the suite means merging the
# fake into main.
#
# Exit 0: verified. Exit 1: refused (the reason is printed). Exit 2: usage or
# API error; callers must not read it as either verdict.
set -euo pipefail
: "${GITHUB_REPOSITORY:?}"
run_id="${1:-}"
commit="${2:-}"
[[ "$run_id" =~ ^[0-9]+$ ]] || { echo "invalid run id '$run_id'" >&2; exit 2; }
[[ "$commit" =~ ^[0-9a-f]{40}$ ]] || { echo "invalid commit '$commit'" >&2; exit 2; }
refuse() { echo "run $run_id does not prove the suite passed on $commit: $1" >&2; exit 1; }

run="$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run_id")" || { echo "could not read run $run_id" >&2; exit 2; }
field() { jq -r "$1 // empty" <<< "$run"; }
[ "$(field .path)" = ".github/workflows/oss-ci.yml" ] || refuse "workflow is '$(field .path)', not .github/workflows/oss-ci.yml"
[ "$(field .event)" = "workflow_dispatch" ] || refuse "event is '$(field .event)', not workflow_dispatch"
[ "$(field .status)" = "completed" ] || refuse "status is '$(field .status)'"
[ "$(field .conclusion)" = "success" ] || refuse "conclusion is '$(field .conclusion)'"
[ "$(field .repository.full_name)" = "$GITHUB_REPOSITORY" ] || refuse "run belongs to '$(field .repository.full_name)'"
[ "$(field .head_repository.full_name)" = "$GITHUB_REPOSITORY" ] || refuse "head repository is '$(field .head_repository.full_name)'"
head_sha="$(field .head_sha)"
[[ "$head_sha" =~ ^[0-9a-f]{40}$ ]] || refuse "head commit is '$head_sha'"

tree_of() {
  local out
  out="$(gh api "repos/$GITHUB_REPOSITORY/git/commits/$1")" || return 1
  jq -r '.tree.sha // empty' <<< "$out"
}
run_tree="$(tree_of "$head_sha")" || { echo "could not read commit $head_sha" >&2; exit 2; }
want_tree="$(tree_of "$commit")" || { echo "could not read commit $commit" >&2; exit 2; }
[[ "$want_tree" =~ ^[0-9a-f]{40}$ ]] || { echo "commit $commit has no tree" >&2; exit 2; }
[ "$run_tree" = "$want_tree" ] || refuse "it tested tree '$run_tree', not $want_tree"

jobs="$(gh api "repos/$GITHUB_REPOSITORY/actions/runs/$run_id/jobs?per_page=100")" || { echo "could not read the jobs of run $run_id" >&2; exit 2; }
for name in "Rust workspace tests (build)" "Rust workspace tests" "Rust examples and doctests" "Record the tested tree"; do
  conclusion="$(jq -r --arg n "$name" '[.jobs[] | select(.name == $n) | .conclusion] | if length == 1 then .[0] // "none" else "count \(length)" end' <<< "$jobs")"
  [ "$conclusion" = success ] || refuse "job '$name' is '$conclusion'"
done
shards="$(jq '[.jobs[] | select(.name | startswith("Rust workspace tests (shard "))] | length' <<< "$jobs")"
passed="$(jq '[.jobs[] | select((.name | startswith("Rust workspace tests (shard ")) and .conclusion == "success")] | length' <<< "$jobs")"
[ "$shards" -gt 0 ] && [ "$passed" = "$shards" ] || refuse "$passed of $shards shard jobs passed"

echo "Run $run_id passed the workspace suite on tree $want_tree (commit $commit)."
