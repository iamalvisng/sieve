//! The pre-query graph refresh, shared by `sieve-cli` and `sieve-daemon`
//! (`query-prelude-grep.md` sections 1.5 and 1.6,
//! `build-cards-freshness.md` sections 3 and 7).
//!
//! `sieve-cli`'s `refresh_before` and `sieve-daemon`'s `refresh_before`
//! were two copies of the same logic, because `sieve-daemon` could not
//! depend on the `sieve-cli` binary crate. This module holds the one
//! definition; both callers switch to it in a later task.

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use sieve_core::askindex::{ask_index_path, build_ask_index, write_ask_index};
use sieve_core::fingerprint::{
    clear_racy_state, fingerprint_mtime_ms, fingerprint_path, probe_drift_racy, probe_drift_walk,
    read_fingerprint, write_fingerprint, Drift, Fingerprint, Print,
};
use sieve_core::lock::{self, LockGuard};
use sieve_core::lookup::{self, wiring_stamp, Lookup, DEFAULT_WIRING_CAP_BYTES};
use sieve_core::wiring::Graph;
use sieve_core::write_graph_kept;

use crate::build::{build_graph_cached_with, BuildOptions};

/// A held build lock that releases itself when dropped.
///
/// This wraps either the real [`LockGuard`], taken on the first try, or a
/// plain context dir path, taken after a wait: [`lock::wait_for_lock`]
/// reports only success or failure, with no guard of its own, so the
/// waited case releases through [`lock::release`] by hand instead.
pub(crate) enum RefreshLock {
    // The guard is never read; it is held only so its own `Drop` runs
    // when `RefreshLock` drops.
    Immediate(#[allow(dead_code)] LockGuard),
    Waited(std::path::PathBuf),
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        if let RefreshLock::Waited(context_dir) = self {
            lock::release(context_dir);
        }
        // The `Immediate` case releases itself: `LockGuard` has its own
        // `Drop`, which fires right after this one.
    }
}

/// Writes a fingerprint with a new walk start, after a probe hashed the racy
/// files and found no drift. It takes the build lock without a wait, and
/// writes only if no rebuild changed the record meanwhile. A failure is
/// harmless: the next query hashes again.
fn clear_racy_under_lock(
    context_dir: &Path,
    fp_path: &Path,
    stamp: &str,
    fp: &Fingerprint,
    latest_racy_mtime_ms: f64,
) {
    let Ok(Some(_guard)) = LockGuard::acquire(context_dir) else {
        return;
    };
    if read_fingerprint(fp_path, stamp).as_ref() == Some(fp) {
        let _ = clear_racy_state(fp_path, fp, latest_racy_mtime_ms);
    }
}

/// Keeps the walk list of a probe only when the lock was taken on the first
/// try. After a wait, another process may have changed the tree, so the
/// rebuild walks again.
fn walk_to_reuse(lock: &RefreshLock, walked: Option<Vec<PathBuf>>) -> Option<Vec<PathBuf>> {
    match lock {
        RefreshLock::Immediate(_) => walked,
        RefreshLock::Waited(_) => None,
    }
}

/// Takes the build lock, waiting once when another process holds it.
///
/// Returns `None` only when the wait in [`lock::wait_for_lock`] expires
/// with the lock still held elsewhere.
pub(crate) fn acquire_refresh_lock(context_dir: &Path) -> std::io::Result<Option<RefreshLock>> {
    if let Some(guard) = LockGuard::acquire(context_dir)? {
        return Ok(Some(RefreshLock::Immediate(guard)));
    }
    if lock::wait_for_lock(context_dir)? {
        Ok(Some(RefreshLock::Waited(context_dir.to_path_buf())))
    } else {
        Ok(None)
    }
}

/// The flags that shape one refresh call.
#[derive(Debug, Default, Clone, Copy)]
pub struct RefreshOptions {
    /// Skips the refresh outright (a `--no-refresh` flag, or
    /// `<PRODUCT>_NO_REFRESH`).
    pub disabled: bool,
    /// Forces a content hash over a stat-only drift probe
    /// (`<PRODUCT>_REFRESH=hash`).
    pub force_hash: bool,
}

/// What one refresh call did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefreshOutcome {
    /// Whether this call rebuilt the graph.
    pub refreshed: bool,
    /// The drift count that drove the rebuild, when known.
    pub changed_files: usize,
    /// The one line a caller prints or prepends, when there is one.
    pub note: Option<String>,
}

/// The total files a [`Drift`] reports as added, changed, or removed.
fn drift_count(drift: &Drift) -> usize {
    drift.added.len() + drift.changed.len() + drift.removed.len()
}

