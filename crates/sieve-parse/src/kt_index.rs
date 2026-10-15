//! The repo-wide index of Kotlin files. The resolver builds it once from the
//! file records and asks it to prove each Kotlin call (see `kt_proof.rs`).
//! The index answers three questions: which type a simple name means, which
//! types a type inherits from, and which types inherit from a type. Any
//! doubt gives an answer that stops the proof.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::extract::{JavaRec, KtBodyKind, KtFileRec, KtKind, KtRec, KtType, RawEdge};
use crate::java_tree::last_segment;

/// A type: the file index and the type index in that file.
pub(crate) type Ty = (usize, usize);

/// The most nested name lookups one proof makes.
const MAX_DEPTH: u32 = 32;

/// The members of `Any`.
const ANY_NAMES: &[&str] = &["equals", "hashCode", "toString"];

/// The implicit members of an enum class.
const ENUM_NAMES: &[&str] = &[
    "clone",
    "compareTo",
    "declaringJavaClass",
    "describeConstable",
    "entries",
    "getDeclaringClass",
    "name",
    "ordinal",
    "valueOf",
    "values",
];

/// The members of `Throwable`.
const THROWABLE_NAMES: &[&str] = &[
    "addSuppressed",
    "cause",
    "fillInStackTrace",
    "getCause",
    "getLocalizedMessage",
    "getMessage",
    "getStackTrace",
    "getSuppressed",
    "initCause",
    "localizedMessage",
    "message",
    "printStackTrace",
    "setStackTrace",
    "stackTrace",
    "suppressed",
];

/// The default-imported types whose members the code freezes as those of
/// `Throwable`.
const THROWABLES: &[&str] = &[
    "ArithmeticException",
    "AssertionError",
    "ClassCastException",
    "ConcurrentModificationException",
    "Error",
    "Exception",
    "IllegalArgumentException",
    "IllegalStateException",
    "IndexOutOfBoundsException",
    "NoSuchElementException",
    "NullPointerException",
    "NumberFormatException",
    "RuntimeException",
    "Throwable",
    "UnsupportedOperationException",
];

/// What a type name means.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Res {
    /// One in-repo Kotlin type.
    Repo(Ty),
    /// No in-repo Kotlin type has the name.
    Outside,
    /// The lookup cannot decide.
    Unknown,
}

/// A type or a body that names supertypes.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Sub {
    Ty(Ty),
    Body(usize, usize),
}

/// The types of one chain and whether an implicit member has the name.
pub(crate) struct Chain {
    /// The type itself and every supertype.
    pub tys: Vec<Ty>,
    /// A member without a node (an `Any`, enum, data class or frozen
    /// member) has the name.
    pub implicit: bool,
}

/// The Kotlin files of a repo.
#[derive(Default)]
pub struct KtIndex {
    pub(crate) files: Vec<(String, KtFileRec)>,
    pub(crate) by_path: HashMap<String, usize>,
    /// The packages of the files without a parse error.
    pub(crate) repo_pkgs: HashSet<String>,
    /// A package and a name to the top-level functions.
    pub(crate) top_funs: HashMap<(String, String), Vec<(usize, usize)>>,
    /// A package and a name to the top-level properties.
    pub(crate) top_props: HashMap<(String, String), Vec<(usize, usize)>>,
    /// A package and a name to the top-level types.
    top_types: HashMap<(String, String), Vec<Ty>>,
    /// A package and a name to a typealias.
    aliases: HashSet<(String, String)>,
    /// The full name of each type, outer types included.
    fqn: HashMap<String, Vec<Ty>>,
    /// Every name the repo declares.
    pub(crate) all_names: HashSet<String>,
    /// The names that the broken files lost.
    pub(crate) lost: HashSet<String>,
    /// The names of the Java types of the repo.
    pub(crate) java_names: HashSet<String>,
    /// The last segment of each written supertype of a Java type.
    java_supers: HashSet<String>,
    /// The packages that an import reaches with a name the repo lacks.
    pub(crate) split: HashSet<String>,
    /// A supertype last segment to the types and bodies that name it.
    subs: HashMap<String, Vec<Sub>>,
    /// The types and bodies with a supertype that may be any type.
    wild: Vec<Sub>,
    /// The names of the types that sit inside another type or a body.
    nested_names: HashSet<String>,
    /// The extension receiver types of the `invoke` functions.
    pub(crate) invoke_ext: HashSet<String>,
    /// The file that gets one trace line per call, from `SIEVE_KT_TRACE`.
    pub(crate) trace: Option<PathBuf>,
    depth: Cell<u32>,
}

