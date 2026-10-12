#!/bin/sh
# FX17 transport proxy v1. Source is fixed independently of candidate app.sh.
set -eu
PATH=/usr/bin:/bin
LC_ALL=C
export PATH LC_ALL
umask 077
tmp=$(mktemp -d /tmp/fx17-proxy.XXXXXX)
trap 'rm -rf "$tmp"' EXIT HUP INT TERM
run_case() {
  id=$1
  shift
  if /bin/sh /artifact/app.sh "$@" >"$tmp/stdout" 2>"$tmp/stderr"; then code=0; else code=$?; fi
  out=$(od -An -v -tx1 "$tmp/stdout" | tr -d ' \n')
  err=$(od -An -v -tx1 "$tmp/stderr" | tr -d ' \n')
  printf '%s\t%s\t%s\t%s\n' "$id" "$code" "$out" "$err"
}
printf 'FX17V1\n'
run_case lookup_alpha lookup alpha
run_case lookup_beta lookup beta
run_case lookup_gamma lookup gamma
run_case list list
run_case arity_zero
run_case lookup_missing lookup
run_case lookup_extra lookup alpha extra
run_case lookup_four lookup alpha '' extra
run_case list_empty_extra list ''
run_case list_key_extra list alpha
run_case list_extra_three list alpha beta
run_case list_four list --limit 2 extra
run_case mode_empty ''
run_case mode_unknown unknown
run_case mode_upper_lookup LOOKUP alpha
run_case mode_trailing_lookup 'lookup ' alpha
run_case mode_leading_list ' list'
run_case mode_upper_list LIST
run_case mode_unicode 'líst'
run_case mode_option --help
run_case mode_shell_substitution '$(printf INJECTED)'
run_case mode_shell_sequence 'list; printf INJECTED'
run_case lookup_empty lookup ''
run_case lookup_unknown lookup delta
run_case lookup_uppercase lookup Alpha
run_case lookup_numeric lookup 0
run_case lookup_trailing_space lookup 'alpha '
run_case lookup_leading_space lookup ' alpha'
run_case lookup_tab lookup 'alpha	'
run_case lookup_newline lookup 'alpha
'
run_case lookup_carriage_return lookup 'alpha'
run_case lookup_nonbreaking_space lookup 'alpha '
run_case lookup_unicode lookup 'άλφα'
run_case lookup_fullwidth lookup 'ａｌｐｈａ'
run_case lookup_combining lookup 'alphá'
run_case lookup_zero_width lookup 'alpha​'
run_case lookup_glob lookup '*'
run_case lookup_question_glob lookup 'a?pha'
run_case lookup_bracket_glob lookup '[a]lpha'
run_case lookup_dollar lookup '$KEY'
run_case lookup_command_substitution lookup '$(printf alpha)'
run_case lookup_backtick lookup '`printf alpha`'
run_case lookup_semicolon lookup 'alpha; printf INJECTED'
run_case lookup_pipe lookup 'alpha | printf INJECTED'
run_case lookup_and lookup 'alpha && printf INJECTED'
run_case lookup_redirect lookup 'alpha > fx17-effect-canary'
run_case lookup_quote lookup 'alpha'"'"'"\'
run_case lookup_percent lookup %s
run_case display_missing display
run_case display_alpha display alpha
run_case display_beta display beta
run_case display_gamma display gamma
run_case display_empty display ''
run_case display_unknown display delta
run_case display_extra display alpha extra
run_case limit_missing_value list --limit
run_case limit_empty list --limit ''
run_case limit_flag_typo list --limits 1
run_case limit_flag_case list --LIMIT 1
run_case limit_0 list --limit 0
run_case limit_1 list --limit 1
run_case limit_2 list --limit 2
run_case limit_3 list --limit 3
run_case limit_03 list --limit 03
run_case limit_999 list --limit 999
