//! Extractor stamp: a short hash that identifies the extractor's version.
//!
//! The stamp hashes the crate version and the grammar crate versions,
//! then truncates to 16 hex characters.
//!
//! ponytail: the stamp misses a local edit to the extractor code within
//! one crate version. Upgrade: a build script that hashes every file
//! under `crates/sieve-parse/src/` when the extraction cache (P2-35)
//! lands.

use sha2::{Digest, Sha256};
use std::sync::OnceLock;

/// The `tree-sitter` grammar crate versions.
/// A version bump in `Cargo.toml` must update these constants too.
const TREE_SITTER_VERSION: &str = "0.23";
const TREE_SITTER_TYPESCRIPT_VERSION: &str = "0.23";
const TREE_SITTER_PYTHON_VERSION: &str = "0.23";

static STAMP: OnceLock<String> = OnceLock::new();

/// Returns the extractor stamp: 16 lowercase hex characters that identify
/// the extractor's package version and grammar crate versions.
pub fn extractor_stamp() -> String {
    STAMP
        .get_or_init(|| {
            let mut hasher = Sha256::new();
            hasher.update(env!("CARGO_PKG_VERSION").as_bytes());
            hasher.update(b"tree-sitter");
            hasher.update(TREE_SITTER_VERSION.as_bytes());
            hasher.update(b"tree-sitter-typescript");
            hasher.update(TREE_SITTER_TYPESCRIPT_VERSION.as_bytes());
            hasher.update(b"tree-sitter-python");
            hasher.update(TREE_SITTER_PYTHON_VERSION.as_bytes());
            let digest = hasher.finalize();
            let hex = digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            hex[..16].to_string()
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::extractor_stamp;

    #[test]
    fn stamp_is_16_lowercase_hex_chars() {
        let stamp = extractor_stamp();
        assert_eq!(stamp.len(), 16);
        assert!(stamp
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }

    #[test]
    fn stamp_is_stable_across_calls() {
        assert_eq!(extractor_stamp(), extractor_stamp());
    }
}