impl KtIndex {
    /// Reads the file records of `raw`.
    pub fn build(raw: &[RawEdge]) -> Self {
        let mut ix = KtIndex::default();
        for e in raw {
            let Some(KtRec::File(rec)) = &e.kt else {
                continue;
            };
            let fi = ix.files.len();
            ix.by_path.insert(e.file.clone(), fi);
            ix.files.push((e.file.clone(), (**rec).clone()));
        }
        if ix.files.is_empty() {
            return ix;
        }
        ix.trace = std::env::var_os("SIEVE_KT_TRACE").map(PathBuf::from);
        ix.read_java(raw);
        ix.read_declarations();
        ix.read_splits();
        ix.read_subtypes();
        ix
    }

    fn read_java(&mut self, raw: &[RawEdge]) {
        for e in raw {
            let Some(JavaRec::File(rec)) = &e.java else {
                continue;
            };
            for t in &rec.types {
                self.java_names.insert(t.name.clone());
                let sups = t.extends.iter().chain(t.implements.iter());
                self.java_supers
                    .extend(sups.map(|s| last_segment(s).to_string()));
            }
            for b in &rec.bodies {
                self.java_supers
                    .extend(b.sups.iter().map(|s| last_segment(s).to_string()));
            }
        }
    }

    fn read_declarations(&mut self) {
        for fi in 0..self.files.len() {
            let rec = self.files[fi].1.clone();
            if rec.broken {
                self.lost.extend(rec.lost.iter().cloned());
                continue;
            }
            self.repo_pkgs.insert(rec.pkg.clone());
            for f in rec.funs.iter().filter(|f| f.name == "invoke") {
                self.invoke_ext.extend(f.ext_recv.clone());
            }
            for (i, f) in rec.funs.iter().enumerate() {
                self.all_names.insert(f.name.clone());
                let key = (rec.pkg.clone(), f.name.clone());
                self.top_funs.entry(key).or_default().push((fi, i));
            }
            for (i, p) in rec.props.iter().enumerate() {
                self.all_names.insert(p.name.clone());
                let key = (rec.pkg.clone(), p.name.clone());
                self.top_props.entry(key).or_default().push((fi, i));
            }
            for a in &rec.aliases {
                self.all_names.insert(a.name.clone());
                self.aliases.insert((rec.pkg.clone(), a.name.clone()));
            }
            for (ti, t) in rec.types.iter().enumerate() {
                self.all_names.insert(t.name.clone());
                if t.outer.is_some() || t.local {
                    self.nested_names.insert(t.name.clone());
                }
                for m in t.methods.iter().filter(|m| m.name == "invoke") {
                    self.invoke_ext.extend(m.ext_recv.clone());
                }
                self.all_names
                    .extend(t.methods.iter().map(|m| m.name.clone()));
                self.all_names
                    .extend(t.props.iter().map(|p| p.name.clone()));
                if t.outer.is_none() && !t.local {
                    let key = (rec.pkg.clone(), t.name.clone());
                    self.top_types.entry(key).or_default().push((fi, ti));
                }
                let mut path = t.name.clone();
                let mut cur = Some((fi, ti));
                while let Some(o) = cur.and_then(|c| self.outer_in(&rec, c.1)) {
                    path = format!("{}.{path}", rec.types[o].name);
                    cur = Some((fi, o));
                }
                let full = if rec.pkg.is_empty() {
                    path
                } else {
                    format!("{}.{path}", rec.pkg)
                };
                self.fqn.entry(full).or_default().push((fi, ti));
            }
            for b in &rec.bodies {
                self.all_names.insert(b.name.clone());
            }
        }
    }

