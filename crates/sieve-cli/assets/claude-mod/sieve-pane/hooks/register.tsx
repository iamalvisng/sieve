import { atom, read, update } from 'claude-code'
import type { EngineInterface as Engine, Register } from 'claude-code'

import type { Band, Query, Row, SievePaneView, Stats, Why } from '../types'
import { DEFAULT_MASCOT, MASCOTS, PALETTE } from './mascots'

const PANE = 'sieve'
const view = atom({ plugin: 'sieve-pane', key: 'view' } as const, null)
const band = atom({ plugin: 'sieve-pane', key: 'band' } as const, null)
const stats = atom({ plugin: 'sieve-pane', key: 'stats' } as const, null)

type Json = any // ponytail: the sieve JSON is not typed; the code reads only the fields it needs.
type Job = { file: string; path: string; tool: string; input: Json; at?: number; query?: Query; isFile?: boolean }
type Put = (v: SievePaneView) => Promise<void>

/** The 1-based line where the edit starts, or undefined. The tool has run, so try new_string, then old_string. A text counts only when it occurs once. */
export function findLine(tool: string, input: Json, text: string): number | undefined {
  if (tool === 'Read') return Math.max(1, Number(input.offset) || 1)
  if (tool === 'Write') return 1
  const edit = tool === 'MultiEdit' ? input.edits?.[0] : input
  for (const s of [edit?.new_string, edit?.old_string]) {
    const at = s ? text.indexOf(s) : -1
    if (at >= 0 && text.indexOf(s, at + 1) < 0) return text.slice(0, at).split('\n').length
  }
  return undefined
}

/** Resolves . and .. in an absolute path. */
export function normalize(p: string): string {
  const out: string[] = []
  for (const part of p.split('/')) {
    if (part === '..') out.pop()
    else if (part && part !== '.') out.push(part)
  }
  return `/${out.join('/')}`
}

const EDITS = ['Edit', 'MultiEdit', 'Write']
const rowsOf = (hits: Json[]) => hits.filter(h => h.depth === 2)

/** The route of a hit, as "POST  ai/chat", or undefined when the hit is not a route handler. */
export function routeOf(h: Json): string | undefined {
  const m = /(?:^|\/)routes\/(?:(.+)\/)?\+server\.\w+$/.exec(String(h.path))
  return m ? `${h.name}  ${(m[1] ?? '').replace(/^api\/v\d+\/?/, '').replace(/\/?\[[^\]]+\]/g, '') || '/'}` : undefined
}

const lineOf = (span: unknown) => String(span).replace(/^L(\d+).*/, '$1')
const base = (p: string) => p.split('/').pop() ?? p

/** One why card: the heading, the date and the SUPERSEDED flag with its successor. */
export function whyCards(why: Json): Why[] {
  return ((why?.symbols?.[0]?.decisions ?? []) as Json[]).slice(0, 2).map(d => ({
    date: d.date ?? '',
    heading: String(d.heading ?? ''),
    superseded: !!d.superseded,
    by: d.supersededBy ?? undefined,
  }))
}

/** Builds the tree rows (depth 1 and depth 2) from the callers hits. `kids` maps a depth-1 id to its depth-2 hits. */
export function makeTree(d1: Json[], kids: Map<string, Json[]>): Row[] {
  const rows: Row[] = []
  d1.forEach((h, i) => {
    rows.push({ d: 1, p: i, label: h.name, tail: `${base(String(h.path))}:${lineOf(h.span)}` })
    for (const c of kids.get(h.id) ?? []) {
      const r = routeOf(c)
      rows.push({ d: 2, p: i, label: r ?? c.name, tail: r ? '' : `${base(String(c.path))}:${lineOf(c.span)}` })
    }
  })
  return rows
}

/** Builds the view from `sieve why --json` and `sieve callers --depth 2 --json`. `kids` is optional. */
export function makeView(file: string, tool: string, why: Json, callers?: Json, kids?: Map<string, Json[]>): SievePaneView {
  const sym = why?.symbols?.[0]
  if (!sym) return { file, tool, why: [], tree: [], note: 'no symbol at this line' }
  const m = callers?.matches?.[0]
  const hits: Json[] = (callers?.matches ?? []).flatMap((x: Json) => x.hits ?? [])
  const d1 = hits.filter(h => h.depth === 1)
  // One depth-1 caller owns every depth-2 hit. With more, the `kids` map says who owns which.
  const map = kids ?? new Map(d1.length === 1 ? [[d1[0].id, rowsOf(hits)]] : [])
  return {
    file,
    tool,
    symbol: sym.name,
    kind: m?.symbol?.kind,
    line: m?.symbol?.span ? Number(lineOf(m.symbol.span)) : sym.line,
    why: whyCards(why),
    tree: makeTree(d1, map),
    n: callers ? hits.length : undefined,
    hops: callers ? Math.max(0, ...hits.map(h => Number(h.depth) || 0)) : undefined,
  }
}

const firstLine = (s: string) => s.split('\n').find(l => l.trim()) ?? ''

const LIMIT_MS = 20000
const TICK_MS = 100
const TICK = Symbol('tick')

/**
 * Runs `sieve <argv>`. It never throws. It uses `$.process.spawn`, because leaving the `for await` loop kills the child.
 * The loop ends early when `stale()` says a newer job waits, or after 20 s. The result is then `{ cancelled: true }`.
 * A piece is `{ stream, text }`. The exit `{ code, signal }` is the last piece or the return value of the iterator. A signal with no code counts as a failure.
 */
