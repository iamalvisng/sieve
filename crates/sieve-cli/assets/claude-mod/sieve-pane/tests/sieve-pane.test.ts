import { expect, test } from 'claude-code/testing'
import { setJob, register, startWorker, worker, bandRender, bandInfo, bandLine, blast, blastStep, findLine, makeView, normalize, onToolCall, sieveCommand, savedTokens, treeLines, queue, step, radar, level, cut, sparkline, parseStats, statsStep, statsRun, routeOf, animStep, anim, mascotWords, packCells, mascotSvg, mood, chart, readMascot, C } from '../hooks/register'
import { MASCOTS, PALETTE, DEFAULT_MASCOT } from '../hooks/mascots'
import mascotJson from '../assets/mascots-raw'

const text = 'a\nb\nfoo bar\nc\n'

test('findLine: Read uses the offset or line 1', () => {
  expect(findLine('Read', { offset: 40 }, '')).toBe(40)
  expect(findLine('Read', {}, '')).toBe(1)
})

test('findLine: Edit finds the line of the text, Write is line 1', () => {
  expect(findLine('Edit', { old_string: 'foo', new_string: 'foo bar' }, text)).toBe(3)
  expect(findLine('Edit', { old_string: 'foo bar', new_string: 'x' }, text)).toBe(3)
  expect(findLine('MultiEdit', { edits: [{ old_string: 'c', new_string: 'zz' }] }, text)).toBe(4)
  expect(findLine('Write', {}, text)).toBe(1)
})

test('findLine: a text that occurs twice or not at all gives no line', () => {
  expect(findLine('Edit', { old_string: 'q', new_string: 'a\nb' }, 'a\nb a\nb')).toBeUndefined()
  expect(findLine('Edit', { old_string: 'q', new_string: 'zz' }, text)).toBeUndefined()
})

const why = {
  symbols: [
    {
      name: 'link_json',
      decisions: [
        { heading: 'F6 links by anchors', doc: 'docs/L.md', line: 9, superseded: false },
        { heading: 'Old rule', doc: 'docs/L.md', line: 3, superseded: true, supersededBy: 'F6' },
        { heading: 'c', doc: 'd', line: 1 },
        { heading: 'd', doc: 'd', line: 2 },
      ],
    },
  ],
}
const callers = { matches: [{ hits: [{ name: 'render_json', path: 'a.rs', span: 'L10-L20', depth: 1, id: 'r' }] }] }

test('view: the why cards keep the first 2 decisions and the SUPERSEDED flag', () => {
  const v = makeView('a.rs', 'Read', why, callers)
  expect(v.symbol).toBe('link_json')
  expect(v.why).toEqual([
    { date: '', heading: 'F6 links by anchors', superseded: false, by: undefined },
    { date: '', heading: 'Old rule', superseded: true, by: 'F6' },
  ])
  expect(v.n).toBe(1)
  expect(v.tree.map(r => r.label)).toEqual(['render_json'])
})

test('view: the sieve not found line', () => {
  const v = { file: 'a.rs', tool: 'Read', why: [], tree: [], note: 'sieve not found on PATH' }
  expect(lines(radar(R, 40, v, null, null))).toContain('sieve not found on PATH')
})

test('normalize resolves .. so a path cannot leave the project dir', () => {
  expect(normalize('/p/a/../../x/./y')).toBe('/x/y')
})

// The fake Box and Text draw plain nodes. `texts` lists each Text node's text and props.
const R = { Box: 'Box', Text: 'Text' }
// A plain Box (no border, width or gap) whose children are all Text is one line: its Texts join, `parts` keeps them.
type Tx = { text: string; props: any; parts?: Tx[] }
const texts = (t: any): Tx[] => {
  if (t == null || typeof t === 'string') return []
  if (Array.isArray(t)) return t.flatMap(texts)
  if (t.type === 'Text') return [{ text: t.children.join(''), props: t.props ?? {} }]
  const kids = [t.children].flat(Infinity).filter((k: any) => k != null && k !== false)
  const p = t.props ?? {}
  if (t.type === 'Box' && !p.borderStyle && !p.width && !p.gap && kids.length > 1 && kids.every((k: any) => k.type === 'Text')) {
    const parts = kids.flatMap(texts)
    return [{ text: parts.map(x => x.text).join(''), props: parts[parts.length - 1]!.props, parts }]
  }
  return texts(t.children)
}
// A node of one element type, anywhere in the tree.
const nodes = (t: any, type: string): any[] => (t == null || typeof t !== 'object' ? [] : Array.isArray(t) ? t.flatMap(x => nodes(x, type)) : [...(t.type === type ? [t] : []), ...nodes(t.children, type)])
const lines = (t: any) => texts(t).map(x => x.text)

// A fake $. The process answers from `answer`. Each call is recorded in `argvs`.
const fake = (answer: (argv: string[]) => Promise<any>, files: Record<string, string> = {}) => {
  const argvs: string[][] = []
  const ended: string[][] = []
  const $ = {
    session: { cwd: async () => '/p' },
    fs: { read: async (f: string) => files[f] ?? Promise.reject(new Error('ENOENT')) },
    // spawn gives the answer as pieces; `ended` lists the argv whose loop ended early (the engine kills that child).
    process: {
      spawn: ({ argv }: { argv: string[] }) => ({
        [Symbol.asyncIterator]() {
          argvs.push(argv)
          const parts: any[] = []
          let i = 0
          return {
            next: async () => {
              if (!parts.length) {
                const r = await answer(argv).catch(() => undefined)
                if (!r) throw new Error('ENOENT')
                parts.push({ stream: 'stdout', text: r.stdout }, { stream: 'stderr', text: r.stderr }, { code: r.exitCode, signal: null })
              }
              return i < parts.length ? { done: false, value: parts[i++] } : { done: true, value: undefined }
            },
            return: async () => (ended.push(argv), { done: true, value: undefined }),
          }
        },
      }),
    },
  }
  return { $: $ as never, argvs, ended }
}
const ok = (stdout: object) => Promise.resolve({ exitCode: 0, stdout: JSON.stringify(stdout), stderr: '' })
const whyOut = { symbols: [{ name: 'f', decisions: [] }] }
const reset = () => {
  queue.job = undefined
  queue.busy = false
}
const tick = async () => { for (let i = 0; i < 10; i++) await Promise.resolve() }

test('onToolCall: next runs first and its result comes back unchanged', async () => {
  reset()
  const { $ } = fake(() => ok({}))
  const order: string[] = []
  const result = { result: 'R' }
  const ran = await onToolCall($, { tool: 'Read', file_path: '/p/a.rs' }, async () => {
    order.push(queue.job ? 'job' : 'next')
    return result
  })
  expect(order).toEqual(['next'])
  expect(ran).toBe(result)
  expect(queue.job?.file).toBe('a.rs')
})

test('onToolCall: a throw in the job code never reaches the tool result', async () => {
  reset()
  const $ = { session: { cwd: () => Promise.reject(new Error('boom')) } } as never
  const result = { result: 'R' }
  expect(await onToolCall($, { tool: 'Read', file_path: '/p/a.rs' }, async () => result)).toBe(result)
})

test('onToolCall: outside paths, .. paths and other tools make no job', async () => {
  reset()
  const { $ } = fake(() => ok({}))
  for (const e of [
    { tool: 'Read', file_path: '/other/a.rs' },
    { tool: 'Read', file_path: '/p/../other/a.rs' },
    { tool: 'Read', file_path: '/pp/a.rs' },
    { tool: 'Bash', file_path: '/p/a.rs' },
  ]) {
    await onToolCall($, e, async () => ({}))
    expect(queue.job).toBeUndefined()
  }
  await onToolCall($, { tool: 'Read', file_path: '/p/sub/../a.rs' }, async () => ({}))
  expect(queue.job?.file).toBe('a.rs')
})

test('step: the argv sent to sieve is exact and the view reaches put', async () => {
  reset()
  const { $, argvs } = fake(a => ok(a[1] === 'why' ? whyOut : { matches: [{ hits: [{ name: 'g', path: 'a.rs', span: 'L2-L3', depth: 1 }] }] }))
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { offset: 7 } }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs).toEqual([
    ['sieve', 'why', '--json', '--', 'a.rs:7'],
    ['sieve', 'callers', '--in', 'a.rs', '--depth', '2', '--json', '--', 'f'],
  ])
  expect(puts.length).toBe(1)
  expect(puts[0].n).toBe(1)
})

test('step: Edit reads the file and sends the line of new_string', async () => {
  reset()
  const { $, argvs } = fake(() => ok({ symbols: [] }), { '/p/a.rs': 'x\ny\nnew\n' })
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Edit', input: { old_string: 'old', new_string: 'new' } }
  await step($, async () => {})
  expect(argvs[0]).toEqual(['sieve', 'why', '--json', '--', 'a.rs:3'])
})