    /// A package is split when an import names something in it that the
    /// repo does not declare: a library may add to the package.
    fn read_splits(&mut self) {
        let mut split = HashSet::new();
        for (_, rec) in self.files.iter().filter(|f| !f.1.broken) {
            for imp in rec.imports.iter().filter(|i| !i.star) {
                let (pkg, last) = split_path(&imp.path);
                let key = (pkg.to_string(), last.to_string());
                let declared = self.top_funs.contains_key(&key)
                    || self.top_props.contains_key(&key)
                    || self.top_types.contains_key(&key)
                    || self.aliases.contains(&key);
                if self.repo_pkgs.contains(pkg) && !declared {
                    split.insert(pkg.to_string());
                }
            }
        }
        self.split = split;
    }

    fn read_subtypes(&mut self) {
        let mut subs: HashMap<String, Vec<Sub>> = HashMap::new();
        let mut wild = Vec::new();
        for fi in 0..self.files.len() {
            let rec = &self.files[fi].1;
            if rec.broken {
                continue;
            }
            let mut entries: Vec<(Sub, &[String])> = Vec::new();
            for (ti, t) in rec.types.iter().enumerate() {
                entries.push((Sub::Ty((fi, ti)), &t.bases));
            }
            for (bi, b) in rec.bodies.iter().enumerate() {
                entries.push((Sub::Body(fi, bi), &b.bases));
            }
            for (sub, bases) in entries {
                let mut any_wild = false;
                for b in bases {
                    match self.base_res(sub, b) {
                        Res::Unknown => any_wild = true,
                        // Key on the declared name: an import alias hides it.
                        Res::Repo(x) => {
                            subs.entry(self.ty(x).name.clone()).or_default().push(sub);
                        }
                        Res::Outside => {
                            subs.entry(b.clone()).or_default().push(sub);
                        }
                    }
                }
                if any_wild {
                    wild.push(sub);
                }
            }
        }
        self.subs = subs;
        self.wild = wild;
    }

    /// What the written supertype `b` of a type or body means.
    fn base_res(&self, sub: Sub, b: &str) -> Res {
        if b == "?" {
            return Res::Unknown;
        }
        let (fi, from, scoped) = match sub {
            Sub::Ty(t) => (t.0, self.outer_of(t), self.ty(t).local),
            Sub::Body(fi, _) => (fi, None, true),
        };
        let res = self.lookup(fi, from, b);
        // A body or a local class sits in a scope the facts do not record.
        // A name of a nested type may mean one of those types.
        if scoped && res != Res::Unknown && self.nested_names.contains(b) {
            return Res::Unknown;
        }
        res
    }

    pub(crate) fn ty(&self, t: Ty) -> &KtType {
        &self.files[t.0].1.types[t.1]
    }

    /// The index of the type that holds the type `ti` of `rec`: the nearest
    /// type of that name that starts before it.
    fn outer_in(&self, rec: &KtFileRec, ti: usize) -> Option<usize> {
        let ty = &rec.types[ti];
        let outer = ty.outer.as_ref()?;
        rec.types
            .iter()
            .enumerate()
            .filter(|(i, x)| {
                *i != ti && x.name == *outer && x.line <= ty.line && x.local == ty.local
            })
            .max_by_key(|(_, x)| x.line)
            .map(|(i, _)| i)
    }

    pub(crate) fn outer_of(&self, t: Ty) -> Option<Ty> {
        self.outer_in(&self.files[t.0].1, t.1).map(|o| (t.0, o))
    }

    /// The companion object of a type.
    pub(crate) fn companion_of(&self, t: Ty) -> Option<Ty> {
        let name = self.ty(t).companion.as_ref()?;
        let rec = &self.files[t.0].1;
        rec.types
            .iter()
            .position(|c| c.kind == KtKind::Companion && c.name == *name && !c.local)
            .filter(|c| self.outer_of((t.0, *c)) == Some(t))
            .map(|c| (t.0, c))
    }

    /// The type of the file `fi` with the name and the start line.
    pub(crate) fn type_at(&self, fi: usize, name: &str, line: u32) -> Option<Ty> {
        let rec = &self.files[fi].1;
        let at = rec
            .types
            .iter()
            .position(|t| t.name == name && t.line == line && !t.local)?;
        Some((fi, at))
    }

