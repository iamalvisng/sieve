// Writes assets/banner.svg from the mascot sprites. Run: node scripts/gen-banner.mjs
// With --check, exits 1 if the file on disk differs from the generated text.
import { readFileSync, writeFileSync } from 'node:fs'
const root = new URL('..', import.meta.url)
const j = JSON.parse(readFileSync(new URL('crates/sieve-cli/assets/mascots.json', root), 'utf8'))

// One <rect> per run of the same color in a row.
function rects(frame, px) {
  const out = []
  frame.forEach((row, y) => {
    for (let x = 0; x < row.length; ) {
      const ch = row[x]
      let e = x
      while (e < row.length && row[e] === ch) e++
      if (j.palette[ch]) out.push(`<rect x="${x * px}" y="${y * px}" width="${(e - x) * px}" height="${px}" fill="${j.palette[ch]}"/>`)
      x = e
    }
  })
  return out.join('')
}

const W = 960, H = 340, GAP = 24, BASE = 220
const sizes = j.order.map(() => 7)
const total = sizes.reduce((a, px) => a + 16 * px, 0) + GAP * (sizes.length - 1)
let x0 = (W - total) / 2
let body = ''
j.order.forEach((id, i) => {
  const px = sizes[i]
  const frames = j.mascots[id].frames
  const y0 = BASE - 14 * px
  const n = frames.length
  body += `<g transform="translate(${x0} ${y0})"><title>${id}</title>`
  frames.forEach((f, k) => {
    body += `<g class="f f${k}" style="animation-duration:${n * 0.5}s;animation-delay:${k * 0.5}s">${rects(f, px)}</g>`
  })
  body += '</g>'
  x0 += 16 * px + GAP
})

// Each frame shows for 0.5 s. The keyframes show a frame for the first 1/n of its loop.
// ponytail: the keyframes assume 2 frames per mascot; the data has 2 for every mascot.
const FONT = 'ui-monospace,Menlo,monospace'
const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${W} ${H}" width="100%" role="img" shape-rendering="crispEdges">
<title>Sieve and five mascots: soya, cloud, sifty, hoot and grit. Sieve is a code graph for coding agents.</title>
<style>
.f{opacity:0;animation-name:show;animation-iteration-count:infinite;animation-timing-function:steps(1,end)}
@keyframes show{0%{opacity:1}50%{opacity:0}100%{opacity:0}}
@media (prefers-reduced-motion:reduce){.f{animation:none}.f0{opacity:1}}
</style>
<rect width="${W}" height="${H}" fill="#1a1b26"/>
${body}
<text x="${W / 2}" y="285" text-anchor="middle" font-family="${FONT}" font-size="40" font-weight="700" fill="#c0caf5">sieve</text>
<text x="${W / 2}" y="318" text-anchor="middle" font-family="${FONT}" font-size="18" fill="#565f89">a code graph for coding agents</text>
</svg>
`

const file = new URL('assets/banner.svg', root)
if (process.argv.includes('--check')) {
  let disk = ''
  try { disk = readFileSync(file, 'utf8') } catch {}
  if (disk !== svg) { console.error('assets/banner.svg is stale: run node scripts/gen-banner.mjs'); process.exit(1) }
} else {
  writeFileSync(file, svg)
}
