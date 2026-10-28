# Rename gate

This script checks `sieve rename` against the TypeScript language service on one repo.
Run it with one command: `scripts/oracle/rename-gate/run.sh <repo-dir>`. Add `--sample N` to cut the plain set to N functions.
It needs node, npm, cargo and git. It builds Sieve release from this checkout, and runs `npm ci` once in `scripts/oracle`.
It builds the index into `<repo>/sieve`, makes the case lists with seed 20261026, and freezes them with a sha256 before it measures.
It runs the oracle and a `sieve rename --json` dry run for each case. It never passes `--write`.
It never edits your source files. It checks `git status` before and after, and fails loudly if a dry run changed anything.
It uses a scratch HOME and refuses to run if that HOME is your real one.
Expect a few minutes for the build, then 10 to 30 minutes for a large repo. Each plan takes about 1 to 3 s.
All files go to one folder under `$TMPDIR`. The script prints its path and a summary block.
Paste back the block between `RENAME GATE SUMMARY` and `END SUMMARY`. It is also in `summary.txt`.
