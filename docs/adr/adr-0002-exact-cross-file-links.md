# ADR 0002: Exact cross-file links only for named relative imports

- **Status:** Superseded by ADR 0005 on 2026-10-06. Named relative imports keep the rule below. ADR 0005 adds aliased, namespace and default imports.
- **Date:** 2026-10-05

## Context

A call across files can resolve to the wrong symbol. A wrong link marked as exact misleads an agent more than no link. A compiler oracle checks each exact link in TypeScript.

## Decision

Sieve marks a call across files as Extracted only when the caller names the callee through a relative TypeScript or JavaScript import. Every other call across files stays Inferred. Aliases, namespace imports and default imports stay Inferred.

## Consequences

- On vite 10033218, Sieve 0.1.0 made 1,232 Extracted cross-file calls. All 1,232 match the TypeScript compiler. The match is by source file, target file and name (a name-level match). The check does not count the 621 calls that stay Inferred.
- Many real calls show as Inferred. A reader treats them as hints, not proof.
- `callers` and `blast` show the confidence of each edge.
- A later ADR can widen the rule. Each new case needs an oracle check first.

- **Code:** `crates/sieve-parse/src/resolve.rs#exact_import_target`, `crates/sieve-parse/src/resolve.rs#resolve_calls`
