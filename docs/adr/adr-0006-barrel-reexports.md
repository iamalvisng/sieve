# ADR 0006: Exact cross-file links through barrel re-exports

- **Status:** Accepted
- **Date:** 2026-10-07
- **Amends:** ADR 0005 (the line "Barrels stay uncovered")

## Context

ADR 0005 leaves barrel re-exports uncovered. The oracle follows `export ... from`. Sieve does not. All uncovered oracle pairs on vite and flowfig are barrel pairs. The design is in `docs/design/xfile-slice3.md`.

## Decision

Sieve follows `export { x } from`, `export * from` and `export * as ns from` to the defining file. The rules continue ADR 0005 (B1 to B9). E is the export name.

- B10: A walk that ends at exactly 1 Function gives an Extracted edge, at any depth.
- B11: 2 or more Functions at the defining file keep the old result.
- B12: An absent name, a cycle, the limit (8 links, 256 lookups), an unresolved link, a star conflict and an unresolved star with 1 hit give no result. A plain named import then keeps the old result (B8). An alias, default or namespace import drops (B9).
- B13: A member call `r.fn()` on a named import that is a namespace export follows the same walk. Any result other than 1 Function keeps the old result.
- B14: A local export beats a re-export of the same name, as in ES. An explicit `export { } from` beats `export *`. `export *` never carries default.
- B15: A star conflict gives no Extracted edge. TS keeps the first star (TS2308). ES excludes the name. A missing edge is safe. A wrong edge is not.

## Gate

The gate stays 0 wrong edges by count 2 on flowfig and on vite. Coverage is reported per shape and per barrel flag. The oracle gains the shape `nsexport` for form 4.

## Consequences

- Barrel pairs become Extracted, or stay as before.
- `CACHE_VERSION` moves from 4 to 5. The first build is cold.
- A re-export record gives no `imports` graph edge.
- The sha256 of both oracle scripts goes into the ledger.
- Measured on 2026-10-07: vite 10033218 gives 1,272 of 1,272 edges right (was 1,246), 0 wrong, barrel pairs 24 of 24 (was 0 of 24). flowfig 402b7d5 gives barrel pairs 11 of 11 (was 0 of 11).

- **Code:** `crates/sieve-parse/src/resolve.rs`, `crates/sieve-parse/src/extract.rs`