    /// The types named `n` that sit directly in the type `t`.
    fn nested(&self, t: Ty, n: &str) -> Vec<Ty> {
        let name = &self.ty(t).name;
        let rec = &self.files[t.0].1;
        (0..rec.types.len())
            .filter(|i| {
                let x = &rec.types[*i];
                x.name == n && x.outer.as_ref() == Some(name) && !x.local
            })
            .map(|i| (t.0, i))
            .filter(|c| self.outer_of(*c) == Some(t))
            .collect()
    }

    /// What the simple name `n` means in the file `fi`, seen from the type
    /// `from` (`None` outside every type).
    pub(crate) fn lookup(&self, fi: usize, from: Option<Ty>, n: &str) -> Res {
        let d = self.depth.get();
        if d > MAX_DEPTH {
            return Res::Unknown;
        }
        self.depth.set(d + 1);
        let r = self.lookup_inner(fi, from, n);
        self.depth.set(d);
        r
    }

    fn lookup_inner(&self, fi: usize, from: Option<Ty>, n: &str) -> Res {
        if self.lost.contains(n) {
            return Res::Unknown;
        }
        let rec = &self.files[fi].1;
        let mut hits: Vec<Ty> = Vec::new();
        let mut cur = from;
        while let Some(t) = cur {
            if self.ty(t).tparams.iter().any(|p| p == n) {
                return Res::Unknown;
            }
            let Ok(chain) = self.chain(t, "") else {
                return Res::Unknown;
            };
            for s in chain.tys {
                for h in self.nested(s, n) {
                    if !hits.contains(&h) {
                        hits.push(h);
                    }
                }
            }
            cur = self.outer_of(t);
        }
        match hits.as_slice() {
            [] => {}
            [one] => return Res::Repo(*one),
            _ => return Res::Unknown,
        }
        let local = rec
            .bodies
            .iter()
            .any(|b| b.kind == KtBodyKind::Local && b.name == n)
            || rec.types.iter().any(|t| t.local && t.name == n);
        if local {
            return Res::Unknown;
        }
        for imp in rec.imports.iter().filter(|i| !i.star) {
            let eff = imp
                .alias
                .as_deref()
                .unwrap_or_else(|| last_segment(&imp.path));
            if eff != n {
                continue;
            }
            return match self.fqn.get(&imp.path).map(Vec::as_slice) {
                Some([one]) => Res::Repo(*one),
                Some([]) | None => {
                    let (pkg, last) = split_path(&imp.path);
                    if self.aliases.contains(&(pkg.to_string(), last.to_string())) {
                        Res::Unknown
                    } else {
                        Res::Outside
                    }
                }
                Some(_) => Res::Unknown,
            };
        }
        let mut pkgs = vec![rec.pkg.as_str()];
        pkgs.extend(
            rec.imports
                .iter()
                .filter(|i| i.star && self.repo_pkgs.contains(&i.path))
                .map(|i| i.path.as_str()),
        );
        for p in &pkgs {
            if self.aliases.contains(&(p.to_string(), n.to_string())) {
                return Res::Unknown;
            }
        }
        let mut found: Vec<Ty> = Vec::new();
        for p in pkgs {
            if let Some(v) = self.top_types.get(&(p.to_string(), n.to_string())) {
                for t in v {
                    if !found.contains(t) {
                        found.push(*t);
                    }
                }
            }
        }
        match found.as_slice() {
            [] => Res::Outside,
            [one] => Res::Repo(*one),
            _ => Res::Unknown,
        }
    }

    /// The type `t` and every supertype. `name` is the member the caller
    /// looks for. An unknown supertype gives `Err`.
    pub(crate) fn chain(&self, t: Ty, name: &str) -> Result<Chain, ()> {
        let mut c = Chain {
            tys: Vec::new(),
            implicit: ANY_NAMES.contains(&name),
        };
        self.chain_into(t, name, &mut c, 0)?;
        Ok(c)
    }

