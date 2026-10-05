//! The `build` subcommand: builds the graph, writes the `sieve/` tree, and
//! prints the Tier-1 build report (P2-26 to P2-32).

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Component, Path, PathBuf};

use clap::Args;

use crate::ojson::{OJson, OMap};
use crate::ui::{self, Ui};

use sieve_core::askindex::{ask_index_path, build_ask_index, write_ask_index};
use sieve_core::cards::{write_cards, write_index};
use sieve_core::covers::write_covers;
use sieve_core::fingerprint::{fingerprint_path, write_fingerprint, Fingerprint, Print};
use sieve_core::ignore::{ensure_gitignored, ensure_searchable};
use sieve_core::lock::{self, LockGuard};
use sieve_core::product::product;
use sieve_core::workspace;
use sieve_core::write_graph;
use sieve_parse::refresh::env_truthy;
use sieve_parse::{build_graph_cached_with, BuildOptions};

/// Flags for `sieve build`. `-e/--extensions` only warns on an unknown
/// extension.
#[derive(Args, Debug)]
pub struct BuildArgs {
    /// The repo root. Default: the current dir.
    #[arg(value_name = "dir", default_value = ".")]
    pub root_dir: PathBuf,

    /// Skips the `.gitignore` edit.
    #[arg(long = "no-gitignore")]
    pub no_gitignore: bool,

    /// Skips the `.ignore` edit.
    #[arg(long = "no-ignore")]
    pub no_ignore: bool,

    /// Runs the optional LSP enrichment layer (P2-23). Never on by
    /// default; a missing server or a server timeout degrades to a
    /// no-op.
    #[arg(long)]
    pub lsp: bool,
    /// Warns on each extension with no parser, then builds with every
    /// extension Sieve knows. A repeat adds to the list, as commander does.
    #[arg(short = 'e', long = "extensions", value_name = "exts", num_args = 1.., action = clap::ArgAction::Append)]
    pub extensions: Vec<String>,
    /// Bare `SKIP_DIRS` names this repo's walks keep. Persisted to
    /// `<root>/.<product>/config.json` before the build.
    #[arg(long = "include-dir", value_name = "name", allow_hyphen_values = true, action = clap::ArgAction::Append)]
    pub include_dir: Vec<String>,
    /// Includes initialized Git submodules recursively; persisted for later builds and automatic refreshes.
    #[arg(long = "follow-submodules", overrides_with = "no_follow_submodules")]
    pub follow_submodules: bool,
    /// Excludes Git submodules; persisted for later builds and automatic refreshes (default).
    #[arg(long = "no-follow-submodules", overrides_with = "follow_submodules")]
    pub no_follow_submodules: bool,
    /// Includes nested Git clones the index does not track (a multi-repo manifest checkout, or any repo cloned into the tree) as ONE graph, so imports across them resolve; persisted for later builds and automatic refreshes.
    #[arg(
        long = "follow-nested-repos",
        overrides_with = "no_follow_nested_repos"
    )]
    pub follow_nested_repos: bool,
    /// Excludes untracked nested Git clones; persisted for later builds and automatic refreshes (default).
    #[arg(
        long = "no-follow-nested-repos",
        overrides_with = "follow_nested_repos"
    )]
    pub no_follow_nested_repos: bool,
    /// Repo-relative prefixes to index; recorded in the fingerprint.
    #[arg(long = "only-dir", value_name = "path", allow_hyphen_values = true, action = clap::ArgAction::Append)]
    pub only_dir: Vec<String>,
    /// Parses every file instead of replaying the extraction cache.
    #[arg(long = "no-reuse")]
    pub no_reuse: bool,
}

impl BuildArgs {
    /// The follow choices given on the command line, as config keys.
    /// A flag not given leaves the persisted value.
    fn follow_patch(&self) -> Vec<(&'static str, bool)> {
        let mut out = Vec::new();
        for (key, on, off) in [
            (
                "followSubmodules",
                self.follow_submodules,
                self.no_follow_submodules,
            ),
            (
                "followNestedRepos",
                self.follow_nested_repos,
                self.no_follow_nested_repos,
            ),
        ] {
            if on || off {
                out.push((key, on));
            }
        }
        out
    }
}

/// The `--include-dir` check: a bare directory name, never a dot name and never
/// a path.
fn include_dir_error(name: &str) -> Option<String> {
    if name.starts_with('.') {
        return Some(format!(
            "--include-dir \"{name}\": dot-directories are never overridable"
        ));
    }
    if name.contains('/') || name.contains('\\') {
        return Some(format!(
            "--include-dir \"{name}\": expected a bare directory name, not a path"
        ));
    }
    None
}