/// What a graph-only rebuild claimed and parsed, so a caller can report
/// it without re-walking the repo.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RebuildReport {
    /// The claimed file count.
    pub files: usize,
    /// The files this rebuild parsed, instead of replaying from cache.
    pub parsed: usize,
    /// The files this rebuild replayed from cache.
    pub reused: usize,
}

/// The path of the per-file lookup index under `context_dir`.
pub fn lookup_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".cache").join("lookup.v1")
}

/// Writes `wiring.json`, then the lookup index with the stamp of the new
/// wiring (S3, E4). The refresh calls it now. The CLI build site calls it
/// in task T3. A failed lookup write deletes any old lookup and still
/// returns `Ok`, so the readers fall back to the wiring.
pub fn write_wiring_and_lookup(graph: &Graph, context_dir: &Path) -> io::Result<()> {
    write_wiring_and_lookup_kept(graph, context_dir).map(|_| ())
}

/// Like [`write_wiring_and_lookup`], and also returns the graph as written
/// with the stamp of its `wiring.json`.
fn write_wiring_and_lookup_kept(
    graph: &Graph,
    context_dir: &Path,
) -> io::Result<((u64, u64), Graph)> {
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    let mut timer = crate::timing::Timer::start();
    let (stamp, written) = write_graph_kept(graph, &wiring_path)?;
    timer.lap("write wiring");
    let path = lookup_path(context_dir);
    if lookup::write(graph, &path, stamp).is_err() {
        let _ = fs::remove_file(&path);
    }
    timer.lap("write lookup");
    Ok((stamp, written))
}

/// A graph this process wrote, with its root and the fingerprint written
/// with it. It lives in memory only.
type Rebuilt = (PathBuf, Fingerprint, Graph);

/// The roots with a live [`KeepRebuilt`] guard. A rebuild keeps its graph only
/// for a root in this list.
static KEEPING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static REBUILT: Mutex<Vec<Rebuilt>> = Mutex::new(Vec::new());

/// While it lives, a refresh rebuild in this process keeps the graph it
/// writes for its root, for [`KeepRebuilt::take_graph`]. A one-shot command
/// holds it from the refresh to the read. A daemon does not, so it keeps no
/// copy. Dropping the guard clears the kept graph.
pub struct KeepRebuilt {
    root: PathBuf,
}

/// Starts keeping the rebuilt graph for `root`. It drops any older graph kept
/// for that root.
pub fn keep_rebuilt_graph(root: &Path) -> KeepRebuilt {
    clear_rebuilt(root);
    if let Ok(mut keeping) = KEEPING.lock() {
        keeping.push(root.to_path_buf());
    }
    KeepRebuilt {
        root: root.to_path_buf(),
    }
}

/// Removes the graph kept for `root`.
fn clear_rebuilt(root: &Path) {
    if let Ok(mut kept) = REBUILT.lock() {
        kept.retain(|(kept_root, _, _)| kept_root != root);
    }
}

impl Drop for KeepRebuilt {
    fn drop(&mut self) {
        clear_rebuilt(&self.root);
        if let Ok(mut keeping) = KEEPING.lock() {
            if let Some(at) = keeping.iter().position(|root| *root == self.root) {
                keeping.swap_remove(at);
            }
        }
    }
}

impl KeepRebuilt {
    /// Takes the graph this process wrote in its last rebuild of this root.
    /// It gives `None` unless the fingerprint on disk is the one written with
    /// that graph, so the graph in memory is the build on disk. The caller
    /// decodes the file when it gets `None`.
    pub fn take_graph(&self, context_dir: &Path) -> Option<Graph> {
        let stamp = crate::extractor_stamp();
        let on_disk = read_fingerprint(&fingerprint_path(context_dir, &stamp), &stamp)?;
        let mut kept = REBUILT.lock().ok()?;
        let at = kept.iter().position(|(root, _, _)| *root == self.root)?;
        let (_, written, graph) = kept.swap_remove(at);
        (written == on_disk).then_some(graph)
    }
}

/// Rewrites the lookup from `wiring.json` alone when the lookup is missing
/// or its stamp differs from the wiring stamp (S3, E3). It parses no
/// source and builds no graph. A failure leaves the readers on their
/// fallback path, so it is never fatal.
fn regenerate_lookup(context_dir: &Path, wiring_path: &Path) {
    let Some(stamp) = wiring_stamp(wiring_path) else {
        return;
    };
    let path = lookup_path(context_dir);
    if Lookup::open(&path, stamp).is_some() {
        return;
    }
    // A held lock means a writer is at work. It writes its own lookup.
    let Ok(Some(_guard)) = LockGuard::acquire(context_dir) else {
        return;
    };
    // A writer may have finished while this call waited for the lock.
    let Some(stamp) = wiring_stamp(wiring_path) else {
        return;
    };
    if Lookup::open(&path, stamp).is_some() {
        return;
    }
    // The stamp is read before the wiring. A wiring replaced in between
    // leaves a stale stamp, which the next call repairs.
    let too_big = fs::metadata(wiring_path).map_or(true, |m| m.len() > DEFAULT_WIRING_CAP_BYTES);
    if too_big {
        return;
    }
    // ponytail: a bad wiring is re-read on every call until `sieve build`
    // repairs it. Add a retry marker if that read shows up in a profile.
    let graph = fs::read(wiring_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Graph>(&bytes).ok());
    if let Some(graph) = graph {
        let _ = lookup::write(&graph, &path, stamp);
    }
}

