//! The type index of a repo's Java files (JVX, design `java-xfile.md` rules
//! JV2 to JV10). The resolver builds it from the file records and asks it
//! to prove each `JavaCall`. A proof means the javac compile-time target is
//! also the one runtime target. Any doubt gives `None`, and the edge stays
//! Inferred.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::extract::{
    JavaBody, JavaCall, JavaFileRec, JavaKind, JavaRec, JavaShape, JavaType, RawEdge,
};
use crate::java_tree::last_segment;

/// A type: the file index and the type index in that file.
type Ty = (usize, u32);

/// A method: its type and its index in `JavaType::methods`.
type Meth = (Ty, usize);

/// The most nested type lookups one proof makes.
const MAX_DEPTH: u32 = 48;

/// JV3: the JDK types a chain may hold, with the method names the code
/// freezes for each. `Object` is in every chain.
const JDK_TYPES: &[(&str, &str, &[&str])] = &[
    (
        "Object",
        "java.lang.Object",
        &[
            "equals",
            "hashCode",
            "toString",
            "getClass",
            "notify",
            "notifyAll",
            "wait",
            "clone",
            "finalize",
        ],
    ),
    ("Serializable", "java.io.Serializable", &[]),
    ("Cloneable", "java.lang.Cloneable", &[]),
    ("Comparable", "java.lang.Comparable", &["compareTo"]),
    ("Runnable", "java.lang.Runnable", &["run"]),
    ("AutoCloseable", "java.lang.AutoCloseable", &["close"]),
    ("Closeable", "java.io.Closeable", &["close"]),
    ("Throwable", "java.lang.Throwable", THROWABLE),
    ("Exception", "java.lang.Exception", THROWABLE),
    ("RuntimeException", "java.lang.RuntimeException", THROWABLE),
    ("Error", "java.lang.Error", THROWABLE),
    (
        "Enum",
        "java.lang.Enum",
        &[
            "name",
            "ordinal",
            "compareTo",
            "getDeclaringClass",
            "describeConstable",
            "valueOf",
            "equals",
            "hashCode",
            "toString",
            "clone",
            "finalize",
        ],
    ),
    (
        "Record",
        "java.lang.Record",
        &["equals", "hashCode", "toString"],
    ),
];

/// The primitive type names.
const PRIMITIVES: &[&str] = &[
    "int", "long", "short", "byte", "char", "boolean", "double", "float",
];

/// The `java.lang` types that a signature compares by name.
const LANG_TYPES: &[&str] = &[
    "String",
    "Object",
    "Integer",
    "Long",
    "Short",
    "Byte",
    "Character",
    "Boolean",
    "Double",
    "Float",
    "Number",
    "CharSequence",
    "Class",
];

/// The method names of `Throwable`.
const THROWABLE: &[&str] = &[
    "getMessage",
    "getLocalizedMessage",
    "getCause",
    "initCause",
    "toString",
    "printStackTrace",
    "getStackTrace",
    "setStackTrace",
    "fillInStackTrace",
    "addSuppressed",
    "getSuppressed",
];

/// The name of the JDK type with the full name `fqn`.
fn jdk_by_fqn(fqn: &str) -> Option<&'static str> {
    JDK_TYPES.iter().find(|t| t.1 == fqn).map(|t| t.0)
}

/// Reports whether the JDK type `jdk` has a method called `m`.
fn jdk_has(jdk: &str, m: &str) -> bool {
    JDK_TYPES.iter().any(|t| t.0 == jdk && t.2.contains(&m))
}

/// The result of a type name lookup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Res {
    /// An in-repo type.
    Found(Ty),
    /// A JDK type of the frozen list.
    Jdk(&'static str),
    /// The lookup cannot decide.
    Stop,
}

/// The members named `m` of a type.
struct Members {
    methods: Vec<Meth>,
    /// An implicit member or a JDK member has the name (fix B3).
    implicit: bool,
}

