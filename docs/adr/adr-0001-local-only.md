# ADR 0001: Sieve runs local only

- **Status:** Accepted
- **Date:** 2026-10-05

## Context

A code-intelligence tool reads private source. A network call or a usage report can leak that source or its paths. Users must trust the tool with no review of its traffic.

## Decision

Sieve runs on the local machine only. A query makes no network call. Sieve sends no telemetry. The hidden background commands for an update check and a telemetry flush do nothing.

## Consequences

- Sieve never checks for a new version. The user updates by hand.
- The `telemetry` command reports "off" and records nothing.
- Sieve cannot show usage numbers from other users. The `stats` command reads local session files only.
- A feature that needs the network must be opt-in and must have a new ADR first.
- SECURITY.md states this rule to the user.

- **Code:** `crates/sieve-cli/src/internal.rs#telemetry_flush`, `crates/sieve-cli/src/telemetry.rs#run`