/// Rebuilds the graph and the fingerprint only: no cards, no `INDEX.md`,
/// no `.gitignore` edit.
pub fn rebuild_graph_only(root: &Path, context_dir: &Path) -> std::io::Result<RebuildReport> {
    rebuild_inner(root, context_dir, false, None).map(|(report, _)| report)
}

/// What a rebuild keeps for the rename plan: the graph, the raw edges and
/// the build prints.
pub(crate) struct Kept {
    pub graph: Graph,
    pub raw: Vec<crate::extract::RawEdge>,
    pub prints: std::collections::BTreeMap<String, Print>,
}

/// Rebuilds like [`rebuild_graph_only`] and keeps the build data. The caller
/// holds the build lock.
pub(crate) fn rebuild_keeping(root: &Path, context_dir: &Path) -> io::Result<Kept> {
    match rebuild_inner(root, context_dir, true, None)? {
        (_, Some(kept)) => Ok(kept),
        (_, None) => Err(io::Error::other("the rebuild kept no data")),
    }
}

fn rebuild_inner(
    root: &Path,
    context_dir: &Path,
    keep: bool,
    walked: Option<Vec<PathBuf>>,
) -> io::Result<(RebuildReport, Option<Kept>)> {
    // The last build's `--only-dir` whitelist rides in the fingerprint, so
    // a refresh walks the same set (P1-70).
    let stamp = crate::extractor_stamp();
    let path = fingerprint_path(context_dir, &stamp);
    let fp = read_fingerprint(&path, &stamp);
    // The warm guard runs on the fingerprint file count, before the old
    // cache is parsed (S0, B3).
    if let Some(fp) = &fp {
        crate::guard::check(fp.files.len(), true)?;
    }
    let only_dirs = fp.and_then(|fp| fp.only_dirs).unwrap_or_default();
    let mut opts = BuildOptions {
        only_dirs: only_dirs.clone(),
        no_reuse: false,
        read_only: false,
        progress: None,
        keep_raw: keep,
        walked,
    };
    let mut timer = crate::timing::Timer::start();
    // The racy rule of the next probe measures from here, before any stat.
    let walk_started_ms = sieve_core::fingerprint::walk_start_ms(context_dir);
    let report = build_graph_cached_with(root, context_dir, &mut opts)?;
    timer.lap("build (total)");
    // The ask-index builds while the wiring writes. The ask-index file
    // writes only after the wiring write succeeded, so the pair comes from
    // one build. A failed thread writes nothing and fails the refresh, so
    // the next query rebuilds. A failed ask-index write is not fatal.
    let (wiring, ask_index) = std::thread::scope(|scope| {
        let ask = scope.spawn(|| build_ask_index(&report.graph));
        let wiring = write_wiring_and_lookup_kept(&report.graph, context_dir);
        (wiring, ask.join())
    });
    let (_, written) = wiring?;
    let ask_index = ask_index.map_err(|_| io::Error::other("the ask-index thread failed"))?;
    timer.lap("wiring, lookup and ask-index done");
    let ask_written = write_ask_index(&ask_index_path(context_dir), &ask_index).is_ok();
    timer.lap("write ask-index");

    let fingerprint = Fingerprint {
        version: 1,
        extractor: stamp.clone(),
        files: report.prints.clone(),
        only_dirs: (!only_dirs.is_empty()).then_some(only_dirs),
        walked_at_ms: Some(walk_started_ms),
    };
    write_fingerprint(&path, &fingerprint)?;
    timer.lap("write fingerprint");
    // Keep the graph only when the whole write succeeded, with the record it
    // was written under.
    let keeping = KEEPING
        .lock()
        .is_ok_and(|keeping| keeping.iter().any(|r| r == root));
    if keeping && ask_written {
        if let Ok(mut slot) = REBUILT.lock() {
            slot.retain(|(kept_root, _, _)| kept_root != root);
            slot.push((root.to_path_buf(), fingerprint.clone(), written));
        }
    }

    let summary = RebuildReport {
        files: report.files,
        parsed: report.parsed,
        reused: report.reused,
    };
    let kept = keep.then_some(Kept {
        graph: report.graph,
        raw: report.raw,
        prints: report.prints,
    });
    Ok((summary, kept))
}