test('step: one query at a time, and a newer job replaces the older pending job', async () => {
  reset()
  let release = () => {}
  const gate = new Promise<void>(r => (release = r))
  const { $, argvs } = fake(a => (a[1] === 'why' ? gate.then(() => ok({ symbols: [] })) : ok({})))
  const job = (n: number) => ({ file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { offset: n } })
  const puts: any[] = []
  const put = async (v: any) => void puts.push(v)
  queue.job = job(1)
  const first = step($, put)
  await tick()
  queue.job = job(2)
  queue.job = job(3) // replaces job 2
  await step($, put) // busy: no second query
  expect(argvs.length).toBe(1)
  release()
  await first
  expect(puts.length).toBe(0) // job 1 is stale: a newer job waits
  await step($, put)
  expect(argvs.map(a => a[4])).toEqual(['a.rs:1', 'a.rs:3'])
  expect(puts.length).toBe(1)
})

test('step: a rejected process call gives the error line and no rejection', async () => {
  reset()
  const { $ } = fake(() => Promise.reject(new Error('spawn sieve ENOENT')))
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: {} }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(puts[0].note).toBe('sieve not found on PATH')
  expect(queue.busy).toBe(false)
})

test('step: a failing put does not reject and frees the worker', async () => {
  reset()
  const { $ } = fake(() => ok({ symbols: [] }))
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: {} }
  await step($, () => Promise.reject(new Error('state down')))
  expect(queue.busy).toBe(false)
})

// The shape of the real `sieve map --json`: { dirs: [{ hubs: [{ name, path, span, inDegree }] }] }. Symbol i of a.rs has i + 1 callers.
const mapOf = (n: number) => ({
  dirs: [{ path: '.', hubs: [...Array.from({ length: n }, (_, i) => ({ name: `s${i}`, path: 'a.rs', span: `L${i + 1}-L${i + 2}`, inDegree: i + 1 })), { name: 'other', path: 'b.rs', span: 'L1', inDegree: 99 }] }],
})
const hitsOf = (n: number) => ({ matches: [{ hits: Array.from({ length: n }, () => ({ name: 'x', path: 'b.rs', span: 'L1' })) }] })
const wholeJob = () => ({ file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { file_path: '/p/a.rs' } })
const fileAnswer = (a: string[], n = 7) => ok(a[1] === 'map' ? mapOf(n) : a[1] === 'why' ? whyOut : hitsOf(2))

test('step: a whole-file Read runs 3 queries (map, why, callers) and shows the top 3 of 7 symbols', async () => {
  reset()
  const { $, argvs } = fake(a => fileAnswer(a))
  queue.job = wholeJob()
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  // Before: 2 + 10 + 2 + 4 = up to 18 runs. Now: 3.
  expect(argvs).toEqual([
    ['sieve', 'map', '--json'],
    ['sieve', 'why', '--json', '--', 'a.rs:7'],
    ['sieve', 'callers', '--in', 'a.rs', '--depth', '2', '--json', '--', 'f'],
  ])
  expect(puts[0].symbols.map((c: any) => `${c.name}:${c.line}:${c.n}`)).toEqual(['s6:7:7', 's5:6:6', 's4:5:5'])
  const l = lines(radar(R, 60, puts[0], null, null))
  expect(l.some(x => x.includes('MOST CALLED'))).toBe(true)
  expect(l).toContain('s6:7   7 callers')
})

test('step: a whole-file view never runs more than 3 queries, also with many callers', async () => {
  reset()
  const many = { matches: [{ symbol: { kind: 'function', span: 'L1-L2' }, hits: ['a', 'b', 'c', 'd', 'e', 'f'].map(n => ({ id: n, name: n, path: 'b.rs', span: 'L1', depth: 1 })) }] }
  const { $, argvs } = fake(a => (a[1] === 'callers' ? ok(many) : fileAnswer(a)))
  queue.job = wholeJob()
  await step($, async () => {})
  expect(argvs.length).toBe(3)
})

test('step: 1 caller is singular', async () => {
  reset()
  const { $ } = fake(a => fileAnswer(a, 1))
  queue.job = wholeJob()
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(lines(radar(R, 60, puts[0], null, null))).toContain('s0:1   1 caller')
})

test('step: a file with no hub runs 2 queries and shows the file decisions', async () => {
  reset()
  const { $, argvs } = fake(a => ok(a[1] === 'map' ? { dirs: [] } : whyOut))
  queue.job = wholeJob()
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs.map(a => a[1])).toEqual(['map', 'why'])
  expect(puts[0].symbols).toEqual([])
})

test('step: a failed map call gives its note', async () => {
  reset()
  const { $ } = fake(() => Promise.resolve({ exitCode: 1, stdout: '', stderr: 'boom' }))
  queue.job = wholeJob()
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(puts[0].note).toBe('boom')
})

test('step: a job that arrives during the file view ends the spawn loop, so the child is killed, and stops the view', async () => {
  reset()
  const { $, argvs, ended } = fake(a => {
    if (a[1] === 'why') setJob({ file: 'b.rs', path: '/p/b.rs', tool: 'Read', input: { offset: 5 } }) // arrives while why runs
    return fileAnswer(a)
  })
  queue.job = wholeJob()
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs.map(a => a[1])).toEqual(['map', 'why']) // the callers call never starts
  expect(ended.map(a => a[1])).toEqual(['why']) // the loop of the stale why call ended early
  expect(puts.length).toBe(0)
  expect(queue.job?.file).toBe('b.rs')
})

test('step: an end piece with a signal and no code is a failure', async () => {
  reset()
  const $ = { session: { cwd: async () => '/p' }, fs: {}, process: { spawn: () => ({ [Symbol.asyncIterator]: () => { let i = 0; const v = [{ stream: 'stderr', text: 'killed' }, { code: null, signal: 'SIGKILL' }]; return { next: async () => (i < v.length ? { done: false, value: v[i++] } : { done: true }), return: async () => ({ done: true }) } } }) } } as never
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { offset: 7 } }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(puts[0].note).toBe('killed')
})

test('step: a spawn that throws gives the not-found note', async () => {
  reset()
  const { $ } = fake(() => Promise.reject(new Error('spawn sieve ENOENT')))
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { offset: 7 } }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(puts[0].note).toBe('sieve not found on PATH')
})

test('step: a Read with only a limit is a partial Read, not a whole-file Read', async () => {
  reset()
  const { $, argvs } = fake(() => ok({ symbols: [] }))
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { file_path: '/p/a.rs', limit: 50 } }
  await step($, async () => {})
  expect(argvs[0]).toEqual(['sieve', 'why', '--json', '--', 'a.rs:1'])
})

test('step: a Read with an offset still queries that line', async () => {
  reset()
  const { $, argvs } = fake(() => ok({ symbols: [] }))
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { offset: 12 } }
  await step($, async () => {})
  expect(argvs[0]).toEqual(['sieve', 'why', '--json', '--', 'a.rs:12'])
})

test('radar: a leading cross or check mark leaves the note text', () => {
  const v = { file: 'a.rs', tool: 'Read', why: [], tree: [], note: '✗ no symbol at a.rs:1' }
  expect(lines(radar(R, 60, v, null, null))).toContain('no symbol at a.rs:1')
})

// --- blast band ---
const blastOut = {
  seeds: [{ name: 'a' }, { name: 'b' }, { name: 'c' }],
  impacted: [{ name: 'build_repo', depth: 1 }, { name: 'run', depth: 2 }, { name: 'main', depth: 1 }, { name: 'x', depth: 1 }],
}
const resetBlast = () => Object.assign(blast, { dirty: true, busy: false, last: -Infinity })

test('band: plain words, one symbol with a count of the rest, singular caller', () => {
  expect(bandLine(bandInfo(blastOut)!)).toBe('✎ a and 2 more changed → 4 callers affected')
  expect(bandLine(bandInfo({ seeds: [{ name: 'a' }], impacted: [{ name: 'x', depth: 1 }] })!)).toBe('✎ a → 1 caller affected')
  expect(bandInfo(blastOut)!.hops).toBe(2)
})

test('band: " in R routes" shows only when the impacted symbols are route handlers', () => {
  const route = (p: string) => ({ name: 'POST', depth: 2, path: `apps/x/src/routes/api/v1/${p}/+server.ts` })
  const b = { seeds: [{ name: 'f' }], impacted: [route('ai/chat'), route('ai/job-fit'), { name: 'g', depth: 1, path: 'lib/g.ts' }] }
  expect(bandLine(bandInfo(b)!)).toBe('✎ f → 3 callers affected in 2 routes')
  expect(bandLine(bandInfo({ ...b, impacted: [route('ai/chat')] })!)).toBe('✎ f → 1 caller affected in 1 route')
  expect(bandLine(bandInfo(blastOut)!)).not.toContain('route')
})

test('routeOf: the route comes from the file path, a parameter segment is dropped', () => {
  expect(routeOf({ name: 'POST', path: 'apps/p/src/routes/api/v1/ai/chat/[profileId]/+server.ts' })).toBe('POST  ai/chat')
  expect(routeOf({ name: 'f', path: 'lib/a.ts' })).toBeUndefined()
  expect(routeOf({ name: 'GET', path: 'src/routes/+server.ts' })).toBe('GET  /')
  expect(routeOf({ name: 'GET', path: 'routes/+server.ts' })).toBe('GET  /')
})

test('band: nothing changed gives no band', () => {
  expect(bandInfo({ seeds: [], impacted: [{ name: 'x' }] })).toBeNull()
  expect(bandInfo({})).toBeNull()
})