async function sieve($: Pick<Engine, 'process'>, argv: string[], stale: () => boolean = () => !!queue.job): Promise<{ json?: Json; note?: string; cancelled?: boolean; raw?: string }> {
  let out = ''
  let err = ''
  let code: number | undefined
  // A piece can be bytes. One streaming decoder per stream keeps a multibyte char whole across pieces.
  const dec = { stdout: new TextDecoder(), stderr: new TextDecoder() }
  const textOf = (d: TextDecoder, t: unknown) => (typeof t === 'string' ? t : t == null ? '' : d.decode(t as Uint8Array, { stream: true }))
  const started = Date.now()
  let timer: ReturnType<typeof setTimeout> | undefined
  try {
    const it = ($.process as Json).spawn({ argv: [bin.path, ...argv] })[Symbol.asyncIterator]()
    let next = it.next()
    for (;;) {
      const r: any = await Promise.race([next, new Promise(res => (timer = setTimeout(() => res(TICK), TICK_MS)))])
      clearTimeout(timer)
      if (stale() || Date.now() - started > LIMIT_MS) {
        await it.return?.() // ends the loop: the engine kills the child
        return { cancelled: true, note: 'sieve was cancelled' }
      }
      if (r === TICK) continue
      // The engine gives the exit as the return value of the iterator (done: true), not as a piece.
      const x = r.value
      const isExit = x != null && ('code' in x || 'signal' in x)
      if (isExit) code = typeof x.code === 'number' ? x.code : 1
      if (r.done) break
      next = it.next()
      if (isExit) continue
      if (x.stream === 'stderr') err += textOf(dec.stderr, x.text)
      else out += textOf(dec.stdout, x.text)
    }
  } catch {
    return { note: notFound() }
  }
  if (code === 127) return { note: notFound() }
  if (code !== undefined && code !== 0) {
    // The CLI prints `[sieve] ` notices on stderr before the error. The error line skips them.
    const line = firstLine(err.split('\n').filter(l => !l.startsWith('[sieve] ')).join('\n') || out)
    // An old binary does not know a new command or flag. Name the binary and its version.
    if (/no command named|unknown (flag|option)|unexpected argument/.test(err || line) && argv[0] !== '--version') {
      const v = await sieve($, ['--version'], stale)
      return { note: `${bin.path} (version ${firstLine(v.raw ?? '') || 'unknown'}) is too old for \`sieve ${argv[0]}\`; fix "bin" in .sieve/config.json` }
    }
    // The CLI error line is `sieve: <what> — <what to run>`.
    return { note: /no (graph|index)/.test(line) ? 'no Sieve graph in this project; run sieve build' : line.replace(/^sieve: /, '') }
  }
  const json = parseJson(out)
  return json !== undefined ? { json } : { raw: out, note: `sieve gave output that is not JSON (${out.length} chars: ${JSON.stringify(out.slice(0, 40))})` }
}

/** Parses the output. A stray line before or after the object (a notice or a footer) is cut off. */
function parseJson(out: string): Json | undefined {
  try {
    return JSON.parse(out)
  } catch {}
  const a = out.indexOf('{')
  const b = out.lastIndexOf('}')
  try {
    return a >= 0 && b > a ? JSON.parse(out.slice(a, b + 1)) : undefined
  } catch {
    return undefined
  }
}

/** The error when the binary does not start. It names the path, so the owner knows which `"bin"` to fix. */
const notFound = () => (bin.path === 'sieve' ? 'sieve not found on PATH' : `cannot start ${bin.path}; fix "bin" in .sieve/config.json`)

/** Reads `"bin"` again at each run, so a changed config applies at once. A `$` with no session or fs makes it `sieve`. */
async function refreshBin($: Json): Promise<void> {
  bin.path = await readBin($)
}

// `--` ends the flags, so a name or a path that starts with `-` stays an argument.
async function query($: Pick<Engine, 'process'>, file: string, tool: string, line: number, deep = true): Promise<SievePaneView | undefined> {
  const why = await sieve($, ['why', '--json', '--', `${file}:${line}`])
  if (!why.json) return { file, tool, why: [], tree: [], note: why.note }
  if (queue.job) return undefined // a newer job waits: stop, the view is stale
  const name: string | undefined = why.json.symbols?.[0]?.name
  const callers = name ? (await sieve($, ['callers', '--in', file, '--depth', '2', '--json', '--', name])).json : undefined
  const hits: Json[] = (callers?.matches ?? []).flatMap((x: Json) => x.hits ?? [])
  const d1 = hits.filter(h => h.depth === 1)
  const kids = new Map<string, Json[]>()
  if (d1.length === 1) kids.set(d1[0].id, rowsOf(hits))
  else if (deep) {
    // Several depth-1 callers: ask each shown one for its own callers, to find who owns which depth-2 hit.
    const d2 = new Set(rowsOf(hits).map(h => h.id))
    for (const h of d1.slice(0, 4)) {
      if (queue.job) return undefined // a newer job waits: stop, the view is stale
      const c = await sieve($, ['callers', '--in', String(h.path), '--json', '--', h.name])
      kids.set(h.id, ((c.json?.matches ?? []) as Json[]).flatMap(x => x.hits ?? []).filter((x: Json) => d2.has(x.id)))
    }
  }
  if (queue.job) return undefined // the last call may have ended after a newer job came in
  return makeView(file, tool, why.json, callers, kids)
}

/** The dirs of `map --json`: the top-level `dirs` (old shape) and every `scopes[].dirs` (new shape). */
const mapDirs = (m: Json): Json[] => [...((m?.dirs ?? []) as Json[]), ...((m?.scopes ?? []) as Json[]).flatMap(sc => sc.dirs ?? [])]

/** The map view: the 4 biggest folders and the 4 most called hubs of `sieve map`. The CLI and the MCP tool give the same view. */
export async function mapView($: Pick<Engine, 'process'>, tool: string): Promise<SievePaneView | undefined> {
  const map = await sieve($, ['map', '--json'])
  if (!map.json) return { file: 'map', tool, why: [], tree: [], note: map.note }
  if (queue.job) return undefined // a newer job waits: stop, the view is stale
  const dirs = mapDirs(map.json)
  const folders = dirs.map(d => ({ path: String(d.path), n: Number(d.symbols) || 0 })).sort((x, y) => y.n - x.n).slice(0, 4)
  const symbols = dirs
    .flatMap(d => d.hubs ?? [])
    .map((h: Json) => ({ name: String(h.name), line: Number(lineOf(h.span)) || 1, n: Number(h.inDegree) || 0, path: String(h.path) }))
    .sort((x: Json, y: Json) => y.n - x.n)
    .slice(0, 4)
  return { file: 'map', tool, why: [], tree: [], folders, symbols }
}

/** The view of `sieve why <arg>`: `path:line` and a symbol name give the symbol view, a code file gives the file view. */
export async function whyView($: Pick<Engine, 'process'>, arg: string, tool: string): Promise<SievePaneView | undefined> {
  const at = /^(.+):(\d+)$/.exec(arg)
  if (at) return query($, at[1]!, tool, Number(at[2]), false)
  return isCode(arg) ? fileView($, arg) : symbolView($, arg, tool)
}

/**
 * The file view: its 3 most called symbols, then the symbol view of the most called one. It runs at most 4 queries:
 * `symbols` (every symbol of the file, with its caller count), `why` and `callers`. A file with no called symbol runs `symbols` and `why` only.
 * An old binary gives no `inDegree`. Then the view reads the hubs of `map`, as before.
 */