/// The `normalizePathPrefix`: posix separators,
/// no leading `./`, no trailing slash. A bare `/` or `.` gives `""`.
fn normalize_path_prefix(path: &str) -> String {
    let mut out = path.replace('\\', "/");
    while let Some(rest) = out.strip_prefix("./") {
        out = rest.to_string();
    }
    out.trim_end_matches('/').to_string()
}

/// The array index a JS object key stands for, or `None`. JS lists these
/// keys first, in ascending order, whatever their place in the source.
fn js_index_key(key: &str) -> Option<u32> {
    let canonical =
        key == "0" || (!key.starts_with('0') && key.bytes().all(|b| b.is_ascii_digit()));
    canonical
        .then(|| key.parse::<u32>().ok())
        .flatten()
        .filter(|n| *n != u32::MAX)
}

/// Puts every object in a value in the order `JSON.stringify` writes it:
/// index keys first, then the rest in source order.
fn js_order(value: OJson) -> OJson {
    match value {
        OJson::Arr(items) => OJson::Arr(items.into_iter().map(js_order).collect()),
        OJson::Obj(pairs) => {
            let (mut indexed, rest): (Vec<_>, Vec<_>) = pairs
                .into_iter()
                .map(|(k, v)| (k, js_order(v)))
                .partition(|(k, _)| js_index_key(k).is_some());
            indexed.sort_by_key(|(k, _)| js_index_key(k));
            indexed.extend(rest);
            OJson::Obj(indexed)
        }
        other => other,
    }
}

/// Merges `includeDirs` into `<root>/.<product>/config.json` and adds the
/// config dir to `.gitignore` (`patchBuildConfig`). The write keeps every
/// other key the file holds.
fn patch_build_config(
    root: &Path,
    include_dirs: &[String],
    follow: &[(&str, bool)],
) -> Result<(), String> {
    let config_dir_name = product().home_dir_name();
    let path = root.join(&config_dir_name).join("config.json");
    let old = std::fs::read_to_string(&path).unwrap_or_default();
    // `OJson::parse` keeps every key's place and every value as written.
    let parsed = match OJson::parse(&old) {
        Some(v @ OJson::Obj(_)) => Some(v),
        Some(OJson::Null) => None,
        None if old.trim().is_empty() => None,
        // A file that is not a JSON object stays as it is.
        _ => {
            eprintln!("⚠ {} is not a JSON object; left unchanged", path.display());
            return Ok(());
        }
    };
    let mut config = OMap::from_existing(parsed.as_ref());
    if !include_dirs.is_empty() {
        let dirs = include_dirs.iter().map(OJson::s).collect();
        config.set("includeDirs", OJson::arr(dirs));
    }
    for (key, value) in follow {
        config.set(key, OJson::Bool(*value));
    }

    let gitignore = root.join(".gitignore");
    let current = std::fs::read_to_string(&gitignore).unwrap_or_default();
    let present = current.lines().any(|line| {
        let value = line.trim();
        value == config_dir_name
            || value == format!("{config_dir_name}/")
            || value == format!("/{config_dir_name}/")
    });
    if !present {
        let gap = if current.is_empty() {
            ""
        } else if current.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        let block = format!(
            "{gap}# {}'s local repository settings — not committed.\n/{config_dir_name}/\n",
            product().name
        );
        // Best effort.
        let _ = std::fs::write(&gitignore, format!("{current}{block}"));
    }

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = js_order(config.into_ojson()).to_pretty();
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, body).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

