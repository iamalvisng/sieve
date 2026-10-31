# Changelog

All notable changes to this project are in this file.

The format follows [Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/).
This project follows [Semantic Versioning](https://semver.org/).

## [0.2.4] - 2026-10-31

This release makes a warm MCP `query` faster, fixes the Read hook, and makes the status line count subagent work.

### Changed

- A warm MCP `query` does no per-query copy of the link graph. On the vite repo on Linux, with no file change since the last call: 24–31 ms in 0.2.3 to 8–16 ms.
- The first warm `query` after a file edit reads the changed files from memory and does not wait for the index write. On the vite repo on Linux: 350–373 ms in 0.2.3 to 236–239 ms.
- `find` and `query` print `~N tokens saved` on the answer line when the answer is smaller than its baseline. `find` compares with the plain grep rows. `query` counts only code that it cuts from a symbol longer than 80 lines. Many calls save nothing, and then the line shows no figure.
- The status line counts the main session and its subagents in "this session". Before, it counted only the main session.

### Fixed

- The Read hook skips a Read only when earlier Reads of the unchanged file returned every requested line. Before, a second Read was skipped even for lines that the agent never saw. A failed Read no longer counts as read.
- The prompt hook no longer stores a subagent report or a host notice as the last query.

## [0.2.3] - 2026-10-31

This release makes warm MCP `query` and `find` calls faster.

### Changed

- A warm MCP `query` reuses the link graph shape, the word counts, the corpus and the ask index between calls. On the vite repo on Linux, with no file change since the last call: 93–104 ms in 0.2.2 to 31–40 ms. After a file edit, the first `query` rebuilds the cache and does not get this speed. That case is not measured here.
- A warm MCP `find` reuses file text between calls and reads only lines that can match. On the vite repo on Linux: 18–23 ms to 7–8 ms. After an edit, only the edited file is read again.
- Each cache checks the file's time, size and inode (and change time on Unix) before use, so an edited file is always read again.
- `query` ranks a file under `scripts/` lower, like a test file, unless the query says "script" or names the file.
- `scripts/bench-mcp.sh` measures warm MCP calls on any repo.

## [0.2.2] - 2026-10-30

This release keeps the index in memory in the MCP server.

### Changed

- The MCP server keeps the index in memory and watches files. A query with no file change answers without a re-scan. On macOS the watch uses kqueue. On Linux it uses inotify.
- On macOS, on a clone of the vite repo (about 1,600 files), M1 Pro, release build, median of 15 runs: a `callers` query through the MCP server takes about 1 ms with no file change, and about 77 ms after a one-file edit. `query` is slower and not tuned yet.
- On Linux the numbers are not measured yet. For about 2 seconds after a new folder appears, Sieve uses a slower full check, because the Linux file watch can miss the first files in a new folder. Answers stay correct on both systems.
- `callers` reads each file once for its savings figure.
- `SIEVE_NO_WARM=1` turns the in-memory index off.

### Fixed

- An edit to `package.json`, `go.mod`, `pyproject.toml`, `setup.py`, `Cargo.toml`, `composer.json`, `pom.xml`, `build.gradle`, `go.work`, `pnpm-workspace.yaml` or a `.csproj` file now refreshes the index. Before, only `Cargo.toml` did.

## [0.2.1] - 2026-10-28

This release adds the 🎯 mark to the status line.

### Changed

- The status line shows 🎯 before the first savings figure: `sieve · 🎯 ~N this session · ~N today`.

## [0.2.0] - 2026-10-28

This release gives the commands names that state their intent, adds an MCP rename tool, and indexes shell scripts.

### Changed

- New command names: `ask` is now `query`, `grep` is now `find`, `blast` is now `impact`, and `skeleton` is now `symbols`. `callers`, `why`, `map`, `rename` and `check` keep their names.
- The old command names still work as aliases.
- New MCP tool names: `sieve_query`, `sieve_find`, `sieve_callers`, `sieve_symbols`, `sieve_map`, `sieve_why`, `sieve_rename` and `sieve_check`. A call with an old tool name still works. `tools/list` shows only the new names.
- The answer line is `🎯 <answer> · ~N tokens saved`. The estimate uses a narrow baseline for each command. Totals count only answers made with this formula.
- The status line shows `sieve · ~N this session · ~N today`, with the mascot face beside it. The mascot blinks.
- The first build after the upgrade re-parses every file.

### Added

- `sieve symbols --json` gives `inDegree`, the caller count, for each entry.
- The `sieve_rename` MCP tool. A call without `write` is a dry run. `write: true` changes files and needs the `plan_id` of a dry run in `expect`.
- Sieve indexes shell scripts (`.sh`, `.bash`): functions, and the code outside them.
- Exact Rust call links for calls inside format and assert macro arguments, such as `println!("{}", f(x))`.
- Exact call links for a function passed as a value, such as `.map(helper)` or `sorted(xs, key=score)`, in Rust, TypeScript, JavaScript and Python. Sieve adds the link only when it proves the target.
- The pane has SESSION, TODAY and 7 DAYS tiles that wrap, and new views for `why` and `map`. The pane draws no background of its own.
- The `bin` key in `.sieve/config.json` picks the binary that the pane runs. The pane reads the key on each run.

### Fixed

- `sieve init --mod` no longer adds a second set of hooks when the existing hooks call Sieve by a quoted or `$VAR` path.
- `query` ranks code above test functions when the question does not ask for tests. A "how does ... work" question no longer matches the word "work".
- `impact` counts the callers in the changed file.
- A lock whose owner process has stopped no longer blocks a graph refresh.
- `map` keeps a space before each count and lists every indexed language.
- The pane shows the error of a failed command, not "output that is not JSON". If the binary is too old for a command, the pane names the binary and its version.
- The pane SESSION tile shows the current session. Before, it showed the newest session file.
- The pane file view lists the most called symbols of every file. Before, it found them only for some folders.

## [0.1.9] - 2026-10-28

This release adds `sieve rename`, a cross-file rename for TypeScript and JavaScript top-level functions.

### Added

- `sieve rename <symbol> <new-name>` renames a TypeScript or JavaScript top-level function across files. It is a dry run by default and prints a plan and a diff. Each plan has an id. `--write --expect <plan-id>` writes the files only when the plan, computed again, has the same id.
- Sieve refuses the whole rename when it cannot prove every site. The refusal names the line and the reason. It refuses a default export, a class, a method, a namespace import and a star barrel. It refuses an untraceable dynamic import when the name is also read as a property, a key or a string.
- Sieve checks its own result after a write. It restores every file on any doubt.
- `--recover` restores the old files after a crash. `--discard-journal` deletes the leftover journal. Other flags: `--json`, `--context <n>`, `--in <path>`, `--dir <path>`.
- Exit codes: 0 for a ready or written plan, 1 for a usage or I/O error, 2 for a refusal.
- Results against the TypeScript language service rename, on vite (commit 10033218) and execa v9.6.0: 0 wrong edits. Vite renamed 162 of 300 sampled functions and refused the rest. Execa renamed 283 of 300 and refused the rest.
- The guide has a new page, [Rename](docs/guide/rename.md).

## [0.1.8] - 2026-10-15

This release adds exact Kotlin call links for `x.f()`, `Type.f()` and `super.f()` calls.

### Added

- Exact Kotlin links for a call on a typed value (`x.f()`), a call on an object or a companion (`Type.f()`) and `super.f()`. Sieve marks the call Extracted only when it proves one target.
- Results on the Kotlin compiler check, Extracted / matched / wrong / unverified: mockito-kotlin v6.4.0 122 / 122 / 0 / 0, kotlinpoet 2.4.0 704 / 679 / 0 / 25, hoplite v3.0.3 385 / 385 / 0 / 0. On ktorm v4.2.1, a repo the rules never saw before the run: 238 / 238 / 0 / 0.

### Changed

- A Kotlin override check stops at any function with the same name in a subtype. This keeps a smart-cast call Inferred.
- The first build after the upgrade re-parses every file.

## [0.1.7] - 2026-10-15

This release adds exact call links for Kotlin, and C# calls that reach a type through a `global using`.

### Added

- C# calls resolve a type through a `global using` of the same project.
- Exact Kotlin call links. Sieve marks a Kotlin call Extracted only when it proves one target. The proof covers bare calls, `this.f()` calls and constructor calls, across files through imports and packages. Calls on a receiver such as `x.f()` stay Inferred. Sieve checks overrides across the in-repo subtypes. A call to an interface, abstract or `expect` target stays Inferred.
- The oracle scripts `scripts/oracle/KtCalls.kt`, `scripts/oracle/kt-calls.sh` and `scripts/oracle/kt-compare.py` check Kotlin links against the Kotlin compiler (kotlinc 2.4.21, K2). Results, Extracted / matched / wrong / unverified: mockito-kotlin v6.4.0 120 / 120 / 0 / 0, kotlinpoet 2.4.0 412 / 388 / 0 / 24, hoplite v3.0.3 195 / 195 / 0 / 0.

### Fixed

- A C# type or method after a leading `#if` line now starts at its first code line.
- Kotlin parsing uses a newer grammar. A class with a primary constructor on the next line now parses, and its members keep the class as owner.

### Changed

- Hook and intercept notes name Sieve in plain words.
- Sieve no longer links a Kotlin call by its name alone. Wrong Extracted links before and after: mockito-kotlin 4 to 0, kotlinpoet 6 to 0, hoplite 5 to 0.
- `CACHE_VERSION` changes. The first build after the upgrade re-parses every file.

## [0.1.6] - 2026-10-14

This release adds exact call links for C#, and fixes two setup issues in `sieve init` and `sieve uninstall`.

### Added

- Exact C# call links. Sieve marks a C# call Extracted only when it proves one target. The proof covers bare and `this.` calls, calls on a local, a parameter or a primary constructor parameter, and `new T()`, `base.M()`, `Type.M()` and field receivers. Sieve checks overrides across the in-repo subtypes. A call to an interface target stays Inferred.
- The oracle scripts `scripts/oracle/CsCalls` and `scripts/oracle/cs-compare.py` check C# links against Roslyn (.NET SDK 10.0.401). Results, Extracted / matched / wrong / unverified: markdig 1.4.0 912 / 890 / 0 / 22, serilog v4.4.0 150 / 149 / 0 / 1, CsvHelper 33.1.0 2,189 / 2,166 / 0 / 23.

### Changed

- `CACHE_VERSION` changes. The first build after the upgrade re-parses every file.

### Fixed

- Sieve no longer links a C# call by its name alone. Wrong Extracted links before: markdig 6 of 528, serilog 7 of 264, CsvHelper 1 of 188. After: 0 on each.
- `sieve init` no longer adds the allowlist entry `Bash(node dist/cli.js:*)`, and `sieve uninstall` no longer removes it. The entry runs another tool's command, so an entry the user had before init now survives uninstall. An install from 0.1.0 to 0.1.5 keeps this entry after uninstall; remove it by hand if you do not need it.
- The viz server retries a read that a signal interrupts.
- When `sieve init` rewrites old millisecond hook timeouts, it now says so after the write (`✓ Rewrote old millisecond hook timeouts in <path>`). Before, it printed a warning that told the user to run `sieve init`. A file that this run does not cover still gets the warning with the command that fixes it.

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
