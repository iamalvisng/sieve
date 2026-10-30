// Oracle for rename. Usage: node ts-rename.mjs <repo> <cases.jsonl>
// The TypeScript LanguageService lists the rename locations of each case.
// Runs the TypeScript rename oracle.
//
// Case row (one JSON per line): {case, path, line, name, occ?}
//   path is relative to <repo>. line is 1-based. name is the identifier.
//   occ is the 0-based index of the whole-word match of name on that line
//   (default 0). strings:true sets findInStrings and findInComments, so TS
//   lists string and comment hits too (tags string, comment). Default false.
//   Other fields (new, expect) are ignored.
//
// Output rows, one JSON per line:
//   {program: {options, files}}     first row: the frozen program set
//   {case, path, start, end, prefix, suffix, tag}   one per rename location
//   {case, excluded: "<why>", message}              TS cannot rename the case
// start and end are UTF-8 byte offsets in the decoded text of the file
// (BOM excluded). For a file that is plain UTF-8 they equal the disk offsets.
// prefix and suffix are null or the text that TS wants to add.
// tag is code, string, comment, jsdoc_link or jsdoc_type.
// Rows sort by case order in the cases file, then path, start.
//
// Program set. Same walk, same file filter and same base options as
// ts-calls.mjs, plus checkJs false. The root tsconfig.json supplies only
// paths and baseUrl. The script reads no other tsconfig.
import fs from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";

const ts = createRequire(import.meta.url)("typescript");
const root = path.resolve(process.argv[2] ?? ".");
const casesFile = process.argv[3];
if (!casesFile) {
  console.error("usage: node ts-rename.mjs <repo> <cases.jsonl>");
  process.exit(1);
}
// Same list as ts-calls.mjs.
const SKIP = new Set(["node_modules", "dist", "build", "_build", "out", "target",
  "vendor", "coverage", "__pycache__", "venv"]);
const EXT = /\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/;

function walk(dir, out) {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (!SKIP.has(e.name) && !e.name.startsWith(".")) walk(p, out);
    } else if (EXT.test(e.name) && !e.name.endsWith(".d.ts")) out.push(p);
  }
  return out;
}

const options = {
  allowJs: true,
  checkJs: false,
  noEmit: true,
  skipLibCheck: true,
  jsx: ts.JsxEmit.Preserve,
  module: ts.ModuleKind.ESNext,
  moduleResolution: ts.ModuleResolutionKind.Bundler,
};
// paths and baseUrl from the root tsconfig only.
const rootCfg = path.join(root, "tsconfig.json");
if (fs.existsSync(rootCfg)) {
  const cfg = ts.readConfigFile(rootCfg, ts.sys.readFile).config?.compilerOptions ?? {};
  if (cfg.baseUrl) options.baseUrl = path.resolve(root, cfg.baseUrl);
  if (cfg.paths) {
    options.paths = cfg.paths;
    options.pathsBasePath = options.baseUrl ?? root;
  }
}

const rel = (f) => path.relative(root, f).split(path.sep).join("/");
const files = walk(root, []).sort();
const host = {
  getScriptFileNames: () => files,
  getScriptVersion: () => "1",
  getScriptSnapshot: (f) => {
    const t = ts.sys.readFile(f);
    return t === undefined ? undefined : ts.ScriptSnapshot.fromString(t);
  },
  getCurrentDirectory: () => root,
  getCompilationSettings: () => options,
  getDefaultLibFileName: (o) => ts.getDefaultLibFilePath(o),
  fileExists: ts.sys.fileExists,
  readFile: ts.sys.readFile,
  readDirectory: ts.sys.readDirectory,
  directoryExists: ts.sys.directoryExists,
  getDirectories: ts.sys.getDirectories,
  realpath: ts.sys.realpath,
};
const service = ts.createLanguageService(host, ts.createDocumentRegistry());
const program = service.getProgram();

const out = [];
const emit = (r) => out.push(JSON.stringify(r));
const jsonOptions = { ...options, jsx: "preserve", module: "ESNext", moduleResolution: "Bundler" };
if (jsonOptions.baseUrl) jsonOptions.baseUrl = rel(jsonOptions.baseUrl) || ".";
delete jsonOptions.pathsBasePath;
emit({ program: { options: jsonOptions, files: files.map(rel) } });

