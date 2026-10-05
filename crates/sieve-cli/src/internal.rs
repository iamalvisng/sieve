//! The three hidden subcommands (P1-68): `_update-check`,
//! `_telemetry-flush` and `_brain-refresh [dir]`. They stand for background
//! jobs that need the network: an update check, a telemetry post and a
//! brain pull. Sieve stops before the network step, prints nothing, and
//! exits 0.

use std::path::PathBuf;

use clap::Args;

/// Flags for `sieve _brain-refresh`.
#[derive(Args, Debug)]
pub struct BrainRefreshArgs {
    /// target repo directory
    #[arg(value_name = "dir", default_value = ".")]
    pub dir: PathBuf,
}

/// `_update-check`: would refresh `~/.sieve/update-check.json` from the
/// registry. Sieve never reaches the registry, so Sieve leaves the cache
/// as it is. No nudge: the command is in the `UPKEEP_SKIP` set.
pub fn update_check() -> Result<(), String> {
    Ok(())
}

/// `_telemetry-flush`: would drain the queue and post it. Sieve neither
/// drains nor posts. The nudge prints: the command is not in
/// `UPKEEP_SKIP`.
pub fn telemetry_flush() -> Result<(), String> {
    Ok(())
}

/// `_brain-refresh [dir]`: would re-pull the attached brain's rules.
/// Sieve has no brain link, so Sieve does nothing. No nudge: the command
/// is in `UPKEEP_SKIP`.
pub fn brain_refresh(_args: &BrainRefreshArgs) -> Result<(), String> {
    Ok(())
}