/// Runs `sieve build`: builds the graph, writes the wiring tree and the
/// cards, edits `.gitignore` and `.ignore`, and prints the build report.
///
/// A missing `dir` returns an `ENOENT`-shaped message; `main` prints that
/// message with no `✗` prefix, matching Sieve.
///
/// This never prints the update nudge itself (P1-47): `sieve init` calls
/// this function too, and `init` stays silent. `main`'s `Command::Build`
/// arm prints the nudge before this runs.
pub fn run(args: &BuildArgs, context_dir_override: Option<&Path>) -> Result<(), String> {
    let started = std::time::Instant::now();
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = resolve_abs(&cwd, &args.root_dir);

    // Sieve checks every flag before it touches the repo (P1-70): the extension
    // warning, then the two directory flags, each on its
    // first bad value.
    crate::check::warn_unknown_extensions(&args.extensions);
    if let Some(message) = args.include_dir.iter().find_map(|n| include_dir_error(n)) {
        return Err(message);
    }
    let only_dirs: Vec<String> = args
        .only_dir
        .iter()
        .map(|p| normalize_path_prefix(p))
        .filter(|p| !p.is_empty())
        .collect();
    if !args.only_dir.is_empty() && only_dirs.is_empty() {
        return Err(
            "--only-dir needs a non-empty repo path \u{2014} try --only-dir packages/app"
                .to_string(),
        );
    }

    if !root.is_dir() {
        return Err(format!(
            "directory not found: {} \u{2014} check the path, or run {} build . inside the repo",
            root.display(),
            product().name
        ));
    }

    // Persisted before the build, so this walk and every later one read
    // the same override.
    let follow = args.follow_patch();
    if !args.include_dir.is_empty() || !follow.is_empty() {
        patch_build_config(&root, &args.include_dir, &follow)?;
    }

    let context_dir = match context_dir_override {
        Some(dir) => resolve_abs(&cwd, dir),
        None => root.join(product().context_dir_name()),
    };

    let skip_gitignore = args.no_gitignore || env_truthy(&product().env_var("NO_GITIGNORE"));
    let skip_ignore = args.no_ignore || env_truthy(&product().env_var("NO_IGNORE"));

    // A workspace parent (P1-55): build each git child on its own, then
    // leave only `workspace.json` here.
    if workspace::is_build_root(&root, &context_dir) {
        return run_workspace(args, &root, &context_dir, skip_gitignore, skip_ignore);
    }

    let built = build_repo(
        &root,
        &context_dir,
        args,
        only_dirs,
        skip_gitignore,
        skip_ignore,
        true,
    )?;

    for error in &built.errors {
        ui::print_error(&error.to_string());
    }

    let rel = relative_path(&cwd, &context_dir);
    let index = match context_dir_override {
        // Print a `--dir` value as typed.
        Some(dir) => dir.display().to_string(),
        None if rel.is_empty() => format!("./{}", product().context_dir_name()),
        None if rel.starts_with("..") => rel,
        None => format!("./{rel}"),
    };
    let summary = Summary {
        name: repo_label(&root),
        elapsed: elapsed_text(started.elapsed()),
        files: built.files,
        symbols: built.nodes,
        links: built.edges,
        languages: built
            .languages
            .split(", ")
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect(),
        index,
        ignored: !skip_gitignore,
        unchanged: built.files > 0
            && built.parsed == 0
            && built.reused == built.files
            && built.errors.is_empty(),
    };
    let ui = Ui::stdout();
    let lines = summary_lines(&ui, &summary);
    match ui::mascot_rows(&root, ui.depth) {
        Some(mascot) => print!("{}", ui::beside_mascot(&mascot, &lines)),
        None => lines.iter().for_each(|l| println!("{l}")),
    }
    Ok(())
}

/// What the build summary says.
struct Summary {
    name: String,
    elapsed: String,
    files: usize,
    symbols: usize,
    links: usize,
    languages: Vec<String>,
    index: String,
    ignored: bool,
    /// True when no file changed since the last build.
    unchanged: bool,
}

