#!/bin/sh
set -eu

usage() {
  cat <<'USAGE'
Usage:
  publish-homebrew-tap.sh --formula <path> --tag <vX.Y.Z> \
    --release-repo <owner/name> --tap-repo <owner/name> \
    --branch <default-branch> --expected-file-sha <40-hex> \
    [--session <broker-session>] [--dry-run]

The expected SHA is the current Git blob SHA for Formula/aethyme.rb in the
tap. --dry-run validates credentials, release formula, target branch, current
file SHA, and portable encoding without creating a coordinated write.
USAGE
}

fail() {
  printf 'error: %s\n' "$*" >&2
  exit 1
}

FORMULA=
TAG=
RELEASE_REPO=
TAP_REPO=
BRANCH=
EXPECTED_FILE_SHA=
SESSION=
DRY_RUN=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --formula) [ "$#" -ge 2 ] || fail '--formula requires a value'; FORMULA=$2; shift 2 ;;
    --tag) [ "$#" -ge 2 ] || fail '--tag requires a value'; TAG=$2; shift 2 ;;
    --release-repo) [ "$#" -ge 2 ] || fail '--release-repo requires a value'; RELEASE_REPO=$2; shift 2 ;;
    --tap-repo) [ "$#" -ge 2 ] || fail '--tap-repo requires a value'; TAP_REPO=$2; shift 2 ;;
    --branch) [ "$#" -ge 2 ] || fail '--branch requires a value'; BRANCH=$2; shift 2 ;;
    --expected-file-sha) [ "$#" -ge 2 ] || fail '--expected-file-sha requires a value'; EXPECTED_FILE_SHA=$2; shift 2 ;;
    --session) [ "$#" -ge 2 ] || fail '--session requires a value'; SESSION=$2; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) fail "unknown option $1" ;;
  esac
done

[ -n "$FORMULA" ] || fail 'missing --formula'
[ -r "$FORMULA" ] || fail "formula is not readable: $FORMULA"
[ -n "$TAG" ] || fail 'missing --tag'
[ -n "$RELEASE_REPO" ] || fail 'missing --release-repo'
[ -n "$TAP_REPO" ] || fail 'missing --tap-repo'
[ -n "$BRANCH" ] || fail 'missing --branch'
[ -n "$EXPECTED_FILE_SHA" ] || fail 'missing --expected-file-sha'
if [ "$DRY_RUN" != true ] && [ -z "$SESSION" ]; then
  fail 'a write requires --session <broker-session>'
fi
if [ -n "$SESSION" ]; then
  case "$SESSION" in *[!0-9]*|'') fail 'session id must be a positive integer' ;; esac
fi

case "$TAG" in
  v*) VERSION=${TAG#v} ;;
  *) fail "release tag must begin with v: $TAG" ;;
esac
printf '%s\n' "$VERSION" | awk '
  $0 !~ /^[0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*$/ { exit 1 }
' || fail "only stable semantic versions can update Homebrew: $TAG"

for repository in "$RELEASE_REPO" "$TAP_REPO"; do
  printf '%s\n' "$repository" | awk '
    split($0, part, "/") != 2 ||
    part[1] !~ /^[A-Za-z0-9_.-]+$/ ||
    part[2] !~ /^[A-Za-z0-9_.-]+$/ { exit 1 }
  ' || fail "repository must be owner/name: $repository"
done
printf '%s\n' "$BRANCH" | awk '
  $0 !~ /^[A-Za-z0-9_.-]+$/ { exit 1 }
' || fail "default branch name is not supported: $BRANCH"

case "$EXPECTED_FILE_SHA" in
  *[!0-9a-f]*|'') fail 'expected file SHA must be 40 lowercase hexadecimal characters' ;;
esac
[ "${#EXPECTED_FILE_SHA}" -eq 40 ] || fail 'expected file SHA must be 40 lowercase hexadecimal characters'

command -v gh >/dev/null 2>&1 || fail 'GitHub CLI (gh) is required'
command -v openssl >/dev/null 2>&1 || fail 'OpenSSL is required for portable, single-line base64 encoding'
gh auth status --hostname github.com >/dev/null 2>&1 || fail 'GitHub authentication failed; authenticate before publishing'

awk -F '"' -v expected_version="$VERSION" \
  -v url_prefix="https://github.com/$RELEASE_REPO/releases/download/$TAG/" '
  /^[[:space:]]*version[[:space:]]+"/ {
    versions++
    if ($2 != expected_version) invalid = 1
  }
  /^[[:space:]]*url[[:space:]]+"/ {
    urls++
    if (index($2, url_prefix) != 1) invalid = 1
  }
  /^[[:space:]]*sha256[[:space:]]+"/ {
    digests++
    if (length($2) != 64 || $2 ~ /[^0-9a-f]/) invalid = 1
  }
  END {
    if (versions != 1 || urls < 1 || urls != digests || invalid) exit 1
  }
' "$FORMULA" || fail 'formula version, release URLs, or SHA-256 digests do not match the requested release'

candidate_file_sha=$(git hash-object "$FORMULA") || fail 'cannot calculate formula Git blob SHA'
case "$candidate_file_sha" in
  *[!0-9a-f]*|'') fail 'Git returned an invalid formula blob SHA' ;;
esac
[ "${#candidate_file_sha}" -eq 40 ] || fail 'Git returned an invalid formula blob SHA'

tap_default_branch=$(gh api "repos/$TAP_REPO" --jq '.default_branch') || fail "cannot read $TAP_REPO default branch"
[ "$tap_default_branch" = "$BRANCH" ] || fail "target branch $BRANCH is not $TAP_REPO default branch ($tap_default_branch)"
branch_sha=$(gh api "repos/$TAP_REPO/branches/$BRANCH" --jq '.commit.sha') || fail "target branch does not exist: $BRANCH"
case "$branch_sha" in
  *[!0-9a-f]*|'') fail 'GitHub returned an invalid target branch SHA' ;;
