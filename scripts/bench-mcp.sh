#!/bin/bash
# usage: scripts/bench-mcp.sh <sieve-binary> <repo> [symbol] [query]
# Prints the warm MCP median time of a query, a find and a callers call.
# The script runs the set twice. It builds the index only if it is missing.
# The symbol is the find literal and the callers name. Default: main.
set -e
here=$(cd "$(dirname "$0")" && pwd)
"$1" --version
uptime
if [ ! -d "$2/sieve" ]; then
  (cd "$2" && "$1" build >/dev/null 2>&1 || true)
fi
for i in 1 2; do
  echo "-- run $i"
  python3 -I "$here/bench-mcp.py" "$1" "$2" "${3:-main}" "${4:-where is the entry point}"
done
