# ADR 0003: Deny a second read of an unchanged file

- **Status:** Accepted
- **Date:** 2026-10-05

## Context

An agent often reads the same file twice in one session. The second read adds the same tokens to the context and adds no new fact. This feature is called remembered reads.

## Decision

The pre-read hook records a hash of each file the Read tool reads in a session. If the agent reads the same file again and the hash is the same, the hook denies the read. The hook gives a short note that says the file is unchanged. Sieve denies every second read of an unchanged file. The read after a deny passes. A changed file always passes.

## Consequences

- The agent saves the tokens of the repeat read.
- The saving counts in the session total.
- The deny note reads: "a.ts is unchanged since your last read. Use that copy. If you no longer hold it, read again."
- Remembered reads cover the Read tool only. Bash reads are not covered.
- If the session record cannot be written, the read passes.

- **Code:** `crates/sieve-cli/src/hook.rs#handle_pre_read`
