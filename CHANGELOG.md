# Changelog

All notable changes to this project are in this file.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
This project follows [Semantic Versioning](https://semver.org/).

## [0.1.5] - 2026-10-13

This release adds exact call links for Go and Java, exact Rust self.f() links, hook timeouts in seconds, and a faster refresh after an edit.

### Added

- Exact Rust `self.f()` links. Sieve marks a `self.f()` call Extracted when the caller sits in a module-level inherent impl and the same impl body holds exactly one `fn f` with an equal receiver form. Oracle results, matched Extracted before -> after, 0 wrong each: Sieve 5,398 -> 5,541, ripgrep 3fce3b5 1,206 -> 1,544, itertools v0.15.0 160 -> 178.
- Exact Go call links. Sieve marks a Go call Extracted only when it proves one target. The proof covers a call whose scope has one binding in the same file. It also covers `pkg.F()` calls through imports and `go.mod`, across files, and receiver types written in the code. Interface calls, func values and promotion stay Inferred.
- The oracle scripts `scripts/oracle/go-calls.py` and `scripts/oracle/go-compare.py` check Go links against scip-go. Results: fzf v0.74.5 has 1,362 matched, 0 wrong and 72 unverified. gin v1.12.0 has 1,573 matched, 0 wrong and 0 unverified. caddy v2.11.7 has 4,156 matched, 0 wrong and 12 unverified.

- Exact Java call links. Sieve marks a Java call Extracted only when it proves one target. The proof covers bare calls, `this` and `super` calls, static calls through imports, typed receivers and `new`. Sieve checks overrides across the in-repo subtypes. A call to an interface or abstract target stays Inferred.
- The oracle scripts `scripts/oracle/JvCalls.java` and `scripts/oracle/jv-compare.py` check Java links against the javac tree API (JDK 21.0.12+8).

### Fixed

- Sieve no longer marks a Java call Extracted by its name or by the first same-file candidate. Wrong Extracted links before and after: jsoup jsoup-1.23.2 140 to 0, commons-lang rel/commons-lang-3.21.0 829 to 0, gson gson-parent-2.14.0 23 to 0.
- Sieve wrote hook timeouts as milliseconds, but Claude Code and Codex read seconds. Sieve now writes 3 to 15 seconds. `sieve init` rewrites old entries. If a settings file still holds old values, `sieve init` prints a warning that names the file and the command that fixes it, and the statusline shows one note until that command runs.

### Changed

- A 1-file refresh before a query now takes about half the time or less. Examples (median of 5): 0.76 s to 0.18 s on a 362-file Go repo, 1.46 s to 0.62 s on a 629-file Java repo, 3.07 s to 0.25 s on a Go repo with a 254 KB file. The output is byte-identical to a full build. `SIEVE_TIMING=1` prints each refresh phase to stderr. Go indexing of very large files stays near the speed of earlier releases.
- `CACHE_VERSION` changes. The first build after the upgrade re-parses every file.

## [0.1.4] - 2026-10-13

This release adds exact call links for Rust and Python.

### Added

- Exact Rust call links. Sieve marks a Rust call Extracted only when it proves one target. The proof covers a free function in the same file, a free function in the same crate through the module tree and `use` items, a function in another workspace crate, and `Type::f()` and `Self::f()` calls. ADR 0007 holds the rules.
- Exact Python call links. Sieve marks a Python call Extracted only when a scope proof finds one binding in the same file, or when an import chain finds one definition in another file. Cross-file proof covers `import`, `from ... import`, relative imports and `__init__.py` re-exports.
- The oracle scripts `scripts/oracle/rs-calls.py` and `scripts/oracle/rs-compare.py` check Rust links against rust-analyzer. The scripts `scripts/oracle/py-calls.py` and `scripts/oracle/py-compare.py` check Python links against scip-python.
- The guide shows the oracle results in the benchmarks page.
- `sieve ask --def <identifier>` prints the body of the top definition (at most 60 lines) and one callers line. The flag is opt-in. The session-start hint and `ask --help` name it. Sieve makes no token-saving claim for it.

### Fixed

- Sieve no longer marks a Rust or Python call Extracted by its name alone. The old rule linked a call to any function with the same name in the same file. Before this fix, Sieve itself had 254 wrong links of 4,622 Rust Extracted links, ripgrep 3fce3b5 had 173 of 938, httpx b5addb6 had 1 of 324 and black 11cff13 had 20 of 833. After the fix, all four have 0 wrong links. A call that Sieve cannot prove stays Inferred.

### Changed

- `CACHE_VERSION` changes. The first build after the upgrade re-parses every file.
- Some Rust and Python call edges that were Extracted are now Inferred. The golden files for the Rust and Python fixtures record this change.

## [0.1.3] - 2026-10-09

0.1.3 counts every search the agent runs and lowers the memory use on large repos.

### Added

- Sieve writes a per-file lookup `sieve/.cache/lookup.v1` next to the wiring on every build. The hooks (search routing, the Read narrowing, the post-edit blast, the scope hint, the stale count) and the MCP `check_freshness` tool read one file's record from the lookup. They no longer load the whole graph. A repo over the wiring cap keeps those hooks. `ask` and the MCP graph tools still need the full graph. Sieve rebuilds a stale or missing lookup from the wiring on the first query. This rebuild does no source parse.

### Changed

- Sieve regroups a Bash `grep` or `rg` result by file and function only when every output row is `path:line:text` from an indexed file and the regrouped text is smaller. Most real agent searches do not meet this rule and pass through unchanged.
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
