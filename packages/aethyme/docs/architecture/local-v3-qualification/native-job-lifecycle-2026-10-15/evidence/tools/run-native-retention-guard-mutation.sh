#!/bin/bash
set -euo pipefail

mutant=${1:?mutant name required}
filter=${2:?focused test filter required}
root=$(/opt/homebrew/bin/git -C "$PWD" rev-parse --show-toplevel)
mutator="$root/target/local-resume/mutate-native-retention-guard.py"
case "$mutant" in
  m1-release-without-archive-use|m2-release-without-owner-authorization|m4-release-dispatching-unknown-roots|m6-prepare-ignores-released-marker)
    source="$root/packages/aethyme/rust/crates/aethyme-broker/src/native_gates.rs"
    ;;
  m3-disabled-schema23-old-client-trigger)
    source="$root/packages/aethyme/rust/crates/aethyme-broker/src/collaboration_state.rs"
    ;;
  m5-gc-ignores-retained-root-marker)
    source="$root/packages/aethyme/rust/crates/aethyme-broker/src/collaboration_gc.rs"
    ;;
  *) echo "unknown mutant: $mutant" >&2; exit 64 ;;
esac

sha() { /usr/bin/shasum -a 256 "$1" | /usr/bin/awk '{print $1}'; }
original_sha=$(sha "$source")
backup=$(mktemp "$root/target/local-resume/mutation-backups/${mutant}.XXXXXX")
/bin/cp -p "$source" "$backup"
backup_sha=$(sha "$backup")
if [[ "$backup_sha" != "$original_sha" ]]; then
  echo "backup hash mismatch: source=$original_sha backup=$backup_sha" >&2
  exit 65
fi
preview=$(/usr/bin/python3 "$mutator" preview "$mutant" "$source")
mutant_sha=$(printf '%s\n' "$preview" | /usr/bin/sed -n 's/^MUTANT_SHA256=//p')
if [[ -z "$mutant_sha" ]]; then
  echo "mutator did not provide mutant SHA256" >&2
  printf '%s\n' "$preview"
  exit 66
fi
restore() {
  saved=$?
  trap - EXIT
  current_sha=$(sha "$source")
  if [[ "$current_sha" == "$mutant_sha" ]]; then
    /bin/cp -p "$backup" "$source"
    restored_sha=$(sha "$source")
    echo "RESTORED_SHA256=$restored_sha"
    if [[ "$restored_sha" != "$original_sha" ]]; then
      echo "restore hash mismatch; expected $original_sha" >&2
      exit 97
    fi
  elif [[ "$current_sha" == "$original_sha" ]]; then
    echo "RESTORED_SHA256=$current_sha (source was already original)"
  else
    echo "REFUSED_RESTORE current=$current_sha original=$original_sha mutant=$mutant_sha" >&2
    exit 98
  fi
  /usr/bin/touch "$source"
  touched_sha=$(sha "$source")
  if [[ "$touched_sha" != "$original_sha" ]]; then
    echo "post-touch source hash mismatch: expected $original_sha actual $touched_sha" >&2
    exit 97
  fi
  echo "RESTORED_MTIME_ADVANCED=$( /usr/bin/stat -f '%m' "$source" )"
  echo "BACKUP_PATH=$backup"
  echo "BACKUP_SHA256=$backup_sha"
  exit "$saved"
}
trap restore EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

printf 'MUTATION_RUNNER_SHA256='
sha "$0"
printf 'MUTATOR_SHA256='
sha "$mutator"
printf 'MUTANT=%s\nTEST_FILTER=%s\nSOURCE=%s\n' "$mutant" "$filter" "$source"
printf 'ORIGINAL_SHA256=%s\n' "$original_sha"
printf 'MUTATION_BACKUP=%s\n' "$backup"
/usr/bin/python3 "$mutator" apply "$mutant" "$source"
current_sha=$(sha "$source")
if [[ "$current_sha" != "$mutant_sha" ]]; then
  echo "mutant hash mismatch: expected=$mutant_sha actual=$current_sha" >&2
  exit 67
fi
printf 'DIFF_BEGIN\n'
set +e
/usr/bin/diff -u "$backup" "$source"
diff_status=$?
set -e
if [[ "$diff_status" -ne 1 ]]; then
  echo "expected one source diff, diff status was $diff_status" >&2
  exit 68
fi
printf 'DIFF_END\nMUTANT_SOURCE_SHA256=%s\n' "$current_sha"
cargo_log=$(mktemp "$TMPDIR/native-retention-${mutant}-cargo.XXXXXX")
set +e
cargo test --locked -j 4 -p aethyme-broker --lib "$filter" -- --nocapture --test-threads=1 >"$cargo_log" 2>&1
cargo_status=$?
set -e
printf 'CARGO_LOG_PATH=%s\nCARGO_EXIT=%s\n' "$cargo_log" "$cargo_status"
cat "$cargo_log"
if [[ "$cargo_status" -eq 0 ]]; then
  echo "MUTANT_SURVIVED: focused test unexpectedly passed" >&2
  exit 42
fi
if ! /usr/bin/grep -F 'failures:' "$cargo_log" >/dev/null || ! /usr/bin/grep -F "    native_gates::tests::${filter}" "$cargo_log" >/dev/null; then
  echo "NO_MUTATION_CREDIT: named focused test is absent from the failure summary" >&2
  exit "$cargo_status"
fi
if ! /usr/bin/grep -F 'test result: FAILED' "$cargo_log" >/dev/null; then
  echo "NO_MUTATION_CREDIT: Cargo failure summary did not confirm a failed test" >&2
  exit "$cargo_status"
fi
echo "NAMED_ASSERTION_FAILURE_CONFIRMED=native_gates::tests::$filter"
exit "$cargo_status"