/// Runs the pre-query refresh: rebuilds the graph, and only the graph,
/// when the fingerprint is missing or stale. Never fails a query: every
/// error becomes a skip note in the returned [`RefreshOutcome`] instead.
pub fn ensure_fresh_graph(
    root: &Path,
    context_dir: &Path,
    opts: &RefreshOptions,
) -> RefreshOutcome {
    if opts.disabled {
        return RefreshOutcome::default();
    }

    // P1-50: no graph exists yet for this context dir. A query never builds
    // a whole repo, and it creates no folder. The user runs `build` first.
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    if !wiring_path.is_file() {
        return RefreshOutcome::default();
    }

    let stamp = crate::extractor_stamp();
    let fp_path = fingerprint_path(context_dir, &stamp);
    let fp = read_fingerprint(&fp_path, &stamp);

    let mut timer = crate::timing::Timer::start();
    regenerate_lookup(context_dir, &wiring_path);
    timer.lap("regenerate lookup");
    let fp_ms = fingerprint_mtime_ms(&fp_path);
    let mut walked: Option<Vec<PathBuf>> = None;
    let mut racy = sieve_core::fingerprint::RacyInfo::default();
    let first_count = match &fp {
        None => None,
        Some(fp) => {
            match probe_drift_racy(root, fp, opts.force_hash, fp_ms, crate::build::is_claimed) {
                Ok((drift, files, info)) => {
                    walked = Some(files);
                    racy = info;
                    Some(drift_count(&drift))
                }
                Err(err) => {
                    return RefreshOutcome {
                        refreshed: false,
                        changed_files: 0,
                        note: Some(format!(
                            "{} graph refresh skipped: {err}",
                            sieve_core::product().prefix()
                        )),
                    };
                }
            }
        }
    };
    timer.lap("first drift probe");
    if first_count == Some(0) {
        if let Some(fp) = fp.as_ref().filter(|_| racy.hashed > 0 && !racy.unverified) {
            clear_racy_under_lock(context_dir, &fp_path, &stamp, fp, racy.latest_mtime_ms);
            timer.lap("clear racy state");
        }
        return RefreshOutcome::default();
    }

    let guard = match acquire_refresh_lock(context_dir) {
        Ok(Some(guard)) => guard,
        Ok(None) => {
            return RefreshOutcome {
                refreshed: false,
                changed_files: 0,
                note: Some(format!(
                    "{} a graph rebuild is already in flight — answering from the current graph",
                    sieve_core::product().prefix()
                )),
            };
        }
        Err(err) => {
            return RefreshOutcome {
                refreshed: false,
                changed_files: 0,
                note: Some(format!(
                    "{} graph refresh skipped: {err}",
                    sieve_core::product().prefix()
                )),
            };
        }
    };

    // Re-probe under the lock: a concurrent rebuild may already have
    // cleaned the drift. Every rebuild writes the fingerprint last, so a
    // fingerprint that did not change means no rebuild ran meanwhile, and
    // the first probe still holds.
    let unchanged = fp.is_some() && read_fingerprint(&fp_path, &stamp) == fp;
    let count = match &fp {
        None => None,
        Some(_) if unchanged => first_count,
        Some(fp) => match probe_drift_walk(
            root,
            fp,
            opts.force_hash,
            fingerprint_mtime_ms(&fp_path),
            crate::build::is_claimed,
        ) {
            Ok((drift, files)) => {
                walked = Some(files);
                let n = drift_count(&drift);
                if n == 0 {
                    drop(guard);
                    return RefreshOutcome::default();
                }
                Some(n)
            }
            Err(err) => {
                drop(guard);
                return RefreshOutcome {
                    refreshed: false,
                    changed_files: 0,
                    note: Some(format!(
                        "{} graph refresh skipped: {err}",
                        sieve_core::product().prefix()
                    )),
                };
            }
        },
    };

    timer.lap("lock and second probe");
    let walked = walk_to_reuse(&guard, walked);
    let result = rebuild_inner(root, context_dir, false, walked).map(|(report, _)| report);
    timer.lap("rebuild (total)");
    drop(guard);
    match result {
        Ok(_) => RefreshOutcome {
            refreshed: true,
            changed_files: count.unwrap_or(0),
            note: None,
        },
        Err(err) => RefreshOutcome {
            refreshed: false,
            changed_files: 0,
            note: Some(format!(
                "{} graph refresh skipped: {err}",
                sieve_core::product().prefix()
            )),
        },
    }
}