    fn chain_into(&self, t: Ty, name: &str, c: &mut Chain, depth: u32) -> Result<(), ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        if c.tys.contains(&t) {
            return Ok(());
        }
        c.tys.push(t);
        let ty = self.ty(t);
        c.implicit |= self.implicit_member(t, name);
        for b in &ty.bases {
            if b == "?" {
                return Err(());
            }
            match self.lookup(t.0, self.outer_of(t), b) {
                Res::Repo(s) => self.chain_into(s, name, c, depth + 1)?,
                Res::Outside => c.implicit |= frozen_base(b).ok_or(())?.contains(&name),
                Res::Unknown => return Err(()),
            }
        }
        Ok(())
    }

    /// Whether the type `t` has a member `name` that no node shows.
    fn implicit_member(&self, t: Ty, name: &str) -> bool {
        let ty = self.ty(t);
        let has_mod = |m: &str| ty.mods.iter().any(|w| w == m);
        let has_annot = |ty: &KtType, a: &str| ty.annots.iter().any(|x| x == a);
        let data = has_mod("data")
            && (matches!(name, "copy" | "equals" | "hashCode" | "toString")
                || name
                    .strip_prefix("component")
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())));
        let enum_member =
            (ty.kind == KtKind::Enum || has_mod("enum")) && ENUM_NAMES.contains(&name);
        let serial = name == "serializer"
            && (has_annot(ty, "Serializable")
                || (ty.kind == KtKind::Companion
                    && self
                        .outer_of(t)
                        .is_some_and(|o| has_annot(self.ty(o), "Serializable"))));
        let parcel =
            matches!(name, "writeToParcel" | "describeContents") && has_annot(ty, "Parcelize");
        data || enum_member || serial || parcel
    }

    /// The reason a subtype of `owner` may override `name`, if one exists.
    /// The walk covers types, anonymous objects, enum entry bodies and
    /// local classes, and every type whose supertype is unknown.
    pub(crate) fn subtype_stop(&self, owner: Ty, name: &str) -> Option<&'static str> {
        let mut queue: Vec<(String, Option<Ty>)> = vec![(self.ty(owner).name.clone(), Some(owner))];
        let mut seen: HashSet<Sub> = HashSet::new();
        let mut at = 0;
        while at < queue.len() {
            let (key, target) = queue[at].clone();
            at += 1;
            if self.java_supers.contains(&key) {
                return Some("java_subtype");
            }
            let listed = self.subs.get(&key).map(Vec::as_slice).unwrap_or_default();
            for sub in listed.iter().chain(self.wild.iter()) {
                if seen.contains(sub) || !self.refers(*sub, target) {
                    continue;
                }
                seen.insert(*sub);
                let rec = |fi: usize| &self.files[fi].1;
                match *sub {
                    Sub::Ty(t) => {
                        let x = self.ty(t);
                        let overrides = x
                            .methods
                            .iter()
                            .any(|m| m.name == name && m.mods.iter().any(|w| w == "override"));
                        if overrides {
                            return Some("override_in_subtype");
                        }
                        queue.push((x.name.clone(), Some(t)));
                    }
                    Sub::Body(fi, bi) => {
                        let b = &rec(fi).bodies[bi];
                        if b.overrides.iter().any(|o| o == name) {
                            return Some("override_in_subtype");
                        }
                        if b.kind == KtBodyKind::Local {
                            queue.push((b.name.clone(), None));
                        }
                    }
                }
            }
        }
        None
    }

    /// Whether some written supertype of `sub` is the type `target`, or may
    /// be any type.
    fn refers(&self, sub: Sub, target: Option<Ty>) -> bool {
        let bases = match sub {
            Sub::Ty(t) => &self.ty(t).bases,
            Sub::Body(fi, bi) => &self.files[fi].1.bodies[bi].bases,
        };
        bases.iter().any(|b| match self.base_res(sub, b) {
            Res::Repo(x) => Some(x) == target,
            Res::Unknown => true,
            Res::Outside => false,
        })
    }
}

/// The package part and the last segment of a dotted path.
pub(crate) fn split_path(path: &str) -> (&str, &str) {
    path.rsplit_once('.').unwrap_or(("", path))
}

/// The frozen member names of a default-imported base type, or `None` if
/// the code does not freeze the type.
fn frozen_base(b: &str) -> Option<&'static [&'static str]> {
    match b {
        "Any" | "Serializable" | "Annotation" => Some(&[]),
        "Cloneable" => Some(&["clone"]),
        "Comparable" => Some(&["compareTo"]),
        "Enum" => Some(ENUM_NAMES),
        t if THROWABLES.contains(&t) => Some(THROWABLE_NAMES),
        _ => None,
    }
}
