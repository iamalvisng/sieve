//! The build memory guard: reads the real process memory during a build
//! and stops the build when it passes the ceiling.

use std::fmt;
use std::path::Path;

// Decimal units, so the message and the ceiling agree.
const MB: u64 = 1_000_000;
const DEFAULT_CEILING: u64 = 1_500 * MB;

/// The env var that overrides the build ceiling, in bytes.
pub const CEILING_ENV: &str = "SIEVE_BUILD_CEILING_BYTES";

/// The file in the context dir that records a stopped build.
const MARKER: &str = "build-stopped";

/// Returns the build ceiling in bytes: the env override, else the smaller
/// of 1.5 GB and 20% of physical memory.
pub fn ceiling_bytes() -> u64 {
    ceiling_from(
        std::env::var(CEILING_ENV).ok().as_deref(),
        physical_memory(),
    )
}

/// Picks the ceiling from the env text and the physical memory. A bad env
/// value falls back to the default.
fn ceiling_from(env: Option<&str>, mem: Option<u64>) -> u64 {
    if let Some(n) = env.and_then(|v| v.trim().parse::<u64>().ok()) {
        return n;
    }
    mem.map_or(DEFAULT_CEILING, |m| (m / 5).min(DEFAULT_CEILING))
}

/// Returns the physical memory in bytes. The first call reads it; later
/// calls reuse the value, so a hook call forks no `sysctl` twice.
fn physical_memory() -> Option<u64> {
    static MEM: std::sync::OnceLock<Option<u64>> = std::sync::OnceLock::new();
    *MEM.get_or_init(read_physical_memory)
}

/// Reads the physical memory in bytes, with std only.
fn read_physical_memory() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        return String::from_utf8_lossy(&out.stdout).trim().parse().ok();
    }
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|l| l.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kib * 1024)
}

/// A stopped build: the process memory passed the ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The process memory in bytes when the build stopped.
    pub used: u64,
    /// The ceiling in bytes.
    pub ceiling: u64,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "build stopped: the build used {} MB, and the ceiling is {} MB \
             — build a smaller part with --only-dir, split the workspace, \
             or set {CEILING_ENV}",
            self.used / MB,
            self.ceiling / MB
        )
    }
}

impl std::error::Error for Refused {}

impl From<Refused> for std::io::Error {
    fn from(r: Refused) -> Self {
        std::io::Error::other(r)
    }
}

/// Returns the current resident memory of this process in bytes, or `None`
/// when the system does not say. It is the current value, not the peak,
/// because the daemon lives long.
pub fn current_memory() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::zeroed();
        // SAFETY: `info` is a zeroed buffer the size of `rusage_info_v2`,
        // the struct that flavor 2 fills. The call writes only into it. The
        // value is read only when the call returns 0.
        let rc = unsafe {
            libc::proc_pid_rusage(
                libc::getpid(),
                libc::RUSAGE_INFO_V2,
                info.as_mut_ptr().cast::<libc::rusage_info_t>(),
            )
        };
        // SAFETY: the call returned 0, so it filled the struct.
        (rc == 0).then(|| unsafe { info.assume_init() }.ri_phys_footprint)
    }
    #[cfg(target_os = "linux")]
    {
        let text = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = text.split_whitespace().nth(1)?.parse().ok()?;
        // SAFETY: `sysconf` reads a system constant and takes no pointer.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        u64::try_from(page).ok().map(|p| pages * p)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

/// The memory check of one build. It records the memory the process held
/// at the start, because the daemon already holds an index, and it stops
/// the build when the growth since then passes the ceiling.
pub struct Guard<'a> {
    marker_dir: Option<&'a Path>,
    files: usize,
    ceiling: u64,
    baseline: u64,
    read: fn() -> Option<u64>,
}

impl<'a> Guard<'a> {
    /// Starts a check. `marker_dir` is where a stop writes the marker; a
    /// `None` dir writes nothing. `files` is the claimed file count. `read`
    /// is the memory reader; `None` means [`current_memory`].
    pub fn start(
        marker_dir: Option<&'a Path>,
        files: usize,
        read: Option<fn() -> Option<u64>>,
    ) -> Self {
        let read = read.unwrap_or(current_memory);
        Guard {
            marker_dir,
            files,
            ceiling: ceiling_bytes(),
            baseline: read().unwrap_or(0),
            read,
        }
    }

    /// Stops the build when its memory growth is over the ceiling, and
    /// writes the marker so an automatic refresh does not parse again. A
    /// system that reports no memory never stops a build.
    pub fn check(&self) -> Result<(), Refused> {
        let used = (self.read)().map(|m| m.saturating_sub(self.baseline));
        let result = check_against(used, self.ceiling);
        if let (Err(r), Some(dir)) = (&result, self.marker_dir) {
            // A failed marker write only costs a later retry.
            let _ = std::fs::create_dir_all(dir);
            let _ = std::fs::write(
                dir.join(MARKER),
                format!("{} {} {}\n", r.used, r.ceiling, self.files),
            );
        }
        result
    }
}

fn check_against(used: Option<u64>, ceiling: u64) -> Result<(), Refused> {
    match used {
        Some(used) if used > ceiling => Err(Refused { used, ceiling }),
        _ => Ok(()),
    }
}

/// Removes the marker. A missing marker is fine.
pub fn clear_marker(context_dir: &Path) {
    let _ = std::fs::remove_file(context_dir.join(MARKER));
}

/// For an automatic build: fails with the recorded stop when a marker
/// exists and the ceiling is not higher than the one it records.
pub fn skip_if_stopped(context_dir: &Path) -> Result<(), Refused> {
    stopped_under(context_dir, ceiling_bytes())
}

fn stopped_under(context_dir: &Path, ceiling: u64) -> Result<(), Refused> {
    let Ok(text) = std::fs::read_to_string(context_dir.join(MARKER)) else {
        return Ok(());
    };
    let mut nums = text.split_whitespace().map(|n| n.parse::<u64>().ok());
    if let (Some(Some(used)), Some(Some(marked))) = (nums.next(), nums.next()) {
        if ceiling <= marked {
            return Err(Refused {
                used,
                ceiling: marked,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s0_guard_env_override_bad_value_falls_back() {
        let big = Some(100_000 * MB);
        assert_eq!(ceiling_from(Some("12345"), big), 12345);
        assert_eq!(ceiling_from(Some("junk"), big), DEFAULT_CEILING);
        assert_eq!(ceiling_from(Some("junk"), Some(2_000 * MB)), 400 * MB);
        assert_eq!(ceiling_from(None, None), DEFAULT_CEILING);
    }

    #[test]
    fn the_check_stops_only_over_the_ceiling() {
        assert!(check_against(Some(10), 10).is_ok());
        assert!(check_against(None, 0).is_ok());
        let r = check_against(Some(11), 10).expect_err("over the ceiling");
        assert_eq!((r.used, r.ceiling), (11, 10));
    }

    #[test]
    fn the_process_memory_is_readable() {
        assert!(current_memory().is_some_and(|m| m > 0));
    }

    #[test]
    fn a_marker_holds_until_the_ceiling_rises() {
        let dir = std::env::temp_dir().join(format!("sieve-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(MARKER), "900 500 7\n").expect("write");
        assert_eq!(stopped_under(&dir, 500).expect_err("held").used, 900);
        assert!(stopped_under(&dir, 501).is_ok());
        clear_marker(&dir);
        assert!(stopped_under(&dir, 1).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
