//! The prelude every query command runs: find the repo root, refresh the
//! graph when it drifted, then load `wiring.json`.

use std::env;
use std::path::{Path, PathBuf};

use sieve_core::product::product;
use sieve_core::wiring::Graph;
use sieve_core::workspace;
use sieve_parse::refresh::{
    ensure_fresh_children, ensure_fresh_graph, env_truthy, refresh_note, RefreshOptions,
};

use crate::build::resolve_abs;

/// Resolves the repo root for a query command.
///
/// An explicit `dir_arg` wins outright, with no search. With no `dir_arg`
/// and no `dir_override` (`--dir`), this walks up from `cwd` to the
/// nearest ancestor whose `sieve/.graph/wiring.json` exists, with no level
/// limit, and prints the ancestor note when the walk moved at least one
/// level. `dir_override` alone short-circuits the walk to `cwd`.
pub fn query_root(dir_arg: Option<&Path>, dir_override: Option<&Path>, cwd: &Path) -> PathBuf {
    let (root, note) = query_root_with_note(dir_arg, dir_override, cwd);
    if let Some(note) = note {
        eprintln!("{note}");
    }
    root
}

/// Resolves the repo root exactly like [`query_root`], but returns the
/// ancestor note as a string instead of printing it. `sieve mcp` needs the
/// note text to hand to [`sieve_daemon::serve`], which prints it first on
/// stderr (`mcp-server.md` section 6).
pub fn query_root_with_note(
    dir_arg: Option<&Path>,
    dir_override: Option<&Path>,
    cwd: &Path,
) -> (PathBuf, Option<String>) {
    if let Some(dir) = dir_arg {
        return (resolve_abs(cwd, dir), None);
    }
    if dir_override.is_some() {
        return (cwd.to_path_buf(), None);
    }

    let mut levels = 0u32;
    let mut current = cwd.to_path_buf();
    let context_name = product().context_dir_name();
    loop {
        // A built repo, or a workspace parent that holds only the index
        // (P1-61).
        let context_dir = current.join(context_name);
        if context_dir.join(".graph/wiring.json").is_file()
            || workspace::workspace_path(&context_dir).exists()
        {
            let note = if levels > 0 {
                Some(format!(
                    "[{context_name}] no {context_name}/ here — answering from {}/{context_name}",
                    current.display()
                ))
            } else {
                None
            };
            return (current, note);
        }
        match current.parent() {
            Some(parent) => {
                current = parent.to_path_buf();
                levels += 1;
            }
            None => return (cwd.to_path_buf(), None),
        }
    }
}

/// Resolves the context dir: `dir_override` verbatim, else `<root>/sieve`.
pub fn context_dir(root: &Path, dir_override: Option<&Path>, cwd: &Path) -> PathBuf {
    match dir_override {
        Some(dir) => resolve_abs(cwd, dir),
        None => root.join(product().context_dir_name()),
    }
}

/// Runs the pre-query refresh: rebuilds the graph, and only the graph,
/// when the fingerprint is missing or stale. At a workspace parent, the
/// refresh walks each child instead (P2-39). Prints
/// the refresh note to stderr. Never fails a query: every error becomes
/// a skip note instead.
fn refresh_before(root: &Path, context_dir: &Path, no_refresh_flag: bool) {
    let p = product();
    let opts = RefreshOptions {
        disabled: no_refresh_flag || env_truthy(&p.env_var("NO_REFRESH")),
        force_hash: env::var(p.env_var("REFRESH")).as_deref() == Ok("hash"),
    };
    let outcome = match workspace::read(context_dir) {
        Some(ws) => ensure_fresh_children(root, &ws.children, &opts),
        None => ensure_fresh_graph(root, context_dir, &opts),
    };
    if let Some(note) = refresh_note(&outcome) {
        eprintln!("{note}");
    }
}

/// Runs the root walk, the context-dir resolution and the pre-query
/// refresh. If a given `--dir` or `[dir]` names a path that is not a
/// directory, it fails with "Directory not found: <as typed>". `grep`,
/// `ask`, `callers` and `skeleton` call this on their own, so they can
/// federate at a workspace parent before any graph load.
pub fn run_prelude(
    dir_arg: Option<&Path>,
    dir_override: Option<&Path>,
    no_refresh_flag: bool,
) -> Result<(PathBuf, PathBuf), String> {
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    let root = query_root(dir_arg, dir_override, &cwd);
    let context_dir = context_dir(&root, dir_override, &cwd);
    if let Some(typed) = dir_arg {
        if !root.is_dir() {
            return Err(format!(
                "directory not found: {} \u{2014} check the path",
                typed.display()
            ));
        }
    } else if let Some(typed) = dir_override {
        if !context_dir.is_dir() {
            return Err(format!(
                "directory not found: {} \u{2014} check the --dir path",
                typed.display()
            ));
        }
    }
    refresh_before(&root, &context_dir, no_refresh_flag);
    Ok((root, context_dir))
}

/// The one line every query command prints when no index exists.
pub const NO_INDEX: &str = "no index here yet \u{2014} run sieve build .";

/// The line a query command prints at a workspace parent, which holds no
/// index of its own.
pub const WORKSPACE_PARENT: &str =
    "this is a workspace parent \u{2014} run the command inside one repo, or give a path under one";

/// Runs the full query prelude and returns the repo root, the context dir,
/// and the loaded graph.
pub fn load_graph_named(
    dir_arg: Option<&Path>,
    dir_override: Option<&Path>,
    no_refresh_flag: bool,
) -> Result<(PathBuf, PathBuf, Graph), String> {
    let (root, context_dir) = run_prelude(dir_arg, dir_override, no_refresh_flag)?;
    let graph = read_wiring_named(&context_dir)?;
    Ok((root, context_dir, graph))
}

/// Reads `wiring.json` under `context_dir`. If the file is absent, it
/// fails with the workspace-parent line or the no-index line.
pub fn read_wiring_named(context_dir: &Path) -> Result<Graph, String> {
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    if !wiring_path.is_file() {
        let line = if workspace::read(context_dir).is_some() {
            WORKSPACE_PARENT
        } else {
            NO_INDEX
        };
        return Err(line.to_string());
    }
    let bytes = std::fs::read(&wiring_path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}
