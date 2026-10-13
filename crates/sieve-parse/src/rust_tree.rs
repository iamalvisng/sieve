//! The Rust crate table, the module tree and the name lookup (RSX1, design
//! `rust-xfile.md` rules R1, R2, R3, R5, R6, R7, R9, R13 and R14). A call
//! gets an exact target only when the lookup ends at exactly one free
//! `fn`. Every doubt gives `Unknown`, and the call takes the capped name
//! path in `resolve.rs`.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::extract::{RawEdge, RustRec};
use crate::rust_items::{ImplFn, RItem, RKind};

/// One Cargo package: its directory, its name, its edition and its target
/// root files, all relative to the repo root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustPackage {
    /// The package directory (posix, `""` for the repo root).
    pub dir: String,
    /// The `[package] name`.
    pub name: String,
    /// The edition year; 2015 when the manifest names none.
    pub edition: u16,
    /// The target root files (`src/lib.rs`, `src/bin/*.rs`, `tests/*.rs`, ...).
    pub roots: Vec<String>,
    /// The crates each root file sees by name (R8): the lib root file of
    /// the crate, or `None` when the name is blocked.
    pub externs: BTreeMap<String, BTreeMap<String, Option<String>>>,
}

/// Which table of a manifest holds a dependency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepScope {
    Normal,
    Dev,
    Build,
    Workspace,
}

/// One dependency entry as the line scanner reads it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Dep {
    scope: DepScope,
    key: String,
    path: Option<String>,
    workspace: bool,
    /// A rename, a target table, or an entry the scanner cannot read.
    blocked: bool,
}

/// What the line scanner reads from one `Cargo.toml`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Manifest {
    is_package: bool,
    name: Option<String>,
    edition: EditionSpec,
    paths: Vec<String>,
    /// Each declared target path with its kind.
    path_kinds: Vec<(String, &'static str)>,
    lib_name: Option<String>,
    /// True when the manifest has a `[workspace]` table.
    is_workspace: bool,
    deps: Vec<Dep>,
    default_build: bool,
    /// Target kinds whose auto-discovery the manifest turns off.
    auto_off: Vec<&'static str>,
    ws_edition: Option<u16>,
}

#[derive(Debug, Default, PartialEq, Eq)]
enum EditionSpec {
    #[default]
    Unset,
    Year(u16),
    Workspace,
}

