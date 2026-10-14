#!/usr/bin/env python3
"""Compare C# oracle rows with Sieve Extracted C# call edges.
Usage: python3 -I cs-compare.py rows.jsonl <repo>/sieve/.graph/wiring.json <repo> build.json

rows.jsonl comes from CsCalls (scripts/oracle/CsCalls). build.json is the input of CsCalls: its
Compile items are the files the compiler saw. Same rules as jv-compare.py, with these changes.
An unmatched edge is unverified when its source file is not a Compile item, or when its call line
is in an inactive #if region (an inactive row from CsCalls). A constructor node is a method node
with the name of a class node in the same file; it uses the start line of that class node.
A target in a generated file (gen row) counts as wrong, as an out-of-repo target. An edge is right
if the oracle has a row with the same src file, a row line inside the span of the
Sieve source node, and a dst span (same dst file) that contains the start line of
the Sieve target node. A target that is a constructor node (Foo.Foo) uses the start
line of its class node, because the oracle maps a constructor to its class. A source
node with no span uses the whole file. An unmatched edge whose source file is not
under the compiled set counts as unverified (Roslyn never saw the file). An edge in a bin or obj dir counts as
skipped-dir. B6: a matched edge whose row has ovr (an in-repo subtype overrides the
target) counts as wrong. B7: an unmatched edge in a compiled file is wrong, and it is
counted apart when the oracle has an oor (out of repo) or impl (no tree) row for the same
name in the span. Prints: Extracted C# edges, matched, wrong (first 20 with file:line),
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
    rp = os.path.realpath(repo) + "/"
    compiled = {os.path.realpath(c["FullPath"])[len(rp):] for c in json.load(open(sys.argv[4]))["Items"]["Compile"]}
    allrows = [json.loads(l) for l in open(rows_path) if l.strip()]
    rows = [r for r in allrows if not r.get("inactive") and not r.get("errfile")]
    errfiles = {r["src"] for r in allrows if r.get("errfile")}
    inactive = collections.defaultdict(list)
    for r in allrows:
        if r.get("inactive"):
            inactive[r["src"]].append((r["start"], r["end"]))
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
        return any(d in ("bin", "obj") for d in p.split("/")[:-1])

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
        if t.get("kind") == "method":
            c = node.get(t["path"] + "#" + t["name"])
            if c and c.get("kind") in ("class", "struct", "record", "interface") and span(c):
                return (span(c)[0], span(c)[0])
        return ts

    edges = [e for e in g["edges"] if e["relation"] == "calls" and e["confidence"] == "extracted"
             and node.get(e["source"], {}).get("path", "").endswith(".cs")
             and node.get(e["target"], {}).get("path", "").endswith(".cs")]
    covered, wrong, nmatch, skip, unver = set(), [], 0, 0, 0
    shapes = collections.Counter()
    absn = collections.Counter()
    cause = collections.Counter()
    unv_cause = collections.Counter()
    ntaint = 0
    for e in edges:
        s, t = node[e["source"]], node[e["target"]]
        ss = span(s) or (1, 10 ** 9)
        ts = tspan(t)
        tn = t["name"].split(".")[-1]
        sh, ln = shape(s, ss if span(s) else (1, len(text(s["path"]).split("\n"))), tn)
        isctor = ts is not None and ts[0] == ts[1] and t.get("kind") == "method" and node.get(t["path"] + "#" + t["name"]) is not None
        hits, other, same = [], [], []
        for r in by_src[s["path"]]:
            if not (ss[0] <= r["line"] <= ss[1]):
                continue
            # A constructor edge matches only the primary part of a partial type, not an alt row
            if r["dst"] == t["path"] and ts and r["dst_start"] <= ts[0] <= r["dst_end"] and not (isctor and r.get("alt")):
                covered.add(id(r)); hits.append(r)
            elif r["name"] == tn:
                other.append(r)
        # Another call of the same name in this source node goes to another target
        tainted = bool(hits) and any(not r.get("alt") and r["dst"] != t["path"] or (r["dst"] == t["path"] and not (ts and r["dst_start"] <= ts[0] <= r["dst_end"])) for r in other)
        hit = bool(hits)
        isabs = any(r.get("abs") or r.get("iface") for r in hits)
        ovr = any(r.get("ovr") for r in hits)
        dyn = any(r.get("dyn") or r.get("intercepted") for r in hits)
        if s["path"] in errfiles:
            unver += 1; unv_cause["file with compiler errors"] += 1; shapes[(sh, "unverified")] += 1
        elif skipped(s["path"]) and not hit:
            skip += 1; shapes[(sh, "skipped-dir")] += 1
        elif hit and not ovr and not dyn:
            nmatch += 1; shapes[(sh, "matched")] += 1
            if tainted: ntaint += 1
            if isabs: absn["matched"] += 1
        elif hit:  # B6: a subtype overrides the target; or dynamic or intercepted dispatch
            wrong.append((s["path"], ln, e["source"], e["target"], sh + (" ovr" if ovr else " dyn"))); shapes[(sh, "wrong")] += 1
            cause["overridden" if ovr else "dynamic or intercepted"] += 1
            if isabs: absn["wrong"] += 1
        elif s["path"] not in compiled or any(a <= ln <= b for a, b in inactive.get(s["path"], [])):
            unver += 1; shapes[(sh, "unverified")] += 1
            unv_cause["file not compiled" if s["path"] not in compiled else "inactive #if text"] += 1
        elif any(r.get("unv") for r in other):
            unver += 1; unv_cause["error row (inaccessible, ambiguous)"] += 1; shapes[(sh, "unverified")] += 1
        else:
            if any(r.get("badcall") for r in other): c = "late-bound or overload failure"
            elif any(r.get("intercepted") for r in other): c = "intercepted"
            elif any(r.get("ext") for r in other): c = "extension method"
            elif any(r.get("deleg") for r in other): c = "delegate invoke"
            elif any(r.get("local") for r in other): c = "local function"
            elif any(r.get("dyn") for r in other): c = "dynamic"
            elif any(r.get("gen") for r in other): c = "generated target"
            elif any(r.get("oor") or r.get("impl") for r in other): c = "out-of-repo/impl target"
            else: c = "other (wrong overload or link)"
            wrong.append((s["path"], ln, e["source"], e["target"], sh + " " + c)); shapes[(sh, "wrong")] += 1
            cause[c] += 1
    print(f"Extracted C# edges: {len(edges)}")
    print(f"matched: {nmatch}")
    print(f"wrong: {len(wrong)}")
    for w in wrong[:20]:
        print(f"  {w[0]}:{w[1]} [{w[4]}] {w[2]} -> {w[3]}")
    for k, v in sorted(cause.items()):
        print(f"  wrong cause: {k}: {v}")
    print(f"unverified: {unver}")
    for k, v in sorted(unv_cause.items()):
        print(f"  unverified cause: {k}: {v}")
    print(f"matched with a tainted pair (same name, two targets in one source node; call-level binding not visible): {ntaint}")
    print(f"skipped-dir: {skip}")
    print(f"edges to an abstract or interface method (own count): matched {absn['matched']}, wrong {absn['wrong']}")
    print("per call shape (matched, wrong, unverified, skipped-dir):")
    for sh in sorted({k[0] for k in shapes}):
        print(f"  {sh}: {shapes[(sh,'matched')]}, {shapes[(sh,'wrong')]}, {shapes[(sh,'unverified')]}, {shapes[(sh,'skipped-dir')]}")
    ef = sum(1 for r in rows if r["src"] in errfiles)
    print(f"oracle rows: {len(rows)}; rows in files with errors: {ef}; extension rows: {sum(1 for r in rows if r.get('ext'))}; base rows: {sum(1 for r in rows if r['shape']=='base')}; generated-target rows: {sum(1 for r in rows if r.get('gen'))}")
    print(f"  delegate rows: {sum(1 for r in rows if r.get('deleg'))}, local function rows: {sum(1 for r in rows if r.get('local'))}, dynamic rows: {sum(1 for r in rows if r.get('dyn'))}, intercepted rows: {sum(1 for r in rows if r.get('intercepted'))}, error rows: {sum(1 for r in rows if r.get('badcall') or r.get('unv'))}")
    print("per shape: rows, matched by an edge")
    print(f"  oor rows: {sum(1 for r in rows if r.get('oor'))}, impl rows: {sum(1 for r in rows if r.get('impl'))}, ovr rows: {sum(1 for r in rows if r.get('ovr'))}")
    for sh in sorted({r["shape"] for r in rows}):
        rs = [r for r in rows if r["shape"] == sh]
        print(f"  {sh}: {len(rs)}, {sum(id(r) in covered for r in rs)}")


main()