const textCache = new Map();
const textOf = (f) => {
  if (!textCache.has(f)) textCache.set(f, ts.sys.readFile(f) ?? "");
  return textCache.get(f);
};
// UTF-16 code unit offset to UTF-8 byte offset. One table per file, built once.
// An index inside a surrogate pair gets the offset after the pair.
const offCache = new Map();
function offsets(f) {
  let m = offCache.get(f);
  if (m) return m;
  const t = textOf(f);
  m = new Uint32Array(t.length + 1);
  let b = 0;
  for (let i = 0; i < t.length; i++) {
    m[i] = b;
    const c = t.charCodeAt(i);
    if (c < 0x80) b += 1;
    else if (c < 0x800) b += 2;
    else if (c >= 0xd800 && c < 0xdc00 && i + 1 < t.length) {
      b += 4; // surrogate pair: 4 bytes, counted at the high surrogate
      m[++i] = b;
    } else b += 3;
  }
  m[t.length] = b;
  offCache.set(f, m);
  return m;
}
const u8 = (f, pos) => offsets(f)[pos];

// Tag a location by its innermost node.
function tagOf(f, start) {
  const sf = program.getSourceFile(f);
  if (!sf) return "code";
  let node = sf;
  let jsdoc = null; // "link" or "type" once the walk is inside a JSDoc node
  for (;;) {
    let next;
    const pick = (c) => {
      if (!next && start >= c.getStart(sf, true) && start < c.getEnd()) next = c;
    };
    // forEachChild does not visit the jsDoc array of a declaration.
    for (const d of node.jsDoc ?? []) pick(d);
    ts.forEachChild(node, pick);
    if (!next) break;
    node = next;
    if (ts.isJSDocLink(node) || ts.isJSDocLinkCode(node) || ts.isJSDocLinkPlain(node) ||
        ts.isJSDocSeeTag(node)) jsdoc = "link";
    else if (ts.isJSDoc(node) && !jsdoc) jsdoc = "type";
  }
  // Plain words inside a JSDoc comment are comment text, not a type or link.
  if (jsdoc && (ts.isJSDoc(node) || node.kind === ts.SyntaxKind.JSDocText)) return "comment";
  if (jsdoc) return `jsdoc_${jsdoc}`;
  if (node === sf) return "comment";
  if (ts.isStringLiteralLike(node) || ts.isTemplateLiteralToken(node) ||
      ts.isJsxText(node)) return "string";
  return "code";
}

const cases = fs.readFileSync(casesFile, "utf8").split("\n").filter((l) => l.trim()).map((l) => JSON.parse(l));
for (const c of cases) {
  const file = path.join(root, c.path);
  const lines = textOf(file).split(/\r\n|\r|\n/);
  const text = lines[c.line - 1];
  if (text === undefined) { emit({ case: c.case, excluded: "bad_case", message: "no such line" }); continue; }
  const re = new RegExp(`(?<![\\p{L}\\p{N}_$])${c.name.replace(/[$]/g, "\\$&")}(?![\\p{L}\\p{N}_$])`, "gu");
  const hits = [...text.matchAll(re)];
  const hit = hits[c.occ ?? 0];
  if (!hit) { emit({ case: c.case, excluded: "bad_case", message: "name not found on line" }); continue; }
  // Line start offset in the UTF-16 text. Lines split on the same rule as the loop above.
  let off = 0;
  const t = textOf(file);
  const lineRe = /\r\n|\r|\n/g;
  for (let i = 1; i < c.line; i++) { lineRe.exec(t); off = lineRe.lastIndex; }
  const pos = off + hit.index;

  const info = service.getRenameInfo(file, pos, { allowRenameOfImportPath: false });
  if (!info.canRename) {
    emit({ case: c.case, excluded: "cannot_rename", message: info.localizedErrorMessage });
    continue;
  }
  // A case with strings:true also asks TS for string and comment hits.
  // This is a report mode. The plain set leaves it off.
  const locs = service.findRenameLocations(file, pos, !!c.strings, !!c.strings,
    { providePrefixAndSuffixTextForRename: false });
  if (!locs || locs.length === 0) { emit({ case: c.case, excluded: "no_locations", message: "findRenameLocations gave undefined" }); continue; }
  const rows = locs.map((l) => ({
    case: c.case,
    path: rel(l.fileName),
    start: u8(l.fileName, l.textSpan.start),
    end: u8(l.fileName, l.textSpan.start + l.textSpan.length),
    prefix: l.prefixText ?? null,
    suffix: l.suffixText ?? null,
    tag: tagOf(l.fileName, l.textSpan.start),
  }));
  rows.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : a.start - b.start));
  for (const r of rows) emit(r);
}
console.log(out.join("\n"));
