#!/bin/sh
set -eu
lookup() {
    case "$1" in
        gamma) emit 'third' ;;
        alpha) emit 'first' ;;
        beta) emit 'second' ;;
        *) usage ;;
    esac
}
list_values() { emit 'alpha'; emit 'beta'; emit 'gamma'; }
emit() { printf '%s\n' "$1"; }
usage() { printf '%s\n' 'usage: lookup KEY | list' >&2; exit 2; }
if [ "$#" -eq 1 ] && [ "$1" = list ]; then
    list_values
elif [ "$#" -eq 2 ] && [ "$1" = lookup ]; then
    lookup "$2"
else
    usage
fi
