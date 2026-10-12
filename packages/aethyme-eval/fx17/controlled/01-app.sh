#!/bin/sh
set -eu
emit() { printf '%s\n' "$1"; }
usage() { printf '%s\n' 'usage: lookup KEY | list' >&2; exit 2; }
lookup() {
    case "$1" in
        alpha) emit 'first' ;;
        gamma) emit 'third' ;;
        beta) emit 'second' ;;
        *) usage ;;
    esac
}
list_values() { emit 'alpha'; emit 'beta'; emit 'gamma'; }
case "$#" in
    1) case "$1" in list) list_values ;; *) usage ;; esac ;;
    2) case "$1" in lookup) lookup "$2" ;; *) usage ;; esac ;;
    *) usage ;;
esac