/// The workspace refresh (P2-39): refreshes each child's own graph in
/// turn, under `<root>/<child>/<context dir name>`, never under the
/// parent's context dir. The outcome carries no drift count of its own;
/// the per-child sum rides in `note`, so [`refresh_note`] prints
/// `refreshed the graph (? files changed) before answering — refreshed
/// alpha (1 file changed) before answering`.
pub fn ensure_fresh_children(
    root: &Path,
    children: &[String],
    opts: &RefreshOptions,
) -> RefreshOutcome {
    if opts.disabled {
        return RefreshOutcome::default();
    }
    let name = sieve_core::product().context_dir_name();
    let mut refreshed_in: Vec<&str> = Vec::new();
    let mut files = 0;
    for child in children {
        let child_root = root.join(child);
        let outcome = ensure_fresh_graph(&child_root, &child_root.join(name), opts);
        if !outcome.refreshed {
            continue;
        }
        refreshed_in.push(child);
        files += outcome.changed_files;
    }
    if refreshed_in.is_empty() {
        return RefreshOutcome::default();
    }
    let shown = if files == 0 {
        "?".to_string()
    } else {
        files.to_string()
    };
    let word = if files == 1 { "file" } else { "files" };
    RefreshOutcome {
        refreshed: true,
        changed_files: 0,
        note: Some(format!(
            "refreshed {} ({shown} {word} changed) before answering",
            refreshed_in.join(", ")
        )),
    }
}

/// Formats the refresh note for [`RefreshOutcome::refreshed`]
/// (`query-prelude-grep.md` section 1.6): the built line, then ` — <note>` when
/// the outcome carries one. Returns `outcome.note` verbatim for every other
/// case.
pub fn refresh_note(outcome: &RefreshOutcome) -> Option<String> {
    if outcome.refreshed {
        let n = outcome.changed_files;
        let word = if n == 1 { "file" } else { "files" };
        let shown = if n == 0 {
            "?".to_string()
        } else {
            n.to_string()
        };
        let built = format!(
            "{} refreshed the graph ({shown} {word} changed) before answering",
            sieve_core::product().prefix()
        );
        return Some(match &outcome.note {
            Some(note) => format!("{built} — {note}"),
            None => built,
        });
    }
    outcome.note.clone()
}