test('level: green for 1 to 5, amber for 6 to 20, red above 20', () => {
  expect([1, 5, 6, 20, 21].map(level)).toEqual(['green', 'green', 'orange', 'orange', 'red'])
})

test('onToolCall: Edit sets the blast flag, Read does not', async () => {
  reset()
  blast.dirty = false
  const { $ } = fake(() => ok({}))
  await onToolCall($, { tool: 'Read', file_path: '/p/a.rs' }, async () => ({}))
  expect(blast.dirty).toBe(false)
  await onToolCall($, { tool: 'Edit', file_path: '/p/a.rs' }, async () => ({}))
  expect(blast.dirty).toBe(true)
})

test('blastStep: the argv is exact and the band text reaches putBand', async () => {
  resetBlast()
  const { $, argvs } = fake(() => ok(blastOut))
  const puts: any[] = []
  await blastStep($, 5000, async t => void puts.push(t))
  expect(argvs).toEqual([['sieve', 'blast', '--format', 'json', '--no-owners']])
  expect(puts).toEqual([{ sym: 'a', more: 2, n: 4, hops: 2, routes: 0 }])
})

test('blastStep: nothing changed and a blast failure both give a null band', async () => {
  for (const answer of [() => ok({ seeds: [], impacted: [] }), () => Promise.resolve({ exitCode: 1, stdout: '', stderr: 'boom' }), () => Promise.reject(new Error('x'))]) {
    resetBlast()
    const { $ } = fake(answer)
    const puts: any[] = []
    await blastStep($, 5000, async t => void puts.push(t))
    expect(puts).toEqual([null])
    expect(blast.busy).toBe(false)
  }
})

test('blastStep: at most once per 2 seconds, and only after a new edit', async () => {
  resetBlast()
  const { $, argvs } = fake(() => ok(blastOut))
  const put = async () => {}
  await blastStep($, 10000, put)
  blast.dirty = true
  await blastStep($, 11999, put) // too soon
  expect(argvs.length).toBe(1)
  await blastStep($, 12000, put)
  expect(argvs.length).toBe(2)
  await blastStep($, 20000, put) // no new edit
  expect(argvs.length).toBe(2)
})

test('blastStep: an edit during the run drops the stale result', async () => {
  resetBlast()
  let release = () => {}
  const gate = new Promise<void>(r => (release = r))
  const { $ } = fake(() => gate.then(() => ok(blastOut)))
  const puts: any[] = []
  const run = blastStep($, 5000, async t => void puts.push(t))
  await tick()
  blast.dirty = true
  release()
  await run
  expect(puts.length).toBe(0)
  expect(blast.dirty).toBe(true)
})

test('band: a whole-file seed shows the file name, a symbol seed wins', () => {
  const b = { seeds: [{ wholeFile: true, path: 'docs/a.md' }, { wholeFile: true, path: 'b.md' }], impacted: [{ name: 'x', depth: 1 }] }
  expect(bandInfo(b)!).toMatchObject({ sym: 'a.md', more: 1 })
  expect(bandInfo({ ...b, seeds: [...b.seeds, { wholeFile: false, name: 'f', path: 'c.rs' }] })!).toMatchObject({ sym: 'f', more: 2 })
})

test('onToolCall: Write and MultiEdit set the blast flag, a path outside the project does not', async () => {
  const { $ } = fake(() => ok({}))
  for (const [tool, path, want] of [['Write', '/p/a.rs', true], ['MultiEdit', '/p/a.rs', true], ['Edit', '/other/a.rs', false]] as const) {
    reset()
    blast.dirty = false
    await onToolCall($, { tool, file_path: path }, async () => ({}))
    expect(blast.dirty).toBe(want)
  }
})

test('startWorker: 3 session.start events start exactly 1 timer', async () => {
  worker.started = false
  let timers = 0
  const handlers: Record<string, any> = {}
  register(((name: string, a: any, b?: any) => void (handlers[name + (b ? ':' + (a.component ?? a.command) : '')] = b ?? a)) as never, {} as never)
  const $ = {
    command: { register: async () => {} },
    clock: { every: () => void timers++ },
  }
  for (let i = 0; i < 3; i++) await handlers['session.start']($, {}, async (e: any) => e)
  expect(timers).toBe(1)
})

test('register: the ui.render hook for AbovePrompt exists', () => {
  const seen: string[] = []
  register(((name: string, a: any) => void seen.push(name === 'ui.render' ? `ui.render:${a.component}` : name)) as never, {} as never)
  expect(seen).toContain('ui.render:AbovePrompt')
})

const band6 = { sym: 'f', n: 6, hops: 2, routes: 0 }

test('bandRender: a survey on screen passes through', () => {
  const next = (e: any) => ({ passed: e })
  const e = { props: { hasSurvey: true } }
  expect(bandRender({} as never, e, next, band6)).toEqual({ passed: e })
})

test('bandRender: no data passes through, data draws the plain words in the level color', () => {
  const next = (e: any) => ({ passed: e })
  const e = { props: {} }
  expect(bandRender({} as never, e, next, null)).toEqual({ passed: e })
  const $ = { ui: { resolve: () => R } }
  for (const [n, color] of [[5, C.green], [6, C.orange], [21, C.red]] as const) {
    const tree = bandRender($ as never, e, next, { ...band6, n })
    expect(texts(tree)).toEqual([{ text: `✎ f → ${n} callers affected`, props: { color } }])
  }
})

// --- the Radar frame ---
const fix = (over: any = {}) => ({
  file: 'apps/x/rate-limit.ts',
  tool: 'Read',
  symbol: 'checkApiKeyRateLimit',
  kind: 'function',
  line: 313,
  why: [{ date: '2026-09-16', heading: 'rate limit per API key', superseded: false }],
  tree: [
    { d: 1 as const, p: 0, label: 'enforceApiKeyRateLimit', tail: 'guard.ts:24' },
    { d: 2 as const, p: 0, label: 'POST  ai/chat', tail: '' },
    { d: 2 as const, p: 0, label: 'POST  ai/job-fit', tail: '' },
  ],
  n: 3,
  hops: 2,
  ...over,
})
const sfix = { tokens: 12900, dollars: 0.04, spark: [1, 2, 5, 3, 7] }
const draw = (v: any, b: any = null, s: any = null, w = 44) => texts(radar(R, w, v, b, s))
const section = (t: any[], title: string) => t.find(x => x.text === title && x.props.bold)

test('radar: the pane has padding and no outer border, and each section shows from the data', () => {
  const tree: any = radar(R, 44, fix(), null, sfix)
  expect(tree.type).toBe('Box')
  expect(tree.props.borderStyle).toBeUndefined() // the engine's pane draws the frame
  expect(tree.props).toMatchObject({ paddingX: 1, paddingTop: 1 })
  const t = draw(fix(), null, sfix)
  for (const name of ['WHO DEPENDS ON THIS', 'IMPACT', 'WHY IT EXISTS', 'SAVED · LAST 7 DAYS']) expect(section(t, name)).toBeTruthy()
  const l = t.map(x => x.text)
  expect(l[0]).toBe('checkApiKeyRateLimit')
  expect(l).toContain('rate-limit.ts:313 · fn · read')
  expect(l).toContain('◉ checkApiKeyRateLimit')
  expect(l).toContain('▍2026-09-16  rate limit per API key')
  expect(l).toContain('12.9k')
  expect(l).toContain('18')
})

test('radar: the symbol is bold, depth 1 and depth 2 differ, the section titles have a color', () => {
  const t = draw(fix())
  expect(t.find(x => x.text === '◉ checkApiKeyRateLimit')!.props.bold).toBe(true)
  const d1 = t.find(x => x.text.includes('enforceApiKeyRateLimit'))!.props
  const d2 = t.find(x => x.text.includes('ai/chat'))!.props
  expect(d1.color).toBe(C.cyan)
  expect(d2.color).toBe(C.blue)
  expect(section(t, 'IMPACT')!.props.color).toBe(C.purple)
})

test('radar: the impact bar and its text follow the caller count and the thresholds', () => {
  for (const [n, color, filled] of [[3, C.green, 4], [6, C.orange, 8], [21, C.red, 14]] as const) {
    const x = draw(fix({ n, hops: 2 })).find(y => y.text.includes('█') || y.text.startsWith('░'))!
    expect(x.props.color).toBe(color)
    expect(x.text.startsWith(`${'█'.repeat(filled)}${'░'.repeat(14 - filled)}`)).toBe(true)
    expect(x.text).toContain(`${n} callers · 2 hops`)
  }
})

test('radar: an edit takes the impact from the band, a read keeps the callers count', () => {
  const b = { sym: 'f', n: 30, hops: 3, routes: 0 }
  const edit = draw(fix({ tool: 'Edit' }), b).find(y => y.text.includes('callers · '))!
  expect(edit.text).toContain('30 callers · 3 hops')
  expect(edit.props.color).toBe(C.red)
  expect(edit.text.startsWith(`${'█'.repeat(14)}  `)).toBe(true) // the bar is full: 4 filled cells would be the callers count of 3
  expect(draw(fix({ tool: 'Read' }), b).find(y => y.text.includes('callers · '))!.text).toContain('3 callers · 2 hops')
})