/// Cuts the comment off one line. Gives the text and its net bracket depth,
/// or `None` for a string that does not close on the line.
fn scan_line(line: &str) -> Option<(String, i32)> {
    let mut out = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    let mut depth = 0;
    for c in line.chars() {
        if let Some(q) = quote {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' && q == '"' {
                escaped = true;
            } else if c == q {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => {
                quote = Some(c);
                out.push(c);
            }
            '#' => break,
            '[' | '{' => {
                depth += 1;
                out.push(c);
            }
            ']' | '}' => {
                depth -= 1;
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    quote.is_none().then_some((out, depth))
}

fn string_value(v: &str) -> Option<String> {
    let v = v.trim();
    let quoted = v.len() >= 2
        && ((v.starts_with('"') && v.ends_with('"')) || (v.starts_with('\'') && v.ends_with('\'')));
    quoted.then(|| v[1..v.len() - 1].to_string())
}

/// Reads one `Cargo.toml`. Gives `None` when a line is unreadable, which
/// voids the package.
pub(crate) fn parse_manifest(text: &str) -> Option<Manifest> {
    let mut m = Manifest {
        default_build: true,
        ..Manifest::default()
    };
    let mut section = String::new();
    let mut open = 0;
    let mut multi: Option<&str> = None;
    for line in text.lines() {
        if let Some(delim) = multi {
            if line.contains(delim) {
                multi = None;
            }
            continue;
        }
        // A multi-line string holds no key this scanner needs.
        let triple = ["\"\"\"", "'''"].into_iter().find(|d| line.contains(*d));
        if let Some(delim) = triple {
            if line.matches(delim).count() % 2 == 1 {
                multi = Some(delim);
            }
            continue;
        }
        let (line, delta) = scan_line(line)?;
        if open > 0 {
            open += delta;
            continue;
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t.starts_with('[') {
            if !t.ends_with(']') || delta != 0 {
                return None;
            }
            section = t.trim_matches(|c| c == '[' || c == ']').trim().to_string();
            m.is_package |= section == "package";
            m.is_workspace |= section == "workspace";
            continue;
        }
        let (key, value) = t.split_once('=')?;
        let (key, value) = (key.trim(), value.trim());
        open = delta;
        if open < 0 {
            return None;
        }
        read_entry(&mut m, &section, key, value)?;
    }
    Some(m)
}

fn read_entry(m: &mut Manifest, section: &str, key: &str, value: &str) -> Option<()> {
    match section {
        "" => return None,
        "package" => match key {
            "name" => m.name = Some(string_value(value)?),
            "edition" => {
                let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
                m.edition = if compact == "{workspace=true}" {
                    EditionSpec::Workspace
                } else {
                    EditionSpec::Year(string_value(value)?.parse().ok()?)
                };
            }
            "edition.workspace" => m.edition = EditionSpec::Workspace,
            "build" if value == "false" => m.default_build = false,
            "build" => {
                m.default_build = false;
                let p = string_value(value)?;
                m.paths.push(p.clone());
                m.path_kinds.push((p, "build"));
            }
            "autolib" | "autobins" | "autotests" | "autoexamples" | "autobenches"
                if value == "false" =>
            {
                m.auto_off.push(match key {
                    "autolib" => "lib",
                    "autobins" => "bin",
                    "autotests" => "test",
                    "autoexamples" => "example",
                    _ => "bench",
                });
            }
            _ => {}
        },
        "lib" | "bin" | "test" | "example" | "bench" if key == "path" => {
            let p = string_value(value)?;
            m.paths.push(p.clone());
            let kind = match section {
                "lib" => "lib",
                "bin" => "bin",
                "test" => "test",
                "example" => "example",
                _ => "bench",
            };
            m.path_kinds.push((p, kind));
        }
        "lib" if key == "name" => m.lib_name = Some(string_value(value)?),
        "workspace.package" if key == "edition" => m.ws_edition = string_value(value)?.parse().ok(),
        _ => {
            if let Some((scope, in_name, target)) = dep_section(section) {
                read_dep(m, scope, in_name, target, key, value);
            }
        }
    }
    Some(())
}

/// Reads a dependency table name. Gives its scope, the dependency key when
/// the name is `<table>.<key>`, and whether it is a `target.` table.
fn dep_section(section: &str) -> Option<(DepScope, Option<&str>, bool)> {
    const TABLES: [(&str, DepScope); 4] = [
        ("dependencies", DepScope::Normal),
        ("dev-dependencies", DepScope::Dev),
        ("build-dependencies", DepScope::Build),
        ("workspace.dependencies", DepScope::Workspace),
    ];
    let target = section.starts_with("target.");
    for (name, scope) in TABLES {
        let rest = if target {
            let dotted = format!(".{name}");
            match section.rfind(&dotted) {
                Some(at) if scope != DepScope::Workspace => &section[at + dotted.len()..],
                _ => continue,
            }
        } else {
            match section.strip_prefix(name) {
                Some(rest) => rest,
                None => continue,
            }
        };
        if rest.is_empty() {
            return Some((scope, None, target));
        }
        if let Some(k) = rest.strip_prefix('.') {
            return Some((scope, Some(unquote(k)), target));
        }
    }
    None
}

fn unquote(k: &str) -> &str {
    k.trim().trim_matches(|c| c == '"' || c == '\'')
}

/// Splits an inline table body at the commas outside quotes and brackets.
fn split_top(body: &str) -> Vec<&str> {
    let (mut out, mut depth, mut start) = (Vec::new(), 0, 0);
    let mut quote: Option<char> = None;
    for (i, c) in body.char_indices() {
        match (quote, c) {
            (Some(q), _) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '[' | '{') => depth += 1,
            (None, ']' | '}') => depth -= 1,
            (None, ',') if depth == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    out.push(&body[start..]);
    out
}

/// Records one line of a dependency table (R8). A shape the scanner cannot
/// read blocks the dependency, which only loses an edge.
fn read_dep(
    m: &mut Manifest,
    scope: DepScope,
    in_name: Option<&str>,
    target: bool,
    key: &str,
    value: &str,
) {
    let (name, attr) = match in_name {
        Some(n) => (n, Some(key)),
        None => match key.split_once('.') {
            Some((n, a)) => (unquote(n), Some(a.trim())),
            None => (unquote(key), None),
        },
    };
    let at = match m
        .deps
        .iter()
        .position(|d| d.scope == scope && d.key == name)
    {
        Some(at) => at,
        None => {
            m.deps.push(Dep {
                scope,
                key: name.to_string(),
                path: None,
                workspace: false,
                blocked: target,
            });
            m.deps.len() - 1
        }
    };
    if target {
        m.deps[at].blocked = true;
    }
    let mut pairs: Vec<(&str, &str)> = Vec::new();
    match attr {
        Some(a) => pairs.push((a, value)),
        None if value.starts_with('{') && value.ends_with('}') => {
            let body = &value[1..value.len() - 1];
            for part in split_top(body).into_iter().filter(|p| !p.trim().is_empty()) {
                match part.split_once('=') {
                    Some((k, v)) => pairs.push((k.trim(), v.trim())),
                    None => m.deps[at].blocked = true,
                }
            }
        }
        None if value.starts_with('{') => m.deps[at].blocked = true,
        None => {}
    }
    let dep = &mut m.deps[at];
    for (k, v) in pairs {
        match k {
            "path" => match string_value(v) {
                Some(p) => dep.path = Some(p),
                None => dep.blocked = true,
            },
            "workspace" => dep.workspace = v == "true",
            "package" => dep.blocked = true,
            _ => {}
        }
    }
}

/// Joins two posix paths; an empty left side gives the right side.
fn join(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else {
        format!("{a}/{b}")
    }
}

/// Removes `.` and `..` segments; `None` when the path leaves the repo.
fn normalize(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?;
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

fn dirname(path: &str) -> String {
    path.rsplit_once('/').map_or("", |(d, _)| d).to_string()
}

/// The package directories whose target root pattern matches file `f`,
/// each with the target kind.
fn target_dirs(f: &str) -> Vec<(String, &'static str)> {
    let parts: Vec<&str> = f.split('/').collect();
    let n = parts.len();
    let dir = |k: usize| parts[..n - k].join("/");
    let last = parts[n - 1];
    let mut out = Vec::new();
    if n >= 2 && parts[n - 2] == "src" && (last == "lib.rs" || last == "main.rs") {
        out.push((dir(2), if last == "lib.rs" { "lib" } else { "bin" }));
    }
    if n >= 3 && parts[n - 3] == "src" && parts[n - 2] == "bin" {
        out.push((dir(3), "bin"));
    }
    if n >= 4 && parts[n - 4] == "src" && parts[n - 3] == "bin" && last == "main.rs" {
        out.push((dir(4), "bin"));
    }
    for (t, kind) in [
        ("tests", "test"),
        ("examples", "example"),
        ("benches", "bench"),
    ] {
        if n >= 2 && parts[n - 2] == t {
            out.push((dir(2), kind));
        }
        if n >= 3 && parts[n - 3] == t && last == "main.rs" {
            out.push((dir(3), kind));
        }
    }
    if last == "build.rs" {
        out.push((dir(1), "build"));
    }
    out
}

/// Builds the crate table from every `Cargo.toml` text (given with its
/// directory) and the repo file list. A manifest with an unreadable line, or
/// with no `[package]`, gives no package.
pub fn packages(manifests: &[(String, String)], files: &[String]) -> Vec<RustPackage> {
    let parsed: Vec<(&str, Manifest)> = manifests
        .iter()
        .filter_map(|(dir, text)| parse_manifest(text).map(|m| (dir.as_str(), m)))
        .collect();
    let file_set: HashSet<&str> = files.iter().map(String::as_str).collect();
    let mut pkgs: PkgMap = HashMap::new();
    for (dir, m) in &parsed {
        let (true, Some(name)) = (m.is_package, m.name.clone()) else {
            continue;
        };
        let edition = match m.edition {
            EditionSpec::Year(y) => y,
            EditionSpec::Unset => 2015,
            EditionSpec::Workspace => parsed
                .iter()
                .filter(|(d, w)| {
                    w.ws_edition.is_some()
                        && (d.is_empty() || *dir == *d || dir.starts_with(&format!("{d}/")))
                })
                .max_by_key(|(d, _)| d.len())
                .and_then(|(_, w)| w.ws_edition)
                .unwrap_or(2015),
        };
        let mut roots = BTreeMap::new();
        for (p, kind) in &m.path_kinds {
            if let Some(f) = normalize(&join(dir, p)).filter(|f| file_set.contains(f.as_str())) {
                roots.entry(f).or_insert(*kind);
            }
        }
        let pkg = RustPackage {
            dir: (*dir).to_string(),
            name,
            edition,
            roots: Vec::new(),
            externs: BTreeMap::new(),
        };
        pkgs.insert(dir, (pkg, m, roots));
    }
    for f in files.iter().filter(|f| f.ends_with(".rs")) {
        for (d, kind) in target_dirs(f) {
            if let Some((_, m, roots)) = pkgs.get_mut(d.as_str()) {
                let off = m.auto_off.contains(&kind) || (kind == "build" && !m.default_build);
                if !off {
                    roots.entry(f.clone()).or_insert(kind);
                }
            }
        }
    }
    let libs = lib_table(&pkgs);
    let mut out: Vec<RustPackage> = pkgs
        .iter()
        .map(|(dir, (pkg, m, roots))| {
            let mut pkg = pkg.clone();
            pkg.roots = roots.keys().cloned().collect();
            for (root, kind) in roots {
                let seen = externs_of(dir, m, kind, &libs, &parsed);
                pkg.externs.insert(root.clone(), seen);
            }
            pkg
        })
        .collect();
    out.sort_by(|a, b| a.dir.cmp(&b.dir));
    out
}

/// Each package with a lib target: its directory, lib name and lib root.
type LibTable = Vec<(String, String, String)>;

type PkgMap<'a> = HashMap<&'a str, (RustPackage, &'a Manifest, BTreeMap<String, &'static str>)>;

fn lib_table(pkgs: &PkgMap) -> LibTable {
    let mut libs = Vec::new();
    for (dir, (pkg, m, roots)) in pkgs {
        let root = roots.iter().find(|(_, k)| **k == "lib").map(|(f, _)| f);
        let Some(root) = root else { continue };
        let name = m
            .lib_name
            .clone()
            .unwrap_or_else(|| pkg.name.replace('-', "_"));
        libs.push(((*dir).to_string(), name, root.clone()));
    }
    libs
}

/// The crates a target root of package `dir` sees by name (R8). A name maps
/// to `None` when it is blocked: a rename, a registry crate, or no single
/// in-repo package with that lib name.
fn externs_of(
    dir: &str,
    m: &Manifest,
    kind: &str,
    libs: &LibTable,
    parsed: &[(&str, Manifest)],
) -> BTreeMap<String, Option<String>> {
    let allowed: &[DepScope] = match kind {
        "lib" | "bin" => &[DepScope::Normal],
        "build" => &[DepScope::Build],
        _ => &[DepScope::Normal, DepScope::Dev],
    };
    let mut seen: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut add = |name: String, root: Option<String>| {
        seen.entry(name)
            .and_modify(|old| {
                if *old != root {
                    *old = None;
                }
            })
            .or_insert(root);
    };
    for d in m.deps.iter().filter(|d| allowed.contains(&d.scope)) {
        add(d.key.replace('-', "_"), dep_root(dir, d, libs, parsed));
    }
    if matches!(kind, "bin" | "test" | "example" | "bench") {
        if let Some((_, name, root)) = libs.iter().find(|(d, _, _)| d == dir) {
            add(name.clone(), Some(root.clone()));
        }
    }
    seen
}

/// The lib root file that dependency `d` of package `dir` names, if exactly
/// one in-repo package with that lib name sits at the `path`.
fn dep_root(dir: &str, d: &Dep, libs: &LibTable, parsed: &[(&str, Manifest)]) -> Option<String> {
    if d.blocked {
        return None;
    }
    let (base, path) = if d.workspace {
        let (wdir, w) = parsed
            .iter()
            .filter(|(_, w)| w.is_workspace)
            .filter(|(wd, _)| wd.is_empty() || dir == *wd || dir.starts_with(&format!("{wd}/")))
            .filter_map(|(wd, w)| {
                let dep = w
                    .deps
                    .iter()
                    .find(|x| x.scope == DepScope::Workspace && x.key == d.key)?;
                Some((*wd, dep))
            })
            .max_by_key(|(wd, _)| wd.len())?;
        if w.blocked {
            return None;
        }
        (wdir, w.path.as_ref()?)
    } else {
        (dir, d.path.as_ref()?)
    };
    let target = normalize(&join(base, path))?;
    let name = d.key.replace('-', "_");
    let mut named = libs.iter().filter(|(_, n, _)| *n == name);
    match (named.next(), named.next()) {
        (Some((ldir, _, root)), None) if *ldir == target => Some(root.clone()),
        _ => None,
    }
}

/// A module key: the file and the inline `mod` chain inside it.
type Mk = (String, Vec<String>);

struct Info {
    root: String,
    parent: Option<Mk>,
}

/// The module tree and the module-level items of a repo's Rust files.
#[derive(Default)]
pub struct RustIndex {
    items: HashMap<Mk, Vec<RItem>>,
    info: HashMap<Mk, Info>,
    visits: HashMap<String, u32>,
    child_files: HashMap<(Mk, String), String>,
    editions: HashMap<String, u16>,
    files: HashSet<String>,
    /// For each target root file, the crates it names (R8).
    externs: HashMap<String, BTreeMap<String, Option<String>>>,
}

/// What the lookup of one call gives.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Exactly one free `fn`, by node id.
    Target(String),
    /// A renamed `use` binds the name and the lookup failed: no edge at all.
    Drop,
    /// No proof: the call keeps the capped name path.
    Unknown,
}

const MAX_DEPTH: u32 = 8;
const MAX_LOOKUPS: u32 = 256;
const MAX_TREE: u32 = 32;

impl RustIndex {
    /// Reads the file records in `raw` and walks the module tree from every
    /// target root of `pkgs`.
    pub fn build(raw: &[RawEdge], pkgs: &[RustPackage]) -> Self {
        let mut ix = Self::default();
        if pkgs.is_empty() {
            return ix;
        }
        for e in raw {
            let Some(RustRec::File { items }) = &e.rust else {
                continue;
            };
            ix.files.insert(e.file.clone());
            ix.items.entry((e.file.clone(), Vec::new())).or_default();
            for it in items {
                ix.items
                    .entry((e.file.clone(), it.mods.clone()))
                    .or_default()
                    .push(it.clone());
            }
        }
        for p in pkgs {
            for (root, seen) in &p.externs {
                ix.externs.insert(root.clone(), seen.clone());
            }
            for r in &p.roots {
                if ix.files.contains(r) {
                    ix.editions.insert(r.clone(), p.edition);
                    ix.enter(r, dirname(r), r, None, 0);
                }
            }
        }
        ix
    }

    fn enter(&mut self, file: &str, dir: String, root: &str, parent: Option<Mk>, depth: u32) {
        let visits = self.visits.entry(file.to_string()).or_default();
        *visits += 1;
        // A file reached twice has no module. Its second walk voids its
        // children; a third walk adds nothing.
        if depth <= MAX_TREE && *visits <= 2 {
            self.module(file, Vec::new(), dir, root, parent, depth);
        }
    }

    fn module(
        &mut self,
        file: &str,
        mods: Vec<String>,
        dir: String,
        root: &str,
        parent: Option<Mk>,
        depth: u32,
    ) {
        let mk = (file.to_string(), mods.clone());
        self.info.entry(mk.clone()).or_insert(Info {
            root: root.to_string(),
            parent,
        });
        let kids: Vec<(String, bool, Option<String>)> = self
            .items
            .get(&mk)
            .map(|v| {
                v.iter()
                    .filter_map(|i| match &i.kind {
                        RKind::Mod { name, inline, path } => {
                            Some((name.clone(), *inline, path.clone()))
                        }
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        for (name, inline, path) in kids {
            if inline {
                if path.is_none() {
                    let mut inner = mods.clone();
                    inner.push(name.clone());
                    let sub = join(&dir, &name);
                    self.module(file, inner, sub, root, Some(mk.clone()), depth + 1);
                }
                continue;
            }
            let cands: Vec<(String, String)> = match path {
                Some(_) if !mods.is_empty() => continue,
                Some(p) => {
                    let Some(f) = normalize(&join(&dirname(file), &p)) else {
                        continue;
                    };
                    let d = dirname(&f);
                    vec![(f, d)]
                }
                None => {
                    let sub = join(&dir, &name);
                    vec![
                        (format!("{sub}.rs"), sub.clone()),
                        (format!("{sub}/mod.rs"), sub),
                    ]
                }
            };
            let present: Vec<&(String, String)> = cands
                .iter()
                .filter(|(f, _)| self.files.contains(f))
                .collect();
            if let [(g, gdir)] = present[..] {
                let (g, gdir) = (g.clone(), gdir.clone());
                self.child_files.insert((mk.clone(), name), g.clone());
                self.enter(&g, gdir, root, Some(mk.clone()), depth + 1);
            }
        }
    }

    /// True when `mk` is a module of a file that exactly one root reaches once.
    fn live(&self, mk: &Mk) -> bool {
        self.info.contains_key(mk) && self.visits.get(&mk.0) == Some(&1)
    }

    fn child(&self, mk: &Mk, name: &str) -> Option<Mk> {
        let items = self.items.get(mk)?;
        let item = items.iter().find_map(|i| match &i.kind {
            RKind::Mod {
                name: n, inline, ..
            } if n == name => Some(*inline),
            _ => None,
        })?;
        let child = if item {
            let mut m = mk.1.clone();
            m.push(name.to_string());
            (mk.0.clone(), m)
        } else {
            (
                self.child_files
                    .get(&(mk.clone(), name.to_string()))?
                    .clone(),
                Vec::new(),
            )
        };
        self.live(&child).then_some(child)
    }

    fn is_ancestor(&self, t: &Mk, m: &Mk) -> bool {
        let mut cur = Some(m.clone());
        while let Some(c) = cur {
            if &c == t {
                return true;
            }
            cur = self.info.get(&c).and_then(|i| i.parent.clone());
        }
        false
    }

    /// R10: the id of the one inherent `fn f` of the struct, enum or union
    /// that `prefix` names, or `None` when any doubt remains.
    fn type_assoc(&self, m: &Mk, prefix: &[String], f: &str, generic: bool) -> Option<String> {
        let mut walk = Walk {
            ix: self,
            budget: MAX_LOOKUPS,
            stack: Vec::new(),
        };
        let Res::Ty(tm, tname) = walk.resolve_path(m, prefix, Ns::Type, 0) else {
            return None;
        };
        let root = &self.info.get(&tm)?.root;
        if self.has_macro(&tm) {
            return None;
        }
        let is_variant = self.items.get(&tm)?.iter().any(|i| {
            matches!(&i.kind, RKind::Adt { name, variants, .. }
                if *name == tname && variants.iter().any(|v| v == f))
        });
        if is_variant {
            return None;
        }
        let mut found: Vec<(&ImplFn, &Mk)> = Vec::new();
        let mut blocks = 0;
        for (mk, items) in &self.items {
            for it in items {
                let RKind::Impl {
                    ty,
                    tr,
                    opaque,
                    fns,
                } = &it.kind
                else {
                    continue;
                };
                if generic && !*tr && ty.last() == Some(&tname) {
                    blocks += 1;
                }
                if !fns.iter().any(|x| x.name == f) {
                    continue;
                }
                if !self.live(mk) {
                    // A file that two roots reach has no module: any doubt blocks.
                    if self.visits.get(&mk.0).is_some_and(|v| *v >= 2) {
                        return None;
                    }
                    continue;
                }
                if self.info.get(mk)?.root != *root {
                    continue;
                }
                if *opaque || ty.is_empty() {
                    return None;
                }
                let mut w = Walk {
                    ix: self,
                    budget: MAX_LOOKUPS,
                    stack: Vec::new(),
                };
                match w.resolve_path(mk, ty, Ns::Type, 0) {
                    Res::Ty(a, b) if a == tm && b == tname => {}
                    Res::Ty(..) | Res::Zero => continue,
                    _ => return None,
                }
                if *tr || it.cfg || self.has_macro(mk) {
                    return None;
                }
                found.extend(fns.iter().filter(|x| x.name == f).map(|x| (x, mk)));
            }
        }
        let [(one, imk)] = found[..] else {
            return None;
        };
        if generic && blocks > 1 {
            return None;
        }
        // A private fn is visible in the impl module and its descendants only.
        let seen = one.public || self.is_ancestor(imk, m);
        (!one.cfg && seen).then(|| one.id.clone())
    }

    fn has_macro(&self, mk: &Mk) -> bool {
        self.items
            .get(mk)
            .is_some_and(|v| v.iter().any(|i| i.kind == RKind::Macro))
    }

    /// Looks up the callee `path` of a call made in module (`file`, `mods`).
    pub fn resolve_call(
        &self,
        file: &str,
        mods: &[String],
        path: &[String],
        generic: bool,
    ) -> Outcome {
        let m: Mk = (file.to_string(), mods.to_vec());
        let (Some(first), true) = (path.first(), self.live(&m)) else {
            return Outcome::Unknown;
        };
        let mut walk = Walk {
            ix: self,
            budget: MAX_LOOKUPS,
            stack: Vec::new(),
        };
        let res = if path.len() == 1 {
            walk.lookup(&m, first, Ns::Value, false, 0)
        } else {
            walk.resolve_path(&m, path, Ns::Value, 0)
        };
        if let Res::Fn(id) = res {
            return Outcome::Target(id);
        }
        if let Some(id) = path
            .len()
            .checked_sub(1)
            .filter(|n| *n > 0)
            .and_then(|n| self.type_assoc(&m, &path[..n], &path[n], generic))
        {
            return Outcome::Target(id);
        }
        let renamed = self.items.get(&m).is_some_and(|v| {
            v.iter().any(|i| {
                matches!(&i.kind, RKind::Use { path: p, local, .. }
                    if local == first && p.last() != Some(first))
            })
        });
        if renamed {
            Outcome::Drop
        } else {
            Outcome::Unknown
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ns {
    Value,
    Type,
}

#[derive(Clone, PartialEq, Eq)]
enum Res {
    Fn(String),
    Mod(Mk),
    /// A struct, enum or union: the module that holds it and its name.
    Ty(Mk, String),
    Other,
    Zero,
    Stop,
    /// The lookup asked for a key that is still in progress.
    Cycle,
}

struct Walk<'a> {
    ix: &'a RustIndex,
    budget: u32,
    stack: Vec<(Mk, String, Ns, bool)>,
}

impl Walk<'_> {
    /// L(m, name, ns): items, then `use` bindings, then globs (R3).
    fn lookup(&mut self, m: &Mk, name: &str, ns: Ns, pub_only: bool, depth: u32) -> Res {
        if depth > MAX_DEPTH || self.budget == 0 {
            return Res::Stop;
        }
        self.budget -= 1;
        let key = (m.clone(), name.to_string(), ns, pub_only);
        if self.stack.contains(&key) {
            return Res::Cycle;
        }
        self.stack.push(key);
        let res = self.lookup_in(m, name, ns, pub_only, depth);
        self.stack.pop();
        res
    }

    fn lookup_in(&mut self, m: &Mk, name: &str, ns: Ns, pub_only: bool, depth: u32) -> Res {
        let ix = self.ix;
        if !ix.live(m) {
            return Res::Stop;
        }
        let Some(items) = ix.items.get(m) else {
            return Res::Zero;
        };
        let mut found: Vec<Res> = Vec::new();
        let mut macro_in = false;
        let mut cycle = false;
        for it in items {
            let hit = match &it.kind {
                RKind::Macro => {
                    macro_in = true;
                    continue;
                }
                RKind::Use {
                    path,
                    local,
                    modonly,
                } if local == name => {
                    if pub_only && !it.public {
                        continue;
                    }
                    if it.cfg {
                        return Res::Stop;
                    }
                    if *modonly && ns == Ns::Value {
                        continue;
                    }
                    match self.resolve_path(m, path, ns, depth + 1) {
                        // An explicit `use` shadows every glob, so a failed one stops.
                        Res::Zero => {
                            // It may bind the other namespace only.
                            let other = if ns == Ns::Value { Ns::Type } else { Ns::Value };
                            if self.resolve_path(m, path, other, depth + 1) == Res::Zero {
                                return Res::Stop;
                            }
                            continue;
                        }
                        // The path runs through the name being looked up. An item
                        // with that name then settles it, since both cannot bind.
                        Res::Cycle => {
                            cycle = true;
                            continue;
                        }
                        r => r,
                    }
                }
                kind => match item_res(ix, m, kind, name, ns) {
                    Some(r) if !(pub_only && !it.public) => {
                        if it.cfg {
                            return Res::Stop;
                        }
                        r
                    }
                    _ => continue,
                },
            };
            found.push(hit);
        }
        if found.contains(&Res::Stop) || found.len() > 1 || (cycle && found.is_empty()) {
            return Res::Stop;
        }
        if let Some(r) = found.pop() {
            return r;
        }
        let globs: Vec<&RItem> = items
            .iter()
            .filter(|i| matches!(&i.kind, RKind::Use { local, .. } if local == "*"))
            .filter(|i| !pub_only || i.public)
            .collect();
        if globs.is_empty() {
            return Res::Zero;
        }
        if macro_in {
            return Res::Stop;
        }
        let mut outs: Vec<Res> = Vec::new();
        for g in globs {
            let RKind::Use { path, .. } = &g.kind else {
                return Res::Stop;
            };
            if g.cfg {
                return Res::Stop;
            }
            let Res::Mod(t) = self.resolve_path(m, path, Ns::Type, depth + 1) else {
                return Res::Stop;
            };
            if ix.has_macro(&t) {
                return Res::Stop;
            }
            let anc = ix.is_ancestor(&t, m);
            match self.lookup(&t, name, ns, pub_only || !anc, depth + 1) {
                Res::Stop | Res::Cycle => return Res::Stop,
                Res::Zero => {}
                r => {
                    if !outs.contains(&r) {
                        outs.push(r);
                    }
                }
            }
        }
        match (outs.pop(), outs.is_empty()) {
            (None, _) => Res::Zero,
            (Some(r), true) => r,
            _ => Res::Stop,
        }
    }

    /// Walks a path from module `m` (R7 and R9). The last segment is looked
    /// up in namespace `ns`; every segment before it must be a module.
    fn resolve_path(&mut self, m: &Mk, path: &[String], ns: Ns, depth: u32) -> Res {
        let ix = self.ix;
        let (Some(first), Some(info)) = (path.first(), ix.info.get(m)) else {
            return Res::Stop;
        };
        let mut i = 1;
        // True once the walk crossed into another crate (R8): from there on
        // only `pub` items and `pub use` re-exports are visible.
        let mut outside = false;
        let mut cur = match first.as_str() {
            "crate" => (info.root.clone(), Vec::new()),
            "self" => m.clone(),
            "super" => {
                let mut cur = m.clone();
                i = 0;
                while path.get(i).map(String::as_str) == Some("super") {
                    let Some(up) = ix.info.get(&cur).and_then(|x| x.parent.clone()) else {
                        return Res::Stop;
                    };
                    cur = up;
                    i += 1;
                }
                cur
            }
            seg => {
                let modern = ix.editions.get(&info.root).is_some_and(|y| *y >= 2018);
                if !modern {
                    return Res::Stop;
                }
                if path.len() == 1 {
                    return self.lookup(m, seg, ns, false, depth + 1);
                }
                match self.lookup(m, seg, Ns::Type, false, depth + 1) {
                    Res::Mod(k) => k,
                    Res::Cycle => return Res::Cycle,
                    Res::Zero => {
                        // An item macro can define a shadowing `mod` or `use`.
                        if ix.has_macro(m) {
                            return Res::Stop;
                        }
                        let lib = ix.externs.get(&info.root).and_then(|e| e.get(seg));
                        let Some(Some(lib)) = lib else {
                            return Res::Stop;
                        };
                        outside = true;
                        (lib.clone(), Vec::new())
                    }
                    _ => return Res::Stop,
                }
            }
        };
        if i >= path.len() {
            return if ns == Ns::Type {
                Res::Mod(cur)
            } else {
                Res::Zero
            };
        }
        let last = path.len() - 1;
        for seg in &path[i..last] {
            match self.lookup(&cur, seg, Ns::Type, outside, depth) {
                Res::Mod(k) => cur = k,
                Res::Cycle => return Res::Cycle,
                _ => return Res::Stop,
            }
        }
        self.lookup(&cur, &path[last], ns, outside, depth)
    }
}

/// The result of one non-`use` item for `name` in `ns`, if it binds it.
fn item_res(ix: &RustIndex, m: &Mk, kind: &RKind, name: &str, ns: Ns) -> Option<Res> {
    match kind {
        RKind::Fn { name: n, id } if ns == Ns::Value && n == name => Some(Res::Fn(id.clone())),
        RKind::Value { name: n } if ns == Ns::Value && n == name => Some(Res::Other),
        RKind::Type { name: n } if ns == Ns::Type && n == name => Some(Res::Other),
        RKind::Both { name: n } if n == name => Some(Res::Other),
        RKind::Adt { name: n, both, .. } if n == name => Some(if ns == Ns::Type {
            Res::Ty(m.clone(), name.to_string())
        } else if *both {
            Res::Other
        } else {
            return None;
        }),
        RKind::Mod { name: n, .. } if ns == Ns::Type && n == name => {
            Some(ix.child(m, name).map_or(Res::Stop, Res::Mod))
        }
        _ => None,
    }
}

#[cfg(test)]
#[path = "rust_tree_tests.rs"]
mod tests;
