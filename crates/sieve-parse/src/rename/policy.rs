//! The repo-level facts for the import rule: when a non-relative import
//! specifier is proven to be a different module.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use super::Reason;

/// How deep an `extends` chain may go before the scan gives up.
const MAX_EXTENDS_DEPTH: usize = 16;

/// Prefixes that mark a bundler alias or a package `imports` entry (import rule, item c).
const ALIAS_PREFIXES: [&str; 5] = ["#", "@/", "~", "$", "virtual:"];

/// The repo facts that decide a non-relative specifier (import rule, items a to e).
#[derive(Debug, Default)]
pub(super) struct Policy {
    /// True when a tsconfig or jsconfig sets `paths` or `baseUrl`, or the
    /// scan cannot read its `extends` chain.
    paths_set: bool,
    /// The `name` of every `package.json` in the repo.
    package_names: HashSet<String>,
    /// Every module name that a `declare module "name"` block declares.
    ambient: HashSet<String>,
    /// True when a bundler or babel config holds the text `alias`.
    alias_text: bool,
    /// True when a config sets `preserveSymlinks` or turns `symlinks` off.
    /// Resolution then keeps link paths, so a link is not a plain alias.
    preserve_symlinks: bool,
}

impl Policy {
    /// Reads the repo facts. `files` are the repo-relative posix paths of the
    /// walk. `ambient` holds the `declare module` names the scan found.
    pub(super) fn load(root: &Path, files: &[String], ambient: HashSet<String>) -> Policy {
        let mut policy = Policy {
            ambient,
            ..Policy::default()
        };
        let known: HashSet<&str> = files.iter().map(String::as_str).collect();
        for rel in files {
            let base = rel.rsplit('/').next().unwrap_or(rel);
            if (base.starts_with("tsconfig") || base.starts_with("jsconfig"))
                && base.ends_with(".json")
            {
                if config_sets_paths(root, &known, rel, 0) {
                    policy.paths_set = true;
                }
                if config_preserves_links(root, &known, rel, 0) {
                    policy.preserve_symlinks = true;
                }
            }
            if base == "package.json" {
                if let Some(name) = package_name(root, rel) {
                    policy.package_names.insert(name);
                }
                if package_maps_modules(root, rel) {
                    policy.alias_text = true;
                }
            }
            if is_alias_config(base) {
                if file_has_alias(root, rel) {
                    policy.alias_text = true;
                }
                if fs::read_to_string(root.join(rel)).map_or(true, |t| text_keeps_links(&t)) {
                    policy.preserve_symlinks = true;
                }
            }
            if matches!(base, "deno.json" | "deno.jsonc" | "import_map.json") {
                let maps = fs::read_to_string(root.join(rel))
                    .map_or(true, |t| t.contains("imports") || t.contains("importMap"));
                policy.alias_text |= maps;
            }
        }
        // The walk skips dot names, so read the dot configs by hand.
        let includes = sieve_core::walk::read_include_dirs(root);
        policy.alias_text |= dot_configs_alias(root, "", false, &includes);
        policy
    }

    /// True when a config keeps link paths in resolution (symlink rule).
    pub(super) fn preserve_symlinks(&self) -> bool {
        self.preserve_symlinks
    }

    /// Says whether the non-relative `spec` is proven not to reach a repo
    /// module. An error holds the reason and the text of the refusal.
    pub(super) fn classify(&self, spec: &str) -> Result<(), (Reason, String)> {
        let unproven = |text: String| Err((Reason::Unproven, text));
        if spec.starts_with('/') || ALIAS_PREFIXES.iter().any(|p| spec.starts_with(p)) {
            return unproven(format!("\"{spec}\" can be a bundler alias"));
        }
        if self.paths_set {
            return unproven(format!(
                "a tsconfig or jsconfig sets paths or baseUrl, so \"{spec}\" can be a repo module"
            ));
        }
        let package = package_of(spec);
        if self.package_names.contains(package) {
            return Err((
                Reason::PackageSelfImport,
                format!("\"{package}\" is a package in this repo"),
            ));
        }
        if self.ambient.contains(spec) {
            return unproven(format!("a declare module block names \"{spec}\""));
        }
        if self.alias_text {
            return unproven(format!(
                "a bundler or babel config holds an alias, so \"{spec}\" can be a repo module"
            ));
        }
        Ok(())
    }
}

