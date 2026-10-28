// Collision set generator. Usage: node ts-collision.mjs <repo> <tag> <out.jsonl>
// Same program as ts-rename.mjs. For a seeded shuffle of top-level functions: find the first
// code reference site of O (not the declaration), take getSymbolsInScope there, keep names that
// are valid identifiers, differ from O, and occur as a whole-word token in a touched file
// (a file with a rename location of O). Pick one by seed. Stop at 100 cases.
import fs from "node:fs";
import path from "node:path";
import { SEED, rng, shuffle, topLevel, wiring, declPos, write, loadTs } from "./common.mjs";
const ts = loadTs();
const [repoArg, tag, outFile] = process.argv.slice(2);
const root = path.resolve(repoArg);
const SKIP = new Set(["node_modules","dist","build","_build","out","target","vendor","coverage","__pycache__","venv"]);
const EXT = /\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/;
const walk = (d, o) => { for (const e of fs.readdirSync(d, { withFileTypes: true })) { const p = path.join(d, e.name); if (e.isDirectory()) { if (!SKIP.has(e.name) && !e.name.startsWith(".")) walk(p, o); } else if (EXT.test(e.name) && !e.name.endsWith(".d.ts")) o.push(p); } return o; };
const options = { allowJs: true, checkJs: false, noEmit: true, skipLibCheck: true, jsx: ts.JsxEmit.Preserve, module: ts.ModuleKind.ESNext, moduleResolution: ts.ModuleResolutionKind.Bundler };
const rootCfg = path.join(root, "tsconfig.json");
if (fs.existsSync(rootCfg)) { const cfg = ts.readConfigFile(rootCfg, ts.sys.readFile).config?.compilerOptions ?? {}; if (cfg.baseUrl) options.baseUrl = path.resolve(root, cfg.baseUrl); if (cfg.paths) { options.paths = cfg.paths; options.pathsBasePath = options.baseUrl ?? root; } }
const files = walk(root, []).sort();
const host = { getScriptFileNames: () => files, getScriptVersion: () => "1", getScriptSnapshot: (f) => { const t = ts.sys.readFile(f); return t === undefined ? undefined : ts.ScriptSnapshot.fromString(t); }, getCurrentDirectory: () => root, getCompilationSettings: () => options, getDefaultLibFileName: (o) => ts.getDefaultLibFilePath(o), fileExists: ts.sys.fileExists, readFile: ts.sys.readFile, readDirectory: ts.sys.readDirectory, directoryExists: ts.sys.directoryExists, getDirectories: ts.sys.getDirectories, realpath: ts.sys.realpath };
const service = ts.createLanguageService(host, ts.createDocumentRegistry());
const program = service.getProgram(), checker = program.getTypeChecker();
const rel = (f) => path.relative(root, f).split(path.sep).join("/");
const text = new Map(); const textOf = (f) => (text.has(f) ? text.get(f) : (text.set(f, ts.sys.readFile(f) ?? ""), text.get(f)));
const IDENT = /^[\p{L}_$][\p{L}\p{N}_$]*$/u;
const hasToken = (f, name) => new RegExp(`(?<![\\p{L}\\p{N}_$])${name.replace(/\$/g, "\\$&")}(?![\\p{L}\\p{N}_$])`, "u").test(textOf(f));
const RESERVED = new Set(["default","arguments","undefined","eval","this","super","null","true","false","new","delete","typeof","void","in","of","if","else","for","while","do","var","let","const","function","class","return","import","export","await","yield","static","enum"]);
const g = wiring(root);
const pool = shuffle(topLevel(g, "function"), SEED + 1);
const r = rng(SEED + 2);
const rows = []; const stats = { tried: 0, no_pos: 0, cannot_rename: 0, no_ref: 0, no_candidate: 0 };
for (const n of pool) {
  if (rows.length >= 100) break;
  stats.tried++;
  const p = declPos(root, n); if (!p) { stats.no_pos++; continue; }
  const file = path.join(root, n.path);
  const lines = textOf(file).split(/\r\n|\r|\n/);
  let off = 0; for (let i = 0; i < p.line - 1; i++) off += lines[i].length + (textOf(file).slice(off + lines[i].length).startsWith("\r\n") ? 2 : 1);
  const pos = off + p.col - 1;
  const info = service.getRenameInfo(file, pos, { allowRenameOfImportPath: false });
  if (!info.canRename) { stats.cannot_rename++; continue; }
  const locs = service.findRenameLocations(file, pos, false, false, { providePrefixAndSuffixTextForRename: false }) ?? [];
  const touched = [...new Set(locs.map((l) => l.fileName))];
  const refs = locs.filter((l) => !(l.fileName === file && l.textSpan.start === pos));
  let cand = null;
  for (const ref of refs) {   // first reference site that gives candidates
    const sf = program.getSourceFile(ref.fileName); if (!sf) continue;
    let node = (function find(nd) { return ts.forEachChild(nd, (c) => (ref.textSpan.start >= c.getStart(sf) && ref.textSpan.start < c.getEnd() ? find(c) || c : undefined)); })(sf);
    if (!node || !ts.isIdentifier(node)) continue;
    const syms = checker.getSymbolsInScope(node, ts.SymbolFlags.Value | ts.SymbolFlags.Type | ts.SymbolFlags.Namespace | ts.SymbolFlags.Alias);
    const names = [...new Set(syms.map((s) => s.getName()))].filter((s) => IDENT.test(s) && s !== n.name && !RESERVED.has(s) && !s.startsWith("__") && touched.some((f) => hasToken(f, s))).sort();
    if (names.length) { cand = { names, site: `${rel(ref.fileName)}:${sf.getLineAndCharacterOfPosition(ref.textSpan.start).line + 1}` }; break; }
  }
  if (!cand) { stats.no_candidate++; continue; }
  const pick = cand.names[Math.floor(r() * cand.names.length)];
  rows.push({ case: "", path: n.path, line: p.line, col: p.col, name: n.name, new: pick, set: "collision", ref_site: cand.site, candidates: cand.names.length });
}
rows.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : a.line - b.line)).forEach((x, i) => (x.case = `${tag}:collision:${i}`));
write(outFile, rows);
console.log(JSON.stringify({ tag, collision: rows.length, ...stats, ts: ts.version }));