/// The repo name in the build summary: the name of the root dir. A run with
/// the test clock set (`SIEVE_TEST_NOW`) prints `repo`, because a test repo
/// lives in a dir with a random name.
fn repo_label(root: &Path) -> String {
    if std::env::var_os("SIEVE_TEST_NOW").is_some() {
        return "repo".to_string();
    }
    root.file_name().map_or_else(
        || root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// The elapsed time, such as `1.8 s`. A run with the test clock set
/// (`SIEVE_TEST_NOW`) prints `0.0 s`, so a golden stays the same.
pub(crate) fn elapsed_text(elapsed: std::time::Duration) -> String {
    if std::env::var_os("SIEVE_TEST_NOW").is_some() {
        return "0.0 s".to_string();
    }
    format!("{:.1} s", elapsed.as_secs_f64())
}

/// The lines of the build summary, with color when `ui` takes color.
fn summary_lines(ui: &Ui, s: &Summary) -> Vec<String> {
    use sieve_savings::format_thousands as n;
    let name = product().name;
    if s.unchanged {
        return vec![format!(
            "{} {} {} {}",
            ui.green(&format!("{name} is up to date")),
            ui.dim("\u{b7}"),
            ui.dim(&format!(
                "{} {} unchanged",
                n(s.files as u64),
                if s.files == 1 { "file" } else { "files" }
            )),
            ui.dim(&format!("\u{b7} {}", s.elapsed)),
        )];
    }
    let keep = if s.ignored {
        "git-ignored \u{b7} stays on this machine"
    } else {
        "not git-ignored \u{b7} add it to your gitignore to keep it out of commits"
    };
    vec![
        format!(
            "{} sifted {} in {}",
            ui.fg(name),
            ui.purple(&s.name),
            ui.green(&s.elapsed)
        ),
        format!(
            "{} {} \u{2192} {} {} \u{b7} {} {}",
            ui.blue(&n(s.files as u64)),
            if s.files == 1 { "file" } else { "files" },
            ui.blue(&n(s.symbols as u64)),
            if s.symbols == 1 { "symbol" } else { "symbols" },
            ui.blue(&n(s.links as u64)),
            if s.links == 1 { "link" } else { "links" }
        ),
        ui.dim(&s.languages.join(" \u{b7} ")),
        ui.dim(&format!("index in {} \u{b7} {keep}", s.index)),
    ]
    .into_iter()
    .filter(|l| !l.is_empty())
    .collect()
}

/// The counts one repo's build report prints.
struct Built {
    nodes: usize,
    edges: usize,
    languages: String,
    parsed: usize,
    files: usize,
    reused: usize,
    /// Non-fatal write errors, printed after the report.
    errors: Vec<String>,
}

/// Builds one repo under its own lock: the graph, the ask sidecar, the
/// fingerprint, the ignore-file edits, the cards and the covers. Prints
/// the per-file progress only when `progress` is set; a workspace child
/// build prints none.
fn build_repo(
    root: &Path,
    context_dir: &Path,
    args: &BuildArgs,
    only_dirs: Vec<String>,
    skip_gitignore: bool,
    skip_ignore: bool,
    progress: bool,
) -> Result<Built, String> {
    // Sieve serializes the whole build under one lock, so two processes
    // never write the same `sieve/` tree at once. A caller that finds the
    // lock held waits up to `lock::LOCK_WAIT_MS`, then gives up.
    let _build_lock = acquire_build_lock(context_dir)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "a graph rebuild is already in flight \u{2014} wait, then run sieve build again"
                .to_string()
        })?;

    // `build_graph_cached` walks the repo, excludes the context dir, and
    // reads and hashes every claimed file exactly once. The CLI reuses
    // that walk order and those prints instead of reading the repo again.
    // The progress line shows only on a terminal.
    let live = progress && progress_wanted();
    let build_opts = BuildOptions {
        only_dirs: only_dirs.clone(),
        no_reuse: args.no_reuse,
        read_only: false,
        progress: live.then_some(show_progress as fn(usize, usize, &str)),
    };
    let mut report =
        build_graph_cached_with(root, context_dir, &build_opts).map_err(|e| e.to_string())?;

    // The LSP layer is opt-in (P2-23): `--lsp` only, never on by
    // default. Any failure — no server on `PATH`, a timeout, an early
    // exit — degrades to 0 added edges; the graph stays as the AST
    // resolver built it.
    if args.lsp {
        let lsp = sieve_parse::enrich_with_lsp_report(root, &mut report.graph);
        if live {
            eprint!("{}", lsp_progress(&lsp));
        }
    }
    if live {
        eprint!("\r\x1b[2K");
    }

    let graph = &report.graph;
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    write_graph(graph, &wiring_path).map_err(|e| e.to_string())?;

    // Section 3 order: `writeGraph`, then the ask sidecar, then
    // `writeFingerprint`, then the gitignore edits and the cards. A sidecar
    // write failure is never fatal; it is recorded and printed after the build
    // report instead. The build's own read errors come first, then the sidecar
    // write errors.
    let mut errors: Vec<String> = report.errors.clone();
    let ask_index = build_ask_index(graph);
    let ask_path = ask_index_path(context_dir);
    if let Err(e) = write_ask_index(&ask_path, &ask_index) {
        errors.push(format!("ask-index: {e}"));
    }

    // F6: the why sidecar. A write failure is never fatal.
    if let Err(e) = crate::why::write_after_build(root, context_dir, graph) {
        errors.push(format!("why-refs: {e}"));
    }

    write_build_fingerprint(context_dir, report.prints, only_dirs).map_err(|e| e.to_string())?;

    ensure_gitignored(root, context_dir, skip_gitignore).map_err(|e| e.to_string())?;
    ensure_searchable(root, context_dir, skip_ignore).map_err(|e| e.to_string())?;

    let card_stats = write_cards(graph, context_dir).map_err(|e| e.to_string())?;
    write_index(context_dir, &card_stats).map_err(|e| e.to_string())?;

    // `writeCovers` runs only on an explicit `sieve build`, never on the
    // query-path refresh (deep-tier.md section 3). It backfills the
    // `covers:` block into every existing concept file's frontmatter.
    write_covers(context_dir, &graph.nodes).map_err(|e| e.to_string())?;

    Ok(Built {
        nodes: graph.nodes.len(),
        edges: graph.edges.len(),
        languages: graph.meta.languages.join(", "),
        parsed: report.parsed,
        files: report.files,
        reused: report.reused,
        errors,
    })
}

