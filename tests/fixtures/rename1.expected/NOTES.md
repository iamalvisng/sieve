# rename1 oracle notes

Recorded 2026-10-26 with `typescript` 5.9.3 (RN0b, `docs/design/rename.md`
section F). Source: `oracle.jsonl` in this folder, made by
`scripts/oracle/ts-rename.mjs tests/fixtures/rename1 tests/fixtures/rename1/cases.jsonl`.

The first row of `oracle.jsonl` freezes the program set: 27 files, the
options, and no `paths` or `baseUrl` (the fixture has no tsconfig).
An offset is a UTF-8 byte offset in the decoded text of the file (BOM
excluded). For `utf16.ts` this is not a disk offset.

## Default export

Cases `default_decl`, `default_import`, `default_alias_import`,
`default_keyword` (`src/dflt.ts`, `src/dflt_user.ts`).

- Rename at the declaration name `dflt`, or at the default importer's local
  name `dflt`: TS gives 3 locations. They are the declaration name
  (`dflt.ts:24`) and the importer's local binding (`dflt_user.ts:7`) with its
  use (`dflt_user.ts:75`).
- TS does not touch `import renamed from "./dflt"`. A default import is a
  local name, so the second importer keeps its name.
- Rename at the `default` keyword (`export default function`): `canRename` is
  true and TS gives the same 3 locations as the name `dflt`.
- Rename at the alias importer's local name `renamed`: 2 locations, both in
  that importer (`dflt_user.ts:34`, `:85`).
- So TS renames the default-import local name in importers that use the same
  name, and leaves other default importers alone. No row has prefix or suffix
  text. The public module shape does not change, because the export is
  `default`. A name-based text match in Sieve would differ from this.
  Slice 1 refuses default targets (`target_kind`).

## Shorthand property

Cases `short_import`, `short_prop` (`src/short.ts`: `{ greet }`).

- With `providePrefixAndSuffixTextForRename: false`, TS returns the shorthand
  site as one plain location (`short.ts:55`, no prefix, no suffix). The edit
  turns `{ greet }` into `{ greetRn }`. That changes the object key, so the
  program changes meaning.
- With the option `true`, TS would give `greet: ` as prefix text. The oracle
  does not use it. The design (G4, S10) refuses shorthand sites in slice 1, so
  a case with this site shows as Refused, never Wrong.
- Contrast: `(globalThis as any).greet = greet` (`src/gt.ts`). TS renames only
  the right side (`gt.ts:60`) and the import (`gt.ts:9`). The key `.greet` is
  kept. Rename at the key itself gives `canRename: false` (case
  `globalthis_key`, excluded).

## Other findings

- JSDoc in a TS file: `{@link greet}` and `@see greet` in `def.ts` and
  `named.ts` are listed (tag `jsdoc_link`). The `{typeof greet}` type of
  `@param` in `named.ts:148` is also listed (tag `jsdoc_type`), even in a
  `.ts` file. S11 makes such a hit Unproven in Sieve, so it must refuse.
- JSDoc in a JS file (`jsdoc.js`): `@type {typeof greet}` (`jsdoc.js:50`) and
  `{@link greet}` (`jsdoc.js:101`) are listed.
- Comment and string hits are listed only when the oracle passes
  `findInStrings` and `findInComments` as true. The plain mode passes false,
  so the line comment and the strings `"greet"` and `"say greet now"` in
  `named.ts` have no row there. A case with `"strings": true` in
  `cases.jsonl` passes true. Case `strings_comments` does this: it adds rows
  with tag `comment` (`def.ts:28`, `named.ts:35`) and tag `string`
  (`named.ts:72`, `:99`), and the same under the `linked.ts` link. The
  comparer treats these rows as report sites. A plan must not edit them.
  Plain words inside a JSDoc comment also get tag `comment`.
- Multi-byte text: `utf8.ts` has `// café ✓`, non-ASCII strings and an
  astral emoji on the same lines as the sites. Offsets come from a per-file
  UTF-16 to UTF-8 table. A check script confirmed that every non-UTF-16 row of
  `oracle.jsonl` (757 rows) spans exactly one identifier word in the file bytes.
- Alias importer `import { greet as hello }`: only the specifier name
  (`alias.ts:9`) is a location. `hello` and its uses stay.
- Named barrel `export { greet } from "./def"`: the specifier is a location
  (`barrel_named.ts:9`), and so are the importers of the barrel.
- Namespace member `d.greet`, star barrel and `export * as` barrel members are
  all locations (`nsuser.ts:48`, `star_user.ts:57`, `ns_user.ts:61`). Slice 1
  refuses these.
- Local-declared `function loc(){}; export { loc }` with an importer: 5
  locations: declaration, export clause, same-file use, importer specifier and
  importer use. TS renames the public name here, with no `as`.
- Merged `function mrg` plus `namespace mrg`: both names are locations
  (`merged.ts:16`, `:64`).
- Decoy module with the same name (`decoy.ts`): not touched by the `greet`
  rename of `def.ts`. A rename of the decoy touches only the decoy and its
  importer.
- Nested shadow (`shadow.ts`): the parameter `greet` and its use are not
  locations of the `def.ts` rename. Neither is the `let greet` in the nested
  block. Rename of the parameter gives 2 locations, and rename of the block
  `let greet` gives 2 locations (case `shadow_block_let`).
- JSX: `<Widget />` and `<Widget></Widget>` give 3 sites in `app.tsx`, plus the
  import (`app.tsx:9`) and the declaration (`widget.tsx:16`).
- UTF-16 importer (`utf16.ts`, BOM and CRLF): TS reads it and lists both sites
  (`utf16.ts:9`, `:49`). Offsets are in decoded text, so they are not disk
  offsets.
- Symlinked file `src/linked.ts -> named.ts`: the walk includes the link as a
  file, and TS treats it as a second file. Every `named.ts` location appears
  again under `linked.ts`. Sieve must refuse by S13. Git holds no symlink.
  Before you run `ts-rename.mjs`, run
  `ln -s named.ts tests/fixtures/rename1/src/linked.ts`. The Rust test
  `test_rn2_matches_frozen_oracle` creates the link in its temp copy.
- `import.meta.glob(...)` in plain form (`glob.ts`): TS lists nothing for the `glob` name
  (`canRename: false`, case `glob_name`, excluded). It is a fixture for the
  Sieve S7 refusal, not an oracle case.
- Excluded rows: `globalthis_key` and `glob_name`. The script also writes an
  `excluded: "no_locations"` row for a case where TS gives no location.
- Each case has an `expect` field in `cases.jsonl`. It is the planned Sieve
  result, for the reader. The oracle script ignores it.
