//! The package index of a repo's Go files (GOX2, design `go-xfile.md` rules
//! GO1, GO3, GO4 and the cross-file part of GO5). The resolver builds it
//! from the file records and asks it to prove each `GoCall`. A proof means
//! exactly 1 binding. Any doubt gives `None`, and the edge stays Inferred.

use std::collections::{HashMap, HashSet};

use crate::extract::{GoCall, GoFileRec, GoRec, GoTy, GoTyFrom, RawEdge};
use crate::resolve::GoModule;

/// A package key: the directory and the name in the package clause.
type Pkg = (String, String);

/// The packages of a repo's Go files.
#[derive(Default)]
pub struct GoIndex {
    files: HashMap<String, GoFileRec>,
    pkgs: HashMap<Pkg, Vec<String>>,
    /// A directory to the package clauses of its non-test files.
    clauses: HashMap<String, HashSet<String>>,
    /// A directory with a file that has a parse error.
    broken: HashSet<String>,
    modules: Vec<GoModule>,
}

/// The directory of a posix path, `""` for the repo root.
fn dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

fn is_test(path: &str) -> bool {
    path.ends_with("_test.go")
}

/// Reports whether `dir` is `base` or lies below it.
fn within(dir: &str, base: &str) -> bool {
    base.is_empty()
        || dir == base
        || dir
            .strip_prefix(base)
            .is_some_and(|rest| rest.starts_with('/'))
}

impl GoIndex {
    /// Reads the file records of `raw` and the Go modules of the repo.
    pub fn build(raw: &[RawEdge], modules: &[GoModule]) -> Self {
        let mut ix = GoIndex {
            modules: modules.to_vec(),
            ..GoIndex::default()
        };
        for e in raw {
            let Some(GoRec::File(rec)) = &e.go else {
                continue;
            };
            let dir = dirname(&e.file).to_string();
            if rec.broken {
                ix.broken.insert(dir.clone());
            }
            // The go tool skips a file in a `testdata`, `_x` or `.x` dir, a
            // file that starts with `_` or `.`, and a file with the build
            // line `ignore`.
            if !rec.ignore && !crate::go_tree::go_skipped(&e.file) {
                if !is_test(&e.file) {
                    ix.clauses
                        .entry(dir.clone())
                        .or_default()
                        .insert(rec.pkg.clone());
                }
                ix.pkgs
                    .entry((dir, rec.pkg.clone()))
                    .or_default()
                    .push(e.file.clone());
            }
            ix.files.insert(e.file.clone(), (**rec).clone());
        }
        ix
    }

    /// Reports whether the files of `dir` form clear packages (GO1): no
    /// parse error and at most 1 non-test package clause.
    fn dir_ok(&self, dir: &str) -> bool {
        !self.broken.contains(dir) && self.clauses.get(dir).is_none_or(|c| c.len() <= 1)
    }

    /// The package that holds the file `path`.
    fn pkg_of(&self, path: &str) -> Option<Pkg> {
        let rec = self.files.get(path)?;
        let key = (dirname(path).to_string(), rec.pkg.clone());
        self.pkgs
            .get(&key)?
            .iter()
            .any(|p| p == path)
            .then_some(key)
    }

    /// The files of `target` that the caller file `caller` sees. A test
    /// file is visible to a test caller in the same package. An external
    /// test package `p_test` also sees the test files of its own `p`.
    fn visible(&self, caller: &str, caller_pkg: &Pkg, target: &Pkg) -> Vec<&str> {
        let Some(files) = self.pkgs.get(target) else {
            return Vec::new();
        };
        let own_xtest = caller_pkg.0 == target.0 && caller_pkg.1 == format!("{}_test", target.1);
        let sees_tests = is_test(caller) && (caller_pkg == target || own_xtest);
        files
            .iter()
            .filter(|f| sees_tests || !is_test(f))
            .map(String::as_str)
            .collect()
    }

