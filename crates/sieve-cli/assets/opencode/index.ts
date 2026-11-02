__MARKER__
import { Plugin } from "@opencode/plugin"

// The footer lives in tui.tsx. OpenCode still expects a server part.
export default Plugin.define({
  id: "local.__BIN__-status.server",
  setup() {},
})
