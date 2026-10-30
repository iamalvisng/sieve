//! The macOS sync of the warm index: one kqueue watches every dir and file of
//! the walk. A drain with a zero timeout
//! before each call proves that the dirty set holds every write that ended
//! before the call, so no cookie file and no wait are needed.
//!
//! Every `unsafe` block here calls `libc` (`open`, `close`, `kevent`,
//! `kqueue`, `getrlimit`). The kqueue replaces the cookie file on macOS, and the
//! libc calls are the only way to reach it.

use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::os::fd::RawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sieve_core::walk::is_skipped_name;

use super::{Class, Filter};

/// The vnode events one registration asks for.
const NOTES: u32 = libc::NOTE_DELETE
    | libc::NOTE_WRITE
    | libc::NOTE_EXTEND
    | libc::NOTE_ATTRIB
    | libc::NOTE_LINK
    | libc::NOTE_RENAME
    | libc::NOTE_REVOKE;
/// The events one `kevent` call reads.
const BATCH: usize = 256;

/// What a drain tells the warm index.
pub(super) enum Out {
    /// A source path changed.
    Source(PathBuf),
    /// A path in the context dir (the config) changed.
    Config(PathBuf),
    /// The sync is no longer complete: restart and probe.
    Broken,
}

/// Why one registration failed.
enum AddError {
    /// The path is gone, or not readable.
    Gone,
    /// The fd budget is spent.
    Full,
}

/// One registered path.
struct Entry {
    path: PathBuf,
    dir: bool,
    /// The size and mtime of a file when the watch last saw it.
    stat: Option<(u64, SystemTime)>,
}

/// The kqueue and the fds it watches.
pub(super) struct Kq {
    kq: RawFd,
    by_fd: HashMap<RawFd, Entry>,
    by_path: HashMap<PathBuf, RawFd>,
    filter: Filter,
    /// The most fds this watch may hold, a half of the soft limit.
    max_fds: usize,
    /// True when a registration met the budget: the watch is incomplete.
    full: bool,
    buf: Vec<libc::kevent>,
}

// SAFETY: the only raw pointers in `Kq` are the `udata` fields of the event
// buffer, and they are always null. The fds belong to this value alone, so
// the one move from the check thread to the worker is sound.
unsafe impl Send for Kq {}

/// The soft limit on open fds, or 0 when `getrlimit` fails.
pub(super) fn soft_limit() -> u64 {
    let mut lim = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `getrlimit` writes one `rlimit` through a valid pointer.
    let rc = unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) };
    if rc == 0 {
        lim.rlim_cur
    } else {
        0
    }
}

fn zero_event() -> libc::kevent {
    libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

fn file_stat(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.len(), meta.modified().ok()?))
}

fn is_gone(flags: u32) -> bool {
    flags & (libc::NOTE_DELETE | libc::NOTE_RENAME | libc::NOTE_REVOKE) != 0
}

