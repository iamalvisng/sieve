//! The module map and the cross-file lookup for exact Python call links
//! (cross-file Python proof).
//! A lookup gives a target only when the chain of bindings is unique. Any
//! doubt gives `Unknown`, and the call keeps the capped name path.

use std::collections::{HashMap, HashSet};

use sieve_core::{Kind, Node};

use crate::extract::{PyBind, PyRec, RawEdge};
use crate::py_stdlib::is_stdlib;

/// Links a from-import chain may follow.
const MAX_DEPTH: u32 = 8;
/// Lookups one call may spend.
const MAX_LOOKUPS: u32 = 256;

/// One module of the repo: a `.py` file, or a namespace package directory.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Mod {
    File(String),
    Dir(String),
}

/// What a module-name lookup gives.
enum At {
    Found(Mod),
    Absent,
    Stop,
}

/// The module-level facts of one file.
struct Info {
    binds: HashMap<String, PyBind>,
    dotted: Vec<String>,
    sys_path: bool,
    broken: bool,
    opaque: bool,
}

/// What the lookup of one call gives.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Exactly one def or class, by node id.
    Target(String),
    /// A repo module is named and does not bind the name: no edge at all.
    Drop,
    /// No proof: the call keeps the capped name path.
    Unknown,
}

/// What one name means in one module.
enum Val {
    Def(String),
    Module(Mod),
    /// The module does not bind the name.
    Zero,
    /// A from-import of a repo module that lacks the name.
    Missing,
    Many,
}

/// The module map of a repo's Python files.
#[derive(Default)]
pub struct PyIndex {
    files: HashMap<String, Info>,
    paths: HashSet<String>,
    pyi: HashSet<String>,
    /// Every directory that holds a `.py` file, directly or below.
    dirs: HashSet<String>,
    /// A first name to the parents of its `<first>/__init__.py` or `<first>.py`.
    regular: HashMap<String, Vec<String>>,
    /// A directory name to its parents, for namespace packages.
    nested: HashMap<String, Vec<String>>,
    /// Each `(module, name)` that a `X.f = ...` write touches.
    patched: HashSet<(Mod, String)>,
    /// Names that a `patch("a.b.name")` string target patches.
    patched_names: HashSet<String>,
}

fn join(dir: &str, seg: &str) -> String {
    if dir.is_empty() {
        seg.to_string()
    } else {
        format!("{dir}/{seg}")
    }
}

fn dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn push_unique(map: &mut HashMap<String, Vec<String>>, key: &str, value: &str) {
    let list = map.entry(key.to_string()).or_default();
    if !list.iter().any(|v| v == value) {
        list.push(value.to_string());
    }
}

impl PyIndex {
    /// Reads the File nodes and the file records, then finds the module
    /// attributes that the repo writes to.
    pub fn build(nodes: &[Node], raw: &[RawEdge]) -> Self {
        let mut ix = Self::default();
        for e in raw {
            let Some(PyRec::File {
                binds,
                dotted,
                sys_path,
                broken,
                opaque,
                ..
            }) = &e.py
            else {
                continue;
            };
            ix.files.insert(
                e.file.clone(),
                Info {
                    binds: binds.iter().cloned().collect(),
                    dotted: dotted.clone(),
                    sys_path: *sys_path,
                    broken: *broken,
                    opaque: *opaque,
                },
            );
        }
        if ix.files.is_empty() {
            return ix;
        }
        for n in nodes.iter().filter(|n| n.kind == Kind::File) {
            ix.add_path(&n.path);
        }
        let mut patched = HashSet::new();
        for e in raw {
            let Some(PyRec::File { patches, .. }) = &e.py else {
                continue;
            };
            for (chain, attr) in patches {
                // An empty chain is the file itself. The chain `["*"]` is a
                // name that a string patch target names in any module.
                if chain.is_empty() {
                    patched.insert((Mod::File(e.file.clone()), attr.clone()));
                    continue;
                }
                if chain[..] == ["*".to_string()] {
                    ix.patched_names.insert(attr.clone());
                    continue;
                }
                let mut w = Walk::new(&ix);
                if let Some(m) = ix.chain_module(&mut w, &e.file, chain) {
                    patched.insert((m, attr.clone()));
                }
            }
        }
        ix.patched = patched;
        ix
    }

