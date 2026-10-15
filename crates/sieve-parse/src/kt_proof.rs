//! The proof of one Kotlin call. A proof means exactly 1 declaration can be
//! the target, with every source outside the repo closed. The proof covers a
//! bare call `f()`, a constructor call `T(..)`, a call `this.f()`, a call
//! `x.f()`, a call `Type.f()` and a call `super.f()`.
//! Any doubt gives a stop reason, and the edge stays Inferred.

use std::collections::HashSet;
use std::io::Write;

use crate::extract::{KtCall, KtFileRec, KtFun, KtKind, KtRty, KtScope, KtShape};
use crate::kt_index::{split_path, KtIndex, Res, Ty};
use crate::kt_stdlib::{has, EXT_ANY, NO_RECV_BARE, TOP};

/// An implicit receiver of a call.
enum Recv {
    /// A class, an object or a companion.
    Ty(Ty),
    /// The written type of an extension receiver.
    Ext(String),
}

/// A declaration that a call may reach.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Item {
    /// A member function: its type and its place in the type.
    Member(Ty, usize),
    /// A top-level function: its file and its place in the file.
    Top(usize, usize),
    /// A class: constructor call.
    Type(Ty),
    /// A property.
    Prop,
}

type Stop = &'static str;

impl KtIndex {
    /// The node id that the call `call` of `name` in the file `caller`
    /// reaches, or `None` when the index does not prove exactly 1 target.
    pub fn resolve(&self, caller: &str, call: &KtCall, name: &str) -> Option<String> {
        let out = self.prove(caller, call, name);
        if let Some(path) = &self.trace {
            let shape = match call.shape {
                KtShape::Bare => "bare",
                KtShape::This => "this",
                KtShape::Super => "super",
                KtShape::Name => "name",
                KtShape::Other => "other",
            };
            let (id, why) = match &out {
                Ok(id) => (id.as_str(), "ok"),
                Err(why) => ("-", *why),
            };
            let line = trace_line(caller, call.line, name, shape, id, why);
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
            {
                // A trace is a debug aid: a failed write is no error.
                let _ = f.write_all(line.as_bytes());
            }
        }
        out.ok()
    }

    fn prove(&self, caller: &str, call: &KtCall, name: &str) -> Result<String, Stop> {
        let fi = *self.by_path.get(caller).ok_or("no_file")?;
        let rec = &self.files[fi].1;
        if rec.broken {
            return Err("broken_file");
        }
        if rec.script {
            return Err("script");
        }
        if call.name != name {
            return Err("name");
        }
        if call.shape == KtShape::Other {
            return Err("shape");
        }
        let bare = call.shape == KtShape::Bare;
        if call.stop.is_some() {
            let label = matches!(call.stop.as_deref(), Some("label" | "super_typed"));
            return Err(if label { "label" } else { "binder" });
        }
        if self.lost.contains(name) {
            return Err("lost_name");
        }
        if bare && self.java_names.contains(name) {
            return Err("java_type");
        }
        if bare && has(TOP, name) {
            return Err("top_list");
        }
        if !bare && has(EXT_ANY, name) {
            return Err("ext_any");
        }
        self.check_imports(rec, name)?;
        let (recvs, from) = self.receivers(fi, call)?;
        let mut items: Vec<Item> = Vec::new();
        match call.shape {
            KtShape::Bare => self.bare_items(fi, from, &recvs, name, &mut items)?,
            KtShape::This => self.this_items(fi, from, &recvs, name, &mut items)?,
            KtShape::Name => return self.name_call(fi, from, &recvs, call, name),
            _ => return self.super_call(fi, from, call, name),
        }
        self.finish(fi, call, name, items, &[])
    }

