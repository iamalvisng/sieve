//! The prelude every query command runs: find the repo root, refresh the
//! graph when it drifted, then load `wiring.json`.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

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
/// stderr.
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
        // .
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
        // A linked worktree has its own files, so main's index would give
        // the wrong lines. Stop here and let the first build run.
        if sieve_core::worktree::main_worktree_root(&current).is_some() {
            return (current, None);
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

/// The refresh switches: the flag, then the environment.
fn refresh_options(no_refresh_flag: bool) -> RefreshOptions {
    let p = product();
    RefreshOptions {
        disabled: no_refresh_flag || env_truthy(&p.env_var("NO_REFRESH")),
        force_hash: env::var(p.env_var("REFRESH")).as_deref() == Ok("hash"),
    }
}

/// Runs the pre-query refresh: rebuilds the graph, and only the graph,
/// when the fingerprint is missing or stale. At a workspace parent, the
/// refresh walks each child instead. Prints the refresh note to stderr.
/// A refresh error becomes a skip note. Only the first build of a repo
/// with no index can fail the query.
fn refresh_before(
    root: &Path,
    context_dir: &Path,
    no_refresh_flag: bool,
    first_build: bool,
) -> Result<(), String> {
    let opts = refresh_options(no_refresh_flag);
    let outcome = match workspace::read(context_dir) {
        Some(ws) => ensure_fresh_children(root, &ws.children, &opts),
        None => ensure_fresh_graph(root, context_dir, &opts),
    };
    if let Some(note) = refresh_note(&outcome) {
        eprintln!("{note}");
    }
    if first_build {
        build_if_missing(root, context_dir, no_refresh_flag)?;
    }
    Ok(())
}

/// Builds the index when `root` is a repo root with none, then prints one
/// note on stderr. Every other case is left alone, so the query prints its
/// usual no-index line:
/// - `root` has no `.git`, so it is not a repo root. A build there could
///   index a whole folder tree, such as a scratch dir or a workspace parent.
/// - `root` is the home folder, which is too large to index by surprise.
/// - `SIEVE_NO_AUTO_BUILD` is set.
///
/// A refresh switch (`--no-refresh` or `SIEVE_NO_REFRESH`) also stops the
/// build. A failed build returns the build error.
pub fn build_if_missing(
    root: &Path,
    context_dir: &Path,
    no_refresh_flag: bool,
) -> Result<(), String> {
    if !wants_build(root, context_dir, no_refresh_flag) {
        return Ok(());
    }
    let started = std::time::Instant::now();
    let Some(files) = crate::build::build_for_query(root, context_dir)? else {
        return Ok(());
    };
    eprintln!(
        "[{}] built the index ({files} {}, {})",
        product().name,
        if files == 1 { "file" } else { "files" },
        crate::build::elapsed_text(started.elapsed()).replace(' ', "")
    );
    Ok(())
}

/// Whether a query at `root` builds the missing index. The cases in the
/// list on [`build_if_missing`] answer no.
pub fn wants_build(root: &Path, context_dir: &Path, no_refresh_flag: bool) -> bool {
    !refresh_options(no_refresh_flag).disabled
        && !context_dir.join(".graph").join("wiring.json").is_file()
        && !workspace::workspace_path(context_dir).exists()
        && root.join(".git").exists()
        && !env_truthy(&product().env_var("NO_AUTO_BUILD"))
        && !is_home(root)
}

/// Whether `dir` is the home folder.
fn is_home(dir: &Path) -> bool {
    let Some(home) = env::var_os("HOME") else {
        return false;
    };
    match (std::fs::canonicalize(dir), std::fs::canonicalize(home)) {
        (Ok(dir), Ok(home)) => dir == home,
        _ => false,
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
    run_prelude_with(dir_arg, dir_override, no_refresh_flag, true)
}

/// Like [`run_prelude`]. With `first_build` off, a repo with no index keeps
/// the no-index stop. `blast` turns it off, because it reads a diff against
/// an index that already exists.
fn run_prelude_with(
    dir_arg: Option<&Path>,
    dir_override: Option<&Path>,
    no_refresh_flag: bool,
    first_build: bool,
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
    // A rebuild in this process hands over the graph it wrote, so the query
    // does not decode the file again. The guard lives until the read below,
    // and its drop clears any graph left unread. An older guard drops first,
    // so its drop cannot clear the new one.
    if let Ok(mut slot) = KEEP.lock() {
        *slot = None;
    }
    let keep = sieve_parse::refresh::keep_rebuilt_graph(&root);
    refresh_before(&root, &context_dir, no_refresh_flag, first_build)?;
    if let Ok(mut slot) = KEEP.lock() {
        *slot = Some(keep);
    }
    Ok((root, context_dir))
}

/// The guard of the last `run_prelude`, until `read_wiring_named` takes it.
static KEEP: Mutex<Option<sieve_parse::refresh::KeepRebuilt>> = Mutex::new(None);

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
    let (root, context_dir) = run_prelude_with(dir_arg, dir_override, no_refresh_flag, false)?;
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
    // The guard drops at the end of this function and clears the slot.
    let keep = KEEP.lock().ok().and_then(|mut slot| slot.take());
    if let Some(graph) = keep.as_ref().and_then(|keep| keep.take_graph(context_dir)) {
        return Ok(graph);
    }
    let bytes = std::fs::read(&wiring_path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(["-c", "user.email=t@t", "-c", "user.name=t"])
            .args(args)
            .output()
            .expect("run git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
    }

    #[test]
    fn a_nested_worktree_query_never_opens_the_main_index() {
        let tmp = std::env::temp_dir().join(format!("sieve-qroot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let main = std::fs::canonicalize({
            std::fs::create_dir_all(&tmp).unwrap();
            &tmp
        })
        .unwrap()
        .join("main");
        std::fs::create_dir_all(main.join("sieve/.graph")).unwrap();
        std::fs::write(main.join("sieve/.graph/wiring.json"), "{}").unwrap();
        git(&main, &["init", "-q"]);
        git(&main, &["commit", "-q", "--allow-empty", "-m", "x"]);
        let wt = main.join(".claude/worktrees/x");
        git(&main, &["worktree", "add", "-q", wt.to_str().unwrap()]);
        std::fs::create_dir_all(wt.join("src/deep")).unwrap();

        let (root, note) = query_root_with_note(None, None, &wt.join("src/deep"));
        assert_eq!(root, wt);
        assert_eq!(note, None);
        // The main checkout still answers from its own index.
        let (root, _) = query_root_with_note(None, None, &main.join(".claude"));
        assert_eq!(root, main);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