    fn add_path(&mut self, path: &str) {
        if path.ends_with(".pyi") {
            self.pyi.insert(path.to_string());
            return;
        }
        let Some(stem) = path.strip_suffix(".py") else {
            return;
        };
        self.paths.insert(path.to_string());
        let mut dir = dirname(path);
        while !dir.is_empty() {
            self.dirs.insert(dir.to_string());
            let name = dir.rsplit('/').next().unwrap_or(dir);
            push_unique(&mut self.nested, name, dirname(dir));
            dir = dirname(dir);
        }
        let file = stem.rsplit('/').next().unwrap_or(stem);
        if file == "__init__" {
            let pkg = dirname(path);
            let name = pkg.rsplit('/').next().unwrap_or(pkg);
            if !pkg.is_empty() {
                push_unique(&mut self.regular, name, dirname(pkg));
            }
        } else {
            push_unique(&mut self.regular, file, dirname(path));
        }
    }

    /// The module at `segs` below the directory `p`.
    fn module_at(&self, p: &str, segs: &[&str]) -> At {
        let path = segs.iter().fold(p.to_string(), |d, s| join(&d, s));
        let init = join(&path, "__init__.py");
        let file = format!("{path}.py");
        let has_init = self.paths.contains(&init);
        let has_file = !segs.is_empty() && self.paths.contains(&file);
        match (has_init, has_file) {
            (true, true) => At::Stop,
            (true, false) => At::Found(Mod::File(init)),
            (false, true) => At::Found(Mod::File(file)),
            _ if self.dirs.contains(&path) => At::Found(Mod::Dir(path)),
            _ => At::Absent,
        }
    }

    /// The module for an absolute dotted name imported by `from`.
    fn find_abs(&self, from: &str, dotted: &str) -> At {
        if self.files.get(from).is_none_or(|i| i.sys_path) {
            return At::Stop;
        }
        let segs: Vec<&str> = dotted.split('.').collect();
        let first = segs[0];
        if first.is_empty() || is_stdlib(first) {
            return At::Stop;
        }
        match self.regular.get(first).map(Vec::as_slice) {
            Some(roots) if !self.near_roots(from, first, roots).is_empty() => {
                match self.near_roots(from, first, roots)[..] {
                    [root] => self.module_at(root, &segs),
                    _ => At::Stop,
                }
            }
            _ => {
                let mut hits = self
                    .nested
                    .get(first)
                    .into_iter()
                    .flatten()
                    .filter_map(|p| match self.module_at(p, &segs) {
                        At::Found(m) => Some(m),
                        _ => None,
                    });
                match (hits.next(), hits.next()) {
                    (Some(m), None) => At::Found(m),
                    (None, _) => At::Absent,
                    _ => At::Stop,
                }
            }
        }
    }

