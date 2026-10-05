# ADR 0004: Each query result opens with one savings line

- **Status:** Accepted
- **Date:** 2026-10-05

## Context

A user needs to see what a query saved. A long report or an instruction text costs tokens itself and can steer the agent.

## Decision

Each query result opens with one line: `[sieve] saved ≈ N tokens`. Sieve computes N as the size of the whole files the answer names, minus the size of the answer. Both sizes convert to tokens first. If the answer is not smaller, or no file size is known, Sieve prints no line.

## Consequences

- The line is an estimate. It is not a measured saving in an agent session.
- The line has no percent and no instruction text.
- The hook reads the number after the `≈` sign to add it to the session total.
- The statusline and the pane show the total.

- **Code:** `crates/sieve-savings/src/lib.rs#savings_line`, `crates/sieve-savings/src/lib.rs#sieve_header`
