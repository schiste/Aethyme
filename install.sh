#!/bin/sh
set -eu

repository_url="${AETHYME_RELEASE_BASE_URL:-https://github.com/schiste/Aethyme}"
install_dir="${AETHYME_INSTALL_DIR:-${HOME}/.local/bin}"
requested_version=""
verify_signature=false

usage() {
    printf '%s\n' \
        'Usage: install.sh [--version <version>] [--install-dir <directory>] [--verify-signature]' \
        '' \
        'Without --version, installs the latest stable GitHub release.' \
        '--verify-signature requires Cosign 3 and verifies the signed release manifest.'
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || { printf '%s\n' 'install: --version requires a value' >&2; exit 2; }
            requested_version="${2#v}"
            shift 2
            ;;
        --install-dir)
            [ "$#" -ge 2 ] || { printf '%s\n' 'install: --install-dir requires a value' >&2; exit 2; }
            install_dir="$2"
            shift 2
            ;;
        --verify-signature)
            verify_signature=true
            shift
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            printf 'install: unknown option %s\n' "$1" >&2
            usage >&2
            exit 2
            ;;
    esac
done

case "$requested_version" in
    *[!0-9.]*|.*|*.)
        printf 'install: invalid version %s\n' "$requested_version" >&2
        exit 2
        ;;
esac

case "$(uname -s):$(uname -m)" in
    Darwin:arm64) target="aarch64-apple-darwin" ;;
    Darwin:x86_64) target="x86_64-apple-darwin" ;;
    Linux:x86_64) target="x86_64-unknown-linux-gnu" ;;
    *)
        printf 'install: unsupported platform %s %s\n' "$(uname -s)" "$(uname -m)" >&2
        exit 1
        ;;
esac

temp_root="$(mktemp -d "${TMPDIR:-/tmp}/aethyme-install.XXXXXX")"
trap 'rm -rf "$temp_root"' EXIT HUP INT TERM

download() {
    curl --fail --silent --show-error --location "$1" --output "$2"
}

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{ print $1 }'
    else
        shasum -a 256 "$1" | awk '{ print $1 }'
    fi
}

if [ -n "$requested_version" ]; then
    release_path="releases/download/v${requested_version}"
else
    release_path="releases/latest/download"
fi
manifest="$temp_root/release-manifest.json"
download "$repository_url/$release_path/release-manifest.json" "$manifest"

version="$(sed -n 's/^[[:space:]]*"version": "\([^"]*\)"[,]\{0,1\}$/\1/p' "$manifest")"
channel="$(sed -n 's/^[[:space:]]*"release_channel": "\([^"]*\)"[,]\{0,1\}$/\1/p' "$manifest")"
case "$version" in
    ''|*[!0-9.]*|.*|*.)
        printf 'install: release manifest has an invalid version\n' >&2
        exit 1
        ;;
esac
if [ -n "$requested_version" ] && [ "$version" != "$requested_version" ]; then
    printf 'install: requested %s but manifest describes %s\n' "$requested_version" "$version" >&2
    exit 1
fi
if [ -z "$requested_version" ] && [ "$channel" != "stable" ]; then
    printf 'install: latest release manifest is on channel %s, not stable\n' "$channel" >&2
    exit 1
fi

if [ "$verify_signature" = true ]; then
    command -v cosign >/dev/null 2>&1 || {
        printf 'install: --verify-signature requires cosign on PATH\n' >&2
        exit 1
    }
    bundle="$temp_root/release-manifest.sigstore.json"
    download "$repository_url/$release_path/release-manifest.sigstore.json" "$bundle"
    cosign verify-blob \
        --bundle "$bundle" \
        --certificate-identity "https://github.com/schiste/Aethyme/.github/workflows/release.yml@refs/tags/v${version}" \
        --certificate-oidc-issuer https://token.actions.githubusercontent.com \
        "$manifest" >/dev/null
    [ -f "$0" ] || {
        printf 'install: signature verification requires running a reviewed installer file\n' >&2
        exit 1
    }
    installer_digest="$(awk '
        /"installer"[[:space:]]*:/ { in_installer = 1 }
        in_installer && /"sha256"[[:space:]]*:/ {
            value = $0
            sub(/^.*"sha256"[[:space:]]*:[[:space:]]*"/, "", value)
            sub(/".*$/, "", value)
            print value
            exit
        }
    ' "$manifest")"
    [ "$(sha256_file "$0")" = "$installer_digest" ] || {
        printf 'install: reviewed installer does not match the signed manifest\n' >&2
        exit 1
    }
fi

archive="aethyme-v${version}-${target}.tar.gz"
grep -F "\"archive\": \"${archive}\"" "$manifest" >/dev/null || {
    printf 'install: release manifest does not support %s\n' "$target" >&2
    exit 1
}

exact_release_path="releases/download/v${version}"
archive_path="$temp_root/$archive"
checksum_path="$temp_root/$archive.sha256"
download "$repository_url/$exact_release_path/$archive" "$archive_path"
download "$repository_url/$exact_release_path/$archive.sha256" "$checksum_path"

expected="$(awk -v archive="$archive" '$2 == archive { print $1 }' "$checksum_path")"
case "$expected" in
    ''|*[!0-9a-f]*)
        printf 'install: invalid checksum asset for %s\n' "$archive" >&2
        exit 1
        ;;
esac
[ "${#expected}" -eq 64 ] || {
    printf 'install: invalid checksum asset for %s\n' "$archive" >&2
    exit 1
}
actual="$(sha256_file "$archive_path")"
[ "$actual" = "$expected" ] || {
    printf 'install: SHA-256 mismatch for %s\n' "$archive" >&2
    exit 1
}

members="$(tar -tzf "$archive_path")"
expected_members="$(printf '%s\n%s' aethyme aethyme-engine-cli)"
[ "$members" = "$expected_members" ] || {
    printf 'install: archive contains unexpected paths\n' >&2
    exit 1
}
payload="$temp_root/payload"
mkdir "$payload"
tar -xzf "$archive_path" -C "$payload"

router_version="$("$payload/aethyme" --version | awk '{ print $2 }')"
engine_version="$("$payload/aethyme-engine-cli" --version | awk '{ print $2 }')"
[ "$router_version" = "$version" ] && [ "$engine_version" = "$version" ] || {
    printf 'install: archive binary versions do not match manifest %s\n' "$version" >&2
    exit 1
}

mkdir -p "$install_dir"
router_stage="$install_dir/.aethyme.new.$$"
engine_stage="$install_dir/.aethyme-engine-cli.new.$$"
cp "$payload/aethyme" "$router_stage"
cp "$payload/aethyme-engine-cli" "$engine_stage"
chmod 755 "$router_stage" "$engine_stage"
mv "$engine_stage" "$install_dir/aethyme-engine-cli"
mv "$router_stage" "$install_dir/aethyme"

printf 'Installed Aethyme %s (%s) to %s\n' "$version" "$target" "$install_dir"
case ":${PATH}:" in
    *:"$install_dir":*) ;;
    *) printf 'Add %s to PATH before running aethyme.\n' "$install_dir" ;;
esac
