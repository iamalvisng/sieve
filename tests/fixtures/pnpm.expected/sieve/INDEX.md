# sieve — repo map

Small markdown nodes summarising this repo. `grep` any term, symbol, or
filename here, or run `sieve query "<task>"`. Each node carries prose plus exact
`file:line`; open a source file only to edit the named span.

The same graph is queryable as MCP tools (`sieve_query`, `sieve_find`,
`sieve_callers`, `sieve_symbols`, `sieve_map`) where a host exposes them, and
as the `sieve` CLI everywhere else. Edges — who calls what — live only in the
graph, not in these files: `sieve callers <symbol>` is the only way to read them.

## Files

4 per-file wiring cards mirror the source tree under `sieve/` (4 carry extracted symbols). They are deliberately not enumerated here —
`grep` a symbol or `find`/`ls` a filename under `sieve/` to land on the card for that file.
