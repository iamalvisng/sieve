#!/usr/bin/env bash
# The UTF-8 read parity case (P3-26). Run it from the root of an `edges`
# fixture copy. `refresh.sh` and crates/sieve-cli/tests/utf8_read_parity.rs
# both run this script, so the bytes live in one place.
#
#   setup.sh seed    writes two TypeScript files that are not valid UTF-8:
#                    a Latin-1 0xE9 in a comment, in a string, on a
#                    call-site line, and in a function name, plus a lead
#                    byte 0xC3 cut off by the end of a line.
#   setup.sh change  appends one function with a 0xE9 byte to ts/latin.ts
#                    (the DV7 blast case).
#
# Sieve decodes each file with `readFileSync(path, "utf8")`, so every
# invalid byte becomes U+FFFD and the file still gets its symbols.
set -euo pipefail
case "${1:-}" in
  seed)
    printf '// caf\351 comment\nexport function latinString(): string {\n  return "caf\351";\n}\nexport function latinCaller(): number {\n  return latinString().length; // caf\351\n}\nexport const tail = 1; // \303\n' >ts/latin.ts
    printf 'export function caf\351Name(): number {\n  return 1;\n}\n' >ts/latin-name.ts
    ;;
  change)
    printf '\nexport function latin(): string { return "caf\351"; }\n' >>ts/latin.ts
    ;;
  *)
    echo "usage: setup.sh seed|change" >&2
    exit 2
    ;;
esac