    /// The file that holds the one package-level binding of `name` in
    /// `files`. `None` for 0, or for 2 or more bindings.
    fn one_binding<'a>(&self, files: &[&'a str], name: &str) -> Option<&'a str> {
        let mut found = None;
        for f in files {
            let n = self.files.get(*f)?.names.iter().find(|(k, _)| k == name);
            match n {
                Some((_, 1)) if found.is_none() => found = Some(*f),
                Some(_) => return None,
                None => {}
            }
        }
        found
    }

    /// The directory that the import `spec` of the file `caller` names (GO3).
    fn import_dir(&self, caller: &str, spec: &str) -> Option<String> {
        let mut best: Option<&GoModule> = None;
        for m in &self.modules {
            let hit = spec == m.module
                || spec
                    .strip_prefix(&m.module)
                    .is_some_and(|rest| rest.starts_with('/'));
            if hit && best.is_none_or(|b| m.module.len() > b.module.len()) {
                best = Some(m);
            }
        }
        let best = best?;
        // 2 go.mod files with one module path stop.
        if self
            .modules
            .iter()
            .filter(|m| m.module == best.module)
            .count()
            > 1
        {
            return None;
        }
        let sub = spec[best.module.len()..].trim_start_matches('/');
        let dir = match (best.dir.is_empty(), sub.is_empty()) {
            (_, true) => best.dir.clone(),
            (true, false) => sub.to_string(),
            (false, false) => format!("{}/{sub}", best.dir),
        };
        let owner = |d: &str| {
            self.modules
                .iter()
                .filter(|m| within(d, &m.dir))
                .max_by_key(|m| m.dir.len())
        };
        // The dir must belong to the module, not to a nested module.
        if owner(&dir)?.dir != best.dir {
            return None;
        }
        let caller_mod = owner(dirname(caller))?;
        if caller_mod.dir != best.dir {
            let linked = caller_mod
                .links
                .iter()
                .any(|(path, d)| *d == best.dir && (path.is_empty() || *path == best.module));
            if !linked {
                return None;
            }
        }
        Some(dir)
    }

    /// The package that the import `spec` of `caller` names: the one
    /// non-test package of the target dir.
    fn import_pkg(&self, caller: &str, spec: &str) -> Option<Pkg> {
        let dir = self.import_dir(caller, spec)?;
        if !self.dir_ok(&dir) {
            return None;
        }
        let clause = self.clauses.get(&dir)?.iter().next()?;
        (clause != "main").then(|| (dir, clause.clone()))
    }

    /// The package that the file-scope name `x` of `caller` names, when
    /// exactly 1 import spec has the local name `x` (GO4). The local name is
    /// the alias, else the package clause of the target dir.
    fn qual_pkg(&self, caller: &str, x: &str) -> Option<Pkg> {
        let rec = self.files.get(caller)?;
        let mut hit = None;
        for (alias, path) in &rec.imports {
            let pkg = match alias.as_deref() {
                Some("_" | ".") => continue,
                Some(a) if a != x => continue,
                Some(_) => self.import_pkg(caller, path)?,
                None => match self.import_pkg(caller, path) {
                    Some(p) if p.1 == x => p,
                    _ => continue,
                },
            };
            if hit.is_some() {
                return None;
            }
            hit = Some(pkg);
        }
        hit
    }

    /// The node id that the call `name` in the file `caller` reaches, or
    /// `None` when the index does not prove exactly 1 target.
    pub fn resolve(&self, caller: &str, call: &GoCall, name: &str) -> Option<String> {
        let rec = self.files.get(caller)?;
        if rec.ignore || rec.broken || rec.dot || !self.dir_ok(dirname(caller)) {
            return None;
        }
        let mine = self.pkg_of(caller)?;
        match call {
            GoCall::Bare => {
                let files = self.visible(caller, &mine, &mine);
                self.func_id(&files, name)
            }
            GoCall::Qual { pkg } => {
                let target = self.qual_pkg(caller, pkg)?;
                let files = self.visible(caller, &mine, &target);
                self.func_id(&files, name)
            }
            GoCall::Method { ty, from } => self.method_id(caller, &mine, ty, *from, name),
        }
    }

    /// The id of the one func `name` in `files`.
    fn func_id(&self, files: &[&str], name: &str) -> Option<String> {
        let f = self.one_binding(files, name)?;
        let rec = self.files.get(f)?;
        let is_func = rec.funcs.iter().any(|(n, _)| n == name);
        is_func.then(|| format!("{f}#{name}"))
    }

    /// Reports whether a visible file of the caller's package binds `new`.
    fn new_is_bound(&self, caller: &str, mine: &Pkg) -> bool {
        self.visible(caller, mine, mine).iter().any(|f| {
            self.files
                .get(*f)
                .is_none_or(|r| r.names.iter().any(|(k, _)| k == "new"))
        })
    }

    /// The id of the one method `name` of the type that `ty` and `from` give.
    fn method_id(
        &self,
        caller: &str,
        mine: &Pkg,
        ty: &GoTy,
        from: GoTyFrom,
        name: &str,
    ) -> Option<String> {
        let tpkg = match &ty.qual {
            None => mine.clone(),
            Some(q) => self.qual_pkg(caller, q)?,
        };
        let files = self.visible(caller, mine, &tpkg);
        // B5: a package-level `new` hides the builtin in the whole package.
        if from == GoTyFrom::New && self.new_is_bound(caller, mine) {
            return None;
        }
        let tname = match from {
            GoTyFrom::Written | GoTyFrom::New => ty.name.clone(),
            GoTyFrom::Ctor => {
                // The ctor result is a plain name in the ctor's package.
                let f = self.one_binding(&files, &ty.name)?;
                let funcs = &self.files.get(f)?.funcs;
                funcs.iter().find(|(n, _)| *n == ty.name)?.1.clone()?
            }
        };
        let holder = self.one_binding(&files, &tname)?;
        let t = self
            .files
            .get(holder)?
            .types
            .iter()
            .find(|t| t.name == tname)?;
        if !t.plain || t.fields.iter().any(|f| f == name) {
            return None;
        }
        let mut found = None;
        for f in &files {
            for (owner, m, n) in &self.files.get(*f)?.methods {
                if *owner == tname && m == name {
                    if *n != 1 || found.is_some() {
                        return None;
                    }
                    found = Some(*f);
                }
            }
        }
        Some(format!("{}#{tname}.{name}", found?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::Extractor;
    use crate::resolve::resolve_edges_with_go_modules;
    use sieve_core::{Confidence, Relation};

    fn module(module: &str, dir: &str, links: &[(&str, &str)]) -> GoModule {
        GoModule {
            module: module.to_string(),
            dir: dir.to_string(),
            links: links
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect(),
        }
    }

    /// The Extracted `calls` edges of the files as `(source, target)`.
    fn proven(files: &[(&str, &str)], modules: &[GoModule]) -> Vec<(String, String)> {
        let mut ext = Extractor::new().expect("extractor builds");
        let (mut nodes, mut raw) = (Vec::new(), Vec::new());
        for (path, src) in files {
            let (n, r) = ext.extract_file(path, src, "go");
            nodes.extend(n);
            raw.extend(r);
        }
        resolve_edges_with_go_modules(&nodes, &raw, modules)
            .into_iter()
            .filter(|e| e.relation == Relation::Calls && e.confidence == Confidence::Extracted)
            .map(|e| (e.source, e.target))
            .collect()
    }

    fn has(got: &[(String, String)], s: &str, t: &str) -> bool {
        got.iter().any(|(a, b)| a == s && b == t)
    }

    const ROOT: fn() -> Vec<GoModule> = || vec![module("m", "", &[])];

    #[test]
    fn test_gox2_path_helpers() {
        assert_eq!(dirname("a/b/c.go"), "a/b");
        assert_eq!(dirname("c.go"), "");
        assert!(is_test("a/x_test.go") && !is_test("a/x.go"));
        assert!(within("a/b", "a") && within("a", "a") && within("a", ""));
        assert!(!within("ab", "a") && !within("a", "a/b"));
    }

    #[test]
    fn test_gox2_build_ignore_needs_a_blank_line() {
        let caller = ("b.go", "package p\nfunc g() { f() }\n");
        for (line, blank, counts) in [
            ("//go:build ignore", true, false),
            ("//go:build ignore", false, true),
            ("// +build ignore", true, false),
            ("// +build ignore", false, true),
        ] {
            let sep = if blank { "\n\n" } else { "\n" };
            let src = format!("{line}{sep}package p\nfunc f() {{}}\n");
            let got = proven(&[("a.go", &src), caller], &ROOT());
            assert_eq!(has(&got, "b.go#g", "a.go#f"), counts, "{line} {blank}");
        }
    }

    #[test]
    fn test_gox2_skipped_paths_give_no_proof() {
        let def = "package p\nfunc f() {}\n";
        let call = "package p\nfunc g() { f() }\n";
        for dir in ["testdata/", "_x/", ".h/", "a/testdata/"] {
            let (a, b) = (format!("{dir}a.go"), format!("{dir}b.go"));
            let got = proven(&[(&a, def), (&b, call)], &ROOT());
            assert!(got.is_empty(), "{dir}: {got:?}");
        }
        // A def in `_x.go` is no target. A caller in `_x.go` gets no proof.
        let got = proven(&[("_x.go", def), ("b.go", call)], &ROOT());
        assert!(got.is_empty(), "{got:?}");
        let got = proven(&[("a.go", def), ("_b.go", call)], &ROOT());
        assert!(got.is_empty(), "{got:?}");
        let got = proven(&[("a.go", def), ("b.go", call)], &ROOT());
        assert!(has(&got, "b.go#g", "a.go#f"));
    }

    const OTHER: &str = "package other\nfunc Run() {}\n";

    #[test]
    fn test_gox2_import_name_shadowed_by_local_stops() {
        let ok = "package p\nimport o \"m/other\"\nfunc g() { o.Run() }\n";
        let got = proven(&[("other/o.go", OTHER), ("a.go", ok)], &ROOT());
        assert!(has(&got, "a.go#g", "other/o.go#Run"));
        for body in ["func g(o int) { o.Run() }", "func g() { o := 1; o.Run() }"] {
            let src = format!("package p\nimport o \"m/other\"\n{body}\n");
            let got = proven(&[("other/o.go", OTHER), ("a.go", &src)], &ROOT());
            assert!(got.is_empty(), "{body}: {got:?}");
        }
    }

    #[test]
    fn test_gox2_two_imports_with_one_local_name_stop() {
        let a = "package other\nfunc Run() {}\n";
        let src =
            "package p\nimport (\n\"m/a/other\"\n\"m/b/other\"\n)\nfunc g() { other.Run() }\n";
        let got = proven(
            &[("a/other/o.go", a), ("b/other/o.go", a), ("c.go", src)],
            &ROOT(),
        );
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn test_gox2_nested_module_dir_is_not_in_the_outer_module() {
        let src = "package p\nimport \"m/sub\"\nfunc g() { sub.Run() }\n";
        let sub = "package sub\nfunc Run() {}\n";
        let files = [("sub/s.go", sub), ("c.go", src)];
        let got = proven(&files, &ROOT());
        assert!(has(&got, "c.go#g", "sub/s.go#Run"));
        let nested = vec![module("m", "", &[]), module("other.org/sub", "sub", &[])];
        assert!(proven(&files, &nested).is_empty());
    }

    #[test]
    fn test_gox2_other_module_needs_a_link() {
        let src = "package p\nimport \"x.org/e\"\nfunc g() { e.Run() }\n";
        let e = "package e\nfunc Run() {}\n";
        let files = [("ext/e.go", e), ("c.go", src)];
        let bare = vec![module("m", "", &[]), module("x.org/e", "ext", &[])];
        assert!(proven(&files, &bare).is_empty());
        let linked = vec![
            module("m", "", &[("x.org/e", "ext")]),
            module("x.org/e", "ext", &[]),
        ];
        assert!(has(&proven(&files, &linked), "c.go#g", "ext/e.go#Run"));
        // A replace path outside the repo gives no link, so the import stops.
        let outside = vec![module("m", "", &[])];
        assert!(proven(&files, &outside).is_empty());
        // 2 go.mod files with one module path stop.
        let twin = vec![
            module("m", "", &[("x.org/e", "ext")]),
            module("x.org/e", "ext", &[]),
            module("x.org/e", "ext2", &[]),
        ];
        assert!(proven(&files, &twin).is_empty());
    }

    #[test]
    fn test_gox2_methods_on_value_and_pointer_in_two_files_stop() {
        let t = "package p\ntype T struct{}\n";
        let a = "package p\nfunc (T) M() {}\n";
        let b = "package p\nfunc (*T) M() {}\n";
        let c = "package p\nfunc g(x T) { x.M() }\n";
        let got = proven(
            &[("t.go", t), ("a.go", a), ("b.go", b), ("c.go", c)],
            &ROOT(),
        );
        assert!(got.is_empty(), "{got:?}");
        let got = proven(&[("t.go", t), ("a.go", a), ("c.go", c)], &ROOT());
        assert!(has(&got, "c.go#g", "a.go#T.M"));
    }

    #[test]
    fn test_gox2_local_new_stops() {
        let t = "package p\ntype T struct{}\nfunc (t *T) M() {}\n";
        let ok = "package p\nfunc g() { d := new(T); d.M() }\n";
        let got = proven(&[("t.go", t), ("c.go", ok)], &ROOT());
        assert!(has(&got, "c.go#g", "t.go#T.M"));
        let local =
            "package p\nfunc g() { new := func(int) *T { return nil }; d := new(1); d.M() }\n";
        assert!(proven(&[("t.go", t), ("c.go", local)], &ROOT()).is_empty());
    }

    #[test]
    fn test_gox2_generic_constructor_stops() {
        let t = "package p\ntype T struct{}\nfunc (t *T) M() {}\nfunc NewT[X any]() *T { return nil }\n";
        for body in ["x := NewT(); x.M()", "x := NewT[int](); x.M()"] {
            let src = format!("package p\nfunc g() {{ {body} }}\n");
            let got = proven(&[("t.go", t), ("c.go", &src)], &ROOT());
            assert!(!has(&got, "c.go#g", "t.go#T.M"), "{body}: {got:?}");
        }
    }
}
