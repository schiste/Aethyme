#!/bin/sh
set -eu
usage() { printf '%s\n' 'usage: lookup KEY | list' >&2; exit 2; }
lookup() {
    case "$1" in
        alpha) emit 'first' ;;
        beta) emit 'second' ;;
        gamma) emit 'third' ;;
        *) usage ;;
    esac
}
list_values() { emit 'alpha'; emit 'beta'; emit 'gamma'; }
emit() { printf '%s\n' "$1"; }
if [ "$#" -eq 1 ] && [ "$1" = list ]; then
    list_values
elif [ "$#" -eq 2 ] && [ "$1" = lookup ]; then
    lookup "$2"
else
    usage
fi
