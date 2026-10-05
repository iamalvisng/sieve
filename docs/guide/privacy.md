# Privacy

Sieve runs on your machine. Sieve sends nothing.

## What Sieve sends

- Sieve makes no network call. `sieve version` runs no update check.
- Sieve sends no telemetry. `sieve telemetry` records nothing and writes no file.
- Sieve has no LLM pass. Sieve does not send your code to a model provider.

## What Sieve stores

| Where | What |
|---|---|
| `sieve/` in the repo | The index: the graph, per-file cards and caches. |
| `sieve/.cache/session/` | Session files. `sieve stats` reads them. |
| `.sieve/config.json` in the repo | Build settings and the pane mascot. |
| `.gitignore`, `.ignore` in the repo | Entries that `build` adds. |

`init` also writes agent files. See [Claude Code](claude-code.md).
Only `init --global` writes under your home folder.

The index holds data about your code. Do not commit `sieve/`. `build` adds it to `.gitignore`.

## Hooks

Hook settings exist only after you run `sieve init`. `sieve uninstall`
removes them. The hooks run the local `sieve` program.

## Remove everything

```sh
sieve uninstall --yes
```

Add `--keep-cache` to keep `sieve/`.

## Report a security problem

Use the GitHub private vulnerability report at github.com/iamalvisng/sieve
(Security tab, "Report a vulnerability"). Do not open a public issue.