/// A type or a class body that names supertypes, for JV10.
struct SubEntry {
    name: Option<String>,
    sups: Vec<String>,
    methods: Vec<String>,
}

/// The types of a repo's Java files.
#[derive(Default)]
pub struct JavaIndex {
    files: Vec<(String, JavaFileRec)>,
    by_path: HashMap<String, usize>,
    /// The full name of each type, nested types included.
    fqn: HashMap<String, Vec<Ty>>,
    /// The full name of each top-level type.
    top: HashMap<String, Vec<Ty>>,
    /// The packages that have a file.
    pkgs: HashSet<String>,
    entries: Vec<SubEntry>,
    /// A supertype simple name to the entries that name it.
    subs: HashMap<String, Vec<usize>>,
    /// A type or local class simple name to its entries.
    by_name: HashMap<String, Vec<usize>>,
    /// A file has a Lombok import or a parse error, so a hidden override
    /// may exist. No virtual dispatch proof holds (JV10).
    risky: bool,
    /// The `(source, target)` pairs where some call reaches the target at
    /// compile time but not as the one runtime target. An edge for such a
    /// pair is never Extracted, because the graph keeps 1 edge per pair.
    tainted: HashSet<(String, String)>,
    depth: Cell<u32>,
}

/// What the proof of 1 call gives.
struct Proof {
    /// The node id of the compile-time target.
    id: String,
    /// The compile-time target is also the one runtime target (JV10).
    exact: bool,
}

impl JavaIndex {
    /// Reads the file records of `raw`.
    pub fn build(raw: &[RawEdge]) -> Self {
        let mut ix = JavaIndex::default();
        for e in raw {
            let Some(JavaRec::File(rec)) = &e.java else {
                continue;
            };
            ix.risky |= rec.broken || rec.lombok;
            let fi = ix.files.len();
            ix.by_path.insert(e.file.clone(), fi);
            ix.files.push((e.file.clone(), (**rec).clone()));
        }
        let files = std::mem::take(&mut ix.files);
        for (fi, (_, rec)) in files.iter().enumerate() {
            ix.pkgs.insert(rec.pkg.clone());
            for (ti, t) in rec.types.iter().enumerate() {
                let ty = (fi, ti as u32);
                let mut names = vec![t.name.as_str()];
                let mut cur = t.outer;
                while let Some(o) = cur {
                    let ot = &rec.types[o as usize];
                    names.push(ot.name.as_str());
                    cur = ot.outer;
                }
                names.reverse();
                let path = names.join(".");
                let full = if rec.pkg.is_empty() {
                    path
                } else {
                    format!("{}.{path}", rec.pkg)
                };
                ix.fqn.entry(full.clone()).or_default().push(ty);
                if t.outer.is_none() {
                    ix.top.entry(full).or_default().push(ty);
                }
                let sups: Vec<&str> = t
                    .extends
                    .iter()
                    .chain(t.implements.iter())
                    .map(|s| last_segment(s))
                    .collect();
                let entry = SubEntry {
                    name: Some(t.name.clone()),
                    sups: sups.iter().map(|s| (*s).to_string()).collect(),
                    methods: t.methods.iter().map(|m| m.name.clone()).collect(),
                };
                ix.add_entry(entry, &sups);
            }
            for b in &rec.bodies {
                let JavaBody {
                    name,
                    sups,
                    methods,
                } = b;
                let entry = SubEntry {
                    name: name.clone(),
                    sups: sups.clone(),
                    methods: methods.clone(),
                };
                let sups: Vec<&str> = sups.iter().map(String::as_str).collect();
                ix.add_entry(entry, &sups);
            }
        }
        ix.files = files;
        for e in raw {
            let Some(JavaRec::Cross(call)) = &e.java else {
                continue;
            };
            let Some(name) = e.name.as_deref() else {
                continue;
            };
            if let Some(p) = ix.proof(&e.file, call, name).filter(|p| !p.exact) {
                ix.tainted.insert((e.source.clone(), p.id));
            }
        }
        ix
    }

