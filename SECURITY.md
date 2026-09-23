# Security Policy

## Supported Scope

The public open-source support scope is `packages/aethyme`, the Aethyme Core
tooling package.

Removed historical packages (recoverable from git history) are not production
security support surfaces unless they are explicitly moved into the public
release scope.

## Reporting A Vulnerability

Do not report exploitable details in a public issue.

Use GitHub private vulnerability reporting when it is enabled for the
repository. If private reporting is not available, open a minimal public issue
asking maintainers to establish a private security contact, without including
exploit details, secrets, or reproduction payloads.

Please include:

- affected package and version or commit
- impact summary
- reproduction steps or proof of concept, if safe to share privately
- whether credentials, tenant isolation, repository data, or generated artifacts
  are involved

## Security Expectations

- Never commit real secrets, access tokens, customer data, private repository
  contents, or production database dumps.
- Treat generated eval reports and local runtime databases as local artifacts
  unless they have been reviewed for publication.
- Use unique development secrets. Example secrets in this repository are not
  suitable for production.

## Threat Model

The Aethyme broker is a cooperative guardrail for agents that run as the same
user on one machine. It keeps well-behaved agents from colliding and makes
shared Git and GitHub mutations deliberate and journaled. It is not a security
boundary or a sandbox: anything that can run a shell as you can do anything you
can do.

### What it stops

- Gates and graph policy are read from the base tree, so a session cannot
  weaken the checks that judge its own diff (v0.8.0).
- `broker git` / `broker gh` classify each command's effect, treat unknown
  commands as writes at minimum, refuse code-executing `-c` configuration
  keys, and verify pushes against the operation journal in a pre-push hook
  (v0.8.0).
- A repeated program name in a coordinated command is refused (v0.8.1).
- `enhance deploy` refuses to write or chmod through a symlinked target or
  parent directory, so a cloned repository cannot redirect generated files
  outside its checkout; the operation journal redacts credential-bearing
  `-c`/`--config-env` values (`http.*extraheader`, authorization, token,
  password keys) and `Authorization`/token `-H` header values (unreleased).

### What it does not stop

- An agent with a shell can run `git` or `gh` directly, bypassing the broker.
- Gate commands and scripts in the tree run with your full user environment,
  credentials, and network access.
- A malicious change to gate scripts that has already been merged runs on the
  next gate like any other trusted code.
- Git aliases and other settings in repository configuration still apply to
  the Git commands the broker runs.
- Review lanes run with your credentials.

### Operator guidance

- Run untrusted repositories in a separate user account or a VM.
- Treat `.aethyme/gates.toml` in a cloned repository like a Makefile: read it
  before running anything that executes gates.

## Disclosure

Maintainers will acknowledge valid private reports, assess affected versions,
and coordinate fixes before public disclosure whenever practical.