test('radar: 5 depth-1 callers show 4 rows, the last one ends with └─, "+1 more" counts the one cut', () => {
  const tree = Array.from({ length: 5 }, (_, i) => ({ d: 1 as const, p: i, label: `c${i}`, tail: '' }))
  const t = draw(fix({ tree, n: 5 }))
  const at = t.findIndex(x => x.text.startsWith('◉'))
  expect(t.slice(at + 1, at + 6).map(x => x.text)).toEqual(['├─● c0', '├─● c1', '├─● c2', '└─● c3', '   +1 more'])
})

test('radar: 1 caller with 5 kids shows 4 rows, the last kid ends with └─, "+2 more" sits at depth 2', () => {
  const tree = [{ d: 1 as const, p: 0, label: 'c', tail: '' }, ...Array.from({ length: 5 }, (_, i) => ({ d: 2 as const, p: 0, label: `k${i}`, tail: '' }))]
  const t = draw(fix({ tree, n: 6 }))
  const at = t.findIndex(x => x.text.startsWith('◉'))
  expect(t.slice(at + 1, at + 6).map(x => x.text)).toEqual(['└─● c', '   ├─○ k0', '   ├─○ k1', '   └─○ k2', '      +2 more'])
})

test('radar: more callers than rows: "+N more" is the true total minus the rows, the impact keeps the total', () => {
  // 6 depth-1 callers, 2 kids under the first: 8 hits in all, 4 rows shown.
  const tree = [{ d: 1 as const, p: 0, label: 'a', tail: '' }, { d: 2 as const, p: 0, label: 'x', tail: '' }, { d: 1 as const, p: 1, label: 'b', tail: '' }, { d: 1 as const, p: 2, label: 'c', tail: '' }, { d: 1 as const, p: 3, label: 'd', tail: '' }]
  const t = draw(fix({ tree, n: 8 }))
  const l = t.map(x => x.text)
  expect(l).toContain('   +4 more')
  expect(l.find(x => x.includes('callers · '))).toContain('8 callers')
  expect(draw(fix()).some(x => x.text.startsWith('+'))).toBe(false)
})

test('step: a job that arrives during the caller loop stops it after the current call, and puts nothing', async () => {
  reset()
  const h = (id: string, depth: number) => ({ id, name: id, path: `${id}.ts`, span: 'L1-L2', depth })
  const top = { matches: [{ symbol: { kind: 'function', span: 'L5-L9' }, hits: [h('a', 1), h('b', 1), h('c', 1), h('x', 2)] }] }
  const { $, argvs } = fake(a => {
    if (a[1] === 'callers' && !a.includes('--depth')) queue.job = { file: 'b.rs', path: '/p/b.rs', tool: 'Read', input: { offset: 5 } } // arrives during the first loop call
    return ok(a[1] === 'why' ? whyOut : top)
  })
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { offset: 5 } }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs.filter(a => a[1] === 'callers' && !a.includes('--depth')).length).toBe(1)
  expect(puts.length).toBe(0)
  expect(queue.job?.file).toBe('b.rs')
})

test('radar: the body is 24 cells at least, and no line is wider than the body minus the margin', () => {
  const long = 'a'.repeat(80)
  for (const x of draw(fix({ symbol: long }), null, null, 10)) expect(x.text.length).toBeLessThanOrEqual(23)
})

test('cut: a long name ends in an ellipsis and the line fits the pane', () => {
  expect(cut('abcdefghij', 6)).toBe('abcde…')
  expect(cut('abc', 6)).toBe('abc')
  const long = 'a'.repeat(80)
  const t = draw(fix({ symbol: long, tree: [{ d: 1, p: 0, label: long, tail: 'g.ts:1' }] }), null, null, 40)
  for (const x of t) expect(x.text.length).toBeLessThanOrEqual(37) // 40 columns, less 2 of padding, less 1 free
  expect(t.some(x => x.text.endsWith('…'))).toBe(true)
})

test('radar: a SUPERSEDED decision shows a red card with the warning sign and the successor', () => {
  const t = draw(fix({ why: [{ date: '2026-09-16', heading: 'old rule', superseded: true, by: '2026-10-02' }] }))
  expect(t.find(x => x.text === '▍⚠ SUPERSEDED by 2026-10-02')!.props.color).toBe(C.red)
  expect(t.find(x => x.text.includes('old rule'))!.props.color).toBe(C.red)
})

test('radar: no decision shows a dim line, no stats hides the savings section', () => {
  const t = draw(fix({ why: [] }))
  expect(t.find(x => x.text === 'no decision recorded')!.props.color).toBe(C.text)
  expect(section(t, 'SAVED · LAST 7 DAYS')).toBeUndefined()
})

test('radar: a whole-file Read draws the most called symbols in the same frame', () => {
  const v = { file: 'a.rs', tool: 'Read', why: [], tree: [], symbols: [{ name: 's1', line: 4, n: 7 }] }
  const tree: any = radar(R, 44, v, null, null)
  const l = texts(tree).map(x => x.text)
  expect(l).toContain('s1:4   7 callers')
  expect(l).toContain('no decision recorded')
})

// --- the savings numbers ---
const statsOut = {
  session: { id: 's', tokens: 5, dollars: 0.01 },
  today: { date: '2026-10-04', tokens: 12900, dollars: 0.04 },
  last7days: [100, 0, 400, 800, 200, 50, 700].map((tokens, i) => ({ date: `d${i}`, tokens, dollars: null })), // newest first
  last7daysTotal: { tokens: 2250, dollars: null },
  basis: 'x',
}
const fakeUi = (answer: (a: string[]) => Promise<any>) => {
  const f = fake(answer)
  return { $: f.$ as never, argvs: f.argvs }
}

test('parseStats: the sparkline reads oldest left, and a null dollar stays null', () => {
  const s = parseStats(statsOut)!
  expect(s.spark).toEqual([700, 50, 200, 800, 400, 0, 100])
  expect(sparkline(s.spark)).toBe('▇▁▃█▅▁▂')
  expect(parseStats({ ...statsOut, today: { tokens: 1, dollars: null } })!.dollars).toBeNull()
  expect(parseStats(null)).toBeNull()
  expect(parseStats({ today: {} })).toBeNull()
})

test('statsStep: the argv, the put, and at most one run per 30 seconds', async () => {
  Object.assign(statsRun, { last: -Infinity, busy: false })
  const { $, argvs } = fakeUi(() => ok(statsOut))
  const puts: any[] = []
  await statsStep($, 1000, async s => void puts.push(s))
  expect(argvs).toEqual([['sieve', 'stats', '--json']])
  expect(puts[0].tokens).toBe(12900)
  await statsStep($, 30999, async () => {})
  expect(argvs.length).toBe(1)
  await statsStep($, 31000, async () => {})
  expect(argvs.length).toBe(2)
})

test('parseStats: the real `sieve stats --json` shape (id null, all zero) draws the savings section', () => {
  const day = (date: string) => ({ date, tokens: 0, dollars: null })
  const real = { session: { id: null, tokens: 0, dollars: null }, today: day('2026-10-04'), last7days: ['04', '03', '02', '01', '30', '29', '28'].map(day), last7daysTotal: { tokens: 0, dollars: null }, basis: 'x' }
  const s = parseStats(real)!
  expect(draw(fix(), null, s).map(x => x.text)).toContain('SAVED TODAY')
})

test('statsStep: a failure or an unknown command hides the section', async () => {
  for (const answer of [() => Promise.resolve({ exitCode: 1, stdout: '', stderr: 'unknown command' }), () => Promise.reject(new Error('x')), () => ok(null as never)]) {
    Object.assign(statsRun, { last: -Infinity, busy: false })
    const { $ } = fakeUi(answer)
    const puts: any[] = []
    await statsStep($, 1000, async s => void puts.push(s))
    expect(puts).toEqual([null])
    expect(statsRun.busy).toBe(false)
  }
})

test('step: a symbol with 2 depth-1 callers asks each one for its own callers', async () => {
  reset()
  const h = (id: string, depth: number) => ({ id, name: id, path: `${id}.ts`, span: 'L1-L2', depth })
  const top = { matches: [{ symbol: { kind: 'function', span: 'L5-L9' }, hits: [h('a', 1), h('b', 1), h('x', 2), h('y', 2)] }] }
  const { $, argvs } = fake(a => ok(a[1] === 'why' ? whyOut : a.includes('--depth') ? top : { matches: [{ hits: a[a.length - 1] === 'a' ? [h('x', 1)] : [h('y', 1)] }] }))
  queue.job = { file: 'a.rs', path: '/p/a.rs', tool: 'Read', input: { offset: 5 } }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs.length).toBe(4)
  expect(puts[0].tree.map((r: any) => `${r.d}:${r.p}:${r.label}`)).toEqual(['1:0:a', '2:0:x', '1:1:b', '2:1:y'])
  expect(treeLines(puts[0].tree, puts[0].n).rows.map(r => r.text)).toEqual(['├─● a   a.ts:1', '│  └─○ x   x.ts:1', '└─● b   b.ts:1', '   └─○ y   y.ts:1'])
  expect(puts[0].n).toBe(4)
  expect(puts[0].hops).toBe(2)
})

