//! The `sieve telemetry [action]` subcommand (P1-67), and the local-date
//! helpers the savings code shares.
//!
//! Sieve records no event and sends nothing (P4-35 to P4-38 are
//! deviations). Every action prints one true line and touches no file.

use std::time::{Duration, SystemTime};

use clap::Args;
use sieve_core::product::product;

/// Flags for `sieve telemetry`.
#[derive(Args, Debug)]
pub struct TelemetryArgs {
    /// status (default) | enable | disable | debug
    #[arg(value_name = "action", default_value = "status")]
    pub action: String,
}

/// The local date `YYYY-MM-DD` of a Unix time in seconds, for a UTC
/// offset in seconds.
pub(crate) fn local_date(secs: u64, offset_secs: i64) -> String {
    let local = secs as i64 + offset_secs;
    let (year, month, day) = civil_from_days(local.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

/// Parses a `date +%z` value such as `+0800` or `-0330` into seconds.
pub(crate) fn parse_offset(text: &str) -> Option<i64> {
    let t = text.trim();
    let sign = match t.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits = t.get(1..)?;
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits[..2].parse().ok()?;
    let minutes: i64 = digits[2..].parse().ok()?;
    Some(sign * (hours * 3600 + minutes * 60))
}

/// The local UTC offset in seconds. The workspace has no time crate, so
/// this runs `date +%z` once per process and caches the answer. It gives 0
/// (UTC) when the command fails.
pub(crate) fn local_offset_secs() -> i64 {
    // A fixed test clock also fixes the zone, so a golden holds the same
    // dates on every machine.
    if std::env::var_os("SIEVE_TEST_NOW").is_some() {
        return 0;
    }
    static OFFSET: std::sync::OnceLock<i64> = std::sync::OnceLock::new();
    *OFFSET.get_or_init(|| {
        std::process::Command::new("date")
            .arg("+%z")
            .output()
            .ok()
            .and_then(|o| parse_offset(&String::from_utf8_lossy(&o.stdout)))
            .unwrap_or(0)
    })
}

/// The local date of a Unix time, at the local offset.
pub(crate) fn local_date_at(secs: u64) -> String {
    local_date(secs, local_offset_secs())
}

/// The current Unix time in seconds. The test seam `SIEVE_TEST_NOW` (a Unix
/// time in seconds) replaces the clock. Only tests set it.
pub(crate) fn now_secs() -> u64 {
    if let Some(fixed) = std::env::var("SIEVE_TEST_NOW")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        return fixed;
    }
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

/// The local date of now.
pub(crate) fn today_local() -> String {
    local_date_at(now_secs())
}

/// Howard Hinnant's `civil_from_days`. A copy of the private helper in
/// `sieve_core::lock`; this crate change may not edit that crate.
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
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Runs `sieve telemetry`: checks the action, then prints one line.
pub fn run(args: &TelemetryArgs) -> Result<(), String> {
    let known = ["status", "enable", "disable", "debug"];
    if !known.contains(&args.action.as_str()) {
        return Err(format!(
            "unknown action \"{}\" — expected status, enable, disable, or debug",
            args.action
        ));
    }
    println!(
        "telemetry: off — {} records and sends nothing",
        product().name
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_savings_local_date_uses_the_offset() {
        // 2026-10-04T23:30:00Z.
        let t = 1_791_072_000 + 23 * 3600 + 1800;
        assert_eq!(local_date(t, 0), "2026-10-04");
        assert_eq!(local_date(t, 8 * 3600), "2026-10-05");
        assert_eq!(local_date(t, -24 * 3600), "2026-10-03");
        assert_eq!(parse_offset("+0800\n"), Some(28_800));
        assert_eq!(parse_offset("-0330"), Some(-12_600));
        assert_eq!(parse_offset("bad"), None);
    }

    #[test]
    fn test_p1_67_civil_from_days_matches_known_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_726), (2026, 9, 30));
    }
}
