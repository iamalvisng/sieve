# Languages

Sieve parses TypeScript, JavaScript, Python, Go, Java, Kotlin, Swift, PHP and R
natively. It parses 14 more with a generic parser, and reads script blocks in
Vue, Svelte and Astro files. Sieve picks a parser from the file extension.
There are three tiers.

## Native tier

| Language | Extensions |
|---|---|
| TypeScript | `.ts`, `.mts`, `.cts`, `.d.ts` |
| TSX | `.tsx` |
| JavaScript | `.js`, `.mjs`, `.cjs` |
| JSX | `.jsx` |
| Python | `.py`, `.pyi` |
| Go | `.go` |
| Java | `.java` |
| Kotlin | `.kt`, `.kts` |
| Swift | `.swift` |
| PHP | `.php` |
| R | `.r` |

## Breadth tier

| Language | Extensions |
|---|---|
| Clojure | `.clj`, `.cljs`, `.cljc`, `.bb` |
| Rust | `.rs` |
| C | `.c`, `.h` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.hh` |
| Ruby | `.rb` |
| C# | `.cs` |
| Scala | `.scala`, `.sc` |
| Elixir | `.ex`, `.exs` |
| Solidity | `.sol` |
| OCaml | `.ml`, `.mli` |
| Zig | `.zig` |
| Dart | `.dart` |
| Nix | `.nix` |
| Lua | `.lua` |

## Container tier

Sieve reads code inside `.vue`, `.svelte` and `.astro` files.

## Exact links

Sieve links imports across files with exact results for relative imports in
TypeScript and JavaScript. These cover named imports, aliased imports
(`{ a as b }`), namespace imports (`* as ns` with `ns.fn()`) and default imports.
They also cover barrel re-exports: `export { x } from`, `export *`, default
re-exports and `export * as ns`. ADR 0005 and ADR 0006 hold the rules.

Sieve does not link these cases as exact: non-relative package specifiers,
`paths` aliases, CommonJS, `export =`, and JavaScript barrel files for re-export
records. Do not treat these links as exact.

## Unsupported files

Sieve ignores a file with an extension that has no parser. `build -e`
warns and lists the supported extensions. `sieve blast` reports a changed file
that no parser claims.
