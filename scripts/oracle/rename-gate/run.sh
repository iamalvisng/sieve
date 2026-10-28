#!/usr/bin/env bash
# Rename gate. Usage: run.sh <repo-dir> [--sample N]
# Runs the cross-file rename gate against one TS or JS repo. Dry runs only.
# Method: docs/design/rename.md, section E. Bash 3.2 compatible (macOS).
set -euo pipefail

SEED=20261026
usage() { echo "usage: $0 <repo-dir> [--sample N]" >&2; exit 1; }
die() { echo "rename-gate: $*" >&2; exit 1; }
step() { printf '\n== %s\n' "$*"; }

REPO_ARG=""
SAMPLE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --sample) [ $# -ge 2 ] || usage; SAMPLE="$2"; shift 2 ;;
    -h|--help) usage ;;
    -*) usage ;;
    *) [ -z "$REPO_ARG" ] || usage; REPO_ARG="$1"; shift ;;
  esac
done
[ -n "$REPO_ARG" ] || usage
case "$SAMPLE" in *[!0-9]*) die "--sample needs a whole number" ;; esac
[ -d "$REPO_ARG" ] || die "not a directory: $REPO_ARG"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
ORACLE_DIR="$(cd "$SCRIPT_DIR/.." && pwd -P)"
SIEVE_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd -P)"
REPO="$(cd "$REPO_ARG" && pwd -P)"
TAG="$(basename "$REPO" | sed 's/[^A-Za-z0-9._]/_/g')"

sha256() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
  else shasum -a 256 "$1" | cut -d' ' -f1; fi
}
sha256_stdin() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum | cut -d' ' -f1
  else shasum -a 256 | cut -d' ' -f1; fi
}

# (1) tools
step "1. Tools"
for t in node npm cargo; do
  command -v "$t" >/dev/null 2>&1 || die "$t is not installed"
done
echo "node  $(node --version)"
echo "npm   $(npm --version)"
echo "cargo $(cargo --version)"
command -v git >/dev/null 2>&1 || die "git is not installed"
IS_GIT=0
if git -C "$REPO" rev-parse --is-inside-work-tree >/dev/null 2>&1; then IS_GIT=1; else echo "note: $REPO is not a git repo; the source-file check is skipped"; fi

# Output folder and scratch dirs
STAMP="$(date +%Y%m%d-%H%M%S)"
OUT="${RENAME_GATE_OUT:-${TMPDIR:-/tmp}/rename-gate-$TAG-$STAMP}"
OUT="${OUT%/}"
mkdir -p "$OUT/cases" "$OUT/oracle" "$OUT/plans" "$OUT/compare"
OUT="$(cd "$OUT" && pwd -P)"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/rename-gate-work.XXXXXX")"
echo "output folder: $OUT"

# (2) npm ci, only when typescript is missing
step "2. TypeScript oracle dependency"
if [ -f "$ORACLE_DIR/node_modules/typescript/package.json" ]; then
  echo "typescript $(node -p "require('$ORACLE_DIR/node_modules/typescript/package.json').version") already installed"
else
  (cd "$ORACLE_DIR" && npm ci) >"$OUT/npm-ci.log" 2>&1 || { tail -20 "$OUT/npm-ci.log" >&2; die "npm ci failed"; }
  echo "npm ci done"
fi
TS_VERSION="$(node -p "require('$ORACLE_DIR/node_modules/typescript/package.json').version")"

# (3) build sieve release from this checkout into a temp target dir
step "3. Build sieve (release)"
TARGET="${RENAME_GATE_TARGET:-$WORK/target}"
(cd "$SIEVE_ROOT" && cargo build --release -p sieve-cli --target-dir "$TARGET") >"$OUT/cargo-build.log" 2>&1 \
  || { tail -30 "$OUT/cargo-build.log" >&2; die "cargo build failed"; }
SIEVE="$TARGET/release/sieve"
[ -x "$SIEVE" ] || die "no binary at $SIEVE"
echo "built: $SIEVE"

