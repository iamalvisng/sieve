// The view that the pane draws. One new tool call replaces the old view.
/** One tree row. `d` is the depth: 1 is a direct caller, 2 is a caller of a caller. */
export type Row = { d: 1 | 2; /** The index of the depth-1 caller that owns the row. */ p: number; label: string; tail: string }
/** One recorded decision. */
export type Why = { date: string; heading: string; superseded: boolean; by?: string }
/** A Sieve query that the agent ran by Bash or MCP. `saved` is the sum of the `[sieve] saved` lines. */
export type Query = { sub: string; arg: string; saved: number }
export type SievePaneView = {
  query?: Query
  file: string
  /** The tool that made the job: Read, Edit, MultiEdit or Write. */
  tool: string
  /** When the job started, in ms. */
  at?: number
  symbol?: string
  kind?: string
  line?: number
  why: Why[]
  /** The callers to depth 2, as rows. */
  tree: Row[]
  /** The caller count and the hops to depth 2. */
  n?: number
  hops?: number
  /** The most called symbols of a file. Set by a whole-file Read. */
  symbols?: { name: string; line: number; n: number }[]
  /** One line that says why the view is empty, such as "sieve not found". */
  note?: string
}
/** The blast of the working diff: the band text and the impact bar read it. */
export type Band = { sym: string; n: number; hops: number; routes: number }
/** The savings numbers. `spark` holds the last 7 days of tokens, oldest first. */
export type Stats = { tokens: number; dollars: number | null; spark: number[] }

declare module 'claude-code' {
  interface PluginState {
    'sieve-pane': { view: SievePaneView | null; band: Band | null; stats: Stats | null }
  }
}