// --- the Tokyo Night look: tiles, chart, mascots ---
const RR = { Box: 'Box', Text: 'Text', Raster: 'Raster' }
const SV = { Box: 'Box', Text: 'Text', Svg: 'Svg' }
const weekStats = { tokens: 61700, dollars: null, spark: [700, 50, 200, 800, 400, 0, 100] }
const decode = (b64: string) => new Uint32Array(Uint8Array.from(atob(b64), c => c.charCodeAt(0)).buffer)
const hex = (h: string) => parseInt(h.slice(1), 16)
const rasterOf = (tree: any) => nodes(tree, 'Raster')[0]
const wordsOf = (tree: any) => decode(rasterOf(tree).props.cells)
const tileBoxes = (tree: any) => nodes(tree, 'Box').filter(x => x.props?.borderStyle === 'round' && x.props?.width)

test('look: every section shows, and the colors follow the level (green, orange, red)', () => {
  for (const [n, color] of [[3, C.green], [6, C.orange], [21, C.red]] as const) {
    const t = texts(radar(RR, 44, fix({ n, hops: 2 }), null, weekStats))
    for (const name of ['WHO DEPENDS ON THIS', 'IMPACT', 'WHY IT EXISTS', 'SAVED · LAST 7 DAYS']) expect(section(t, name)!.props.color).toBe(C.purple)
    expect(t.find(x => x.text === String(n))!.props.color).toBe(color) // the IMPACT tile value
    expect(t.find(x => x.text.includes(`${n} callers · 2 hops`))!.props.color).toBe(color) // the bar line
  }
  expect(rasterOf(radar(RR, 44, fix(), null, weekStats))).toBeTruthy()
})

test('look: the header names the symbol in bold and the meta in dim, the connectors are dim, depth 1 cyan, depth 2 blue', () => {
  const t = texts(radar(RR, 44, fix({ at: new Date(2026, 9, 5, 21, 58).getTime(), tool: 'Edit' }), null, null))
  expect(t[0]).toMatchObject({ text: 'checkApiKeyRateLimit', props: { bold: true, color: C.fg } })
  const meta = t.filter(x => x.props.color === C.text && /rate-limit|edited/.test(x.text)).map(x => x.text)
  expect(meta).toEqual(['rate-limit.ts:313 · fn', 'edited 9:58pm'])
  const d1 = t.find(x => x.text.includes('enforceApiKeyRateLimit'))!
  expect(d1.parts!.map(p => p.props.color)).toEqual([C.dim, C.cyan])
  expect(t.find(x => x.text.includes('ai/chat'))!.parts!.map(p => p.props.color)).toEqual([C.dim, C.blue])
})

test('look: the stat tiles are bordered, in the order today, week, impact', () => {
  const tree: any = radar(RR, 44, fix(), null, weekStats)
  const tiles = tileBoxes(tree)
  expect(tiles.length).toBe(3)
  expect(tiles.map(x => texts(x).map(y => y.text))).toEqual([['SAVED TODAY', '61.7k'], ['THIS WEEK', '2.3k'], ['IMPACT', '3']])
  expect(tiles.map(x => texts(x)[1]!.props.color)).toEqual([C.green, C.blue, C.green])
  expect(tiles.reduce((a, x) => a + x.props.width, 0) + 2).toBeLessThanOrEqual(41) // 3 tiles and 2 gaps fit the 44-column pane, less the padding
  expect(nodes(tree, 'Box').find(x => x.props?.gap === 1 && tileBoxes(x).length === 3)).toBeTruthy() // 1 cell between tiles
})

test('look: the tile text has a padding column on each side, and the label and the value share a left edge', () => {
  const tree: any = radar(RR, 44, fix(), null, weekStats)
  for (const tile of tileBoxes(tree)) {
    expect(tile.props.paddingX).toBe(1)
    // Draw the tile as text: the border takes 1 cell, the padding 1 cell, each side.
    const inner = tile.props.width - 2
    const rows = texts(tile).map(x => `│${' '.repeat(tile.props.paddingX)}${x.text.padEnd(inner - 2 * tile.props.paddingX)}${' '.repeat(tile.props.paddingX)}│`)
    for (const r of rows) {
      expect(r.length).toBe(tile.props.width)
      expect(r[1]).toBe(' ') // a padding column after the left border
      expect(r[2]).not.toBe(' ') // the text starts in column 2, the same for the label and the value
      expect(r[r.length - 2]).toBe(' ') // a padding column before the right border
    }
    for (const x of texts(tile)) expect(x.text.length).toBeLessThanOrEqual(tile.props.width - 4)
  }
  const head = nodes(tree, 'Box').find(x => x.props?.alignItems === 'flex-start')
  expect(head.props.gap).toBe(2) // 2 columns between the mascot and the text, both at the top
})

test('look: a non-code file animates the mascot, the savings tiles and the not-code line', () => {
  const v = { file: 'docs/fix-batch.md', tool: 'Read', why: [], tree: [], note: '✗ no symbol at docs/fix-batch.md:1' }
  const tree: any = radar(RR, 44, v, null, weekStats, { name: 'soya', frame: 1 })
  const t = texts(tree).map(x => x.text)
  expect(t).toContain('fix-batch.md')
  expect(t).toContain('not code · Sieve indexes code files only')
  expect(t.some(x => x.includes('no symbol at'))).toBe(false)
  expect(tileBoxes(tree).map(x => texts(x)[0]!.text)).toEqual(['SAVED TODAY', 'THIS WEEK'])
  expect(t.some(x => x === 'WHO DEPENDS ON THIS' || x === 'WHY IT EXISTS' || x === 'SAVED · LAST 7 DAYS')).toBe(false)
  expect(Array.from(wordsOf(tree))).toEqual(Array.from(mascotWords('soya', 1)!.words)) // the asked frame
})

test('look: the 7-day chart reads oldest left, today green, the others blue, with bar glyphs', () => {
  const t = texts(radar(RR, 44, fix(), null, weekStats))
  const at = t.findIndex(x => x.text === 'SAVED · LAST 7 DAYS')
  const days = t.slice(at + 1, at + 8)
  expect(days.map(x => x.text)).toEqual(['▇▇', '▁▁', '▃▃', '██', '▅▅', '▁▁', '▂▂'])
  expect(days.map(x => x.props.color)).toEqual([C.blue, C.blue, C.blue, C.blue, C.blue, C.blue, C.green])
  expect(chart([0, 0, 0])).toEqual(['▁', '▁', '▁'])
  expect(chart([1, 8]).join('')).toBe('▂█')
})

test('look: the mascot option: none hides it, hoot uses the hoot palette', () => {
  expect(rasterOf(radar(RR, 44, fix(), null, null, { name: 'none', frame: 0 }))).toBeUndefined()
  const hoot = wordsOf(radar(RR, 44, fix(), null, null, { name: 'hoot', frame: 0 }))
  const soya = wordsOf(radar(RR, 44, fix(), null, null, { name: 'soya', frame: 0 }))
  const colors = (w: Uint32Array) => new Set([...w].filter((c, i) => i % 3 > 0 && c !== 0x01000000))
  const owl = new Set([...MASCOTS.hoot!.small.flat().join('')].filter(c => c !== '.').map(c => hex(PALETTE[c]!)))
  expect([...colors(hoot)].every(c => owl.has(c))).toBe(true)
  expect(colors(hoot).has(hex('#bb9af7'))).toBe(true) // hoot is purple
  expect(colors(soya).has(hex('#bb9af7'))).toBe(false)
  expect(DEFAULT_MASCOT).toBe('sieve')
  expect(rasterOf(radar(RR, 44, fix(), null, null))).toBeTruthy() // the default shows
})

test('look: the Raster cells of one frame: count, glyphs and colors', () => {
  const r = rasterOf(radar(RR, 44, fix(), null, null, { name: 'soya', frame: 0 }))
  expect([r.props.columns, r.props.rows]).toEqual([10, 3]) // the small frame: 10 x 6 pixels
  const w = decode(r.props.cells)
  expect(w.length).toBe(10 * 3 * 3)
  const cell = (row: number, col: number) => Array.from(w.slice((row * 10 + col) * 3, (row * 10 + col) * 3 + 3))
  expect(cell(0, 0)).toEqual([0x20, 0x01000000, 0x01000000]) // transparent: the terminal color
  expect(cell(0, 2)).toEqual([0x2580, hex('#c08a5c'), hex('#9c6644')]) // upper pixel l over lower pixel n
  expect(cell(1, 0)[1]).toBe(hex('#6f4628')) // pixel rows 2 and 3 at column 0: N over N
  const dim = mascotWords('soya', 0, true)!.words
  expect(dim[(0 * 10 + 2) * 3 + 1]).not.toBe(hex('#c08a5c'))
  expect(packCells(mascotWords('soya', 1)!.words)).not.toBe(r.props.cells) // frame 1 differs
})

test('look: the desktop draws the mascot as Svg, the phone has none', () => {
  const svg = nodes(radar(SV, 44, fix(), null, null, { name: 'soya', frame: 0 }), 'Svg')[0]
  expect(svg.props.source.startsWith('<svg')).toBe(true)
  expect(svg.props.source).toContain('fill="#9c6644"')
  expect(mascotSvg('hoot', 0)).toContain('#bb9af7')
  expect(mascotSvg('soya', 0, true)).toContain('opacity="0.5"')
  const phone = radar(R, 44, fix(), null, null)
  expect(nodes(phone, 'Svg').length + nodes(phone, 'Raster').length).toBe(0)
})

