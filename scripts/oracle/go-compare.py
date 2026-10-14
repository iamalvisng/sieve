#!/usr/bin/env python3
"""Compare Go oracle rows with Sieve Extracted Go call edges.
Usage: python3 -I go-compare.py rows.jsonl <repo>/sieve/.graph/wiring.json [repo]

An edge is right if the oracle has a row with the same src file, a row line
inside the span of the Sieve source node, and a dst span (same dst file) that
contains the start line of the Sieve target node. A source node with no span (a
package-level `var x = f()` call has the file node as source) uses the whole
file as its span. An edge whose only oracle rows are ambiguous counts as
unverified. An unmatched edge whose call sits in a file with a `//go:build` or
`// +build` line, a GOOS/GOARCH file-name suffix or `import "C"` counts as
unverified. An edge in a testdata, _x or .x dir counts as skipped-dir. Prints: Extracted Go edges, matched, wrong (first 20 with file:line),
unverified, a count per call shape, and the oracle coverage per shape.
"""
import collections, json, os, re, sys

OS = "aix android darwin dragonfly freebsd hurd illumos ios js linux nacl netbsd openbsd plan9 solaris wasip1 windows zos"
ARCH = "386 amd64 amd64p32 arm armbe arm64 arm64be loong64 mips mipsle mips64 mips64le mips64p32 mips64p32le ppc ppc64 ppc64le riscv riscv64 s390 s390x sparc sparc64 wasm"
SUFFIX = re.compile(r"_(%s)(_(%s))?\.go$|_(%s)\.go$" % ("|".join(OS.split()), "|".join(ARCH.split()), "|".join(ARCH.split())))
TAG = re.compile(r"^\s*(//go:build\b|// \+build\b|import\s+\"C\"|import\s+\(?\s*\"C\")", re.M)


def span(n):
    m = re.fullmatch(r"L(\d+)-L(\d+)", n.get("span") or "")
    return (int(m[1]), int(m[2])) if m else None


def main():
    rows_path, graph_path = sys.argv[1:3]
    repo = sys.argv[3] if len(sys.argv) > 3 else os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(graph_path))))
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

    def skipped(p):  # the go tool skips testdata, _x and .x dirs, so scip-go has no rows there
        return any(d == "testdata" or d[:1] in "_." for d in p.split("/")[:-1])

    def constrained(p):
        return bool(SUFFIX.search(p)) or bool(TAG.search(text(p)))

    def shape(s, ss, tname):
        """Call shape read from the source text: first line in the span that calls the target name."""
        ls = text(s["path"]).split("\n")
        rx = re.compile(r"(.?)\b%s\s*[\[(]" % re.escape(tname))
        for k in range(ss[0] - 1, min(ss[1], len(ls))):
            for m in rx.finditer(ls[k]):
                if m[1] != ".":
                    return "bare", k + 1
                pre = ls[k][:m.start() + 1].rstrip(".")
                if re.search(r"(^|[^\w.])\w+$", pre):
                    return "x.name (one ident)", k + 1
                return "chain.name", k + 1
        return "no call text", ss[0]

    edges = [e for e in g["edges"] if e["relation"] == "calls" and e["confidence"] == "extracted"
             and node.get(e["source"], {}).get("path", "").endswith(".go")
             and node.get(e["target"], {}).get("path", "").endswith(".go")]
    covered, wrong, unver, nmatch, skip, selfe = set(), [], [], 0, 0, []
    shapes = collections.Counter()
    for e in edges:
        s, t = node[e["source"]], node[e["target"]]
        ss = span(s) or (1, 10 ** 9)  # A file source spans the whole file
        ts = span(t)
        tn = t["name"].split(".")[-1]
        sh, ln = shape(s, ss if span(s) else (1, len(text(s["path"]).split("\n"))), tn)
        hit = amb = False
        for r in by_src[s["path"]]:
            if not (ss[0] <= r["line"] <= ss[1]):
                continue
            if r["dst"] == t["path"] and ts and r["dst_start"] <= ts[0] <= r["dst_end"]:
                covered.add(id(r))
                if r.get("ambiguous"): amb = True
                else: hit = True
            elif r.get("ambiguous") and r["name"] == tn:
                amb = True
        if skipped(s["path"]) and not hit:
            skip += 1; shapes[(sh, "skipped-dir")] += 1
            if e["source"] == e["target"]:
                selfe.append(f'{s["path"]}:{ln} {e["source"]}')
        elif hit:
            nmatch += 1; shapes[(sh, "matched")] += 1
        elif amb or constrained(s["path"]):
            unver.append(e); shapes[(sh, "unverified")] += 1
        else:
            wrong.append((s["path"], ln, e["source"], e["target"], sh)); shapes[(sh, "wrong")] += 1
    print(f"Extracted Go edges: {len(edges)}")
    print(f"matched: {nmatch}")
    print(f"wrong: {len(wrong)}")
    for w in wrong[:20]:
        print(f"  {w[0]}:{w[1]} [{w[4]}] {w[2]} -> {w[3]}")
    print(f"unverified: {len(unver)}")
    print(f"skipped-dir: {skip}")
    for x in selfe:
        print(f"  self edge: {x}")
    print("per call shape (matched, wrong, unverified, skipped-dir):")
    for sh in sorted({k[0] for k in shapes}):
        print(f"  {sh}: {shapes[(sh,'matched')]}, {shapes[(sh,'wrong')]}, {shapes[(sh,'unverified')]}, {shapes[(sh,'skipped-dir')]}")
    print(f"oracle rows: {len(rows)}; per shape: rows, matched by an edge")
    for sh in sorted({r["shape"] for r in rows}):
        rs = [r for r in rows if r["shape"] == sh]
        print(f"  {sh}: {len(rs)}, {sum(id(r) in covered for r in rs)}")


main()
