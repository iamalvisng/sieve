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

Exact links cover TypeScript, JavaScript, Rust, Python, Go, Java, C# and
Kotlin. Each language has its own scope, listed below. A call that Sieve
cannot prove stays Inferred. Swift, PHP, R and the breadth-tier languages
other than Rust and C# have no exact links.

For TypeScript and JavaScript, Sieve links imports across files with exact
results for relative imports. These cover named imports, aliased imports
(`{ a as b }`), namespace imports (`* as ns` with `ns.fn()`) and default imports.
They also cover barrel re-exports: `export { x } from`, `export *`, default
re-exports and `export * as ns`. ADR 0005 and ADR 0006 hold the rules.

Sieve marks a Rust, Python, Go, Java, C# or Kotlin call Extracted only when it proves one target. See
[Concepts](concepts.md#exact-links) and ADR 0007.

The scope of each language:

- Rust: a free function in the same file or the same crate (through the module tree and `use`), a function in another workspace crate, and `Type::f()` and `Self::f()` calls. A `self.f()` call is exact when the caller sits in a module-level inherent impl and that impl holds one matching `fn f`.
- Python: a name with one binding in the same file, and an import chain to one definition in another file (`import`, `from ... import`, relative imports and `__init__.py` re-exports).
- Go: a call whose scope has one binding in the same file, `pkg.F()` calls through imports and `go.mod`, and receiver types written in the code.
- Java: bare calls, `this` and `super` calls, static calls through imports, typed receivers and `new`.
- C#: bare and `this.` calls, calls on a local, a parameter or a primary constructor parameter, `new T()`, `base.M()`, `Type.M()`, field receivers, and a type reached through a `global using`.
- Kotlin: bare, `this.` and constructor calls across files, and calls on a typed value (`x.f()`), on an object or a companion (`Type.f()`) and `super.f()`.

For Rust, these calls stay Inferred or get no edge: a method call `x.f()`,
a `self.f()` call outside the case above, a trait method, `<T as Trait>::f()`, a call to a trait-impl
function through a type, a call through a type alias, a call inside a macro
argument, a macro call, and code made by `include!` or a procedural macro.
A name with a `cfg` attribute on two definitions stays Inferred.

For Python, these calls stay Inferred or get no edge: a method call `x.f()`
on an unknown object, `self.f()`, `getattr`, `importlib`, a call to a function
with a decorator that can replace it, a name that the code binds more than once,
a module that uses `globals()`, `exec` or a module `__getattr__`, and a call to a
patched module attribute. A name from `from m import *` is exact only when the
star import has one clear result.

For Go, these calls stay Inferred or get no edge: an interface method call,
a call through a func value, and a call to a promoted method.

For Java, these calls stay Inferred or get no edge: a call to an interface or
abstract target, and any call in a repo with a Lombok annotation or a parse error
when a subtype can override the target.

For C#, these calls stay Inferred or get no edge: a call to an interface target,
and an extension method call.
A parse-error file adds text facts only.

Sieve links a Kotlin call `x.f()` on a typed value, a call `Type.f()` on an object or a companion,
and `super.f()` as exact when it proves one target. On ktorm v4.2.1, a repo the rules never saw before the run,
238 of 238 Extracted code links match the compiler and 0 are wrong.

For Kotlin, these calls stay Inferred or get no edge: a call to an interface,
abstract or `expect` target, a call on a call result (`a.b().f()`), a call where
a class override and its base function are both in the chain, a call inside a lambda
that the proof does not cover, a call on an anonymous object, a local function or class,
and any call in a `.kts` file. A repo with generated sources can stop more calls.
A parse-error file adds text facts only.

Sieve does not link these TypeScript and JavaScript cases as exact: non-relative package specifiers,
`paths` aliases, CommonJS, `export =`, and JavaScript barrel files for re-export
records. Do not treat these links as exact.

## Rename

`sieve rename` uses only the TypeScript and JavaScript links. See
[Rename](rename.md).

## Unsupported files

Sieve ignores a file with an extension that has no parser. `build -e`
warns and lists the supported extensions. `sieve blast` reports a changed file
that no parser claims.
