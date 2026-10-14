//! The type index of a repo's C# files (the C#
//! call proof). The resolver builds it from the file records
//! and asks it to prove each `CsCall`. A proof means the C# compile-time
//! target is also the one runtime target. Any doubt gives `None`. A bare
//! call then stays Inferred, and a member call or a `new` makes no edge.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use crate::cs_tree::last_segment;
use crate::extract::{
    CsCall, CsFileRec, CsKind, CsMethod, CsRec, CsShape, CsType, CsUsing, RawEdge,
};

/// A type declaration: the file index and the type index in that file.
type Ty = (usize, u32);

/// A method: its logical type, its declaration and its index in
/// `CsType::methods`.
type Meth = (usize, Ty, usize);

/// The most nested type lookups one proof makes.
const MAX_DEPTH: u32 = 48;

/// The method names of `object`, which every class and struct has.
const OBJECT: &[&str] = &[
    "Equals",
    "GetHashCode",
    "ToString",
    "GetType",
    "MemberwiseClone",
    "ReferenceEquals",
    "Finalize",
];

/// The member names that a record adds.
const RECORD: &[&str] = &["Deconstruct", "PrintMembers", "EqualityContract", "Clone"];

/// The BCL classes a chain may hold, with the member names the code
/// freezes for each.
const FROZEN_CLASSES: &[(&str, &[&str])] = &[
    (
        "Exception",
        &[
            "Message",
            "InnerException",
            "StackTrace",
            "Source",
            "HelpLink",
            "Data",
            "TargetSite",
            "HResult",
            "GetBaseException",
            "GetObjectData",
            "SerializeObjectState",
        ],
    ),
    (
        "Attribute",
        &[
            "Match",
            "IsDefaultAttribute",
            "TypeId",
            "GetCustomAttribute",
            "GetCustomAttributes",
            "IsDefined",
        ],
    ),
    ("EventArgs", &["Empty"]),
    ("ValueType", &[]),
];

/// The namespaces whose extension methods the code freezes in
/// `FROZEN_EXT`.
const FROZEN_NS: &[&str] = &[
    "System",
    "System.Collections",
    "System.Collections.Generic",
    "System.Collections.Concurrent",
    "System.ComponentModel",
    "System.Diagnostics",
    "System.Diagnostics.CodeAnalysis",
    "System.Globalization",
    "System.IO",
    "System.Linq",
    "System.Net",
    "System.Net.Http",
    "System.Reflection",
    "System.Runtime.CompilerServices",
    "System.Runtime.InteropServices",
    "System.Text",
    "System.Text.RegularExpressions",
    "System.Threading",
    "System.Threading.Tasks",
];

/// The extension method names of `FROZEN_NS`. A call with
/// one of these names never gets a member proof.
const FROZEN_EXT: &[&str] = &[
    "AddRange",
    "IndexOfAnyExcept",
    "Shuffle",
    "TryGetNonEnumeratedCount",
    "ToAsyncEnumerable",
    "AggregateAsync",
    "AggregateByAsync",
    "AllAsync",
    "AnyAsync",
    "AverageAsync",
    "ContainsAsync",
    "CountAsync",
    "CountByAsync",
    "ElementAtAsync",
    "ElementAtOrDefaultAsync",
    "FirstAsync",
    "FirstOrDefaultAsync",
    "LastAsync",
    "LastOrDefaultAsync",
    "LongCountAsync",
    "MaxAsync",
    "MaxByAsync",
    "MinAsync",
    "MinByAsync",
    "SequenceEqualAsync",
    "SingleAsync",
    "SingleOrDefaultAsync",
    "SumAsync",
    "ToArrayAsync",
    "ToDictionaryAsync",
    "ToHashSetAsync",
    "ToListAsync",
    "ToLookupAsync",
    "Aggregate",
    "AggregateBy",
    "All",
    "Any",
    "Append",
    "AsEnumerable",
    "AsMemory",
    "AsOrdered",
    "AsParallel",
    "AsQueryable",
    "AsReadOnly",
    "AsSequential",
    "AsSpan",
    "AsUnordered",
    "Average",
    "BinarySearch",
    "Cast",
    "Chunk",
    "CommonPrefixLength",
    "CompareTo",
    "Concat",
    "ConfigureAwait",
    "Contains",
    "ContainsAny",
    "ContainsAnyExcept",
    "Convert",
    "CopyTo",
    "Count",
    "CountBy",
    "Deconstruct",
    "DefaultIfEmpty",
    "Distinct",
    "DistinctBy",
    "ElementAt",
    "ElementAtOrDefault",
    "EndsWith",
    "EnumerateLines",
    "EnumerateRunes",
    "Equals",
    "Except",
    "ExceptBy",
    "First",
    "FirstOrDefault",
    "ForAll",
    "GetAwaiter",
    "GetBytes",
    "GetChars",
    "GetCustomAttribute",
    "GetCustomAttributes",
    "GetMetadataToken",
    "GetMethodInfo",
    "GetRuntimeBaseDefinition",
    "GetRuntimeEvent",
    "GetRuntimeEvents",
    "GetRuntimeField",
    "GetRuntimeFields",
    "GetRuntimeInterfaceMap",
    "GetRuntimeMethod",
    "GetRuntimeMethods",
    "GetRuntimeProperties",
    "GetRuntimeProperty",
    "GetString",
    "GetTypeInfo",
    "GetValueOrDefault",
    "GroupBy",
    "GroupJoin",
    "HasMetadataToken",
    "Index",
    "IndexOf",
    "IndexOfAny",
    "InsertRange",
    "Intersect",
    "IntersectBy",
    "IsDefined",
    "IsWhiteSpace",
    "Join",
    "Last",
    "LastIndexOf",
    "LastIndexOfAny",
    "LastOrDefault",
    "LeftJoin",
    "LongCount",
    "Max",
    "MaxBy",
    "Min",
    "MinBy",
    "OfType",
    "Order",
    "OrderBy",
    "OrderByDescending",
    "OrderDescending",
    "Overlaps",
    "Prepend",
    "Remove",
    "Replace",
    "Reverse",
    "RightJoin",
    "Select",
    "SelectMany",
    "SequenceCompareTo",
    "SequenceEqual",
    "Single",
    "SingleOrDefault",
    "Skip",
    "SkipLast",
    "SkipWhile",
    "Sort",
    "Split",
    "SplitAny",
    "StartsWith",
    "Sum",
    "Take",
    "TakeLast",
    "TakeWhile",
    "ThenBy",
    "ThenByDescending",
    "ToArray",
    "ToBlockingEnumerable",
    "ToDictionary",
    "ToHashSet",
    "ToList",
    "ToLookup",
    "ToLower",
    "ToLowerInvariant",
    "ToUpper",
    "ToUpperInvariant",
    "Trim",
    "TrimEnd",
    "TrimStart",
    "TryAdd",
    "TryWrite",
    "Union",
    "UnionBy",
    "Unwrap",
    "WaitAsync",
    "Where",
    "WithCancellation",
    "WithDegreeOfParallelism",
    "WithExecutionMode",
    "WithMergeOptions",
    "Zip",
];

