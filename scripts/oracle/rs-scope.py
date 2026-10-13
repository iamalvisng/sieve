#!/usr/bin/env python3
"""Frozen RMX1 in-scope count. Usage: python3 -I rs-scope.py rows.jsonl <repo> [rows.jsonl <repo> ...]

A self_method row (owner inherent) is in scope when:
  - the caller fn and the target fn are direct children of the same inherent impl block;
  - that impl block sits at the file root or in a mod body;
  - the block holds exactly one fn with the target name;
  - the target fn has no cfg attribute, no where clause and no generic bound;
  - the receiver forms of caller and target are equal (&self, &mut self, self).
Prints the count per repo, the total, and 80 percent of the total.
"""
import bisect, json, os, re, sys


def blank(t):
    """Replace comments, strings and chars by spaces. Offsets and newlines stay."""
    out, i, n = [], 0, len(t)
    def fill(a, b): out.append(re.sub(r"[^\n]", " ", t[a:b]))
    while i < n:
        if t.startswith("//", i):
            j = t.find("\n", i); j = n if j < 0 else j; fill(i, j); i = j
        elif t.startswith("/*", i):
            d, j = 1, i + 2
            while j < n and d:
                if t.startswith("/*", j): d += 1; j += 2
                elif t.startswith("*/", j): d -= 1; j += 2
                else: j += 1
            fill(i, j); i = j
        elif (m := re.compile(r'b?r(#*)"').match(t, i)):
            e = t.find('"' + m.group(1), m.end()); j = n if e < 0 else e + 1 + len(m.group(1)); fill(i, j); i = j
        elif t[i] == '"':
            j = i + 1
            while j < n and t[j] != '"': j += 2 if t[j] == "\\" else 1
            fill(i, j + 1); i = j + 1
        elif t[i] == "'" and (m := re.compile(r"'(\\.[^']*|[^\\'])'").match(t, i)):
            fill(i, m.end()); i = m.end()
        else:
            out.append(t[i]); i += 1
    return "".join(out)


def blocks(t):
    """List of blocks (kind, header, open, close, parent index, header start). Kinds: impl, inherent, mod, fn, other."""
    res, stack, last = [], [], 0
    for m in re.finditer(r"[{};]", t):
        c, o = m.group(), m.start()
        if c == "{":
            h = t[last:o]
            hs = h.strip()
            core = re.sub(r"#!?\[[^\]]*\]", "", hs).strip()
            if re.match(r"(pub(\([^)]*\))?\s+)?(unsafe\s+)?impl\b", core):
                k = "trait_impl" if re.search(r"\bfor\b(?!\s*<)", core) else "inherent"
            elif re.search(r"\bmod\s+\w+\s*$", core): k = "mod"
            elif re.search(r"\bfn\s+\w+", core): k = "fn"
            else: k = "other"
            res.append([k, h, o, None, stack[-1] if stack else -1])
            stack.append(len(res) - 1)
        elif c == "}":
            if stack: res[stack.pop()][3] = o
        last = o + 1
    return res


def recv(h):
    m = re.search(r"\bfn\s+\w+\s*(<.*?>)?\s*\(\s*(.*?)(,|\)|$)", h, re.S)
    p = re.sub(r"\s+", " ", m.group(2)) if m else ""
    if re.match(r"&\s*mut\s+self\b(?!\s*:)", p) or re.match(r"self\s*:\s*&\s*mut\s+Self\b", p): return "&mut self"
    if re.match(r"&\s*('\w+\s+)?self\b(?!\s*:)", p) or re.match(r"self\s*:\s*&\s*('\w+\s+)?Self\b", p): return "&self"
    if re.match(r"(mut\s+)?self\s*(:\s*Self)?$", p): return "self"
    return None


def clean(h):
    """Target fn header: no cfg attr, no where clause, no generic bound."""
    if re.search(r"#\[\s*cfg", h) or re.search(r"\bwhere\b", h): return False
    core = re.sub(r"#!?\[[^\]]*\]", "", h)
    g = re.search(r"\bfn\s+\w+\s*(<.*?>)\s*\(", core, re.S)
    if g and ":" in g.group(1): return False
    return not re.search(r"\bimpl\b", core.split("fn", 1)[-1])


def scope(rows_path, repo):
    rows = [json.loads(l) for l in open(rows_path) if l.strip()]
    rows = [r for r in rows if r["shape"] == "self_method" and r["owner"] == "inherent"]
    cache, n = {}, 0
    for r in rows:
        if r["src"] != r["dst"]: continue
        if r["src"] not in cache:
            try: t = blank(open(os.path.join(repo, r["src"]), encoding="utf-8").read())
            except OSError: t = ""
            nl = [m.start() for m in re.finditer("\n", t)]
            cache[r["src"]] = (t, blocks(t), nl)
        t, bl, nl = cache[r["src"]]
        line = lambda o: bisect.bisect_left(nl, o) + 1  # 1-based line of offset o
        # caller: innermost fn block holding the row line
        cands = [i for i, b in enumerate(bl) if b[0] == "fn" and b[3] and line(b[2]) <= r["line"] <= line(b[3])]
        if not cands: continue
        ci = max(cands, key=lambda i: bl[i][2])
        imp = bl[ci][4]
        if imp < 0 or bl[imp][0] != "inherent": continue
        par = bl[imp][4]
        if par >= 0 and bl[par][0] != "mod": continue
        kids = [i for i, b in enumerate(bl) if b[4] == imp and b[0] == "fn"]
        nm = lambda i: (re.search(r"\bfn\s+(\w+)", bl[i][1]) or [0, ""])[1]
        same = [i for i in kids if nm(i) == r["name"]]
        if len(same) != 1: continue
        ti = same[0]
        if not (r["dst_start"] <= line(bl[ti][2]) <= r["dst_end"]) or not clean(bl[ti][1]): continue
        rc, rt = recv(bl[ci][1]), recv(bl[ti][1])
        if rc and rc == rt: n += 1
    return n


def main():
    a, tot = sys.argv[1:], 0
    for rows, repo in zip(a[::2], a[1::2]):
        n = scope(rows, os.path.realpath(repo)); tot += n
        print(f"{os.path.basename(os.path.realpath(repo))}: in scope {n}")
    print(f"total in scope: {tot}; 80 percent: {-(-tot * 4 // 5)}")


main()
