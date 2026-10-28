# Rename

`sieve rename <symbol> <new-name>` renames a top-level function across files.
It works on TypeScript and JavaScript. Sieve proves each edit site first. If
Sieve cannot prove one site, it refuses the whole rename. It never makes a
partial plan.

## Use it

The command is a dry run by default. It prints a plan and a diff and changes no
file.

```
$ sieve rename parseArgs parseCliArgs
rename plan 3f9a1c2e: src/args.ts#parseArgs (function) -> parseCliArgs
4 files, 7 edits. Dry run: no file changed.
Write it: sieve rename parseArgs parseCliArgs --write --expect 3f9a1c2e

--- a/src/args.ts
+++ b/src/args.ts
@@ -12 +12 @@
-export function parseArgs(argv: string[]): Args {
+export function parseCliArgs(argv: string[]): Args {
...
```

To write the files, repeat the command with `--write` and `--expect <plan-id>`.
Sieve computes the plan again. It writes only when the new plan id equals the id
you give. If a file changed since the dry run, the id changes and Sieve writes
nothing.

```
$ sieve rename parseArgs parseCliArgs --write --expect 3f9a1c2e
renamed src/args.ts#parseArgs -> parseCliArgs: 4 files, 7 edits. Graph refreshed. Check passed.
```

## The safety rule

- Sieve edits a site only when it proves that the site means the target.
- If Sieve cannot prove one site, it refuses the whole rename. It names the
  file, the line, the column and the reason.
- Sieve lists hits in comments, strings and docs as "not edited". It never edits
  them. The exception is a JSDoc `@link`, `@see` or type site that Sieve can bind
  to the target.
- After a write, Sieve checks its own result. If the check fails, Sieve restores
  every file and says `rolled back`.
- Sieve writes a journal before it writes files. After a crash, you can restore
  the old files.

A refusal looks like this:

```
$ sieve rename resolve resolvePath
rename refused: src/path.ts#resolve (function) -> resolvePath
Sieve proved 12 sites and cannot prove 3. No file changed.
  src/load.ts:44:18     dynamic_import  import("./path") reaches the target module
  playground/x.ts:3:10  unproven        import { resolve } from "vite": "vite" is a package in this repo
  src/path.ts:80:7      collision       resolvePath is already declared in this module
```

## Flags

| Flag | Meaning |
|---|---|
| `--write` | Write the files. Sieve computes the plan again first. |
| `--expect <plan-id>` | With `--write`, write only when the new plan id equals this id. |
| `--json` | Print the plan as JSON. |
| `--context <n>` | Context lines in the diff. The default is 0. |
| `--in <path>` | Narrow the choice of symbol. It does not narrow the scan. |
| `--dir <path>` | The index folder (the top-level `--dir` option). Run the command from the repo root. |
| `--recover` | After a crash, write the old bytes back from the journal. |
| `--discard-journal` | After a crash, delete the journal and keep the files as they are. |

The symbol is a bare name, a node id such as `src/args.ts#parseArgs`, or
`path:line`. If two or more nodes match, Sieve lists the candidates.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | The plan is ready, or Sieve wrote it. |
| 1 | A usage error or an I/O error. |
| 2 | Sieve refused the rename. |

## Limits

- Sieve renames top-level functions in TypeScript and JavaScript only. This
  includes `function f` and `const f = () => ...`.
- Sieve refuses a default export, a class, a method, a namespace import
  (`import * as ns`) and a star barrel (`export * from`) that reaches the
  target module.
- Sieve refuses a dynamic `import()` or `require()` that it cannot trace when
  the repo also reads the old name as a property, a key or a string. If the
  repo holds no such use, the dynamic access cannot reach the function by name,
  and Sieve does not refuse.
- Sieve refuses when a nested scope declares the old name or the new name in a
  file that has a hit.
- Sieve refuses a package import of a package that the repo itself holds, a
  merged declaration and a symlinked path.
- Code that reads export names as data (for example `Object.keys(mod)`), or
  builds the name from parts, sees the new name. The TypeScript rename does the
  same.
- Sieve refuses many cases. On the benchmark repos it refused 138 of 300 cases
  on vite and 17 of 300 on execa. A refusal is the safe result.

See [Benchmarks](benchmarks.md) for the other measured claims and the
[README](../../README.md#proof) for the rename proof.