/// The predefined type names, which no `using` or alias can change.
const PREDEFINED: &[&str] = &[
    "bool", "byte", "sbyte", "char", "decimal", "double", "float", "int", "uint", "long", "ulong",
    "short", "ushort", "object", "string", "nint", "nuint",
];

/// The result of a type name lookup.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Res {
    /// An in-repo type, by logical index.
    Found(usize),
    /// A BCL class of the frozen list.
    Frozen(&'static str),
    /// No in-repo type has the name, and no using hides one.
    Missing,
    /// The lookup cannot decide.
    Stop,
}

/// The base class of a type.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Base {
    /// An in-repo class.
    Lt(usize),
    /// A BCL class of the frozen list.
    Frozen(&'static str),
}

/// The members named `m` of a type.
struct Members {
    methods: Vec<Meth>,
    /// A field, a property, an event, a nested type or a frozen member has
    /// the name.
    implicit: bool,
}

/// One logical type: the parts of a partial type merge into one.
struct Lt {
    parts: Vec<Ty>,
    name: String,
    kind: CsKind,
    unknown: bool,
    partial: bool,
    sealed: bool,
    stat: bool,
    abs: bool,
}

/// A type that names supertypes.
struct SubEntry {
    name: String,
    methods: Vec<String>,
}

/// The types of a repo's C# files.
#[derive(Default)]
pub struct CsIndex {
    files: Vec<(String, CsFileRec)>,
    by_path: HashMap<String, usize>,
    lts: Vec<Lt>,
    lt_of: HashMap<Ty, usize>,
    /// The full name of each type, nested types included.
    fqn: HashMap<String, Vec<usize>>,
    /// The full name of each top-level type.
    top: HashMap<String, Vec<usize>>,
    /// The declared namespaces and their prefixes.
    nss: HashSet<String>,
    /// Every `global using` of the repo, with the file that holds it.
    globals: Vec<(usize, CsUsing)>,
    /// The names of the in-repo extension methods.
    ext_names: HashSet<String>,
    /// A file with a parse error holds an extension method signature, so
    /// a lost extension method may take a member call.
    ext_lost: bool,
    /// The names of the extension methods in the text of the files with a
    /// parse error.
    lost_ext_names: HashSet<String>,
    /// The words after `override` in the files with a parse error.
    lost_overrides: HashSet<String>,
    /// The type names declared in the text of the files with a parse error.
    lost_types: HashSet<String>,
    /// Whether each file can show every extension method in scope.
    ext_ok: Vec<bool>,
    entries: Vec<SubEntry>,
    /// A supertype simple name to the entries that name it.
    subs: HashMap<String, Vec<usize>>,
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
    /// The compile-time target is also the one runtime target.
    exact: bool,
}

/// Reports whether `s` is a type that no scope can change: a predefined
/// type, an array of one, or a nullable one.
fn predefined_only(s: &str) -> bool {
    let s = s
        .trim_start_matches("ref ")
        .trim_start_matches("out ")
        .trim_start_matches("in ");
    let mut base = s;
    loop {
        let next = base
            .strip_suffix("[]")
            .or_else(|| base.strip_suffix('?'))
            .or_else(|| base.strip_suffix(','));
        match next {
            Some(n) => base = n,
            None => break,
        }
    }
    PREDEFINED.contains(&base)
}

