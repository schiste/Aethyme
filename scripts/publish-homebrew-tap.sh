#!/bin/sh
set -eu

usage() {
  cat <<'USAGE'
Usage:
  publish-homebrew-tap.sh --formula <path> --tag <vX.Y.Z> \
    --release-repo <owner/name> --tap-repo <owner/name> [--dry-run]

Publishes Formula/aethyme.rb to the tap's default branch with one Contents
API write. The write carries the formula's current blob SHA, so GitHub
refuses it if the file changed after it was read, and it is read back
byte-for-byte afterwards. Republishing the same formula is a no-op.

--dry-run runs every check and read without writing. Writes run only inside
GitHub Actions (the Homebrew tap workflow); to retry a publication, re-run
that workflow for the tag instead of writing from a workstation.
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
DRY_RUN=false

while [ "$#" -gt 0 ]; do
  case "$1" in
    --formula) [ "$#" -ge 2 ] || fail '--formula requires a value'; FORMULA=$2; shift 2 ;;
    --tag) [ "$#" -ge 2 ] || fail '--tag requires a value'; TAG=$2; shift 2 ;;
    --release-repo) [ "$#" -ge 2 ] || fail '--release-repo requires a value'; RELEASE_REPO=$2; shift 2 ;;
    --tap-repo) [ "$#" -ge 2 ] || fail '--tap-repo requires a value'; TAP_REPO=$2; shift 2 ;;
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
if [ "$DRY_RUN" != true ] && [ "${GITHUB_ACTIONS:-}" != true ]; then
  fail "writes run only in the Homebrew tap workflow; re-run it for $TAG (workflow_dispatch on homebrew-tap.yml), or pass --dry-run to check"
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

command -v gh >/dev/null 2>&1 || fail 'GitHub CLI (gh) is required'
command -v openssl >/dev/null 2>&1 || fail 'OpenSSL is required for portable, single-line base64 encoding'
gh auth status --hostname github.com >/dev/null 2>&1 || fail 'GitHub authentication failed; set GH_TOKEN'

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
    # Homebrew infers the version from the URLs, which must all name the tag;
    # an explicit version line is optional but must agree.
    if (versions > 1 || urls < 1 || urls != digests || invalid) exit 1
  }
' "$FORMULA" || fail 'formula version, release URLs, or SHA-256 digests do not match the requested release'

is_sha() {
  case "$1" in *[!0-9a-f]*|'') return 1 ;; esac
  [ "${#1}" -eq 40 ]
}

candidate_file_sha=$(git hash-object "$FORMULA") || fail 'cannot calculate formula Git blob SHA'
is_sha "$candidate_file_sha" || fail 'Git returned an invalid formula blob SHA'
encoded_formula=$(openssl base64 -A -in "$FORMULA") || fail 'could not encode the formula; nothing was written'
[ -n "$encoded_formula" ] || fail 'formula encoding was empty; nothing was written'

branch=$(gh api "repos/$TAP_REPO" --jq '.default_branch') || fail "cannot read $TAP_REPO default branch"
printf '%s\n' "$branch" | awk '$0 !~ /^[A-Za-z0-9_.\/-]+$/ { exit 1 }' \
  || fail "default branch name is not supported: $branch"
formula_endpoint="repos/$TAP_REPO/contents/Formula/aethyme.rb"

decoded_formula=$(mktemp "${TMPDIR:-/tmp}/aethyme-homebrew-tap.XXXXXX") || fail 'could not create a temporary read-back file'
trap 'rm -f "$decoded_formula"' 0 HUP INT TERM

# Reads the tap formula into $decoded_formula and its blob SHA into
# $remote_file_sha; returns non-zero when either read fails.
read_tap_formula() {
  remote_file_sha=$(gh api "$formula_endpoint?ref=$branch" --jq '.sha') || return 1
  is_sha "$remote_file_sha" || return 1
  gh api "$formula_endpoint?ref=$branch" --jq '.content' \
    | tr -d '\r\n' | openssl base64 -d -A > "$decoded_formula"
}

tap_matches_candidate() {
  [ "$remote_file_sha" = "$candidate_file_sha" ] && cmp -s "$decoded_formula" "$FORMULA"
}

read_tap_formula || fail "cannot read Formula/aethyme.rb from $TAP_REPO@$branch"
if tap_matches_candidate; then
  printf 'Homebrew formula already matches %s on %s@%s (file SHA %s); nothing to write.\n' \
    "$TAG" "$TAP_REPO" "$branch" "$remote_file_sha"
  exit 0
fi

if [ "$DRY_RUN" = true ]; then
  printf 'Preflight passed: %s -> %s@%s, current file SHA %s, candidate file SHA %s. No write performed.\n' \
    "$TAG" "$TAP_REPO" "$branch" "$remote_file_sha" "$candidate_file_sha"
  exit 0
fi

# --raw-field, not --field: --field would turn an all-digit SHA into a number.
write_status=0
gh api "$formula_endpoint" --method PUT \
  --raw-field "message=chore: update Aethyme to $TAG" \
  --raw-field "content=$encoded_formula" \
  --raw-field "branch=$branch" \
  --raw-field "sha=$remote_file_sha" >/dev/null || write_status=$?

# A response can be lost after the write landed, and a successful response is
# not proof of the published bytes, so the read-back decides either way.
attempt=1
until read_tap_formula && tap_matches_candidate; do
  [ "$attempt" -lt 3 ] || {
    if [ "$write_status" -ne 0 ]; then
      fail "write failed (status $write_status) and the tap does not carry the $TAG formula (file SHA ${remote_file_sha:-unreadable}); re-run the workflow, which refuses a formula that changed since it was read"
    fi
    fail "write reported success, but the tap read-back (file SHA ${remote_file_sha:-unreadable}) is not the $TAG formula ($candidate_file_sha); inspect $TAP_REPO before retrying"
  }
  attempt=$((attempt + 1))
  sleep "${HOMEBREW_TAP_READBACK_DELAY:-5}"
done
[ "$write_status" -eq 0 ] \
  || printf 'warning: the write returned status %s, but the tap read-back matches %s.\n' "$write_status" "$TAG" >&2

commit_sha=$(gh api "repos/$TAP_REPO/commits?path=Formula/aethyme.rb&sha=$branch&per_page=1" --jq '.[0].sha') \
  || fail 'formula was written and verified, but the resulting tap commit could not be read'
is_sha "$commit_sha" || fail 'GitHub returned an invalid tap commit SHA'
printf 'Published and verified Formula/aethyme.rb for %s on %s@%s at commit %s (file SHA %s).\n' \
  "$TAG" "$TAP_REPO" "$branch" "$commit_sha" "$candidate_file_sha"
