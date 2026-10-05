# sieve-pane

A Claude Code mod. It shows a Tokyo Night dashboard for the code the agent reads and edits.

The pane (`/sieve-pane`) has these parts, top to bottom:
- Header: a pixel mascot, the symbol name in bold, and `file:line · kind · edited 9:58pm` in dim.
- Tiles: SAVED TODAY (green), THIS WEEK (blue) and IMPACT (green, orange or red).
- WHO DEPENDS ON THIS: the callers to depth 2 as a tree, 4 rows, then "+N more".
- IMPACT: a bar and "N callers · H hops". Green is 1 to 5, orange 6 to 20, red above 20.
- WHY IT EXISTS: the recorded decisions. A superseded one is red with ⚠.
- SAVED · LAST 7 DAYS: a bar chart, oldest left, today green.
A non-code file shows the resting mascot, the savings tiles and "not code · Sieve indexes code files only".

The mascot is a `Raster` in the terminal and an `Svg` on desktop and vscode. The phone has none.
It alternates 2 frames every 600 ms while the pane is open, and only blits a change.
It wags (frame 1) after a new saving. A red impact or a SUPERSEDED decision dims it and keeps frame 0.
The default mascot is sieve. The six mascots are sieve, soya, cloud, sifty, hoot and grit. The value `none` shows no mascot.
`/sieve-mascot <name>` saves the choice in `.sieve/config.json`, as `"mascot": "<name>"`.
You can also set that key by hand.
`assets/mascots.json` is the source. Run `node scripts/gen-mascots.mjs` to make `hooks/mascots.ts`.
The pane reacts to the Read, Edit and Write tools, and to Sieve queries made through Bash and MCP.
The band above the prompt shows, after an Edit, MultiEdit or Write:
`✎ checkApiKeyRateLimit and 2 more changed → 6 callers affected in 2 routes`, in the level color.
The saved numbers come from `sieve stats --json`, at most every 30 seconds.
Try it: run `sieve build`, put `sieve` on PATH, then `claude --plugin-dir <absolute path to this folder>`.
Run `claude plugin test <this folder>` for the tests. The mod never changes a tool call and makes no model call.
