#!/usr/bin/env python3
"""Oracle for Python call links. Usage: python3 -I py-calls.py <repo> [out.jsonl]

Runs scip-python with an empty package environment (`--environment` file holds
`[]`, so no host pip introspection) and a scratch pyrightconfig.json
(extraPaths ["src"], pythonVersion "3.12"). The config is written into <repo>
only if absent and removed after the run. Finds call sites with `ast`, looks
up the SCIP reference at each callee name, and writes one JSON row per call:
  {src, line, shape, dst, dst_line, name}   (lines 1-based, paths relative)
dst_line is the line of the target definition name (the `def`/`class` line).
A symbol with 2 or more definitions: every row has "ambiguous": true.
A call to a class also writes a row for its __init__ when SCIP defines it.
A function used as a value in an argument position writes a row with shape
"ref_" + the call shape (ref_bare, ref_self_method, ...).
Shapes: bare, local_def, from_import, alias, relative, reexport, star,
module_attr, ctor, class_attr, self_method, typed_local, member.
Env SCIP_PYTHON=<index.js> overrides the scip-python path; SCIP_KEEP=<path>
keeps the SCIP file. Reuses the SCIP reader of rs-calls.py (copied, stdlib).
"""
import ast, json, os, re, subprocess, sys, tempfile, warnings

warnings.simplefilter("ignore")

SP = os.environ.get("SCIP_PYTHON", os.path.expanduser(
    "~/.local/share/sieve-oracle/scip-python/node_modules/@sourcegraph/scip-python/index.js"))


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
                o = {"role": 0}
                for h, wt, x in fields(w):
                    if h == 1: o["range"] = packed(x)
                    elif h == 2: o["sym"] = x.decode()
                    elif h == 3: o["role"] = x
                occs.append(o)
        docs.append((rel, occs))
    return docs


DESC = re.compile(r"([^/` .#()]+|`[^`]+`)(\(\)\.|#)$")


def sym_info(sym):
    """(name, kind) for a function/method ('f') or class ('c') symbol, else None."""
    m = DESC.search(sym)
    if not m:
        return None
    return m.group(1).strip("`"), "f" if m.group(2) == "()." else "c"


def build_ctx(tree):
    """Import facts: from-imported names -> (level, has_alias, is_module_guess), module names, star?"""
    froms, mods, star = {}, set(), False
    for n in ast.walk(tree):
        if isinstance(n, ast.ImportFrom):
            for a in n.names:
                if a.name == "*":
                    star = True
                else:
                    froms[a.asname or a.name] = (n.level, a.asname is not None)
        elif isinstance(n, ast.Import):
            for a in n.names:
                mods.add(a.asname or a.name.split(".")[0])
    return froms, mods, star


def main():
    repo = os.path.realpath(sys.argv[1])
    out = open(sys.argv[2], "w") if len(sys.argv) > 2 else sys.stdout
    with tempfile.TemporaryDirectory() as td:
        env, scip = os.path.join(td, "env.json"), os.path.join(td, "index.scip")
        json.dump([], open(env, "w"))
        cfgp = os.path.join(repo, "pyrightconfig.json")
        made = not os.path.exists(cfgp)
        if made:
            json.dump({"extraPaths": ["src"], "pythonVersion": "3.12"}, open(cfgp, "w"))
        try:
            r = subprocess.run(["node", SP, "index", "--cwd", repo, "--project-name", "oracle",
                                "--project-version", "0", "--environment", env, "--output", scip,
                                "--quiet"], cwd=repo, stdout=subprocess.DEVNULL,
                               stderr=subprocess.PIPE, text=True)
        finally:
            if made:
                os.remove(cfgp)
        if r.returncode:
            sys.exit("scip-python failed: " + r.stderr[-500:])
        docs = read_scip(scip)
        if os.environ.get("SCIP_KEEP"):
            import shutil; shutil.copy(scip, os.environ["SCIP_KEEP"])
    defs = {}  # symbol -> [(file, line1)]
    for rel, occs in docs:
        for o in occs:
            if o["role"] & 1 and "range" in o and sym_info(o.get("sym", "")):
                defs.setdefault(o["sym"], []).append((rel, o["range"][0] + 1))
    rows = []
    for rel, occs in docs:
        if not rel.endswith(".py"):
            continue
        try:
            text = open(os.path.join(repo, rel), encoding="utf-8").read()
            tree = ast.parse(text)
        except (OSError, SyntaxError, ValueError):
            continue
        lines = text.split("\n")
        at = {}  # (line0, col) -> symbol, references only
        for o in occs:
            if not o["role"] & 1 and "range" in o and o.get("sym"):
                at[(o["range"][0], o["range"][1])] = o["sym"]
        froms, mods, star = build_ctx(tree)
        sites = []  # (callee-or-argument node, is a function used as a value)
        for n in ast.walk(tree):
            if isinstance(n, ast.Call):
                sites.append((n.func, False))
                sites += [(a, True) for a in n.args + [k.value for k in n.keywords]
                          if isinstance(a, (ast.Name, ast.Attribute))]
        for f, ref in sites:
            if isinstance(f, ast.Name):
                l0, c0, nm = f.lineno - 1, f.col_offset, f.id
            elif isinstance(f, ast.Attribute) and f.end_lineno == f.lineno:
                l0, c0, nm = f.end_lineno - 1, f.end_col_offset - len(f.attr), f.attr
            else:
                continue
            sym = at.get((l0, c0))
            if sym not in defs:
                continue
            ds = defs[sym]
            name, kind = sym_info(sym)
            if ref and kind != "f":
                continue
            if isinstance(f, ast.Name):
                if kind == "c": shape = "ctor"
                elif ds[0][0] == rel: shape = "local_def"
                elif nm in froms:
                    lvl, al = froms[nm]
                    shape = ("relative" if lvl else "alias" if al
                             else "reexport" if ds[0][0].endswith("__init__.py") else "from_import")
                elif star: shape = "star"
                else: shape = "bare"
            else:
                v = f.value
                if kind == "c": shape = "ctor"
                elif isinstance(v, ast.Name) and v.id in ("self", "cls"): shape = "self_method"
                elif isinstance(v, ast.Name) and v.id in mods | set(froms): 
                    vs = at.get((v.lineno - 1, v.col_offset), "")
                    shape = "class_attr" if vs.endswith("#") else "module_attr"
                elif isinstance(v, ast.Name):
                    vs = at.get((v.lineno - 1, v.col_offset), "")
                    shape = "class_attr" if vs.endswith("#") else "typed_local"
                else: shape = "member"
            if ref:
                shape = "ref_" + shape
            targets = [(ds, name)]
            if kind == "c":
                init = sym + "__init__()."
                if init in defs:
                    targets.append((defs[init], "__init__"))
            for d, tn in targets:
                for df, dl in d:
                    row = {"src": rel, "line": l0 + 1, "shape": shape, "dst": df, "dst_line": dl, "name": tn}
                    if len(d) > 1:
                        row["ambiguous"] = True
                    rows.append(row)
    rows.sort(key=lambda r: (r["src"], r["line"], r["dst"], r["name"], r["dst_line"]))
    for r in rows:
        out.write(json.dumps(r) + "\n")


if __name__ == "__main__":
    main()
