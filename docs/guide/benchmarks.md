# Benchmarks

This page gives the machine, the versions, the repos and the method of each
number in the README. You can repeat each run.

## Setup

- Machine: Apple M1 Pro, 16 GB of memory, macOS 26.5.2.
- Binary: Sieve 0.1.0, a release build of `sieve-cli`.
- The numbers are re-measured on the release commit before each release.
- ripgrep: commit 3fce3b5bb0236da2df6d99672afb8a719642eca7 (111 files).
- vite: commit 10033218d239c927cdc375970b5741cce408e81b (1583 files).
- Each repo is a fresh shallow clone with no `sieve/` folder.

## Method

1. Cold build: run `sieve build .` on 3 fresh copies of the clone, with no `sieve/` folder. Report the median.
2. Warm build: run the same command again with no change. The number is the median of 5 runs.
3. `query` latency: 10 questions per repo, each run 3 times. That gives 30 timings. The number is the wall time of one `sieve query` process. Report the median and the maximum.
4. Answer size: count the bytes of `sieve query "<question>" --source`. Count the bytes of every distinct file the output names. Compare the two totals. This is not tokens. It is not an agent session.
5. Exact cross-file calls: `scripts/oracle/ts-calls.mjs` lists the call rows that the TypeScript compiler resolves in vite. `scripts/oracle/compare.mjs` checks each Extracted cross-file call edge and reports two counts. Count 1 keys on (source file, target file, name). Count 2 is the gate: the oracle row line must fall inside the span of the Sieve edge's source node. Sieve found 1,272 such edges in vite, and all 1,272 match. Barrel re-exports resolve. The build and ask times in items 1 and 2 are from the 0.1.1 release build, not re-measured after this change. Calls that stay Inferred are not counted.
6. The questions were written before the first run. No question was dropped. Medians move by a few ms because the times are 25 to 250 ms.

## Results

| Number | ripgrep | vite |
|---|---|---|
| Cold build | 0.73 s | 1.83 s |
| Warm build | 0.31 s | 0.80 s |
| `ask` median, max | 62 ms, 101 ms | 158 ms, 185 ms |
| Answer size against file size | 60,093 B against 3,068,726 B | 67,162 B against 806,930 B |

The 0.1.2 build, re-measured on 2026-10-08, gives a cold build median of 1.75 s (5 runs, spread 10.3%) and an `ask` median of 150 ms (5 questions, 15 timings). The table keeps the 0.1.1 figures, which used the documented method.

## The 20 questions

ripgrep:

1. how does ripgrep respect gitignore rules while walking directories
2. how are glob patterns compiled into a matcher
3. how does ripgrep decompress files before searching
4. how does the searcher detect binary files
5. how does ripgrep print search results with colors
6. how does ripgrep output results as JSON lines
7. how are command line flags parsed
8. how does the PCRE2 regex matcher work
9. how does ripgrep search files in parallel
10. how does ripgrep handle file type definitions like rust or python

vite:

1. how does vite resolve module imports
2. how does hot module replacement work
3. how does vite load and merge the config file
4. how does the dependency optimizer pre-bundle packages
5. how does the dev server handle requests with middlewares
6. how does vite process CSS and CSS modules
7. how does vite handle static assets and the public directory
8. how does the plugin container run plugin hooks
9. how does vite build for production
10. how does vite preview the production build

## Exact call links checked by an oracle

An oracle is an outside tool that resolves each call with a compiler or a
language server. The check compares every Extracted call link with the oracle
result. A link is wrong when the oracle names a different target. A link is
unverified when the oracle gives no single answer.