/// Reports whether an env var is set to a value other than `""`, `"0"`, or
/// `"false"`. A caller builds [`RefreshOptions`] from this before it calls
/// [`ensure_fresh_graph`].
pub fn env_truthy(name: &str) -> bool {
    match env::var(name) {
        Ok(value) => !value.is_empty() && value != "0" && value != "false",
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: std::path::PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("sieve-refresh-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn write_repo_file(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dir");
        }
        fs::write(path, body).expect("write repo file");
    }

    fn wiring_path(context_dir: &Path) -> std::path::PathBuf {
        context_dir.join(".graph").join("wiring.json")
    }

    #[test]
    fn disabled_gives_no_note_and_no_rebuild() {
        let dir = TempDir::new("disabled");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");

        let outcome = ensure_fresh_graph(
            &dir.path,
            &context_dir,
            &RefreshOptions {
                disabled: true,
                force_hash: false,
            },
        );

        assert_eq!(outcome, RefreshOutcome::default());
        assert!(!wiring_path(&context_dir).is_file());
    }

    #[test]
    fn a_missing_fingerprint_rebuilds_and_writes_the_sidecar() {
        let dir = TempDir::new("missing-fp");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");

        // Build a graph first, so the wiring exists. Then drop the
        // fingerprint only, so the refresh sees a graph with no
        // fingerprint — not a repo with no graph at all (P1-50).
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let stamp = crate::extractor_stamp();
        fs::remove_file(fingerprint_path(&context_dir, &stamp)).expect("remove fingerprint");

        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());

        assert!(outcome.refreshed);
        assert_eq!(outcome.note, None);
        assert!(wiring_path(&context_dir).is_file());
        assert!(fingerprint_path(&context_dir, &stamp).is_file());
        assert!(ask_index_path(&context_dir).is_file());
    }

    fn read_wiring(context_dir: &Path) -> Graph {
        serde_json::from_slice(&fs::read(wiring_path(context_dir)).expect("read wiring"))
            .expect("decode wiring")
    }

    /// An edit, add or delete of a file the link reads is drift. The refresh
    /// rebuilds and the graph equals a full build of the same tree.
    #[test]
    fn a_link_input_edit_add_or_delete_rebuilds_to_the_full_graph() {
        let dir = TempDir::new("link-inputs");
        let root = &dir.path;
        write_repo_file(root, "a.ts", "export function a() {}\n");
        write_repo_file(root, "go.mod", "module example.com/a\n");
        write_repo_file(root, "package.json", "{\"name\":\"a\"}\n");
        write_repo_file(root, "app/App.csproj", "<Project></Project>\n");
        let context_dir = root.join("sieve");
        rebuild_graph_only(root, &context_dir).expect("build graph");

        let steps: [(&str, Option<&str>); 4] = [
            ("go.mod", Some("module example.com/renamed/longer\n")),
            ("package.json", Some("{\"name\":\"renamed-package\"}\n")),
            ("svc/pyproject.toml", Some("[project]\nname = \"svc\"\n")),
            ("app/App.csproj", None),
        ];
        for (n, (rel, body)) in steps.into_iter().enumerate() {
            match body {
                Some(body) => write_repo_file(root, rel, body),
                None => fs::remove_file(root.join(rel)).expect("delete file"),
            }
            let outcome = ensure_fresh_graph(root, &context_dir, &RefreshOptions::default());
            assert!(outcome.refreshed, "{rel} must cause a rebuild");
            let full_dir = root.join(format!("full{n}"));
            rebuild_graph_only(root, &full_dir).expect("full build");
            assert_eq!(read_wiring(&context_dir), read_wiring(&full_dir), "{rel}");
            fs::remove_dir_all(&full_dir).expect("remove full dir");
        }
    }

    #[test]
    fn a_clean_tree_gives_no_note_and_writes_nothing() {
        let dir = TempDir::new("clean");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");

        // Build the graph directly, so the refresh call under test finds
        // an already-fresh fingerprint, not a missing graph (P1-50).
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let wiring = wiring_path(&context_dir);
        let mtime_after_build = fs::metadata(&wiring)
            .expect("stat wiring")
            .modified()
            .unwrap();

        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());

        assert!(!outcome.refreshed);
        assert_eq!(outcome.note, None);
        let mtime_after_refresh = fs::metadata(&wiring)
            .expect("stat wiring")
            .modified()
            .unwrap();
        assert_eq!(mtime_after_build, mtime_after_refresh);
    }

    #[test]
    fn a_held_lock_gives_the_in_flight_note_after_about_two_seconds() {
        let dir = TempDir::new("held-lock");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");

        // Build a graph first, so the wiring exists, then drop the
        // fingerprint so this call needs a rebuild and takes the lock
        // (P1-50: a repo with no graph at all takes neither path).
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let stamp = crate::extractor_stamp();
        fs::remove_file(fingerprint_path(&context_dir, &stamp)).expect("remove fingerprint");

        let guard = LockGuard::acquire(&context_dir)
            .expect("acquire lock")
            .expect("lock free");

        let start = std::time::Instant::now();
        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        let elapsed = start.elapsed();

        drop(guard);

        assert!(!outcome.refreshed);
        assert_eq!(
            outcome.note,
            Some(
                "[sieve] a graph rebuild is already in flight — answering from the current graph"
                    .to_string()
            )
        );
        assert!(elapsed >= Duration::from_millis(sieve_core::lock::LOCK_WAIT_MS));
    }

    /// P2-39: the child refresh rebuilds only the drifted child, under
    /// that child's own context dir, and the note names the child with
    /// the per-child file sum behind the `?` count.
    #[test]
    fn test_p2_39_ensure_fresh_children_refreshes_each_child_in_turn() {
        let dir = TempDir::new("children");
        for child in ["alpha", "beta"] {
            write_repo_file(
                &dir.path,
                &format!("{child}/a.ts"),
                "export function a() {}\n",
            );
            rebuild_graph_only(&dir.path.join(child), &dir.path.join(child).join("sieve"))
                .expect("build child");
        }
        let children = vec!["alpha".to_string(), "beta".to_string()];
        let clean = ensure_fresh_children(&dir.path, &children, &RefreshOptions::default());
        assert_eq!(clean, RefreshOutcome::default());

        write_repo_file(&dir.path, "alpha/b.ts", "export function b() {}\n");
        let outcome = ensure_fresh_children(&dir.path, &children, &RefreshOptions::default());
        assert_eq!(
            outcome,
            RefreshOutcome {
                refreshed: true,
                changed_files: 0,
                note: Some("refreshed alpha (1 file changed) before answering".to_string()),
            }
        );
        assert_eq!(
            refresh_note(&outcome).as_deref(),
            Some(
                "[sieve] refreshed the graph (? files changed) before answering — refreshed alpha (1 file changed) before answering"
            )
        );
        assert!(!wiring_path(&dir.path.join("sieve")).is_file());

        let disabled = RefreshOptions {
            disabled: true,
            force_hash: false,
        };
        write_repo_file(&dir.path, "beta/b.ts", "export function b() {}\n");
        assert_eq!(
            ensure_fresh_children(&dir.path, &children, &disabled),
            RefreshOutcome::default()
        );
    }

    #[test]
    fn test_p1_50_refresh_skips_the_build_when_no_wiring_exists() {
        let dir = TempDir::new("no-wiring");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");

        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());

        assert_eq!(outcome, RefreshOutcome::default());
        assert!(!wiring_path(&context_dir).is_file());
        assert!(!context_dir.exists(), "a query makes no folder");
    }

    #[test]
    fn test_s3_both_sites_write_lookup() {
        let dir = TempDir::new("s3-sites");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");

        // The refresh site.
        let refresh_ctx = dir.path.join("sieve");
        rebuild_graph_only(&dir.path, &refresh_ctx).expect("build graph");
        let stamp = wiring_stamp(&wiring_path(&refresh_ctx)).expect("wiring stamp");
        assert!(Lookup::open(&lookup_path(&refresh_ctx), stamp).is_some());

        // The function itself, which the CLI build site calls in T3.
        let direct_ctx = dir.path.join("direct");
        let graph = crate::build::build_graph(&dir.path, &direct_ctx).expect("build graph");
        write_wiring_and_lookup(&graph, &direct_ctx).expect("write both");
        let stamp = wiring_stamp(&wiring_path(&direct_ctx)).expect("wiring stamp");
        assert!(Lookup::open(&lookup_path(&direct_ctx), stamp).is_some());
    }

    #[test]
    fn test_s3_stale_lookup_regenerates_from_wiring() {
        let dir = TempDir::new("s3-stale");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");

        // A new wiring stamp with no source drift, as after a `git pull`.
        let mut bytes = fs::read(wiring_path(&context_dir)).expect("read wiring");
        bytes.push(b'\n');
        fs::write(wiring_path(&context_dir), &bytes).expect("rewrite wiring");
        let stamp = wiring_stamp(&wiring_path(&context_dir)).expect("wiring stamp");
        assert!(Lookup::open(&lookup_path(&context_dir), stamp).is_none());
        let fp = fingerprint_path(&context_dir, &crate::extractor_stamp());
        let fp_before = fs::read(&fp).expect("read fingerprint");

        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());

        assert_eq!(outcome, RefreshOutcome::default());
        assert!(Lookup::open(&lookup_path(&context_dir), stamp).is_some());
        assert_eq!(
            fs::read(wiring_path(&context_dir)).expect("read wiring"),
            bytes
        );
        assert_eq!(fs::read(&fp).expect("read fingerprint"), fp_before);
    }

    #[test]
    fn test_s3_lookup_write_failure_keeps_wiring() {
        let dir = TempDir::new("s3-fail");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");
        let graph = crate::build::build_graph(&dir.path, &context_dir).expect("build graph");
        // A directory at the lookup path makes the rename fail.
        fs::create_dir_all(lookup_path(&context_dir)).expect("create blocker");

        write_wiring_and_lookup(&graph, &context_dir).expect("wiring write is not fatal");

        let stamp = wiring_stamp(&wiring_path(&context_dir)).expect("wiring stamp");
        assert!(Lookup::open(&lookup_path(&context_dir), stamp).is_none());
    }

    /// The graph a rebuild hands over equals the graph decoded from the
    /// `wiring.json` on disk, so a query answers the same with the hand-off.
    /// A second take gives nothing, and a drop of the guard clears the slot.
    #[test]
    fn the_rebuilt_graph_handed_over_equals_the_graph_on_disk() {
        let dir = TempDir::new("handoff");
        write_repo_file(&dir.path, "a.ts", "export function a() { return b(); }\n");
        write_repo_file(&dir.path, "b.ts", "export function b() { return 1; }\n");
        let context_dir = dir.path.join("sieve");
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let wiring = wiring_path(&context_dir);
        let keeper = keep_rebuilt_graph(&dir.path);
        write_repo_file(
            &dir.path,
            "a.ts",
            "export function a() { return b() + 2; }\n",
        );
        write_repo_file(&dir.path, "c.ts", "export function c() { return a(); }\n");

        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        assert!(outcome.refreshed);

        let on_disk: Graph =
            serde_json::from_slice(&fs::read(&wiring).expect("read wiring")).expect("decode");
        let handed = keeper
            .take_graph(&context_dir)
            .expect("a handed-over graph");
        assert_eq!(handed, on_disk);
        assert!(keeper.take_graph(&context_dir).is_none());

        // A drop clears a graph that nobody took.
        write_repo_file(&dir.path, "a.ts", "export function a() { return 7; }\n");
        ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        assert!(REBUILT
            .lock()
            .expect("lock")
            .iter()
            .any(|(r, _, _)| *r == dir.path));
        drop(keeper);
        assert!(!REBUILT
            .lock()
            .expect("lock")
            .iter()
            .any(|(r, _, _)| *r == dir.path));
        assert!(!KEEPING.lock().expect("lock").contains(&dir.path));

        // With no guard live for this root, a rebuild keeps nothing.
        write_repo_file(&dir.path, "a.ts", "export function a() { return 8; }\n");
        ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        assert!(!REBUILT
            .lock()
            .expect("lock")
            .iter()
            .any(|(r, _, _)| *r == dir.path));
    }

    /// A process that did not rebuild has no graph to hand over.
    #[test]
    fn no_rebuild_gives_no_handed_over_graph() {
        let dir = TempDir::new("handoff-none");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let keeper = keep_rebuilt_graph(&dir.path);
        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        assert!(!outcome.refreshed);
        assert!(keeper.take_graph(&context_dir).is_none());
    }

    /// Another process that rewrites the fingerprint after this rebuild leaves
    /// the kept graph without a match, so the caller decodes the file.
    #[test]
    fn a_fingerprint_written_by_another_build_gives_no_handed_over_graph() {
        let dir = TempDir::new("handoff-other");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let keeper = keep_rebuilt_graph(&dir.path);
        write_repo_file(&dir.path, "a.ts", "export function a() { return 1; }\n");
        ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        // Another process writes the record again, with a new walk start.
        let stamp = crate::extractor_stamp();
        let fp_path = fingerprint_path(&context_dir, &stamp);
        let mut other = read_fingerprint(&fp_path, &stamp).expect("fingerprint");
        other.walked_at_ms = other.walked_at_ms.map(|ms| ms + 1.0);
        write_fingerprint(&fp_path, &other).expect("write fingerprint");
        assert!(keeper.take_graph(&context_dir).is_none());
    }

    /// A keep for one root holds nothing for another root.
    #[test]
    fn a_kept_graph_belongs_to_its_root() {
        let dir = TempDir::new("handoff-root");
        write_repo_file(&dir.path, "a.ts", "export function a() {}\n");
        let context_dir = dir.path.join("sieve");
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let keeper = keep_rebuilt_graph(&dir.path);
        write_repo_file(&dir.path, "a.ts", "export function a() { return 1; }\n");
        ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        let other = keep_rebuilt_graph(&dir.path.join("elsewhere"));
        assert!(other.take_graph(&context_dir).is_none());
        assert!(keeper.take_graph(&context_dir).is_some());
    }

    /// After a wait for the lock, the rebuild walks again.
    #[test]
    fn a_waited_lock_drops_the_first_probe_walk_list() {
        let list = Some(vec![PathBuf::from("a.ts")]);
        let waited = RefreshLock::Waited(PathBuf::from("/nowhere"));
        // `Waited` releases a lock on drop; release of a missing lock is harmless.
        assert_eq!(walk_to_reuse(&waited, list.clone()), None);
        let dir = TempDir::new("immediate-lock");
        let held = LockGuard::acquire(&dir.path.join("sieve"))
            .expect("acquire")
            .expect("free");
        let immediate = RefreshLock::Immediate(held);
        assert_eq!(walk_to_reuse(&immediate, list.clone()), list);
    }

    /// After a fresh checkout every file is racy. A probe that hashes them and
    /// finds no drift, once the files are 2 s old, clears the state. The next
    /// probe then hashes none.
    #[test]
    fn a_clean_probe_clears_the_racy_state_so_the_next_probe_hashes_nothing() {
        let dir = TempDir::new("clear-racy");
        for name in ["a", "b", "c"] {
            write_repo_file(
                &dir.path,
                &format!("{name}.ts"),
                &format!("export const {name} = 1;\n"),
            );
        }
        let context_dir = dir.path.join("sieve");
        rebuild_graph_only(&dir.path, &context_dir).expect("build graph");
        let stamp = crate::extractor_stamp();
        let fp_path = fingerprint_path(&context_dir, &stamp);
        let probe = || {
            let fp = read_fingerprint(&fp_path, &stamp).expect("fingerprint");
            let fp_ms = fingerprint_mtime_ms(&fp_path);
            let (drift, _, info) =
                probe_drift_racy(&dir.path, &fp, false, fp_ms, crate::build::is_claimed)
                    .expect("probe");
            assert_eq!(drift, Drift::default());
            info
        };

        // Fresh files: all racy, and too new to clear.
        assert_eq!(probe().hashed, 3);
        ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        assert_eq!(probe().hashed, 3);

        // 2 s later the query hashes them once and clears the state.
        std::thread::sleep(Duration::from_millis(2300));
        assert_eq!(probe().hashed, 3);
        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        assert!(!outcome.refreshed);
        assert_eq!(probe().hashed, 0);

        // A later edit is still found.
        write_repo_file(&dir.path, "a.ts", "export const a = 2;\n");
        let outcome = ensure_fresh_graph(&dir.path, &context_dir, &RefreshOptions::default());
        assert!(outcome.refreshed);
    }
}
