# Local v3 dogfooding on Aethyme

Use the Local workflow while developing this repository. Agents keep using
broker-managed branches and worktrees of this same Git repository.

## Initial operating profile

- Enable only this repository with `[collaboration] capture = "advisory"`.
- Preserve verify-only promotion and the existing review and delivery rules.
- Capture is local retained storage; it does not enable an autonomous
  controller, an external service or remote acceptance.
- Older binaries can continue operating but do not capture contributions.
  Agents participating in dogfooding must use the deployed Local binary pair.
- Experimental records and the open composition, model-quality, pressure and
  operator qualification items retain their existing limits. See the
  [qualification ledger](../architecture/local-v3-qualification.md).

## Land and deploy

1. Land the independently reviewed Local runtime and its migration definitions
   through a main-based integration PR. The integration retains the tested
   stack; it does not rebase it or replay consumed qualification trials.
2. Land the separately reviewed repository opt-in. Refresh the fetched default
   ref through the broker: capture policy reads that committed configuration,
   not a session's working-copy proposal.
3. Build the CLI and engine together from an exact, clean commit contained in
   the fetched default branch, using the workspace lockfile and `-j 4`.
   Do not writable-open the real broker database with an unmerged development
   build: migration numbers are assigned by the default branch.
4. Retain a manifest naming the source commit and both binary hashes. Install
   the pair in a repository-scoped, immutable runtime directory and select the
   pair together. Preserve the previous pair for diagnosis; a binary rollback
   does not authorize a database downgrade.
5. Resolve the selected runtime directory once per agent and put its immutable
   version's `bin` directory at the front of the clean PATH. Pin that resolved
   path for the operation so CLI, hooks and engine subprocesses use the same
   pair even if a later deployment changes the selection.
6. Run `aethyme collab status --json` against the actual checkout. Confirm
   `config_source` is `committed`, policy is `advisory`, and the required-capture
   fence has not been raised. Missing state before the first submit is normal.
   Run `aethyme certify --json` and confirm `certify.config` recognizes the
   collaboration section without an unknown-key warning.

## Use it for real work

Start a broker session, inspect task context with Explore, make a genuine
product change, and use the normal commit, push and draft-PR workflow.
Run `aethyme broker submit --session <id> --verify-only --json` when the change
is ready for verification. Inspect both the ordinary verification result and
`collaboration_capture`: advisory capture cannot make a failed verification
successful or make an unsuccessful capture block a legacy submit verdict.

Use the returned operation identifier to inspect its retained receipt and
context through the public collaboration CLI. Keep its exact source identity
with the task evidence. Preserve state needed by live sessions; reclamation
uses reviewed public GC plans rather than deleting the archive directly.

## Fix, review and redeploy

When normal use exposes a bug or a concrete improvement, record the observed
behavior and fix the generic product in a new broker session. Add a regression
check, obtain independent review, and land through the same delivery rules.
Build and deploy the next exact default-compatible binary pair, then repeat the
operation that exposed the problem and continue the Local plan work.

Dogfooding on Aethyme is operational use. Formal evaluations, fresh model
trials and held-out qualification remain on Playground repositories. Never
replay consumed campaigns or change frozen fixtures to improve their scores.
