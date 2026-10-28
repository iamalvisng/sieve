// Dry-run `sieve rename` for each case. Never passes --write.
// Usage: node run-plans.mjs <sieve-bin> <repo> <cases.jsonl> <plans.jsonl>
// Run it with HOME set to a scratch dir (run.sh does this).
// `--dir` is the context dir (<repo>/sieve). The repo root is the cwd.
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
const [bin, repoArg, casesFile, outFile] = process.argv.slice(2);
if (!bin || !repoArg || !casesFile || !outFile) { console.error("usage: node run-plans.mjs <sieve-bin> <repo> <cases.jsonl> <plans.jsonl>"); process.exit(1); }
const repo = path.resolve(repoArg);
const cases = fs.readFileSync(casesFile, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l));
const out = fs.openSync(outFile, "w");
const errs = fs.openSync(outFile.replace(/\.jsonl$/, "") + ".errors.txt", "w");
cases.forEach((c, i) => {
  const t0 = process.hrtime.bigint();
  const r = spawnSync(path.resolve(bin), ["rename", `${c.path}#${c.name}`, c.new, "--json", "--dir", path.join(repo, "sieve")], { cwd: repo, encoding: "utf8", maxBuffer: 1 << 30 });
  const ms = Number(process.hrtime.bigint() - t0) / 1e6;
  let plan;
  try { plan = JSON.parse(r.stdout); } catch { plan = { version: 1, status: "error", files: [], refused: [], stdout_head: (r.stdout || "").slice(0, 300) }; }
  if (r.stderr) fs.writeSync(errs, `${c.case}\t${r.stderr.slice(0, 300).replace(/\n/g, " | ")}\n`);
  fs.writeSync(out, JSON.stringify({ case: c.case, set: c.set, symbol: `${c.path}#${c.name}`, exit: r.status, time_ms: Math.round(ms), ...plan }) + "\n");
  if ((i + 1) % 50 === 0 || i + 1 === cases.length) console.error(`  plans ${i + 1}/${cases.length}`);
});