test('look: no pink in the frame or the band', () => {
  const all = JSON.stringify([radar(RR, 44, fix({ why: [{ date: '', heading: 'x', superseded: true }] }), null, weekStats), radar(R, 44, fix(), null, weekStats)])
  expect(all).not.toMatch(/magenta|pink/)
})

test('mood: a red level or a SUPERSEDED decision dims and rests, every other view moves', () => {
  expect(mood(fix(), null)).toEqual({ dim: false, rest: false })
  expect(mood(fix({ n: 21 }), null)).toEqual({ dim: true, rest: true })
  expect(mood(fix({ why: [{ date: '', heading: 'x', superseded: true }] }), null)).toEqual({ dim: true, rest: true })
  expect(mood(fix({ tool: 'Edit' }), { sym: 'f', n: 30, hops: 1, routes: 0 }).dim).toBe(true)
  expect(mood({ file: 'a.md', tool: 'Read', why: [], tree: [] }, null)).toEqual({ dim: false, rest: false }) // a non-code view animates
  expect(mood(null, null)).toEqual({ dim: false, rest: false }) // an idle pane moves
})

// A fake $ for the animation: the blits are recorded.
const resetAnim = (over: any = {}) => Object.assign(anim, { name: 'soya', open: true, isRaster: true, ticks: 0, frame: 0, wag: 0, seen: undefined, drawn: '', view: fix(), band: null, stats: weekStats }, over)
const fakeBlit = (deny?: string, panes: { id: string }[] = [{ id: 'sieve' }]) => {
  const blits: any[] = []
  const inv = { n: 0 }
  const list = { panes }
  const $ = { ui: { panes: async () => list.panes, blit: async (a: any) => (blits.push(a), deny ? { deny } : {}), invalidate: () => void inv.n++ } }
  return { $: $ as never, blits, inv, list }
}
const steps = async ($: any, n: number) => { for (let i = 0; i < n; i++) await animStep($) }
const sameCells = (cells: string, frame: number) => decode(cells).join() === Array.from(mascotWords('soya', frame)!.words).join()
const frameOf = (cells: string) => (sameCells(cells, 1) ? 1 : sameCells(cells, 0) ? 0 : -1)

test('anim: the 2 frames alternate every 2nd worker tick (600 ms), and each change is one blit to the mascot', async () => {
  resetAnim()
  const { $, blits } = fakeBlit()
  await steps($, 8)
  expect(blits.length).toBe(4)
  expect(blits.map(b => frameOf(b.cells))).toEqual([1, 0, 1, 0])
  expect(blits[0]).toMatchObject({ requestId: 'sieve', key: 'mascot' })
})

test('anim: nothing changed, no redraw: a resting mascot blits once, a none mascot never', async () => {
  const { $, blits } = fakeBlit()
  resetAnim({ view: fix({ n: 30 }) }) // red: frame 0, dimmed
  await steps($, 12)
  expect(blits.length).toBe(1)
  expect(frameOf(blits[0].cells)).toBe(-1) // dimmed: not the plain frame 0
  resetAnim({ name: 'none' })
  await steps($, 12)
  expect(blits.length).toBe(1)
  resetAnim({ drawn: 'soya:1:false', frame: 0 }) // the draw showed frame 1: the next step is frame 1, the same picture, no blit
  const again = fakeBlit()
  await steps(again.$, 2)
  expect(again.blits.length).toBe(0)
})

test('anim: a new saving wags: frame 1 for 2 steps, then the alternation goes on', async () => {
  resetAnim({ seen: 61700, frame: 1 })
  const { $, blits } = fakeBlit()
  await steps($, 2) // no new saving: the frame goes 1 -> 0
  Object.assign(anim, { stats: { ...weekStats, tokens: 70000 } })
  await steps($, 6)
  expect(blits.map(b => frameOf(b.cells))).toEqual([0, 1, 0]) // wag step 1 shows 1, wag step 2 stays on 1 (no blit), then 0
  expect(anim.wag).toBe(0)
})

test('anim: a denied blit means the pane closed: the worker stops until the next draw', async () => {
  resetAnim()
  const { $, blits } = fakeBlit('not mounted')
  await steps($, 8)
  expect(blits.length).toBe(1)
  expect(anim.open).toBe(false)
  const thrower = { ui: { panes: async () => [{ id: 'sieve' }], blit: () => Promise.reject(new Error('x')), invalidate: () => {} } } as never
  resetAnim()
  await steps(thrower, 2)
  expect(anim.open).toBe(false)
})

test('anim: the worker tick runs the animation step', async () => {
  worker.started = false
  resetAnim()
  const blits: any[] = []
  let tick = async () => {}
  const $ = { clock: { every: (_: number, f: any) => void (tick = f), now: async () => 0 }, process: { spawn: () => ({ [Symbol.asyncIterator]: () => ({ next: async () => ({ done: false, value: { code: 1, signal: null } }), return: async () => ({ done: true }) }) }) }, fs: {}, state: {}, ui: { panes: async () => [{ id: 'sieve' }], blit: async (a: any) => (blits.push(a), {}), invalidate: () => {} } }
  queue.job = undefined
  Object.assign(blast, { dirty: false })
  startWorker($ as never, async () => {}, async () => {})
  await tick()
  await tick()
  expect(blits.length).toBe(1)
})

test('mascot option: .sieve/config.json sets it, a bad or missing value gives the default', async () => {
  const mk = (files: Record<string, string>) => ({ session: { cwd: async () => '/p' }, fs: { read: async (f: string) => files[f] ?? Promise.reject(new Error('ENOENT')) } }) as never
  expect(await readMascot(mk({ '/p/.sieve/config.json': '{"mascot":"hoot"}' }))).toBe('hoot')
  expect(await readMascot(mk({ '/p/.sieve/config.json': '{"mascot":"none"}' }))).toBe('none')
  expect(await readMascot(mk({ '/p/.sieve/config.json': '{"mascot":"cat"}' }))).toBeUndefined()
  expect(await readMascot(mk({ '/p/.sieve/config.json': 'not json' }))).toBeUndefined()
  expect(await readMascot(mk({}))).toBeUndefined()
})

test('anim: an idle pane (no file read yet) animates', async () => {
  resetAnim({ view: null, stats: null })
  const { $, blits } = fakeBlit()
  await steps($, 8)
  expect(blits.map(b => frameOf(b.cells))).toEqual([1, 0, 1, 0])
})

// /sieve-mascot, with a fake file system.
const mascotCmd = (files: Record<string, string>) => {
  resetAnim()
  const handlers: Record<string, any> = {}
  register(((name: string, a: any, b?: any) => void (handlers[name + (b ? ':' + (a.component ?? a.command) : '')] = b ?? a)) as never, {} as never)
  const toasts: string[] = []
  const t = { invalidated: 0 }
  const $ = {
    session: { cwd: async () => '/p' },
    ui: { invalidate: () => void t.invalidated++, toast: (x: string) => void toasts.push(x) },
    fs: { read: async (f: string) => files[f] ?? Promise.reject(new Error('ENOENT')), write: async (f: string, x: string) => void (files[f] = x) },
  }
  return { run: (args: string) => handlers['command.run:sieve-mascot']($, { command: 'sieve-mascot', args }), toasts, t }
}
const CFG = '/p/.sieve/config.json'

test('/sieve-mascot: creates .sieve/config.json with the mascot key, updates the pane, returns {}', async () => {
  const files: Record<string, string> = {}
  const c = mascotCmd(files)
  expect(await c.run(' Sifty ')).toEqual({})
  expect(JSON.parse(files[CFG]!)).toEqual({ mascot: 'sifty' })
  expect(anim.name).toBe('sifty')
  expect(c.t.invalidated).toBe(1)
  expect(c.toasts[0]).toContain('sifty')
  await c.run('none')
  expect(JSON.parse(files[CFG]!)).toEqual({ mascot: 'none' })
})

test('/sieve-mascot: keeps every other key of an existing file', async () => {
  const files: Record<string, string> = { [CFG]: '{"a":1,"mascot":"soya","b":{"c":[2]}}' }
  await mascotCmd(files).run('hoot')
  expect(JSON.parse(files[CFG]!)).toEqual({ a: 1, mascot: 'hoot', b: { c: [2] } })
})

test('/sieve-mascot: a file that is not valid JSON stays as it is, with a toast', async () => {
  const files: Record<string, string> = { [CFG]: 'not json' }
  const c = mascotCmd(files)
  expect(await c.run('hoot')).toEqual({})
  expect(files[CFG]).toBe('not json')
  expect(c.toasts[0]).toContain('not valid JSON')
  expect(anim.name).toBe('soya')
})

test('/sieve-mascot: an unknown name changes nothing', async () => {
  const files: Record<string, string> = {}
  const c = mascotCmd(files)
  expect(await c.run('cat')).toEqual({})
  expect(files).toEqual({})
  expect(anim.name).toBe('soya')
  expect(c.t.invalidated).toBe(0)
  expect(c.toasts[0]).toContain('Usage')
})