    fn add_entry(&mut self, entry: SubEntry, sups: &[&str]) {
        let i = self.entries.len();
        if let Some(n) = &entry.name {
            self.by_name.entry(n.clone()).or_default().push(i);
        }
        self.entries.push(entry);
        for s in sups {
            self.subs.entry((*s).to_string()).or_default().push(i);
        }
    }

    fn ty(&self, t: Ty) -> &JavaType {
        &self.files[t.0].1.types[t.1 as usize]
    }

    fn pkg(&self, t: Ty) -> &str {
        &self.files[t.0].1.pkg
    }

    /// The node id that the call `call` of `name` in the file `caller`
    /// reaches, or `None` when the index does not prove exactly 1 target.
    pub fn resolve(
        &self,
        caller: &str,
        source: &str,
        call: &JavaCall,
        name: &str,
    ) -> Option<String> {
        let p = self.proof(caller, call, name)?;
        let key = (source.to_string(), p.id);
        (p.exact && !self.tainted.contains(&key)).then_some(key.1)
    }

    /// The compile-time target of the call and whether it is also the one
    /// runtime target.
    fn proof(&self, caller: &str, call: &JavaCall, name: &str) -> Option<Proof> {
        let fi = *self.by_path.get(caller)?;
        let rec = &self.files[fi].1;
        if rec.broken || rec.lombok || call.name != name {
            return None;
        }
        rec.types.get(call.ctx as usize)?;
        self.depth.set(0);
        let ctx: Ty = (fi, call.ctx);
        let id = match &call.shape {
            JavaShape::Bare => {
                let (recv, m) = self.bare_target(ctx, name)?;
                return Some(self.dispatch(recv, m, name));
            }
            JavaShape::This => {
                let m = self.one_member(ctx, name)?;
                return Some(self.dispatch(ctx, m, name));
            }
            JavaShape::Super => self.super_target(ctx, name)?,
            JavaShape::Name { name: n } => return self.name_target(ctx, n, name),
            JavaShape::Var { ty } => {
                let recv = self.class_type(ctx, ty)?;
                let m = self.one_member(recv, name)?;
                return Some(self.dispatch(recv, m, name));
            }
            JavaShape::Field { name: f } => {
                let (owner, fty) = self.own_field(ctx, f)?;
                let recv = self.class_type(owner, &fty?)?;
                let m = self.one_member(recv, name)?;
                return Some(self.dispatch(recv, m, name));
            }
            JavaShape::New { ty } => {
                let t = match self.lookup(ctx.0, Some(ctx.1), ty) {
                    Res::Found(t) => t,
                    _ => return None,
                };
                let jt = self.ty(t);
                let creatable = matches!(jt.kind, JavaKind::Class | JavaKind::Record);
                (creatable && !jt.is_abstract).then(|| jt.id.clone())?
            }
        };
        (!id.is_empty()).then_some(Proof { id, exact: true })
    }

    // ---- JV2: type names -------------------------------------------------

    /// JV2: the type that the simple name `n` names in the file `fi`, seen
    /// from the type `from` (`None` at the top of the file).
    fn lookup(&self, fi: usize, from: Option<u32>, n: &str) -> Res {
        let d = self.depth.get();
        if d > MAX_DEPTH {
            return Res::Stop;
        }
        self.depth.set(d + 1);
        let res = self.lookup_inner(fi, from, n);
        self.depth.set(d);
        res
    }

