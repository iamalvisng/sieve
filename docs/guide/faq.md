# FAQ

## What is Sieve?

Sieve is a code-intelligence tool for coding agents. It indexes a repo into
a symbol graph with call edges. It answers `ask`, `grep`, `callers`, `blast`,
`map`, `skeleton` and `why` queries. It also runs an MCP server.

## Does Sieve need an API key?

No. The build and every query run with no key and no LLM.

## Does Sieve send my code anywhere?

No. Sieve makes no network call and sends no telemetry. See [Privacy](privacy.md).

## Does it work on Windows?

There is no Windows build in 0.1.0. See [Install](install.md).

## Which agents does it support?

Claude Code has the full setup: hooks, status line and MCP. Other agents get
instruction files and an MCP entry. See [Claude Code](claude-code.md).

## Which languages does it support?

See [Languages](languages.md).

## Are the links exact?

Only for named relative imports in TypeScript and JavaScript. On vite
10033218, all 1,232 exact cross-file calls match the TypeScript compiler by name.
621 calls stay Inferred.

## How many tokens does it save?

Sieve prints an estimate after a query and `sieve stats` totals it. The
estimate is the size of the whole files the answer names, minus the answer.
It is not a measured saving in an agent session. See
[Benchmarks](benchmarks.md) for the measured numbers.

## Should I commit `sieve/`?

No. `build` adds it to `.gitignore`. A teammate runs `sieve build`.

## How do I undo `sieve init`?

Run `sieve uninstall --yes`. Without `--yes`, it only lists what it would remove.

## What does `sieve why` read?

Decision records in your repo: `docs/decisions/`, `docs/adr/`, `adr/`, and
`LEDGER.md`. See [Commands](commands.md#why).

## Which version is this guide for?

Version 0.1.0.
