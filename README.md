<h1 align="center">Sieve</h1>

<p align="center">Sieve gives your coding agent exact files, lines and callers. It runs locally and needs no API key.</p>

<p align="center"><img src="assets/banner.svg" alt="Six pixel mascots. Sieve, a kitchen sieve, is the default." width="480"></p>

<p align="center">
  <a href="https://github.com/iamalvisng/sieve/actions"><img src="https://img.shields.io/github/actions/workflow/status/iamalvisng/sieve/ci.yml?label=CI" alt="CI"></a>
  <a href="https://github.com/iamalvisng/sieve/releases"><img src="https://img.shields.io/github/v/release/iamalvisng/sieve" alt="version"></a>
  <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="license"></a>
</p>

## Why

A coding agent that needs to find code usually greps, opens whole files and guesses at what calls what. That costs tokens and time, and a wrong guess leads to a wrong edit.

Sieve indexes your repo into a symbol graph with call edges. Your agent (Claude Code, Codex or any MCP host) asks Sieve "where is this" or "what calls this", and gets the exact file, the line range and the callers in one answer. The agent reads only the lines it needs.

Real output from the vite repo:

```
$ sieve callers handleHMRUpdate --depth 2
handleHMRUpdate  fn  packages/vite/src/node/server/hmr.ts:411-679
2 callers · 5 within 2 hops
├─ testRestartDuringHotUpdate  fn  packages/vite/src/node/server/__tests__/hmr.spec.ts:6-40
│  │  34: handleHMRUpdate(
│  └─ hmr.spec.ts  file  packages/vite/src/node/server/__tests__/hmr.spec.ts:1-56
└─ onHMRUpdate  fn  packages/vite/src/node/server/index.ts:908-915
   │  913: await handleHMRUpdate(type, file, server)
   ├─ onFileAddUnlink  fn  packages/vite/src/node/server/index.ts:917-953
   └─ onFileChange  fn  packages/vite/src/node/server/index.ts:955-969
```

Sieve runs on your machine. It sends no telemetry and makes no network call for a query.

## Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/iamalvisng/sieve/releases/latest/download/sieve-cli-installer.sh | sh
```

The installer puts `sieve` in `$CARGO_HOME/bin`. Put that folder on your `PATH`.

Other channels:

```sh
brew install iamalvisng/tap/sieve
cargo binstall sieve-cli
cargo install --locked --git https://github.com/iamalvisng/sieve sieve-cli
```

See [docs/guide/install.md](docs/guide/install.md) for each operating system.

## Quick start

```sh
cd your-repo
sieve init --dry-run   # lists every file init would write; writes nothing
sieve init             # builds the index and connects your agents
sieve query "how does login work"
```

`init` builds the index and sets up the agents it finds: instruction files, the MCP server, and for Claude Code the hooks and the status line. It also writes the Codex config under `~/.codex/`. Add `--no-global` to keep every write inside the repo. `sieve init --mod` adds the Claude Code pane. `sieve uninstall` removes everything `init` wrote.

After that, just work. Your agent calls Sieve by itself, and the index refreshes before each query, including edits you have not committed.

To use Sieve from another MCP host, register `sieve mcp` as a stdio server:

```json
{ "mcpServers": { "sieve": { "command": "sieve", "args": ["mcp"] } } }
```

See [docs/guide/mcp.md](docs/guide/mcp.md).

## Commands

| Command | What it does |
|---|---|
| `sieve query "<question>"` | Ranked symbols with exact file and line. `--source` inlines the code. |
| `sieve find <text>` | Every match, grouped under its symbol. |
| `sieve callers <symbol>` | Who calls a symbol, to the depth you choose. Each edge is exact or inferred. |
| `sieve impact` | What depends on the lines your diff changed, and whether a test reaches them. |
| `sieve symbols <file>` | The outline of one file, with line ranges. |
| `sieve why <symbol>` | The decision records, tests and history that link to a symbol. |
| `sieve map` | An overview of the repo. |
| `sieve rename <old> <new>` | Renames a TypeScript or JavaScript top-level function across files. Dry run by default. |
| `sieve check` | Checks the index. |

The old names `ask`, `grep`, `blast` and `skeleton` still work. The [command reference](docs/guide/commands.md) lists every flag. See also [rename](docs/guide/rename.md).

## Fresh and fast

The MCP server keeps the index in memory and watches your files, so an answer never comes from an old index.

Warm MCP calls on a clone of the vite repo (commit 90bc034a, about 1,600 files). The test ran in a Linux arm64 Docker container with release builds. Each cell is the median of 5 calls after one warm-up. The range shows two runs. No file changed between the calls.

After a file edit, `find` reads again only the edited file. The first `query` after an edit is slower; the second table shows it.

| Call | 0.2.0 | 0.2.1 | 0.2.2 | 0.2.3 |
|---|---|---|---|---|
| `query` | 119–168 ms | 120–158 ms | 93–104 ms | 31–40 ms |
| `find`, plain text | 44–57 ms | 47–60 ms | 18–21 ms | 7–8 ms |
| `find`, regex | 45–49 ms | 46–61 ms | 19–23 ms | 7–8 ms |
| `callers` | 70–71 ms | 71–80 ms | 0.4–0.5 ms | 0.4–0.5 ms |

0.2.3 against 0.2.4, measured in one later session with the same method. The 0.2.3 release binary is faster here than in the first table, so compare only within one table.

| Call | 0.2.3 | 0.2.4 |
|---|---|---|
| `query` | 24–31 ms | 8–16 ms |
| `query`, first call after a file edit | 350–373 ms | 236–239 ms |
| `find`, plain text | 6 ms | 6 ms |
| `find`, regex | 5–6 ms | 5–6 ms |
| `callers` | 0.3 ms | 0.3 ms |

The after-edit row appends one line to one file, waits 300 ms, then times one `query`.

macOS shows the same trend. The macOS numbers are not in this table because the measuring Mac was under load.

Run `scripts/bench-mcp.sh <sieve-binary> <repo>` to measure on your own machine and repo.

For about 2 seconds after a new folder appears, Linux uses a slower full check. Set `SIEVE_NO_WARM=1` to turn the in-memory index off.

## Languages

Sieve parses TypeScript, JavaScript, Python, Go, Java, Kotlin, Swift, PHP and R natively. It parses 14 more with a generic parser. It reads script blocks in Vue, Svelte and Astro files. Exact cross-file links cover TypeScript, JavaScript, Rust, Python, Go, Java, C# and Kotlin, each with its own scope. Other links stay inferred: treat them as hints. See [docs/guide/languages.md](docs/guide/languages.md).

## Limits

- Sieve has not measured token savings in a real agent session.
- `sieve rename` handles top-level functions in TypeScript and JavaScript only. It refuses many renames.
- There is no Windows build.

The [FAQ](docs/guide/faq.md) lists more limits.

## Docs

[Install](docs/guide/install.md) · [Quickstart](docs/guide/quickstart.md) · [Concepts](docs/guide/concepts.md) · [Commands](docs/guide/commands.md) · [Claude Code](docs/guide/claude-code.md) · [MCP](docs/guide/mcp.md) · [Configuration](docs/guide/configuration.md) · [Privacy](docs/guide/privacy.md) · [Benchmarks](docs/guide/benchmarks.md) · [Troubleshooting](docs/guide/troubleshooting.md) · [FAQ](docs/guide/faq.md) · [Decision records](docs/adr/)

Also: [CHANGELOG](CHANGELOG.md) · [CONTRIBUTING](CONTRIBUTING.md) · [SECURITY](SECURITY.md)

## License

Sieve uses the MIT license or the Apache-2.0 license, at your choice. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). Third-party notices are in [THIRD_PARTY.md](THIRD_PARTY.md).