impl Kq {
    /// Opens a watch on the tree of `filter`. It gives `None` when the fd
    /// count would pass half of `limit`, or when the kernel refuses.
    pub(super) fn open(filter: &Filter, limit: u64) -> Option<Kq> {
        let max_fds = usize::try_from(limit / 2).unwrap_or(usize::MAX);
        // SAFETY: `kqueue` takes no argument and gives a new fd or -1.
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return None;
        }
        // From here `Drop` closes the kqueue and every fd on each exit.
        let mut watch = Kq {
            kq,
            by_fd: HashMap::new(),
            by_path: HashMap::new(),
            filter: filter.clone(),
            max_fds,
            full: false,
            buf: vec![zero_event(); BATCH],
        };
        let mut listed: Vec<(PathBuf, bool)> = Vec::new();
        watch.list_tree(&filter.root, &mut listed);
        if let Some(excludes) = &filter.excludes {
            listed.push((excludes.clone(), false));
        }
        // Count first, so a big repo opens no fd at all.
        if listed.len() + 1 >= max_fds {
            return None;
        }
        for (path, dir) in listed {
            match watch.add(&path, dir) {
                // A path that vanished since the listing: the start probe
                // covers it.
                Ok(()) | Err(AddError::Gone) => {}
                Err(AddError::Full) => return None,
            }
        }
        Some(watch)
    }

    /// The count of watched paths.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.by_fd.len()
    }

    /// True when `path` is a dir the watch follows.
    fn want_dir(&self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.filter.root) else {
            return false;
        };
        let mut names = rel.components().map(|c| c.as_os_str());
        match names.next() {
            None => true,
            // Only `refs` and `info` of `.git` move a branch or an exclude.
            Some(first) if first == ".git" => match names.next() {
                None => true,
                Some(second) => second == "refs" || second == "info",
            },
            Some(_) => rel
                .components()
                .all(|c| !is_skipped_name(c.as_os_str(), &self.filter.includes)),
        }
    }

    /// Lists the dirs and files to watch under `dir`, with no fd opened.
    fn list_tree(&self, dir: &Path, out: &mut Vec<(PathBuf, bool)>) {
        out.push((dir.to_path_buf(), true));
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if path == self.filter.ctx {
                    // The config lives in the top of the context dir only.
                    out.push((path.clone(), true));
                    self.list_ctx_files(&path, out);
                } else if self.want_dir(&path) {
                    self.list_tree(&path, out);
                }
            } else if kind.is_file() && self.filter.classify(&path) != Class::Ignore {
                out.push((path, false));
            }
        }
    }

    fn list_ctx_files(&self, ctx: &Path, out: &mut Vec<(PathBuf, bool)>) {
        let Ok(entries) = std::fs::read_dir(ctx) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|k| k.is_file())
                && self.filter.classify(&path) == Class::Config
            {
                out.push((path, false));
            }
        }
    }

    /// Opens `path` with `O_EVTONLY` and registers it.
    fn add(&mut self, path: &Path, dir: bool) -> Result<(), AddError> {
        if self.by_path.contains_key(path) {
            return Ok(());
        }
        if self.by_fd.len() + 1 >= self.max_fds {
            self.full = true;
            return Err(AddError::Full);
        }
        let c_path = CString::new(path.as_os_str().as_bytes()).map_err(|_| AddError::Gone)?;
        // SAFETY: `c_path` is a NUL-terminated string that outlives the call.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_EVTONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            let errno = std::io::Error::last_os_error().raw_os_error();
            return if matches!(errno, Some(libc::EMFILE | libc::ENFILE)) {
                self.full = true;
                Err(AddError::Full)
            } else {
                Err(AddError::Gone)
            };
        }
        let change = libc::kevent {
            ident: fd as usize,
            filter: libc::EVFILT_VNODE,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: NOTES,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: `change` is one valid change record; the event list is empty,
        // so the kernel writes nothing back.
        let rc = unsafe {
            libc::kevent(
                self.kq,
                &change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if rc < 0 {
            close_fd(fd);
            return Err(AddError::Gone);
        }
        self.by_fd.insert(
            fd,
            Entry {
                path: path.to_path_buf(),
                dir,
                stat: if dir { None } else { file_stat(path) },
            },
        );
        self.by_path.insert(path.to_path_buf(), fd);
        Ok(())
    }

    /// Closes the fd of `path` and every fd below it. Gives the removed paths.
    fn remove_tree(&mut self, path: &Path) -> Vec<PathBuf> {
        let gone: Vec<PathBuf> = self
            .by_path
            .keys()
            .filter(|p| p.starts_with(path))
            .cloned()
            .collect();
        for p in &gone {
            if let Some(fd) = self.by_path.remove(p) {
                self.by_fd.remove(&fd);
                close_fd(fd);
            }
        }
        gone
    }

    fn emit(&self, path: PathBuf, out: &mut Vec<Out>) {
        match self.filter.classify(&path) {
            Class::Ignore | Class::Cookie => {}
            Class::Config => out.push(Out::Config(path)),
            Class::Source => out.push(Out::Source(path)),
        }
    }

    /// Reads `dir`, registers each new entry and reports it. A new dir gets
    /// its fd before it is read, so a write after the read shows at the next
    /// drain.
    fn rescan(&mut self, dir: &Path, out: &mut Vec<Out>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            self.emit(dir.to_path_buf(), out);
            return;
        };
        let is_ctx = dir == self.filter.ctx;
        for entry in entries.flatten() {
            let path = entry.path();
            if self.by_path.contains_key(&path) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                // A new dir reports only its files: an empty dir moves nothing.
                if !is_ctx && self.want_dir(&path) && self.add(&path, true).is_ok() {
                    self.rescan(&path, out);
                }
            } else if kind.is_file() {
                if self.filter.classify(&path) == Class::Ignore {
                    continue;
                }
                // A failed registration still reports the path.
                let _ = self.add(&path, false);
                self.emit(path, out);
            } else if !is_ctx {
                // A symlink or a special file: no fd, but the path is news.
                self.emit(path, out);
            }
        }
    }

    /// Reads every pending event with a zero timeout. Each event maps its fd
    /// back to a path; a dir event rescans the dir.
    pub(super) fn drain(&mut self, out: &mut Vec<Out>) {
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let mut rescans: HashSet<PathBuf> = HashSet::new();
        loop {
            // SAFETY: `buf` holds BATCH records, which the kernel fills up to
            // that count; `timeout` is valid for the call.
            let n = unsafe {
                libc::kevent(
                    self.kq,
                    std::ptr::null(),
                    0,
                    self.buf.as_mut_ptr(),
                    BATCH as libc::c_int,
                    &timeout,
                )
            };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                out.push(Out::Broken);
                break;
            }
            let n = n as usize;
            for at in 0..n {
                let event = self.buf[at];
                self.on_event(event.ident as RawFd, event.fflags, &mut rescans, out);
            }
            if n < BATCH {
                break;
            }
        }
        for dir in rescans {
            if self.by_path.contains_key(&dir) {
                self.rescan(&dir, out);
            }
        }
        if self.full {
            out.push(Out::Broken);
        }
    }

    fn on_event(
        &mut self,
        fd: RawFd,
        flags: u32,
        rescans: &mut HashSet<PathBuf>,
        out: &mut Vec<Out>,
    ) {
        let Some(entry) = self.by_fd.get(&fd) else {
            // The fd closed earlier in this drain.
            return;
        };
        let (path, dir) = (entry.path.clone(), entry.dir);
        if is_gone(flags) {
            // A renamed or deleted path: its fd follows the old vnode, so it
            // closes. The parent dir event registers any new vnode.
            let removed = if dir {
                self.remove_tree(&path)
            } else {
                if let Some(fd) = self.by_path.remove(&path) {
                    self.by_fd.remove(&fd);
                    close_fd(fd);
                }
                vec![path]
            };
            for p in removed {
                self.emit(p, out);
            }
        } else if dir {
            if flags & (libc::NOTE_WRITE | libc::NOTE_EXTEND | libc::NOTE_LINK) != 0 {
                rescans.insert(path);
            }
        } else if flags & libc::NOTE_LINK != 0 {
            // A rename over a hard-linked file leaves this fd on the old
            // inode with only NOTE_LINK. Open the path again, so the watch
            // follows the new inode and a later in-place edit shows.
            if let Some(old) = self.by_path.remove(&path) {
                self.by_fd.remove(&old);
                close_fd(old);
            }
            // A failed registration still reports the path.
            let _ = self.add(&path, false);
            self.emit(path, out);
        } else {
            // A read can change the access time, and macOS reports that as
            // an attribute event (git does it to `.git/index` on each
            // `ls-files`). A file whose size and mtime are the same, and
            // whose only event is an attribute change, holds the same bytes.
            let now = file_stat(&path);
            let same = now.is_some() && now == self.by_fd.get(&fd).and_then(|e| e.stat);
            if let Some(entry) = self.by_fd.get_mut(&fd) {
                entry.stat = now;
            }
            if flags == libc::NOTE_ATTRIB && same {
                return;
            }
            self.emit(path, out);
        }
    }
}

