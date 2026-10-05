# Use Sieve with Claude Code

`sieve init` wires Sieve into your agents. This page covers Claude Code,
the pane, and what `init` writes for other agents.

## Preview first

```sh
sieve init --dry-run
```

This prints every file `init` would touch and writes nothing.

## What `sieve init` writes

Run `init` in the repo root. By default it writes only inside the repo.
For Claude Code it writes:

- `.claude/settings.json`: a status line, hook entries, a footer link rule
  and `Bash(sieve:*)` allow entries. Sieve keeps every entry that is not its own.
- `.claude/skills/sieve/SKILL.md`: the Sieve skill.
- `.mcp.json`: the `sieve` MCP server entry. Restart Claude Code to load it.

If `.claude/settings.json` already has a status line, Sieve keeps it and
prints a warning. Use `--no-statusline` to skip the status line.

## What `init` writes under your home folder

Without `--global`, `init` writes nothing outside the repo. `--no-global`
says so in the report. Only `--global` writes under your home folder. With `--global`, for Claude Code,
`init` merges the hook entries into `~/.claude/settings.json` and adds the
`sieve` server to `~/.claude.json`. Both files may be new. Other keys stay.

`--no-global` skips every write outside the repo.

## The hooks

The hooks run `sieve hook <name>`, so `sieve` must be on your `PATH`.
`init` warns if it is not.

- Read intercept: Sieve answers a file read from the index. A read that
  names `offset` or `limit` passes unchanged.
- Remembered reads: the second read of an unchanged file gets this note:
  "[sieve] a.ts is unchanged since your last read. Use that copy. If you no
  longer hold it, read again." The third read passes. A changed file passes.
- Hooks also run at session start, at each prompt and at stop.

`--no-hooks` skips hook installation for other agents.

## Other agents

Pick agents with `--agents <ids...>`, or `--all-agents`. `--list-agents`
prints the ids. In the repo, `init` writes:

| Agent | Files |
|---|---|
| agents, hermes, antigravity | `AGENTS.md` (a fenced Sieve section) |
| cursor | `.cursor/rules/sieve.mdc`, `.cursor/mcp.json`, `.cursor/hooks.json` |
| gemini | `GEMINI.md`, `.gemini/settings.json` |
| copilot | `.github/copilot-instructions.md` |
| grok | `.grok/skills/sieve/SKILL.md`, `.grok/config.toml` |
| kiro | `.kiro/steering/sieve.md`, `.kiro/settings/mcp.json` |
| windsurf | `.windsurf/rules/sieve.md` |
| adal | `.adal/skills/sieve/SKILL.md` |

With `--global`, Codex gets `~/.codex/config.toml` and `~/.codex/hooks.json`
when `~/.codex` exists. Antigravity gets `~/.gemini/config/mcp_config.json` and
`~/.gemini/skills/sieve/SKILL.md`. If `~/.config/opencode` exists, `init`
writes `opencode.json` in the repo.

## The pane

```sh
sieve init --mod
```

This installs the `sieve-pane` Claude Code mod in `.claude/skills/sieve-pane`
and enables it in `.claude/settings.json`.

- `/sieve-pane` opens the pane: the callers, the impact and the decision
  records of the code the agent reads and edits, and the saved tokens.
- After an edit, a band above the prompt shows how many callers the change affects.
- The status line has 3 lines.
- `/sieve-mascot <name>` sets the mascot. Names: `sieve` (default),
  `soya`, `cloud`, `sifty`, `hoot`, `grit`, `none`.
  Sieve saves the choice in `.sieve/config.json`.

## Remove Sieve

```sh
sieve uninstall        # lists what it would remove
sieve uninstall --yes  # removes it
```

`--keep-cache` keeps `sieve/`. `--no-global` leaves files outside the repo alone.