test('mascots.ts equals assets/mascots.json', () => {
  const j: any = mascotJson
  expect(PALETTE).toEqual(j.palette)
  expect(DEFAULT_MASCOT).toBe(j.default)
  expect(Object.keys(MASCOTS)).toEqual(Object.keys(j.mascots))
  for (const k of Object.keys(j.mascots)) {
    expect(MASCOTS[k]!.name).toBe(j.mascots[k].name)
    expect(MASCOTS[k]!.frames).toEqual(j.mascots[k].frames)
    expect(MASCOTS[k]!.small).toEqual(j.mascots[k].small)
    expect(MASCOTS[k]!.frames.length).toBe(2)
    const [h, w] = [14, 16]
    for (const f of MASCOTS[k]!.frames) expect([f.length, ...new Set(f.map(r => r.length))]).toEqual([h, w])
  }
  expect(Object.keys(MASCOTS)).toEqual(['sieve', 'soya', 'cloud', 'sifty', 'hoot', 'grit'])
})

test('mascot: the Raster size follows the frame size', () => {
  const s = mascotWords('sieve', 0)!
  expect([s.columns, s.rows, s.words.length]).toEqual([12, 3, 12 * 3 * 3]) // the pane draws the small frame: 12 x 6 pixels
  expect(mascotSvg('sieve', 0)).toContain('viewBox="0 0 12 6"')
  Object.assign(MASCOTS, { odd: { name: 'odd', frames: [], small: [Array(22).fill('a'.repeat(28))] } })
  expect([mascotWords('odd', 0)!.columns, mascotWords('odd', 0)!.rows]).toEqual([28, 11]) // 28x22 gives 28 columns x 11 rows
  delete MASCOTS.odd
})

test('mascot: a bad name falls back to sieve, the 6 names are accepted', async () => {
  const mk = (n: string) => ({ session: { cwd: async () => '/p' }, fs: { read: async () => `{"mascot":"${n}"}` } }) as never
  for (const n of ['sieve', 'soya', 'cloud', 'sifty', 'hoot', 'grit']) expect(await readMascot(mk(n))).toBe(n)
  expect((await readMascot(mk('cat'))) ?? DEFAULT_MASCOT).toBe('sieve')
})

test('look: the narrow pane: the mascot shows when the body fits its width plus 18, else it hides', () => {
  expect(rasterOf(radar(RR, 40, fix(), null, null))).toBeTruthy() // body 35 >= 34
  expect(rasterOf(radar(RR, 30, fix(), null, null))).toBeUndefined() // body 24 < 34
})

test('anim: a closed pane (not in panes()) makes no blit and no invalidate, on Raster and on Svg', async () => {
  for (const isRaster of [true, false]) {
    resetAnim({ isRaster })
    const { $, blits, inv } = fakeBlit(undefined, [])
    await steps($, 12)
    expect(blits.length + inv.n).toBe(0)
    expect(anim.open).toBe(false)
  }
})

test('anim: the Svg path invalidates only while the pane is listed, and a reopen restarts the animation', async () => {
  resetAnim({ isRaster: false })
  const { $, blits, inv, list } = fakeBlit()
  await steps($, 8)
  expect(inv.n).toBe(4)
  expect(blits.length).toBe(0)
  list.panes = []
  await steps($, 8)
  expect(inv.n).toBe(4) // closed: no more redraws
  expect(anim.open).toBe(false)
  list.panes = [{ id: 'sieve' }]
  await steps($, 8)
  expect(inv.n).toBe(4) // still stopped: only a draw reopens it
  anim.open = true // the Pane render hook does this on the next draw
  await steps($, 4)
  expect(inv.n).toBe(6)
})

test('anim: the Raster path asks panes() once per step, and a reopen blits again', async () => {
  resetAnim()
  let calls = 0
  const { $, blits, list } = fakeBlit()
  const ui = ($ as any).ui
  const was = ui.panes
  ui.panes = async () => (calls++, was())
  await steps($, 4)
  expect(calls).toBe(2)
  list.panes = []
  await steps($, 2)
  expect(anim.open).toBe(false)
  list.panes = [{ id: 'sieve' }]
  Object.assign(anim, { open: true })
  const before = blits.length
  await steps($, 2)
  expect(blits.length).toBe(before + 1)
})

const SAVED = '[sieve] saved ≈ 7,582 tokens\nbody'

test('savedTokens: sums every saved line and reads commas', () => {
  expect(savedTokens(SAVED)).toBe(7582)
  expect(savedTokens(`${SAVED}\n[sieve] saved ≈ 1,000,418 tokens\n[sieve] saved ≈ 8 tokens`)).toBe(1008008)
  expect(savedTokens('no line')).toBe(0)
})

test('sieveCommand: reads the first sieve sub-command, also in a joined command', () => {
  expect(sieveCommand('sieve skeleton src/x.ts')).toEqual({ sub: 'skeleton', arg: 'src/x.ts' })
  expect(sieveCommand('cd /p && sieve callers --depth 2 diff; ls')).toEqual({ sub: 'callers', arg: 'diff' })
  expect(sieveCommand('sieve ask "how does it work" --json')).toEqual({ sub: 'ask', arg: 'how does it work' })
  expect(sieveCommand('sieve build')).toBeUndefined()
  expect(sieveCommand('ls -la')).toBeUndefined()
})

test('onToolCall: a Bash sieve skeleton call on a repo file makes a file job with the saved sum', async () => {
  reset()
  const { $ } = fake(() => ok({}))
  const ran = { text: SAVED }
  expect(await onToolCall($, { tool: 'Bash', command: 'sieve skeleton src/x.ts' }, async () => ran)).toBe(ran)
  expect(queue.job).toMatchObject({ file: 'src/x.ts', path: '/p/src/x.ts', isFile: true, query: { sub: 'skeleton', arg: 'src/x.ts', saved: 7582 } })
})

test('onToolCall: a joined Bash command and an MCP call make a query job', async () => {
  reset()
  const { $ } = fake(() => ok({}))
  await onToolCall($, { tool: 'Bash', command: 'cd /p && sieve callers diff --depth 2' }, async () => ({ text: SAVED }))
  expect(queue.job).toMatchObject({ file: 'diff', isFile: false, query: { sub: 'callers', saved: 7582 } })
  reset()
  await onToolCall($, { tool: 'mcp__sieve__skeleton', file: 'a.rs' }, async () => ({ text: SAVED }))
  expect(queue.job).toMatchObject({ file: 'a.rs', isFile: true, query: { sub: 'skeleton', saved: 7582 } })
})

test('onToolCall: a Bash call that is not sieve makes no job', async () => {
  reset()
  const { $ } = fake(() => ok({}))
  await onToolCall($, { tool: 'Bash', command: 'ls -la' }, async () => ({ text: SAVED }))
  expect(queue.job).toBeUndefined()
})

test('radar: a query view shows the query line and the saved line in green', () => {
  const t = draw({ file: 'diff', tool: 'callers', why: [], tree: [], query: { sub: 'callers', arg: 'diff', saved: 7582 } })
  expect(t.find(x => x.text === 'sieve callers diff')).toBeDefined()
  expect(t.find(x => x.text === '+7,582 tokens saved')?.props.color).toBe(C.green)
})

test('sieveCommand: quotes, pipes, redirects, env prefix, paths and flag values', () => {
  expect(sieveCommand('sieve callers "a b"')).toEqual({ sub: 'callers', arg: 'a b' })
  expect(sieveCommand("echo 'x; sieve ask y'; sieve why f")).toEqual({ sub: 'why', arg: 'f' })
  expect(sieveCommand('sieve grep foo | head -5')).toEqual({ sub: 'grep', arg: 'foo' })
  expect(sieveCommand('sieve skeleton a.ts > out.txt 2>&1')).toEqual({ sub: 'skeleton', arg: 'a.ts' })
  expect(sieveCommand('sieve callers 2> err.txt f')).toEqual({ sub: 'callers', arg: '' })
  expect(sieveCommand('false || sieve map src')).toEqual({ sub: 'map', arg: 'src' })
  expect(sieveCommand('HOME=x sieve ask why')).toEqual({ sub: 'ask', arg: 'why' })
  expect(sieveCommand('./target/release/sieve blast a.rs')).toEqual({ sub: 'blast', arg: 'a.rs' })
  expect(sieveCommand('npx sieve map')).toEqual({ sub: 'map', arg: '' })
  expect(sieveCommand('sieve callers -n 3 --in a.rs --depth=2 f')).toEqual({ sub: 'callers', arg: 'f' })
  expect(sieveCommand('sieve ask-x y')).toBeUndefined()
})

test('sieveCommand: a word that only holds sieve is not a sieve call', () => {
  for (const c of ['echo sieve ask', 'cd sieve && ls', 'grep sieve x', 'cat sieve/ask']) expect(sieveCommand(c)).toBeUndefined()
})

test('savedTokens: a number with a bad comma group is not read', () => {
  expect(savedTokens('[sieve] saved ≈ 12,34 tokens')).toBe(0)
})

test('onToolCall: an absolute or ../ file argument outside the repo gives a query view only', async () => {
  for (const arg of ['/other/x.ts', '../x.ts']) {
    reset()
    const { $ } = fake(() => ok({}))
    await onToolCall($, { tool: 'Bash', command: `sieve skeleton ${arg}` }, async () => ({ text: SAVED }))
    expect(queue.job?.isFile).toBe(false)
  }
})

