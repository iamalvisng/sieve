__MARKER__
import { execFile } from "node:child_process"
import { existsSync } from "node:fs"
import { join } from "node:path"
import { Plugin } from "@opencode/plugin/tui"

const REFRESH_MS = 30_000

// `__BIN__ statusline --format plain` prints one line with no color code.
function run(cwd: string): Promise<string> {
  return new Promise((resolve) => {
    const child = execFile(
      "__BIN__",
      ["statusline", "--format", "plain"],
      { cwd, timeout: 5_000 },
      (error, stdout) => {
        resolve(error ? "" : stdout.trim())
      },
    )
    // A missing binary closes stdin early. Ignore that error.
    child.stdin?.on("error", () => {})
    child.stdin?.end("{}")
  })
}

export default Plugin.define({
  id: "local.__BIN__-status",
  setup(context) {
    const directory = () => (context.location ?? context.data.location.default())?.directory
    const [state, update] = context.storage.memory("line", { initial: { text: "" } })

    let busy = false
    const refresh = async () => {
      const dir = directory()
      if (busy || !dir || !existsSync(join(dir, "__CONTEXT_DIR__"))) return
      busy = true
      try {
        const text = await run(dir)
        update((draft) => {
          draft.text = text
        })
      } finally {
        busy = false
      }
    }

    refresh()
    const timer = setInterval(refresh, REFRESH_MS)
    const stopEvents = context.data.on("session.execution.succeeded", () => refresh())

    const render = () => <text fg={context.theme.text.muted ?? context.theme.text.base}>{state.text}</text>
    const stopPrompt = context.ui.slot({ append: "prompt.footer.status", render })
    const stopHome = context.ui.slot({ append: "home.footer.status", render })

    return () => {
      clearInterval(timer)
      stopEvents()
      stopPrompt?.()
      stopHome?.()
    }
  },
})