export async function fileView($: Pick<Engine, 'process'>, file: string): Promise<SievePaneView | undefined> {
  const syms = await sieve($, ['symbols', '--json', '--', file])
  const entries = (syms.json?.entries ?? []) as Json[]
  let list: { name: string; line: number; n: number }[]
  if (entries.some(e => typeof e.inDegree === 'number')) {
    list = entries.map(e => ({ name: String(e.name), line: Number(lineOf(e.span)) || 1, n: Number(e.inDegree) || 0 }))
  } else {
    const map = await sieve($, ['map', '--json'])
    if (!map.json) return { file, tool: 'Read', why: [], tree: [], note: map.note }
    list = mapDirs(map.json).flatMap(d => d.hubs ?? []).filter(h => h.path === file).map(h => ({ name: String(h.name), line: Number(lineOf(h.span)), n: Number(h.inDegree) || 0 }))
  }
  if (queue.job) return undefined // a newer job waits: stop, the view is stale
  const symbols = list.filter(x => x.n > 0).sort((x, y) => y.n - x.n).slice(0, 3)
  if (!symbols.length) {
    const why = await sieve($, ['why', '--json', '--', file])
    return { file, tool: 'Read', why: why.json ? whyCards(why.json) : [], tree: [], symbols }
  }
  // Below the short list: the full symbol view of the most called symbol.
  const top = await query($, file, 'Read', symbols[0]!.line, false)
  return top && { ...top, symbols }
}

/** The symbol view of a name, from `sieve callers`. The first match gives the file and the line, as the edit view finds them with `sieve why`. */
export async function symbolView($: Pick<Engine, 'process'>, name: string, tool: string): Promise<SievePaneView | undefined> {
  if (!name) return { file: name, tool, why: [], tree: [] }
  const found = await sieve($, ['callers', '--json', '--', name])
  const sym = found.json?.matches?.[0]?.symbol
  if (!sym?.path) return { file: name, tool, why: [], tree: [], note: found.note ?? `no symbol named ${name}` }
  if (queue.job) return undefined // a newer job waits: stop, the view is stale
  return query($, String(sym.path), tool, Number(lineOf(sym.span)) || 1, false)
}

/** The symbol view of the top hit of `sieve query`. A top hit that is not a symbol gives a note. */
export async function askView($: Pick<Engine, 'process'>, text: string, tool: string): Promise<SievePaneView | undefined> {
  if (!text) return { file: text, tool, why: [], tree: [] }
  const found = await sieve($, ['query', '--json', '--', text])
  const top = found.json?.hits?.[0]
  const at = top?.kind === 'symbol' ? /^(.+):L(\d+)/.exec(String(top.pointer)) : null
  if (!at) return { file: text, tool, why: [], tree: [], note: found.note ?? 'the top hit is not a symbol' }
  if (queue.job) return undefined // a newer job waits: stop, the view is stale
  return query($, at[1]!, tool, Number(at[2]), false)
}

/** The old command names, each with the new name it stands for. */
const OLD_SUBS: Record<string, string> = { ask: 'query', grep: 'find', blast: 'impact', skeleton: 'symbols' }
const SUBS = new Set(['query', 'find', 'callers', 'impact', 'map', 'symbols', 'why', ...Object.keys(OLD_SUBS)])
const VALUE_FLAGS = new Set(['--depth', '--in', '--format', '--limit', '--max', '--budget'])
/** The MCP tool names (new and old) and the command each one stands for. */
const MCP_SUBS: Record<string, string> = { find_code: 'query', find_all: 'find', trace_calls: 'callers', file_api: 'symbols', repo_map: 'map', check_freshness: 'check', ask: 'query', grep: 'find', skeleton: 'symbols', why: 'why' }

/** Splits a shell command into simple commands of words. Quotes keep a word whole. `;`, `&`, `|` and a newline end a command. `>` and `<` end the words of a command. */
function shellWords(command: string): string[][] {
  const cmds: string[][] = []
  let words: string[] = []
  let word: string | undefined
  let quote = ''
  let cut = false // a redirect: the rest of this command is not an argument
  const endWord = () => {
    if (word !== undefined && !cut) words.push(word)
    word = undefined
  }
  const endCmd = () => {
    endWord()
    if (words.length) cmds.push(words)
    words = []
    cut = false
  }
  for (const ch of command) {
    if (quote) {
      if (ch === quote) quote = ''
      else word = (word ?? '') + ch
    } else if (ch === '"' || ch === "'") {
      quote = ch
      word = word ?? ''
    } else if (/\s/.test(ch) && ch !== '\n') endWord()
    else if (ch === ';' || ch === '&' || ch === '|' || ch === '\n') endCmd()
    else if (ch === '>' || ch === '<') {
      if (word !== undefined && /^\d+$/.test(word)) word = undefined // the 2 of 2>
      endWord()
      cut = true
    } else word = (word ?? '') + ch
  }
  endCmd()
  return cmds
}

/** The first `sieve <sub>` of a Bash command, also in a joined command, after an env prefix, as `npx sieve` or as a path that ends in `/sieve`. `arg` is the main argument. */
export function sieveCommand(command: string): { sub: string; arg: string } | undefined {
  for (const cmd of shellWords(command)) {
    let i = 0
    while (/^[A-Za-z_]\w*=/.test(cmd[i] ?? '')) i++
    if (cmd[i] === 'npx') i++
    const bin = cmd[i] ?? ''
    const typed = cmd[i + 1] ?? ''
    if (!(bin === 'sieve' || bin.endsWith('/sieve')) || !SUBS.has(typed)) continue
    const sub = OLD_SUBS[typed] ?? typed
    const words: string[] = []
    const rest = cmd.slice(i + 2)
    for (let j = 0; j < rest.length; j++) {
      const w = rest[j]!
      if (w === '--') continue
      if (!w.startsWith('-')) words.push(w)
      else if (!w.includes('=') && (VALUE_FLAGS.has(w) || /^\d+$/.test(rest[j + 1] ?? ''))) j++ // the value of `--flag N` or `-x N`
    }
    return { sub, arg: sub === 'query' ? words.join(' ') : (words[0] ?? '') }
  }
  return undefined
}

