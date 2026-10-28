// Compare Sieve rename plans with the TypeScript oracle.
// Usage: node rename-compare.mjs <oracle.jsonl> <sieve-plans.jsonl> [--json]
// Design: docs/design/rename.md, sections C and E.
//
// oracle.jsonl: output of ts-rename.mjs. Rows: {program}, {case, path, start,
//   end, prefix, suffix, tag} and {case, excluded, message}.
// sieve-plans.jsonl: one JSON row per case. Each row is a plan JSON v1
//   (section C) plus a "case" field that names the oracle case:
//   {case, version, status: "ready"|"refused", files: [{path, edits:
//   [{start, end, ...}]}], refused: [{reason, ...}], ...}
//
// Classes, per case:
//   Excluded  the oracle row says TS cannot rename the case, and Sieve gave
//             no ready plan.
//   Refused   status is "refused" and the plan holds no edit. The first
//             refused row gives the reason.
//   Exact     status is "ready", the edit set equals the TS location set,
//             and no TS location carries prefix or suffix text.
//   Wrong     any of:
//               - status "ready" with an extra edit (wrong edit), a missing
//                 edit (missed edit), or a TS location with prefix/suffix text;
//               - a duplicate or overlapping edit span in one file, or an
//                 edit with a bad span (malformed plan);
//               - a ready plan for an Excluded case;
//               - status "refused" with edits in "files" (malformed plan);
//               - a status that is neither "ready" nor "refused".
//   NoPlan    the oracle has the case and the plans file has no row for it.
//   Unknown   a plan row for a case that the oracle does not know.
// Oracle rows with tag string or comment (a strings:true case) are report
// sites. The plan must not edit them: an edit there is a wrong edit.
// This script does not run the post-apply diagnostic check (--apply, later).
// Exit code: 0 clean; 2 when Wrong is above 0; 3 when NoPlan or Unknown is
// above 0 and Wrong is 0; 1 on a usage error, a bad file or a duplicate case
// id in the plans file.
import fs from "node:fs";

const [oraclePath, plansPath, ...flags] = process.argv.slice(2);
if (!oraclePath || !plansPath) {
  console.error("usage: node rename-compare.mjs <oracle.jsonl> <sieve-plans.jsonl> [--json]");
  process.exit(1);
}
const fail = (m) => { console.error(`rename-compare: ${m}`); process.exit(1); };
const readRows = (f) => {
  try {
    return fs.readFileSync(f, "utf8").split("\n").filter((l) => l.trim()).map((l) => JSON.parse(l));
  } catch (e) {
    return fail(`${f}: ${e.message}`);
  }
};

const REPORT_TAGS = new Set(["string", "comment"]);
const oracle = new Map(); // case -> {excluded, locs: Map key -> row}
for (const r of readRows(oraclePath)) {
  if (r.program || r.case === undefined) continue;
  const c = oracle.get(r.case) ?? { excluded: null, locs: new Map() };
  if (r.excluded) c.excluded = r.excluded;
  else c.locs.set(`${r.path}:${r.start}:${r.end}`, r);
  oracle.set(r.case, c);
}

const plans = new Map();
for (const p of readRows(plansPath)) {
  if (typeof p.case !== "string") fail("a plan row has no case id");
  if (plans.has(p.case)) fail(`duplicate case id in plans: ${p.case}`);
  plans.set(p.case, p);
}

const n = { Exact: 0, Wrong: 0, Refused: 0, Excluded: 0, NoPlan: 0, Unknown: 0 };
const refusedBy = {}; // primary reason -> cases
const refusedAll = {}; // every distinct reason -> cases
const excludedBy = {}; // excluded reason -> cases
const site = { matched: 0, wrong: 0, missed: 0, refusedTs: 0 };
const missedByTag = {};
const wrongCases = [];
const noPlan = [];
const unknown = [];

const bump = (m, k) => { m[k] = (m[k] ?? 0) + 1; };
const wrong = (name, why) => { n.Wrong++; wrongCases.push({ case: name, why }); };

// Spans of a plan, as [path, start, end, edit]. Reports malformed spans.
function spansOf(p) {
  const list = [];
  const bad = [];
  for (const f of p.files ?? []) {
    for (const e of f.edits ?? []) {
      if (!Number.isInteger(e.start) || !Number.isInteger(e.end) || e.start < 0 || e.end <= e.start) {
        bad.push(`${f.path}:${e.start}-${e.end}`);
      } else list.push([f.path, e.start, e.end]);
    }
  }
  return { list, bad };
}

