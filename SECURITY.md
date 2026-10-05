# Security policy

## Supported versions

| Version | Supported |
|---|---|
| 0.1.x | Yes |

## Report a problem

Use the GitHub private vulnerability report on the repository
(github.com/iamalvisng/sieve). Open the Security tab and choose
"Report a vulnerability". Do not open a public issue.

Include the Sieve version, the steps to repeat the problem, and the effect.

The response goal is 7 days. This is a goal, not a promise.

## Facts about Sieve

- Sieve runs local only.
- Sieve makes no network call. The `sieve version` command prints the crate
  version and runs no update check. No command prints an update notice.
- Sieve sends no telemetry.
- Sieve writes hook settings only when you run `sieve init`.
- `sieve uninstall` removes those hook settings.
