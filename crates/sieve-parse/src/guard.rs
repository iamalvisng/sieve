//! The build memory guard (S0, B0): projects the build peak from the file
//! count and refuses a build that would pass the ceiling.

use std::fmt;

// Decimal units: the M1 fit is in MB and KB, not MiB and KiB.
const MB: u64 = 1_000_000;
const KB: u64 = 1_000;
const DEFAULT_CEILING: u64 = 1_500 * MB;

/// The env var that overrides the build ceiling, in bytes.
pub const CEILING_ENV: &str = "SIEVE_BUILD_CEILING_BYTES";

/// The margin on the cold fit, in percent. The bare fit under-shoots every
/// M1 point.
const FIT_MARGIN_PERCENT: u64 = 135;

/// The warm factor in percent. S2 made the warm path no larger than the
/// cold path, so the factor is 100. Change it here after a new measure.
const WARM_FACTOR_PERCENT: u64 = 100;

/// Projects the build peak in bytes: (64 MB plus 220 KB per file) times
/// 1.35, times the warm factor on the warm path. The units are decimal: 1 MB
/// is 1,000,000 bytes.
///
/// The M1 points, projection against measure (MB): 880 files cold 348
/// against 293; 2,273 files cold 761 against 750; 4,432 files cold 1,403
/// against 1,059. Before S2 the warm measures were 1,503 MB and 1,635 MB, and
/// the factor was 1.5. After S2 (2026-10-09), on 4,454 files, the warm peak
/// is 902 MB against a cold peak of 941 MB, so the factor is 1.0.
pub fn projected_peak_bytes(files: usize, warm: bool) -> u64 {
    let cold = (64 * MB + 220 * KB * files as u64) * FIT_MARGIN_PERCENT / 100;
    if warm {
        cold * WARM_FACTOR_PERCENT / 100
    } else {
        cold
    }
}

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

/// A refused build: the projected peak is over the ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// The projected peak in bytes.
    pub projected: u64,
    /// The ceiling in bytes.
    pub ceiling: u64,
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "build refused: the projected peak memory is {} MB and the ceiling is {} MB \
             — build a smaller part with --only-dir, split the workspace (P2-39), \
             or set {CEILING_ENV}",
            self.projected / MB,
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

/// Checks the projected peak for `files` against the ceiling.
pub fn check(files: usize, warm: bool) -> Result<(), Refused> {
    check_against(files, warm, ceiling_bytes())
}

/// Checks the projected peak for `files` against a given `ceiling`. This is
/// the test seam: it reads no env var.
pub(crate) fn check_against(files: usize, warm: bool, ceiling: u64) -> Result<(), Refused> {
    let projected = projected_peak_bytes(files, warm);
    if projected > ceiling {
        return Err(Refused { projected, ceiling });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s0_guard_projection_numbers() {
        assert_eq!(projected_peak_bytes(0, false), 86_400_000);
        assert_eq!(projected_peak_bytes(1000, false), 383_400_000);
        assert_eq!(projected_peak_bytes(1000, true), 383_400_000);
    }

    #[test]
    fn test_s0_guard_env_override_bad_value_falls_back() {
        let big = Some(100_000 * MB);
        assert_eq!(ceiling_from(Some("12345"), big), 12345);
        assert_eq!(ceiling_from(Some("junk"), big), DEFAULT_CEILING);
        assert_eq!(ceiling_from(Some("junk"), Some(2_000 * MB)), 400 * MB);
        assert_eq!(ceiling_from(None, None), DEFAULT_CEILING);
    }
}
