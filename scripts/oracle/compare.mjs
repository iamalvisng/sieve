// Compare oracle rows with Sieve edges. Usage: node compare.mjs rows.jsonl wiring.json
// wiring.json is <repo>/sieve/.graph/wiring.json, which `sieve build` writes. It holds
// nodes {id, path, name, span "L1-L9"} and edges {source, target, relation, confidence}.
// The script keeps the Extracted calls edges between two files.
// Count 1 (slice 1): the key is (source file, target file, name).
// Count 2 (gate): a row matches an edge only if the row line is inside the
// span of the edge source node, and the target file and name also match.
// A Sieve edge with no match is wrong. Coverage is per shape, for rows with a
// relative specifier only. One line shows the count of the other rows.
import fs from "node:fs";

const [rowsPath, graphPath] = process.argv.slice(2);
const all = fs.readFileSync(rowsPath, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
const g = JSON.parse(fs.readFileSync(graphPath, "utf8"));
const node = new Map(g.nodes.map((n) => [n.id, n]));
const last = (name) => name.split(".").pop();
function span(n) {
  const m = /^L(\d+)-L(\d+)$/.exec(n.span ?? "");
  if (!m) {
    console.error(`bad span "${n.span}" on node ${n.id}`);
    process.exit(1);
  }
  return [Number(m[1]), Number(m[2])];
}

const edges = g.edges
  .filter((e) => e.relation === "calls" && e.confidence === "extracted")
  .map((e) => ({ s: node.get(e.source), t: node.get(e.target) }))
  .filter((e) => e.s && e.t && e.s.path !== e.t.path)
  .map((e) => ({ src: e.s.path, dst: e.t.path, name: last(e.t.name), span: span(e.s), id: `${e.s.id} -> ${e.t.id}` }));

const key = (r) => `${r.src}|${r.dst}|${r.name}`;
const byKey = new Map();
for (const r of all) {
  if (!byKey.has(key(r))) byKey.set(key(r), []);
  byKey.get(key(r)).push(r);
}
const edgeKeys = new Set(edges.map(key));
const hits = (e) => (byKey.get(key(e)) ?? []).filter((r) => r.line >= e.span[0] && r.line <= e.span[1]);

// An edge that matches a row with name null (anonymous target) by span and files is uncovered, not wrong.
const nullRows = all.filter((r) => r.name === null);
const onNull = (e) => nullRows.some((r) => r.src === e.src && r.dst === e.dst && r.line >= e.span[0] && r.line <= e.span[1]);
const covered = new Set();
const wrong2 = [];
for (const e of edges) {
  const h = hits(e);
  if (h.length === 0 && !onNull(e)) wrong2.push(e);
  for (const r of h) covered.add(r);
}
const wrong1 = edges.filter((e) => !byKey.has(key(e)) && !onNull(e));

const rows = all.filter((r) => r.spec.startsWith("."));
const keys = new Set(rows.map(key));
const unc1 = [...keys].filter((k) => !edgeKeys.has(k)).map((k) => byKey.get(k)[0]);
const unc2 = rows.filter((r) => !covered.has(r));

console.log(`Sieve Extracted cross-file call edges: ${edges.length}`);
console.log(`non-relative oracle rows (not in coverage): ${all.length - rows.length}`);
console.log(`count 1: ${edges.length - wrong1.length} of ${edges.length} edges right; ${keys.size - unc1.length} of ${keys.size} pairs covered`);
console.log(`count 2: ${edges.length - wrong2.length} of ${edges.length} edges right; ${rows.length - unc2.length} of ${rows.length} rows covered`);
console.log("per shape: shape, rows, pairs, pairs covered (count 1), rows covered (count 2)");
for (const shape of [...new Set(rows.map((r) => r.shape))].sort()) {
  const rs = rows.filter((r) => r.shape === shape);
  const ks = new Set(rs.map(key));
  const c1 = [...ks].filter((k) => edgeKeys.has(k)).length;
  const c2 = rs.filter((r) => covered.has(r)).length;
  console.log(`  ${shape}: ${rs.length} rows, ${ks.size} pairs, ${c1} covered (count 1), ${c2} covered (count 2)`);
}
console.log("per shape and barrel flag: shape, barrel, rows, pairs, pairs covered (count 1), rows covered (count 2)");
for (const shape of [...new Set(rows.map((r) => r.shape))].sort()) {
  for (const barrel of [false, true]) {
    const rs = rows.filter((r) => r.shape === shape && r.barrel === barrel);
    if (rs.length === 0) continue;
    const ks = new Set(rs.map(key));
    const c1 = [...ks].filter((k) => edgeKeys.has(k)).length;
    const c2 = rs.filter((r) => covered.has(r)).length;
    console.log(`  ${shape}, barrel=${barrel}: ${rs.length} rows, ${ks.size} pairs, ${c1} covered (count 1), ${c2} covered (count 2)`);
  }
}
console.log(`wrong edges, count 1: ${wrong1.length}`);
for (const e of wrong1) console.log(`  ${e.id} (${key(e)})`);
console.log(`wrong edges, count 2: ${wrong2.length}`);
for (const e of wrong2) console.log(`  ${e.id} (${key(e)})`);
console.log(`uncovered pairs, count 1: ${unc1.length}`);
for (const r of unc1) console.log(`  [${r.shape}${r.barrel ? ",barrel" : ""}] ${key(r)}`);
console.log(`uncovered rows, count 2: ${unc2.length}`);
for (const r of unc2) console.log(`  [${r.shape}${r.barrel ? ",barrel" : ""}] ${r.src}:${r.line} -> ${r.dst}#${r.name}`);