    fn lookup_inner(&self, fi: usize, from: Option<u32>, n: &str) -> Res {
        let rec = &self.files[fi].1;
        // Steps 1 and 3: a type parameter or a member type of an enclosing
        // type, inherited member types included.
        let mut cur = from;
        while let Some(i) = cur {
            let t = &rec.types[i as usize];
            if t.tparams.iter().any(|p| p == n) {
                return Res::Stop;
            }
            match self.member_type((fi, i), n) {
                Err(()) => return Res::Stop,
                Ok(Some(found)) => return Res::Found(found),
                Ok(None) => {}
            }
            cur = t.outer;
        }
        // Step 4: a single-type import.
        if rec
            .imports
            .iter()
            .any(|i| i.stat && !i.star && last_segment(&i.path) == n)
        {
            return Res::Stop;
        }
        let mut hits: Vec<&str> = rec
            .imports
            .iter()
            .filter(|i| !i.stat && !i.star && last_segment(&i.path) == n)
            .map(|i| i.path.as_str())
            .collect();
        hits.dedup();
        if hits.len() > 1 {
            return Res::Stop;
        }
        if let Some(path) = hits.first() {
            return match self.fqn.get(*path) {
                Some(v) if v.len() == 1 => Res::Found(v[0]),
                Some(_) => Res::Stop,
                None => jdk_by_fqn(path).map_or(Res::Stop, Res::Jdk),
            };
        }
        // Step 5: the same package.
        let key = if rec.pkg.is_empty() {
            n.to_string()
        } else {
            format!("{}.{n}", rec.pkg)
        };
        match self.top.get(&key) {
            Some(v) if v.len() == 1 => return Res::Found(v[0]),
            Some(_) => return Res::Stop,
            None => {}
        }
        // Step 6: on-demand imports. Every one must be in the repo.
        if rec.imports.iter().any(|i| i.stat && i.star) {
            return Res::Stop;
        }
        let mut found: Vec<Ty> = Vec::new();
        let mut jdk_star = false;
        for i in rec.imports.iter().filter(|i| i.star && !i.stat) {
            if self.pkgs.contains(&i.path) {
                let k = format!("{}.{n}", i.path);
                for &t in self.top.get(&k).into_iter().flatten() {
                    if !self.ty(t).public {
                        return Res::Stop;
                    }
                    found.push(t);
                }
            } else if let Some(&[outer]) = self.fqn.get(&i.path).map(Vec::as_slice) {
                match self.member_type(outer, n) {
                    Ok(Some(t)) => found.push(t),
                    Ok(None) => {}
                    Err(()) => return Res::Stop,
                }
            } else if i.path == "java.lang" || i.path == "java.io" {
                jdk_star = jdk_star || i.path == "java.io";
            } else {
                return Res::Stop;
            }
        }
        found.sort_unstable();
        found.dedup();
        if found.len() > 1 {
            return Res::Stop;
        }
        if let Some(&t) = found.first() {
            return Res::Found(t);
        }
        match JDK_TYPES.iter().find(|t| t.0 == n) {
            Some(t) if t.1.starts_with("java.lang.") || jdk_star => Res::Jdk(t.0),
            _ => Res::Stop,
        }
    }

    /// The member type `n` of the type `t`: its own, else an inherited one.
    /// `Err` means a supertype is unknown.
    fn member_type(&self, t: Ty, n: &str) -> Result<Option<Ty>, ()> {
        let jt = self.ty(t);
        if let Some((_, i)) = jt.members.iter().find(|(m, _)| m == n) {
            return Ok(Some((t.0, *i)));
        }
        for s in jt.extends.iter().chain(jt.implements.iter()) {
            match self.super_of(t, s) {
                Res::Found(st) => {
                    if let Some(found) = self.member_type(st, n)? {
                        // B2 for member types: a private type, or a package
                        // type of another package, is not inherited.
                        let v = self.ty(found).vis;
                        if v != 0 && (v != 1 || self.pkg(found) == self.pkg(t)) {
                            return Ok(Some(found));
                        }
                    }
                }
                Res::Jdk(_) => {}
                Res::Stop => return Err(()),
            }
        }
        Ok(None)
    }

    /// The supertype that the written name `s` of the type `t` names.
    fn super_of(&self, t: Ty, s: &str) -> Res {
        if s.contains('.') {
            return jdk_by_fqn(s).map_or(Res::Stop, Res::Jdk);
        }
        match self.lookup(t.0, self.ty(t).outer, s) {
            Res::Found(st) if self.ty(st).kind != JavaKind::Annotation => Res::Found(st),
            Res::Jdk(j) => Res::Jdk(j),
            _ => Res::Stop,
        }
    }