fn close_fd(fd: RawFd) {
    // SAFETY: the caller owns `fd` and drops every copy of it.
    unsafe {
        libc::close(fd);
    }
}

impl Drop for Kq {
    fn drop(&mut self) {
        for fd in self.by_fd.keys() {
            close_fd(*fd);
        }
        close_fd(self.kq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sieve-kq-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::canonicalize(dir).unwrap()
    }

    fn filter(root: &Path) -> Filter {
        Filter {
            root: root.to_path_buf(),
            ctx: root.join(".sieve"),
            includes: Vec::new(),
            excludes: None,
        }
    }

    fn sources(kq: &mut Kq) -> Vec<PathBuf> {
        let mut out = Vec::new();
        kq.drain(&mut out);
        out.into_iter()
            .filter_map(|o| match o {
                Out::Source(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    fn open_fds() -> usize {
        fs::read_dir("/dev/fd").map_or(0, |d| d.count())
    }

    #[test]
    fn a_big_tree_is_refused_under_a_low_fd_limit() {
        let root = scratch("limit");
        for n in 0..10 {
            fs::write(root.join(format!("f{n}.ts")), "x").unwrap();
        }
        assert!(Kq::open(&filter(&root), 8).is_none());
        assert!(Kq::open(&filter(&root), 1000).is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_write_a_new_file_a_delete_and_a_rename_show_at_the_next_drain() {
        let root = scratch("events");
        fs::write(root.join("a.ts"), "1").unwrap();
        let mut kq = Kq::open(&filter(&root), 100_000).unwrap();
        fs::write(root.join("a.ts"), "22").unwrap();
        assert!(sources(&mut kq).contains(&root.join("a.ts")));
        fs::write(root.join("b.ts"), "x").unwrap();
        assert!(sources(&mut kq).contains(&root.join("b.ts")));
        fs::remove_file(root.join("b.ts")).unwrap();
        assert!(sources(&mut kq).contains(&root.join("b.ts")));
        fs::write(root.join("t.swap"), "3").unwrap();
        fs::rename(root.join("t.swap"), root.join("a.ts")).unwrap();
        assert!(sources(&mut kq).contains(&root.join("a.ts")));
        // The new vnode of `a.ts` is watched: a later write shows.
        fs::write(root.join("a.ts"), "4444").unwrap();
        assert!(sources(&mut kq).contains(&root.join("a.ts")));
        drop(kq);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rename_over_a_hard_linked_file_keeps_the_watch_on_the_new_inode() {
        let root = scratch("hardlink");
        fs::write(root.join("a.ts"), "1").unwrap();
        fs::hard_link(root.join("a.ts"), root.join("keep.ts")).unwrap();
        let mut kq = Kq::open(&filter(&root), 100_000).unwrap();
        let _ = sources(&mut kq);
        fs::write(root.join("t.swap"), "2").unwrap();
        fs::rename(root.join("t.swap"), root.join("a.ts")).unwrap();
        assert!(sources(&mut kq).contains(&root.join("a.ts")));
        // An in-place edit of the new inode shows at the next drain.
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(root.join("a.ts"))
            .unwrap();
        std::io::Write::write_all(&mut file, b"3333").unwrap();
        drop(file);
        assert!(sources(&mut kq).contains(&root.join("a.ts")));
        drop(kq);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_new_dir_reports_its_files_and_a_removed_dir_closes_its_fds() {
        let root = scratch("dirs");
        fs::write(root.join("a.ts"), "1").unwrap();
        let mut kq = Kq::open(&filter(&root), 100_000).unwrap();
        let base = kq.len();
        fs::create_dir_all(root.join("pkg/deep")).unwrap();
        fs::write(root.join("pkg/deep/x.ts"), "x").unwrap();
        let seen = sources(&mut kq);
        assert!(seen.contains(&root.join("pkg/deep/x.ts")), "{seen:?}");
        assert_eq!(kq.len(), base + 3);
        fs::write(root.join("pkg/deep/y.ts"), "y").unwrap();
        assert!(sources(&mut kq).contains(&root.join("pkg/deep/y.ts")));
        fs::remove_dir_all(root.join("pkg")).unwrap();
        let _ = sources(&mut kq);
        assert_eq!(kq.len(), base);
        drop(kq);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_hundred_create_and_delete_turns_leak_no_fd() {
        let root = scratch("leak");
        fs::write(root.join("a.ts"), "1").unwrap();
        let mut kq = Kq::open(&filter(&root), 100_000).unwrap();
        let (base, fds) = (kq.len(), open_fds());
        for n in 0..100 {
            let file = root.join(format!("f{n}.ts"));
            let dir = root.join(format!("d{n}"));
            fs::write(&file, "x").unwrap();
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("in.ts"), "y").unwrap();
            let _ = sources(&mut kq);
            fs::remove_file(&file).unwrap();
            fs::remove_dir_all(&dir).unwrap();
            let _ = sources(&mut kq);
        }
        assert_eq!(kq.len(), base);
        // Other test threads open fds too, so allow a small drift. A leak is
        // at least 100.
        assert!(open_fds() <= fds + 30, "{} -> {}", fds, open_fds());
        drop(kq);
        let _ = fs::remove_dir_all(&root);
    }
}
