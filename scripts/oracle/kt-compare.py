#!/usr/bin/env python3
"""Compare Kotlin oracle rows with Sieve Extracted Kotlin call edges.
Usage: python3 -I kt-compare.py rows.jsonl[,rows2.jsonl...] <repo>/sieve/.graph/wiring.json <repo>

rows.jsonl comes from kt-calls.sh (KtCalls.kt, kotlinc 2.4.21, K2). Next to each rows file is
rows.jsonl.files: the compile set. Several rows files give the union of their rows and files.
Same rules as cs-compare.py, with these changes.
An edge is right if the oracle has a row with the same src file, a row line inside the span of
the Sieve source node, and a dst span (same dst file) that contains the start line of the Sieve
target node. A row dst span is the declaration header, so a member does not match its class.
A target that is a class node reached by `Foo(...)` matches a constructor row (the oracle maps a
constructor to its class). A source node with no span uses the whole file.
Each call row belongs to its innermost enclosing declaration (a function, a constructor, a
property or a type, from the oracle decl rows; a constructor counts as its class). A row counts
for a Sieve source node only when that declaration has the node's name (without a `~N` suffix
or backticks)
and overlaps the node's span. When that declaration is a property, the next 2 enclosing ones (a
companion, then its class) may match instead. A Sieve `variable` source node (a local `val`) has no decl row: its row counts
when the node span lies inside that innermost declaration. So a call in a local function or a nested type never judges an edge from the
outer function.
A matched edge whose target has an in-repo override (an "ovrfact" row names the target as a
base) counts as wrong "overridden". An edge to an expect declaration counts as wrong "expect".
An unmatched edge in a compiled file is wrong, with the cause read from the oracle rows of
the same name in the span (out of repo, extension, function value, local, operator, ...).
An unmatched edge is unverified when its source file is not in the compile set, or when the
oracle has an error row of that name (the compiler could not resolve the call).
Prints: Extracted Kotlin edges, matched, wrong (each with file:line), wrong by cause,
unverified by cause, edges to an abstract or interface member in their own count, a count per
call shape, and the oracle coverage per shape and per flag.
"""
import collections, json, os, re, sys


def span(n):
    m = re.fullmatch(r"L(\d+)-L(\d+)", n.get("span") or "")
    return (int(m[1]), int(m[2])) if m else None