    /// The class type that `name` names, seen from the type `ctx`. An
    /// interface gives `None` (JV8).
    fn class_type(&self, ctx: Ty, name: &str) -> Option<Ty> {
        match self.lookup(ctx.0, Some(ctx.1), name) {
            Res::Found(t)
                if !matches!(self.ty(t).kind, JavaKind::Interface | JavaKind::Annotation) =>
            {
                Some(t)
            }
            _ => None,
        }
    }

    // ---- JV4: members ----------------------------------------------------

    /// JV4: every member of `t` called `m`, over the whole chain, after the
    /// inheritance filter (fix B2). `None` when a supertype is unknown.
    fn members(&self, t: Ty, m: &str, depth: u32) -> Option<Members> {
        if depth > MAX_DEPTH || self.files[t.0].1.lombok {
            return None;
        }
        let jt = self.ty(t);
        let mut out = Members {
            methods: jt
                .methods
                .iter()
                .enumerate()
                .filter(|(_, x)| x.name == m)
                .map(|(i, _)| (t, i))
                .collect(),
            implicit: jdk_has("Object", m),
        };
        match jt.kind {
            JavaKind::Record => {
                out.implicit |= jt.components.iter().any(|c| c == m) || jdk_has("Record", m);
            }
            JavaKind::Enum => {
                out.implicit |= matches!(m, "values" | "valueOf") || jdk_has("Enum", m);
            }
            _ => {}
        }
        for s in jt.extends.iter().chain(jt.implements.iter()) {
            match self.super_of(t, s) {
                Res::Found(st) => {
                    let sub = self.members(st, m, depth + 1)?;
                    out.implicit |= sub.implicit;
                    for cand in sub.methods {
                        self.inherit(t, cand, &mut out.methods);
                    }
                }
                Res::Jdk(j) => out.implicit |= jdk_has(j, m),
                Res::Stop => return None,
            }
        }
        Some(out)
    }

    /// Adds the method `cand` of a supertype to the members `out` of `t`,
    /// unless `t` does not inherit it (fix B2) or a method of the same
    /// signature overrides it. An own method of `t` wins. A class method
    /// wins over an interface method.
    fn inherit(&self, t: Ty, cand: Meth, out: &mut Vec<Meth>) {
        let (dt, mi) = cand;
        let dm = &self.ty(dt).methods[mi];
        let cand_iface = self.ty(dt).kind == JavaKind::Interface;
        let hidden = dm.vis == 0
            || (cand_iface && dm.is_static)
            || (dm.vis == 1 && self.pkg(dt) != self.pkg(t));
        if hidden || out.contains(&cand) {
            return;
        }
        for slot in out.iter_mut() {
            if !self.same_sig(*slot, cand) {
                continue;
            }
            let slot_iface = self.ty(slot.0).kind == JavaKind::Interface;
            if slot.0 == t || (cand_iface && !slot_iface) {
                return;
            }
            if slot_iface && !cand_iface {
                *slot = cand;
                return;
            }
        }
        out.push(cand);
    }

    /// Reports whether 2 methods have the same signature: the same count of
    /// parameters, and the same type for each one.
    fn same_sig(&self, a: Meth, b: Meth) -> bool {
        let (ma, mb) = (&self.ty(a.0).methods[a.1], &self.ty(b.0).methods[b.1]);
        ma.params.len() == mb.params.len()
            && ma
                .params
                .iter()
                .zip(&mb.params)
                .all(|(x, y)| self.same_type(a.0, x, b.0, y))
    }

