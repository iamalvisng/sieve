#!/usr/bin/env bash
set -euo pipefail

if command -v node >/dev/null; then node scripts/gen-banner.mjs --check; else echo "skip: node not found, banner check not run"; fi
# The export scripts stay in the private repo. Run each one only when it exists.
if [ -f scripts/test-export.sh ]; then scripts/test-export.sh; fi
if command -v node >/dev/null; then node --check crates/sieve-cli/assets/viewer/app.js; else echo "skip: node not found, viewer syntax check not run"; fi
if command -v claude >/dev/null; then
  pane_out=$(claude plugin validate crates/sieve-cli/assets/claude-mod/sieve-pane 2>&1 || true)
  echo "$pane_out"
  if echo "$pane_out" | grep -qE 'Validation failed|✘ Found'; then echo "pane mod validation failed"; exit 1; fi
else
  echo "skip: claude not found, pane mod validate not run"
fi
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
