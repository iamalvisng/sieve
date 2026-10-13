#!/usr/bin/env python3
"""Oracle for Rust call links. Usage: python3 -I rs-calls.py <repo> [out.jsonl]

Runs the pinned rust-analyzer `scip` subcommand with all features on (cfg(test)
is on by default), reads the SCIP file with a small protobuf reader, and
writes one JSON row per call reference to a fn or method:
  {src, line, shape, dst, name, dst_start, dst_end}
Lines are 1-based. Paths are relative to <repo>. dst_start and dst_end are the
enclosing range of the target definition (the whole fn item).
A symbol with many definitions (same fn name in two test crates) resolves to
the definition in the reference's own crate target; if that is not unique,
one row per candidate is written with "ambiguous": true.
Env SCIP_KEEP=<path> keeps the SCIP file.
Shapes: bare, path, use_alias, glob, crate, self_ty, type_assoc, self_method,
method, macro_arg.
"""
import json, os, re, subprocess, sys, tempfile

RA = os.environ.get("RA_BIN", os.path.expanduser("~/.local/share/sieve-oracle/rust-analyzer"))
CFG = {"cargo": {"features": "all"}}


def varint(b, i):
    r = s = 0
    while True:
        c = b[i]; i += 1
        r |= (c & 0x7F) << s; s += 7
        if c < 0x80:
            return r, i


def fields(b):
    """Yield (field number, wire type, value) for one message."""
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


def rng(r):  # SCIP range -> (l0, c0, l1, c1)
    return (r[0], r[1], r[0], r[2]) if len(r) == 3 else tuple(r[:4])


def read_scip(path):
    data = open(path, "rb").read()
    docs = []
    for f, _, v in fields(data):
        if f != 2:
            continue
        rel, occs = None, []
        for g, _, w in fields(v):
            if g == 1:
                rel = w.decode()
            elif g == 2:
                o = {"role": 0, "enc": None}
                for h, wt, x in fields(w):
                    if h == 1: o["range"] = rng(packed(x))
                    elif h == 2: o["sym"] = x.decode()
                    elif h == 3: o["role"] = x
                    elif h == 7: o["enc"] = rng(packed(x))
                occs.append(o)
        docs.append((rel, occs))
    return docs


def target(rel):
    """Crate target of a path: tests/<x>.rs, src/bin/<x>, examples/<x>, benches/<x> are
    their own targets; the rest of src/ is the lib or main target."""
    parts = rel.split("/")
    for i, c in enumerate(parts):
        if c in ("src", "tests", "examples", "benches"):
            root, rest = "/".join(parts[:i]), parts[i + 1:]
            if c == "src":
                if len(rest) > 1 and rest[0] == "bin":
                    return (root, "bin", rest[1].removesuffix(".rs"))
                return (root, "lib")
            if len(rest) > 1 or rest and rest[0].endswith(".rs"):
                return (root, c, rest[0].removesuffix(".rs"))
            break
    return ("/".join(parts[:-1]), "file", rel)


FN = re.compile(r"([A-Za-z0-9_]+)`?\(\+?\d*\)\.$")
RAW = re.compile(r'b?r(#*)"')
CHR = re.compile(r"'(\\.[^']*|[^\\'])'")


def macro_depth(text):
    """Return a function (line0, col0) -> True if inside a macro call token tree.
    Rough scanner: skips comments, strings and chars; tracks brackets."""
    lines_start = [0]
    for m in re.finditer("\n", text):
        lines_start.append(m.end())
    marks = []  # (offset, in_macro) at each change
    stack, i, n = [], 0, len(text)
    cur = False
    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i); i = n if j < 0 else j; continue
        if text.startswith("/*", i):
            j = text.find("*/", i + 2); i = n if j < 0 else j + 2; continue
        m = RAW.match(text, i)
        if m:
            end = text.find('"' + m.group(1), m.end()); i = n if end < 0 else end + 1 + len(m.group(1)); continue
        if c == '"':
            i += 1
            while i < n and text[i] != '"':
                i += 2 if text[i] == "\\" else 1
            i += 1; continue
        if c == "'":
            m = CHR.match(text, i)
            if m: i = m.end(); continue
        if c in "([{":
            k = i - 1
            while k >= 0 and text[k] in " \t\n": k -= 1
            ismac = k >= 1 and text[k] == "!" and (text[k - 1].isalnum() or text[k - 1] == "_")
            stack.append(ismac or (bool(stack) and stack[-1]))
        elif c in ")]}" and stack:
            stack.pop()
        now = bool(stack) and stack[-1]
        if now != cur:
            marks.append((i, now)); cur = now
        i += 1
    import bisect
    offs = [m[0] for m in marks]

    def q(l0, c0):
        o = lines_start[l0] + c0
        k = bisect.bisect_right(offs, o) - 1
        return marks[k][1] if k >= 0 else False
    return q