    /// Reports whether the written type `x` in the context `ta` and the
    /// written type `y` in the context `tb` name one type. A name that
    /// neither context resolves in the repo must be a primitive or a plain
    /// `java.lang` name that no import hides.
    fn same_type(&self, ta: Ty, x: &str, tb: Ty, y: &str) -> bool {
        if x != y || x.contains('?') {
            return false;
        }
        let base = x.trim_end_matches("[]");
        if PRIMITIVES.contains(&base) {
            return true;
        }
        if base.contains('.') {
            return false;
        }
        match (
            self.lookup(ta.0, Some(ta.1), base),
            self.lookup(tb.0, Some(tb.1), base),
        ) {
            (Res::Found(p), Res::Found(q)) => p == q,
            (Res::Found(_), _) | (_, Res::Found(_)) => false,
            _ => {
                let imported = |fi: usize| {
                    self.files[fi]
                        .1
                        .imports
                        .iter()
                        .any(|i| last_segment(&i.path) == base)
                };
                // F2: a class type parameter can share a `java.lang` name.
                let tparam = |t: Ty| {
                    let mut cur = Some(t.1);
                    while let Some(k) = cur {
                        let jt = self.ty((t.0, k));
                        if jt.tparams.iter().any(|p| p == base) {
                            return true;
                        }
                        cur = jt.outer;
                    }
                    false
                };
                LANG_TYPES.contains(&base)
                    && !imported(ta.0)
                    && !imported(tb.0)
                    && !tparam(ta)
                    && !tparam(tb)
            }
        }
    }

    /// The one method `m` of `t`, when the chain holds exactly 1 and no
    /// implicit member has the name.
    fn one_member(&self, t: Ty, m: &str) -> Option<Meth> {
        let mem = self.members(t, m, 0)?;
        match (mem.implicit, mem.methods.as_slice()) {
            (false, [only]) => Some(*only),
            _ => None,
        }
    }

    // ---- JV5 to JV8: the call shapes ---------------------------------------

    /// JV5: the innermost enclosing type that has a member `m`, and the one
    /// method. An unknown chain, or 2 or more hits, stops.
    fn bare_target(&self, ctx: Ty, m: &str) -> Option<(Ty, Meth)> {
        let mut cur = Some(ctx.1);
        while let Some(i) = cur {
            let e = (ctx.0, i);
            let mem = self.members(e, m, 0)?;
            if mem.implicit || !mem.methods.is_empty() {
                return match (mem.implicit, mem.methods.as_slice()) {
                    (false, [only]) => Some((e, *only)),
                    _ => None,
                };
            }
            cur = self.ty(e).outer;
        }
        None
    }

    /// JV6: `super.m()`. The call is not virtual, so only the target counts.
    fn super_target(&self, ctx: Ty, m: &str) -> Option<String> {
        let jt = self.ty(ctx);
        if jt.kind != JavaKind::Class {
            return None;
        }
        let Res::Found(st) = self.super_of(ctx, jt.extends.as_ref()?) else {
            return None;
        };
        let (dt, mi) = self.one_member(st, m)?;
        let dm = &self.ty(dt).methods[mi];
        let visible = dm.vis != 0 && (dm.vis != 1 || self.pkg(dt) == self.pkg(ctx));
        (visible && !dm.is_abstract).then(|| dm.id.clone())
    }

    /// The field `f` that the type `t` declares itself, with its written
    /// type. An inherited field, or an unknown chain, gives `None`.
    fn own_field(&self, t: Ty, f: &str) -> Option<(Ty, Option<String>)> {
        let jt = self.ty(t);
        if let Some((_, ty)) = jt.fields.iter().find(|(n, _)| n == f) {
            return Some((t, ty.clone()));
        }
        None
    }

