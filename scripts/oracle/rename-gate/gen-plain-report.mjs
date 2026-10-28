// Usage: node gen-plain-report.mjs <repo> <tag> <outdir> [sample]
// Plain set: every top-level function, or a seeded sample of N when [sample] is given.
import fs from "node:fs";
import { SEED, shuffle, topLevel, wiring, declPos, write, loadTs } from "./common.mjs";
const ts = loadTs();
const [repo, tag, outdir, sampleArg] = process.argv.slice(2);
const sample = sampleArg ? Number(sampleArg) : 0; // 0 = every top-level function
const g = wiring(repo);
const row = (set, i, n, pos, extra = {}) => ({ case: `${tag}:${set}:${i}`, path: n.path, line: pos.line, col: pos.col, name: n.name, new: set === "plain" ? n.name + "Rn" : undefined, set, ...extra });
const sortRows = (r) => r.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : a.line - b.line));
// plain
const fns = topLevel(g, "function"); let skipped = 0;
let pool = sample && fns.length > sample ? shuffle(fns, SEED) : fns;
let plain = [];
for (const n of pool) { if (sample && plain.length >= sample) break; const p = declPos(repo, n); if (!p) { skipped++; continue; } plain.push({ n, p }); }
plain = plain.sort((a, b) => (a.n.id < b.n.id ? -1 : 1)).map(({ n, p }, i) => row("plain", i, n, p));
write(`${outdir}/${tag}-plain.jsonl`, plain);
// report: classes
const cls = shuffle(topLevel(g, "class"), SEED).slice(0, 30), crows = [];
for (const n of cls) { const p = declPos(repo, n); if (p) crows.push({ n, p }); }
// report: default exports with a name token
const defs = [];
for (const n of g.nodes.filter((n) => n.kind === "file" && /\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/.test(n.path) && !n.path.endsWith(".d.ts")).sort((a, b) => (a.path < b.path ? -1 : 1))) {
  const f = `${repo}/${n.path}`; const text = fs.readFileSync(f, "utf8");
  const sf = ts.createSourceFile(f, text, ts.ScriptTarget.Latest, true);
  for (const s of sf.statements) {
    let id;
    if (ts.isExportAssignment(s) && !s.isExportEquals && ts.isIdentifier(s.expression)) id = s.expression;
    else if ((ts.isFunctionDeclaration(s) || ts.isClassDeclaration(s)) && s.name && s.modifiers?.some((m) => m.kind === ts.SyntaxKind.DefaultKeyword)) id = s.name;
    if (id) { const lc = sf.getLineAndCharacterOfPosition(id.getStart()); defs.push({ path: n.path, name: id.text, line: lc.line + 1, col: lc.character + 1 }); }
  }
}
const drows = shuffle(defs, SEED).slice(0, 30);
const rep = [...crows.map(({ n, p }) => ({ n, p, kind: "class" })), ...drows.map((d) => ({ n: { path: d.path, name: d.name }, p: d, kind: "default_export" }))]
  .map((x, i) => ({ case: `${tag}:report:${i}`, path: x.n.path, line: x.p.line, col: x.p.col, name: x.n.name, set: "report", kind: x.kind }));
write(`${outdir}/${tag}-report.jsonl`, rep);
console.log(JSON.stringify({ tag, functions_total: fns.length, plain: plain.length, skipped_no_pos: skipped, classes_total: topLevel(g, "class").length, report_classes: crows.length, defaults_total: defs.length, report_defaults: drows.length }));
