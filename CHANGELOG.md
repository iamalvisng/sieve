# Changelog

All notable changes to this project are in this file.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
This project follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.3] - 2026-10-09

0.1.3 routes searches on the output shape and records every search segment.

### Added

- Sieve writes a per-file lookup `sieve/.cache/lookup.v1` next to the wiring on every build. The hooks (search routing, the Read narrowing, the post-edit blast, the scope hint, the stale count) and the MCP `check_freshness` tool read one file's record from the lookup. They no longer load the whole graph. A repo over the wiring cap keeps those hooks. `ask` and the MCP graph tools still need the full graph. Sieve rebuilds a stale or missing lookup from the wiring on the first query. This rebuild does no source parse.

### Changed

- Search routing now routes on the output shape. If every stdout row of a Bash `grep` or `rg` command is `path:line:text` from indexed files, the hook regroups the rows by file and enclosing symbol. This holds for compound and piped commands too.
- Every search segment of a Bash command gets a `routed` or `passed` record. `sieve stats` shows the count of each record.
- The `pre-search` hook block and the `-n` injection are removed. `sieve init` drops the old block from an existing config.
- The build keeps one copy of each node. This lowers the peak memory of the warm rebuild and the cold build. `wiring.json` and `ask-index.json` are byte-identical to before.
- The README and the languages guide now say that exact links cover aliased, namespace and default imports and barrels. ADR 0004 now says the savings line is last.

### Fixed

- `sieve hook pre-read` no longer denies a first Read of a file that a subagent of the same session read. The read record now keys on the session id and the agent id. A file outside the repo passes through with no record. A binary file (a NUL byte in its first 8,000 bytes) passes through. The repo check runs before any file read.

## [0.1.2] - 2026-10-08

### Added

- The public export holds the oracle scripts in `scripts/oracle/`.
- The `xfile2` and `xfile3` fixtures with frozen oracles, and the `nsexport` shape in the TypeScript oracle.
- ADR 0005 and ADR 0006.
- A PostToolUse hook `sieve hook post-search` regroups Grep tool matches by file and enclosing symbol, only when every match stays and the output shrinks by 10% or more. It counts `picked`, `routed` and `passed` per session and agent.
- Two Bash hooks, `pre-search` and `post-search`, route a single simple `grep` or `rg` command. `pre-search` adds `-n`. `post-search` regroups the matches under the same rule as the Grep tool. A Bash output of 30,000 bytes or more passes.
- `sieve init` adds the two Bash hook blocks and the Grep hook block to an existing config without duplicates.
- A `capped` flag reports an index over the size cap.


### Changed

- A TS or JS call through an aliased, namespace or default import is now an Extracted edge when the target file has one matching Function.
- A TS call through a barrel re-export (`export { x } from`, `export *`, `export * as ns`) is now an Extracted edge when the chain gives one Function. On vite, 1,272 of 1,272 Extracted edges match the compiler. CACHE_VERSION is now 5, so the first build after the upgrade runs cold.
- A build that projects over the memory ceiling now refuses with one line and writes nothing under `sieve/`. The ceiling is the smaller of 1.5 GB and 20% of physical memory. The env var `SIEVE_BUILD_CEILING_BYTES` sets it.
- A wiring file over 64 MB makes every hook pass through. The env var `SIEVE_WIRING_CAP_BYTES` sets the cap. The statusline shows "index over the size cap; hooks pass through".
- The statusline and `sieve init` read counts from `sieve/.cache/counts.json` before any wiring parse.

### Fixed

- A brace inside a string no longer loses the `<script>` block of a Svelte or Astro file.
- The build memory guard counted every walked file. It now counts the parsed files, so it no longer refuses a cold build that fits.

## [0.1.1] - 2026-10-06

### Changed

- Sieve has its own output voice.
  `callers` and `blast` print trees.
  Every row has one grammar: `name kind path:start-end`.
  Every error has one shape: `sieve: ... — ...`, with exit code 1.
  Color and the mascot show only on a terminal.
  The saving line is last.
  JSON output is unchanged.
- The pane shows the caller tree for whole-file reads.
  The pane also shows the caller tree for sieve queries from Bash and MCP.
- The pane text has a readable contrast.
- The pane mascot is smaller.
- The status-line mascots have 2 lines.
- The pane tiles have padding.
- The band says "X and N more changed → M callers affected".

### Fixed

- Card names that differ only in case resolve the same way on macOS and Linux.
- Tests set their own `PATH` and color environment.
- `sieve skeleton` on a file that is not in the index exits with code 1.
- The pane runs at most 3 queries per view.
  The pane stops an old query when a new query starts.
- The `cargo install` docs use `--locked`.

## [0.1.0] - 2026-10-05

First public release.

### Added

- The index. Sieve reads a repo into a symbol graph with call edges.
- The queries: `ask`, `grep`, `callers`, `blast`, `map`, `skeleton` and `why`.
- `sieve why` and its decision anchors. An anchor links a recorded rule or
  reason to the code it covers.
- The read intercept. Sieve answers a read from the graph.
  Sieve does not narrow a read that names `offset` or `limit`.
- Remembered reads. Sieve denies every second read of an unchanged file.
  The read after a deny passes.
- Exact cross-file links for named relative imports in TypeScript and JavaScript.
- The Claude Code pane with 6 mascots and a 3-line status line.
  The look uses Tokyo Night colors.
- The graph viewer.
- The MCP server.
- `sieve stats`. It reports tokens saved for the session, the day and 7 days.
  Use `--json` for machine output.
- A local-only design. Sieve sends no telemetry.

[0.1.3]: https://github.com/iamalvisng/sieve/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/iamalvisng/sieve/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/iamalvisng/sieve/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/iamalvisng/sieve/releases/tag/v0.1.0
