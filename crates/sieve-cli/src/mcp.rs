//! The `sieve mcp` subcommand: runs the NDJSON JSON-RPC server over stdio
//!.

use std::env;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use clap::Args;

use crate::query;

/// Flags for `sieve mcp`.
#[derive(Args, Debug)]
pub struct McpArgs {
    /// The repo root. Default: the nearest folder with a graph; the walk stops at a linked git worktree.
    #[arg(value_name = "dir")]
    pub dir: Option<PathBuf>,
}

/// The state of the first index build.
struct BuildState {
    /// True while the build runs.
    pending: bool,
    /// The build error, kept for every call that waited.
    error: Option<String>,
}

static BUILD: Mutex<BuildState> = Mutex::new(BuildState {
    pending: false,
    error: None,
});
static BUILD_ENDED: Condvar = Condvar::new();

/// Locks the build state. A panic in the build thread poisons the lock;
/// the state is still valid, so the poison is ignored.
fn build_state() -> MutexGuard<'static, BuildState> {
    BUILD.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Ends the first build when it drops, on a success, an error or a panic,
/// so no waiting call stays blocked.
struct ReleaseWaiters;

impl Drop for ReleaseWaiters {
    fn drop(&mut self) {
        let mut state = build_state();
        if std::thread::panicking() {
            state
                .error
                .get_or_insert_with(|| "the index build failed".to_string());
        }
        state.pending = false;
        drop(state);
        BUILD_ENDED.notify_all();
    }
}

/// Starts the first index build on a thread and gates tool calls on it.
fn start_build(root: PathBuf, context_dir: PathBuf) {
    build_state().pending = true;
    sieve_daemon::tools::set_call_gate(wait_for_build);
    std::thread::spawn(move || {
        let _release = ReleaseWaiters;
        if let Err(message) = query::build_if_missing(&root, &context_dir, false) {
            crate::ui::print_error(&message);
            build_state().error = Some(message);
        }
    });
}

/// Blocks until the first index build ends, then gives the build error if
/// it failed. It returns at once when no build runs.
fn wait_for_build() -> Result<(), String> {
    let state = BUILD_ENDED
        .wait_while(build_state(), |state| state.pending)
        .unwrap_or_else(PoisonError::into_inner);
    state.error.clone().map_or(Ok(()), Err)
}

/// Runs `sieve mcp`: resolves the root and context dir, then serves one
/// NDJSON session on stdin/stdout/stderr until EOF. Exits 0 on EOF
///. The CLI `UPKEEP_SKIP` set holds `mcp`, but the server
/// runs the upkeep at boot itself. So the update nudge
/// goes to stderr and to the start of `instructions`. The nudge is
/// the CLI banner under the active product: same cache, same text.
pub fn run(args: &McpArgs, dir_override: Option<&Path>) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    let (root, note) = query::query_root_with_note(args.dir.as_deref(), dir_override, &cwd);
    let context_dir = query::context_dir(&root, dir_override, &cwd);

    // A repo with no index gets one on a thread, so the server answers
    // `initialize` at once. The tool list is full from the start, and a
    // tool call waits for the build. A failed build prints its error, and
    // the call returns the same error. `dir_override` only decides the tool
    // list; the warm index loads on the first call, after the gate.
    let building = query::wants_build(&root, &context_dir, false);
    if building {
        start_build(root.clone(), context_dir.clone());
    }

    sieve_daemon::tools::set_why_handler(crate::why::mcp_why);
    let stdin = BufReader::new(io::stdin());
    let stdout = io::stdout();
    let stderr = io::stderr();
    let code = sieve_daemon::serve_with_upkeep(
        &root,
        &context_dir,
        dir_override.is_some() || building,
        crate::version::current_version(),
        stdin,
        // No `.lock()`: a held lock blocks the writer thread of the warm index
        // when it prints a timing line. Only the main thread writes stdout.
        stdout,
        stderr,
        note.as_deref(),
        &[],
    );
    if code == 0 {
        Ok(())
    } else {
        Err(format!("mcp session exited with code {code}"))
    }
}
