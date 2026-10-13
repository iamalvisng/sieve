#!/usr/bin/env python3
"""Compare Java oracle rows with Sieve Extracted Java call edges.
Usage: python3 -I jv-compare.py rows.jsonl <repo>/sieve/.graph/wiring.json [repo] [srcroot...]

rows.jsonl comes from JvCalls.java. Same rules as go-compare.py. An edge is right
if the oracle has a row with the same src file, a row line inside the span of the
Sieve source node, and a dst span (same dst file) that contains the start line of
the Sieve target node. A target that is a constructor node (Foo.Foo) uses the start
line of its class node, because the oracle maps a constructor to its class. A source
node with no span uses the whole file. An unmatched edge whose source file is not
under a given source root (default src/main/java and src/test/java) counts as
unverified (javac never saw the file). An edge in a build or target dir counts as
skipped-dir. B6: a matched edge whose row has ovr (an in-repo subtype overrides the
target) counts as wrong. B7: an unmatched edge in a compiled file is wrong, and it is
counted apart when the oracle has an oor (out of repo) or impl (no tree) row for the same
name in the span. Prints: Extracted Java edges, matched, wrong (first 20 with file:line),
unverified, skipped-dir, a count per call shape, edges to an interface or abstract
method in their own line, and the oracle coverage per shape.
"""
import collections, json, os, re, sys


def span(n):
    m = re.fullmatch(r"L(\d+)-L(\d+)", n.get("span") or "")
    return (int(m[1]), int(m[2])) if m else None


def main():
    rows_path, graph_path = sys.argv[1:3]
    repo = sys.argv[3] if len(sys.argv) > 3 else os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(graph_path))))
    roots = tuple(r.rstrip("/") + "/" for r in sys.argv[4:]) or ("src/main/java/", "src/test/java/")
    rows = [json.loads(l) for l in open(rows_path) if l.strip()]
    g = json.load(open(graph_path))
    node = {n["id"]: n for n in g["nodes"]}
    by_src = collections.defaultdict(list)
    for r in rows:
        by_src[r["src"]].append(r)
    cache = {}

    def text(p):
        if p not in cache:
            try:
                cache[p] = open(os.path.join(repo, p), encoding="utf-8").read()
            except OSError:
                cache[p] = ""
        return cache[p]

    def skipped(p):
        return any(d in ("build", "target") for d in p.split("/")[:-1])

    def shape(s, ss, tname):
        """Call shape read from the source text: first line in the span that calls the target name."""
        ls = text(s["path"]).split("\n")
        rx = re.compile(r"(new\s+)?(.?)\b%s\s*[<(]" % re.escape(tname))
        for k in range(ss[0] - 1, min(ss[1], len(ls))):
            for m in rx.finditer(ls[k]):
                if m[1]:
                    return "new", k + 1
                if m[2] != ".":
                    return ("this/super call" if tname in ("this", "super") else "bare"), k + 1
                pre = ls[k][:m.start() + len(m[1] or "") + 1].rstrip(".")
                if re.search(r"(^|[^\w.)\]])\w+$", pre):
                    return "x.name (one ident)", k + 1
                return "chain.name", k + 1
        return "no call text", ss[0]

    def tspan(t):
        """Span of the target for matching; a constructor node maps to its class."""
        ts = span(t)
        m = re.fullmatch(r"(.*#)(?:[\w$.]*\.)?(\w+)\.(\w+)(~\d+)?", t["id"])
        if m and m[2] == m[3] and t.get("kind") == "method":
            c = node.get(m[1] + m[2])
            if c and span(c):
                return (span(c)[0], span(c)[0])
        return ts

    edges = [e for e in g["edges"] if e["relation"] == "calls" and e["confidence"] == "extracted"
             and node.get(e["source"], {}).get("path", "").endswith(".java")
             and node.get(e["target"], {}).get("path", "").endswith(".java")]
    covered, wrong, nmatch, skip, unver = set(), [], 0, 0, 0
    shapes = collections.Counter()
    absn = collections.Counter()
    cause = collections.Counter()
    for e in edges:
        s, t = node[e["source"]], node[e["target"]]
        ss = span(s) or (1, 10 ** 9)
        ts = tspan(t)
        tn = t["name"].split(".")[-1]
        sh, ln = shape(s, ss if span(s) else (1, len(text(s["path"]).split("\n"))), tn)
        hits, other = [], []
        for r in by_src[s["path"]]:
            if not (ss[0] <= r["line"] <= ss[1]):
                continue
            if r["dst"] == t["path"] and ts and r["dst_start"] <= ts[0] <= r["dst_end"]:
                covered.add(id(r)); hits.append(r)
            elif r["name"] == tn and (r.get("oor") or r.get("impl")):
                other.append(r)
        hit = bool(hits)
        isabs = any(r.get("abs") or r.get("iface") for r in hits)
        ovr = any(r.get("ovr") for r in hits)
        if skipped(s["path"]) and not hit:
            skip += 1; shapes[(sh, "skipped-dir")] += 1
        elif hit and not ovr:
            nmatch += 1; shapes[(sh, "matched")] += 1
            if isabs: absn["matched"] += 1
        elif hit:  # B6: a subtype overrides the target
            wrong.append((s["path"], ln, e["source"], e["target"], sh + " ovr")); shapes[(sh, "wrong")] += 1
            cause["overridden"] += 1
            if isabs: absn["wrong"] += 1
        elif not s["path"].startswith(roots):
            unver += 1; shapes[(sh, "unverified")] += 1
        else:
            wrong.append((s["path"], ln, e["source"], e["target"], sh + (" oor" if other else ""))); shapes[(sh, "wrong")] += 1
            cause["out-of-repo/impl target" if other else "other (wrong overload or link)"] += 1
    print(f"Extracted Java edges: {len(edges)}")
    print(f"matched: {nmatch}")
    print(f"wrong: {len(wrong)}")
    for w in wrong[:20]:
        print(f"  {w[0]}:{w[1]} [{w[4]}] {w[2]} -> {w[3]}")
    for k, v in sorted(cause.items()):
        print(f"  wrong cause: {k}: {v}")
    print(f"unverified: {unver}")
    print(f"skipped-dir: {skip}")
    print(f"edges to an abstract or interface method (own count): matched {absn['matched']}, wrong {absn['wrong']}")
    print("per call shape (matched, wrong, unverified, skipped-dir):")
    for sh in sorted({k[0] for k in shapes}):
        print(f"  {sh}: {shapes[(sh,'matched')]}, {shapes[(sh,'wrong')]}, {shapes[(sh,'unverified')]}, {shapes[(sh,'skipped-dir')]}")
    print(f"oracle rows: {len(rows)}; per shape: rows, matched by an edge")
    print(f"  oor rows: {sum(1 for r in rows if r.get('oor'))}, impl rows: {sum(1 for r in rows if r.get('impl'))}, ovr rows: {sum(1 for r in rows if r.get('ovr'))}")
    for sh in sorted({r["shape"] for r in rows}):
        rs = [r for r in rows if r["shape"] == sh]
        print(f"  {sh}: {len(rs)}, {sum(id(r) in covered for r in rs)}")


main()