/// Runs the workspace build (P1-55): prints the migration note when a mega
/// graph sits here, builds each Reports the context dir when it is the
/// workspace root, an ancestor of the root, or a child dir or its ancestor, by
/// canonical path. `None` when the context dir is safe to delete, or does not
/// exist yet.
fn context_dir_covers_repos(
    root: &Path,
    context_dir: &Path,
    children: &[String],
) -> Option<PathBuf> {
    let context = std::fs::canonicalize(context_dir).ok()?;
    let mut repos = vec![root.to_path_buf()];
    repos.extend(children.iter().map(|c| root.join(c)));
    let covered = repos
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|r| r.starts_with(&context));
    covered.then_some(context)
}

/// git child into its own context dir, deletes the parent's context dir,
/// writes `workspace.json`, and gitignores it.
///
/// A child build takes no `--only-dir` and prints no progress; a
/// `--include-dir` is persisted into each child first.
fn run_workspace(
    args: &BuildArgs,
    root: &Path,
    context_dir: &Path,
    skip_gitignore: bool,
    skip_ignore: bool,
) -> Result<(), String> {
    let name = product().context_dir_name();
    let children = workspace::discover_children(root);
    // A deliberate safety rule, to prevent the loss of the child repos: `build
    // --dir <root>` at a workspace parent makes the context dir the root
    // itself, and the delete below would then remove every child repo. Sieve
    // refuses before any child build and before any delete.
    if let Some(path) = context_dir_covers_repos(root, context_dir, &children) {
        return Err(format!(
            "the context dir {} holds the workspace root or a child repo, and a workspace build deletes \
             the context dir \u{2014} pass --dir with a path outside the repos",
            path.display()
        ));
    }
    if context_dir.join(".graph").join("wiring.json").exists() {
        eprintln!("{}", workspace::migration_note(&children));
    }
    eprintln!(
        "building {} workspace repos: {}",
        children.len(),
        children.join(", ")
    );

    for child in &children {
        let child_root = root.join(child);
        let follow = args.follow_patch();
        if !args.include_dir.is_empty() || !follow.is_empty() {
            patch_build_config(&child_root, &args.include_dir, &follow)?;
        }
        let built = build_repo(
            &child_root,
            &child_root.join(name),
            args,
            Vec::new(),
            skip_gitignore,
            skip_ignore,
            false,
        )?;
        println!(
            "sifted {child}/ \u{b7} {} \u{2192} {} \u{b7} {}",
            sieve_core::voice::count(built.files, "file"),
            sieve_core::voice::count(built.nodes, "symbol"),
            sieve_core::voice::count(built.edges, "link")
        );
        for error in &built.errors {
            crate::ui::print_error(&format!("{child}/: {error}"));
        }
    }

    // The parent keeps only the index: the mega graph, its cache and its
    // cards go.
    if let Err(e) = std::fs::remove_dir_all(context_dir) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e.to_string());
        }
    }
    workspace::write(context_dir, &children).map_err(|e| e.to_string())?;
    ensure_gitignored(root, context_dir, skip_gitignore).map_err(|e| e.to_string())?;

    println!(
        "{} sifted {} workspace repos \u{b7} index in {name}/workspace.json",
        product().name,
        children.len()
    );
    println!(
        "{name}/ is git-ignored \u{b7} each teammate runs {} build to make it locally",
        product().name
    );
    Ok(())
}

/// A held build lock that releases itself when dropped.
///
/// This wraps either the real [`LockGuard`], taken on the first try, or a
/// plain context dir path, taken after a wait: [`lock::wait_for_lock`]
/// reports only success or failure, with no guard of its own, so the
/// waited case releases through [`lock::release`] by hand instead.
pub(crate) enum BuildLock {
    // The guard is never read; it is held only so its own `Drop` runs
    // when `BuildLock` drops.
    Immediate(#[allow(dead_code)] LockGuard),
    Waited(PathBuf),
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        if let BuildLock::Waited(context_dir) = self {
            lock::release(context_dir);
        }
        // The `Immediate` case releases itself: `LockGuard` has its own
        // `Drop`, which fires right after this one.
    }
}

/// Takes the build lock, waiting once when another process holds it.
///
/// Returns `None` only when the wait in [`lock::wait_for_lock`] expires
/// with the lock still held elsewhere.
pub(crate) fn acquire_build_lock(context_dir: &Path) -> std::io::Result<Option<BuildLock>> {
    if let Some(guard) = LockGuard::acquire(context_dir)? {
        return Ok(Some(BuildLock::Immediate(guard)));
    }
    if lock::wait_for_lock(context_dir)? {
        Ok(Some(BuildLock::Waited(context_dir.to_path_buf())))
    } else {
        Ok(None)
    }
}

