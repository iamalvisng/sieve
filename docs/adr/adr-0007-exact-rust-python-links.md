# ADR 0007: Exact Rust and Python call links by proof

- **Status:** Accepted
- **Date:** 2026-10-13
- **Amends:** the earlier deferral of full Rust call edges (2026-10-04)

## Context

The old same-file name rule marked a call Extracted when one function in the file had the call's name. In Rust and Python this gave wrong edges. Before the change, Sieve had 254 wrong of 4,622 Rust Extracted edges, ripgrep 3fce3b5 had 173 of 938, httpx b5addb6 had 1 of 324 and black 11cff13 had 20 of 833.

## Decision

Sieve marks a Rust or Python call Extracted only when a static proof finds exactly 1 target. Any doubt gives Inferred or no edge. The old name path is capped at Inferred for `.rs` and `.py`.

- Rust: the proof walks the module tree, `use` items, workspace crates by name, and `Type::f()` and `Self::f()` calls to 1 inherent function. Rules R0 to R14.
- Python: the proof follows the scopes of the caller, the module bindings, imports, relative imports and `__init__.py` re-exports to 1 definition. Rules PY0 to PY9, with a decorator allowlist.
- An outside oracle checks every Extracted edge: rust-analyzer 0.3.3065 and scip-python 0.6.6.

## Constraint

- A wrong Extracted edge is worse than an Inferred one.
- The default build needs no toolchain and no new crate dependency.
- The goldens for the Rust and Python fixtures change. The golden audit records this change.
- The gate is 0 wrong Extracted edges on each gate repo.

## Consequences

- Some edges that were Extracted are now Inferred.
- `CACHE_VERSION` changes. The first build after the upgrade re-parses every file.
- Measured: Sieve 5,044 Rust Extracted, 0 wrong. ripgrep 3fce3b5 1,209, 0 wrong, 3 unverified (platform `cfg`). httpx b5addb6 367, 0 wrong. black 11cff13 1,161, 0 wrong, 2 unverified.
- Not covered: method calls `x.f()`, `self.f()` in Rust, trait dispatch, and dynamic Python features.

- **Code:** `crates/sieve-parse/src/rust_proof.rs`, `crates/sieve-parse/src/rust_tree.rs`, `crates/sieve-parse/src/rust_items.rs`, `crates/sieve-parse/src/py_proof.rs`, `crates/sieve-parse/src/py_tree.rs`
