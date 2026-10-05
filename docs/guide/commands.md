# Commands

Run `sieve --help` for the full list. The samples come from a run on the Vite
repo. A line with `...` marks cut output. Every other line is as Sieve printed it.
A `[dir]` argument defaults to the nearest ancestor folder with a `sieve/` index.

## build

```
$ sieve build .
sieve sifted vite-new in 1.6 s
1,583 files → 4,866 symbols · 10,055 links
astro · javascript · jsx · svelte · tsx · typescript · vue
index in ./sieve · git-ignored · stays on this machine
```

Flags: `-e`, `--no-reuse`, `--lsp`, `--include-dir`, `--only-dir`, `--no-gitignore`,
`--no-ignore`, and the follow flags. See [Configuration](configuration.md).

## ask

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

Flags: `-n/--limit`, `--source`, `--full`, `--in <path>`, `--json`, `--no-refresh`.

## callers

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

Each edge has a confidence, Extracted or Inferred. See
[ADR 0002](../adr/adr-0002-exact-cross-file-links.md).

Flags: `--direction <in|out>` (`out` lists callees), `-d/--depth <n|all>`, `--in`, `--json`.

## grep

```
$ sieve grep "handleHMRUpdate"
grep "handleHMRUpdate" · 5 hits in 5 symbols across 3 files · searched 1583 files

handleHMRUpdate  fn  packages/vite/src/node/server/hmr.ts:411-679  · 2 links in
  411: export async function handleHMRUpdate(

_createServer.onHMRUpdate  fn  packages/vite/src/node/server/index.ts:908-915  · 2 links in
  913: await handleHMRUpdate(type, file, server)

...
```

Flags: `-i/--ignore-case`, `--fixed`, `--in <path>`, `--json`, `--no-refresh`.

## map

```
$ sieve map
map · 1583 files · 3283 symbols · 10055 links · javascript, jsx, tsx, typescript

## playground/environment-react-ssr/
playground/environment-react-ssr/vite.config.ts1 file · 8 symbols   hubs: importWithRetry (vite.config.ts, 2←), vitePluginSsrMiddleware (vite.config.ts, 1←)
playground/environment-react-ssr/src/3 files · 4 symbols   hubs: importHtml (entry-server.tsx, 1←), main (entry-client.tsx, 1←)
playground/environment-react-ssr/__tests__/1 file · 0 symbols

## playground/optimize-missing-deps/
playground/optimize-missing-deps/__test__/2 files · 2 symbols
playground/optimize-missing-deps/server.js1 file · 2 symbols   hubs: createServer (server.js, 1←), resolve (server.js, 1←)
...
```

Flags: `--max-dirs <n>` (default 16), `--json`, `--no-refresh`.

## skeleton

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

Flags: `--json`, `--no-refresh`. The argument is a repo-relative path or a unique file name.

## blast

This sample ran after one edited line in `handleHMRUpdate` (`hmr.ts`).

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

Flags: `--base <ref>`, `-d/--depth`, `--format text|markdown|mermaid|json`,
`--export-viz <dir>`, `--no-owners`, `--pr-author`. `--format markdown` makes a PR comment.

## stats

```
$ sieve stats
no saved tokens yet — they appear after an agent session uses sieve
```

## why

`sieve why <symbol>` shows the decision records that explain a symbol. Sieve
reads these decision folders in your repo: `docs/decisions/`, `docs/adr/`,
`adr/`, and `LEDGER.md`.

A record links to code with an anchor line. The line starts with `- **Code:**`
and lists node ids in backticks. A record with a `Status:` line that says
Superseded shows as SUPERSEDED.

Example only. This ADR is made up:

```
# adr-0007-use-keyset-cursor
Status: Accepted
- **Date:** 2026-01-02
- **Code:** `src/orders.ts#listOrders`
- **Decision:** List orders with a keyset cursor, not an offset.
```

Real output on the Sieve tree:

```
$ sieve why savings_line
why savings_line  crates/sieve-savings/src/lib.rs:96
decision  docs/adr/adr-0004-savings-header.md:1  ADR 0004: Each query result opens with one savings line
          link: Code anchor
test  crates/sieve-savings/src/lib.rs:236  savings_line_names_the_given_product
```

Flags: `--check` lists SUPERSEDED records that still link to code. `--all`
shows every row, not the first 10. `--suggest`, `--diff`, `--json`.

## Other commands

- `check`: fails if the index is stale. `viz`: serves a graph viewer (`-p/--port`, `--no-open`, `--export <dir>`).
- `mcp`: see [MCP](mcp.md). `init`, `uninstall`: see [Claude Code](claude-code.md).
- `telemetry` shows that Sieve sends no usage data. The command records nothing. `version` prints one line. Top-level options: `-v`, `--dir <path>`.
