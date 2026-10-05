# Quickstart

This page takes you from a fresh install to your first answer.

## 1. Build the index

Go to the root of a repo. Run:

```sh
sieve build .
```

Sieve reads your code and writes an index folder named `sieve/`.
The build needs no API key.
Sieve adds `sieve/` to your `.gitignore`. The index is a local cache.

## 2. Ask a question

A query does not build the index. If no index exists, Sieve prints
"No index here yet. Run `sieve build .` first."

```sh
sieve ask "how does login work"
```

Sieve returns ranked symbols with exact `file:line` spans.
Add `--source` to see the code inline.

## 3. Find who calls a symbol

```sh
sieve callers createServer --depth 2
```

## 4. Search for text

```sh
sieve grep "handleHMRUpdate"
```

Sieve groups each hit under the symbol that holds it.

## 5. See your change reach

```sh
sieve blast
```

This shows what depends on the lines of your working-tree diff.

## 6. Wire Sieve into your agent

```sh
sieve init
```

Without `--yes`, `init` shows a picker. Without `--global`, `init` writes
only inside the repo. See [Claude Code](claude-code.md) for the details.
To see the plan first, run `sieve init --dry-run`.

## Stay in sync

Query commands refresh the index before they answer. Pass `--no-refresh`
to skip that. To fail a CI job when the index is stale, run `sieve check`.

## Next steps

- [Concepts](concepts.md)
- [Commands](commands.md)