/// The package name of a bare specifier: `@scope/name` or the first segment.
fn package_of(spec: &str) -> &str {
    let mut ends = spec.match_indices('/').map(|(i, _)| i);
    let cut = if spec.starts_with('@') {
        ends.nth(1)
    } else {
        ends.next()
    };
    cut.map_or(spec, |i| &spec[..i])
}

/// Words in a config file that map one module name to another or add a
/// module search root.
const ALIAS_WORDS: [&str; 10] = [
    "alias",
    "moduleNameMapper",
    "module-resolver",
    "_moduleAliases",
    "moduleDirectories",
    "modulePaths",
    "roots",
    "NODE_PATH",
    "resolve.modules",
    "modules:",
];

/// Reads the dot configs that the build walk skips, below `rel`: every
/// `.swcrc*`, `.babelrc*` and `.env*` file at any depth, and every file in a
/// `.storybook` directory. True when one holds an alias word. Other dot
/// directories and the skip directories are not entered.
fn dot_configs_alias(root: &Path, rel: &str, in_storybook: bool, includes: &[String]) -> bool {
    let dir = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let Ok(entries) = fs::read_dir(&dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let child = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        if meta.is_dir() {
            let storybook = name == ".storybook";
            let skipped = name.starts_with('.')
                || (sieve_core::walk::SKIP_DIRS.contains(&name.as_str())
                    && !includes.contains(&name));
            if (storybook || in_storybook || !skipped)
                && dot_configs_alias(root, &child, in_storybook || storybook, includes)
            {
                return true;
            }
        } else if meta.is_file() {
            let relevant = in_storybook
                || [".swcrc", ".babelrc", ".env"]
                    .iter()
                    .any(|p| name.starts_with(p));
            if relevant && file_has_alias(root, &child) {
                return true;
            }
        }
    }
    false
}

/// True for a file that can hold a module alias: any `*.config.<script>`
/// (vite, vitest, webpack, rollup, esbuild, jest, babel and so on), a
/// `webpack*.js`, a `babel.config.*` or a `.babelrc*`.
fn is_alias_config(base: &str) -> bool {
    const SCRIPT: [&str; 6] = ["js", "cjs", "mjs", "ts", "cts", "mts"];
    let script_config = base
        .rsplit_once('.')
        .and_then(|(stem, ext)| stem.ends_with(".config").then_some(ext))
        .is_some_and(|ext| SCRIPT.contains(&ext));
    script_config
        || (base.starts_with("webpack") && base.ends_with(".js"))
        || base.starts_with("babel.config.")
        || base.starts_with(".babelrc")
}

fn file_has_alias(root: &Path, rel: &str) -> bool {
    // An unreadable config is a doubt, so it counts as an alias.
    fs::read_to_string(root.join(rel))
        .map_or(true, |text| ALIAS_WORDS.iter().any(|w| text.contains(w)))
}

/// True when a `package.json` maps module names: `imports`, a `browser`
/// object, `jest.moduleNameMapper`, babel `module-resolver` or an alias key.
/// An unreadable or invalid file counts as true.
fn package_maps_modules(root: &Path, rel: &str) -> bool {
    let Ok(text) = fs::read_to_string(root.join(rel)) else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return true;
    };
    value.get("imports").is_some()
        || value.get("browser").is_some_and(|b| b.is_object())
        || ALIAS_WORDS.iter().any(|w| text.contains(w))
}