| Repo | Version | Extracted | Wrong | Unverified | Oracle |
|---|---|---|---|---|---|
| Sieve | f8a181f | 5,541 | 0 | 0 | rust-analyzer 0.3.3065 |
| ripgrep | 3fce3b5 | 1,547 | 0 | 3 (platform `cfg`) | rust-analyzer 0.3.3065 |
| itertools | v0.15.0 | 178 | 0 | 0 | rust-analyzer 0.3.3065 |
| httpx | b5addb6 | 367 | 0 | 0 | scip-python 0.6.6 |
| black | 11cff13 | 1,161 | 0 | 2 | scip-python 0.6.6 |
| fzf | v0.74.5 | 1,434 | 0 | 72 | scip-go v0.2.7 |
| gin | v1.12.0 | 1,573 | 0 | 0 | scip-go v0.2.7 |
| caddy | v2.11.7 | 4,168 | 0 | 12 | scip-go v0.2.7 |
| jsoup | jsoup-1.23.2 | 2,501 | 0 | 46 | javac tree API, JDK 21.0.12+8 |
| commons-lang | rel/commons-lang-3.21.0 | 5,612 | 0 | 1 | javac tree API, JDK 21.0.12+8 |
| gson | gson-parent-2.14.0 | 3,211 | 0 | 304 | javac tree API, JDK 21.0.12+8 |
| markdig | 1.4.0 | 912 | 0 | 22 | Roslyn, .NET SDK 10.0.401 |
| serilog | v4.4.0 | 150 | 0 | 1 | Roslyn, .NET SDK 10.0.401 |
| CsvHelper | 33.1.0 | 2,189 | 0 | 23 | Roslyn, .NET SDK 10.0.401 |
| mockito-kotlin | v6.4.0 | 122 | 0 | 0 | kotlinc 2.4.21 (K2) |
| kotlinpoet | 2.4.0 | 704 | 0 | 25 (file not compiled) | kotlinc 2.4.21 (K2) |
| hoplite | v3.0.3 | 385 | 0 | 0 | kotlinc 2.4.21 (K2) |
| ktorm | v4.2.1 | 238 | 0 | 0 | kotlinc 2.4.21 (K2) |

Before the fix, the same-file name rule gave wrong Extracted links:

| Repo | Wrong | Extracted |
|---|---|---|
| Sieve | 254 | 4,622 |
| ripgrep 3fce3b5 | 173 | 938 |
| httpx b5addb6 | 1 | 324 |
| black 11cff13 | 20 | 833 |
| jsoup jsoup-1.23.2 | 140 | 1,522 |
| commons-lang rel/commons-lang-3.21.0 | 829 | 4,689 |
| gson gson-parent-2.14.0 | 23 | 1,543 |
| markdig 1.4.0 | 6 | 528 |
| serilog v4.4.0 | 7 | 264 |
| CsvHelper 33.1.0 | 1 | 188 |
| mockito-kotlin v6.4.0 | 4 | 17 |
| kotlinpoet 2.4.0 | 6 | 135 |
| hoplite v3.0.3 | 5 | 48 |

The oracle scripts are in `scripts/oracle/`. `rs-calls.py` and
`rs-compare.py` check Rust. `py-calls.py` and `py-compare.py` check Python. `go-calls.py` and `go-compare.py` check Go. `JvCalls.java` and `jv-compare.py` check Java. `CsCalls` and `cs-compare.py` check C#. `KtCalls.kt`, `kt-calls.sh` and `kt-compare.py` check Kotlin.
`ts-calls.mjs` and `compare.mjs` check TypeScript.

## Refresh after an edit

Sieve refreshes the index before a query. This table shows the time of a 1-file refresh in seconds. The numbers are the median of 5 on an Apple M-series machine with a release build. The time is the time of `sieve query` minus the time of the same query with `--no-refresh`. Output is byte-identical to a full build on all 6 repos. A refresh with no change takes 0.03 to 0.17 s, unchanged. `SIEVE_TIMING=1` prints each phase to stderr.

| Repo | Files | Before | After |
|---|---|---|---|
| caddy v2.11.7 | 362 Go | 0.76 | 0.18 |
| commons-lang 3.21.0 | 629 Java | 1.46 | 0.62 |
| vite (the benchmarks commit) | 578 .ts of 1,583 | 0.85 | 0.34 |
| Sieve source | 193 .rs | 0.91 | 0.31 |
| gson 2.14.0 (held out) | 262 .java | 0.498 | 0.239 |
| fzf v0.74.5 (held out) | 90 .go | 3.073 | 0.250 |

The fzf "Before" time comes from a development build with a Go indexing cost that no release had. Against that build with only the Go fix, the refresh change alone takes fzf from 0.487 s to 0.250 s.

## Limits

- Sieve has not measured token savings in an agent session.
- The runs used one machine. Other machines give other times.