/** The sum of every `🎯 … · ~N tokens saved` line. N is a number or a `k` number such as `1.9k`. */
export function savedTokens(text: string): number {
  let sum = 0
  for (const m of text.matchAll(/🎯 [^\n"\\]*? · ~?(\d+(?:\.\d)?)(k?) tokens saved/g)) sum += Math.round(Number(m[1]) * (m[2] ? 1000 : 1))
  return sum
}

/** The query of a Sieve Bash call or a Sieve MCP call, or undefined for any other call. */
function sieveQuery(e: Json, ran: Json): Query | undefined {
  const tool = String(e.tool)
  let found: { sub: string; arg: string } | undefined
  if (tool === 'Bash') found = sieveCommand(String(e.command ?? ''))
  else if (tool.startsWith('mcp__sieve__')) {
    // The MCP input fields sit flat on `e`, as `command` does for Bash (claude-code.d.ts, McpToolCallInputFallback).
    const arg = [e.pattern, e.symbol, e.query, e.file, e.path, e.name, e.question].find(v => typeof v === 'string')
    const name = tool.slice('mcp__sieve__'.length).replace(/^sieve_/, '') // the tool is `sieve_callers`, so the full name is mcp__sieve__sieve_callers
    found = { sub: MCP_SUBS[name] ?? name, arg: arg ?? '' }
  }
  return found && { ...found, saved: savedTokens(String(ran?.text ?? '')) }
}

/** The newest job waits here. The tool.call hook writes it. The worker takes it. */
export const queue: { job?: Job; busy: boolean } = { busy: false }

/** Queues a job. A job that is already running is stale now: its `sieve` loop sees `queue.job` and ends. */
export function setJob(job: Job) {
  queue.job = job
}

/**
 * The tool.call hook. It awaits next(e) first and returns the result unchanged.
 * Then it records the job. It makes no query: the docs do not say that `$` stays valid after a hook returns.
 */
export async function onToolCall($: Pick<Engine, 'session'>, e: Json, next: (e: Json) => Promise<any>) {
  const ran = await next(e)
  try {
    const tool = String(e.tool)
    const cwd = normalize(await $.session.cwd())
    const q = sieveQuery(e, ran)
    if (q) {
      statsRun.last = -Infinity // a Sieve call changed the stats: the next tick refetches
      const abs = normalize(q.arg.startsWith('/') ? q.arg : `${cwd}/${q.arg}`)
      const isFile = q.sub === 'symbols' && !!q.arg && isCode(q.arg) && abs.startsWith(`${cwd}/`)
      setJob({ file: isFile ? abs.slice(cwd.length + 1) : q.arg, path: abs, tool: q.sub, input: e, at: Date.now(), query: q, isFile })
      return ran
    }
    const path = normalize(String(e.file_path ?? ''))
    if (['Read', ...EDITS].includes(tool) && path.startsWith(`${cwd}/`)) {
      setJob({ file: path.slice(cwd.length + 1), path, tool, input: e, at: Date.now() })
    }
    if (EDITS.includes(tool) && path.startsWith(`${cwd}/`)) blast.dirty = true
  } catch {} // A failure never reaches the tool result.
  return ran
}

/** One worker step: take the newest job, run at most one query, drop the result when a newer job came in. */
export async function step($: Pick<Engine, 'process' | 'fs'>, put: Put): Promise<void> {
  const job = queue.job
  if (queue.busy || !job) return
  queue.busy = true
  queue.job = undefined
  try {
    await refreshBin($)
    const text = job.tool === 'Edit' || job.tool === 'MultiEdit' ? String(await $.fs.read(job.path).catch(() => '')) : ''
    const whole = job.tool === 'Read' && !job.input.offset && !job.input.limit // a whole-file Read: line 1 is an import, not a symbol
    const line = whole || job.query ? 1 : findLine(job.tool, job.input, text)
    const v: SievePaneView | undefined = job.query
      ? job.isFile
        ? await fileView($, job.file)
        : job.query.sub === 'callers'
          ? await symbolView($, job.query.arg, job.tool)
          : job.query.sub === 'query'
            ? await askView($, job.query.arg, job.tool)
            : job.query.sub === 'map'
              ? await mapView($, job.tool)
              : job.query.sub === 'why'
                ? await whyView($, job.query.arg, job.tool)
                : { file: job.file, tool: job.tool, why: [], tree: [] }
      : whole
      ? await fileView($, job.file)
      : line === undefined
        ? { file: job.file, tool: job.tool, why: [], tree: [], note: 'edit line not found in the file' }
        : await query($, job.file, job.tool, line)
    if (v) Object.assign(v, { at: job.at, query: job.query })
    if (v && !queue.job) await put(v)
    if (v?.query?.saved) statsRun.last = -Infinity // a saving changed the stats: the next tick refetches
  } catch (err) {
    await put({ file: job.file, tool: job.tool, why: [], tree: [], note: `sieve-pane error: ${String(err)}` }).catch(() => {})
  } finally {
    queue.busy = false
  }
}

/** The band data from `sieve impact --format json`, or null when nothing changed. */
export function bandInfo(b: Json): Band | null {
  const seeds: Json[] = b?.seeds ?? []
  if (!seeds.length) return null
  const hit: Json[] = b.impacted ?? []
  const first = seeds.find(x => !x.wholeFile) ?? seeds[0]
  const more = seeds.length - 1
  return {
    sym: String(first.name ?? base(String(first.path))),
    more,
    n: hit.length,
    hops: Math.max(0, ...hit.map(h => Number(h.depth) || 0)),
    routes: hit.filter(h => routeOf(h)).length, // one per handler row, as the tree counts
  }
}

/** The Tokyo Night colors. `bg` is the pane background. `text` (fg_dark) is the dim text color. `dim` is for borders, bar tracks and the dimmed mascot only: it gives 2.8:1 on the pane background. */
export const C = { fg: '#c0caf5', dim: '#565f89', text: '#9aa5ce', blue: '#7aa2f7', cyan: '#7dcfff', purple: '#bb9af7', green: '#9ece6a', orange: '#ff9e64', red: '#f7768e', yellow: '#e0af68', border: '#3b4261', bg: '#1a1b26' }

/** The level of a caller count: green for 1 to 5, orange for 6 to 20, red above 20. */
export const level = (n: number) => (n > 20 ? 'red' : n > 5 ? 'orange' : 'green')
export const levelColor = (n: number) => C[level(n)]

/** The band text, in plain words. `more` counts the other changed symbols. */
export const bandLine = (b: Band) =>
  `✎ ${b.sym}${b.more ? ` and ${b.more} more changed` : ''} → ${b.n} ${b.n === 1 ? 'caller' : 'callers'} affected${b.routes ? ` in ${b.routes} ${b.routes === 1 ? 'route' : 'routes'}` : ''}`

/** A new edit sets dirty. The worker clears it. */
export const blast: { dirty: boolean; busy: boolean; last: number } = { dirty: false, busy: false, last: -Infinity }
const MIN_GAP_MS = 2000

/** One blast step: run at most once per 2 seconds, and only after a new edit. A failure clears the band. */
export async function blastStep($: Pick<Engine, 'process'> & Partial<Pick<Engine, 'session' | 'fs'>>, now: number, putBand: (b: Band | null) => Promise<void>): Promise<void> {
  if (blast.busy || !blast.dirty || now - blast.last < MIN_GAP_MS) return
  blast.busy = true
  blast.dirty = false
  blast.last = now
  try {
    await refreshBin($)
    const out = await sieve($, ['impact', '--format', 'json', '--no-owners'], () => false)
    if (!blast.dirty) await putBand(out.json ? bandInfo(out.json) : null) // a newer edit wins
  } catch {} // A failing putBand is dropped. A blast failure puts null: the band clears.
  finally {
    blast.busy = false
  }
}

/** The AbovePrompt hook: no band or a survey on screen, no band. */
export function bandRender($: Pick<Engine, 'ui'>, e: Json, next: (e: Json) => any, b: Band | null) {
  if (!b || e.props?.hasSurvey) return next(e)
  const { Box, Text } = $.ui.resolve(e)
  return (
    <Box>
      <Text color={levelColor(b.n)}>{bandLine(b)}</Text>
    </Box>
  )
}

// --- the savings numbers ---
const STATS_GAP_MS = 30000
export const statsRun: { last: number; busy: boolean } = { last: -Infinity, busy: false }

/** The stats from `sieve stats --json`, or null when the shape is wrong. `spark` holds the 7 days, oldest first. */
export function parseStats(j: Json): Stats | null {
  const t = j?.today?.tokens
  if (typeof t !== 'number') return null
  const days: number[] = ((j.last7days ?? []) as Json[]).map(d => Number(d.tokens) || 0).reverse()
  return { tokens: t, dollars: typeof j.today.dollars === 'number' ? j.today.dollars : null, spark: days, session: Number(j.session?.tokens) || 0 }
}

export const kfmt = (n: number) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n))
export const sparkline = (days: number[]) => {
  const top = Math.max(0, ...days)
  return days.map(v => '▁▂▃▄▅▆▇█'[top ? Math.round((v / top) * 7) : 0]).join('')
}

/** One stats step: at most once per 30 s. A failure hides the section. */
export async function statsStep($: Pick<Engine, 'process'> & Partial<Pick<Engine, 'session' | 'fs'>>, now: number, putStats: (s: Stats | null) => Promise<void>): Promise<void> {
  if (statsRun.busy || now - statsRun.last < STATS_GAP_MS) return
  statsRun.busy = true
  statsRun.last = now
  try {
    await refreshBin($)
    // The current session's id, as the status line reads `session_id`. Without it the CLI reads the newest session file.
    const id: unknown = await Promise.resolve().then(() => ($ as Json).session.id()).catch(() => undefined)
    const out = await sieve($, ['stats', '--json', ...(typeof id === 'string' && id ? ['--session', id] : [])], () => false)
    const s = out.json ? parseStats(out.json) : null
    await putStats(s)
  } catch {} // A failing putStats is dropped.
  finally {
    statsRun.busy = false
  }
}

// --- the mascot ---
const rgb = (h: string) => parseInt(h.slice(1), 16)
const TERM_DEFAULT = 0x01000000 // the terminal's own color, as RasterProps says
const PANE_BG = [0x1a, 0x1b, 0x26] // the same color as `C.bg`
/** Mixes a color 45% toward the pane background: the dimmed mascot. */
const dimmed = (c: number) => PANE_BG.reduce((out, bg, i) => out | (Math.round(bg * 0.45 + ((c >> (16 - 8 * i)) & 255) * 0.55) << (16 - 8 * i)), 0)
export const MASCOT_NAMES = [...Object.keys(MASCOTS), 'none']

const pixel = (name: string, frame: number) => {
  const m = MASCOTS[name]
  return m ? m.frames[frame % m.frames.length]! : undefined // the full frames: 14 pixel rows by 16 columns, 7 cell rows. No crop, no scale.
}

/**
 * The Raster cells of one mascot frame: [codePoint, fg, bg] per cell, row-major.
 * Two pixel rows make one cell row: ▀ has the upper pixel as fg and the lower as bg. Transparent shows the terminal color.
 */
export function mascotWords(name: string, frame: number, dim = false): { columns: number; rows: number; words: Uint32Array } | undefined {
  const px = pixel(name, frame)
  if (!px) return undefined
  const columns = Math.max(...px.map(r => r.length))
  const rows = Math.ceil(px.length / 2)
  const at = (r: number, c: number) => {
    const h = PALETTE[px[r]?.[c] ?? '.']
    return h ? (dim ? dimmed(rgb(h)) : rgb(h)) : undefined
  }
  const words = new Uint32Array(columns * rows * 3)
  for (let r = 0; r < rows; r++)
    for (let c = 0; c < columns; c++) {
      const top = at(2 * r, c)
      const low = at(2 * r + 1, c)
      const cell = top === undefined && low === undefined ? [0x20, TERM_DEFAULT, TERM_DEFAULT] : low === undefined ? [0x2580, top!, TERM_DEFAULT] : top === undefined ? [0x2584, low, TERM_DEFAULT] : [0x2580, top, low]
      words.set(cell, (r * columns + c) * 3)
    }
  return { columns, rows, words }
}

/** Standard padded base64 of the cell words, little-endian (arm64 and x64 are). */
export function packCells(words: Uint32Array): string {
  const bytes = new Uint8Array(words.buffer, words.byteOffset, words.byteLength)
  let s = ''
  for (let i = 0; i < bytes.length; i += 8192) s += String.fromCharCode(...bytes.subarray(i, i + 8192))
  return btoa(s)
}

/** The same frame as SVG markup, for the desktop and the editor. */
export function mascotSvg(name: string, frame: number, dim = false): string | undefined {
  const px = pixel(name, frame)
  if (!px) return undefined
  const rects = px.flatMap((row, y) => [...row].flatMap((ch, x) => (PALETTE[ch] ? [`<rect x="${x}" y="${y}" width="1" height="1" fill="${PALETTE[ch]}"/>`] : [])))
  return `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${Math.max(...px.map(r => r.length))} ${px.length}" shape-rendering="crispEdges" opacity="${dim ? 0.5 : 1}">${rects.join('')}</svg>`
}

// --- the Radar frame ---
const BAR = 14
export const cut = (s: string, n: number) => (s.length <= n ? s : n <= 1 ? s.slice(0, Math.max(n, 0)) : `${s.slice(0, n - 1)}…`)
const clock = (ms: number) => {
  const d = new Date(ms)
  const h = d.getHours()
  return `${h % 12 || 12}:${String(d.getMinutes()).padStart(2, '0')}${h < 12 ? 'am' : 'pm'}`
}
const MAX_ROWS = 4
const CODE = /\.(rs|ts|tsx|js|jsx|mjs|cjs|py|go|java|kt|c|h|cc|cpp|hpp|cs|rb|php|swift|svelte|vue|scala|lua|sh)$/
export const isCode = (file: string) => CODE.test(file)
const NOT_CODE = 'not code · Sieve indexes code files only'

/**
 * The visible tree lines. The last visible row ends with └─, whatever was cut after it.
 * `pre` is the connector part of `text`, so the frame can color it apart from the name.
 * `more` counts the callers not shown: the true total minus the visible rows.
 * ponytail: a glyph counts 1 cell, as in a monospace terminal. `radar` keeps a 1-cell margin.
 */
export function treeLines(tree: Row[], total?: number): { rows: { d: 1 | 2; text: string; pre: number }[]; more: number; indent: string } {
  const seen = tree.slice(0, MAX_ROWS)
  const lastP = seen.length ? seen[seen.length - 1]!.p : -1
  const rows = seen.map((r, i) => {
    const endsGroup = !seen.slice(i + 1).some(x => x.p === r.p)
    const tail = r.tail ? `   ${r.tail}` : ''
    if (r.d === 1) return { d: 1 as const, text: `${r.p === lastP ? '└' : '├'}─● ${r.label}${tail}`, pre: 2 }
    return { d: 2 as const, text: `${r.p === lastP ? '   ' : '│  '}${endsGroup ? '└' : '├'}─○ ${r.label}${tail}`, pre: 5 }
  })
  const depth = seen.length ? seen[seen.length - 1]!.d : 1
  return { rows, more: Math.max(total ?? 0, tree.length) - seen.length, indent: '   '.repeat(depth) }
}

/** The impact of a view: an edit takes the blast of the whole diff from the band, a read keeps the callers count. */
const impactOf = (v: SievePaneView, b: Band | null) => (EDITS.includes(v.tool) && b ? { n: b.n, hops: b.hops } : { n: v.n, hops: v.hops })

/**
 * The mascot mood. `dim` is a red impact level or a SUPERSEDED decision: a calm, visible signal.
 * `rest` keeps frame 0 only when dimmed. Every other view, idle or not, animates.
 */
export function mood(v: SievePaneView | null, b: Band | null): { rest: boolean; dim: boolean } {
  const dim = !!v && ((impactOf(v, b).n ?? 0) > 20 || v.why.some(d => d.superseded))
  return { dim, rest: dim }
}

export type Art = { name: string; frame: number }
const GLYPHS = '▁▂▃▄▅▆▇█'

/** The bar chart of the last days: one glyph pair per day, oldest left, today (the last) green. */
export const chart = (days: number[]) => {
  const top = Math.max(0, ...days)
  return days.map(v => GLYPHS[top ? Math.round((v / top) * 7) : 0]!)
}

/**
 * The Radar tree. `width` is the pane width in cells. The root box has no background: the pane shows the frame color.
 * The root box never shrinks (flexShrink 0), so it grows to fit every row.
 * Pure: it makes no query.
 */
export function radar(ui: { Box: any; Text: any; Raster?: any; Svg?: any }, width: number, v: SievePaneView | null, b: Band | null, s: Stats | null, art: Art = { name: DEFAULT_MASCOT, frame: 0 }) {
  const { Box, Text, Raster, Svg } = ui
  const w = Math.max(width - 2, 24) - 1 // the padding takes 2 cells (the engine's pane draws the frame), the body has 24 at least, 1 cell stays free
  const T = (props: Json, text: string, n = w) => <Text {...props}>{cut(text, n)}</Text>
  const title = (t: string) => T({ bold: true, color: C.purple }, t)
  const blank = () => <Text> </Text> // 1 blank row before a section
  const out: any[] = []
  const edit = !!v && EDITS.includes(v.tool)
  const m = mood(v, b)
  const noncode = !!v && !v.query && !v.symbol && !v.symbols && !isCode(v.file)

  // 1. the header: the mascot, then the name and the meta
  const shape = art.name !== 'none' && (Raster || Svg) ? mascotWords(art.name, m.rest ? 0 : art.frame, m.dim) : undefined
  const showArt = !!shape && w >= shape.columns + 2 + 17 // the text column needs 17 cells, 2 cells stay between the mascot and the text
  const tw = showArt ? w - shape!.columns - 2 : w
  const head: any[] = []
  head.push(T({ bold: true, color: C.fg }, v ? (v.symbols ? base(v.file) : (v.symbol ?? base(v.file))) : 'SIEVE', tw)) // a file view names the file, the symbol below it is the most called one
  if (!v) head.push(<Text color={C.text} wrap="wrap">No file yet. The pane fills when the agent reads or edits a file.</Text>)
  else if (noncode) head.push(<Text color={C.text} wrap="wrap">{NOT_CODE}</Text>)
  else {
    const kind = v.symbols ? undefined : v.kind === 'function' ? 'fn' : v.kind
    const when = v.query ? v.query.sub : edit ? (v.at ? `edited ${clock(v.at)}` : 'edited') : 'read'
    // The title is the file name or the symbol. Never repeat the file name: a file view shows the folder path.
    const folder = v.file.includes('/') ? v.file.slice(0, v.file.lastIndexOf('/')) : ''
    const loc = v.symbols || !v.symbol ? folder : `${base(v.file)}${v.line ? `:${v.line}` : ''}`
    const one = [loc, kind, when].filter(Boolean).join(' · ')
    if (one.length <= tw) head.push(T({ color: C.text }, one, tw))
    else head.push(T({ color: C.text }, [loc, kind].filter(Boolean).join(' · '), tw), T({ color: C.text }, when, tw))
    if (v.note) head.push(<Text color={C.text} wrap="wrap">{v.note.replace(/^[✗✓] /, '')}</Text>)
  }
  const art2 = !showArt ? null : Raster ? <Raster key="mascot" columns={shape!.columns} rows={shape!.rows} cells={packCells(shape!.words)} /> : (
    <Svg source={mascotSvg(art.name, m.rest ? 0 : art.frame, m.dim)!} alt={`${MASCOTS[art.name]!.name}`} width={shape!.columns * 4} height={shape!.rows * 8} />
  )
  out.push(
    <Box gap={2} alignItems="flex-start">
      {art2}
      <Box flexDirection="column" width={tw}>
        {head}
      </Box>
    </Box>,
  )

  if (v?.query) {
    out.push(T({ color: C.cyan }, `sieve ${v.query.sub} ${cut(v.query.arg, 40)}`))
    if (v.query.saved) out.push(T({ bold: true, color: C.green }, `+~${v.query.saved.toLocaleString('en-US')} tokens saved`))
  }

  // 2. the stat tiles
  const imp = v && !noncode ? impactOf(v, b) : { n: undefined, hops: undefined }
  const tiles: [string, string, string][] = []
  if (s) tiles.push(['SESSION', `~${kfmt(s.session)}`, C.green], ['TODAY', `~${kfmt(s.tokens)}`, C.green], ['7 DAYS', `~${kfmt(s.spark.reduce((a, x) => a + x, 0))}`, C.blue])
  // The IMPACT tile shows in the symbol view only. A file view (it has `symbols`) and a non-code view show none.
  if (imp.n !== undefined && v?.symbol && !v.symbols) tiles.push(['IMPACT', String(imp.n), levelColor(imp.n)])
  if (tiles.length) {
    // Each tile fits its text, 4 cells wider (border and padding). A tile that does not fit the rest of the row starts the next row. The gap is 1 cell in every row.
    const fit = tiles.map(([k, val]) => Math.min(w, Math.max(k.length, val.length) + 4))
    const rows: number[][] = []
    let used = 0
    fit.forEach((f, i) => {
      if (!rows.length || used + 1 + f > w) (rows.push([i]), (used = f))
      else (rows[rows.length - 1]!.push(i), (used += 1 + f))
    })
    for (const row of rows)
      out.push(
        <Box gap={1}>
          {row.map(i => {
            const [k, val, color] = tiles[i]!
            const tile = fit[i]!
            return (
              <Box flexDirection="column" borderStyle="round" borderColor={C.border} paddingX={1} width={tile}>
                {T({ color: C.text }, k, tile - 4)}
                {T({ bold: true, color }, val, tile - 4)}
              </Box>
            )
          })}
        </Box>,
      )
  }

  if (v && !noncode) {
    if (v.symbols) {
      if (v.folders) {
        out.push(blank(), title('TOP FOLDERS'))
        out.push(...(v.folders.length ? v.folders.map(f => T({ color: C.cyan }, `${f.path}   ${f.n} symbols`)) : [T({ color: C.text }, 'none')]))
      }
      out.push(blank(), title(v.folders ? 'HUBS' : 'MOST CALLED SYMBOLS'))
      out.push(...(v.symbols.length ? v.symbols.map(c => T({ color: C.cyan }, `${c.name}${c.path ? `  ${c.path}` : `:${c.line}`}   ${c.n} ${c.n === 1 ? 'caller' : 'callers'}`)) : [T({ color: C.text }, 'none')]))
    }
    if (v.symbol) {
      // 3. who depends on this
      out.push(blank(), title('WHO DEPENDS ON THIS'))
      out.push(T({ bold: true, color: C.fg }, `◉ ${v.symbol}`))
      const tl = treeLines(v.tree, v.n)
      out.push(
        ...tl.rows.map(r => (
          <Box>
            {T({ color: C.dim }, r.text.slice(0, r.pre), w)}
            {T({ color: r.d === 1 ? C.cyan : C.blue }, r.text.slice(r.pre), w - r.pre)}
          </Box>
        )),
      )
      if (tl.more > 0) out.push(T({ color: C.text }, `${tl.indent}+${tl.more} more`))
      if (!v.tree.length) out.push(T({ color: C.text }, 'no callers'))
      // 4. the impact bar
      if (imp.n !== undefined) {
        const on = Math.min(BAR, Math.round(imp.n * 1.4))
        out.push(blank(), title('IMPACT'))
        out.push(T({ color: levelColor(imp.n) }, `${'█'.repeat(on)}${'░'.repeat(BAR - on)}  ${imp.n} ${imp.n === 1 ? 'caller' : 'callers'} · ${imp.hops} ${imp.hops === 1 ? 'hop' : 'hops'}`))
      }
    }
    // 5. why it exists
    if (v.symbol || (v.symbols && !v.folders)) {
      out.push(blank(), title('WHY IT EXISTS'))
      if (!v.why.length) out.push(T({ color: C.text }, 'no decision recorded'))
      for (const d of v.why) {
        const mark = d.superseded ? C.red : C.yellow
        out.push(
          <Box>
            {T({ color: mark }, '▍', 1)}
            {T({ color: d.superseded ? C.red : C.fg }, `${d.date ? `${d.date}  ` : ''}${d.heading}`, w - 1)}
          </Box>,
        )
        if (d.superseded) out.push(<Box>{T({ bold: true, color: C.red }, `▍⚠ SUPERSEDED${d.by ? ` by ${d.by}` : ''}`)}</Box>)
      }
    }
    // 6. the 7-day chart
    if (s && s.spark.length) {
      out.push(blank(), title('SAVED · LAST 7 DAYS'))
      out.push(
        <Box gap={1}>
          {chart(s.spark).map((g, i, all) => (
            <Text color={i === all.length - 1 ? C.green : C.blue}>{g + g}</Text>
          ))}
        </Box>,
      )
    }
  }
  return (
    <Box flexDirection="column" paddingX={1} paddingTop={1} flexGrow={1} flexShrink={0}>
      {out}
    </Box>
  )
}

// --- the animation ---
/** What the worker needs to animate. The Pane render hook fills `view`, `band`, `stats`, `open` and `drawn`. */
export const anim: {
  name: string
  open: boolean
  isRaster: boolean
  ticks: number
  frame: 0 | 1
  wag: number
  seen?: number
  drawn: string
  view: SievePaneView | null
  band: Band | null
  stats: Stats | null
} = { name: DEFAULT_MASCOT, open: false, isRaster: false, ticks: 0, frame: 0, wag: 0, drawn: '', view: null, band: null, stats: null }

const artKey = (name: string, frame: number, dim: boolean) => `${name}:${frame}:${dim}`

/**
 * One animation step, run from the 300 ms worker tick. It works only while the pane is open, on every 2nd tick (600 ms).
 * A new saving shows frame 1 for 2 steps. Else the 2 frames alternate. A red impact or a SUPERSEDED decision rests on frame 0, dimmed.
 * It blits (Raster) or invalidates (Svg) only when the frame, the dim flag or the mascot changed.
 * Once per step it asks `$.ui.panes()`. If the pane is not listed, the pane closed: the animation stops until the next draw.
 */
export async function animStep($: Pick<Engine, 'ui'>, a = anim): Promise<void> {
  if (!a.open || a.name === 'none' || ++a.ticks % 2) return
  const listed = await $.ui.panes().then(ps => ps.some(p => p.id === PANE), () => false)
  if (!listed) return void Object.assign(a, { open: false, drawn: '' })
  const m = mood(a.view, a.band)
  const t = a.stats?.tokens
  if (t !== undefined && a.seen !== undefined && t > a.seen) a.wag = 2
  if (t !== undefined) a.seen = t
  if (m.rest) a.wag = 0
  a.frame = m.rest ? 0 : a.wag > 0 ? (a.wag--, 1) : a.frame ? 0 : 1
  const key = artKey(a.name, a.frame, m.dim)
  if (key === a.drawn) return
  a.drawn = key
  try {
    if (!a.isRaster) return void $.ui.invalidate('ui.render')
    const shape = mascotWords(a.name, a.frame, m.dim)
    if (!shape) return
    const r = await $.ui.blit({ requestId: PANE, key: 'mascot', cells: packCells(shape.words) })
    if (r?.deny) Object.assign(a, { open: false, drawn: '' })
  } catch {
    Object.assign(a, { open: false, drawn: '' }) // a blit that throws stops the animation until the next draw
  }
}

/** The binary the pane spawns. `readBin` sets it at session start. */
export const bin = { path: 'sieve' }

/** The `bin` path from the project's `.sieve/config.json`, or `sieve`. Never throws. */
export async function readBin($: Pick<Engine, 'session' | 'fs'>): Promise<string> {
  try {
    const cwd = normalize(await $.session.cwd())
    const b = JSON.parse(String(await $.fs.read(`${cwd}/.sieve/config.json`))).bin
    return typeof b === 'string' && b !== '' ? b : 'sieve'
  } catch {
    return 'sieve'
  }
}

/** The mascot name from the project's `.sieve/config.json`, or undefined. Never throws. */
export async function readMascot($: Pick<Engine, 'session' | 'fs'>): Promise<string | undefined> {
  try {
    const cwd = normalize(await $.session.cwd())
    const name = JSON.parse(String(await $.fs.read(`${cwd}/.sieve/config.json`))).mascot
    return MASCOT_NAMES.includes(name) ? name : undefined
  } catch {
    return undefined
  }
}

/** The one worker tick. The flag resets when the module reloads, as the timer ends then. */
export const worker = { started: false }

/** Starts the one 300 ms tick: the pane step, the blast step, the stats step, then the animation step. */
export function startWorker(
  $: Pick<Engine, 'clock' | 'process' | 'fs' | 'state' | 'ui'>,
  put: Put,
  putBand: (b: Band | null) => Promise<void>,
  putStats: (s: Stats | null) => Promise<void> = async () => {},
) {
  if (worker.started) return
  worker.started = true
  $.clock.every(300, async () => {
    await step($, put).catch(() => {})
    const now = Date.now() // not $.clock.now(): its unit is not in the types, and a wrong unit blocks the 30 s gap
    await blastStep($, now, putBand).catch(() => {})
    await statsStep($, now, putStats).catch(() => {})
    await animStep($).catch(() => {})
  })
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({ name: 'sieve-pane', description: 'Open the Sieve pane: the callers, the impact and the why of the code' })
    await $.command.register({ name: 'sieve-mascot', description: `Set and save the pane mascot: ${MASCOT_NAMES.join(', ')}` })
    anim.name = (await readMascot($)) ?? DEFAULT_MASCOT
    bin.path = await readBin($)
    // The worker starts here, with this hook's $. reference.md, "Work that outlives a dispatch", lines 129-133:
    // "Work meant to outlive a dispatch belongs elsewhere: start it from a `session.start` hook, ...
    //  and keep it going with `$.clock.every` and `$.clock.after`, whose timers run until cancelled or until the module reloads."
    const put: Put = async v => void (await update($, view, () => v))
    const putBand = async (b: Band | null) => void (await update($, band, () => b))
    const putStats = async (s: Stats | null) => void (await update($, stats, () => s))
    startWorker($, put, putBand, putStats)
    return next(e)
  })

  on('command.run', { command: 'sieve-pane' }, async $ => {
    await $.ui.open({ id: PANE, title: 'Sieve' })
    return {} // No text: the model reads the command output.
  })

  // The choice goes to <project>/.sieve/config.json, which the status line reads too.
  on('command.run', { command: 'sieve-mascot' }, async ($, e) => {
    const name = String(e.args ?? '').trim().toLowerCase()
    if (!MASCOT_NAMES.includes(name)) return $.ui.toast(`Usage: /sieve-mascot <${MASCOT_NAMES.join('|')}>. Now: ${anim.name}.`), {}
    const file = `${normalize(await $.session.cwd())}/.sieve/config.json`
    let cfg: Json = {}
    try {
      const text = String(await $.fs.read(file))
      try {
        cfg = JSON.parse(text)
        if (!cfg || typeof cfg !== 'object' || Array.isArray(cfg)) throw 0
      } catch {
        return $.ui.toast(`${file} is not valid JSON. The mascot is not saved.`), {}
      }
    } catch {} // no file yet: start a new one
    try {
      await $.fs.write(file, JSON.stringify({ ...cfg, mascot: name }, null, 2) + '\n')
    } catch {
      return $.ui.toast(`Cannot write ${file}. The mascot is not saved.`), {}
    }
    Object.assign(anim, { name, drawn: '' })
    $.ui.invalidate('ui.render')
    $.ui.toast(`Sieve mascot: ${name} (saved to .sieve/config.json)`)
    return {} // No text: the model reads nothing.
  })

  on('tool.call', ($, e, next) => onToolCall($, e, next))

  on('ui.render', { component: 'Pane', requestId: PANE }, async ($, e) => {
    const ui = $.ui.resolve(e) as Json
    const [v, b, s] = [await read($, view), await read($, band), await read($, stats)]
    // Record what this draw shows, so the worker blits only a change.
    if (!anim.open) anim.seen = s?.tokens
    const m = mood(v, b)
    Object.assign(anim, { open: true, isRaster: !!ui.Raster, view: v, band: b, stats: s, drawn: artKey(anim.name, m.rest ? 0 : anim.frame, m.dim) })
    return radar(ui as never, Number(e.props?.bodyColumns) || Number(e.viewport?.columns) || 44, v, b, s, { name: anim.name, frame: anim.frame })
  })

  on('ui.render', { component: 'AbovePrompt' }, async ($, e, next) => bandRender($, e, next, await read($, band)))
}
