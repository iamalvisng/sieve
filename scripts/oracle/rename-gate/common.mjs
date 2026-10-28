import fs from "node:fs";
import { createRequire } from "node:module";
// typescript comes from scripts/oracle/node_modules (run `npm ci` in scripts/oracle).
export const loadTs = () => createRequire(new URL("../package.json", import.meta.url))("typescript");
export const SEED = 20261026;
export function rng(seed) { let a = seed >>> 0; return () => { a = (a + 0x6d2b79f5) >>> 0; let t = a; t = Math.imul(t ^ (t >>> 15), t | 1); t ^= t + Math.imul(t ^ (t >>> 7), t | 61); return ((t ^ (t >>> 14)) >>> 0) / 4294967296; }; }
export function shuffle(arr, seed) { const a = [...arr], r = rng(seed); for (let i = a.length - 1; i > 0; i--) { const j = Math.floor(r() * (i + 1)); [a[i], a[j]] = [a[j], a[i]]; } return a; }
export const TSJS = /\.(ts|tsx|js|jsx|mjs|cjs|mts|cts)$/;
export const wiring = (repo) => JSON.parse(fs.readFileSync(`${repo}/sieve/.graph/wiring.json`, "utf8"));
export const topLevel = (g, kind) => g.nodes.filter((n) => n.kind === kind && n.id === `${n.path}#${n.name}` && TSJS.test(n.path) && !n.path.endsWith(".d.ts")).sort((a, b) => (a.id < b.id ? -1 : 1));
// Position of the declaration name token: first whole-word hit at or after the span start line.
export function declPos(repo, n) {
  const lines = fs.readFileSync(`${repo}/${n.path}`, "utf8").split(/\r\n|\r|\n/);
  const start = Number(/^L(\d+)/.exec(n.span)[1]);
  const re = new RegExp(`(?<![\\p{L}\\p{N}_$])${n.name.replace(/\$/g, "\\$&")}(?![\\p{L}\\p{N}_$])`, "gu");
  for (let l = start; l <= Math.min(lines.length, start + 5); l++) {
    for (const m of lines[l - 1].matchAll(re)) {
      const pre = lines[l - 1].slice(0, m.index);
      if (/(function\s*\*?\s*|\b(const|let|var)\s+|\bclass\s+|\binterface\s+)$/.test(pre)) return { line: l, col: m.index + 1 };
    }
  }
  return null;
}
export const write = (f, rows) => fs.writeFileSync(f, rows.map((r) => JSON.stringify(r)).join("\n") + "\n");