    /// The roots in `roots` that a file `from` can import `first` from. A
    /// lone `first.py` counts only if its dir is the dir of `from` or an
    /// ancestor of it.
    fn near_roots<'r>(&self, from: &str, first: &str, roots: &'r [String]) -> Vec<&'r str> {
        let here = dirname(from);
        roots
            .iter()
            .map(String::as_str)
            .filter(|r| {
                self.paths.contains(&join(&join(r, first), "__init__.py"))
                    || r.is_empty()
                    || here == *r
                    || here.starts_with(&format!("{r}/"))
            })
            .collect()
    }

    /// The module for a relative import of `text` (`..a.b`) in `from`.
    fn find_rel(&self, from: &str, text: &str) -> At {
        let rest = text.trim_start_matches('.');
        let level = text.len() - rest.len();
        let mut dir = dirname(from).to_string();
        let has_init = |d: &str| self.paths.contains(&join(d, "__init__.py"));
        if !has_init(&dir) {
            return At::Stop;
        }
        for _ in 1..level {
            if dir.is_empty() {
                return At::Stop;
            }
            dir = dirname(&dir).to_string();
            if !has_init(&dir) {
                return At::Stop;
            }
        }
        let segs: Vec<&str> = rest.split('.').filter(|s| !s.is_empty()).collect();
        self.module_at(&dir, &segs)
    }

    fn find(&self, from: &str, module: &str) -> At {
        if module.starts_with('.') {
            self.find_rel(from, module)
        } else {
            self.find_abs(from, module)
        }
    }

    /// The submodule `name` of the package `m`, if any.
    fn submodule(&self, m: &Mod, name: &str) -> At {
        let dir = match m {
            Mod::Dir(d) => d.as_str(),
            Mod::File(f) if f.ends_with("__init__.py") => dirname(f),
            Mod::File(_) => return At::Absent,
        };
        self.module_at(dir, &[name])
    }

    /// The module that the receiver chain of a call names, in file `file`.
    fn chain_module(&self, w: &mut Walk, file: &str, chain: &[String]) -> Option<Mod> {
        let me = Mod::File(file.to_string());
        if chain.len() == 1 {
            return match w.bind(&me, &chain[0], 0) {
                Val::Module(m) => Some(m),
                _ => None,
            };
        }
        let info = self.files.get(file)?;
        let Some(PyBind::Module { plain: true, .. }) = info.binds.get(&chain[0]) else {
            return None;
        };
        let mut name = chain[0].clone();
        for seg in &chain[1..] {
            let At::Found(parent) = self.find(file, &name) else {
                return None;
            };
            if !matches!(w.bind(&parent, seg, 0), Val::Zero) {
                return None;
            }
            name = format!("{name}.{seg}");
            let sub = format!("{name}.");
            if !info
                .dotted
                .iter()
                .any(|d| *d == name || d.starts_with(&sub))
            {
                return None;
            }
        }
        match self.find(file, &name) {
            At::Found(m) => Some(m),
            _ => None,
        }
    }

    /// Resolves the call `chain.name()` in `file`. An empty `chain` is a
    /// bare call.
    pub fn resolve_call(&self, file: &str, chain: &[String], name: &str) -> Outcome {
        let Some(info) = self.files.get(file).filter(|i| !i.broken) else {
            return Outcome::Unknown;
        };
        let mut w = Walk::new(self);
        if chain.is_empty() {
            // The local name of an alias is free, so a name match on
            // it means nothing.
            let alias = matches!(
                info.binds.get(name),
                Some(PyBind::From { member, .. }) if member != name
            );
            return match w.bind(&Mod::File(file.to_string()), name, 0) {
                Val::Def(t) => Outcome::Target(t),
                Val::Missing if alias => Outcome::Drop,
                _ => Outcome::Unknown,
            };
        }
        let Some(m) = self.chain_module(&mut w, file, chain) else {
            return Outcome::Unknown;
        };
        // A sibling submodule `m/name.py` may be the attribute.
        if !matches!(self.submodule(&m, name), At::Absent) {
            return Outcome::Unknown;
        }
        match w.bind(&m, name, 0) {
            Val::Def(t) => Outcome::Target(t),
            Val::Zero => Outcome::Drop,
            _ => Outcome::Unknown,
        }
    }
}

/// The state of one lookup: the budget and the cycle stack.
struct Walk<'a> {
    ix: &'a PyIndex,
    budget: u32,
    stack: Vec<(Mod, String)>,
}

impl<'a> Walk<'a> {
    fn new(ix: &'a PyIndex) -> Self {
        Walk {
            ix,
            budget: MAX_LOOKUPS,
            stack: Vec::new(),
        }
    }

    /// B(m, x): what the name `x` means at the module level of `m`.
    fn bind(&mut self, m: &Mod, x: &str, depth: u32) -> Val {
        if depth > MAX_DEPTH || self.budget == 0 {
            return Val::Many;
        }
        self.budget -= 1;
        if self.stack.iter().any(|(sm, sx)| sm == m && sx == x) {
            return Val::Many;
        }
        self.stack.push((m.clone(), x.to_string()));
        let v = self.bind_in(m, x, depth);
        self.stack.pop();
        v
    }

    fn bind_in(&mut self, m: &Mod, x: &str, depth: u32) -> Val {
        let ix = self.ix;
        let Mod::File(f) = m else {
            return Val::Zero;
        };
        if ix.pyi.contains(&format!("{f}i")) {
            return Val::Many;
        }
        let Some(info) = ix.files.get(f) else {
            return Val::Many;
        };
        if info.broken
            || info.opaque
            || ix.patched_names.contains(x)
            || ix.patched.contains(&(m.clone(), x.to_string()))
            || ix.patched.contains(&(m.clone(), "*".to_string()))
        {
            return Val::Many;
        }
        match info.binds.get(x) {
            None => Val::Zero,
            Some(PyBind::Node { target }) => Val::Def(target.clone()),
            Some(PyBind::Module { module, plain }) => {
                let name = if *plain { x } else { module.as_str() };
                match ix.find_abs(f, name) {
                    At::Found(t) => Val::Module(t),
                    _ => Val::Many,
                }
            }
            Some(PyBind::From { module, member }) => match ix.find(f, module) {
                At::Found(t) => self.member(&t, member, depth + 1),
                _ => Val::Many,
            },
            Some(PyBind::Def { .. } | PyBind::Other) => Val::Many,
        }
    }

