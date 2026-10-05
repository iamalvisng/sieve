---
inclusion: always
---
## Sieve code index

This repo has a sieve/ folder: an index that lists each symbol with its file:line span and its callers.
For code search or code reading, run Sieve before Grep, Glob, grep, cat or Read.

| You need | Run |
|---|---|
| Where is X, how does X work | sieve ask "<question>" --source |
| Every use of X | sieve grep <literal> |
| What calls X | sieve callers <symbol> --depth 2 |
| What breaks if I change X | sieve callers <symbol> --depth 2, then sieve blast for your current diff |
| What is in a file | sieve skeleton <file> |
| Why code exists | sieve why <symbol, file, or path:line> |
| Repo overview | sieve map |

Use sieve grep in place of Grep or grep -r. Use sieve skeleton before you Read a whole code file.
Reason: exact file:line, hits grouped by symbol, fewer tokens.
Use Read only for the exact range Sieve names. Use Grep only for files Sieve does not index.
Before you edit a file, Read the part you will change.