def classify(text, lines, l0, c0, c1, name, dstname, src, dst, in_macro, globs, imported):
    line = lines[l0]
    before = line[:c0]
    if in_macro:
        return "macro_arg"
    if before.rstrip().endswith("."):
        return "self_method" if re.search(r"\bself\s*\.\s*$", before) else "method"
    m = re.search(r"((?:<[^<>]*>|[A-Za-z_][A-Za-z0-9_]*)(?:::(?:<[^<>]*>)?[A-Za-z_][A-Za-z0-9_]*)*)::$", before)
    if m:
        segs = m.group(1)
        first = re.split(r"::", segs)[0]
        last = re.split(r"::", segs)[-1]
        if first == "Self" and "::" not in segs:
            return "self_ty"
        if first in ("crate", "super", "self"):
            return "crate"
        if last[:1].isupper() or segs.startswith("<"):
            return "type_assoc"
        return "path"
    if name != dstname:
        return "use_alias"
    if src != dst and name not in imported and globs:
        return "glob"
    return "bare"


def owner(sym):
    """Owner of the target fn from its SCIP symbol: inherent, trait_impl, trait, free."""
    m = re.search(r"impl#\[[^\]]*\](\[[^\]]*\])?[^/\[]*$", sym)
    if m:
        return "trait_impl" if m.group(1) else "inherent"
    return "trait" if re.search(r"[A-Za-z0-9_]#" + FN.pattern, sym) else "free"


def main():
    repo = os.path.realpath(sys.argv[1])
    out = open(sys.argv[2], "w") if len(sys.argv) > 2 else sys.stdout
    with tempfile.TemporaryDirectory() as td:
        cfg, scip = os.path.join(td, "cfg.json"), os.path.join(td, "index.scip")
        json.dump(CFG, open(cfg, "w"))
        r = subprocess.run([RA, "scip", repo, "--output", scip, "--config-path", cfg, "--exclude-vendored-libraries"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        if r.returncode:
            sys.exit("rust-analyzer failed: " + r.stderr[-500:])
        docs = read_scip(scip)
        if os.environ.get("SCIP_KEEP"):
            import shutil; shutil.copy(scip, os.environ["SCIP_KEEP"])
    defs = {}
    for rel, occs in docs:
        for o in occs:
            if o["role"] & 1 and FN.search(o.get("sym", "")):
                s = o["enc"] or o["range"]
                defs.setdefault(o["sym"], []).append((rel, s[0] + 1, s[2] + 1, o["range"]))
    rows = []
    for rel, occs in docs:
        rows.append({"src": rel, "line": 0, "shape": "doc", "dst": rel, "name": "", "dst_start": 0, "dst_end": 0, "owner": "none"})
        for o in occs:  # fn definitions: lets the compare tell cfg-off callers from wrong edges
            if o["role"] & 1 and FN.search(o.get("sym", "")) and "range" in o:
                rows.append({"src": rel, "line": o["range"][0] + 1, "shape": "def", "dst": rel, "name": "",
                             "dst_start": 0, "dst_end": 0, "owner": "none"})
        p = os.path.join(repo, rel)
        try:
            text = open(p, encoding="utf-8").read()
        except OSError:
            continue
        lines = text.split("\n")
        inm = macro_depth(text)
        globs = re.search(r"^\s*(pub(\([^)]*\))?\s+)?use\s[^;]*\*\s*;", text, re.M) is not None
        imported = set(re.findall(r"\b([A-Za-z_][A-Za-z0-9_]*)\b", " ".join(re.findall(r"^\s*(?:pub(?:\([^)]*\))?\s+)?use\s[^;]*;", text, re.M))))
        for o in occs:
            sym = o.get("sym", "")
            if o["role"] & 1 or sym not in defs or "range" not in o:
                continue
            l0, c0, l1, c1 = o["range"]
            if l0 != l1 or l0 >= len(lines):
                continue
            line = lines[l0]
            if not re.match(r"\s*(::\s*<[^()]*>)?\s*\(", line[c1:]):
                continue  # not a call
            if re.match(r"\s*(pub(\([^)]*\))?\s+)?use\b", line):
                continue
            ds = defs[sym]
            if len(ds) > 1:
                same = [x for x in ds if target(x[0]) == target(rel)]
                ds = same if same else ds
            amb = len(ds) > 1
            name = line[c0:c1]
            dstname = FN.search(sym).group(1)
            for d in ds:
                shape = classify(text, lines, l0, c0, c1, name, dstname, rel, d[0], inm(l0, c0), globs, imported)
                row = {"src": rel, "line": l0 + 1, "shape": shape, "dst": d[0], "name": dstname,
                       "dst_start": d[1], "dst_end": d[2], "owner": owner(sym)}
                if amb:
                    row["ambiguous"] = True
                rows.append(row)
    rows.sort(key=lambda r: (r["src"], r["line"], r["dst"], r["name"], r["dst_start"]))
    for r in rows:
        out.write(json.dumps(r) + "\n")


if __name__ == "__main__":
    main()