/// Writes the freshness fingerprint for this build (P2-33): one [`Print`]
/// per claimed file, taken from the extractor's own read of that file, so
/// the hash covers the same bytes `build_graph_cached` parsed.
pub(crate) fn write_build_fingerprint(
    context_dir: &Path,
    prints: BTreeMap<String, Print>,
    only_dirs: Vec<String>,
) -> std::io::Result<()> {
    let stamp = sieve_parse::extractor_stamp();
    let fingerprint = Fingerprint {
        version: 1,
        extractor: stamp.clone(),
        files: prints,
        only_dirs: (!only_dirs.is_empty()).then_some(only_dirs),
    };
    let path = fingerprint_path(context_dir, &stamp);
    write_fingerprint(&path, &fingerprint)
}

/// True when stderr is a terminal that takes a line that rewrites itself.
fn progress_wanted() -> bool {
    std::io::stderr().is_terminal() && std::env::var("TERM").map_or(true, |t| t != "dumb")
}

/// Draws the one progress line, such as `sifting 812 / 1,583 · src/a.ts`. The
/// line rewrites itself. It redraws every 8 files and at the end.
fn show_progress(done: usize, total: usize, path: &str) {
    use sieve_savings::format_thousands as n;
    if !done.is_multiple_of(8) && done != total {
        return;
    }
    let ui = Ui::stderr();
    let field = progress_field(path);
    eprint!(
        "\r\x1b[2K{} {} / {} {} {}",
        ui.fg("sifting"),
        ui.blue(&n(done as u64)),
        ui.blue(&n(total as u64)),
        ui.dim("\u{b7}"),
        ui.dim(field.trim_end())
    );
}

/// The one `--lsp` progress line: `summarizing {added+1}/{queried}:
/// lsp:{server}`, the field cut to 50 chars and padded.
fn lsp_progress(r: &sieve_parse::LspReport) -> String {
    let server = r.server.as_deref().unwrap_or("none");
    format!(
        "\rsummarizing {}/{}: {}",
        r.added + 1,
        r.queried,
        progress_field(&format!("lsp:{server}"))
    )
}

/// Truncates `path` to 50 UTF-16 units, then right-pads it to 50 units with
/// spaces, matching `path.slice(0, 50).padEnd(50)`. A cut inside a surrogate
/// pair leaves a lone high surrogate, which Node writes as U+FFFD.
fn progress_field(path: &str) -> String {
    let mut out = String::new();
    let mut units = 0;
    for c in path.chars() {
        let len = c.len_utf16();
        if units + len > 50 {
            if units == 49 {
                out.push('\u{FFFD}');
                units = 50;
            }
            break;
        }
        out.push(c);
        units += len;
    }
    out.extend(std::iter::repeat_n(' ', 50 - units));
    out
}

/// Resolves `path` to an absolute path against `base`, purely lexically:
/// no symlink resolution, matching Node's `path.resolve`.
pub(crate) fn resolve_abs(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize_lexical(&joined)
}

/// Collapses `.` and `..` components lexically, without touching the file
/// system.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out.into_iter().collect()
}

/// Returns the `/`-joined relative path from `from` to `to`, matching
/// Node's `path.relative` on two already-absolute paths. Returns `""` when
/// the two paths are equal.
fn relative_path(from: &Path, to: &Path) -> String {
    let from_parts = normal_parts(from);
    let to_parts = normal_parts(to);
    let common = from_parts
        .iter()
        .zip(to_parts.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = from_parts.len() - common;
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_string(), ups).collect();
    parts.extend(to_parts[common..].iter().cloned());
    parts.join("/")
}