test('onToolCall: an MCP call reads the real argument names and maps the tool name', async () => {
  const { $ } = fake(() => ok({}))
  for (const [tool, input, sub, arg] of [
    ['mcp__sieve__find_code', { pattern: 'how x' }, 'ask', 'how x'],
    ['mcp__sieve__trace_calls', { symbol: 'diff' }, 'callers', 'diff'],
    ['mcp__sieve__sieve_trace_calls', { symbol: 'diff' }, 'callers', 'diff'],
    ['mcp__sieve__find_all', { query: 'q' }, 'grep', 'q'],
    ['mcp__sieve__file_api', { file: 'a.rs' }, 'skeleton', 'a.rs'],
  ] as const) {
    reset()
    await onToolCall($, { tool, ...input }, async () => ({ text: SAVED }))
    expect(queue.job?.query).toEqual({ sub, arg, saved: 7582 })
  }
})

test('onToolCall: a throwing session.cwd still returns the result of a sieve call', async () => {
  reset()
  const $ = { session: { cwd: () => Promise.reject(new Error('boom')) } } as never
  const ran = { text: SAVED }
  expect(await onToolCall($, { tool: 'Bash', command: 'sieve skeleton a.ts' }, async () => ran)).toBe(ran)
})

test('step: a query job runs no findLine and shows the query', async () => {
  reset()
  const { $, argvs } = fake(() => ok({}))
  queue.job = { file: 'diff', path: '/p/diff', tool: 'blast', input: {}, query: { sub: 'blast', arg: 'diff', saved: 5 }, isFile: false }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs).toEqual([])
  expect(puts[0].query.saved).toBe(5)
})

// The WCAG contrast ratio of two #rrggbb colors.
const luma = (h: string) => {
  const v = [1, 3, 5].map(i => parseInt(h.slice(i, i + 2), 16) / 255).map(c => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4))
  return 0.2126 * v[0]! + 0.7152 * v[1]! + 0.0722 * v[2]!
}
const contrast = (a: string, b: string) => (Math.max(luma(a), luma(b)) + 0.05) / (Math.min(luma(a), luma(b)) + 0.05)

test('band: "and N more changed" shows for 1 or more other symbols, singular for 1', () => {
  expect(bandLine({ sym: 'f', more: 1, n: 1, hops: 1, routes: 0 })).toBe('✎ f and 1 more changed → 1 caller affected')
  expect(bandLine({ sym: 'f', more: 0, n: 2, hops: 1, routes: 0 })).toBe('✎ f → 2 callers affected')
})

test('colors: every text color has a contrast of 4.5 or more on the pane background', () => {
  const bg = '#1a1b26'
  for (const [name, c] of Object.entries(C)) {
    if (name === 'dim' || name === 'border') continue // the dim color is for borders and bar tracks, not text
    expect(contrast(c, bg)).toBeGreaterThanOrEqual(4.5)
  }
  expect(contrast(C.dim, bg)).toBeLessThan(4.5) // the old dim text color failed: it stays for borders only
})

// The symbol view of a query job: the same tree, impact and why as the edit view.
const symOut = { matches: [{ symbol: { name: 'double', kind: 'function', path: 'src/calc.ts', span: 'L3-L5' }, hits: [{ name: 'alpha', path: 'src/calc.ts', span: 'L17-L19', depth: 1, id: 'a' }] }] }
const symAnswer = (a: string[]) => ok(a[1] === 'why' ? { symbols: [{ name: 'double', decisions: [{ heading: 'doubles n', date: '2026-01-01' }] }] } : a[1] === 'ask' ? { hits: [{ kind: 'symbol', pointer: 'src/calc.ts:L3-L5' }] } : symOut)
const seeTree = (v: any) => {
  const l = lines(radar(R, 60, v, null, null))
  expect(l).toContain('WHO DEPENDS ON THIS')
  expect(l).toContain('◉ double')
  expect(l.some(x => x.startsWith('└─● alpha'))).toBe(true)
  expect(l).toContain('IMPACT')
  expect(l).toContain('WHY IT EXISTS')
  expect(l.some(x => x.includes('doubles n'))).toBe(true)
}

test('step: a whole-file Read shows 3 symbols, then the tree, impact and why of the most called symbol', async () => {
  reset()
  const { $, argvs } = fake(a => (a[1] === 'map' ? ok({ dirs: [{ hubs: [{ name: 'double', path: 'a.rs', span: 'L3-L5', inDegree: 1 }] }] }) : symAnswer(a)))
  queue.job = wholeJob()
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs.some(a => a[1] === 'why' && a[4] === 'a.rs:3')).toBe(true)
  expect(puts[0].symbols.length).toBe(1)
  const l = lines(radar(R, 60, puts[0], null, null))
  expect(l.indexOf('MOST CALLED SYMBOLS')).toBeLessThan(l.indexOf('WHO DEPENDS ON THIS'))
  expect(l[0]).toBe('a.rs')
  seeTree(puts[0])
})

test('step: `sieve callers <symbol>` shows the symbol view, resolved with sieve callers then sieve why', async () => {
  reset()
  const { $, argvs } = fake(symAnswer)
  queue.job = { file: 'double', path: '/p/double', tool: 'callers', input: {}, query: { sub: 'callers', arg: 'double', saved: 3 }, isFile: false }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs[0]).toEqual(['sieve', 'callers', '--json', '--', 'double'])
  expect(argvs[1]).toEqual(['sieve', 'why', '--json', '--', 'src/calc.ts:3'])
  expect(puts[0]).toMatchObject({ file: 'src/calc.ts', symbol: 'double', query: { sub: 'callers', saved: 3 } })
  seeTree(puts[0])
})

test('step: an MCP sieve_trace_calls call shows the symbol view', async () => {
  reset()
  const { $ } = fake(symAnswer)
  await onToolCall($, { tool: 'mcp__sieve__sieve_trace_calls', symbol: 'double' }, async () => ({ text: '' }))
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  seeTree(puts[0])
})

test('step: `sieve ask` shows the symbol view of the top hit; a top hit that is not a symbol gives a note', async () => {
  reset()
  const { $, argvs } = fake(symAnswer)
  queue.job = { file: 'how to double', path: '/p/x', tool: 'ask', input: {}, query: { sub: 'ask', arg: 'how to double', saved: 0 }, isFile: false }
  const puts: any[] = []
  await step($, async v => void puts.push(v))
  expect(argvs[0]).toEqual(['sieve', 'ask', '--json', '--', 'how to double'])
  expect(argvs[1]).toEqual(['sieve', 'why', '--json', '--', 'src/calc.ts:3'])
  seeTree(puts[0])
  const doc = fake(() => ok({ hits: [{ kind: 'doc', pointer: 'README.md:L1' }] }))
  queue.job = { file: 'q', path: '/p/q', tool: 'ask', input: {}, query: { sub: 'ask', arg: 'q', saved: 0 }, isFile: false }
  await step(doc.$, async v => void puts.push(v))
  expect(puts[1].note).toBe('the top hit is not a symbol')
})

test('radar: 1 blank row sits before each section, and the subtitle never repeats the file name', () => {
  const l = draw(fix(), null, sfix).map(x => x.text)
  for (const name of ['WHO DEPENDS ON THIS', 'IMPACT', 'WHY IT EXISTS', 'SAVED · LAST 7 DAYS']) expect(l[l.lastIndexOf(name) - 1]).toBe(' ')
  expect(l[1]).toBe('rate-limit.ts:313 · fn · read') // a symbol view keeps the file name and the line
  const file = draw({ file: 'src/lib/a.rs', tool: 'Read', why: [], tree: [], symbols: [{ name: 's', line: 1, n: 1 }] }).map(x => x.text)
  expect(file.slice(0, 2)).toEqual(['a.rs', 'src/lib · read'])
})

test('onToolCall: only `sieve skeleton` of a code file starts the file view; grep and map do not', async () => {
  for (const [command, isFile] of [['sieve skeleton src/x.ts', true], ['sieve grep src/x.ts', false], ['sieve map src/x.ts', false], ['sieve callers src/x.ts', false]] as const) {
    reset()
    const { $ } = fake(() => ok({}))
    await onToolCall($, { tool: 'Bash', command }, async () => ({ text: SAVED }))
    expect(queue.job?.isFile).toBe(isFile)
  }
})

test('onToolCall: the MCP tools sieve_why, sieve_repo_map and sieve_check_freshness map to why, map and check', async () => {
  for (const [tool, input, sub, arg] of [
    ['mcp__sieve__sieve_why', { symbol: 'f' }, 'why', 'f'],
    ['mcp__sieve__sieve_repo_map', {}, 'map', ''],
    ['mcp__sieve__sieve_check_freshness', {}, 'check', ''],
    ['mcp__sieve__why', { symbol: 'f' }, 'why', 'f'],
    ['mcp__sieve__repo_map', {}, 'map', ''],
    ['mcp__sieve__check_freshness', {}, 'check', ''],
  ] as const) {
    reset()
    const { $ } = fake(() => ok({}))
    await onToolCall($, { tool, ...input }, async () => ({ text: '' }))
    expect(queue.job?.query).toMatchObject({ sub, arg })
    expect(queue.job?.isFile).toBe(false)
  }
})
