# Local v3 exact execution inputs (#670)

Last Updated: 2026-10-11

An execution descriptor binds the candidate's retained raw source, any declared
preparation recipe and its actual source output, prepared dependencies,
configuration, harness, dataset, assertion inventory, image identities,
architecture, runtime profile, environment, commands and resource limits.
Source identity, execution identity and immutable build output identity are
separate. Descriptors and materialized files grant no verification authority.

## Materialization

Every input must already exist in the retained collaboration archive. The
existing archive verifier and raw materializer are reused; no Git checkout,
filter, hook, package manager, preparation script or source-controlled command
runs during materialization. Each input gets its own new private directory.
Ignored files and developer environment are never copied from a checkout.
Raw line endings, binary bytes and executable modes are preserved and checked.
Non-UTF-8 paths, symlinks, nonportable backslash names, folded path/directory
collisions, unresolved LFS pointers and missing/corrupt objects refuse.
Submodules and source-affecting filters are already refused by retained Git
capture. No new support for them is introduced here.

The entire descriptor and its source roots are pinned to the selected target
revision under the archive lock. Physical object locators are explicit beside
logical record IDs, because their hash domains differ. Reads verify both.
Historical descriptors remain data; a new managed materialization requires
current controller authority and the exact active target revision.

## Execution boundary

Read-only host file permissions are defense in depth. They do not qualify T13
or prevent transient mutation by privileged host code. The supervisor in #671
must enforce read-only workload input mounts, a pinned image, no network or
control socket, fresh writable outputs and bounded process lifetime, then stop
all descendants before sealing output. The external verifier and evidence
producer in #672 must run after sealing and own their mandatory inventory.
A candidate exit code or uploaded result is not an independent verdict.

The first profile describes Linux Docker jobs with exact image content IDs,
architecture and a separately approved runtime descriptor. The supervisor
must verify actual image/runtime properties before dispatch; a declared ID
alone does not prove an executor was used. Environment values are explicit
operator-provided nonsecret recipe data, never inherited ambient credentials.
Prepared inputs and their provenance remain declared until observed by the
supervisor; this package issues no success-shaped execution receipt.

Experimental schemas and composition qualification still depend on E1 #650.
No broker schema, queue, gate or legacy publication behavior is changed. Typed
candidate integration waits for #725. End-to-end Local qualification follows
L6/L7 and the remaining Local packages; these records alone do not close Q3.