    /// The target of the one item that the reach set holds. `skip` lists
    /// the types whose members need no subtype walk.
    fn finish(
        &self,
        fi: usize,
        call: &KtCall,
        name: &str,
        items: Vec<Item>,
        skip: &[Ty],
    ) -> Result<String, Stop> {
        let mut uniq: Vec<Item> = Vec::new();
        for i in items {
            if !uniq.contains(&i) {
                uniq.push(i);
            }
        }
        let item = match uniq.as_slice() {
            [] => return Err("no_candidate"),
            [one] => *one,
            _ => return Err("two_candidates"),
        };
        match item {
            Item::Prop => Err("property"),
            Item::Type(t) => self.constructor(t),
            Item::Top(f, i) => {
                let fun = &self.files[f].1.funs[i];
                if f != fi && fun.mods.iter().any(|w| w == "private") {
                    return Err("private_other_file");
                }
                if fun
                    .mods
                    .iter()
                    .any(|w| matches!(w.as_str(), "abstract" | "expect" | "actual"))
                {
                    return Err("abstract");
                }
                arity(fun, call)?;
                id_of(&fun.id)
            }
            Item::Member(t, i) => self.member(t, i, call, name, skip),
        }
    }

    /// The import stops: a star import of a package outside the repo, an
    /// import of the name from outside the repo, a package that a library
    /// may add to.
    fn check_imports(&self, rec: &KtFileRec, name: &str) -> Result<(), Stop> {
        for imp in &rec.imports {
            if imp.star {
                if !self.repo_pkgs.contains(&imp.path) {
                    return Err("star_outside");
                }
                if self.split.contains(&imp.path) {
                    return Err("split_package");
                }
                continue;
            }
            let eff = imp
                .alias
                .as_deref()
                .unwrap_or_else(|| split_path(&imp.path).1);
            if eff != name {
                continue;
            }
            let (pkg, _) = split_path(&imp.path);
            if !self.repo_pkgs.contains(pkg) {
                return Err("out_of_repo_import");
            }
            if self.split.contains(pkg) {
                return Err("split_package");
            }
        }
        if self.split.contains(&rec.pkg) {
            return Err("split_package");
        }
        Ok(())
    }

    /// The implicit receivers of a call, innermost first, and the innermost
    /// type around the call. Every lambda and body around the call must pass.
    fn receivers(&self, fi: usize, call: &KtCall) -> Result<(Vec<Recv>, Option<Ty>), Stop> {
        let rec = &self.files[fi].1;
        let mut recvs = Vec::new();
        let mut from = None;
        for scope in &call.ctx {
            match scope {
                KtScope::Fun { ext: Some(e), .. } => {
                    if e == "?" {
                        return Err("unknown_chain");
                    }
                    recvs.push(Recv::Ext(e.clone()));
                }
                KtScope::Fun { .. } => {}
                KtScope::Class { name, line }
                | KtScope::Object { name, line }
                | KtScope::Companion { name, line } => {
                    let t = self.type_at(fi, name, *line).ok_or("unknown_chain")?;
                    from = from.or(Some(t));
                    recvs.push(Recv::Ty(t));
                }
                KtScope::Body { .. } => return Err("anon_body"),
                KtScope::Lambda {
                    call: c,
                    recv,
                    bound,
                } => {
                    let imported = rec.imports.iter().any(|i| {
                        !i.star
                            && i.alias.as_deref().unwrap_or_else(|| split_path(&i.path).1) == c
                            && !self.repo_pkgs.contains(split_path(&i.path).0)
                    });
                    let plain = !*recv
                        && !*bound
                        && !imported
                        && has(NO_RECV_BARE, c)
                        && !self.all_names.contains(c.as_str())
                        && !self.lost.contains(c.as_str())
                        && !rec.gradle;
                    if !plain {
                        return Err("lambda_receiver");
                    }
                }
            }
        }
        Ok((recvs, from))
    }

    /// The types whose members an implicit receiver reaches. A class also
    /// reaches the companions of its chain, if `companions` is true.
    fn reach_types(
        &self,
        fi: usize,
        from: Option<Ty>,
        recv: &Recv,
        name: &str,
        companions: bool,
    ) -> Result<Vec<Ty>, Stop> {
        let t = match recv {
            Recv::Ty(t) => *t,
            Recv::Ext(e) => match self.lookup(fi, from, e) {
                Res::Repo(t) => return self.chain_of(t, name),
                _ => return Err("unknown_chain"),
            },
        };
        let mut out = self.chain_of(t, name)?;
        if companions {
            for x in out.clone() {
                if let Some(co) = self.companion_of(x) {
                    out.extend(self.chain_of(co, name)?);
                }
            }
        }
        Ok(out)
    }

