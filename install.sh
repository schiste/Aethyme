#!/bin/sh
set -eu

repository_url="${AETHYME_RELEASE_BASE_URL:-https://github.com/schiste/Aethyme}"
install_dir="${AETHYME_INSTALL_DIR:-${HOME}/.local/bin}"
requested_version=""
# auto: verify the signed release manifest whenever cosign is on PATH.
# require: fail without cosign, and also check this installer against the
#          signed manifest. off: checksums only.
signature_mode=auto
signature_opt_out=false

usage() {
    printf '%s\n' \
        'Usage: install.sh [--version <version>] [--install-dir <directory>]' \
        '                  [--require-signature | --no-verify-signature]' \
        '' \
        'Without --version, installs the latest stable GitHub release.' \
        'When cosign (Cosign 3) is on PATH, the signed release manifest is verified' \
        'before anything is installed. Without cosign, a note is printed and the' \
        'archive is still checked against the manifest SHA-256.' \
        '--require-signature fails when cosign is missing and also checks this' \
        '  installer file against the signed manifest (--verify-signature is an alias).' \
        '--no-verify-signature skips signature verification.'
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
        --verify-signature|--require-signature)
            signature_mode=require
            shift
            ;;
        --no-verify-signature)
            signature_opt_out=true
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

if [ "$signature_opt_out" = true ]; then
    if [ "$signature_mode" = require ]; then
        printf '%s\n' 'install: --no-verify-signature conflicts with --require-signature' >&2
        exit 2
    fi
    signature_mode=off
fi

command -v jq >/dev/null 2>&1 || {
    printf 'install: jq is required to parse the release manifest safely\n' >&2
    exit 1
}

case "$requested_version" in
    *[!0-9.]*|.*|*.)
        printf 'install: invalid version %s\n' "$requested_version" >&2
        exit 2
        ;;
esac

# Prints gnu or musl. ldd names its C library; the musl loader is the
# fallback because Debian's musl-tools installs it on glibc systems too.
linux_libc() {
    ldd_banner="$(ldd --version 2>&1 || true)"
    case "$ldd_banner" in
        *musl*) printf '%s\n' musl; return ;;
        *GNU*|*glibc*|*GLIBC*) printf '%s\n' gnu; return ;;
    esac
    for loader in /lib/ld-musl-*; do
        [ -e "$loader" ] && { printf '%s\n' musl; return; }
    done
    printf '%s\n' gnu
}

# Prints the release target for this machine, or fails for an unsupported one.
detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os:$arch" in
        Darwin:arm64|Darwin:aarch64) printf '%s\n' aarch64-apple-darwin ;;
        Darwin:x86_64) printf '%s\n' x86_64-apple-darwin ;;
        Linux:x86_64|Linux:amd64)
            case "$(linux_libc)" in
                musl) printf '%s\n' x86_64-unknown-linux-musl ;;
                *) printf '%s\n' x86_64-unknown-linux-gnu ;;
            esac
            ;;
        Linux:aarch64|Linux:arm64)
            case "$(linux_libc)" in
                musl)
                    printf 'install: no musl release for %s; use a glibc distribution or build from source\n' "$arch" >&2
                    return 1
                    ;;
                *) printf '%s\n' aarch64-unknown-linux-gnu ;;
            esac
            ;;
        *)
            printf 'install: unsupported platform %s %s\n' "$os" "$arch" >&2
            return 1
            ;;
    esac
}

target="$(detect_target)" || exit 1

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

version="$(jq -er '.version | select(type == "string")' "$manifest")"
channel="$(jq -er '.release_channel | select(type == "string")' "$manifest")"
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

# Mirrors the "Sign and verify release manifest" step in release.yml: keyless
# Sigstore, bound to this repository's release workflow at the exact tag.
verify_manifest_signature() {
    bundle="$temp_root/release-manifest.sigstore.json"
    download "$repository_url/$release_path/release-manifest.sigstore.json" "$bundle" || {
        printf 'install: could not download the release manifest signature bundle\n' >&2
        exit 1
    }
    cosign verify-blob \
        --bundle "$bundle" \
        --certificate-identity "https://github.com/schiste/Aethyme/.github/workflows/release.yml@refs/tags/v${version}" \
        --certificate-oidc-issuer https://token.actions.githubusercontent.com \
        "$manifest" >/dev/null || {
        printf 'install: release manifest signature verification failed (pass --no-verify-signature to skip)\n' >&2
        exit 1
    }
}

case "$signature_mode" in
    require)
        command -v cosign >/dev/null 2>&1 || {
            printf 'install: --require-signature requires cosign on PATH\n' >&2
            exit 1
        }
        verify_manifest_signature
        [ -f "$0" ] || {
            printf 'install: signature verification requires running a reviewed installer file\n' >&2
            exit 1
        }
        installer_digest="$(jq -er '.installer.sha256 | select(type == "string")' "$manifest")"
        [ "$(sha256_file "$0")" = "$installer_digest" ] || {
            printf 'install: reviewed installer does not match the signed manifest\n' >&2
            exit 1
        }
        ;;
    auto)
        if command -v cosign >/dev/null 2>&1; then
            verify_manifest_signature
            printf 'install: verified the signed release manifest for v%s\n' "$version" >&2
        else
            printf 'install: cosign not found; skipping signature verification (checksums alone do not authenticate a release)\n' >&2
        fi
        ;;
    *)
        printf 'install: signature verification is disabled; checksums alone do not authenticate a release\n' >&2
        ;;
esac

archive="aethyme-v${version}-${target}.tar.gz"
expected="$(jq -er --arg archive "$archive" --arg target "$target" '
    [.artifacts[] | select(.archive == $archive and .target == $target)]
    | if length == 1 then .[0].sha256 else error("artifact must be unique") end
    | select(type == "string")
' "$manifest")" || {
    printf 'install: release manifest has no unique artifact for %s\n' "$target" >&2
    exit 1
}

exact_release_path="releases/download/v${version}"
archive_path="$temp_root/$archive"
download "$repository_url/$exact_release_path/$archive" "$archive_path"

# The manifest is the trust root, including in signature-verification mode.
# Never substitute a separately downloaded, unauthenticated checksum asset.
case "$expected" in
    ''|*[!0-9a-f]*)
        printf 'install: invalid manifest checksum for %s\n' "$archive" >&2
        exit 1
        ;;
esac
[ "${#expected}" -eq 64 ] || {
    printf 'install: invalid manifest checksum for %s\n' "$archive" >&2
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

"$payload/aethyme" update bootstrap \
    --payload "$payload" \
    --install-dir "$install_dir" \
    --manifest "$manifest" \
    --target "$target"
case ":${PATH}:" in
    *:"$install_dir":*) ;;
    *) printf 'Add %s to PATH before running aethyme.\n' "$install_dir" ;;
esac
