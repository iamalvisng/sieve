__MARKER__
import { execFile } from "node:child_process"
import { Plugin } from "@opencode/plugin"

// The footer lives in tui.tsx. This part refuses a plain code search once and
// names the equal `__BIN__ find` command. The same command sent again runs.
// Any failure lets the call run.

// OpenCode names the tools `shell` and `grep`. The hook reads the Claude names.
const TOOL_NAMES: Record<string, string> = { shell: "Bash", grep: "Grep" }

function denyText(input: object, cwd: string): Promise<string> {
  return new Promise((resolve) => {
    const child = execFile(
      "__BIN__",
      ["hook", "pre-search-deny"],
      { cwd, timeout: 3_000 },
      (error, stdout) => resolve(error ? "" : stdout.trim()),
    )
    // A missing binary closes stdin early. Ignore that error.
    child.stdin?.on("error", () => {})
    child.stdin?.end(JSON.stringify(input))
  })
}

export default Plugin.define({
  id: "local.__BIN__-status.server",
  setup(context: any) {
    context.tool.hook("execute.before", async (event: any) => {
      const toolName = TOOL_NAMES[event.tool]
      if (!toolName) return
      let cwd: string | undefined
      try {
        const session = await context.session.get({ sessionID: event.sessionID })
        cwd = session?.location?.directory
      } catch {
        return
      }
      if (!cwd) return
      const text = await denyText(
        { tool_name: toolName, tool_input: event.input, session_id: event.sessionID, cwd },
        cwd,
      )
      if (text) throw new Error(text)
    })
  },
})