    fn chain_of(&self, t: Ty, name: &str) -> Result<Vec<Ty>, Stop> {
        let c = self.chain(t, name).map_err(|()| "unknown_chain")?;
        if c.implicit {
            return Err("implicit_member");
        }
        Ok(c.tys)
    }

    /// The members named `name` of the types, as items.
    fn members_of(&self, tys: &[Ty], name: &str, ext_only: bool, items: &mut Vec<Item>) {
        let mut seen: HashSet<Ty> = HashSet::new();
        for t in tys.iter().filter(|t| seen.insert(**t)) {
            let ty = self.ty(*t);
            for (i, m) in ty.methods.iter().enumerate() {
                if m.name == name && (!ext_only || m.ext_recv.is_some()) {
                    items.push(Item::Member(*t, i));
                }
            }
            if ty.props.iter().any(|p| p.name == name) {
                items.push(Item::Prop);
            }
        }
    }

    /// The top-level functions and properties named `name` that the file
    /// sees. `ext_only` keeps the extensions.
    fn top_items(&self, fi: usize, name: &str, ext_only: bool, items: &mut Vec<Item>) {
        for key in &self.places(fi, name) {
            for (f, i) in self.top_funs.get(key).into_iter().flatten() {
                if !ext_only || self.files[*f].1.funs[*i].ext_recv.is_some() {
                    items.push(Item::Top(*f, *i));
                }
            }
            for (f, i) in self.top_props.get(key).into_iter().flatten() {
                if !ext_only || self.files[*f].1.props[*i].ext_recv.is_some() {
                    items.push(Item::Prop);
                }
            }
        }
    }

    /// The package and name keys where the file `fi` sees a top-level
    /// declaration `name`.
    fn places(&self, fi: usize, name: &str) -> Vec<(String, String)> {
        let rec = &self.files[fi].1;
        let mut places: Vec<(&str, &str)> = vec![(rec.pkg.as_str(), name)];
        for imp in &rec.imports {
            let (pkg, last) = split_path(&imp.path);
            if imp.star && self.repo_pkgs.contains(&imp.path) {
                places.push((&imp.path, name));
            } else if !imp.star && imp.alias.as_deref().unwrap_or(last) == name {
                places.push((pkg, last));
            }
        }
        let mut seen: HashSet<(&str, &str)> = HashSet::new();
        places
            .into_iter()
            .filter(|p| seen.insert(*p))
            .map(|(p, n)| (p.to_string(), n.to_string()))
            .collect()
    }

    /// The reach set of a bare call `f()`.
    fn bare_items(
        &self,
        fi: usize,
        from: Option<Ty>,
        recvs: &[Recv],
        name: &str,
        items: &mut Vec<Item>,
    ) -> Result<(), Stop> {
        let mut tys = Vec::new();
        for r in recvs {
            tys.extend(self.reach_types(fi, from, r, name, true)?);
        }
        self.members_of(&tys, name, false, items);
        self.top_items(fi, name, false, items);
        match self.lookup(fi, from, name) {
            Res::Repo(t) => items.push(Item::Type(t)),
            Res::Unknown => return Err("unknown_type"),
            Res::Outside => {}
        }
        Ok(())
    }

    /// The reach set of a call `this.f()`: the members of the innermost
    /// receiver, and the extensions the file sees.
    fn this_items(
        &self,
        fi: usize,
        from: Option<Ty>,
        recvs: &[Recv],
        name: &str,
        items: &mut Vec<Item>,
    ) -> Result<(), Stop> {
        let first = recvs.first().ok_or("unknown_chain")?;
        let tys = self.reach_types(fi, from, first, name, false)?;
        self.members_of(&tys, name, false, items);
        let mut outer = Vec::new();
        for r in recvs.iter().skip(1) {
            outer.extend(self.reach_types(fi, from, r, name, false)?);
        }
        self.members_of(&outer, name, true, items);
        self.top_items(fi, name, true, items);
        Ok(())
    }

