//! Design S7: does a glob or `require.context` call reach a chain file?
//!
//! The matcher follows Vite glob rules for `*`, `**`, `?`, `{a,b}` and
//! `[...]` classes. A form it does not handle gives "reaches", so the rename
//! refuses. A match is never removed: negative patterns are skipped.

use std::collections::HashSet;

/// The most alternatives that one brace pattern may expand to.
const MAX_ALTERNATIVES: usize = 1024;

/// What a glob or context call can reach (design S7 and S7d).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Reach {
    /// The call cannot reach a chain file.
    No,
    /// The call names a chain file or a symlink path. The module object is
    /// traceable, so the rename refuses.
    Traced,
    /// The scan cannot judge the call, or a `require.context` directory holds
    /// a chain file. Design S7d refuses only when an old-name form exists. A
    /// glob pattern that matches a chain file is `Traced` and always refuses.
    Untraceable,
}

impl Reach {
    /// The stronger of two results: `Traced`, then `Untraceable`, then `No`.
    fn max(self, other: Reach) -> Reach {
        match (self, other) {
            (Reach::Traced, _) | (_, Reach::Traced) => Reach::Traced,
            (Reach::Untraceable, _) | (_, Reach::Untraceable) => Reach::Untraceable,
            _ => Reach::No,
        }
    }
}

/// What the positive patterns of an `import.meta.glob*` call in `file` can
/// reach. `links` holds the repo paths of the symlinks the scan knows. A
/// pattern whose fixed directory part meets one of them is `Traced`. A
/// pattern the matcher cannot judge is `Untraceable`.
pub(super) fn glob_reaches(
    file: &str,
    patterns: &[String],
    links: &[String],
    chain: &HashSet<String>,
) -> Reach {
    patterns
        .iter()
        .filter(|p| !p.starts_with('!'))
        .fold(Reach::No, |acc, p| {
            acc.max(pattern_reaches(file, p, links, chain))
        })
}

/// What `require.context(dir, deep, regex)` in `file` can reach. A chain file
/// under `dir` is `Untraceable`: no regex is evaluated and the module keys are
/// data. A `dir` the scan cannot resolve is `Untraceable`. A symlink is
/// `Traced`.
pub(super) fn context_reaches(
    file: &str,
    dir: &str,
    shallow: bool,
    links: &[String],
    chain: &HashSet<String>,
) -> Reach {
    if !(dir == "." || dir == ".." || dir.starts_with("./") || dir.starts_with("../")) {
        return Reach::Untraceable;
    }
    let base = super::join_posix(super::dir_of(file), dir);
    if link_meets(&base, links) {
        return Reach::Traced;
    }
    let under = chain.iter().any(|f| {
        let rest = if base.is_empty() {
            Some(f.as_str())
        } else {
            f.strip_prefix(&format!("{base}/"))
        };
        rest.is_some_and(|r| !shallow || !r.contains('/'))
    });
    if under {
        Reach::Untraceable
    } else {
        Reach::No
    }
}

/// True when a symlink is the directory, lies under it, or holds it.
fn link_meets(dir: &str, links: &[String]) -> bool {
    links.iter().any(|l| {
        dir.is_empty()
            || l == dir
            || l.starts_with(&format!("{dir}/"))
            || dir.starts_with(&format!("{l}/"))
    })
}

fn pattern_reaches(file: &str, pattern: &str, links: &[String], chain: &HashSet<String>) -> Reach {
    // A `?` is a one-character wildcard or a query suffix: the scan cannot judge it.
    if pattern.contains('?') {
        return Reach::Untraceable;
    }
    let Some(alts) = expand_braces(pattern) else {
        return Reach::Untraceable;
    };
    alts.iter().fold(Reach::No, |acc, alt| {
        if !valid_syntax(alt) {
            return acc.max(Reach::Untraceable);
        }
        let Some(segs) = resolve(file, alt) else {
            return acc.max(Reach::Untraceable);
        };
        let fixed: Vec<&str> = segs
            .iter()
            .map(String::as_str)
            .take_while(|s| !s.contains(['*', '[']))
            .collect();
        if link_meets(&fixed.join("/"), links) {
            return acc.max(Reach::Traced);
        }
        let hit = chain.iter().any(|f| {
            let parts: Vec<&str> = f.split('/').collect();
            match_segments(&segs, &parts)
        });
        if hit {
            acc.max(Reach::Traced)
        } else {
            acc
        }
    })
}

/// Resolves one brace-free pattern to repo-relative segments. `None` means an
/// alias, a root-relative pattern (the Vite root may differ from the repo
/// root), or a `..` after a name.
fn resolve(file: &str, alt: &str) -> Option<Vec<String>> {
    let first = alt.split('/').next().unwrap_or("");
    if first != "." && first != ".." {
        return None;
    }
    let mut all: Vec<String> = super::dir_of(file)
        .split('/')
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    let rest = alt;
    let mut leading = true;
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." if leading => {
                all.pop();
            }
            ".." => return None,
            s => {
                leading = false;
                all.push(s.to_string());
            }
        }
    }
    Some(all)
}