// Duplicate or overlapping spans inside one file.
function overlaps(list) {
  const byFile = new Map();
  for (const [p, s, e] of list) (byFile.get(p) ?? byFile.set(p, []).get(p)).push([s, e]);
  const hits = [];
  for (const [p, v] of byFile) {
    v.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
    for (let i = 1; i < v.length; i++) {
      if (v[i][0] < v[i - 1][1]) hits.push(`${p}:${v[i - 1][0]}-${v[i - 1][1]}/${v[i][0]}-${v[i][1]}`);
    }
  }
  return hits;
}

for (const [name, o] of oracle) {
  const p = plans.get(name);
  const hasEdits = p ? spansOf(p).list.length + spansOf(p).bad.length > 0 : false;
  if (o.excluded) {
    if (p && p.status === "ready") { wrong(name, "ready plan for an excluded case"); continue; }
    if (p && p.status !== "refused") { wrong(name, `bad status ${JSON.stringify(p.status)}`); continue; }
    if (p && hasEdits) { wrong(name, "refused plan holds edits"); continue; }
    n.Excluded++; bump(excludedBy, o.excluded); continue;
  }
  if (!p) { n.NoPlan++; noPlan.push(name); continue; }
  if (p.status === "refused") {
    if (hasEdits) { wrong(name, "refused plan holds edits"); continue; }
    n.Refused++;
    const rs = (p.refused ?? []).map((x) => x.reason);
    bump(refusedBy, rs[0] ?? "(none)");
    for (const r of new Set(rs)) bump(refusedAll, r);
    site.refusedTs += o.locs.size;
    continue;
  }
  if (p.status !== "ready") { wrong(name, `bad status ${JSON.stringify(p.status)}`); continue; }
  const { list, bad } = spansOf(p);
  if (bad.length) { wrong(name, `bad span ${bad.join(" ")}`); continue; }
  const dup = overlaps(list);
  if (dup.length) { wrong(name, `duplicate or overlapping edit ${dup.join(" ")}`); continue; }
  const mine = new Set(list.map(([f, s, e]) => `${f}:${s}:${e}`));
  let extra = 0, missed = 0, matched = 0, affix = 0;
  for (const k of mine) {
    const r = o.locs.get(k);
    if (r && !REPORT_TAGS.has(r.tag)) matched++; else extra++;
  }
  for (const [k, r] of o.locs) {
    if (REPORT_TAGS.has(r.tag)) continue;
    if (!mine.has(k)) { missed++; bump(missedByTag, r.tag); }
    if (r.prefix || r.suffix) affix++;
  }
  site.matched += matched; site.wrong += extra; site.missed += missed;
  if (extra || missed || affix) {
    wrong(name, `extra ${extra}, missed ${missed}, prefix/suffix ${affix}`);
  } else n.Exact++;
}
for (const name of plans.keys()) if (!oracle.has(name)) { n.Unknown++; unknown.push(name); }

const result = { counts: n, refused_primary: refusedBy, refused_any: refusedAll,
  excluded: excludedBy, sites: site, missed_by_tag: missedByTag, wrong_cases: wrongCases,
  no_plan: noPlan, unknown };
if (flags.includes("--json")) {
  console.log(JSON.stringify(result, null, 2));
} else {
  console.log(`Exact ${n.Exact}  Wrong ${n.Wrong}  Refused ${n.Refused}  Excluded ${n.Excluded}  NoPlan ${n.NoPlan}  Unknown ${n.Unknown}`);
  const hist = (title, m) => {
    console.log(title);
    const ks = Object.keys(m).sort((a, b) => m[b] - m[a] || (a < b ? -1 : 1));
    if (!ks.length) console.log("  (none)");
    for (const k of ks) console.log(`  ${k}: ${m[k]}`);
  };
  hist("Refused by primary reason:", refusedBy);
  hist("Refused by any reason:", refusedAll);
  hist("Excluded by reason:", excludedBy);
  console.log(`Sites: matched ${site.matched}, wrong ${site.wrong}, missed ${site.missed}, TS sites in refused cases ${site.refusedTs}`);
  hist("Missed sites by tag:", missedByTag);
  for (const w of wrongCases) console.log(`WRONG ${w.case}: ${w.why}`);
  for (const c of noPlan) console.log(`NOPLAN ${c}`);
  for (const c of unknown) console.log(`UNKNOWN ${c}`);
}
process.exit(n.Wrong > 0 ? 2 : n.NoPlan + n.Unknown > 0 ? 3 : 0);
