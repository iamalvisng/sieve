<h1 align="center">Sieve</h1>

<p align="center">Sieve gives your coding agent exact files, lines and callers. It runs locally and needs no API key.</p>

<p align="center"><img src="assets/banner.svg" alt="Six pixel mascots. Sieve, a kitchen sieve, is the default." width="480"></p>

```
$ sieve ask "how does the dev server handle hot module replacement"
ask  how does the dev server handle hot module replacement · 8 hits

1  [packages/vite/] createServerModuleRunner  fn  packages/vite/src/node/ssr/runtime/serverModuleRunner.ts:143-160
   function createServerModuleRunner( environment: DevEnvironment, options: ServerModuleRunnerOptions = {}, ): ModuleRunner

2  [packages/vite/] handleHMRUpdate  fn  packages/vite/src/node/server/hmr.ts:411-679
   async function handleHMRUpdate( type: 'create' | 'delete' | 'update', file: string, server: ViteDevServer, ): Promise<void>

3  [packages/vite/] _createServer  fn  packages/vite/src/node/server/index.ts:513-1148
...
```

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

On vite (1,583 files), `sieve ask` answers in 158 ms median on an Apple M1 Pro. See [Proof](#proof).

<!-- DEMO: assets/pane-demo.gif, recorded by the owner. The caption names the Claude Code version. -->

Then run two commands in your repo:

```sh
sieve build .
sieve ask "how does login work"
```

To wire Sieve into your agent, run `sieve init --dry-run`. The command lists every file `init` would write. Then run `sieve init`. For the Claude Code pane, run `sieve init --mod`.

`init` writes only inside the repo. Only `--global` writes under your home folder.

## Find out why code exists

`sieve why <symbol>` shows the decision records that link to a symbol. A record is an ADR file or a `LEDGER.md` entry with a `- **Code:**` line. Your existing ADR files work.

Sieve reads these decision folders: `docs/decisions/`, `docs/adr/`, `adr/`, and `LEDGER.md`. Sieve ships its own records in [docs/adr/](docs/adr/).

This is real output from the public Sieve tree:

```
$ sieve why savings_line
why savings_line  crates/sieve-savings/src/lib.rs:96
decision  docs/adr/adr-0004-savings-header.md:1  ADR 0004: Each query result opens with one savings line
          link: Code anchor
test  crates/sieve-savings/src/lib.rs:236  savings_line_names_the_given_product
test  crates/sieve-savings/src/lib.rs:248  test_savings_sieve_header_has_no_tally_instruction
...
```

## What you get

The samples are real output from vite. A `...` line marks cut output.

### Find the code without grep

`sieve ask` returns ranked symbols with exact file and line. The `--source` flag inlines the code. `sieve grep` finds every match and groups each hit under its symbol.

### See who calls a symbol

`sieve callers` lists the callers to a depth you choose. Each row gives the file and the line. `callers` and `blast` mark each edge as exact or inferred.

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
[sieve] saved ≈ 20,282 tokens
```

### See what a diff breaks before CI does

`sieve blast` lists what depends on the lines your diff changed. It also says if a test reaches the code. This sample ran after one edited line in `handleHMRUpdate` (`hmr.ts`).

```
$ sieve blast
your diff changes handleHMRUpdate · 1 symbol in 2 files · 1 of 1 reached by a test · working tree vs HEAD
3 callers affected within 2 hops
└─ onHMRUpdate  fn  packages/vite/src/node/server/index.ts:908-915
   ├─ onFileAddUnlink  fn  packages/vite/src/node/server/index.ts:917-953
   └─ onFileChange  fn  packages/vite/src/node/server/index.ts:955-969
1 test suite also references this code (not listed)
tests for handleHMRUpdate were not updated
ask for review  翠 (80 commits, 11d ago) · btea (4 commits, 28d ago)
not indexed  1 file: .gitignore
```

### Read a file as an outline

`sieve skeleton` prints the outline of one file with line ranges.

```
$ sieve skeleton packages/vite/src/node/server/index.ts
packages/vite/src/node/server/hmr.ts · 42 symbols
    48-56  WsOptions  iface
    58-88  HmrOptions  iface
    90-97  HotUpdateOptions  iface
   99-105  HmrContext  iface
  107-111  PropagationBoundary  iface
  113-115  HotChannelClient  iface
  117-120  HotChannelListener  type  <T extends string = string> = ( data: InferCustomEventPayload<T>, client: HotChannelClient, ) => void
...
```

Sieve also has `map`. The [command reference](docs/guide/commands.md) lists all commands.

### Graph viewer

`sieve viz` serves a graph viewer in your browser.

<!-- SCREENSHOT: assets/viewer.png, taken by the owner. -->

### Read intercept

Sieve answers a Read of a file from the index. A read with `offset` or `limit` passes unchanged.

### Remembered reads

Sieve remembers each file the agent reads in a session. The second read of an unchanged file gets this note:

```
[sieve] a.ts is unchanged since your last read. Use that copy. If you no longer hold it, read again.
```

Sieve denies every second read of an unchanged file. The read after a deny passes. A changed file always passes.

### Pane

`sieve init --mod` adds a pane and a 3-line status line to Claude Code. Run `/sieve-pane` to open the pane. It shows callers, impact, decision records and saved tokens. Run `/sieve-mascot <name>` to pick one of six pixel mascots. Sieve, the kitchen sieve, is the default.

### MCP

`sieve mcp` serves the index to any MCP host over stdio. The server has seven tools: `sieve_find_code`, `sieve_find_all`, `sieve_trace_calls`, `sieve_file_api`, `sieve_repo_map`, `sieve_check_freshness` and `sieve_why`.

Register it with `{ "mcpServers": { "sieve": { "command": "sieve", "args": ["mcp"] } } }`. See [docs/guide/mcp.md](docs/guide/mcp.md).

## Proof

Sieve 0.1.0 on an Apple M1 Pro with 16 GB of memory, macOS 26.5.2.

| Claim | Repo and commit | Number | Method |
|---|---|---|---|
| Cold build | ripgrep 3fce3b5 (111 files) | 0.73 s | Median of 3 builds, each on a fresh copy. |
| Cold build | vite 10033218 (1583 files) | 1.83 s | Median of 3 builds, each on a fresh copy. |
| Warm build | ripgrep 3fce3b5 | 0.31 s | Median of 5 builds with no change. |
| Warm build | vite 10033218 | 0.80 s | Median of 5 builds with no change. |
| `ask` latency | ripgrep 3fce3b5 | 62 ms median, 101 ms max | Wall time of one `sieve ask` process. 30 timings: 10 questions, 3 runs each. |
| `ask` latency | vite 10033218 | 158 ms median, 185 ms max | The same method. |
| Exact cross-file calls | vite 10033218 | 1,272 of 1,272 match the TypeScript compiler | Name-level match. Inferred calls are not counted. |

[docs/guide/benchmarks.md](docs/guide/benchmarks.md) gives the method and the questions, so you can repeat each run.

## Languages

Sieve parses TypeScript, JavaScript, Python, Go, Java, Kotlin, Swift, PHP and R natively. It parses 14 more with a generic parser. It reads script blocks in Vue, Svelte and Astro files. Exact cross-file links cover relative TypeScript and JavaScript imports: named, aliased, namespace and default imports, and barrel re-exports. They do not cover package specifiers, `paths` aliases, CommonJS or `export =`. See [docs/guide/languages.md](docs/guide/languages.md).

## Local only

Sieve runs on your machine. Sieve sends no telemetry and makes no network call for a query. See [ADR 0001](docs/adr/adr-0001-local-only.md), [docs/guide/privacy.md](docs/guide/privacy.md) and [SECURITY.md](SECURITY.md).

## When not to use Sieve

- Sieve links calls across files as exact only in TypeScript and JavaScript, through relative imports and barrel re-exports. Package specifiers, `paths` aliases, CommonJS and other languages stay Inferred. Treat Inferred links as hints. See [ADR 0006](docs/adr/adr-0006-barrel-reexports.md).
- Sieve has not measured token savings in a real agent session.
- There is no Windows build in v0.1.0.

The [FAQ](docs/guide/faq.md) lists more limits.

## Docs

- [Install](docs/guide/install.md)
- [Quickstart](docs/guide/quickstart.md)
- [Concepts](docs/guide/concepts.md)
- [Commands](docs/guide/commands.md)
- [Claude Code](docs/guide/claude-code.md)
- [MCP](docs/guide/mcp.md)
- [Configuration](docs/guide/configuration.md)
- [Languages](docs/guide/languages.md)
- [Privacy](docs/guide/privacy.md)
- [Benchmarks](docs/guide/benchmarks.md)
- [Troubleshooting](docs/guide/troubleshooting.md)
- [FAQ](docs/guide/faq.md)
- [Decision records](docs/adr/)

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md) before you open a pull request. Report a security issue as [SECURITY.md](SECURITY.md) describes. The [changelog](CHANGELOG.md) lists each release.

## License

Sieve uses the MIT license or the Apache-2.0 license, at your choice. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE). Third-party notices are in [THIRD_PARTY.md](THIRD_PARTY.md).

<p align="center">
  <a href="https://github.com/iamalvisng/sieve/actions"><img src="https://img.shields.io/github/actions/workflow/status/iamalvisng/sieve/ci.yml?label=CI" alt="CI"></a>
  <a href="https://github.com/iamalvisng/sieve/releases"><img src="https://img.shields.io/github/v/release/iamalvisng/sieve" alt="version"></a>
  <a href="LICENSE-MIT"><img src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue" alt="license"></a>
</p>
