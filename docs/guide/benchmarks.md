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
3. `ask` latency: 10 questions per repo, each run 3 times. That gives 30 timings. The number is the wall time of one `sieve ask` process. Report the median and the maximum.
4. Answer size: count the bytes of `sieve ask "<question>" --source`. Count the bytes of every distinct file the output names. Compare the two totals. This is not tokens. It is not an agent session.
5. Exact cross-file calls: an oracle script lists the call rows that the TypeScript compiler resolves in vite. A compare script checks each Extracted cross-file call edge. An edge matches if the compiler has a call in the same source file. The call must resolve to the same target file and the same name. Sieve found 1,232 such edges. All 1,232 match. The 621 calls that stay Inferred are not counted.
6. The questions were written before the first run. No question was dropped. Medians move by a few ms because the times are 25 to 250 ms.

## Results

| Number | ripgrep | vite |
|---|---|---|
| Cold build | 0.73 s | 1.83 s |
| Warm build | 0.31 s | 0.80 s |
| `ask` median, max | 62 ms, 101 ms | 158 ms, 185 ms |
| Answer size against file size | 60,093 B against 3,068,726 B | 67,162 B against 806,930 B |

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

## Limits

- Sieve has not measured token savings in an agent session.
- The runs used one machine. Other machines give other times.
