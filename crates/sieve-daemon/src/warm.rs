//! The warm index of one MCP process (`docs/design/daemon-speed.md`, steps 3a
//! and 3a v2).
//!
//! The process keeps the graph, the fingerprint record, every file's nodes and
//! raw edges, and a savings word index in memory. A `notify` watcher feeds a
//! dirty set. Before each call, a cookie file proves that the dirty set holds
//! every write that ended before the call. A dirty source file swaps its own
//! parse and links the whole set in memory (V2). Every case the watcher cannot
//! vouch for runs the step 1 refresh (`ensure_fresh_graph`) instead. The disk
//! writes of a patch run after the reply. The MCP loop is the one worker
//! thread, so calls run in series; the registry lock keeps that true for any
//! caller.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use sieve_core::askindex::{ask_index_path, build_ask_index, write_ask_index};
use sieve_core::fingerprint::{
    decode_source, fingerprint_path, hash_source, read_fingerprint, walk_start_ms,
    write_fingerprint, Fingerprint, Print,
};
use sieve_core::lock::{self, LockGuard};
use sieve_core::walk::{is_skipped_name, read_include_dirs, SKIP_DIRS};
use sieve_core::wiring::Graph;
use sieve_core::wiring::Node;
use sieve_parse::cache::{cache_path, read_cache, write_cache, ExtractCache};
use sieve_parse::refresh::{
    ensure_fresh_graph, env_truthy, keep_rebuilt_graph, KeepRebuilt, RefreshOptions, RefreshOutcome,
};
use sieve_parse::{
    claims, extractor_stamp, fresh_entry, link_entries_moving, walk_files, Extractors, ScopeMemo,
};
use sieve_query::callers::{word_grep_chars_indexed, GraphIndex, WordIndex};

#[cfg(target_os = "macos")]
mod kq;
/// Linux keeps inotify and the cookie, so this stand-in never opens a watch.
#[cfg(not(target_os = "macos"))]
mod kq {
    use super::Filter;
    use std::path::PathBuf;

    #[allow(dead_code)] // The variants are built only by the macOS watch.
    pub(super) enum Out {
        Source(PathBuf),
        Config(PathBuf),
        Broken,
    }

    pub(super) struct Kq;

    impl Kq {
        pub(super) fn open(_: &Filter, _: u64) -> Option<Kq> {
            None
        }

        pub(super) fn drain(&mut self, _: &mut Vec<Out>) {}
    }

    pub(super) fn soft_limit() -> u64 {
        0
    }
}
use kq::{Kq, Out};

/// How long a call waits for its cookie event before it runs the probe.
const COOKIE_WAIT: Duration = Duration::from_millis(200);
/// The git racy window: a source event this recent turns the shortcut off.
const RACY_WINDOW: Duration = Duration::from_secs(2);
/// How long the post-reply step waits for the background checks.
const READY_WAIT: Duration = Duration::from_millis(500);

/// What the watcher callback sends to the worker.
enum Msg {
    /// A source path changed, with the time the callback saw it.
    Source(Instant, PathBuf),
    /// A cookie file event.
    Cookie(PathBuf),
    /// An overflow, an error, or a change to the config: restart and probe.
    Broken,
}

/// How the filter sorts one event path.
#[derive(Debug, PartialEq, Eq)]
enum Class {
    Ignore,
    Cookie,
    Source,
    /// A path in the context dir outside `.cache` and `.graph`. The config
    /// lives there, and it changes the walk rules.
    Config,
}

/// The pure path rules of "Event filters". Cloned into the callback.
#[derive(Clone)]
struct Filter {
    root: PathBuf,
    ctx: PathBuf,
    includes: Vec<String>,
    excludes: Option<PathBuf>,
}

impl Filter {
    /// Sorts one absolute event path. An unmatched path is `Source`: the
    /// filter never drops a path it cannot name.
    fn classify(&self, path: &Path) -> Class {
        if self.excludes.as_deref() == Some(path) {
            return Class::Source;
        }
        if let Ok(inner) = path.strip_prefix(&self.ctx) {
            return match inner
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
            {
                Some(".cache") if inner.starts_with(".cache/cookies") => Class::Cookie,
                Some(".cache" | ".graph") => Class::Ignore,
                _ => Class::Config,
            };
        }
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return Class::Source;
        };
        let names: Vec<_> = rel.components().map(|c| c.as_os_str()).collect();
        if names.first().is_some_and(|n| *n == ".git") {
            let kept = match names.get(1).and_then(|n| n.to_str()) {
                Some("index" | "HEAD" | "packed-refs" | "refs") => true,
                Some("info") => names.get(2).is_some_and(|n| *n == "exclude"),
                _ => false,
            };
            return if kept { Class::Source } else { Class::Ignore };
        }
        let Some((last, dirs)) = names.split_last() else {
            return Class::Source;
        };
        if dirs.iter().any(|n| is_skipped_name(n, &self.includes)) {
            return Class::Ignore;
        }
        // The last part may be a dot-file such as `.gitignore`, which moves
        // the walk. Only a skipped dir name (not a dot name) drops here.
        let last = last.to_string_lossy();
        if SKIP_DIRS.contains(&last.as_ref()) && !self.includes.iter().any(|n| *n == last) {
            return Class::Ignore;
        }
        Class::Source
    }
}

/// Counters for tests and for `SIEVE_TIMING`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stats {
    /// Calls that ran the step 1 refresh.
    pub probes: usize,
    /// Probes that ran because the cookie event took over 200 ms.
    pub fallbacks: usize,
    /// Calls answered from memory with no probe.
    pub memory: usize,
    /// Calls answered from a one-file patch (V2).
    pub patches: usize,
    /// Times the graph came from disk (a decode).
    pub decodes: usize,
    /// Times the watcher restarted.
    pub restarts: usize,
    /// Source events the worker took in.
    pub sources: usize,
}

/// What the background checks of V4 found.
struct Ready {
    filter: Filter,
    watcher: Option<RecommendedWatcher>,
    /// The kqueue watch of macOS, in place of the watcher.
    kq: Option<Kq>,
    /// False when a "When the shortcut is off" case holds for this root.
    shortcut_ok: bool,
}

/// Every file's nodes and raw edges, in memory (V2).
struct Parts {
    /// The walk list, in walk order.
    files: Vec<PathBuf>,
    /// The extraction cache entry of every file.
    cache: ExtractCache,
    /// True when an event the patch did not read may have moved the walk.
    walk_stale: bool,
    /// The scope markers of each dir, so the link lists no dir on disk.
    scopes: ScopeMemo,
}

/// The disk writes a patch owes, done after the reply.
struct Pending {
    /// The record the patch started from.
    base: Fingerprint,
    /// The record the patch wrote to memory.
    record: Fingerprint,
    /// The graph as a reader decodes it: sorted, with no body text.
    graph: Arc<Graph>,
    /// The body text of each node of `graph`, in written order.
    bodies: Vec<Option<String>>,
    /// The written index of each node, by its place in link order.
    pos: Vec<u32>,
    /// The files in link order with their node counts, when the patch moved
    /// their nodes out of the cache entries.
    moved: Option<Vec<(String, usize, usize)>>,
}

/// A patch's disk writes with their own snapshot of the extraction cache, so
/// the write needs nothing the next patch changes.
struct Job {
    pending: Pending,
    /// The cache as the patch left it: the moved files hold no nodes yet.
    cache: ExtractCache,
}

/// What the writer thread and the call path share.
#[derive(Default)]
struct WriterState {
    /// The newest job that waits. A newer job replaces it.
    slot: Option<Job>,
    /// True while the writer thread lives.
    running: bool,
    /// True when a write failed or met a moved fingerprint.
    failed: bool,
    /// The stat id of the fingerprint file the last good write left.
    stat: Option<(u64, SystemTime)>,
}

/// The one writer thread of a warm pair. It runs the disk writes off the
/// call path, one job at a time and in order.
#[derive(Default)]
struct Writer {
    state: Mutex<WriterState>,
    idle: Condvar,
}

impl Writer {
    fn lock(&self) -> std::sync::MutexGuard<'_, WriterState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// True while a job waits or runs.
    fn busy(&self) -> bool {
        let st = self.lock();
        st.running || st.slot.is_some()
    }

    /// Queues a job. A job that waits is replaced: the new job starts from
    /// the record on disk, which is the base of the job it replaces. So a
    /// write never lands an older state after a newer one.
    fn submit(self: &Arc<Self>, context_dir: &Path, mut job: Job) {
        let mut st = self.lock();
        if let Some(old) = st.slot.take() {
            job.pending.base = old.pending.base;
        }
        st.slot = Some(job);
        self.start(st, context_dir);
    }

    /// Starts the thread, unless one runs.
    fn start(self: &Arc<Self>, mut st: std::sync::MutexGuard<'_, WriterState>, context_dir: &Path) {
        if st.running {
            return;
        }
        st.running = true;
        drop(st);
        let (me, ctx) = (Arc::clone(self), context_dir.to_path_buf());
        if std::thread::Builder::new()
            .name("sieve-warm-writer".into())
            .spawn(move || me.run(&ctx))
            .is_err()
        {
            // No thread: write on the caller, as the older code did.
            self.run(context_dir);
        }
    }

    /// Writes jobs until the slot is empty.
    fn run(&self, ctx: &Path) {
        let mut exit = ExitGuard {
            writer: self,
            clean: false,
        };
        loop {
            let mut st = self.lock();
            let Some(job) = st.slot.take() else {
                // The same critical section sees the empty slot and ends the
                // thread. A second lock would let `submit` queue a job for a
                // thread that is about to exit.
                st.running = false;
                self.idle.notify_all();
                exit.clean = true;
                return;
            };
            drop(st);
            let stat = write_job(ctx, job);
            let mut st = self.lock();
            match stat {
                Some(stat) => st.stat = Some(stat),
                None => st.failed = true,
            }
        }
    }

    /// Waits until no job waits or runs.
    fn wait_idle(&self) {
        let mut st = self.lock();
        while st.running || st.slot.is_some() {
            st = self.idle.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
    }
}

/// Ends the writer thread in every case, a panic too. Else `running` stays
/// true and every wait for the writer blocks for ever.
struct ExitGuard<'a> {
    writer: &'a Writer,
    clean: bool,
}

impl Drop for ExitGuard<'_> {
    fn drop(&mut self) {
        if self.clean {
            // `run` ended the thread inside its own critical section.
            return;
        }
        let mut st = self.writer.lock();
        // A queued job would start from a record this thread never wrote.
        st.slot = None;
        st.failed = true;
        st.running = false;
        self.writer.idle.notify_all();
    }
}

/// Copies the nodes of the graph back into the moved cache entries. The
/// cache holds them as extraction made them: no meaning layer, and with the
/// body text.
fn copy_back(cache: &mut ExtractCache, p: &Pending) {
    let Some(files) = &p.moved else {
        return;
    };
    let mut at = 0;
    for (id, n, _) in files {
        let Some(entry) = cache.files.get_mut(id) else {
            at += n;
            continue;
        };
        entry.nodes = (at..at + n)
            .map(|k| {
                let w = p.pos[k] as usize;
                let mut node: Node = p.graph.nodes[w].clone();
                node.body_text = p.bodies[w].clone();
                node.summary = None;
                node.crux = None;
                node.summary_state = sieve_core::wiring::SummaryState::Pending;
                node
            })
            .collect();
        at += n;
    }
}

/// What a test asks of the next write for one context dir: `true` panics,
/// `false` pauses.
#[cfg(test)]
static HOOK: Mutex<Option<(PathBuf, bool)>> = Mutex::new(None);

/// Runs the test hook for `ctx`, once.
#[cfg(test)]
fn test_hook(ctx: &Path) {
    let mut hook = HOOK.lock().unwrap_or_else(PoisonError::into_inner);
    if hook.as_ref().is_some_and(|(dir, _)| dir == ctx) {
        let panic = hook.take().is_some_and(|(_, panic)| panic);
        drop(hook);
        if panic {
            panic!("forced writer panic");
        }
        std::thread::sleep(Duration::from_millis(400));
    }
}

