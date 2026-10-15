#!/bin/bash
# Self-check for the Kotlin oracle. Runs kt-calls.sh on Fx.kt and asserts known rows.
# Usage: scripts/oracle/kt-fixture/check.sh   (needs the pinned Kotlin and JDK; takes about 20 s)
set -eu
HERE="$(cd "$(dirname "$0")" && pwd)"
T="$(mktemp -d)"
cd "$HERE"
bash "$HERE/../kt-calls.sh" "$T/rows.jsonl" -d "$T/out" Fx.kt > "$T/log" 2>&1 || true
grep -q "kotlinc exit: 0" "$T/log" || { cat "$T/log"; echo "FAIL: kotlinc did not exit 0"; exit 1; }
python3 -I - "$T/rows.jsonl" <<'PY'
import json, sys
rows = [json.loads(l) for l in open(sys.argv[1])]
calls = [r for r in rows if "kind" not in r]
decls = {(r["dkind"], r["name"], tuple(r["owner"]), r["start"]) for r in rows if r.get("kind") == "decl"}
facts = {(r["sub"], r["base"]) for r in rows if r.get("kind") == "ovrfact"}

DECLS = [
    ("class", "Base", (), 4),                  # the start skips KDoc and keeps the annotation
    ("interface", "Shape", (), 10),
    ("function", "area", ("Shape",), 11),
    ("constructor", "<init>", ("Sq",), 16),
    ("function", "inner", ("Use", "run"), 40),
    ("enum", "Color", (), 47),
    ("object", "Reg", (), 49),
    ("property", "size", ("Reg",), 50),
    ("companion", "Companion", ("Holder",), 54),
    ("function", "make", ("Holder", "Companion"), 55),
]

def row(line, name):
    m = [r for r in calls if r["line"] == line and r["name"] == name]
    assert len(m) == 1, (line, name, m)
    return m[0]

def check(line, name, dst_start, **flags):
    r = row(line, name)
    assert r["dst_start"] == dst_start, (line, name, r)
    for k, v in flags.items():
        assert bool(r.get(k)) == v, (line, name, k, r)

check(30, "greet", 6, virt=True, ovd=False)       # b.greet: Base.greet, open
check(31, "plain", 7, virt=False)                 # b.plain
check(32, "area", 11, abs=True, iface=True)       # sh.area: interface member
check(34, "top", 24, top=True)                    # top-level function
check(35, "shout", 26, ext=True)                  # extension function
check(36, "f", 0, fnval=True, oor=True)           # call of a function value
check(37, "Sq", 15, ctor=True)                    # primary constructor maps to the class
check(38, "Sq", 15, ctor=True)                    # secondary constructor maps to the class
check(41, "inner", 40, local=True)                # local function
check(20, "super", 4)                             # super(...) call maps to class Base
assert ("Fx.kt:21", "Fx.kt:6") in facts, facts    # Sub.greet overrides Base.greet
assert ("Fx.kt:17", "Fx.kt:11") in facts, facts   # Sq.area overrides Shape.area
for d in DECLS:
    assert d in decls, (d, sorted(decls))
print("kt-fixture: ok")
PY
find "$T" -delete