/// Expands every `{a,b}` group (nested too). `None` for a group that is
/// unbalanced, holds no comma, or expands past the limit.
fn expand_braces(pattern: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '[' => {
                i += class_end(&chars[i..])?;
                continue;
            }
            '{' => {
                let (parts, end) = split_group(&chars, i)?;
                let head: String = chars[..i].iter().collect();
                let tail: String = chars[end + 1..].iter().collect();
                let mut out = Vec::new();
                for part in parts {
                    let joined = format!("{head}{part}{tail}");
                    out.extend(expand_braces(&joined)?);
                    if out.len() > MAX_ALTERNATIVES {
                        return None;
                    }
                }
                return Some(out);
            }
            '}' => return None,
            _ => {}
        }
        i += 1;
    }
    Some(vec![pattern.to_string()])
}

/// Splits the group that opens at `open` on its top-level commas. It gives
/// the parts and the index of the closing brace.
fn split_group(chars: &[char], open: usize) -> Option<(Vec<String>, usize)> {
    let mut depth = 0usize;
    let mut parts = vec![String::new()];
    let mut i = open;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '[' => {
                let n = class_end(&chars[i..])?;
                let text: String = chars[i..i + n].iter().collect();
                parts.last_mut()?.push_str(&text);
                i += n;
                continue;
            }
            '{' => {
                depth += 1;
                if depth > 1 {
                    parts.last_mut()?.push(c);
                }
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (parts.len() > 1).then_some((parts, i));
                }
                parts.last_mut()?.push(c);
            }
            ',' if depth == 1 => parts.push(String::new()),
            _ => parts.last_mut()?.push(c),
        }
        i += 1;
    }
    None
}

/// The length of the class that starts at `chars[0] == '['`, with its `]`.
fn class_end(chars: &[char]) -> Option<usize> {
    let mut i = 1;
    if matches!(chars.get(i), Some('!' | '^')) {
        i += 1;
    }
    if chars.get(i) == Some(&']') {
        i += 1;
    }
    while i < chars.len() {
        if chars[i] == ']' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// False for syntax that the matcher does not handle: an escape, a group
/// with parentheses, a POSIX class, or a class that holds a slash.
fn valid_syntax(alt: &str) -> bool {
    if alt.contains(['\\', '(', ')', '{', '}']) {
        return false;
    }
    let chars: Vec<char> = alt.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '[' {
            let Some(n) = class_end(&chars[i..]) else {
                return false;
            };
            let body = &chars[i + 1..i + n - 1];
            if body.contains(&'/') || body.first() == Some(&':') || body.contains(&'[') {
                return false;
            }
            i += n;
        } else {
            i += 1;
        }
    }
    true
}

/// Matches pattern segments against path segments. A `**` segment takes any
/// number of path segments, zero included.
fn match_segments(pat: &[String], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|k| match_segments(rest, &path[k..]))
        }
        Some((first, rest)) => match path.split_first() {
            Some((head, tail)) => {
                let p: Vec<char> = first.chars().collect();
                let t: Vec<char> = head.chars().collect();
                match_name(&p, &t) && match_segments(rest, tail)
            }
            None => false,
        },
    }
}

/// Matches one path segment against one pattern segment.
fn match_name(p: &[char], t: &[char]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some('*') => (0..=t.len()).any(|k| match_name(&p[1..], &t[k..])),
        Some('[') => {
            let Some(n) = class_end(p) else {
                return false;
            };
            match t.split_first() {
                Some((c, tail)) => in_class(&p[1..n - 1], *c) && match_name(&p[n..], tail),
                None => false,
            }
        }
        Some(c) => t.first() == Some(c) && match_name(&p[1..], &t[1..]),
    }
}

