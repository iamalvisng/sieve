//! The `sieve mcp` subcommand: runs the NDJSON JSON-RPC server over stdio
//! (the `mcp-server.md` note sections 6, 7).

use std::env;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};

use clap::Args;

use crate::query;

/// Flags for `sieve mcp`.
#[derive(Args, Debug)]
pub struct McpArgs {
    /// The repo root. Default: the nearest ancestor with a graph.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,
}

/// Runs `sieve mcp`: resolves the root and context dir, then serves one
/// NDJSON session on stdin/stdout/stderr until EOF. Exits 0 on EOF
/// (section 7). The CLI `UPKEEP_SKIP` set holds `mcp`, but the server
/// runs the upkeep at boot itself. So the update nudge
/// goes to stderr and to the start of `instructions` (P4-57). The nudge is
/// the CLI banner under the active product: same cache, same text.
pub fn run(args: &McpArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    let (root, note) = query::query_root_with_note(args.dir.as_deref(), dir_override, &cwd);
    let context_dir = query::context_dir(&root, dir_override, &cwd);

    sieve_daemon::tools::set_why_handler(crate::why::mcp_why);
    let stdin = BufReader::new(io::stdin());
    let stdout = io::stdout();
    let stderr = io::stderr();
    let code = sieve_daemon::serve_with_upkeep(
        &root,
        &context_dir,
        dir_override.is_some(),
        crate::version::current_version(),
        stdin,
        stdout.lock(),
        stderr.lock(),
        note.as_deref(),
        &[],
    );
    if code == 0 {
        Ok(())
    } else {
        Err(format!("mcp session exited with code {code}"))
    }
}
