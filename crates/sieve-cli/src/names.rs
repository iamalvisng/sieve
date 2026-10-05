//! Product strings for `sieve init`, `sieve uninstall`, the hook, and the
//! statusline commands. A rename touches only this file (the `hosts-hooks.md`
//! note section 3, the ledger entry of 2026-09-12, "Sieve writes no .cjs
//! shim").

/// The binary name every written command line invokes. Sieve resolves
/// this name by `PATH`, the way Sieve resolves the `sieve` binary for the
/// MCP entry — never a local shim file.
pub const BIN: &str = "sieve";

/// Builds the shell command a hook config runs for one hook sub-command,
/// for example `sieve hook post-edit`.
pub fn hook_command(sub: &str) -> String {
    format!("{BIN} hook {sub}")
}

/// The statusline command every settings file installs.
pub fn statusline_command() -> String {
    format!("{BIN} statusline")
}

/// The MCP server command Sieve registers into every `mcpServers.sieve`
/// entry.
pub const MCP_COMMAND: &str = BIN;

/// The MCP server args Sieve registers alongside [`MCP_COMMAND`].
pub const MCP_ARGS: [&str; 1] = ["mcp"];
