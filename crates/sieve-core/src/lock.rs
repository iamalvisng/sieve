//! The build lock. Sieve serializes a graph rebuild through one lock file
//! per context directory, so two processes never write the graph at once.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

/// How long a caller waits for a held lock before it gives up.
pub const LOCK_WAIT_MS: u64 = 2000;
/// How often a waiting caller retries the lock.
pub const LOCK_POLL_MS: u64 = 50;
/// How old a lock file may be before a caller treats it as abandoned.
pub const LOCK_STALE_MS: u64 = 300_000;

/// Builds the lock file path for one context directory.
pub fn lock_path(context_dir: &Path) -> PathBuf {
    context_dir.join(".cache").join(".sync.lock")
}

/// Tries once to take the lock, without waiting.
///
/// This creates the `.cache` directory, then creates the lock file with
/// `create_new`. When the file already exists and its age is over
/// [`LOCK_STALE_MS`], this removes it and retries once. Returns `false`
/// when a live lock still holds the file.
pub fn try_acquire(context_dir: &Path) -> io::Result<bool> {
    let dir = context_dir.join(".cache");
    fs::create_dir_all(&dir)?;
    let path = lock_path(context_dir);

    match create_lock_file(&path) {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
            if lock_is_stale(&path)? {
                let _ = fs::remove_file(&path);
                match create_lock_file(&path) {
                    Ok(()) => Ok(true),
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                    Err(err) => Err(err),
                }
            } else {
                Ok(false)
            }
        }
        Err(err) => Err(err),
    }
}

fn create_lock_file(path: &Path) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let body = format!(
        "{{\"pid\":{},\"at\":\"{}\"}}",
        std::process::id(),
        rfc3339_now()
    );
    file.write_all(body.as_bytes())
}

fn lock_is_stale(path: &Path) -> io::Result<bool> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    let modified = metadata.modified()?;
    let age_ms = SystemTime::now()
        .duration_since(modified)
        .unwrap_or(Duration::ZERO)
        .as_millis() as u64;
    Ok(age_ms >= LOCK_STALE_MS)
}

/// Waits for the lock, trying then sleeping, until [`LOCK_WAIT_MS`] pass.
///
/// A caller on the query path uses this so a query answers from the
/// on-disk graph, instead of blocking forever, when a rebuild is slow.
/// Sieve does not install a signal handler here: the daemon process that
/// wraps this call waits for the daemon's own work to finish before it
/// exits, so the lock file is released through the normal drop path.
pub fn wait_for_lock(context_dir: &Path) -> io::Result<bool> {
    let deadline = Instant::now() + Duration::from_millis(LOCK_WAIT_MS);
    loop {
        if try_acquire(context_dir)? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(LOCK_POLL_MS));
    }
}

/// Releases the lock. This ignores a failure.
pub fn release(context_dir: &Path) {
    let _ = fs::remove_file(lock_path(context_dir));
}

/// A held lock that releases itself when dropped.
pub struct LockGuard {
    context_dir: PathBuf,
}

impl LockGuard {
    /// Tries once to take the lock and wraps it in a guard on success.
    pub fn acquire(context_dir: &Path) -> io::Result<Option<LockGuard>> {
        if try_acquire(context_dir)? {
            Ok(Some(LockGuard {
                context_dir: context_dir.to_path_buf(),
            }))
        } else {
            Ok(None)
        }
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        release(&self.context_dir);
    }
}

/// Formats the current time as an RFC 3339 UTC timestamp, by hand.
fn rfc3339_now() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO);
    let secs = now.as_secs();
    let millis = now.subsec_millis();

    let days = secs / 86_400;
    let day_secs = secs % 86_400;
    let hour = day_secs / 3600;
    let minute = (day_secs % 3600) / 60;
    let second = day_secs % 60;

    let (year, month, day) = civil_from_days(days as i64);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

/// Converts a day count since the Unix epoch to a civil `(year, month,
/// day)`, using Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let pid = std::process::id();
            let nanos = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("system clock before epoch")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("sieve-lock-{label}-{pid}-{n}-{nanos}"));
            fs::create_dir_all(&path).expect("create temp dir");
            TempDir { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn a_second_acquire_fails_while_the_first_holds_the_lock() {
        let dir = TempDir::new("double");
        assert!(try_acquire(&dir.path).expect("first acquire"));
        assert!(!try_acquire(&dir.path).expect("second acquire"));
    }

    #[test]
    fn release_then_acquire_succeeds() {
        let dir = TempDir::new("release");
        assert!(try_acquire(&dir.path).expect("first acquire"));
        release(&dir.path);
        assert!(try_acquire(&dir.path).expect("acquire after release"));
    }

    #[test]
    fn a_stale_lock_is_taken_over() {
        let dir = TempDir::new("stale");
        assert!(try_acquire(&dir.path).expect("first acquire"));

        let path = lock_path(&dir.path);
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open lock file");
        let stale = SystemTime::now() - Duration::from_secs(600);
        let _ = file.set_modified(stale);

        assert!(try_acquire(&dir.path).expect("acquire over a stale lock"));
    }

    #[test]
    fn wait_for_lock_gives_up_after_about_two_seconds() {
        let dir = TempDir::new("wait");
        assert!(try_acquire(&dir.path).expect("first acquire"));

        let start = Instant::now();
        let got = wait_for_lock(&dir.path).expect("wait for lock");
        let elapsed = start.elapsed();

        assert!(!got);
        assert!(elapsed >= Duration::from_millis(LOCK_WAIT_MS));
        assert!(elapsed < Duration::from_millis(4000));
    }

    #[test]
    fn a_lock_guard_releases_on_drop() {
        let dir = TempDir::new("guard");
        {
            let guard = LockGuard::acquire(&dir.path).expect("acquire guard");
            assert!(guard.is_some());
            assert!(!try_acquire(&dir.path).expect("second acquire while held"));
        }
        assert!(try_acquire(&dir.path).expect("acquire after guard drop"));
    }
}