    /// `from t import name`. The name is a submodule only if `t`
    /// binds nothing under it.
    fn member(&mut self, t: &Mod, name: &str, depth: u32) -> Val {
        let v = self.bind(t, name, depth);
        match (v, self.ix.submodule(t, name)) {
            (_, At::Stop) => Val::Many,
            (Val::Zero, At::Found(s)) => Val::Module(s),
            (Val::Zero, At::Absent) => Val::Missing,
            (_, At::Found(_)) => Val::Many,
            (v, At::Absent) => v,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::extract::Extractor;
    use crate::resolve::resolve_edges;
    use sieve_core::{span, Confidence, Kind, Node, Origin, Relation, SummaryState};

    type Calls = Vec<(String, String, Confidence)>;

    fn file_node(path: &str) -> Node {
        Node {
            id: path.to_string(),
            name: path.rsplit('/').next().unwrap_or(path).to_string(),
            kind: Kind::File,
            owner: None,
            path: path.to_string(),
            span: span(1, 1),
            signature: None,
            exported: true,
            origin: Origin::Ast,
            body_hash: String::new(),
            chars: None,
            body_text: None,
            arity: None,
            variadic: None,
            summary_state: SummaryState::Pending,
            summary: None,
            crux: None,
        }
    }

    /// The `calls` edges of a small repo. A `.pyi` path mints only a File node.
    fn run(files: &[(&str, &str)]) -> Calls {
        let mut ext = Extractor::new().expect("extractor builds");
        let mut nodes = Vec::new();
        let mut raw = Vec::new();
        for (path, src) in files {
            nodes.push(file_node(path));
            if path.ends_with(".py") {
                let (n, r) = ext.extract_file(path, src, "python");
                nodes.extend(n);
                raw.extend(r);
            }
        }
        resolve_edges(&nodes, &raw)
            .into_iter()
            .filter(|e| e.relation == Relation::Calls)
            .map(|e| (e.source, e.target, e.confidence))
            .collect()
    }

    fn extracted(got: &Calls, from: &str, to: &str) -> bool {
        got.iter()
            .any(|(s, t, c)| s == from && t == to && *c == Confidence::Extracted)
    }

    fn none_extracted(got: &Calls) {
        assert!(
            got.iter().all(|(_, _, c)| *c != Confidence::Extracted),
            "{got:?}"
        );
    }

    const LIB: (&str, &str) = ("lib.py", "def f():\n    pass\n");

    #[test]
    fn test_pyx2_from_import() {
        let got = run(&[LIB, ("main.py", "from lib import f\ndef g():\n    f()\n")]);
        assert!(extracted(&got, "main.py#g", "lib.py#f"), "{got:?}");
    }

    #[test]
    fn test_pyx2_alias() {
        let got = run(&[
            LIB,
            ("main.py", "from lib import f as h\ndef g():\n    h()\n"),
        ]);
        assert!(extracted(&got, "main.py#g", "lib.py#f"), "{got:?}");
    }

    #[test]
    fn test_pyx2_alias_absent_drops() {
        let got = run(&[
            ("lib.py", "def other():\n    pass\n"),
            ("o.py", "def h():\n    pass\n"),
            ("main.py", "from lib import f as h\ndef g():\n    h()\n"),
        ]);
        assert!(got.iter().all(|(s, _, _)| s != "main.py#g"), "{got:?}");
    }

    #[test]
    fn test_pyx2_relative_levels() {
        let got = run(&[
            ("pkg/__init__.py", ""),
            ("pkg/sub/__init__.py", ""),
            ("pkg/util.py", "def u():\n    pass\n"),
            ("pkg/sub/sib.py", "def s():\n    pass\n"),
            (
                "pkg/sub/m.py",
                "from ..util import u\nfrom .sib import s\ndef g():\n    u()\n    s()\n",
            ),
        ]);
        assert!(
            extracted(&got, "pkg/sub/m.py#g", "pkg/util.py#u"),
            "{got:?}"
        );
        assert!(
            extracted(&got, "pkg/sub/m.py#g", "pkg/sub/sib.py#s"),
            "{got:?}"
        );
    }

    #[test]
    fn test_pyx2_relative_outside_package_stops() {
        let got = run(&[
            ("x/u.py", "def f():\n    pass\n"),
            ("x/m.py", "from .u import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
        // A climb above the package root stops too.
        let got = run(&[
            ("pkg/__init__.py", ""),
            ("u.py", "def f():\n    pass\n"),
            ("pkg/m.py", "from ..u import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_init_reexport() {
        let got = run(&[
            ("pkg/__init__.py", "from .core import f\n"),
            ("pkg/core.py", "def f():\n    pass\n"),
            ("main.py", "from pkg import f\ndef g():\n    f()\n"),
        ]);
        assert!(extracted(&got, "main.py#g", "pkg/core.py#f"), "{got:?}");
    }

    #[test]
    fn test_pyx2_src_layout() {
        let got = run(&[
            ("src/pkg/__init__.py", ""),
            ("src/pkg/core.py", "def f():\n    pass\n"),
            ("main.py", "from pkg.core import f\ndef g():\n    f()\n"),
        ]);
        assert!(extracted(&got, "main.py#g", "src/pkg/core.py#f"), "{got:?}");
    }

    #[test]
    fn test_pyx2_monorepo_subdir_root() {
        let got = run(&[
            ("proj/pkg/__init__.py", ""),
            ("proj/pkg/m.py", "def f():\n    pass\n"),
            ("svc/main.py", "from pkg.m import f\ndef g():\n    f()\n"),
        ]);
        assert!(
            extracted(&got, "svc/main.py#g", "proj/pkg/m.py#f"),
            "{got:?}"
        );
    }

    #[test]
    fn test_pyx2_two_roots_stop() {
        let got = run(&[
            ("a/pkg/__init__.py", ""),
            ("a/pkg/m.py", "def f():\n    pass\n"),
            ("b/pkg/__init__.py", ""),
            ("b/pkg/m.py", "def f():\n    pass\n"),
            ("main.py", "from pkg.m import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_stdlib_name_stops() {
        let got = run(&[
            ("json.py", "def loads():\n    pass\n"),
            ("main.py", "from json import loads\ndef g():\n    loads()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_try_import_stops() {
        let src =
            "try:\n    from lib import f\nexcept ImportError:\n    f = None\ndef g():\n    f()\n";
        none_extracted(&run(&[LIB, ("main.py", src)]));
    }

    #[test]
    fn test_pyx2_type_checking_stops() {
        let src = "if TYPE_CHECKING:\n    from lib import f\ndef g():\n    f()\n";
        none_extracted(&run(&[LIB, ("main.py", src)]));
    }

    #[test]
    fn test_pyx2_module_attr_call() {
        let got = run(&[
            LIB,
            (
                "main.py",
                "import lib\nimport lib as m\ndef g():\n    lib.f()\n    m.f()\n",
            ),
        ]);
        assert!(extracted(&got, "main.py#g", "lib.py#f"), "{got:?}");
        // A repo module that lacks the name leaves no edge.
        let got = run(&[
            LIB,
            ("o.py", "def nope():\n    pass\n"),
            ("main.py", "import lib\ndef g():\n    lib.nope()\n"),
        ]);
        assert!(got.iter().all(|(s, _, _)| s != "main.py#g"), "{got:?}");
    }

    #[test]
    fn test_pyx2_import_a_b_binds_a() {
        let pkg = [
            ("pkg/__init__.py", ""),
            ("pkg/sub.py", "def f():\n    pass\n"),
        ];
        let mut files = pkg.to_vec();
        files.push(("main.py", "import pkg.sub\ndef g():\n    pkg.sub.f()\n"));
        let got = run(&files);
        assert!(extracted(&got, "main.py#g", "pkg/sub.py#f"), "{got:?}");
        let mut files = pkg.to_vec();
        files.push(("main.py", "import pkg.sub as s\ndef g():\n    s.f()\n"));
        let got = run(&files);
        assert!(extracted(&got, "main.py#g", "pkg/sub.py#f"), "{got:?}");
    }

    #[test]
    fn test_pyx2_submodule_and_attr_stop() {
        // `import pkg` alone does not bind `pkg.sub`.
        let got = run(&[
            ("pkg/__init__.py", ""),
            ("pkg/sub.py", "def f():\n    pass\n"),
            ("main.py", "import pkg\ndef g():\n    pkg.sub.f()\n"),
        ]);
        none_extracted(&got);
        // The package binds `sub` and has a submodule `sub`: both, so stop.
        let got = run(&[
            ("pkg/__init__.py", "sub = 1\n"),
            ("pkg/sub.py", "def f():\n    pass\n"),
            ("main.py", "from pkg import sub\ndef g():\n    sub.f()\n"),
        ]);
        none_extracted(&got);
        // Only a submodule: a proof.
        let got = run(&[
            ("pkg/__init__.py", ""),
            ("pkg/sub.py", "def f():\n    pass\n"),
            ("main.py", "from pkg import sub\ndef g():\n    sub.f()\n"),
        ]);
        assert!(extracted(&got, "main.py#g", "pkg/sub.py#f"), "{got:?}");
    }

    #[test]
    fn test_pyx2_cycle_stops() {
        let got = run(&[
            ("a.py", "from b import f\n"),
            ("b.py", "from a import f\n"),
            ("c.py", "from a import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_monkey_patch_stops() {
        let got = run(&[
            LIB,
            ("p.py", "import lib\nlib.f = lambda: 1\n"),
            ("main.py", "from lib import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_sys_path_stops() {
        let src = "import sys\nsys.path.insert(0, 'x')\nfrom lib import f\ndef g():\n    f()\n";
        none_extracted(&run(&[LIB, ("main.py", src)]));
    }

    #[test]
    fn test_pyx2_pyi_sibling_stops() {
        let got = run(&[
            LIB,
            ("lib.pyi", ""),
            ("main.py", "from lib import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_empty_init_file() {
        let pkg = [
            ("pkg/u.py", "def f():\n    pass\n"),
            ("pkg/m.py", "from .u import f\ndef g():\n    f()\n"),
            ("main.py", "from pkg.u import f\ndef h():\n    f()\n"),
        ];
        // With a File node for the empty `__init__.py`, both imports prove.
        let mut files = pkg.to_vec();
        files.push(("pkg/__init__.py", ""));
        let got = run(&files);
        assert!(extracted(&got, "pkg/m.py#g", "pkg/u.py#f"), "{got:?}");
        assert!(extracted(&got, "main.py#h", "pkg/u.py#f"), "{got:?}");
        // Without it, the relative import stops. The namespace package still
        // proves the absolute import.
        let got = run(&pkg);
        assert!(!extracted(&got, "pkg/m.py#g", "pkg/u.py#f"), "{got:?}");
        assert!(extracted(&got, "main.py#h", "pkg/u.py#f"), "{got:?}");
    }

    #[test]
    fn test_pyx2_dunder_main_stops() {
        let got = run(&[
            ("__main__.py", "def f():\n    pass\n"),
            ("main.py", "from __main__ import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_lone_module_elsewhere_does_not_shadow_third_party() {
        let got = run(&[
            ("examples/requests.py", "def get():\n    pass\n"),
            (
                "src/app.py",
                "import requests\ndef g():\n    requests.get()\n",
            ),
        ]);
        none_extracted(&got);
        let got = run(&[
            ("examples/requests.py", "def get():\n    pass\n"),
            (
                "examples/app.py",
                "import requests\ndef g():\n    requests.get()\n",
            ),
        ]);
        assert!(
            extracted(&got, "examples/app.py#g", "examples/requests.py#get"),
            "{got:?}"
        );
    }

    #[test]
    fn test_pyx2_attr_with_submodule_sibling_stops() {
        let got = run(&[
            ("a/__init__.py", "def g():\n    pass\n"),
            ("a/g.py", ""),
            ("main.py", "import a\ndef h():\n    a.g()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_setattr_on_module_stops() {
        let got = run(&[
            LIB,
            ("p.py", "import lib\nsetattr(lib, 'f', None)\n"),
            ("main.py", "from lib import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_mock_patch_stops() {
        let got = run(&[
            LIB,
            (
                "t.py",
                "from unittest import mock\n@mock.patch('lib.f')\ndef t(m):\n    pass\n",
            ),
            ("main.py", "from lib import f\ndef g():\n    f()\n"),
        ]);
        none_extracted(&got);
    }

    #[test]
    fn test_pyx2_from_sys_import_path_stops() {
        let src =
            "from sys import path\npath.insert(0, 'x')\nfrom lib import f\ndef g():\n    f()\n";
        none_extracted(&run(&[LIB, ("main.py", src)]));
    }
}
