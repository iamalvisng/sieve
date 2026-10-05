---
name: sieve
description: Use for any code-understanding or code-search task in a repo that has a sieve/ folder. Triggers - find where code lives, explain how code works, find every use of a name, find what calls a symbol, check what a change breaks, list what a file contains, get a repo overview. Run the sieve command before you use Grep, Glob, grep or cat on source files.
---

# sieve

This repo has a `sieve/` folder. It holds an index that lists each symbol with its file and line span, and the calls between symbols.

Use `sieve` for code search and code reading. Use `sieve grep` in place of Grep or `grep -r`. Use `sieve skeleton` before you Read a whole code file.

Reason: every answer gives the exact file:line, groups hits by symbol, and costs fewer tokens than raw search.

## Which command

| You need | Run |
|---|---|
| Where is X, or how does X work | `sieve ask "<question>" --source` |
| Every use of X | `sieve grep <literal>` |
| What calls X | `sieve callers <symbol> --depth 2` |
| What breaks if I change X | `sieve callers <symbol> --depth 2`, then `sieve blast` for your current diff |
| What is in this file | `sieve skeleton <file>` |
| Why does this code exist | `sieve why <symbol, file, or path:line>` |
| Overview of an unknown repo | `sieve map` |

Run one command, then act on the answer. Do not ask the same question again in other words.

## Commands in detail

- `sieve ask "<question>" --source` returns ranked hits with the code inline. Each hit shows a short excerpt. Add `--full` for the whole definition. Add `--in <path>` to limit the search to one folder.
- `sieve grep <literal>` finds every match in indexed files and groups the matches by symbol. Add `-i` for any case. Add `--in <path>` to limit the search. Use a short name, not a long guessed signature. If it finds nothing, shorten the pattern and run it again.
- `sieve callers <symbol>` shows who calls the symbol. Add `--direction out` to see what the symbol calls. Add `--depth 2` for indirect callers. Add `--depth all` before a rename or a change in many files.
- `sieve blast` shows what depends on the lines in your current diff. Add `--base <ref>` to compare with another ref.
- `sieve skeleton <file>` lists every signature in one file, with line spans. Run it once per file.
- `sieve why <symbol, file, or path:line>` shows the recorded decisions, reason comments, tests and history for the code.
- `sieve map` shows folders, hub files and hotspots.

## When to use another tool

- Use Read when you need the full text of a file you will edit. Use the file:line from Sieve to open only that range.
- Use Grep for files Sieve does not index: docs, config files, new files.
- Sieve refreshes its index before each query. It reflects edits you have not committed.
- Do not pipe a `sieve` command through `head` or `tail`. The output is already limited.

If the MCP server is connected, the same tools exist as `sieve_find_code`, `sieve_find_all`, `sieve_trace_calls`, `sieve_file_api`, `sieve_repo_map`, `sieve_why` and `sieve_check_freshness`. The advice is the same.
