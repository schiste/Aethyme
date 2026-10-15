# Local composed candidates at the native gate boundary

Last Updated: 2026-10-15

`aethyme.native-local-gates/experimental-v1` prepares a durable native job for
the current Local wave and invokes the existing broker gate executor on its
exact candidate commit. It uses the typed verification candidate contract from
#725 in this session's branch. The separately owned draft remains untouched.

## Binding and execution

Preparation records the repository and Git common directory, target revision,
authority epoch, wave, recipe, original baseline and input commit identities,
candidate tree and commit, and raw baseline gate/graph/preparation policy hashes.
Dispatch requires current explicit policy trust and a verifying target/wave;
budget, capacity and requirement holds refuse dispatch. Revoked trust cannot be
replaced by legacy gate-history grandfathering.

The private checkout reconstructs retained source bytes and installs metadata
for the original candidate commit. It disables hooks, templates, attributes and
fsmonitor. Attribute-bearing trees require a separately appointed Git profile.
Placement inside an enclosing repository or project manifest is refused. Gate
commands run with native host authority after explicit baseline policy trust;
this profile is not a command sandbox.

The profile admits at most 32 gates, each with a 1–60 second timeout, resource
admission wait of at most 60 seconds, and no managed cache. A 300-second gate
runner allowance covers owner/resource admission and commands, including checks
immediately before spawn and before crediting completion. This allowance starts
after source materialization and graph regeneration; it is not an end-to-end
job deadline. Baseline graph checking uses the existing implementation in a
separate disposable checkout.

One durable prepared-to-dispatching transition permits one execution. Reopening
a dispatching or unknown job never silently retries. Completion rechecks exact
source, modes, symlink targets, HEAD and tree. Observations confer neither
`TrustedEvidence` nor provider acceptance. No legacy merge queue entry or ref
promotion is created. The independent verifier and fresh provider acceptance
remain mandatory for autonomous acceptance.

## Retention and qualification

Collaboration schema/floor 18 adds native jobs and independent roots for the
baseline, subject, recipe, job record and original contribution roles. Releasing
revision roots does not release these jobs. Roots currently remain indefinitely;
inactive job release and reconciliation remain implementation work. Quiesce old
open collaboration clients before upgrading; the floor protects fresh opens.

The Local integration is retained at `cb6dfeb57dc0f6c1d423e13fc78ea16e90e24529`
(#758), with passing final workspace, lint, example/doctest, Windows and product
CI checks. Local controls cover the actual composer-to-native-gate path,
revoked trust, exact candidate policy, deadlines and private placement. The first
compile/runtime failures remain historical failures, rather than passing evidence.

The separately appointed [Local controller](local-v3-autonomous-controller.md)
now consumes this native job boundary on its actual finite two-wave path. That
path explicitly allows empty native commands and still runs independent artifact
verification and fresh private GenericGit acceptance. It does not extend the
native profile's host trust or deadline guarantees, or qualify FX17.