# (4) scratch HOME. Never the real one.
step "4. Scratch HOME"
REAL_HOME="$(cd "${HOME:-/nonexistent}" 2>/dev/null && pwd -P || echo "${HOME:-}")"
SCRATCH_HOME="$WORK/home"
mkdir -p "$SCRATCH_HOME"
SCRATCH_REAL="$(cd "$SCRATCH_HOME" && pwd -P)"
if [ -z "$SCRATCH_REAL" ] || [ "$SCRATCH_REAL" = "$REAL_HOME" ] || [ "$SCRATCH_REAL" = "/" ]; then
  die "scratch HOME would be the real HOME ($REAL_HOME); refusing to run"
fi
export HOME="$SCRATCH_REAL"
[ "$(cd "$HOME" && pwd -P)" != "$REAL_HOME" ] || die "HOME is still the real one; refusing to run"
echo "HOME=$HOME (real HOME untouched: $REAL_HOME)"

# Source-file snapshot: status plus a hash of the tracked diff.
snapshot() {
  if [ "$IS_GIT" -eq 1 ]; then
    git -C "$REPO" status --porcelain
    echo "diff-sha256 $(git -C "$REPO" diff | sha256_stdin)"
  fi
}
snapshot >"$OUT/git-status-before-build.txt"
REPO_HEAD="n/a"; REPO_DIRTY="n/a"
if [ "$IS_GIT" -eq 1 ]; then
  REPO_HEAD="$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo none)"
  if [ -n "$(git -C "$REPO" status --porcelain)" ]; then REPO_DIRTY="dirty before run"; else REPO_DIRTY="clean"; fi
fi

# (5) sieve build in the repo
step "5. sieve build"
echo "The index goes to $REPO/sieve (git-ignored). The build may add a .gitignore entry."
T0=$(date +%s)
(cd "$REPO" && "$SIEVE" build) >"$OUT/sieve-build.log" 2>&1 || { tail -20 "$OUT/sieve-build.log" >&2; die "sieve build failed"; }
echo "sieve build: $(( $(date +%s) - T0 )) s"
[ -f "$REPO/sieve/.graph/wiring.json" ] || die "no $REPO/sieve/.graph/wiring.json after the build"
SIEVE_VERSION="$("$SIEVE" --version 2>/dev/null | head -1 || echo unknown)"
snapshot >"$OUT/git-status-after-build.txt"

# (6) case lists
step "6. Case lists (seed $SEED)"
GEN_ARGS=("$REPO" "$TAG" "$OUT/cases")
[ -z "$SAMPLE" ] || GEN_ARGS+=("$SAMPLE")
node "$SCRIPT_DIR/gen-plain-report.mjs" "${GEN_ARGS[@]}" | tee "$OUT/cases/gen-plain-report.json"
node "$SCRIPT_DIR/ts-collision.mjs" "$REPO" "$TAG" "$OUT/cases/$TAG-collision.jsonl" | tee "$OUT/cases/ts-collision.json"
cat "$OUT/cases/$TAG-plain.jsonl" "$OUT/cases/$TAG-collision.jsonl" "$OUT/cases/$TAG-report.jsonl" >"$OUT/cases/$TAG-cases.jsonl"
for f in "$TAG-plain" "$TAG-collision" "$TAG-report" "$TAG-cases"; do
  echo "$(wc -l <"$OUT/cases/$f.jsonl" | tr -d ' ') cases  $f.jsonl"
done

# (7) freeze: hash now, check again after the measurement
step "7. Freeze the case files"
: >"$OUT/cases/SHA256SUMS"
for f in "$TAG-plain" "$TAG-collision" "$TAG-report" "$TAG-cases"; do
  H="$(sha256 "$OUT/cases/$f.jsonl")"
  echo "$H  $f.jsonl" | tee -a "$OUT/cases/SHA256SUMS"
  chmod a-w "$OUT/cases/$f.jsonl"
done
CASES_SHA="$(sha256 "$OUT/cases/$TAG-cases.jsonl")"

