# Supervised generated-artifact claims

Last Updated: 2026-10-25

Run a generator from its broker session worktree:

```sh
aethyme broker advanced resources artifact-run config/generated.json --session 123 -- generator --output config/generated.json
```

This explicit cooperative workflow uses the existing host coordinator's
exclusive-key allocation and supervised command lifecycle. The same repository
and normalized relative file path produce the same exclusive key across
worktrees and sessions. Another invocation cannot take the claim over because
its owner becomes idle. It can wait with `--wait`; the default is immediate
contention refusal. Normal named ownership claims keep their existing semantics.

The request is bound to an open recorded session and requires running inside
that session's own checkout. Closed sessions, another checkout, ambiguous paths,
Git-administrative paths, existing symlinks, directories and Unix hard-linked
files are refused before resource acquisition. Keys fold NFC Unicode, lowercase,
and trailing dots/spaces conservatively, including on case-sensitive filesystems.

The claim lasts through command completion, optional `--cleanup-command`, and
verified original process-group cleanup. Unknown completion retains the existing
quarantine/reconciliation fence. A 300-second resource TTL is renewed while the
runner lives; TTL expiry is not permission to run a competing generator. As with
`resources run`, the TTL is not a command timeout. Descendants that deliberately
leave the original process group are outside its observation guarantee.

Public host-resource inventory retains `run_id = artifact-session:<id>:<nonce>`
and a digest of the worktree path. Raw worktree paths and ownership tokens are
not added to command output. Request IDs are public one-shot replay identifiers;
a collision refuses a launch, rather than adopting an earlier supervised run.
The existing run report and child stdout/stderr behavior remain unchanged.

This is not a filesystem sandbox or enforcement against arbitrary editors,
commands outside the protocol, aliases created after validation, or different
host registries. A generator must only modify the artifact it claimed; multiple
files need a complete typed resource bundle using `resources run`. Services,
containers and asynchronous dispatch must not be launched as detached generators.
The base-failure incident registry and automatic generator adoption under #760
remain separate work.