    /// Reports whether a supertype of `t` declares a field `f`. `Err` means
    /// a supertype is unknown.
    fn inherits_field(&self, t: Ty, f: &str, depth: u32) -> Result<bool, ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        let jt = self.ty(t);
        for s in jt.extends.iter().chain(jt.implements.iter()) {
            match self.super_of(t, s) {
                Res::Found(st) => {
                    let has = self.ty(st).fields.iter().any(|(n, _)| n == f);
                    if has || self.inherits_field(st, f, depth + 1)? {
                        return Ok(true);
                    }
                }
                Res::Jdk(_) => {}
                Res::Stop => return Err(()),
            }
        }
        Ok(false)
    }

    /// JV7 and JV8: `n.m()` where `n` is no local name.
    fn name_target(&self, ctx: Ty, n: &str, m: &str) -> Option<Proof> {
        let rec = &self.files[ctx.0].1;
        // JV8: a field of an enclosing type, innermost first.
        let mut cur = Some(ctx.1);
        while let Some(i) = cur {
            let e = (ctx.0, i);
            if let Some((owner, fty)) = self.own_field(e, n) {
                let recv = self.class_type(owner, &fty?)?;
                let target = self.one_member(recv, m)?;
                return Some(self.dispatch(recv, target, m));
            }
            if self.inherits_field(e, n, 0).ok()? {
                return None;
            }
            cur = self.ty(e).outer;
        }
        // A static import can name a field or a type (fix B5).
        let static_hit = rec
            .imports
            .iter()
            .any(|i| i.stat && (i.star || last_segment(&i.path) == n));
        if static_hit {
            return None;
        }
        // JV7: a type, and one static method.
        let Res::Found(t) = self.lookup(ctx.0, Some(ctx.1), n) else {
            return None;
        };
        let (dt, mi) = self.one_member(t, m)?;
        let dm = &self.ty(dt).methods[mi];
        (dm.is_static && !dm.is_abstract && !dm.id.is_empty()).then(|| Proof {
            id: dm.id.clone(),
            exact: true,
        })
    }

    // ---- JV10: dispatch ----------------------------------------------------

    /// JV10: the compile-time target `target` of a call on a `recv`, and
    /// whether it is also the one runtime target. An abstract or an
    /// overridden target is not.
    fn dispatch(&self, recv: Ty, target: Meth, m: &str) -> Proof {
        let (dt, mi) = target;
        let dm = &self.ty(dt).methods[mi];
        let r = self.ty(recv);
        let fixed =
            dm.is_static || dm.vis == 0 || dm.is_final || self.ty(dt).is_final || r.is_final;
        Proof {
            id: dm.id.clone(),
            exact: !dm.is_abstract
                && !dm.id.is_empty()
                && (fixed || (!self.risky && !self.overridden(&r.name, &self.ty(dt).name, m))),
        }
    }

    /// Reports whether JV10 sees an override of `m` for a receiver of the
    /// simple name `sup` whose target sits in the type `decl`. It looks at
    /// every in-repo subtype, and at every supertype of those subtypes
    /// (an interface default method, or a class method, can override).
    /// The walk matches simple names.
    fn overridden(&self, sup: &str, decl: &str, m: &str) -> bool {
        let mut seen: HashSet<&str> = HashSet::new();
        let mut todo: Vec<&str> = vec![sup];
        let mut visited: Vec<usize> = Vec::new();
        while let Some(s) = todo.pop() {
            for &ei in self.subs.get(s).into_iter().flatten() {
                let e = &self.entries[ei];
                if e.methods.iter().any(|x| x == m) {
                    return true;
                }
                visited.push(ei);
                if let Some(n) = e.name.as_deref() {
                    if seen.insert(n) {
                        todo.push(n);
                    }
                }
            }
        }
        // F1: the supertypes of a subtype that are no supertypes of the
        // receiver. The ancestors of the receiver are in the member union.
        let mut up: Vec<&str> = visited
            .iter()
            .flat_map(|&ei| self.entries[ei].sups.iter().map(String::as_str))
            .collect();
        let mut seen_up: HashSet<&str> = HashSet::new();
        while let Some(s) = up.pop() {
            if s == sup || s == decl || !seen_up.insert(s) {
                continue;
            }
            for &ei in self.by_name.get(s).into_iter().flatten() {
                let e = &self.entries[ei];
                if e.methods.iter().any(|x| x == m) {
                    return true;
                }
                up.extend(e.sups.iter().map(String::as_str));
            }
        }
        false
    }
}
