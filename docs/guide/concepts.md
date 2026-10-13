# Concepts

## The index

`sieve build` reads your code and writes an index in the `sieve/` folder
at the repo root. The index is a graph. Each node is a file or a symbol,
such as a function, method, class, interface, type or enum. Each edge links
two nodes.

Edge kinds are call, reference, import, implements and extends.

The index is a cache. Sieve adds `sieve/` to `.gitignore`. A teammate runs
`sieve build` to get their own copy.

## Spans

Every answer names a symbol and an exact `file:line` span, such as
`411-679`. You can open the span at once.

## Hubs and links in

A link in is a link that points at a symbol. A hub is a symbol with many
links in. `sieve map` lists hubs for each directory.

## Callers and blast radius

A caller is a symbol that calls or references another symbol. The blast
radius of a change is the set of symbols that depend on the changed lines.
`--depth` sets how many hops Sieve walks over the edges.

## Exact links

An Extracted link is a call that Sieve proves has one target. An Inferred
link is a call that Sieve matches by name or by a weaker rule. For TypeScript,
JavaScript, Rust and Python, Sieve marks a call Extracted only after it follows
the scopes, the imports and the module tree to exactly one definition. If it
finds none or two, the link stays Inferred. An outside oracle checks the proofs:
the TypeScript compiler, rust-analyzer and scip-python. A wrong Extracted link
fails the check. See [Benchmarks](benchmarks.md#exact-call-links-checked-by-an-oracle).

## Fresh index

Query commands refresh the index before they answer, so the answer
includes uncommitted edits. `--no-refresh` skips the refresh.
`sieve check` reports drift between the index and the code.

## Token savings

Sieve prints a line such as `[sieve] saved ≈ 30,366 tokens` after a query.
The number is an estimate. It is the size of the whole files the answer
names, minus the size of the answer. Sieve converts both sizes to tokens.
It is not a measured saving in an agent session.
`sieve stats` shows the savings for the session, the day and 7 days.

## Folders

`sieve/` is the index folder. `.sieve/` holds config.

## Decision records

`sieve why` links a symbol to the decision records that explain it.
See [Commands](commands.md#why).

## Local only

Sieve runs on your machine and sends nothing. See [Privacy](privacy.md).
