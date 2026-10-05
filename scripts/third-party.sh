#!/usr/bin/env bash
# Refresh the crate section of THIRD_PARTY.md. Needs cargo-about.
# The text above the "## Rust crate dependencies" line stays as it is.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp="$(mktemp)"
trap 'rm -f "$tmp"' EXIT
sed '/^## Rust crate dependencies/,$d' THIRD_PARTY.md > "$tmp"
cargo about generate --locked about.hbs >> "$tmp"
mv "$tmp" THIRD_PARTY.md
