# sieve — repo map

Small markdown nodes summarising this repo. `grep` any term, symbol, or
filename here, or run `sieve ask "<task>"`. Each node carries prose plus exact
`file:line`; open a source file only to edit the named span.

The same graph is queryable as MCP tools (`sieve_find_code`, `sieve_find_all`,
`sieve_trace_calls`, `sieve_file_api`, `sieve_repo_map`) where a host exposes them, and
as the `sieve` CLI everywhere else. Edges — who calls what — live only in the
graph, not in these files: `sieve callers <symbol>` is the only way to read them.

## Concepts

- [application-module](application-module.md) — Application Module · src/app.ts
- [basic-arithmetic-helpers](basic-arithmetic-helpers.md) — Basic Arithmetic Helpers · py/helpers.py
- [calculator-application](calculator-application.md) — Calculator Application · py/main.py
- [greeting-component](greeting-component.md) — Greeting Component · src/view.tsx
- [legacy-greeting-functionality](legacy-greeting-functionality.md) — Legacy Greeting Functionality · src/legacy.mjs
- [shared-versioning-utility](shared-versioning-utility.md) — Shared Versioning Utility · packages/shared/index.ts
- [utility-functions](utility-functions.md) — Utility Functions · src/util.ts

## Files

9 per-file wiring cards mirror the source tree under `sieve/` (9 carry extracted symbols). They are deliberately not enumerated here —
`grep` a symbol or `find`/`ls` a filename under `sieve/` to land on the card for that file.