/// Collects the named path components of an absolute path as strings,
/// dropping the root marker.
fn normal_parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(unchanged: bool) -> Summary {
        Summary {
            name: "vite".to_string(),
            elapsed: "1.8 s".to_string(),
            files: 1583,
            symbols: 4866,
            links: 10055,
            languages: vec!["typescript".to_string(), "javascript".to_string()],
            index: "./sieve".to_string(),
            ignored: true,
            unchanged,
        }
    }

    #[test]
    fn the_summary_names_the_repo_and_counts_with_commas() {
        let lines = summary_lines(&Ui::plain(), &summary(false));
        assert_eq!(
            lines,
            [
                "sieve sifted vite in 1.8 s",
                "1,583 files \u{2192} 4,866 symbols \u{b7} 10,055 links",
                "typescript \u{b7} javascript",
                "index in ./sieve \u{b7} git-ignored \u{b7} stays on this machine",
            ]
        );
    }

    #[test]
    fn a_warm_build_with_no_change_says_so_in_one_line() {
        let lines = summary_lines(&Ui::plain(), &summary(true));
        assert_eq!(
            lines,
            ["sieve is up to date \u{b7} 1,583 files unchanged \u{b7} 1.8 s"]
        );
    }

    #[test]
    fn the_summary_has_no_escape_code_without_color() {
        let text = summary_lines(&Ui::plain(), &summary(false)).join("\n");
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn test_p2_23_dv21_lsp_progress_line_matches_golden() {
        let found = sieve_parse::LspReport {
            added: 3,
            queried: 7,
            server: Some("/x/rust-analyzer".to_string()),
        };
        let line = lsp_progress(&found);
        assert_eq!(
            line,
            format!("\rsummarizing 4/7: {:<50}", "lsp:/x/rust-analyzer")
        );
        let none = sieve_parse::LspReport {
            added: 0,
            queried: 0,
            server: None,
        };
        assert_eq!(
            lsp_progress(&none),
            format!("\rsummarizing 1/0: {:<50}", "lsp:none")
        );
    }

    #[test]
    fn progress_field_truncates_and_pads_to_50() {
        let field = progress_field("src/a.ts");
        assert_eq!(field.chars().count(), 50);
        assert!(field.starts_with("src/a.ts"));

        let long = "x".repeat(80);
        let field = progress_field(&long);
        assert_eq!(field, "x".repeat(50));
    }

    /// Expected bytes come from a recorded run on a scratch repo.
    #[test]
    fn test_p2_23_progress_field_counts_utf16_units_like_golden() {
        let e = '\u{1F600}';
        // Two astral chars count 4 units: 45 + 4 + 1 = 50, no padding.
        let path = format!("{}{e}{e}bbbb.ts", "a".repeat(45));
        assert_eq!(progress_field(&path), format!("{}{e}{e}b", "a".repeat(45)));
        // The cut falls inside a pair: Node writes the lone surrogate as U+FFFD.
        let path = format!("{}{e}.ts", "c".repeat(49));
        assert_eq!(progress_field(&path), format!("{}\u{FFFD}", "c".repeat(49)));
        // A short path pads by units: one astral char takes 2 of 50.
        assert_eq!(
            progress_field(&e.to_string()),
            format!("{e}{}", " ".repeat(48))
        );
    }

    #[test]
    fn relative_path_masks_a_common_prefix_and_adds_up_dirs() {
        let from = Path::new("/a/b/c");
        let to = Path::new("/a/x/sieve");
        assert_eq!(relative_path(from, to), "../../x/sieve");
    }

    #[test]
    fn relative_path_is_empty_for_equal_paths() {
        let path = Path::new("/a/b");
        assert_eq!(relative_path(path, path), "");
    }

    #[test]
    fn resolve_abs_collapses_parent_dir_components() {
        let base = Path::new("/repo");
        let resolved = resolve_abs(base, Path::new("sub/../sieve"));
        assert_eq!(resolved, PathBuf::from("/repo/sieve"));
    }

    /// P1-70: `--include-dir` takes a bare name; `--only-dir` normalizes
    /// to the `--in` form and rejects a prefix that normalizes to `""`.
    #[test]
    fn test_p1_70_include_dir_and_only_dir_checks() {
        assert!(include_dir_error(".x").is_some_and(|m| m.contains("dot-directories")));
        assert!(include_dir_error("a/b").is_some_and(|m| m.contains("not a path")));
        assert!(include_dir_error("a\\b").is_some());
        assert_eq!(include_dir_error("dist"), None);

        assert_eq!(normalize_path_prefix("./src/"), "src");
        assert_eq!(normalize_path_prefix("a\\b//"), "a/b");
        assert_eq!(normalize_path_prefix("/"), "");
        assert_eq!(normalize_path_prefix("./"), "");
    }

    /// P1-70: a repeated `-e`, `--include-dir` or `--only-dir` adds to the
    /// list, as commander's `[...prev, val]` reducer does, even under the
    /// global `args_override_self`.
    #[test]
    fn test_p1_70_repeated_list_flags_accumulate() {
        use clap::Parser;
        #[derive(Parser)]
        #[command(args_override_self = true)]
        struct Cli {
            #[command(flatten)]
            build: BuildArgs,
        }
        let cli = Cli::try_parse_from([
            "sieve",
            "-e",
            "ts",
            "-e",
            "js",
            "--include-dir",
            "a",
            "--include-dir",
            "b",
            "--only-dir",
            "x",
            "--only-dir",
            "y",
        ])
        .expect("parse");
        assert_eq!(cli.build.extensions, ["ts", "js"]);
        assert_eq!(cli.build.include_dir, ["a", "b"]);
        assert_eq!(cli.build.only_dir, ["x", "y"]);
    }

    /// P1-70: `patch_build_config` merges `includeDirs` into the existing
    /// config and adds the config dir to `.gitignore` once.
    #[test]
    fn test_p1_70_patch_build_config_merges_and_ignores_once() {
        let dir = std::env::temp_dir().join(format!(
            "sieve-build-config-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".sieve")).expect("mkdir");
        std::fs::write(
            dir.join(".sieve/config.json"),
            r#"{"followSubmodules":true}"#,
        )
        .expect("seed config");
        std::fs::write(dir.join(".gitignore"), "/sieve/\n").expect("seed gitignore");

        patch_build_config(&dir, &["dist".to_string()], &[]).expect("patch");
        patch_build_config(&dir, &["dist".to_string(), "out".to_string()], &[]).expect("patch");

        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join(".sieve/config.json")).expect("read"))
                .expect("json");
        assert_eq!(config["followSubmodules"], true);
        assert_eq!(config["includeDirs"], serde_json::json!(["dist", "out"]));
        let gitignore = std::fs::read_to_string(dir.join(".gitignore")).expect("read");
        assert_eq!(
            gitignore,
            "/sieve/\n\n# sieve's local repository settings — not committed.\n/.sieve/\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The expected `patchBuildConfig` output on this input: index keys
    /// first, the rest in file order, `1.0` as `1`, `null` kept, then the
    /// new keys in patch order. Recorded from a build with `--include-dir vendor
    /// --follow-nested-repos`.
    #[test]
    fn test_p1_70_patch_build_config_matches_golden_bytes() {
        let dir = std::env::temp_dir().join(format!(
            "sieve-build-config-bytes-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".sieve")).expect("mkdir");
        std::fs::write(
            dir.join(".sieve/config.json"),
            r#"{"zeta":1.0,"alpha":null,"mid":{"b":[],"a":1.50},"e":1e2,"followSubmodules":false,"3":1,"1":2,"s":"é\u0001"}"#,
        )
        .expect("seed config");

        patch_build_config(
            &dir,
            &["vendor".to_string()],
            &[("followNestedRepos", true)],
        )
        .expect("patch");

        let written = std::fs::read_to_string(dir.join(".sieve/config.json")).expect("read");
        assert_eq!(
            written,
            "{\n  \"1\": 2,\n  \"3\": 1,\n  \"zeta\": 1,\n  \"alpha\": null,\n  \"mid\": {\n    \"b\": [],\n    \"a\": 1.5\n  },\n  \"e\": 100,\n  \"followSubmodules\": false,\n  \"s\": \"é\\u0001\",\n  \"includeDirs\": [\n    \"vendor\"\n  ],\n  \"followNestedRepos\": true\n}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Runs `patch_build_config` on a seed config, with no new keys but
    /// `followSubmodules`, and returns the file text.
    fn patch_seed(label: &str, seed: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "sieve-build-config-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".sieve")).expect("mkdir");
        std::fs::write(dir.join(".sieve/config.json"), seed).expect("seed config");
        patch_build_config(&dir, &[], &[("followSubmodules", true)]).expect("patch");
        let written = std::fs::read_to_string(dir.join(".sieve/config.json")).expect("read");
        let _ = std::fs::remove_dir_all(&dir);
        written
    }

    /// The expected output for index keys: only canonical indexes below 2^32-1
    /// go first, in nested objects and arrays too.
    #[test]
    fn test_p1_70_patch_build_config_index_key_order_matches_golden() {
        let seed = r#"{"01":0,"-1":1,"4294967295":2,"4294967294":3,"1.5":4,"b":5," 1":6,"":7,"0":8,"o":{"b":1,"9":2,"1":[{"z":1,"2":2}]}}"#;
        assert_eq!(
            patch_seed("order", seed),
            "{\n  \"0\": 8,\n  \"4294967294\": 3,\n  \"01\": 0,\n  \"-1\": 1,\n  \"4294967295\": 2,\n  \"1.5\": 4,\n  \"b\": 5,\n  \" 1\": 6,\n  \"\": 7,\n  \"o\": {\n    \"1\": [\n      {\n        \"2\": 2,\n        \"z\": 1\n      }\n    ],\n    \"9\": 2,\n    \"b\": 1\n  },\n  \"followSubmodules\": true\n}"
        );
    }

    /// A config Sieve cannot read (a lone surrogate, an array root) stays
    /// as the user wrote it. A lenient parser would keep the surrogate file's keys
    /// and rewrite the array with `"0"` keys; Sieve leaves both alone.
    #[test]
    fn test_p1_70_patch_build_config_leaves_an_unreadable_file() {
        for seed in [r#"{"u":"\ud800","keep":1}"#, "[1,2]", "{bad"] {
            assert_eq!(patch_seed("bad", seed), seed);
        }
    }
}
