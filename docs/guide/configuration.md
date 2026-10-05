# Configuration

Sieve needs no config file. This page lists the settings that exist.

## The index folder

The index lives in `<repo>/sieve/`. Two things change that folder:

- `--dir <path>` is a top-level option for the index folder.
- The `SIEVE_DIR` variable sets it for hooks and for `sieve stats`.
  A relative value is relative to the repo root. `stats` does not read `--dir`.

## `.sieve/config.json`

`sieve build` writes `.sieve/config.json` in the repo root. It adds `/.sieve/`
to `.gitignore`. The file holds:

- `includeDirs`: folders you add with `--include-dir`.
- Follow flags: your last choice for submodules and nested repos.
- `mascot`: the pane mascot. See [Claude Code](claude-code.md).

Sieve keeps every other key in the file. If the file is not a JSON object,
Sieve leaves it as it is and prints a warning.

## Build options

Build options that you set persist for later builds and for automatic refreshes:

- `--include-dir <name>` adds a folder that the walk skips by default.
  Dot folders are never added.
- `--only-dir <path>` indexes only files under that path.
- `--follow-submodules` and `--follow-nested-repos` include more code.
- `--no-follow-submodules` and `--no-follow-nested-repos` turn them off (the default).
- `-e, --extensions` picks the extensions to index.
- `--no-reuse` parses every file again.
- `--lsp` adds call edges from a language server, if one is installed.
  It is slower and opt-in.

## Environment variables

| Variable | Effect |
|---|---|
| `SIEVE_DIR` | The index folder for hooks and `stats`. |
| `SIEVE_NO_REFRESH` | Skips the refresh before a query. |
| `SIEVE_REFRESH=hash` | Forces a hash check in the refresh. |
| `SIEVE_NO_GITIGNORE` | Stops `build` from editing `.gitignore`. |
| `SIEVE_NO_IGNORE` | Stops `build` from writing `.ignore`. |
| `SIEVE_NO_STATUSLINE` | Stops `init` from writing the status line. |

## `.gitignore` and `.ignore`

By default, `build` adds `sieve/` to `.gitignore`. It also writes a `.ignore`
block so that ripgrep still searches `sieve/`. `--no-gitignore` and
`--no-ignore` skip these edits.

Sieve has no LLM pass. No setting needs an API key.
