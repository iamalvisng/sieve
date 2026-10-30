// Split oracle and plans by set, run rename-compare.mjs per set, apply the ship rules.
// Usage: node verdict.mjs <oracle.jsonl> <plans.jsonl> <outdir>
// Writes compare-<set>.{txt,json} to <outdir>.
// Prints the counts table and the verdict. Exit code is always 0 when it ran.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const [oraclePath, plansPath, outdir] = process.argv.slice(2);
if (!oraclePath || !plansPath || !outdir) { console.error("usage: node verdict.mjs <oracle.jsonl> <plans.jsonl> <outdir>"); process.exit(1); }
const here = path.dirname(fileURLToPath(import.meta.url));
const compare = path.join(here, "..", "rename-compare.mjs");
const rows = (f) => fs.readFileSync(f, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
const setOf = (id) => /:(plain|collision|report):\d+$/.exec(id)?.[1];
const oracle = rows(oraclePath), plans = rows(plansPath);
const SETS = ["plain", "collision", "report"];
const res = {};
for (const set of SETS) {
  const o = oracle.filter((r) => r.case !== undefined && setOf(r.case) === set);
  const p = plans.filter((r) => setOf(r.case) === set);
  const of = path.join(outdir, `compare-${set}.oracle.jsonl`), pf = path.join(outdir, `compare-${set}.plans.jsonl`);
  fs.writeFileSync(of, o.map((r) => JSON.stringify(r)).join("\n") + (o.length ? "\n" : ""));
  fs.writeFileSync(pf, p.map((r) => JSON.stringify(r)).join("\n") + (p.length ? "\n" : ""));
  const txt = spawnSync("node", [compare, of, pf], { encoding: "utf8" });
  fs.writeFileSync(path.join(outdir, `compare-${set}.txt`), txt.stdout + txt.stderr);
  const js = spawnSync("node", [compare, of, pf, "--json"], { encoding: "utf8" });
  fs.writeFileSync(path.join(outdir, `compare-${set}.json`), js.stdout);
  res[set] = { cases: new Set(o.map((r) => r.case)).size, ...JSON.parse(js.stdout) };
}

const med = (a) => { const t = [...a].sort((x, y) => x - y); if (!t.length) return NaN; return t.length % 2 ? t[(t.length - 1) / 2] : (t[t.length / 2 - 1] + t[t.length / 2]) / 2; };
const p95 = (a) => { const t = [...a].sort((x, y) => x - y); return t.length ? t[Math.min(t.length - 1, Math.ceil(0.95 * t.length) - 1)] : NaN; };
const times = (sets) => plans.filter((r) => sets.includes(setOf(r.case))).map((r) => r.time_ms);

console.log("Counts per set");
for (const set of SETS) {
  const { counts: c, refused_primary: rp, sites: s } = res[set];
  const top = Object.entries(rp).sort((a, b) => b[1] - a[1]).slice(0, 6).map(([k, v]) => `${k} ${v}`).join(", ") || "none";
  console.log(`- ${set} (${res[set].cases}): Exact ${c.Exact}, Wrong ${c.Wrong}, Refused ${c.Refused}, Excluded ${c.Excluded}, NoPlan ${c.NoPlan}, Unknown ${c.Unknown}`);
  console.log(`  refused by primary reason: ${top}`);
  console.log(`  sites matched/wrong/missed: ${s.matched}/${s.wrong}/${s.missed}, TS sites in refused cases: ${s.refusedTs}`);
}
const execErr = plans.filter((r) => r.status === "error").length;
if (execErr) console.log(`- plans with status "error" (sieve gave no JSON plan): ${execErr}. See plans.errors.txt`);

// Rule 1
const wrong = res.plain.counts.Wrong + res.collision.counts.Wrong;
const missing = SETS.reduce((n, s) => n + res[s].counts.NoPlan + res[s].counts.Unknown, 0);
const r1 = wrong === 0 && res.plain.cases > 0;
// Rule 2
const pc = res.plain.counts, rp = res.plain.refused_primary;
const selfImp = rp.package_self_import ?? 0;
const inScope = res.plain.cases - pc.Excluded - selfImp;
const rate = inScope > 0 ? pc.Exact / inScope : 0;
const s = res.plain.sites;
const inScopeRefusedSites = s.refusedTs; // report only: includes package_self_import cases
const siteRate = s.matched + inScopeRefusedSites > 0 ? s.matched / (s.matched + inScopeRefusedSites) : 0;
const reasons = Object.entries(rp).filter(([k]) => k !== "package_self_import").sort((a, b) => b[1] - a[1]);
const [topReason, topN] = reasons[0] ?? ["none", 0];
const topShare = inScope > 0 ? topN / inScope : 0;
const r2a = rate >= 0.25, r2b = topShare <= 0.5, r2 = r2a && r2b && inScope > 0;
// Rule 3
const planMed = med(times(["plain", "collision"]));
const r3 = planMed <= 3000;
const pct = (x) => (100 * x).toFixed(1) + "%";
const pf = (b) => (b ? "PASS" : "FAIL");

console.log("\nShip rules");
console.log(`- Rule 1, 0 Wrong in plain and collision: ${pf(r1)} (plain Wrong ${pc.Wrong}, collision Wrong ${res.collision.counts.Wrong}${missing ? `, NoPlan/Unknown ${missing}` : ""})`);
console.log(`- Rule 2a, Exact >= 25% of in-scope plain: ${pf(r2a)} (${pc.Exact}/${inScope} = ${pct(rate)}; excluded ${pc.Excluded}, package_self_import ${selfImp}; by site ${pct(siteRate)})`);
console.log(`- Rule 2b, no reason above 50% of in-scope plain: ${pf(r2b)} (largest ${topReason} ${topN}/${inScope} = ${pct(topShare)})`);
console.log(`- Rule 3, median plan time <= 3 s: ${pf(r3)} (median ${Math.round(planMed)} ms, p95 ${Math.round(p95(times(["plain", "collision"])))} ms, n ${times(["plain", "collision"]).length}; the rule names vite, read this as the flowfig figure)`);
console.log(`VERDICT: ${r1 && r2 && r3 ? "SHIP-RULES MET on this repo" : r1 ? "RULE 1 MET; RULE 2 or 3 FAILED (owner reads the histogram)" : "RULE 1 FAILED (the feature does not ship)"}`);
if (wrong) console.log("Wrong cases: see compare-plain.txt and compare-collision.txt");
