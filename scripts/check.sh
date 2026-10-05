#!/usr/bin/env bash
set -euo pipefail

if command -v node >/dev/null; then node scripts/gen-banner.mjs --check; else echo "skip: node not found, banner check not run"; fi
# The export scripts stay in the private repo. Run each one only when it exists.
if [ -f scripts/test-export.sh ]; then scripts/test-export.sh; fi
if command -v node >/dev/null; then node --check crates/sieve-cli/assets/viewer/app.js; else echo "skip: node not found, viewer syntax check not run"; fi
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
