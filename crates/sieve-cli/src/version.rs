//! The `sieve version` subcommand (P1-37).
//!
//! `version` prints one line: the product name and the crate version.
//! Sieve runs no update check and prints no update nudge. It never calls
//! the npm registry.

use clap::Args;
use sieve_core::product::product;

/// The version the product reports: the crate version.
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Flags for `sieve version`. No arguments, no flags but `-h`.
#[derive(Args, Debug)]
pub struct VersionArgs;

/// Runs `sieve version`: prints the one-line report to stdout. No nudge,
/// no refresh, no telemetry preamble (P1-38).
pub fn run(_args: &VersionArgs) -> Result<(), String> {
    println!("{} {}", product().name, current_version());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_sieve_prints_its_own_version() {
        assert_eq!(current_version(), env!("CARGO_PKG_VERSION"));
    }
}