fn package_name(root: &Path, rel: &str) -> Option<String> {
    let text = fs::read_to_string(root.join(rel)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("name")?.as_str().map(str::to_string)
}

/// True when the config `rel` sets `paths` or `baseUrl`, directly or through
/// `extends`. Any config the scan cannot read or follow counts as true.
fn config_sets_paths(root: &Path, known: &HashSet<&str>, rel: &str, depth: usize) -> bool {
    if depth > MAX_EXTENDS_DEPTH {
        return true;
    }
    let Ok(text) = fs::read_to_string(root.join(rel)) else {
        return true;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else {
        return true;
    };
    if let Some(options) = value.get("compilerOptions") {
        if options.get("paths").is_some() || options.get("baseUrl").is_some() {
            return true;
        }
    }
    let parents: Vec<&str> = match value.get("extends") {
        None => Vec::new(),
        Some(serde_json::Value::String(one)) => vec![one.as_str()],
        Some(serde_json::Value::Array(many)) => {
            let names: Vec<&str> = many.iter().filter_map(|v| v.as_str()).collect();
            if names.len() != many.len() {
                return true;
            }
            names
        }
        Some(_) => return true,
    };
    let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
    for parent in parents {
        if !parent.starts_with('.') {
            return true;
        }
        let mut path = join_posix(dir, parent);
        if !path.ends_with(".json") {
            path.push_str(".json");
        }
        if !known.contains(path.as_str()) || config_sets_paths(root, known, &path, depth + 1) {
            return true;
        }
    }
    false
}

/// True when the text holds `preserveSymlinks` or a `symlinks: false`.
fn text_keeps_links(text: &str) -> bool {
    if text.contains("preserveSymlinks") {
        return true;
    }
    text.match_indices("symlinks").any(|(i, w)| {
        let rest = text[i + w.len()..].trim_start_matches(['"', '\'', ' ', '\t']);
        rest.strip_prefix(':')
            .is_some_and(|r| r.trim_start().starts_with("false"))
    })
}

/// True when the config `rel` or a config that it extends keeps link paths.
/// An `extends` that the scan cannot follow is not a doubt for this flag.
fn config_preserves_links(root: &Path, known: &HashSet<&str>, rel: &str, depth: usize) -> bool {
    if depth > MAX_EXTENDS_DEPTH {
        return false;
    }
    let Ok(text) = fs::read_to_string(root.join(rel)) else {
        return true;
    };
    if text_keeps_links(&text) {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text)) else {
        return false;
    };
    let parents: Vec<&str> = match value.get("extends") {
        Some(serde_json::Value::String(one)) => vec![one.as_str()],
        Some(serde_json::Value::Array(many)) => many.iter().filter_map(|v| v.as_str()).collect(),
        _ => Vec::new(),
    };
    let dir = rel.rsplit_once('/').map_or("", |(d, _)| d);
    parents
        .into_iter()
        .filter(|p| p.starts_with('.'))
        .any(|parent| {
            let mut path = join_posix(dir, parent);
            if !path.ends_with(".json") {
                path.push_str(".json");
            }
            known.contains(path.as_str()) && config_preserves_links(root, known, &path, depth + 1)
        })
}

fn join_posix(dir: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// Removes comments and trailing commas from JSONC text.
fn strip_jsonc(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    let mut in_string = false;
    while i < chars.len() {
        let c = chars[i];
        if in_string {
            out.push(c);
            if c == '\\' {
                if let Some(&next) = chars.get(i + 1) {
                    out.push(next);
                    i += 1;
                }
            } else if c == '"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match (c, chars.get(i + 1)) {
            ('"', _) => {
                in_string = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                while i < chars.len() && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            ('/', Some('*')) => {
                i += 2;
                while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
                continue;
            }
            _ => out.push(c),
        }
        i += 1;
    }
    strip_trailing_commas(&out)
}

fn strip_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            if c == '"' && chars.get(i.wrapping_sub(1)) != Some(&'\\') {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}
