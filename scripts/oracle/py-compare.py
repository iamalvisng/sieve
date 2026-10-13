#!/usr/bin/env python3
"""Compare Python oracle rows with Sieve Extracted call edges.
Usage: python3 -I py-compare.py rows.jsonl <repo>/sieve/.graph/wiring.json

An edge is right if the oracle has a row with the same src file, a row line
inside the span of the Sieve source node, the same dst file, dst_line equal to
the Sieve target start line, and the same name (last dotted part). Same-file
edges count. An edge whose only matching rows are ambiguous is unverified.
Prints: Extracted edges, matched, wrong (first 20), unverified, per-shape coverage.
"""
import ast, collections, json, os, re, sys

_guard = {}


def guarded(repo, path, ss, name):
    """True if a call to `name` inside span ss sits in an `if sys.platform/version_info` block."""
    if path not in _guard:
        try:
            tree = ast.parse(open(os.path.join(repo, path), encoding="utf-8").read())
        except (OSError, SyntaxError, ValueError):
            tree = None
        calls = []
        for n in ast.walk(tree) if tree else []:
            if isinstance(n, ast.If) and re.search(r"sys\.(platform|version_info)", ast.unparse(n.test)):
                for b in n.body + n.orelse:
                    for c in ast.walk(b):
                        if isinstance(c, ast.Call):
                            f = c.func
                            calls.append((c.lineno, f.id if isinstance(f, ast.Name) else getattr(f, "attr", "")))
        _guard[path] = calls
    return any(ss and ss[0] <= l <= ss[1] and nm == name for l, nm in _guard[path])


def span(n):
    m = re.fullmatch(r"L(\d+)-L(\d+)", n.get("span") or "")
    return (int(m[1]), int(m[2])) if m else None


def main():
    rows_path, graph_path = sys.argv[1:3]
    repo = sys.argv[3] if len(sys.argv) > 3 else "."
    rows = [json.loads(l) for l in open(rows_path) if l.strip()]
    g = json.load(open(graph_path))
    node = {n["id"]: n for n in g["nodes"]}
    by_src = collections.defaultdict(list)
    for r in rows:
        by_src[r["src"]].append(r)
    edges = [e for e in g["edges"] if e["relation"] == "calls" and e["confidence"] == "extracted"
             and node.get(e["source"], {}).get("path", "").endswith(".py")
             and node.get(e["target"], {}).get("path", "").endswith(".py")]
    covered, wrong, unver = set(), [], []
    for e in edges:
        s, t = node[e["source"]], node[e["target"]]
        ss, ts = span(s), span(t)
        tn = t["name"].split(".")[-1]
        hit = amb = False
        for r in by_src[s["path"]]:
            if (ss and ss[0] <= r["line"] <= ss[1] and r["dst"] == t["path"] and ts
                    and r["dst_line"] == ts[0] and r["name"] == tn):
                covered.add(id(r))
                if r.get("ambiguous"): amb = True
                else: hit = True
        if not hit and amb:
            unver.append(e)
        elif not hit and guarded(repo, s["path"], ss, tn):
            unver.append(e)  # platform/version guard
        elif not hit:
            wrong.append((e["source"], e["target"], s["path"], ss, t["path"], ts))
    print(f"Extracted edges: {len(edges)}")
    print(f"matched: {len(edges) - len(wrong) - len(unver)}")
    print(f"wrong: {len(wrong)}")
    for w in wrong[:20]:
        print(f"  {w[2]} {w[3]} -> {w[4]} {w[5]}  [{w[0]} -> {w[1]}]")
    print(f"unverified: {len(unver)}")
    print(f"oracle rows: {len(rows)}; per shape: rows, matched by an edge")
    for sh in sorted({r["shape"] for r in rows}):
        rs = [r for r in rows if r["shape"] == sh]
        print(f"  {sh}: {len(rs)}, {sum(id(r) in covered for r in rs)}")


main()
