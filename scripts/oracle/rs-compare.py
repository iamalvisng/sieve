#!/usr/bin/env python3
"""Compare Rust oracle rows with Sieve Extracted call edges.
Usage: python3 -I rs-compare.py rows.jsonl <repo>/sieve/.graph/wiring.json [repo]

An edge is right if the oracle has a row with the same src file, a row line
inside the span of the Sieve source node, and a dst span (same dst file) that
contains the start line of the Sieve target node. Same-file edges count.
An edge whose only oracle rows are ambiguous counts as unverified.
Prints: Extracted edges, matched, wrong (first 20), wrong by kind, and the
oracle coverage per shape (rows matched by some edge).
"""
import collections, json, os, re, sys

STD = set("new get len push from default fmt clone into as_str iter next map insert remove contains is_empty "
          "to_string unwrap take drop eq cmp hash write read open close build run set add".split())


def span(n):
    m = re.fullmatch(r"L(\d+)-L(\d+)", n.get("span") or "")
    return (int(m[1]), int(m[2])) if m else None


def main():
    rows_path, graph_path = sys.argv[1:3]
    repo = sys.argv[3] if len(sys.argv) > 3 else os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(graph_path))))
    allrows = [json.loads(l) for l in open(rows_path) if l.strip()]
    docs = {r["src"] for r in allrows if r["shape"] == "doc"}
    defl = collections.defaultdict(list)  # fn definition lines per file
    for r in allrows:
        if r["shape"] == "def":
            defl[r["src"]].append(r["line"])
    rows = [r for r in allrows if r["shape"] not in ("doc", "def")]
    have_defs = bool(docs)
    g = json.load(open(graph_path))
    node = {n["id"]: n for n in g["nodes"]}
    by_src = collections.defaultdict(list)
    for r in rows:
        by_src[r["src"]].append(r)
    lines_cache = {}

    def lines(p):
        if p not in lines_cache:
            try:
                lines_cache[p] = open(os.path.join(repo, p), encoding="utf-8").read().split("\n")
            except OSError:
                lines_cache[p] = []
        return lines_cache[p]

    def kind(t, ts):
        ls = lines(t["path"])
        if ts[0] - 1 < len(ls):
            ind = len(ls[ts[0] - 1]) - len(ls[ts[0] - 1].lstrip())
            for k in range(ts[0] - 2, -1, -1):
                s = ls[k]
                if s.strip() and len(s) - len(s.lstrip()) < ind:
                    if re.match(r"\s*(pub(\([^)]*\))?\s+)?(unsafe\s+)?(impl|trait)\b", s):
                        return "impl method target"
                    ind = len(s) - len(s.lstrip())
                    if ind == 0:
                        break
        return "std-like name" if t["name"].split(".")[-1] in STD else "other"

    edges = [e for e in g["edges"] if e["relation"] == "calls" and e["confidence"] == "extracted"
             and node.get(e["source"], {}).get("path", "").endswith(".rs")
             and node.get(e["target"], {}).get("path", "").endswith(".rs")]
    covered, wrong, unver = set(), [], []
    for e in edges:
        s, t = node[e["source"]], node[e["target"]]
        ss, ts = span(s), span(t)
        hit, amb, trait = False, False, False
        tn = t["name"].split(".")[-1]
        for i, r in enumerate(by_src[s["path"]]):
            if not (ss and ss[0] <= r["line"] <= ss[1]):
                continue
            if r["dst"] == t["path"] and ts and r["dst_start"] <= ts[0] <= r["dst_end"]:
                covered.add(id(r))
                if r.get("ambiguous"): amb = True
                elif r.get("owner") == "trait": trait = True
                else: hit = True
            elif r.get("ambiguous") and r["name"] == tn:
                amb = True
        if trait:
            wrong.append(("dispatched", e["source"], e["target"]))
        elif not hit and amb:
            unver.append(("ambiguous", e))  # oracle rows for this edge are all ambiguous
        elif not hit and have_defs and s["path"] not in docs:
            unver.append(("no document", e))
        elif not hit and have_defs and ss and not any(ss[0] <= l <= ss[1] for l in defl[s["path"]]):
            unver.append(("cfg-off caller", e))
        elif not hit:
            wrong.append((kind(t, ts) if ts else "other", e["source"], e["target"]))
    print(f"Extracted edges: {len(edges)}")
    print(f"matched: {len(edges) - len(wrong) - len(unver)}")
    print(f"unverified: {len(unver)}")
    for k, v in collections.Counter(u[0] for u in unver).most_common():
        print(f"  {k}: {v}")
    print(f"wrong: {len(wrong)}")
    print(f"dispatched (trait-owned match, counted in wrong): {sum(w[0] == 'dispatched' for w in wrong)}")
    for k, v in collections.Counter(w[0] for w in wrong).most_common():
        print(f"  {k}: {v}")
    for w in wrong[:20]:
        print(f"  [{w[0]}] {w[1]} -> {w[2]}")
    print(f"oracle rows: {len(rows)}; per shape: rows, matched by an edge")
    shapes = sorted({r["shape"] for r in rows})
    for sh in shapes:
        rs = [r for r in rows if r["shape"] == sh]
        print(f"  {sh}: {len(rs)}, {sum(id(r) in covered for r in rs)}")
    print("self_method matched by owner:")
    for ow in sorted({r["owner"] for r in rows if r["shape"] == "self_method"}):
        rs = [r for r in rows if r["shape"] == "self_method" and r["owner"] == ow]
        print(f"  {ow}: {len(rs)}, {sum(id(r) in covered for r in rs)}")


main()
