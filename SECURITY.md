# Security Policy

## Supported Scope

The public open-source support scope is `packages/aethyme`, the Aethyme Core
tooling package.

`packages/aethyme-cloud` and `packages/aethyme-eval-ui` are not production
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

## Disclosure

Maintainers will acknowledge valid private reports, assess affected versions,
and coordinate fixes before public disclosure whenever practical.
