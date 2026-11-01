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

Sieve prints a saving on the answer line only when it can compare the
answer with what an agent reads without Sieve, and `sieve stats` totals it:

- `callers`: grep output for the same name, minus the answer.
- `find`: the `grep -n` rows for the same hits, minus the answer.
- `query`: the code the answer cut from a long symbol.
- A Read that the hook skips: the lines that Read asked for.

`query --def` and `symbols` print no saving. Their only true baseline is
about the size of the answer, or costs a full scan. Most calls save little,
and then the line shows no figure. The status line also counts lookups,
which needs no estimate. See [Benchmarks](benchmarks.md) for the measured
numbers.

## Should I commit `sieve/`?

No. `build` adds it to `.gitignore`. A teammate runs `sieve build`.

## How do I undo `sieve init`?

Run `sieve uninstall --yes`. Without `--yes`, it only lists what it would remove.

## What does `sieve why` read?

Decision records in your repo: `docs/decisions/`, `docs/adr/`, `adr/`, and
`LEDGER.md`. See [Commands](commands.md#why).

## Which version is this guide for?

Version 0.1.0.
