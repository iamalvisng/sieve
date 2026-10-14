#!/usr/bin/env python3
"""Oracle for Go call links. Usage: python3 -I go-calls.py <repo> <index.scip> [rows.jsonl]

Reads an index.scip made by scip-go (run it once per go.mod, with the pinned Go
and scip-go) and writes one JSON row per
call reference:
  {src, line, shape, dst, name, dst_start, dst_end}
A call reference is a reference occurrence of a func or method symbol whose next
source char (after blanks) is "(" or "[". Lines are 1-based. Paths are relative
to <repo>. dst is the in-repo Definition occurrence; dst_start and dst_end are
its enclosing range (the whole func). A dst outside the repo (module cache,
paths that start with "..") is dropped (T2).
Fixes: dedupe by (file, range, symbol) for the `p` and `p [p.test]` twins (T1). A symbol with several in-repo definitions (build-tag twins) resolves to
the definitions in the reference's own directory; if not unique, one row per
candidate is written with "ambiguous": true.
Shapes: bare (f(x)), qual (pkg.F(x)), method (x.M(y)), generic (next char "[").
"""
import json, os, re, sys


def varint(b, i):
    r = s = 0
    while True:
        c = b[i]; i += 1
        r |= (c & 0x7F) << s; s += 7
        if c < 0x80:
            return r, i


def fields(b):
    i, n = 0, len(b)
    while i < n:
        k, i = varint(b, i)
        f, w = k >> 3, k & 7
        if w == 0:
            v, i = varint(b, i)
        elif w == 2:
            l, i = varint(b, i); v = b[i:i + l]; i += l
        elif w == 1:
            v = b[i:i + 8]; i += 8
        elif w == 5:
            v = b[i:i + 4]; i += 4
        else:
            raise ValueError("wire type %d" % w)
        yield f, w, v


def packed(b):
    out, i = [], 0
    while i < len(b):
        v, i = varint(b, i); out.append(v)
    return out


def rng(r):
    return (r[0], r[1], r[0], r[2]) if len(r) == 3 else tuple(r[:4])


def read_scip(path):
    docs = []
    for f, _, v in fields(open(path, "rb").read()):
        if f != 2:
            continue
        rel, occs = None, []
        for g, _, w in fields(v):
            if g == 1:
                rel = w.decode()
            elif g == 2:
                o = {"role": 0, "enc": None, "sym": ""}
                for h, wt, x in fields(w):
                    if h == 1: o["range"] = rng(packed(x))
                    elif h == 2: o["sym"] = x.decode()
                    elif h == 3: o["role"] = x
                    elif h == 7: o["enc"] = rng(packed(x))
                occs.append(o)
        docs.append((rel, occs))
    return docs


# a func or method symbol ends with `Name().` or `Type#Name().`
FN = re.compile(r"/(?:[^/`]*#)?([^#/`]+)\(\)\.$")


def main():
    repo = os.path.realpath(sys.argv[1])
    docs = read_scip(sys.argv[2])
    out = open(sys.argv[3], "w") if len(sys.argv) > 3 else sys.stdout
    defs = {}
    for rel, occs in docs:
        if rel.startswith("..") or os.path.isabs(rel):
            continue  # T2: module cache
        for o in occs:
            if o["role"] & 1 and "range" in o and FN.search(o["sym"]):
                s = o["enc"] or o["range"]
                d = (rel, s[0] + 1, s[2] + 1)
                if d not in defs.setdefault(o["sym"], []):
                    defs[o["sym"]].append(d)  # T1 twin guard
    rows, seen = [], set()
    for rel, occs in docs:
        if rel.startswith("..") or os.path.isabs(rel):
            continue
        try:
            lines = open(os.path.join(repo, rel), encoding="utf-8").read().split("\n")
        except OSError:
            continue
        for o in occs:
            sym = o["sym"]
            if o["role"] & 1 or sym not in defs or "range" not in o:
                continue
            l0, c0, l1, c1 = o["range"]
            if l0 != l1 or l0 >= len(lines):
                continue
            key = (rel, o["range"], sym)
            if key in seen:
                continue  # T1: `p` and `p [p.test]` twins
            seen.add(key)
            line = lines[l0]
            rest = line[c1:].lstrip()
            if not rest or rest[0] not in "([":
                continue  # `f (` is kept, `f/*c*/(` is a known miss
            before = line[:c0].rstrip()
            ismeth = "#" in sym.rsplit("/", 1)[-1]
            if rest[0] == "[":
                shape = "generic"
            elif before.endswith("."):
                shape = "method" if ismeth else "qual"
            else:
                shape = "bare"
            ds = defs[sym]
            if len(ds) > 1:
                same = [x for x in ds if os.path.dirname(x[0]) == os.path.dirname(rel)]
                ds = same if same else ds
            amb = len(ds) > 1
            name = FN.search(sym).group(1)
            for d in ds:
                row = {"src": rel, "line": l0 + 1, "shape": shape, "dst": d[0], "name": name,
                       "dst_start": d[1], "dst_end": d[2]}
                if amb:
                    row["ambiguous"] = True
                rows.append(row)
    rows.sort(key=lambda r: (r["src"], r["line"], r["dst"], r["name"], r["dst_start"]))
    for r in rows:
        out.write(json.dumps(r) + "\n")


if __name__ == "__main__":
    main()
