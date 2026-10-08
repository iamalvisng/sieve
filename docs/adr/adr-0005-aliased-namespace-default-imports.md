# ADR 0005: Exact cross-file links for aliased, namespace and default imports

- **Status:** Accepted
- **Date:** 2026-10-06
- **Supersedes:** ADR 0002

## Context

ADR 0002 limits Extracted cross-file links to named relative imports. Aliased, namespace and default imports stay Inferred. An alias call `b()` can name-match a wrong symbol `b`. A namespace call `ns.fn()` gives no edge. The design is in `docs/design/xfile-slice2.md`.

## Decision

Sieve marks a call Extracted for three more import shapes. The shapes are `import { a as b }`, `import * as ns` with `ns.fn()`, and `import x` (default). The resolver follows these rules.

- B1: A bare call whose local name is an import binding, and no shadow holds the name, carries the specifier and the export name. The export name for a default binding is "default".
- B2: A call `ns.fn()` with a namespace binding carries the export name `fn`. `ns.a.fn()` and `ns["fn"]()` get nothing.
- B3: A Node built-in specifier drops the edge, for member calls too.
- B4: A default export name means the target file's default-exported Function.
- B5: Any other export name means a directly exported Function of that name.
- B6: Exactly 1 candidate gives an Extracted edge.
- B7: 2 or more candidates keep the old result.
- B8: A plain named import with 0 candidates keeps the old result.
- B9: The drop rule. An alias, default or namespace import with 0 candidates, whose specifier is a repo file, drops the edge. The local name is free, so a name match is meaningless.

A parameter, local, destructured name, loop binding, catch binding or named function expression shadows the import.

## Gate

The gate uses the span-matched oracle (count 2). An oracle row matches a Sieve edge only if the row line is inside the span of the edge source node. The target file and name must also match. A Sieve edge with no match is wrong. The gate is 0 wrong edges on flowfig and on vite. Coverage is reported per shape, with no threshold. Barrels resolve since ADR 0006 (2026-10-07).

## Consequences

- Some calls that were Inferred become Extracted. Some wrong Inferred edges disappear (rule B9).
- `scripts/oracle/` holds the oracle. Its sha256 goes into the ledger.
- The commit that lands the resolver sets ADR 0002 to "Superseded by" this ADR, and this ADR to "Accepted".

- **Code:** `crates/sieve-parse/src/resolve.rs#resolve_calls`, `crates/sieve-parse/src/extract.rs`
