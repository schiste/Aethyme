#!/bin/sh
# FX10 fixed independent route and CLI contract proxy.
set -eu
PATH=/usr/bin:/bin
LC_ALL=C
export PATH LC_ALL
umask 077
tmp=$(mktemp -d /tmp/fx10-proxy.XXXXXX)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
run_case() {
  id=$1
  shift
  if /bin/sh /artifact/app.sh "$@" >"$tmp/stdout" 2>"$tmp/stderr"; then code=0; else code=$?; fi
  out=$(od -An -v -tx1 "$tmp/stdout" | tr -d ' \n')
  err=$(od -An -v -tx1 "$tmp/stderr" | tr -d ' \n')
  if [ -z "$out" ]; then out=-; fi
  if [ -z "$err" ]; then err=-; fi
  printf '%s\t%s\t%s\t%s\n' "$id" "$code" "$out" "$err"
}
printf 'FX10V1\n'
run_case lookup_alpha lookup alpha
run_case lookup_beta lookup beta
run_case lookup_gamma lookup gamma
run_case lookup_delta lookup delta
run_case lookup_epsilon lookup epsilon
run_case lookup_zeta lookup zeta
run_case list list
run_case arity_zero
run_case lookup_missing lookup
run_case lookup_extra lookup alpha extra
run_case mode_unknown route
run_case lookup_unknown lookup eta
run_case lookup_shell_sequence lookup 'alpha; printf INJECTED'
run_case lookup_command_substitution lookup '$(printf alpha)'
run_case mode_upper_lookup LOOKUP alpha
run_case list_extra list alpha