/// Reports whether `s` names an interface by the `IName` convention.
fn looks_like_interface(s: &str) -> bool {
    let mut it = s.chars();
    matches!(
        (it.next(), it.next(), it.next()),
        (Some('I'), Some(b), Some(c)) if b.is_ascii_uppercase() && c.is_ascii_lowercase()
    )
}

/// `s` without its `<..>` groups, nested groups included.
fn strip_generics(s: &str) -> String {
    let mut depth = 0u32;
    let mut out = String::new();
    for ch in s.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            c if depth == 0 && !c.is_whitespace() => out.push(c),
            _ => {}
        }
    }
    out
}

/// The namespaces that enclose `ns`, innermost first, ending with the
/// global namespace `""`.
fn levels(ns: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = ns;
    while !cur.is_empty() {
        out.push(cur.to_string());
        cur = cur.rsplit_once('.').map_or("", |(a, _)| a);
    }
    out.push(String::new());
    out
}

impl CsIndex {
    /// Reads the file records of `raw`.
    pub fn build(raw: &[RawEdge]) -> Self {
        let mut ix = CsIndex::default();
        for e in raw {
            let Some(CsRec::File(rec)) = &e.cs else {
                continue;
            };
            let fi = ix.files.len();
            ix.by_path.insert(e.file.clone(), fi);
            ix.files.push((e.file.clone(), (**rec).clone()));
            ix.ext_lost |= rec.lost_ext;
            ix.lost_ext_names.extend(rec.lost_ext_names.iter().cloned());
            ix.lost_overrides.extend(rec.lost_overrides.iter().cloned());
            ix.lost_types.extend(rec.lost_types.iter().cloned());
        }
        ix.build_types();
        for e in raw {
            let Some(CsRec::Cross(call)) = &e.cs else {
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

    /// Groups the type declarations into logical types and fills the
    /// name maps.
    fn build_types(&mut self) {
        let files = std::mem::take(&mut self.files);
        let mut groups: Vec<(String, Vec<Ty>)> = Vec::new();
        let mut group_of: HashMap<String, Vec<usize>> = HashMap::new();
        for (fi, (_, rec)) in files.iter().enumerate() {
            for (ti, t) in rec.types.iter().enumerate() {
                let mut names = vec![t.name.as_str()];
                let mut cur = t.outer;
                while let Some(o) = cur {
                    names.push(rec.types[o as usize].name.as_str());
                    cur = rec.types[o as usize].outer;
                }
                names.reverse();
                let path = names.join(".");
                let full = if t.ns.is_empty() {
                    path
                } else {
                    format!("{}.{path}", t.ns)
                };
                let ty = (fi, ti as u32);
                // Partial parts of one name merge. Any other twin stays
                // apart, so a lookup of the name stops.
                let slot = group_of.get(&full).and_then(|g| {
                    g.iter().copied().find(|&gi| {
                        t.partial
                            && groups[gi]
                                .1
                                .iter()
                                .all(|p| files[p.0].1.types[p.1 as usize].partial)
                    })
                });
                match slot {
                    Some(gi) => groups[gi].1.push(ty),
                    None => {
                        group_of.entry(full.clone()).or_default().push(groups.len());
                        groups.push((full, vec![ty]));
                    }
                }
                let mut ns = t.ns.as_str();
                while !ns.is_empty() {
                    self.nss.insert(ns.to_string());
                    ns = ns.rsplit_once('.').map_or("", |(a, _)| a);
                }
            }
            for u in rec.usings.iter().filter(|u| u.global) {
                self.globals.push((fi, u.clone()));
            }
        }
        for (full, parts) in groups {
            let first = &files[parts[0].0].1.types[parts[0].1 as usize];
            let ts: Vec<&CsType> = parts
                .iter()
                .map(|p| &files[p.0].1.types[p.1 as usize])
                .collect();
            let li = self.lts.len();
            self.lts.push(Lt {
                name: first.name.clone(),
                kind: first.kind,
                // A type of a broken file is unknown, and so is a
                // type whose name the text of a broken file declares.
                unknown: ts.iter().any(|t| t.unknown || t.kind != first.kind)
                    || parts.iter().any(|p| files[p.0].1.broken)
                    || self.lost_types.contains(&first.name),
                partial: ts.iter().any(|t| t.partial),
                sealed: ts.iter().any(|t| t.sealed),
                stat: ts.iter().any(|t| t.is_static),
                abs: ts.iter().any(|t| t.is_abstract),
                parts: parts.clone(),
            });
            for p in &parts {
                self.lt_of.insert(*p, li);
            }
            if first.outer.is_none() {
                self.top.entry(full.clone()).or_default().push(li);
            }
            self.fqn.entry(full).or_default().push(li);
            for (p, t) in parts.iter().zip(ts) {
                let entry = SubEntry {
                    name: t.name.clone(),
                    methods: t.methods.iter().map(|m| m.name.clone()).collect(),
                };
                let ei = self.entries.len();
                self.entries.push(entry);
                for b in &t.bases {
                    // Fix B-B: a base written through an alias is also a
                    // subtype of the alias target.
                    for name in self.base_names(&files[p.0].1, b) {
                        self.subs.entry(name).or_default().push(ei);
                    }
                }
                self.ext_names.extend(
                    t.methods
                        .iter()
                        .filter(|m| m.is_ext)
                        .map(|m| m.name.clone()),
                );
            }
        }
        self.ext_ok = (0..files.len())
            .map(|fi| self.ext_scope_ok(&files, fi))
            .collect();
        self.files = files;
    }

    /// The simple names that the written base `b` of a type in `rec`
    /// can mean: its own last segment, and the target of each alias of
    /// that name in the file or in a `global using`.
    fn base_names(&self, rec: &CsFileRec, b: &str) -> Vec<String> {
        let mut out = vec![last_segment(b).to_string()];
        let mut i = 0;
        while i < out.len() && out.len() < 16 {
            let name = out[i].clone();
            let mine = rec.usings.iter();
            let global = self.globals.iter().map(|(_, u)| u);
            for u in mine.chain(global) {
                if u.alias.as_deref() == Some(name.as_str()) {
                    let target = strip_generics(&u.path);
                    let seg = last_segment(&target).to_string();
                    if !seg.is_empty() && !out.contains(&seg) {
                        out.push(seg);
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// Reports whether every namespace in scope of the file `fi` is an
    /// in-repo namespace or one of the frozen BCL namespaces.
    fn ext_scope_ok(&self, files: &[(String, CsFileRec)], fi: usize) -> bool {
        let rec = &files[fi].1;
        if rec.extern_alias {
            return false;
        }
        let mine = rec.usings.iter();
        let global = self.globals.iter().map(|(_, u)| u);
        mine.chain(global).all(|u| {
            if u.alias.is_some() {
                return true;
            }
            if u.stat {
                return self.fqn.contains_key(&u.path);
            }
            self.nss.contains(&u.path) || FROZEN_NS.contains(&u.path.as_str())
        })
    }

    fn ty(&self, t: Ty) -> &CsType {
        &self.files[t.0].1.types[t.1 as usize]
    }

    /// The logical type of the declaration `t`.
    fn lt_at(&self, t: Ty) -> Option<usize> {
        self.lt_of.get(&t).copied()
    }

    /// The node id that the call `call` of `name` in the file `caller`
    /// reaches, or `None` when the index does not prove exactly 1 target.
    pub fn resolve(&self, caller: &str, source: &str, call: &CsCall, name: &str) -> Option<String> {
        let p = self.proof(caller, call, name)?;
        let key = (source.to_string(), p.id);
        (p.exact && !self.tainted.contains(&key)).then_some(key.1)
    }

    /// The compile-time target of the call and whether it is also the one
    /// runtime target.
    fn proof(&self, caller: &str, call: &CsCall, name: &str) -> Option<Proof> {
        let fi = *self.by_path.get(caller)?;
        let rec = &self.files[fi].1;
        if rec.broken || rec.dynamic || call.name != name {
            return None;
        }
        rec.types.get(call.ctx as usize)?;
        self.depth.set(0);
        let ctx: Ty = (fi, call.ctx);
        let ctx_lt = self.lt_at(ctx)?;
        if self.lts[ctx_lt].unknown {
            return None;
        }
        // A generated part of a partial base class can hold a member.
        if !matches!(call.shape, CsShape::New { .. }) && self.partial_base(ctx_lt)? {
            return None;
        }
        let site = self.site_chain(ctx);
        match &call.shape {
            CsShape::Bare => {
                let (recv, m) = self.bare_target(fi, &site, name, call.arity)?;
                Some(self.dispatch(recv, m, name))
            }
            CsShape::This => {
                if self.ext_stop(fi, name) {
                    return None;
                }
                self.member_call(fi, &site, ctx_lt, name, call.arity)
            }
            CsShape::Var { ty } => {
                if self.ext_stop(fi, name) {
                    return None;
                }
                let recv = self.receiver_type(ctx, ty)?;
                if let Some(p) = &call.recv {
                    // Fix B-C: a member of the chain hides the parameter.
                    let hide = self.members(ctx_lt, p, 0)?;
                    if hide.implicit || !hide.methods.is_empty() {
                        return None;
                    }
                }
                self.member_call(fi, &site, recv, name, call.arity)
            }
            CsShape::Base => {
                if self.ext_stop(fi, name) {
                    return None;
                }
                self.base_call(fi, &site, ctx_lt, name, call.arity)
            }
            CsShape::Field { name: f } => {
                if self.ext_stop(fi, name) {
                    return None;
                }
                self.field_call(fi, &site, ctx, f, name, call.arity)
            }
            CsShape::Name { name: x } => {
                if self.own_field(ctx_lt, x).is_some() {
                    // Color Color: a field named like a type stops.
                    let res = self.lookup(fi, Some(ctx.1), &self.ty(ctx).ns, x);
                    if res != Res::Missing || self.ext_stop(fi, name) {
                        return None;
                    }
                    return self.field_call(fi, &site, ctx, x, name, call.arity);
                }
                self.static_call(fi, &site, ctx, x, name, call.arity)
            }
            CsShape::New { ty } => {
                let Res::Found(l) = self.lookup(fi, Some(ctx.1), &self.ty(ctx).ns, ty) else {
                    return None;
                };
                let lt = &self.lts[l];
                let id = &self.ty(lt.parts[0]).id;
                let creatable = matches!(lt.kind, CsKind::Class | CsKind::Record)
                    && !lt.abs
                    && !lt.stat
                    && !lt.partial
                    && lt.parts.len() == 1
                    && !lt.unknown;
                (creatable && !id.is_empty()).then(|| Proof {
                    id: id.clone(),
                    exact: true,
                })
            }
        }
    }

    /// The logical types that enclose the call, innermost first.
    fn site_chain(&self, ctx: Ty) -> Vec<usize> {
        let mut out = Vec::new();
        let mut cur = Some(ctx.1);
        while let Some(i) = cur {
            out.extend(self.lt_at((ctx.0, i)));
            cur = self.ty((ctx.0, i)).outer;
        }
        out
    }

    /// The stops of a call `x.M()` and `this.M()`. An extension
    /// method, or a lost one in a broken file, can take the call.
    fn ext_stop(&self, fi: usize, m: &str) -> bool {
        self.ext_lost
            || self.files[fi].1.proj.as_ref().is_some_and(|p| p.risky)
            || self.lost_ext_names.contains(m)
            || !self.ext_ok.get(fi).copied().unwrap_or(false)
            || self.ext_names.contains(m)
            || FROZEN_EXT.contains(&m)
    }

    /// The class, struct or record type that the written name `ty`
    /// names.
    fn receiver_type(&self, ctx: Ty, ty: &str) -> Option<usize> {
        match self.lookup(ctx.0, Some(ctx.1), &self.ty(ctx).ns, ty) {
            Res::Found(l)
                if !self.lts[l].unknown
                    && matches!(
                        self.lts[l].kind,
                        CsKind::Class | CsKind::Struct | CsKind::Record
                    ) =>
            {
                Some(l)
            }
            _ => None,
        }
    }

    /// `x.M()` and `this.M()`: the one instance method of the receiver.
    fn member_call(
        &self,
        fi: usize,
        site: &[usize],
        recv: usize,
        m: &str,
        arity: u32,
    ) -> Option<Proof> {
        let meth = self.one_member(fi, site, recv, m, arity)?;
        if self.method(meth).is_static {
            return None;
        }
        Some(self.dispatch(recv, meth, m))
    }

    /// `base.M()`. The call is non-virtual, so the nearest member up
    /// the chain from the base class is also the runtime target. An
    /// abstract target stops.
    fn base_call(
        &self,
        fi: usize,
        site: &[usize],
        ctx_lt: usize,
        m: &str,
        arity: u32,
    ) -> Option<Proof> {
        let Some(Base::Lt(b)) = self.base_class(ctx_lt).ok()? else {
            return None;
        };
        let meth = self.one_member(fi, site, b, m, arity)?;
        let dm = self.method(meth);
        let in_interface = self.lts[meth.0].kind == CsKind::Interface;
        if dm.is_static || dm.is_abstract || in_interface || dm.id.is_empty() {
            return None;
        }
        Some(Proof {
            id: dm.id.clone(),
            exact: true,
        })
    }

    /// The one field `f` that the type `t` declares: the declaring part
    /// and the written type. A property, an event, a constant, a member
    /// type, a second field or a method of the name gives `None`.
    fn own_field(&self, t: usize, f: &str) -> Option<(Ty, Option<String>)> {
        let mut hits = Vec::new();
        let mut others = 0;
        for &p in &self.lts[t].parts {
            let ty = self.ty(p);
            others += ty.others.iter().filter(|o| *o == f).count();
            if ty.methods.iter().any(|x| x.name == f) {
                return None;
            }
            let mine = ty.fields.iter().filter(|x| x.0 == f);
            hits.extend(mine.map(|x| (p, x.1.clone())));
        }
        match hits.as_slice() {
            [one] if others == 1 => Some(one.clone()),
            _ => None,
        }
    }

    /// `f.M()` and `this.f.M()` for a field `f` of the call's class.
    /// The field type is written, and no base class has a member `f`.
    fn field_call(
        &self,
        fi: usize,
        site: &[usize],
        ctx: Ty,
        f: &str,
        m: &str,
        arity: u32,
    ) -> Option<Proof> {
        let ctx_lt = self.lt_at(ctx)?;
        let (decl, ty) = self.own_field(ctx_lt, f)?;
        if self.hidden_above(ctx_lt, f)? {
            return None;
        }
        let recv = self.receiver_type(decl, &ty?)?;
        self.member_call(fi, site, recv, m, arity)
    }

    /// Reports whether a class in the base chain of `t` is partial.
    /// `None` when the chain is unknown.
    fn partial_base(&self, t: usize) -> Option<bool> {
        let mut cur = t;
        for _ in 0..MAX_DEPTH {
            match self.base_class(cur).ok()? {
                Some(Base::Lt(b)) if self.lts[b].partial => return Some(true),
                Some(Base::Lt(b)) => cur = b,
                _ => return Some(false),
            }
        }
        None
    }

    /// Reports whether a base class of `t` has a member named `f`.
    fn hidden_above(&self, t: usize, f: &str) -> Option<bool> {
        match self.base_class(t).ok()? {
            Some(Base::Lt(b)) => {
                let mem = self.members(b, f, 0)?;
                Some(mem.implicit || !mem.methods.is_empty())
            }
            Some(Base::Frozen(n)) => {
                Some(FROZEN_CLASSES.iter().any(|c| c.0 == n && c.1.contains(&f)))
            }
            None => Some(OBJECT.contains(&f)),
        }
    }

    /// `X.M()` where `X` is no local name. No member of an enclosing
    /// class has the name, and the type `X` resolves by the type-name rules. The target is
    /// one static method.
    fn static_call(
        &self,
        fi: usize,
        site: &[usize],
        ctx: Ty,
        x: &str,
        m: &str,
        arity: u32,
    ) -> Option<Proof> {
        let rec = &self.files[fi].1;
        let global_stat = self.globals.iter().any(|(_, u)| u.stat);
        if global_stat || rec.usings.iter().any(|u| u.stat) {
            return None;
        }
        // A namespace segment of the name can hide the type.
        if self.nss.iter().any(|n| n.split('.').any(|s| s == x)) {
            return None;
        }
        // A generated part of a partial type can hold `x`.
        if site.iter().any(|&e| self.lts[e].partial) {
            return None;
        }
        for &e in site {
            let mem = self.members(e, x, 0)?;
            let pctor = self.lts[e]
                .parts
                .iter()
                .any(|&p| self.ty(p).pctor.iter().any(|(n, _)| n == x));
            if mem.implicit || !mem.methods.is_empty() || pctor {
                return None;
            }
        }
        let kind_ok = |k| matches!(k, CsKind::Class | CsKind::Struct | CsKind::Record);
        let l = match self.lookup(ctx.0, Some(ctx.1), &self.ty(ctx).ns, x) {
            Res::Found(l) if kind_ok(self.lts[l].kind) => l,
            _ => return None,
        };
        let meth = self.one_member(fi, site, l, m, arity)?;
        if !self.method(meth).is_static {
            return None;
        }
        Some(self.dispatch(l, meth, m))
    }

    fn method(&self, m: Meth) -> &CsMethod {
        &self.ty(m.1).methods[m.2]
    }

    // ---- Type names -------------------------------------------------

    /// The type that the simple name `n` names in the file `fi`, seen
    /// from the type `from` (`None` at the top of the file). `ns` is the
    /// namespace of the type that holds the use.
    fn lookup(&self, fi: usize, from: Option<u32>, ns: &str, n: &str) -> Res {
        let d = self.depth.get();
        if d > MAX_DEPTH {
            return Res::Stop;
        }
        self.depth.set(d + 1);
        let res = self.lookup_inner(fi, from, ns, n);
        self.depth.set(d);
        res
    }

    fn lookup_inner(&self, fi: usize, from: Option<u32>, ns: &str, n: &str) -> Res {
        let rec = &self.files[fi].1;
        // Step 1: a type parameter, or a member type of an enclosing type.
        let mut cur = from;
        while let Some(i) = cur {
            let t = &rec.types[i as usize];
            if t.tparams.iter().any(|p| p == n) {
                return Res::Stop;
            }
            let Some(lt) = self.lt_at((fi, i)) else {
                return Res::Stop;
            };
            match self.member_type(lt, n, 0) {
                Err(()) => return Res::Stop,
                Ok(Some(found)) => return self.checked(found),
                Ok(None) => {}
            }
            cur = t.outer;
        }
        // An alias, an extern alias or a `using` inside a
        // namespace block stops the rest.
        let alias = |u: &CsUsing| u.alias.as_deref() == Some(n);
        if rec.extern_alias
            || rec.inner_using
            || rec.usings.iter().any(alias)
            || self.globals.iter().any(|(_, u)| alias(u))
        {
            return Res::Stop;
        }
        // Step 2: each enclosing namespace, innermost first.
        for level in levels(ns) {
            let key = if level.is_empty() {
                n.to_string()
            } else {
                format!("{level}.{n}")
            };
            match self.top.get(&key).map(Vec::as_slice) {
                Some([one]) => return self.checked(*one),
                Some(_) => return Res::Stop,
                None => {}
            }
        }
        // Step 3: the `using` directives of the file, then the global ones.
        let mut own: Vec<usize> = Vec::new();
        let mut global: Vec<usize> = Vec::new();
        let mine = rec.usings.iter().filter(|u| !u.global).map(|u| (fi, u));
        let all = self.globals.iter().map(|(f, u)| (*f, u));
        for (f, u) in mine.chain(all) {
            if u.alias.is_some() || u.stat {
                continue;
            }
            let key = format!("{}.{n}", u.path);
            let hits = self.top.get(&key).into_iter().flatten().copied();
            if f == fi {
                own.extend(hits);
            } else {
                global.extend(hits);
            }
        }
        own.sort_unstable();
        own.dedup();
        global.sort_unstable();
        global.dedup();
        match (own.as_slice(), global.as_slice()) {
            ([one], g) if g.iter().all(|x| x == one) => return self.checked(*one),
            ([], []) => {}
            // A type found only through a global
            // `using` of another file stops.
            _ => return Res::Stop,
        }
        match FROZEN_CLASSES.iter().find(|c| c.0 == n) {
            Some(c) => Res::Frozen(c.0),
            None => Res::Missing,
        }
    }

    /// A found type that the proof cannot read gives `Stop`.
    fn checked(&self, l: usize) -> Res {
        if self.lts[l].unknown {
            Res::Stop
        } else {
            Res::Found(l)
        }
    }

    /// The member type `n` of the type `t`: its own, else an inherited
    /// one. `Err` means a base class is unknown.
    fn member_type(&self, t: usize, n: &str, depth: u32) -> Result<Option<usize>, ()> {
        if depth > MAX_DEPTH {
            return Err(());
        }
        for &p in &self.lts[t].parts {
            if let Some((_, i)) = self.ty(p).members.iter().find(|(m, _)| m == n) {
                return Ok(self.lt_at((p.0, *i)));
            }
        }
        if let Some(Base::Lt(b)) = self.base_class(t)? {
            if let Some(found) = self.member_type(b, n, depth + 1)? {
                // A private type of a base class is not inherited. A
                // private protected type needs the assembly, so it stops.
                match self.ty(self.lts[found].parts[0]).vis {
                    0 => {}
                    1 => return Err(()),
                    _ => return Ok(Some(found)),
                }
            }
        }
        Ok(None)
    }

    // ---- The chain --------------------------------------------------

    /// The base class of the type `t`. `Ok(None)` means `object`.
    /// `Err` means the chain is unknown.
    fn base_class(&self, t: usize) -> Result<Option<Base>, ()> {
        let mut out: Option<Base> = None;
        for &p in &self.lts[t].parts {
            let ty = self.ty(p);
            let found = match ty.kind {
                CsKind::Struct => Some(Base::Frozen("ValueType")),
                CsKind::Class | CsKind::Record => match ty.bases.first() {
                    Some(first) => self.first_base(p, first)?,
                    None => None,
                },
                CsKind::Interface => None,
                CsKind::Enum | CsKind::Delegate => return Err(()),
            };
            match (out, found) {
                (_, None) => {}
                (None, f) => out = f,
                (Some(a), Some(b)) if a == b => {}
                _ => return Err(()),
            }
        }
        Ok(out)
    }

    /// The class that the first entry `first` of a base list names, or
    /// `None` when the entry is an interface or `object`.
    fn first_base(&self, p: Ty, first: &str) -> Result<Option<Base>, ()> {
        let ty = self.ty(p);
        let name = if first.contains('.') {
            // Only a plain `System.X` name stays readable.
            let rest = first.strip_prefix("System.").ok_or(())?;
            if rest.contains('.') {
                return Err(());
            }
            rest
        } else {
            first
        };
        if matches!(name, "object" | "Object") && !first.contains('.') {
            return Ok(None);
        }
        if first.contains('.') {
            return match FROZEN_CLASSES.iter().find(|c| c.0 == name) {
                Some(c) => Ok(Some(Base::Frozen(c.0))),
                None if looks_like_interface(name) => Ok(None),
                None => Err(()),
            };
        }
        match self.lookup(p.0, ty.outer, &ty.ns, name) {
            Res::Found(l) => match self.lts[l].kind {
                CsKind::Interface => Ok(None),
                CsKind::Class | CsKind::Record => Ok(Some(Base::Lt(l))),
                _ => Err(()),
            },
            Res::Frozen(f) => Ok(Some(Base::Frozen(f))),
            Res::Missing if looks_like_interface(name) => Ok(None),
            _ => Err(()),
        }
    }

    // ---- Members ----------------------------------------------------

    /// Every member of `t` called `m`, over the whole chain. `None`
    /// when the chain is unknown.
    fn members(&self, t: usize, m: &str, depth: u32) -> Option<Members> {
        if depth > MAX_DEPTH || self.lts[t].unknown {
            return None;
        }
        let lt = &self.lts[t];
        let mut out = Members {
            methods: Vec::new(),
            implicit: OBJECT.contains(&m),
        };
        for &p in &lt.parts {
            let ty = self.ty(p);
            out.methods.extend(
                ty.methods
                    .iter()
                    .enumerate()
                    .filter(|(_, x)| x.name == m)
                    .map(|(i, _)| (t, p, i)),
            );
            out.implicit |= ty.others.iter().any(|o| o == m);
        }
        if lt.kind == CsKind::Record {
            out.implicit |= RECORD.contains(&m);
        }
        match self.base_class(t).ok()? {
            Some(Base::Lt(b)) => {
                let sub = self.members(b, m, depth + 1)?;
                out.implicit |= sub.implicit;
                for cand in sub.methods {
                    self.inherit(cand, &mut out.methods);
                }
            }
            Some(Base::Frozen(f)) => {
                out.implicit |= FROZEN_CLASSES.iter().any(|c| c.0 == f && c.1.contains(&m));
            }
            None => {}
        }
        Some(out)
    }

    /// Adds the method `cand` of a base class to the members `out`, unless
    /// a method of the derived type has the same signature and so replaces
    /// it. An `override` replaces a base method of the same written
    /// signature. Any other method replaces it only when every parameter
    /// has a predefined type, because a written name can mean two types.
    fn inherit(&self, cand: Meth, out: &mut Vec<Meth>) {
        if out.contains(&cand) {
            return;
        }
        let c = self.method(cand);
        let replaced = out.iter().any(|&slot| {
            if slot.0 == cand.0 {
                return false;
            }
            let s = self.method(slot);
            s.params == c.params
                && !s.params.iter().any(|p| p.contains('?'))
                && (s.is_override || s.params.iter().all(|p| predefined_only(p)))
        });
        if !replaced {
            out.push(cand);
        }
    }

    /// Reports whether the member `vis` of the type `decl` is accessible
    /// from the call site. `None` means the chain is unknown.
    fn accessible(&self, fi: usize, site: &[usize], decl: usize, vis: u8) -> Option<bool> {
        // An assembly-bound member of another project stops.
        if matches!(vis, 1 | 3 | 4) && self.cross_project(fi, decl) {
            return None;
        }
        match vis {
            0 => Some(site.contains(&decl)),
            1 | 2 => {
                for &x in site {
                    if x == decl || self.derives(x, decl)? {
                        return Some(true);
                    }
                }
                Some(false)
            }
            _ => Some(true),
        }
    }

    /// Reports whether the type `decl` sits in another project than the
    /// file `fi`. A file with no project is never in another project.
    fn cross_project(&self, fi: usize, decl: usize) -> bool {
        // A missing project on either side counts as another one.
        let Some(a) = self.files[fi].1.proj.as_ref() else {
            return true;
        };
        self.lts[decl].parts.iter().any(|p| {
            self.files[p.0]
                .1
                .proj
                .as_ref()
                .is_none_or(|b| b.dir != a.dir)
        })
    }

    /// Reports whether the class `x` derives from the class `base`.
    fn derives(&self, x: usize, base: usize) -> Option<bool> {
        let mut cur = x;
        for _ in 0..MAX_DEPTH {
            match self.base_class(cur).ok()? {
                Some(Base::Lt(b)) if b == base => return Some(true),
                Some(Base::Lt(b)) => cur = b,
                _ => return Some(false),
            }
        }
        None
    }

    /// The accessible methods named `m` of `t`, and whether a non-method
    /// or frozen member has the name. `None` when the chain is unknown.
    fn visible(&self, fi: usize, site: &[usize], t: usize, m: &str) -> Option<(Vec<Meth>, bool)> {
        let mem = self.members(t, m, 0)?;
        let mut out = Vec::new();
        for cand in mem.methods {
            if self.accessible(fi, site, cand.0, self.method(cand).vis)? {
                out.push(cand);
            }
        }
        Some((out, mem.implicit))
    }

    /// The one method `m` of `t` for a call of `arity` arguments. A
    /// non-method member of the name, an overload, a partial method, a
    /// `params` or optional parameter, or a wrong count stops.
    fn one_member(&self, fi: usize, site: &[usize], t: usize, m: &str, arity: u32) -> Option<Meth> {
        let (methods, implicit) = self.visible(fi, site, t, m)?;
        match (implicit, methods.as_slice()) {
            (false, [only]) => self.fit(*only, arity),
            _ => None,
        }
    }

    /// The method `meth` when a call of `arity` arguments fits it exactly.
    fn fit(&self, meth: Meth, arity: u32) -> Option<Meth> {
        let m = self.method(meth);
        (!m.loose && !m.is_partial && m.arity == arity).then_some(meth)
    }

    // ---- Bare calls -------------------------------------------------

    /// The innermost enclosing type that has a member `m`, and the one
    /// method. The first chain with any member of the name decides.
    fn bare_target(&self, fi: usize, site: &[usize], m: &str, arity: u32) -> Option<(usize, Meth)> {
        for &e in site {
            let (methods, implicit) = self.visible(fi, site, e, m)?;
            if implicit || !methods.is_empty() {
                return match (implicit, methods.as_slice()) {
                    (false, [only]) => Some((e, self.fit(*only, arity)?)),
                    _ => None,
                };
            }
            // A generated part of a partial type can hold `m`.
            if self.lts[e].partial {
                return None;
            }
        }
        None
    }

    // ---- Dispatch --------------------------------------------------

    /// The compile-time target `target` of a call on a `recv`, and
    /// whether it is also the one runtime target. An abstract or an
    /// overridden target is not.
    fn dispatch(&self, recv: usize, target: Meth, m: &str) -> Proof {
        let dm = self.method(target);
        let d = &self.lts[target.0];
        let r = &self.lts[recv];
        let fixed = dm.is_static
            || dm.vis == 0
            || !dm.is_virtual
            || (dm.is_override && dm.is_sealed)
            || d.sealed
            || d.stat
            || d.kind == CsKind::Struct
            || r.sealed
            || r.kind == CsKind::Struct;
        Proof {
            id: dm.id.clone(),
            exact: !dm.is_abstract
                && d.kind != CsKind::Interface
                && !dm.id.is_empty()
                && (fixed || !self.overridden(&r.name, m)),
        }
    }

    /// Reports whether the dispatch check sees an override of `m` for a receiver of the
    /// simple name `sup`. It looks at every in-repo subtype, transitively.
    /// The walk matches simple names.
    fn overridden(&self, sup: &str, m: &str) -> bool {
        // A lost override is in the text of a broken file.
        if self.lost_overrides.contains(m) {
            return true;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        let mut todo: Vec<&str> = vec![sup];
        while let Some(s) = todo.pop() {
            for &ei in self.subs.get(s).into_iter().flatten() {
                let e = &self.entries[ei];
                if e.methods.iter().any(|x| x == m) {
                    return true;
                }
                if seen.insert(e.name.as_str()) {
                    todo.push(e.name.as_str());
                }
            }
        }
        false
    }
}

#[cfg(test)]
#[path = "cs_index_tests.rs"]
mod tests;