    /// The proof of a constructor call.
    fn constructor(&self, t: Ty) -> Result<String, Stop> {
        let ty = self.ty(t);
        if ty.kind != KtKind::Class {
            return Err("not_ctor");
        }
        let bad = ["abstract", "sealed", "annotation", "expect", "actual"];
        if ty.mods.iter().any(|w| bad.contains(&w.as_str())) {
            return Err("abstract");
        }
        let invoke = self.companion_of(t).is_some_and(|c| {
            self.ty(c).methods.iter().any(|m| m.name == "invoke")
                || self.invoke_ext.contains("?")
                || self.invoke_ext.contains(&self.ty(c).name)
        });
        if invoke {
            return Err("companion_invoke");
        }
        id_of(&ty.id)
    }

    /// The proof of a member function: dispatch must give 1 runtime target.
    fn member(
        &self,
        t: Ty,
        i: usize,
        call: &KtCall,
        name: &str,
        skip: &[Ty],
    ) -> Result<String, Stop> {
        let ty = self.ty(t);
        let m = &ty.methods[i];
        let any = |mods: &[String], set: &[&str]| mods.iter().any(|w| set.contains(&w.as_str()));
        let open_set = ["abstract", "expect", "actual"];
        if ty.kind == KtKind::Interface || any(&m.mods, &open_set) || any(&ty.mods, &open_set) {
            return Err("abstract");
        }
        arity(m, call)?;
        // A member in `skip` is called on an exact receiver: no walk.
        let fixed = any(&m.mods, &["private", "final"])
            || matches!(ty.kind, KtKind::Object | KtKind::Companion)
            || (m.ext_recv.is_none() && skip.contains(&t));
        if !fixed {
            if let Some(why) = self.subtype_stop(t, name) {
                return Err(why);
            }
        }
        id_of(&m.id)
    }
}

/// A property that the name `x` may mean, with the scope of its declaration.
struct PropAt {
    file: usize,
    from: Option<Ty>,
    ty: Option<String>,
    ext: bool,
    private_other: bool,
}

impl KtIndex {
    /// The call `x.f()` on a typed value.
    fn name_call(
        &self,
        fi: usize,
        from: Option<Ty>,
        recvs: &[Recv],
        call: &KtCall,
        name: &str,
    ) -> Result<String, Stop> {
        let x = call.recv.as_deref().ok_or("no_recv")?;
        self.check_imports(&self.files[fi].1, x)?;
        let t = match &call.rty {
            Some(KtRty::Stop(_)) => return Err("receiver_binder"),
            Some(KtRty::Param { ty, .. } | KtRty::Val { ty, .. }) => {
                if call.cast {
                    return Err("cast");
                }
                self.value_type(fi, from, ty, tparam(call, ty))?
            }
            Some(KtRty::New { ty }) => {
                if call.cast {
                    return Err("cast");
                }
                self.new_type(fi, from, recvs, ty, tparam(call, ty))?
            }
            None => {
                let props = self.props_named(fi, from, recvs, x)?;
                match props.as_slice() {
                    [] => return self.type_call(fi, from, recvs, call, name, x),
                    [p] => {
                        if call.cast {
                            return Err("cast");
                        }
                        // A type or a library name `x` may beat the property.
                        if has(TOP, x) {
                            return Err("top_list");
                        }
                        if self.lost.contains(x) {
                            return Err("lost_name");
                        }
                        if self.lookup(fi, from, x) != Res::Outside {
                            return Err("type_named_x");
                        }
                        if p.ext {
                            return Err("ext_property");
                        }
                        if p.private_other {
                            return Err("private_other_file");
                        }
                        let ty = p.ty.as_deref().ok_or("untyped_property")?;
                        self.value_type(p.file, p.from, ty, false)?
                    }
                    _ => return Err("two_properties"),
                }
            }
        };
        let c = self.chain_of(t, name)?;
        let mut items = self.plain_members(&c, name);
        self.ext_items(fi, from, recvs, name, &c, &mut items)?;
        self.finish(fi, call, name, items, &[])
    }

