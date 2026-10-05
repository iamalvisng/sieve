# Changelog

All notable changes to this project are in this file.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
This project follows [Semantic Versioning](https://semver.org/).

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

[0.1.1]: https://github.com/iamalvisng/sieve/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/iamalvisng/sieve/releases/tag/v0.1.0