/// Writes one job to disk under the build lock (V2 step 5). The cache goes
/// first and the fingerprint last, as a rebuild writes them. A moved
/// fingerprint or a failed write gives `None`.
fn write_job(ctx: &Path, job: Job) -> Option<(u64, SystemTime)> {
    #[cfg(test)]
    test_hook(ctx);
    let Job {
        pending: p,
        mut cache,
    } = job;
    let stamp = extractor_stamp();
    let fp_path = fingerprint_path(ctx, &stamp);
    let _held = take_lock(ctx)?;
    if read_fingerprint(&fp_path, &stamp).as_ref() != Some(&p.base) {
        return None;
    }
    copy_back(&mut cache, &p);
    let cache_ok = write_cache(&cache_path(ctx, &stamp), &cache).is_ok();
    // The ask-index reads the body text, so it builds from the graph with
    // the bodies put back, as a rebuild builds it.
    let mut full: Graph = (*p.graph).clone();
    for (node, body) in full.nodes.iter_mut().zip(p.bodies) {
        node.body_text = body;
    }
    let graph: &Graph = &full;
    let (wiring, ask) = std::thread::scope(|scope| {
        let ask = scope.spawn(|| build_ask_index(graph));
        let wiring = sieve_parse::write_wiring_and_lookup(graph, ctx);
        (wiring, ask.join())
    });
    let ask_ok = ask.is_ok_and(|index| write_ask_index(&ask_index_path(ctx), &index).is_ok());
    let ok = cache_ok && wiring.is_ok() && ask_ok && write_fingerprint(&fp_path, &p.record).is_ok();
    if ok {
        stat_id(&fp_path)
    } else {
        None
    }
}

/// What a dirty set asks of the call.
enum Plan {
    /// Every dirty file matches its record.
    Clean,
    /// Run the step 1 probe.
    Probe,
    /// Swap these files; `walk` adds a fresh walk for a new or deleted file.
    Patch {
        ids: Vec<String>,
        walk: bool,
        /// Link again even when no entry changed.
        relink: bool,
    },
}

/// The warm state of one (root, context dir) pair.
pub(crate) struct Warm {
    root: PathBuf,
    context_dir: PathBuf,
    filter: Filter,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    watcher: Option<RecommendedWatcher>,
    /// The kqueue watch. When it runs, the drain replaces the cookie.
    kq: Option<Kq>,
    /// The soft fd limit; a watch may use half of it.
    kq_limit: u64,
    /// The sender the checks thread answers on, until the thread starts.
    checks_tx: Option<Sender<Ready>>,
    /// The background checks of V4. `None` once they arrive.
    ready_rx: Option<Receiver<Ready>>,
    /// True once the checks arrived and the watcher runs.
    ready: bool,
    /// True when the checks arrived and one probe must still run.
    fresh_start: bool,
    /// False when a "When the shortcut is off" case holds for this root.
    shortcut_ok: bool,
    dirty: BTreeSet<PathBuf>,
    broken: bool,
    /// The last source event on a path a patch cannot read (a dir, a manifest).
    last_unindexed: Option<Instant>,
    record: Option<Fingerprint>,
    fp_stat: Option<(u64, SystemTime)>,
    graph: Option<Arc<Graph>>,
    parts: Option<Parts>,
    /// True when the parts did not load. Then the reply stops trying until
    /// the graph reloads from disk, which a probe that writes causes.
    parts_failed: bool,
    /// The parsers a patch reuses, so their queries compile once.
    parsers: Option<Extractors>,
    words: Option<WordIndex>,
    /// The query lookups of the graph they were built for.
    qindex: Option<(Arc<Graph>, Arc<GraphIndex>)>,
    /// A patch's writes, not yet handed to the writer thread.
    pending: Option<Pending>,
    writer: Arc<Writer>,
    /// The graph a patch replaced. Its drop waits for the post-reply step.
    retired: Option<Arc<Graph>>,
    opts: RefreshOptions,
    keeper: KeepRebuilt,
    cookie_n: u64,
    stats: Stats,
    timing: bool,
}

/// Starts the watcher on the canonical root, and on the excludes file.
fn start_watcher(filter: &Filter, tx: &Sender<Msg>) -> Option<RecommendedWatcher> {
    let f = filter.clone();
    let tx = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(event) = res else {
            let _ = tx.send(Msg::Broken);
            return;
        };
        // inotify reports every read as an Access event. Our own probe reads
        // files, so keeping these would feed the watcher back to itself.
        if matches!(event.kind, EventKind::Access(_)) {
            return;
        }
        if event.need_rescan() {
            let _ = tx.send(Msg::Broken);
        }
        for path in event.paths {
            match f.classify(&path) {
                Class::Ignore => {}
                Class::Cookie => {
                    let _ = tx.send(Msg::Cookie(path));
                }
                Class::Config => {
                    let _ = tx.send(Msg::Broken);
                    let _ = tx.send(Msg::Source(Instant::now(), path));
                }
                Class::Source => {
                    let _ = tx.send(Msg::Source(Instant::now(), path));
                }
            }
        }
    })
    .ok()?;
    // One batch: a second `watch` call stops and restarts the FSEvents stream
    // and can spin on the run loop of the first.
    let mut paths = watcher.paths_mut();
    paths.add(&filter.root, RecursiveMode::Recursive).ok()?;
    if let Some(excludes) = &filter.excludes {
        let _ = paths.add(excludes, RecursiveMode::NonRecursive);
    }
    paths.commit().ok()?;
    Some(watcher)
}

/// Reports whether `mount` output puts `root` on a network or fuse mount.
/// It reads the macOS form `src on /mp (smbfs, ...)` and the Linux form
/// `src on /mp type nfs4 (...)`. The longest mount point above `root` wins.
fn remote_mount_in(mounts: &str, root: &Path) -> bool {
    let mut best: Option<(usize, &str)> = None;
    for line in mounts.lines() {
        let Some((_, rest)) = line.split_once(" on ") else {
            continue;
        };
        let (point, fstype) = if let Some((point, tail)) = rest.split_once(" type ") {
            (point, tail.split_whitespace().next().unwrap_or(""))
        } else if let Some((point, tail)) = rest.split_once(" (") {
            let first = tail.split([',', ')']).next().unwrap_or("");
            (point, first.trim())
        } else {
            continue;
        };
        if root.starts_with(point) && best.is_none_or(|(len, _)| point.len() >= len) {
            best = Some((point.len(), fstype));
        }
    }
    best.is_some_and(|(_, t)| {
        t.starts_with("nfs")
            || t.starts_with("fuse")
            || t.ends_with("fuse")
            || ["smbfs", "cifs", "afpfs", "webdav"].contains(&t)
    })
}

/// Reports whether two paths sit on one device.
#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_device(_: &Path, _: &Path) -> bool {
    false
}