esac
[ "${#branch_sha}" -eq 40 ] || fail 'GitHub returned an invalid target branch SHA'

contents_endpoint="repos/$TAP_REPO/contents/Formula/aethyme.rb?ref=$BRANCH"
remote_file_sha=$(gh api "$contents_endpoint" --jq '.sha') || fail 'cannot read Formula/aethyme.rb from the tap'
remote_content=$(gh api "$contents_endpoint" --jq '.content') || fail 'cannot read the current tap formula content'
encoded_formula=$(openssl base64 -A -in "$FORMULA") || fail 'could not encode the formula; no coordinated write was started'
[ -n "$encoded_formula" ] || fail 'formula encoding was empty; no coordinated write was started'

temporary_root=${TMPDIR:-/tmp}
decoded_formula=$(mktemp "$temporary_root/aethyme-homebrew-tap.XXXXXX") || fail 'could not create a temporary read-back file'
trap 'rm -f "$decoded_formula"' 0 HUP INT TERM
if ! printf '%s' "$remote_content" | tr -d '\r\n' | openssl base64 -d -A > "$decoded_formula"; then
  fail 'could not decode the current tap formula; no coordinated write was started'
fi

if cmp -s "$decoded_formula" "$FORMULA"; then
  commit_sha=$(gh api "repos/$TAP_REPO/commits?path=Formula/aethyme.rb&sha=$BRANCH&per_page=1" --jq '.[0].sha') || fail 'formula is current, but its tap commit could not be verified'
  printf 'Homebrew formula already matches %s on %s at commit %s (file SHA %s).\n' \
    "$TAG" "$BRANCH" "$commit_sha" "$remote_file_sha"
  exit 0
fi

[ "$remote_file_sha" = "$EXPECTED_FILE_SHA" ] || fail "stale expected file SHA: expected $EXPECTED_FILE_SHA, current $remote_file_sha"

if [ "$DRY_RUN" = true ]; then
  printf 'Preflight passed: %s -> %s, branch %s at %s, current file SHA %s, candidate file SHA %s. No write performed.\n' \
    "$TAG" "$TAP_REPO" "$BRANCH" "$branch_sha" "$remote_file_sha" "$candidate_file_sha"
  exit 0
fi

command -v "${AETHYME_BIN:-aethyme}" >/dev/null 2>&1 || fail 'Aethyme CLI is required for broker-coordinated publication'
current_branch_sha=$(gh api "repos/$TAP_REPO/branches/$BRANCH" --jq '.commit.sha') || fail 'cannot recheck target branch before publication'
[ "$current_branch_sha" = "$branch_sha" ] || fail "target branch moved during preflight: $branch_sha -> $current_branch_sha"
current_file_sha=$(gh api "$contents_endpoint" --jq '.sha') || fail 'cannot recheck Formula/aethyme.rb before publication'
[ "$current_file_sha" = "$EXPECTED_FILE_SHA" ] || fail "formula changed during preflight: $EXPECTED_FILE_SHA -> $current_file_sha"

commit_message="chore: update Aethyme to $TAG"
write_status=0
if "${AETHYME_BIN:-aethyme}" broker advanced gh \
  --session "$SESSION" \
  --repo "$TAP_REPO" \
  --reason "publish signed Aethyme release $TAG formula through the broker" \
  -- gh api --method PUT "$contents_endpoint" \
    --field "message=$commit_message" \
    --field "content=$encoded_formula" \
    --field "branch=$BRANCH" \
    --field "sha=$EXPECTED_FILE_SHA"; then
  write_status=0
else
  write_status=$?
fi

post_file_sha=$(gh api "$contents_endpoint" --jq '.sha') || fail 'write returned; tap read-back failed, inspect broker operation before retrying'
post_content=$(gh api "$contents_endpoint" --jq '.content') || fail 'write returned; tap content read-back failed, inspect broker operation before retrying'
if ! printf '%s' "$post_content" | tr -d '\r\n' | openssl base64 -d -A > "$decoded_formula"; then
  fail 'write returned; tap content could not be decoded, inspect broker operation before retrying'
fi
if [ "$post_file_sha" != "$candidate_file_sha" ] || ! cmp -s "$decoded_formula" "$FORMULA"; then
  if [ "$write_status" -ne 0 ]; then
    fail "broker write returned status $write_status and tap read-back SHA is $post_file_sha, expected $candidate_file_sha; inspect and reconcile the broker operation before retrying"
  fi
  fail "broker reported a successful write, but tap read-back SHA is $post_file_sha, expected $candidate_file_sha; inspect the tap before retrying"
fi
if [ "$write_status" -ne 0 ]; then
  fail "broker write returned status $write_status although the tap content matches; inspect and reconcile the broker operation before retrying"
fi

commit_sha=$(gh api "repos/$TAP_REPO/commits?path=Formula/aethyme.rb&sha=$BRANCH&per_page=1" --jq '.[0].sha') || fail 'formula was written and verified, but the resulting tap commit could not be read'
case "$commit_sha" in
  *[!0-9a-f]*|'') fail 'GitHub returned an invalid tap commit SHA' ;;
esac
[ "${#commit_sha}" -eq 40 ] || fail 'GitHub returned an invalid tap commit SHA'
printf 'Published and verified Formula/aethyme.rb for %s on %s at commit %s (file SHA %s).\n' \
  "$TAG" "$BRANCH" "$commit_sha" "$post_file_sha"