    /// The type that the written name `ty` means in the scope `from` of the
    /// file `fi`.
    fn value_type(&self, fi: usize, from: Option<Ty>, ty: &str, tparam: bool) -> Result<Ty, Stop> {
        if tparam {
            return Err("type_param");
        }
        match self.lookup(fi, from, ty) {
            Res::Repo(t) => Ok(t),
            Res::Outside => Err("outside_type"),
            Res::Unknown => Err("unknown_type"),
        }
    }

    /// The type of `val x = T(..)`: the bare proof of `T(..)` must reach the
    /// class node.
    fn new_type(
        &self,
        fi: usize,
        from: Option<Ty>,
        recvs: &[Recv],
        ty: &str,
        tparam: bool,
    ) -> Result<Ty, Stop> {
        let t = self.value_type(fi, from, ty, tparam)?;
        if self.java_names.contains(ty) || self.lost.contains(ty) || has(TOP, ty) {
            return Err("new_not_proved");
        }
        self.check_imports(&self.files[fi].1, ty)?;
        let mut items = Vec::new();
        self.bare_items(fi, from, recvs, ty, &mut items)?;
        if items != [Item::Type(t)] {
            return Err("new_not_proved");
        }
        self.constructor(t)?;
        Ok(t)
    }

    /// The properties `x` that the implicit receivers and the file reach.
    fn props_named(
        &self,
        fi: usize,
        from: Option<Ty>,
        recvs: &[Recv],
        x: &str,
    ) -> Result<Vec<PropAt>, Stop> {
        let mut tys: Vec<Ty> = Vec::new();
        for r in recvs {
            for t in self.reach_types(fi, from, r, x, true)? {
                if !tys.contains(&t) {
                    tys.push(t);
                }
            }
        }
        let mut out = Vec::new();
        for t in tys {
            for p in self.ty(t).props.iter().filter(|p| p.name == x) {
                out.push(PropAt {
                    file: t.0,
                    from: Some(t),
                    ty: p.ty.clone(),
                    ext: p.ext_recv.is_some(),
                    private_other: false,
                });
            }
        }
        for key in self.places(fi, x) {
            for (f, i) in self.top_props.get(&key).into_iter().flatten() {
                let p = &self.files[*f].1.props[*i];
                out.push(PropAt {
                    file: *f,
                    from: None,
                    ty: p.ty.clone(),
                    ext: p.ext_recv.is_some(),
                    private_other: *f != fi && p.mods.iter().any(|w| w == "private"),
                });
            }
        }
        Ok(out)
    }

    /// The members named `name` of the types, without the extensions.
    fn plain_members(&self, tys: &[Ty], name: &str) -> Vec<Item> {
        let mut items = Vec::new();
        self.members_of(tys, name, false, &mut items);
        items.retain(|i| match i {
            Item::Member(t, n) => self.ty(*t).methods[*n].ext_recv.is_none(),
            _ => true,
        });
        items
    }

    /// The extensions `name` that the call may reach: member extensions of
    /// the implicit receivers and the top-level extensions of the file. An
    /// extension on a type outside `chain` drops out.
    fn ext_items(
        &self,
        fi: usize,
        from: Option<Ty>,
        recvs: &[Recv],
        name: &str,
        chain: &[Ty],
        items: &mut Vec<Item>,
    ) -> Result<(), Stop> {
        let mut outer = Vec::new();
        for r in recvs {
            outer.extend(self.reach_types(fi, from, r, name, true)?);
        }
        let mut ext = Vec::new();
        self.members_of(&outer, name, true, &mut ext);
        self.top_items(fi, name, true, &mut ext);
        items.extend(ext.into_iter().filter(|i| self.ext_applies(i, chain)));
        Ok(())
    }

    /// Whether the extension may take a receiver of a type in `chain`. The
    /// receiver name resolves in the file and type of the extension.
    /// An unknown receiver stays in the set.
    fn ext_applies(&self, item: &Item, chain: &[Ty]) -> bool {
        let (file, from, recv) = match *item {
            Item::Member(t, i) => (t.0, Some(t), self.ty(t).methods[i].ext_recv.as_deref()),
            Item::Top(f, i) => (f, None, self.files[f].1.funs[i].ext_recv.as_deref()),
            _ => return true,
        };
        let generic = match *item {
            Item::Member(t, i) => &self.ty(t).methods[i].tparams,
            Item::Top(f, i) => &self.files[f].1.funs[i].tparams,
            _ => return true,
        };
        match recv {
            None | Some("?") => true,
            // A type parameter receiver takes any type.
            Some(r) if generic.iter().any(|p| p == r) => true,
            Some(r) => match self.lookup(file, from, r) {
                Res::Repo(x) => chain.contains(&x),
                _ => true,
            },
        }
    }

