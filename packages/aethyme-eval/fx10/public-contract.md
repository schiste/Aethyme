# FX10 route lookup public contract

`app.sh` is a stdlib-only POSIX shell route lookup command. It accepts exactly one of:

- `list`: print `alpha`, `beta`, `gamma`, `delta`, `epsilon`, `zeta` in that order, one per line, then exit 0.
- `lookup KEY`: for one exact key, print its route and departure as `ROUTE|HH:MM` and exit 0.

The independent route outputs are:

| Key | Output |
| --- | --- |
| alpha | `Alpine|08:10` |
| beta | `Coastal|08:25` |
| gamma | `River|08:40` |
| delta | `Garden|08:55` |
| epsilon | `Harbor|09:10` |
| zeta | `Market|09:25` |

Any other arity, mode, key, case, whitespace, Unicode or shell-metacharacter input prints exactly `usage: lookup KEY | list` plus one newline to stderr, prints nothing to stdout, and exits 2. Arguments are data; the command never evaluates them. It performs no network access or filesystem writes. The six feature modules are separate source paths and each is executed only after the router's exact-key allowlist.

A valid feature submission updates only its corresponding route module while preserving every public vector above. A-C are one required atomic older group; D-F are three independent, later-arriving compatible submissions. Acceptance is allowed only through the appointed native gate, isolated Docker build, external verifier and exact disposable bare-Git target. The oracle and expected transcript stay outside candidate artifacts.

The sustained-arrival qualification first admits A-C and observes D-F arriving while the real A-C native gate is dispatching behind a typed host capacity hold. New admission width stays at three; six is only the maximum cumulative group after later arrivals are considered. The same service appointment must preserve the active A-C wave and then accept D-F in the next wave, with all six obligations retained and completed.

A separate negative schedule uses one more disposable target and one controlled external target update. Its accepted bare ref advances once from the frozen baseline to a fast-forward child commit with the same tree while the A-C native job is dispatching behind a finite one-unit resource hold. The hold releases after 250 ms. The actual external verifier then runs and must produce a passing receipt after the ref advance; the candidate is stale for publication. One ordinary compare-and-swap push must fail, the competing ref must remain at its new commit, and the service must hold with no private completion and no automatic retry. This is a single bounded update case, not a repeated-drift reliability claim.
