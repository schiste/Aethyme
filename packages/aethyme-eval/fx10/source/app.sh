#!/bin/sh
set -eu
usage() {
    printf '%s\n' 'usage: lookup KEY | list' >&2
    exit 2
}
if [ "$#" -eq 1 ] && [ "$1" = list ]; then
    printf '%s\n' alpha beta gamma delta epsilon zeta
    exit 0
fi
if [ "$#" -ne 2 ] || [ "$1" != lookup ]; then
    usage
fi
case "$2" in
    alpha|beta|gamma|delta|epsilon|zeta)
        /bin/sh "/artifact/features/$2.sh"
        ;;
    *)
        usage
        ;;
esac
