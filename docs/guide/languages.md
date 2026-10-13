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

The exact-link tier now covers TypeScript, JavaScript, Rust and Python. Sieve
marks a Rust or Python call Extracted only when it proves one target. See
[Concepts](concepts.md#exact-links) and ADR 0007.

For Rust, these calls stay Inferred or get no edge: a method call `x.f()`,
`self.f()`, a trait method, `<T as Trait>::f()`, a call to a trait-impl
function through a type, a call through a type alias, a call inside a macro
argument, a macro call, and code made by `include!` or a procedural macro.
A name with a `cfg` attribute on two definitions stays Inferred.

For Python, these calls stay Inferred or get no edge: a method call `x.f()`
on an unknown object, `self.f()`, `getattr`, `importlib`, a call to a function
with a decorator that can replace it, a name that the code binds more than once,
a module that uses `globals()`, `exec` or a module `__getattr__`, and a call to a
patched module attribute. A name from `from m import *` is exact only when the
star import has one clear result.

Sieve does not link these TypeScript and JavaScript cases as exact: non-relative package specifiers,
`paths` aliases, CommonJS, `export =`, and JavaScript barrel files for re-export
records. Do not treat these links as exact.

## Unsupported files

Sieve ignores a file with an extension that has no parser. `build -e`
warns and lists the supported extensions. `sieve blast` reports a changed file
that no parser claims.