def main():
    rows_paths = sys.argv[1].split(",")
    graph_path = sys.argv[2]
    repo = sys.argv[3] if len(sys.argv) > 3 else os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(graph_path))))
    seen, allrows, facts, decls, compiled, plugin_err = set(), [], [], [], set(), 0
    for rp in rows_paths:
        for l in open(rp):
            l = l.strip()
            if not l or l in seen:
                continue
            seen.add(l)
            r = json.loads(l)
            if r.get("kind") == "ovrfact":
                facts.append(r)
            elif r.get("kind") == "pluginerr":
                plugin_err += 1
            elif r.get("kind") == "decl":
                decls.append(r)
            else:
                allrows.append(r)
        for l in open(rp + ".files"):
            if l.strip():
                compiled.add(os.path.normpath(l.strip()))
    overridden = {f["base"] for f in facts}
    rows = allrows
    encl_kinds = {"function", "constructor", "property", "class", "interface", "object", "companion", "enum"}
    decl_by_file = collections.defaultdict(list)
    for d in decls:
        if d["dkind"] in encl_kinds:
            nm = d["owner"][-1] if d["dkind"] == "constructor" and d["owner"] else d["name"]
            decl_by_file[d["file"]].append((d["start"], d["end"], nm, d["dkind"] == "property"))
    for r in rows:
        chain = sorted((d for d in decl_by_file.get(r["src"], ()) if d[0] <= r["line"] <= d[1]),
                       key=lambda d: d[1] - d[0])
        r["_encl"] = chain[:3] if chain and chain[0][3] else chain[:1]
    encl_drop = 0
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

    def shape(s, ss, tname):
        """Call shape read from the source text: first line in the span that calls the target name."""
        ls = text(s["path"]).split("\n")
        rx = re.compile(r"(\??\.)?\b%s\s*[<(\{]" % re.escape(tname))
        for k in range(ss[0] - 1, min(ss[1], len(ls))):
            for m in rx.finditer(ls[k]):
                if m[1]:
                    pre = ls[k][:m.start()]
                    return ("x.name (one ident)" if re.search(r"(^|[^\w.)\]])\w+$", pre) else "chain.name"), k + 1
                return "bare", k + 1
        return "no call text", ss[0]

    edges = [e for e in g["edges"] if e["relation"] == "calls" and e["confidence"] == "extracted"
             and node.get(e["source"], {}).get("path", "").endswith(".kt")
             and node.get(e["target"], {}).get("path", "").endswith(".kt")]
    to_java = sum(1 for e in g["edges"] if e["relation"] == "calls" and e["confidence"] == "extracted"
                  and node.get(e["source"], {}).get("path", "").endswith(".kt")
                  and node.get(e["target"], {}).get("path", "").endswith(".java"))
    covered, wrong, nmatch, unver = set(), [], 0, 0
    shapes = collections.Counter()
    absn = collections.Counter()
    cause = collections.Counter()
    unv_cause = collections.Counter()
    flag_hit = collections.Counter()
    for e in edges:
        s, t = node[e["source"]], node[e["target"]]
        ss = span(s) or (1, 10 ** 9)
        ts = span(t)
        tstart = ts[0] if ts else 0
        tn = t["name"].split(".")[-1]
        sh, ln = shape(s, ss if span(s) else (1, len(text(s["path"]).split("\n"))), tn)
        hits, other = [], []
        sname = re.sub(r"~\d+$", "", s["name"].split(".")[-1]).strip("`")
        for r in by_src[s["path"]]:
            if not (ss[0] <= r["line"] <= ss[1]):
                continue
            if r["_encl"] and not any(d[2] == sname and d[0] <= ss[1] and ss[0] <= d[1] for d in r["_encl"]) \
                    and not (s.get("kind") == "variable" and r["_encl"][0][0] <= ss[0] and ss[1] <= r["_encl"][0][1]):
                if r["dst"] == t["path"] and r["dst_start"] <= tstart <= r["dst_end"]:
                    encl_drop += 1
                continue
            if r["dst"] == t["path"] and r["dst_start"] <= tstart <= r["dst_end"]:
                covered.add(id(r)); hits.append(r)
            elif r["name"] == tn:
                other.append(r)
        isabs = any(r.get("abs") or r.get("iface") for r in hits)
        ovr = any(r["dkey"] in overridden for r in hits)
        exp = any(r.get("expect") for r in hits)
        if hits and not ovr and not exp:
            nmatch += 1; shapes[(sh, "matched")] += 1
            if os.environ.get("KT_SHOW") == "matched":
                print(f"  matched {s['path']}:{ln} {e['source']} -> {e['target']}")
            if isabs: absn["matched"] += 1
            for r in hits:
                for k, v in r.items():
                    if v is True: flag_hit[k] += 1
        elif hits:
            c = "overridden" if ovr else "expect declaration"
            wrong.append((s["path"], ln, e["source"], e["target"], sh + " " + c)); shapes[(sh, "wrong")] += 1
            cause[c] += 1
            if isabs: absn["wrong"] += 1
        elif s["path"] not in compiled:
            unver += 1; shapes[(sh, "unverified")] += 1; unv_cause["file not compiled"] += 1
        elif any(r.get("err") for r in other):
            unver += 1; shapes[(sh, "unverified")] += 1; unv_cause["error row (compiler could not resolve)"] += 1
        else:
            for k, name in (("oor", "out-of-repo target"), ("java", "java target"), ("ext", "extension"),
                            ("fnval", "function value invoke"), ("local", "local function"),
                            ("op", "operator, infix or indexing"), ("impl", "implicit member")):
                if any(r.get(k) for r in other):
                    c = name
                    break
            else:
                c = "other (wrong overload or link)" if other else "no call row of that name"
            if other and not any(r.get(k) for r in other for k in ("oor", "java", "ext", "fnval", "local", "op", "impl")):
                # a same-name row hit a repo target that is not the Sieve target
                inrepo = [r for r in other if r["dst"]]
                if inrepo and all(r.get("ctor") for r in inrepo):
                    c = "constructor of another class"
            wrong.append((s["path"], ln, e["source"], e["target"], sh + " " + c)); shapes[(sh, "wrong")] += 1
            cause[c] += 1
    print(f"Extracted Kotlin edges: {len(edges)} (Kotlin to Java edges, not judged: {to_java})")
    print(f"matched: {nmatch}")
    print(f"wrong: {len(wrong)}")
    for w in wrong:
        print(f"  {w[0]}:{w[1]} [{w[4]}] {w[2]} -> {w[3]}")
    for k, v in sorted(cause.items()):
        print(f"  wrong cause: {k}: {v}")
    print(f"unverified: {unver}")
    print(f"  rows that hit the target but sit in another enclosing declaration (not counted): {encl_drop}")
    for k, v in sorted(unv_cause.items()):
        print(f"  unverified cause: {k}: {v}")
    print(f"edges to an abstract or interface member (own count): matched {absn['matched']}, wrong {absn['wrong']}")
    print("per call shape (matched, wrong, unverified):")
    for sh in sorted({k[0] for k in shapes}):
        print(f"  {sh}: {shapes[(sh,'matched')]}, {shapes[(sh,'wrong')]}, {shapes[(sh,'unverified')]}")
    errrows = sum(1 for r in rows if r.get("err"))
    print(f"oracle rows: {len(rows)}; error rows: {errrows}; override facts: {len(facts)}; plugin errors: {plugin_err}; compile set files: {len(compiled)}")
    flags = ["ctor", "ext", "fnval", "local", "top", "abs", "iface", "ovd", "virt", "oor", "impl", "java", "op", "expect", "annot"]
    print("  rows per flag: " + ", ".join(f"{f} {sum(1 for r in rows if r.get(f))}" for f in flags))
    print("per shape: rows, matched by an edge")
    for sh in sorted({r["shape"] for r in rows}):
        rs = [r for r in rows if r["shape"] == sh]
        print(f"  {sh}: {len(rs)}, {sum(id(r) in covered for r in rs)}")


main()
