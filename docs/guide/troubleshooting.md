# Troubleshooting

## `sieve: command not found`

The installer puts `sieve` in `$CARGO_HOME/bin`. Add that folder to your
`PATH`. Agent hooks also find `sieve` by `PATH`. If it is missing, a hook fails
and the agent shows nothing. `sieve init` prints a warning in that case.

## "No index here yet"

A query does not build the index. If the folder has no index, Sieve prints
"No index here yet. Run `sieve build .` first." Run that command.

## A query returns old results

Query commands refresh the index first. If you passed `--no-refresh` or set
`SIEVE_NO_REFRESH`, remove them. To see the drift, run `sieve check`.
To rebuild, run `sieve build`. Add `--no-reuse` to parse every file again.

## A file is missing from the results

- Sieve indexes only [supported extensions](languages.md).
- `sieve blast` reports a changed file that no parser claims, or a file that the
  index does not hold yet.
- A folder that the walk skips by default needs `--include-dir <name>`.
- A nested Git clone needs `--follow-nested-repos`. A submodule needs `--follow-submodules`.

## The agent does not use Sieve

- Restart the agent after `sieve init`. The MCP server loads at start.
- Check that `.mcp.json` has a `sieve` entry.
- Run `sieve init --dry-run` to see what `init` would write.
- Codex needs `~/.codex` and `sieve init --global`.

## `init` left a file unchanged

If `init` reports "not valid JSON", the file is not a JSON object. Sieve
leaves it as it is. Fix the file, then run `init` again, or add the
`sieve` server by hand. See [MCP](mcp.md).

## My status line is not Sieve's

`init` keeps a status line that you set. To use Sieve's, point `statusLine` at
the command that the warning names.

## `sieve stats` says no session recorded

Stats read session files from agent sessions. Use Sieve in an agent session,
then run `sieve stats` again. Stats look in `sieve/` or `SIEVE_DIR`.

## A read is denied

The second read of an unchanged file gets this note: "[sieve] a.ts is
unchanged since your last read. Use that copy. If you no longer hold it, read
again." The first read has the content. If you no longer hold it, read the
file again. The third read passes.

## The pane or mascot does not change

`/sieve-mascot` saves to `.sieve/config.json`. If that file is not valid
JSON, Sieve does not save. Fix the file.

## Report a bug

Open an issue at github.com/iamalvisng/sieve. For a security problem, use the
private report. See [Privacy](privacy.md).
