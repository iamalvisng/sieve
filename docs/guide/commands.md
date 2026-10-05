# Commands

Run `sieve --help` for the full list. The samples come from a run on the Vite
repo. A line with `...` marks cut output. Every other line is as Sieve printed it.
A `[dir]` argument defaults to the nearest ancestor folder with a `sieve/` index.

## build

```
$ sieve build .
...
  parsed: 0 of 1583 files (1583 replayed from cache)
...
  sieve/ is git-ignored (added automatically) — a local cache; teammates run `sieve build` to get their own.
```

Flags: `-e`, `--no-reuse`, `--lsp`, `--include-dir`, `--only-dir`, `--no-gitignore`,
`--no-ignore`, and the follow flags. See [Configuration](configuration.md).

## ask

```
$ sieve ask "how does the dev server handle hot module replacement" --source
[sieve] saved ≈ 30,366 tokens
...
1. [packages/vite/] createServerModuleRunner · function  [symbol]
   packages/vite/src/node/ssr/runtime/serverModuleRunner.ts:L143-L160
   function createServerModuleRunner( environment: DevEnvironment, options: ServerModuleRunnerOptions = {}, ): ModuleRunner
...
```

Flags: `-n/--limit`, `--source`, `--full`, `--in <path>`, `--json`, `--no-refresh`.

## callers

```
$ sieve callers handleHMRUpdate --depth 2
[sieve] saved ≈ 20,269 tokens

handleHMRUpdate · function · packages/vite/src/node/server/hmr.ts:L411-L679
  calls ← testRestartDuringHotUpdate (packages/vite/src/node/server/__tests__/hmr.spec.ts:L6-L40) [depth 1]
      34: handleHMRUpdate(
  calls ← onHMRUpdate (packages/vite/src/node/server/index.ts:L908-L915) [depth 1]
      913: await handleHMRUpdate(type, file, server)
  calls ← hmr.spec.ts (packages/vite/src/node/server/__tests__/hmr.spec.ts:L1-L56) [depth 2]
  calls ← onFileAddUnlink (packages/vite/src/node/server/index.ts:L917-L953) [depth 2]
...
```

Each edge has a confidence, Extracted or Inferred. See
[ADR 0002](../adr/adr-0002-exact-cross-file-links.md).

Flags: `--direction <in|out>` (`out` lists callees), `-d/--depth <n|all>`, `--in`, `--json`.

## grep

```
$ sieve grep "handleHMRUpdate"
[sieve] saved ≈ 20,230 tokens
...
"handleHMRUpdate" — 5 hits in 5 symbols across 3 files (searched 1583 indexed files)
...
handleHMRUpdate · function · packages/vite/src/node/server/hmr.ts:L411-L679 · 2 in-edges
  L411: export async function handleHMRUpdate(
```

Flags: `-i/--ignore-case`, `--fixed`, `--in <path>`, `--json`, `--no-refresh`.

## map

```
$ sieve map
...
repo map — 1583 files · 3283 symbols · 10055 edges · javascript, jsx, tsx, typescript
...
```

Flags: `--max-dirs <n>` (default 16), `--json`, `--no-refresh`.

## skeleton

```
$ sieve skeleton packages/vite/src/node/server/index.ts
[sieve] saved ≈ 10,282 tokens
...
- L241-L268  interface FileSystemServeOptions  interface FileSystemServeOptions
...
```

Flags: `--json`, `--no-refresh`. The argument is a repo-relative path or a unique file name.

## blast

This sample ran after one edited line in `handleHMRUpdate` (`hmr.ts`).

```
$ sieve blast
blast radius — working tree vs HEAD (depth 2)
  changed: 2 files in 1 area, 1 seed symbol
  impacted: 3 symbols in 1 area
...
⚠ handleHMRUpdate — 1 changed file, 1/1 reached by a test
...
  calls ← onHMRUpdate (packages/vite/src/node/server/index.ts:L908-L915) [depth 1]
```

Flags: `--base <ref>`, `-d/--depth`, `--format text|markdown|mermaid|json`,
`--export-viz <dir>`, `--no-owners`, `--pr-author`. `--format markdown` makes a PR comment.

## stats

```
$ sieve stats
sieve stats: no session recorded yet — use sieve in an agent session, then look again.
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
