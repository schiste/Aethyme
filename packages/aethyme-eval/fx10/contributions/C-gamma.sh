#!/bin/sh
set -eu
case River in
    River) printf '%s|%s\n' River 08:40 ;;
    *) exit 1 ;;
esac