/// The size and mtime of a file, to spot a rewritten fingerprint cheaply.
fn stat_id(path: &Path) -> Option<(u64, SystemTime)> {
    let m = fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// Reads the `core.excludesFile` path from the output of `git config`, or
/// git's default path. A file that does not exist gives `None`: a watch needs
/// an existing path, so a new excludes file is not seen (the user doc says so).
fn excludes_path(configured: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let path = match configured.trim() {
        "" => match std::env::var_os("XDG_CONFIG_HOME") {
            Some(x) if !x.is_empty() => PathBuf::from(x).join("git/ignore"),
            _ => home?.join(".config/git/ignore"),
        },
        p => match p.strip_prefix("~/") {
            Some(tail) => home?.join(tail),
            None => PathBuf::from(p),
        },
    };
    fs::canonicalize(path).ok()
}

/// Runs the V4 checks: `mount`, `git config`, the canonical paths and the
/// watcher start. A thread runs it, so the first call does not wait.
fn check_and_watch(root: &Path, context_dir: &Path, tx: &Sender<Msg>) -> Ready {
    // Start both helper processes first, so they run beside the watcher.
    let mount = Command::new("mount")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    let git = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["config", "--get", "core.excludesFile"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok();
    // FSEvents reports real paths, so the watcher gets the canonical root.
    let canon_root = fs::canonicalize(root);
    let canon_ctx = fs::canonicalize(context_dir);
    let (root_c, ctx_c) = match (&canon_root, &canon_ctx) {
        (Ok(r), Ok(c)) => (r.clone(), c.clone()),
        _ => (root.to_path_buf(), context_dir.to_path_buf()),
    };
    let configured = git
        .and_then(|c| c.wait_with_output().ok())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let filter = Filter {
        includes: read_include_dirs(root),
        excludes: excludes_path(&configured),
        root: root_c,
        ctx: ctx_c,
    };
    let mounts = mount
        .and_then(|c| c.wait_with_output().ok())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
    // A failed `mount` is unknown, and unknown turns the shortcut off.
    let local = mounts.is_some_and(|m| !remote_mount_in(&m, &filter.root));
    let inside = canon_root.is_ok()
        && filter.ctx.starts_with(&filter.root)
        && same_device(&filter.root, &filter.ctx);
    let ok = local && inside;
    // The cookie dir must exist before the watch starts. On Linux, inotify
    // adds a watch for a new dir only after it sees the create, so a cookie
    // file written at once into a new dir is missed and the first call falls
    // back to the probe. The dir stays; `cookie_sync` removes only its file.
    let cookies = filter.ctx.join(".cache").join("cookies");
    let _ = fs::remove_dir_all(&cookies);
    let _ = fs::create_dir_all(&cookies);
    let kq = if ok {
        Kq::open(&filter, kq::soft_limit())
    } else {
        None
    };
    // A repo over the fd budget keeps FSEvents and the cookie.
    let watcher = if kq.is_some() {
        None
    } else {
        start_watcher(&filter, tx)
    };
    Ready {
        filter,
        watcher,
        kq,
        shortcut_ok: ok,
    }
}

/// Holds the build lock for the post-reply writes.
enum Held {
    Guard(#[allow(dead_code)] LockGuard),
    Waited(PathBuf),
}

impl Drop for Held {
    fn drop(&mut self) {
        if let Held::Waited(dir) = self {
            lock::release(dir);
        }
    }
}

/// Takes the build lock, waiting once for another process.
fn take_lock(context_dir: &Path) -> Option<Held> {
    match LockGuard::acquire(context_dir) {
        Ok(Some(guard)) => Some(Held::Guard(guard)),
        Ok(None) => match lock::wait_for_lock(context_dir) {
            Ok(true) => Some(Held::Waited(context_dir.to_path_buf())),
            _ => None,
        },
        Err(_) => None,
    }
}

/// The repo-relative id of an absolute event path, if it is under `root`.
fn id_of(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?.to_str()?;
    Some(rel.replace('\\', "/"))
}

impl Warm {
    /// Makes the warm state. The background checks and the watcher start after
    /// the first reply (`start_checks`), so the first call, which runs the
    /// step 1 path, does not share the machine with two child processes.
    fn new(root: &Path, context_dir: &Path) -> Warm {
        let (tx, rx) = channel();
        let (ready_tx, ready_rx) = channel();
        Warm {
            checks_tx: Some(ready_tx),
            root: root.to_path_buf(),
            context_dir: context_dir.to_path_buf(),
            filter: Filter {
                root: root.to_path_buf(),
                ctx: context_dir.to_path_buf(),
                includes: Vec::new(),
                excludes: None,
            },
            tx,
            rx,
            watcher: None,
            kq: None,
            kq_limit: kq::soft_limit(),
            ready_rx: Some(ready_rx),
            ready: false,
            fresh_start: false,
            shortcut_ok: false,
            dirty: BTreeSet::new(),
            broken: false,
            last_unindexed: None,
            record: None,
            fp_stat: None,
            graph: None,
            parts: None,
            parts_failed: false,
            parsers: None,
            words: None,
            qindex: None,
            pending: None,
            writer: Arc::new(Writer::default()),
            retired: None,
            opts: RefreshOptions::default(),
            keeper: keep_rebuilt_graph(root),
            cookie_n: 0,
            stats: Stats::default(),
            timing: env_truthy(&sieve_core::product().env_var("TIMING")),
        }
    }

    /// Starts the background checks and the watcher, once.
    fn start_checks(&mut self) {
        let Some(ready_tx) = self.checks_tx.take() else {
            return;
        };
        let (root, ctx, tx) = (self.root.clone(), self.context_dir.clone(), self.tx.clone());
        std::thread::spawn(move || {
            let _ = ready_tx.send(check_and_watch(&root, &ctx, &tx));
        });
    }

    /// Takes the background checks, if they arrived. Events from before the
    /// watcher started are unseen, so one probe must follow.
    fn poll_ready(&mut self, _wait: Duration) {
        let Some(rx) = self.ready_rx.take() else {
            return;
        };
        let got = rx.try_recv().map_err(|e| match e {
            TryRecvError::Empty => RecvTimeoutError::Timeout,
            TryRecvError::Disconnected => RecvTimeoutError::Disconnected,
        });
        self.accept_ready(rx, got);
    }

    /// Applies the answer of the checks thread: use it, or keep waiting.
    fn accept_ready(&mut self, rx: Receiver<Ready>, got: Result<Ready, RecvTimeoutError>) {
        match got {
            Ok(r) => {
                self.filter = r.filter;
                self.watcher = r.watcher;
                self.kq = r.kq;
                self.shortcut_ok = r.shortcut_ok;
                self.ready = true;
                self.fresh_start = true;
            }
            // The checks are still running: ask again on a later call.
            Err(RecvTimeoutError::Timeout) => self.ready_rx = Some(rx),
            // The thread died: the shortcut stays off.
            Err(RecvTimeoutError::Disconnected) => {}
        }
    }

    /// True for an event path that cannot hide a lost event: a source file a
    /// patch reads, or a path that is gone (a temp file of a rename). A dir
    /// or any other file may stand for a burst that inotify missed.
    fn readable_path(&self, path: &Path) -> bool {
        let Some(id) = id_of(&self.filter.root, path) else {
            return false;
        };
        match fs::symlink_metadata(path) {
            // A plain file is no burst. A dot name (`.gitignore`, `.git/index`)
            // moves the walk, so it keeps the window open.
            Ok(m) => m.is_file() && !id.split('/').any(|part| part.starts_with('.')),
            Err(_) => true,
        }
    }

    /// Takes one message into the dirty set. A cookie message is not ours
    /// here: only `cookie_sync` waits for one, so it is dropped.
    fn absorb(&mut self, msg: Msg) {
        match msg {
            Msg::Source(at, path) => {
                self.stats.sources += 1;
                self.forget_scopes(&path);
                if !self.readable_path(&path) {
                    self.last_unindexed = Some(at);
                }
                self.dirty.insert(path);
            }
            Msg::Cookie(_) => {}
            Msg::Broken => self.broken = true,
        }
    }

    /// Takes every pending message into the dirty set. With a kqueue watch, a
    /// zero-timeout drain of the kqueue is the sync point.
    /// Drops the kept scope markers of the dir that holds `path`, and of `path`
    /// itself (it may be a dir): a create, a delete or a rename there can add
    /// or remove a marker file.
    fn forget_scopes(&mut self, path: &Path) {
        let Some(id) = id_of(&self.filter.root, path) else {
            return;
        };
        let Some(parts) = self.parts.as_mut() else {
            return;
        };
        parts.scopes.forget(&id);
        let parent = id.rfind('/').map_or("", |at| &id[..at]);
        parts.scopes.forget(parent);
    }

    fn drain(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            self.absorb(msg);
        }
        let mut out = Vec::new();
        if let Some(kq) = self.kq.as_mut() {
            kq.drain(&mut out);
        }
        for item in out {
            match item {
                Out::Source(path) => self.absorb(Msg::Source(Instant::now(), path)),
                Out::Config(path) => {
                    self.broken = true;
                    self.absorb(Msg::Source(Instant::now(), path));
                }
                Out::Broken => self.broken = true,
            }
        }
    }

    fn racy(&self) -> bool {
        self.last_unindexed
            .is_some_and(|t| t.elapsed() < RACY_WINDOW)
    }

    /// Writes a cookie file and waits for its event. Events that arrive
    /// first go to the dirty set. Gives `false` on a write failure or after
    /// 200 ms.
    fn cookie_sync(&mut self) -> bool {
        self.cookie_n += 1;
        let dir = self.filter.ctx.join(".cache").join("cookies");
        let path = dir.join(format!("c{}-{}", std::process::id(), self.cookie_n));
        if fs::create_dir_all(&dir).is_err() || fs::write(&path, b"").is_err() {
            return false;
        }
        let deadline = Instant::now() + COOKIE_WAIT;
        let mut seen = false;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(left) {
                Ok(Msg::Cookie(p)) if p == path => {
                    seen = true;
                    break;
                }
                Ok(msg) => self.absorb(msg),
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break,
            }
        }
        let _ = fs::remove_file(&path);
        seen
    }

    /// Restarts the watcher on a fresh filter (the config may have changed).
    fn restart(&mut self) {
        self.stats.restarts += 1;
        self.watcher = None;
        self.kq = None;
        self.filter.includes = read_include_dirs(&self.root);
        if self.shortcut_ok {
            self.kq = Kq::open(&self.filter, self.kq_limit);
        }
        if self.kq.is_none() {
            self.watcher = start_watcher(&self.filter, &self.tx);
        }
        self.broken = false;
    }

    /// True when the content of `path` still hashes to the print of `id`.
    fn matches_record(&self, path: &Path, id: &str) -> bool {
        let Some(print) = self.record.as_ref().and_then(|r| r.files.get(id)) else {
            return false;
        };
        if print.hash.is_empty() {
            return false;
        }
        fs::read(path)
            .ok()
            .and_then(|b| decode_source(&b))
            .is_some_and(|text| hash_source(text.as_bytes()) == print.hash)
    }

    /// Sorts the dirty set: clean, a probe, or a one-file patch (V2).
    /// `clean` collects the source files that match their record, and `stale`
    /// says that a file outside the set may have moved the walk.
    fn plan(&self, clean: &mut Vec<String>, stale: &mut bool) -> Plan {
        let patchable = self.parts.is_some();
        let mut ids: Vec<String> = Vec::new();
        let mut walk = false;
        let mut relink = false;
        for path in &self.dirty {
            let Some(id) = id_of(&self.filter.root, path) else {
                return Plan::Probe;
            };
            match fs::symlink_metadata(path) {
                Ok(m) if m.is_file() => {
                    let source = claims(Path::new(&id));
                    if self.matches_record(path, &id) {
                        if source {
                            clean.push(id);
                        }
                        continue;
                    }
                    if !source {
                        // A manifest or a dot file changes the walk or the link
                        // rules. Any other file only moves the walk list, which
                        // step 1 reads at its next walk.
                        let dot = path
                            .file_name()
                            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
                        let listed = self
                            .parts
                            .as_ref()
                            .is_some_and(|p| p.files.iter().any(|f| f == Path::new(&id)));
                        let in_record = self
                            .record
                            .as_ref()
                            .is_some_and(|r| r.files.contains_key(&id));
                        if !patchable || dot {
                            return Plan::Probe;
                        }
                        if in_record {
                            // The record holds this manifest. Swap its entry and
                            // link again, as for a new manifest.
                            if sieve_parse::read_at_link(Path::new(&id)) {
                                relink = true;
                            }
                            ids.push(id);
                            continue;
                        }
                        if sieve_parse::read_at_link(Path::new(&id)) {
                            // The link reads this file, but the record does not
                            // hold it. Walk and link again.
                            walk = true;
                            relink = true;
                        }
                        *stale |= !listed;
                        continue;
                    }
                    if !patchable {
                        return Plan::Probe;
                    }
                    let known = self
                        .parts
                        .as_ref()
                        .is_some_and(|p| p.cache.files.contains_key(&id));
                    walk |= !known;
                    ids.push(id);
                }
                Ok(_) => return Plan::Probe,
                Err(_) => {
                    let Some(parts) = &self.parts else {
                        return Plan::Probe;
                    };
                    if parts.cache.files.contains_key(&id) {
                        // A tracked file that is gone stays in the walk, so the
                        // patch drops its entry by id.
                        walk = true;
                        ids.push(id);
                        continue;
                    }
                    // A missing path that was never in the set changes nothing,
                    // unless it was a dir that held indexed files.
                    let dir = format!("{id}/");
                    let held = parts
                        .cache
                        .files
                        .range(dir.clone()..)
                        .next()
                        .is_some_and(|(k, _)| k.starts_with(&dir));
                    let listed = parts.files.iter().any(|f| f == Path::new(&id));
                    if held || listed {
                        return Plan::Probe;
                    }
                }
            }
        }
        if ids.is_empty() && !walk {
            Plan::Clean
        } else {
            Plan::Patch { ids, walk, relink }
        }
    }

    /// Swaps the dirty files in memory and links the whole set (V2). Gives
    /// `false` when the call must run the probe. The disk writes wait in
    /// `pending`.
    fn patch(&mut self, ids: &[String], walk: bool, relink: bool) -> bool {
        let (Some(base), Some(prior)) = (self.record.clone(), self.graph.clone()) else {
            return false;
        };
        let Some(parts) = self.parts.as_mut() else {
            return false;
        };
        if self.parsers.is_none() {
            self.parsers = Extractors::new().ok();
        }
        let Some(parsers) = self.parsers.as_mut() else {
            return false;
        };
        let mut timer = sieve_parse::timing::Timer::start();
        let walk_ms = walk_start_ms(&self.context_dir);
        timer.lap("patch: clock");
        let only = base.only_dirs.clone().unwrap_or_default();
        let mut touched: Vec<String> = Vec::new();
        let mut removed: Vec<String> = Vec::new();
        let mut moved = false;
        let mut live: Option<HashSet<String>> = None;
        if walk || parts.walk_stale {
            let Ok(files) = walk_files(&self.root, &self.context_dir, &only) else {
                self.parts = None;
                return false;
            };
            moved = files != parts.files;
            let set: HashSet<String> = files.iter().map(|f| slash(f)).collect();
            for id in &set {
                if !parts.cache.files.contains_key(id) && claims(Path::new(id)) {
                    match fresh_entry(&self.root, id, parsers) {
                        Ok(Some(entry)) => {
                            parts.cache.files.insert(id.clone(), entry);
                            touched.push(id.clone());
                        }
                        Ok(None) => {}
                        Err(_) => {
                            self.parts = None;
                            return false;
                        }
                    }
                }
            }
            parts.cache.files.retain(|id, _| {
                let keep = set.contains(id);
                if !keep {
                    removed.push(id.clone());
                }
                keep
            });
            parts.files = files;
            parts.walk_stale = false;
            live = Some(set);
        }
        for id in ids {
            if live.as_ref().is_some_and(|set| !set.contains(id)) || touched.contains(id) {
                continue;
            }
            let fresh = fresh_entry(&self.root, id, parsers);
            timer.lap("patch: fresh entry");
            match fresh {
                Ok(Some(entry)) => {
                    parts.cache.files.insert(id.clone(), entry);
                    touched.push(id.clone());
                }
                Ok(None) => {
                    if parts.cache.files.remove(id).is_some() {
                        removed.push(id.clone());
                    }
                }
                Err(_) => {
                    self.parts = None;
                    return false;
                }
            }
        }
        timer.lap("patch: walk and parse");
        if touched.is_empty() && removed.is_empty() && !moved && !relink {
            return true;
        }
        let Ok((graph, moved)) = link_entries_moving(
            &self.root,
            &parts.files,
            &mut parts.cache,
            &prior,
            &mut parts.scopes,
        ) else {
            self.parts = None;
            return false;
        };
        let moved_files = moved.map(|mut m| {
            m.restore_raw(&mut parts.cache);
            m.files
        });
        timer.lap("patch: link entries");
        let prints: BTreeMap<String, Print> = parts
            .cache
            .files
            .iter()
            .map(|(id, e)| {
                let print = Print {
                    size: e.size,
                    mtime_ms: e.mtime_ms,
                    hash: e.hash.clone(),
                };
                (id.clone(), print)
            })
            .collect();
        let record = Fingerprint {
            version: base.version,
            extractor: base.extractor.clone(),
            files: prints,
            only_dirs: base.only_dirs.clone(),
            walked_at_ms: Some(walk_ms),
        };
        if let Some(words) = self.words.as_mut() {
            for id in &touched {
                words.refresh_file(&self.root, id);
            }
            for id in &removed {
                words.drop_file(id);
            }
        }
        // The graph a reader decodes from disk: sorted, with no body text.
        timer.lap("patch: prints and words");
        let (written, bodies, pos) = sieve_core::into_written_keeping_hinted(graph, &prior);
        let graph = Arc::new(written);
        timer.lap("patch: sort graph");
        self.pending = Some(Pending {
            base,
            record: record.clone(),
            graph: Arc::clone(&graph),
            bodies,
            pos,
            moved: moved_files,
        });
        self.retired = self.graph.replace(graph);
        self.record = Some(record);
        true
    }

    /// Drops everything in memory, so the next call reloads from disk.
    fn invalidate(&mut self) {
        self.graph = None;
        self.record = None;
        self.fp_stat = None;
        self.parts = None;
        self.words = None;
        self.pending = None;
        // A queued job starts from a record this graph no longer follows.
        let mut st = self.writer.lock();
        st.slot = None;
        st.stat = None;
    }

    /// Hands the writes of a patch to the writer thread. The snapshot of the
    /// cache is a cheap clone: the moved files hold no nodes yet. The call
    /// path then gets its nodes back, and the writer thread does the same on
    /// its own copy.
    fn hand_off(&mut self) {
        let Some(p) = self.pending.take() else {
            return;
        };
        let Some(parts) = self.parts.as_mut() else {
            self.invalidate();
            return;
        };
        let started = Instant::now();
        let snapshot = parts.cache.clone();
        copy_back(&mut parts.cache, &p);
        if self.timing {
            eprintln!(
                "[sieve] warm: hand off ({:.1} ms)",
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        let job = Job {
            pending: p,
            cache: snapshot,
        };
        self.writer.submit(&self.context_dir, job);
    }

    /// Takes the result of the writer thread. A failed write drops the graph,
    /// so the next call reloads from disk.
    fn absorb_writes(&mut self) {
        let (failed, stat) = {
            let mut st = self.writer.lock();
            (std::mem::take(&mut st.failed), st.stat.take())
        };
        if failed {
            self.invalidate();
        } else if stat.is_some() {
            self.fp_stat = stat;
        }
    }

    /// Waits for the writes, then takes their result.
    fn settle_writes(&mut self) {
        self.hand_off();
        self.writer.wait_idle();
        self.absorb_writes();
    }

    /// The cache on disk does not match the record. Else the reply would read
    /// the cache, and fail, every time. Remove the record under the build
    /// lock, so every reader sees the pair as stale. The next patch falls back
    /// to the probe, which rebuilds and writes a matched pair.
    fn heal_pair(&mut self) {
        self.parts_failed = true;
        let held = take_lock(&self.context_dir);
        // Without the lock another build may write now, and its pair is whole.
        if held.is_some() {
            let fp_path = fingerprint_path(&self.context_dir, &extractor_stamp());
            let _ = fs::remove_file(fp_path);
        }
    }

    /// Reads every file's parse from the cache on disk and the walk list, if
    /// the cache holds exactly the entries of the record.
    fn load_parts(&mut self) {
        let Some(record) = &self.record else {
            return;
        };
        let stamp = extractor_stamp();
        let only = record.only_dirs.clone().unwrap_or_default();
        let Ok(files) = walk_files(&self.root, &self.context_dir, &only) else {
            return;
        };
        let cache = read_cache(&cache_path(&self.context_dir, &stamp), &stamp);
        let same = cache.files.len() == record.files.len()
            && record.files.iter().all(|(id, p)| {
                cache.files.get(id).is_some_and(|e| {
                    e.hash == p.hash && e.size == p.size && e.mtime_ms == p.mtime_ms
                })
            });
        if same {
            self.parts = Some(Parts {
                files,
                cache,
                walk_stale: false,
                scopes: ScopeMemo::default(),
            });
        }
    }

    /// The work that follows a reply, before the next call: the disk writes of
    /// a patch, and (after a reply only) the checks of V4, the in-memory
    /// parts and the word index.
    fn finish(&mut self, post_reply: bool) {
        self.hand_off();
        self.retired = None;
        // The rest is heavy, so it never runs inside a call.
        if !post_reply {
            return;
        }
        self.poll_ready(Duration::ZERO);
        if self.ready && self.fresh_start {
            // The watcher runs now. One probe covers the time before it.
            self.fresh_start = false;
            self.drain();
            self.dirty.clear();
            let opts = self.opts;
            ensure_fresh_graph(&self.root, &self.context_dir, &opts);
            self.sync_graph();
        }
        if !(self.ready && self.shortcut_ok && self.graph.is_some()) {
            return;
        }
        if self.parts.is_none() && !self.parts_failed {
            let started = Instant::now();
            self.load_parts();
            if self.parts.is_none() {
                self.heal_pair();
            }
            if self.timing {
                eprintln!(
                    "[sieve] warm: parts {} ({:.1} ms)",
                    if self.parts.is_some() {
                        "loaded"
                    } else {
                        "not loaded"
                    },
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        if let Some(graph) = &self.graph {
            if !self
                .qindex
                .as_ref()
                .is_some_and(|(g, _)| Arc::ptr_eq(g, graph))
            {
                let started = Instant::now();
                let index = Arc::new(GraphIndex::build(graph));
                if self.timing {
                    eprintln!(
                        "[sieve] warm: query index ({:.1} ms)",
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                }
                self.qindex = Some((Arc::clone(graph), index));
            }
        }
        if self.words.is_none() {
            if let Some(graph) = &self.graph {
                let started = Instant::now();
                let index = WordIndex::build(graph, &self.root);
                if self.timing {
                    eprintln!(
                        "[sieve] warm: word index {:.1} MB ({:.1} ms)",
                        index.approx_bytes() as f64 / 1e6,
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                }
                self.words = Some(index);
            }
        }
    }

    /// Reloads the graph only when the fingerprint on disk differs from the
    /// record this process holds. So its own writes cause no reload.
    fn sync_graph(&mut self) {
        let stamp = extractor_stamp();
        let fp_path = fingerprint_path(&self.context_dir, &stamp);
        let id = stat_id(&fp_path);
        if self.graph.is_some() && id == self.fp_stat {
            return;
        }
        let fp = read_fingerprint(&fp_path, &stamp);
        if self.graph.is_some() && fp == self.record {
            self.fp_stat = id;
            return;
        }
        // The graph this process just wrote needs no decode.
        let graph = self.keeper.take_graph(&self.context_dir).or_else(|| {
            self.stats.decodes += 1;
            let path = self.context_dir.join(".graph").join("wiring.json");
            serde_json::from_slice(&fs::read(path).ok()?).ok()
        });
        self.record = fp;
        self.fp_stat = id;
        self.graph = graph.map(Arc::new);
        // The parts and the word index belong to the graph they came from.
        self.parts = None;
        self.parts_failed = false;
        self.words = None;
    }

    /// The warm refresh for one call.
    fn refresh(&mut self, opts: &RefreshOptions) -> RefreshOutcome {
        let started = Instant::now();
        self.opts = *opts;
        self.finish(false);
        self.absorb_writes();
        self.drain();
        self.poll_ready(Duration::ZERO);
        let cold =
            !self.ready || !self.shortcut_ok || self.graph.is_none() || self.record.is_none();
        // Another process may have rewritten the fingerprint. Reload before the
        // dirty set is read, so a patch never starts from an older graph.
        // A write in flight leaves the disk older than the memory, so a reload
        // now would give an older graph. The writer checks the fingerprint.
        if !cold && !self.writer.busy() {
            self.sync_graph();
        }
        let mut probe = cold || self.graph.is_none() || self.record.is_none();
        let mut why = if self.ready {
            "cold or shortcut off"
        } else {
            "checks pending"
        };
        if self.ready && self.fresh_start {
            self.fresh_start = false;
            probe = true;
            why = "watcher start";
        }
        if self.ready && ((self.watcher.is_none() && self.kq.is_none()) || self.broken) {
            self.restart();
            probe = true;
            why = "watcher restart";
        }
        if !probe && self.racy() {
            probe = true;
            why = "racy window";
        }
        // The kqueue drain above (`self.drain()`) is the sync point.
        if !probe && self.kq.is_none() && !self.cookie_sync() {
            self.stats.fallbacks += 1;
            probe = true;
            why = "cookie timeout";
        }
        if !probe && self.broken {
            self.restart();
            probe = true;
            why = "watcher restart";
        }
        if !probe && self.racy() {
            probe = true;
            why = "racy window";
        }
        let mut patched = false;
        if !probe && !self.dirty.is_empty() {
            let (mut clean, mut stale) = (Vec::new(), false);
            let plan = self.plan(&mut clean, &mut stale);
            if stale {
                if let Some(parts) = self.parts.as_mut() {
                    parts.walk_stale = true;
                }
            }
            // A file back at its recorded text may have had a mid-edit read
            // in the word index, so its entry is read again.
            if let Some(words) = self.words.as_mut() {
                for id in &clean {
                    words.refresh_file(&self.root, id);
                }
            }
            match plan {
                Plan::Clean => self.dirty.clear(),
                Plan::Patch { ids, walk, relink } if self.patch(&ids, walk, relink) => {
                    self.dirty.clear();
                    self.stats.patches += 1;
                    patched = true;
                }
                Plan::Patch { .. } | Plan::Probe => {
                    probe = true;
                    why = "dirty set";
                }
            }
        }
        let mut outcome = RefreshOutcome::default();
        if probe {
            self.stats.probes += 1;
            // The probe reads the disk, so the writes end first.
            self.settle_writes();
            // Events that end before the probe walk are covered by the walk.
            self.drain();
            if !self.dirty.is_empty() {
                // A probe reads no unclaimed file, so the walk list may be old.
                if let Some(parts) = self.parts.as_mut() {
                    parts.walk_stale = true;
                }
            }
            self.dirty.clear();
            // Events that ended before the probe are not in the dirty set.
            if let Some(parts) = self.parts.as_mut() {
                parts.scopes.clear();
            }
            outcome = ensure_fresh_graph(&self.root, &self.context_dir, opts);
        } else if !patched {
            self.stats.memory += 1;
        }
        // A patch holds the newest graph. A reload now could swap in an older
        // disk graph after the dirty set is cleared.
        if !patched && !(self.graph.is_some() && self.writer.busy()) {
            self.sync_graph();
        }
        if self.timing {
            eprintln!(
                "[sieve] warm: {} ({:.1} ms)",
                if probe {
                    why
                } else if patched {
                    "patch in memory"
                } else {
                    "answer from memory"
                },
                started.elapsed().as_secs_f64() * 1000.0
            );
        }
        outcome
    }
}

/// Joins path components with `/`, as the walk list ids are.
fn slash(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

static WARMS: Mutex<Vec<Warm>> = Mutex::new(Vec::new());

thread_local! {
    /// The graph the current call verified, for `load_graph`.
    static CURRENT: RefCell<Option<(PathBuf, Arc<Graph>)>> = const { RefCell::new(None) };
}

/// Clears the call's warm graph when the call ends.
pub(crate) struct ClearOnDrop;

impl Drop for ClearOnDrop {
    fn drop(&mut self) {
        CURRENT.with(|c| c.borrow_mut().take());
    }
}

/// The graph this call verified for `context_dir`, if the warm path ran.
pub(crate) fn current_graph(context_dir: &Path) -> Option<Arc<Graph>> {
    CURRENT.with(|c| {
        let c = c.borrow();
        let (ctx, graph) = c.as_ref()?;
        (ctx == context_dir).then(|| Arc::clone(graph))
    })
}

/// The pre-call refresh for a single root: the warm path when an index
/// exists and refresh is on, else the step 1 refresh.
pub(crate) fn refresh(root: &Path, context_dir: &Path, opts: &RefreshOptions) -> RefreshOutcome {
    let has_index = context_dir.join(".graph").join("wiring.json").is_file();
    // `<PRODUCT>_NO_WARM` keeps the step 1 path, to measure against it.
    let off = env_truthy(&sieve_core::product().env_var("NO_WARM"));
    if opts.disabled || !has_index || off {
        return ensure_fresh_graph(root, context_dir, opts);
    }
    let mut warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
    let at = match warms
        .iter()
        .position(|w| w.root == root && w.context_dir == context_dir)
    {
        Some(at) => at,
        None => {
            warms.push(Warm::new(root, context_dir));
            warms.len() - 1
        }
    };
    let warm = &mut warms[at];
    let outcome = warm.refresh(opts);
    let graph = warm.graph.clone();
    CURRENT.with(|c| *c.borrow_mut() = graph.map(|g| (context_dir.to_path_buf(), g)));
    outcome
}

/// Runs the work that follows a reply for one pair: the disk writes of a
/// patch, the background checks, the parts and the word index. The MCP loop
/// calls it right after it writes a reply. The wait for the checks runs with
/// the registry unlocked.
pub(crate) fn after_reply(root: &Path, context_dir: &Path) {
    let rx = {
        let mut warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
        let warm = warms
            .iter_mut()
            .find(|w| w.root == root && w.context_dir == context_dir);
        warm.and_then(|w| {
            w.start_checks();
            w.ready_rx.take()
        })
    };
    let waited = rx.map(|rx| {
        let got = rx.recv_timeout(READY_WAIT);
        (rx, got)
    });
    let mut warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(w) = warms
        .iter_mut()
        .find(|w| w.root == root && w.context_dir == context_dir)
    {
        if let Some((rx, got)) = waited {
            w.accept_ready(rx, got);
        }
        w.finish(true);
    }
}

/// Finishes the pending disk writes of one pair before a tool reads the disk.
/// The wait runs with the registry unlocked.
///
/// Gives `true` when the writes failed and the graph in memory was dropped.
/// The caller then refreshes again, so it never reads two states in one call.
pub(crate) fn flush(root: &Path, context_dir: &Path) -> bool {
    let writer = {
        let mut warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(w) = warms
            .iter_mut()
            .find(|w| w.root == root && w.context_dir == context_dir)
        else {
            return false;
        };
        w.hand_off();
        Arc::clone(&w.writer)
    };
    writer.wait_idle();
    let mut warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(w) = warms
        .iter_mut()
        .find(|w| w.root == root && w.context_dir == context_dir)
    {
        let had = w.graph.is_some();
        w.absorb_writes();
        return had && w.graph.is_none();
    }
    false
}

/// The query lookups for `graph`, when this pair built them for that graph.
pub(crate) fn query_index(
    root: &Path,
    context_dir: &Path,
    graph: &Arc<Graph>,
) -> Option<Arc<GraphIndex>> {
    let warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
    let (g, index) = warms
        .iter()
        .find(|w| w.root == root && w.context_dir == context_dir)?
        .qindex
        .as_ref()?;
    Arc::ptr_eq(g, graph).then(|| Arc::clone(index))
}

/// The `word_grep_chars` figure for one name, from the word index when this
/// pair has one, else from the disk scan.
pub(crate) fn word_chars(root: &Path, context_dir: &Path, graph: &Graph, name: &str) -> u64 {
    let warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
    let index = warms
        .iter()
        .find(|w| w.root == root && w.context_dir == context_dir)
        .and_then(|w| w.words.as_ref());
    word_grep_chars_indexed(index, graph, root, name)
}

/// Drops the warm state of one pair, and so its watcher.
#[cfg(test)]
pub(crate) fn forget(root: &Path, context_dir: &Path) {
    let mut warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
    warms.retain(|w| !(w.root == root && w.context_dir == context_dir));
}

/// Runs `f` on the warm state of one pair, if it exists.
#[cfg(test)]
pub(crate) fn with<T>(
    root: &Path,
    context_dir: &Path,
    f: impl FnOnce(&mut Warm) -> T,
) -> Option<T> {
    let mut warms = WARMS.lock().unwrap_or_else(PoisonError::into_inner);
    warms
        .iter_mut()
        .find(|w| w.root == root && w.context_dir == context_dir)
        .map(f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::call;
    use serde_json::json;

    /// A temp dir that removes itself on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("sieve-warm-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Scratch(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// A repo with `a.ts` defining `alpha`, built, with its context dir.
    fn repo(label: &str) -> (Scratch, PathBuf, PathBuf) {
        crate::tools::force_refresh_for_test();
        let s = Scratch::new(label);
        let root = s.0.join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.ts"), "export function alpha() { return 1; }\n").unwrap();
        let ctx = root.join(".sieve");
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        (s, root, ctx)
    }

    fn trace(root: &Path, ctx: &Path, symbol: &str) -> String {
        call("sieve_trace_calls", &json!({ "symbol": symbol }), root, ctx).0
    }

    fn shows(root: &Path, ctx: &Path, symbol: &str) -> bool {
        trace(root, ctx, symbol).contains(symbol) && !trace(root, ctx, symbol).contains("no symbol")
    }

    fn stats(root: &Path, ctx: &Path) -> Stats {
        with(root, ctx, |w| w.stats).unwrap()
    }

    /// Runs the step that follows a reply, as the MCP loop does: the checks
    /// of V4, the parts and the word index.
    fn settle(root: &Path, ctx: &Path) {
        // The FSEvents start can take seconds on a busy machine.
        let until = Instant::now() + Duration::from_secs(60);
        loop {
            after_reply(root, ctx);
            if with(root, ctx, |w| w.ready).unwrap() || Instant::now() > until {
                break;
            }
        }
        let state = with(root, ctx, |w| {
            (
                w.ready,
                w.shortcut_ok,
                w.graph.is_some(),
                w.record.is_some(),
                w.parts.is_some(),
            )
        })
        .unwrap();
        assert!(state.0 && state.4, "{state:?}");
    }

    /// A built repo from `files`, with its context dir.
    fn fixture(label: &str, files: &[(&str, &str)]) -> (Scratch, PathBuf, PathBuf) {
        crate::tools::force_refresh_for_test();
        let s = Scratch::new(label);
        let root = s.0.join("repo");
        fs::create_dir_all(&root).unwrap();
        for (rel, body) in files {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        let ctx = root.join(".sieve");
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        (s, root, ctx)
    }

    /// The graph in memory and the graph of a full build of the same tree,
    /// as JSON text.
    fn memory_and_full(root: &Path, ctx: &Path) -> (String, String) {
        flush(root, ctx);
        let memory = with(root, ctx, |w| w.graph.clone()).unwrap().unwrap();
        let full = sieve_parse::build_graph_cached(root, ctx).unwrap().graph;
        (
            serde_json::to_string(&*memory).unwrap(),
            serde_json::to_string(&sieve_core::into_written(full)).unwrap(),
        )
    }

    /// Builds `files`, warms the index, applies `edits` (`None` deletes), calls
    /// once, and checks that the patched graph equals a full build.
    fn parity(label: &str, files: &[(&str, &str)], edits: &[(&str, Option<&str>)]) {
        let (_s, root, ctx) = fixture(label, files);
        trace(&root, &ctx, "zzz");
        settle(&root, &ctx);
        let before = stats(&root, &ctx);
        for (rel, body) in edits {
            let path = root.join(rel);
            match body {
                Some(text) => {
                    fs::create_dir_all(path.parent().unwrap()).unwrap();
                    fs::write(path, text).unwrap();
                }
                None => fs::remove_file(path).unwrap(),
            }
        }
        trace(&root, &ctx, "zzz");
        let after = stats(&root, &ctx);
        assert_eq!(after.patches, before.patches + 1, "{label}: {after:?}");
        assert_eq!(after.probes, before.probes, "{label}: {after:?}");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(
            memory == full,
            "{label}: the patched graph differs from a full build"
        );
        forget(&root, &ctx);
    }

    #[test]
    fn an_edit_then_an_immediate_call_shows_the_edit() {
        let (_s, root, ctx) = repo("edit");
        assert!(shows(&root, &ctx, "alpha"));
        fs::write(root.join("a.ts"), "export function beta() { return 2; }\n").unwrap();
        assert!(shows(&root, &ctx, "beta"));
        forget(&root, &ctx);
    }

    #[test]
    fn a_quiet_call_answers_from_memory() {
        let (_s, root, ctx) = repo("quiet");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        for _ in 0..3 {
            trace(&root, &ctx, "alpha");
        }
        // A loaded machine may time out one cookie (a counted fallback), so
        // the test allows that and still needs one answer from memory.
        let st = stats(&root, &ctx);
        assert!(st.memory >= 1, "{st:?}");
        assert_eq!(st.probes, 1 + st.fallbacks, "{st:?}");
        forget(&root, &ctx);
    }

    #[test]
    fn a_write_by_rename_shows_at_once() {
        let (_s, root, ctx) = repo("rename");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(root.join("tmp.swap"), "export function gamma() {}\n").unwrap();
        fs::rename(root.join("tmp.swap"), root.join("a.ts")).unwrap();
        assert!(shows(&root, &ctx, "gamma"));
        forget(&root, &ctx);
    }

    #[test]
    fn a_new_dir_with_fifty_files_in_a_burst_shows_at_once() {
        let (_s, root, ctx) = repo("burst");
        trace(&root, &ctx, "alpha");
        fs::create_dir_all(root.join("pkg")).unwrap();
        for n in 0..50 {
            let body = format!("export function burst{n}() {{}}\n");
            fs::write(root.join("pkg").join(format!("f{n}.ts")), body).unwrap();
        }
        assert!(shows(&root, &ctx, "burst0"));
        assert!(shows(&root, &ctx, "burst49"));
        forget(&root, &ctx);
    }

    #[test]
    fn a_delete_shows_at_once() {
        let (_s, root, ctx) = repo("delete");
        fs::write(root.join("b.ts"), "export function doomed() {}\n").unwrap();
        assert!(shows(&root, &ctx, "doomed"));
        fs::remove_file(root.join("b.ts")).unwrap();
        assert!(!shows(&root, &ctx, "doomed"));
        forget(&root, &ctx);
    }

    /// A warm repo with a kqueue watch, after the first reply.
    #[cfg(target_os = "macos")]
    fn kq_repo(label: &str) -> (Scratch, PathBuf, PathBuf) {
        let (s, root, ctx) = repo(label);
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        trace(&root, &ctx, "alpha");
        assert!(with(&root, &ctx, |w| w.kq.is_some()).unwrap());
        (s, root, ctx)
    }

    /// The kqueue path answers with no probe and no cookie fallback.
    #[cfg(target_os = "macos")]
    fn assert_kq_path(root: &Path, ctx: &Path, before: Stats) {
        let after = stats(root, ctx);
        assert_eq!(after.probes, before.probes, "{after:?}");
        assert_eq!(after.fallbacks, 0, "{after:?}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kqueue_sees_an_edit_a_new_file_and_a_delete_at_once() {
        let (_s, root, ctx) = kq_repo("kq-basic");
        let before = stats(&root, &ctx);
        fs::write(root.join("a.ts"), "export function beta() { return 2; }\n").unwrap();
        assert!(shows(&root, &ctx, "beta"));
        fs::write(root.join("b.ts"), "export function newfile() {}\n").unwrap();
        assert!(shows(&root, &ctx, "newfile"));
        fs::remove_file(root.join("b.ts")).unwrap();
        assert!(!shows(&root, &ctx, "newfile"));
        assert_kq_path(&root, &ctx, before);
        forget(&root, &ctx);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn kqueue_sees_a_new_dir_with_files_and_a_write_by_rename() {
        let (_s, root, ctx) = kq_repo("kq-dir");
        let before = stats(&root, &ctx);
        fs::create_dir_all(root.join("pkg/inner")).unwrap();
        fs::write(root.join("pkg/inner/x.ts"), "export function deepfn() {}\n").unwrap();
        assert!(shows(&root, &ctx, "deepfn"));
        // A file in the new dir has its own watch.
        fs::write(root.join("pkg/inner/x.ts"), "export function deeper() {}\n").unwrap();
        assert!(shows(&root, &ctx, "deeper"));
        fs::write(root.join("tmp.swap"), "export function gamma() {}\n").unwrap();
        fs::rename(root.join("tmp.swap"), root.join("a.ts")).unwrap();
        assert!(shows(&root, &ctx, "gamma"));
        fs::write(root.join("a.ts"), "export function delta() {}\n").unwrap();
        assert!(shows(&root, &ctx, "delta"));
        assert_kq_path(&root, &ctx, before);
        forget(&root, &ctx);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_low_fd_limit_keeps_fsevents_and_the_cookie() {
        let (_s, root, ctx) = kq_repo("kq-limit");
        with(&root, &ctx, |w| {
            w.kq_limit = 4;
            w.restart();
            assert!(w.kq.is_none());
            assert!(w.watcher.is_some());
        })
        .unwrap();
        // The restart asks for one probe; the cookie path serves the edit.
        trace(&root, &ctx, "alpha");
        fs::write(root.join("a.ts"), "export function epsilon() {}\n").unwrap();
        assert!(shows(&root, &ctx, "epsilon"));
        forget(&root, &ctx);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_hundred_create_and_delete_turns_keep_the_watch_size() {
        let (_s, root, ctx) = kq_repo("kq-leak");
        let base = with(&root, &ctx, |w| w.kq.as_ref().map(|k| k.len())).unwrap();
        for n in 0..100 {
            let name = format!("t{n}.ts");
            fs::write(root.join(&name), format!("export function t{n}() {{}}\n")).unwrap();
            assert!(shows(&root, &ctx, &format!("t{n}")));
            fs::remove_file(root.join(&name)).unwrap();
            trace(&root, &ctx, "alpha");
        }
        let after = with(&root, &ctx, |w| w.kq.as_ref().map(|k| k.len())).unwrap();
        assert_eq!(base, after);
        forget(&root, &ctx);
    }

    #[test]
    fn a_marker_file_added_then_removed_in_a_dir_keeps_the_kept_scopes_true() {
        let body: String = (0..8)
            .map(|n| format!("export function pk{n}() {{}}\n"))
            .collect();
        let files = [
            ("a.ts", "export function alpha() {}\n"),
            ("pkg/x.ts", body.as_str()),
        ];
        let (_s, root, ctx) = fixture("scope-memo", &files);
        trace(&root, &ctx, "zzz");
        settle(&root, &ctx);
        // The first patch lists every dir and keeps the markers.
        fs::write(
            root.join("pkg/x.ts"),
            format!("{body}export function more() {{}}\n"),
        )
        .unwrap();
        trace(&root, &ctx, "zzz");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "after the first patch");
        // A marker appears in a dir that the memo holds as empty.
        fs::write(root.join("pkg/Cargo.toml"), "[package]\nname = \"pkg\"\n").unwrap();
        trace(&root, &ctx, "zzz");
        fs::write(
            root.join("pkg/x.ts"),
            format!("{body}export function more2() {{}}\n"),
        )
        .unwrap();
        trace(&root, &ctx, "zzz");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "after the marker appeared");
        assert!(memory.contains("Cargo.toml"), "the scope holds the marker");
        // The marker goes away again.
        fs::remove_file(root.join("pkg/Cargo.toml")).unwrap();
        fs::write(
            root.join("pkg/x.ts"),
            format!("{body}export function more3() {{}}\n"),
        )
        .unwrap();
        trace(&root, &ctx, "zzz");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "after the marker went");
        assert!(!memory.contains("Cargo.toml"));
        forget(&root, &ctx);
    }

    #[test]
    fn an_edit_to_a_manifest_in_the_record_patches_and_equals_a_full_build() {
        let files = [
            ("package.json", "{\"name\":\"a\"}\n"),
            ("src/a.ts", "export function alpha() {}\n"),
        ];
        let (_s, root, ctx) = fixture("manifest", &files);
        trace(&root, &ctx, "zzz");
        settle(&root, &ctx);
        assert!(with(&root, &ctx, |w| w
            .record
            .as_ref()
            .is_some_and(|r| r.files.contains_key("package.json")))
        .unwrap());
        let before = stats(&root, &ctx);
        fs::write(
            root.join("package.json"),
            "{\"name\":\"b\",\"workspaces\":[\"src\"]}\n",
        )
        .unwrap();
        trace(&root, &ctx, "zzz");
        let after = stats(&root, &ctx);
        assert_eq!(after.patches, before.patches + 1, "{after:?}");
        assert_eq!(after.probes, before.probes, "{after:?}");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(
            memory == full,
            "the patched graph differs from a full build"
        );
        forget(&root, &ctx);
    }

    /// Builds `files`, warms the index, edits one manifest, calls twice (the
    /// second after an ordinary edit), and checks the memory equals a full build.
    fn manifest_then_second_call(label: &str, files: &[(&str, &str)], manifest: (&str, &str)) {
        let (_s, root, ctx) = fixture(label, files);
        trace(&root, &ctx, "zzz");
        settle(&root, &ctx);
        let before = stats(&root, &ctx);
        fs::write(root.join(manifest.0), manifest.1).unwrap();
        trace(&root, &ctx, "zzz");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(
            memory == full,
            "{label}: the manifest call differs from a full build"
        );
        // A second call, with an ordinary edit after the manifest patch.
        let (rel, _) = files[1];
        fs::write(root.join(rel), "export function second() {}\n").unwrap();
        assert!(shows(&root, &ctx, "second") || !rel.ends_with(".ts"));
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(
            memory == full,
            "{label}: the second call differs from a full build"
        );
        let after = stats(&root, &ctx);
        assert_eq!(after.probes, before.probes, "{label}: {after:?}");
        forget(&root, &ctx);
    }

    #[test]
    fn an_edit_to_cargo_toml_equals_a_full_build() {
        parity(
            "manifest-cargo",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
                ),
                ("src/lib.rs", "pub fn alpha() {}\n"),
            ],
            &[(
                "Cargo.toml",
                Some("[package]\nname = \"b\"\nversion = \"0.2.0\"\n[dependencies]\n"),
            )],
        );
    }

    #[test]
    fn an_edit_to_go_mod_equals_a_full_build() {
        parity(
            "manifest-gomod",
            &[
                ("go.mod", "module example.com/a\n\ngo 1.21\n"),
                ("main.go", "package main\n\nfunc Alpha() {}\n"),
            ],
            &[("go.mod", Some("module example.com/b\n\ngo 1.22\n"))],
        );
    }

    #[test]
    fn a_second_call_after_a_manifest_patch_equals_a_full_build() {
        manifest_then_second_call(
            "manifest-second",
            &[
                ("package.json", "{\"name\":\"a\"}\n"),
                ("src/a.ts", "export function alpha() {}\n"),
            ],
            (
                "package.json",
                "{\"name\":\"b\",\"workspaces\":[\"src\"]}\n",
            ),
        );
    }

    #[test]
    fn two_patches_at_once_leave_the_disk_equal_to_a_full_build() {
        let (_s, root, ctx) = repo("coalesce");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(root.join("a.ts"), "export function one() {}\n").unwrap();
        assert!(shows(&root, &ctx, "one"));
        fs::write(root.join("b.ts"), "export function two() {}\n").unwrap();
        assert!(shows(&root, &ctx, "two"));
        flush(&root, &ctx);
        assert_eq!(stats(&root, &ctx).patches, 2);
        let on_disk = fs::read_to_string(ctx.join(".graph").join("wiring.json")).unwrap();
        let disk: Graph = serde_json::from_str(&on_disk).unwrap();
        let full = sieve_parse::build_graph_cached(&root, &ctx).unwrap().graph;
        assert_eq!(
            serde_json::to_string(&disk).unwrap(),
            serde_json::to_string(&sieve_core::into_written(full)).unwrap()
        );
        // The record on disk is the record in memory: no drift for a probe.
        let opts = RefreshOptions::default();
        assert!(!ensure_fresh_graph(&root, &ctx, &opts).refreshed);
        forget(&root, &ctx);
    }

    #[test]
    fn flush_waits_for_the_write_and_the_next_call_does_not() {
        let (_s, root, ctx) = repo("flush-wait");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        // Another holder of the build lock keeps the write waiting.
        let holder = LockGuard::acquire(&ctx).unwrap().unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            drop(holder);
        });
        fs::write(root.join("a.ts"), "export function omega() {}\n").unwrap();
        assert!(shows(&root, &ctx, "omega"));
        after_reply(&root, &ctx);
        assert!(with(&root, &ctx, |w| w.writer.busy()).unwrap());
        // A call answers from memory while the write waits.
        let started = Instant::now();
        assert!(shows(&root, &ctx, "omega"));
        assert!(started.elapsed() < Duration::from_millis(400));
        assert!(with(&root, &ctx, |w| w.writer.busy()).unwrap());
        flush(&root, &ctx);
        assert!(!with(&root, &ctx, |w| w.writer.busy()).unwrap());
        let on_disk = fs::read_to_string(ctx.join(".graph").join("wiring.json")).unwrap();
        assert!(on_disk.contains("omega"));
        release.join().unwrap();
        forget(&root, &ctx);
    }

    #[test]
    fn a_panic_in_the_writer_does_not_block_flush_and_the_next_call_reloads() {
        let (_s, root, ctx) = repo("writer-panic");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        // The second call of `shows` hands the write to the writer.
        *HOOK.lock().unwrap() = Some((ctx.clone(), true));
        fs::write(root.join("a.ts"), "export function omega() {}\n").unwrap();
        assert!(shows(&root, &ctx, "omega"));
        // The wait ends, and the graph in memory is dropped.
        assert!(flush(&root, &ctx));
        assert!(!with(&root, &ctx, |w| w.writer.busy()).unwrap());
        assert!(with(&root, &ctx, |w| w.graph.is_none()).unwrap());
        assert!(shows(&root, &ctx, "omega"));
        forget(&root, &ctx);
    }

    #[test]
    fn an_outside_build_during_a_queued_write_gives_one_state_to_the_call() {
        let (_s, root, ctx) = repo("outside-build");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        // The writer pauses, so the outside build moves the record first.
        *HOOK.lock().unwrap() = Some((ctx.clone(), false));
        fs::write(root.join("a.ts"), "export function omega() {}\n").unwrap();
        assert!(shows(&root, &ctx, "omega"));
        fs::write(root.join("b.ts"), "export function tau() {}\n").unwrap();
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        // A tool that reads the disk: the write fails inside the call, and the
        // call refreshes again. The answer holds the outside build.
        let (text, _) = call("sieve_find_code", &json!({ "query": "tau" }), &root, &ctx);
        assert!(text.contains("tau"), "{text}");
        let graph = current_or_disk(&ctx);
        assert!(graph.contains("tau") && graph.contains("omega"));
        forget(&root, &ctx);
    }

    /// The wiring on disk, as text.
    fn current_or_disk(ctx: &Path) -> String {
        fs::read_to_string(ctx.join(".graph").join("wiring.json")).unwrap()
    }

    #[test]
    fn a_cache_that_does_not_match_the_record_drops_the_record_and_the_probe_heals() {
        let (_s, root, ctx) = repo("parts-heal");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        // Break the pair on disk: the cache holds an entry the record lacks.
        with(&root, &ctx, |w| w.parts = None);
        let stamp = extractor_stamp();
        let path = cache_path(&ctx, &stamp);
        let mut cache = read_cache(&path, &stamp);
        let entry = cache.files.values().next().unwrap().clone();
        cache.files.insert("ghost.ts".to_string(), entry);
        write_cache(&path, &cache).unwrap();
        after_reply(&root, &ctx);
        assert!(with(&root, &ctx, |w| w.parts.is_none() && w.parts_failed).unwrap());
        assert!(!fingerprint_path(&ctx, &stamp).exists());
        // More replies do not read the cache again.
        after_reply(&root, &ctx);
        // The next call probes, and the probe writes a matched pair.
        let before = stats(&root, &ctx).probes;
        assert!(shows(&root, &ctx, "alpha"));
        assert!(stats(&root, &ctx).probes > before);
        assert!(fingerprint_path(&ctx, &stamp).exists());
        after_reply(&root, &ctx);
        assert!(with(&root, &ctx, |w| w.parts.is_some() && !w.parts_failed).unwrap());
        forget(&root, &ctx);
    }

    #[test]
    fn a_gitignore_edit_runs_the_probe() {
        let (_s, root, ctx) = repo("gitignore");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        let before = stats(&root, &ctx).probes;
        std::thread::sleep(RACY_WINDOW + Duration::from_millis(200));
        fs::write(root.join(".gitignore"), "a.ts\n").unwrap();
        trace(&root, &ctx, "alpha");
        assert_eq!(stats(&root, &ctx).probes, before + 1);
        forget(&root, &ctx);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_root_shows_the_edit() {
        let (s, real, _) = repo("symlink");
        let link = s.0.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let ctx = link.join(".sieve");
        assert!(shows(&link, &ctx, "alpha"));
        settle(&link, &ctx);
        fs::write(link.join("a.ts"), "export function delta() {}\n").unwrap();
        assert!(shows(&link, &ctx, "delta"));
        assert!(with(&link, &ctx, |w| w.shortcut_ok).unwrap());
        forget(&link, &ctx);
    }

    #[test]
    fn a_context_dir_outside_the_root_turns_the_shortcut_off() {
        crate::tools::force_refresh_for_test();
        let s = Scratch::new("outside");
        let root = s.0.join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.ts"), "export function alpha() {}\n").unwrap();
        let ctx = s.0.join("ctx");
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        assert!(shows(&root, &ctx, "alpha"));
        fs::write(root.join("a.ts"), "export function omega() {}\n").unwrap();
        assert!(shows(&root, &ctx, "omega"));
        let st = stats(&root, &ctx);
        assert_eq!(st.memory, 0);
        forget(&root, &ctx);
    }

    #[test]
    fn a_rebuild_by_another_process_is_reloaded() {
        let (_s, root, ctx) = repo("concurrent");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(root.join("a.ts"), "export function sigma() {}\n").unwrap();
        // The other process rebuilds before this process calls again.
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        assert!(shows(&root, &ctx, "sigma"));
        // A rebuild with no source change swaps the fingerprint only.
        std::thread::sleep(RACY_WINDOW + Duration::from_millis(200));
        trace(&root, &ctx, "sigma");
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        let before = stats(&root, &ctx);
        assert!(shows(&root, &ctx, "sigma"));
        assert_eq!(stats(&root, &ctx).probes, before.probes);
        forget(&root, &ctx);
    }

    #[test]
    fn a_watcher_overflow_or_a_dead_watcher_falls_back_to_the_probe() {
        let (_s, root, ctx) = repo("overflow");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        std::thread::sleep(RACY_WINDOW + Duration::from_millis(200));
        let before = stats(&root, &ctx);
        with(&root, &ctx, |w| w.tx.send(Msg::Broken).unwrap());
        trace(&root, &ctx, "alpha");
        let mid = stats(&root, &ctx);
        assert_eq!(
            (mid.probes, mid.restarts),
            (before.probes + 1, before.restarts + 1)
        );
        with(&root, &ctx, |w| {
            w.watcher = None;
            w.kq = None;
        });
        fs::write(root.join("a.ts"), "export function kappa() {}\n").unwrap();
        assert!(shows(&root, &ctx, "kappa"));
        assert_eq!(stats(&root, &ctx).restarts, mid.restarts + 1);
        forget(&root, &ctx);
    }

    #[test]
    fn the_process_own_writes_cause_no_event_and_no_decode() {
        let (_s, root, ctx) = repo("own");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(root.join("a.ts"), "export function theta() {}\n").unwrap();
        assert!(shows(&root, &ctx, "theta"));
        std::thread::sleep(RACY_WINDOW + Duration::from_millis(200));
        let before = stats(&root, &ctx);
        trace(&root, &ctx, "theta");
        let after = stats(&root, &ctx);
        // The rebuild wrote under .cache and .graph; the watcher saw none of it.
        assert_eq!(after.sources, before.sources);
        assert_eq!(after.decodes, before.decodes);
        assert_eq!(after.memory, before.memory + 1);
        forget(&root, &ctx);
    }

    #[test]
    fn a_body_edit_patches_in_memory_and_equals_a_full_build() {
        parity(
            "p-body",
            &[
                (
                    "a.ts",
                    "import { b } from './b';\nexport function a() { return b(); }\n",
                ),
                ("b.ts", "export function b() { return 1; }\n"),
            ],
            &[(
                "a.ts",
                Some("import { b } from './b';\nexport function a() { return b() + b(); }\n"),
            )],
        );
    }

    #[test]
    fn a_rust_pub_use_patches_and_equals_a_full_build() {
        parity(
            "p-rust",
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                ),
                ("src/lib.rs", "pub mod inner;\npub mod user;\n"),
                ("src/inner.rs", "pub fn deep() {}\n"),
                ("src/user.rs", "use crate::deep;\npub fn u() { deep(); }\n"),
            ],
            &[(
                "src/lib.rs",
                Some("pub mod inner;\npub mod user;\npub use inner::deep;\n"),
            )],
        );
    }

    #[test]
    fn a_ts_export_star_patches_and_equals_a_full_build() {
        parity(
            "p-star",
            &[
                ("barrel.ts", "export const q = 1;\n"),
                ("defs.ts", "export function y() { return 2; }\n"),
                (
                    "use.ts",
                    "import { y } from './barrel';\nexport function z() { return y(); }\n",
                ),
            ],
            &[("barrel.ts", Some("export * from './defs';\n"))],
        );
    }

    #[test]
    fn a_class_extends_change_patches_and_equals_a_full_build() {
        parity(
            "p-extends",
            &[
                ("a.ts", "export class A { m() { return 1; } }\nexport class B { n() { return 2; } }\n"),
                ("c.ts", "import { B } from './a';\nexport class C extends B { k() { return this.m(); } }\n"),
            ],
            &[("a.ts", Some("export class A { m() { return 1; } }\nexport class B extends A { n() { return 2; } }\n"))],
        );
    }

    #[test]
    fn a_python_import_change_patches_and_equals_a_full_build() {
        parity(
            "p-python",
            &[
                ("p1.py", "def f():\n    return 1\n"),
                ("p2.py", "def f():\n    return 2\n"),
                ("use.py", "from p1 import f\n\ndef g():\n    return f()\n"),
            ],
            &[(
                "use.py",
                Some("from p2 import f\n\ndef g():\n    return f()\n"),
            )],
        );
    }

    #[test]
    fn an_arity_change_patches_and_equals_a_full_build() {
        parity(
            "p-arity",
            &[
                ("a.ts", "export function f(a: number) { return a; }\nexport function f(a: number, b: number) { return a + b; }\n"),
                ("b.ts", "import { f } from './a';\nexport function g() { return f(1); }\n"),
            ],
            &[("a.ts", Some("export function f(a: number, b: number) { return a + b; }\n"))],
        );
    }

    #[test]
    fn a_new_file_patches_and_equals_a_full_build() {
        parity(
            "p-new",
            &[("b.ts", "export function g() { return ghost(); }\n")],
            &[("c.ts", Some("export function ghost() { return 1; }\n"))],
        );
    }

    #[test]
    fn a_deleted_file_patches_and_equals_a_full_build() {
        parity(
            "p-del",
            &[
                ("b.ts", "export function g() { return ghost(); }\n"),
                ("c.ts", "export function ghost() { return 1; }\n"),
            ],
            &[("c.ts", None)],
        );
    }

    #[test]
    fn a_name_that_makes_an_inferred_link_ambiguous_equals_a_full_build() {
        parity(
            "p-ambiguous",
            &[
                ("a.ts", "export function dup() { return 1; }\n"),
                ("b.ts", "export function g() { return dup(); }\n"),
                ("c.ts", "export function other() { return 2; }\n"),
            ],
            &[(
                "c.ts",
                Some(
                    "export function other() { return 2; }\nexport function dup() { return 3; }\n",
                ),
            )],
        );
    }

    #[test]
    fn a_patch_writes_the_disk_after_the_reply_and_the_probe_finds_no_drift() {
        let (_s, root, ctx) = repo("disk");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(root.join("a.ts"), "export function omega() {}\n").unwrap();
        assert!(shows(&root, &ctx, "omega"));
        assert_eq!(stats(&root, &ctx).patches, 1);
        after_reply(&root, &ctx);
        assert!(with(&root, &ctx, |w| w.pending.is_none()).unwrap());
        flush(&root, &ctx);
        // A step 1 probe from another process sees a fresh index.
        let opts = RefreshOptions::default();
        assert!(!ensure_fresh_graph(&root, &ctx, &opts).refreshed);
        let on_disk = fs::read_to_string(ctx.join(".graph").join("wiring.json")).unwrap();
        assert!(on_disk.contains("omega") && !on_disk.contains("alpha"));
        // The disk graph equals a full build, node for node.
        let disk: Graph = serde_json::from_str(&on_disk).unwrap();
        let full = sieve_parse::build_graph_cached(&root, &ctx).unwrap().graph;
        assert_eq!(disk.nodes.len(), full.nodes.len());
        forget(&root, &ctx);
    }

    #[test]
    fn a_fingerprint_moved_by_another_build_drops_the_patch() {
        let (_s, root, ctx) = repo("moved");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(root.join("a.ts"), "export function sigma() {}\n").unwrap();
        // One call only: its patch still owes the disk writes.
        assert!(trace(&root, &ctx, "sigma").contains("sigma"));
        assert!(with(&root, &ctx, |w| w.pending.is_some()).unwrap());
        // Another build moves the record before this process writes.
        fs::write(root.join("b.ts"), "export function tau() {}\n").unwrap();
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        after_reply(&root, &ctx);
        flush(&root, &ctx);
        assert!(with(&root, &ctx, |w| w.graph.is_none() && w.parts.is_none()).unwrap());
        // The next call reloads from disk and shows both edits.
        assert!(shows(&root, &ctx, "sigma"));
        assert!(shows(&root, &ctx, "tau"));
        forget(&root, &ctx);
    }

    #[test]
    fn a_temp_file_rename_patches_with_no_probe() {
        let (_s, root, ctx) = repo("rename-patch");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        let before = stats(&root, &ctx);
        fs::write(root.join("tmp.swap"), "export function gamma() {}\n").unwrap();
        fs::rename(root.join("tmp.swap"), root.join("a.ts")).unwrap();
        assert!(shows(&root, &ctx, "gamma"));
        let after = stats(&root, &ctx);
        assert_eq!(after.probes, before.probes, "{after:?}");
        assert!(after.patches > before.patches, "{after:?}");
        forget(&root, &ctx);
    }

    #[test]
    fn a_checks_pending_call_uses_the_step_1_path() {
        let (_s, root, ctx) = repo("pending");
        let mut warm = Warm::new(&root, &ctx);
        assert!(!warm.ready && !warm.shortcut_ok);
        // Until the checks finish, a refresh probes and answers.
        let out = warm.refresh(&RefreshOptions::default());
        assert!(!out.refreshed);
        assert!(warm.graph.is_some());
        // The checks start after the first reply, as `after_reply` does.
        warm.start_checks();
        let until = Instant::now() + Duration::from_secs(60);
        while !warm.ready && Instant::now() < until {
            warm.finish(true);
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(warm.ready && warm.parts.is_some() && warm.words.is_some());
    }

    #[test]
    fn a_branch_switch_shows_at_once_and_a_later_edit_patches() {
        let git = |root: &Path, args: &[&str]| {
            let ok = Command::new("git")
                .arg("-C")
                .arg(root)
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "git {args:?}");
        };
        let (_s, root, ctx) = repo("branch");
        fs::write(root.join(".gitignore"), ".sieve/\n").unwrap();
        git(&root, &["init", "-q"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "one"]);
        git(&root, &["checkout", "-q", "-b", "feat"]);
        fs::write(root.join("a.ts"), "export function onfeat() {}\n").unwrap();
        fs::write(root.join("c.ts"), "export function extra() {}\n").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-q", "-m", "two"]);
        git(&root, &["checkout", "-q", "-"]);
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        assert!(shows(&root, &ctx, "alpha"));
        settle(&root, &ctx);
        // The switch moves .git/HEAD and the index, edits a file, adds one.
        git(&root, &["checkout", "-q", "feat"]);
        assert!(shows(&root, &ctx, "onfeat"));
        assert!(shows(&root, &ctx, "extra"));
        assert!(!shows(&root, &ctx, "alpha"));
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "after the switch");
        // After the probe rebuilt the index, an edit takes the patch path.
        settle(&root, &ctx);
        // The git events of the switch keep the racy window open for 2 s.
        std::thread::sleep(RACY_WINDOW + Duration::from_millis(200));
        let before = stats(&root, &ctx);
        fs::write(root.join("c.ts"), "export function extra2() {}\n").unwrap();
        assert!(shows(&root, &ctx, "extra2"));
        let after = stats(&root, &ctx);
        assert_eq!(after.patches, before.patches + 1, "{after:?}");
        assert_eq!(after.probes, before.probes, "{after:?}");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "after the edit");
        // Switching back removes the file and restores the old body.
        git(&root, &["checkout", "-q", "-f", "-"]);
        assert!(shows(&root, &ctx, "alpha"));
        assert!(!shows(&root, &ctx, "extra2"));
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "after the switch back");
        forget(&root, &ctx);
    }

    #[test]
    fn the_word_index_follows_a_patch() {
        let (_s, root, ctx) = repo("words");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        let graph = with(&root, &ctx, |w| w.graph.clone()).unwrap().unwrap();
        let want = sieve_query::callers::word_grep_chars(&graph, &root, "alpha");
        assert_eq!(word_chars(&root, &ctx, &graph, "alpha"), want);
        assert!(with(&root, &ctx, |w| w
            .words
            .as_ref()
            .unwrap()
            .chars("alpha")
            .is_some())
        .unwrap());
        fs::write(
            root.join("a.ts"),
            "export function alpha() { return alpha; }\n// alpha\n",
        )
        .unwrap();
        trace(&root, &ctx, "alpha");
        let graph = with(&root, &ctx, |w| w.graph.clone()).unwrap().unwrap();
        let want = sieve_query::callers::word_grep_chars(&graph, &root, "alpha");
        assert_eq!(word_chars(&root, &ctx, &graph, "alpha"), want);
        assert_eq!(
            with(&root, &ctx, |w| w.words.as_ref().unwrap().chars("alpha")).unwrap(),
            Some(want)
        );
        forget(&root, &ctx);
    }

    #[test]
    fn a_patch_writes_the_same_ask_index_and_lookup_as_a_rebuild() {
        let (_s, root, ctx) = repo("askidx");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(
            root.join("a.ts"),
            "export function omega() { return distinctivebodyword + omega; }\n",
        )
        .unwrap();
        assert!(shows(&root, &ctx, "omega"));
        after_reply(&root, &ctx);
        flush(&root, &ctx);
        let ask = ctx.join(".cache").join("ask-index.json");
        let lookup = sieve_parse::lookup_path(&ctx);
        let (ask_patch, lookup_patch) = (fs::read(&ask).unwrap(), fs::read(&lookup).unwrap());
        assert!(String::from_utf8_lossy(&ask_patch).contains("distinctivebodyword"));
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        assert!(ask_patch == fs::read(&ask).unwrap(), "ask-index differs");
        // The first line holds the stamp of the wiring file, which differs.
        let body = |b: &[u8]| b.iter().position(|c| *c == b'\n').map(|i| b[i..].to_vec());
        assert!(
            body(&lookup_patch) == body(&fs::read(&lookup).unwrap()),
            "lookup differs"
        );
        forget(&root, &ctx);
    }

    #[test]
    fn a_rebuild_from_an_older_tree_state_does_not_hide_a_later_edit() {
        let (_s, root, ctx) = repo("older");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        // Another process rebuilds when the file holds `vone`.
        fs::write(root.join("a.ts"), "export function vone() {}\n").unwrap();
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        // This process then sees the edit to `vtwo`.
        fs::write(root.join("a.ts"), "export function vtwo() {}\n").unwrap();
        assert!(shows(&root, &ctx, "vtwo"));
        assert!(!shows(&root, &ctx, "vone"));
        forget(&root, &ctx);
    }

    #[test]
    fn an_edit_and_a_revert_leave_the_word_index_true() {
        let (_s, root, ctx) = repo("revert");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        let original = fs::read_to_string(root.join("a.ts")).unwrap();
        // A mid-edit read lands in the word index, then the file is reverted.
        fs::write(root.join("a.ts"), "alpha alpha alpha alpha\n").unwrap();
        with(&root, &ctx, |w| {
            w.words.as_mut().unwrap().refresh_file(&root, "a.ts")
        });
        fs::write(root.join("a.ts"), &original).unwrap();
        trace(&root, &ctx, "alpha");
        let graph = with(&root, &ctx, |w| w.graph.clone()).unwrap().unwrap();
        let want = sieve_query::callers::word_grep_chars(&graph, &root, "alpha");
        let got = with(&root, &ctx, |w| w.words.as_ref().unwrap().chars("alpha")).unwrap();
        assert_eq!(got, Some(want));
        forget(&root, &ctx);
    }

    #[test]
    fn a_patch_carries_the_meaning_layer_like_a_rebuild() {
        let (_s, root, ctx) = fixture(
            "meaning",
            &[
                ("a.ts", "export function alpha() { return 1; }\n"),
                ("b.ts", "export function beta() { return 2; }\n"),
            ],
        );
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        // Give both nodes a summary, in memory and on disk.
        let mut graph = with(&root, &ctx, |w| w.graph.clone()).unwrap().unwrap();
        let g = Arc::make_mut(&mut graph);
        for node in g
            .nodes
            .iter_mut()
            .filter(|n| n.name == "alpha" || n.name == "beta")
        {
            node.summary = Some(format!("summary of {}", node.name));
            node.summary_state = sieve_core::wiring::SummaryState::Ready;
        }
        sieve_parse::write_wiring_and_lookup(g, &ctx).unwrap();
        with(&root, &ctx, |w| w.graph = Some(graph.clone()));
        // A body edit of alpha makes its summary stale; beta stays ready.
        fs::write(
            root.join("a.ts"),
            "export function alpha() { return 11; }\n",
        )
        .unwrap();
        trace(&root, &ctx, "alpha");
        let state = |name: &str| {
            with(&root, &ctx, |w| {
                let g = w.graph.clone().unwrap();
                g.nodes
                    .iter()
                    .find(|n| n.name == name)
                    .map(|n| (n.summary_state, n.summary.clone()))
            })
            .unwrap()
            .unwrap()
        };
        assert_eq!(state("alpha").0, sieve_core::wiring::SummaryState::Stale);
        assert_eq!(state("beta").0, sieve_core::wiring::SummaryState::Ready);
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "the meaning layer differs from a rebuild");
        forget(&root, &ctx);
    }

    #[test]
    fn an_edit_to_a_file_the_walk_ignores_runs_no_probe() {
        let (_s, root, ctx) = repo("notes");
        fs::write(root.join("README.md"), "one\n").unwrap();
        sieve_parse::refresh::rebuild_graph_only(&root, &ctx).unwrap();
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        let before = stats(&root, &ctx);
        fs::write(root.join("README.md"), "two\n").unwrap();
        fs::write(root.join("NEW.md"), "three\n").unwrap();
        assert!(shows(&root, &ctx, "alpha"));
        let after = stats(&root, &ctx);
        assert_eq!(after.probes, before.probes, "{after:?}");
        // A later patch walks again, because NEW.md may have moved the list.
        fs::write(root.join("a.ts"), "export function alpha() { return 5; }\n").unwrap();
        trace(&root, &ctx, "alpha");
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full);
        forget(&root, &ctx);
    }

    #[test]
    fn a_patch_that_grows_the_wiring_past_the_cap_is_capped_in_the_same_call() {
        let (_s, root, ctx) = repo("cap");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        let size = fs::metadata(ctx.join(".graph").join("wiring.json"))
            .unwrap()
            .len();
        crate::tools::set_test_wiring_cap(Some(size + 50));
        let mut body = String::new();
        for n in 0..40 {
            body.push_str(&format!("export function grow{n}() {{ return {n}; }}\n"));
        }
        fs::write(root.join("a.ts"), body).unwrap();
        let (text, is_error) = call(
            "sieve_trace_calls",
            &json!({ "symbol": "grow1" }),
            &root,
            &ctx,
        );
        crate::tools::set_test_wiring_cap(None);
        assert!(is_error && text.contains("over the size cap"), "{text}");
        forget(&root, &ctx);
    }

    /// Builds `files`, warms the index, applies `edits`, calls once, and checks
    /// that the answer graph equals a full build and that a probe ran.
    fn probe_equals_full(label: &str, files: &[(&str, &str)], edits: &[(&str, &str)]) {
        let (_s, root, ctx) = fixture(label, files);
        trace(&root, &ctx, "zzz");
        settle(&root, &ctx);
        let before = stats(&root, &ctx);
        for (rel, body) in edits {
            let path = root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, body).unwrap();
        }
        trace(&root, &ctx, "zzz");
        let after = stats(&root, &ctx);
        assert!(
            after.probes + after.patches > before.probes + before.patches,
            "{label}: the graph was not refreshed"
        );
        let (memory, full) = memory_and_full(&root, &ctx);
        assert!(memory == full, "{label}: differs from a full build");
        forget(&root, &ctx);
    }

    #[test]
    fn an_edit_to_a_file_the_link_reads_refreshes_and_equals_a_full_build() {
        probe_equals_full(
            "n1-go",
            &[
                ("go.mod", "module example.com/a\n"),
                ("a.go", "package a\nfunc A() {}\n"),
            ],
            &[("go.mod", "module example.com/b\n")],
        );
        probe_equals_full(
            "n1-pkg",
            &[
                ("package.json", "{}"),
                ("web/a.ts", "export function a() {}\n"),
            ],
            &[("package.json", "{\"workspaces\": [\"web\"]}")],
        );
        probe_equals_full(
            "n1-cargo",
            &[
                ("src/lib.rs", "pub fn top() {}\n"),
                ("sub/src/lib.rs", "pub fn deep() {}\n"),
            ],
            &[(
                "sub/Cargo.toml",
                "[package]\nname = \"sub\"\nversion = \"0.1.0\"\n",
            )],
        );
    }

    #[test]
    fn a_patch_is_not_replaced_by_the_end_of_call_reload() {
        let (_s, root, ctx) = repo("n2");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        fs::write(root.join("a.ts"), "export function patched() {}\n").unwrap();
        // Make the disk fingerprint look moved: a stale stat id forces a reload
        // check at the end of the call.
        with(&root, &ctx, |w| w.fp_stat = None);
        assert!(shows(&root, &ctx, "patched"));
        assert!(with(&root, &ctx, |w| w.graph.is_some()).unwrap());
        forget(&root, &ctx);
    }

    #[test]
    fn the_filter_follows_the_event_rules() {
        let f = Filter {
            root: PathBuf::from("/r"),
            ctx: PathBuf::from("/r/.sieve"),
            includes: Vec::new(),
            excludes: Some(PathBuf::from("/h/ignore")),
        };
        let c = |p: &str| f.classify(Path::new(p));
        assert_eq!(c("/r/.sieve/.cache/lookup.v1"), Class::Ignore);
        assert_eq!(c("/r/.sieve/.graph/wiring.json"), Class::Ignore);
        assert_eq!(c("/r/.sieve/.cache/cookies/c1"), Class::Cookie);
        assert_eq!(c("/r/.sieve/config.json"), Class::Config);
        assert_eq!(c("/r/node_modules/x/y.js"), Class::Ignore);
        assert_eq!(c("/r/target"), Class::Ignore);
        assert_eq!(c("/r/.git/objects/ab/cd"), Class::Ignore);
        assert_eq!(c("/r/.git/index"), Class::Source);
        assert_eq!(c("/r/.git/refs/heads/main"), Class::Source);
        assert_eq!(c("/r/.git/info/exclude"), Class::Source);
        assert_eq!(c("/r/.gitignore"), Class::Source);
        assert_eq!(c("/r/src/a.ts"), Class::Source);
        assert_eq!(c("/h/ignore"), Class::Source);
        assert_eq!(c("/elsewhere/x"), Class::Source);
    }

    #[test]
    fn the_mount_parser_finds_remote_mounts() {
        let mac = "/dev/disk3s1 on / (apfs, local)\n//u@h/share on /Volumes/share (smbfs, nodev)\n";
        assert!(remote_mount_in(mac, Path::new("/Volumes/share/repo")));
        assert!(!remote_mount_in(mac, Path::new("/Users/me/repo")));
        let linux = "/dev/sda1 on / type ext4 (rw)\nsrv:/x on /mnt type nfs4 (rw)\n";
        assert!(remote_mount_in(linux, Path::new("/mnt/repo")));
        assert!(!remote_mount_in(linux, Path::new("/home/me")));
    }

    /// 200 turns of edit, new file, write by rename, delete and new dir, each
    /// followed at once by a call. Every answer must show the change. The path
    /// counts print with `--nocapture`.
    #[test]
    fn a_freshness_loop_of_two_hundred_turns_shows_every_change() {
        let (_s, root, ctx) = repo("loop");
        trace(&root, &ctx, "alpha");
        settle(&root, &ctx);
        let before = stats(&root, &ctx);
        let start = Instant::now();
        for n in 0..200 {
            let sym = format!("turn{n}");
            let body = format!("export function {sym}() {{}}\n");
            match n % 5 {
                0 => {
                    fs::write(root.join("a.ts"), &body).unwrap();
                    assert!(shows(&root, &ctx, &sym), "edit {n}");
                }
                1 => {
                    fs::write(root.join(format!("n{n}.ts")), &body).unwrap();
                    assert!(shows(&root, &ctx, &sym), "new file {n}");
                }
                2 => {
                    fs::write(root.join("tmp.swap"), &body).unwrap();
                    fs::rename(root.join("tmp.swap"), root.join("a.ts")).unwrap();
                    assert!(shows(&root, &ctx, &sym), "rename {n}");
                }
                3 => {
                    // Delete the file that turn n - 2 made.
                    let old = format!("turn{}", n - 2);
                    fs::remove_file(root.join(format!("n{}.ts", n - 2))).unwrap();
                    assert!(!shows(&root, &ctx, &old), "delete {n}");
                }
                _ => {
                    let dir = root.join(format!("d{n}"));
                    fs::create_dir_all(&dir).unwrap();
                    for k in 0..3 {
                        let body = format!("export function {sym}k{k}() {{}}\n");
                        fs::write(dir.join(format!("f{k}.ts")), body).unwrap();
                    }
                    assert!(shows(&root, &ctx, &format!("{sym}k2")), "new dir {n}");
                }
            }
        }
        let after = stats(&root, &ctx);
        eprintln!(
            "freshness loop: {:?}, patches {}, probes {}, fallbacks {}",
            start.elapsed(),
            after.patches - before.patches,
            after.probes - before.probes,
            after.fallbacks - before.fallbacks
        );
        // The cookie always arrives, on both OSes.
        assert_eq!(after.fallbacks, before.fallbacks, "{after:?}");
        // The Linux rule: inotify can miss files in a new dir, so a new dir
        // keeps the 2 s window open and those calls probe. A new dir comes
        // every 5th turn, so the window rarely closes. The answers stay true.
        // kqueue has no such gap, so macOS never probes here.
        if cfg!(target_os = "macos") {
            assert_eq!(after.probes, before.probes, "{after:?}");
        }
        forget(&root, &ctx);
    }
}