    /// The call `Type.f()`.
    fn type_call(
        &self,
        fi: usize,
        from: Option<Ty>,
        recvs: &[Recv],
        call: &KtCall,
        name: &str,
        x: &str,
    ) -> Result<String, Stop> {
        if tparam(call, x) {
            return Err("type_param");
        }
        let mut cur = from;
        while let Some(o) = cur {
            if self.ty(o).entries.iter().any(|e| e == x) {
                return Err("value_named_x");
            }
            cur = self.outer_of(o);
        }
        let t = match self.lookup(fi, from, x) {
            Res::Repo(t) => t,
            Res::Outside => return Err("outside_type"),
            Res::Unknown => return Err("unknown_type"),
        };
        let ty = self.ty(t);
        let mut items: Vec<Item> = self.nested(t, name).into_iter().map(Item::Type).collect();
        // The receiver is exact: an object, or the companion of a class.
        let mut exact = Vec::new();
        if matches!(ty.kind, KtKind::Object | KtKind::Companion) {
            exact = self.chain_of(t, name)?;
        } else {
            if ty.entries.iter().any(|e| e == name) {
                return Err("enum_entry");
            }
            // The class chain must be known and free of implicit members.
            self.chain_of(t, name)?;
            if let Some(co) = self.companion_of(t) {
                exact = self.chain_of(co, name)?;
            }
        }
        items.extend(self.plain_members(&exact, name));
        if !exact.is_empty() {
            self.ext_items(fi, from, recvs, name, &exact, &mut items)?;
        }
        self.finish(fi, call, name, items, &exact)
    }

    /// The call `super.f()`. No subtype walk: a `super` call does not
    /// dispatch.
    fn super_call(
        &self,
        fi: usize,
        from: Option<Ty>,
        call: &KtCall,
        name: &str,
    ) -> Result<String, Stop> {
        for s in &call.ctx {
            match s {
                KtScope::Fun { ext: Some(_), .. } => return Err("ext_super"),
                KtScope::Class { .. } | KtScope::Object { .. } | KtScope::Companion { .. } => {
                    break;
                }
                _ => {}
            }
        }
        let t = from.ok_or("unknown_chain")?;
        let sups: Vec<Ty> = self
            .chain_of(t, name)?
            .into_iter()
            .filter(|x| *x != t)
            .collect();
        let items = self.plain_members(&sups, name);
        self.finish(fi, call, name, items, &sups)
    }
}

/// Whether a type parameter of an enclosing function has the name `n`.
fn tparam(call: &KtCall, n: &str) -> bool {
    call.ctx.iter().any(|s| match s {
        KtScope::Fun { tparams, .. } => tparams.iter().any(|p| p == n),
        _ => false,
    })
}

/// The arity stops: a named or spread argument, `vararg`, a wrong count.
fn arity(f: &KtFun, call: &KtCall) -> Result<(), Stop> {
    if call.loose {
        return Err("loose");
    }
    if f.vararg {
        return Err("vararg");
    }
    if call.arity < f.n.saturating_sub(f.d) || call.arity > f.n {
        return Err("arity");
    }
    Ok(())
}

fn id_of(id: &str) -> Result<String, Stop> {
    if id.is_empty() {
        Err("no_id")
    } else {
        Ok(id.to_string())
    }
}

/// One line of the trace file.
pub(crate) fn trace_line(
    file: &str,
    line: u32,
    name: &str,
    shape: &str,
    target: &str,
    why: &str,
) -> String {
    format!("{file}:{line}\t{name}\t{shape}\t{target}\t{why}\n")
}

#[cfg(test)]
#[path = "kt_proof_tests.rs"]
mod tests;
