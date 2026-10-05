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

Sieve links imports across files with exact results only for named relative
imports in TypeScript and JavaScript. Do not treat other cross-file links as exact.

## Unsupported files

Sieve ignores a file with an extension that has no parser. `build -e`
warns and lists the supported extensions. `sieve blast` reports a changed file
that no parser claims.