/// True when `c` is in a class body (the text between `[` and `]`).
fn in_class(body: &[char], c: char) -> bool {
    let (negate, items) = match body.first() {
        Some('!' | '^') => (true, &body[1..]),
        _ => (false, body),
    };
    let mut hit = false;
    let mut i = 0;
    while i < items.len() {
        if items.get(i + 1) == Some(&'-') && i + 2 < items.len() {
            hit |= (items[i]..=items[i + 2]).contains(&c);
            i += 3;
        } else {
            hit |= items[i] == c;
            i += 1;
        }
    }
    hit != negate
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(files: &[&str]) -> HashSet<String> {
        files.iter().map(|f| (*f).to_string()).collect()
    }

    fn reaches(file: &str, pattern: &str, files: &[&str]) -> bool {
        glob_reaches(file, &[pattern.to_string()], &[], &chain(files)) != Reach::No
    }

    #[test]
    fn star_stays_in_one_directory() {
        assert!(reaches("src/m.ts", "./*.ts", &["src/def.ts"]));
        assert!(!reaches("src/m.ts", "./*.ts", &["src/sub/def.ts"]));
        assert!(!reaches("other/m.ts", "./*.ts", &["src/def.ts"]));
    }

    #[test]
    fn double_star_matches_any_depth_and_zero_dirs() {
        assert!(reaches("src/m.ts", "./**/*.ts", &["src/a/b/c/def.ts"]));
        assert!(reaches("src/m.ts", "./**/*.ts", &["src/def.ts"]));
        assert!(!reaches("src/m.ts", "./**/*.js", &["src/def.ts"]));
    }

    #[test]
    fn braces_nest_and_expand() {
        assert!(reaches("m.ts", "./{a,b}/def.ts", &["b/def.ts"]));
        assert!(reaches("m.ts", "./{a,{b,c}}/def.ts", &["c/def.ts"]));
        assert!(!reaches("m.ts", "./{a,b}/def.ts", &["c/def.ts"]));
        assert!(reaches("m.ts", "./*.{js,ts}", &["def.ts"]));
    }

    #[test]
    fn question_mark_and_classes() {
        assert!(reaches("m.ts", "./de?.ts", &["zzz.ts"]));
        assert!(reaches("m.ts", "./def.ts?raw", &["zzz.ts"]));
        assert!(reaches("m.ts", "./[a-e]ef.ts", &["def.ts"]));
        assert!(!reaches("m.ts", "./[!d]ef.ts", &["def.ts"]));
        assert!(reaches("m.ts", "./[^x]ef.ts", &["def.ts"]));
    }

    #[test]
    fn unhandled_syntax_reaches() {
        assert!(reaches("m.ts", "./+(a|b).ts", &["zzz.ts"]));
        assert!(reaches("m.ts", "./[[:alpha:]].ts", &["zzz.ts"]));
        assert!(reaches("m.ts", "./{a}.ts", &["zzz.ts"]));
        assert!(reaches("m.ts", "./[a.ts", &["zzz.ts"]));
        assert!(reaches("m.ts", "./a/*/../b.ts", &["zzz.ts"]));
    }

    #[test]
    fn untraceable_and_traced_are_told_apart() {
        let c = chain(&["src/def.ts"]);
        let one = |p: &str| glob_reaches("src/m.ts", &[p.to_string()], &[], &c);
        assert_eq!(one("#src/*.ts"), Reach::Untraceable);
        assert_eq!(one("/src/*.ts"), Reach::Untraceable);
        assert_eq!(one("./*.ts?raw"), Reach::Untraceable);
        assert_eq!(one("./+(a|b).ts"), Reach::Untraceable);
        assert_eq!(one("./*.ts"), Reach::Traced);
        assert_eq!(one("./def.ts"), Reach::Traced);
        assert_eq!(one("./*.js"), Reach::No);
        let both = ["#a/*.ts".to_string(), "./*.ts".to_string()];
        assert_eq!(glob_reaches("src/m.ts", &both, &[], &c), Reach::Traced);
    }

    #[test]
    fn alias_and_root_patterns() {
        assert!(reaches("m.ts", "@/mods/*.ts", &["zzz.ts"]));
        assert!(reaches("m.ts", "/src/*.ts", &["zzz.ts"]));
        assert!(reaches("m.ts", "/lib/*.ts", &["src/def.ts"]));
    }

    #[test]
    fn a_symlink_under_the_fixed_directory_reaches() {
        let c = chain(&["lib/def.ts"]);
        let links = ["src/link".to_string()];
        let p = ["./link/*.ts".to_string()];
        assert_eq!(glob_reaches("src/m.ts", &p, &links, &c), Reach::Traced);
        let q = ["./other/*.ts".to_string()];
        assert_eq!(glob_reaches("src/m.ts", &q, &links, &c), Reach::No);
        assert_eq!(
            context_reaches("src/m.ts", "./link", false, &links, &c),
            Reach::Traced
        );
        assert_eq!(
            context_reaches("src/m.ts", "./other", false, &links, &c),
            Reach::No
        );
    }

    #[test]
    fn negative_pattern_is_skipped() {
        let c = chain(&["src/def.ts"]);
        let p = ["./*.ts".to_string(), "!./def.ts".to_string()];
        assert_eq!(glob_reaches("src/m.ts", &p, &[], &c), Reach::Traced);
        assert_eq!(
            glob_reaches("src/m.ts", &["!./def.ts".to_string()], &[], &c),
            Reach::No
        );
    }

    #[test]
    fn parent_directory_prefix_resolves() {
        assert!(reaches("src/a/m.ts", "../*.ts", &["src/def.ts"]));
        assert!(!reaches("src/a/m.ts", "../*.ts", &["src/a/def.ts"]));
    }

    #[test]
    fn context_checks_the_directory_and_depth() {
        let c = chain(&["src/lib/def.ts"]);
        assert!(context_reaches("src/m.ts", "./lib", false, &[], &c) != Reach::No);
        assert!(context_reaches("src/m.ts", "./lib", true, &[], &c) != Reach::No);
        assert!(context_reaches("src/m.ts", "./other", false, &[], &c) == Reach::No);
        assert!(context_reaches("src/m.ts", ".", false, &[], &c) != Reach::No);
        assert!(context_reaches("src/m.ts", ".", true, &[], &c) == Reach::No);
        assert!(context_reaches("src/m.ts", "lib", false, &[], &c) != Reach::No);
    }
}
