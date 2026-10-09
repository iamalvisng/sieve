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

use sieve_core::askindex::{ask_index_path, build_ask_index, write_ask_index};
use sieve_core::fingerprint::{
    fingerprint_path, probe_drift, read_fingerprint, write_fingerprint, Drift, Fingerprint,
};
use sieve_core::lock::{self, LockGuard};
use sieve_core::lookup::{self, wiring_stamp, Lookup, DEFAULT_WIRING_CAP_BYTES};
use sieve_core::wiring::Graph;
use sieve_core::write_graph_stamped;

use crate::build::{build_graph_cached_with, BuildOptions};

/// A held build lock that releases itself when dropped.
///
/// This wraps either the real [`LockGuard`], taken on the first try, or a
/// plain context dir path, taken after a wait: [`lock::wait_for_lock`]
/// reports only success or failure, with no guard of its own, so the
/// waited case releases through [`lock::release`] by hand instead.
enum RefreshLock {
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

/// Takes the build lock, waiting once when another process holds it.
///
/// Returns `None` only when the wait in [`lock::wait_for_lock`] expires
/// with the lock still held elsewhere.
fn acquire_refresh_lock(context_dir: &Path) -> std::io::Result<Option<RefreshLock>> {
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
    let wiring_path = context_dir.join(".graph").join("wiring.json");
    let stamp = write_graph_stamped(graph, &wiring_path)?;
    let path = lookup_path(context_dir);
    if lookup::write(graph, &path, stamp).is_err() {
        let _ = fs::remove_file(&path);
    }
    Ok(())
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
    let opts = BuildOptions {
        only_dirs: only_dirs.clone(),
        no_reuse: false,
        read_only: false,
        progress: None,
    };
    let report = build_graph_cached_with(root, context_dir, &opts)?;
    write_wiring_and_lookup(&report.graph, context_dir)?;

    // A sidecar write failure is never fatal: the refresh has no build
    // report to print it on.
    let ask_index = build_ask_index(&report.graph);
    let ask_path = ask_index_path(context_dir);
    let _ = write_ask_index(&ask_path, &ask_index);

    let fingerprint = Fingerprint {
        version: 1,
        extractor: stamp.clone(),
        files: report.prints,
        only_dirs: (!only_dirs.is_empty()).then_some(only_dirs),
    };
    write_fingerprint(&path, &fingerprint)?;

    Ok(RebuildReport {
        files: report.files,
        parsed: report.parsed,
        reused: report.reused,
    })
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

    regenerate_lookup(context_dir, &wiring_path);
    let needs_rebuild = match &fp {
        None => true,
        Some(fp) => match probe_drift(root, fp, opts.force_hash, crate::build::is_claimed) {
            Ok(drift) => drift_count(&drift) > 0,
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
        },
    };
    if !needs_rebuild {
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
    // cleaned the drift.
    let count = match &fp {
        None => None,
        Some(fp) => match probe_drift(root, fp, opts.force_hash, crate::build::is_claimed) {
            Ok(drift) => {
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

    let result = rebuild_graph_only(root, context_dir);
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
}