# (8) TS oracle
step "8. TypeScript oracle"
T0=$(date +%s)
node "$ORACLE_DIR/ts-rename.mjs" "$REPO" "$OUT/cases/$TAG-cases.jsonl" >"$OUT/oracle/$TAG-oracle.jsonl" 2>"$OUT/oracle/$TAG-oracle.err" \
  || { tail -20 "$OUT/oracle/$TAG-oracle.err" >&2; die "ts-rename.mjs failed"; }
echo "oracle: $(( $(date +%s) - T0 )) s, $(wc -l <"$OUT/oracle/$TAG-oracle.jsonl" | tr -d ' ') rows"

# (9) sieve rename dry runs. run-plans.mjs never passes --write.
step "9. sieve rename dry runs"
T0=$(date +%s)
node "$SCRIPT_DIR/run-plans.mjs" "$SIEVE" "$REPO" "$OUT/cases/$TAG-cases.jsonl" "$OUT/plans/$TAG.jsonl"
echo "dry runs: $(( $(date +%s) - T0 )) s"

# (10) source files unchanged
step "10. Source files unchanged?"
snapshot >"$OUT/git-status-after-plans.txt"
SRC_OK="yes"
if [ "$IS_GIT" -eq 1 ]; then
  # The index build may add .gitignore and .ignore. Show that as a note.
  # The test is strict: the dry runs must change nothing after the build.
  if ! diff <(grep -v ' sieve/' "$OUT/git-status-before-build.txt") <(grep -v ' sieve/' "$OUT/git-status-after-build.txt") >"$OUT/git-status-build.diff"; then
    echo "note: the index build changed these lines of git status (index files, not source):"
    cat "$OUT/git-status-build.diff"
  fi
  if ! cmp -s "$OUT/git-status-after-build.txt" "$OUT/git-status-after-plans.txt"; then
    SRC_OK="NO: the dry runs changed the repo"
    diff "$OUT/git-status-after-build.txt" "$OUT/git-status-after-plans.txt" || true
  fi
  echo "source files unchanged by the dry runs: $SRC_OK"
else
  SRC_OK="not checked (no git)"; echo "$SRC_OK"
fi
# Frozen inputs must not have changed while measuring.
FROZEN_OK="yes"
for f in "$TAG-plain" "$TAG-collision" "$TAG-report" "$TAG-cases"; do
  [ "$(sha256 "$OUT/cases/$f.jsonl")" = "$(grep "  $f.jsonl\$" "$OUT/cases/SHA256SUMS" | cut -d' ' -f1)" ] || FROZEN_OK="NO ($f changed)"
done

# (11) compare and verdict
step "11. Compare and verdict"
node "$SCRIPT_DIR/verdict.mjs" "$OUT/oracle/$TAG-oracle.jsonl" "$OUT/plans/$TAG.jsonl" "$OUT/compare" | tee "$OUT/verdict.txt"

# (12) summary block
step "12. Summary"
{
  echo "----- RENAME GATE SUMMARY (paste this back) -----"
  echo "repo: $TAG  head: $REPO_HEAD  ($REPO_DIRTY)"
  echo "sieve: $SIEVE_VERSION  sieve checkout: $(git -C "$SIEVE_ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "node $(node --version), typescript $TS_VERSION, seed $SEED${SAMPLE:+, plain sample $SAMPLE}"
  echo "cases sha256: $CASES_SHA"
  echo "frozen cases unchanged: $FROZEN_OK"
  [ ! -s "$OUT/git-status-build.diff" ] || echo "index build also touched (not source): $(grep "^> " "$OUT/git-status-build.diff" | grep -v diff-sha256 | tr "\n" ";" | cut -c1-200)"
  echo "source files unchanged by the dry runs: $SRC_OK"
  cat "$OUT/verdict.txt"
  echo "output folder: $OUT"
  echo "----- END SUMMARY -----"
} | tee "$OUT/summary.txt"

rm -rf "$WORK/home"
[ -n "${RENAME_GATE_TARGET:-}" ] || rm -rf "$WORK"
echo
echo "All files: $OUT"
